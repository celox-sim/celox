"""Independent symbolic ISA versus integer oracle, without a DUT dependency."""
import random
import unittest
from audit.veryl_scaling import rv32i_spec as spec
from audit.veryl_scaling.rv32i_isa import execute, INSTRUCTIONS
from audit.veryl_scaling.test_rv32i_isa import r, i, s, branch, jal


def evaluate(expression, inputs):
    cache = {}
    def ev(e):
        if isinstance(e, bool): return e
        if isinstance(e, str): return inputs[e]
        key = id(e)
        if key in cache: return cache[key]
        name = e[0]
        if name == 'bv': result = (e[2], e[1])
        elif name == 'extract':
            value, _ = ev(e[3]); width = e[1] - e[2] + 1
            result = ((value >> e[2]) & ((1 << width) - 1), width)
        elif name in ('sext', 'zext'):
            value, width = ev(e[2]); total = width + e[1]
            if name == 'sext' and value >> (width - 1): value -= 1 << width
            result = (value & ((1 << total) - 1), total)
        elif name == 'ite': result = ev(e[2] if ev(e[1]) else e[3])
        elif name == 'not': result = not ev(e[1])
        elif name in ('and', 'or'):
            a, z = ev(e[1]), ev(e[2]); result = a and z if name == 'and' else a or z
        else:
            a, z = ev(e[1]), ev(e[2])
            if name == 'eq': result = a == z
            elif name == 'concat': result = ((a[0] << z[1]) | z[0], a[1] + z[1])
            else:
                av, width = a; zv, zw = z
                if width != zw: raise AssertionError((name, width, zw))
                signed = lambda v: v - (1 << width) if v >> (width - 1) else v
                if name == 'ult': result = av < zv
                elif name == 'slt': result = signed(av) < signed(zv)
                else:
                    if name == 'add': value = av + zv
                    elif name == 'sub': value = av - zv
                    elif name == 'band': value = av & zv
                    elif name == 'bor': value = av | zv
                    elif name == 'bxor': value = av ^ zv
                    elif name == 'shl': value = av << zv if zv < width else 0
                    elif name == 'lshr': value = av >> zv if zv < width else 0
                    else: raise AssertionError(name)
                    result = (value & ((1 << width) - 1), width)
        cache[key] = result
        return result
    answer = ev(expression)
    return answer[0] if isinstance(answer, tuple) else answer


class SymbolicRV32ITests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.action = spec.build_execute('ir', 'pc', 'a', 'b', 'ifault', 'word', 'dfault')

    def check(self, ir, a, b, pc=0x100, word=0xdeadbeef, ifault=False, dfault=False):
        regs = [0] * 32
        rs1, rs2 = (ir >> 15) & 31, (ir >> 20) & 31
        if rs1: regs[rs1] = a
        if rs2: regs[rs2] = b
        inputs = {k: (v, 32) for k, v in {'ir': ir, 'pc': pc, 'a': regs[rs1], 'b': regs[rs2], 'word': word}.items()}
        inputs.update(ifault=ifault, dfault=dfault)
        got = {k: evaluate(v, inputs) for k, v in self.action.items()}
        ref = execute(ir, pc, regs, word, ifault, dfault)
        for key, expected in [('trap', ref.trap), ('next_pc', ref.next_pc), ('cause', ref.cause), ('trap_value', ref.trap_value)]:
            self.assertEqual(got[key], expected, (hex(ir), key, hex(a), hex(b)))
        self.assertEqual(got['rd_write'], bool(ref.rd), hex(ir))
        if got['rd_write']: self.assertEqual((got['rd'], got['rd_value']), (ref.rd, ref.value), hex(ir))
        if ref.store_mask:
            self.assertEqual((got['dmem_address'], got['store_data'], got['store_mask']), (ref.store_address, ref.store_data, ref.store_mask))
        if got['trap']: self.assertFalse(got['rd_write'])
        return got

    def test_randomized_legal_and_reserved_encodings(self):
        rng = random.Random(0x5256333249)
        for _ in range(1600):
            immediate = rng.randrange(-2048, 2048)
            ir = rng.choice([r(rng.randrange(8), rng.choice([0, 32, 1])), i(immediate, rng.randrange(8)),
                i(immediate, rng.randrange(8), 3), s(immediate, rng.randrange(8)),
                branch(immediate * 2, rng.randrange(8)), jal(immediate * 2),
                i(immediate, rng.randrange(8), 0x67), rng.getrandbits(20) << 12 | 0x1b7,
                rng.getrandbits(20) << 12 | 0x197, 0x73, 0x100073, rng.getrandbits(32),
                rng.getrandbits(17) << 15 | rng.randrange(32) << 7 | 0xf])
            self.check(ir, rng.getrandbits(32), rng.getrandbits(32), word=rng.getrandbits(32),
                       pc=rng.getrandbits(30) * 4, ifault=rng.randrange(20) == 0, dfault=rng.randrange(4) == 0)

    def test_every_rv32i_instruction(self):
        instructions = [0x123451b7, 0x12345197, jal(8), i(0, opcode=0x67), 0xf, 0x73, 0x100073]
        instructions += [r(f3) for f3 in range(8)] + [r(0, 32), r(5, 32)]
        instructions += [i(3, f3) for f3 in range(8)] + [i(0x403, 5)]
        instructions += [branch(8, f3) for f3 in (0, 1, 4, 5, 6, 7)]
        instructions += [i(0, f3, 3) for f3 in (0, 1, 2, 4, 5)]
        instructions += [s(0, f3) for f3 in (0, 1, 2)]
        names = set()
        for ir in instructions:
            self.check(ir, 0x80000000, 3)
            regs = [0] * 32; regs[1] = 0x80000000; regs[2] = 3
            names.add(execute(ir, 0x100, regs).mnemonic)
        self.assertEqual(names, set(INSTRUCTIONS))

    def test_load_store_lanes_and_faults(self):
        for lane in range(4):
            for f3 in (0, 1, 2, 4, 5):
                for fault in (False, True): self.check(i(0, f3, 3), 0x80000000 + lane, 0, dfault=fault)
            for f3 in range(3):
                for fault in (False, True): self.check(s(0, f3), 0x80000000 + lane, 0x12345678, dfault=fault)

    def test_shift_signed_and_zero_destination(self):
        for amount in (0, 1, 31, 32, 63, 0xffffffff):
            for f3, f7 in ((1, 0), (5, 0), (5, 32), (2, 0), (3, 0)):
                self.check(r(f3, f7), 0x80000001, amount)
                self.check(r(f3, f7, rd=0), 0x80000001, amount)
        self.check(i(0, 2, 3, rd=0), 0xffffffff, 0, dfault=True)

    def test_unqualified_redirect_and_writes(self):
        for fault in (False, True):
            got = self.check(jal(8), 0, 0, ifault=fault)
            self.assertTrue(got['redirect'])
            self.assertTrue(got['writes'])
            self.assertEqual(got['rd_write'], not fault)
            got = self.check(jal(2), 0, 0, ifault=fault)
            self.assertTrue(got['redirect'])
            self.assertTrue(got['writes'])
            self.assertFalse(got['rd_write'])
            got = self.check(jal(8, rd=0), 0, 0, ifault=fault)
            self.assertTrue(got['redirect'])
            self.assertFalse(got['writes'])
            got = self.check(branch(8), 7, 7, ifault=fault)
            self.assertTrue(got['redirect'])
            self.assertFalse(got['writes'])
            got = self.check(branch(8), 7, 8, ifault=fault)
            self.assertFalse(got['redirect'])
        for ir in (0xffffffff, 0x73, 0x100073, 0xf, s(0)):
            got = self.check(ir, 0, 0)
            self.assertFalse(got['redirect'])
            self.assertFalse(got['writes'])

    def test_pc_alignment_has_priority(self):
        for pc in (1, 2, 3, 0xffffffff):
            for fault in (False, True): self.check(0xffffffff, 0, 0, pc=pc, ifault=fault)

    def test_register_mux_exhaustive_selectors_random_banks(self):
        rng = random.Random(0x52454753)
        for _ in range(100):
            bank = [rng.getrandbits(32) for _ in range(32)]
            values = [spec.b(32, value) for value in bank]
            expression = spec.reg_read('index', values)
            old = spec.choose([(spec.eq('index', spec.b(5, n)), values[n])
                               for n in range(1, 32)], spec.b(32, 0))
            for index in range(32):
                inputs = {'index': (index, 5)}
                actual = evaluate(expression, inputs)
                self.assertEqual(actual, evaluate(old, inputs))
                self.assertEqual(actual, bank[index] if index else 0)

    def test_x0_reads_even_corrupt_backing(self):
        for index in range(32):
            expression = spec.reg_read(spec.b(5, index), [spec.b(32, 100 + n) for n in range(32)])
            self.assertEqual(evaluate(expression, {}), 0 if index == 0 else 100 + index)


if __name__ == '__main__': unittest.main()
