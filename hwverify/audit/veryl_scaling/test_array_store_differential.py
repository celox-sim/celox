#!/usr/bin/env python3
"""Independent explicit total-array enumeration; no solver implementation imports."""
import argparse, hashlib, itertools, json, os, pathlib, random, subprocess, tempfile, time
P=argparse.ArgumentParser();P.add_argument('--count',type=int,default=1000);P.add_argument('--out',type=pathlib.Path,required=True);P.add_argument('--root',type=pathlib.Path,default=pathlib.Path(__file__).resolve().parents[2]);a=P.parse_args()
if a.count < 0:P.error('count must be nonnegative')
ROOT=a.root.resolve();BIN=ROOT/'target/release/hwverify-rs';a.out.mkdir(parents=True,exist_ok=True)
R=random.Random(620104); A,B,C='spec.A','spec.B','spec.C';X,Y='spec.x','spec.y'; BV=lambda n:['bv',1,n];EQ=lambda x,y:['eq',x,y];W=lambda x,i,v:['write',x,i,v];RD=lambda x,i:['read',x,i];N=lambda x:['not',x];I=lambda c,x,y:['ite',c,x,y];AND=lambda x,y:['and',x,y]
def array(d):
 if d<=0:return R.choice([A,B,C])
 op=R.randrange(5)
 if op<2:return R.choice([A,B,C])
 if op<4:return W(array(d-1),scalar(d-1),scalar(d-1))
 return I(boolean(d-1),array(d-1),array(d-1))
def scalar(d):
 if d<=0 or R.randrange(4)==0:return R.choice([X,Y,BV(0),BV(1)])
 if R.randrange(3):return RD(array(d-1),scalar(d-1))
 return I(boolean(d-1),scalar(d-1),scalar(d-1))
def boolean(d):
 if d<=0:return EQ(scalar(0),scalar(0))
 op=R.randrange(6)
 if op==0:return EQ(array(d),array(d))
 if op==1:return EQ(scalar(d),scalar(d))
 if op==2:return N(boolean(d-1))
 if op==3:return I(boolean(d-1),boolean(d-1),boolean(d-1))
 return ['and' if op==4 else 'or',boolean(d-1),boolean(d-1)]
def ev(t,e):
 if isinstance(t,bool):return t
 if isinstance(t,str):return e[t]
 op,*q=t
 if op=='bv':return q[1]
 if op=='ite':return ev(q[1] if ev(q[0],e) else q[2],e)
 q=[ev(z,e) for z in q]
 if op=='eq':return q[0]==q[1]
 if op=='not':return not q[0]
 if op=='and':return q[0] and q[1]
 if op=='or':return q[0] or q[1]
 if op=='read':return q[0][q[1]]
 if op=='write':
  z=list(q[0]);z[q[1]]=q[2];return tuple(z)
 raise ValueError(op)
arrays=list(itertools.product(range(2),repeat=2)); envs=[dict(zip([A,B,C,X,Y],z)) for z in itertools.product(arrays,arrays,arrays,range(2),range(2))]
def truth(t):
 vals={ev(t,e) for e in envs};return True in vals,False in vals
def doc(t):
 s={n:{'mem':[1,1]} for n in ['A','B','C']};s.update(x={'bv':1},y={'bv':1})
 return {'version':2,'name':'independent_mutable_array_oracle','inputs':{'rst':'bool',**s},'reset_input':'rst','spec':{'state':s,'reset':{k:'i.'+k for k in s},'next':{k:'s.'+k for k in s},'outputs':{'can':True}},'impl':{'state':{'q':'bool'},'reset':{'q':False},'next':{'q':'s.q'},'outputs':{'commit':False}},'binding':t,'commit':'commit','can_step':'can','hold_when':True,'progress':{'enabled':True,'rank':BV(0)}}
cases=[('store_only_incompatible',AND(EQ(W(A,BV(0),BV(1)),W(A,BV(1),BV(1))),EQ(W(A,BV(0),BV(0)),W(A,BV(1),BV(0))))),('same_address_last_wins',N(EQ(W(W(A,X,Y),X,BV(0)),W(A,X,BV(0))))),('frame',AND(N(EQ(X,Y)),N(EQ(RD(W(A,X,BV(1)),Y),RD(A,Y))))),('read_written',N(EQ(RD(W(A,X,Y),X),Y))),('zero_store_sparse',AND(EQ(RD(A,X),BV(1)),EQ(W(A,X,BV(0)),A))),('conditional_store',N(EQ(RD(I(EQ(X,Y),W(A,X,BV(1)),W(B,Y,BV(0))),X),I(EQ(X,Y),BV(1),RD(B,X))))),('store_extensional_transitivity',AND(EQ(W(A,X,Y),B),AND(EQ(B,C),N(EQ(W(A,X,Y),C))))),('nested_index',N(EQ(RD(W(A,RD(B,X),RD(C,Y)),RD(B,X)),RD(C,Y))))]
# Additional deterministic store-equality formulas receive exhaustive original-array interpretation.
for i in range(80):
 left=array(3);right=array(3);cases.append(('focused_store_eq_'+str(i), EQ(W(left,scalar(2),scalar(2)),right)))
for i in range(a.count):cases.append(('random_'+str(i),boolean(R.choice([2,3,4]))))
start=time.monotonic();binary_hash=hashlib.sha256(BIN.read_bytes()).hexdigest();results=[];counts={'sat':0,'unsat':0};replays=0
with tempfile.TemporaryDirectory(prefix='transient-',dir=a.out) as tmp:
 tmp=pathlib.Path(tmp);trip=tmp/'z3';trip.write_text('#!/bin/sh\ntouch "'+str(tmp/'external-used')+'"\nexit 97\n');trip.chmod(0o755)
 for number,(name,t) in enumerate(cases):
  yes,no=truth(t);want={'binding_nonempty':yes,'reset_binding':no,'microstep_refinement':yes and no,'commit_eligible':False,'hold_contract':False,'progress_nonvacuity':yes,'commit_reachable_in_relation':False,'noncommit_rank_decreases':yes}
  (tmp/'doc.json').write_text(json.dumps(doc(t)));p=subprocess.run([str(BIN),str(tmp/'doc.json'),'--out',str(tmp/'proof')],env={**os.environ,'HWVERIFY_SOLVER':'finite','Z3_BIN':str(trip)},capture_output=True,text=True,timeout=60)
  try:qs=json.loads(p.stdout)['obligations']
  except Exception:raise AssertionError((name,p.returncode,p.stdout,p.stderr))
  assert len(qs)==8 and {q['name'] for q in qs}==set(want)
  for q in qs:
   expected='sat' if want[q['name']] else 'unsat'
   if q['solver_result']!=expected:raise AssertionError((name,t,q['name'],expected,q))
   assert q['backend'] in ['finite_bv','structural_kernel'];counts[expected]+=1
   if expected=='sat':assert q.get('finite',{}).get('original_formula_validated') is True
  if yes:
   evidence=json.loads((tmp/'proof/binding_nonempty.finite.json').read_text());e={A:(0,0),B:(0,0),C:(0,0),X:0,Y:0}
   for k,v in evidence['assignments'].items():
    if k.startswith('spec_'):e[k.replace('spec_','spec.',1)]=v['value']
   arr=evidence.get('readonly_arrays',evidence.get('arrays',{})).get('array_assignments',{})
   for k,v in arr.items():
    if k.startswith('spec_'):e[k.replace('spec_','spec.',1)]=tuple(v['entries'].get(str(i),v['default']) for i in range(2))
   assert ev(t,e),(name,'independent witness replay failed',e,evidence,t);replays+=1
  results.append({'name':name,'formula':t,'has_true':yes,'has_false':no})
  if (number+1)%100==0:print(json.dumps({'completed':number+1,'seconds':time.monotonic()-start}),flush=True)
 assert not (tmp/'external-used').exists()
assert hashlib.sha256(BIN.read_bytes()).hexdigest()==binary_hash,'binary changed during audit'
summary={'seed':620104,'formulas':len(cases),'obligations':8*len(cases),'exhaustive_assignments_per_formula':256,'independent_sat_replays':replays,'counts':counts,'checker_sha256':binary_hash,'seconds':time.monotonic()-start,'cases':results}
(a.out/'summary.json').write_text(json.dumps(summary));print(json.dumps({k:v for k,v in summary.items() if k!='cases'}))
