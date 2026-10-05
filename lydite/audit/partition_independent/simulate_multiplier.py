"""Exhaust all 8-bit factors and 4-bit loop counts, using integer oracle."""
from pathlib import Path
import json,sys,time
HERE=Path(__file__).resolve().parent
sys.path.insert(0,str(HERE.parent))
from interpreter import BV,evaluate_record,outputs,related

def trial(d,factor,count,stalls):
    i={'reset':True,'wait':False,'factor':BV(8,factor),'iterations':BV(4,count)}
    s=evaluate_record(d['spec'],'reset',{},i);t=evaluate_record(d['impl'],'reset',{},i)
    n=count;acc=0;cycles=0;commits=0
    while n:
        i={'reset':False,'wait':stalls and cycles%3==0,'factor':BV(8,factor+1),'iterations':BV(4,count+1)}
        assert related(d,s,t)
        out=outputs(d['impl'],t,i);nt=evaluate_record(d['impl'],'next',t,i)
        if out['retired']:
            n-=1;acc=(acc+factor)%256;commits+=1
            s={'remaining':BV(4,n),'accumulator':BV(8,acc),'factor':BV(8,factor)}
        if i['wait']:assert nt==t
        t=nt;cycles+=1
        assert related(d,s,t) and cycles<=60
    assert acc==(factor*count)%256 and commits==count
    if not stalls:assert cycles==2*count
    i={'reset':False,'wait':False,'factor':BV(8,0),'iterations':BV(4,0)}
    for _ in range(2):
        assert not outputs(d['impl'],t,i)['retired']
        assert evaluate_record(d['impl'],'next',t,i)==t
    return cycles,commits

if __name__=='__main__':
    d=json.loads((HERE/'multiplier_good.json').read_text());cycles=commits=cases=0;start=time.monotonic()
    for factor in range(256):
        for count in range(16):
            for stalls in [False,True]:
                c,k=trial(d,factor,count,stalls);cycles+=c;commits+=k;cases+=1
    result={'status':'pass','cases':cases,'cycles':cycles,'commits':commits,'seconds':time.monotonic()-start,
            'scope':'All256 factors ×16 counts ×2 stall schedules; independent integer oracle, no Z3. Test, not proof.'}
    (HERE/'simulation_results.json').write_text(json.dumps(result,indent=2)+'\n');print(result)
