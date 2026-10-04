"""Independent concrete small-width oracle for new memory increment workloads.
No Z3; exhaust all four 2-bit memory cells and addresses, two stall schedules.
"""
from pathlib import Path
import itertools,json,sys
HERE=Path(__file__).resolve().parent;sys.path.insert(0,str(HERE.parent))
from interpreter import BV,Mem,evaluate_record,outputs,related

def small(x):
 if isinstance(x,list):
  r=[small(a)for a in x]
  if r and r[0]=='bv' and r[1]==8:r[1]=2
  return r
 if isinstance(x,dict):
  r={k:small(v)for k,v in x.items()}
  if r=={'bv':8}:return {'bv':2}
  if r=={'mem':[2,8]}:return {'mem':[2,2]}
  return r
 return x
rows=[]
for name in ['memory_increment_good','memory_increment_bad_shadow','memory_increment_bad_alias']:
 d=small(json.loads((HERE/(name+'.json')).read_text()));trials=cycles=0;witness=None
 for cells in itertools.product(range(4),repeat=4):
  for address in range(4):
   for stalled in [False,True]:
    mem=Mem(2,2,0,dict(enumerate(cells)));i={'rst':True,'stall':False,'address':BV(2,address),'mem':mem,**{f'distract{k}':BV(2,k)for k in range(3)}}
    s=evaluate_record(d['spec'],'reset',{},i);t=evaluate_record(d['impl'],'reset',{},i);commits=0
    for cycle in range(12):
     # Change live reset inputs after reset; stored state is authoritative.
     i.update(rst=False,stall=stalled and cycle%3==0,address=BV(2,address+1),mem=Mem(2,2,3,{}))
     commit=outputs(d['impl'],t,i)['commit'];ns=evaluate_record(d['spec'],'next',s,i)if commit else s;nt=evaluate_record(d['impl'],'next',t,i);cycles+=1;commits+=commit
     if not related(d,ns,nt):witness={'cells':cells,'address':address,'stall_pattern':stalled,'cycle':cycle};break
     s,t=ns,nt
    trials+=1
    if witness:break
    assert commits==3 and s['count'].v==0
    assert s['shadow'].v==(cells[address]+3)%4, (name,cells,address,stalled,s['shadow'],commits,s)
    for a in range(4):assert s['mem'].read(BV(2,a)).v==(cells[a]+(3 if a==address else 0))%4
   if witness:break
  if witness:break
 if name.endswith('good'):assert witness is None and trials==2048
 else:assert witness is not None
 rows.append({'name':name,'trials':trials,'cycles':cycles,'mismatch':witness,'status':'pass'if witness is None else 'deliberate_mutation_rejected'})
(HERE/'simulation_results.json').write_text(json.dumps({'status':'pass','scope':'exhaustive 2-bit data, 4 cells, all addresses, two fixed stall patterns; not an unbounded proof','runs':rows},indent=2)+'\n')
print(json.dumps(rows,indent=2))
