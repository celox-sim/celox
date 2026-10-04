#!/usr/bin/env python3
"""Tripwire finite runs; original-query identity against immutable 0.11 evidence.

No fallback is allowed. UNKNOWN remains inconclusive. Run offline Z3 and replay
using the earlier independent scripts separately, on this script's new output.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]

def sha(p):
    return hashlib.sha256(p.read_bytes()).hexdigest()

def main():
    p = argparse.ArgumentParser()
    p.add_argument('--binary', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    a = p.parse_args()
    binary = a.binary.resolve()
    out = a.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    marker = out/'Z3_WAS_CALLED'
    assert not marker.exists(), 'preexisting tripwire marker'
    tripwire = out/'z3_tripwire.py'
    tripwire.write_text('#!/usr/bin/env python3\nfrom pathlib import Path\nPath('+repr(str(marker))+').write_text("Unexpected external solver invocation\\n")\nraise SystemExit(97)\n')
    tripwire.chmod(0o755)
    files = sorted((ROOT/'examples').glob('branch_pipeline_w*.json'))
    files += sorted((ROOT/'results/branch_pipeline_independent/extra_mutants').glob('*.json'))
    assert len(files) == 40, len(files)
    files += [ROOT/'examples'/f'{name}.json' for name in ('pipeline','pipeline_bad_forward','pipeline_bad_priority','pipeline_bad_interlock','pipeline_bad_retire','pipeline_bad_stall')]
    sources = sorted((ROOT/'crates').glob('*/src/*.rs'))
    frozen = {'binary':sha(binary), 'fixtures':{str(s.relative_to(ROOT)):sha(s) for s in files}, 'sources':{str(s.relative_to(ROOT)):sha(s) for s in sources}}
    (out/'frozen-inputs.json').write_text(json.dumps(frozen, indent=2)+'\n')
    rows, assertions = [], []
    for file in files:
        name = file.stem
        dest = out/name
        env = dict(os.environ)
        env['HWVERIFY_SOLVER'] = 'finite'
        env.pop('HWVERIFY_KERNEL', None)
        start = time.perf_counter()
        proc = subprocess.run([str(binary), str(file), '--out', str(dest), '--z3', str(tripwire)], env=env, text=True, capture_output=True, timeout=180)
        elapsed = time.perf_counter()-start
        report = json.loads(proc.stdout)
        (dest/'stdout.json').write_text(proc.stdout)
        (dest/'stderr.log').write_text(proc.stderr)
        assert not marker.exists(), ('attempted Z3 fallback', name)
        assert report['engine_summary']['z3_queries'] == 0, name
        assert all(q['backend'] in ('finite_bv', 'structural_kernel') for q in report['obligations']), name
        expected = {'counterexample', 'unknown'} if '_bad_' in name else {'stuttering_refinement_verified', 'unknown'}
        assert report['status'] in expected, (name, report['status'], expected)
        if name.startswith('pipeline'):
            baseline = ROOT/'results/finite_speed_independent/pipeline/current'/name
        else:
            baseline = ROOT/('results/branch_pipeline/finite' if file.parent.name == 'examples' else 'results/branch_pipeline_independent/finite')/name
        old = json.loads((baseline/'report.json').read_text())
        oldqueries = {q['name']:q for q in old['obligations']}
        for q in report['obligations']:
            current = (dest/q['evidence']).read_text().split('(check-sat)')[0]
            previous = (baseline/oldqueries[q['name']]['evidence']).read_text().split('(check-sat)')[0]
            assert current == previous, ('original query changed',name,q['name'])
            assertions.append({'case':name, 'query':q['name'], 'sha256':hashlib.sha256(current.encode()).hexdigest(), 'baseline_result':oldqueries[q['name']]['solver_result'], 'current_result':q['solver_result']})
            if q['solver_result'] == 'unknown' and q['backend'] == 'finite_bv':
                evidence = json.loads((dest/(q['name']+'.finite.json')).read_text())
                assert evidence['reason'] and not evidence['original_formula_validated']
                assert not evidence['assignments'] and not evidence['context_values']
        row = {'case':name,'status':report['status'],'baseline_status':old['status'],'seconds_not_benchmark':elapsed,'exit_code':proc.returncode,'unknown_obligations':[q['name'] for q in report['obligations'] if q['solver_result']=='unknown']}
        rows.append(row)
        print(json.dumps(row), flush=True)
    assert sha(binary) == frozen['binary'], 'binary changed during audit'
    assert all(sha(ROOT/p) == h for p,h in frozen['fixtures'].items()), 'fixture changed during audit'
    assert all(sha(ROOT/p) == h for p,h in frozen['sources'].items()), 'source changed during audit'
    result = {'status':'pass','models':len(rows),'original_queries_unchanged':len(assertions),'z3_tripwire_invocations':0,'unknown_models':sum(bool(r['unknown_obligations']) for r in rows),'frozen_inputs':frozen,'rows':rows,'query_identity':assertions}
    (out/'summary.json').write_text(json.dumps(result, indent=2)+'\n')

if __name__ == '__main__':
    main()
