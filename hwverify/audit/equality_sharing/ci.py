"""Fail-closed, fresh finite-only execution of the frozen 32-row phase2 matrix.

Serialized reports are audit diagnostics, never proof certificates. This driver
executes an immutable snapshot of the supplied checker on every generated input.
"""
import argparse
from collections import Counter
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import time

from audit.equality_sharing.generate import alpha_rename, cases
from audit.veryl_scaling.rv32i_pipeline import validate_conjunctive_obligation, validate_proof_bundle

VERIFIED = "stuttering_refinement_verified"
FLAGS = {"HWVERIFY_SOLVER": "finite", "HWVERIFY_CONJUNCTIVE_LEMMAS": "1",
         "HWVERIFY_AUTOMATIC_PROOFS": "independent_lemmas"}
LIMITS = {"max_work": 100_000_000, "max_clauses": 1_000_000,
          "timeout_ms": 10_000, "max_terms": 100_000,
          "max_variables": 200_000, "max_depth": 512}
POSITIVES = ("compound_sum_transport", "compound_product_transport",
             "transitive_encoded_transport", "reversed_transitive_mac_shift",
             "guarded_compound_transport", "nested_guard_compound_shift",
             "nested_guard_compound_mac", "branched_compound_reuse")
LIMITATIONS = {"compound_product_transport", "nested_guard_compound_mac"}
HELDOUT = ("compound_product_transport", "transitive_encoded_transport",
           "reversed_transitive_mac_shift", "nested_guard_compound_mac")
ALPHA = ("transitive_encoded_transport", "transitive_encoded_transport_wrong_result",
         "transitive_encoded_transport_complement_wrong")
OBLIGATIONS = {"binding_nonempty", "reset_binding", "microstep_refinement",
               "commit_eligible", "progress_nonvacuity", "commit_reachable_in_relation",
               "noncommit_rank_decreases"}
SAT_OBLIGATIONS = {"binding_nonempty", "progress_nonvacuity", "commit_reachable_in_relation"}
PRIMITIVES = {"finite_bv", "structural_kernel"}
AGGREGATES = {"conjunctive_lemmas", "checked_proof_bundle"}
# Frozen byte hashes from the paired canonical input corpus, not saved verdicts.
INPUT_SHA256 = {
    (32, None, 'compound_sum_transport'): 'c001e45f46345c72f57e864360fed2f62a3171216fe2f950542d48cc9d9485f9',
    (32, None, 'compound_product_transport'): '9d9089a2543ec5cbbe9312f770f9bfb1f08793dd08bfbdf1be78e8659eb2510c',
    (32, None, 'transitive_encoded_transport'): '4d693be3a8fc0a9c94fde1d770f774e5a83e0d74cca02a3f6b874c3dc8515e84',
    (32, None, 'reversed_transitive_mac_shift'): '2d7534fcc0435ced1a905c2ae20109df57095cfcf7d6dc6181ab4fcd0a78d2f1',
    (32, None, 'guarded_compound_transport'): 'd6aff22207842dd00c303573dcc91418679da9c9b34a02d13501ab4f1a5c528e',
    (32, None, 'nested_guard_compound_shift'): '85984b91a189a09ab1e25676934f08f931bc730da6534e6ec76fc16937baf8fe',
    (32, None, 'nested_guard_compound_mac'): '73a51b5e540534a560e504d7ec17ab9bee0b7ae80610fbdb9b0df68f21d51084',
    (32, None, 'branched_compound_reuse'): 'dcbc00c23ce4c6996c01750d077babb846574970cb4422c64c942ad4cad3bb36',
    (32, None, 'compound_sum_transport_wrong_result'): '62f8c2b45becac986b1d9053f259c7cb62d91b53ebfbe5ace8e18046d6ede8f9',
    (32, None, 'compound_sum_transport_complement_wrong'): '0250ed2635b46b1c42d1f79f468bd4db65b3dd606abee937747aa6c145280282',
    (32, None, 'compound_product_transport_wrong_result'): 'f4be14f54e3af8f28a36b3e27b9171fe746e664924b061e6bf2aaf3dcb8ef571',
    (32, None, 'compound_product_transport_complement_wrong'): '89691edcc6dbd48b87f641690ad32dd21e25525f73eedcd0f15992ab28c9a1f2',
    (32, None, 'transitive_encoded_transport_wrong_result'): '82fb7455e5572bb5826bca235e61450ec81a2a1551755e8690a0a1628f7d3a1e',
    (32, None, 'transitive_encoded_transport_complement_wrong'): '00b7bef76175ba16fba62c8d706727f02611ec981d0581fd004a4d76c5e18ae0',
    (32, None, 'reversed_transitive_mac_shift_wrong_result'): '44ed04a0a317173d055ec5c9a21a8b313106d2b5ccbd2f4c7c743b6426501506',
    (32, None, 'reversed_transitive_mac_shift_complement_wrong'): 'de2b06a230a2eaca6898adce00bec41a752e035b87c8823b9367acfa196525b6',
    (32, None, 'guarded_compound_transport_wrong_result'): 'd3da16a35580438935ba0d0effccae904c053b15cda5be1354d55440ffb09d4c',
    (32, None, 'guarded_compound_transport_complement_wrong'): '51015485240883e319ff6d8d469ad83b954b1a72ca1cc06abe3867146e19e2f1',
    (32, None, 'nested_guard_compound_shift_wrong_result'): 'de33cf663edb358e9bed7abd959736f5551e66282c18346480d0448a3e63d039',
    (32, None, 'nested_guard_compound_shift_complement_wrong'): '5d5625e013fb762384a291e698a4cad22404c2de960d73b1ea6077ff00a0e79a',
    (32, None, 'nested_guard_compound_mac_wrong_result'): 'faad00c6fb464b9e33ccd8bfb93d21885833755871f0fe6a656987fe46b8739c',
    (32, None, 'nested_guard_compound_mac_complement_wrong'): '175f0e4c04eac95c1c2a829a3b786204ad89fde88accc8b9684abfbb2c9c3e47',
    (32, None, 'branched_compound_reuse_wrong_result'): 'bfa86af83d69928e8806a92d8e0c1090975a93b0c615f257c2d925aef2faff2a',
    (32, None, 'branched_compound_reuse_complement_wrong'): '82f88fe0dcb9ce6d7df1b472295bbcd1d7190d5ffd519b54418c037f0647a189',
    (32, None, 'guarded_fact_escape_wrong'): '445c661ca74cbb3bf1502437d9e3f067631838d3d3afda56e731e2b63cff47ca',
    (24, None, 'compound_product_transport'): '27b9cbe57e8bc8cb711ade8148d61f7415e8daa2f1d154fe58ffdb4e6767810f',
    (24, None, 'transitive_encoded_transport'): '467c89f469d876b43fa27987653b78e9cfdf41ae9477b387939e97ec4dd7b687',
    (24, None, 'reversed_transitive_mac_shift'): '2ddf29c984a5a000b2ec44748f6340ebe6be88f21d21189204ea4c043c68aa54',
    (24, None, 'nested_guard_compound_mac'): '759fbc4cd53a4e5b67a2555a41abc1be5bd8a0f673a754bbfac22439f7e97ebb',
    (24, 9413, 'transitive_encoded_transport__alpha_9413'): '6f7e5057e5f03b8c6581d91bfc79be416937d6b5d2684196ea79358452448404',
    (24, 9413, 'transitive_encoded_transport_wrong_result__alpha_9413'): 'b5bc3bd90bc0ca1a546cfe65bac4282a4df4dd5127ab5448a06a0353c92087d9',
    (24, 9413, 'transitive_encoded_transport_complement_wrong__alpha_9413'): '40dab0b64d62f93b862609be73ed1b51ec1d0205793aaa5bfe4e459561d87353',
}


class AuditFailure(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise AuditFailure(message)


def integer(value, label):
    require(type(value) is int and value >= 0, "malformed nonnegative integer: " + label)
    return value


def seconds(value, label):
    require(type(value) in (int, float) and math.isfinite(value) and value >= 0,
            "malformed duration: " + label)
    return value


def digest(data):
    return hashlib.sha256(data).hexdigest()


def row_key(row):
    return (row["width"], row["alpha_seed"], row["name"])


def matrix():
    """No caller-selected subset: all negatives and all diagnostics are mandatory."""
    names32 = list(POSITIVES)
    names32 += [name + suffix for name in POSITIVES
                for suffix in ("_wrong_result", "_complement_wrong")]
    names32 += ["guarded_fact_escape_wrong"]
    rows = []
    for width, seed, names in ((32, None, names32), (24, None, HELDOUT), (24, 9413, ALPHA)):
        generated = list(cases(width))
        generated_names = [doc["name"] for doc in generated]
        require(len(generated_names) == 25 and set(generated_names) == set(names32),
                "generator case matrix changed or contains duplicates")
        documents = {doc["name"]: doc for doc in generated}
        for name in names:
            doc = documents[name]
            role = "negative" if name not in POSITIVES else "limitation" if name in LIMITATIONS else "covered_positive"
            if seed is not None:
                doc = alpha_rename(doc, seed)
            row = {"name": doc["name"], "width": width, "alpha_seed": seed, "role": role}
            payload = (json.dumps(doc, indent=2) + "\n").encode()
            require(digest(payload) == INPUT_SHA256.get(row_key(row)),
                    "frozen generated input hash changed: " + str(row_key(row)))
            rows.append((row, payload))
    validate_matrix([row for row, _ in rows])
    return rows


def validate_matrix(rows):
    keys = [row_key(row) for row in rows]
    require(len(keys) == len(set(keys)), "duplicate matrix row")
    require(len(keys) == 32 and set(keys) == set(INPUT_SHA256), "missing or unexpected matrix row")
    for row in rows:
        base = row["name"].split("__alpha_")[0]
        role = "negative" if base not in POSITIVES else "limitation" if base in LIMITATIONS else "covered_positive"
        require(row.get("role") == role, "wrong matrix row role: " + row["name"])
    require(Counter(row["role"] for row in rows) ==
            Counter(negative=19, covered_positive=9, limitation=4), "matrix role counts changed")


def read_json(path):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, "duplicate JSON key: " + key)
            result[key] = value
        return result
    return json.loads(path.read_text(), object_pairs_hook=unique,
                      parse_constant=lambda value: (_ for _ in ()).throw(AuditFailure("invalid JSON number: " + value)))


def sidecar(directory, name):
    require(isinstance(name, str) and name and Path(name).name == name,
            "missing or unsafe sidecar path: " + str(name))
    path = directory / name
    require(path.is_file() and not path.is_symlink(), "missing sidecar: " + name)
    return path


def validate_aggregate(q):
    """Require live aggregate protocol structure, also for Unknown diagnostics.

    The existing successful-proof validators check the full conjunction/bundle
    contract. Unknown has no proof authority, but still needs a well-formed fresh
    attempt, exact ordered children, source validation and partial graph.
    """
    backend, children = q["backend"], q.get("children")
    require(isinstance(children, list) and children and all(isinstance(c, dict) for c in children),
            "empty or malformed aggregate children")
    require(not q.get("conjunctive_children"), "unexpected alternate aggregate children")
    coverage, cost = q.get("coverage"), q.get("cost")
    require(isinstance(coverage, dict) and isinstance(cost, dict), "missing aggregate coverage or budget")
    validation = q.get("original_source_validation")
    require(isinstance(validation, dict) and validation.get("includes_derived_context") is True,
            "missing original source validation")
    integer(validation.get("nodes"), "source validation nodes")
    integer(validation.get("work"), "source validation work")
    seconds(validation.get("seconds"), "source validation seconds")
    attempt = q.get("original_attempt")
    require(isinstance(attempt, dict) and attempt.get("name") == q.get("name")
            and attempt.get("backend") == "finite_bv" and attempt.get("status") == "unknown"
            and attempt.get("solver_result") == "unknown" and attempt.get("logical_expectation") == "unsat",
            "missing or invalid original attempt")
    if backend == "conjunctive_lemmas":
        expected = [q["name"] + "_lemma_" + format(i, "04d") for i in range(len(children))]
        require(len(children) <= 4096 and coverage.get("rule") == "exact-conjunction-introduction-v1"
                and coverage.get("complete") is True and coverage.get("ordered_names") == expected
                and [c.get("name") for c in children] == expected
                and coverage.get("same_full_precondition") is True
                and coverage.get("postconditions_used_as_assumptions") is False,
                "incomplete or reordered conjunctive coverage")
        require(cost.get("original_monolithic_result") == "unknown"
                and cost.get("whole_bundle_budget_is_not_a_single_query_budget") is True,
                "malformed conjunction budget mode")
        require(all(c.get("logical_expectation") == "unsat" for c in children), "changed conjunctive child expectation")
        require(q["status"] == ("passed" if all(c.get("status") == "passed" for c in children) else
                                "counterexample" if any(c.get("status") == "counterexample" for c in children) else "unknown"),
                "conjunction verdict does not match complete children")
        # Count identities rather than explicit replay pointers twice.
        leaves = {}
        def collect(child):
            if child.get("backend") in PRIMITIVES:
                leaves[child.get("evidence")] = child
            for key in ("original_attempt", "original_recheck"):
                if isinstance(child.get(key), dict):
                    collect(child[key])
            for nested in child.get("children", []):
                collect(nested)
        for child in children:
            collect(child)
        for field, measure in (("total_child_work", "work"), ("total_child_clauses", "clauses")):
            require(integer(cost.get(field), field) == sum(c.get("finite", {}).get(measure, 0) for c in leaves.values()),
                    "conjunction aggregate cost mismatch")
        if q["status"] == "passed":
            validate_conjunctive_obligation(q)
        return
    require(len(children) <= 512 and coverage.get("rule") == "fresh-acyclic-sequent-bundle-v1"
            and coverage.get("fresh_handles_only") is True
            and coverage.get("saved_reports_are_authority") is False and validation.get("complete") is True,
            "incomplete or stale proof bundle")
    for index, child in enumerate(children):
        require(child.get("name") == q["name"] + "_query_" + format(index, "04d")
                and child.get("backend") in PRIMITIVES and child.get("logical_expectation") == "unsat",
                "missing or reordered fresh bundle query")
    work = sum(c.get("finite", {}).get("work", 0) + c.get("emission_work", 0) for c in children)
    require(integer(cost.get("work_including_validation_and_derived_steps"), "bundle work") >= work,
            "bundle aggregate work mismatch")
    require(integer(cost.get("allocated_clauses"), "bundle clauses") == sum(c.get("finite", {}).get("clauses", 0) for c in children),
            "bundle aggregate clauses mismatch")
    graph = q.get("proof_graph")
    require(isinstance(graph, list) and len(graph) <= 512, "missing or malformed proof graph")
    arities = {"universal-typed-simultaneous-instantiation": 1, "guard-preserving-projection": 1,
               "exact-antecedent-modus-ponens": 2, "guarded-equality-with-original-fallback": 1,
               "exhaustive-guard-complement": 2}
    used = set()
    for index, node in enumerate(graph):
        require(isinstance(node, dict) and type(node.get("id")) is int and node["id"] == index,
                "missing or reordered proof graph node")
        dependencies = node.get("dependencies")
        require(isinstance(dependencies, list) and all(type(d) is int and 0 <= d < index for d in dependencies),
                "cyclic or missing proof graph dependency")
        require(node.get("statement") == q["name"] + "_statement_" + format(index, "04d") + ".smt2",
                "stale proof graph statement")
        rule = node.get("rule")
        if rule == "fresh-solver-unsat":
            query = node.get("query_index")
            require(not dependencies and type(query) is int and 0 <= query < len(children)
                    and query not in used and children[query].get("solver_result") == "unsat"
                    and children[query].get("status") == "passed", "missing fresh graph solver authority")
            used.add(query)
        elif rule == "exact-congruence-with-original-premise-retained":
            require(2 <= len(dependencies) <= 33, "missing congruence dependencies")
        else:
            require(rule in arities and len(dependencies) == arities[rule], "unknown or incomplete graph inference")
    root, replay = q.get("root"), q.get("original_recheck")
    if root is not None:
        require(type(root) is int and 0 <= root < len(graph) and q["status"] == "passed"
                and coverage.get("complete") is True and coverage.get("exact_original_sequent") is True,
                "invalid exact-original proof graph root")
    else:
        require(isinstance(replay, dict) and replay in children
                and replay.get("proof_label") == "original counterexample replay"
                and replay.get("status") == q["status"] and replay.get("solver_result") == q["solver_result"],
                "missing original replay after unproved auxiliary")
        if q["status"] == "unknown":
            require(coverage.get("complete") is False and coverage.get("exact_original_sequent") is False,
                    "Unknown diagnostic claims complete proof coverage")
    if q["status"] == "passed":
        validate_proof_bundle(q)


def audit_report(report, directory):
    """Inspect every actual primitive, including original attempts and replays."""
    require(isinstance(report, dict) and not report.get("error"), "error or malformed report")
    obligations = report.get("obligations")
    require(isinstance(obligations, list) and all(isinstance(q, dict) for q in obligations),
            "malformed obligations")
    names = [q.get("name") for q in obligations]
    require(len(names) == len(OBLIGATIONS) and set(names) == OBLIGATIONS,
            "missing, duplicate or unexpected top-level obligation")
    primitives, bundles, referenced, work_exhaustions = {}, [], set(), []

    def file(name):
        path = sidecar(directory, name)
        referenced.add(path.name)
        return path

    def visit(q):
        require(isinstance(q, dict), "malformed query")
        backend = q.get("backend")
        require(backend in PRIMITIVES | AGGREGATES, "forbidden or missing backend: " + str(backend))
        require(q.get("status") in {"passed", "counterexample", "unknown"}, "query failed or malformed status")
        require(q.get("solver_result") in {"sat", "unsat", "unknown"}, "malformed solver verdict")
        expected = q.get("logical_expectation")
        require(expected in ("sat", "unsat"), "missing logical expectation")
        expected_status = ("unknown" if q["solver_result"] == "unknown" else
                           "passed" if q["solver_result"] == expected else "counterexample")
        require(q["status"] == expected_status, "query status/verdict/expectation mismatch")
        require(seconds(q.get("z3_seconds"), "z3_seconds") == 0, "external solver timing is nonzero")
        for key in ("original_attempt", "original_recheck"):
            if q.get(key) is not None:
                visit(q[key])
        for key in ("children", "conjunctive_children"):
            children = q.get(key, [])
            require(isinstance(children, list), "malformed children")
            for child in children:
                visit(child)
        if q.get("evidence") is not None:
            file(q["evidence"])
        for step in q.get("proof_graph", []):
            file(step.get("statement"))
        plan = q.get("automatic_plan")
        if plan is not None:
            require(plan.get("saved_hints_used") is False and plan.get("source_names_used") is False,
                    "automatic plan used saved hints or source names")
        sharing = q.get("equality_sharing")
        if sharing is not None:
            require(sharing.get("saved_proofs_used") is False, "saved proofs used")
        if backend in AGGREGATES:
            validate_aggregate(q)
            cost = q.get("cost")
            require(isinstance(cost, dict), "missing bundle budget")
            prefix = "per_lemma_" if backend == "conjunctive_lemmas" else "per_query_"
            for field, expected in (("work_limit", LIMITS["max_work"]),
                                    ("clause_limit", LIMITS["max_clauses"]),
                                    ("timeout_ms", LIMITS["timeout_ms"])):
                value = integer(cost.get(prefix + field), prefix + field)
                require(value == expected, "changed per-primitive budget: " + prefix + field)
            coverage = q.get("coverage", {})
            if backend != "conjunctive_lemmas":
                require(coverage.get("saved_reports_are_authority") is False, "saved reports treated as authority")
            else:
                require(coverage.get("postconditions_used_as_assumptions") is False
                        and coverage.get("same_full_precondition") is True, "unsafe conjunctive coverage")
            if q["status"] == "passed":
                require(q["solver_result"] == "unsat" and coverage.get("complete") is True,
                        "incomplete successful bundle")
            if backend != "conjunctive_lemmas":
                require(cost.get("mode") == "independent_lemmas" and cost.get("whole_bundle_is_one_query") is False,
                        "changed proof-bundle budget mode")
                integer(cost.get("work_including_validation_and_derived_steps"), "bundle work")
                bundles.append(q)
            return
        evidence = file(q.get("evidence"))
        output = file(q.get("solver_output"))
        require(output.read_text().splitlines()[0] == q["solver_result"], "solver output verdict mismatch")
        timeouts = re.findall(r"\(set-option\s+:timeout\s+(\d+)\)", evidence.read_text())
        require(timeouts == [str(LIMITS["timeout_ms"])], "changed or missing emitted query timeout")
        integer(q.get("emission_work"), "emission work")
        kernel = q.get("kernel")
        require(isinstance(kernel, dict), "missing kernel diagnostics")
        if kernel.get("enabled") is True:
            file(kernel.get("residual"))
            kernel_path = file(evidence.with_suffix(".kernel.json").name)
            require(read_json(kernel_path) == kernel, "kernel sidecar mismatch")
        if backend == "structural_kernel":
            require(kernel.get("enabled") is True and kernel.get("closed") is True
                    and q["status"] == "passed" and q["solver_result"] == "unsat",
                    "unclosed structural primitive")
        else:
            finite = q.get("finite")
            require(isinstance(finite, dict), "missing finite diagnostics")
            require(read_json(file(q.get("finite_diagnostics"))) == finite, "finite sidecar mismatch")
            require(finite.get("solver_result") == q["solver_result"], "finite verdict mismatch")
            for field, limit in (("clauses", "max_clauses"), ("terms", "max_terms"),
                                 ("variables", "max_variables"), ("work", "max_work")):
                value = integer(finite.get(field), "finite " + field)
                if value > LIMITS[limit]:
                    exhausted = (field == "work" and value == LIMITS[limit] + 1
                                 and q["solver_result"] == "unknown" and q["status"] == "unknown"
                                 and finite.get("reason") == "finite solver work budget exhausted")
                    require(exhausted, "over-limit finite " + field)
                    work_exhaustions.append(evidence.name)
            if q["solver_result"] == "sat":
                require(finite.get("original_formula_validated") is True, "unvalidated finite SAT")
        identity = evidence.name
        if identity in primitives:
            require(primitives[identity] == q, "conflicting duplicate primitive")
        else:
            primitives[identity] = q

    for query in obligations:
        require(query.get("logical_expectation") == ("sat" if query["name"] in SAT_OBLIGATIONS else "unsat"),
                "changed top-level logical expectation")
        visit(query)
    engine = report.get("engine_summary")
    require(isinstance(engine, dict), "missing engine summary")
    require(integer(engine.get("z3_queries"), "z3 query count") == 0
            and seconds(engine.get("z3_seconds"), "z3 total seconds") == 0, "forbidden external solver used")
    require(integer(engine.get("not_run"), "not_run") == 0, "primitive was not run")
    finite = [q for q in primitives.values() if q["backend"] == "finite_bv"]
    kernel = [q for q in primitives.values() if q["backend"] == "structural_kernel"]
    total_work = sum(q["finite"]["work"] for q in finite)
    require(integer(engine.get("finite_queries"), "finite query count") == len(finite),
            "unaccounted finite primitive")
    require(integer(engine.get("custom_closed"), "kernel query count") == len(kernel),
            "unaccounted structural primitive")
    require(integer(engine.get("finite_total_work"), "finite total work") == total_work,
            "finite work accounting mismatch")
    actual_outputs = {p.name for p in directory.glob("*.out")}
    require(actual_outputs == {q["solver_output"] for q in primitives.values()}, "unaccounted solver output")
    require({p.name for p in directory.glob("*.finite.json")} == {q["finite_diagnostics"] for q in finite},
            "unaccounted finite sidecar")
    return {"primitive_queries": len(primitives), "finite_queries": len(finite),
            "structural_queries": len(kernel), "finite_work_all_originals_and_auxiliaries": total_work,
            "emission_work_all_primitives": sum(q["emission_work"] for q in primitives.values()),
            "bundle_work_including_validation_and_derived_steps": sum(
                q["cost"]["work_including_validation_and_derived_steps"] for q in bundles),
            "bundle_work_overlaps_primitive_work": True,
            "unknown_work_cap_plus_one_attempts": sorted(set(work_exhaustions)),
            "primitive_evidence": sorted(primitives), "referenced_sidecars": sorted(referenced)}


def validate_outcome(row, report, exit_code):
    status = report.get("status")
    require(report.get("name") == row["name"], "report case name mismatch")
    require(type(exit_code) is int and exit_code == {VERIFIED: 0, "counterexample": 1, "unknown": 3}.get(status),
            "checker error or exit/status mismatch")
    obligations = report["obligations"]
    microstep = next(q for q in obligations if q["name"] == "microstep_refinement")
    require(all(q["status"] == "passed" for q in obligations if q["name"] != "microstep_refinement"),
            "non-microstep obligation failed or unknown")
    if row["role"] == "negative":
        require(status == "counterexample" and microstep.get("status") == "counterexample"
                and microstep.get("backend") == "finite_bv" and microstep.get("solver_result") == "sat"
                and microstep.get("finite", {}).get("original_formula_validated") is True
                and microstep.get("evidence") == "microstep_refinement.smt2",
                "negative lacks direct validated original microstep counterexample; auxiliary SAT is insufficient")
        return "validated_original_counterexample"
    if row["role"] == "covered_positive":
        require(status == VERIFIED and microstep.get("status") == "passed"
                and microstep.get("solver_result") == "unsat", "covered positive is not verified")
        return "verified_coverage"
    require(status in (VERIFIED, "unknown"), "limitation positive produced counterexample or error")
    require(microstep.get("status") == ("passed" if status == VERIFIED else "unknown"),
            "limitation microstep/top-level mismatch")
    require(microstep.get("solver_result") == ("unsat" if status == VERIFIED else "unknown"),
            "limitation microstep verdict mismatch")
    return "limitation_improved_verified" if status == VERIFIED else "diagnostic_unknown"


def summarize(rows):
    validate_matrix(rows)
    errors = [str(row_key(row)) + ": " + error for row in rows for error in row.get("errors", [])]
    dispositions = Counter(row.get("disposition") for row in rows)
    require(all(row.get("disposition") in {"verified_coverage", "validated_original_counterexample",
                "diagnostic_unknown", "limitation_improved_verified", "failed"} for row in rows),
            "missing row disposition")
    if dispositions["verified_coverage"] != 9:
        errors.append("required verified coverage is not 9/9")
    if dispositions["validated_original_counterexample"] != 19:
        errors.append("required validated original counterexamples are not 19/19")
    if dispositions["diagnostic_unknown"] + dispositions["limitation_improved_verified"] != 4:
        errors.append("mandatory limitation diagnostics are not 4/4")
    return {"status": "failed" if errors else "passed_with_diagnostic_unknowns" if dispositions["diagnostic_unknown"] else "passed",
            "errors": errors, "matrix_rows": len(rows), "required_covered_positives": 9,
            "verified_covered_positives": dispositions["verified_coverage"], "negative_instances": 19,
            "validated_original_counterexamples": dispositions["validated_original_counterexample"],
            "mandatory_limitation_instances": 4, "diagnostic_unknowns": dispositions["diagnostic_unknown"],
            "limitation_improvements_verified": dispositions["limitation_improved_verified"],
            "total_positive_instances": 13,
            "total_verified_positive_instances": dispositions["verified_coverage"] + dispositions["limitation_improved_verified"]}


def tripwire(out):
    directory = out / "forbidden-solvers"
    directory.mkdir()
    marker = out / "FORBIDDEN_EXTERNAL_SOLVER_INVOKED"
    script = "#!/bin/sh\nprintf '%s\\n' \"$0 $*\" >> " + shlex.quote(str(marker)) + "\nexit 97\n"
    for name in ("FORBIDDEN_EXTERNAL_SOLVER", "z3", "cvc4", "cvc5", "yices", "yices-smt2", "boolector", "bitwuzla"):
        path = directory / name
        path.write_text(script)
        path.chmod(0o755)
    return directory / "FORBIDDEN_EXTERNAL_SOLVER", marker


def evaluation_fingerprints():
    """Include imported local validators and their loaded evaluation dependencies."""
    source = Path(__file__).resolve().parent
    checkout = source.parents[1]
    paths = {p.resolve() for p in source.iterdir() if p.suffix in (".py", ".md")}
    for module in tuple(sys.modules.values()):
        name = getattr(module, "__file__", None)
        if name:
            path = Path(name).resolve()
            if path.is_relative_to(checkout) and path.suffix == ".py" and path.is_file():
                paths.add(path)
    return {str(path.relative_to(checkout)): digest(path.read_bytes()) for path in sorted(paths)}


def run(checker, out, expected_sha256=None):
    generated = matrix()
    checker = checker.resolve(strict=True)
    checker_sha = digest(checker.read_bytes())
    require(expected_sha256 is None or checker_sha == expected_sha256, "checker SHA-256 mismatch")
    # Existing output trees are never accepted or reused, including partial runs.
    out.mkdir(parents=True, exist_ok=False)
    snapshot = out / "checker"
    shutil.copy2(checker, snapshot)
    require(digest(snapshot.read_bytes()) == checker_sha, "checker changed while snapshotting")
    forbidden, marker = tripwire(out)
    env = {k: v for k, v in os.environ.items() if not k.startswith("HWVERIFY_")}
    env.update(FLAGS)
    env["Z3_BIN"] = str(forbidden)
    env["PATH"] = str(forbidden.parent) + os.pathsep + env.get("PATH", "")
    source = Path(__file__).parent
    fingerprints = evaluation_fingerprints()
    summary = {"format_version": 1, "phase": "strict_equality_sharing_phase2",
               "checker_source": str(checker), "checker_snapshot": str(snapshot), "checker_sha256": checker_sha,
               "environment": FLAGS.copy(), "per_primitive_limits_unchanged": LIMITS.copy(),
               "budget_note": "Unknown work exhaustion may report exactly max_work+1 after the failing tick; successful leaves never may. Bundle totals overlap primitive totals and are not single-query limits.",
               "proof_authority": "Fresh live checker execution only; saved reports and sidecars are audit diagnostics, not independently checked certificates.",
               "all_loaded_local_evaluation_source_sha256": fingerprints,
               "evaluation_source_sha256": {p.name: digest(p.read_bytes()) for p in sorted(source.iterdir()) if p.suffix in (".py", ".md")},
               "rows": []}
    started = time.monotonic()
    for spec, payload in generated:
        row = dict(spec, errors=[], disposition="failed")
        case_dir = out / (f"width{spec['width']}-alpha{spec['alpha_seed']}" + "-" + spec["name"])
        case_dir.mkdir()
        input_file = case_dir / "input.json"
        input_file.write_bytes(payload)
        row.update(input_path=str(input_file), input_sha256=digest(payload))
        report_dir = case_dir / "output"
        command = [str(snapshot), str(input_file), "--out", str(report_dir), "--z3", str(forbidden)]
        row["command"] = command
        case_started = time.monotonic()
        try:
            process = subprocess.run(command, env=env, capture_output=True, timeout=180)
            (case_dir / "stdout.txt").write_bytes(process.stdout)
            (case_dir / "stderr.txt").write_bytes(process.stderr)
            row["exit_code"] = process.returncode
            require(not marker.exists(), "forbidden external solver tripwire invoked")
            require(digest(input_file.read_bytes()) == INPUT_SHA256[row_key(row)], "input changed during execution")
            report = read_json(report_dir / "report.json")
            row["status"] = report.get("status")
            row["accounting"] = audit_report(report, report_dir)
            row["disposition"] = validate_outcome(row, report, process.returncode)
        except (AuditFailure, OSError, ValueError, KeyError, TypeError, IndexError, subprocess.TimeoutExpired) as error:
            row["errors"].append(type(error).__name__ + ": " + str(error))
        row["wall_seconds"] = time.monotonic() - case_started
        row["artifacts_sha256"] = {str(p.relative_to(case_dir)): digest(p.read_bytes())
                                   for p in sorted(case_dir.rglob("*")) if p.is_file() and not p.is_symlink()}
        summary["rows"].append(row)
        (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(json.dumps({k: row[k] for k in ("name", "width", "alpha_seed", "disposition", "errors")}), flush=True)
    summary.update(summarize(summary["rows"]))
    if marker.exists():
        summary["errors"].append("forbidden external solver tripwire invoked")
    if digest(snapshot.read_bytes()) != checker_sha or digest(checker.read_bytes()) != checker_sha:
        summary["errors"].append("checker changed during execution")
    if evaluation_fingerprints() != fingerprints:
        summary["errors"].append("evaluation source changed during execution")
    if summary["errors"]:
        summary["status"] = "failed"
    summary["wall_seconds"] = time.monotonic() - started
    summary["forbidden_solver_invocations"] = marker.read_text() if marker.exists() else ""
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps({k: v for k, v in summary.items() if k not in ("rows", "evaluation_source_sha256")}, indent=2))
    return summary


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--checker", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--expected-checker-sha256", help="optional immutable binary identity pin")
    args = parser.parse_args(argv)
    output_existed = args.out.exists()
    try:
        summary = run(args.checker, args.out.resolve(), args.expected_checker_sha256)
    except (AuditFailure, OSError, ValueError, KeyError, TypeError) as error:
        print("strict equality-sharing CI failed: " + str(error), file=sys.stderr)
        # Preserve a machine-readable failure even before the first invocation.
        # Never overwrite an output tree that predated this call.
        if not output_existed:
            try:
                args.out.mkdir(parents=True, exist_ok=True)
                path = args.out / "summary.json"
                summary = read_json(path) if path.exists() else {"rows": []}
                summary["status"] = "failed"
                summary.setdefault("errors", []).append(type(error).__name__ + ": " + str(error))
                path.write_text(json.dumps(summary, indent=2) + "\n")
            except (OSError, ValueError, TypeError):
                pass  # An unwritable destination cannot hold an artifact.
        return 1
    return 1 if summary["errors"] else 0


if __name__ == "__main__":
    sys.exit(main())
