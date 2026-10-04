"""Failure controls for saved identities and simulator comparison, no golden oracle."""
import copy
import unittest
import json
from pathlib import Path
import tempfile
from project import PIN, compare_simulation, validate_saved, load_manifest, mappings, prepare_project, write
from run import fixture_project, cli

class ReplayControls(unittest.TestCase):
    def test_stale_identity_extra_fields_and_bad_stimuli_are_rejected(self):
        case = {'name': 'counter', 'goal': 'safety', 'depth': 2, 'identity': {'source': 'original'}}
        good = {'version': 2, 'project': 'counter', 'goal': 'safety', 'depth': 2, 'identity': case['identity'], 'inputs': [{'rst': True}]}
        validate_saved(good, case)
        for key, value in [('identity', {'source': 'changed'}), ('goal', 'positive_cover'), ('inputs', []), ('extra', 'ignored')]:
            bad = copy.deepcopy(good)
            bad[key] = value
            with self.assertRaises(ValueError):
                validate_saved(bad, case)

    def test_divergence_is_not_a_reproduced_property_failure(self):
        expected = [{'edge': 0, 'state_after': {'count': {'value': 0}}, 'state_before': None, 'controls': None},
                    {'edge': 1, 'state_after': {'count': {'value': 2}}, 'state_before': {'count': {'value': 0}}, 'controls': None}]
        actual = {'status': 'simulated', 'celox_revision': PIN, 'trace': [
            {'edge': 0, 'before': {}, 'after': {'count': '0'}},
            {'edge': 1, 'before': {'count': '0'}, 'after': {'count': '2'}}]}
        self.assertEqual(compare_simulation(expected, actual, ['count'])['status'], 'simulation_matches_validated_trace')
        actual['trace'][1]['after']['count'] = '1'
        self.assertEqual(compare_simulation(expected, actual, ['count'])['status'], 'simulator_divergence')
        actual['trace'].pop()
        self.assertEqual(compare_simulation(expected, actual, ['count'])['status'], 'simulator_divergence')

class ExternalProjects(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='external lydite project ')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.path = fixture_project('response_correct', self.root / 'user')
        self.original = json.loads(self.path.read_text())

    def test_manifest_rejects_unknown_fields_expressions_duplicates_and_escape(self):
        variants = []
        for key, value in [('depth', True), ('depth', 33), ('sources', []), ('sources', ['../outside.veryl']), ('sources', ['rtl/unit.veryl', 'rtl/./unit.veryl']), ('four_state', True), ('property', 'cover')]:
            m = copy.deepcopy(self.original); m[key] = value; variants.append(m)
        for key, name, value in [('inputs', 'request', {'signal': 'request_pin', 'expr': True}), ('state', 'busy', 'ticks_pin'), ('state', 'busy', 'u.busy')]:
            m = copy.deepcopy(self.original); m[key][name] = value; variants.append(m)
        for m in variants:
            write(self.path, m)
            with self.assertRaises(ValueError): load_manifest(self.path)
        self.path.write_text(json.dumps(self.original))
        (self.path.parent / 'escape.veryl').symlink_to('/etc/hosts')
        m = copy.deepcopy(self.original); m['sources'] = ['escape.veryl']; write(self.path, m)
        with self.assertRaises(ValueError): load_manifest(self.path)

    def test_duplicate_manifest_fields_and_relational_nondeterminism(self):
        self.path.write_text('{"version":1,"version":1}')
        with self.assertRaises(ValueError): load_manifest(self.path)
        path = fixture_project('counter_wrong_update', self.root / 'relational')
        spec = path.parent / 'contracts/model.lyd'
        spec.write_text(spec.read_text().replace('n.value == s.value + 1u4', 'true'))
        # All next values are allowed: do not choose one relational witness as a golden.
        saved = self.root / 'should-not-exist.json'
        result = cli('search', path, '--out', self.root / 'nondeterministic', '--save-regression', saved)
        self.assertEqual(result['status'], 'bounded_no_failure')
        self.assertFalse(saved.exists())

    def test_compiled_mapping_rejects_missing_inputs_widths_arrays_and_comb_state(self):
        out = self.root / 'compiled'; out.mkdir()
        prepare_project(self.path, out)
        doc = json.loads((out / 'canonical.json').read_text())
        compiled = json.loads((out / 'compiled.json').read_text())
        for key, name, value in [('inputs', 'request', 'busy_pin'), ('state', 'busy', 'accept_pin'), ('state', 'count', 'ticks_pin'), ('signals', 'accept', {'signal': 'unknown_pin', 'type': 'bool'})]:
            m = copy.deepcopy(self.original); m[key][name] = value
            with self.assertRaises(ValueError): mappings(m, doc, compiled)
        m = copy.deepcopy(self.original); m['inputs'].pop('stall')
        with self.assertRaises(ValueError): mappings(m, doc, compiled)
        c = copy.deepcopy(compiled)
        next(s for s in c['signals'] if s['path'] == ['request_pin'])['metadata']['array_dims'] = [1]
        with self.assertRaises(ValueError): mappings(self.original, doc, c)
        bad_path = self.path.parent / 'bad.json'; write(bad_path, m)
        result = cli('search', bad_path, '--out', self.root / 'rejected', allowed=(2,))
        self.assertEqual(result['status'], 'project_error')

    def test_public_cli_multiple_sources_active_high_and_portable_replay(self):
        path = fixture_project('counter_wrong_update', self.root / 'external')
        manifest = json.loads(path.read_text())
        rtl = path.parent / 'rtl/unit.veryl'
        rtl.write_text(rtl.read_text().replace('if !rst_n_pin', 'if rst_n_pin'))
        manifest['reset']['active'] = 1
        manifest['sources'].append('rtl/helper.veryl')
        (path.parent / 'rtl/helper.veryl').write_text('module Unused (a: input bit, b: output bit) { assign b = a; }')
        write(path, manifest)
        saved = self.root / 'failure.json'
        result = cli('search', path, '--out', self.root / 'search', '--save-regression', saved)
        self.assertEqual(result['status'], 'reset_reachable_failure')
        self.assertTrue(saved.is_file())
        result = cli('replay', path, saved, '--out', self.root / 'replay')
        self.assertEqual(result['simulation'], 'simulation_matches_validated_trace')
        rtl.write_text(rtl.read_text() + '\n// changed source identity\n')
        rejected = cli('replay', path, saved, '--out', self.root / 'stale', allowed=(2,))
        self.assertEqual(rejected['status'], 'project_error')
        self.assertIn('identity', rejected['error'])

if __name__ == '__main__':
    unittest.main()
