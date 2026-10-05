"""Compiler-free directed semantic tests; run with python3 -m unittest."""
import unittest
from audit.veryl_scaling.rv32i_isa import execute, INSTRUCTIONS, MASK


def r(f3, f7=0, rd=3, rs1=1, rs2=2):
    return f7 << 25 | rs2 << 20 | rs1 << 15 | f3 << 12 | rd << 7 | 0x33


def i(imm, f3=0, opcode=0x13, rd=3, rs1=1):
    return (imm & 4095) << 20 | rs1 << 15 | f3 << 12 | rd << 7 | opcode


def s(imm, f3=2, rs1=1, rs2=2):
    n = imm & 4095
    return (n >> 5) << 25 | rs2 << 20 | rs1 << 15 | f3 << 12 | (n & 31) << 7 | 0x23


def branch(imm, f3=0, rs1=1, rs2=2):
    n = imm & 8191
    return ((n >> 12) & 1) << 31 | ((n >> 5) & 63) << 25 | rs2 << 20 | rs1 << 15 | f3 << 12 | ((n >> 1) & 15) << 8 | ((n >> 11) & 1) << 7 | 0x63


def jal(imm, rd=3):
    n = imm & 0x1fffff
    return ((n >> 20) & 1) << 31 | ((n >> 1) & 1023) << 21 | ((n >> 11) & 1) << 20 | ((n >> 12) & 255) << 12 | rd << 7 | 0x6f


class RV32IReferenceTests(unittest.TestCase):
    def run_ir(self, ir, a=0, b=0, pc=0x100, word=0, **kw):
        regs = [0xdeadbeef] + [0] * 31
        regs[1], regs[2] = a, b
        before = regs[:]
        result = execute(ir, pc, regs, word, **kw)
        self.assertEqual(regs, before)
        return result

    def test_all_40_instruction_names_and_basic_values(self):
        seen = set()
        cases = [
            (0x123451b7, 0, 0, 0x12345000, 'LUI'),
            (0x12345197, 0, 0, 0x12345100, 'AUIPC'),
            (jal(8), 0, 0, 0x104, 'JAL'),
            (i(3, opcode=0x67), 0x105, 0, 0x104, 'JALR'),
        ]
        for f3, name, expected in [(0,'ADD',9),(1,'SLL',28),(2,'SLT',0),(3,'SLTU',0),(4,'XOR',5),(5,'SRL',1),(6,'OR',7),(7,'AND',2)]:
            cases.append((r(f3), 7, 2, expected, name))
        cases += [(r(0,32),7,2,5,'SUB'),(r(5,32),0xfffffff8,2,0xfffffffe,'SRA')]
        for f3,name,expected in [(0,'ADDI',9),(1,'SLLI',28),(2,'SLTI',0),(3,'SLTIU',0),(4,'XORI',5),(5,'SRLI',1),(6,'ORI',7),(7,'ANDI',2)]:
            cases.append((i(2,f3),7,0,expected,name))
        cases.append((i(0x402,5),0xfffffff8,0,0xfffffffe,'SRAI'))
        for ir,a,b,value,name in cases:
            with self.subTest(name=name):
                got = self.run_ir(ir,a,b)
                self.assertFalse(got.trap)
                self.assertEqual((got.value,got.mnemonic),(value,name))
                seen.add(name)
        for f3,name in [(0,'BEQ'),(1,'BNE'),(4,'BLT'),(5,'BGE'),(6,'BLTU'),(7,'BGEU')]:
            got=self.run_ir(branch(8,f3)); self.assertEqual(got.mnemonic,name); seen.add(name)
        for f3,name,value in [(0,'LB',0xffffff80),(1,'LH',0xffff8080),(2,'LW',0x80808080),(4,'LBU',128),(5,'LHU',0x8080)]:
            got=self.run_ir(i(0,f3,0x03),word=0x80808080)
            self.assertEqual((got.mnemonic,got.value),(name,value)); seen.add(name)
        for f3,name,mask in [(0,'SB',1),(1,'SH',3),(2,'SW',15)]:
            got=self.run_ir(s(0,f3),b=0xdeadbeef)
            self.assertEqual((got.mnemonic,got.store_mask),(name,mask)); seen.add(name)
        for ir,name,cause in [(0xf, 'FENCE',None),(0x73,'ECALL',8),(0x100073,'EBREAK',3)]:
            got=self.run_ir(ir); self.assertEqual(got.mnemonic,name); seen.add(name)
            if cause is not None: self.assertEqual(got.cause,cause)
        self.assertEqual(seen,set(INSTRUCTIONS)); self.assertEqual(len(INSTRUCTIONS),40)

    def test_immediates_wrap_alias_and_zero(self):
        for immediate in (-2048,-1,0,2047):
            self.assertEqual(self.run_ir(i(immediate),a=0xffffffff).value,(0xffffffff+immediate)&MASK)
        self.assertEqual(self.run_ir(i(1,rd=1),a=5).value,6)
        self.assertEqual(self.run_ir(i(1,rs1=0)).value,1)
        self.assertEqual(self.run_ir(i(1,rd=0),a=5).value,0)
        self.assertEqual(self.run_ir(0xfffff197,pc=0x80000000).value,0x7ffff000)
        self.assertEqual(self.run_ir(i(0),pc=0xfffffffc).next_pc,0)
        self.assertEqual(self.run_ir(i(0,opcode=0x67,rd=1),a=0xfffffffd).next_pc,0xfffffffc)
        for offset in (-1048576,-4096,-4,0,4,1048572):
            self.assertEqual(self.run_ir(jal(offset),pc=0x80000000).next_pc,(0x80000000+offset)&MASK)
        for offset in (-4096,-4,0,4,4092):
            self.assertEqual(self.run_ir(branch(offset),pc=0x80000000).next_pc,(0x80000000+offset)&MASK)

    def test_signed_unsigned_and_shift_boundaries(self):
        for f3,expected in [(2,1),(3,0)]: self.assertEqual(self.run_ir(r(f3),0x80000000,1).value,expected)
        self.assertEqual(self.run_ir(i(-1,3),0xfffffffe).value,1)
        for sh in (0,31,32,63,0xffffffff):
            n=sh&31
            for f3,f7,value in [(1,0,(0x80000001<<n)&MASK),(5,0,0x80000001>>n),(5,32,(-2147483647>>n)&MASK)]:
                self.assertEqual(self.run_ir(r(f3,f7),0x80000001,sh).value,value)
        for sh in (0,31):
            self.assertEqual(self.run_ir(i(sh,1),1).value,1<<sh)
            self.assertEqual(self.run_ir(i(0x400|sh,5),0x80000000).value,(-2147483648>>sh)&MASK)
        for f3, pairs in [(0,[(1,1,True),(1,2,False)]),(1,[(1,1,False),(1,2,True)]),(4,[(MASK,0,True),(0,MASK,False)]),(5,[(MASK,0,False),(0,MASK,True)]),(6,[(MASK,0,False),(0,MASK,True)]),(7,[(MASK,0,True),(0,MASK,False)])]:
            for a,b,taken in pairs:
                self.assertEqual(self.run_ir(branch(8,f3),a,b).next_pc,0x108 if taken else 0x104)

    def test_memory_lanes_and_address_wrap(self):
        for lane in range(4):
            got=self.run_ir(i(lane,4,3),a=0x80000000,word=0x80ff017f)
            self.assertEqual(got.value,[127,1,255,128][lane]); self.assertEqual(got.memory_address,0x80000000+lane)
            got=self.run_ir(s(lane,0),a=0x80000000,b=0x123456ab)
            self.assertEqual((got.store_address,got.store_mask,got.store_data),(0x80000000+lane,1<<lane,(0x123456ab<<(8*lane))&MASK))
        self.assertEqual(self.run_ir(i(2,1,3),word=0x80007fff).value,0xffff8000)
        self.assertEqual(self.run_ir(s(2,1),b=0x1234).store_data,0x12340000)
        self.assertEqual(self.run_ir(i(-1,4,3),a=0,word=0xab000000).value,0xab)
        self.assertEqual(self.run_ir(s(1,0),a=MASK).store_address,0)
        self.assertEqual(self.run_ir(s(-2048),a=2048,b=7).store_address,0)

    def test_faults_suppress_effects_and_priority(self):
        cases=[(jal(2),{},0,0x102),(i(0,opcode=0x67),{'a':3},0,2),(branch(2),{},0,0x102),
               (i(1,1,3),{},4,1),(s(2),{},6,2),(i(0,2,3,rd=0),{'dmem_fault':True},5,0),
               (s(0),{'dmem_fault':True},7,0),(0xffffffff,{},2,0xffffffff),
               (0x73,{},8,0),(0x100073,{},3,0x100),(i(0),{'imem_fault':True},1,0x100),
               (i(1,1,3),{'dmem_fault':True},4,1),(i(0),{'pc':2,'imem_fault':True},0,2)]
        for ir,kw,cause,tval in cases:
            got=self.run_ir(ir,**kw)
            self.assertTrue(got.trap); self.assertEqual((got.cause,got.trap_value),(cause,tval))
            self.assertEqual((got.rd,got.value,got.store_mask),(0,0,0)); self.assertEqual(got.next_pc,kw.get('pc',0x100))
        self.assertFalse(self.run_ir(branch(2,1)).trap)  # untaken misaligned target
        self.assertFalse(self.run_ir(i(1),dmem_fault=True).trap)
        self.assertEqual(self.run_ir(i(0,opcode=0x67),a=1).next_pc,0)  # clear bit zero first

    def test_encoder_golden_words_and_register_aliases(self):
        # Literal machine words prevent matching errors in encoder/decoder helpers.
        self.assertEqual(i(-1, rd=1, rs1=0), 0xfff00093)
        self.assertEqual(r(0, 32), 0x402081b3)
        self.assertEqual(s(-4), 0xfe20ae23)
        self.assertEqual(branch(-4), 0xfe208ee3)
        self.assertEqual(jal(-4), 0xffdff1ef)
        self.assertEqual(self.run_ir(r(0, rd=2), 7, 9).value, 16)
        self.assertEqual(self.run_ir(r(0, rd=1), 7, 9).value, 16)
        self.assertEqual(self.run_ir(r(0, rs1=0, rs2=0)).value, 0)
        self.assertEqual(self.run_ir(jal(8, rd=0)).value, 0)
        self.assertTrue(self.run_ir(i(1, 1, 3, rd=0)).trap)

    def test_complete_register_and_shift_function_legality(self):
        for f7 in range(128):
            for f3 in range(8):
                legal = f7 == 0 or (f7 == 32 and f3 in (0, 5))
                got = self.run_ir(r(f3, f7))
                self.assertEqual(not got.trap, legal, (f7, f3))
            for f3 in (1, 5):
                legal = f7 == 0 or (f7 == 32 and f3 == 5)
                got = self.run_ir(i((f7 << 5) | 31, f3))
                self.assertEqual(not got.trap, legal, (f7, f3))

    def test_reserved_and_excluded_encodings(self):
        invalid=[0,0xffffffff,0x100f,0x2073,0x200073,i(0,1,0x67),branch(4,2),i(0,3,3),s(0,3),i(32,1),i(0x420,5),r(0,1),r(1,32)]
        for ir in invalid:
            self.assertEqual(self.run_ir(ir).cause,2,hex(ir))
        for fm in range(16):
            got=self.run_ir((fm<<28)|0x0ff00000|(31<<15)|(31<<7)|0xf)
            self.assertFalse(got.trap); self.assertEqual(got.mnemonic,'FENCE')


if __name__ == '__main__': unittest.main()
