"""Handwritten RV32I architectural expressions, independent of the Veryl DUT.

The memory environment supplies access faults and a little-endian aligned word.
Addresses remain complete 32-bit byte addresses. Unsupported encodings trap;
FENCE's reserved fields are accepted as a conservative full fence. No CSR,
FENCE.I, privileged execution, compressed instructions, or misalignment emulation.
"""


def b(w, n): return ['bv', w, n & ((1 << w) - 1)]
def eq(a, z): return ['eq', a, z]
def it(c, a, z): return ['ite', c, a, z]
def ex(hi, lo, a): return ['extract', hi, lo, a]
def op(name, a, z): return [name, a, z]
def land(*args):
    result = True
    for arg in args: result = ['and', result, arg]
    return result

def lor(*args):
    result = False
    for arg in args: result = ['or', result, arg]
    return result

def neg(a): return ['not', a]
def _width(a):
    if a[0] == 'bv': return a[1]
    if a[0] == 'extract': return a[1] - a[2] + 1
    if a[0] == 'concat': return _width(a[1]) + _width(a[2])
    raise ValueError('extension needs an explicitly sized expression')

def sext(width, a): return ['sext', width - _width(a), a]
def zext(width, a): return ['zext', width - _width(a), a]
def cat(*args):
    result = args[0]
    for arg in args[1:]: result = ['concat', result, arg]
    return result

def choose(cases, default):
    for condition, value in reversed(cases): default = it(condition, value, default)
    return default

def reg_read(index, values=None, prefix='s.r'):
    """Read 32 architectural registers; x0 is zero regardless of backing state."""
    if values is None: values = [prefix + str(i) for i in range(32)]
    if len(values) != 32: raise ValueError('RV32I has exactly 32 registers')
    return choose([(eq(index, b(5, i)), b(32, 0) if i == 0 else values[i])
                   for i in range(31)], values[31])


def decode(insn):
    """Exact legal instruction predicates and source/destination selectors."""
    opcode, f3, f7 = ex(6, 0, insn), ex(14, 12, insn), ex(31, 25, insn)
    def base(code): return eq(opcode, b(7, code))
    def form(code, funct): return land(base(code), eq(f3, b(3, funct)))
    d = {'rd': ex(11, 7, insn), 'rs1': ex(19, 15, insn), 'rs2': ex(24, 20, insn)}
    for name, code in [('lui', 0x37), ('auipc', 0x17), ('jal', 0x6f)]: d[name] = base(code)
    d['jalr'] = form(0x67, 0)
    for name, funct in [('beq', 0), ('bne', 1), ('blt', 4), ('bge', 5), ('bltu', 6), ('bgeu', 7)]: d[name] = form(0x63, funct)
    for name, funct in [('lb', 0), ('lh', 1), ('lw', 2), ('lbu', 4), ('lhu', 5)]: d[name] = form(3, funct)
    for name, funct in [('sb', 0), ('sh', 1), ('sw', 2)]: d[name] = form(0x23, funct)
    for name, funct in [('addi', 0), ('slti', 2), ('sltiu', 3), ('xori', 4), ('ori', 6), ('andi', 7)]: d[name] = form(0x13, funct)
    for name, funct, upper in [('slli', 1, 0), ('srli', 5, 0), ('srai', 5, 32)]: d[name] = land(form(0x13, funct), eq(f7, b(7, upper)))
    for name, funct, upper in [('add', 0, 0), ('sub', 0, 32), ('sll', 1, 0), ('slt', 2, 0), ('sltu', 3, 0), ('xor', 4, 0), ('srl', 5, 0), ('sra', 5, 32), ('or', 6, 0), ('and', 7, 0)]:
        d[name] = land(form(0x33, funct), eq(f7, b(7, upper)))
    d['fence'] = form(0x0f, 0)
    d['ecall'], d['ebreak'] = eq(insn, b(32, 0x73)), eq(insn, b(32, 0x100073))
    d['legal'] = lor(*(d[k] for k in list(d)[3:]))
    d['load'] = lor(*(d[k] for k in ('lb', 'lh', 'lw', 'lbu', 'lhu')))
    d['store'] = lor(*(d[k] for k in ('sb', 'sh', 'sw')))
    d['branch'] = lor(*(d[k] for k in ('beq', 'bne', 'blt', 'bge', 'bltu', 'bgeu')))
    d['uses1'] = land(d['legal'], lor(base(3), base(0x23), base(0x63), base(0x13), base(0x33), base(0x67)))
    d['uses2'] = land(d['legal'], lor(base(0x23), base(0x63), base(0x33)))
    return d


def build_execute(insn, pc, rs1, rs2, imem_fault=False, dmem_word=None, dmem_fault=False):
    """Return a pure architectural action, with no references to DUT state.

    dmem_address is the full effective BYTE address, even for a failing access.
    dmem_word corresponds to address & ~3. dmem_valid requests a side-effect-free
    read/permission check. Only nontrapping retirement may apply store_mask/data.
    cause is BV4; exception priority and trap values are explicit platform policy.
    The caller suppresses actions after terminal halt and qualifies retirement.
    """
    if dmem_word is None: dmem_word = b(32, 0)
    d = decode(insn)
    imm_i = sext(32, ex(31, 20, insn))
    imm_s = sext(32, cat(ex(31, 25, insn), ex(11, 7, insn)))
    imm_b = sext(32, cat(ex(31, 31, insn), ex(7, 7, insn), ex(30, 25, insn), ex(11, 8, insn), b(1, 0)))
    imm_u = cat(ex(31, 12, insn), b(12, 0))
    imm_j = sext(32, cat(ex(31, 31, insn), ex(19, 12, insn), ex(20, 20, insn), ex(30, 21, insn), b(1, 0)))
    add = lambda a, z: op('add', a, z)
    taken = lor(land(d['beq'], eq(rs1, rs2)), land(d['bne'], neg(eq(rs1, rs2))),
                land(d['blt'], op('slt', rs1, rs2)), land(d['bge'], neg(op('slt', rs1, rs2))),
                land(d['bltu'], op('ult', rs1, rs2)), land(d['bgeu'], neg(op('ult', rs1, rs2))))
    target = choose([(d['jalr'], op('band', add(rs1, imm_i), b(32, 0xfffffffe))),
                     (d['jal'], add(pc, imm_j))], add(pc, imm_b))
    redirect = lor(d['jal'], d['jalr'], taken)
    target_bad = land(redirect, neg(eq(ex(1, 0, target), b(2, 0))))
    address = add(rs1, it(d['store'], imm_s, imm_i))
    half = lor(d['lh'], d['lhu'], d['sh'])
    word = lor(d['lw'], d['sw'])
    misaligned = lor(land(half, neg(eq(ex(0, 0, address), b(1, 0)))),
                     land(word, neg(eq(ex(1, 0, address), b(2, 0)))))
    shift = cat(b(27, 0), ex(1, 0, address), b(3, 0))
    shifted = op('lshr', dmem_word, shift)
    loaded = choose([(d['lb'], sext(32, ex(7, 0, shifted))), (d['lh'], sext(32, ex(15, 0, shifted))),
                     (d['lbu'], zext(32, ex(7, 0, shifted))), (d['lhu'], zext(32, ex(15, 0, shifted)))], dmem_word)
    def arithmetic_shift(value, amount):
        # IR intentionally has no native arithmetic shift: sign-fill using NOT.
        return it(eq(ex(31, 31, value), b(1, 0)), op('lshr', value, amount),
                  op('bxor', op('lshr', op('bxor', value, b(32, 0xffffffff)), amount), b(32, 0xffffffff)))
    shamt_i, shamt_r = zext(32, ex(24, 20, insn)), zext(32, ex(4, 0, rs2))
    result_cases = [(d['lui'], imm_u), (d['auipc'], add(pc, imm_u)), (lor(d['jal'], d['jalr']), add(pc, b(32, 4))), (d['load'], loaded)]
    for name, ir_op in [('add', 'add'), ('sub', 'sub'), ('xor', 'bxor'), ('or', 'bor'), ('and', 'band')]:
        result_cases.append((d[name], op(ir_op, rs1, rs2)))
        if name != 'sub': result_cases.append((d[name + 'i'], op(ir_op, rs1, imm_i)))
    for name, cmp in [('slt', 'slt'), ('sltu', 'ult')]:
        result_cases.append((d[name], it(op(cmp, rs1, rs2), b(32, 1), b(32, 0))))
        iname = 'slti' if name == 'slt' else 'sltiu'
        result_cases.append((d[iname], it(op(cmp, rs1, imm_i), b(32, 1), b(32, 0))))
    for name, amount, predicate in [('sll', shamt_r, d['sll']), ('sll', shamt_i, d['slli']), ('srl', shamt_r, d['srl']), ('srl', shamt_i, d['srli']), ('sra', shamt_r, d['sra']), ('sra', shamt_i, d['srai'])]:
        result_cases.append((predicate, arithmetic_shift(rs1, amount) if name == 'sra' else op('shl' if name == 'sll' else 'lshr', rs1, amount)))
    # Priority: PC alignment, fetch fault, illegal, system, target, data alignment/access.
    failures = [(neg(eq(ex(1, 0, pc), b(2, 0))), 0, pc), (imem_fault, 1, pc), (neg(d['legal']), 2, insn), (d['ecall'], 8, b(32, 0)),
                (d['ebreak'], 3, pc), (target_bad, 0, target),
                (land(d['load'], misaligned), 4, address), (land(d['store'], misaligned), 6, address),
                (land(d['load'], dmem_fault), 5, address), (land(d['store'], dmem_fault), 7, address)]
    trap = lor(*(condition for condition, _, _ in failures))
    writes = land(d['legal'], neg(lor(d['branch'], d['store'], d['fence'], d['ecall'], d['ebreak'])))
    return {**{k: d[k] for k in ('rd', 'rs1', 'rs2', 'uses1', 'uses2', 'legal')},
            'redirect': redirect,
            'writes': land(writes, neg(eq(d['rd'], b(5, 0)))),
            'rd_write': land(writes, neg(eq(d['rd'], b(5, 0))), neg(trap)),
            'rd_value': choose(result_cases, b(32, 0)),
            'next_pc': it(trap, pc, it(redirect, target, add(pc, b(32, 4)))),
            'dmem_address': address, 'dmem_write': d['store'],
            'dmem_valid': land(lor(d['load'], d['store']), eq(ex(1, 0, pc), b(2, 0)), neg(imem_fault), neg(misaligned)),
            'store_data': op('shl', rs2, shift),
            'store_mask': op('shl', choose([(d['sb'], b(4, 1)), (d['sh'], b(4, 3)), (d['sw'], b(4, 15))], b(4, 0)), zext(4, ex(1, 0, address))),
            'trap': trap, 'cause': choose([(cond, b(4, cause)) for cond, cause, _ in failures], b(4, 0)),
            'trap_value': choose([(cond, value) for cond, _, value in failures], b(32, 0))}
