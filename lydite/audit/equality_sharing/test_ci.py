"""Fast hostile-report tests. Mocks never supply universal proof authority."""
from copy import deepcopy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from audit.equality_sharing import ci


def primitive(name, result="unsat", expectation="unsat", work=10):
    status = "unknown" if result == "unknown" else "passed" if result == expectation else "counterexample"
    return {"name": name, "backend": "finite_bv", "status": status, "solver_result": result,
            "logical_expectation": expectation, "z3_seconds": 0, "emission_work": 1,
            "evidence": name + ".smt2", "solver_output": name + ".out",
            "finite_diagnostics": name + ".finite.json", "kernel": {"enabled": False},
            "finite": {"solver_result": result, "work": work, "clauses": 1,
                       "terms": 1, "variables": 1, "original_formula_validated": result == "sat",
                       "reason": "finite solver work budget exhausted" if result == "unknown" else None}}


def report_for(row):
    result = "sat" if row["role"] == "negative" else "unknown" if row["role"] == "limitation" else "unsat"
    queries = [primitive(name, result if name == "microstep_refinement" else "sat" if name in ci.SAT_OBLIGATIONS else "unsat",
                         "sat" if name in ci.SAT_OBLIGATIONS else "unsat")
               for name in sorted(ci.OBLIGATIONS)]
    return {"name": row["name"], "status": {"sat": "counterexample", "unsat": ci.VERIFIED, "unknown": "unknown"}[result],
            "obligations": queries, "engine_summary": {"finite_queries": 7, "custom_closed": 0,
             "finite_total_work": 70, "not_run": 0, "z3_queries": 0, "z3_seconds": 0}}


def descendants(report):
    def visit(q):
        yield q
        for key in ("original_attempt", "original_recheck"):
            if q.get(key):
                yield from visit(q[key])
        for key in ("children", "conjunctive_children"):
            for child in q.get(key, []):
                yield from visit(child)
    for query in report["obligations"]:
        yield from visit(query)


def write_sidecars(report, directory):
    directory.mkdir(exist_ok=True)
    for q in descendants(report):
        if q.get("evidence"):
            (directory / q["evidence"]).write_text("(set-option :timeout 10000)\n(assert false)\n(check-sat)\n")
        for node in q.get("proof_graph", []):
            (directory / node["statement"]).write_text("; diagnostic statement\n")
        if q.get("solver_output"):
            (directory / q["solver_output"]).write_text(q["solver_result"] + "\n")
        if q.get("finite_diagnostics"):
            (directory / q["finite_diagnostics"]).write_text(json.dumps(q["finite"]))
    (directory / "report.json").write_text(json.dumps(report))


def bundle(name="microstep_refinement_lemma_0000"):
    child = primitive(name + "_query_0000")
    original = primitive(name, "unknown", work=100_000_001)
    for field, suffix in (("evidence", ".smt2"), ("solver_output", ".out"), ("finite_diagnostics", ".finite.json")):
        original[field] = name + "_uncut" + suffix
    return {"name": name, "backend": "checked_proof_bundle", "status": "passed",
            "solver_result": "unsat", "logical_expectation": "unsat", "z3_seconds": 0,
            "children": [child], "original_attempt": original, "original_recheck": None,
            "original_source_validation": {"complete": True, "includes_derived_context": True,
                                           "nodes": 1, "work": 1, "seconds": 0},
            "coverage": {"rule": "fresh-acyclic-sequent-bundle-v1", "complete": True,
                         "exact_original_sequent": True, "fresh_handles_only": True,
                         "saved_reports_are_authority": False},
            "cost": {"mode": "independent_lemmas", "per_query_work_limit": 100_000_000,
                     "per_query_clause_limit": 1_000_000, "per_query_timeout_ms": 10_000,
                     "whole_bundle_is_one_query": False, "allocated_clauses": 1,
                     "work_including_validation_and_derived_steps": 12},
            "root": 0, "proof_graph": [{"id": 0, "dependencies": [], "rule": "fresh-solver-unsat",
                        "query_index": 0, "statement": name + "_statement_0000.smt2"}]}


def conjunction(child=None):
    child = child or primitive("microstep_refinement_lemma_0000")
    return {"name": "microstep_refinement", "backend": "conjunctive_lemmas", "status": "passed",
            "solver_result": "unsat", "logical_expectation": "unsat", "z3_seconds": 0,
            "children": [child], "original_attempt": primitive("microstep_refinement", "unknown"),
            "original_source_validation": {"includes_derived_context": True, "nodes": 1, "work": 1, "seconds": 0},
            "coverage": {"rule": "exact-conjunction-introduction-v1", "complete": True,
                         "ordered_names": [child["name"]], "same_full_precondition": True,
                         "postconditions_used_as_assumptions": False},
            "cost": {"per_lemma_work_limit": 100_000_000, "per_lemma_clause_limit": 1_000_000,
                     "per_lemma_timeout_ms": 10_000, "original_monolithic_result": "unknown",
                     "whole_bundle_budget_is_not_a_single_query_budget": True,
                     "total_child_work": 10, "total_child_clauses": 1}}


class StrictMatrixCI(unittest.TestCase):
    def setUp(self):
        self.rows = [row for row, _ in ci.matrix()]
        self.positive = next(r for r in self.rows if r["role"] == "covered_positive")
        self.negative = next(r for r in self.rows if r["role"] == "negative")
        self.limitation = next(r for r in self.rows if r["role"] == "limitation")
        self.temp = tempfile.TemporaryDirectory()
        self.directory = Path(self.temp.name)
        self.addCleanup(self.temp.cleanup)

    def audit(self, report):
        write_sidecars(report, self.directory)
        return ci.audit_report(report, self.directory)

    def test_imported_validator_sources_are_fingerprinted(self):
        fingerprints = ci.evaluation_fingerprints()
        self.assertIn("audit/equality_sharing/ci.py", fingerprints)
        self.assertIn("audit/veryl_scaling/rv32i_pipeline.py", fingerprints)
        self.assertIn("audit/veryl_scaling/cpu_memory_contract.py", fingerprints)

    def test_exact_matrix_and_frozen_hashes(self):
        ci.validate_matrix(self.rows)
        self.assertEqual(len(self.rows), 32)
        self.assertEqual(sum(r["role"] == "negative" for r in self.rows), 19)
        with patch.dict(ci.INPUT_SHA256, {ci.row_key(self.rows[0]): "0" * 64}):
            with self.assertRaisesRegex(ci.AuditFailure, "frozen generated input hash changed"):
                ci.matrix()

    def test_missing_matrix_row_and_every_negative_omission_fail(self):
        for index in range(len(self.rows)):
            with self.subTest(row=self.rows[index]):
                with self.assertRaisesRegex(ci.AuditFailure, "missing or unexpected"):
                    ci.validate_matrix(self.rows[:index] + self.rows[index + 1:])

    def test_duplicate_row_fails_even_with_correct_count(self):
        rows = deepcopy(self.rows)
        rows[-1] = rows[0]
        with self.assertRaisesRegex(ci.AuditFailure, "duplicate"):
            ci.validate_matrix(rows)

    def test_role_tampering_fails(self):
        rows = deepcopy(self.rows)
        rows[-1]["role"] = "limitation"
        with self.assertRaisesRegex(ci.AuditFailure, "wrong matrix row role"):
            ci.validate_matrix(rows)

    def test_mock_covered_positive_and_negative(self):
        for row in (self.positive, self.negative, self.limitation):
            report = report_for(row)
            self.audit(report)
            ci.validate_outcome(row, report, {ci.VERIFIED: 0, "counterexample": 1, "unknown": 3}[report["status"]])

    def test_empty_or_incomplete_aggregates_rejected(self):
        for factory in (bundle, conjunction):
            ci.validate_aggregate(factory())
            for field in ("children", "original_attempt", "original_source_validation", "coverage"):
                query = factory()
                query.pop(field)
                with self.subTest(factory=factory.__name__, field=field), self.assertRaises(ci.AuditFailure):
                    ci.validate_aggregate(query)
            query = factory()
            query["children"] = []
            with self.assertRaisesRegex(ci.AuditFailure, "empty or malformed"):
                ci.validate_aggregate(query)

    def test_conjunction_order_rule_and_child_result_checked(self):
        for mutate in (lambda q: q["coverage"].update(ordered_names=[]),
                       lambda q: q["coverage"].update(rule="fake"),
                       lambda q: q["children"][0].update(name="stale"),
                       lambda q: q["children"][0].update(status="unknown", solver_result="unknown")):
            query = conjunction()
            mutate(query)
            with self.assertRaises(ci.AuditFailure):
                ci.validate_aggregate(query)

    def test_graph_authority_root_and_dependencies_checked(self):
        for mutate in (lambda q: q.update(root=True), lambda q: q.update(root=1),
                       lambda q: q.update(proof_graph=[]),
                       lambda q: q["proof_graph"][0].update(dependencies=[0]),
                       lambda q: q["proof_graph"][0].update(rule="saved-proof"),
                       lambda q: q["proof_graph"][0].update(query_index=True),
                       lambda q: q["proof_graph"][0].update(statement="stale.smt2"),
                       lambda q: q["coverage"].update(exact_original_sequent=False),
                       lambda q: q["coverage"].update(saved_reports_are_authority=True)):
            query = bundle()
            mutate(query)
            with self.assertRaises(ci.AuditFailure):
                ci.validate_aggregate(query)

    def test_covered_bundle_retains_exhausted_original_attempt(self):
        report = report_for(self.positive)
        query = bundle("microstep_refinement")
        report["obligations"] = [query if q["name"] == "microstep_refinement" else q for q in report["obligations"]]
        report["engine_summary"].update(finite_queries=8, finite_total_work=100_000_071)
        facts = self.audit(report)
        self.assertEqual(facts["primitive_queries"], 8)
        self.assertEqual(facts["unknown_work_cap_plus_one_attempts"], ["microstep_refinement_uncut.smt2"])
        self.assertEqual(ci.validate_outcome(self.positive, report, 0), "verified_coverage")

    def test_unknown_bundle_requires_replay_and_stays_diagnostic(self):
        query = bundle("microstep_refinement")
        replay = primitive("microstep_refinement_query_0001", "unknown")
        replay["proof_label"] = "original counterexample replay"
        query.update(status="unknown", solver_result="unknown", root=None, original_recheck=replay)
        query["children"].append(replay)
        query["coverage"].update(complete=False, exact_original_sequent=False)
        query["cost"].update(allocated_clauses=2, work_including_validation_and_derived_steps=22)
        ci.validate_aggregate(query)
        report = report_for(self.limitation)
        report["obligations"] = [query if q["name"] == "microstep_refinement" else q for q in report["obligations"]]
        report["engine_summary"].update(finite_queries=9, finite_total_work=100_000_081)
        facts = self.audit(report)
        self.assertEqual(facts["primitive_queries"], 9)  # replay pointer does not double-count
        self.assertEqual(ci.validate_outcome(self.limitation, report, 3), "diagnostic_unknown")
        query["coverage"]["complete"] = True
        with self.assertRaisesRegex(ci.AuditFailure, "Unknown diagnostic"):
            ci.validate_aggregate(query)

    def test_startup_failure_has_uploadable_summary(self):
        output = self.directory / "bad-startup"
        with patch("sys.stderr"):
            result = ci.main(["--checker", str(self.directory / "missing"), "--out", str(output)])
        self.assertEqual(result, 1)
        self.assertEqual(json.loads((output / "summary.json").read_text())["status"], "failed")

    def test_forged_success_with_sat_microstep_fails(self):
        report = report_for(self.positive)
        q = next(q for q in report["obligations"] if q["name"] == "microstep_refinement")
        q["solver_result"] = "sat"
        with self.assertRaisesRegex(ci.AuditFailure, "status/verdict/expectation"):
            self.audit(report)
        with self.assertRaisesRegex(ci.AuditFailure, "covered positive"):
            ci.validate_outcome(self.positive, report, 0)

    def test_unknown_covered_positive_fails(self):
        report = report_for(self.positive)
        report["status"] = "unknown"
        with self.assertRaisesRegex(ci.AuditFailure, "covered positive"):
            ci.validate_outcome(self.positive, report, 3)

    def test_limitation_improvement_is_separate(self):
        report = report_for(self.limitation)
        report["status"] = ci.VERIFIED
        next(q for q in report["obligations"] if q["name"] == "microstep_refinement").update(status="passed", solver_result="unsat")
        self.assertEqual(ci.validate_outcome(self.limitation, report, 0), "limitation_improved_verified")
        report["status"] = "counterexample"
        with self.assertRaises(ci.AuditFailure):
            ci.validate_outcome(self.limitation, report, 1)

    def test_unvalidated_sat_fails(self):
        report = report_for(self.negative)
        q = next(q for q in report["obligations"] if q["name"] == "microstep_refinement")
        q["finite"]["original_formula_validated"] = False
        with self.assertRaisesRegex(ci.AuditFailure, "unvalidated finite SAT"):
            self.audit(report)
        with self.assertRaisesRegex(ci.AuditFailure, "direct validated original"):
            ci.validate_outcome(self.negative, report, 1)

    def test_auxiliary_sat_never_counts_as_original_negative(self):
        report = report_for(self.negative)
        q = next(q for q in report["obligations"] if q["name"] == "microstep_refinement")
        q.update(backend="checked_proof_bundle", children=[primitive("auxiliary", "sat")])
        # Even a forged top-level counterexample and finite marker do not let an
        # auxiliary SAT pass as a direct microstep witness.
        with self.assertRaisesRegex(ci.AuditFailure, "auxiliary SAT is insufficient"):
            ci.validate_outcome(self.negative, report, 1)

    def test_forbidden_backend_anywhere_fails(self):
        for backend in ("z3", "external", None):
            report = report_for(self.positive)
            report["obligations"][0]["original_attempt"] = dict(primitive("hidden"), backend=backend)
            with self.assertRaisesRegex(ci.AuditFailure, "forbidden or missing backend"):
                self.audit(report)

    def test_external_engine_counts_fail(self):
        report = report_for(self.positive)
        report["engine_summary"]["z3_queries"] = 1
        with self.assertRaisesRegex(ci.AuditFailure, "forbidden external"):
            self.audit(report)

    def test_malformed_or_over_limit_work_fails(self):
        for value in ("10", True, -1, 100_000_001, 100_000_002):
            report = report_for(self.positive)
            report["obligations"][0]["finite"]["work"] = value
            with self.subTest(value=value), self.assertRaises(ci.AuditFailure):
                self.audit(report)

    def test_exact_unknown_exhaustion_plus_one_counted_and_only_that(self):
        report = report_for(self.limitation)
        q = next(q for q in report["obligations"] if q["name"] == "microstep_refinement")
        q["finite"]["work"] = 100_000_001
        report["engine_summary"]["finite_total_work"] = 100_000_061
        facts = self.audit(report)
        self.assertEqual(facts["finite_work_all_originals_and_auxiliaries"], 100_000_061)
        self.assertEqual(facts["unknown_work_cap_plus_one_attempts"], ["microstep_refinement.smt2"])
        for value, reason in ((100_000_002, "finite solver work budget exhausted"),
                              (100_000_001, "some other reason")):
            q["finite"].update(work=value, reason=reason)
            with self.assertRaisesRegex(ci.AuditFailure, "over-limit finite work"):
                self.audit(report)

    def test_clauses_terms_variables_over_limit_fail(self):
        for field, cap in (("clauses", 1_000_000), ("terms", 100_000), ("variables", 200_000)):
            report = report_for(self.positive)
            report["obligations"][0]["finite"][field] = cap + 1
            with self.assertRaisesRegex(ci.AuditFailure, "over-limit finite"):
                self.audit(report)

    def test_bundle_budget_malformed_or_changed_fails(self):
        for value in (None, "100000000", True, 100_000_001, 99_999_999):
            report = report_for(self.positive)
            q = report["obligations"][0]
            q.update(backend="checked_proof_bundle", cost={"per_query_work_limit": value})
            with self.assertRaises(ci.AuditFailure):
                self.audit(report)

    def test_missing_and_mismatched_sidecars_fail(self):
        report = report_for(self.positive)
        write_sidecars(report, self.directory)
        target = self.directory / report["obligations"][0]["finite_diagnostics"]
        target.unlink()
        with self.assertRaisesRegex(ci.AuditFailure, "missing sidecar"):
            ci.audit_report(report, self.directory)
        target.write_text('{}')
        with self.assertRaisesRegex(ci.AuditFailure, "finite sidecar mismatch"):
            ci.audit_report(report, self.directory)

    def test_changed_emitted_timeout_fails(self):
        report = report_for(self.positive)
        write_sidecars(report, self.directory)
        (self.directory / report["obligations"][0]["evidence"]).write_text('(set-option :timeout 20000)')
        with self.assertRaisesRegex(ci.AuditFailure, "emitted query timeout"):
            ci.audit_report(report, self.directory)

    def test_unaccounted_primitive_or_work_fails(self):
        for field in ("finite_queries", "finite_total_work", "custom_closed"):
            report = report_for(self.positive)
            report["engine_summary"][field] += 1
            with self.assertRaises(ci.AuditFailure):
                self.audit(report)

    def test_missing_top_level_obligation_fails(self):
        report = report_for(self.positive)
        report["obligations"].pop()
        with self.assertRaisesRegex(ci.AuditFailure, "top-level obligation"):
            self.audit(report)

    def test_tripwire_really_fires(self):
        executable, marker = ci.tripwire(self.directory)
        result = subprocess.run([str(executable), "a", "b"], capture_output=True)
        self.assertEqual(result.returncode, 97)
        self.assertIn("a b", marker.read_text())

    def test_summary_never_claims_thirteen_verified(self):
        rows = deepcopy(self.rows)
        for row in rows:
            row.update(errors=[], disposition={"covered_positive": "verified_coverage", "negative": "validated_original_counterexample",
                                               "limitation": "diagnostic_unknown"}[row["role"]])
        summary = ci.summarize(rows)
        self.assertEqual(summary["total_verified_positive_instances"], 9)
        self.assertEqual(summary["diagnostic_unknowns"], 4)
        self.assertEqual(summary["validated_original_counterexamples"], 19)
        rows[-1]["disposition"] = "failed"
        self.assertEqual(ci.summarize(rows)["status"], "failed")

    def test_fresh_runner_does_not_skip_negatives_after_failure(self):
        checker = self.directory / "fake-checker"
        checker.write_text("mocked binary")
        observed = []
        output = self.directory / "fresh"
        def invoke(command, **kwargs):
            payload = json.loads(Path(command[1]).read_text())
            row = next(row for row in self.rows if row["name"] == payload["name"] and row["width"] == next(
                sort["bv"] for sort in payload["inputs"].values() if isinstance(sort, dict)))
            observed.append(row)
            self.assertEqual({k: v for k, v in kwargs["env"].items() if k.startswith("LYDITE_")}, ci.FLAGS)
            report = report_for(row)
            if len(observed) == 1:
                report["status"] = "unknown"  # failure must not skip any later row
            directory = Path(command[command.index("--out") + 1])
            write_sidecars(report, directory)
            code = {ci.VERIFIED: 0, "counterexample": 1, "unknown": 3}[report["status"]]
            return subprocess.CompletedProcess(command, code, b"", b"")
        with patch.object(ci.subprocess, "run", side_effect=invoke), patch.dict(os.environ, {"LYDITE_KERNEL": "off"}), patch("builtins.print"):
            summary = ci.run(checker, output)
        self.assertEqual(len(observed), 32)
        self.assertEqual(sum(row["role"] == "negative" for row in observed), 19)
        self.assertEqual(summary["status"], "failed")
        self.assertTrue((output / "summary.json").is_file())
        with self.assertRaises(FileExistsError):
            ci.run(checker, output)


if __name__ == "__main__":
    unittest.main()
