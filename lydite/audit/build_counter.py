"""A second consumer: split-register counter, independent of CPU field names."""
from pathlib import Path
import json,copy
ROOT=Path(__file__).resolve().parents[1]
b=lambda w,n:['bv',w,n]
eq=lambda a,c:['eq',a,c]
ite=lambda c,a,d:['ite',c,a,d]
run=['not','i.freeze'];execute=eq('s.phase',b(1,1));retire=['and',run,execute]
spec={'state':{'count':{'bv':16}},'reset':{'count':b(16,0)},'next':{'count':['add','s.count',b(16,1)]},'outputs':{'allowed':True}}
impl={'state':{'hi':{'bv':8},'lo':{'bv':8},'phase':{'bv':1}},'reset':{'hi':b(8,0),'lo':b(8,0),'phase':b(1,0)},'next':{'lo':ite(retire,['add','s.lo',b(8,1)],'s.lo'),'hi':ite(['and',retire,eq('s.lo',b(8,255))],['add','s.hi',b(8,1)],'s.hi'),'phase':ite(run,['bxor','s.phase',b(1,1)],'s.phase')},'outputs':{'retire':retire}}
d={'version':2,'name':'16-bit count vs split 8-bit registers','inputs':{'clear':'bool','freeze':'bool'},'reset_input':'clear','spec':spec,'impl':impl,'binding':eq('spec.count',['concat','impl.hi','impl.lo']),'commit':'retire','can_step':'allowed','hold_when':'i.freeze','progress':{'enabled':run,'rank':ite(eq('impl.phase',b(1,0)),b(1,1),b(1,0))}}
(ROOT/'examples/split_counter.json').write_text(json.dumps(d,indent=2)+'\n')
x=copy.deepcopy(d);x['impl']['next']['hi'][1][2][2]=b(8,0)
(ROOT/'examples/split_counter_bad_carry.json').write_text(json.dumps(x,indent=2)+'\n')
