"""Replay every final v0.5 counterexample with the independent concrete IR evaluator."""
from pathlib import Path
import json,sys
HERE=Path(__file__).resolve().parent;ROOT=HERE.parents[1];sys.path.insert(0,str(HERE.parent))
from interpreter import evaluate,evaluate_record,outputs,related
from replay_program_counterexamples import context

def replay(doc,report,out,ob):
 ctx=context(out,ob);rec=lambda p:{k[len(p):]:v for k,v in ctx.items()if k.startswith(p)}
 i=rec('i.');name=ob['name']
 if name.startswith('program_'):
  c=doc['program_contract'];s=rec('s.');ns=evaluate_record(doc['spec'],'next',s,i);assert ns==rec('next.')
  p={k:v for k,v in ctx.items()if k.startswith('p.')};env={**{'s.'+k:v for k,v in s.items()},**p};nenv={**{'s.'+k:v for k,v in ns.items()},**p}
  active=evaluate(c['invariant'],env)and not evaluate(c['terminal'],env)
  if name.startswith('program_invariant_preserved'):
   assert active and not evaluate(c['invariant'],nenv)
   axes=[x for x in report['partition_plan'].get('candidates',[])if x['decision']=='selected']
   if axes:
    suffix='_'.join('auto_'+x['state']+'_'+(str(s[x['state']].v)if s[x['state']].v in x['values']else'other')for x in axes)
    assert name=='program_invariant_preserved_'+suffix
  elif name=='program_rank_decreases':assert active and evaluate(c['rank'],nenv).v>=evaluate(c['rank'],env).v
  elif name=='program_initialization':
   assert i[doc['reset_input']] and evaluate(c['precondition'],{**{'i.'+k:v for k,v in i.items()},**p})
   reset=evaluate_record(doc['spec'],'reset',{},i);assert not evaluate(c['invariant'],{**{'s.'+k:v for k,v in reset.items()},**p})
  elif name=='program_postcondition':assert evaluate(c['invariant'],env)and evaluate(c['terminal'],env)and not evaluate(c['postcondition'],env)
  elif name=='program_step_available':assert active and not outputs(doc['spec'],s,i)[doc['can_step']]
  elif name=='program_terminal_quiescent':assert evaluate(c['invariant'],env)and evaluate(c['terminal'],env)and outputs(doc['spec'],s,i)[doc['can_step']]
  else:raise AssertionError(name)
 else:
  s,t=rec('spec.'),rec('impl.')
  ns=evaluate_record(doc['spec'],'reset',{},i)if i[doc['reset_input']]else evaluate_record(doc['spec'],'next',s,i)if ctx['commit']else s
  nt=evaluate_record(doc['impl'],'reset',{},i)if i[doc['reset_input']]else evaluate_record(doc['impl'],'next',t,i)
  assert ns==rec('spec_next.')and nt==rec('impl_next.')
  if name=='microstep_refinement':assert related(doc,s,t)and not related(doc,ns,nt)
  elif name=='noncommit_rank_decreases':assert related(doc,s,t)and not i[doc['reset_input']]and ctx['progress_enabled']and not ctx['commit']and ctx['rank_next'].v>=ctx['rank'].v
  else:raise AssertionError(name)
 return name
rows=[]
for reportpath in sorted((ROOT/'results/structural_0_5/v0.5').glob('*/report.json')):
 report=json.loads(reportpath.read_text());obs=[o for o in report.get('obligations',[])if o['status']=='counterexample']
 if not obs:continue
 doc=json.loads((reportpath.parent/'input.json').read_text())
 rows.append({'case':reportpath.parent.name,'replayed':[replay(doc,report,reportpath.parent,o)for o in obs]})
assert rows,'no counterexamples present'
result={'status':'pass','scope':'concrete evaluator independent of Z3 and Rust rewriting; every final v0.5 SAT counterexample, including auto guard membership','runs':rows,'total_witnesses':sum(len(r['replayed'])for r in rows)}
(HERE/'replay_results.json').write_text(json.dumps(result,indent=2)+'\n');print('PASS',len(rows),'cases',result['total_witnesses'],'witnesses')
