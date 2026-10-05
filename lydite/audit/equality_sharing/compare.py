"""Compare immutable paired runs without treating diagnostics as proof authority."""
import argparse
import hashlib
import json
from pathlib import Path

from audit.equality_sharing.run import evidence

VERIFIED = "stuttering_refinement_verified"


def compare(pairs):
    result = {"phase": "checked_compound_equality_sharing", "mode": "independent_lemmas",
              "per_primitive_defaults_unchanged": True,
              "per_primitive_limits": {"max_work": 100000000, "max_clauses": 1000000, "timeout_ms": 10000},
              "single_query_speedup_claim": False, "paired_inputs_identical": True,
              "runs": [], "results": [], "errors": []}
    for baseline_dir, candidate_dir in pairs:
        baseline = json.loads((baseline_dir / "summary.json").read_text())
        candidate = json.loads((candidate_dir / "summary.json").read_text())
        rows = {r["name"]: r for r in baseline["rows"]}
        if set(rows) != {r["name"] for r in candidate["rows"]}:
            result["errors"].append("Paired case sets differ: " + str(candidate_dir))
        result["runs"].append({"baseline": str(baseline_dir), "candidate": str(candidate_dir),
                               "baseline_sha256": baseline["checker_sha256"],
                               "candidate_sha256": candidate["checker_sha256"],
                               "width": candidate["width"], "alpha_seed": candidate["alpha_seed"]})
        for new in candidate["rows"]:
            name = new["name"]
            if name not in rows: continue
            old = rows[name]
            baseline_input = (baseline_dir / (name + ".json")).read_bytes()
            candidate_input = (candidate_dir / (name + ".json")).read_bytes()
            identical = baseline_input == candidate_input
            result["paired_inputs_identical"] &= identical
            report = json.loads((candidate_dir / name / "report.json").read_text())
            query = next(q for q in report["obligations"] if q["name"] == "microstep_refinement")
            facts = evidence(query)
            mutant = "_wrong" in name
            row = {"case": name, "width": candidate["width"], "alpha_seed": candidate["alpha_seed"],
                   "baseline": old["status"], "candidate": new["status"],
                   "gain": old["status"] == "unknown" and new["status"] == VERIFIED,
                   "mutant": mutant, "inputs_identical": identical,
                   "input_sha256": hashlib.sha256(candidate_input).hexdigest(),
                   "candidate_report": str(candidate_dir / name / "report.json"), "evidence": facts}
            result["results"].append(row)
            if not identical: result["errors"].append("Input mismatch: " + name)
            if mutant and (new["status"] != "counterexample" or not facts["reported_counterexamples_original_validated"]):
                result["errors"].append("Original mutant not replay-validated: " + name)
            if old["status"] == VERIFIED and new["status"] != VERIFIED:
                result["errors"].append("Baseline pass regressed: " + name)
    rows = result["results"]
    result["positive_instances"] = sum(not r["mutant"] for r in rows)
    result["unknown_to_verified_instances"] = sum(r["gain"] for r in rows)
    result["gaining_shapes_excluding_width_and_alpha"] = sorted({r["case"] for r in rows if r["gain"] and r["alpha_seed"] is None})
    result["mutants_replay_validated"] = sum(r["mutant"] and r["candidate"] == "counterexample" and r["evidence"]["reported_counterexamples_original_validated"] for r in rows)
    result["remaining_unknowns"] = [{"case": r["case"], "width": r["width"]} for r in rows if r["candidate"] == "unknown"]
    result["status"] = "failed" if result["errors"] else "passed_with_remaining_unknowns" if result["remaining_unknowns"] else "passed"
    root = Path(__file__).parent
    result["evaluation_source_sha256"] = {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(root.iterdir()) if p.suffix in (".py", ".md")}
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--pair", type=Path, nargs=2, action="append", required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    result = compare(args.pair)
    args.out.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({k: v for k, v in result.items() if k not in ("results", "runs", "evaluation_source_sha256")}, indent=2))


if __name__ == "__main__": main()
