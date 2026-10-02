import copy
import importlib.util
import pathlib
import unittest

spec = importlib.util.spec_from_file_location('symbolic_run', pathlib.Path(__file__).resolve().parents[1]/'run.py')
run = importlib.util.module_from_spec(spec)
spec.loader.exec_module(run)


class GateTests(unittest.TestCase):
    def report(self):
        return {'status': 'stuttering_refinement_verified', 'engine_summary': {'z3_queries': 0},
                'obligations': [{'name': name, 'status': 'passed', 'solver_result': run.EXPECTED_RESULTS[name],
                                 'finite': {'original_formula_validated': True}}
                                for name in sorted(run.OBLIGATIONS)]}

    def test_complete_pass(self):
        self.assertEqual(run.validate_report(self.report(), None), [])

    def test_missing_duplicate_unknown_external_fail(self):
        for fault in ('missing', 'duplicate', 'unknown', 'external', 'bad_status'):
            with self.subTest(fault=fault):
                r = self.report()
                if fault == 'missing': r['obligations'].pop()
                if fault == 'duplicate': r['obligations'][0] = r['obligations'][1]
                if fault == 'unknown': r['obligations'][0]['solver_result'] = 'unknown'
                if fault == 'external': r['engine_summary']['z3_queries'] = 1
                if fault == 'bad_status': r['status'] = 'unknown'
                with self.assertRaises(RuntimeError): run.validate_report(r, None)

    def test_mutant_requires_validated_original_sat(self):
        r = self.report()
        r['status'] = 'counterexample'
        q = next(o for o in r['obligations'] if o['name'] == 'microstep_refinement')
        q.update(status='failed', solver_result='sat', finite={'original_formula_validated': True})
        self.assertEqual(len(run.validate_report(r, 'wrong_add')), 1)
        for value in (False, None, 1):
            bad = copy.deepcopy(r)
            next(o for o in bad['obligations'] if o['name'] == 'microstep_refinement')['finite']['original_formula_validated'] = value
            with self.assertRaises(RuntimeError): run.validate_report(bad, 'wrong_add')

    def test_each_obligation_requires_its_actual_logical_result(self):
        for name in run.OBLIGATIONS:
            for value in (None, 'invalid', 'sat' if run.EXPECTED_RESULTS[name] == 'unsat' else 'unsat'):
                with self.subTest(name=name, value=value):
                    r = self.report()
                    q = next(o for o in r['obligations'] if o['name'] == name)
                    if value is None: q.pop('solver_result')
                    else: q['solver_result'] = value
                    with self.assertRaises(RuntimeError): run.validate_report(r, None)
        for name in run.EXISTENCE_OBLIGATIONS:
            for value in (False, None, 1):
                r = self.report()
                q = next(o for o in r['obligations'] if o['name'] == name)
                q['finite']['original_formula_validated'] = value
                with self.assertRaises(RuntimeError): run.validate_report(r, None)

    def test_state_reference_detection(self):
        self.assertTrue(run.references(['ite', 'i.stall', 's.r0', ['bv', 32, 0]], 's.'))
        self.assertFalse(run.references(['bv', 32, 0], 's.'))

if __name__ == '__main__': unittest.main()
