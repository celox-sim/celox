"""CI must be red for missing cases, weakened predicates, UNKNOWN or fallback."""
import copy
import unittest
from catalog import validate_golden, validate_results


def manifest():
    return {'schema': 'veryl-fv-coverage-v1', 'source_hashes': {'fixture.rs': 'sourcehash'},
            'cases': [{'case': 'fixture', 'disposition': 'expected_pass', 'assertions': 2,
                       'action_sha256': 'predicatehash', 'reason': 'supported', 'frames': 3, 'assertion_frames': 2}]}


def result():
    return {'case': 'fixture', 'status': 'passed', 'assertions': 2, 'frames': 3,
            'normal': {'backends': ['finite_bv', 'structural_kernel'], 'cases': [{'assertion_frames': 2}]}}


class CoverageGateTests(unittest.TestCase):
    def test_exact_manifest_and_result_pass(self):
        validate_golden(manifest(), manifest())
        validate_results(manifest(), [result()])

    def test_removed_case_fails(self):
        changed = manifest(); changed['cases'] = []
        with self.assertRaises(ValueError): validate_golden(changed, manifest())

    def test_new_skip_fails(self):
        changed = manifest(); changed['cases'][0]['disposition'] = 'unsupported_frontend'
        with self.assertRaises(ValueError): validate_golden(changed, manifest())

    def test_predicate_loss_or_change_fails(self):
        for field, value in [('assertions', 1), ('action_sha256', 'weaker')]:
            changed = manifest(); changed['cases'][0][field] = value
            with self.assertRaises(ValueError): validate_golden(changed, manifest())

    def test_changed_source_fails(self):
        changed = manifest(); changed['source_hashes']['fixture.rs'] = 'changed'
        with self.assertRaises(ValueError): validate_golden(changed, manifest())

    def test_unknown_and_fail_never_pass(self):
        for status in ('unknown', 'failed', 'unsupported_fv_subset'):
            changed = result(); changed['status'] = status
            with self.assertRaises(ValueError): validate_results(manifest(), [changed])

    def test_missing_duplicate_and_extra_results_fail(self):
        other = result(); other['case'] = 'not-expected'
        for rows in ([], [result(), result()], [result(), other]):
            with self.assertRaises(ValueError): validate_results(manifest(), rows)

    def test_unexpected_backends_fail(self):
        for backend in ('z3', 'unknown_backend', 'smt_fallback'):
            changed = result(); changed['normal']['backends'] = [backend]
            with self.assertRaises(ValueError): validate_results(manifest(), [changed])

    def test_solver_assertion_frame_loss_fails(self):
        changed = result(); changed['normal']['cases'][0]['assertion_frames'] = 1
        with self.assertRaises(ValueError): validate_results(manifest(), [changed])

    def test_empty_expected_pass_set_fails(self):
        changed = manifest(); changed['cases'] = []
        with self.assertRaises(ValueError): validate_golden(changed, changed)


if __name__ == '__main__':
    unittest.main()
