"""CI-plumbing/adversarial tests; mocked reports are never proof authority."""
import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from audit.automatic_proof import ci
from audit.automatic_proof.generate import cases


class StrictGate(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def fixture(self, negative=False):
        name = 'wrapped_multiplier_complement_wrong' if negative else 'wrapped_multiplier'
        doc = next(x for x in cases(4) if x['name'] == name)
        input_path = self.root / (name + '.json')
        input_path.write_text(json.dumps(doc, indent=2) + '\n')
        folder = self.root / name
        folder.mkdir()
        values = {'i.' + k: ({'sort': 'Bool', 'value': False} if sort == 'bool' else
                            {'sort': 'Bv', 'width': sort['bv'], 'value': 0})
                  for k, sort in doc['inputs'].items()}
        values.update({'spec.out': {'sort': 'Bv', 'width': 4, 'value': 0},
                       'impl.out': {'sort': 'Bv', 'width': 4, 'value': 0},
                       'binding_before': {'sort': 'Bool', 'value': True},
                       'binding_after': {'sort': 'Bool', 'value': not negative},
                       'commit': {'sort': 'Bool', 'value': True}})
        queries = []
        for item in sorted(ci.OBLIGATIONS):
            bad = negative and item == 'microstep_refinement'
            queries.append({'name': item, 'status': 'counterexample' if bad else 'passed',
                'backend': 'finite_bv', 'solver_result': 'sat' if bad else ci.EXPECTED_RESULTS[item],
                'logical_expectation': ci.EXPECTED_RESULTS[item],
                'z3_seconds': 0, 'evidence': item + '.smt2',
                'solver_output': item + '.out', 'finite_diagnostics': item + '.finite.json',
                'finite': {'work': 1, 'clauses': 1, 'terms': 1, 'variables': 1, 'solver_result': 'sat' if bad else ci.EXPECTED_RESULTS[item],
                           'reason': None, 'original_formula_validated': True,
                           'context_values': copy.deepcopy(values)}})
        status = 'counterexample' if negative else 'stuttering_refinement_verified'
        report = {'name': name, 'status': status, 'engine_summary': {'z3_queries': 0, 'z3_seconds': 0},
                  'obligations': queries}
        group = {'cases': [{'name': name, 'expected': 'counterexample' if negative else 'verified',
                            'input_sha256': ci.sha(input_path)}]}
        summary = {'checker_sha256': 'fixed', 'automatic': True, 'per_query_limits_unchanged': True,
            'environment': {'LYDITE_SOLVER': 'finite', 'LYDITE_CONJUNCTIVE_LEMMAS': '1',
                            'LYDITE_AUTOMATIC_PROOFS': 'independent_lemmas'},
            'rows': [{'name': name, 'status': status, 'exit_code': int(negative)}]}
        self.save(folder, report)
        return doc, folder, report, group, summary

    def save(self, folder, report):
        (folder / 'report.json').write_text(json.dumps(report))
        for q in report['obligations']:
            if q.get('evidence'):
                (folder / q['evidence']).write_text('(set-option :timeout 10000)\n; mocked gate-plumbing fixture, not a proof\n')
            if q.get('solver_output'):
                (folder / q['solver_output']).write_text(q['solver_result'] + '\n')
            if q.get('finite_diagnostics'):
                (folder / q['finite_diagnostics']).write_text(json.dumps(q['finite']))

    def validate(self, group, summary):
        return ci.validate_group(self.root, group, summary, 'fixed')

    def test_fixed_matrix_has_29_cases(self):
        m = ci.matrix()
        self.assertEqual(sum(len(g['cases']) for g in m['groups']), 29)

    def test_mock_positive_plumbing(self):
        _, _, _, g, s = self.fixture()
        self.assertEqual(len(self.validate(g, s)), 1)

    def test_negative_independent_original_replay(self):
        _, _, _, g, s = self.fixture(True)
        self.assertTrue(self.validate(g, s)[0]['original_formula_replayed_by_python'])

    def test_unknown_positive_rejected(self):
        _, f, r, g, s = self.fixture()
        r['status'] = s['rows'][0]['status'] = 'unknown'
        self.save(f, r)
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_missing_obligation_rejected(self):
        _, f, r, g, s = self.fixture()
        r['obligations'].pop()
        self.save(f, r)
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_missing_case_rejected(self):
        _, _, _, g, s = self.fixture()
        s['rows'] = []
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_duplicate_case_rejected(self):
        _, _, _, g, s = self.fixture()
        s['rows'] *= 2
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_missing_model_replay_rejected(self):
        _, f, r, g, s = self.fixture(True)
        next(q for q in r['obligations'] if q['name'] == 'microstep_refinement')['finite']['original_formula_validated'] = False
        self.save(f, r)
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_wrong_complement_witness_rejected(self):
        _, f, r, g, s = self.fixture(True)
        next(q for q in r['obligations'] if q['name'] == 'microstep_refinement')['finite']['context_values']['i.pick']['value'] = True
        self.save(f, r)
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_sat_output_with_model_is_accepted(self):
        _, f, _, g, s = self.fixture(True)
        (f / 'microstep_refinement.out').write_text('sat\n; finite backend witness\n(model)\n')
        self.assertTrue(self.validate(g, s)[0]['original_formula_replayed_by_python'])

    def test_non_sat_output_rejected(self):
        _, f, _, g, s = self.fixture(True)
        (f / 'microstep_refinement.out').write_text('unknown\nsat\n')
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_tripwire_marker_rejected(self):
        marker = self.root / 'tripwire'
        ci.check_tripwire(marker)
        marker.write_text('unexpected invocation')
        with self.assertRaises(ValueError): ci.check_tripwire(marker)

    def test_missing_model_sidecar_rejected(self):
        _, f, _, g, s = self.fixture(True)
        (f / 'microstep_refinement.finite.json').unlink()
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_modified_model_sidecar_rejected(self):
        _, f, _, g, s = self.fixture(True)
        (f / 'microstep_refinement.finite.json').write_text('{}')
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_missing_original_formula_rejected(self):
        _, f, _, g, s = self.fixture(True)
        (f / 'microstep_refinement.smt2').unlink()
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_wrong_exit_code_rejected(self):
        _, _, _, g, s = self.fixture(True)
        s['rows'][0]['exit_code'] = 0
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_external_query_rejected(self):
        _, f, r, g, s = self.fixture()
        r['engine_summary']['z3_queries'] = 1
        self.save(f, r)
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_solver_override_rejected(self):
        _, _, _, g, s = self.fixture()
        s['environment']['LYDITE_MAX_WORK'] = '200000000'
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_input_hash_change_rejected(self):
        _, _, _, g, s = self.fixture()
        (self.root / 'wrapped_multiplier.json').write_text('{}')
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_successful_overbudget_query_rejected(self):
        _, f, r, g, s = self.fixture()
        r['obligations'][0]['finite']['work'] = 100000001
        self.save(f, r)
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_one_tick_unknown_overshoot_is_preserved(self):
        _, f, r, _, _ = self.fixture()
        q = r['obligations'][0]
        q['status'] = q['solver_result'] = 'unknown'
        q['finite'].update(work=100000001, solver_result='unknown',
                           reason='finite solver work budget exhausted')
        self.save(f, r)
        ci.query_evidence(f, q)
        q['finite']['work'] = 100000002
        self.save(f, r)
        with self.assertRaises(ValueError): ci.query_evidence(f, q)

    def test_passed_sat_preservation_rejected(self):
        _, f, r, g, s = self.fixture()
        q = next(q for q in r['obligations'] if q['name'] == 'microstep_refinement')
        q['solver_result'] = q['finite']['solver_result'] = 'sat'
        self.save(f, r)
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_changed_logical_expectation_rejected(self):
        _, f, r, g, s = self.fixture()
        q = next(q for q in r['obligations'] if q['name'] == 'microstep_refinement')
        q['logical_expectation'] = q['solver_result'] = q['finite']['solver_result'] = 'sat'
        self.save(f, r)
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_recursive_external_backend_rejected(self):
        _, f, r, _, _ = self.fixture()
        child = copy.deepcopy(r['obligations'][0])
        child['backend'] = 'z3'
        r['obligations'][0]['original_attempt'] = child
        with self.assertRaises(ValueError): ci.query_evidence(f, r['obligations'][0])

    def test_unknown_cannot_expand_clause_budget(self):
        _, f, r, _, _ = self.fixture()
        q = r['obligations'][0]
        q['status'] = q['solver_result'] = 'unknown'
        q['finite'].update(work=1, clauses=1000001, solver_result='unknown', reason='anything')
        self.save(f, r)
        with self.assertRaises(ValueError): ci.query_evidence(f, q)

    def test_overshoot_requires_exact_exhaustion_reason(self):
        _, f, r, _, _ = self.fixture()
        q = r['obligations'][0]
        q['status'] = q['solver_result'] = 'unknown'
        q['finite'].update(work=100000001, solver_result='unknown', reason='anything')
        self.save(f, r)
        with self.assertRaises(ValueError): ci.query_evidence(f, q)

    def test_emitted_timeout_change_rejected(self):
        _, f, _, g, s = self.fixture()
        (f / 'microstep_refinement.smt2').write_text('(set-option :timeout 20000)\n')
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_term_budget_change_rejected(self):
        _, f, r, g, s = self.fixture()
        r['obligations'][0]['finite']['terms'] = 100001
        self.save(f, r)
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_variable_budget_change_rejected(self):
        _, f, r, g, s = self.fixture()
        r['obligations'][0]['finite']['variables'] = 200001
        self.save(f, r)
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_primitive_output_mismatch_rejected(self):
        _, f, _, g, s = self.fixture()
        (f / 'reset_binding.out').write_text('unknown\n')
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_source_change_rejected(self):
        p = self.root / 'source'
        p.write_text('before')
        sealed = {str(p): ci.sha(p)}
        p.write_text('after')
        with self.assertRaises(ValueError): ci.check_seal(sealed)

    def test_unsafe_evidence_path_rejected(self):
        with self.assertRaises(ValueError): ci.relative_file(self.root, '../outside')

    def test_incomplete_conjunction_rejected(self):
        _, f, r, g, s = self.fixture()
        q = next(q for q in r['obligations'] if q['name'] == 'microstep_refinement')
        q.update(backend='conjunctive_lemmas', children=[], coverage={'complete': False})
        self.save(f, r)
        with self.assertRaises(ValueError): self.validate(g, s)

    def test_tripwire_or_missing_run_fails_and_records_status(self):
        # Abort before a solver run while checking fail-closed final status writing.
        out = self.root / 'failed-gate'
        with patch.object(ci, 'matrix', side_effect=ValueError('missing fixed coverage')):
            with self.assertRaises(ValueError): ci.gate(out, self.root / 'no-checker')
        self.assertEqual(json.loads((out / 'run-status.json').read_text())['status'], 'failed')


if __name__ == '__main__':
    unittest.main()
