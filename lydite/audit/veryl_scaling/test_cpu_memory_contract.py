#!/usr/bin/env python3
"""Independent finite regressions for the CPU/read-memory contract boundary.

Compiler-free schema, integer-ISA, and strict-result-gate tests:
    python3 -m unittest audit.veryl_scaling.test_cpu_memory_contract

Also execute actual compiled-and-lifted Veryl modules (no solver invoked):
    python3 audit/veryl_scaling/test_cpu_memory_contract.py --evidence /tmp/run
    python3 audit/veryl_scaling/test_cpu_memory_contract.py --evidence /tmp/run --sizes 4

Requested evidence must contain cpu/ and memory_{4,16,64}/ machine.json and
source/lift sidecars. Missing requested evidence fails rather than skips. These
finite interpreter runs are not universal proofs and do not read proof verdicts.
"""
import argparse
from collections import Counter, deque
import copy
import json
from pathlib import Path
import random
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from audit.interpreter import BV, evaluate, evaluate_record, outputs, related
from audit.veryl_scaling import cpu_memory_contract as contract

COUNTS = (4, 16, 64)
MASK = (1 << 32) - 1
STATS = Counter()


def encode(opcode, rd=0, rs=0, immediate=0):
    """The documented 41-bit encoding, independent of generator helpers."""
    if not (0 <= opcode < 8 and 0 <= rd < 8 and 0 <= rs < 8):
        raise ValueError('instruction field out of range')
    return (opcode << 38) | (rd << 35) | (rs << 32) | (immediate & MASK)


def decode(instruction):
    return instruction >> 38, (instruction >> 35) & 7, (instruction >> 32) & 7, instruction & MASK


def read_word(words, address):
    return words[address] if 0 <= address < len(words) else 0


class IntegerISA:
    """Plain integer retirement; no generated spec, binding, or DUT expressions."""
    def __init__(self, registers=None, pc=0):
        self.registers = list(registers) if registers is not None else [0] * 8
        self.pc = pc

    def retire(self, instruction, load_response=0):
        opcode, rd, rs, immediate = decode(instruction)
        source = self.registers[rs]
        self.pc = (self.pc + 1) & 63
        if opcode == 0:
            self.registers[rd] = immediate
        elif opcode == 1:
            self.registers[rd] = (source + immediate) & MASK
        elif opcode == 2:
            self.registers[rd] = source ^ immediate
        elif opcode == 3:
            self.registers[rd] = load_response & MASK
        elif opcode == 4 and source == 0:
            self.pc = immediate & 63
        return opcode

    def record(self):
        return {'pc': BV(6, self.pc),
                **{f'r{k}': BV(32, value) for k, value in enumerate(self.registers)}}


def blank_state(machine, ones=False):
    return {name: ones if ty == 'bool' else BV(ty['bv'], -int(ones))
            for name, ty in machine['state'].items()}


def cpu_stub():
    """Shape-only fixture: never used as a substitute for the imported DUT."""
    state = contract.cpu_state()
    return {'state': state, 'wires': {},
            'reset': {k: False if ty == 'bool' else ['bv', ty['bv'], 0] for k, ty in state.items()},
            'next': {k: 's.' + k for k in state},
            'outputs': {'imem_address': 's.fetch_pc', 'dmem_address': ['extract', 5, 0, 's.x_ir'],
                        'imem_valid': True, 'dmem_valid': False, 'commit': 's.w_valid'}}


def memory_stub(count):
    state = contract.immutable(count)
    def mux(kind, width, address):
        expression = ['bv', width, 0]
        for index in reversed(range(count)):
            expression = ['ite', ['eq', 'i.' + address, ['bv', 6, index]],
                          's.' + kind + str(index), expression]
        return expression
    return {'state': state, 'wires': {}, 'reset': {k: 'i.seed_' + k for k in state},
            'next': {k: 's.' + k for k in state},
            'outputs': {'imem_response': mux('rom', 41, 'imem_address'),
                        'dmem_response': mux('data', 32, 'dmem_address')}}


def memory_inputs(rom, data, ia=0, da=0, reset=False):
    return {'rst': reset, 'imem_address': BV(6, ia), 'dmem_address': BV(6, da),
            **{f'seed_rom{k}': BV(41, v) for k, v in enumerate(rom)},
            **{f'seed_data{k}': BV(32, v) for k, v in enumerate(data)}}


def contract_environment(state, inputs, next_state=None):
    env = {**{'s.' + k: v for k, v in state.items()},
           **{'i.' + k: v for k, v in inputs.items()},
           **{'o.' + k: v for k, v in state.items()}}
    if next_state is not None:
        env.update({'n.' + k: v for k, v in next_state.items()})
    return env


class GenerationTests(unittest.TestCase):
    def test_cpu_contract_has_no_memory_or_seed_input(self):
        raw = cpu_stub()
        before = copy.deepcopy(raw)
        doc = contract.cpu_contract(raw)
        self.assertEqual(raw, before, 'observer construction must not mutate imported machine')
        self.assertEqual(doc['version'], 4)
        leaf = doc['specs']['Contract']
        self.assertEqual(leaf['inputs'], {'rst': 'bool', 'stall': 'bool',
                                         'imem_response': {'bv': 41}, 'dmem_response': {'bv': 32}})
        self.assertEqual(set(leaf['operations']), {'tick'})
        self.assertEqual(doc['implementation']['operations'], {'tick': True})
        self.assertEqual(set(leaf['outputs']), {'pc', *[f'r{k}' for k in range(8)]})
        self.assertEqual(set(raw['state']), set(contract.cpu_state()))
        self.assertFalse(any(k.startswith(('rom', 'data', 'seed_')) for k in leaf['state']))
        for field in ('pc', 'fetch_pc', 'd_pc', 'x_pc', 'w_pc', 'w_next_pc',
                      'obs_imem_address', 'obs_dmem_address'):
            self.assertEqual(leaf['state'][field], {'bv': 6})
        for field in ('d_ir', 'x_ir', 'w_ir'):
            self.assertEqual(leaf['state'][field], {'bv': 41})
        self.assertEqual(leaf['state']['obs_load'], {'bv': 32})
        binding = doc['implementation']['binding']['states']['contract']
        self.assertEqual(binding, {k: 's.' + k for k in leaf['state']})
        self.assertNotIn('assumptions', leaf)
        self.assertNotIn('hold_when', doc)

    def test_observers_are_read_only_and_sample_actual_outputs(self):
        raw = cpu_stub()
        doc = contract.cpu_contract(raw)
        augmented = doc['implementation']
        for part in ('next', 'reset'):
            self.assertEqual({k: augmented[part][k] for k in raw['state']}, raw[part])
        self.assertEqual(augmented['wires'], raw['wires'])
        self.assertEqual(augmented['next']['obs_imem_address'], raw['outputs']['imem_address'])
        self.assertEqual(augmented['next']['obs_dmem_address'], raw['outputs']['dmem_address'])
        state = blank_state(augmented)
        state.update(fetch_pc=BV(6, 61), x_ir=BV(41, encode(3, immediate=37)), obs_load=BV(32, 9))
        inp = {'rst': False, 'stall': True, 'imem_response': BV(41, 0), 'dmem_response': BV(32, 123)}
        new = evaluate_record(augmented, 'next', state, inp)
        self.assertEqual(new['obs_imem_address'], BV(6, 61), 'port monitors still sample during stall')
        self.assertEqual(new['obs_dmem_address'], BV(6, 37))
        self.assertEqual(new['obs_load'], BV(32, 9), 'historical load ghost freezes during stall')

    def test_observer_namespace_dependencies_are_rejected_everywhere(self):
        for part in ('next', 'reset', 'wires', 'outputs', 'state'):
            machine = cpu_stub()
            if part == 'state':
                machine[part]['obs_backdoor'] = {'bv': 6}
            else:
                machine[part][next(iter(machine[part]), 'backdoor')] = 's.obs_backdoor'
            with self.subTest(part=part), self.assertRaisesRegex(ValueError, 'observer namespace'):
                contract.cpu_contract(machine)
        machine = cpu_stub()
        machine['wires']['indirect'] = ['ite', True, ['add', 's.obs_hidden', ['bv', 6, 1]], ['bv', 6, 0]]
        with self.assertRaises(ValueError):
            contract.cpu_contract(machine)

    def test_memory_shapes_and_unmapped_lookup_for_every_address(self):
        for count in COUNTS:
            doc = contract.memory_contract(count, memory_stub(count))
            leaf = doc['specs']['Contract']
            self.assertEqual(len(contract.immutable(count)), 2 * count)
            self.assertEqual(len(leaf['state']), 4 * count + 2)
            self.assertEqual(leaf['inputs']['imem_address'], {'bv': 6})
            self.assertEqual(leaf['inputs']['dmem_address'], {'bv': 6})
            self.assertEqual(set(leaf['outputs']), {'obs_imem_response', 'obs_dmem_response'})
            for kind, width in (('rom', 41), ('data', 32)):
                words = [((index + 1) * 0x10203041) & ((1 << width) - 1) for index in range(count)]
                env = {f's.{kind}{k}': BV(width, v) for k, v in enumerate(words)}
                expression = contract.lookup(kind, count, 'i.addr')
                for addr in range(64):
                    with self.subTest(count=count, kind=kind, address=addr):
                        self.assertEqual(evaluate(expression, {**env, 'i.addr': BV(6, addr)}),
                                         BV(width, read_word(words, addr)))
                        STATS['lookup_cases'] += 1
        for count in (0, 1, 8, 32, 65):
            with self.subTest(count=count), self.assertRaises(ValueError):
                contract.memory_source(count)

    def test_integer_isa_against_generated_architecture_all_fields(self):
        for count in COUNTS:
            doc = contract.architectural_composition(count, cpu_stub(), memory_stub(count))
            self.assertEqual(doc['spec']['state']['pc'], {'bv': 6})
            self.assertEqual(doc['inputs'], {'rst': 'bool', 'stall': 'bool',
                **{'seed_' + k: ty for k, ty in contract.immutable(count).items()}})
            data = [((k + 1) * 0x10203041) & MASK for k in range(count)]
            registers = [0, MASK, 0x80000000, 7, 0, 17, MASK - 1, 0x13579BDF]
            # Exhaust every source/destination pair, all eight opcodes, low/high
            # address collisions, wrapping arithmetic, and PC 63 -> 0.
            for rd in range(8):
                for rs in range(8):
                    for opcode in range(8):
                        for immediate in (0, 3, 4, 15, 16, 63, 0x80000000, MASK):
                            instruction = encode(opcode, rd, rs, immediate)
                            self.assertEqual(decode(instruction), (opcode, rd, rs, immediate))
                            pc = (rd * 8 + rs) % count
                            rom = [encode(7)] * count
                            rom[pc] = instruction
                            oracle = IntegerISA(registers, pc)
                            state = {**oracle.record(), **{f'rom{k}': BV(41, v) for k, v in enumerate(rom)},
                                     **{f'data{k}': BV(32, v) for k, v in enumerate(data)}}
                            new = evaluate_record(doc['spec'], 'next', state, {})
                            oracle.retire(instruction, read_word(data, immediate & 63))
                            self.assertEqual({k: new[k] for k in oracle.record()}, oracle.record())
                            self.assertEqual({k: new[k] for k in contract.immutable(count)},
                                             {k: state[k] for k in contract.immutable(count)})
                            STATS['integer_isa_steps'] += 1
            # Unmapped instruction fetch returns the zero MOVI encoding, not NOP.
            for pc in range(count, 64):
                oracle = IntegerISA([17] * 8, pc)
                state = {**oracle.record(), **{k: BV(ty['bv'], 0) for k, ty in contract.immutable(count).items()}}
                new = evaluate_record(doc['spec'], 'next', state, {})
                oracle.retire(0)
                self.assertEqual({k: new[k] for k in oracle.record()}, oracle.record())
            oracle = IntegerISA(registers, 63)
            state = {**oracle.record(), **{k: BV(ty['bv'], 0) for k, ty in contract.immutable(count).items()}}
            self.assertEqual(evaluate_record(doc['spec'], 'next', state, {})['pc'], BV(6, 0))

    def test_composition_rejects_response_to_address_feedback(self):
        for port in ('imem_address', 'dmem_address'):
            for indirect in (False, True):
                machine = cpu_stub()
                expression = ['extract', 5, 0, 'i.dmem_response']
                if indirect:
                    machine['wires']['feedback'] = expression
                    expression = 'w.feedback'
                machine['outputs'][port] = expression
                with self.subTest(port=port, indirect=indirect), self.assertRaisesRegex(ValueError, 'state-only'):
                    contract.architectural_composition(4, machine, memory_stub(4))


class ReportGateTests(unittest.TestCase):
    def report(self, fault=None):
        bad = ('binding_reset_establishes_product' if fault == 'wrong_reset'
               else 'binding_product_preservation') if fault else None
        obligations = []
        for name in ('binding_reset_nonempty', 'binding_reset_establishes_product', 'binding_product_preservation'):
            result = 'sat' if name in ('binding_reset_nonempty', bad) else 'unsat'
            obligations.append({'name': name, 'status': 'counterexample' if name == bad else 'passed',
                'solver_result': result, 'backend': 'finite_bv',
                'finite': {'original_formula_validated': True}})
        examples = [{'target': 'Contract', 'example': name, 'expect': expect, 'admitted': expect == 'positive', 'status': 'passed',
            'evidence': {'solver_result': result, 'backend': 'finite_bv',
                         'finite': {'original_formula_validated': True}}}
            for name, expect, result in (('accept', 'positive', 'sat'), ('reject', 'negative', 'unsat'))]
        return {'status': 'implementation_binding_failed' if fault else 'spec_examples_and_binding_verified',
                'implementation_binding': {'status': 'failed' if fault else 'verified', 'obligations': obligations},
                'examples': examples, 'engine_summary': {'z3_queries': 0}}

    def test_exact_positive_and_each_negative_kind_accepted(self):
        for fault in (None, 'load_response', 'wrong_reset'):
            self.assertEqual(len(contract.validate_scoped(self.report(fault), fault)), 3)

    def test_missing_duplicate_extra_or_malformed_obligations_rejected(self):
        report = self.report()
        rows = report['implementation_binding']['obligations']
        for changed in ([], rows[:-1], rows + [rows[0]], [rows[0], rows[0], rows[2]], [{}] * 3):
            candidate = copy.deepcopy(report)
            candidate['implementation_binding']['obligations'] = changed
            with self.subTest(rows=changed), self.assertRaises((KeyError, RuntimeError)):
                contract.validate_scoped(candidate)
        for part in ('implementation_binding', 'examples'):
            candidate = copy.deepcopy(report)
            del candidate[part]
            with self.subTest(part=part), self.assertRaises((KeyError, RuntimeError)):
                contract.validate_scoped(candidate)

    def test_unknown_missing_wrong_verdict_and_status_rejected(self):
        for fault in (None, 'load_response', 'wrong_reset'):
            original = self.report(fault)
            for index in range(3):
                wanted = original['implementation_binding']['obligations'][index]['solver_result']
                for verdict in (None, 'unknown', 'invalid', 'sat' if wanted == 'unsat' else 'unsat'):
                    candidate = copy.deepcopy(original)
                    row = candidate['implementation_binding']['obligations'][index]
                    if verdict is None:
                        del row['solver_result']
                    else:
                        row['solver_result'] = verdict
                    with self.subTest(fault=fault, index=index, verdict=verdict), self.assertRaises(RuntimeError):
                        contract.validate_scoped(candidate, fault)
                candidate = copy.deepcopy(original)
                candidate['implementation_binding']['obligations'][index]['status'] = 'unknown'
                with self.assertRaises(RuntimeError):
                    contract.validate_scoped(candidate, fault)
            candidate = copy.deepcopy(original)
            candidate['implementation_binding']['status'] = 'unknown'
            with self.assertRaises(RuntimeError):
                contract.validate_scoped(candidate, fault)

    def test_unvalidated_sat_and_external_backend_rejected(self):
        for fault in (None, 'load_response', 'wrong_reset'):
            original = self.report(fault)
            for index, row in enumerate(original['implementation_binding']['obligations']):
                for backend in (None, 'z3', 'unknown'):
                    candidate = copy.deepcopy(original)
                    candidate['implementation_binding']['obligations'][index]['backend'] = backend
                    with self.subTest(fault=fault, index=index, backend=backend), self.assertRaises(RuntimeError):
                        contract.validate_scoped(candidate, fault)
                if row['solver_result'] != 'sat':
                    continue
                for flag in (None, False, 'true', 1):
                    candidate = copy.deepcopy(original)
                    candidate['implementation_binding']['obligations'][index]['finite'] = {'original_formula_validated': flag}
                    with self.subTest(fault=fault, index=index, flag=flag), self.assertRaises(RuntimeError):
                        contract.validate_scoped(candidate, fault)

    def test_example_missing_unknown_wrong_verdict_and_validation_rejected(self):
        original = self.report()
        for index, row in enumerate(original['examples']):
            for verdict in (None, 'unknown', 'invalid', 'sat' if row['expect'] == 'negative' else 'unsat'):
                candidate = copy.deepcopy(original)
                q = candidate['examples'][index]['evidence']
                if verdict is None:
                    del q['solver_result']
                else:
                    q['solver_result'] = verdict
                with self.subTest(index=index, verdict=verdict), self.assertRaises(RuntimeError):
                    contract.validate_scoped(candidate)
            for field, value in (('backend', None), ('backend', 'z3'), ('finite', {}),
                                 ('finite', {'original_formula_validated': False}),
                                 ('finite', {'original_formula_validated': 1})):
                if field == 'finite' and row['expect'] == 'negative':
                    continue
                candidate = copy.deepcopy(original)
                candidate['examples'][index]['evidence'][field] = value
                with self.subTest(index=index, field=field, value=value), self.assertRaises(RuntimeError):
                    contract.validate_scoped(candidate)
            candidate = copy.deepcopy(original)
            del candidate['examples'][index]['evidence']
            with self.assertRaises(RuntimeError):
                contract.validate_scoped(candidate)
        for examples in ([], [{'status': 'failed'}], [{'status': 'unknown'}]):
            with self.assertRaises(RuntimeError):
                contract.validate_scoped({**original, 'examples': examples})

    def test_example_missing_or_unknown_expectation_is_not_negative(self):
        for expectation in (None, '', 'unknown', 'typo'):
            candidate = self.report()
            if expectation is None:
                del candidate['examples'][1]['expect']
            else:
                candidate['examples'][1]['expect'] = expectation
            with self.subTest(expectation=expectation), self.assertRaises(RuntimeError):
                contract.validate_scoped(candidate)


    def test_top_level_status_and_example_polarity_duplicates_rejected(self):
        for fault in (None, 'load_response', 'wrong_reset'):
            for status in (None, 'unknown', 'counterexample', 'passed'):
                candidate = self.report(fault)
                if status is None:
                    del candidate['status']
                else:
                    candidate['status'] = status
                with self.subTest(fault=fault, status=status), self.assertRaises(RuntimeError):
                    contract.validate_scoped(candidate, fault)
        original = self.report()
        for examples in ([original['examples'][0]], [original['examples'][1]],
                         original['examples'] + [original['examples'][0]]):
            with self.subTest(examples=examples), self.assertRaises(RuntimeError):
                contract.validate_scoped({**original, 'examples': examples})

    def test_document_gate_requires_exact_declared_examples(self):
        # Stub only the checker process, not the validation function under test.
        # An otherwise valid subset/rename must not masquerade as full evidence.
        doc = contract.cpu_contract(cpu_stub())
        report = self.report()
        prototypes = {example['expect']: example for example in report['examples']}
        report['examples'] = [{**copy.deepcopy(prototypes[example['expect']]), 'example': name}
            for name, example in doc['specs']['Contract']['examples'].items()]
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            with patch.object(contract, 'execute', return_value=(report, 0.0)):
                self.assertTrue(contract.check_document(doc, out / 'good', 'unused-checker')['correct_outcome'])
            missing = copy.deepcopy(report)
            # Preserve both polarities so exact-set validation is the deciding gate.
            missing['examples'] = [next(e for e in missing['examples'] if e['expect'] == expectation)
                                   for expectation in ('positive', 'negative')]
            renamed = copy.deepcopy(report)
            renamed['examples'][0]['example'] = 'undeclared_example'
            cases = [renamed]
            if len(missing['examples']) < len(report['examples']):
                cases.append(missing)
            for index, candidate in enumerate(cases):
                with patch.object(contract, 'execute', return_value=(candidate, 0.0)):
                    with self.subTest(index=index), self.assertRaisesRegex(RuntimeError, 'example results'):
                        contract.check_document(doc, out / f'bad_{index}', 'unused-checker')


class CPUTrial:
    """Execute imported CPU with arbitrary responses and an independent FIFO ISA.

    The FIFO holds accepted fetches, removes killed younger fetches at branches,
    and remembers each load's actual X-stage response until its W retirement.
    It does not evaluate the generated ISA when deciding architectural values.
    """
    def __init__(self, test, machine, rom=None, data=None, seed=0):
        self.test, self.machine, self.rom, self.data = test, machine, rom, data
        self.rng = random.Random(seed)
        self.doc = contract.cpu_contract(machine)
        self.monitored = self.doc['implementation']
        self.state = blank_state(machine, ones=True)
        self.observed = blank_state(self.monitored, ones=True)
        self.pending = deque()
        self.oracle = IntegerISA()
        self.commits = 0
        self.opcodes = set()
        self.tick(reset=True)

    def tick(self, *, stall=False, reset=False, instruction=None, response=None):
        t, old = self.test, self.state
        if instruction is None:
            instruction = read_word(self.rom, old['fetch_pc'].v) if self.rom is not None else self.rng.getrandbits(41)
        if response is None:
            response = read_word(self.data, old['x_ir'].v & 63) if self.data is not None else self.rng.getrandbits(32)
        inp = {'rst': reset, 'stall': stall, 'imem_response': BV(41, instruction), 'dmem_response': BV(32, response)}
        port = outputs(self.machine, old, inp)
        dop, _drd, drs, _di = decode(old['d_ir'].v)
        xop, xrd, xrs, _xi = decode(old['x_ir'].v)
        hazard = old['d_valid'] and dop in (1, 2, 4) and old['x_valid'] and xop == 3 and xrd == drs
        taken = old['x_valid'] and xop == 4 and old['x_operand'].v == 0
        t.assertEqual(port, {'imem_address': BV(6, old['fetch_pc'].v),
            'dmem_address': BV(6, old['x_ir'].v & 63), 'imem_valid': not stall and not hazard and not taken,
            'dmem_valid': not stall and old['x_valid'] and xop == 3, 'commit': not stall and old['w_valid']})
        new = evaluate_record(self.machine, 'reset' if reset else 'next', old, inp)
        observed = evaluate_record(self.monitored, 'reset' if reset else 'next', self.observed, inp)
        if reset:
            self.pending.clear()
            self.oracle = IntegerISA()
            t.assertEqual(new, blank_state(self.machine), 'CPU reset must ignore prestate and both responses')
            t.assertEqual(observed, blank_state(self.monitored))
            STATS['cpu_resets'] += 1
        else:
            if stall:
                t.assertEqual(new, old, 'every DUT state cell must freeze during stall')
                STATS['cpu_stalls'] += 1
            else:
                if port['commit']:
                    t.assertTrue(self.pending, 'retirement without accepted fetch')
                    packet = self.pending.popleft()
                    t.assertEqual((old['w_pc'].v, old['w_ir'].v), (packet['pc'], packet['ir']))
                    t.assertEqual(packet['pc'], self.oracle.pc, 'ISA retirement PC')
                    if decode(packet['ir'])[0] == 3:
                        t.assertIn('load', packet, 'load retirement lacks historical response')
                    self.opcodes.add(self.oracle.retire(packet['ir'], packet.get('load', 0)))
                    self.commits += 1
                    STATS['cpu_retirements'] += 1
                if old['x_valid']:
                    t.assertTrue(self.pending)
                    t.assertEqual((self.pending[0]['pc'], self.pending[0]['ir']),
                                  (old['x_pc'].v, old['x_ir'].v))
                    if xop in (1, 2, 4):
                        t.assertEqual(old['x_operand'].v, self.oracle.registers[xrs], 'executing ISA operand')
                    if xop == 3:
                        self.pending[0]['load'] = response & MASK
                        STATS['cpu_loads'] += 1
                if taken:
                    t.assertTrue(self.pending)
                    killed = len(self.pending) - 1
                    self.pending = deque([self.pending[0]])
                    STATS['killed_fetches'] += killed
                if port['imem_valid']:
                    self.pending.append({'pc': port['imem_address'].v, 'ir': instruction})
                if hazard:
                    for field in ('d_pc', 'd_ir', 'fetch_pc'):
                        t.assertEqual(new[field], old[field])
                    t.assertFalse(new['x_valid'])
                    STATS['load_use_bubbles'] += 1
                if taken:
                    t.assertFalse(new['d_valid'])
                    t.assertFalse(new['x_valid'])
                    t.assertEqual(new['fetch_pc'].v, old['x_ir'].v & 63)
                    STATS['taken_branches'] += 1
            leaf = self.doc['specs']['Contract']
            t.assertTrue(evaluate(leaf['operations']['tick'], contract_environment(self.observed, inp, observed)),
                         'independent run violates generated tick contract')
            for name, value in port.items():
                t.assertEqual(observed['obs_' + name], BV(1, int(value)) if type(value) is bool else value)
        for name, value in self.oracle.record().items():
            t.assertEqual(new[name], value, f'architectural state {name}')
        for name in new:
            t.assertEqual(observed[name], new[name], f'observer noninterference {name}')
        t.assertTrue(evaluate(self.doc['specs']['Contract']['invariant'], contract_environment(observed, inp)))
        self.state, self.observed = new, observed
        STATS['cpu_ticks'] += 1
        return old, new

    def run(self, ticks, random_stalls=False):
        for _ in range(ticks):
            self.tick(stall=random_stalls and self.rng.randrange(4) == 0)
        return self


class ImportedMachineTests(unittest.TestCase):
    evidence = None
    sizes = COUNTS

    @classmethod
    def setUpClass(cls):
        if cls.evidence is None:
            raise unittest.SkipTest('use --evidence to execute compiled-and-lifted Veryl modules')
        cls.cpu = json.loads((cls.evidence / 'cpu/machine.json').read_text())
        cls.memories = {count: json.loads((cls.evidence / f'memory_{count}/machine.json').read_text())
                        for count in cls.sizes}

    def test_evidence_is_actual_source_import(self):
        for name, machine, source in [('cpu', self.cpu, contract.source()),
                *[(f'memory_{count}', m, contract.memory_source(count)) for count, m in self.memories.items()]]:
            with self.subTest(module=name):
                folder = self.evidence / name
                self.assertEqual((folder / 'source.veryl').read_text(), source)
                normal = json.loads((folder / 'normal-lift.json').read_text())
                reset = json.loads((folder / 'reset-lift.json').read_text())
                for part in ('next', 'wires', 'outputs'):
                    self.assertEqual(machine[part], normal[part])
                self.assertEqual(machine['reset'], reset['next'])
                self.assertEqual(json.loads((folder / 'compiled.json').read_text()).get('allowed_diagnostics'), [])
        self.assertEqual(self.cpu['state'], contract.cpu_state())
        for count, machine in self.memories.items():
            self.assertEqual(machine['state'], contract.immutable(count))

    def test_all_registers_opcodes_forwarding_and_pc_wrap(self):
        for destination in range(8):
            source = destination ^ 4
            rom = [encode(7)] * 64
            rom[:8] = [encode(0, destination, immediate=MASK), encode(1, source, destination, 2),
                encode(2, destination, source, 0x80000000), encode(3, source, immediate=63),
                encode(4, rs=source, immediate=17), encode(5, destination, source, MASK),
                encode(6, destination, source, MASK), encode(7, destination, source, MASK)]
            data = list(range(64))
            trial = CPUTrial(self, self.cpu, rom, data).run(150)
            self.assertEqual(trial.opcodes, set(range(8)))
            self.assertGreater(trial.commits, 128)
        # Two simultaneous writers to the same GPR: the younger X value wins.
        rom = [encode(0, 7, immediate=10), encode(0, 7, immediate=20), encode(1, 0, 7, 1)] + [encode(7)] * 61
        trial = CPUTrial(self, self.cpu, rom, [0] * 64).run(12)
        self.assertEqual(trial.oracle.registers[0], 21)

    def test_load_remembers_historical_response_across_stalls(self):
        for address in (0, 3, 4, 15, 16, 37, 63):
            rom = [encode(3, 7, immediate=0xFFFFFFC0 | address), encode(1, 0, 7, 1)] + [encode(7)] * 62
            trial = CPUTrial(self, self.cpu, rom)
            trial.tick(response=11)
            trial.tick(response=22)
            old, new = trial.tick(response=0x12345678)
            self.assertEqual(old['x_ir'].v & 63, address)
            self.assertEqual(new['w_result'], BV(32, 0x12345678))
            self.assertFalse(new['x_valid'])
            for value in (0, MASK, 0x87654321):
                trial.tick(stall=True, response=value, instruction=encode(0, 7, immediate=0xBAD))
                self.assertEqual(trial.observed['obs_load'], BV(32, 0x12345678))
            trial.tick(response=0xDEADBEEF)
            self.assertEqual(trial.oracle.registers[7], 0x12345678)
            trial.run(8)
            self.assertEqual(trial.oracle.registers[0], 0x12345679)

    def test_taken_not_taken_load_branch_and_killed_fetches(self):
        for value in (0, 9):
            rom = [encode(0, 7, immediate=value), encode(4, rs=7, immediate=37),
                   encode(0, 0, immediate=0xBAD), encode(0, 1, immediate=0xBAD)] + [encode(7)] * 60
            trial = CPUTrial(self, self.cpu, rom, [0] * 64).run(24)
            self.assertEqual(trial.oracle.registers[0], 0 if value == 0 else 0xBAD)
            self.assertEqual(trial.oracle.registers[1], 0 if value == 0 else 0xBAD)
        rom = [encode(3, 7, immediate=63), encode(4, rs=7, immediate=63),
               encode(0, 0, immediate=0xBAD)] + [encode(7)] * 60 + [encode(4, rs=7, immediate=63)]
        trial = CPUTrial(self, self.cpu, rom, [0] * 64).run(48)
        self.assertEqual(trial.oracle.pc, 63)
        self.assertEqual(trial.oracle.registers[0], 0)
        self.assertGreater(trial.commits, 8)

    def test_arbitrary_changing_responses_stalls_and_midflight_reset(self):
        seen = set()
        for seed in range(8):
            trial = CPUTrial(self, self.cpu, seed=seed)
            rng = random.Random(0xC0FFEE + seed)
            for tick in range(100):
                trial.tick(stall=rng.randrange(4) == 0, reset=tick in (31, 67),
                           instruction=rng.getrandbits(41), response=rng.getrandbits(32))
            seen.update(trial.opcodes)
            self.assertGreater(trial.commits, 15)
        self.assertEqual(seen, set(range(8)))

    def test_memory_all_addresses_seed_changes_repeated_reads_and_resets(self):
        for count, raw in self.memories.items():
            doc = contract.memory_contract(count, raw)
            monitored = doc['implementation']
            state, observed = blank_state(raw, True), blank_state(monitored, True)
            for reset_number in range(2):
                rom = [encode((k + reset_number) & 7, k & 7, (k >> 3) & 7, MASK - k) for k in range(count)]
                data = [((k + 1) * 0x10203041 + reset_number) & MASK for k in range(count)]
                inp = memory_inputs(rom, data, reset=True)
                state = evaluate_record(raw, 'reset', state, inp)
                observed = evaluate_record(monitored, 'reset', observed, inp)
                self.assertEqual(state, {**{f'rom{k}': BV(41, v) for k, v in enumerate(rom)},
                                         **{f'data{k}': BV(32, v) for k, v in enumerate(data)}})
                rng = random.Random(count + reset_number)
                for address in range(64):
                    for repeat in range(2):
                        ia, da = address, (address * 17 + 3) & 63
                        inp = memory_inputs([rng.getrandbits(41) for _ in range(count)],
                                            [rng.getrandbits(32) for _ in range(count)], ia, da)
                        response = outputs(raw, state, inp)
                        self.assertEqual(response, {'imem_response': BV(41, read_word(rom, ia)),
                                                    'dmem_response': BV(32, read_word(data, da))})
                        new = evaluate_record(raw, 'next', state, inp)
                        next_observed = evaluate_record(monitored, 'next', observed, inp)
                        self.assertEqual(new, state, 'live seed inputs cannot recapture immutable memory')
                        for key, value in response.items():
                            self.assertEqual(next_observed['obs_' + key], value)
                        self.assertTrue(evaluate(doc['specs']['Contract']['operations']['tick'],
                                                contract_environment(observed, inp, next_observed)))
                        self.assertTrue(evaluate(doc['specs']['Contract']['invariant'],
                                                contract_environment(next_observed, inp)))
                        for key, value in state.items():
                            self.assertEqual(next_observed[key], value)
                            self.assertEqual(next_observed['obs_frozen_' + key], value)
                        state, observed = new, next_observed
                        STATS['memory_reads'] += 1

    def test_actual_composed_modules_against_integer_isa(self):
        for count, memory in self.memories.items():
            doc = contract.architectural_composition(count, self.cpu, memory)
            machine = doc['impl']
            rng = random.Random(0xBAD5EED + count)
            state = blank_state(machine, True)
            oracle = IntegerISA()
            commits = 0
            for tick in range(160):
                reset = tick in (0, 73)
                if reset:
                    rom = [encode(3, 7, immediate=63), encode(1, 0, 7, 1),
                           encode(2, 1, 0, MASK), encode(4, rs=2, immediate=0)]
                    rom += [encode(rng.randrange(8), rng.randrange(8), rng.randrange(8), rng.getrandbits(32))
                            for _ in range(count - 4)]
                    data = [rng.getrandbits(32) for _ in range(count)]
                seeds_rom = rom if reset else [rng.getrandbits(41) for _ in range(count)]
                seeds_data = data if reset else [rng.getrandbits(32) for _ in range(count)]
                inp = {**memory_inputs(seeds_rom, seeds_data, reset=reset), 'stall': rng.randrange(4) == 0}
                old = state
                new = evaluate_record(machine, 'reset' if reset else 'next', old, inp)
                if reset:
                    oracle = IntegerISA()
                else:
                    commit = outputs(machine, old, inp)['commit']
                    self.assertEqual(commit, not inp['stall'] and old['w_valid'])
                    if inp['stall']:
                        self.assertEqual(new, old)
                    if commit:
                        instruction = read_word(rom, oracle.pc)
                        self.assertEqual(old['w_pc'].v, oracle.pc)
                        self.assertEqual(old['w_ir'].v, instruction)
                        oracle.retire(instruction, read_word(data, instruction & 63))
                        commits += 1
                expected = {**oracle.record(), **{f'rom{k}': BV(41, v) for k, v in enumerate(rom)},
                            **{f'data{k}': BV(32, v) for k, v in enumerate(data)}}
                self.assertEqual({k: new[k] for k in expected}, expected)
                self.assertTrue(related(doc, expected, new))
                state = new
                STATS['composition_ticks'] += 1
            self.assertGreater(commits, 30)


    def test_composed_fetch_unmapped_zero_and_fixed64_wrap(self):
        for count, memory in self.memories.items():
            doc = contract.architectural_composition(count, self.cpu, memory)
            machine = doc['impl']
            rom = [encode(7)] * count
            rom[0] = encode(0, 0, immediate=0x1234)
            rom[-1] = encode(0, 7, immediate=0x5678)
            data = [k + 17 for k in range(count)]
            inputs = {**memory_inputs(rom, data, reset=True), 'stall': False}
            state = evaluate_record(machine, 'reset', blank_state(machine, True), inputs)
            inputs['rst'] = False
            oracle = IntegerISA()
            retired_pcs = []
            for tick in range(80):
                old = state
                state = evaluate_record(machine, 'next', old, inputs)
                if outputs(machine, old, inputs)['commit']:
                    instruction = read_word(rom, oracle.pc)
                    retired_pcs.append(oracle.pc)
                    self.assertEqual(old['w_pc'].v, oracle.pc)
                    self.assertEqual(old['w_ir'].v, instruction)
                    oracle.retire(instruction, read_word(data, instruction & 63))
                self.assertEqual({key: state[key] for key in oracle.record()}, oracle.record())
                STATS['composition_ticks'] += 1
            self.assertEqual(retired_pcs[:65], list(range(64)) + [0])
            self.assertEqual(oracle.registers[7], 0x5678)


def main():
    parser = argparse.ArgumentParser(description=__doc__, add_help=False)
    parser.add_argument('--evidence', type=Path)
    parser.add_argument('--sizes', nargs='+', type=int, choices=COUNTS, default=COUNTS)
    args, unittest_args = parser.parse_known_args()
    ImportedMachineTests.evidence = args.evidence
    ImportedMachineTests.sizes = tuple(args.sizes)
    result = unittest.main(argv=[sys.argv[0], *unittest_args], exit=False)
    if result.result.wasSuccessful():
        print(json.dumps({'scope': 'finite concrete regression, not universal proof',
                          **dict(sorted(STATS.items()))}, sort_keys=True))
    raise SystemExit(not result.result.wasSuccessful())


if __name__ == '__main__':
    main()
