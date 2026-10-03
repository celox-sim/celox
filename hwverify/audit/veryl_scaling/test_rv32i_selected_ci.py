"""Runner plumbing tests with mocked proofs: these are not proof evidence."""
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch, MagicMock

from audit.veryl_scaling import rv32i_selected_ci as gate


class SelectedGateTests(unittest.TestCase):
    def exercise(self, failure=None):
        with tempfile.TemporaryDirectory() as folder, patch.dict(os.environ, {}, clear=True), patch.object(gate, 'capture_provenance', return_value={}), patch.object(gate, 'execute_program_checks'):
            out = Path(folder) / 'fresh'
            session = MagicMock()
            session.prove_memory.side_effect = [object(), object(), object()]
            session.prove_cpu.side_effect = failure
            session.compose.return_value = {'mock_only': True}
            with patch.object(gate.refinement, 'ProofSession', return_value=session), \
                 patch.object(gate.latency, 'run') as latency, \
                 patch.object(gate.selected, 'check_mutations') as mutations, \
                 patch.object(gate.memory, 'check_mutations') as memory:
                if failure:
                    with self.assertRaises(type(failure)):
                        gate.run(out, '/mock/checker')
                else:
                    gate.run(out, '/mock/checker')
                self.assertEqual(latency.call_count, 2)
                mutations.assert_called_once()
                memory.assert_called_once()
                summary = json.loads((out / 'run-status.json').read_text())
                self.assertNotIn('HWVERIFY_SOLVER', os.environ)
                return summary, session.compose.call_count, (out / 'composition.json').exists()

    def test_mock_unknown_cannot_compose_or_pass(self):
        summary, calls, exists = self.exercise(ValueError('microstep Unknown'))
        self.assertEqual(summary['status'], 'failed')
        self.assertEqual(summary['architectural_refinement'], 'not_verified')
        self.assertEqual(summary['composition'], 'not_issued')
        self.assertEqual(calls, 0)
        self.assertFalse(exists)

    def test_mock_success_requires_three_live_compositions(self):
        summary, calls, exists = self.exercise()
        self.assertEqual(summary['status'], 'passed')
        self.assertEqual(calls, 3)
        self.assertTrue(exists)

    def test_component_failure_cannot_reach_cpu_or_composition(self):
        for component in ('latency', 'mutation'):
            with tempfile.TemporaryDirectory() as folder, patch.dict(os.environ, {}, clear=True), patch.object(gate, 'capture_provenance', return_value={}), patch.object(gate, 'execute_program_checks'):
                out = Path(folder) / 'fresh'
                session = MagicMock()
                with patch.object(gate.refinement, 'ProofSession', return_value=session), \
                     patch.object(gate.latency, 'run') as latency, \
                     patch.object(gate.selected, 'check_mutations') as mutations:
                    (latency if component == 'latency' else mutations).side_effect = ValueError('component Unknown')
                    with self.assertRaises(ValueError):
                        gate.run(out, '/mock/checker')
                    session.prove_cpu.assert_not_called()
                    session.compose.assert_not_called()
                    self.assertFalse((out / 'composition.json').exists())
                    self.assertEqual(json.loads((out / 'run-status.json').read_text())['status'], 'failed')

    def test_memory_or_late_composition_failure_cannot_pass(self):
        for failure in ('memory-proof', 'memory-mutant', 'second-composition'):
            with tempfile.TemporaryDirectory() as folder, patch.dict(os.environ, {}, clear=True), \
                 patch.object(gate, 'capture_provenance', return_value={}), \
                 patch.object(gate, 'execute_program_checks'):
                out = Path(folder) / 'fresh'
                session = MagicMock()
                with patch.object(gate.refinement, 'ProofSession', return_value=session), \
                     patch.object(gate.latency, 'run'), \
                     patch.object(gate.selected, 'check_mutations'), \
                     patch.object(gate.memory, 'check_mutations') as memory:
                    if failure == 'memory-proof':
                        session.prove_memory.side_effect = ValueError('memory Unknown')
                    elif failure == 'memory-mutant':
                        memory.side_effect = ValueError('mutation not replayed')
                    else:
                        session.compose.side_effect = [{'mock_only': True}, ValueError('stale handle')]
                    with self.assertRaises(ValueError):
                        gate.run(out, '/mock/checker')
                    self.assertFalse((out / 'composition.json').exists())
                    self.assertEqual(json.loads((out / 'run-status.json').read_text())['status'], 'failed')

    def test_gate_provenance_rejects_source_change(self):
        import hashlib
        with tempfile.TemporaryDirectory() as folder:
            source = Path(folder) / 'source'
            source.write_text('first')
            seals = {str(source): hashlib.sha256(source.read_bytes()).hexdigest()}
            gate.check_provenance(seals)
            source.write_text('second')
            with self.assertRaises(ValueError):
                gate.check_provenance(seals)

    def test_nondefault_or_external_solver_rejected_before_output(self):
        for env in ({'HWVERIFY_SOLVER': 'z3'}, {'HWVERIFY_KERNEL': 'off'},
                    {'HWVERIFY_CONJUNCTIVE_LEMMAS': '1'}):
            with tempfile.TemporaryDirectory() as folder, patch.dict(os.environ, env, clear=True):
                out = Path(folder) / 'fresh'
                with self.assertRaises(ValueError):
                    gate.run(out, '/mock/checker')
                self.assertFalse(out.exists())

    def test_output_inside_source_repository_is_rejected(self):
        out = gate.selected.ROOT / 'conformance/veryl-symbolic/work/forbidden-ci-output'
        with self.assertRaisesRegex(ValueError, 'outside the source repository'):
            gate.run(out, '/mock/checker')
        self.assertFalse(out.exists())

    def test_existing_directory_is_never_overwritten(self):
        with tempfile.TemporaryDirectory() as folder, patch.dict(os.environ, {}, clear=True), patch.object(gate, 'capture_provenance', return_value={}), patch.object(gate, 'execute_program_checks'):
            out = Path(folder)
            marker = out / 'keep'
            marker.write_text('keep')
            with self.assertRaises(FileExistsError):
                gate.run(out, '/mock/checker')
            self.assertEqual(marker.read_text(), 'keep')


if __name__ == '__main__':
    unittest.main()
