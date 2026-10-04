"""Exhaustive independent state/input check of the restricted abstraction bridge."""
from pathlib import Path
import argparse,copy,itertools,json,subprocess,sys
HERE=Path(__file__).resolve().parent;ROOT=HERE.parents[1];sys.path.insert(0,str(HERE.parent))
from interpreter import BV,evaluate,evaluate_record
from finite_oracle import members,domain

def check(doc):
 m=doc['implementation'];names=members(doc,m['composition']);skeys=list(m['state']);ikeys=list(doc['inputs']);bad={};combinations=0
 def abstract(state):
  env={'s.'+k:v for k,v in state.items()};private={c:{k:evaluate(e,env) for k,e in fields.items()} for c,fields in m['binding']['states'].items()};obs={k:evaluate(e,env) for k,e in m['binding']['observations'].items()};return private,obs
 def scope(c,private,obs):return {**{'s.'+k:v for k,v in private[c].items()},**{'o.'+k:v for k,v in obs.items()}}
 def invariant(private,obs):return all(evaluate(doc['components'][c]['invariant'],scope(c,private,obs)) for c in names)
 for values in itertools.product(*(domain(t) for t in m['state'].values())):
  state=dict(zip(skeys,values));before,obs=abstract(state)
  for values in itertools.product(*(domain(t) for t in doc['inputs'].values())):
   inp=dict(zip(ikeys,values));combinations+=1
   def fail(label):bad.setdefault(label,{'state':{k:v.v if isinstance(v,BV) else v for k,v in state.items()},'input':{k:v.v if isinstance(v,BV) else v for k,v in inp.items()}})
   if inp[m['reset_input']]:
    reset=evaluate_record(m,'reset',{},inp);private,ob=abstract(reset)
    if not invariant(private,ob) or not all(evaluate(doc['components'][c]['init'],scope(c,private,ob)) for c in names):fail('reset')
    continue
   if not invariant(before,obs):continue
   nxt=evaluate_record(m,'next',state,inp);after,no=abstract(nxt)
   env={**{'s.'+k:v for k,v in state.items()},**{'i.'+k:v for k,v in inp.items()}}
   selected=[op for op,expr in m['operations'].items() if evaluate(expr,env,m.get('wires',{}))]
   if len(selected)>1:fail('exclusive')
   if not selected and (before!=after or obs!=no):fail('stutter')
   for op in selected:
    if not invariant(after,no):fail('next_invariant')
    for c in names:
     e=scope(c,before,obs)|{**{'n.'+k:v for k,v in after[c].items()},**{'no.'+k:v for k,v in no.items()},**{'i.'+k:v for k,v in inp.items()}}
     if not evaluate(doc['components'][c]['steps'][op],e):fail('step:'+c)
 return {'verified':not bad,'combinations':combinations,'bad':bad}
if __name__=='__main__':
 p=argparse.ArgumentParser();p.add_argument('--binary',required=True,type=Path);p.add_argument('--z3',required=True);p.add_argument('--out',type=Path,default=ROOT/'results/composition_0_7_bridge_oracle');a=p.parse_args();a.out.mkdir(parents=True,exist_ok=True)
 base=json.loads((ROOT/'examples/budgeted_counter.json').read_text());cases={'good':base}
 d=copy.deepcopy(base);d['implementation']['next']['count']=['ite','w.accept',['add',['add','s.count','i.amount'],['bv',4,1]],'s.count'];cases['wrong_arithmetic']=d
 d=copy.deepcopy(base);d['implementation']['reset']['remaining']=['bv',4,4];cases['wrong_reset']=d
 d=copy.deepcopy(base);d['implementation']['operations']['add']=False;d['implementation']['next']['count']=['add','s.count',['bv',4,1]];cases['unlabelled_change']=d
 d=copy.deepcopy(base);d['implementation']['operations']['add']=False;d['implementation']['next']={k:'s.'+k for k in d['implementation']['state']};cases['eternal_stutter_safety_only']=d
 rows=[]
 for name,doc in cases.items():
  expected=check(doc);out=a.out/name;out.mkdir(parents=True,exist_ok=True);source=out/'input.json';source.write_text(json.dumps(doc,indent=2)+'\n')
  run=subprocess.run([str(a.binary.resolve()),str(source),'--z3',a.z3,'--out',str(out)],capture_output=True,text=True,timeout=90)
  (out/'run.stdout').write_text(run.stdout);(out/'run.stderr').write_text(run.stderr)
  report=json.loads((out/'report.json').read_text());binding=report['implementation_binding'];assert binding['status']==('verified' if expected['verified'] else 'failed'),(name,report,expected)
  rows.append({'case':name,**expected,'reported_status':binding['status']});print(name,binding['status'],expected['combinations'],flush=True)
 (HERE/'bridge_results.json').write_text(json.dumps({'status':'pass','cases':rows,'total_state_input_combinations':sum(r['combinations'] for r in rows)},indent=2)+'\n')
