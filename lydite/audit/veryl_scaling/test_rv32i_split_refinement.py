"""Independent executable checks of split proof relations (not universal proof)."""
import json
import random
import unittest
import copy
import tempfile
from unittest.mock import patch
from pathlib import Path
from audit.interpreter import BV, Mem, evaluate, evaluate_record, related
from audit.veryl_scaling import rv32i_split_pipeline as p
from audit.veryl_scaling import rv32i_split_reuse as r
from audit.veryl_scaling.test_rv32i_pipeline import enc_i,enc_r,enc_s,enc_b

def conjuncts(e):
    if isinstance(e,list) and e[0]=='and':
        yield from conjuncts(e[1]);yield from conjuncts(e[2])
    else: yield e

def audit_binding(raw,program,cycles=200,seed=23,canonical_bank=False,transport_view=False,control_invariant=False,operand_views=False):
    doc=r.abstract_composition(raw,canonical_bank=canonical_bank,transport_view=transport_view,control_invariant=control_invariant,operand_views=operand_views);rng=random.Random(seed)
    inputs={'rst':False,'stall':False,'seed_rom':Mem(7,32,0,dict(enumerate(program))),
        'seed_data':Mem(7,32,0,{i:rng.getrandbits(32) for i in range(64)}),'seed_limit':BV(7,64)}
    env={'i.'+k:v for k,v in inputs.items()}
    impl={k:evaluate(v,env) for k,v in doc['impl']['reset'].items()}
    spec={k:evaluate(v,env) for k,v in doc['spec']['reset'].items()}
    terms=list(conjuncts(doc['binding']));retired=0
    for cycle in range(cycles):
        env={**{'impl.'+k:v for k,v in impl.items()},**{'spec.'+k:v for k,v in spec.items()}}
        for index,term in enumerate(terms):
            if not evaluate(term,env):
                raise AssertionError((cycle,index,json.dumps(term)[:800],{k:v for k,v in impl.items() if k in ('pc','fetch_pc','w_pc','m_pc','x_pc','d_pc','w_valid','m_valid','x_valid','d_valid','w_ir','m_ir','x_ir','w_fault','w_redirect')}))
        if impl['halted']: return retired
        inputs['stall']=rng.randrange(4)==0
        ports=evaluate_record(doc['impl'],'outputs',impl,inputs)
        if ports['retire']:
            spec=evaluate_record(doc['spec'],'next',spec,inputs);retired+=1
        nxt=evaluate_record(doc['impl'],'next',impl,inputs)
        if inputs['stall']: assert nxt==impl
        impl=nxt
    raise AssertionError('program did not halt')

class Structure(unittest.TestCase):
    def test_selected_source(self):self.assertEqual(p.sha(p.SOURCE),p.SELECTED_SHA256)
    def test_state(self):
        state=p.cpu_state();self.assertEqual(len(state),91)
        self.assertEqual(state['d_sel1'],{'bv':32});self.assertIn('m_valid',state)
    def test_sequential_frontier(self):
        env={'impl.pc':BV(32,100),**{'impl.'+st+'_valid':True for st in ('w','m','x','d')}}
        got={k:evaluate(v,env).v for k,v in r.sequential_frontier().items()}
        self.assertEqual(got,dict(w=100,m=104,x=108,d=112,fetch=116))
    def test_register_file_partition_covers_all_bits(self):
        raw={'state':p.cpu_state(),'wires':{}}
        doc=p.register_file_contract(raw,{k:'w.'+k for k in p.INTERNALS})
        operation=doc['specs']['Contract']['operations']['tick']
        terms=[x for x in conjuncts(operation[2]) if x is not True]
        self.assertEqual(len(terms),128)
        self.assertEqual({(x[1][3],x[1][1]) for x in terms},{('n.'+k,bit) for k in p.INTERNALS for bit in range(32)})
    def test_saved_json_cannot_mint_normalization_handle(self):
        session=r.ProofSession.__new__(r.ProofSession);session._check=lambda:None;session._equation_handles={}
        for handle in ({'status':'verified'},object(),None):
            with self.assertRaises(ValueError):session.normalize_register_file(handle)
    def test_saved_json_cannot_mint_stage_handles(self):
        session=r.ProofSession.__new__(r.ProofSession);session._check=lambda:None;session._equation_handles={}
        for handles in ([],[object()], [{'status':'verified'}]):
            with self.assertRaises(ValueError):session.normalize_stage_equations(handles)
    def test_instruction_partition(self):
        cases=p.instruction_cases();self.assertEqual(len(cases),41);self.assertEqual(len(set(cases)),41)
        self.assertIn('fence',cases);self.assertIn('ecall',cases);self.assertIn('ebreak',cases)
    def test_mutations_have_unique_source_sites(self):
        text=p.source()
        for name,(before,after,stage,case) in p.MUTATIONS.items():
            self.assertEqual(text.count(before),1,name);self.assertNotEqual(before,after)
    def test_reject_unknown_instruction_case(self):
        with self.assertRaises(ValueError):p.case_guard(p.a.decode('i.ir'),'mul')
    def test_fence_reserved_fields(self):
        d=p.decode_payload('i.ir')
        for word in (0x0f,0xffff8f8f):self.assertTrue(evaluate(d['legal'],{'i.ir':BV(32,word)}))
    def test_decode_all_legal_classes(self):
        d=p.decode_payload('i.ir');self.assertEqual(len(d),15)
        self.assertEqual(evaluate(d['legal'],{'i.ir':BV(32,0x73)}),True)
        self.assertEqual(evaluate(d['legal'],{'i.ir':BV(32,0x2000033)}),False)

class RewriteBoundary(unittest.TestCase):
    """Mock proof transport only; these tests are NOT proof certificates."""
    def session(self):
        session=r.ProofSession.__new__(r.ProofSession)
        folder=tempfile.TemporaryDirectory();self.addCleanup(folder.cleanup)
        session.out=Path(folder.name);session._check=lambda:None
        state=p.cpu_state()
        session.raw={'state':state,'reset':{k:p.c.zero(v) for k,v in state.items()},
            'next':{k:p.c.zero(v) for k,v in state.items()},'wires':{},
            'outputs':{k:p.c.zero(v) for k,v in p.PORTS.items()}}
        session._raw_hash=p.base.digest(session.raw);session._equation_handles={}
        session.cpu=None;session._normalization=None
        session._records={stage+'-execution':{'document_sha256':p.base.digest(builder(session.raw))}
            for stage,builder in [('x',p.x_execution_contract),('m',p.m_execution_contract)]}
        return session
    def test_impure_rf_reference_is_rejected(self):
        session=self.session()
        session._internal={k:'w.'+k for k in p.INTERNALS}
        session._internal_hash=p.base.digest(session._internal)
        session.raw['wires']={k:p.b(32,0) for k in p.INTERNALS}
        session._raw_hash=p.base.digest(session.raw)
        session._prove=lambda label,doc,scoped:{'document_sha256':p.base.digest(doc)}
        terms=p.register_file_terms();terms['rf_operand1']='n.unproved'
        with patch.object(p,'register_file_terms',return_value=terms):
            handle=session.prove_register_file()
            with self.assertRaisesRegex(ValueError,'pure original prestate'):session.normalize_register_file(handle)
    def test_foreign_and_duplicate_stage_handles(self):
        first=self.session();second=self.session();handles=first.prove_stage_equations()
        with self.assertRaises(ValueError):second.normalize_stage_equations(handles)
        with self.assertRaises(ValueError):first.normalize_stage_equations([handles[0],handles[0]])
    def test_stale_stage_handle(self):
        session=self.session();handles=session.prove_stage_equations()
        session._equation_handles[handles[0]]['machine_sha256']='stale'
        with self.assertRaises(ValueError):session.normalize_stage_equations(handles)
    def test_changed_guard_rejected(self):
        session=self.session();handles=session.prove_stage_equations()
        doc=p.x_execution_contract(session.raw)
        doc['specs']['Contract']['operations']['tick'][1]=True
        with patch.object(p,'x_execution_contract',return_value=doc):
            with self.assertRaises(ValueError):session.normalize_stage_equations(handles)
    def test_missing_field_rejected(self):
        session=self.session();doc=p.x_execution_contract(session.raw)
        doc['specs']['Contract']['operations']['tick']=['implies',True,['eq','n.m_valid',True]]
        with patch.object(p,'x_execution_contract',return_value=doc):
            session._records['x-execution']['document_sha256']=p.base.digest(doc)
            handles=session.prove_stage_equations()
            with self.assertRaises(ValueError):session.normalize_stage_equations(handles)
    def test_request_pins_remain_original(self):
        session=self.session();handles=session.prove_stage_equations();before=copy.deepcopy(session.raw['outputs'])
        normalized=session.normalize_stage_equations(handles)
        self.assertEqual(normalized['outputs'],before)
        pins=[x for x in session._normalization['coverage'] if x['part']=='outputs']
        self.assertEqual(len(pins),3);self.assertTrue(all(x['applied'] is False for x in pins))
    def test_selected_stage_scope_is_explicit(self):
        session=self.session();handles=session.prove_stage_equations()
        normalized=session.normalize_stage_equations(handles,('x',))
        self.assertEqual(normalized['next']['w_result'],session.raw['next']['w_result'])
        self.assertNotEqual(normalized['next']['m_result'],session.raw['next']['m_result'])
        self.assertTrue(all(not x['applied'] for x in session._normalization['coverage'] if x['stage']=='m'))

    def test_arithmetic_field_subset_exact(self):
        session=self.session();handles=session.prove_stage_equations()
        fields=('m_result','m_address','m_taken','m_target')
        before=copy.deepcopy(session.raw)
        normalized=session.normalize_stage_equations(handles,('x',),fields)
        for key in ('state','reset','outputs'):self.assertEqual(normalized[key],before[key])
        for key,value in before['next'].items():
            if key not in fields:self.assertEqual(normalized['next'][key],value)
        self.assertEqual({x['field'] for x in session._normalization['coverage'] if x['applied']},set(fields))
        self.assertEqual(session._normalization['stage_fields'],list(fields))
    def test_invalid_field_subsets_rejected(self):
        for fields in ((),('missing',),('m_result','m_result'),('w_result',),('dmem_addr',)):
            session=self.session();handles=session.prove_stage_equations()
            with self.assertRaises(ValueError):session.normalize_stage_equations(handles,('x',),fields)

class ProofProgramGeneration(unittest.TestCase):
    def inputs(self):
        state=p.cpu_state()
        cpu={'state':state,'reset':{k:p.c.zero(v) for k,v in state.items()},
            'next':{k:'s.'+k for k in state},'wires':{},
            'outputs':{k:p.c.zero(v) for k,v in p.PORTS.items()}}
        cpu['next']['m_ir']=['ite','i.stall','s.m_ir','s.x_ir']
        for field in ('m_address','m_result','m_taken','m_target'):
            cpu['next'][field]=['ite',False,p.c.zero(state[field]),'s.'+field]
        return cpu,r.abstract_composition(cpu,transport_view=True,control_invariant=True)
    def test_six_exact_expression_targets_without_saved_verdicts(self):
        from audit.veryl_scaling import rv32i_split_proof_program as program
        cpu,doc=self.inputs();before=copy.deepcopy((cpu,doc));value=program.build(cpu,doc)
        self.assertEqual((cpu,doc),before)
        self.assertEqual({x['id'] for x in value['programs']},{'m_address','m_result','m_taken','m_target','x_operand1','x_operand2'})
        self.assertEqual(len(value['variables']),7)
        self.assertEqual(value['mode'],'independent_lemmas')
        for row in value['programs']:
            self.assertEqual(set(row),{'id','match_rhs','steps','result'})
            self.assertEqual(row['match_rhs'],program._rhs(doc,row['id']))
    def test_ambiguous_or_missing_binding_target_rejected(self):
        from audit.veryl_scaling import rv32i_split_proof_program as program
        cpu,doc=self.inputs();doc['binding']=['and',doc['binding'],doc['binding']]
        with self.assertRaises(ValueError):program.build(cpu,doc)
        cpu,doc=self.inputs();doc['binding']=True
        with self.assertRaises(ValueError):program.build(cpu,doc)
    def test_copy_and_normalization_shapes_required(self):
        from audit.veryl_scaling import rv32i_split_proof_program as program
        cpu,doc=self.inputs();cpu['next']['m_ir']='s.d_ir'
        with self.assertRaises(ValueError):program.build(cpu,doc)
        cpu,doc=self.inputs();cpu['next']['m_result']='s.m_result'
        with self.assertRaises(ValueError):program.build(cpu,doc)
    def test_unexpected_state_rejected(self):
        from audit.veryl_scaling import rv32i_split_proof_program as program
        cpu,doc=self.inputs();cpu['state']['unproved_ghost']={'bv':32}
        with self.assertRaises(ValueError):program.build(cpu,doc)

class BankBoundary(RewriteBoundary):
    def bank_session(self):
        session=self.session()
        session._records['retirement']={'document_sha256':p.base.digest(p.retirement_contract(session.raw))}
        session._prove=lambda label,doc,scoped:{'document_sha256':p.base.digest(doc)}
        return session
    def test_xm_noninterference_covers_safe_and_illegal_sources(self):
        env={'s.x_valid':True,'s.m_valid':True,'s.m_writes':True,
            's.x_ir':BV(32,enc_i(0x13,1,3,0)),'s.m_ir':BV(32,enc_i(0x13,3,0,0))}
        self.assertFalse(evaluate(p.xm_noninterference(),env))
        env['s.x_ir']=BV(32,enc_r(1,0,3));self.assertFalse(evaluate(p.xm_noninterference(),env))
        env['s.x_ir']=BV(32,enc_r(1,0,3,f7=1));self.assertFalse(evaluate(p.xm_noninterference(),env))
        env['s.x_ir']=BV(32,0x37|(3<<15));self.assertTrue(evaluate(p.xm_noninterference(),env))
        env['s.m_writes']=False;env['s.x_ir']=BV(32,enc_i(0x13,1,3,0));self.assertTrue(evaluate(p.xm_noninterference(),env))
    def test_transport_view_retains_independent_packet_equations(self):
        arch={'pc':{'bv':32},'halted':'bool',**{'r'+str(i):{'bv':32} for i in range(32)},**r.mem.ARRAYS}
        relation=r.binding(arch,transport_view=True);found={};seen=set()
        def visit(e):
            if not isinstance(e,list) or id(e) in seen:return
            seen.add(id(e))
            if e[0]=='eq' and isinstance(e[1],str) and e[1] in ('impl.w_result','impl.m_result'):found[e[1]]=e[2]
            for x in e:visit(x)
        visit(relation)
        self.assertEqual(set(found),{'impl.w_result','impl.m_result'})
        self.assertFalse(p.c.RUNNER.references(found['impl.w_result'],'impl.w_result'))
        self.assertFalse(p.c.RUNNER.references(found['impl.m_result'],'impl.m_result'))
    def test_no_match_guard_is_not_dropped(self):
        doc=p.arbitrary_bank_contract('no_match')
        self.assertEqual(doc['specs']['Contract']['operations']['tick'][0],'implies')
        self.assertNotEqual(doc['specs']['Contract']['operations']['tick'][1],True)
    def test_bank_algebra_concrete(self):
        rng=random.Random(95312)
        for kind in ('single','cascade','transport'):
            doc=p.arbitrary_bank_contract(kind)
            for n in range(100):
                inputs={'rst':False,**{'r'+str(i):BV(32,rng.getrandbits(32)) for i in range(32)},
                    'q':BV(5,n%32),**{'g'+x:bool(rng.randrange(2)) for x in ('w','m','x','safe')},
                    **{'d'+x:BV(5,rng.randrange(32)) for x in ('w','m','x')},
                    **{'v'+x:BV(32,rng.getrandbits(32)) for x in ('w','m','x','safe')}}
                if inputs['gsafe']:inputs['gm']=True;inputs['vsafe']=inputs['vm']
                if inputs['gm'] and not inputs['gsafe']:inputs['dm']=BV(5,inputs['q'].v+1)
                if inputs['gx']:inputs['dx']=BV(5,inputs['q'].v+1)
                env={'i.'+k:v for k,v in inputs.items()}
                env['n.read']=evaluate(doc['implementation']['next']['read'],env)
                self.assertTrue(evaluate(doc['specs']['Contract']['operations']['tick'],env),kind)
    def test_bank_foreign_and_stale_handles(self):
        first=self.bank_session();second=self.bank_session();handle=first.prove_bank_equations()
        with self.assertRaises(ValueError):second.normalize_bank_equations(handle)
        first._equation_handles[handle]['terms_sha256']='stale'
        with self.assertRaises(ValueError):first.normalize_bank_equations(handle)
    def test_missing_bank_dependency(self):
        session=self.bank_session();handle=session.prove_bank_equations()
        del session._equation_handles[handle]['records']['transport']
        with self.assertRaises(ValueError):session.normalize_bank_equations(handle)
    def test_bank_scope_and_pin_preservation(self):
        session=self.bank_session();handle=session.prove_bank_equations()
        before=copy.deepcopy(session.raw)
        after=session.normalize_bank_equations(handle)
        self.assertEqual(after['outputs'],before['outputs'])
        for name in before['next']:
            if name not in {f'r{i}' for i in range(32)}:self.assertEqual(after['next'][name],before['next'][name])
        self.assertEqual(session._bank_record['source_next_fields'],sorted(f'r{i}' for i in range(32)))
    def test_factoring_requires_all_literal_roots(self):
        with self.assertRaises(ValueError):p.factor_pending_bank_reads(True)
        terms=p.pending_bank_read_terms('impl.')
        original=p.c.conj(p.eq(pair[0],pair[0]) for pair in terms.values())
        factored,counts=p.factor_pending_bank_reads(original)
        self.assertEqual(set(counts),set(terms));self.assertTrue(all(counts.values()))
        self.assertNotEqual(original,factored)

class DispatchBoundary(RewriteBoundary):
    def dispatch_session(self):
        session=self.session();session._prove=lambda label,doc,scoped:{'document_sha256':p.base.digest(doc)}
        return session
    def test_dispatch_foreign_stale_handles(self):
        first=self.dispatch_session();second=self.dispatch_session();handle=first.prove_dispatch_equations()
        with self.assertRaises(ValueError):second.normalize_dispatch_equations(handle)
        first._equation_handles[handle]['terms_sha256']='stale'
        with self.assertRaises(ValueError):first.normalize_dispatch_equations(handle)
    def test_dispatch_exact_scope(self):
        session=self.dispatch_session();handle=session.prove_dispatch_equations();before=copy.deepcopy(session.raw)
        after=session.normalize_dispatch_equations(handle)
        self.assertEqual(after['outputs'],before['outputs'])
        for k in before['next']:
            if k not in ('d_pc','d_ir','d_fetch_fault'):self.assertEqual(before['next'][k],after['next'][k])
        self.assertEqual(after['next']['d_pc'][1],after['next']['d_ir'][1])
        self.assertEqual(after['next']['d_pc'][1],after['next']['d_fetch_fault'][1])
    def test_dispatch_full_bit_coverage(self):
        session=self.dispatch_session();doc=p.dispatch_payload_contract(session.raw)
        terms=[x for x in conjuncts(doc['specs']['Contract']['operations']['tick'][2]) if x is not True]
        self.assertEqual(len(terms),65)
        self.assertEqual({(x[1][3],x[1][1]) for x in terms if isinstance(x[1],list)},
            {(field,i) for field in ('n.d_ir','n.d_pc') for i in range(32)})

class HistoryBoundary(unittest.TestCase):
    def source(self):
        state=p.cpu_state()
        raw={'state':state,'reset':{k:p.c.zero(v) for k,v in state.items()},
            'next':{k:'s.'+k for k in state},'wires':{},'outputs':{k:p.c.zero(v) for k,v in p.PORTS.items()}}
        raw['next']['m_operand2']=['ite','i.stall','s.m_operand2',['ite','s.halted','s.m_operand2',
            ['ite',['and','s.w_valid','s.w_fault'],'s.m_operand2','s.x_operand2']]]
        return raw
    def test_history_projection_and_exact_capture(self):
        raw=self.source();extended,record=p.extend_operand_history(raw)
        for section in ('state','reset','next'):
            self.assertEqual({k:v for k,v in extended[section].items() if k!='ghost_m_operand1'},raw[section])
        self.assertEqual(extended['outputs'],raw['outputs']);self.assertEqual(extended['wires'],raw['wires'])
        self.assertTrue(record['physical_projection_unchanged']);self.assertTrue(record['no_feedback'])
        for bits in range(16):
            stall,halted,wvalid,wfault=[bool(bits&(1<<i)) for i in range(4)]
            env={'i.stall':stall,'s.halted':halted,'s.w_valid':wvalid,'s.w_fault':wfault,
                's.ghost_m_operand1':BV(32,7),'s.x_operand1':BV(32,13)}
            self.assertEqual(evaluate(extended['next']['ghost_m_operand1'],env),BV(32,7 if stall or halted or (wvalid and wfault) else 13))
    def test_history_rejects_feedback(self):
        raw=self.source();raw['next']['r1']='s.ghost_m_operand1'
        with self.assertRaises(ValueError):p.extend_operand_history(raw)
    def test_history_rejects_feedback_in_each_physical_section(self):
        for section,key in [('reset','r1'),('wires','bad'),('outputs','trap_pc')]:
            raw=self.source();raw[section][key]='s.ghost_m_operand1'
            with self.assertRaises(ValueError):p.extend_operand_history(raw)
    def test_history_rejects_duplicate_state(self):
        raw=self.source();extended,_=p.extend_operand_history(raw)
        with self.assertRaises(ValueError):p.extend_operand_history(raw,extended)
    def test_history_negative_hold_and_capture(self):
        raw=self.source();extended,_=p.extend_operand_history(raw)
        env={'i.stall':False,'s.halted':False,'s.w_valid':False,'s.w_fault':False,
            's.ghost_m_operand1':BV(32,7),'s.x_operand1':BV(32,13)}
        correct=extended['next']['ghost_m_operand1']
        self.assertNotEqual(evaluate(correct,env),evaluate('s.ghost_m_operand1',env))
        env['i.stall']=True
        self.assertNotEqual(evaluate(correct,env),evaluate('s.x_operand1',env))
    def test_history_rejects_arithmetic_copy(self):
        raw=self.source();raw['next']['m_operand2']=['add','s.x_operand2',p.b(32,1)]
        with self.assertRaises(ValueError):p.extend_operand_history(raw)
    def test_history_rejects_inferred_control(self):
        raw=self.source();raw['next']['m_operand2']=['ite','s.m_is_load','s.x_operand2','s.m_operand2']
        with self.assertRaises(ValueError):p.extend_operand_history(raw)
    def test_history_rejects_nonzero_source_reset(self):
        raw=self.source();raw['reset']['m_operand2']=p.b(32,1)
        with self.assertRaises(ValueError):p.extend_operand_history(raw)
    def test_history_operand_provenance_remains_explicit(self):
        arch={'pc':{'bv':32},'halted':'bool',**{'r'+str(i):{'bv':32} for i in range(32)},**r.mem.ARRAYS}
        relation=r.binding(arch,transport_view=True,operand_views=True)
        found=set();seen=set()
        def visit(e):
            if not isinstance(e,list) or id(e) in seen:return
            seen.add(id(e))
            if e[0]=='eq' and isinstance(e[1],str) and e[1] in ('impl.ghost_m_operand1','impl.m_operand2'):found.add(e[1])
            for x in e:visit(x)
        visit(relation)
        self.assertEqual(found,{'impl.ghost_m_operand1','impl.m_operand2'})

if __name__=='__main__':
    import argparse
    ap=argparse.ArgumentParser();ap.add_argument('--machine',type=Path);ap.add_argument('--canonical-bank',action='store_true');ap.add_argument('--transport-view',action='store_true');ap.add_argument('--control-invariant',action='store_true');ap.add_argument('--operand-views',action='store_true')
    args=ap.parse_args()
    if args.machine:
        raw=json.loads(args.machine.read_text())
        programs=[
            [enc_i(0x13,1,0,5),enc_i(0x13,2,1,9),enc_r(3,1,2),0x73],
            [0x10b7,enc_i(0x13,2,0,127),enc_s(1,2,0),enc_i(3,3,1,0),enc_r(4,3,2),0x100073],
            [enc_i(0x13,1,0,1),enc_b(1,1,8),0xffffffff,enc_i(0x13,2,1,4),0x73],
            [0x6f,0xffffffff],
            [0xffffffff,enc_i(0x13,1,0,1)],
            [0x10b7,enc_i(3,2,1,1,2),enc_i(0x13,3,2,1)],
        ]
        # Loop case is handled separately by shortening to a terminating JAL.
        programs[3]=[0x8000ef,0xffffffff,0x73]
        print({'programs':len(programs),'retirements':sum(audit_binding(raw,x,canonical_bank=args.canonical_bank,transport_view=args.transport_view,control_invariant=args.control_invariant,operand_views=args.operand_views) for x in programs)})
    else:unittest.main(argv=['test'])
