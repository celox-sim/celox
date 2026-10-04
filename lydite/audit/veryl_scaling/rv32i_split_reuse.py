"""D/X/M/W architectural refinement over checked finite disjoint byte memory.

The binding is inductive proof input, not an assumed theorem. Wrong-path semantic
relations are guarded by older redirect/trap; structural provenance, precise W
retirement, and all externally visible effects remain checked.
"""
import copy
import json
import os
from pathlib import Path
from audit.veryl_scaling import rv32i_split_pipeline as p
from audit.veryl_scaling import rv32i_spec as a
from audit.veryl_scaling import rv32i_reuse as mem
from audit.veryl_scaling import rv32i_control_invariants as control
from audit.veryl_scaling import cpu_memory_contract as c
from audit.veryl_scaling.cpu_memory_reuse import expand
b,eq,it,land,lor,neg=a.b,a.eq,a.it,a.land,a.lor,a.neg

def bank_after(regs, valid, action):
    return [b(32,0)]+[it(land(valid,action['rd_write'],eq(action['rd'],b(5,i))),action['rd_value'],regs[i]) for i in range(1,32)]

def sequential_frontier(prefix='impl.'):
    pc=prefix+'pc';result={}
    for stage in ('w','m','x','d'):
        result[stage]=pc
        pc=['add',pc,it(prefix+stage+'_valid',b(32,4),b(32,0))]
    result['fetch']=pc
    return result

def binding(arch,canonical_bank=False,transport_view=False,control_invariant=False,operand_views=False):
    q='impl.';regs=[q+'r'+str(i) for i in range(32)]
    wd,md,xd=[a.decode(q+st+'_ir') for st in ('w','m','x')]
    w=mem.action(q+'w_ir',q+'w_pc',regs,q+'rom',q+'data',q+'limit',q+'w_address')
    # W's payload is constrained below; this bank matches physical W forwarding.
    wr=[b(32,0)]+[it(land(q+'w_valid',neg(q+'w_fault'),q+'w_writes',eq(wd['rd'],b(5,i))),q+'w_result',regs[i]) for i in range(1,32)]
    after_w=mem.update(q+'data',q+'w_address',q+'w_data',q+'w_mask',
        land(q+'w_valid',neg(q+'w_fault'),q+'w_store'),q+'limit')
    m=mem.action(q+'m_ir',q+'m_pc',wr,q+'rom',after_w,q+'limit',q+'m_address')
    if operand_views:
        m=action_from_operands(q+'m_ir',q+'m_pc',q+'ghost_m_operand1',q+'m_operand2',q+'rom',after_w,q+'limit',q+'m_address')
    mr=[b(32,0)]+[it(land(q+'m_valid',q+'m_safe_bypass',eq(md['rd'],b(5,i))),q+'m_result',wr[i]) for i in range(1,32)]
    if transport_view:mr=wr
    killw=land(q+'w_valid',lor(q+'w_fault',q+'w_redirect'))
    killm=land(q+'m_valid',lor(m['trap'],m['redirect']))
    front=sequential_frontier()
    relation=[eq('spec.'+k,q+k) for k in arch]
    if transport_view:relation.append(p.xm_noninterference(q))
    if control_invariant:relation.append(control.invariant(q,adjacency=False))
    relation += [eq(q+'r0',b(32,0)),p.selectors(q),
        c.implies(q+'halted',land(*[neg(q+st+'_valid') for st in ('d','x','m','w')]))]
    # Captured invalid-stage nextPC may be arbitrary. Only valid W payloads
    # have architectural alignment; no invariant asserts invalid payload safety.
    relation += [eq(a.ex(1,0,q+k),b(2,0)) for k in ('pc','fetch_pc','d_pc','x_pc','m_pc','w_pc')]
    for st in ('w','m','x','d'):
        relation.append(c.implies(q+st+'_valid',c.conj([
            eq(q+st+'_pc',front[st]),
            eq(q+st+'_ir',mem.instruction(q+'rom',q+st+'_pc',q+'limit')),
            *([] if st=='w' else [eq(q+st+'_fetch_fault',neg(mem.rom_mapped(q+st+'_pc',q+'limit')))])])))
    relation.append(c.implies(neg(q+'halted'),eq(q+'fetch_pc',front['fetch'])))
    relation.append(c.implies(q+'w_valid',c.conj([
        eq(q+'w_fault',w['trap']),eq(q+'w_cause',w['cause']),eq(q+'w_tval',w['trap_value']),
        eq(q+'w_next_pc',w['next_pc']),eq(q+'w_writes',w['rd_write']),
        eq(q+'w_redirect',land(w['redirect'],neg(w['trap']))),
        c.implies(w['rd_write'],eq(q+'w_result',w['rd_value'])),
        eq(q+'w_store',land(w['dmem_write'],neg(w['trap']))),
        c.implies(lor(wd['load'],wd['store']),eq(q+'w_address',w['dmem_address'])),
        c.implies(q+'w_store',land(eq(q+'w_data',w['store_data']),eq(q+'w_mask',w['store_mask']))) ])))
    relation.append(c.implies(land(q+'w_valid',q+'w_fault'),land(neg(q+'w_writes'),neg(q+'w_store'))))
    relation.append(c.implies(land(q+'w_valid',q+'w_writes'),neg(eq(wd['rd'],b(5,0)))))
    mp=p.decode_payload(q+'m_ir');safe=land(mp['writes'],mp['legal'],eq(a.ex(1,0,q+'m_pc'),b(2,0)),neg(q+'m_fetch_fault'),
        lor(*[eq(mp['x_op'],b(7,op)) for op in (0x13,0x33,0x37,0x17)]))
    relation.append(c.implies(q+'m_valid',c.conj([
        *[eq(q+'m_'+dst,mp[src]) for dst,src in [('legal','legal'),('writes','writes'),('is_load','is_load'),('is_store','is_store'),('f3','f3')]],
        eq(q+'m_safe_bypass',safe)])))
    relation.append(c.implies(land(q+'m_valid',neg(killw)),c.conj([
        *([c.implies(md['uses1'],eq(q+'ghost_m_operand1',a.reg_read(md['rs1'],wr)))] if operand_views else []),
        c.implies(md['uses2'],eq(q+'m_operand2',a.reg_read(md['rs2'],wr))),
        c.implies(lor(md['load'],md['store']),eq(q+'m_address',m['dmem_address'])),
        c.implies(land(m['writes'],neg(md['load'])),eq(q+'m_result',m['rd_value'])),
        c.implies(md['legal'],eq(q+'m_taken',m['redirect'])),
        # next_pc folds exceptions; target relation must retain raw jump target.
        c.implies(land(md['legal'],m['redirect']),eq(q+'m_target',target(q+'m_ir',q+'m_pc',q+'ghost_m_operand1' if operand_views else a.reg_read(md['rs1'],wr)))) ])))
    relation.append(c.implies(q+'x_valid',c.conj(eq(q+k,v) for k,v in p.decode_payload(q+'x_ir').items())))
    relation.append(c.implies(land(q+'x_valid',neg(killw),neg(killm)),c.conj([
        c.implies(xd['uses1'],eq(q+'x_operand1',a.reg_read(xd['rs1'],mr))),
        c.implies(xd['uses2'],eq(q+'x_operand2',a.reg_read(xd['rs2'],mr))),
        c.implies(land(q+'m_valid',q+'m_writes',neg(q+'m_safe_bypass')),
            neg(lor(land(xd['uses1'],eq(xd['rs1'],md['rd'])),land(xd['uses2'],eq(xd['rs2'],md['rd']))))) ])))
    relation=c.conj(relation)
    return p.factor_pending_bank_reads(relation)[0] if canonical_bank else relation

def target(ir,pc,rs1):
    d=a.decode(ir)
    imm=p.decode_payload(ir)
    return it(d['jalr'],['band',['add',rs1,imm['imm_i']],b(32,0xfffffffe)],
        it(d['jal'],['add',pc,imm['imm_j']],['add',pc,imm['imm_b']]))

def action_from_operands(ir,pc,rs1,rs2,rom,data,limit,checked_address):
    """Pure ISA action; operand provenance and full EA equality stay in binding."""
    preliminary=a.build_execute(ir,pc,rs1,rs2)
    address=preliminary['dmem_address']
    return a.build_execute(ir,pc,rs1,rs2,neg(mem.rom_mapped(pc,limit)),
        mem.data_word(rom,data,checked_address,limit),mem.data_fault(address,preliminary['dmem_write'],limit))

def abstract_composition(cpu,canonical_bank=False,transport_view=False,control_invariant=False,operand_views=False):
    if canonical_bank and transport_view:raise ValueError('bank factoring and transport-view combination is not checked')
    expected_state={**p.cpu_state(),**({'ghost_m_operand1':{'bv':32}} if operand_views else {})}
    if cpu['state']!=expected_state or set(cpu['outputs'])!=set(p.PORTS): raise ValueError('split CPU interface mismatch')
    requests={k:expand(cpu['outputs'][k],cpu['wires']) for k in
        ('imem_address','dmem_address','dmem_write','write_enable','write_address','write_data','write_mask')}
    if any(c.RUNNER.references(v,'i.'+k) for v in requests.values() for k in ('imem_response','dmem_response','imem_fault','dmem_fault')):
        raise ValueError('cyclic memory request')
    updated=mem.update('s.data',requests['write_address'],requests['write_data'],requests['write_mask'],requests['write_enable'],'s.limit')
    replace={'i.imem_response':mem.instruction('s.rom',requests['imem_address'],'s.limit'),
        'i.imem_fault':neg(mem.rom_mapped(requests['imem_address'],'s.limit')),
        'i.dmem_response':mem.data_word('s.rom',updated,requests['dmem_address'],'s.limit'),
        'i.dmem_fault':mem.data_fault(requests['dmem_address'],requests['dmem_write'],'s.limit')}
    machine=c.rename(copy.deepcopy(cpu),replace)
    machine['state'].update(mem.ARRAYS);machine['reset'].update({k:'i.seed_'+k for k in mem.ARRAYS})
    machine['next'].update(rom='s.rom',data=updated,limit='s.limit')
    machine['outputs']={'retire':machine['outputs']['retire']}
    arch={'pc':{'bv':32},'halted':'bool',**{'r'+str(i):{'bv':32} for i in range(32)},**mem.ARRAYS}
    regs=['s.r'+str(i) for i in range(32)]
    ir=mem.instruction('s.rom','s.pc','s.limit');e=mem.action(ir,'s.pc',regs,'s.rom','s.data','s.limit')
    spec={'state':arch,'reset':{'pc':b(32,0),'halted':False,**{'r'+str(i):b(32,0) for i in range(32)},**{k:'i.seed_'+k for k in mem.ARRAYS}},
        'next':{'pc':e['next_pc'],'halted':e['trap'],**{'r'+str(i):it(land(e['rd_write'],eq(e['rd'],b(5,i))),e['rd_value'],regs[i]) for i in range(32)},
            'rom':'s.rom','limit':'s.limit','data':mem.update('s.data',e['dmem_address'],e['store_data'],e['store_mask'],land(e['dmem_write'],neg(e['trap'])),'s.limit')},
        'outputs':{'can_step':neg('s.halted')}}
    return {'version':2,'name':'Selected split RV32I precise finite-memory refinement','inputs':{'rst':'bool','stall':'bool',**{'seed_'+k:v for k,v in mem.ARRAYS.items()}},
        'reset_input':'rst','spec':spec,'impl':machine,'binding':binding(arch,canonical_bank,transport_view,control_invariant,operand_views),'commit':'retire','can_step':'can_step','hold_when':'i.stall',
        'progress':{'enabled':land(neg('i.stall'),neg('impl.halted')),
        'rank':it('impl.w_valid',b(3,0),it('impl.m_valid',b(3,1),it('impl.x_valid',b(3,2),it('impl.d_valid',b(3,3),b(3,4)))) )}}

class ProofSession:
    """Current-process handles only; every imported equation/tool is sealed.

    No saved report can authorize composition. Unknown/failure raises before
    handle creation. The global query preserves actual source equations.
    """
    def __init__(self,out,checker=None,frontend=None,lifter=None,conjunctive_lemmas=False):
        from audit.veryl_scaling import rv32i_memory
        self.out=Path(out);self.out.mkdir(parents=True,exist_ok=False)
        self.checker=Path(checker or p.ROOT/'../target/release/lydite').resolve()
        self.frontend=Path(frontend or p.ROOT/'../target/debug/lydite-celox-export').resolve()
        self.lifter=Path(lifter or p.ROOT/'../target/release/lydite-celox-lift').resolve()
        self.conjunctive_lemmas=bool(conjunctive_lemmas);self._handles={};self._records={};self._equation_handles={}
        self.cpu=None;self._cpu_hash=None;self._normalization=None;self._canonical_bank=False;self._bank_record=None;self._transport_view=False;self._control_invariant=False;self._dispatch_record=None;self._operand_views=False;self._history_record=None;self._proof_programs=False
        if p.sha(p.SOURCE)!=p.SELECTED_SHA256:raise ValueError('selected RTL changed')
        files=[p.ROOT/'audit/lemma_candidates/rv-delivery.lyd',p.ROOT/'audit/lemma_candidates/migrate.py',p.ROOT/'audit/lemma_candidates/validate.py',Path(__file__),Path(__file__).with_name('rv32i_split_proof_program.py'),Path(p.__file__),Path(a.__file__),Path(c.__file__),Path(mem.__file__),Path(p.base.__file__),
            Path(rv32i_memory.__file__),Path(control.__file__),Path(control.p.__file__),
            p.ROOT/'audit/veryl_scaling/rv32i_latency_variants.py',Path(control.c.registers.__file__),
            p.ROOT/'examples/build_pipeline.py',p.ROOT/'audit/veryl_scaling/cpu_memory_reuse.py',p.ROOT/'conformance/veryl-symbolic/run.py',
            self.checker,self.frontend,self.lifter,p.SOURCE]
        self._files={str(x.resolve()):p.sha(x) for x in files}
        snapshots=self.out/'source-snapshots';snapshots.mkdir()
        for path,digest in self._files.items():
            if path.endswith(('.py','.veryl','.lyd')):
                data=Path(path).read_bytes()
                if p.hashlib.sha256(data).hexdigest()!=digest:raise ValueError('source changed while snapshotting')
                (snapshots/(digest+'-'+Path(path).name)).write_bytes(data)
        self.raw=p.compile_machine(self.out/'import',self.frontend,self.lifter)
        self._files.update({str(x.resolve()):p.sha(x) for x in (self.out/'import').iterdir() if x.is_file()})
        self._raw_hash=p.base.digest(self.raw)
        self._internal=json.loads((self.out/'import/internal.json').read_text());self._internal_hash=p.base.digest(self._internal)
        self._check()

    def _check(self):
        if p.base.digest(self.raw)!=self._raw_hash:raise ValueError('imported machine changed')
        if p.base.digest(self._internal)!=self._internal_hash:raise ValueError('internal roots changed')
        if self.cpu is not None and p.base.digest(self.cpu)!=self._cpu_hash:raise ValueError('normalized machine changed')
        if any(p.sha(path)!=v for path,v in self._files.items()):raise ValueError('source, rule, tool, or artifact changed during session')

    def _prove(self,label,doc,scoped=False):
        self._check();folder=self.out/label;folder.mkdir()
        path=folder/'contract.json';path.write_text(json.dumps(doc,separators=(',',':')))
        keys=('LYDITE_SOLVER','LYDITE_CONJUNCTIVE_LEMMAS');previous={k:os.environ.get(k) for k in keys}
        if any(k.startswith('LYDITE_') and k not in keys for k in os.environ):raise ValueError('non-default solver settings forbidden')
        decompose=self.conjunctive_lemmas and not label.startswith('memory-')
        os.environ['LYDITE_SOLVER']='finite';os.environ['LYDITE_CONJUNCTIVE_LEMMAS']='1' if decompose else '0'
        command=[self.checker,path,'--out',folder/'proof']
        native_lemmas=not scoped and 'proof_programs' in doc
        if native_lemmas:command+=['--lemmas',p.ROOT/'audit/lemma_candidates/rv-delivery.lyd']
        try:report,seconds=c.execute(command,folder/'report.json',(0,1,3))
        finally:
            for k,v in previous.items():
                if v is None:os.environ.pop(k,None)
                else:os.environ[k]=v
        self._check()
        if json.loads(path.read_text())!=doc:raise ValueError('contract changed during solve')
        attempt={'status':report['status'],'document_sha256':p.base.digest(doc),'report_sha256':p.sha(folder/'report.json'),
            'native_lemma_source':'audit/lemma_candidates/rv-delivery.lyd' if native_lemmas else None,
            'seconds':seconds,'files':copy.deepcopy(self._files),'machine_sha256':self._raw_hash,'saved_records_are_certificates':False}
        c.RUNNER.write_json(folder/'attempt.json',attempt)
        if scoped:
            if doc['specs']['Contract'].get('examples'):c.validate_scoped(report)
            else:p.base.validate_lemma(report,allow_conjunctive=decompose)
        else:
            c.RUNNER.validate_report(report,None)
            for query in report['obligations']:
                if query.get('backend')=='conjunctive_lemmas' and self.conjunctive_lemmas:p.base.validate_conjunctive_obligation(query)
                elif query.get('backend') not in ('finite_bv','structural_kernel'):raise ValueError('unsupported proof backend')
        if not scoped and 'proof_programs' in doc:
            from audit.lemma_candidates.validate import validate, validate_native_rv_sources
            validate_native_rv_sources(report,p.ROOT/'audit/lemma_candidates/rv-delivery.lyd')
            coverage=validate(report,doc['proof_programs'])
            c.RUNNER.write_json(folder/'candidate-coverage.json',coverage)
        self._check()
        if json.loads(path.read_text())!=doc:raise ValueError('contract changed during solve')
        queries=report['implementation_binding']['obligations'] if scoped else report['obligations']
        record={'document_sha256':p.base.digest(doc),'report_sha256':p.sha(folder/'report.json'),'seconds':seconds,
            'aggregate_finite_work':sum(q.get('finite',{}).get('work',0)+q.get('original_attempt',{}).get('finite',{}).get('work',0)+q.get('cost',{}).get('total_child_work',0) for q in queries),
            'status':report['status']}
        self._records[label]=record;return record

    def prove_register_file(self):
        self._check()
        record=self._prove('register-file-equations',p.register_file_contract(self.raw,self._internal),True)
        handle=object();self._equation_handles[handle]={'record':record,'internal_sha256':self._internal_hash,
            'terms_sha256':p.base.digest(p.register_file_terms()),'machine_sha256':self._raw_hash}
        return handle

    def normalize_register_file(self,handle):
        self._check()
        try:record=self._equation_handles[handle]
        except (KeyError,TypeError):raise ValueError('current-session register-file equation handle required') from None
        if (record['internal_sha256']!=self._internal_hash or record['machine_sha256']!=self._raw_hash
            or record['terms_sha256']!=p.base.digest(p.register_file_terms())
            or record['record']['document_sha256']!=p.base.digest(p.register_file_contract(self.raw,self._internal))):
            raise ValueError('stale register-file equations')
        roots=list(self._internal.values())
        if len(set(roots))!=4 or any(not isinstance(v,str) or not v.startswith('w.') for v in roots):
            raise ValueError('distinct exact wire roots required')
        machine=copy.deepcopy(self.raw);terms=p.register_file_terms();guard=p.selectors()
        if any(c.RUNNER.references(expr,prefix) for expr in (guard,*terms.values())
               for prefix in ('n.','w.','o.','impl.','spec.','i.')):
            raise ValueError('register-file equations must be pure original prestate expressions')
        for name,root in self._internal.items():
            machine['wires'][root[2:]]=it(guard,terms[name],copy.deepcopy(self.raw['wires'][root[2:]]))
        self.cpu=machine;self._cpu_hash=p.base.digest(machine)
        self._normalization={'rule':'exact-guarded-original-register-file-root-rewrite-v1',
            'original_machine_sha256':self._raw_hash,'normalized_machine_sha256':self._cpu_hash,
            'guard_preserved':True,'dependency':copy.deepcopy(record),'saved_records_are_certificates':False}
        c.RUNNER.write_json(self.out/'normalized-machine.json',machine)
        c.RUNNER.write_json(self.out/'normalization.json',self._normalization)
        return machine

    def prove_stage_equations(self):
        """Mint stage handles only from freshly checked all-encoding documents."""
        handles=[]
        for stage,builder in [('x',p.x_execution_contract),('m',p.m_execution_contract)]:
            label=stage+'-execution'
            if label not in self._records:self._prove(label,builder(self.raw),True)
            record=self._records[label]
            if record['document_sha256']!=p.base.digest(builder(self.raw)):raise ValueError('stale stage proof')
            handle=object();self._equation_handles[handle]={'stage':stage,'record':copy.deepcopy(record),'machine_sha256':self._raw_hash}
            handles.append(handle)
        return handles

    def normalize_stage_equations(self,handles,stage_scope=('x','m'),stage_fields=None):
        """Exact guarded next/output replacements, retaining every precondition.

        stage_fields optionally selects a nonempty exact subset of sequential
        fields belonging to stage_scope. Complete theorem coverage is still
        validated; unselected fields retain their original expressions.

        M's universally quantified proof operands are instantiated with the
        architectural bank after W. Their semantic consistency is NOT assumed:
        the full proved M payload guard remains in every replacement.
        """
        self._check();records=[]
        if not stage_scope or len(set(stage_scope))!=len(stage_scope) or set(stage_scope)-{'x','m'}:raise ValueError('invalid stage scope')
        if stage_fields is not None:
            if not stage_fields or len(set(stage_fields))!=len(stage_fields):raise ValueError('invalid stage field subset')
            stage_fields=tuple(stage_fields)
        for handle in handles:
            try:records.append(self._equation_handles[handle])
            except (KeyError,TypeError):raise ValueError('current-session stage equation handles required') from None
        if len(records)!=2 or {x.get('stage') for x in records}!={'x','m'}:raise ValueError('complete distinct stage handles required')
        machine=copy.deepcopy(self.cpu if self.cpu is not None else self.raw)
        wd=a.decode('s.w_ir');md=a.decode('s.m_ir')
        wr=[b(32,0)]+[it(land('s.w_valid',neg('s.w_fault'),'s.w_writes',eq(wd['rd'],b(5,i))),'s.w_result','s.r'+str(i)) for i in range(1,32)]
        rename={'i.pre_'+k:'s.'+k for k in self.raw['state']}
        rename.update({'i.proof_operand1':a.reg_read(md['rs1'],wr),'i.proof_operand2':a.reg_read(md['rs2'],wr)})
        interned={}
        def share(expr):
            if not isinstance(expr,list):return expr
            value=[expr[0],*[share(x) for x in expr[1:]]]
            if expr[0]=='bv':return value
            key=json.dumps(value,separators=(',',':'))
            if key not in interned:
                name='split_stage_canonical_'+str(len(interned))
                if name in machine['wires']:raise ValueError('canonical name collision')
                machine['wires'][name]=value;interned[key]='w.'+name
            return interned[key]
        coverage=[]
        def walk(expr,guards):
            if expr is True:return
            if isinstance(expr,list) and expr[0]=='and':
                for part in expr[1:]:yield from walk(part,guards)
            elif isinstance(expr,list) and expr[0]=='implies':yield from walk(expr[2],[*guards,expr[1]])
            else:yield guards,expr
        for record in records:
            builder=p.x_execution_contract if record['stage']=='x' else p.m_execution_contract
            doc=builder(self.raw)
            if record['machine_sha256']!=self._raw_hash or record['record']['document_sha256']!=p.base.digest(doc):raise ValueError('stale stage document')
            seen=set()
            for guards,term in walk(doc['specs']['Contract']['operations']['tick'],[]):
                if not (isinstance(term,list) and term[0]=='eq' and isinstance(term[1],str) and term[1].startswith('n.')):
                    raise ValueError('stage normalization needs an exact scalar equality')
                field=term[1][2:]
                if field in seen:raise ValueError('duplicate stage equality')
                seen.add(field)
                part='next' if field in self.raw['next'] else 'outputs'
                if field not in self.raw[part]:raise ValueError('unbound stage equality')
                guard=c.rename(c.conj(guards),rename);value=c.rename(term[2],rename)
                if any(c.RUNNER.references(x,prefix) for x in (guard,value) for prefix in ('n.','i.pre_','i.proof_','w.')):raise ValueError('derived equation dependency')
                # Request pins must remain response-independent. Their equations
                # are checked above but only sequential fields are rewritten.
                eligible=part=='next' and record['stage'] in stage_scope
                applied=eligible and (stage_fields is None or field in stage_fields)
                if eligible:
                    candidate=share(it(guard,value,copy.deepcopy(machine[part][field])))
                    if applied:machine[part][field]=candidate
                coverage.append({'stage':record['stage'],'part':part,'field':field,'applied':applied,'guard_sha256':p.base.digest(guard),'value_sha256':p.base.digest(value)})
            if seen!=set(doc['implementation']['state']):raise ValueError('incomplete stage equation coverage')
        eligible_fields={x['field'] for x in coverage if x['part']=='next' and x['stage'] in stage_scope}
        if stage_fields is not None and not set(stage_fields)<=eligible_fields:raise ValueError('unknown or mismatched-stage field subset')
        prior=copy.deepcopy(self._normalization)
        self.cpu=machine;self._cpu_hash=p.base.digest(machine)
        self._normalization={'stage_fields':list(stage_fields) if stage_fields is not None else None,'rule':'exact-guarded-original-stage-next-rewrite-v1','guard_preserved':True,
            'original_machine_sha256':self._raw_hash,'normalized_machine_sha256':self._cpu_hash,
            'prior':prior,'coverage':coverage,'dependencies':copy.deepcopy(records),'saved_records_are_certificates':False}
        c.RUNNER.write_json(self.out/'normalized-machine.json',machine);c.RUNNER.write_json(self.out/'normalization.json',self._normalization)
        return machine

    def prove_bank_equations(self):
        """Fresh algebra and source-bank consequences; no saved child authority."""
        records={}
        for kind in ('single','cascade','transport'):
            records[kind]=self._prove('bank-'+kind,p.arbitrary_bank_contract(kind),True)
        records['pending']=self._prove('bank-pending-reads',p.pending_bank_contract(self.raw),True)
        if 'retirement' not in self._records:self._prove('retirement',p.retirement_contract(self.raw),True)
        records['retirement']=copy.deepcopy(self._records['retirement'])
        if records['retirement']['document_sha256']!=p.base.digest(p.retirement_contract(self.raw)):raise ValueError('stale source-bank theorem')
        handle=object();self._equation_handles[handle]={'kind':'bank','records':records,
            'terms_sha256':p.base.digest(p.pending_bank_read_terms()),'machine_sha256':self._raw_hash}
        return handle

    def normalize_bank_equations(self,handle,bank_scope='both'):
        if bank_scope not in ('both','reads','next'):raise ValueError('invalid bank rewrite scope')
        self._check()
        try:record=self._equation_handles[handle]
        except (KeyError,TypeError):raise ValueError('current-session bank handle required') from None
        if record.get('kind')!='bank' or record['machine_sha256']!=self._raw_hash or record['terms_sha256']!=p.base.digest(p.pending_bank_read_terms()):raise ValueError('stale bank handle')
        docs={kind:p.arbitrary_bank_contract(kind) for kind in ('single','cascade','transport')}
        docs.update(pending=p.pending_bank_contract(self.raw),retirement=p.retirement_contract(self.raw))
        if set(record['records'])!=set(docs) or any(record['records'][k]['document_sha256']!=p.base.digest(doc) for k,doc in docs.items()):raise ValueError('bank equation document changed')
        machine=copy.deepcopy(self.cpu if self.cpu is not None else self.raw)
        rename={'i.pre_'+k:'s.'+k for k in self.raw['state']}
        operation=docs['retirement']['specs']['Contract']['operations']['tick']
        if operation[0]!='implies' or operation[1] is not True:raise ValueError('source bank theorem has undisclosed premises')
        def conjuncts(expr):
            if isinstance(expr,list) and expr[0]=='and':
                for x in expr[1:]:yield from conjuncts(x)
            elif expr is not True:yield expr
        expected={f'r{i}' for i in range(32)};seen=set()
        for eqn in conjuncts(operation[2]):
            if not (isinstance(eqn,list) and eqn[0]=='eq' and isinstance(eqn[1],str)):raise ValueError('unexpected retirement equation')
            field=eqn[1][2:]
            if field not in expected:continue
            if eqn[1]!='n.'+field or field in seen:raise ValueError('duplicate bank field')
            seen.add(field);value=c.rename(eqn[2],rename)
            if any(c.RUNNER.references(value,prefix) for prefix in ('n.','w.','o.','impl.','spec.','i.pre_')):raise ValueError('derived bank consequence')
            if bank_scope in ('both','next'):machine['next'][field]=value
        if seen!=expected:raise ValueError('incomplete source bank equation coverage')
        # Unlike the ISA-heavy stage rewrite, this exact update is universal;
        # no current/post invariant premise is introduced or discarded.
        self.cpu=machine;self._cpu_hash=p.base.digest(machine);self._canonical_bank=bank_scope in ('both','reads')
        self._bank_record={'rule':'fresh-exact-source-next-bank-and-pending-read-factors-v1',
            'source_machine_sha256':self._raw_hash,'machine_sha256':self._cpu_hash,
            'source_next_fields':sorted(seen) if bank_scope in ('both','next') else [],'proved_source_next_fields':sorted(seen),'scope':bank_scope,'dependency':copy.deepcopy(record),'saved_records_are_certificates':False}
        c.RUNNER.write_json(self.out/'bank-normalization.json',self._bank_record)
        c.RUNNER.write_json(self.out/'normalized-machine.json',machine)
        return machine

    def prove_dispatch_equations(self):
        record=self._prove('dispatch-payload',p.dispatch_payload_contract(self.raw),True)
        handle=object();self._equation_handles[handle]={'kind':'dispatch','record':record,
            'terms_sha256':p.base.digest(p.dispatch_payload_terms()),'machine_sha256':self._raw_hash}
        return handle

    def normalize_dispatch_equations(self,handle):
        self._check()
        try:record=self._equation_handles[handle]
        except (KeyError,TypeError):raise ValueError('current-session dispatch handle required') from None
        terms=p.dispatch_payload_terms()
        if (record.get('kind')!='dispatch' or record['machine_sha256']!=self._raw_hash
            or record['terms_sha256']!=p.base.digest(terms)
            or record['record']['document_sha256']!=p.base.digest(p.dispatch_payload_contract(self.raw))):raise ValueError('stale dispatch equation')
        if set(terms)!={'d_ir','d_pc','d_fetch_fault'}:raise ValueError('dispatch payload scope changed')
        if any(c.RUNNER.references(v,prefix) for v in terms.values() for prefix in ('n.','w.','o.','impl.','spec.')):raise ValueError('derived dispatch equation')
        machine=copy.deepcopy(self.cpu if self.cpu is not None else self.raw)
        machine['next'].update(copy.deepcopy(terms))
        self.cpu=machine;self._cpu_hash=p.base.digest(machine)
        self._dispatch_record={'rule':'exact-shared-dispatch-payload-gate-v1','dependency':copy.deepcopy(record),
            'machine_sha256':self._cpu_hash,'saved_records_are_certificates':False}
        c.RUNNER.write_json(self.out/'dispatch-normalization.json',self._dispatch_record)
        c.RUNNER.write_json(self.out/'normalized-machine.json',machine)
        return machine

    def prove_cpu(self,normalize_register_file=False,normalize_stages=False,stage_scope=('x','m'),normalize_bank=False,bank_scope='both',transport_view=False,control_invariant=False,normalize_dispatch=False,operand_views=False,stage_fields=None,checked_programs=False):
        if checked_programs and not (self.conjunctive_lemmas and normalize_register_file and normalize_stages and tuple(stage_scope)==('x',) and stage_fields is not None and set(stage_fields)=={'m_result','m_address','m_taken','m_target'} and transport_view and control_invariant and normalize_dispatch and not normalize_bank and not operand_views):raise ValueError('checked programs require the exact audited arithmetic-only configuration')
        self._proof_programs=bool(checked_programs)
        if stage_fields is not None and not normalize_stages:raise ValueError('stage fields require stage normalization')
        for label,doc in [('predecode',p.decode_contract(self.raw)),('selector',p.selector_contract(self.raw)),('x-execution',p.x_execution_contract(self.raw)),('m-execution',p.m_execution_contract(self.raw)),('retirement',p.retirement_contract(self.raw))]:
            self._prove(label,doc,True)
        if normalize_register_file:self.normalize_register_file(self.prove_register_file())
        else:self.cpu=copy.deepcopy(self.raw);self._cpu_hash=self._raw_hash
        if normalize_stages:self.normalize_stage_equations(self.prove_stage_equations(),stage_scope,stage_fields)
        if transport_view:
            if normalize_bank:raise ValueError('transport view and bank factoring cannot be combined yet')
            self._prove('xm-noninterference',p.xm_noninterference_contract(self.raw),True)
            self._prove('bank-no-match',p.arbitrary_bank_contract('no_match'),True)
            self._transport_view=True
        if control_invariant:
            self._prove('control-invariant',control.document(self.raw,adjacency=False),True)
            self._control_invariant=True
        if normalize_dispatch:self.normalize_dispatch_equations(self.prove_dispatch_equations())
        if normalize_bank:self.normalize_bank_equations(self.prove_bank_equations(),bank_scope)
        if operand_views:
            self.cpu,self._history_record=p.extend_operand_history(self.raw,self.cpu)
            self._cpu_hash=p.base.digest(self.cpu);self._operand_views=True
            c.RUNNER.write_json(self.out/'operand-history.json',self._history_record)
            c.RUNNER.write_json(self.out/'normalized-machine.json',self.cpu)
        self._prove('abstract-refinement',self._cpu_document())
        handle=object();self._handles[handle]={'kind':'cpu','machine_sha256':self._cpu_hash,'proof_programs':self._proof_programs,
            'normalization':copy.deepcopy(self._normalization),'bank_normalization':copy.deepcopy(self._bank_record),'transport_view':self._transport_view,'control_invariant':self._control_invariant,'dispatch_normalization':copy.deepcopy(self._dispatch_record),'operand_history':copy.deepcopy(self._history_record),'operand_views':self._operand_views,'records':copy.deepcopy(self._records)}
        return handle

    def _cpu_document(self):
        doc=abstract_composition(self.cpu,canonical_bank=self._canonical_bank,transport_view=self._transport_view,control_invariant=self._control_invariant,operand_views=self._operand_views)
        if getattr(self,'_proof_programs',False):
            from audit.veryl_scaling import rv32i_split_proof_program
            doc['proof_programs']=rv32i_split_proof_program.build(self.cpu,doc)
        return doc

    def prove_memory(self,count):
        from audit.veryl_scaling import rv32i_memory as m
        if count not in (4,16,64):raise ValueError('unsupported finite memory size')
        self._check();folder=self.out/('memory-'+str(count)+'-import')
        raw=m.compile_machine(count,folder,self.frontend,self.lifter)
        self._files.update({str(x.resolve()):p.sha(x) for x in folder.iterdir() if x.is_file()})
        record=self._prove('memory-'+str(count),m.memory_contract(count,raw),True)
        handle=object();self._handles[handle]={'kind':'memory','count':count,'raw':raw,'machine_sha256':p.base.digest(raw),'record':record}
        return handle

    def compose(self,cpu_handle,memory_handle,count,connections=None):
        from audit.veryl_scaling import rv32i_memory as m
        self._check()
        try:cpu=self._handles[cpu_handle];memory=self._handles[memory_handle]
        except (KeyError,TypeError):raise ValueError('current-session verified handles required') from None
        if cpu.get('kind')!='cpu' or memory.get('kind')!='memory' or memory['count']!=count:raise ValueError('mismatched proof dependencies')
        if cpu.get('proof_programs',False)!=getattr(self,'_proof_programs',False):raise ValueError('stale proof-program configuration')
        if cpu.get('operand_views')!=self._operand_views or cpu.get('operand_history')!=self._history_record:raise ValueError('stale operand history')
        if cpu.get('control_invariant')!=self._control_invariant or cpu.get('dispatch_normalization')!=self._dispatch_record:raise ValueError('stale control/dispatch dependency')
        if cpu.get('transport_view')!=self._transport_view:raise ValueError('stale transport view')
        if cpu.get('bank_normalization')!=self._bank_record:raise ValueError('stale bank normalization record')
        if cpu['records']['abstract-refinement']['document_sha256']!=p.base.digest(self._cpu_document()):raise ValueError('stale architectural theorem')
        if cpu['machine_sha256']!=p.base.digest(self.cpu) or memory['machine_sha256']!=p.base.digest(memory['raw']):raise ValueError('stale machine')
        expected={'cpu.rst':'system.rst','memory.rst':'system.rst','cpu.stall':'system.stall',
            **{'memory.'+k:'cpu.'+k for k in m.memory_inputs(count) if k!='rst' and not k.startswith('seed_')},
            **{'cpu.'+k:'memory.'+k for k in m.PORTS}}
        if connections is not None and connections!=expected:raise ValueError('connection graph mismatch')
        for k,ty in m.memory_inputs(count).items():
            if k!='rst' and not k.startswith('seed_') and p.PORTS.get(k)!=ty:raise ValueError('request type mismatch')
        if any(p.INPUTS.get(k)!=ty for k,ty in m.PORTS.items()):raise ValueError('response type mismatch')
        # Instantiate total arrays; unmapped backing cells never bypass full map checks.
        def array(kind,seeds=False):
            value=['const_mem',7,b(32,0)]
            for index in range(count):value=['write',value,b(7,index),'memory.'+('seed_' if seeds else '')+kind+str(index)]
            return value
        return {'status':'checked_split_rv32i_finite_memory_composition','capacity':count,'connections':expected,
            'instantiation':{'limit':b(7,count),**{kind:array(kind) for kind in ('rom','data')},**{'seed_'+kind:array(kind,True) for kind in ('rom','data')}},
            'source_sha256':p.SELECTED_SHA256,'files':copy.deepcopy(self._files),'cpu':copy.deepcopy(cpu),'memory':{k:v for k,v in memory.items() if k!='raw'},
            'saved_records_are_certificates':False,
            'rule':'Instantiate total seven-bit arrays at finite capacity with full32-bit ROM/RAM permission guards. Imported memory proves exact atomic byte-mask W update and same-edge write-through read; the checked global original-RTL refinement uses that same W-before-M response and common synchronous reset.'}

if __name__=='__main__':
    import argparse
    ap=argparse.ArgumentParser();ap.add_argument('--out',type=Path,required=True);ap.add_argument('--checker',type=Path)
    ap.add_argument('--operand-views',action='store_true');ap.add_argument('--control-invariant',action='store_true');ap.add_argument('--normalize-dispatch',action='store_true');ap.add_argument('--transport-view',action='store_true');ap.add_argument('--normalize-bank',action='store_true');ap.add_argument('--normalize-stages',action='store_true');ap.add_argument('--normalize-register-file',action='store_true');ap.add_argument('--conjunctive-lemmas',action='store_true');ap.add_argument('--sizes',type=int,nargs='+',choices=(4,16,64),default=(4,16,64))
    args=ap.parse_args();session=ProofSession(args.out,args.checker,conjunctive_lemmas=args.conjunctive_lemmas)
    cpu=session.prove_cpu(normalize_register_file=args.normalize_register_file,normalize_stages=args.normalize_stages,normalize_bank=args.normalize_bank,transport_view=args.transport_view,control_invariant=args.control_invariant,normalize_dispatch=args.normalize_dispatch,operand_views=args.operand_views);result=[session.compose(cpu,session.prove_memory(n),n) for n in args.sizes]
    c.RUNNER.write_json(args.out/'composition.json',result);print(json.dumps(result,indent=2))
