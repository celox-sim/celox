"""Concrete imported-source token witnesses; corroboration, not proof authority."""
from audit.interpreter import BV,evaluate,evaluate_record
from audit.veryl_scaling import rv32i_latency_common as common

def initial(machine):return {k:evaluate(e,{}) for k,e in machine['reset'].items()}
def val(x):return x.v if isinstance(x,BV) else x
def inputs():return dict(rst=False,rst_n=True,stall=False,imem_response=BV(32,0x13),imem_fault=False,dmem_response=BV(32,0),dmem_fault=False)
def enc_i(op,rd,rs,imm,f3=0):return ((imm&4095)<<20)|(rs<<15)|(f3<<12)|(rd<<7)|op
def enc_r(rd,a,b,f3=0,f7=0):return (f7<<25)|(b<<20)|(a<<15)|(f3<<12)|(rd<<7)|0x33
def enc_s(a,b,imm,f3=2):return (((imm&4095)>>5)<<25)|(b<<20)|(a<<15)|(f3<<12)|((imm&31)<<7)|0x23
def enc_b(a,b,off,f3=0):return (((off>>12)&1)<<31)|(((off>>5)&63)<<25)|(b<<20)|(a<<15)|(f3<<12)|(((off>>1)&15)<<8)|(((off>>11)&1)<<7)|0x63

def witness(machine,roots,program,variant,pick=0,stall_edges=()):
    control,_,build,_,_,_=common.builders(variant)
    four=variant.family=='fourstage'
    stages={1:'d',2:'x',3:'m',4:'w'} if four else {1:'d',2:'x',3:'w'}
    first_terminal=5 if four else 4
    dut=initial(machine);ghost=build()['implementation'];track=initial(ghost)
    seen=0;accepted=None;trace=[]
    for edge in range(25):
        inp=inputs();inp['stall']=edge in stall_edges
        pc=val(dut['fetch_pc']);inp['imem_response']=BV(32,program[(pc//4)%len(program)])
        inp['dmem_response']=BV(32,11)
        port=evaluate_record(machine,'outputs',dut,inp)
        controls=evaluate_record({**machine,'controls':roots},'controls',dut,inp)
        choose=bool(port['imem_valid'] and seen==pick)
        if port['imem_valid']:seen+=1
        if choose:accepted=(pc,inp['imem_response'])
        gin=dict(rst=False,stall=inp['stall'],h=controls['h'],f=controls['f'],choose=choose)
        if four:gin['t']=dut['m_taken']
        else:gin['r']=controls['r']
        after=evaluate_record(machine,'next',dut,inp)
        for selector,_,_ in variant.nonzero_resets:
            shift=15 if selector=='d_sel1' else 20
            assert val(after[selector])==1<<((val(after['d_ir'])>>shift)&31)
        gt=evaluate_record(ghost,'next',track,gin)
        for k in control:assert gt[k]==after[k],(k,gt[k],after[k])
        tag=val(gt['tag']);age=val(gt['age'])
        if tag in stages:
            stage=stages[tag]
            assert after[stage+'_valid'] and (val(after[stage+'_pc']),after[stage+'_ir'])==accepted
        trace.append({'edge':edge,'choose':choose,'stall':inp['stall'],'tag':tag,'age':age,
                      'h':controls['h'],'r':controls['r'],'f':controls['f'],'retire':port['retire'],'trap':port['trap_valid']})
        dut,track=after,gt
        if tag>=first_terminal:return {'outcome':tag,'enabled_edges':age,'trace':trace}
    raise AssertionError('witness failed to terminate')

def actual_witnesses(machine,roots,variant):
    four=variant.family=='fourstage';normal=5 if four else 4;trap=normal+1;squash=normal+2
    nominal=4 if four else 3;worst=6 if four else 4;kill_age=3 if four else 1
    nop=0x13;load=enc_i(3,1,0,0,2)
    cases={
      'nominal':([nop],0,(),normal,nominal),
      'own-illegal-trap':([0xffffffff,nop],0,(),trap,nominal),
      'older-branch-squash':([enc_b(0,0,8),nop,nop],1,(),squash,kill_age),
      'older-jump-squash':([0x0080006f,nop,nop],1,(),squash,kill_age),
      'older-fault-squash':([0xffffffff,nop],1,(),squash,kill_age),
      'load-rs1-hazard':([load,enc_i(0x13,2,1,0),nop],1,(),normal,worst),
      'load-rs2-hazard':([load,enc_r(2,0,1),nop],1,(),normal,worst),
      'load-storedata-hazard':([load,enc_s(0,1,0),nop],1,(),normal,worst),
      'stalled-own-Wtrap':([0xffffffff,nop],0,(4,5,6) if four else (3,4,5),trap,nominal),
      'repeated-PC-loop':([0x0000006f,nop],4 if four else 2,(),normal,nominal)}
    if four:cases['M-nonmemory-bypass']=([enc_i(0x13,1,0,1),enc_i(0x13,2,1,0),nop],1,(),normal,5)
    results={}
    for name,(program,pick,stalls,outcome,age) in cases.items():
        result=witness(machine,roots,program,variant,pick,stalls)
        assert (result['outcome'],result['enabled_edges'])==(outcome,age),(name,result)
        results[name]=result
    return results
