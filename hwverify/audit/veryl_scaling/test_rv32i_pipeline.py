"""Independent concrete audit of an actual imported RV32I machine.

Run explicitly (a missing requested artifact is an error):
  python -m audit.veryl_scaling.test_rv32i_pipeline --machine /path/machine.json
Add --memory-machine /path/memory.json for actual 4/16/64-word EEI composition.
Or set RV32I_MACHINE and RV32I_MEMORY_MACHINE for unittest discovery.
Without the corresponding import, discovery explicitly skips that coverage.

The integer ISA is handwritten separately from both RTL and symbolic spec.
This is finite differential coverage, not universal refinement or progress proof.
The dependency suite supplies independent Harvard instruction/data responses,
which is an arbitrary CPU-port environment, not the final disjoint ROM/RAM EEI.
No runtime Z3, source edits, or frontend construction occurs in this test.
"""
import argparse
import json
import os
from pathlib import Path
import random
import unittest

from audit.interpreter import BV, evaluate, evaluate_record
from audit.veryl_scaling.rv32i_isa import execute

def inputs():return dict(rst=False,rst_n=True,stall=False,imem_response=BV(32,0x13),imem_fault=False,dmem_response=BV(32,0),dmem_fault=False)
def initial(m):return {k:evaluate(e,{}) for k,e in m['reset'].items()}
def val(x):return x.v if isinstance(x,BV) else x

def alu_audit(m,count=3000):
 rng=random.Random(923170); n=0; covered=set()
 # Enumerate every primary opcode/funct3/funct7 plus legal immediate/branch/memory forms.
 cases=[]
 for op in [0x03,0x0f,0x13,0x17,0x23,0x33,0x37,0x63,0x67,0x6f,0x73,0x00,0x7f]:
  for f3 in range(8):
   for f7 in [0,1,0x20,0x7f]:cases.append((f7<<25)|(2<<20)|(1<<15)|(f3<<12)|(3<<7)|op)
 cases += [0x73,0x100073]+[rng.getrandbits(32) for _ in range(count)]
 for ir in cases:
  s=initial(m);i=inputs(); regs=[rng.getrandbits(32) for _ in range(32)];regs[0]=0
  pc=rng.getrandbits(30)*4; rs1=(ir>>15)&31;rs2=(ir>>20)&31
  s.update(x_valid=True,x_pc=BV(32,pc),x_ir=BV(32,ir),x_operand1=BV(32,regs[rs1]),x_operand2=BV(32,regs[rs2]))
  for r,v in enumerate(regs):s['r'+str(r)]=BV(32,v)
  i['dmem_response']=BV(32,rng.getrandbits(32));i['imem_fault']=bool(rng.randrange(5)==0);i['dmem_fault']=bool(rng.randrange(5)==0)
  s['x_fetch_fault']=bool(rng.randrange(20)==0) if ir not in (0x73,0x100073) else False
  expected=execute(ir,pc,regs,i['dmem_response'].v,s['x_fetch_fault'],i['dmem_fault'])
  covered.add(expected.mnemonic)
  out=evaluate_record(m,'next',s,i)
  got={k:val(out[k]) for k in ['w_fault','w_cause','w_tval','w_result','w_writes','w_next_pc','w_store','w_address','w_data','w_mask']}
  assert got['w_fault']==expected.trap,(hex(ir),got,expected)
  if expected.trap:assert (got['w_cause'],got['w_tval'],got['w_writes'],got['w_store'])==(expected.cause,expected.trap_value,False,False),(hex(ir),got,expected)
  else:
   assert got['w_next_pc']==expected.next_pc,(hex(ir),got,expected)
   if expected.rd:assert got['w_result']==expected.value,(hex(ir),got,expected)
   assert got['w_writes']==bool(expected.rd),(hex(ir),got,expected)
   assert got['w_store']==bool(expected.store_mask),(hex(ir),got,expected)
   if expected.store_mask:assert (got['w_address'],got['w_data'],got['w_mask'])==(expected.store_address,expected.store_data,expected.store_mask),(hex(ir),got,expected)
  n+=1
 from audit.veryl_scaling.rv32i_isa import INSTRUCTIONS
 assert set(INSTRUCTIONS)<=covered, set(INSTRUCTIONS)-covered
 return n

def enc_i(op,rd,rs,imm,f3=0):return ((imm&4095)<<20)|(rs<<15)|(f3<<12)|(rd<<7)|op
def enc_r(rd,a,b,f3=0,f7=0):return (f7<<25)|(b<<20)|(a<<15)|(f3<<12)|(rd<<7)|0x33
def enc_s(a,b,imm,f3=2):return (((imm&4095)>>5)<<25)|(b<<20)|(a<<15)|(f3<<12)|((imm&31)<<7)|0x23
def enc_b(a,b,off,f3=0):return (((off>>12)&1)<<31)|(((off>>5)&63)<<25)|(b<<20)|(a<<15)|(f3<<12)|(((off>>1)&15)<<8)|(((off>>11)&1)<<7)|0x63

def program_audit(m,prog,seeds=None,maxcycles=2000,seed=121):
 rng=random.Random(seed); rom=prog if isinstance(prog,dict) else dict(enumerate(prog));rom={k*4:v for k,v in rom.items()}
 mem={k*4:v for k,v in enumerate(seeds or [0x807f80ff^i for i in range(64)])}
 regs=[0]*32;pc=0;s=initial(m);retired=0
 for cycle in range(maxcycles):
  i=inputs();i['stall']=rng.randrange(5)==0
  pre=evaluate_record(m,'outputs',s,i)
  if i['stall']:
   assert all(not pre[k] for k in ('retire','commit','trap_valid','write_enable','imem_valid','dmem_valid'))
   assert evaluate_record(m,'next',s,i)==s
  if pre['retire']:
   ir=rom.get(pc,0)
   probe=execute(ir,pc,regs,imem_fault=pc not in rom)
   addr=probe.memory_address
   # Fault address retained by oracle only when nonfaulting probe; use arithmetic to locate load/store.
   if ir&127 in (3,35):
    off=(ir>>20) if ir&127==3 else ((ir>>25)<<5)|((ir>>7)&31)
    off=off-4096 if off&2048 else off
    addr=(regs[(ir>>15)&31]+off)&0xffffffff
   expected=execute(ir,pc,regs,mem.get(addr&~3,0),pc not in rom,(addr&~3) not in mem)
   assert val(s['w_pc'])==pc,(cycle,'retirement pc',val(s['w_pc']),pc)
   assert bool(pre['trap_valid'])==expected.trap,(cycle,hex(ir),expected,{k:val(v) for k,v in pre.items() if k in ('trap_valid','trap_cause','trap_pc','trap_value')})
   if expected.trap:
    assert tuple(val(pre[k]) for k in ('trap_cause','trap_pc','trap_value'))==(expected.cause,pc,expected.trap_value)
    assert not pre['write_enable']
    after=evaluate_record(m,'next',s,i)
    assert after['halted']
    assert all(val(after['r'+str(r)])==v for r,v in enumerate(regs))
    for _ in range(3):
     obs=evaluate_record(m,'outputs',after,inputs());assert not obs['retire'] and not obs['write_enable'] and not obs['imem_valid'] and not obs['dmem_valid']
     assert evaluate_record(m,'next',after,inputs())==after
    return retired+1
   assert bool(pre['write_enable'])==bool(expected.store_mask),(cycle,'store enable',expected)
   if expected.store_mask:
    assert tuple(val(pre[k]) for k in ('write_address','write_data','write_mask'))==(expected.store_address,expected.store_data,expected.store_mask)
   if expected.rd:regs[expected.rd]=expected.value
   pc=expected.next_pc;retired+=1
  if pre['write_enable']:
   addr=val(pre['write_address'])&~3;word=mem[addr];data=val(pre['write_data']);mask=val(pre['write_mask'])
   for lane in range(4):
    if mask>>lane&1:word=(word&~(255<<(8*lane)))|(data&(255<<(8*lane)))
   mem[addr]=word
  fa=val(pre['imem_address']);da=val(pre['dmem_address'])&~3
  i.update(imem_response=BV(32,rom.get(fa,0)),imem_fault=fa not in rom,dmem_response=BV(32,mem.get(da,0)),dmem_fault=da not in mem)
  s=evaluate_record(m,'next',s,i)
  for r,v in enumerate(regs):assert val(s['r'+str(r)])==v,(cycle,'register',r,val(s['r'+str(r)]),v)
 raise AssertionError(('no terminal trap',retired,pc))

def programs_audit(m):
 tests=[]
 # Two-source forwarding, load-use on store data and address, all byte lanes.
 tests.append([enc_i(0x13,1,0,17),enc_r(2,1,1),enc_s(0,2,1,0),enc_i(3,3,0,1,4),enc_r(4,3,3),enc_s(0,4,2,1),enc_i(3,5,0,2,1),enc_s(0,5,4,2),0x73])
 # Wrong-path stores and faulting loads squashed by taken branch.
 tests.append([enc_i(0x13,1,0,1),enc_b(1,1,12),enc_s(0,1,0),enc_i(3,2,0,-4,2),enc_i(0x13,3,0,7),0x73])
 # Untaken branch to misaligned target stays legal; taken target traps.
 tests.append([enc_i(0x13,1,0,1),enc_b(0,1,2),enc_i(0x13,2,0,8),enc_b(1,1,2),enc_s(0,2,0)])
 # JALR clears bit0; aliases rd/rs1 preserve old source; target bit1 traps.
 tests.append([enc_i(0x13,1,0,13),enc_i(0x67,1,1,0),enc_s(0,0,0),enc_i(0x13,2,1,1),0x73])
 tests.append([enc_i(0x13,1,0,3),enc_i(0x67,1,1,0),enc_s(0,1,0)])
 # Faulting load to x0, high address must not alias low RAM.
 tests.append([0x800000b7,enc_i(3,0,1,0,2),enc_s(0,1,0)])
 # Older write preserved before illegal and younger store killed.
 tests.append([enc_i(0x13,1,0,127),enc_s(0,1,0),0xffffffff,enc_s(0,0,0)])
 # Full register bank and x0 write suppression through actual pipeline.
 p=[enc_i(0x13,r,0,r*3) for r in range(32)]
 p += [enc_r(r,r,31,0) for r in range(32)] + [0x73]
 tests.append(p)
 # Upper instruction address bits, AUIPC, link, and PC wraparound.
 tests.append({0:0x800000b7,1:enc_i(0x67,31,1,0),0x20000000:0x00000117,0x20000001:0x73})
 tests.append({0:enc_i(0x13,1,0,-4),1:enc_i(0x67,0,1,0),0x3fffffff:0x73})
 # Repeated short random dependency-rich legal streams.
 rng=random.Random(871)
 for _ in range(30):
  p=[]
  for j in range(80):
   rd,a,b=[rng.randrange(8) for _ in range(3)];kind=rng.randrange(4)
   if kind==0:
    f3=rng.choice([0,2,3,4,6,7]);p.append(enc_i(0x13,rd,a,rng.randrange(-2048,2048),f3))
   elif kind==1:
    f3=rng.randrange(8);f7=0x20 if f3 in (0,5) and rng.randrange(2) else 0;p.append(enc_r(rd,a,b,f3,f7))
   elif kind==2:
    f3=rng.choice([0,1,2,4,5]);size=1<<(f3&3);p.append(enc_i(3,rd,0,rng.randrange(32)*size,f3))
   else:
    f3=rng.randrange(3);p.append(enc_s(0,b,rng.randrange(32)*(1<<f3),f3))
  p.append(0x73);tests.append(p)
 return {'programs':len(tests),'retirements':sum(program_audit(m,p,seed=i) for i,p in enumerate(tests))}


def composed_program_audit(cpu, memory, program, seed=1):
    """Actual imported CPU + actual imported memory, independently checked EEI."""
    count = len(memory['state']) // 2
    assert len(program) <= count
    rom = {4*j: program[j] if j < len(program) else 0 for j in range(count)}
    ram = {0x1000+4*j: (0x807f80ff ^ j) for j in range(count)}
    seed_inputs = {'i.seed_rom'+str(j): BV(32, rom[4*j]) for j in range(count)}
    seed_inputs.update({'i.seed_data'+str(j): BV(32, ram[0x1000+4*j]) for j in range(count)})
    ms = {k: evaluate(e, seed_inputs) for k,e in memory['reset'].items()}
    s = initial(cpu); regs = [0]*32; pc = 0; retired = 0
    rng = random.Random(seed)
    request_names = ('imem_address','imem_valid','dmem_address','dmem_valid','dmem_write',
                     'write_enable','write_address','write_data','write_mask')
    for cycle in range(1000):
        ins = inputs(); ins['stall'] = rng.randrange(4) == 0
        preliminary = evaluate_record(cpu,'outputs',s,ins)
        mi = {'rst':False, **{k:preliminary[k] for k in request_names},
              **{k[2:]:v for k,v in seed_inputs.items()}}
        actual_mem = evaluate_record(memory,'outputs',ms,mi)
        ins.update(actual_mem)
        out = evaluate_record(cpu,'outputs',s,ins)
        assert all(out[k] == preliminary[k] for k in request_names if k not in ('imem_valid','dmem_valid')), 'address/write request feedback'
        mi.update({k:out[k] for k in request_names})
        assert evaluate_record(memory,'outputs',ms,mi) == actual_mem, 'response depends on request-valid feedback'
        trapped = False
        if out['retire']:
            ir = rom.get(pc,0); opcode = ir & 127
            address = 0
            if opcode in (3,35):
                imm = ir>>20 if opcode==3 else ((ir>>25)<<5)|((ir>>7)&31)
                if imm&2048: imm-=4096
                address = (regs[(ir>>15)&31]+imm)&0xffffffff
            readable = {**rom,**ram}
            fault = (address&~3) not in (ram if opcode==35 else readable)
            expected = execute(ir,pc,regs,readable.get(address&~3,0),pc not in rom,fault)
            assert val(s['w_pc']) == pc
            assert bool(out['trap_valid']) == expected.trap
            if expected.trap:
                assert tuple(val(out[k]) for k in ('trap_cause','trap_pc','trap_value')) == (expected.cause,pc,expected.trap_value)
                assert not out['write_enable']; trapped = True
            else:
                assert bool(out['write_enable']) == bool(expected.store_mask)
                if expected.store_mask:
                    assert tuple(val(out[k]) for k in ('write_address','write_data','write_mask')) == (expected.store_address,expected.store_data,expected.store_mask)
                if expected.rd: regs[expected.rd] = expected.value
                pc = expected.next_pc
            retired += 1
        # Independent byte replacement at W, preceding this edge's X word read.
        if out['write_enable']:
            addr=val(out['write_address'])&~3
            assert addr in ram
            word=ram[addr]; data=val(out['write_data']); mask=val(out['write_mask'])
            for lane in range(4):
                if mask>>lane&1: word=(word&~(255<<(8*lane)))|(data&(255<<(8*lane)))
            ram[addr]=word
        ia=val(out['imem_address']); da=val(out['dmem_address']); readable={**rom,**ram}
        assert val(actual_mem['imem_response']) == rom.get(ia&~3,0)
        assert actual_mem['imem_fault'] == (ia&~3 not in rom)
        assert val(actual_mem['dmem_response']) == readable.get(da&~3,0)
        assert actual_mem['dmem_fault'] == (da&~3 not in (ram if out['dmem_write'] else readable))
        ns=evaluate_record(cpu,'next',s,ins); nms=evaluate_record(memory,'next',ms,mi)
        if ins['stall']:
            assert ns==s
            assert all(not out[k] for k in ('retire','commit','trap_valid','write_enable','imem_valid','dmem_valid'))
        for r,v in enumerate(regs): assert val(ns['r'+str(r)])==v
        assert all(val(nms['rom'+str(j)])==rom[4*j] for j in range(count))
        assert all(val(nms['data'+str(j)])==ram[0x1000+4*j] for j in range(count))
        s,ms=ns,nms
        if trapped:
            assert s['halted']
            for _ in range(4):
                o=evaluate_record(cpu,'outputs',s,inputs())
                assert all(not o[k] for k in ('retire','commit','trap_valid','write_enable','imem_valid','dmem_valid'))
                assert evaluate_record(cpu,'next',s,inputs())==s
            return retired
    raise AssertionError('no terminal trap in composed program')


def composed_audit(cpu,memory):
    count = len(memory['state']) // 2
    assert count in (4,16,64), 'unsupported memory fixture capacity'
    ram_base=0x00001f37  # lui x30,1
    programs=[
        [ram_base,enc_i(0x13,1,0,-1),enc_s(30,1,1,0),enc_i(3,2,30,0,2),enc_s(30,2,2,1),enc_i(3,3,30,2,5),0x73],
        [enc_i(3,1,0,0,2),0x73],  # ROM is also data-readable
        [enc_i(0x13,1,0,17),enc_s(0,1,0,2),0x73],  # ROM write faults
        [ram_base,enc_i(0x67,1,30,0)],  # RAM is not executable
        [0x800000b7,enc_i(3,0,1,0,2),0x73],
        [ram_base,enc_i(0x13,1,0,127),enc_s(30,1,0),0xffffffff,enc_s(30,0,0)],
        [ram_base,enc_i(0x13,1,0,1),enc_b(1,1,12),enc_s(30,1,0),enc_i(3,2,0,-4,2),0x73],
        [ram_base,enc_s(30,0,1,1),0x73],
        [enc_i(0x67,1,0,len(memory['state'])*2)],  # first unmapped instruction word
        [ram_base,enc_i(3,0,30,0,2),0x73],
    ]
    if count == 4:
        programs = [
            [ram_base,enc_i(0x13,1,0,-1),enc_s(30,1,3,0),enc_i(3,2,30,3,0)],
            [enc_i(3,1,0,0,2),0x73],
            [enc_i(0x13,1,0,17),enc_s(0,1,0,2),0x73],
            [ram_base,enc_i(0x67,1,30,0)],
            [0x800000b7,enc_i(3,0,1,0,2),0x73],
            [ram_base,enc_s(30,0,0,0),0xffffffff,enc_s(30,0,2,1)],
        ]
    return {'programs':len(programs), 'retirements':sum(composed_program_audit(cpu,memory,p,j) for j,p in enumerate(programs))}


class ImportedRV32ITests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        path = os.environ.get('RV32I_MACHINE')
        if not path:
            raise unittest.SkipTest('RV32I_MACHINE is not set: imported RTL tests not run')
        cls.machine = json.loads(Path(path).read_text())
        required = {'state', 'reset', 'next', 'wires', 'outputs'}
        if not required <= cls.machine.keys():
            raise ValueError('requested file is not an imported RV32I machine')
        if len([k for k in cls.machine['state'] if k.startswith('r') and k[1:].isdigit()]) != 32:
            raise ValueError('requested machine does not have all 32 RV32I registers')

    def test_actual_imported_disjoint_eei_composition(self):
        path = os.environ.get('RV32I_MEMORY_MACHINE')
        if not path:
            self.skipTest('RV32I_MEMORY_MACHINE is not set: memory composition not run')
        memory = json.loads(Path(path).read_text())
        result = composed_audit(self.machine, memory)
        expected = {'programs': 6, 'retirements': 17} if len(memory['state']) == 8 else {'programs': 10, 'retirements': 31}
        self.assertEqual(result, expected)

    def test_imported_reset_initializes_all_state(self):
        state = initial(self.machine)
        self.assertEqual(set(state), set(self.machine['state']))
        self.assertTrue(all(val(value) == 0 for value in state.values()))

    def test_all_40_instruction_names_and_encoding_classes(self):
        self.assertEqual(alu_audit(self.machine), 3418)

    def test_dependency_precision_and_stall_programs(self):
        self.assertEqual(programs_audit(self.machine), {'programs': 40, 'retirements': 2530})


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--machine', type=Path, required=True)
    parser.add_argument('--memory-machine', type=Path)
    args = parser.parse_args()
    if not args.machine.is_file():
        parser.error('requested imported machine does not exist: ' + str(args.machine))
    if args.memory_machine is not None:
        if not args.memory_machine.is_file():
            parser.error('requested imported memory does not exist: ' + str(args.memory_machine))
        os.environ['RV32I_MEMORY_MACHINE'] = str(args.memory_machine)
    os.environ['RV32I_MACHINE'] = str(args.machine)
    unittest.main(argv=['test_rv32i_pipeline'], verbosity=2)
