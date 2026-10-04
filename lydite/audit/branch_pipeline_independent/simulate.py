#!/usr/bin/env python3
"""Run JSON transitions against an independent integer retirement ISA.

Expected architectural values never come from spec/binding/datapath expressions.
The existing independent Python AST evaluator executes the supplied implementation;
it imports neither the Rust verifier nor any solver. Mutation runs disable both the
binding check and all expected microarchitecture checks.
"""
import argparse
from collections import Counter
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import random
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'audit'))
from interpreter import BV, evaluate_record, outputs, related


def decode(ir, width):
    return ir >> (width + 2), (ir >> (width + 1)) & 1, (ir >> width) & 1, ir & ((1 << width) - 1)


def encode(width, opcode, rd=0, rs=0, imm=0):
    return (opcode << (width + 2)) | (rd << (width + 1)) | (rs << width) | (imm & ((1 << width) - 1))


def isa(ir, pc, regs, data, width):
    """Direct integer ISA. Opcodes 5..7 are explicitly non-writing NOPs."""
    opcode, rd, rs, imm = decode(ir, width)
    out = list(regs)
    target = (pc + 1) & 3
    if opcode == 0:
        out[rd] = imm
    elif opcode == 1:
        out[rd] = (regs[rs] + imm) & ((1 << width) - 1)
    elif opcode == 2:
        out[rd] = regs[rs] ^ imm
    elif opcode == 3:
        out[rd] = data[imm & 3]
    elif opcode == 4 and regs[rs] == 0:
        target = imm & 3
    return target, out


@dataclass
class Case:
    name: str
    rom: list
    data: list
    mode: str = 'periodic'
    cycles: int = 44


def cases(width):
    e = lambda op, rd=0, rs=0, imm=0: encode(width, op, rd, rs, imm)
    mask = (1 << width) - 1
    # Directed branch/nonwriter corner cases precede randomized coverage, so
    # mutation counterexamples remain small and comprehensible.
    directed = [
        ('taken_older_retire', [e(0, 0, 0, 7), e(4, 0, 1, 3), e(0, 0, 0, 11), e(2, 1, 0, 3)], [0, 1, mask, 7]),
        ('not_taken_forward_x', [e(0, 0, 0, 1), e(4, 1, 0, 3), e(1, 1, 0, 2), e(2, 0, 1, 7)], [0, 1, 2, 3]),
        ('taken_forward_x', [e(0, 0, 0, 0), e(4, 1, 0, 3), e(0, 1, 0, 9), e(1, 0, 0, 1)], [mask, 0, 2, 3]),
        ('forward_x_over_w', [e(0, 0, 0, 1), e(0, 0, 0, 0), e(4, 1, 0, 0), e(0, 1, 0, 13)], [0, 1, 2, 3]),
        ('load_to_taken_branch', [e(3, 0, 0, 1), e(4, 1, 0, 3), e(0, 1, 0, 13), e(1, 1, 0, 1)], [mask, 0, 2, 3]),
        ('load_to_not_taken_branch', [e(3, 1, 0, 2), e(4, 0, 1, 0), e(1, 0, 1, 7), e(2, 1, 0, mask)], [0, mask, 1, 3]),
        ('branch_must_not_forward_x', [e(0, 0, 0, 7), e(4, 0, 0, 2), e(1, 1, 0, 1), e(4, 1, 1, 0)], [0, 1, 2, 3]),
        ('branch_must_not_forward_w', [e(0, 0, 0, 7), e(4, 0, 0, 2), e(5, 1, 0, 9), e(1, 1, 0, 1)], [0, 1, 2, 3]),
        ('nop_must_not_write_or_forward', [e(0, 0, 0, mask), e(5, 0, 0, 0), e(6, 0, 0, 1), e(1, 1, 0, 1)], [0, 1, 2, 3]),
        ('taken_back_to_self', [e(4, 1, 0, 0), e(0, 0, 0, mask), e(0, 1, 0, mask), e(7, 0, 1, 3)], [0, 1, 2, 3]),
        ('taken_to_sequential', [e(4, 0, 0, 1), e(0, 0, 0, 1), e(1, 1, 0, mask), e(4, 1, 1, 0)], [0, 1, 2, 3]),
        ('overflow_zero_branch', [e(0, 0, 0, mask), e(1, 0, 0, 1), e(4, 1, 0, 0), e(0, 1, 0, 8)], [0, 1, 2, 3]),
    ]
    for name, rom, data in directed:
        for mode in ('none', 'periodic', 'branch_stall', 'branch_reset', 'load_stall', 'full_reset'):
            yield Case(name + '_' + mode, rom, data, mode)
    # Exhaust every 4-bit instruction encoding, lifted to wider widths with
    # both low-only and high immediate bits. This includes reserved NOP opcodes.
    for raw in range(512):
        op, rd, rs, low = decode(raw, 4)
        imm = low if width == 4 or raw & 1 else (mask & ~15) | low
        ir = e(op, rd, rs, imm)
        yield Case('encoding_' + str(raw), [e(0, rs, 0, raw & mask), ir, e(1, 1-rd, rd, 3), e(4, rd, 1-rd, 0)], [(raw + j*5) & mask for j in range(4)])
    rng = random.Random(0xB12A00 + width)
    for n in range(192):
        rom = [e(rng.randrange(8), rng.randrange(2), rng.randrange(2), rng.getrandbits(width)) for _ in range(4)]
        data = [rng.getrandbits(width) for _ in range(4)]
        yield Case('random_' + str(n), rom, data, ('periodic', 'branch_stall', 'branch_reset', 'full_reset')[n % 4], 60)


def run(doc, case, *, check_binding=True, check_micro=True):
    width = doc['impl']['state']['r0']['bv']
    mask = (1 << width) - 1
    imask = (1 << (width + 5)) - 1
    m = doc['impl']
    state = arch = None
    regs, pc = [0, 0], 0
    captured_rom, captured_data = case.rom[:], case.data[:]
    cov = Counter()
    trace = []
    fired = False
    stall_left = 0
    for cycle in range(case.cycles):
        reset = cycle == 0
        stall = cycle % 11 in (4, 5, 6) if case.mode == 'periodic' else False
        if state is not None:
            dop, drd, drs, _ = decode(state['d_ir'].v, width)
            xop, xrd, xrs, ximm = decode(state['x_ir'].v, width)
            wop, wrd, _, _ = decode(state['w_ir'].v, width)
            d, x, w = (state[stage + '_valid'] for stage in ('d', 'x', 'w'))
            branch_x = x and xop == 4
            hazard = d and dop in (1, 2, 4) and x and xop == 3 and xrd == drs
            trigger = (case.mode in ('branch_stall', 'branch_reset') and branch_x) or (case.mode == 'load_stall' and hazard) or (case.mode == 'full_reset' and d and x and w)
            if trigger and not fired:
                fired = True
                if case.mode in ('branch_reset', 'full_reset'):
                    reset = True
                    stall = True  # synchronous reset wins over external stall
                    captured_rom = case.rom[1:] + case.rom[:1]
                    captured_data = [v ^ mask for v in case.data]
                    cov['reset_while_branch_x' if branch_x else 'reset_while_full'] += 1
                else:
                    stall_left = 3
            if stall_left:
                stall = True
                stall_left -= 1
        # On nonreset cycles inputs are deliberately unrelated to stored ROM/data.
        inp = {'rst': reset, 'stall': stall,
               **{f'rom{i}': BV(width+5, v if reset else v ^ imask) for i, v in enumerate(captured_rom)},
               **{f'data{i}': BV(width, v if reset else v ^ mask) for i, v in enumerate(captured_data)}}
        if reset:
            state = evaluate_record(m, 'reset', {}, inp)
            arch = evaluate_record(doc['spec'], 'reset', {}, inp)
            regs, pc = [0, 0], 0
            cov['resets'] += 1
            cov['reset_with_stall'] += int(stall)
            assert state['pc'].v == 0 and [state['r0'].v, state['r1'].v] == [0, 0], ('reset_architecture', case.name, cycle)
            assert not any(state[k+'_valid'] for k in ('d', 'x', 'w')), ('reset_pipeline', case.name, cycle)
            if check_binding:
                assert related(doc, arch, state), ('reset_binding', case.name, cycle)
            continue
        old = state
        commit = outputs(m, old, inp)['commit']
        assert commit == (w and not stall), ('commit_contract', case.name, cycle)
        cov['full_pipeline'] += int(d and x and w)
        cov['stalls'] += int(stall)
        cov['branch_x_stalled'] += int(branch_x and stall)
        cov['load_use'] += int(hazard and not stall)
        cov['load_branch_interlock'] += int(hazard and dop == 4 and not stall)
        cov['load_branch_stalled'] += int(hazard and dop == 4 and stall)
        taken = branch_x and old['x_operand'].v == 0
        cov['taken_resolutions'] += int(taken and not stall)
        cov['not_taken_resolutions'] += int(branch_x and not taken and not stall)
        cov['younger_d_squashed'] += int(taken and d and not stall)
        cov['taken_with_older_retire'] += int(taken and commit)
        cov['branch_x_forward'] += int(d and dop == 4 and x and xop in (0, 1, 2) and xrd == drs and not stall)
        cov['branch_w_forward'] += int(d and dop == 4 and w and wop < 4 and wrd == drs and not stall)
        cov['branch_double_match'] += int(d and dop == 4 and x and w and xop < 3 and wop < 4 and drs == xrd == wrd and not stall)
        cov['nonwriting_x_match_ignored'] += int(d and dop in (1, 2, 4) and x and xop >= 4 and xrd == drs and not stall)
        cov['nonwriting_w_match_ignored'] += int(d and dop in (1, 2, 4) and w and wop >= 4 and wrd == drs and not stall)
        event = {'cycle': cycle, 'stall': stall, 'commit': commit, 'before_pc': pc,
                 'stage_pcs': {k: old[k+'_pc'].v if old[k+'_valid'] else None for k in ('d','x','w')},
                 'stage_ops': {'d': dop, 'x': xop, 'w': wop}, 'taken_x': taken}
        if commit:
            assert old['w_pc'].v == pc and old['w_ir'].v == captured_rom[pc], ('retire_order', case.name, event, trace[-6:])
            op = decode(captured_rom[pc], width)[0]
            cov['test_instruction_retired'] += int(case.name.startswith('encoding_') and captured_rom[pc] == case.rom[1])
            pc, regs = isa(captured_rom[pc], pc, regs, captured_data, width)
            cov['retirements'] += 1
            cov['branch_retirements'] += int(op == 4)
            cov['nop_retirements'] += int(op >= 5)
            arch = evaluate_record(doc['spec'], 'next', arch, inp)
        state = evaluate_record(m, 'next', old, inp)
        event.update(expected_pc=pc, actual_pc=state['pc'].v, expected_regs=regs[:], actual_regs=[state['r0'].v, state['r1'].v])
        assert state['pc'].v == pc and event['actual_regs'] == regs, ('architectural_result', case.name, event, trace[-6:])
        assert arch['pc'].v == pc and [arch['r0'].v, arch['r1'].v] == regs, ('spec_vs_integer_ISA', case.name, event)
        assert all(state[f'rom{i}'].v == v for i,v in enumerate(captured_rom)), ('immutable_rom', case.name, cycle)
        assert all(state[f'data{i}'].v == v for i,v in enumerate(captured_data)), ('immutable_data', case.name, cycle)
        if stall:
            assert state == old, ('external_stall', case.name, event)
        if check_binding:
            assert related(doc, arch, state), ('binding', case.name, event)
        if check_micro:
            if taken and not stall:
                assert state['w_valid'] and state['w_pc'] == old['x_pc'], ('branch_survives_squash', case.name, event)
                assert not state['d_valid'] and not state['x_valid'] and state['fetch_pc'].v == (ximm & 3), ('squash_redirect', case.name, event)
            # W is resolved, X and younger fetches are still predicted sequential.
            age, pending_regs = pc, regs[:]
            for stage in ('w', 'x', 'd'):
                if not state[stage+'_valid']:
                    continue
                ir = state[stage+'_ir'].v
                assert state[stage+'_pc'].v == age and ir == captured_rom[age], ('instruction_age', stage, case.name, event)
                if stage == 'w':
                    age, pending_regs = isa(ir, age, pending_regs, captured_data, width)
                    assert state['w_next_pc'].v == age, ('resolved_next_pc', case.name, event)
                else:
                    op, _, rs, _ = decode(ir, width)
                    if stage == 'x' and op in (1, 2, 4):
                        assert state['x_operand'].v == pending_regs[rs], ('captured_operand', case.name, event)
                    age = (age + 1) & 3
            assert state['fetch_pc'].v == age, ('fetch_age', case.name, event)
        trace.append(event)
        cov['cycles'] += 1
    return cov


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--widths', default='4,8,16,32')
    parser.add_argument('--output', type=Path, default=ROOT/'results/branch_pipeline_independent/simulation.json')
    parser.add_argument('--mutants', action='store_true')
    args = parser.parse_args()
    rows, mutant_rows = [], []
    for width in map(int, args.widths.split(',')):
        source = ROOT / f'examples/branch_pipeline_w{width}.json'
        doc = json.loads(source.read_text())
        corpus = list(cases(width))
        total = Counter()
        for case in corpus:
            coverage = run(doc, case)
            if case.name.startswith('encoding_'):
                assert coverage['test_instruction_retired'] > 0, ('encoding_never_retired', width, case.name)
            total.update(coverage)
        required = ['taken_resolutions','not_taken_resolutions','branch_x_forward','branch_w_forward','branch_double_match','load_branch_interlock','load_branch_stalled','branch_x_stalled','reset_while_branch_x','reset_while_full','taken_with_older_retire','younger_d_squashed','nop_retirements','nonwriting_x_match_ignored','nonwriting_w_match_ignored']
        assert all(total[k] for k in required), ('missing_coverage', width, [k for k in required if not total[k]])
        rows.append({'width': width, 'source': str(source.relative_to(ROOT)), 'sha256': sha(source), 'trials': len(corpus), 'coverage': dict(total)})
        if args.mutants:
            for source in sorted((ROOT/'examples').glob(f'branch_pipeline_w{width}_bad_*.json')):
                bad = json.loads(source.read_text())
                assert bad['spec'] == doc['spec'] and bad['binding'] == doc['binding'], ('nonimplementation_mutant', source)
                caught = None
                for case in corpus:
                    try:
                        run(bad, case, check_binding=False, check_micro=False)
                    except AssertionError as error:
                        caught = {'case': case.__dict__, 'failure': str(error)}
                        break
                assert caught, ('mutant_survived', source)
                mutant_rows.append({'source': str(source.relative_to(ROOT)), 'sha256': sha(source), 'binding_checked': False, 'microarchitecture_checked': False, 'counterexample': caught})
    result = {'status': 'pass', 'isa': 'independent integer arithmetic and retirement order', 'rows': rows, 'mutants': mutant_rows}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2)+'\n')
    print(json.dumps({'status': 'pass', 'widths': [r['width'] for r in rows], 'trials': sum(r['trials'] for r in rows), 'mutants_caught': len(mutant_rows)}))

if __name__ == '__main__':
    main()
