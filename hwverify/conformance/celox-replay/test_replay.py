"""Failure controls for saved identities and simulator comparison, no golden oracle."""
import copy
import unittest
from run import PIN, compare_simulation, validate_saved

class ReplayControls(unittest.TestCase):
    def test_stale_identity_extra_fields_and_bad_stimuli_are_rejected(self):
        case = {'case': 'counter', 'goal': 'safety', 'identity': {'source': 'original'}}
        good = {'version': 1, **case, 'inputs': [{'rst': True}]}
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

if __name__ == '__main__':
    unittest.main()
