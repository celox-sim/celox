"""Source-bound D/X/M/W RV32I proof construction.

No theorem is implied by constructing a document. The original imported RTL
transition is retained; this module does not normalize or replace DUT equations.
"""
import copy
import hashlib
import json
import re
from pathlib import Path
from audit.veryl_scaling import rv32i_pipeline as base
from audit.veryl_scaling import rv32i_spec as a
from audit.veryl_scaling import cpu_memory_contract as c

ROOT = base.ROOT
INPUTS, PORTS = base.INPUTS, base.PORTS
SOURCE = ROOT / 'conformance/veryl-symbolic/rv32i_pipeline_split_onehot_candidate.veryl'
TOP = 'RV32IPipelineSplitOnehotCandidate'
INTERNALS=('rf_operand1','rf_operand2','operand1','operand2')
SELECTED_SHA256 = 'b5c634c60b31c07313f5c54b069b5428013fa162b63f96e24cbb0f56f74722ab'
b, eq, it, land, lor, neg = a.b, a.eq, a.it, a.land, a.lor, a.neg

def source(): return SOURCE.read_text()
def sha(path): return hashlib.sha256(Path(path).read_bytes()).hexdigest()

def cpu_state(text=None):
    text = source() if text is None else text
    seq = text.split('always_ff (clk) {', 1)[1]
    names = set(re.findall(r'\b(\w+)\s*=(?!=)', seq.split('} else if', 1)[0]))
    if names != set(re.findall(r'\b(\w+)\s*=(?!=)', seq)):
        raise ValueError('every sequential field must have an explicit reset')
    types = {k: {'bv': int(w)} if w else 'bool' for k, w in
             re.findall(r'\b(\w+)\s*:\s*(?:output\s+)?bit(?:<(\d+)>)?', text)}
    if names - types.keys(): raise ValueError('unknown state types')
    return {k: types[k] for k in sorted(names)}

def compile_machine(out, frontend=None, lifter=None, text=None):
    frontend = frontend or ROOT / 'conformance/veryl-proof/target/debug/veryl-proof-frontend'
    lifter = lifter or ROOT / 'target/release/hwverify-sir-lift'
    text = source() if text is None else text
    out = Path(out); out.mkdir(parents=True, exist_ok=False)
    (out / 'source.veryl').write_text(text)
    c.RUNNER.write_json(out / 'design.json', {'top': TOP, 'four_state': False,
        'sources': [{'path': 'source.veryl', 'text': text}]})
    compiled, _ = c.execute([frontend, out / 'design.json'], out / 'compiled.json')
    if compiled.get('allowed_diagnostics') != []: raise ValueError('unwaived frontend diagnostics')
    state = cpu_state(text)
    cfg = {'event': 'clk', 'inputs': {'rst_n': {'name': 'rst', 'type': 'bool', 'expr': ['not', 'i.rst']},
           **{k: {'type': v} for k,v in INPUTS.items() if k != 'rst'}},
           'state': {k: {'type': v} for k,v in state.items()}}
    raw = {'state': state}
    for mode, reset in [('normal', False), ('reset', True)]:
        cfg['overrides'] = {'rst': reset}
        cfg['outputs'] = {} if reset else {**{k: {'signal': k, 'type': ty} for k,ty in PORTS.items()},
            **{'proof_'+k:{'signal':k,'type':{'bv':32}} for k in INTERNALS}}
        c.RUNNER.write_json(out / (mode + '-bindings.json'), cfg)
        lifted, _ = c.execute([lifter, out / 'compiled.json', out / (mode + '-bindings.json')]
            + (['--inline'] if reset else []), out / (mode + '-lift.json'))
        if reset:
            if any(c.RUNNER.references(v, prefix) for v in lifted['next'].values() for prefix in ('s.', 'w.')):
                raise ValueError('reset depends on arbitrary prestate')
            raw['reset'] = lifted['next']
        else:
            raw.update({k: lifted[k] for k in ('next','wires')})
            raw['outputs']={k:lifted['outputs'][k] for k in PORTS}
            c.RUNNER.write_json(out/'internal.json',{k:lifted['outputs']['proof_'+k] for k in INTERNALS})
    c.RUNNER.write_json(out / 'machine.json', raw)
    return raw

def decode_payload(ir):
    """Independent registered predecode equations, including illegal opcodes."""
    d = a.decode(ir); op = a.ex(6,0,ir)
    return {'x_op': op, 'f3': a.ex(14,12,ir), 'f7': a.ex(31,25,ir),
        'legal': d['legal'], 'writes': a.build_execute(ir, b(32,0), b(32,0), b(32,0))['writes'],
        'is_load': eq(op,b(7,3)), 'is_store': eq(op,b(7,0x23)),
        'is_branch': eq(op,b(7,0x63)), 'is_jump': lor(eq(op,b(7,0x6f)),eq(op,b(7,0x67))),
        'is_alu_imm': eq(op,b(7,0x13)),
        'imm_i': a.sext(32,a.ex(31,20,ir)),
        'imm_s': a.sext(32,a.cat(a.ex(31,25,ir),a.ex(11,7,ir))),
        'imm_b': a.sext(32,a.cat(a.ex(31,31,ir),a.ex(7,7,ir),a.ex(30,25,ir),a.ex(11,8,ir),b(1,0))),
        'imm_u': a.cat(a.ex(31,12,ir),b(12,0)),
        'imm_j': a.sext(32,a.cat(a.ex(31,31,ir),a.ex(19,12,ir),a.ex(20,20,ir),a.ex(30,21,ir),b(1,0)))}

def selectors(prefix='s.'):
    return c.conj(eq(prefix + 'd_sel' + str(i), ['shl', b(32,1), a.zext(32,a.ex(19,15,prefix+'d_ir') if i==1 else a.ex(24,20,prefix+'d_ir'))]) for i in (1,2))

def combinational_document(raw, actual, expected, guard=True, name='split source lemma', extra_inputs=None):
    """Universal prestate lemma over actual imported expressions; no assumptions."""
    rename = {'s.'+k:'i.pre_'+k for k in raw['state']}
    state = {k: ty for k,(ty,_) in actual.items()}
    machine = {'state':state,'reset':{k:c.zero(ty) for k,ty in state.items()},
        'next':c.rename({k:v for k,(_,v) in actual.items()},rename),
        'wires':c.rename(raw['wires'],rename)}
    return c.scoped_document(name,state,{**INPUTS,**{'pre_'+k:ty for k,ty in raw['state'].items()},**(extra_inputs or {})},{},
        c.conj(eq('s.'+k,c.zero(ty)) for k,ty in state.items()),True,
        c.rename(c.implies(guard,c.conj(expected)),rename),machine)

def decode_contract(raw):
    advance = land(neg('i.stall'),neg('s.halted'),neg(land('s.w_valid','s.w_fault')))
    actual = {k:(raw['state'][k],raw['next'][k]) for k in decode_payload('s.d_ir')}
    expected = [eq('n.'+k,it(advance,v,'s.'+k)) for k,v in decode_payload('s.d_ir').items()]
    return combinational_document(raw,actual,expected,name='Split independent complete D predecode')

def selector_contract(raw):
    machine=copy.deepcopy(raw);machine.pop('outputs')
    initial=c.conj(eq('s.'+k,v) for k,v in raw['reset'].items())
    return c.scoped_document('Split selector identity and reset',raw['state'],INPUTS,{},initial,
                            selectors(),True,machine)

def retirement_contract(raw):
    active=land(neg('i.stall'),neg('s.halted'))
    retire=land(active,'s.w_valid');trap=land(retire,'s.w_fault');commit=land(retire,neg('s.w_fault'))
    expected_ports={'retire':retire,'commit':commit,'trap_valid':trap,
        'trap_pc':'s.w_pc','trap_cause':'s.w_cause','trap_value':'s.w_tval',
        'write_enable':land(commit,'s.w_store'),'write_address':'s.w_address',
        'write_data':'s.w_data','write_mask':'s.w_mask','imem_address':'s.fetch_pc'}
    actual={k:(PORTS[k],raw['outputs'][k]) for k in expected_ports}
    expected=[eq('n.'+k,v) for k,v in expected_ports.items()]
    actual.update({k:(raw['state'][k],raw['next'][k]) for k in ('pc','halted',*[f'r{i}' for i in range(32)])})
    expected += [eq('n.pc',it(retire,it('s.w_fault','s.w_pc','s.w_next_pc'),'s.pc')),
        eq('n.halted',lor('s.halted',trap)),eq('n.r0','s.r0')]
    rd=a.ex(11,7,'s.w_ir')
    expected += [eq('n.r'+str(i),it(land(commit,'s.w_writes',eq(rd,b(5,i))),'s.w_result','s.r'+str(i))) for i in range(1,32)]
    return combinational_document(raw,actual,expected,name='Split W retirement and precise trap pin consistency')

def instruction_cases():
    return tuple(k for k in a.decode('i.ir') if k not in ('rd','rs1','rs2','legal','load','store','branch','uses1','uses2'))+('illegal',)

def case_guard(decoded,instruction):
    if instruction not in (None,*instruction_cases()):raise ValueError('unknown instruction case')
    return True if instruction is None else neg(decoded['legal']) if instruction=='illegal' else decoded[instruction]

def m_semantic_relation(ir,pc,rs1,rs2):
    """Payload semantics needed by M; complete fault priority is proved later."""
    from audit.veryl_scaling.rv32i_split_reuse import target
    d=a.decode(ir);dec=decode_payload(ir)
    e=a.build_execute(ir,pc,rs1,rs2,'s.m_fetch_fault','i.dmem_response','i.dmem_fault')
    return c.conj([
        *[eq('s.m_'+dst,dec[src]) for dst,src in [('legal','legal'),('writes','writes'),('is_load','is_load'),('is_store','is_store'),('f3','f3')]],
        c.implies(d['uses2'],eq('s.m_operand2',rs2)),
        c.implies(lor(d['load'],d['store']),eq('s.m_address',e['dmem_address'])),
        c.implies(land(e['writes'],neg(d['load'])),eq('s.m_result',e['rd_value'])),
        c.implies(d['legal'],eq('s.m_taken',e['redirect'])),
        c.implies(land(d['legal'],e['redirect']),eq('s.m_target',target(ir,pc,rs1)))])

def m_execution_contract(raw,instruction=None):
    ir,pc='s.m_ir','s.m_pc';rs1,rs2='i.proof_operand1','i.proof_operand2'
    e=a.build_execute(ir,pc,rs1,rs2,'s.m_fetch_fault','i.dmem_response','i.dmem_fault');d=a.decode(ir)
    guard=land(neg('i.stall'),neg('s.halted'),'s.m_valid',
        neg(land('s.w_valid',lor('s.w_fault','s.w_redirect'))),
        m_semantic_relation(ir,pc,rs1,rs2),case_guard(d,instruction))
    values={'w_valid':True,'w_pc':pc,'w_ir':ir,'w_fault':e['trap'],'w_cause':e['cause'],'w_tval':e['trap_value'],
        'w_next_pc':e['next_pc'],'w_writes':e['rd_write'],'w_store':land(d['store'],neg(e['trap'])),
        'w_redirect':land(e['redirect'],neg(e['trap']))}
    selected=(*values,'w_result','w_address','w_data','w_mask')
    actual={k:(raw['state'][k],raw['next'][k]) for k in selected}
    actual.update({k:(PORTS[k],raw['outputs'][k]) for k in ('dmem_valid','dmem_address','dmem_write')})
    expected=[eq('n.'+k,v) for k,v in values.items()]
    expected += [c.implies(e['rd_write'],eq('n.w_result',e['rd_value'])),
        c.implies(lor(d['load'],d['store']),eq('n.w_address',e['dmem_address'])),
        c.implies(land(d['store'],neg(e['trap'])),land(eq('n.w_data',e['store_data']),eq('n.w_mask',e['store_mask']))),
        eq('n.dmem_valid',e['dmem_valid']),
        c.implies(e['dmem_valid'],land(eq('n.dmem_address',e['dmem_address']),eq('n.dmem_write',e['dmem_write'])))]
    doc=combinational_document(raw,actual,expected,guard,'Split M instruction semantics '+str(instruction or 'all'),
        {'proof_operand1':{'bv':32},'proof_operand2':{'bv':32}})
    return doc

def x_execution_contract(raw,instruction=None):
    from audit.veryl_scaling.rv32i_split_reuse import target
    ir,pc='s.x_ir','s.x_pc';d=a.decode(ir);dec=decode_payload(ir)
    e=a.build_execute(ir,pc,'s.x_operand1','s.x_operand2','s.x_fetch_fault')
    guard=land(neg('i.stall'),neg('s.halted'),'s.x_valid',
        neg(land('s.w_valid',lor('s.w_fault','s.w_redirect'))),
        c.conj(eq('s.'+k,v) for k,v in dec.items()),case_guard(d,instruction))
    safe=land(e['writes'],d['legal'],eq(a.ex(1,0,pc),b(2,0)),neg('s.x_fetch_fault'),
        lor(*[eq(dec['x_op'],b(7,op)) for op in (0x13,0x33,0x37,0x17)]))
    values={'m_valid':True,'m_pc':pc,'m_ir':ir,'m_fetch_fault':'s.x_fetch_fault','m_operand2':'s.x_operand2',
        **{'m_'+dst:dec[src] for dst,src in [('legal','legal'),('writes','writes'),('is_load','is_load'),('is_store','is_store'),('f3','f3')]},
        'm_safe_bypass':safe}
    actual={k:(raw['state'][k],raw['next'][k]) for k in (*values,'m_result','m_address','m_target','m_taken')}
    expected=[eq('n.'+k,v) for k,v in values.items()]+[
        c.implies(land(e['writes'],neg(d['load'])),eq('n.m_result',e['rd_value'])),
        c.implies(lor(d['load'],d['store']),eq('n.m_address',e['dmem_address'])),
        c.implies(d['legal'],eq('n.m_taken',e['redirect'])),
        c.implies(land(d['legal'],e['redirect']),eq('n.m_target',target(ir,pc,'s.x_operand1')))]
    return combinational_document(raw,actual,expected,guard,'Split X instruction semantics '+str(instruction or 'all'))

MUTATIONS={
    'wrong_add':("3'd0: result = x_operand1 + rhs;","3'd0: result = x_operand1 + rhs + 32'd1;",'x','addi'),
    'wrong_sub':('result = x_operand1 - rhs;','result = x_operand1 + rhs;','x','sub'),
    'wrong_shift':('shift = rhs[4:0];','shift = rhs[5:1];','x','sll'),
    'wrong_jalr_mask':("target = (x_operand1 + imm_i) & 32'hfffffffe;","target = (x_operand1 + imm_i) & 32'hfffffffc;",'x','jalr'),
    'wrong_byte_lane':("store_mask = 4'd1 << m_address[1:0];","store_mask = 4'd1;",'m','sb'),
    'unsigned_load_sign':("if m_f3 == 3'd0 && shifted_load[7]","if shifted_load[7]",'m','lbu'),
    'load_x0_drops_fault':('else if (m_is_load || m_is_store) && dmem_fault','else if (m_is_load || m_is_store) && dmem_fault && m_rd != 5\'d0','m','lw'),
    'early_store':('write_enable = commit && w_store;','write_enable = m_valid && m_is_store;','retirement',None),
    'wrong_ecall_cause':("else if m_ir == 32'h00000073 { fault = 1; cause = 4'd8; }","else if m_ir == 32'h00000073 { fault = 1; cause = 4'd3; }",'m','ecall'),
}

def check_mutations(out,checker,frontend=None,lifter=None):
    out=Path(out);out.mkdir(parents=True,exist_ok=False);results=[]
    for label,(before,after,stage,instruction) in MUTATIONS.items():
        text=source()
        if text.count(before)!=1:raise ValueError('nonunique mutation '+label)
        raw=compile_machine(out/(label+'-import'),frontend,lifter,text.replace(before,after))
        doc=retirement_contract(raw) if stage=='retirement' else (x_execution_contract if stage=='x' else m_execution_contract)(raw,instruction)
        folder=out/label;folder.mkdir();path=folder/'contract.json';path.write_text(json.dumps(doc,separators=(',',':')))
        report,seconds=c.execute([checker,path,'--out',folder/'proof'],folder/'report.json',(0,1,3))
        base.validate_negative_lemma(report)
        results.append({'mutation':label,'source_sha256':sha(out/(label+'-import')/'source.veryl'),
            'document_sha256':base.digest(doc),'report_sha256':sha(folder/'report.json'),'seconds':seconds,
            'status':'original_formula_validated_counterexample'})
    c.RUNNER.write_json(out/'summary.json',results);return results

def register_file_terms():
    d=a.decode('s.d_ir');m=a.decode('s.m_ir');w=a.decode('s.w_ir');terms={}
    for n in (1,2):
        index=d['rs'+str(n)];value=a.reg_read(index)
        terms['rf_operand'+str(n)]=value
        mhit=land('s.m_valid','s.m_safe_bypass',eq(m['rd'],index))
        whit=land('s.w_valid','s.w_writes',neg('s.w_fault'),eq(w['rd'],index))
        terms['operand'+str(n)]=it(mhit,'s.m_result',it(whit,'s.w_result',value))
    return terms

def register_file_contract(raw,internal,fields=None):
    fields=tuple(INTERNALS) if fields is None else tuple(fields)
    if not fields or any(k not in INTERNALS for k in fields):raise ValueError('unknown register-file root')
    terms=register_file_terms()
    actual={k:({'bv':32},internal[k]) for k in fields}
    # Exact BV32 equality by all 32 bit equalities, no omitted bit or field.
    expected=[eq(a.ex(bit,bit,'n.'+k),a.ex(bit,bit,terms[k])) for k in fields for bit in range(32)]
    return combinational_document(raw,actual,expected,selectors(),'Split exact register-file and forwarding roots')

def bank_update(values,enable,destination,value):
    """Pure architectural bank update: register zero is always zero."""
    if len(values)!=32:raise ValueError('bank requires exactly 32 registers')
    return [b(32,0)]+[it(land(enable,eq(destination,b(5,i))),value,values[i]) for i in range(1,32)]

def bank_forward(query,base,enable,destination,value):
    return it(land(enable,neg(eq(query,b(5,0))),eq(destination,query)),value,base)

def arbitrary_bank_contract(kind='single'):
    """Universal finite bank algebra, independent of ISA/pipeline invariants.

    Transport drops a speculative writer only under an explicit no-match guard;
    safe M bypass additionally requires the full M write/value to agree. No
    source premise or poststate invariant is invented by this arithmetic lemma.
    """
    if kind not in ('single','cascade','transport','no_match'):raise ValueError('unknown bank theorem')
    inputs={'rst':'bool',**{'r'+str(i):{'bv':32} for i in range(32)},'q':{'bv':5},
        **{'g'+s:'bool' for s in ('w','m','x','safe')},
        **{'d'+s:{'bv':5} for s in ('w','m','x')},
        **{'v'+s:{'bv':32} for s in ('w','m','x','safe')}}
    regs=['i.r'+str(i) for i in range(32)];q='i.q'
    w=bank_update(regs,'i.gw','i.dw','i.vw')
    m=bank_update(w,'i.gm','i.dm','i.vm')
    x=bank_update(m,'i.gx','i.dx','i.vx')
    baseline=a.reg_read(q,regs)
    forward_w=bank_forward(q,baseline,'i.gw','i.dw','i.vw')
    forward_m=bank_forward(q,forward_w,'i.gm','i.dm','i.vm')
    guard=True
    if kind=='single':actual=a.reg_read(q,w);expected=forward_w
    elif kind=='no_match':actual=a.reg_read(q,w);expected=baseline;guard=c.implies('i.gw',neg(eq(q,'i.dw')))
    elif kind=='cascade':actual=a.reg_read(q,m);expected=forward_m
    else:
        actual=a.reg_read(q,x)
        expected=a.reg_read(q,bank_update(w,'i.gsafe','i.dm','i.vsafe'))
        guard=land(c.implies('i.gsafe',land('i.gm',eq('i.vm','i.vsafe'))),
            c.implies(land('i.gm',neg('i.gsafe')),neg(eq(q,'i.dm'))),
            c.implies('i.gx',neg(eq(q,'i.dx'))))
    state={'read':{'bv':32}}
    machine={'state':state,'reset':{'read':b(32,0)},'next':{'read':actual},'wires':{}}
    operation=c.implies(guard,c.conj(eq(a.ex(bit,bit,'n.read'),a.ex(bit,bit,expected)) for bit in range(32)))
    return c.scoped_document('Independent exact arbitrary-bank '+kind,state,inputs,{},eq('s.read',b(32,0)),True,operation,machine)

def pending_bank_read_terms(prefix='s.'):
    """Exact read expressions used by the pending-stage binding, plus factors."""
    q=prefix;regs=[q+'r'+str(i) for i in range(32)]
    wd,md,xd=[a.decode(q+stage+'_ir') for stage in ('w','m','x')]
    gw=land(q+'w_valid',neg(q+'w_fault'),q+'w_writes')
    gm=land(q+'m_valid',q+'m_safe_bypass')
    # Keep these exact original binding shapes, including their x0 clamp.
    wr=[b(32,0)]+[it(land(q+'w_valid',neg(q+'w_fault'),q+'w_writes',eq(wd['rd'],b(5,i))),q+'w_result',regs[i]) for i in range(1,32)]
    mr=[b(32,0)]+[it(land(q+'m_valid',q+'m_safe_bypass',eq(md['rd'],b(5,i))),q+'m_result',wr[i]) for i in range(1,32)]
    terms={}
    for stage,d,bank in [('m',md,wr),('x',xd,mr)]:
        for n in (1,2):
            index=d['rs'+str(n)]
            value=bank_forward(index,a.reg_read(index,regs),gw,wd['rd'],q+'w_result')
            if stage=='x':value=bank_forward(index,value,gm,md['rd'],q+'m_result')
            terms[stage+'_source'+str(n)]=(a.reg_read(index,bank),value)
    return terms

def pending_bank_contract(raw):
    terms=pending_bank_read_terms()
    actual={k:({'bv':32},v[0]) for k,v in terms.items()}
    expected=[eq(a.ex(bit,bit,'n.'+k),a.ex(bit,bit,v[1])) for k,v in terms.items() for bit in range(32)]
    return combinational_document(raw,actual,expected,name='Exact current-prestate pending-bank read factors')

def factor_pending_bank_reads(expression,prefix='impl.'):
    """Syntactic equality substitution only, with complete matched-root coverage."""
    terms=pending_bank_read_terms(prefix);counts={k:0 for k in terms};memo={}
    def visit(expr):
        if not isinstance(expr,list):return expr
        if id(expr) in memo:return memo[id(expr)]
        if expr[0]=='ite':
            for name,(before,after) in terms.items():
                if expr==before:
                    counts[name]+=1;memo[id(expr)]=after;return after
        value=[visit(x) for x in expr];memo[id(expr)]=value;return value
    result=visit(expression)
    if not all(counts.values()):raise ValueError('pending bank expression missing from binding: '+str(counts))
    return result,counts

def xm_noninterference(prefix='s.'):
    q=prefix;op=a.ex(6,0,q+'x_ir');xd=a.decode(q+'x_ir');md=a.decode(q+'m_ir')
    uses1=lor(*(eq(op,b(7,n)) for n in (0x13,0x33,0x67,3,0x23,0x63)))
    uses2=lor(*(eq(op,b(7,n)) for n in (0x33,0x23,0x63)))
    return c.implies(land(q+'x_valid',q+'m_valid',q+'m_writes'),
        neg(lor(land(uses1,eq(xd['rs1'],md['rd'])),land(uses2,eq(xd['rs2'],md['rd'])))))

def xm_noninterference_contract(raw):
    """Inductive source invariant: D waits for ANY X writer before admission.

    The retained fields keep their actual reset/next expressions. All other
    prestate is universally quantified input, overapproximating the full CPU.
    This theorem does not assume an ISA-valid packet or a bank postcondition.
    """
    selected=('x_valid','m_valid','x_ir','m_ir','m_writes')
    rename={'s.'+k:'i.pre_'+k for k in raw['state'] if k not in selected}
    state={k:raw['state'][k] for k in selected}
    machine={'state':state,'reset':{k:raw['reset'][k] for k in selected},
        'next':c.rename({k:raw['next'][k] for k in selected},rename),'wires':c.rename(raw['wires'],rename)}
    inputs={**INPUTS,**{'pre_'+k:v for k,v in raw['state'].items() if k not in selected}}
    initial=c.conj(eq('s.'+k,raw['reset'][k]) for k in selected)
    return c.scoped_document('Actual split X/M RAW noninterference induction',state,inputs,{},initial,xm_noninterference(),True,machine)

def dispatch_payload_terms():
    """Exact D capture gate, independent of execution arithmetic and ISA data."""
    d=a.decode('s.d_ir');x=a.decode('s.x_ir');m=a.decode('s.m_ir');op=a.ex(6,0,'s.d_ir')
    uses1=lor(*(eq(op,b(7,n)) for n in (0x13,0x33,0x67,3,0x23,0x63)))
    uses2=lor(*(eq(op,b(7,n)) for n in (0x33,0x23,0x63)))
    def match(rd):return lor(land(uses1,eq(rd,d['rs1'])),land(uses2,eq(rd,d['rs2'])))
    hazard=land('s.d_valid',lor(land('s.x_valid','s.writes',match(x['rd'])),
        land('s.m_valid','s.m_writes',neg('s.m_safe_bypass'),match(m['rd']))))
    gate=land(neg('i.stall'),neg('s.halted'),neg(land('s.w_valid','s.w_fault')),neg(hazard))
    return {'d_pc':it(gate,'s.fetch_pc','s.d_pc'),'d_ir':it(gate,'i.imem_response','s.d_ir'),
        'd_fetch_fault':it(gate,'i.imem_fault','s.d_fetch_fault')}

def dispatch_payload_contract(raw):
    terms=dispatch_payload_terms();actual={k:(raw['state'][k],raw['next'][k]) for k in terms}
    expected=[]
    for k,value in terms.items():
        if raw['state'][k]=='bool':expected.append(eq('n.'+k,value))
        else:expected.extend(eq(a.ex(bit,bit,'n.'+k),a.ex(bit,bit,value)) for bit in range(32))
    return combinational_document(raw,actual,expected,name='Exact shared D instruction and PC capture gate')

def extend_operand_history(raw,machine=None):
    """Conservative deterministic observer of the actual X-to-M copy/hold.

    Only data leaves of the ORIGINAL imported m_operand2 transition are renamed.
    Its exact stall/halt/trap controls are retained; no ISA result or next-state
    expression controls the observer. Original state/reset/next/wires/ports stay
    byte-for-byte equal to the supplied proven-equivalent physical machine.
    """
    from audit.veryl_scaling.cpu_memory_reuse import expand
    machine=raw if machine is None else machine
    name='ghost_m_operand1'
    if name in raw['state'] or name in machine['state']:raise ValueError('operand history already present')
    for physical in (raw,machine):
        for section in ('next','reset','wires','outputs'):
            if any(c.RUNNER.references(v,'s.'+name) or c.RUNNER.references(v,'n.'+name) for v in physical[section].values()):
                raise ValueError('physical machine refers to operand history')
    if any(raw['state'].get(k)!={'bv':32} for k in ('x_operand1','x_operand2','m_operand2')):
        raise ValueError('operand history requires exact BV32 data fields')
    capture=expand(raw['next']['m_operand2'],raw['wires'])
    controls={'i.stall','s.halted','s.w_valid','s.w_fault'};leaves=set()
    def control_refs(expr):
        if isinstance(expr,str):
            if expr not in controls:raise ValueError('unexpected operand capture control')
        elif isinstance(expr,list):
            if expr[0] not in ('and','or','not'):raise ValueError('non-Boolean operand capture control')
            for x in expr[1:]:control_refs(x)
        elif type(expr) is not bool:raise ValueError('unexpected capture control literal')
    def copy_tree(expr):
        if isinstance(expr,str) and expr in ('s.x_operand2','s.m_operand2'):
            leaves.add(expr);return
        if not isinstance(expr,list) or len(expr)!=4 or expr[0]!='ite':raise ValueError('capture is not an exact copy/hold tree')
        control_refs(expr[1]);copy_tree(expr[2]);copy_tree(expr[3])
    copy_tree(capture)
    if leaves!={'s.x_operand2','s.m_operand2'} or raw['reset']['m_operand2']!=b(32,0):raise ValueError('incomplete copy/hold/reset source')
    ghost_next=c.rename(capture,{'s.x_operand2':'s.x_operand1','s.m_operand2':'s.'+name})
    extended=copy.deepcopy(machine)
    extended['state'][name]={'bv':32};extended['reset'][name]=b(32,0);extended['next'][name]=ghost_next
    for section in ('state','reset','next'):
        if {k:v for k,v in extended[section].items() if k!=name}!=machine[section]:raise ValueError('observer changed physical transition')
    if extended['wires']!=machine['wires'] or extended['outputs']!=machine['outputs']:raise ValueError('observer changed physical wires/ports')
    record={'rule':'readonly-original-copy-hold-operand-history-v1','source_machine_sha256':base.digest(raw),
        'physical_machine_sha256':base.digest(machine),'extended_machine_sha256':base.digest(extended),
        'capture_source_field':'m_operand2','capture_source_expression':capture,'ghost_next_expression':ghost_next,
        'ghost_reset':b(32,0),'physical_projection_unchanged':True,'no_feedback':True,'saved_records_are_certificates':False}
    return extended,record
