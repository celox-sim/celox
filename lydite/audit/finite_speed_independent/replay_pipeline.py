"""Re-evaluate every finite SAT witness on original JSON using Python semantics."""
from pathlib import Path
import json,sys,argparse
ROOT=Path(__file__).resolve().parents[2];sys.path.insert(0,str(ROOT/'audit'))
from interpreter import BV,evaluate,evaluate_record,outputs,related

def decode(x):return x['value'] if x['sort']=='Bool' else BV(x['width'],x['value'])
def record(prefix,types,values):
 # Unmentioned symbols have no influence on this formula; complete them with zero.
 return {k:decode(values[prefix+k]) if prefix+k in values else (False if t=='bool' else BV(t['bv'],0)) for k,t in types.items()}
p=argparse.ArgumentParser();p.add_argument("--input",type=Path,required=True);p.add_argument("--output",type=Path,required=True);args=p.parse_args()
rows=[]
for folder in sorted(args.input.iterdir()):
 doc=json.loads((ROOT/'examples'/f'{folder.name}.json').read_text())
 for file in sorted(folder.glob('*.finite.json')):
  data=json.loads(file.read_text())
  if data['solver_result']!='sat':continue
  values=data['assignments'];s=record('spec_',doc['spec']['state'],values);t=record('impl_',doc['impl']['state'],values);i=record('input_',doc['inputs'],values)
  env={**{'spec.'+k:v for k,v in s.items()},**{'impl.'+k:v for k,v in t.items()},**{'i.'+k:v for k,v in i.items()}}
  before=related(doc,s,t);commit=outputs(doc['impl'],t,i)['commit'];can=outputs(doc['spec'],s,i)['can_step']
  sn=evaluate_record(doc['spec'],'reset' if i['rst'] else 'next',s,i) if i['rst'] or commit else s
  tn=evaluate_record(doc['impl'],'reset' if i['rst'] else 'next',t,i)
  after=related(doc,sn,tn);enabled=evaluate(doc['progress']['enabled'],env);rank=evaluate(doc['progress']['rank'],env)
  next_env={**{'spec.'+k:v for k,v in sn.items()},**{'impl.'+k:v for k,v in tn.items()}}
  rankn=evaluate(doc['progress']['rank'],next_env)
  name=file.name.removesuffix('.finite.json')
  formula={
   'binding_nonempty':before,
   'progress_nonvacuity':before and not i['rst'] and enabled,
   'commit_reachable_in_relation':before and not i['rst'] and enabled and commit,
   'microstep_refinement':before and not after,
   'commit_eligible':before and not i['rst'] and commit and not can,
   'hold_contract':before and not i['rst'] and evaluate(doc['hold_when'],env) and t!=tn,
   'noncommit_rank_decreases':before and not i['rst'] and enabled and not commit and not rankn.v<rank.v,
  }
  assert name in formula and formula[name],(folder.name,name)
  expected=env|{'commit':commit,'binding_before':before,'binding_after':after,'rank':rank,'rank_next':rankn,'progress_enabled':enabled}|{'spec_next.'+k:v for k,v in sn.items()}|{'impl_next.'+k:v for k,v in tn.items()}
  for key,value in data['context_values'].items():assert expected[key]==decode(value),(folder.name,name,key,expected[key],value)
  rows.append({'model':folder.name,'query':name,'context_values_replayed':len(data['context_values'])})
result={'status':'pass','sat_witnesses_replayed':len(rows),'rows':rows}
args.output.write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result))
