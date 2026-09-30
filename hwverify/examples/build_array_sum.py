"""16-element modular sum; finite specification fold, inductive execution proof.
The legacy CPU is unchanged. Opcodes 6/7 and idx are new in this variant.
"""
import copy, json
from pathlib import Path
D = Path(__file__).parent
B = lambda w,n: ['bv',w,n]
def eq(a,b): return ['eq',a,b]
def ite(c,a,b): return ['ite',c,a,b]
def AND(*xs):
    r=True
    for x in xs: r=['and',r,x]
    return r
def OR(*xs):
    r=False
    for x in xs: r=['or',r,x]
    return r
def save(name,d): (D/(name+'.json')).write_text(json.dumps(d,indent=2)+'\n')
d=json.loads((D/'cpu.json').read_text())
d['name']='Array sum: extended ISA and fetch/execute refinement'
d['inputs'].update(data={'mem':[8,8]},start_index={'bv':8})
for name in ['spec','impl']:
    m=d[name]
    m['state']['idx']={'bv':8}
    m['reset']['idx']='i.start_index'
    m['reset']['mem']='i.data'
    op=lambda n:eq('w.op',B(3,n))
    idxn=['add','s.idx',B(8,1)]
    # Under commit, the old transition remains the exact legacy ISA meaning.
    body={k:(v if name=='spec' else v[2]) for k,v in m['next'].items() if k in d['spec']['state'] and k!='idx'}
    body['acc']=ite(op(6),['add','s.acc',['read','s.mem','s.idx']],body['acc'])
    body['idx']=ite(op(7),idxn,'s.idx')
    body['pc']=ite(op(7),ite(['ne',idxn,B(8,0)],'w.imm',['add','s.pc',B(8,1)]),body['pc'])
    for k,v in body.items(): m['next'][k]=v if name=='spec' else ite('w.commit',v,'s.'+k)
d['binding']=AND(d['binding'],eq('spec.idx','impl.idx'))
# ADDX ; IXJ 0 ; HALT. All other program addresses contain HALT.
program=['const_mem',8,B(16,4<<13)]
for pc,word in enumerate([6<<13,7<<13,4<<13]): program=['write',program,B(8,pc),B(16,word)]
def prefix(idx):
    r=B(8,0)
    for a in range(240,256): r=['add',r,ite(['ult',B(8,a),idx],['read','p.data',B(8,a)],B(8,0))]
    return r
def total():
    r=B(8,0)
    for a in range(240,256): r=['add',r,['read','p.data',B(8,a)]]
    return r
at=lambda pc:eq('s.pc',B(8,pc))
valid_idx=['ule',B(8,240),'s.idx']
inv=AND(eq('s.program',program),eq('s.mem','p.data'),OR(
    AND(['not','s.halted'],valid_idx,at(0),eq('s.acc',prefix('s.idx'))),
    AND(['not','s.halted'],valid_idx,at(1),eq('s.acc',['add',prefix('s.idx'),['read','p.data','s.idx']])),
    AND(['not','s.halted'],at(2),eq('s.idx',B(8,0)),eq('s.acc',total())),
    AND('s.halted',at(3),eq('s.idx',B(8,0)),eq('s.acc',total()))))
# Number of ISA instructions remaining: 2*(256-idx)+1 at loop head.
remaining=['mul',B(10,2),['sub',B(10,256),['zext',2,'s.idx']]]
rank=ite('s.halted',B(10,0),ite(at(0),['add',remaining,B(10,1)],ite(at(1),remaining,B(10,1))))
d['program_contract']={'parameters':{'data':{'mem':[8,8]}},
    'precondition':AND(eq('i.program',program),eq('i.data','p.data'),eq('i.start_index',B(8,240))),
    'invariant':inv,'terminal':'s.halted',
    'split':{'pc':{'expr':'s.pc','min':0,'max':2},'index':{'expr':'s.idx','min':240,'max':255}},
    'postcondition':AND(eq('s.acc',total()),eq('s.mem','p.data')),'rank':rank}
save('array_sum',d)
x=copy.deepcopy(d);x['program_contract']['invariant']=False;save('array_sum_false_invariant',x)
x=copy.deepcopy(d);x['program_contract']['precondition']=False;save('array_sum_false_pre',x)
x=copy.deepcopy(d);x['program_contract']['rank']=B(10,0);save('array_sum_bad_rank',x)
x=copy.deepcopy(d);x['program_contract']['postcondition']=eq('s.acc',['add',total(),B(8,1)]);save('array_sum_bad_post',x)
x=copy.deepcopy(d);x['program_contract']['invariant']='i.stall';save('array_sum_input_invariant',x)
# Same buggy program in precondition and invariant: skip ADDX on subsequent iterations.
x=copy.deepcopy(d)
def alter(v):
    if isinstance(v,list):
        if v==B(16,7<<13): return B(16,(7<<13)|1)
        return [alter(a) for a in v]
    if isinstance(v,dict): return {k:alter(a) for k,a in v.items()}
    return v
x['program_contract']=alter(x['program_contract']);save('array_sum_bad_program',x)
x=copy.deepcopy(d);x['impl']['next']['acc'][2][2]=['add','s.acc',['read','s.mem',['add','s.idx',B(8,1)]]];save('array_sum_wrong_indexed_load',x)

x=copy.deepcopy(d);x['program_contract'].pop('split');x['program_contract']['cases']={'only_exit':at(2)};save('array_sum_missing_case',x)

# Same contract without hints, retained for a reproducible solver-cost comparison.
x=copy.deepcopy(d);x['program_contract'].pop('split');x['program_contract']['partitioning']='none';save('array_sum_baseline_unsplit',x)
x=copy.deepcopy(d);x['program_contract']['terminal']=True;save('array_sum_bad_terminal',x)
x=copy.deepcopy(d);x['spec']['outputs']['can_step']=False;save('array_sum_unavailable_step',x)
x=copy.deepcopy(d);x['program_contract']['split']['index']={'expr':'s.idx','min':240,'max':240}
x['program_contract']['postcondition']=False;save('array_sum_split_other_bad_post',x)
# Wrong program with a split range excluding every reachable index must still fail.
x=copy.deepcopy(d);x['program_contract']=alter(x['program_contract'])
x['program_contract']['split']['index']={'expr':'s.idx','min':1,'max':1};save('array_sum_split_other_bad_program',x)
