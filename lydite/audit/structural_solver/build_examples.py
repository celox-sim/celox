"""Small independent-shaped structural workloads, no measured-state assumptions."""
import copy,json
from pathlib import Path
ROOT=Path(__file__).resolve().parent
bv=lambda w,v:['bv',w,v]
def allof(xs):
 r=True
 for x in xs:r=['and',r,x]
 return r
states={'count':{'bv':4},'address':{'bv':2},'mem':{'mem':[2,8]},'shadow':{'bv':8},**{f'distract{k}':{'bv':8} for k in range(3)}}
reset={'count':bv(4,3),'address':'i.address','mem':'i.mem','shadow':['read','i.mem','i.address'],**{f'distract{k}':f'i.distract{k}' for k in range(3)}}
active=['ne','s.count',bv(4,0)]
read=['read','s.mem','s.address']
value=['add',read,bv(8,1)]
nexts={k:f's.{k}' for k in states}
nexts.update(count=['ite',active,['sub','s.count',bv(4,1)],'s.count'],mem=['ite',active,['write','s.mem','s.address',value],'s.mem'],shadow=['ite',active,['add','s.shadow',bv(8,1)],'s.shadow'])
impl_next={k:['ite','i.stall',f's.{k}',v] for k,v in nexts.items()}
# Redundant same-address write is intentionally not present in the spec.
impl_next['mem']=['ite','i.stall','s.mem',['ite',active,['write',['write','s.mem','s.address',bv(8,199)],'s.address',value],'s.mem']]
binding=allof([['eq',f'spec.{k}',f'impl.{k}'] for k in states])
inv=allof([['ule','s.count',bv(4,3)],['eq','s.shadow',read]]+[['ule',f's.distract{k}',bv(8,255)] for k in range(3)])
d=dict(version=2,name='memory increment with irrelevant invariant scalars',inputs={'rst':'bool','stall':'bool','address':{'bv':2},'mem':{'mem':[2,8]},**{f'distract{k}':{'bv':8} for k in range(3)}},reset_input='rst',spec=dict(state=states,reset=reset,next=nexts,outputs={'can_step':active}),impl=dict(state=states,reset=reset,next=impl_next,outputs={'commit':allof([['not','i.rst'],['not','i.stall'],active])}),binding=binding,commit='commit',can_step='can_step',hold_when=allof([['not','i.rst'],'i.stall']),progress={'enabled':allof([['not','i.stall'],['ne','impl.count',bv(4,0)]]),'rank':bv(1,0)},program_contract={'parameters':{},'precondition':True,'invariant':inv,'terminal':['eq','s.count',bv(4,0)],'postcondition':['eq','s.shadow',read],'rank':'s.count'})
for name,doc in [('memory_increment_good',d),('memory_increment_bad_shadow',json.loads(json.dumps(d))),('memory_increment_bad_alias',json.loads(json.dumps(d)))]:
 if name.endswith('bad_shadow'):doc['impl']['next']['shadow'][3][2]=['add','s.shadow',bv(8,2)]
 if name.endswith('bad_alias'):doc['impl']['next']['mem'][3][2][2]=['add','s.address',bv(2,1)]
 (ROOT/f'{name}.json').write_text(json.dumps(doc,indent=2)+'\n')
