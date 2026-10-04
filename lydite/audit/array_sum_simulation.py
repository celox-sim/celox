"""Independent integer ISA oracle and finite concrete regression, not the proof."""
import json, random
from pathlib import Path
from interpreter import BV,Mem,evaluate,outputs,related
from cpu_simulation import cycle, reset, isa as legacy_isa
ROOT=Path(__file__).resolve().parents[1]

def oracle(a):
    op=a['program'].read(a['pc']).v>>13
    imm=a['program'].read(a['pc']).v&255
    if op<6: return legacy_isa(a)[0]
    n=dict(a);n['pc']=BV(8,a['pc'].v+1)
    if op==6: n['acc']=BV(8,a['acc'].v+a['mem'].read(a['idx']).v)
    else:
        n['idx']=BV(8,a['idx'].v+1)
        if n['idx'].v!=0:n['pc']=BV(8,imm)
    return n

def trial(seed,stall):
    rng=random.Random(seed)
    data=Mem(8,8,0,{k:rng.randrange(256) for k in range(256)})
    prog=Mem(8,16,4<<13,{0:6<<13,1:7<<13,2:4<<13})
    d=json.loads((ROOT/'examples/array_sum.json').read_text())
    i={'rst':True,'stall':False,'program':prog,'data':data,'start_index':BV(8,240)}
    a=reset(d['spec'],i);s=reset(d['impl'],i);cycles=0;commits=0
    while not a['halted']:
        # Mutate all live initialization inputs: only reset-time values matter.
        i={'rst':False,'stall':stall and rng.randrange(3)==0,'program':Mem(8,16,0,{}),
           'data':Mem(8,8,1,{}),'start_index':BV(8,rng.randrange(256))}
        assert related(d,a,s)
        env={**{'s.'+n:v for n,v in a.items()},'p.data':data}
        assert evaluate(d['program_contract']['invariant'],env)
        oldrank=evaluate(d['program_contract']['rank'],env).v
        c=outputs(d['impl'],s,i)['commit'];new=cycle(d['impl'],s,i)
        if c:
            a=oracle(a);commits+=1
            newenv={**{'s.'+n:v for n,v in a.items()},'p.data':data}
            assert evaluate(d['program_contract']['rank'],newenv).v<oldrank
        if i['stall']: assert s==new
        s=new;cycles+=1
        assert related(d,a,s)
        assert cycles<1000
    assert commits==33
    assert a['acc'].v==sum(data.read(BV(8,k)).v for k in range(240,256))%256
    assert a['mem']==data
    # Once terminal, additional cycles cannot commit or change architectural state.
    for _ in range(5):
        assert not outputs(d['impl'],s,i)['commit']
        s=cycle(d['impl'],s,i);assert related(d,a,s)
    if not stall: assert cycles==66
    return {'seed':seed,'stalls':stall,'cycles':cycles,'commits':commits,'sum':a['acc'].v}

if __name__=='__main__':
    rows=[trial(seed,stall) for seed in range(20) for stall in [False,True]]
    (ROOT/'audit/array_sum_simulation_results.json').write_text(json.dumps(rows,indent=2)+'\n')
    print({'trials':len(rows),'cycles':sum(r['cycles'] for r in rows),'commits':sum(r['commits'] for r in rows)})
