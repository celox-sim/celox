#!/usr/bin/env python3
"""Compare preserved obligations under default and opt-in finite backends.

Rechecking the finite backend's ORIGINAL SMT with Z3 is independent audit work,
not part of (and not a hidden dependency of) the finite verification invocation.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time
ROOT = Path(__file__).resolve().parents[2]


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--binary', type=Path, required=True)
    p.add_argument('--z3', type=Path, required=True)
    p.add_argument('--out', type=Path, default=ROOT / 'results/pipeline_0_10')
    args = p.parse_args()
    binary, z3 = args.binary.resolve(), args.z3.resolve()
    rows, rechecks = [], []
    for mode in ('default', 'finite'):
        for name in ('pipeline', 'pipeline_bad_forward', 'pipeline_bad_priority',
                     'pipeline_bad_interlock', 'pipeline_bad_retire', 'pipeline_bad_stall'):
            out = args.out / mode / name
            env = dict(os.environ)
            env.pop('HWVERIFY_SOLVER', None)
            if mode == 'finite': env['HWVERIFY_SOLVER'] = 'finite'
            cmd = [str(binary), str(ROOT / 'examples' / (name + '.json')),
                   '--out', str(out), '--z3', str(z3) if mode == 'default' else '/definitely/absent/z3']
            start = time.perf_counter()
            result = subprocess.run(cmd, env=env, capture_output=True, text=True)
            seconds = time.perf_counter() - start
            report = json.loads(result.stdout)
            expected = 'stuttering_refinement_verified' if name == 'pipeline' else 'counterexample'
            assert report['status'] == expected, (mode, name, report)
            if mode == 'finite':
                assert report['engine_summary']['z3_queries'] == 0
                assert all(q['backend'] != 'z3' for q in report['obligations'])
            row = {'mode': mode, 'case': name, 'status': report['status'],
                   'wall_seconds': seconds, 'exit_code': result.returncode,
                   'engine_summary': report['engine_summary'],
                   'obligations': [{'name': q['name'], 'backend': q['backend'],
                                    'status': q['status'], 'solver_result': q['solver_result']}
                                   for q in report['obligations']]}
            rows.append(row)
            print(mode, name, report['status'], f'{seconds:.3f}s', flush=True)
            if mode == 'finite':
                baseline = args.out / 'default' / name
                for q in report['obligations']:
                    evidence = out / q['evidence']
                    text = evidence.read_text()
                    # SAT models may use a backend-specific witness capture suffix.
                    original = text.split('(check-sat)')[0]
                    previous = (baseline / q['evidence']).read_text().split('(check-sat)')[0]
                    assert original == previous, (name, q['name'], 'obligation changed')
                    answer = subprocess.run([str(z3), '-in', '-smt2'], input=original + '(check-sat)\n',
                                            capture_output=True, text=True, check=True).stdout.strip()
                    assert answer == q['solver_result'], (name, q['name'], answer, q['solver_result'])
                    rechecks.append({'case': name, 'obligation': q['name'], 'result': answer})
    report = {'timing_note': 'One sequential run per mode/case; not a statistical speed claim',
              'runs': rows, 'original_smt_rechecks': rechecks}
    (ROOT / 'audit/pipeline/results.json').write_text(json.dumps(report, indent=2) + '\n')
    print('All original finite-backend obligations independently rechecked:', len(rechecks))


if __name__ == '__main__': main()
