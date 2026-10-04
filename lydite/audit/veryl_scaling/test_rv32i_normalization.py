"""Compiler-free tests of normalization ledger authorization and rewrite guards.

Mocked solver reports exercise only fail-closed plumbing. They are NOT semantic
proofs, do not mint production-session handles, and never count as RV32I evidence.
Actual imported equations must still pass the real current-session checker.
"""
import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from audit.interpreter import BV, evaluate_record
from audit.veryl_scaling import rv32i_pipeline as p
from audit.veryl_scaling import rv32i_spec as isa


def passed_report():
    answers = {'binding_reset_nonempty': 'sat',
               'binding_reset_establishes_product': 'unsat',
               'binding_product_preservation': 'unsat'}
    return {'status': 'binding_verified_no_examples', 'implementation_binding': {
        'status': 'verified', 'obligations': [
            {'name': name, 'status': 'passed', 'solver_result': verdict,
             'backend': 'finite_bv', 'z3_seconds': 0,
             'finite': {'original_formula_validated': verdict == 'sat'}}
            for name, verdict in answers.items()]}}


def fake_import(out, frontend=None, lifter=None, text=None):
    """Deliberately arbitrary equation values: tests validate ledger, not ISA."""
    out = Path(out)
    out.mkdir(parents=True)
    text = p.source() if text is None else text
    (out / 'source.veryl').write_text(text)
    (out / 'design.json').write_text(json.dumps({'sources': [
        {'path': 'source.veryl', 'text': text}]}))
    state = p.cpu_state()
    roots = {key: 'w.fake_' + key for key in p.INTERNALS}
    machine = {'state': state, 'reset': {k: p.c.zero(v) for k,v in state.items()},
               'next': {k: 's.' + k for k in state},
               'outputs': {k: p.c.zero(v) for k,v in p.PORTS.items()},
               'wires': {'fake_' + k: False if ty == 'bool' else isa.b(ty['bv'], 17)
                         for k, (_, ty) in p.INTERNALS.items()}}
    (out / 'internal.json').write_text(json.dumps(roots))
    (out / 'machine.json').write_text(json.dumps(machine))
    (out / 'compiled.json').write_text('{}')
    return machine


class LedgerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.path = Path(cls.temp.name)
        cls.tools = []
        for name in ('checker', 'frontend', 'lifter'):
            path = cls.path / name
            path.write_text('test-only tool placeholder ' + name)
            cls.tools.append(path)
        with patch.object(p, 'compile_machine', side_effect=fake_import), \
             patch.object(p.c, 'execute', return_value=(passed_report(), 0.0)):
            cls.base = p.NormalizationSession(cls.path / 'session', *cls.tools)
            cls.base_handles = [cls.base.prove((field,)) for field in cls.base.GLOBAL_FIELDS]
            cls.base_handles += [cls.base.prove(cls.base.CASE_FIELDS, case)
                                 for case in cls.base._cases]

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def setUp(self):
        self.session = copy.deepcopy(self.base)
        self.handles = list(self.session._handles)

    def test_mocked_complete_ledger_and_guard_fallback(self):
        # This asserts conditional rewrite shape, not the fake equation's truth.
        normalized, record = self.session.normalize(self.handles)
        self.assertTrue(record['guards_preserved'])
        self.assertFalse(record['saved_records_are_certificates'])
        original = self.session._machine
        state = {k: False if ty == 'bool' else BV(ty['bv'], 0)
                 for k,ty in original['state'].items()}
        ins = {'rst': False, 'stall': False, 'imem_response': BV(32, 0),
               'imem_fault': False, 'dmem_response': BV(32, 0), 'dmem_fault': False}
        # ADDI x0 is legal but rd_write false: arbitrary original result retained.
        state['x_ir'] = BV(32, 0x13)
        normalized['outputs'] = {'result': self.session._internal['alu_result']}
        self.assertEqual(evaluate_record(normalized, 'outputs', state, ins)['result'], BV(32, 17))
        # ADDI x1 with fetch fault also preserves the original result fallback.
        state['x_ir'] = BV(32, 0x93)
        state['x_fetch_fault'] = True
        self.assertEqual(evaluate_record(normalized, 'outputs', state, ins)['result'], BV(32, 17))

    def test_missing_duplicate_foreign_and_saved_record_handles_rejected(self):
        cases = [self.handles[:-1], self.handles + self.handles[:1],
                 [object()] + self.handles[1:],
                 [self.base_handles[0]] + self.handles[1:],
                 [next(iter(self.session._handles.values()))] + self.handles[1:]]
        for handles in cases:
            with self.subTest(kind=type(handles[0]).__name__), self.assertRaises(ValueError):
                self.session.normalize(handles)

    def test_changed_machine_root_guard_and_reference_rejected(self):
        for change in ('machine', 'root', 'guard', 'reference'):
            session = copy.deepcopy(self.base)
            if change == 'machine': session._machine['next']['pc'] = isa.b(32, 99)
            elif change == 'root': session._internal['alu_result'] = 'w.other'
            elif change == 'guard': session._terms['alu_result'] = (True, session._terms['alu_result'][1])
            else: session._terms['alu_result'] = (session._terms['alu_result'][0], isa.b(32, 99))
            with self.subTest(change=change), self.assertRaises(ValueError):
                session.normalize(list(session._handles))

    def test_stale_tool_and_import_artifact_rejected(self):
        paths = [self.tools[0], self.path / 'session/import/compiled.json',
                 self.path / 'session/import/source.veryl']
        for path in paths:
            original = path.read_bytes()
            try:
                path.write_bytes(original + b' changed')
                with self.subTest(path=path.name), self.assertRaises(ValueError):
                    self.session.normalize(self.handles)
            finally:
                path.write_bytes(original)

    def test_source_snapshot_and_design_source_mismatch_rejected(self):
        for target in ('source.veryl', 'design.json'):
            def bad_import(out, *args):
                model = fake_import(out, *args)
                path = Path(out) / target
                path.write_text('different source' if target == 'source.veryl' else json.dumps({'sources': []}))
                return model
            with self.subTest(target=target), patch.object(p, 'compile_machine', side_effect=bad_import):
                with self.assertRaises(ValueError):
                    p.NormalizationSession(self.path / ('bad-' + target), *self.tools)

    def test_impure_reference_rejected_at_session_construction(self):
        real = p.internal_contract
        for prefix in ('w.', 'n.', 'o.', 'impl.', 'spec.'):
            def impure(*args, **kwargs):
                doc, terms = real(*args, **kwargs)
                terms['alu_result'] = (True, prefix + 'forbidden')
                return doc, terms
            with self.subTest(prefix=prefix), patch.object(p, 'compile_machine', side_effect=fake_import), \
                 patch.object(p, 'internal_contract', side_effect=impure):
                with self.assertRaises(ValueError):
                    p.NormalizationSession(self.path / ('impure-' + prefix), *self.tools)

    def test_nondirect_or_duplicated_imported_roots_rejected(self):
        for mode in ('nondirect', 'duplicate'):
            def altered_import(out, *args):
                model = fake_import(out, *args)
                path = Path(out) / 'internal.json'
                roots = json.loads(path.read_text())
                roots['alu_result'] = 's.x_operand1' if mode == 'nondirect' else roots['next_pc']
                path.write_text(json.dumps(roots))
                return model
            with self.subTest(mode=mode), patch.object(p, 'compile_machine', side_effect=altered_import), \
                 patch.object(p.c, 'execute', return_value=(passed_report(), 0.0)):
                session = p.NormalizationSession(self.path / ('roots-' + mode), *self.tools)
                handles = [session.prove((field,)) for field in session.GLOBAL_FIELDS]
                handles += [session.prove(session.CASE_FIELDS, case) for case in session._cases]
                with self.assertRaisesRegex(ValueError, 'wire roots'):
                    session.normalize(handles)

    def test_concurrent_current_source_change_rejected(self):
        captured = p.source()
        with patch.object(p, 'source', side_effect=[captured, captured + '\n']), \
             patch.object(p, 'compile_machine', side_effect=fake_import):
            with self.assertRaisesRegex(ValueError, 'source changed'):
                p.NormalizationSession(self.path / 'changed-during-import', *self.tools)

    def test_partition_reordering_rejected_without_coverage_reproof(self):
        cases = list(self.session._cases)
        cases[0], cases[1] = cases[1], cases[0]
        self.session._cases = tuple(cases)
        with self.assertRaisesRegex(ValueError, 'partition'):
            self.session.normalize(self.handles)

    def test_incomplete_unknown_unvalidated_and_external_verdicts_rejected(self):
        reports = []
        x = passed_report(); x['implementation_binding']['obligations'].pop(); reports.append(x)
        x = passed_report(); x['implementation_binding']['obligations'][2]['solver_result'] = 'unknown'; reports.append(x)
        x = passed_report(); x['implementation_binding']['obligations'][0]['finite']['original_formula_validated'] = False; reports.append(x)
        x = passed_report(); x['implementation_binding']['obligations'][2]['backend'] = 'z3'; reports.append(x)
        x = passed_report(); x['implementation_binding']['obligations'][2]['z3_seconds'] = 0.01; reports.append(x)
        for report in reports:
            with self.assertRaises(ValueError): p.validate_lemma(report)


def fake_memory_import(count, out, frontend=None, lifter=None):
    from audit.veryl_scaling import rv32i_memory as memory
    out = Path(out); out.mkdir(parents=True)
    text = memory.memory_source(count)
    (out / 'source.veryl').write_text(text)
    (out / 'design.json').write_text(json.dumps({'sources': [{'path': 'source.veryl', 'text': text}]}))
    state = memory.memory_state(count)
    raw = {'state': state, 'reset': {k: 'i.seed_' + k for k in state},
           'next': {k: 's.' + k for k in state}, 'wires': {},
           'outputs': {k: p.c.zero(v) for k,v in memory.PORTS.items()}}
    (out / 'machine.json').write_text(json.dumps(raw))
    return raw


def mock_document_verdict(command, output, *args):
    """Only a ledger test double; never usable as real checker evidence."""
    doc = json.loads(Path(command[1]).read_text())
    if 'specs' in doc:
        report = passed_report()
        examples = doc['specs']['Contract'].get('examples')
        if examples:
            report['status'] = 'spec_examples_and_binding_verified'
            report['examples'] = []
            for name, example in examples.items():
                verdict = 'sat' if example['expect'] == 'positive' else 'unsat'
                report['examples'].append({'target': 'Contract', 'example': name,
                    'expect': example['expect'], 'status': 'passed',
                    'evidence': {'solver_result': verdict, 'backend': 'finite_bv',
                                 'finite': {'original_formula_validated': verdict == 'sat'}}})
        return report, 0.0
    return {'status': 'stuttering_refinement_verified', 'engine_summary': {'z3_queries': 0},
            'obligations': [{'name': name, 'status': 'passed', 'solver_result': verdict,
                             'backend': 'finite_bv',
                             'finite': {'original_formula_validated': verdict == 'sat'}}
                            for name,verdict in p.c.RUNNER.EXPECTED_RESULTS.items()]}, 0.0


class CompositionLedgerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        from audit.veryl_scaling import rv32i_reuse as reuse
        from audit.veryl_scaling import rv32i_memory as memory
        cls.temp = tempfile.TemporaryDirectory()
        cls.path = Path(cls.temp.name)
        tools = []
        for name in ('checker', 'frontend', 'lifter'):
            path = cls.path / name; path.write_text('mock composition ' + name); tools.append(path)
        with patch.object(p, 'compile_machine', side_effect=fake_import), \
             patch.object(memory, 'compile_machine', side_effect=fake_memory_import), \
             patch.object(p.c, 'execute', side_effect=mock_document_verdict):
            cls.base = reuse.ProofSession(cls.path / 'session', *tools)
            cls.base.prove_cpu()
            cls.base.prove_memory(4)

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def setUp(self):
        self.session = copy.deepcopy(self.base)
        self.cpu = next(h for h,r in self.session._proofs.items() if r['kind'] == 'cpu')
        self.memory = next(h for h,r in self.session._proofs.items() if r['kind'] == 'memory')

    def test_mocked_typed_graph_and_shared_reset(self):
        # Plumbing acceptance only; setUpClass deliberately mocks every verdict.
        record = self.session.compose(self.cpu, self.memory, 4)
        self.assertFalse(record['saved_records_are_certificates'])
        self.assertEqual(record['connections']['cpu.rst'], 'system.rst')
        self.assertEqual(record['connections']['memory.rst'], 'system.rst')
        self.assertEqual(record['connections']['cpu.dmem_fault'], 'memory.dmem_fault')

    def test_swapped_port_and_independent_reset_rejected(self):
        graph = self.session.compose(self.cpu, self.memory, 4)['connections']
        for key,value in [('memory.dmem_address', 'cpu.imem_address'),
                          ('memory.write_data', 'cpu.write_address'),
                          ('memory.rst', 'system.other_reset')]:
            changed = dict(graph); changed[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.session.compose(self.cpu, self.memory, 4, changed)

    def test_count_foreign_and_role_handles_rejected(self):
        for cpu,memory,count in [(self.cpu,self.memory,16), (object(),self.memory,4),
                                  (self.cpu,object(),4), (next(iter(self.base._proofs)),self.memory,4),
                                  (self.memory,self.cpu,4),
                                  (self.cpu,self.cpu,4)]:
            with self.assertRaises(ValueError):
                self.session.compose(cpu,memory,count)

    def test_stale_memory_machine_and_import_rejected(self):
        self.session._memories[self.memory]['next']['data0'] = isa.b(32, 99)
        with self.assertRaises(ValueError): self.session.compose(self.cpu,self.memory,4)
        fresh = copy.deepcopy(self.base)
        cpu = next(h for h,r in fresh._proofs.items() if r['kind'] == 'cpu')
        memory = next(h for h,r in fresh._proofs.items() if r['kind'] == 'memory')
        path = self.path / 'session/memory-4-import/source.veryl'
        old = path.read_bytes()
        try:
            path.write_bytes(old+b' altered')
            with self.assertRaises(ValueError): fresh.compose(cpu,memory,4)
        finally:
            path.write_bytes(old)


def mocked_conjunction():
    name = 'binding_product_preservation'
    names = [name + '_lemma_' + format(i, '04d') for i in range(2)]
    children = [{'name': child, 'status': 'passed', 'solver_result': 'unsat',
                 'backend': 'finite_bv', 'logical_expectation': 'unsat',
                 'z3_seconds': 0, 'finite': {'work': 10, 'clauses': 100}}
                for child in names]
    return {'name': name, 'backend': 'conjunctive_lemmas', 'status': 'passed',
            'solver_result': 'unsat', 'z3_seconds': 0, 'children': children,
            'coverage': {'rule': 'exact-conjunction-introduction-v1',
                         'ordered_names': names, 'complete': True,
                         'same_full_precondition': True,
                         'postconditions_used_as_assumptions': False},
            'original_source_validation': {'includes_derived_context': True},
            'original_attempt': {'status': 'unknown', 'solver_result': 'unknown',
                                 'backend': 'finite_bv', 'z3_seconds': 0},
            'cost': {'per_lemma_work_limit': 100_000_000,
                     'per_lemma_clause_limit': 1_000_000,
                     'per_lemma_timeout_ms': 10_000,
                     'total_child_work': 20,
                     'original_monolithic_result': 'unknown',
                     'whole_bundle_budget_is_not_a_single_query_budget': True}}


class ConjunctiveReportTests(unittest.TestCase):
    def test_opt_in_required_even_for_complete_mocked_report(self):
        query = mocked_conjunction()
        p.validate_conjunctive_obligation(query)
        report = passed_report()
        report['implementation_binding']['obligations'][2] = query
        with self.assertRaises(ValueError): p.validate_lemma(report)
        p.validate_lemma(report, allow_conjunctive=True)

    def test_missing_reordered_unknown_sat_and_circular_coverage_rejected(self):
        variants = []
        q=mocked_conjunction(); q['children'].pop(); variants.append(q)
        q=mocked_conjunction(); q['children'].reverse(); variants.append(q)
        q=mocked_conjunction(); q['children'][1]=copy.deepcopy(q['children'][0]); variants.append(q)
        q=mocked_conjunction(); q['children']=[]; q['coverage']['ordered_names']=[]; variants.append(q)
        for status,verdict in [('unknown','unknown'),('counterexample','sat')]:
            q=mocked_conjunction(); q['children'][0].update(status=status,solver_result=verdict); variants.append(q)
        for key,value in [('complete',False),('same_full_precondition',False),
                          ('postconditions_used_as_assumptions',True),('rule','other')]:
            q=mocked_conjunction(); q['coverage'][key]=value; variants.append(q)
        q=mocked_conjunction(); q['original_source_validation']['includes_derived_context']=False; variants.append(q)
        q=mocked_conjunction(); q['original_attempt']['solver_result']='sat'; variants.append(q)
        q=mocked_conjunction(); q['children'][0]['backend']='z3'; variants.append(q)
        for query in variants:
            with self.assertRaises(ValueError): p.validate_conjunctive_obligation(query)

    def test_cost_caps_and_external_original_attempt_rejected(self):
        variants = []
        q=mocked_conjunction(); q['cost']['total_child_work']=21; variants.append(q)
        q=mocked_conjunction(); q['cost']['per_lemma_work_limit']=200_000_000; variants.append(q)
        q=mocked_conjunction(); q['cost']['whole_bundle_budget_is_not_a_single_query_budget']=False; variants.append(q)
        q=mocked_conjunction(); q['original_attempt']['z3_seconds']=1.0; variants.append(q)
        for field,value in [('work',100_000_001),('clauses',1_000_001),('work',-1),('clauses',-1)]:
            q=mocked_conjunction(); q['children'][0]['finite'][field]=value
            q['cost']['total_child_work']=sum(child['finite']['work'] for child in q['children'])
            variants.append(q)
        for query in variants:
            with self.assertRaises(ValueError): p.validate_conjunctive_obligation(query)


def mocked_proof_bundle():
    """Format-only fixture; never a real proof or handle-minting authority."""
    name = 'mock_bundle'
    child = {'name': name + '_query_0000', 'backend': 'finite_bv',
             'status': 'passed', 'solver_result': 'unsat', 'logical_expectation': 'unsat',
             'z3_seconds': 0, 'emission_work': 2, 'proof_label': 'lemma',
             'finite': {'work': 10, 'clauses': 20}}
    return {'name': name, 'backend': 'checked_proof_bundle', 'status': 'passed',
            'solver_result': 'unsat', 'logical_expectation': 'unsat', 'z3_seconds': 0,
            'seconds': 0.1, 'children': [child], 'root': 0, 'original_recheck': None,
            'original_source_validation': {'complete': True, 'includes_derived_context': True},
            'coverage': {'rule': 'fresh-acyclic-sequent-bundle-v1', 'complete': True,
                         'fresh_handles_only': True, 'saved_reports_are_authority': False,
                         'exact_original_sequent': True},
            'proof_graph': [{'id': 0, 'rule': 'fresh-solver-unsat', 'dependencies': [],
                             'query_index': 0, 'statement': name + '_statement_0000.smt2'}],
            'cost': {'mode': 'independent_lemmas', 'per_query_work_limit': 100_000_000,
                     'per_query_clause_limit': 1_000_000, 'per_query_timeout_ms': 10_000,
                     'whole_bundle_is_one_query': False, 'kernel_enabled': True,
                     'work_including_validation_and_derived_steps': 15,
                     'allocated_clauses': 20}}


class ProofBundleReportTests(unittest.TestCase):
    def reject(self, query):
        with self.assertRaises(ValueError): p.validate_proof_bundle(query)

    def test_mock_success_and_explicit_independent_aggregate(self):
        q = mocked_proof_bundle(); p.validate_proof_bundle(q)
        q['children'][0]['finite'] = {'work': 70_000_000, 'clauses': 700_000}
        second = copy.deepcopy(q['children'][0]); second['name'] = q['name'] + '_query_0001'
        q['children'].append(second)
        q['proof_graph'] += [dict(id=1, rule='fresh-solver-unsat', dependencies=[], query_index=1,
                                  statement=q['name'] + '_statement_0001.smt2'),
                             dict(id=2, rule='exact-congruence-with-original-premise-retained',
                                  dependencies=[0, 1], statement=q['name'] + '_statement_0002.smt2')]
        q['root'] = 2
        q['cost'].update(work_including_validation_and_derived_steps=140_000_010,
                         allocated_clauses=1_400_000)
        p.validate_proof_bundle(q)
        q['cost'].update(mode='shared_query', whole_bundle_is_one_query=True, kernel_enabled=False)
        self.reject(q)

    def test_source_scope_mode_and_authority_markers(self):
        changes = [('coverage', 'complete', False), ('coverage', 'fresh_handles_only', False),
                   ('coverage', 'saved_reports_are_authority', True),
                   ('coverage', 'exact_original_sequent', False),
                   ('original_source_validation', 'complete', False),
                   ('original_source_validation', 'includes_derived_context', False),
                   ('cost', 'mode', 'saved_report'), ('cost', 'per_query_work_limit', 200_000_000),
                   ('cost', 'whole_bundle_is_one_query', True)]
        for section, key, value in changes:
            q = mocked_proof_bundle(); q[section][key] = value
            with self.subTest(section=section, key=key): self.reject(q)

    def test_graph_root_acyclicity_and_leaf_identity(self):
        variants = []
        for root in (-1, 1, True):
            q = mocked_proof_bundle(); q['root'] = root; variants.append(q)
        for key, value in [('id', 1), ('dependencies', [0]), ('query_index', 1),
                           ('rule', 'saved-verdict'), ('statement', 'foreign.smt2')]:
            q = mocked_proof_bundle(); q['proof_graph'][0][key] = value; variants.append(q)
        q = mocked_proof_bundle(); q['proof_graph'] = []; variants.append(q)
        q = mocked_proof_bundle(); q['proof_graph'].append(dict(q['proof_graph'][0], id=1,
            statement=q['name'] + '_statement_0001.smt2')); q['root'] = 1; variants.append(q)
        q = mocked_proof_bundle(); q['proof_graph'].append(dict(id=1,
            rule='exact-antecedent-modus-ponens', dependencies=[0],
            statement=q['name'] + '_statement_0001.smt2')); q['root'] = 1; variants.append(q)
        for q in variants: self.reject(q)

    def test_unknown_sat_external_and_reordered_leaves(self):
        for key, value in [('solver_result', 'unknown'), ('solver_result', 'sat'),
                           ('backend', 'z3'), ('name', 'foreign_query_0000'),
                           ('z3_seconds', 1)]:
            q = mocked_proof_bundle(); q['children'][0][key] = value; self.reject(q)
        q = mocked_proof_bundle(); q['children'] = []; self.reject(q)
        q = mocked_proof_bundle(); q['children'].append(copy.deepcopy(q['children'][0])); self.reject(q)
        q = mocked_proof_bundle(); q['children'][0].update(solver_result='sat', status='counterexample')
        q['children'][0]['finite']['original_formula_validated'] = True
        self.reject(q)  # A validated auxiliary SAT still cannot mint an UNSAT leaf.

    def test_costs_and_shared_mode_caps(self):
        for key, value in [('work', -1), ('work', True), ('work', 100_000_001),
                           ('clauses', -1), ('clauses', 1_000_001)]:
            q = mocked_proof_bundle(); q['children'][0]['finite'][key] = value; self.reject(q)
        for key, value in [('allocated_clauses', 21), ('work_including_validation_and_derived_steps', 11)]:
            q = mocked_proof_bundle(); q['cost'][key] = value; self.reject(q)
        q = mocked_proof_bundle(); q['cost'].update(mode='shared_query',
            whole_bundle_is_one_query=True, kernel_enabled=False); p.validate_proof_bundle(q)
        for seconds in (10, 11):
            bad = copy.deepcopy(q); bad['seconds'] = seconds; self.reject(bad)
        q['children'][0]['backend'] = 'structural_kernel'; self.reject(q)

    def test_original_recheck_cannot_be_saved_or_unknown(self):
        q = mocked_proof_bundle(); q['root'] = None; q['proof_graph'] = []
        q['coverage']['exact_original_sequent'] = False
        q['children'][0]['proof_label'] = 'original counterexample replay'
        q['original_recheck'] = copy.deepcopy(q['children'][0]); p.validate_proof_bundle(q)
        for value in ({}, {'status': 'passed', 'solver_result': 'unsat'},
                      dict(q['children'][0], name='foreign')):
            bad = copy.deepcopy(q); bad['original_recheck'] = value; self.reject(bad)
        bad = copy.deepcopy(q); bad['children'][0].update(status='unknown', solver_result='unknown')
        bad['original_recheck'] = copy.deepcopy(bad['children'][0]); self.reject(bad)

    def test_cost_and_replay_metadata_are_not_forged(self):
        # Strict report-format checks complement, but never replace, opaque live handles.
        for key, value in [('emission_work', -100), ('emission_work', True),
                           ('logical_expectation', 'sat')]:
            q = mocked_proof_bundle(); q['children'][0][key] = value; self.reject(q)
        q = mocked_proof_bundle(); q['cost']['allocated_clauses'] = 20.0; self.reject(q)
        q = mocked_proof_bundle(); q['root'] = None; q['proof_graph'] = []
        q['coverage']['exact_original_sequent'] = False
        q['original_recheck'] = copy.deepcopy(q['children'][0]); self.reject(q)


if __name__ == '__main__':
    unittest.main(verbosity=2)
