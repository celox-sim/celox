#!/usr/bin/env python3
"""Compile handwritten Veryl, lift arbitrary symbols, check ISA refinement.

The ISA/binding come from examples/build_branch_pipeline.py. Implementation
reset/next/output expressions come exclusively from the compiled Veryl SIR.
No DUT observations, simulator or expected values enter the lifting process.
"""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'examples'))
from build_branch_pipeline import build

FAULTS = {
    'no_flush': ('x_valid = d_valid && !hazard && !taken;', 'x_valid = d_valid && !hazard;'),
    'wrong_target': ('if taken { fetch_pc = x_ir[1:0]; }', 'if taken { fetch_pc = x_ir[1:0] + 2\'d1; }'),
    'no_forward': ("if x_match && x_op != 3'd3 { operand = result; }", "if 1'b0 { operand = result; }"),
    'no_interlock': ('&& x_match && x_op == 3\'d3;', "&& x_match && 1'b0;"),
    'wrong_add': ('result = x_operand + x_ir[@IMMHI@:0];', 'result = x_operand ^ x_ir[@IMMHI@:0];'),
    'missing_reset': ("pc = '0; r0 = '0; r1 = '0; fetch_pc = '0;", "pc = '0; r1 = '0; fetch_pc = '0;"),
}


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')


def references(expr, prefix):
    if isinstance(expr, str):
        return expr.startswith(prefix)
    if isinstance(expr, list):
        return any(references(x, prefix) for x in expr)
    return False


OBLIGATIONS = {
    'binding_nonempty', 'reset_binding', 'microstep_refinement', 'commit_eligible',
    'hold_contract', 'progress_nonvacuity', 'commit_reachable_in_relation',
    'noncommit_rank_decreases',
}


EXISTENCE_OBLIGATIONS = {
    'binding_nonempty', 'progress_nonvacuity', 'commit_reachable_in_relation',
}
EXPECTED_RESULTS = {name: 'sat' if name in EXISTENCE_OBLIGATIONS else 'unsat'
                    for name in OBLIGATIONS}

def validate_report(report, fault):
    obligations = report['obligations']
    names = [o['name'] for o in obligations]
    if len(names) != len(OBLIGATIONS) or set(names) != OBLIGATIONS:
        raise RuntimeError('missing, duplicate or unexpected proof obligations')
    if any(o.get('solver_result') == 'unknown' for o in obligations):
        raise RuntimeError('UNKNOWN is not a success or a negative control')
    if report['engine_summary']['z3_queries'] != 0:
        raise RuntimeError('unexpected external solver use')
    for obligation in obligations:
        expected = ('sat' if fault and obligation['name'] == 'microstep_refinement'
                    else EXPECTED_RESULTS[obligation['name']])
        if obligation.get('solver_result') != expected:
            raise RuntimeError(f"{obligation['name']}: missing or contradictory solver result")
        if expected == 'sat' and obligation.get('finite', {}).get('original_formula_validated') is not True:
            raise RuntimeError(f"{obligation['name']}: SAT lacks original-formula validation")
    failed = [o for o in obligations if o['status'] != 'passed']
    if fault:
        witnesses = [o for o in failed if o['name'] == 'microstep_refinement'
                     and o.get('solver_result') == 'sat'
                     and o.get('finite', {}).get('original_formula_validated') is True]
        if report['status'] != 'counterexample' or not witnesses:
            raise RuntimeError(f'{fault}: no original-formula-validated counterexample')
        if any(o['name'] != 'microstep_refinement' for o in failed):
            raise RuntimeError(f'{fault}: unexpected secondary obligation failure')
    elif failed or report['status'] != 'stuttering_refinement_verified':
        raise RuntimeError(f'correct CPU failed: {[o["name"] for o in failed]}')
    return failed


def run_case(width, fault, out, frontend, lifter, checker):
    out.mkdir(parents=True, exist_ok=False)
    source = Path(__file__).with_name('pipeline.veryl.in').read_text()
    if fault:
        old, new = FAULTS[fault]
        if source.count(old) != 1:
            raise RuntimeError(f'mutation {fault} does not have exactly one source site')
        source = source.replace(old, new)
    for key, value in {'W': width, 'IW': width+5, 'HI': width+4,
                       'OPLO': width+2, 'RD': width+1, 'IMMHI': width-1}.items():
        source = source.replace('@'+key+'@', str(value))
    (out / 'pipeline.veryl').write_text(source)
    write_json(out / 'design.json', {'top': 'PipelineCPU', 'four_state': False,
        'sources': [{'path': 'pipeline.veryl', 'text': source}]})
    timings = {}
    def execute(args, output, allowed_codes=(0,)):
        started = time.monotonic()
        result = subprocess.run([str(x) for x in args], text=True, capture_output=True,
                                timeout=180, env={**os.environ, 'LYDITE_SOLVER': 'finite'})
        timings[output] = time.monotonic() - started
        write_json(out / 'phase-seconds.json', timings)
        (out / output).write_text(result.stdout)
        (out / (output + '.stderr')).write_text(result.stderr)
        if result.returncode not in allowed_codes:
            raise RuntimeError(f'{args[0]} exit {result.returncode}: {result.stderr[:2000]}')
        return json.loads(result.stdout)
    compiled = execute([frontend, out / 'design.json'], 'compiled.json')
    if compiled.get('allowed_diagnostics') != []:
        raise RuntimeError('pipeline fixture unexpectedly requires a frontend diagnostic waiver')
    doc = build(width)
    config = {'event': 'clk', 'inputs': {
        'rst_n': {'name': 'rst', 'type': 'bool', 'expr': ['not', 'i.rst']},
        'stall': {'type': 'bool'}},
        'state': {k: {'type': v} for k, v in doc['impl']['state'].items()},
        'outputs': {'commit': {'signal': 'commit', 'type': 'bool'}}}
    for name, ty in doc['inputs'].items():
        if name not in ('rst', 'stall'):
            config['inputs']['seed_'+name] = {'name': name, 'type': ty}
    for mode, rst in [('normal', False), ('reset', True)]:
        cfg = copy.deepcopy(config)
        cfg['overrides'] = {'rst': rst}
        write_json(out / (mode+'-bindings.json'), cfg)
        lifted = execute([lifter, out / 'compiled.json', out / (mode+'-bindings.json')]
                         + (['--inline'] if rst else []), mode+'-lift.json')
        if rst:
            # v2 reset denotes a reset-established state, not a relation over
            # uninitialized hardware. Reject residual prestate conservatively.
            unresolved = [k for k, v in lifted['next'].items()
                          if references(v, 's.') or references(v, 'w.')]
            if unresolved:
                if fault != 'missing_reset' or unresolved != ['r0']:
                    raise RuntimeError(f'reset depends on arbitrary prestate: {unresolved}')
                result = {'width': width, 'fault': fault, 'status': 'reset_rejected',
                          'state_dependent_reset': unresolved}
                write_json(out / 'summary.json', result)
                return result
            doc['impl']['reset'] = lifted['next']
        else:
            for key in ('next', 'outputs', 'wires'):
                doc['impl'][key] = lifted[key]
    if fault == 'missing_reset':
        raise RuntimeError('missing reset was not detected')
    doc['name'] = f'veryl_branch_pipeline_w{width}' + ('_bad_'+fault if fault else '')
    write_json(out / 'refinement.json', doc)
    started = time.monotonic()
    report = execute([checker, out / 'refinement.json', '--out', out / 'proof'], 'report.json', (0, 1))
    failed = validate_report(report, fault)
    result = {'width': width, 'fault': fault, 'status': report['status'],
              'seconds': time.monotonic()-started,
              'source_sha256': hashlib.sha256(source.encode()).hexdigest(),
              'failed_obligations': [o['name'] for o in failed],
              'engine_summary': report['engine_summary']}
    write_json(out / 'summary.json', result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--widths', nargs='+', type=int, default=[4, 8, 16, 32])
    parser.add_argument('--faults', nargs='*', choices=FAULTS, default=list(FAULTS))
    parser.add_argument('--frontend', type=Path, default=ROOT/'conformance/veryl-proof/target/debug/veryl-proof-frontend')
    parser.add_argument('--lifter', type=Path, default=ROOT/'../target/debug/lydite-sir-lift')
    parser.add_argument('--checker', type=Path, default=ROOT/'../target/release/lydite')
    args = parser.parse_args()
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    results = []
    for width in args.widths:
        for fault in [None, *args.faults]:
            name = f'w{width}' + ('_bad_'+fault if fault else '')
            result = run_case(width, fault, args.out/name, args.frontend.resolve(),
                              args.lifter.resolve(), args.checker.resolve())
            results.append(result)
            print(json.dumps(result), flush=True)
    write_json(args.out/'summary.json', results)

if __name__ == '__main__':
    main()
