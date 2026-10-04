"""Independent finite enumeration of relational products; no Z3 in the oracle."""
from pathlib import Path
import argparse,functools,itertools,json,subprocess,sys,time
HERE=Path(__file__).resolve().parent;ROOT=HERE.parents[1];sys.path.insert(0,str(HERE.parent))
from interpreter import BV,evaluate
W=lambda v:['bv',2,v]
eq=lambda a,b:['eq',a,b]

def fixture():
 def comp(state,init,inv,inc,hold):return {'state':state,'init':init,'invariant':inv,'steps':{'inc':inc,'hold':hold},'examples':{}}
 counter=comp({'x':{'bv':2}},eq('s.x',W(0)),eq('o.value','s.x'),eq('n.x',['ite','i.enable',['add','s.x',W(1)],'s.x']),eq('n.x','s.x'))
 limiter=comp({'x':'bool'},['not','s.x'],['ule','o.value',W(2)],eq('n.x',['not','s.x']),eq('n.x','s.x'))
 zero=comp({'x':'bool'},['not','s.x'],True,eq('n.x','s.x'),eq('n.x','s.x'))
 one=comp({'x':'bool'},'s.x',True,eq('n.x','s.x'),eq('n.x','s.x'))
 obs0=comp({},True,eq('o.value',W(0)),True,True);obs1=comp({},True,eq('o.value',W(1)),True,True)
 return {'version':3,'kind':'specification','name':'independent tiny relational products','inputs':{'enable':'bool'},'observations':{'value':{'bv':2}},'operations':{'inc':{},'hold':{}},'components':{'Counter':counter,'Limit':limiter,'PrivateZero':zero,'PrivateOne':one,'ObsZero':obs0,'ObsOne':obs1},'compositions':{'Bounded':{'members':['Counter','Limit'],'examples':{}},'PrivatePair':{'members':['PrivateZero','PrivateOne'],'examples':{}},'Nested':{'members':['Bounded','PrivatePair'],'examples':{}},'Impossible':{'members':['ObsZero','ObsOne'],'examples':{}}}}

def members(doc,name):
 if name in doc['components']:return [name]
 return sorted(x for n in doc['compositions'][name]['members'] for x in members(doc,n))

def domain(t):
 if t=='bool':return [False,True]
 if 'bv' in t:return [BV(t['bv'],x) for x in range(1<<t['bv'])]
 raise ValueError('oracle finite subset excludes arrays')
class Oracle:
 def __init__(self,doc,names):
  self.doc=doc;self.names=tuple(names);self.fields=[(c,n,t) for c in names for n,t in sorted(doc['components'][c]['state'].items())]
  self.all_states=list(itertools.product(domain({'bv':2}),*(domain(t) for c,n,t in self.fields)))
  self.valid=[s for s in self.all_states if all(evaluate(doc['components'][c]['invariant'],self.env(c,s)) for c in names)]
  self.initial=[s for s in self.valid if all(evaluate(doc['components'][c]['init'],self.env(c,s)) for c in names)]
  self.transitions=0
 def env(self,c,s):return {'o.value':s[0],**{'s.'+n:s[k+1] for k,(co,n,t) in enumerate(self.fields) if co==c}}
 @functools.lru_cache(None)
 def successors(self,s,op,enable):
  result=[]
  for nxt in self.valid:
   ok=True
   for c in self.names:
    env=self.env(c,s)|{'no.value':nxt[0],'i.enable':enable,**{'n.'+n:nxt[k+1] for k,(co,n,t) in enumerate(self.fields) if co==c}}
    self.transitions+=1
    if not evaluate(self.doc['components'][c]['steps'][op],env):ok=False;break
   if ok:result.append(nxt)
  return result
 def admitted(self,example):
  def fits(s,obs):return all(s[0]==evaluate(e,{}) for name,e in obs.items() if name=='value')
  current={s for s in self.initial if fits(s,example['initial'])}
  for step in example['trace']:
   enables=[evaluate(step['inputs']['enable'],{})] if 'enable' in step['inputs'] else [False,True]
   current={n for s in current for e in enables for n in self.successors(s,step['operation'],e) if fits(n,step['observe'])}
  return bool(current)

def cases():
 def obs(v):return {} if v is None else {'value':W(v)}
 def step(op,v,e=None):return {'operation':op,'inputs':{} if e is None else {'enable':e},'observe':obs(v)}
 base=[({},[]),({'value':W(0)},[]),({'value':W(1)},[])]
 for v in [None,0,1,2,3]:base.append(({'value':W(0)},[step('inc',v)]))
 base += [({'value':W(0)},[step('inc',1,False)]),({'value':W(0)},[step('inc',1,True)]),({'value':W(0)},[step('hold',3)]),({},[step('inc',2)]),({'value':W(0)},[step('inc',1),step('inc',2)]),({'value':W(0)},[step('inc',1),step('inc',2),step('inc',3)])]
 return [{'initial':i,'trace':t} for i,t in base]
if __name__=='__main__':
 p=argparse.ArgumentParser();p.add_argument('--binary',required=True,type=Path);p.add_argument('--z3',required=True);p.add_argument('--out',type=Path,default=ROOT/'results/composition_0_7_independent');a=p.parse_args();a.out.mkdir(parents=True,exist_ok=True)
 d=fixture();oracle_rows=[];expected={}
 for kind in ['components','compositions']:
  for name,target in d[kind].items():
   oracle=Oracle(d,members(d,name));exs={}
   for index,ex in enumerate(cases()):
    admitted=oracle.admitted(ex);case=f'case_{index:02d}';expected[(name,case)]=admitted;exs[case]={'expect':'positive' if admitted else 'negative',**ex}
   target['examples']=exs;oracle_rows.append({'target':name,'members':members(d,name),'enumerated_product_states':len(oracle.all_states),'invariant_states':len(oracle.valid),'initial_states':len(oracle.initial),'transition_predicates_evaluated':oracle.transitions})
 source=a.out/'oracle_specification.json';source.write_text(json.dumps(d,indent=2)+'\n');start=time.monotonic()
 proc=subprocess.run([str(a.binary.resolve()),str(source),'--z3',a.z3,'--out',str(a.out/'run')],capture_output=True,text=True,timeout=120)
 (a.out/'run.stdout').write_text(proc.stdout);(a.out/'run.stderr').write_text(proc.stderr)
 report=json.loads((a.out/'run/report.json').read_text());assert report['status']=='spec_examples_passed',(proc.returncode,report)
 assert len(report['examples'])==len(expected)
 for ex in report['examples']:assert ex['admitted']==expected[(ex['target'],ex['example'])],ex
 result={'status':'pass','case_count':len(expected),'positive':sum(expected.values()),'negative':len(expected)-sum(expected.values()),'oracle':oracle_rows,'solver_wall_seconds':time.monotonic()-start}
 (HERE/'finite_results.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result),flush=True)
