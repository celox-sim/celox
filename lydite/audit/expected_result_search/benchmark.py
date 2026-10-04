#!/usr/bin/env python3
"""Cross caller-declared hint × actual result on unchanged original formulas.

Every model is passed to EVERY mode; names/results never select a candidate hint.
Baselines are published 0.11.2 and the historical rejected unconditional policy.
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
EXTRA = tuple(sorted((ROOT / 'results/branch_pipeline_independent/extra_mutants').glob('*.json')))
FILES = {case: ROOT / 'examples' / f'{case}.json' for case in GOOD + LEGACY + MUTANTS}
FILES.update({path.stem: path for path in EXTRA})
MODES = ('baseline', 'historical_probe', 'query', 'sat', 'unsat')

def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--baseline', type=Path, required=True)
    p.add_argument('--historical-probe', type=Path, required=True)
    p.add_argument('--candidate', type=Path, required=True)
    p.add_argument('--repeats', type=int, default=5)
    p.add_argument('--out', type=Path, required=True)
    args = p.parse_args()
    assert args.repeats >= 3
    assert len(EXTRA) == 8 and len(FILES) == 46
    out = args.out.resolve()
    assert not out.exists(), 'Use a fresh output directory'
    binaries = {'baseline': args.baseline.resolve(),
                'historical_probe': args.historical_probe.resolve(),
                **{m: args.candidate.resolve() for m in ('query', 'sat', 'unsat')}}
    hashes = {m: sha(path) for m, path in binaries.items()}
    model_hashes = {c: sha(p) for c, p in FILES.items()}
    out.mkdir(parents=True)
    marker = out / 'Z3_WAS_CALLED'
    tripwire = out / 'z3_tripwire.py'
    tripwire.write_text('#!/usr/bin/env python3\nfrom pathlib import Path\nPath(' + repr(str(marker)) + ').write_text("Unexpected external solver invocation\\n")\nraise SystemExit(97)\n')
    tripwire.chmod(0o755)
    rows, assertions, verdicts, statuses = [], {}, {}, {}
    cases = tuple(FILES)
    for repeat in range(args.repeats):
        order = cases if repeat % 2 == 0 else cases[::-1]
        for case_index, case in enumerate(order):
            # Rotate all five modes so no mode always occupies the same slot.
            rotation = (repeat + case_index) % len(MODES)
            modes = MODES[rotation:] + MODES[:rotation]
            for mode in modes:
                folder = out / f'{repeat:02d}' / mode / case
                command = [str(binaries[mode]), str(FILES[case]), '--out', str(folder), '--z3', str(tripwire)]
                if mode in ('query', 'sat', 'unsat'):
                    command += ['--finite-search-hint', mode]
                env = dict(os.environ, LYDITE_SOLVER='finite', LYDITE_KERNEL='on')
                env.pop('LYDITE_FINITE_SEARCH_HINT', None)
                start = time.perf_counter()
                proc = subprocess.run(command, env=env, capture_output=True, text=True, timeout=180)
                wall = time.perf_counter() - start
                report = json.loads(proc.stdout)
                (folder / 'stderr.log').write_text(proc.stderr)
                assert not marker.exists(), 'finite mode attempted an external solver call'
                assert report['engine_summary']['z3_queries'] == 0
                assert case not in statuses or statuses[case] == report['status'], (case, mode, 'status changed')
                statuses[case] = report['status']
                assert report['status'] in ('stuttering_refinement_verified', 'counterexample')
                assert proc.returncode == (0 if report['status'] == 'stuttering_refinement_verified' else 1)
                diagnostics = {}
                for q in report['obligations']:
                    key = f"{case}/{q['name']}"
                    original = (folder / q['evidence']).read_text().split('(check-sat)')[0]
                    digest = hashlib.sha256(original.encode()).hexdigest()
                    assert key not in assertions or assertions[key] == digest, (key, 'original assertion changed')
                    assertions[key] = digest
                    actual = q['solver_result']
                    assert actual in ('sat', 'unsat'), (key, mode, actual)
                    assert key not in verdicts or verdicts[key] == actual, (key, mode, 'verdict changed')
                    verdicts[key] = actual
                    d = dict(solver_result=actual, backend=q['backend'], seconds=q['seconds'])
                    assert q['backend'] != 'z3'
                    if 'finite' in q:
                        f = q['finite']
                        d.update({k: f[k] for k in ('reason', 'terms', 'variables', 'clauses', 'decisions', 'conflicts', 'work')})
                        for k in ('base_clauses', 'peak_live_clauses', 'split_alternatives', 'split_completed', 'split_unsat', 'search_slices', 'search_yields', 'probe_result', 'probe_work', 'search_hint', 'search_strategy'):
                            if k in f: d[k] = f[k]
                        if actual == 'sat': assert f['original_formula_validated']
                        if actual == 'unsat' and f.get('split_alternatives', 0) and f.get('probe_result') != 'unsat':
                            assert f['split_completed'] == f['split_unsat'] == f['split_alternatives']
                        if mode in ('query', 'sat', 'unsat'):
                            assert f['search_hint'] == (q['logical_expectation'] if mode == 'query' else mode)
                            d['logical_expectation'] = q['logical_expectation']
                            d['search_hint_source'] = q['search_hint_source']
                            if f['search_hint'] == 'unsat':
                                assert f['probe_work'] == 0 and f['probe_result'] is None
                    diagnostics[q['name']] = d
                row = dict(repeat=repeat, mode=mode, declared_cli_hint=mode if mode in ('query','sat','unsat') else None,
                           case=case, wall_seconds=wall, exit_code=proc.returncode,
                           status=report['status'], engine_summary=report['engine_summary'], diagnostics=diagnostics)
                rows.append(row)
                print(f'{repeat} {mode} {case} {report["status"]} {wall:.6f}s', flush=True)
    # Beyond verdicts, confirm the intended legacy search paths really match.
    by_mode_case = {(r['mode'], r['case']): r for r in rows}
    for case in cases:
        for mode in ('query', 'sat', 'unsat'):
            for name, d in by_mode_case[mode, case]['diagnostics'].items():
                if d['backend'] != 'finite_bv': continue
                reference = 'baseline' if d['search_hint'] == 'unsat' else 'historical_probe'
                old = by_mode_case[reference, case]['diagnostics'][name]
                for field in ('work', 'decisions', 'conflicts', 'terms', 'variables', 'clauses', 'base_clauses', 'split_completed', 'split_unsat'):
                    assert d[field] == old[field], (case, name, mode, reference, field, d[field], old[field])
    summary = []
    for case in cases:
        timings = {}
        for mode in MODES:
            group = [r for r in rows if r['case'] == case and r['mode'] == mode]
            times = [r['wall_seconds'] for r in group]
            timings[mode] = dict(median_seconds=statistics.median(times), min_seconds=min(times),
                                 max_seconds=max(times), runs=len(times))
        summary.append(dict(case=case, actual_status=statuses[case], timings=timings))
    matrix = []
    for case in cases:
        first = next(r for r in rows if r['case'] == case)
        for name in first['diagnostics']:
            for mode in ('query', 'sat', 'unsat'):
                groups = [r['diagnostics'][name] for r in rows if r['case'] == case and r['mode'] == mode]
                if groups[0]['backend'] != 'finite_bv': continue
                matrix.append(dict(case=case, query=name, cli_hint=mode, declared_hint=groups[0]['search_hint'],
                                   actual_result=groups[0]['solver_result'],
                                   median_query_seconds=statistics.median(g['seconds'] for g in groups),
                                   work=groups[0]['work'], strategy=groups[0]['search_strategy']))
    assert hashes == {m: sha(p) for m, p in binaries.items()}
    assert model_hashes == {c: sha(FILES[c]) for c in model_hashes}
    result = dict(note='Every original model is run under all declared hints. Matching and mismatched hints are timed separately; no actual result selects a hint. Whole-CLI wall times include evidence I/O. These same-host samples are not universal guarantees.',
                  baseline_commit='8e700fa040d23334dc1fddd1c108c141375c2506',
                  historical_probe_commit='01400a4de7fe57b9d7f49a178d8c241c9c657338',
                  binaries={m:dict(path=str(p),sha256=hashes[m]) for m,p in binaries.items()},
                  model_sha256=model_hashes, original_assertions_sha256=assertions,
                  summary=summary, hint_actual_query_matrix=matrix, runs=rows)
    (out / 'summary.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(summary[:len(GOOD)], indent=2))

if __name__ == '__main__':
    main()
