#!/usr/bin/env python3
"""Preserve identical branch-pipeline obligations and honest finite width limits.

The finite invocation uses an absent Z3 executable. Separate default-backend and
original-SMT Z3 runs are comparators, never a fallback from finite UNKNOWN.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import statistics
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]
WIDTHS = (4, 8, 16, 32)
FAULTS = ('no_flush', 'wrong_kill', 'wrong_target', 'stall_branch',
          'branch_forward', 'branch_interlock', 'branch_write')


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(binary, z3, mode, name, out):
    env = dict(os.environ)
    env.pop('HWVERIFY_SOLVER', None)
    if mode == 'finite':
        env['HWVERIFY_SOLVER'] = 'finite'
    cmd = [str(binary), str(ROOT / 'examples' / (name + '.json')),
           '--out', str(out), '--z3', str(z3) if mode == 'default' else '/definitely/absent/z3']
    start = time.perf_counter()
    result = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=180)
    seconds = time.perf_counter() - start
    report = json.loads(result.stdout)
    if mode == 'finite':
        assert report['engine_summary']['z3_queries'] == 0
        assert all(q['backend'] != 'z3' for q in report['obligations'])
    if mode == 'default':
        assert report['status'] == ('counterexample' if '_bad_' in name else 'stuttering_refinement_verified')
    obligations = []
    for q in report['obligations']:
        item = {k: q[k] for k in ('name', 'backend', 'status', 'solver_result', 'seconds')}
        if 'finite' in q:
            item['finite'] = {k: q['finite'][k] for k in
                              ('reason', 'terms', 'variables', 'clauses', 'decisions', 'conflicts', 'work')}
        obligations.append(item)
    row = {'mode': mode, 'case': name, 'status': report['status'], 'wall_seconds': seconds,
           'exit_code': result.returncode, 'output': str(out.relative_to(ROOT)),
           'engine_summary': report['engine_summary'], 'obligations': obligations}
    print(mode, name, report['status'], f'{seconds:.6f}s', flush=True)
    return row, report


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--binary', type=Path, required=True)
    p.add_argument('--z3', type=Path, required=True)
    p.add_argument('--repeats', type=int, default=5)
    p.add_argument('--out', type=Path, default=ROOT / 'results/branch_pipeline')
    p.add_argument('--summary', type=Path, default=ROOT / 'audit/branch_pipeline/results.json')
    args = p.parse_args()
    assert args.repeats >= 1
    binary, z3, out = args.binary.resolve(), args.z3.resolve(), args.out.resolve()
    rows, rechecks = [], []
    for repeat in range(args.repeats):
        for width in (WIDTHS if repeat % 2 == 0 else WIDTHS[::-1]):
            for mode in (('default', 'finite') if repeat % 2 == 0 else ('finite', 'default')):
                name = f'branch_pipeline_w{width}'
                folder = out / (mode if repeat == 0 else f'repeat_{repeat}/{mode}') / name
                row, _ = run(binary, z3, mode, name, folder)
                row['repeat'] = repeat
                rows.append(row)
    for width in WIDTHS:
        for fault in FAULTS:
            name = f'branch_pipeline_w{width}_bad_{fault}'
            for mode in ('default', 'finite'):
                row, _ = run(binary, z3, mode, name, out / mode / name)
                row['repeat'] = 0
                rows.append(row)
    # Recheck exactly the original finite formula, including UNKNOWN queries.
    # Unknown does not "agree" with UNSAT: preserve both verdicts explicitly.
    for row in rows:
        if row['mode'] != 'finite' or row['repeat'] != 0:
            continue
        folder = ROOT / row['output']
        report = json.loads((folder / 'report.json').read_text())
        for q in report['obligations']:
            original = (folder / q['evidence']).read_text().split('(check-sat)')[0]
            baseline = (out / 'default' / row['case'] / q['evidence']).read_text().split('(check-sat)')[0]
            assert original == baseline, (row['case'], q['name'], 'formula changed between backends')
            reply = subprocess.run([str(z3), '-in', '-smt2'], input=original + '(check-sat)\n',
                                   capture_output=True, text=True, check=True, timeout=30).stdout.strip()
            assert reply in ('sat', 'unsat', 'unknown'), reply
            if q['solver_result'] != 'unknown':
                assert reply == q['solver_result'], (row['case'], q['name'], reply, q['solver_result'])
            rechecks.append({'case': row['case'], 'obligation': q['name'],
                             'finite_result': q['solver_result'], 'independent_z3_result': reply,
                             'original_assertions_sha256': hashlib.sha256(original.encode()).hexdigest()})
    summary = []
    for width in WIDTHS:
        for mode in ('default', 'finite'):
            subset = [r for r in rows if r['case'] == f'branch_pipeline_w{width}' and r['mode'] == mode]
            times = [r['wall_seconds'] for r in subset]
            summary.append({'width': width, 'mode': mode, 'statuses': sorted({r['status'] for r in subset}),
                            'runs': len(times), 'median_wall_seconds': statistics.median(times),
                            'min_wall_seconds': min(times), 'max_wall_seconds': max(times)})
    result = {'kind': 'unbounded-in-time one-step inductive obligations over finite state widths',
              'timing_note': 'Sequential interleaved runs; primary default budgets unchanged; wall time includes I/O',
              'limits_per_query': {'timeout_ms': 10000, 'max_work': 100000000, 'max_terms': 100000,
                                   'max_variables': 200000, 'max_clauses': 1000000, 'max_depth': 512},
              'elevated_budget': 'Not run: CLI has no public work-budget knob; no source or specification changes for scaling',
              'binary_sha256': sha(binary), 'z3_sha256': sha(z3),
              'model_sha256': {p.name: sha(p) for p in sorted((ROOT / 'examples').glob('branch_pipeline*.json'))},
              'width_summary': summary, 'runs': rows, 'original_smt_rechecks': rechecks}
    args.summary.parent.mkdir(parents=True, exist_ok=True)
    args.summary.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(summary, indent=2))
    print('Original finite SMT independently checked:', len(rechecks))


if __name__ == '__main__':
    main()
