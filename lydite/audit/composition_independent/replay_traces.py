"""Replay all admitted examples from complete SAT witnesses using concrete semantics."""
from pathlib import Path
import argparse,json,sys
HERE=Path(__file__).resolve().parent;sys.path.insert(0,str(HERE.parent))
from interpreter import evaluate
from replay_program_counterexamples import context

def replay(doc,report,out):
 rows=[]
 for ex in report['examples']:
  if ex['admitted'] is not True:continue
  definition=doc['components' if ex['target_kind']=='component' else 'compositions'][ex['target']]['examples'][ex['example']]
  ob=ex['evidence'];ctx=context(out,ob) if ob['context_symbols'] else {}
  def record(prefix):return {k[len(prefix):]:v for k,v in ctx.items() if k.startswith(prefix)}
  for frame in range(len(definition['trace'])+1):
   obs=record(f'frame{frame}.o.');assert set(obs)==set(doc['observations'])
   assignment=definition['initial'] if frame==0 else definition['trace'][frame-1]['observe']
   for name,value in assignment.items():assert obs[name]==evaluate(value,{})
   for component in ex['members']:
    c=doc['components'][component];state=record(f'frame{frame}.{component}.');assert set(state)==set(c['state'])
    env={**{'s.'+k:v for k,v in state.items()},**{'o.'+k:v for k,v in obs.items()}}
    assert evaluate(c['invariant'],env)
    if frame==0:assert evaluate(c['init'],env)
    if frame<len(definition['trace']):
     step=definition['trace'][frame];inp=record(f'step{frame}.i.');assert set(inp)==set(doc['inputs'])
     for name,value in step['inputs'].items():assert inp[name]==evaluate(value,{})
     env|={**{'n.'+k:v for k,v in record(f'frame{frame+1}.{component}.').items()},**{'no.'+k:v for k,v in record(f'frame{frame+1}.o.').items()},**{'i.'+k:v for k,v in inp.items()}}
     assert evaluate(c['steps'][step['operation']],env)
  rows.append({'target':ex['target'],'example':ex['example'],'steps':len(definition['trace']),'replayed':True})
 return rows
if __name__=='__main__':
 p=argparse.ArgumentParser();p.add_argument('--source',required=True,type=Path);p.add_argument('--results',required=True,type=Path);p.add_argument('--out',required=True,type=Path);a=p.parse_args()
 rows=replay(json.loads(a.source.read_text()),json.loads((a.results/'report.json').read_text()),a.results)
 assert rows,'no SAT witnesses to replay';a.out.write_text(json.dumps({'status':'pass','witnesses':rows,'count':len(rows)},indent=2)+'\n');print(len(rows),'SAT traces independently replayed')
