"""Compiler-free regressions for checked immutable-memory substitution glue."""
import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from audit.veryl_scaling import cpu_memory_reuse as reuse
from audit.veryl_scaling.test_cpu_memory_contract import cpu_stub, memory_stub


class AbstractionTests(unittest.TestCase):
    def test_capacity_absent_and_complete_state_relation(self):
        cpu = cpu_stub()
        before = copy.deepcopy(cpu)
        doc = reuse.abstract_composition(cpu)
        self.assertEqual(cpu, before)
        for side in ('spec', 'impl'):
            self.assertEqual(doc[side]['state']['rom'], {'mem': [6, 41]})
            self.assertEqual(doc[side]['state']['data'], {'mem': [6, 32]})
            self.assertEqual(doc[side]['next']['rom'], 's.rom')
            self.assertEqual(doc[side]['reset']['rom'], 'i.seed_rom')
            self.assertFalse(any(k.startswith(('rom0', 'data0')) for k in doc[side]['state']))
        self.assertNotIn('i.', json.dumps(doc['binding']))
        self.assertNotIn('seed_rom0', json.dumps(doc))
        self.assertIn('["eq", "spec.rom", "impl.rom"]', json.dumps(doc['binding']))
        self.assertIn('["read", "impl.rom", "impl.w_pc"]', json.dumps(doc['binding']))
        self.assertIn('["read", "impl.data", ["extract", 5, 0, "impl.w_ir"]]', json.dumps(doc['binding']))

    def test_nested_dead_cycles_and_response_feedback_rejected(self):
        for changes in ({'a': 'w.a'}, {'a': 'w.b', 'b': 'w.a'}, {'a': 'w.missing'}):
            cpu = cpu_stub()
            cpu['wires'].update(changes)
            with self.assertRaises(ValueError):
                reuse.abstract_composition(cpu)
        cpu = cpu_stub()
        cpu['outputs']['imem_address'] = ['extract', 5, 0, 'i.imem_response']
        with self.assertRaises(ValueError):
            reuse.abstract_composition(cpu)

    def test_faults_modify_real_connections(self):
        cpu = cpu_stub()
        cpu['next']['d_ir'] = 'i.imem_response'
        cpu['next']['w_result'] = 'i.dmem_response'
        normal = reuse.abstract_composition(cpu)
        for fault in reuse.c.COMPOSITION_FAULTS:
            self.assertNotEqual(normal['impl']['next'], reuse.abstract_composition(cpu, fault)['impl']['next'])


class SessionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name)
        self.checker = self.path / 'checker'
        self.checker.write_text('test checker')
        self.frontend = self.path / 'frontend'
        self.frontend.write_text('test frontend')
        self.lifter = self.path / 'lifter'
        self.lifter.write_text('test lifter')
        self.session = reuse.ProofSession(self.checker)

    def imported(self, kind, count=None):
        def compile_mock(source, kind, count, out, frontend, lifter):
            out.mkdir()
            (out / 'source.veryl').write_text(source)
            machine = cpu_stub() if kind == 'cpu' else memory_stub(count)
            (out / 'machine.json').write_text(json.dumps(machine))
            return machine
        with patch.object(reuse.c, 'compile_machine', side_effect=compile_mock):
            return self.session.import_component(kind, kind, count, self.path / kind, self.frontend, self.lifter)

    def prove_pair(self):
        cpu = self.imported('cpu')
        memory = self.imported('memory', 4)
        cpu_report = {'status': 'stuttering_refinement_verified', 'engine_summary': {'z3_queries': 0},
            'obligations': [{'name': name, 'status': 'passed', 'solver_result': result,
                            'backend': 'finite_bv', 'finite': {'original_formula_validated': True}}
                           for name, result in reuse.c.RUNNER.EXPECTED_RESULTS.items()]}
        from audit.veryl_scaling.test_cpu_memory_contract import ReportGateTests
        memory_report = ReportGateTests().report()
        prototypes = {e['expect']: e for e in memory_report['examples']}
        memory_report['examples'] = [{**copy.deepcopy(prototypes[e['expect']]), 'example': name}
            for name, e in reuse.c.memory_examples(4).items()]
        with patch.object(reuse.c, 'execute', return_value=(cpu_report, 0.0)), \
                patch.object(reuse.c, 'check_document', return_value={'status': 'spec_examples_and_binding_verified'}):
            cp, _ = self.session.prove('cpu', cpu, 'cpu', self.path / 'cpu-proof')
        with patch.object(reuse.c, 'execute', return_value=(memory_report, 0.0)):
            mp, _ = self.session.prove('memory', memory, 'memory', self.path / 'memory-proof', 4)
        return cp, mp, cpu, memory

    def test_verified_handles_reused_and_capacity_machine_bound(self):
        cp, mp, cpu, memory = self.prove_pair()
        result = self.session.compose(cp, mp, cpu, memory, 'cpu', 'memory', 4)
        self.assertFalse(result['concrete_memory_expanded_in_cpu_proof'])
        with self.assertRaises(ValueError):
            self.session.compose(cp, mp, cpu, memory, 'cpu', 'memory', 16)
        self.session._proofs[mp]['dependencies'] = [cp]
        with self.assertRaisesRegex(ValueError, 'cyclic'):
            self.session.compose(cp, mp, cpu, memory, 'cpu', 'memory', 4)

    def test_missing_port_contract_prerequisite_rejected(self):
        cp, mp, cpu, memory = self.prove_pair()
        del self.session._proofs[cp]['cpu_port_contract']
        with self.assertRaisesRegex(ValueError, 'port-contract prerequisite'):
            self.session.compose(cp, mp, cpu, memory, 'cpu', 'memory', 4)

    def test_checker_changed_after_proof_rejected(self):
        cp, mp, cpu, memory = self.prove_pair()
        self.checker.write_text('replacement checker')
        with self.assertRaisesRegex(ValueError, 'stale'):
            self.session.compose(cp, mp, cpu, memory, 'cpu', 'memory', 4)

    def test_failed_unknown_proof_does_not_issue_handle(self):
        cpu = self.imported('cpu')
        with patch.object(reuse.c, 'execute', return_value=({'status': 'unknown', 'obligations': []}, 0.0)), \
                patch.object(reuse.c, 'check_document', return_value={'status': 'spec_examples_and_binding_verified'}):
            with self.assertRaises(RuntimeError):
                self.session.prove('cpu', cpu, 'cpu', self.path / 'failed-proof')
        self.assertEqual(self.session._proofs, {})

    def test_unimported_machine_cannot_receive_proof(self):
        with self.assertRaisesRegex(ValueError, 'checked import'):
            self.session.prove('cpu', cpu_stub(), 'cpu', self.path / 'proof')

    def test_import_binds_machine_source_tools_and_sidecars(self):
        cpu = self.imported('cpu')
        self.session._checked_import(cpu, 'cpu', 'cpu', None)
        with self.assertRaises(ValueError):
            self.session._checked_import(copy.deepcopy(cpu), 'cpu', 'cpu', None)
        with self.assertRaises(ValueError):
            self.session._checked_import(cpu, 'wrong source', 'cpu', None)
        cpu['next']['pc'] = ['bv', 6, 1]
        with self.assertRaises(ValueError):
            self.session._checked_import(cpu, 'cpu', 'cpu', None)

    def test_changed_importer_rejected(self):
        cpu = self.imported('cpu')
        self.lifter.write_text('changed')
        with self.assertRaisesRegex(ValueError, 'stale'):
            self.session._checked_import(cpu, 'cpu', 'cpu', None)

    def test_saved_labels_and_cycles_cannot_compose(self):
        cpu, memory = cpu_stub(), memory_stub(4)
        for cpu_handle, memory_handle in (({'status': 'passed'}, {}), ('forged', 'forged2'), (object(), object())):
            with self.assertRaisesRegex(ValueError, 'proof handle'):
                self.session.compose(cpu_handle, memory_handle, cpu, memory, 'cpu', 'memory', 4)
        handle = object()
        with self.assertRaisesRegex(ValueError, 'cyclic'):
            self.session.compose(handle, handle, cpu, memory, 'cpu', 'memory', 4)

    def test_wrong_connections_rejected_before_proof_lookup(self):
        with self.assertRaisesRegex(ValueError, 'connection graph'):
            self.session.compose(object(), object(), cpu_stub(), memory_stub(4), 'cpu', 'memory', 4,
                                 {'cpu.imem_response': 'memory.dmem_response'})


if __name__ == '__main__':
    unittest.main()
