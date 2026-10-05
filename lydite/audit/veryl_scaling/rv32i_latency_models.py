"""Explicit source-specific latency control models; no imported proof authority."""
from audit.veryl_scaling import rv32i_latency_common as p
from audit.veryl_scaling import rv32i_spec as s
c=p.c
b,eq,it,land,lor,neg=s.b,s.eq,s.it,s.land,s.lor,s.neg
BASELINE_CONTROL=('d_valid','x_valid','w_valid','w_fault','halted')
FOURSTAGE_CONTROL=('d_valid','x_valid','m_valid','w_valid','w_fault','w_redirect','halted')

def baseline_equations(h,r,f):
    active=land(neg('i.stall'),neg('s.halted'))
    wt=land('s.w_valid','s.w_fault')
    kill=lor(r,land('s.x_valid',f))
    advance=land(active,neg(wt))
    accept=land(advance,neg(h),neg(kill))
    nxt={'d_valid':it(active,it(wt,False,it(kill,False,it(h,'s.d_valid',True))),'s.d_valid'),
         'x_valid':it(active,it(wt,False,land('s.d_valid',neg(h),neg(kill))),'s.x_valid'),
         'w_valid':it(active,it(wt,False,'s.x_valid'),'s.w_valid'),
         'w_fault':it(advance,land('s.x_valid',f),'s.w_fault'),
         'halted':lor('s.halted',land(active,wt))}
    ports={'imem_valid':accept,'retire':land(active,'s.w_valid'),
           'commit':land(active,'s.w_valid',neg('s.w_fault')),
           'trap_valid':land(active,wt)}
    return nxt,ports,active,wt,kill,advance

def baseline_source_document(raw, roots):
    """No source constraints: arbitrary source prestate and port replies."""
    nxt,ports,active,wt,kill,advance=baseline_equations('n.h','n.r','n.f')
    state={k:raw['state'][k] for k in BASELINE_CONTROL}
    payload=('d_pc','d_ir','d_fetch_fault','x_pc','x_ir','x_fetch_fault','w_pc','w_ir')
    state.update({k:raw['state'][k] for k in payload})
    state.update({k:'bool' for k in ('h','r','f',*ports)})
    actual={k:raw['next'][k] for k in (*BASELINE_CONTROL,*payload)}
    actual.update({k:raw['outputs'][k] for k in ports})
    actual.update(roots)
    expected=[eq('n.'+k,v) for k,v in {**nxt,**ports}.items()]
    expected += [c.implies('n.h',land('s.d_valid','s.x_valid')),c.implies('n.r','s.x_valid')]
    accept=ports['imem_valid']
    for dst,src in [('d_pc','s.fetch_pc'),('d_ir','i.imem_response'),('d_fetch_fault','i.imem_fault')]:
        expected.append(eq('n.'+dst,it(accept,src,'s.'+dst)))
    for dst,src in [('x_pc','d_pc'),('x_ir','d_ir'),('x_fetch_fault','d_fetch_fault'),('w_pc','x_pc'),('w_ir','x_ir')]:
        expected.append(eq('n.'+dst,it(advance,'s.'+src,'s.'+dst)))
    rename={'s.'+k:'i.pre_'+k for k in raw['state']}
    machine={'state':state,'reset':{k:c.zero(v) for k,v in state.items()},
             'next':c.rename(actual,rename),'wires':c.rename(raw['wires'],rename)}
    initial=c.conj(eq('s.'+k,c.zero(v)) for k,v in state.items())
    return c.scoped_document('RV32I actual source control and token-transfer simulation',state,
        {**p.INPUTS,**{'pre_'+k:v for k,v in raw['state'].items()}},{},initial,True,
        c.rename(c.conj(expected),rename),machine)

def baseline_control_document(mutation=None):
    """Conservative control model; hazard/redirect guards established separately."""
    h=land('i.h','s.d_valid','s.x_valid');r=land('i.r','s.x_valid');f='i.f'
    nxt,ports,active,wt,kill,advance=baseline_equations(h,r,f)
    tag='s.tag';age='s.age'
    isd=eq(tag,b(3,1));isx=eq(tag,b(3,2));isw=eq(tag,b(3,3))
    live=lor(isd,isx,isw)
    terminal=it('s.w_fault',b(3,5),b(3,4))
    move=it(isw,terminal,it(wt,b(3,6),it(isx,b(3,3),it(kill,b(3,6),it(h,tag,b(3,2))))))
    if mutation=='stuck': move=it(isd,tag,move)
    if mutation=='early-retire': move=it(isd,b(3,4),move)
    if mutation=='missed-squash': move=it(land(isd,kill),b(3,2),move)
    nxt['tag']=it(live,it(active,move,tag),it(land(eq(tag,b(3,0)),ports['imem_valid'],'i.choose'),b(3,1),tag))
    nxt['age']=it(land(live,active),['add',age,b(3,1)],age)
    state={**{k:'bool' for k in BASELINE_CONTROL},'tag':{'bv':3},'age':{'bv':3}}
    invariant=c.conj([
        ['ule',tag,b(3,6)],['ule',age,b(3,4)],
        c.implies(eq(tag,b(3,0)),eq(age,b(3,0))),
        c.implies(live,neg('s.halted')),
        c.implies(isd,land('s.d_valid',['ule',age,b(3,1)])),
        c.implies(land(isd,eq(age,b(3,1))),neg('s.x_valid')),
        c.implies(isx,land('s.x_valid',['ule',age,b(3,2)])),
        c.implies(isw,land('s.w_valid',['ule',age,b(3,3)])),
        c.implies('s.halted',land(neg('s.d_valid'),neg('s.x_valid'),neg('s.w_valid')))])
    # Terminal classifications must match the selected token's stage and event.
    changed=neg(eq('n.tag',tag))
    op=c.conj([
        c.implies(land(changed,eq('n.tag',b(3,4))),land(isw,ports['commit'])),
        c.implies(land(changed,eq('n.tag',b(3,5))),land(isw,ports['trap_valid'])),
        c.implies(land(changed,eq('n.tag',b(3,6))),land(active,lor(land(wt,lor(isd,isx)),land(isd,kill)))),
        c.implies(land(live,active,eq(age,b(3,3))),['ule',b(3,4),'n.tag']),
        c.implies(land(live,neg(active)),land(eq('n.tag',tag),eq('n.age',age)))])
    machine={'state':state,'next':nxt,'reset':{k:c.zero(v) for k,v in state.items()},'wires':{}}
    return c.scoped_document('RV32I selected acceptance terminates within four enabled edges',state,
        {k:'bool' for k in ('rst','stall','h','r','f','choose')},{},
        c.conj(eq('s.'+k,c.zero(v)) for k,v in state.items()),invariant,op,machine)


def fourstage_equations(h,r,f,t):
    active=land(neg('i.stall'),neg('s.halted'))
    wt=land('s.w_valid','s.w_fault')
    advance=land(active,neg(wt))
    kill=lor(wt,r)
    accept=land(advance,neg(h),neg(r))
    nxt={'d_valid':it(active,it(kill,False,it(h,'s.d_valid',True)),'s.d_valid'),
         'x_valid':it(active,it(kill,False,land('s.d_valid',neg(h))),'s.x_valid'),
         'm_valid':it(active,it(kill,False,'s.x_valid'),'s.m_valid'),
         'w_valid':it(active,it(kill,False,'s.m_valid'),'s.w_valid'),
         'w_fault':it(advance,land('s.m_valid',f),'s.w_fault'),
         'w_redirect':it(advance,land(t,neg(f)),'s.w_redirect'),
         'halted':lor('s.halted',land(active,wt))}
    ports={'imem_valid':accept,'retire':land(active,'s.w_valid'),
           'commit':land(active,'s.w_valid',neg('s.w_fault')),'trap_valid':land(active,wt)}
    return nxt,ports,active,wt,kill,advance

def fourstage_source_document(raw, roots):
    """No source constraints: arbitrary source prestate and port replies."""
    nxt,ports,active,wt,kill,advance=fourstage_equations('n.h','n.r','n.f','s.m_taken')
    state={k:raw['state'][k] for k in FOURSTAGE_CONTROL}
    payload=('d_pc','d_ir','d_fetch_fault','x_pc','x_ir','x_fetch_fault','m_pc','m_ir','m_fetch_fault','w_pc','w_ir')
    state.update({k:raw['state'][k] for k in payload})
    state.update({k:'bool' for k in ('h','r','f',*ports)})
    actual={k:raw['next'][k] for k in (*FOURSTAGE_CONTROL,*payload)}
    actual.update({k:raw['outputs'][k] for k in ports})
    actual.update(roots)
    expected=[eq('n.'+k,v) for k,v in {**nxt,**ports}.items()]
    expected += [c.implies('n.h',land('s.d_valid',lor('s.x_valid','s.m_valid'))),eq('n.r',land('s.w_valid','s.w_redirect',neg('s.w_fault')))]
    accept=ports['imem_valid']
    for dst,src in [('d_pc','s.fetch_pc'),('d_ir','i.imem_response'),('d_fetch_fault','i.imem_fault')]:
        expected.append(eq('n.'+dst,it(land(advance,neg('n.h')),src,'s.'+dst)))
    for dst,src in [('x_pc','d_pc'),('x_ir','d_ir'),('x_fetch_fault','d_fetch_fault'),('m_pc','x_pc'),('m_ir','x_ir'),('m_fetch_fault','x_fetch_fault'),('w_pc','m_pc'),('w_ir','m_ir')]:
        expected.append(eq('n.'+dst,it(advance,'s.'+src,'s.'+dst)))
    rename={'s.'+k:'i.pre_'+k for k in raw['state']}
    machine={'state':state,'reset':{k:c.zero(v) for k,v in state.items()},
             'next':c.rename(actual,rename),'wires':c.rename(raw['wires'],rename)}
    initial=c.conj(eq('s.'+k,c.zero(v)) for k,v in state.items())
    return c.scoped_document('RV32I actual source control and token-transfer simulation',state,
        {**p.INPUTS,**{'pre_'+k:v for k,v in raw['state'].items()}},{},initial,True,
        c.rename(c.conj(expected),rename),machine)

def fourstage_control_document(mutation=None):
    h=land('i.h','s.d_valid',lor('s.x_valid','s.m_valid'))
    r=land('s.w_valid','s.w_redirect',neg('s.w_fault'))
    nxt,ports,active,wt,kill,advance=fourstage_equations(h,r,'i.f','i.t')
    tag='s.tag';age='s.age'
    isd=eq(tag,b(3,1));isx=eq(tag,b(3,2));ism=eq(tag,b(3,3));isw=eq(tag,b(3,4))
    live=lor(isd,isx,ism,isw)
    terminal=it('s.w_fault',b(3,6),b(3,5))
    move=it(isw,terminal,it(kill,b(3,7),it(ism,b(3,4),it(isx,b(3,3),it(h,tag,b(3,2))))))
    if mutation=='stuck':move=it(isd,tag,move)
    if mutation=='early-retire':move=it(isd,b(3,5),move)
    if mutation=='missed-squash':move=it(land(isd,kill),b(3,2),move)
    nxt['tag']=it(live,it(active,move,tag),it(land(eq(tag,b(3,0)),ports['imem_valid'],'i.choose'),b(3,1),tag))
    nxt['age']=it(land(live,active),['add',age,b(3,1)],age)
    state={**{k:'bool' for k in FOURSTAGE_CONTROL},'tag':{'bv':3},'age':{'bv':3}}
    invariant=c.conj([
      ['ule',age,b(3,6)],c.implies(eq(tag,b(3,0)),eq(age,b(3,0))),
      c.implies(live,neg('s.halted')),
      c.implies(isd,land('s.d_valid',['ule',age,b(3,2)])),
      c.implies(land(isd,neg(eq(age,b(3,0)))),neg('s.x_valid')),
      c.implies(land(isd,eq(age,b(3,2))),neg('s.m_valid')),
      c.implies(isx,land('s.x_valid',['ule',age,b(3,3)])),
      c.implies(ism,land('s.m_valid',['ule',age,b(3,4)])),
      c.implies(isw,land('s.w_valid',['ule',age,b(3,5)])),
      c.implies('s.halted',land(*[neg('s.'+st+'_valid') for st in ('d','x','m','w')]))])
    changed=neg(eq('n.tag',tag))
    op=c.conj([
      c.implies(land(changed,eq('n.tag',b(3,5))),land(isw,ports['commit'])),
      c.implies(land(changed,eq('n.tag',b(3,6))),land(isw,ports['trap_valid'])),
      c.implies(land(changed,eq('n.tag',b(3,7))),land(active,kill,lor(isd,isx,ism))),
      c.implies(land(live,active,eq(age,b(3,5))),['ule',b(3,5),'n.tag']),
      c.implies(land(live,neg(active)),land(eq('n.tag',tag),eq('n.age',age)))])
    machine={'state':state,'next':nxt,'reset':{k:c.zero(v) for k,v in state.items()},'wires':{}}
    return c.scoped_document('Candidate selected acceptance terminates within six enabled edges',state,
      {k:'bool' for k in ('rst','stall','h','f','t','choose')},{},
      c.conj(eq('s.'+k,c.zero(v)) for k,v in state.items()),invariant,op,machine)


def selector_alignment_document(raw,selector):
    """Independent source-bound induction, never assumed by token latency.

    Keep D IR and one selector as actual state; universally forget every other
    prestate field. Exact imported reset/next expressions establish a conservative
    transition projection, so its inductive invariant applies to the source.
    """
    if selector not in ('d_sel1','d_sel2'):raise ValueError('unknown D selector')
    selected=('d_ir',selector)
    if any(raw['state'].get(k)!={'bv':32} for k in selected):raise ValueError('invalid selector-state widths')
    state={k:raw['state'][k] for k in selected}
    rename={'s.'+k:'i.pre_'+k for k in raw['state'] if k not in selected}
    machine={'state':state,'reset':{k:raw['reset'][k] for k in selected},
             'next':c.rename({k:raw['next'][k] for k in selected},rename),
             'wires':c.rename(raw['wires'],rename)}
    lo=15 if selector=='d_sel1' else 20
    wanted=['shl',b(32,1),s.zext(32,s.ex(lo+4,lo,'s.d_ir'))]
    invariant=eq('s.'+selector,wanted)
    initial=c.conj(eq('s.'+k,raw['reset'][k]) for k in selected)
    inputs={**p.INPUTS,**{'pre_'+k:v for k,v in raw['state'].items() if k not in selected}}
    return c.scoped_document('Actual D one-hot selector alignment: '+selector,state,inputs,{},
                             initial,invariant,True,machine)
