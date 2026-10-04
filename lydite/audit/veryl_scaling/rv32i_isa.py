"""Independent integer RV32I v2.1 retirement oracle (no DUT expressions).

EEI: little endian, IALIGN=32, naturally aligned data accesses, synchronous
word responses. Reserved/non-RV32I encodings trap by platform choice. FENCE
fields (including reserved fm) are ignored conservatively in this serial model.
ECALL reports cause 8 by environment convention; this is not a privileged ISA.
Trap next_pc is the faulting PC; no trap-vector/CSR machinery is modeled.
"""
from dataclasses import dataclass

MASK = 0xffffffff
INSTRUCTIONS = tuple('LUI AUIPC JAL JALR BEQ BNE BLT BGE BLTU BGEU LB LH LW LBU LHU SB SH SW ADDI SLTI SLTIU XORI ORI ANDI SLLI SRLI SRAI ADD SUB SLL SLT SLTU XOR SRL SRA OR AND FENCE ECALL EBREAK'.split())


def signed(value, width=32):
    value &= (1 << width) - 1
    return value - (1 << width) if value & (1 << (width - 1)) else value


@dataclass(frozen=True)
class StepResult:
    next_pc: int
    rd: int = 0
    value: int = 0
    store_address: int = 0
    store_data: int = 0
    store_mask: int = 0
    trap: bool = False
    cause: int = 0
    trap_value: int = 0
    memory_address: int = 0
    mnemonic: str = ''


def execute(ir, pc, regs, load_word=0, imem_fault=False, dmem_fault=False):
    """Return one architectural effect without mutating regs.

    load_word is the 32-bit word at effective_address & ~3. Store address is the
    full effective byte address, data is lane shifted, mask has one bit per byte. A trap
    suppresses register and store effects. x0 always reads zero, even if regs[0]
    is corrupt. Fault inputs matter only for the corresponding active access.
    """
    if len(regs) != 32:
        raise ValueError('RV32I requires exactly 32 integer register values')
    ir, pc = ir & MASK, pc & MASK
    def fault(cause, value, name=''):
        return StepResult(pc, trap=True, cause=cause, trap_value=value & MASK, mnemonic=name)
    if pc % 4:
        return fault(0, pc)
    if imem_fault:
        return fault(1, pc)
    opcode, rd, f3 = ir & 127, (ir >> 7) & 31, (ir >> 12) & 7
    rs1, rs2, f7 = (ir >> 15) & 31, (ir >> 20) & 31, ir >> 25
    a, b = (regs[rs1] & MASK) if rs1 else 0, (regs[rs2] & MASK) if rs2 else 0
    imm = signed(ir >> 20, 12)
    npc, result, name = (pc + 4) & MASK, 0, ''
    write = False
    if opcode in (0x37, 0x17):
        name = 'LUI' if opcode == 0x37 else 'AUIPC'
        result = (ir & 0xfffff000) + (pc if opcode == 0x17 else 0)
        write = True
    elif opcode in (0x6f, 0x67):
        if opcode == 0x67 and f3 != 0:
            return fault(2, ir)
        name = 'JAL' if opcode == 0x6f else 'JALR'
        offset = signed(((ir >> 31) << 20) | (((ir >> 12) & 255) << 12) | (((ir >> 20) & 1) << 11) | (((ir >> 21) & 1023) << 1), 21)
        target = (pc + offset) & MASK if opcode == 0x6f else ((a + imm) & MASK) & ~1
        if target % 4:
            return fault(0, target, name)
        result, npc, write = npc, target, True
    elif opcode == 0x63:
        choices = {0: ('BEQ', a == b), 1: ('BNE', a != b), 4: ('BLT', signed(a) < signed(b)), 5: ('BGE', signed(a) >= signed(b)), 6: ('BLTU', a < b), 7: ('BGEU', a >= b)}
        if f3 not in choices:
            return fault(2, ir)
        name, taken = choices[f3]
        offset = signed(((ir >> 31) << 12) | (((ir >> 7) & 1) << 11) | (((ir >> 25) & 63) << 5) | (((ir >> 8) & 15) << 1), 13)
        if taken:
            npc = (pc + offset) & MASK
            if npc % 4:
                return fault(0, npc, name)
    elif opcode in (0x03, 0x23):
        load = opcode == 0x03
        names = {0: 'LB', 1: 'LH', 2: 'LW', 4: 'LBU', 5: 'LHU'} if load else {0: 'SB', 1: 'SH', 2: 'SW'}
        if f3 not in names:
            return fault(2, ir)
        name = names[f3]
        offset = imm if load else signed(((ir >> 25) << 5) | ((ir >> 7) & 31), 12)
        address = (a + offset) & MASK
        size = 1 << (f3 & 3)
        if address % size:
            return fault(4 if load else 6, address, name)
        if dmem_fault:
            return fault(5 if load else 7, address, name)
        lane = address % 4
        if not load:
            return StepResult(npc, store_address=address, store_data=(b << (lane * 8)) & MASK, store_mask=((1 << size) - 1) << lane, memory_address=address, mnemonic=name)
        result = (load_word >> (lane * 8)) & ((1 << (8 * size)) - 1)
        if f3 < 4:
            result = signed(result, 8 * size)
        return StepResult(npc, rd=rd, value=(result & MASK) if rd else 0, memory_address=address, mnemonic=name)
    elif opcode in (0x13, 0x33):
        immediate = opcode == 0x13
        rhs = imm & MASK if immediate else b
        shift = (ir >> 20) & 31 if immediate else b & 31
        if not immediate and f7 not in (0, 0x20):
            return fault(2, ir)
        if not immediate and f7 == 0x20 and f3 not in (0, 5):
            return fault(2, ir)
        if f3 in (1, 5) and immediate and f7 not in ((0,) if f3 == 1 else (0, 0x20)):
            return fault(2, ir)
        if f3 == 0:
            name = 'ADDI' if immediate else ('SUB' if f7 == 0x20 else 'ADD')
            result = a - rhs if name == 'SUB' else a + rhs
        elif f3 == 1:
            name, result = 'SLL', a << shift
        elif f3 == 2:
            name, result = 'SLT', int(signed(a) < signed(rhs))
        elif f3 == 3:
            name, result = 'SLTU', int(a < rhs)
        elif f3 == 4:
            name, result = 'XOR', a ^ rhs
        elif f3 == 5:
            name = 'SRA' if f7 == 0x20 else 'SRL'
            result = (signed(a) if name == 'SRA' else a) >> shift
        elif f3 == 6:
            name, result = 'OR', a | rhs
        else:
            name, result = 'AND', a & rhs
        if immediate and f3:
            name = 'SLTIU' if name == 'SLTU' else name + 'I'
        write = True
    elif opcode == 0x0f and f3 == 0:
        name = 'FENCE'
    elif ir == 0x00000073:
        return fault(8, 0, 'ECALL')
    elif ir == 0x00100073:
        return fault(3, pc, 'EBREAK')
    else:
        return fault(2, ir)
    return StepResult(npc, rd=rd if write else 0, value=(result & MASK) if write and rd else 0, mnemonic=name)
