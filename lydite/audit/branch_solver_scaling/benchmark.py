#!/usr/bin/env python3
"""Frozen, interleaved release comparison with unchanged original assertions.

Both finite executables receive an absent external-solver path. Full evidence is
preserved per run; UNKNOWN is a resource outcome, never counted as a proof.
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
GOOD = tuple(f'branch_pipeline_w{w}' for w in (4, 8, 16, 32))
LEGACY = tuple('pipeline' + s for s in ('', '_bad_forward', '_bad_priority',
                                     '_bad_interlock', '_bad_retire', '_bad_stall'))
MUTANTS = tuple(f'branch_pipeline_w{w}_bad_{s}' for w in (4, 8, 16, 32)
                for s in ('no_flush', 'wrong_kill', 'wrong_target', 'stall_branch',
                          'branch_forward', 'branch_interlock', 'branch_write'))


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--baseline', type=Path, required=True)
    p.add_argument('--candidate', type=Path, required=True)
    p.add_argument('--repeats', type=int, default=5)
    p.add_argument('--out', type=Path, required=True)
    args = p.parse_args()
    assert args.repeats >= 3
    out = args.out.resolve()
    assert not out.exists(), 'Use a fresh output directory'
    binaries = {'baseline': args.baseline.resolve(), 'candidate': args.candidate.resolve()}
    hashes = {mode: sha(path) for mode, path in binaries.items()}
    model_hashes = {case: sha(ROOT / 'examples' / f'{case}.json')
                    for case in GOOD + LEGACY + MUTANTS}
    rows, smt_hashes, verdicts = [], {}, {}
    for repeat in range(args.repeats):
        cases = GOOD + LEGACY + (MUTANTS if repeat == 0 else ())
        if repeat % 2:
            cases = cases[::-1]
        for case in cases:
            modes = ('baseline', 'candidate') if repeat % 2 == 0 else ('candidate', 'baseline')
            for mode in modes:
                folder = out / f'{repeat:02d}' / mode / case
                start = time.perf_counter()
                proc = subprocess.run([str(binaries[mode]), str(ROOT / 'examples' / f'{case}.json'),
                                       '--out', str(folder), '--z3', '/definitely/absent/z3'],
                                      env=dict(os.environ, LYDITE_SOLVER='finite'),
                                      capture_output=True, text=True, timeout=180)
                wall = time.perf_counter() - start
                report = json.loads(proc.stdout)
                (folder / 'stderr.log').write_text(proc.stderr)
                assert report['engine_summary']['z3_queries'] == 0
                assert all(q['backend'] != 'z3' for q in report['obligations'])
                expected = 'counterexample' if '_bad_' in case else 'stuttering_refinement_verified'
                assert report['status'] in (expected, 'unknown'), (case, mode, report['status'])
                if mode == 'candidate':
                    assert report['status'] == ('unknown' if case == 'branch_pipeline_w32' else expected)
                diagnostics = {}
                for q in report['obligations']:
                    key = f"{case}/{q['name']}"
                    assertions = (folder / q['evidence']).read_text().split('(check-sat)')[0]
                    digest = hashlib.sha256(assertions.encode()).hexdigest()
                    assert key not in smt_hashes or smt_hashes[key] == digest, (key, 'original SMT changed')
                    smt_hashes[key] = digest
                    verdict_key = (mode, key)
                    assert verdict_key not in verdicts or verdicts[verdict_key] == q['solver_result']
                    verdicts[verdict_key] = q['solver_result']
                    d = dict(solver_result=q['solver_result'], backend=q['backend'])
                    if 'finite' in q:
                        d.update({k: q['finite'][k] for k in
                                  ('reason', 'terms', 'variables', 'clauses', 'decisions', 'conflicts', 'work')})
                        if q['solver_result'] == 'sat':
                            assert q['finite']['original_formula_validated']
                    diagnostics[q['name']] = d
                row = dict(repeat=repeat, mode=mode, case=case, wall_seconds=wall,
                           exit_code=proc.returncode, status=report['status'],
                           engine_summary=report['engine_summary'], diagnostics=diagnostics)
                rows.append(row)
                print(f'{repeat} {mode} {case} {report["status"]} {wall:.6f}s', flush=True)
    for key in smt_hashes:
        old, new = verdicts['baseline', key], verdicts['candidate', key]
        assert old == 'unknown' or old == new, (key, 'decided baseline verdict changed', old, new)
    summary = []
    for case in GOOD + LEGACY + MUTANTS:
        timings = {}
        for mode in binaries:
            group = [r for r in rows if r['case'] == case and r['mode'] == mode]
            times = [r['wall_seconds'] for r in group]
            timings[mode] = dict(median_seconds=statistics.median(times), min_seconds=min(times),
                                 max_seconds=max(times), runs=len(times),
                                 statuses=sorted({r['status'] for r in group}))
        speedup = None
        if all(timings[m]['statuses'] != ['unknown'] for m in binaries):
            speedup = timings['baseline']['median_seconds'] / timings['candidate']['median_seconds']
        summary.append(dict(case=case, timings=timings, completed_task_speedup=speedup))
    assert hashes == {m: sha(p) for m, p in binaries.items()}
    assert model_hashes == {c: sha(ROOT / 'examples' / f'{c}.json') for c in model_hashes}
    result = dict(note='Same-host interleaved measurements; UNKNOWN stopping time is not proof time. '
                       'Good widths and six legacy models repeated; branch mutants run once each.',
                  baseline_commit='167d52564e2dce3bc3fb9402742723ebaf01f877',
                  binaries={m: dict(path=str(p), sha256=hashes[m]) for m, p in binaries.items()},
                  model_sha256=model_hashes, original_assertions_sha256=smt_hashes,
                  summary=summary, runs=rows)
    (out / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(summary[:len(GOOD + LEGACY)], indent=2))


if __name__ == '__main__':
    main()
