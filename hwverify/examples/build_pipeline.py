#!/usr/bin/env python3
"""Generate a finite, genuinely overlapping D/X/W in-order CPU refinement.

Only a model generator, not a verifier. Spec execution and implementation
forwarding are written separately; audit/pipeline/simulate.py is an integer ISA
oracle that does not use either datapath expression to obtain expected results.
"""
import copy
import json
import sys
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
from json_to_hwv import print_document


def b(w, n): return ['bv', w, n]
def eq(a, z): return ['eq', a, z]
def it(c, a, z): return ['ite', c, a, z]
def add(a, z): return ['add', a, z]
def ex(hi, lo, a): return ['extract', hi, lo, a]
def land(*args):
    r = True
    for a in args: r = ['and', r, a]
    return r
def lor(a, z): return ['or', a, z]
def neg(a): return ['not', a]
def implies(a, z): return ['implies', a, z]
def mux(index, values):
    r = values[-1]
    for i in reversed(range(len(values) - 1)):
        r = it(eq(index, b(2, i)), values[i], r)
    return r

def read_reg(rs, regs): return it(eq(rs, b(1, 0)), regs[0], regs[1])
def rom(prefix, pc): return mux(pc, [prefix + 'rom' + str(i) for i in range(4)])
def memory(prefix, addr): return mux(addr, [prefix + 'data' + str(i) for i in range(4)])
def op(ir): return ex(7, 6, ir)
def rd(ir): return ex(5, 5, ir)
def rs(ir): return ex(4, 4, ir)
def imm(ir): return ex(3, 0, ir)
def uses(ir): return lor(eq(op(ir), b(2, 1)), eq(op(ir), b(2, 2)))


def architectural_value(ir, regs, prefix):
    """ISA expression, also used to state each pending instruction's meaning."""
    src = read_reg(rs(ir), regs)
    return it(eq(op(ir), b(2, 0)), imm(ir),
              it(eq(op(ir), b(2, 1)), add(src, imm(ir)),
                 it(eq(op(ir), b(2, 2)), ['bxor', src, imm(ir)],
                    memory(prefix, ex(1, 0, ir)))))


def build():
    immutable = {**{f'rom{i}': {'bv': 8} for i in range(4)},
                 **{f'data{i}': {'bv': 4} for i in range(4)}}
    arch = {'pc': {'bv': 2}, 'r0': {'bv': 4}, 'r1': {'bv': 4}, **immutable}
    reset = {'pc': b(2, 0), 'r0': b(4, 0), 'r1': b(4, 0),
             **{k: 'i.' + k for k in immutable}}
    spec = {'state': copy.deepcopy(arch), 'reset': copy.deepcopy(reset),
            'wires': {'ir': rom('s.', 's.pc'),
                      'result': architectural_value('w.ir', ['s.r0', 's.r1'], 's.')},
            'next': {'pc': add('s.pc', b(2, 1)),
                     'r0': it(eq(rd('w.ir'), b(1, 0)), 'w.result', 's.r0'),
                     'r1': it(eq(rd('w.ir'), b(1, 1)), 'w.result', 's.r1'),
                     **{k: 's.' + k for k in immutable}},
            'outputs': {'can_step': True}}
    pipeline = {'fetch_pc': {'bv': 2}}
    for stage in ('d', 'x', 'w'):
        pipeline.update({f'{stage}_valid': 'bool', f'{stage}_pc': {'bv': 2},
                         f'{stage}_ir': {'bv': 8}})
    pipeline.update({'x_operand': {'bv': 4}, 'w_result': {'bv': 4}})
    pipeline_reset = {k: False if v == 'bool' else b(v['bv'], 0)
                      for k, v in pipeline.items()}
    wires = {
        'commit': land(neg('i.stall'), 's.w_valid'),
        'd_uses': uses('s.d_ir'), 'd_rs': rs('s.d_ir'),
        'x_rd': rd('s.x_ir'), 'w_rd': rd('s.w_ir'),
        'x_load': eq(op('s.x_ir'), b(2, 3)),
        'x_match': land('s.x_valid', eq('w.x_rd', 'w.d_rs')),
        'w_match': land('s.w_valid', eq('w.w_rd', 'w.d_rs')),
        'hazard': land('s.d_valid', 'w.d_uses', 'w.x_match', 'w.x_load'),
        'x_forward': land('w.x_match', neg('w.x_load')),
        'reg_operand': it(eq('w.d_rs', b(1, 1)), 's.r1', 's.r0'),
        'w_operand': it('w.w_match', 's.w_result', 'w.reg_operand'),
        'd_operand': it('w.x_forward', 'w.x_result', 'w.w_operand'),
        # Independent implementation datapath: decode by low two opcode bits.
        'x_result': it(eq(op('s.x_ir'), b(2, 3)),
                       memory('s.', ex(1, 0, 's.x_ir')),
                       it(eq(op('s.x_ir'), b(2, 2)),
                          ['bxor', 's.x_operand', imm('s.x_ir')],
                          it(eq(op('s.x_ir'), b(2, 1)),
                             add('s.x_operand', imm('s.x_ir')), imm('s.x_ir')))),
        'fetched': rom('s.', 's.fetch_pc'),
    }
    next_state = {k: 's.' + k for k in immutable}
    next_state.update({
        'pc': it('s.w_valid', add('s.pc', b(2, 1)), 's.pc'),
        'r0': it(land('s.w_valid', eq('w.w_rd', b(1, 0))), 's.w_result', 's.r0'),
        'r1': it(land('s.w_valid', eq('w.w_rd', b(1, 1))), 's.w_result', 's.r1'),
        'w_valid': 's.x_valid', 'w_pc': 's.x_pc', 'w_ir': 's.x_ir',
        'w_result': 'w.x_result',
        'x_valid': land('s.d_valid', neg('w.hazard')),
        'x_pc': 's.d_pc', 'x_ir': 's.d_ir', 'x_operand': 'w.d_operand',
        'd_valid': it('w.hazard', 's.d_valid', True),
        'd_pc': it('w.hazard', 's.d_pc', 's.fetch_pc'),
        'd_ir': it('w.hazard', 's.d_ir', 'w.fetched'),
        'fetch_pc': it('w.hazard', 's.fetch_pc', add('s.fetch_pc', b(2, 1))),
    })
    implementation = {'state': {**copy.deepcopy(arch), **pipeline},
                      'reset': {**copy.deepcopy(reset), **pipeline_reset},
                      'wires': wires,
                      'next': {k: it('i.stall', 's.' + k, v) for k, v in next_state.items()},
                      'outputs': {'commit': 'w.commit'}}
    # Relation is about current state only. No execution observations or current
    # inputs are assumptions. Invalid stages carry unconstrained payload bits.
    a = ['impl.r0', 'impl.r1']
    pending_w = [it(land('impl.w_valid', eq(rd('impl.w_ir'), b(1, i))),
                    'impl.w_result', a[i]) for i in range(2)]
    pc_x = add('impl.pc', it('impl.w_valid', b(2, 1), b(2, 0)))
    pc_d = add(pc_x, it('impl.x_valid', b(2, 1), b(2, 0)))
    pc_f = add(pc_d, it('impl.d_valid', b(2, 1), b(2, 0)))
    relation = [eq('spec.' + k, 'impl.' + k) for k in arch]
    relation += [
        implies('impl.w_valid', land(eq('impl.w_pc', 'impl.pc'),
            eq('impl.w_ir', rom('impl.', 'impl.w_pc')),
            eq('impl.w_result', architectural_value('impl.w_ir', a, 'impl.')))),
        implies('impl.x_valid', land(eq('impl.x_pc', pc_x),
            eq('impl.x_ir', rom('impl.', 'impl.x_pc')),
            implies(uses('impl.x_ir'), eq('impl.x_operand', read_reg(rs('impl.x_ir'), pending_w))))),
        implies('impl.d_valid', land(eq('impl.d_pc', pc_d),
            eq('impl.d_ir', rom('impl.', 'impl.d_pc')))),
        eq('impl.fetch_pc', pc_f),
    ]
    return {'version': 2, 'name': 'D/X/W in-order pipeline with load-use interlock',
            'inputs': {'rst': 'bool', 'stall': 'bool', **immutable},
            'reset_input': 'rst', 'spec': spec, 'impl': implementation,
            'binding': land(*relation), 'commit': 'commit', 'can_step': 'can_step',
            'hold_when': 'i.stall',
            'progress': {'enabled': neg('i.stall'),
                         'rank': it('impl.w_valid', b(2, 0),
                                    it('impl.x_valid', b(2, 1),
                                       it('impl.d_valid', b(2, 2), b(2, 3))))}}


def fixtures():
    doc = build()
    result = {'pipeline': doc}
    bad = copy.deepcopy(doc)
    bad['impl']['wires']['d_operand'] = 'w.reg_operand'
    result['pipeline_bad_forward'] = bad
    bad = copy.deepcopy(doc)
    bad['impl']['wires']['d_operand'] = it('w.w_match', 's.w_result',
                                         it('w.x_forward', 'w.x_result', 'w.reg_operand'))
    result['pipeline_bad_priority'] = bad
    bad = copy.deepcopy(doc)
    bad['impl']['wires']['hazard'] = False
    result['pipeline_bad_interlock'] = bad
    bad = copy.deepcopy(doc)
    bad['impl']['next']['r0'] = it('i.stall', 's.r0',
        it(land('s.w_valid', eq('w.w_rd', b(1, 0))), 'w.x_result', 's.r0'))
    result['pipeline_bad_retire'] = bad
    bad = copy.deepcopy(doc)
    bad['impl']['next']['d_ir'] = 'w.fetched'
    result['pipeline_bad_stall'] = bad
    for name, model in result.items():
        model['name'] = name
    return result


def main():
    for name, doc in fixtures().items():
        (ROOT / 'examples' / (name + '.json')).write_text(json.dumps(doc, indent=2) + '\n')
        if name == 'pipeline':
            (ROOT / 'examples' / (name + '.hwv')).write_text(print_document(doc, infix=True))


if __name__ == '__main__': main()
