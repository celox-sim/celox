import copy
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from finite_trace_check import lower_fixed_forall, run

BINARY = os.environ.get('HWVERIFY_BIN', str(Path(__file__).resolve().parents[2] / 'target/release/hwverify-rs'))


def fixture(constrained=True):
    return {'version': 4, 'kind': 'specification', 'specs': {'Top': {
        'inputs': {}, 'outputs': {'x': {'bv': 2}}, 'state': {}, 'init': True,
        'invariant': True,
        'operations': {'emit': ['eq', 'no.x', ['bv', 2, 0]] if constrained else True},
        'examples': {'case': {'quantifiers': [], 'expect': 'forall', 'initial': {},
                             'trace': [{'operation': 'emit', 'inputs': {}, 'observe': {'x': ['bv', 2, 0]}}]}}
    }}, 'compositions': {}}


class FiniteTraceTests(unittest.TestCase):
    def check(self, doc):
        with tempfile.TemporaryDirectory() as td:
            d = Path(td); tripwire = d / 'z3'; marker = d / 'called'
            tripwire.write_text('#!/bin/sh\nprintf called >> "' + str(marker) + '"\nexit 77\n')
            tripwire.chmod(0o700)
            result = run(doc, BINARY, d / 'out', tripwire)
            self.assertFalse(marker.exists(), 'runtime invoked external solver')
            self.assertFalse(any('z3' in b for b in result['backends']))
            return result

    def test_feasible_universal_result(self):
        r = self.check(fixture())
        self.assertEqual(r['status'], 'passed')
        self.assertIs(r['cases'][0]['feasible'], True)

    def test_wrong_expected_value_is_not_an_assumption(self):
        d = fixture(); d['specs']['Top']['examples']['case']['trace'][0]['observe']['x'] = ['bv', 2, 1]
        r = self.check(d)
        self.assertEqual(r['status'], 'failed')
        self.assertIs(r['cases'][0]['feasible'], True)
        self.assertIs(r['cases'][0]['violation_queries'][0]['sat'], True)

    def test_infeasible_trace_does_not_pass(self):
        d = fixture(); d['specs']['Top']['init'] = False
        r = self.check(d)
        self.assertEqual(r['status'], 'failed')
        self.assertIs(r['cases'][0]['feasible'], False)

    def test_exists_matching_value_is_not_forall(self):
        r = self.check(fixture(False))
        self.assertEqual(r['status'], 'failed')
        self.assertIs(r['cases'][0]['feasible'], True)

    def test_other_frame_expectations_never_constrain_execution(self):
        d = fixture(False); ex = d['specs']['Top']['examples']['case']
        ex['trace'].append({'operation': 'emit', 'inputs': {}, 'observe': {'x': ['bv', 2, 1]}})
        qf, manifest = lower_fixed_forall(d)
        for ex in qf['specs']['Top']['examples'].values():
            self.assertTrue(all(not t['observe'] for t in ex['trace']))
            self.assertLessEqual(sum('ensure' in t for t in ex['trace']), 1)
        r = self.check(d)
        self.assertEqual(r['status'], 'failed')
        self.assertEqual(len(r['cases'][0]['violation_queries']), 2)

    def test_ensure_is_checked(self):
        d = fixture(); f = d['specs']['Top']['examples']['case']['trace'][0]
        f['observe'] = {}; f['ensure'] = ['ne', 'o.x', ['bv', 2, 0]]
        self.assertEqual(self.check(d)['status'], 'failed')

    def test_empty_expectations_check_only_feasibility(self):
        d = fixture(); d['specs']['Top']['examples']['case']['trace'][0]['observe'] = {}
        r = self.check(d)
        self.assertEqual(r['status'], 'passed')
        self.assertEqual(r['cases'][0]['assertion_frames'], 0)

    def test_unsupported_memory_is_unknown_without_fallback(self):
        d = fixture(); d['specs']['Top']['state'] = {'m': {'mem': [1, 1]}}
        self.assertEqual(self.check(d)['status'], 'unknown')

    def test_outer_quantifiers_rejected(self):
        d = fixture(); d['specs']['Top']['examples']['case']['quantifiers'] = [{'kind': 'forall', 'variables': {'a': {'bv': 2}}}]
        with self.assertRaises(ValueError): lower_fixed_forall(d)

    def test_compositions_rejected(self):
        d = fixture(); d['compositions'] = {'C': {}}
        with self.assertRaises(ValueError): lower_fixed_forall(d)

    def test_nonforall_rejected(self):
        d = fixture(); d['specs']['Top']['examples']['case']['expect'] = 'exists'
        with self.assertRaises(ValueError): lower_fixed_forall(d)

    def test_stale_solver_report_cannot_be_reused(self):
        with tempfile.TemporaryDirectory() as td:
            p = Path(td) / 'solver'
            p.mkdir()
            (p / 'report.json').write_text('{"status": "passed"}')
            with self.assertRaises(FileExistsError):
                run(fixture(), BINARY, td)

    def test_source_document_not_mutated(self):
        d = fixture(); original = copy.deepcopy(d)
        lower_fixed_forall(d)
        self.assertEqual(d, original)


if __name__ == '__main__': unittest.main()
