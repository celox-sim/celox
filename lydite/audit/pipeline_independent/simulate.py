"""Independent integer-ISA retirement oracle for the D/X/W JSON model."""
import argparse,json,random,sys
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2];sys.path.insert(0,str(ROOT/'audit'))
from interpreter import BV,evaluate,evaluate_record,outputs,related

def isa(ir,regs,data):
 op=ir>>6;rd=(ir>>5)&1;rs=(ir>>4)&1;imm=ir&15
 v=imm if op==0 else ((regs[rs]+imm)&15 if op==1 else (regs[rs]^imm if op==2 else data[imm&3]))
 out=list(regs);out[rd]=v;return out

def cases():
 # Every instruction encoding, with a preceding write and a following dependent read.
 for ir in range(256):
  rd=(ir>>5)&1
  yield [3,ir,64|(1-rd)<<5|rd<<4|7,128|rd<<5|(1-rd)<<4|15],[(ir+i*5)&15 for i in range(4)],False
 # Same-register older and younger producers: X must beat W.
 yield [1,64|2,128|32|4,192|3],[6,9,12,15],False
 # Load-use interlock, long external stalls, and reset in a full pipeline.
 yield [192|2,64|32|1,128|16|3,192|32|1],[0,4,8,12],True
 rng=random.Random(9173)
 for _ in range(128):yield [rng.randrange(256) for _ in range(4)],[rng.randrange(16) for _ in range(4)],rng.randrange(4)==0

def run(doc,rom,data,midreset=False,limit=32,check_binding=True):
 machine=doc['impl'];s=None;arch=None;regs=[0,0];pc=0;history=[];cov={'cycles':0,'retirements':0,'full_pipeline':0,'load_use':0,'double_match':0,'stalls':0,'resets':0}
 for cycle in range(limit):
  reset=cycle==0 or (midreset and cycle==17);stall=cycle%11 in (4,5,6)
  # After reset, changing external ROM/data inputs must not alter captured memory.
  inp={'rst':reset,'stall':stall,**{f'rom{i}':BV(8,v if reset else v^255) for i,v in enumerate(rom)},**{f'data{i}':BV(4,v if reset else v^15) for i,v in enumerate(data)}}
  if reset:
   s=evaluate_record(machine,'reset',{},inp);arch=evaluate_record(doc['spec'],'reset',{},inp);regs=[0,0];pc=0;cov['resets']+=1
   assert related(doc,arch,s)
   continue
  commit=outputs(machine,s,inp)['commit'];old=s
  d,x,w=(s[k+'_valid'] for k in ['d','x','w']);di,xi,wi=(s[k+'_ir'].v for k in ['d','x','w'])
  cov['full_pipeline']+=int(d and x and w);cov['stalls']+=int(stall)
  hazard=d and x and di>>6 in (1,2) and xi>>6==3 and ((di>>4)&1)==((xi>>5)&1)
  both=d and x and w and di>>6 in (1,2) and ((di>>4)&1)==((xi>>5)&1)==((wi>>5)&1)
  cov['load_use']+=int(hazard and not stall);cov['double_match']+=int(both and not stall)
  assert commit==(w and not stall)
  if commit:
   assert s['w_pc'].v==pc and wi==rom[pc],('retire-order',cycle)
   regs=isa(rom[pc],regs,data);pc=(pc+1)&3;cov['retirements']+=1
   arch=evaluate_record(doc['spec'],'next',arch,inp)
  s=evaluate_record(machine,'next',old,inp)
  event={'cycle':cycle,'stall':stall,'commit':commit,'pc':pc,'expected_regs':regs[:], 'actual_regs':[s['r0'].v,s['r1'].v]};history.append(event)
  assert [s['r0'].v,s['r1'].v]==regs and s['pc'].v==pc,('architectural-result',event)
  assert [arch['r0'].v,arch['r1'].v]==regs and arch['pc'].v==pc,('spec-vs-independent-ISA',event)
  if stall:assert s==old,('external-stall',event)
  if check_binding:assert related(doc,arch,s),('binding',event)
  # Check the actual overlapping instruction age chain independently.
  age=pc
  for st in ['w','x','d']:
   if s[st+'_valid']:
    assert s[st+'_pc'].v==age and s[st+'_ir'].v==rom[age],('stage-age',st,event)
    age=(age+1)&3
  assert s['fetch_pc'].v==age,('fetch-age',event)
  cov['cycles']+=1
 return cov

def main():
 p=argparse.ArgumentParser();p.add_argument('--out',type=Path,default=ROOT/'audit/pipeline_independent/simulation_results.json');a=p.parse_args()
 good=json.loads((ROOT/'examples/pipeline.json').read_text());corpus=list(cases());total={};passed=0
 for rom,data,reset in corpus:
  c=run(good,rom,data,reset);passed+=1
  for k,v in c.items():total[k]=total.get(k,0)+v
 assert all(total[k]>0 for k in ['full_pipeline','load_use','double_match','stalls'])
 mutants=[]
 for path in sorted((ROOT/'examples').glob('pipeline_bad_*.json')):
  doc=json.loads(path.read_text());caught=None
  for rom,data,reset in corpus:
   try:run(doc,rom,data,reset,check_binding=False)
   except AssertionError as e:caught={'rom':rom,'data':data,'midreset':reset,'failure':str(e)};break
  assert caught,(path,'mutant survived corpus')
  mutants.append({'case':path.name,'counterexample':caught})
 result={'status':'pass','trials':passed,'coverage':total,'mutants':mutants};a.out.write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result),flush=True)
if __name__=='__main__':main()
