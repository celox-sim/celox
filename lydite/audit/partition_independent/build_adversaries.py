import copy,json
from pathlib import Path
D=Path(__file__).resolve().parent
B=lambda n:['bv',4,n]
eq=lambda a,b:['eq',a,b]
active=['ne','s.x',B(0)]
stepx=['ite',active,['sub','s.x',B(1)],'s.x']
base={'version':2,'name':'Generic four-bit countdown','inputs':{'reset':'bool','initial':{'bv':4}},'reset_input':'reset',
 'spec':{'state':{'x':{'bv':4},'y':{'bv':4}},'reset':{'x':'i.initial','y':B(0)},'next':{'x':stepx,'y':'s.y'},'outputs':{'can':active}},
 'binding':['and',eq('spec.x','impl.x'),eq('spec.y','impl.y')],'commit':'retire','can_step':'can',
 'progress':{'enabled':['ne','spec.x',B(0)],'rank':B(0)},
 'program_contract':{'parameters':{},'precondition':True,'invariant':True,'terminal':eq('s.x',B(0)),'postcondition':eq('s.x',B(0)),'rank':'s.x'}}
base['impl']=copy.deepcopy(base['spec']);base['impl']['outputs']={'retire':active}
def save(n,d):(D/(n+'.json')).write_text(json.dumps(d,indent=2)+'\n')
save('countdown_good',base)
# Upper-bound occurrence in a disjunction does not bound all admissible x.
# The complement must remain: x=9,y=0 satisfies inv, but y'=1,x'=8 does not.
x=copy.deepcopy(base)
x['program_contract']['invariant']=['or',['ule','s.x',B(2)],eq('s.y',B(0))]
for m in ('spec','impl'):x[m]['next']['y']=['ite',eq('s.x',B(9)),B(1),'s.y']
save('disjunction_bound_bad',x)
# A lower bound only on one side of implication also cannot restrict x globally.
x=copy.deepcopy(base)
x['program_contract']['invariant']=['implies',['ule',B(8),'s.x'],eq('s.y',B(0))]
for m in ('spec','impl'):x[m]['next']['y']=['ite',eq('s.x',B(10)),B(1),'s.y']
save('implication_bound_bad',x)
# Inverted bounds describe false, not an unsigned wraparound interval.
x=copy.deepcopy(base);x['program_contract']['invariant']=['or',['and',['ule',B(12),'s.x'],['ule','s.x',B(2)]],eq('s.y',B(0))]
for m in ('spec','impl'):x[m]['next']['y']=['ite',eq('s.x',B(7)),B(1),'s.y']
save('inverted_bound_bad',x)
# A literal outside the word width wraps; planning must use frontend semantics.
x=copy.deepcopy(base);x['program_contract']['invariant']=['or',eq('s.x',B(18)),eq('s.y',B(0))]
for m in ('spec','impl'):x[m]['next']['y']=['ite',eq('s.x',B(7)),B(1),'s.y']
save('wrapped_literal_bad',x)
print('built five independent fixtures')
