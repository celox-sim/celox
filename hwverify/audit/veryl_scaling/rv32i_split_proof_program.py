"""Untrusted source-expression plans for the selected split refinement.

Every expression is lowered against the current validated Design. Plans contain
no verdicts, SMT identifiers, or numeric child IDs. The live sequent kernel must
prove every local lemma, premise implication, branch, and final original goal.
"""
from audit.veryl_scaling import rv32i_split_pipeline as p
from audit.veryl_scaling import rv32i_split_reuse as r
from audit.veryl_scaling.cpu_memory_reuse import expand

a,c=p.a,p.c
AND=lambda x,y:['and',x,y]
NOT=lambda x:['not',x]


def _bank(prefix):
    rd=a.ex(11,7,prefix+'w_ir')
    return [a.b(32,0)]+[a.it(a.land(prefix+'w_valid',a.neg(prefix+'w_fault'),prefix+'w_writes',a.eq(rd,a.b(5,i))),prefix+'w_result',prefix+'r'+str(i)) for i in range(1,32)]


def _uses(ir):
    op=a.ex(6,0,ir)
    return {j:a.lor(*(a.eq(op,a.b(7,k)) for k in codes)) for j,codes in
            ((1,(0x13,0x33,0x67,3,0x23,0x63)),(2,(0x33,0x23,0x63)))}


def _rhs(doc,field):
    """Locate the independent binding equality, then use checked next aliases."""
    matches=[]
    def walk(e):
        if not isinstance(e,list):return
        if len(e)==3 and e[0]=='eq' and e[1]=='impl.'+field:matches.append(e[2])
        for x in e[1:]:walk(x)
    walk(doc['binding'])
    if len(matches)!=1:raise ValueError('ambiguous source binding target '+field)
    names={side+'.'+key:side+'_next.'+key for side in ('impl','spec') for key in doc[side]['state']}
    return c.rename(matches[0],names)


def build(cpu,doc):
    """Construct advisory typed proof programs; this function proves nothing."""
    if cpu['state']!=p.cpu_state():raise ValueError('unexpected split state')
    q,n='impl.','impl_next.'
    post=_bank(n);before=_bank(q)
    current_uses=_uses(q+'x_ir');m_uses=_uses(n+'m_ir');x_uses=_uses(n+'x_ir')
    md=a.decode(n+'m_ir');xd=a.decode(q+'x_ir')
    read_m={j:a.reg_read(md['rs'+str(j)],post) for j in (1,2)}
    next_xd=a.decode(n+'x_ir')
    read_x={j:a.reg_read(next_xd['rs'+str(j)],post) for j in (1,2)}
    provenance=[c.implies(current_uses[j],a.eq(q+'x_operand'+str(j),a.reg_read(xd['rs'+str(j)],before))) for j in (1,2)]
    m_pre=a.land(a.neg('i.rst'),a.neg('i.stall'),a.neg(q+'halted'),q+'x_valid',
        a.neg(a.land(q+'w_valid',a.lor(q+'w_fault',q+'w_redirect'))),p.xm_noninterference(q),c.conj(provenance))
    m_post=c.conj(c.implies(a.land(n+'m_valid',m_uses[j]),a.eq(read_m[j],q+'x_operand'+str(j))) for j in (1,2))
    mp=p.decode_payload(q+'m_ir')
    safe=a.land(mp['writes'],mp['legal'],a.eq(a.ex(1,0,q+'m_pc'),a.b(2,0)),a.neg(q+'m_fetch_fault'),
        a.lor(*(a.eq(mp['x_op'],a.b(7,op)) for op in (0x13,0x33,0x37,0x17))))
    metadata=c.implies(q+'m_valid',c.conj([
        *[a.eq(q+'m_'+dst,mp[src]) for dst,src in [('legal','legal'),('writes','writes'),('is_load','is_load'),('is_store','is_store'),('f3','f3')]],
        a.eq(q+'m_safe_bypass',safe)]))
    x_pre=a.land(a.neg('i.rst'),c.implies(a.land(q+'w_valid',q+'w_writes'),a.neg(a.eq(a.ex(11,7,q+'w_ir'),a.b(5,0)))),
        a.neg(a.land(n+'w_valid',a.lor(n+'w_fault',n+'w_redirect'))),metadata,a.neg('i.stall'),a.neg(q+'halted'),a.eq(q+'r0',a.b(32,0)),p.selectors(q))
    x_post=c.conj(c.implies(a.land(n+'x_valid',x_uses[j]),a.eq(read_x[j],n+'x_operand'+str(j))) for j in (1,2))
    rename={'s.'+key:q+key for key in cpu['state']}
    copy_guards=[NOT('i.rst')];cursor=expand(cpu['next']['m_ir'],cpu['wires'])
    while isinstance(cursor,list) and cursor[0]=='ite':
        if len(cursor)!=4:raise ValueError('malformed source copy')
        copy_guards.append(NOT(c.rename(cursor[1],rename)));cursor=cursor[3]
    if cursor!='s.x_ir':raise ValueError('source M IR is not a checked copy tree')
    copy=copy_guards[0]
    for guard in copy_guards[1:]:copy=AND(copy,guard)
    normal={}
    for field in ('m_address','m_result','m_taken','m_target'):
        e=expand(cpu['next'][field],cpu['wires'])
        if not isinstance(e,list) or e[0]!='ite':raise ValueError('missing exact stage normalization guard')
        normal[field]=c.rename(e[1],rename)
    updated=r.mem.update(n+'data',n+'w_address',n+'w_data',n+'w_mask',a.land(n+'w_valid',a.neg(n+'w_fault'),n+'w_store'),n+'limit')
    response=r.mem.data_word(n+'rom',updated,n+'m_address',n+'limit')
    ir,pc,rs1,rs2,alt1,alt2,resp=['formal.'+k for k in ('ir','pc','rs1','rs2','alt1','alt2','load_response')]
    decode=a.decode(ir);direct=a.build_execute(ir,pc,rs1,rs2)
    conditional=a.build_execute(ir,pc,a.it(decode['uses1'],rs1,alt1),a.it(decode['uses2'],rs2,alt2),False,resp)
    # Preserve the independently tested theorem orientation. Symmetry is a
    # separate freshly checked propositional consequence below.
    alu_pre=a.land(direct['writes'],a.neg(decode['load']))
    alu_post=a.eq(direct['rd_value'],conditional['rd_value'])
    actual_direct=a.build_execute(q+'x_ir',q+'x_pc',q+'x_operand1',q+'x_operand2')
    actual_conditional=a.build_execute(q+'x_ir',q+'x_pc',a.it(xd['uses1'],q+'x_operand1',read_m[1]),a.it(xd['uses2'],q+'x_operand2',read_m[2]),False,response)
    alu_reverse=a.eq(actual_conditional['rd_value'],actual_direct['rd_value'])
    definitions={'m_pre':m_pre,'m_post':m_post,'x_pre':x_pre,'x_post':x_post,'copy':copy,
        'use1':xd['uses1'],'use2':xd['uses2'],'read_m1':read_m[1],'read_m2':read_m[2],
        'alu_pre':alu_pre,'alu_post':alu_post,'alu_reverse':alu_reverse,'alu_use1':decode['uses1'],'alu_use2':decode['uses2'],'response':response,
        **{'normal_'+field:value for field,value in normal.items()}}
    programs=[]
    def program(field,steps,result):
        programs.append({'id':field,'match_rhs':_rhs(doc,field),'steps':steps,'result':result})
    def step(op,id,**kw):return {'op':op,'id':id,**kw}
    def prove(id,pre,post):return step('prove',id,pre=pre,post=post)
    def project(id,source,path):return step('project',id,source=source,path=path)
    def apply(id,lemma,premise):return step('apply',id,lemma=lemma,premise=premise)
    def rewrite(id,pre,goal,equalities,retain=True):return step('prepare_rewrite',id,pre=pre,goal=goal,equalities=equalities,retain_rewritten_pre=retain)
    def finish(id,plan,proof):return step('finish_rewrite',id,plan=plan,proof=proof)
    def join(id,pre,goal,guard,positive,negative):return step('join',id,pre=pre,goal=goal,guard=guard,positive=positive,negative=negative)
    # The address's small complete two-branch proof uses direct full-P cuts.
    g='let.normal_m_address';branch=AND('$pre',g)
    steps=[prove('operand',branch,a.eq('let.read_m1',q+'x_operand1')),
        prove('ir',branch,a.eq(n+'m_ir',q+'x_ir')),rewrite('rw',branch,'$goal',['operand','ir'],False),
        prove('rewritten','plan.rw.pre','plan.rw.post'),finish('positive','rw','rewritten'),
        prove('negative',AND('$pre',NOT(g)),'$goal'),join('complete','$pre','$goal',g,'positive','negative')]
    program('m_address',steps,'complete')
    for field,indices,with_pc in [('m_result',(1,2),True),('m_taken',(1,2),False),('m_target',(1,),True)]:
        branch=AND('$pre','let.copy');steps=[prove('delivery','let.m_pre','let.m_post')];equalities=[]
        for j in indices:
            use='let.use'+str(j);tag='operand'+str(j)
            steps += [project(tag+'_lemma','delivery',[j-1,1]),
                prove(tag+'_premise',AND(branch,use),'handle.'+tag+'_lemma.pre'),
                apply(tag+'_guarded',tag+'_lemma',tag+'_premise'),
                step('conditional_eq',tag,pre=branch,guard=use,source=tag+'_guarded')]
            equalities.append(tag)
        steps.append(prove('ir',branch,a.eq(n+'m_ir',q+'x_ir')));equalities.append('ir')
        if with_pc:steps.append(prove('pc',branch,a.eq(n+'m_pc',q+'x_pc')));equalities.append('pc')
        steps.append(rewrite('rw',branch,'$goal',equalities))
        if field=='m_result':
            ng='let.normal_m_result';positive=AND('plan.rw.pre',ng)
            steps += [prove('alu_both',AND(AND('let.alu_pre','let.alu_use1'),'let.alu_use2'),'let.alu_post'),
                prove('alu_one',AND(AND('let.alu_pre','let.alu_use1'),NOT('let.alu_use2')),'let.alu_post'),
                join('alu_used',AND('let.alu_pre','let.alu_use1'),'let.alu_post','let.alu_use2','alu_both','alu_one'),
                prove('alu_none',AND('let.alu_pre',NOT('let.alu_use1')),'let.alu_post'),
                join('alu','let.alu_pre','let.alu_post','let.alu_use1','alu_used','alu_none'),
                step('instantiate','alu_instance',source='alu',substitution=[
                {'from':'formal.'+name,'to':value} for name,value in [('ir',q+'x_ir'),('pc',q+'x_pc'),('rs1',q+'x_operand1'),('rs2',q+'x_operand2'),
                    ('alt1','let.read_m1'),('alt2','let.read_m2'),('load_response','let.response')]]),
                prove('symmetry','handle.alu_instance.post','let.alu_reverse'),apply('alu_symmetric','symmetry','alu_instance'),
                prove('alu_premise',positive,'handle.alu_symmetric.pre'),apply('alu_equality','alu_symmetric','alu_premise'),
                rewrite('alu_rw',positive,'plan.rw.post',['alu_equality'],False),
                prove('alu_rewritten','plan.alu_rw.pre','plan.alu_rw.post'),finish('normal','alu_rw','alu_rewritten'),
                prove('nonnormal',AND('plan.rw.pre',NOT(ng)),'plan.rw.post'),
                join('rewritten','plan.rw.pre','plan.rw.post',ng,'normal','nonnormal')]
        else:steps.append(prove('rewritten','plan.rw.pre','plan.rw.post'))
        steps += [finish('positive','rw','rewritten'),prove('negative',AND('$pre',NOT('let.copy')),'$goal'),
            join('complete','$pre','$goal','let.copy','positive','negative')]
        program(field,steps,'complete')
    for j in (1,2):
        steps=[prove('delivery','let.x_pre','let.x_post'),project('local','delivery',[j-1,1]),
            prove('premise','$pre','handle.local.pre'),apply('equality','local','premise'),
            rewrite('rw','$pre','$goal',['equality'],False),prove('rewritten','plan.rw.pre','plan.rw.post'),finish('complete','rw','rewritten')]
        program('x_operand'+str(j),steps,'complete')
    return {'version':1,'mode':'independent_lemmas','variables':{k:{'bv':32} for k in ('ir','pc','rs1','rs2','alt1','alt2','load_response')},
        'lets':[{'id':key,'expr':value} for key,value in definitions.items()],'programs':programs}
