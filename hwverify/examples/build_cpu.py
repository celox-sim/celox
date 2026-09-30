"""Emit declarative CPU examples; no proof logic lives in this generator."""
import json,copy
from pathlib import Path
D=Path(__file__).parent
B=lambda w,n:['bv',w,n]
def AND(*xs):
 r=True
 for x in xs:r=['and',r,x]
 return r
def ite(c,a,b):return ['ite',c,a,b]
def eq(a,b):return ['eq',a,b]
def isa(prefix):
 op='w.op';imm='w.imm';isop=lambda n:eq(op,B(3,n))
 return {'pc':ite(AND(isop(3),eq('s.acc',B(8,0))),imm,['add','s.pc',B(8,1)]),
 'acc':ite(isop(0),['add','s.acc',imm],ite(isop(1),['read','s.mem',imm],ite(isop(5),['bxor','s.acc',imm],'s.acc'))),
 'mem':ite(isop(2),['write','s.mem',imm,'s.acc'],'s.mem'),
 'halted':['or','s.halted',isop(4)],'program':'s.program'}
state={'pc':{'bv':8},'acc':{'bv':8},'mem':{'mem':[8,8]},'program':{'mem':[8,16]},'halted':'bool'}
reset={'pc':B(8,0),'acc':B(8,0),'mem':['const_mem',8,B(8,0)],'program':'i.program','halted':False}
spec={'state':state,'reset':reset,'wires':{'instruction':['read','s.program','s.pc'],'op':['extract',15,13,'w.instruction'],'imm':['extract',7,0,'w.instruction']},'next':isa(''),'outputs':{'can_step':['not','s.halted']}}
impl={'state':{**copy.deepcopy(state),'phase':{'bv':1},'ir':{'bv':16}},'reset':{**copy.deepcopy(reset),'phase':B(1,0),'ir':B(16,0)},'wires':{'op':['extract',15,13,'s.ir'],'imm':['extract',7,0,'s.ir'],'run':AND(['not','i.stall'],['not','s.halted']),'execute':eq('s.phase',B(1,1)),'commit':AND('w.run','w.execute')},'next':{},'outputs':{'commit':'w.commit'}}
for n,v in isa('').items():impl['next'][n]=ite('w.commit',v,'s.'+n)
impl['next']['phase']=ite('w.run',ite('w.execute',B(1,0),B(1,1)),'s.phase')
impl['next']['ir']=ite(AND('w.run',['not','w.execute']),['read','s.program','s.pc'],'s.ir')
binding=AND(*[eq('spec.'+n,'impl.'+n) for n in state],['implies',eq('impl.phase',B(1,1)),eq('impl.ir',['read','impl.program','impl.pc'])])
d={'version':2,'name':'CPU ISA vs two-microcycle implementation','inputs':{'rst':'bool','stall':'bool','program':{'mem':[8,16]}},'reset_input':'rst','spec':spec,'impl':impl,'binding':binding,'commit':'commit','can_step':'can_step','hold_when':'i.stall','progress':{'enabled':AND(['not','spec.halted'],['not','i.stall']),'rank':ite(eq('impl.phase',B(1,0)),B(2,1),B(2,0))}}
def save(name,obj): (D/(name+'.json')).write_text(json.dumps(obj,indent=2)+'\n')
save('cpu',d)
x=copy.deepcopy(d);x['impl']['next']['acc'][2][3][2]=['read','s.mem',['add','w.imm',B(8,1)]];save('wrong_load',x)
x=copy.deepcopy(d);x['impl']['next']['pc'][2][2]=['add','w.imm',B(8,1)];save('wrong_branch',x)
x=copy.deepcopy(d);x['impl']['wires']['commit']=AND(['not','s.halted'],'w.execute');save('ignores_stall',x)
x=copy.deepcopy(d);x['impl']['outputs']['commit']=False;save('missing_commit',x)
x=copy.deepcopy(d);x['impl']['next']['phase'][2][3]=B(1,0);save('hang_fetch',x)
x=copy.deepcopy(d);x['impl']['outputs']['commit']=True;save('commit_after_halt',x)
x=copy.deepcopy(d);x['progress']['rank']=ite('i.stall',B(2,1),B(2,0));save('input_dependent_rank',x)

x=copy.deepcopy(d);x['binding']=eq('i.stall','impl.halted');save('input_dependent_binding',x)
