"""Independent second consumer: repeated-add loop and two-phase implementation."""
import json,copy
from pathlib import Path
D=Path(__file__).resolve().parent
B=lambda w,n:['bv',w,n]
def eq(a,b):return ['eq',a,b]
def AND(*xs):
 out=True
 for x in xs:out=['and',out,x]
 return out
def ite(c,a,b):return ['ite',c,a,b]
state={'remaining':{'bv':4},'accumulator':{'bv':8},'factor':{'bv':8}}
active=['ne','s.remaining',B(4,0)]
reset={'remaining':'i.iterations','accumulator':B(8,0),'factor':'i.factor'}
nexts={'remaining':['sub','s.remaining',B(4,1)],'accumulator':['add','s.accumulator','s.factor'],'factor':'s.factor'}
spec={'state':state,'reset':reset,'next':nexts,'outputs':{'can_run':active}}
impl={'state':{**state,'phase':{'bv':1},'pending':{'bv':8}},'reset':{**reset,'phase':B(1,0),'pending':B(8,0)},
 'wires':{'execute':eq('s.phase',B(1,1)),'running':AND(active,['not','i.wait']),'commit':AND('w.execute','w.running')},
 'next':{'remaining':ite('w.commit',nexts['remaining'],'s.remaining'),
 'accumulator':ite('w.commit','s.pending','s.accumulator'),'factor':'s.factor',
 'pending':ite(AND('w.running',['not','w.execute']),nexts['accumulator'],'s.pending'),
 'phase':ite('w.running',ite('w.execute',B(1,0),B(1,1)),'s.phase')},'outputs':{'retired':'w.commit'}}
bind=AND(*[eq('spec.'+n,'impl.'+n) for n in state],['implies',eq('impl.phase',B(1,1)),eq('impl.pending',['add','impl.accumulator','impl.factor'])])
used=['sub',['zext',4,'p.iterations'],['zext',4,'s.remaining']]
contract={'parameters':{'factor':{'bv':8},'iterations':{'bv':4}},'precondition':AND(eq('i.factor','p.factor'),eq('i.iterations','p.iterations')),
 'invariant':AND(eq('s.factor','p.factor'),['ule','s.remaining','p.iterations'],eq('s.accumulator',['mul',used,'p.factor'])),
 'terminal':eq('s.remaining',B(4,0)),'postcondition':eq('s.accumulator',['mul',['zext',4,'p.iterations'],'p.factor']),'rank':'s.remaining'}
d={'version':2,'name':'Repeated-add multiplier, two-phase implementation','inputs':{'reset':'bool','wait':'bool','factor':{'bv':8},'iterations':{'bv':4}},'reset_input':'reset','spec':spec,'impl':impl,'binding':bind,
 'commit':'retired','can_step':'can_run','hold_when':'i.wait','progress':{'enabled':AND(['ne','spec.remaining',B(4,0)],['not','i.wait']),'rank':ite(eq('impl.phase',B(1,0)),B(2,1),B(2,0))},'program_contract':contract}
def save(n,x):(D/(n+'.json')).write_text(json.dumps(x,indent=2)+'\n')
save('multiplier_good',d)
x=copy.deepcopy(d);x['impl']['next']['pending'][2]=['add',nexts['accumulator'],B(8,1)];save('multiplier_wrong_capture',x)
x=copy.deepcopy(d);x['spec']['next']['remaining']='s.remaining';x['impl']['next']['remaining']='s.remaining';save('multiplier_missing_decrement',x)
x=copy.deepcopy(d);x['impl']['next']['phase'][2][3]=B(1,0);save('multiplier_hang_capture',x)
print('built multiplier plus three mutants')
