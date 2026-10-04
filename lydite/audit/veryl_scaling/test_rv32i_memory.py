#!/usr/bin/env python3
"""Independent byte oracle; optionally run actual imported Veryl and mutations.

python3 audit/veryl_scaling/test_rv32i_memory.py --evidence DIR
DIR must hold memory-{4,16,64}-import and memory-4-bad-FAULT-import.
Missing requested evidence fails; finite traces never stand in for universal proof.
"""
import argparse
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
from audit.interpreter import BV, evaluate, evaluate_record, outputs
from audit.veryl_scaling import rv32i_memory as m
from audit.veryl_scaling.test_cpu_memory_contract import blank_state, contract_environment


def inputs(count, **kw):
    values = {'rst': False, 'imem_address': 0, 'imem_valid': True,
        'dmem_address': 4096, 'dmem_valid': True, 'dmem_write': False,
        'write_enable': False, 'write_address': 4096, 'write_data': 0x12345678,
        'write_mask': 15, **{f'seed_rom{i}': 0x01020304+i for i in range(count)},
        **{f'seed_data{i}': 0xfedcba98-i for i in range(count)}, **kw}
    return {k: bool(values[k]) if ty == 'bool' else BV(ty['bv'], values[k])
            for k, ty in m.memory_inputs(count).items()}


def integer_oracle(state, inp, count):
    """Byte dictionaries, independent of DUT expressions and contract helpers."""
    rom, ram = {}, {}
    for i in range(count):
        for byte in range(4):
            rom[4*i+byte] = (state[f'rom{i}'].v >> (byte*8)) & 255
            ram[4096+4*i+byte] = (state[f'data{i}'].v >> (byte*8)) & 255
    wa = inp['write_address'].v
    if inp['write_enable'] and not inp['rst'] and wa in ram:
        start = wa-wa%4
        for byte in range(4):
            if inp['write_mask'].v & (1 << byte):
                ram[start+byte] = (inp['write_data'].v >> (byte*8)) & 255
    def word(memory, addr):
        start = addr-addr%4
        return sum(memory.get(start+i, 0) << (8*i) for i in range(4)) if addr in memory else 0
    ia, da = inp['imem_address'].v, inp['dmem_address'].v
    observed = {'imem_response': BV(32, word(rom, ia)),
        'dmem_response': BV(32, word(rom, da) if da in rom else word(ram, da)),
        'imem_fault': ia not in rom,
        'dmem_fault': da not in ram and (inp['dmem_write'] or da not in rom)}
    new = {**{f'rom{i}': BV(32, word(rom, 4*i)) for i in range(count)},
           **{f'data{i}': BV(32, word(ram, 4096+4*i)) for i in range(count)}}
    if inp['rst']: new = {k: inp['seed_'+k] for k in state}
    return new, observed


def cases(count):
    boundary = [0, 1, 3, 4, 4*count-1, 4*count, 4095, 4096, 4097,
                4099, 4100, 4096+4*count-1, 4096+4*count, 0x80000000,
                0x80001000, 0xffffffff]
    yield inputs(count, rst=True, write_enable=True, write_mask=15)
    for enabled in (False, True):
        for a in boundary:
            for mask in range(16):
                yield inputs(count, write_enable=enabled, write_address=a, write_mask=mask,
                    dmem_address=a, imem_address=a, dmem_write=bool(mask & 1))
    # Every mapped byte address, including distinct read/write offsets within a word.
    for a in range(4*count):
        yield inputs(count, imem_address=a, dmem_address=a, dmem_write=False)
        yield inputs(count, write_enable=True, write_address=4096+a,
            dmem_address=4096+(a^3), write_mask=1 << (a%4))
    rng = random.Random(count)
    for _ in range(80):
        yield inputs(count, write_enable=bool(rng.randrange(2)),
            write_address=rng.choice(boundary), dmem_address=rng.choice(boundary),
            imem_address=rng.choice(boundary), write_mask=rng.randrange(16),
            write_data=rng.getrandbits(32), dmem_write=bool(rng.randrange(2)),
            imem_valid=bool(rng.randrange(2)), dmem_valid=bool(rng.randrange(2)),
            seed_data0=rng.getrandbits(32))
    yield inputs(count, rst=True, write_enable=True, seed_data0=0x99887766)


def stub(count):
    state = m.memory_state(count)
    return {'state': state, 'wires': {}, 'next': {k:'s.'+k for k in state},
        'reset': {k:'i.seed_'+k for k in state},
        'outputs': {k: False if ty=='bool' else ['bv',32,0] for k,ty in m.PORTS.items()}}


class ContractTests(unittest.TestCase):
    def test_shape_independence_and_fault_variants(self):
        for count in (4, 16, 64):
            raw = stub(count); before = copy.deepcopy(raw)
            doc = m.memory_contract(count, raw)
            self.assertEqual(raw, before)
            self.assertNotIn('assumptions', doc['specs']['Contract'])
            raw['next']['data0'] = ['bv',32,999]
            self.assertEqual(doc['specs'], m.memory_contract(count, raw)['specs'])
            for k in before['state']:
                self.assertEqual(doc['implementation']['next'][k], before['next'][k])
        for fault in m.MEMORY_FAULTS:
            self.assertNotEqual(m.memory_source(4), m.memory_source(4, fault))
        for count in (0,1,8,65):
            with self.assertRaises(ValueError): m.memory_source(count)
        with self.assertRaises(ValueError): m.memory_source(4,'unknown')

    def test_mutation_helper_never_overwrites_existing_evidence(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            occupied = root / 'memory-mutation-summary.json'
            occupied.write_text('existing evidence')
            with patch.object(m, 'compile_machine') as compile_mock:
                with self.assertRaises(FileExistsError):
                    m.check_mutations(root, root / 'unused-checker')
                compile_mock.assert_not_called()
                self.assertEqual(occupied.read_text(), 'existing evidence')
                occupied.unlink()
                (root / 'memory-4-bad-ignore_mask-import').mkdir()
                with self.assertRaises(FileExistsError):
                    m.check_mutations(root, root / 'unused-checker')
                compile_mock.assert_not_called()

    def test_independent_contract_against_byte_oracle(self):
        for count in (4,16,64):
            doc = m.memory_contract(count, stub(count)); machine = doc['implementation']
            state = evaluate_record(machine, 'reset', blank_state(machine), inputs(count,rst=True))
            step = doc['specs']['Contract']['operations']['tick']
            for inp in cases(count):
                if inp['rst']: continue
                physical = {k:state[k] for k in m.memory_state(count)}
                new, obs = integer_oracle(physical, inp, count)
                monitored = {**new, **{'obs_model_'+k:v for k,v in new.items()},
                             **{'obs_'+k:BV(1,int(v)) if isinstance(v,bool) else v for k,v in obs.items()}}
                self.assertTrue(evaluate(step, contract_environment(state, inp, monitored)))
                for field in ('data0','rom0','obs_dmem_response','obs_dmem_fault'):
                    old=monitored[field]; changed=not old if isinstance(old,bool) else BV(old.w,old.v^1)
                    self.assertFalse(evaluate(step, contract_environment(state, inp, {**monitored,field:changed})))
                state=monitored


class ImportedTests(unittest.TestCase):
    evidence = None
    @classmethod
    def setUpClass(cls):
        if cls.evidence is None: raise unittest.SkipTest('use --evidence for compiled Veryl')
    def load(self,count,fault=None):
        folder=self.evidence/(f'memory-{count}'+('-bad-'+fault if fault else '')+'-import')
        raw=json.loads((folder/'machine.json').read_text())
        self.assertEqual((folder/'source.veryl').read_text(),m.memory_source(count,fault))
        self.assertEqual(json.loads((folder/'compiled.json').read_text())['allowed_diagnostics'],[])
        normal=json.loads((folder/'normal-lift.json').read_text())
        for part in ('next','outputs','wires'): self.assertEqual(raw[part],normal[part])
        self.assertEqual(raw['reset'],json.loads((folder/'reset-lift.json').read_text())['next'])
        return raw
    def test_actual_compiled_memory(self):
        for count in (4,16,64):
            raw=self.load(count); doc=m.memory_contract(count,raw); machine=doc['implementation']
            state=blank_state(raw,True); monitored=blank_state(machine,True)
            for inp in cases(count):
                expected, ports=integer_oracle(state,inp,count)
                part='reset' if inp['rst'] else 'next'
                actual=evaluate_record(raw,part,state,inp)
                observed=evaluate_record(machine,part,monitored,inp)
                self.assertEqual(actual,expected)
                if not inp['rst']:
                    self.assertEqual(outputs(raw,state,inp),ports)
                    self.assertTrue(evaluate(doc['specs']['Contract']['operations']['tick'],
                        contract_environment(monitored,inp,observed)))
                self.assertTrue(evaluate(doc['specs']['Contract']['invariant'],contract_environment(observed,inp)))
                state,monitored=actual,observed
    def test_every_actual_compiled_mutation_detected(self):
        for fault in m.MEMORY_FAULTS:
            raw=self.load(4,fault)
            state={k:v for k,v in ((k,BV(32,0xabcde000+i)) for i,k in enumerate(m.memory_state(4)))}
            detected=False
            for inp in cases(4):
                expected, ports=integer_oracle(state,inp,4)
                actual=evaluate_record(raw,'reset' if inp['rst'] else 'next',state,inp)
                if actual!=expected or (not inp['rst'] and outputs(raw,state,inp)!=ports):
                    detected=True;break
            self.assertTrue(detected,'undetected compiled mutation: '+fault)

if __name__=='__main__':
    parser=argparse.ArgumentParser(add_help=False);parser.add_argument('--evidence',type=Path)
    args,rest=parser.parse_known_args();ImportedTests.evidence=args.evidence
    unittest.main(argv=[sys.argv[0],*rest])
