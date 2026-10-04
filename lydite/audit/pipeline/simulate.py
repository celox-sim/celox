#!/usr/bin/env python3
"""Concrete pipeline regression against a separately written integer ISA.

This is finite testing, NOT the universal inductive verification. The JSON IR
is executed by audit/interpreter.py; expected retirement results use only isa().
"""
import argparse
import json
import random
import sys
from pathlib import Path
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'audit'))
from interpreter import BV, evaluate, evaluate_record, related


def instruction(op, rd=0, rs=0, imm=0):
    return (op << 6) | (rd << 5) | (rs << 4) | imm


def isa(pc, regs, rom, data):
    word = rom[pc]
    opcode, destination, source, immediate = word >> 6, (word >> 5) & 1, (word >> 4) & 1, word & 15
    result = (immediate if opcode == 0 else
              regs[source] + immediate if opcode == 1 else
              regs[source] ^ immediate if opcode == 2 else data[immediate & 3]) & 15
    regs = list(regs)
    regs[destination] = result
    return (pc + 1) & 3, regs, word, destination, result


def inputs(rom, data, rst=False, stall=False):
    return {'rst': rst, 'stall': stall,
            **{f'rom{i}': BV(8, word) for i, word in enumerate(rom)},
            **{f'data{i}': BV(4, word) for i, word in enumerate(data)}}


def reset(model, ins):
    return {k: evaluate(v, {'i.' + n: x for n, x in ins.items()}) for k, v in model['reset'].items()}


def trial(doc, rom, data, ticks=50, seed=0, stalls=None, reset_at=()):
    rng = random.Random(seed)
    state = reset(doc['impl'], inputs(rom, data, rst=True))
    pc, regs = 0, [0, 0]
    counts = dict(cycles=0, retires=0, hazards=0, x_forwards=0, w_forwards=0,
                  simultaneous_writers=0, triple_occupancy=0, pc_wraps=0, resets=0, stalls=0)
    sequence = []
    for tick in range(ticks):
        rst = tick in reset_at
        stall = rng.randrange(5) == 0 if stalls is None else tick in stalls
        # Mutate live input ROM/data every cycle: only reset may capture them.
        ins = inputs(rom if rst else [rng.randrange(256) for _ in range(4)],
                     data if rst else [rng.randrange(16) for _ in range(4)], rst, stall)
        arch = {'pc': BV(2, pc), 'r0': BV(4, regs[0]), 'r1': BV(4, regs[1]),
                **{f'rom{i}': BV(8, x) for i, x in enumerate(rom)},
                **{f'data{i}': BV(4, x) for i, x in enumerate(data)}}
        assert related(doc, arch, state), f'relation before cycle {tick}'
        wires = {k: evaluate('w.' + k,
                  {**{'s.' + n: x for n, x in state.items()}, **{'i.' + n: x for n, x in ins.items()}},
                  doc['impl']['wires']) for k in ('commit', 'hazard', 'x_forward', 'w_match', 'd_uses')}
        nxt = reset(doc['impl'], ins) if rst else evaluate_record(doc['impl'], 'next', state, ins)
        counts['cycles'] += 1
        if rst:
            pc, regs = 0, [0, 0]
            counts['resets'] += 1
        elif stall:
            counts['stalls'] += 1
            assert nxt == state, f'stall changed state at cycle {tick}'
        else:
            counts['triple_occupancy'] += int(all(state[s + '_valid'] for s in ('d', 'x', 'w')))
            counts['hazards'] += int(wires['hazard'])
            consuming = state['d_valid'] and wires['d_uses'] and not wires['hazard']
            counts['x_forwards'] += int(consuming and wires['x_forward'])
            counts['w_forwards'] += int(consuming and not wires['x_forward'] and wires['w_match'])
            counts['simultaneous_writers'] += int(consuming and wires['x_forward'] and wires['w_match'])
            if wires['hazard']:
                assert nxt['d_ir'] == state['d_ir'] and nxt['d_pc'] == state['d_pc']
                assert nxt['fetch_pc'] == state['fetch_pc'] and not nxt['x_valid']
                assert nxt['w_valid'] == state['x_valid']
            if wires['commit']:
                oldpc = pc
                pc, regs, word, rd, value = isa(pc, regs, rom, data)
                assert state['w_pc'].v == oldpc, f'retirement order at cycle {tick}'
                assert state['w_ir'].v == word and state['w_result'].v == value, f'retirement value at cycle {tick}'
                counts['retires'] += 1
                counts['pc_wraps'] += int(pc == 0)
                if len(sequence) < 12: sequence.append({'pc': oldpc, 'word': word, 'rd': rd, 'value': value})
        assert nxt['pc'].v == pc and [nxt['r0'].v, nxt['r1'].v] == regs, f'ISA mismatch at cycle {tick}'
        for i in range(4):
            assert nxt[f'rom{i}'].v == rom[i] and nxt[f'data{i}'].v == data[i]
        arch.update(pc=BV(2, pc), r0=BV(4, regs[0]), r1=BV(4, regs[1]))
        assert related(doc, arch, nxt), f'relation after cycle {tick}'
        state = nxt
    return {**counts, 'first_retires': sequence}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--out', type=Path, default=ROOT / 'audit/pipeline/simulation_results.json')
    parser.add_argument('--random-cases', type=int, default=100)
    args = parser.parse_args()
    doc = json.loads((ROOT / 'examples/pipeline.json').read_text())
    programs = {
        'youngest_writer': [instruction(0, 0, 0, 3), instruction(1, 0, 0, 4),
                            instruction(2, 1, 0, 1), instruction(1, 0, 1, 2)],
        'load_use': [instruction(3, 0, 0, 2), instruction(1, 1, 0, 3),
                     instruction(3, 1, 0, 1), instruction(2, 0, 1, 7)],
        'w_only_forward': [instruction(0, 0, 0, 7), instruction(0, 1, 0, 2),
                           instruction(1, 1, 0, 3), instruction(2, 0, 1, 5)],
        'arithmetic_wrap': [instruction(0, 0, 0, 15), instruction(1, 0, 0, 1),
                            instruction(1, 1, 0, 15), instruction(1, 0, 1, 15)],
    }
    rows = []
    for name, rom in programs.items():
        rows.append({'case': name, **trial(doc, rom, [1, 11, 9, 4], ticks=80, stalls=set())})
        rows.append({'case': name + '_stall_reset', **trial(doc, rom, [1, 11, 9, 4], ticks=80,
                     stalls={3, 4, 5, 12, 18, 19, 20, 30, 31}, reset_at=(17, 49))})
    for seed in range(args.random_cases):
        rng = random.Random(seed)
        rom, data = [rng.randrange(256) for _ in range(4)], [rng.randrange(16) for _ in range(4)]
        rows.append({'case': f'random_{seed}', **trial(doc, rom, data, ticks=120, seed=seed, reset_at=(37, 83))})
    mutants = {}
    for name in ('forward', 'priority', 'interlock', 'retire', 'stall'):
        mutant = json.loads((ROOT / 'examples' / f'pipeline_bad_{name}.json').read_text())
        for case, rom in programs.items():
            try:
                trial(mutant, rom, [1, 11, 9, 4], ticks=30, stalls={4, 5, 12})
            except AssertionError as failure:
                mutants[name] = {'case': case, 'rejected': str(failure)}
                break
        assert name in mutants, f'mutant escaped concrete tests: {name}'
    totals = {key: sum(row[key] for row in rows) for key in rows[0] if isinstance(rows[0][key], int)}
    for key in ('hazards', 'x_forwards', 'w_forwards', 'simultaneous_writers', 'triple_occupancy', 'pc_wraps', 'resets', 'stalls'):
        assert totals[key] > 0, f'coverage missing: {key}'
    report = {'kind': 'finite concrete regression, not universal proof', 'cases': len(rows),
              'totals': totals, 'mutants': mutants, 'runs': rows}
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({k: v for k, v in report.items() if k != 'runs'}, indent=2))


if __name__ == '__main__': main()
