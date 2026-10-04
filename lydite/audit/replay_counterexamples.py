"""Parse SMT get-value data and replay obligations without importing Z3."""
from pathlib import Path
import json,re,subprocess,sys,copy,os,shutil
from interpreter import BV,Mem,evaluate,evaluate_record,outputs,related
from cpu_simulation import reset,cycle,isa
ROOT=Path(__file__).resolve().parents[1]
BIN=ROOT/'../target/debug/lydite'
Z3=os.environ.get('Z3_BIN') or shutil.which('z3') or str(ROOT.parent/'proof-binding-study/.venv/bin/z3')

def sexprs(text):
 ts=re.findall(r'\(|\)|\|[^|]*\||"(?:\\.|[^"\\])*"|[^\s()]+',text);pos=0
 def one():
  nonlocal pos
  x=ts[pos];pos+=1
  if x!='(':return x
  a=[]
  while ts[pos]!=')':a.append(one())
  pos+=1;return a
 out=[]
 while pos<len(ts):out.append(one())
 return out

def val(x):
 if x=='true':return True
 if x=='false':return False
 if isinstance(x,str) and x.startswith('#x'):return BV((len(x)-2)*4,int(x[2:],16))
 if isinstance(x,str) and x.startswith('#b'):return BV(len(x)-2,int(x[2:],2))
 if isinstance(x,list) and x[:1]==['_'] and x[1].startswith('bv'):return BV(int(x[2]),int(x[1][2:]))
 if isinstance(x,list) and len(x)==2 and isinstance(x[0],list) and x[0][:2]==['as','const']:
  sort=x[0][2];d=val(x[1]);return Mem(int(sort[1][2]),int(sort[2][2]),d.v,{})
 if isinstance(x,list) and x[:1]==['store']:return val(x[1]).write(val(x[2]),val(x[3]))
 raise ValueError('unreplayable SMT value, not evaluated as code: '+repr(x))

def replay(d,ob,out):
 roots=sexprs((out/ob['solver_output']).read_text());pairs=roots[-1]
 assert roots[0]=='sat' and all(isinstance(p,list) and len(p)==2 for p in pairs)
 values={p[0]:val(p[1]) for p in pairs}
 ctx={n:values[sym] for n,sym in ob['context_symbols'].items()}
 rec=lambda prefix:{k[len(prefix):]:v for k,v in ctx.items() if k.startswith(prefix)}
 s=rec('spec.');t=rec('impl.');i=rec('i.')
 r=related(d,s,t)
 ns=reset(d['spec'],i) if i['rst'] else evaluate_record(d['spec'],'next',s,i) if ctx['commit'] else s
 nt=cycle(d['impl'],t,i)
 assert ns==rec('spec_next.') and nt==rec('impl_next.')
 assert r==ctx['binding_before'] and related(d,ns,nt)==ctx['binding_after']
 name=ob['name']
 if name=='reset_binding':assert not related(d,reset(d['spec'],i),reset(d['impl'],i))
 elif name=='microstep_refinement':assert r and not related(d,ns,nt)
 elif name=='commit_eligible':assert r and not i['rst'] and ctx['commit'] and not outputs(d['spec'],s,i)[d['can_step']]
 elif name=='hold_contract':
  env={**{'spec.'+k:v for k,v in s.items()},**{'impl.'+k:v for k,v in t.items()},**{'i.'+k:v for k,v in i.items()}}
  assert r and not i['rst'] and evaluate(d['hold_when'],env) and nt!=t
 elif name=='noncommit_rank_decreases':
  assert r and not i['rst'] and ctx['progress_enabled'] and not ctx['commit'] and not ctx['rank_next'].v<ctx['rank'].v
 else:raise AssertionError(name)
 if not i['rst'] and ctx['commit'] and not s['halted']:assert ns==isa(s)[0]
 return {'obligation':name,'replayed':True,'pc':s['pc'].v,'acc':s['acc'].v,'phase':t['phase'].v,'commit':ctx['commit'],'reset':i['rst'],'stall':i['stall']}

if __name__=='__main__':
 base=json.loads((ROOT/'examples/cpu.json').read_text())
 fixtures=ROOT/'audit/fixtures';fixtures.mkdir(exist_ok=True)
 d=copy.deepcopy(base);d['impl']['next']['mem'][2][2][2]=['add','w.imm',['bv',8,1]]
 (fixtures/'wrong_store.json').write_text(json.dumps(d,indent=2)+'\n')
 d=copy.deepcopy(base);d['impl']['reset']['acc']=['bv',8,1]
 (fixtures/'wrong_reset.json').write_text(json.dumps(d,indent=2)+'\n')
 paths=[ROOT/'examples'/f'{n}.json' for n in ['wrong_load','wrong_branch','ignores_stall','missing_commit','hang_fetch','commit_after_halt']]+list(fixtures.glob('*.json'))
 rows=[]
 for p in paths:
  d=json.loads(p.read_text());out=ROOT/'audit/results'/p.stem;out.mkdir(parents=True,exist_ok=True)
  run=subprocess.run([str(BIN),str(p),'--z3',str(Z3),'--out',str(out)],capture_output=True,text=True,timeout=60)
  (out/'run.log').write_text(run.stdout+run.stderr);assert run.returncode==1,(p,run.returncode,run.stdout)
  report=json.loads((out/'report.json').read_text());assert report['status']=='counterexample'
  witnesses=[replay(d,o,out) for o in report['obligations'] if o['status']=='counterexample']
  assert witnesses
  rows.append({'case':p.stem,'witnesses':witnesses})
 (ROOT/'audit/replay_results.json').write_text(json.dumps(rows,indent=2)+'\n')
 print({'cases':len(rows),'counterexamples':sum(len(r['witnesses']) for r in rows),'all_replayed':True})
