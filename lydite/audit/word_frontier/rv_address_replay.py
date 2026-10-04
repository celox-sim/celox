"""Bounded concrete replay of exact original model transitions; not a Rust verdict."""
import argparse,hashlib,json,sys,time
from pathlib import Path
if not __debug__:
 raise SystemExit("optimized Python is forbidden for counterexample replay")
REPO=Path(__file__).resolve().parents[2]
sys.setrecursionlimit(10000)
from audit.interpreter import BV,Mem,evaluate,evaluate_record,related
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--base',type=Path,required=True)
parser.add_argument('--mutants',type=Path,required=True)
parser.add_argument('--out',type=Path,required=True)
args=parser.parse_args()
BASE=args.base.resolve();MUTANTS=args.mutants.resolve();OUT=args.out.resolve()
OUT.mkdir(parents=True,exist_ok=False)
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def encode(x):
 if isinstance(x,BV):return {'bv':x.w,'value':x.v}
 if isinstance(x,Mem):return {'mem':[x.a,x.w],'default':x.default,'cells':[[k,v] for k,v in sorted(x.cells.items())]}
 if isinstance(x,dict):return {k:encode(v) for k,v in x.items()}
 if isinstance(x,list):return [encode(v) for v in x]
 return x

def step(doc,s,t,inputs):
 commit=evaluate_record(doc['impl'],'outputs',t,inputs)[doc['commit']]
 assert type(commit) is bool
 if inputs[doc['reset_input']]:
  ns=evaluate_record(doc['spec'],'reset',{},inputs);nt=evaluate_record(doc['impl'],'reset',{},inputs)
 else:
  ns=evaluate_record(doc['spec'],'next',s,inputs) if commit else s.copy()
  nt=evaluate_record(doc['impl'],'next',t,inputs)
 return ns,nt,commit

def false_parts(e,env):
 todo=[e];bad=[]
 while todo:
  t=todo.pop()
  if isinstance(t,list) and t[0]=='and':todo.extend(reversed(t[1:]));continue
  if evaluate(t,env) is False:bad.append(t)
 return bad

base=json.loads(BASE.read_text())
assert sha(BASE)=='7a53eebbc19beef3fee8a46d2944334c888d3487968a8df7b1c4d459f3a64623'
# LW x1,0(x0); ECALL. These are concrete witness inputs, not assumptions
# added to either the model or the automatic proof engine.
program=[0x00002083,0x00000073]
results=[]
for name in ['active_plus_one','complement_plus_one']:
 start=time.monotonic();path=MUTANTS/(name+'.json');doc=json.loads(path.read_text())
 changed=doc['impl']['next']['m_address']
 doc['impl']['next']['m_address']=base['impl']['next']['m_address']
 assert doc==base, 'non-address model field changed'
 doc['impl']['next']['m_address']=changed
 top=base['impl']['next']['m_address'];seen=set()
 while isinstance(top,str) and top.startswith('w.'):
  assert top not in seen and len(seen)<64
  seen.add(top);top=base['impl']['wires'][top[2:]]
 assert isinstance(top,list) and len(top)==4 and top[0]=='ite'
 expected=list(top);arm=2 if name=='active_plus_one' else 3
 expected[arm]=['add',top[arm],['bv',32,1]]
 assert changed==expected, 'not the exact declared model-level plus-one mutant'
 inputs={'rst':True,'stall':False,'seed_rom':Mem(7,32,0,dict(enumerate(program))),
         'seed_data':Mem(7,32,0,{0:0x31415926}),'seed_limit':BV(7,64)}
 s=evaluate_record(doc['spec'],'reset',{},inputs);t=evaluate_record(doc['impl'],'reset',{},inputs)
 assert related(doc,s,t) is True
 trace=[];found=None
 for cycle in range(16):
  inputs['rst']=False
  inputs['stall']=name=='complement_plus_one' and t['m_valid'] is True
  before=related(doc,s,t);assert before is True
  guard=evaluate(doc['impl']['next']['m_address'][1],{**{'s.'+k:v for k,v in t.items()},**{'i.'+k:v for k,v in inputs.items()}},doc['impl']['wires'])
  ns,nt,commit=step(doc,s,t,inputs)
  after=related(doc,ns,nt)
  bs,bt,bc=step(base,s,t,inputs)
  baseline_after=related(base,bs,bt)
  row={'cycle':cycle,'rst':False,'stall':inputs['stall'],'commit':commit,'actual_mutated_ite_guard':guard,
       'binding_before':before,'binding_after':after,'baseline_binding_after_same_prestate':baseline_after,
       'original_bad_formula':before and not after}
  trace.append(row)
  if not after:
   env={**{'spec.'+k:v for k,v in ns.items()},**{'impl.'+k:v for k,v in nt.items()}}
   assert baseline_after is True
   assert guard is (name=='active_plus_one')
   assert [k for k in bt if bt[k]!=nt[k]]==['m_address']
   assert nt['m_address']==BV(32,bt['m_address'].v+1)
   found={**row,'inputs':encode(inputs),'spec_before':encode(s),'impl_before':encode(t),
          'spec_after':encode(ns),'impl_after':encode(nt),'baseline_impl_after':encode(bt),
          'false_binding_conjuncts':false_parts(doc['binding'],env)}
   break
  s,t=ns,nt
 result={'name':name,'source_sha256':sha(path),'input_base_sha256':sha(BASE),
         'status':'concrete_original_counterexample_replayed' if found else 'no_witness_in_16_cycles',
         'cycle_bound':16,'reset_binding_checked':True,'trace':trace,'witness':found,
         'seconds':time.monotonic()-start,'rust_original_microstep_status':'unknown',
         'rust_report_unchanged':True,'not_counted_as_rust_original_sat_success':True}
 (OUT/(name+'.json')).write_text(json.dumps(result,indent=2)+'\n');results.append(result)
 print(name,result['status'],len(trace),result['seconds'],flush=True)
summary={'scope':'Independent concrete interpreter replay of exact original model binding and reset/commit-priority transitions; model-level mutations, not a fresh RTL import or Rust solver verdict',
 'script_sha256':sha(Path(__file__)),'interpreter_sha256':sha(REPO/'audit/interpreter.py'),
 'base_input_sha256':sha(BASE),'program_words':program,'maximum_cycles_per_mutant':16,
 'original_microstep_formula':'binding_before AND NOT binding_after, with reset/commit transition exactly as the checker',
 'no_assumptions_added_to_models':True,'rust_original_sat_successes':0,
 'concrete_original_counterexamples':sum(r['witness'] is not None for r in results),
 'results':[{'name':r['name'],'status':r['status'],'cycle':r['witness']['cycle'] if r['witness'] else None,'seconds':r['seconds'],'report_sha256':sha(OUT/(r['name']+'.json'))} for r in results]}
(OUT/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
assert summary['concrete_original_counterexamples']==2
