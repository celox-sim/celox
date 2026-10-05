#!/usr/bin/env python3
"""Generate a width-scalable D/X/W CPU with X-resolved BZ and younger squash.

A declarative model generator, not a verifier. Existing pipeline fixtures are
intentionally untouched. All widths keep the same ISA/control structure.
"""
import copy
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
from json_to_lyd import print_document
from build_pipeline import b, eq, it, add, ex, land, lor, neg, implies, read_reg, rom, memory

WIDTHS = (4, 8, 16, 32)
FAULTS = ('no_flush', 'wrong_kill', 'wrong_target', 'stall_branch', 'branch_forward', 'branch_interlock', 'branch_write')


def build(width=4):
    if width not in WIDTHS:
        raise ValueError(f'width must be one of {WIDTHS}')
    iw = width + 5
    def op(ir): return ex(width + 4, width + 2, ir)
    def rd(ir): return ex(width + 1, width + 1, ir)
    def rs(ir): return ex(width, width, ir)
    def imm(ir): return ex(width - 1, 0, ir)
    def isop(ir, code): return eq(op(ir), b(3, code))
    def writes(ir): return eq(ex(width + 4, width + 4, ir), b(1, 0))
    def uses(ir): return lor(lor(isop(ir, 1), isop(ir, 2)), isop(ir, 4))
    def architectural_value(ir, regs, prefix):
        src = read_reg(rs(ir), regs)
        return it(isop(ir, 0), imm(ir),
                  it(isop(ir, 1), add(src, imm(ir)),
                     it(isop(ir, 2), ['bxor', src, imm(ir)],
                        memory(prefix, ex(1, 0, ir)))))
    def architectural_pc(ir, pc, regs):
        taken = land(isop(ir, 4), eq(read_reg(rs(ir), regs), b(width, 0)))
        return it(taken, ex(1, 0, ir), add(pc, b(2, 1)))

    immutable = {**{f'rom{i}': {'bv': iw} for i in range(4)},
                 **{f'data{i}': {'bv': width} for i in range(4)}}
    arch = {'pc': {'bv': 2}, 'r0': {'bv': width}, 'r1': {'bv': width}, **immutable}
    reset = {'pc': b(2, 0), 'r0': b(width, 0), 'r1': b(width, 0),
             **{k: 'i.' + k for k in immutable}}
    spec = {'state': copy.deepcopy(arch), 'reset': copy.deepcopy(reset),
            'wires': {'ir': rom('s.', 's.pc'),
                      'result': architectural_value('w.ir', ['s.r0', 's.r1'], 's.')},
            'next': {'pc': architectural_pc('w.ir', 's.pc', ['s.r0', 's.r1']),
                     'r0': it(land(writes('w.ir'), eq(rd('w.ir'), b(1, 0))), 'w.result', 's.r0'),
                     'r1': it(land(writes('w.ir'), eq(rd('w.ir'), b(1, 1))), 'w.result', 's.r1'),
                     **{k: 's.' + k for k in immutable}},
            'outputs': {'can_step': True}}
    pipeline = {'fetch_pc': {'bv': 2}}
    for stage in ('d', 'x', 'w'):
        pipeline.update({f'{stage}_valid': 'bool', f'{stage}_pc': {'bv': 2},
                         f'{stage}_ir': {'bv': iw}})
    pipeline.update({'x_operand': {'bv': width}, 'w_result': {'bv': width},
                     'w_next_pc': {'bv': 2}})
    pipeline_reset = {k: False if v == 'bool' else b(v['bv'], 0)
                      for k, v in pipeline.items()}
    wires = {
        'commit': land(neg('i.stall'), 's.w_valid'),
        'd_uses': uses('s.d_ir'), 'd_rs': rs('s.d_ir'),
        'd_branch': isop('s.d_ir', 4),
        'x_rd': rd('s.x_ir'), 'w_rd': rd('s.w_ir'),
        'x_load': isop('s.x_ir', 3), 'x_writes': writes('s.x_ir'),
        'w_writes': writes('s.w_ir'),
        'x_match': land('s.x_valid', 'w.x_writes', eq('w.x_rd', 'w.d_rs')),
        'w_match': land('s.w_valid', 'w.w_writes', eq('w.w_rd', 'w.d_rs')),
        'hazard': land('s.d_valid', 'w.d_uses', 'w.x_match', 'w.x_load'),
        'x_forward': land('w.x_match', neg('w.x_load')),
        'reg_operand': it(eq('w.d_rs', b(1, 1)), 's.r1', 's.r0'),
        'w_operand': it('w.w_match', 's.w_result', 'w.reg_operand'),
        'd_operand': it('w.x_forward', 'w.x_result', 'w.w_operand'),
        # Implementation datapath is written separately from ISA execution.
        'x_result': it(isop('s.x_ir', 3), memory('s.', ex(1, 0, 's.x_ir')),
                       it(isop('s.x_ir', 2), ['bxor', 's.x_operand', imm('s.x_ir')],
                          it(isop('s.x_ir', 1), add('s.x_operand', imm('s.x_ir')), imm('s.x_ir')))),
        'x_taken': land('s.x_valid', isop('s.x_ir', 4), eq('s.x_operand', b(width, 0))),
        'x_next_pc': it('w.x_taken', ex(1, 0, 's.x_ir'), add('s.x_pc', b(2, 1))),
        'fetched': rom('s.', 's.fetch_pc'),
    }
    next_state = {k: 's.' + k for k in immutable}
    next_state.update({
        'pc': it('s.w_valid', 's.w_next_pc', 's.pc'),
        'r0': it(land('s.w_valid', 'w.w_writes', eq('w.w_rd', b(1, 0))), 's.w_result', 's.r0'),
        'r1': it(land('s.w_valid', 'w.w_writes', eq('w.w_rd', b(1, 1))), 's.w_result', 's.r1'),
        'w_valid': 's.x_valid', 'w_pc': 's.x_pc', 'w_ir': 's.x_ir',
        'w_result': 'w.x_result', 'w_next_pc': 'w.x_next_pc',
        'x_valid': land('s.d_valid', neg('w.hazard'), neg('w.x_taken')),
        'x_pc': 's.d_pc', 'x_ir': 's.d_ir', 'x_operand': 'w.d_operand',
        'd_valid': it('w.x_taken', False, it('w.hazard', 's.d_valid', True)),
        'd_pc': it('w.hazard', 's.d_pc', 's.fetch_pc'),
        'd_ir': it('w.hazard', 's.d_ir', 'w.fetched'),
        'fetch_pc': it('w.x_taken', ex(1, 0, 's.x_ir'),
                       it('w.hazard', 's.fetch_pc', add('s.fetch_pc', b(2, 1)))),
    })
    implementation = {'state': {**copy.deepcopy(arch), **pipeline},
                      'reset': {**copy.deepcopy(reset), **pipeline_reset},
                      'wires': wires,
                      'next': {k: it('i.stall', 's.' + k, v) for k, v in next_state.items()},
                      'outputs': {'commit': 'w.commit'}}
    # State-only inductive relation. D/fetch are speculative until X resolves.
    # Invalid payloads and W.result for nonwriters are intentionally irrelevant.
    a = ['impl.r0', 'impl.r1']
    pending_w = [it(land('impl.w_valid', writes('impl.w_ir'), eq(rd('impl.w_ir'), b(1, i))),
                    'impl.w_result', a[i]) for i in range(2)]
    pc_x = it('impl.w_valid', 'impl.w_next_pc', 'impl.pc')
    pc_d = add(pc_x, it('impl.x_valid', b(2, 1), b(2, 0)))
    pc_f = add(pc_d, it('impl.d_valid', b(2, 1), b(2, 0)))
    relation = [eq('spec.' + k, 'impl.' + k) for k in arch]
    relation += [
        implies('impl.w_valid', land(eq('impl.w_pc', 'impl.pc'),
            eq('impl.w_ir', rom('impl.', 'impl.w_pc')),
            eq('impl.w_next_pc', architectural_pc('impl.w_ir', 'impl.w_pc', a)),
            implies(writes('impl.w_ir'), eq('impl.w_result', architectural_value('impl.w_ir', a, 'impl.'))))),
        implies('impl.x_valid', land(eq('impl.x_pc', pc_x),
            eq('impl.x_ir', rom('impl.', 'impl.x_pc')),
            implies(uses('impl.x_ir'), eq('impl.x_operand', read_reg(rs('impl.x_ir'), pending_w))))),
        implies('impl.d_valid', land(eq('impl.d_pc', pc_d),
            eq('impl.d_ir', rom('impl.', 'impl.d_pc')))),
        eq('impl.fetch_pc', pc_f),
    ]
    return {'version': 2, 'name': f'branch_pipeline_w{width}',
            'inputs': {'rst': 'bool', 'stall': 'bool', **immutable},
            'reset_input': 'rst', 'spec': spec, 'impl': implementation,
            'binding': land(*relation), 'commit': 'commit', 'can_step': 'can_step',
            'hold_when': 'i.stall',
            'progress': {'enabled': neg('i.stall'),
                         'rank': it('impl.w_valid', b(2, 0),
                                    it('impl.x_valid', b(2, 1),
                                       it('impl.d_valid', b(2, 2), b(2, 3))))}}


def fixtures(width=4):
    doc = build(width)
    result = {f'branch_pipeline_w{width}': doc}
    for fault in FAULTS:
        bad = copy.deepcopy(doc)
        wires, nxt = bad['impl']['wires'], bad['impl']['next']
        if fault == 'no_flush':
            nxt['x_valid'] = it('i.stall', 's.x_valid', land('s.d_valid', neg('w.hazard')))
            nxt['d_valid'] = it('i.stall', 's.d_valid', it('w.hazard', 's.d_valid', True))
        elif fault == 'wrong_kill':
            # Kill the resolving branch instead of its younger D instruction.
            nxt['w_valid'] = it('i.stall', 's.w_valid', land('s.x_valid', neg('w.x_taken')))
            nxt['x_valid'] = it('i.stall', 's.x_valid', land('s.d_valid', neg('w.hazard')))
        elif fault == 'wrong_target':
            nxt['fetch_pc'][3][2] = add(ex(1, 0, 's.x_ir'), b(2, 1))
        elif fault == 'stall_branch':
            # Incorrectly perform the branch squash/redirect even while stalled.
            for key in ('x_valid', 'd_valid', 'fetch_pc'):
                nxt[key] = it('w.x_taken', nxt[key][3], nxt[key])
        elif fault == 'branch_forward':
            wires['d_operand'] = it('w.d_branch', 'w.reg_operand', wires['d_operand'])
        elif fault == 'branch_interlock':
            wires['hazard'] = land(neg('w.d_branch'), wires['hazard'])
        elif fault == 'branch_write':
            wires['w_writes'] = lor(wires['w_writes'], eq(ex(width + 4, width + 2, 's.w_ir'), b(3, 4)))
        name = f'branch_pipeline_w{width}_bad_{fault}'
        bad['name'] = name
        result[name] = bad
    return result


def main():
    for width in WIDTHS:
        for name, doc in fixtures(width).items():
            (ROOT / 'examples' / (name + '.json')).write_text(json.dumps(doc, indent=2) + '\n')
            if '_bad_' not in name:
                (ROOT / 'examples' / (name + '.lyd')).write_text(print_document(doc, infix=True))


if __name__ == '__main__':
    main()
