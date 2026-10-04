#!/usr/bin/env python3
"""Interleaved release timings, preserving identical original SMT obligations.

No historical evidence is overwritten. Each trial preserves a complete report.
The finite backend always receives an intentionally missing Z3 executable.
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
CASES = ('pipeline', 'pipeline_bad_forward', 'pipeline_bad_priority',
         'pipeline_bad_interlock', 'pipeline_bad_retire', 'pipeline_bad_stall')


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--baseline', type=Path, required=True)
    p.add_argument('--candidate', type=Path, required=True)
    p.add_argument('--z3', type=Path)
    p.add_argument('--repeats', type=int, default=7)
    p.add_argument('--out', type=Path, default=ROOT / 'results/pipeline_solver_speed/repeated')
    args = p.parse_args()
    assert args.repeats >= 3
    args.out = args.out.resolve()
    assert not (args.out / 'summary.json').exists(), 'Use a fresh output directory'
    binaries = {'baseline': args.baseline.resolve(), 'candidate': args.candidate.resolve()}
    if args.z3:
        binaries['default_z3'] = binaries['candidate']
    hashes = {name: sha(exe) for name, exe in binaries.items()}
    rows = []
    expected_smt = {}
    reference_diagnostics = {}
    for run in range(args.repeats):
        for case in CASES:
            modes = list(binaries)
            if run % 2:
                modes.reverse()
            for mode in modes:
                out = args.out / f'{run:02d}' / mode / case
                env = dict(os.environ)
                env.pop('LYDITE_SOLVER', None)
                if mode != 'default_z3':
                    env['LYDITE_SOLVER'] = 'finite'
                start = time.perf_counter()
                proc = subprocess.run(
                    [str(binaries[mode]), str(ROOT / 'examples' / f'{case}.json'),
                     '--out', str(out), '--z3',
                     str(args.z3.resolve()) if mode == 'default_z3' else '/definitely/absent/z3'],
                    env=env, capture_output=True, text=True)
                wall = time.perf_counter() - start
                report = json.loads(proc.stdout)
                expected = 'stuttering_refinement_verified' if case == 'pipeline' else 'counterexample'
                assert report['status'] == expected, (mode, case, report)
                if mode != 'default_z3':
                    assert report['engine_summary']['z3_queries'] == 0
                diagnostics = {}
                for obligation in report['obligations']:
                    key = (case, obligation['name'])
                    smt = (out / obligation['evidence']).read_text().split('(check-sat)')[0]
                    digest = hashlib.sha256(smt.encode()).hexdigest()
                    if key in expected_smt:
                        assert expected_smt[key] == digest, (key, mode, 'original SMT changed')
                    expected_smt[key] = digest
                    diag = out / f"{obligation['name']}.finite.json"
                    if diag.exists():
                        d = json.loads(diag.read_text())
                        diagnostics[obligation['name']] = {
                            k: d[k] for k in ('solver_result', 'terms', 'variables', 'clauses',
                                              'decisions', 'conflicts', 'work')}
                        # Heap ties and watch compaction preserve exact search
                        # behavior for this suite, beyond mere verdict agreement.
                        stable = {k: v for k, v in d.items() if k != 'work'}
                        if key in reference_diagnostics:
                            assert stable == reference_diagnostics[key], (key, mode, 'search/witness changed')
                        reference_diagnostics[key] = stable
                row = dict(run=run, mode=mode, case=case, wall_seconds=wall,
                           exit_code=proc.returncode, status=report['status'],
                           engine_summary=report['engine_summary'], diagnostics=diagnostics)
                rows.append(row)
                print(f'{run} {mode} {case} {wall:.6f}s', flush=True)
    summary = []
    for case in CASES:
        case_rows = {}
        for mode in binaries:
            times = [r['wall_seconds'] for r in rows if r['case'] == case and r['mode'] == mode]
            case_rows[mode] = dict(median_seconds=statistics.median(times),
                                   min_seconds=min(times), max_seconds=max(times), runs=len(times))
        summary.append(dict(case=case, timings=case_rows,
                            speedup=case_rows['baseline']['median_seconds'] /
                                    case_rows['candidate']['median_seconds']))
    assert hashes == {name: sha(exe) for name, exe in binaries.items()}
    artifact = dict(note='Sequential interleaved wall timings on one host; not a general speed guarantee',
                    binaries={k: dict(path=str(v), sha256=hashes[k]) for k, v in binaries.items()},
                    original_smt_sha256={f'{c}/{q}': v for (c, q), v in expected_smt.items()},
                    summary=summary, runs=rows)
    (args.out / 'summary.json').write_text(json.dumps(artifact, indent=2) + '\n')
    print(json.dumps(summary, indent=2))


if __name__ == '__main__':
    main()
