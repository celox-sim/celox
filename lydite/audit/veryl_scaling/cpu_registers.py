#!/usr/bin/env python3
"""Scale real D/X/W CPU GPRs, keeping datapath=32 and ROM/data=4 words.

The independent architectural specification and state relation below are never
translated into the DUT. The handwritten Veryl template supplies ALL DUT reset,
next-state, wire and output expressions through the pinned frontend/SIR lifter.
All registers, including r0, are ordinary writable registers in this custom ISA;
this is not RV32I. The ISA is [3-bit opcode | rd | rs | 32-bit immediate].
"""
import argparse
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'examples'))
from build_pipeline import b, eq, it, add, ex, land, lor, neg, implies, rom, memory

_spec = importlib.util.spec_from_file_location('register_pipeline_runner', ROOT / 'conformance/veryl-symbolic/run.py')
runner = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(runner)
WIDTH = 32
CAPACITY = 4
DEFAULT_SIZES = (8, 16, 32)
FAULT_NAMES = (*runner.FAULTS, 'register_read_alias', 'register_write_alias',
               'read_high_bit_drop', 'write_high_bit_drop', 'forward_high_bit_drop',
               'wrong_destination', 'missing_high_reset')


def register_bits(count):
    if count < 2 or count & (count - 1):
        raise ValueError('register count must be a power of two >= 2')
    return (count - 1).bit_length()


def balanced_and(items):
    """Association only: retain every state-relation predicate."""
    if len(items) == 1:
        return items[0]
    middle = len(items) // 2
    return ['and', balanced_and(items[:middle]), balanced_and(items[middle:])]


def model(count):
    """Independent reference/binding plus DUT state schema, not a DUT model."""
    rb = register_bits(count)
    iw = WIDTH + 2 * rb + 3
    def op(ir): return ex(iw - 1, WIDTH + 2 * rb, ir)
    def rd(ir): return ex(WIDTH + 2 * rb - 1, WIDTH + rb, ir)
    def rs(ir): return ex(WIDTH + rb - 1, WIDTH, ir)
    def imm(ir): return ex(WIDTH - 1, 0, ir)
    def isop(ir, code): return eq(op(ir), b(3, code))
    def writes(ir): return eq(ex(iw - 1, iw - 1, ir), b(1, 0))
    def uses(ir): return lor(lor(isop(ir, 1), isop(ir, 2)), isop(ir, 4))
    def read_reg(selector, values):
        value = values[-1]
        for index in reversed(range(count - 1)):
            value = it(eq(selector, b(rb, index)), values[index], value)
        return value
    def architectural_value(ir, regs, prefix):
        src = read_reg(rs(ir), regs)
        return it(isop(ir, 0), imm(ir),
                  it(isop(ir, 1), add(src, imm(ir)),
                     it(isop(ir, 2), ['bxor', src, imm(ir)],
                        memory(prefix, ex(1, 0, ir)))))
    def architectural_pc(ir, pc, regs):
        taken = land(isop(ir, 4), eq(read_reg(rs(ir), regs), b(WIDTH, 0)))
        return it(taken, ex(1, 0, ir), add(pc, b(2, 1)))

    immutable = {**{f'rom{i}': {'bv': iw} for i in range(CAPACITY)},
                 **{f'data{i}': {'bv': WIDTH} for i in range(CAPACITY)}}
    registers = {f'r{i}': {'bv': WIDTH} for i in range(count)}
    arch = {'pc': {'bv': 2}, **registers, **immutable}
    reset = {'pc': b(2, 0), **{k: b(WIDTH, 0) for k in registers},
             **{k: 'i.' + k for k in immutable}}
    regs = ['s.' + k for k in registers]
    spec = {'state': copy.deepcopy(arch), 'reset': reset,
            'wires': {'ir': rom('s.', 's.pc'),
                      'result': architectural_value('w.ir', regs, 's.')},
            'next': {'pc': architectural_pc('w.ir', 's.pc', regs),
                     **{f'r{i}': it(land(writes('w.ir'), eq(rd('w.ir'), b(rb, i))),
                                   'w.result', f's.r{i}') for i in range(count)},
                     **{k: 's.' + k for k in immutable}},
            'outputs': {'can_step': True}}
    pipeline = {'fetch_pc': {'bv': 2}}
    for stage in ('d', 'x', 'w'):
        pipeline.update({f'{stage}_valid': 'bool', f'{stage}_pc': {'bv': 2},
                         f'{stage}_ir': {'bv': iw}})
    pipeline.update({'x_operand': {'bv': WIDTH}, 'w_result': {'bv': WIDTH},
                     'w_next_pc': {'bv': 2}})
    a = [f'impl.r{i}' for i in range(count)]
    pending_w = [it(land('impl.w_valid', writes('impl.w_ir'), eq(rd('impl.w_ir'), b(rb, i))),
                    'impl.w_result', a[i]) for i in range(count)]
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
    return {'version': 2, 'name': f'branch_pipeline_32bit_{count}gpr',
            'inputs': {'rst': 'bool', 'stall': 'bool', **immutable},
            'reset_input': 'rst', 'spec': spec,
            # Deliberately incomplete until run_case imports the compiled DUT.
            'impl': {'state': {**copy.deepcopy(arch), **pipeline}},
            'binding': balanced_and(relation), 'commit': 'commit', 'can_step': 'can_step',
            'hold_when': 'i.stall',
            'progress': {'enabled': neg('i.stall'),
                         'rank': it('impl.w_valid', b(2, 0),
                                    it('impl.x_valid', b(2, 1),
                                       it('impl.d_valid', b(2, 2), b(2, 3))))}}


def source(count):
    rb = register_bits(count)
    values = {'W': WIDTH, 'IW': WIDTH + 2 * rb + 3, 'HI': WIDTH + 2 * rb + 2,
              'OPLO': WIDTH + 2 * rb, 'RB': rb, 'RSHI': WIDTH + rb - 1,
              'RSLO': WIDTH, 'RDHI': WIDTH + 2 * rb - 1, 'RDLO': WIDTH + rb,
              'REGISTER_PORTS': '\n    '.join(f'r{i}: output bit<32>,' for i in range(count)),
              'REGISTER_RESET': '\n            '.join(f"r{i} = '0;" for i in range(count)),
              'REGISTER_READ': '\n            '.join(
                  [f"{rb}'d{i}: operand = r{i};" for i in range(count - 1)] +
                  [f'default: operand = r{count - 1};']),
              'REGISTER_WRITE': '\n                        '.join(
                  [f"{rb}'d{i}: r{i} = w_result;" for i in range(count - 1)] +
                  [f'default: r{count - 1} = w_result;'])}
    text = (ROOT / 'conformance/veryl-symbolic/pipeline_registers.veryl.in').read_text()
    for key, value in values.items():
        text = text.replace('@' + key + '@', str(value))
    # Retain @IMMHI@ until the shared runner applies its wrong-ADD source mutant.
    return text


def faults(count):
    rb = register_bits(count)
    result = dict(runner.FAULTS)
    result['missing_reset'] = ("r0 = '0;", '')
    result['missing_high_reset'] = (f"r{count - 1} = '0;", '')
    result['register_read_alias'] = (f'default: operand = r{count - 1};',
                                     'default: operand = r0;')
    result['register_write_alias'] = (f'default: r{count - 1} = w_result;',
                                      'default: r0 = w_result;')
    result['read_high_bit_drop'] = ('case d_rs {', f"case d_rs & {rb}'d{count // 2 - 1} {{")
    result['write_high_bit_drop'] = ('case w_rd {', f"case w_rd & {rb}'d{count // 2 - 1} {{")
    result['forward_high_bit_drop'] = ('x_rd == d_rs;',
        f"(x_rd & {rb}'d{count // 2 - 1}) == (d_rs & {rb}'d{count // 2 - 1});")
    result['wrong_destination'] = (f'w_rd = w_ir[{WIDTH + 2 * rb - 1}:{WIDTH + rb}];',
        f"w_rd = w_ir[{WIDTH + 2 * rb - 1}:{WIDTH + rb}] + {rb}'d1;")
    return result


def validate_report(report, fault):
    """Require actual verdicts, not just self-reported per-obligation success."""
    return runner.validate_report(report, fault)


def run_case(count, fault, out, frontend, lifter, checker):
    """Compile/import each source independently; fail closed on every obligation."""
    out.mkdir(parents=True, exist_ok=False)
    text = source(count)
    if fault:
        old, new = faults(count)[fault]
        if text.count(old) != 1:
            raise RuntimeError(f'mutation {fault} does not have exactly one source site')
        text = text.replace(old, new)
    text = text.replace('@IMMHI@', str(WIDTH - 1))
    (out / 'pipeline.veryl').write_text(text)
    runner.write_json(out / 'design.json', {'top': 'PipelineCPU', 'four_state': False,
        'sources': [{'path': 'pipeline.veryl', 'text': text}]})
    timings = {}
    def execute(args, output, allowed_codes=(0,)):
        started = time.monotonic()
        result = subprocess.run([str(x) for x in args], text=True, capture_output=True,
                                timeout=180, env={**os.environ, 'LYDITE_SOLVER': 'finite'})
        timings[output] = time.monotonic() - started
        runner.write_json(out / 'phase-seconds.json', timings)
        (out / output).write_text(result.stdout)
        (out / (output + '.stderr')).write_text(result.stderr)
        if result.returncode not in allowed_codes:
            raise RuntimeError(f'{args[0]} exit {result.returncode}: {result.stderr[:2000]}')
        return json.loads(result.stdout)
    compiled = execute([frontend, out / 'design.json'], 'compiled.json')
    if compiled.get('allowed_diagnostics') != []:
        raise RuntimeError('pipeline requires a frontend diagnostic waiver')
    doc = model(count)
    config = {'event': 'clk', 'inputs': {
        'rst_n': {'name': 'rst', 'type': 'bool', 'expr': ['not', 'i.rst']},
        'stall': {'type': 'bool'}},
        'state': {k: {'type': v} for k, v in doc['impl']['state'].items()},
        'outputs': {'commit': {'signal': 'commit', 'type': 'bool'}}}
    for name, ty in doc['inputs'].items():
        if name not in ('rst', 'stall'):
            config['inputs']['seed_' + name] = {'name': name, 'type': ty}
    for mode, rst in [('normal', False), ('reset', True)]:
        cfg = copy.deepcopy(config)
        cfg['overrides'] = {'rst': rst}
        runner.write_json(out / (mode + '-bindings.json'), cfg)
        lifted = execute([lifter, out / 'compiled.json', out / (mode + '-bindings.json')]
                         + (['--inline'] if rst else []), mode + '-lift.json')
        if rst:
            unresolved = [k for k, v in lifted['next'].items()
                          if runner.references(v, 's.') or runner.references(v, 'w.')]
            if unresolved:
                missing = 'r0' if fault == 'missing_reset' else f'r{count - 1}'
                if fault not in ('missing_reset', 'missing_high_reset') or unresolved != [missing]:
                    raise RuntimeError(f'reset depends on arbitrary prestate: {unresolved}')
                result = {'status': 'reset_rejected', 'state_dependent_reset': unresolved,
                          'source_sha256': hashlib.sha256(text.encode()).hexdigest()}
                runner.write_json(out / 'summary.json', result)
                return result
            doc['impl']['reset'] = lifted['next']
        else:
            for key in ('next', 'outputs', 'wires'):
                doc['impl'][key] = lifted[key]
    if fault in ('missing_reset', 'missing_high_reset'):
        raise RuntimeError('missing reset was not detected')
    if fault:
        doc['name'] += '_bad_' + fault
    runner.write_json(out / 'refinement.json', doc)
    started = time.monotonic()
    report = execute([checker, out / 'refinement.json', '--out', out / 'proof'],
                     'report.json', (0, 1, 3))
    failed = validate_report(report, fault)
    result = {'status': report['status'], 'seconds': time.monotonic() - started,
              'source_sha256': hashlib.sha256(text.encode()).hexdigest(),
              'failed_obligations': [o['name'] for o in failed],
              'engine_summary': report['engine_summary']}
    runner.write_json(out / 'summary.json', result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--sizes', type=int, nargs='+', default=DEFAULT_SIZES)
    parser.add_argument('--faults', nargs='*', choices=FAULT_NAMES, default=list(FAULT_NAMES))
    parser.add_argument('--require-success', action='store_true')
    parser.add_argument('--frontend', type=Path, default=ROOT / 'conformance/veryl-proof/target/debug/veryl-proof-frontend')
    parser.add_argument('--lifter', type=Path, default=ROOT / '../target/release/lydite-sir-lift')
    parser.add_argument('--checker', type=Path, default=ROOT / '../target/release/lydite')
    args = parser.parse_args()
    for count in args.sizes:
        register_bits(count)
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    results = []
    for count in args.sizes:
        for fault in [None, *args.faults]:
            out = args.out / (f'cpu_{count}gpr' + ('_bad_' + fault if fault else ''))
            accepted = False
            try:
                result = run_case(count, fault, out, args.frontend.resolve(),
                                  args.lifter.resolve(), args.checker.resolve())
                expected = ('stuttering_refinement_verified' if fault is None else
                            'reset_rejected' if fault in ('missing_reset', 'missing_high_reset') else 'counterexample')
                accepted = result['status'] == expected
            except RuntimeError as error:
                result = {'status': 'error', 'error': str(error)}
            report_path = out / 'report.json'
            report = json.loads(report_path.read_text()) if report_path.exists() else {}
            if report:
                result['status'] = report.get('status', result['status'])
                result['engine_summary'] = report.get('engine_summary', {})
            phase_path = out / 'phase-seconds.json'
            if phase_path.exists():
                result['phase_seconds'] = json.loads(phase_path.read_text())
            result.update(registers=count, register_bits=register_bits(count),
                          instruction_bits=WIDTH + 2 * register_bits(count) + 3,
                          datapath_bits=WIDTH, rom_words=CAPACITY, data_words=CAPACITY,
                          stages='D/X/W', fault=fault, correct_outcome=accepted,
                          microstep=next((o for o in report.get('obligations', [])
                                          if o['name'] == 'microstep_refinement'), {}))
            results.append(result)
            runner.write_json(args.out / 'summary.json', results)
            print(json.dumps({k: result.get(k) for k in
                              ('registers', 'fault', 'status', 'seconds', 'correct_outcome', 'error')}), flush=True)
    if args.require_success and any(not r['correct_outcome'] for r in results):
        raise SystemExit('CPU register regression failed')


if __name__ == '__main__':
    main()
