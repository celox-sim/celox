#!/usr/bin/env python3
"""Independent concrete regression for the 32-bit, four-word D/X/W CPU.

Generation/reference tests need no compiler or solver:
    python3 -m unittest audit.veryl_scaling.test_cpu_registers

Also execute the actual compiled-and-lifted Veryl implementation:
    python3 audit/veryl_scaling/test_cpu_registers.py --evidence /tmp/gpr-run
    python3 audit/veryl_scaling/test_cpu_registers.py --evidence /tmp/gpr-run --sizes 8

An evidence directory must contain cpu_{8,16,32}gpr/refinement.json and its
source/lift sidecars for every requested size. Missing evidence is a failure,
not a skipped passing case. These finite tests are not universal proofs, and
they neither read proof verdicts nor reinterpret UNKNOWN as success.
"""
import argparse
from collections import Counter
import copy
import json
from pathlib import Path
import random
import re
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from audit.interpreter import BV, evaluate_record, outputs, related
from audit.veryl_scaling import cpu_registers

COUNTS = (8, 16, 32)
MASK = (1 << 32) - 1
STATS = Counter()


def encode(count, opcode, rd=0, rs=0, immediate=0):
    """Integer encoding, deliberately independent of generator expressions."""
    bits = count.bit_length() - 1
    if count not in COUNTS or not (0 <= opcode < 8 and 0 <= rd < count and 0 <= rs < count):
        raise ValueError('instruction field out of range')
    return (opcode << (32 + 2 * bits)) | (rd << (32 + bits)) | (rs << 32) | (immediate & MASK)


def decode(count, instruction):
    bits = count.bit_length() - 1
    return (instruction >> (32 + 2 * bits),
            (instruction >> (32 + bits)) & (count - 1),
            (instruction >> 32) & (count - 1), instruction & MASK)


class IntegerISA:
    """Ordinary Python integer ISA; no generated spec or pipeline expressions."""
    def __init__(self, count, rom, data, registers=None, pc=0):
        self.count = count
        self.rom = tuple(rom)
        self.data = tuple(data)
        self.registers = list(registers) if registers is not None else [0] * count
        self.pc = pc

    def retire(self):
        opcode, rd, rs, immediate = decode(self.count, self.rom[self.pc])
        source = self.registers[rs]
        self.pc = (self.pc + 1) & 3
        if opcode == 0:
            self.registers[rd] = immediate
        elif opcode == 1:
            self.registers[rd] = (source + immediate) & MASK
        elif opcode == 2:
            self.registers[rd] = source ^ immediate
        elif opcode == 3:
            self.registers[rd] = self.data[immediate & 3]
        elif opcode == 4 and source == 0:
            self.pc = immediate & 3
        # Opcodes 5, 6 and 7 retire without changing any GPR.
        return opcode

    def record(self):
        width = 35 + 2 * (self.count.bit_length() - 1)
        return {'pc': BV(2, self.pc),
                **{f'r{k}': BV(32, value) for k, value in enumerate(self.registers)},
                **{f'rom{k}': BV(width, value) for k, value in enumerate(self.rom)},
                **{f'data{k}': BV(32, value) for k, value in enumerate(self.data)}}


def input_record(count, rom, data, *, stall=False, reset=False):
    width = 35 + 2 * (count.bit_length() - 1)
    return {'rst': reset, 'stall': stall,
            **{f'rom{k}': BV(width, value) for k, value in enumerate(rom)},
            **{f'data{k}': BV(32, value) for k, value in enumerate(data)}}


class GenerationTests(unittest.TestCase):
    def test_register_count_validation(self):
        for count in (0, 1, 3, 7, 12, 31, -8):
            with self.subTest(count=count), self.assertRaises(ValueError):
                cpu_registers.register_bits(count)
        for count, bits in ((8, 3), (16, 4), (32, 5)):
            self.assertEqual(cpu_registers.register_bits(count), bits)

    def test_schema_and_field_widths(self):
        for count in COUNTS:
            with self.subTest(count=count):
                doc = cpu_registers.model(count)
                bits = count.bit_length() - 1
                width = 35 + 2 * bits
                arch = {'pc': {'bv': 2},
                        **{f'r{k}': {'bv': 32} for k in range(count)},
                        **{f'rom{k}': {'bv': width} for k in range(4)},
                        **{f'data{k}': {'bv': 32} for k in range(4)}}
                pipeline = {'fetch_pc': {'bv': 2}, 'x_operand': {'bv': 32},
                            'w_result': {'bv': 32}, 'w_next_pc': {'bv': 2}}
                for stage in ('d', 'x', 'w'):
                    pipeline.update({f'{stage}_valid': 'bool', f'{stage}_pc': {'bv': 2},
                                     f'{stage}_ir': {'bv': width}})
                self.assertEqual(doc['spec']['state'], arch)
                self.assertEqual(doc['impl'], {'state': {**arch, **pipeline}})
                self.assertEqual(set(doc['spec']['reset']), set(arch))
                self.assertEqual(set(doc['spec']['next']), set(arch))
                self.assertEqual(doc['inputs'], {'rst': 'bool', 'stall': 'bool',
                    **{k: v for k, v in arch.items() if k.startswith(('rom', 'data'))}})
                self.assertEqual(doc['hold_when'], 'i.stall')

    def test_source_exhaustive_register_choices_and_selector_slices(self):
        for count in COUNTS:
            with self.subTest(count=count):
                bits = count.bit_length() - 1
                source = cpu_registers.source(count)
                self.assertEqual(set(re.findall(r'@[A-Z_]+@', source)), {'@IMMHI@'})
                for k in range(count):
                    self.assertEqual(source.count(f'r{k}: output bit<32>,'), 1)
                    self.assertEqual(source.count(f"r{k} = '0;"), 1)
                    label = f"{bits}'d{k}" if k < count - 1 else 'default'
                    self.assertEqual(source.count(f'{label}: operand = r{k};'), 1)
                    self.assertEqual(source.count(f'{label}: r{k} = w_result;'), 1)
                self.assertIn(f'd_rs = d_ir[{31 + bits}:32];', source)
                for stage in ('x', 'w'):
                    self.assertIn(f'{stage}_rd = {stage}_ir[{31 + 2 * bits}:{32 + bits}];', source)
                for old, _new in cpu_registers.faults(count).values():
                    self.assertEqual(source.count(old), 1, f'nonunique source fault site: {old}')

    def test_reset_all_writable_registers_and_seed_capture(self):
        for count in COUNTS:
            with self.subTest(count=count):
                doc = cpu_registers.model(count)
                rom = [encode(count, k, count - 1, count - 1, MASK - k) for k in range(4)]
                data = [0, 1, 0x80000000, MASK]
                inp = input_record(count, rom, data, reset=True, stall=True)
                old = IntegerISA(count, rom, data, [MASK] * count, 3).record()
                actual = evaluate_record(doc['spec'], 'reset', old, inp)
                self.assertEqual(actual, IntegerISA(count, rom, data).record())

    def test_binding_covers_every_architectural_register(self):
        for count in COUNTS:
            doc = cpu_registers.model(count)
            spec = IntegerISA(count, [0] * 4, [0] * 4).record()
            impl = {name: False if ty == 'bool' else BV(ty['bv'], 0)
                    for name, ty in doc['impl']['state'].items()}
            self.assertTrue(related(doc, spec, impl))
            for index in range(count):
                with self.subTest(count=count, register=index):
                    changed = {**impl, f'r{index}': BV(32, 1)}
                    self.assertFalse(related(doc, spec, changed))


    def check_reference(self, count, opcode, rd, rs, immediate, registers, pc=0):
        instruction = encode(count, opcode, rd, rs, immediate)
        self.assertEqual(decode(count, instruction), (opcode, rd, rs, immediate & MASK))
        data = [0x01020304, 0x80000000, MASK, 0xABCDEF98]
        oracle = IntegerISA(count, [instruction] * 4, data, registers, pc)
        before = oracle.record()
        # Initialization inputs deliberately differ from captured ROM/data.
        inputs = input_record(count, [0] * 4, [0] * 4)
        actual = evaluate_record(cpu_registers.model(count)['spec'], 'next', before, inputs)
        oracle.retire()
        self.assertEqual(actual, oracle.record(),
                         f'{count} GPRs op={opcode} rd={rd} rs={rs} imm={immediate:#x}')
        self.assertEqual([actual[f'r{k}'] for k in range(count) if k != rd],
                         [before[f'r{k}'] for k in range(count) if k != rd])
        STATS['reference_steps'] += 1

    def test_reference_all_destination_source_pairs(self):
        # Exhaust every selector pair, including all high-bit alias collisions.
        for count in COUNTS:
            registers = [((k + 1) * 0x10203041) & MASK for k in range(count)]
            for rd in range(count):
                for rs in range(count):
                    for opcode in (1, 2):
                        self.check_reference(count, opcode, rd, rs, 0x80000003,
                                             registers, pc=3)

    def test_reference_every_opcode_register_and_immediate_edges(self):
        for count in COUNTS:
            registers = [0 if k % 2 else MASK for k in range(count)]
            for index in range(count):
                for opcode in range(8):
                    for immediate in (0, 1, 2, 3, 0x80000000, MASK):
                        self.check_reference(count, opcode, index, count - 1 - index,
                                             immediate, registers, pc=index & 3)


class ReportGateTests(unittest.TestCase):
    """A finite trace pass cannot weaken the separate universal-proof gate."""
    def report(self, fault=False):
        report = {'status': 'stuttering_refinement_verified',
                  'engine_summary': {'z3_queries': 0},
                  'obligations': [{'name': name, 'status': 'passed',
                                   'solver_result': ('sat' if name in ('binding_nonempty',
                                       'progress_nonvacuity', 'commit_reachable_in_relation')
                                       else 'unsat')}
                                  for name in sorted(cpu_registers.runner.OBLIGATIONS)]}
        for obligation in report['obligations']:
            if obligation['solver_result'] == 'sat':
                obligation['finite'] = {'original_formula_validated': True}
        if fault:
            report['status'] = 'counterexample'
            micro = next(row for row in report['obligations']
                         if row['name'] == 'microstep_refinement')
            micro.update(status='failed', solver_result='sat',
                         finite={'original_formula_validated': True})
        return report

    def test_exact_positive_and_validated_negative_are_accepted(self):
        self.assertEqual(cpu_registers.validate_report(self.report(), None), [])
        failed = cpu_registers.validate_report(self.report(True), 'register_read_alias')
        self.assertEqual([row['name'] for row in failed], ['microstep_refinement'])

    def test_unknown_is_rejected_even_if_other_status_fields_claim_success(self):
        for fault in (None, 'register_read_alias'):
            report = self.report(bool(fault))
            for index in range(len(report['obligations'])):
                changed = copy.deepcopy(report)
                changed['obligations'][index]['solver_result'] = 'unknown'
                with self.subTest(fault=fault, index=index), self.assertRaises(RuntimeError):
                    cpu_registers.validate_report(changed, fault)

    def test_missing_duplicate_unexpected_and_malformed_obligations_rejected(self):
        normal = self.report()
        cases = [normal['obligations'][:-1],
                 normal['obligations'][:-1] + [normal['obligations'][0]],
                 normal['obligations'] + [{'name': 'invented', 'status': 'passed'}],
                 [{}]]
        for rows in cases:
            with self.subTest(rows=rows), self.assertRaises((KeyError, RuntimeError)):
                cpu_registers.validate_report({**normal, 'obligations': rows}, None)

    def test_missing_unrecognized_and_contradictory_solver_results_rejected(self):
        for fault in (None, 'register_read_alias'):
            original = self.report(bool(fault))
            for index, row in enumerate(original['obligations']):
                wrong = 'unsat' if row['solver_result'] == 'sat' else 'sat'
                for result in (None, 'unrecognized', wrong):
                    report = copy.deepcopy(original)
                    if result is None:
                        del report['obligations'][index]['solver_result']
                    else:
                        report['obligations'][index]['solver_result'] = result
                    with self.subTest(fault=fault, name=row['name'], result=result), \
                            self.assertRaises((KeyError, RuntimeError)):
                        cpu_registers.validate_report(report, fault)

    def test_unvalidated_or_wrong_obligation_negative_rejected(self):
        for validation in (False, None, 'true'):
            report = self.report(True)
            micro = next(row for row in report['obligations']
                         if row['name'] == 'microstep_refinement')
            micro['finite']['original_formula_validated'] = validation
            with self.subTest(validation=validation), self.assertRaises(RuntimeError):
                cpu_registers.validate_report(report, 'register_read_alias')
        report = self.report(True)
        other = next(row for row in report['obligations'] if row['name'] == 'reset_binding')
        other.update(status='failed', solver_result='sat')
        with self.assertRaises(RuntimeError):
            cpu_registers.validate_report(report, 'register_read_alias')

    def test_unvalidated_positive_existence_witnesses_rejected(self):
        for fault in (None, 'register_read_alias'):
            original = self.report(bool(fault))
            for index, row in enumerate(original['obligations']):
                if row['status'] != 'passed' or row['solver_result'] != 'sat':
                    continue
                for validation in ('missing_finite', 'missing_flag', False, None, 'true', 1):
                    report = copy.deepcopy(original)
                    changed = report['obligations'][index]
                    if validation == 'missing_finite':
                        del changed['finite']
                    elif validation == 'missing_flag':
                        changed['finite'] = {}
                    else:
                        changed['finite']['original_formula_validated'] = validation
                    with self.subTest(fault=fault, name=row['name'], validation=validation), \
                            self.assertRaises(RuntimeError):
                        cpu_registers.validate_report(report, fault)

    def test_external_solver_and_inconsistent_report_status_rejected(self):
        for fault in (None, 'register_read_alias'):
            report = self.report(bool(fault))
            report['engine_summary']['z3_queries'] = 1
            with self.subTest(fault=fault), self.assertRaises(RuntimeError):
                cpu_registers.validate_report(report, fault)
            report = self.report(bool(fault))
            report['status'] = 'unknown'
            with self.subTest(fault=fault), self.assertRaises(RuntimeError):
                cpu_registers.validate_report(report, fault)


class DUTTrial:
    """Execute lifted DUT IR and compare every retirement with IntegerISA."""
    def __init__(self, test, doc, count, rom, data, seed=0):
        self.test, self.doc, self.count = test, doc, count
        self.rng = random.Random(seed)
        self.commits = 0
        self.opcodes = set()
        self.state = {name: True if ty == 'bool' else BV(ty['bv'], -1)
                      for name, ty in doc['impl']['state'].items()}
        self.oracle = IntegerISA(count, rom, data)
        self.tick(reset=(rom, data))

    def tick(self, *, stall=False, reset=None):
        t, doc, count = self.test, self.doc, self.count
        width = 35 + 2 * (count.bit_length() - 1)
        # Live seeds are noise except on reset. The DUT must use its captured copy.
        rom = [self.rng.getrandbits(width) for _ in range(4)]
        data = [self.rng.getrandbits(32) for _ in range(4)]
        if reset is not None:
            rom, data = reset
        inp = input_record(count, rom, data, stall=stall, reset=reset is not None)
        old = self.state
        commit = outputs(doc['impl'], old, inp)['commit']
        new = evaluate_record(doc['impl'], 'reset' if reset is not None else 'next', old, inp)
        if reset is not None:
            self.oracle = IntegerISA(count, rom, data)
            for key, value in new.items():
                if key.startswith(('rom', 'data')):
                    continue
                t.assertEqual(value, False if doc['impl']['state'][key] == 'bool'
                              else BV(doc['impl']['state'][key]['bv'], 0), key)
            STATS['resets'] += 1
        else:
            t.assertTrue(related(doc, self.oracle.record(), old), 'pre-edge relation')
            t.assertEqual(commit, not stall and old['w_valid'], 'pre-edge commit')
            if stall:
                t.assertEqual(new, old, 'stall must hold the complete DUT state')
                STATS['stalls'] += 1
            elif commit:
                t.assertEqual(old['w_pc'].v, self.oracle.pc, 'retired PC')
                t.assertEqual(old['w_ir'].v, self.oracle.rom[self.oracle.pc], 'retired instruction')
                self.opcodes.add(self.oracle.retire())
                self.commits += 1
                STATS['retirements'] += 1
        self.state = new
        # Check architectural values directly, independently of the binding.
        for key, expected in self.oracle.record().items():
            t.assertEqual(new[key], expected, f'architectural state {key}')
        t.assertTrue(related(doc, self.oracle.record(), new), 'post-edge relation')
        STATS['dut_ticks'] += 1
        return old, new

    def run(self, ticks, *, random_stalls=False):
        for _ in range(ticks):
            self.tick(stall=random_stalls and self.rng.randrange(4) == 0)
        self.test.assertGreater(self.commits, 0, 'trial never retired an instruction')
        return self


class ImportedDUTTests(unittest.TestCase):
    evidence = None
    sizes = COUNTS

    @classmethod
    def setUpClass(cls):
        if cls.evidence is None:
            raise unittest.SkipTest('use --evidence to test compiled Veryl DUT fixtures')
        cls.docs = {}
        for count in cls.sizes:
            folder = cls.evidence / f'cpu_{count}gpr'
            cls.docs[count] = json.loads((folder / 'refinement.json').read_text())

    def test_fixtures_are_the_actual_source_import(self):
        for count, doc in self.docs.items():
            with self.subTest(count=count):
                folder = self.evidence / f'cpu_{count}gpr'
                self.assertEqual((folder / 'pipeline.veryl').read_text(),
                                 cpu_registers.source(count).replace('@IMMHI@', '31'))
                normal = json.loads((folder / 'normal-lift.json').read_text())
                reset = json.loads((folder / 'reset-lift.json').read_text())
                for field in ('next', 'wires', 'outputs'):
                    self.assertEqual(doc['impl'][field], normal[field])
                self.assertEqual(doc['impl']['reset'], reset['next'])
                generated = cpu_registers.model(count)
                self.assertEqual(doc['impl']['state'], generated['impl']['state'])
                self.assertEqual(doc['spec'], generated['spec'])
                self.assertEqual(doc['binding'], generated['binding'])
                compiled = json.loads((folder / 'compiled.json').read_text())
                self.assertEqual(compiled.get('allowed_diagnostics'), [])

    def test_every_register_and_high_selector_bit_aliases(self):
        for count, doc in self.docs.items():
            for rd in range(count):
                with self.subTest(count=count, rd=rd):
                    other = rd ^ (count // 2)
                    rom = [encode(count, 0, rd, 0, 0x80000101),
                           encode(count, 1, other, rd, MASK),
                           encode(count, 2, rd, other, 0xFEDCBA98),
                           encode(count, 7, other, rd, MASK)]
                    trial = DUTTrial(self, doc, count, rom, [17, 29, 41, 53]).run(16)
                    self.assertEqual(trial.commits, 13)
            for low in range(count // 2):
                with self.subTest(count=count, low=low):
                    high = low + count // 2
                    rom = [encode(count, 0, low, 0, 0x13570000 + low),
                           encode(count, 0, high, 0, 0x24680000 + high),
                           encode(count, 1, low, high, 7),
                           encode(count, 2, high, low, 0xFFFFFFFF)]
                    DUTTrial(self, doc, count, rom, [0] * 4).run(20)

    def test_x_forwarding_has_priority_over_w_forwarding(self):
        for count, doc in self.docs.items():
            with self.subTest(count=count):
                last = count - 1
                rom = [encode(count, 0, last, 0, 10),
                       encode(count, 0, last, 0, 20),
                       encode(count, 1, 0, last, 1),
                       encode(count, 7)]
                trial = DUTTrial(self, doc, count, rom, [0] * 4)
                for _ in range(3):
                    trial.tick()
                old, new = trial.tick()
                self.assertTrue(old['w_valid'] and old['x_valid'] and old['d_valid'])
                self.assertEqual(old['w_result'].v, 10)
                self.assertEqual(decode(count, old['x_ir'].v)[1], last)
                self.assertEqual(decode(count, old['d_ir'].v)[2], last)
                self.assertEqual(new['x_operand'].v, 20)
                trial.run(8)
                self.assertEqual(trial.oracle.registers[0], 21)

    def test_register_file_reads_after_bypass_drains(self):
        # Revisit a self-increment after three NOPs: both bypass writers have
        # drained, so every selector must read its actual stored register.
        for count, doc in self.docs.items():
            for register in range(count):
                with self.subTest(count=count, register=register):
                    rom = [encode(count, 1, register, register, 0x10203041),
                           encode(count, 7), encode(count, 7), encode(count, 7)]
                    trial = DUTTrial(self, doc, count, rom, [0] * 4).run(16)
                    self.assertEqual(trial.oracle.registers[register],
                                     (4 * 0x10203041) & MASK)

    def test_high_selector_bit_difference_must_not_forward(self):
        for count, doc in self.docs.items():
            for low in range(count // 2):
                with self.subTest(count=count, low=low):
                    high = low + count // 2
                    dst_low = (low + 1) % (count // 2)
                    dst_high = dst_low + count // 2
                    rom = [encode(count, 0, low, 0, 3),
                           encode(count, 0, high, 0, 11),
                           encode(count, 1, dst_low, low, 1),
                           encode(count, 1, dst_high, high, 2)]
                    trial = DUTTrial(self, doc, count, rom, [0] * 4)
                    for _ in range(3):
                        trial.tick()
                    _old, new = trial.tick()
                    self.assertEqual(new['x_operand'].v, 3,
                                     'different high selector bit is not an X match')
                    trial.run(4)
                    self.assertEqual(trial.oracle.registers[dst_low], 4)
                    self.assertEqual(trial.oracle.registers[dst_high], 13)

    def test_load_use_bubble_and_w_forwarding(self):
        for count, doc in self.docs.items():
            for address in range(4):
                with self.subTest(count=count, address=address):
                    last = count - 1
                    data = [0x80000000, MASK, 0x12345678, 17]
                    rom = [encode(count, 3, last, 0, 0xFFFFFFFC | address),
                           encode(count, 1, 0, last, 1),
                           encode(count, 7), encode(count, 7)]
                    trial = DUTTrial(self, doc, count, rom, data)
                    trial.tick()
                    trial.tick()
                    old, new = trial.tick()
                    self.assertTrue(old['x_valid'] and old['d_valid'])
                    self.assertFalse(new['x_valid'], 'load-use hazard must insert a bubble')
                    self.assertTrue(new['d_valid'])
                    for field in ('d_pc', 'd_ir', 'fetch_pc'):
                        self.assertEqual(new[field], old[field], field)
                    self.assertTrue(new['w_valid'])
                    _old, new = trial.tick()
                    self.assertTrue(new['x_valid'])
                    self.assertEqual(new['x_operand'].v, data[address])
                    trial.run(12)
                    self.assertEqual(trial.oracle.registers[0], (data[address] + 1) & MASK)

    def test_taken_branch_flush_and_not_taken_branch(self):
        for count, doc in self.docs.items():
            for value in (0, 9):
                with self.subTest(count=count, branch_source=value):
                    last = count - 1
                    rom = [encode(count, 0, last, 0, value),
                           encode(count, 4, 0, last, 3),
                           encode(count, 0, 0, 0, 0xBAD),
                           encode(count, 7)]
                    trial = DUTTrial(self, doc, count, rom, [0] * 4)
                    for _ in range(3):
                        trial.tick()
                    old, new = trial.tick()
                    self.assertEqual(decode(count, old['x_ir'].v)[0], 4)
                    self.assertEqual(old['x_operand'].v, value)
                    if value == 0:
                        self.assertFalse(new['x_valid'])
                        self.assertFalse(new['d_valid'])
                        self.assertEqual(new['fetch_pc'].v, 3)
                    else:
                        self.assertTrue(new['x_valid'])
                    trial.run(20)
                    self.assertEqual(trial.oracle.registers[0], 0 if value == 0 else 0xBAD)

    def test_load_to_branch_and_self_branch(self):
        for count, doc in self.docs.items():
            with self.subTest(count=count):
                last = count - 1
                rom = [encode(count, 3, last, 0, 2),
                       encode(count, 4, 0, last, 3),
                       encode(count, 0, 0, 0, 0xBAD),
                       encode(count, 4, 0, last, 3)]
                trial = DUTTrial(self, doc, count, rom, [19, 23, 0, 37]).run(40)
                self.assertEqual(trial.oracle.pc, 3)
                self.assertEqual(trial.oracle.registers[0], 0)
                self.assertGreater(trial.commits, 5)

    def test_stall_hold_and_reset_with_pending_pipeline(self):
        for count, doc in self.docs.items():
            with self.subTest(count=count):
                last = count - 1
                rom = [encode(count, 0, last, 0, MASK),
                       encode(count, 1, 0, last, 2),
                       encode(count, 2, last, 0, 0x1234), encode(count, 7)]
                trial = DUTTrial(self, doc, count, rom, [0] * 4)
                trial.tick(stall=True)  # Empty pipeline.
                trial.tick()
                trial.tick(stall=True)  # D occupied.
                trial.tick()
                trial.tick(stall=True)  # D/X occupied.
                trial.tick()
                for _ in range(4):
                    trial.tick(stall=True)  # D/X/W occupied; must not retire W.
                trial.run(6)
                replacement = [encode(count, 3, 0, 0, 3), encode(count, 7),
                               encode(count, 7), encode(count, 7)]
                data = [11, 22, 33, 0x87654321]
                self.assertTrue(trial.state['w_valid'])
                trial.tick(stall=True, reset=(replacement, data))
                trial.tick(stall=True, reset=(replacement, data))
                trial.run(16)
                self.assertEqual(trial.oracle.registers[0], data[3])
                self.assertEqual(trial.oracle.registers[last], 0)

    def test_seeded_random_programs_stalls_and_midflight_reset(self):
        all_opcodes = set()
        for count, doc in self.docs.items():
            for seed in range(16):
                with self.subTest(count=count, seed=seed):
                    rng = random.Random(0xC0FFEE + seed + 100 * count)
                    selectors = [0, count // 2 - 1, count // 2, count - 1]
                    def program():
                        return [encode(count, rng.randrange(8),
                                       rng.choice(selectors) if rng.randrange(2) else rng.randrange(count),
                                       rng.choice(selectors) if rng.randrange(2) else rng.randrange(count),
                                       rng.getrandbits(32)) for _ in range(4)]
                    data = [rng.getrandbits(32) for _ in range(4)]
                    trial = DUTTrial(self, doc, count, program(), data, seed=seed)
                    for tick in range(96):
                        reset = (program(), [rng.getrandbits(32) for _ in range(4)]) if tick in (29, 63) else None
                        trial.tick(stall=rng.randrange(4) == 0, reset=reset)
                    self.assertGreater(trial.commits, 8)
                    all_opcodes.update(trial.opcodes)
        self.assertEqual(all_opcodes, set(range(8)), 'seeded regression must retire every opcode')


def main():
    parser = argparse.ArgumentParser(description=__doc__, add_help=False)
    parser.add_argument('--evidence', type=Path)
    parser.add_argument('--sizes', nargs='+', choices=COUNTS, type=int, default=COUNTS)
    args, unittest_args = parser.parse_known_args()
    ImportedDUTTests.evidence = args.evidence
    ImportedDUTTests.sizes = tuple(args.sizes)
    result = unittest.main(argv=[sys.argv[0], *unittest_args], exit=False)
    if result.result.wasSuccessful():
        print(json.dumps({'scope': 'finite concrete regression, not universal proof',
                          **dict(sorted(STATS.items()))}, sort_keys=True))
    raise SystemExit(not result.result.wasSuccessful())


if __name__ == '__main__':
    main()
