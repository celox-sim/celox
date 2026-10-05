#!/usr/bin/env python3
"""Compiler-free structure tests plus opt-in actual imported memory regressions.

python3 audit/veryl_scaling/test_cpu_writable_memory.py --evidence DIR
The evidence must contain memory-{4,16,64}-import/ and memory-4-bad-FAULT-import/.
Standalone underscore-named import directories are also accepted.
Finite integer tests complement, and never replace, universal checker results.
"""
import argparse
import copy
import json
from pathlib import Path
import random
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from audit.interpreter import BV, evaluate, evaluate_record, outputs
from audit.veryl_scaling import cpu_writable_memory as m
from audit.veryl_scaling.test_cpu_memory_contract import blank_state, contract_environment, memory_stub


def inputs(count, address=0, read=0, enabled=False, reset=False, value=0xF0F055AA):
    return {'rst': reset, 'imem_address': BV(6, (read + 17) & 63), 'dmem_address': BV(6, read),
            'write_enable': enabled, 'write_address': BV(6, address), 'write_data': BV(32, value),
            **{f'seed_rom{k}': BV(41, 0x100000001 + k) for k in range(count)},
            **{f'seed_data{k}': BV(32, 17 + k) for k in range(count)}}


class ShapeTests(unittest.TestCase):
    def test_capacity_fault_validation(self):
        for count in (0, 1, 8, 65):
            with self.assertRaises(ValueError): m.memory_source(count)
        with self.assertRaises(ValueError): m.memory_source(4, 'unknown')
        for fault in m.MEMORY_FAULTS:
            self.assertNotEqual(m.memory_source(4, fault), m.memory_source(4))

    def test_independent_model_and_noninterference(self):
        for count in (4, 16, 64):
            raw = memory_stub(count)
            before = copy.deepcopy(raw)
            doc = m.memory_contract(count, raw)
            self.assertEqual(raw, before)
            leaf, augmented = doc['specs']['Contract'], doc['implementation']
            self.assertEqual(len(leaf['state']), 4 * count + 2)
            self.assertNotIn('assumptions', leaf)
            self.assertEqual(augmented['operations'], {'tick': True})
            for part in ('next', 'reset'):
                self.assertEqual({k: augmented[part][k] for k in raw['state']}, raw[part])
            for port in ('imem_response', 'dmem_response'):
                self.assertEqual(augmented['next']['obs_' + port], raw['outputs'][port])
            for k in raw['state']:
                self.assertEqual(augmented['reset']['obs_model_' + k], 'i.seed_' + k)
                self.assertNotIn('w.', json.dumps(augmented['next']['obs_model_' + k]))
            # Changing DUT transitions cannot change the independent expected contract.
            raw['next']['data0'] = ['bv', 32, 999]
            changed = m.memory_contract(count, raw)['specs']['Contract']
            self.assertEqual(leaf, changed)

    def test_contract_exhaustive_addresses_and_frames(self):
        for count in (4, 16, 64):
            doc = m.memory_contract(count, memory_stub(count))
            model = doc['implementation']
            state = evaluate_record(model, 'reset', blank_state(model), inputs(count, reset=True))
            step = doc['specs']['Contract']['operations']['tick']
            for enabled in (False, True):
                for address in range(64):
                    for read in (address, (address + 1) & 63):
                        inp = inputs(count, address, read, enabled)
                        new = dict(state)
                        for i in range(count):
                            value = inp['write_data'] if enabled and address == i else state[f'data{i}']
                            new[f'data{i}'] = new[f'obs_model_data{i}'] = value
                        ia = inp['imem_address'].v
                        new['obs_imem_response'] = state[f'rom{ia}'] if ia < count else BV(41, 0)
                        new['obs_dmem_response'] = new[f'data{read}'] if read < count else BV(32, 0)
                        env = contract_environment(state, inp, new)
                        self.assertTrue(evaluate(step, env))
                        for field in ('data0', 'rom0', 'obs_dmem_response'):
                            damaged = {**new, field: BV(new[field].w, new[field].v ^ 1)}
                            self.assertFalse(evaluate(step, contract_environment(state, inp, damaged)))


class ImportedTests(unittest.TestCase):
    evidence = None
    @classmethod
    def setUpClass(cls):
        if cls.evidence is None:
            raise unittest.SkipTest('use --evidence for actual compiled Veryl')

    def load(self, count, fault=None):
        folder = self.evidence / (f'memory-{count}' + ('-bad-' + fault if fault else '') + '-import')
        if not folder.is_dir():
            folder = self.evidence / (f'memory_{count}' + ('_bad_' + fault if fault else ''))
        raw = json.loads((folder / 'machine.json').read_text())
        self.assertEqual((folder / 'source.veryl').read_text(), m.memory_source(count, fault))
        self.assertEqual(json.loads((folder / 'compiled.json').read_text())['allowed_diagnostics'], [])
        normal = json.loads((folder / 'normal-lift.json').read_text())
        for part in ('next', 'outputs', 'wires'): self.assertEqual(raw[part], normal[part])
        self.assertEqual(raw['reset'], json.loads((folder / 'reset-lift.json').read_text())['next'])
        return raw

    def test_imported_memories_against_integer_model(self):
        for count in (4, 16, 64):
            raw = self.load(count)
            doc = m.memory_contract(count, raw)
            model = doc['implementation']
            state = blank_state(raw, True)
            monitored = blank_state(model, True)
            rng = random.Random(count)
            cases = [(True, 0, 0, True)]
            cases += [(False, a, r, enable) for enable in (False, True) for a in range(64)
                      for r in (a, (a + 1) & 63)]
            cases += [(True, 63, 63, True)]
            cases += [(False, rng.randrange(64), rng.randrange(64), bool(rng.randrange(2))) for _ in range(100)]
            rom, data = [], []
            for reset, address, read, enabled in cases:
                inp = inputs(count, address, read, enabled, reset, rng.getrandbits(32))
                for k in range(count):
                    inp[f'seed_data{k}'] = BV(32, rng.getrandbits(32))
                    inp[f'seed_rom{k}'] = BV(41, rng.getrandbits(41))
                new = evaluate_record(raw, 'reset' if reset else 'next', state, inp)
                observed = evaluate_record(model, 'reset' if reset else 'next', monitored, inp)
                if reset:
                    rom = [inp[f'seed_rom{k}'].v for k in range(count)]
                    data = [inp[f'seed_data{k}'].v for k in range(count)]
                else:
                    if enabled and address < count: data[address] = inp['write_data'].v
                    response = outputs(raw, state, inp)
                    ia = inp['imem_address'].v
                    self.assertEqual(response, {'imem_response': BV(41, rom[ia] if ia < count else 0),
                                                'dmem_response': BV(32, data[read] if read < count else 0)})
                    self.assertTrue(evaluate(doc['specs']['Contract']['operations']['tick'],
                                            contract_environment(monitored, inp, observed)))
                self.assertEqual(new, {**{f'rom{k}': BV(41, v) for k, v in enumerate(rom)},
                                       **{f'data{k}': BV(32, v) for k, v in enumerate(data)}})
                self.assertTrue(evaluate(doc['specs']['Contract']['invariant'], contract_environment(observed, inp)))
                state, monitored = new, observed

    def test_every_imported_mutant_has_independent_counterexample(self):
        count = 4
        for fault in m.MEMORY_FAULTS:
            raw = self.load(count, fault)
            detected = False
            state = {**{f'rom{k}': BV(41, 100 + k) for k in range(count)},
                     **{f'data{k}': BV(32, 200 + k) for k in range(count)}}
            for reset in (False, True):
                for enabled in (False, True):
                    for address in (0, 1, 3, 4, 63):
                        for read in (address, (address + 1) & 63):
                            inp = inputs(count, address, read, enabled, reset)
                            expected = dict(state)
                            if reset:
                                expected = {k: inp['seed_' + k] for k in state}
                            elif enabled and address < count:
                                expected[f'data{address}'] = inp['write_data']
                            actual = evaluate_record(raw, 'reset' if reset else 'next', state, inp)
                            detected |= actual != expected
                            if not reset:
                                ia = inp['imem_address'].v
                                expected_ports = {'imem_response': state[f'rom{ia}'] if ia < count else BV(41, 0),
                                                  'dmem_response': expected[f'data{read}'] if read < count else BV(32, 0)}
                                detected |= outputs(raw, state, inp) != expected_ports
            self.assertTrue(detected, 'undetected imported mutation: ' + fault)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument('--evidence', type=Path)
    args, rest = parser.parse_known_args()
    ImportedTests.evidence = args.evidence
    unittest.main(argv=[sys.argv[0], *rest])
