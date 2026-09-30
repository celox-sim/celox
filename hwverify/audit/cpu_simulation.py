"""Independent integer ISA oracle vs declarative microarchitecture; no Z3."""
from pathlib import Path
import json,random,copy
from interpreter import BV,Mem,evaluate,evaluate_record,outputs,related
ROOT=Path(__file__).resolve().parents[1]

def reset(m,i):return {k:evaluate(e,{'i.'+n:v for n,v in i.items()}) for k,e in m['reset'].items()}
def cycle(m,s,i):return reset(m,i) if i['rst'] else evaluate_record(m,'next',s,i)
def instr(op,imm=0,pad=0):return (op<<13)|((pad&31)<<8)|(imm&255)
def program(words):return Mem(8,16,instr(6),dict(enumerate(words)))
def isa(a):
 assert not a['halted'],'commit after halt'
 ir=a['program'].read(a['pc']).v;op=ir>>13;imm=ir&255
 n=dict(a);n['pc']=BV(8,a['pc'].v+1)
 if op==0:n['acc']=BV(8,a['acc'].v+imm)
 elif op==1:n['acc']=a['mem'].read(BV(8,imm))
 elif op==2:n['mem']=a['mem'].write(BV(8,imm),a['acc'])
 elif op==3 and a['acc'].v==0:n['pc']=BV(8,imm)
 elif op==4:n['halted']=True
 elif op==5:n['acc']=BV(8,a['acc'].v^imm)
 return n,op

def trial(doc,prog,ticks,seed,reset_at=()):
 rng=random.Random(seed);i={'rst':True,'stall':False,'program':prog}
 s=reset(doc['impl'],i);a={'pc':BV(8,0),'acc':BV(8,0),'mem':Mem(8,8,0,{}),'program':prog,'halted':False}
 commits=0;ops=set();resets=0
 for k in range(ticks):
  # Changing live program input must not change the immutable loaded state.
  other=Mem(8,16,instr(rng.randrange(8),rng.randrange(256)),{})
  i={'rst':k in reset_at,'stall':rng.randrange(4)==0,'program':prog if k in reset_at else other}
  assert related(doc,a,s)
  commit=outputs(doc['impl'],s,i)['commit']
  old=s;new=cycle(doc['impl'],s,i)
  env={**{'spec.'+n:v for n,v in a.items()},**{'impl.'+n:v for n,v in s.items()},**{'i.'+n:v for n,v in i.items()}}
  enabled=evaluate(doc['progress']['enabled'],env)
  oldrank=evaluate(doc['progress']['rank'],env)
  if i['rst']:
   a={'pc':BV(8,0),'acc':BV(8,0),'mem':Mem(8,8,0,{}),'program':prog,'halted':False};resets+=1
  elif commit:
   assert not i['stall'];a,op=isa(a);ops.add(op);commits+=1
  else:
   if i['stall']:assert old==new,'stall changed state'
  newenv={**{'spec.'+n:v for n,v in a.items()},**{'impl.'+n:v for n,v in new.items()},**{'i.'+n:v for n,v in i.items()}}
  if enabled and not i['rst'] and not commit:assert evaluate(doc['progress']['rank'],newenv).v<oldrank.v
  assert related(doc,a,new);s=new
 return {'ticks':ticks,'commits':commits,'resets':resets,'opcodes':sorted(ops),'pc':a['pc'].v,'acc':a['acc'].v,'halted':a['halted']}

if __name__=='__main__':
 d=json.loads((ROOT/'examples/cpu.json').read_text());rows=[]
 words=[instr(0,5,31),instr(2,128),instr(5,5),instr(3,6),instr(0,99),instr(4),instr(1,128),instr(0,251),instr(3,10),instr(7),instr(4)]
 r=trial(d,program(words),100,1);assert r['halted'] and r['pc']==11 and r['acc']==0 and r['commits']==8;rows.append({'case':'directed',**r})
 rows.append({'case':'branch_self_loop',**trial(d,program([instr(3,0)]),200,3)})
 r=trial(d,program([]),1000,4);assert r['pc']==r['commits']%256;rows.append({'case':'pc_wrap',**r})
 for seed in range(20):
  rng=random.Random(seed);p=program([instr(rng.randrange(8),rng.randrange(256),rng.randrange(32)) for _ in range(256)])
  rows.append({'case':f'random_{seed}',**trial(d,p,300,seed,(17,73,151,227))})
 assert set.union(*(set(r['opcodes']) for r in rows))==set(range(8))
 (ROOT/'audit/simulation_results.json').write_text(json.dumps(rows,indent=2)+'\n')
 print({'trials':len(rows),'cycles':sum(r['ticks'] for r in rows),'commits':sum(r['commits'] for r in rows),'all_opcodes':True})
