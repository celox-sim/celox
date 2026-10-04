"""Audit-accounting regressions, separate from the solver's proof authority."""
import unittest
from audit.equality_sharing.run import evidence


def sat(name):
    return {"name": name, "evidence": name + ".smt2", "backend": "finite_bv",
            "status": "counterexample", "solver_result": "sat",
            "finite": {"original_formula_validated": True, "work": 13}}


class EvidenceAccounting(unittest.TestCase):
    def test_auxiliary_sat_is_not_original_counterexample(self):
        bundle = {"backend": "checked_proof_bundle", "status": "unknown", "children": [sat("auxiliary")]}
        result = evidence(bundle)
        self.assertEqual(result["primitive_queries"], 1)
        self.assertFalse(result["reported_counterexamples_original_validated"])

    def test_original_replay_counted_once_and_validated(self):
        replay = sat("original_replay")
        bundle = {"backend": "checked_proof_bundle", "status": "counterexample", "children": [replay],
                  "original_recheck": replay, "cost": {"work_including_validation_and_derived_steps": 20}}
        result = evidence(bundle)
        self.assertEqual(result["primitive_queries"], 1)
        self.assertEqual(result["finite_work_including_original_attempts"], 13)
        self.assertEqual(result["bundle_work_including_validation_and_derived_steps"], 20)
        self.assertTrue(result["bundle_work_overlaps_primitive_work"])
        self.assertTrue(result["reported_counterexamples_original_validated"])

    def test_missing_replay_validation_never_counts_as_rejection(self):
        replay = sat("replay")
        replay["finite"]["original_formula_validated"] = False
        bundle = {"backend": "checked_proof_bundle", "status": "counterexample", "children": [replay], "original_recheck": replay}
        self.assertFalse(evidence(bundle)["reported_counterexamples_original_validated"])

    def test_conjunctive_original_leaf_is_validated(self):
        query = {"backend": "conjunctive_lemmas", "status": "counterexample", "children": [sat("child")]}
        self.assertTrue(evidence(query)["reported_counterexamples_original_validated"])


if __name__ == "__main__": unittest.main()
