"""Replay actual SMT counterexamples to binding with the independent interpreter."""
from pathlib import Path
import argparse,json,sys
HERE=Path(__file__).resolve().parent;sys.path.insert(0,str(HERE.parent))
from interpreter import evaluate,evaluate_record
from replay_program_counterexamples import context
from finite_oracle import members

def replay(doc,report,out):
 m=doc['implementation'];names=members(doc,m['composition']);rows=[]
 def abstraction(s):
  env={'s.'+k:v for k,v in s.items()};return ({c:{k:evaluate(e,env) for k,e in fields.items()} for c,fields in m['binding']['states'].items()},{k:evaluate(e,env) for k,e in m['binding']['observations'].items()})
 def scope(c,p,o):return {**{'s.'+k:v for k,v in p[c].items()},**{'o.'+k:v for k,v in o.items()}}
 def invariant(p,o):return all(evaluate(doc['components'][c]['invariant'],scope(c,p,o)) for c in names)
 for ob in report['implementation_binding']['obligations']:
  if ob['status']!='counterexample':continue
  ctx=context(out,ob);rec=lambda pre:{k[len(pre):]:v for k,v in ctx.items() if k.startswith(pre)}
  s=rec('impl.');i=rec('i.');before,obs=abstraction(s)
  if ob['name']=='binding_reset_establishes_product':
   assert i[m['reset_input']];p,o=abstraction(evaluate_record(m,'reset',{},i));assert not invariant(p,o) or not all(evaluate(doc['components'][c]['init'],scope(c,p,o)) for c in names)
  else:
   assert not i[m['reset_input']] and invariant(before,obs)
   ns=evaluate_record(m,'next',s,i);assert ns==rec('impl_next.');after,no=abstraction(ns);assert obs==rec('o.') and no==rec('no.')
   env={**{'s.'+k:v for k,v in s.items()},**{'i.'+k:v for k,v in i.items()}}
   ops=[op for op,e in m['operations'].items() if evaluate(e,env,m.get('wires',{}))]
   if ob['name']=='binding_operation_exclusive':assert len(ops)>1
   elif ob['name']=='binding_no_operation_stutters':assert not ops and (before!=after or obs!=no)
   elif ob['name'].startswith('binding_operation_'):
    op=ob['operation'];assert op in ops
    relation=all(evaluate(doc['components'][c]['steps'][op],scope(c,before,obs)|{**{'n.'+k:v for k,v in after[c].items()},**{'no.'+k:v for k,v in no.items()},**{'i.'+k:v for k,v in i.items()}}) for c in names)
    assert not relation or not invariant(after,no)
   else:raise AssertionError(ob['name'])
  rows.append({'obligation':ob['name'],'replayed':True})
 return rows
if __name__=='__main__':
 p=argparse.ArgumentParser();p.add_argument('--results',required=True,type=Path);p.add_argument('--out',required=True,type=Path);a=p.parse_args();rows=[]
 for path in sorted(a.results.glob('*/report.json')):
  d=json.loads((path.parent/'input.json').read_text());r=json.loads(path.read_text());items=replay(d,r,path.parent)
  if items:rows.append({'case':path.parent.name,'witnesses':items})
 assert rows;a.out.write_text(json.dumps({'status':'pass','cases':rows,'count':sum(len(r['witnesses']) for r in rows)},indent=2)+'\n');print(sum(len(r['witnesses']) for r in rows),'binding counterexamples replayed')
