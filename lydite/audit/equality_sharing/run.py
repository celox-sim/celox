"""Run compound state-encoding fixtures with unchanged finite limits."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

from audit.equality_sharing.generate import alpha_rename, cases


def evidence(query):
    """Count actual attempts once; derived totals are not extra solver work."""
    queries = []
    bundles = []
    seen_queries = set()
    def visit(q):
        if not isinstance(q, dict):
            return
        if q.get("backend") == "checked_proof_bundle":
            bundles.append(q)
        visit(q.get("original_attempt"))
        visit(q.get("original_recheck"))
        for key in ("children", "conjunctive_children"):
            for child in q.get(key, []):
                visit(child)
        if q.get("backend") not in (None, "checked_proof_bundle", "checked_congruence", "conjunctive_lemmas"):
            # Replay reports can occur both among children and as an explicit
            # original_recheck pointer. Each evidence file is one invocation.
            identity = (q.get("name"), q.get("evidence"))
            if identity not in seen_queries:
                seen_queries.add(identity)
                queries.append(q)
    visit(query)
    plans = []
    sharing = []
    validated_originals = []
    def collect(q):
        if not isinstance(q, dict): return
        if q.get("automatic_plan") is not None: plans.append(q["automatic_plan"])
        if q.get("equality_sharing") is not None: sharing.append(q["equality_sharing"])
        if q.get("backend") == "checked_proof_bundle":
            replay = q.get("original_recheck") or {}
            if q.get("status") == "counterexample":
                validated_originals.append(replay.get("solver_result") == "sat" and replay.get("finite", {}).get("original_formula_validated") is True)
            return
        if q.get("status") == "counterexample" and q.get("backend") == "finite_bv":
            validated_originals.append(q.get("finite", {}).get("original_formula_validated") is True)
        for child in q.get("children", []): collect(child)
    collect(query)
    return {"primitive_queries": len(queries),
            "automatic_plans": plans,
            "equality_sharing": sharing,
            "reported_counterexamples_original_validated": bool(validated_originals) and all(validated_originals),
            "finite_work_including_original_attempts": sum(q.get("finite", {}).get("work", 0) for q in queries),
            "emission_work_including_original_attempts": sum(q.get("emission_work", 0) for q in queries),
            "bundle_work_including_validation_and_derived_steps": sum(q.get("cost", {}).get("work_including_validation_and_derived_steps", 0) for q in bundles),
            "bundle_work_overlaps_primitive_work": True,
            "original_attempts": [{"status": b.get("original_attempt", {}).get("status"),
                                   "work": b.get("original_attempt", {}).get("finite", {}).get("work")}
                                  for b in bundles],
            "sat_queries": [{"name": q.get("name"), "original_formula_validated": q.get("finite", {}).get("original_formula_validated")}
                            for q in queries if q.get("solver_result") == "sat"]}


def run(checker, out, width, automatic, names=(), alpha_seed=None):
    out.mkdir(parents=True, exist_ok=False)
    env = {k: v for k, v in os.environ.items() if not k.startswith("LYDITE_")}
    env.update(LYDITE_SOLVER="finite", LYDITE_CONJUNCTIVE_LEMMAS="1")
    if automatic:
        env["LYDITE_AUTOMATIC_PROOFS"] = "independent_lemmas"
    summary = {"checker": str(checker), "checker_sha256": hashlib.sha256(checker.read_bytes()).hexdigest(),
               "width": width, "automatic": automatic, "alpha_seed": alpha_seed, "environment": {k: v for k, v in env.items() if k.startswith("LYDITE_")},
               "per_query_limits_unchanged": True, "rows": []}
    for case in cases(width):
        name = case["name"]
        if names and name not in names:
            continue
        if alpha_seed is not None:
            case = alpha_rename(case, alpha_seed)
            name = case["name"]
        file = out / (name + ".json")
        file.write_text(json.dumps(case, indent=2) + "\n")
        started = time.monotonic()
        result = subprocess.run([str(checker), str(file), "--out", str(out / name),
                                 "--z3", "FORBIDDEN_EXTERNAL_SOLVER"], env=env,
                                stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                                text=True, timeout=180)
        report = json.loads((out / name / "report.json").read_text())
        q = next((q for q in report.get("obligations", []) if q["name"] == "microstep_refinement"), {})
        row = {"name": name, "input_sha256": hashlib.sha256(file.read_bytes()).hexdigest(), "status": report["status"], "exit_code": result.returncode,
               "wall_seconds": time.monotonic() - started, "backend": q.get("backend"),
               "original_formula_validated": q.get("finite", {}).get("original_formula_validated"),
               "work": q.get("finite", {}).get("work"),
               "evidence": evidence(q),
               "children": [{"status": c.get("status"), "backend": c.get("backend"),
                             "work": c.get("finite", {}).get("work"), "cost": c.get("cost"),
                             "automatic_plan": c.get("automatic_plan")}
                            for c in q.get("children", [])]}
        if report.get("error"):
            row["error"] = report["error"]
        if result.stderr:
            row["stderr"] = result.stderr
        summary["rows"].append(row)
        (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(json.dumps(row), flush=True)
    return summary


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--checker", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--width", type=int, default=32)
    parser.add_argument("--automatic", action="store_true")
    parser.add_argument("--names", nargs="*", default=[])
    parser.add_argument("--alpha-seed", type=int)
    args = parser.parse_args()
    run(args.checker.resolve(), args.out.resolve(), args.width, args.automatic, args.names, args.alpha_seed)


if __name__ == "__main__":
    main()
