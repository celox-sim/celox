#!/usr/bin/env python3
"""Deterministic exhaustive tiny-array oracle against all public v2 obligations.
No external SMT. Every array is a complete 2-cell Boolean-valued table.
Transient checker files recycled; compact reproducible inputs/verdicts retained.
"""
import argparse,hashlib,itertools,json,os,pathlib,random,subprocess,tempfile,time,sys
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('count',type=int,nargs='?',default=1000)
parser.add_argument('--root',type=pathlib.Path,default=pathlib.Path.cwd())
parser.add_argument('--out',type=pathlib.Path,required=True)
args=parser.parse_args()
if args.count < 1: parser.error('count must be positive')
ROOT=args.root.resolve();DEST=args.out.resolve();DEST.mkdir(parents=True,exist_ok=True)
SEED=391407;COUNT=args.count
rng=random.Random(SEED)
BV=lambda v:['bv',1,v]
EQ=lambda a,b:['eq',a,b]
IT=lambda c,a,b:['ite',c,a,b]
NOT=lambda a:['not',a]
A=['spec.A','spec.B','spec.C'];S=['spec.x','spec.y',BV(0),BV(1)]
def ar(depth):
 if depth<=0 or rng.randrange(3):return rng.choice(A)
 return IT(bo(depth-1),ar(depth-1),ar(depth-1))
def sc(depth):
 if depth<=0 or not rng.randrange(3):return rng.choice(S)
 if rng.randrange(3):return ['read',ar(depth-1),sc(depth-1)]
 return IT(bo(depth-1),sc(depth-1),sc(depth-1))
def bo(depth):
 if depth<=0:return EQ(rng.choice(S),rng.choice(S))
 op=rng.randrange(7)
 if op==0:return EQ(ar(depth-1),ar(depth-1))
 if op==1:return EQ(sc(depth-1),sc(depth-1))
 if op==2:return NOT(bo(depth-1))
 if op in [3,4]:return ['and' if op==3 else 'or',bo(depth-1),bo(depth-1)]
 if op==5:return IT(bo(depth-1),bo(depth-1),bo(depth-1))
 return EQ(['read',ar(depth-1),sc(depth-1)],['read',ar(depth-1),sc(depth-1)])
def ev(t,e):
 if isinstance(t,bool):return t
 if isinstance(t,str):return e[t]
 op,*a=t
 if op=='bv':return a[1]
 if op=='ite':return ev(a[1] if ev(a[0],e) else a[2],e)
 a=[ev(z,e) for z in a]
 if op=='eq':return a[0]==a[1]
 if op=='not':return not a[0]
 if op=='and':return a[0] and a[1]
 if op=='or':return a[0] or a[1]
 if op=='read':return a[0][a[1]]
 raise ValueError(op)
arrays=list(itertools.product(range(2),repeat=2))
assignments=[dict(zip(A+['spec.x','spec.y'],v)) for v in itertools.product(arrays,arrays,arrays,range(2),range(2))]
def oracle(t):
 yes=no=False
 for e in assignments:
  v=ev(t,e);yes|=v;no|=not v
 return yes,no
def document(t):
 s={k:{'mem':[1,1]} for k in ['A','B','C']};s.update(x={'bv':1},y={'bv':1})
 return {'version':2,'name':'independent_readonly_campaign','inputs':{'rst':'bool',**s},'reset_input':'rst','spec':{'state':s,'reset':{k:'i.'+k for k in s},'next':{k:'s.'+k for k in s},'outputs':{'can':True}},'impl':{'state':{'q':'bool'},'reset':{'q':False},'next':{'q':'s.q'},'outputs':{'commit':False}},'binding':t,'commit':'commit','can_step':'can','hold_when':True,'progress':{'enabled':True,'rank':BV(0)}}
started=time.monotonic();cases=[];totals={'sat':0,'unsat':0,'unknown':0};maxwork=0
binary=ROOT/'target/release/hwverify-rs';binary_hash=hashlib.sha256(binary.read_bytes()).hexdigest()
with tempfile.TemporaryDirectory(prefix='hwverify-readonly-audit-',dir=DEST) as temp:
 d=pathlib.Path(temp);path=d/'query.json';proof=d/'proof'
 tripwire=d/'z3-tripwire'
 tripwire.write_text('#!/bin/sh\nprintf invoked > "'+str(d/'z3-invoked')+'"\nexit 97\n')
 tripwire.chmod(0o755)
 for n in range(COUNT):
  t=bo(rng.choice([2,3,4]));yes,no=oracle(t)
  truth={'binding_nonempty':yes,'reset_binding':no,'microstep_refinement':yes and no,'commit_eligible':False,'hold_contract':False,'progress_nonvacuity':yes,'commit_reachable_in_relation':False,'noncommit_rank_decreases':yes}
  path.write_text(json.dumps(document(t)))
  p=subprocess.run([str(binary),str(path),'--out',str(proof)],capture_output=True,text=True,env={**os.environ,'HWVERIFY_SOLVER':'finite','Z3_BIN':str(tripwire)},timeout=60)
  try:report=json.loads(p.stdout);queries=report['obligations']
  except Exception:raise AssertionError((n,p.returncode,p.stdout,p.stderr))
  observed={};works={}
  if len(queries)!=len(truth) or {q['name'] for q in queries} != set(truth): raise AssertionError((n,report))
  for q in queries:
   name=q['name'];actual=q['solver_result'];want='sat' if truth[name] else 'unsat';totals[actual]+=1
   if actual!=want: raise AssertionError((n,t,name,want,actual,q.get('finite')))
   if actual=='sat' and q.get('finite',{}).get('original_formula_validated') is not True: raise AssertionError((n,name,'unvalidated SAT'))
   if q.get('backend') not in ['structural_kernel','finite_bv']: raise AssertionError((n,name,'unsupported backend'))
   observed[name]=actual;works[name]=q.get('finite',{}).get('work',0);maxwork=max(maxwork,works[name])
  cases.append({'number':n,'formula':t,'truth_has_true':yes,'truth_has_false':no,'verdicts':observed,'work':works})
  if (n+1)%100==0:print(json.dumps({'done':n+1,'elapsed':time.monotonic()-started}),flush=True)
 if (d/'z3-invoked').exists(): raise AssertionError('external solver invoked')
if hashlib.sha256(binary.read_bytes()).hexdigest()!=binary_hash: raise AssertionError('checker changed during campaign')
result={'seed':SEED,'case_count':COUNT,'query_count':COUNT*8,'domains':'3 independent total arrays, 1-bit addresses and values; 2 scalar 1-bit variables; exhaustive 256 assignments per formula','checker_sha256':binary_hash,'counts':totals,'max_query_work':maxwork,'elapsed_seconds':time.monotonic()-started,'cases':cases}
(DEST/'campaign-results.json').write_text(json.dumps(result,indent=2));print(json.dumps({k:v for k,v in result.items() if k!='cases'}))
