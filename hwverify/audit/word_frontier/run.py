"""Fresh paired fixtures; strict evidence audit, finite defaults, external tripwire."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
from audit.equality_sharing.ci import FLAGS, LIMITS, audit_report, digest, evaluation_fingerprints, read_json, require, tripwire, validate_outcome
from audit.word_frontier.generate import FAMILIES, alpha_rename, cases


def run(checker, out, width, alpha_seed=None, names=(), expected_sha256=None):
    checker = checker.resolve(strict=True)
    sha = digest(checker.read_bytes())
    require(expected_sha256 is None or sha == expected_sha256, 'wrong checker hash')
    out.mkdir(parents=True, exist_ok=False)
    snapshot = out / 'checker'
    shutil.copy2(checker, snapshot)
    require(digest(snapshot.read_bytes()) == sha, 'checker snapshot mismatch')
    forbidden, marker = tripwire(out)
    env = {k:v for k,v in os.environ.items() if not k.startswith('HWVERIFY_')}
    env.update(FLAGS)
    env['PATH'] = str(forbidden.parent) + os.pathsep + env.get('PATH','')
    env['Z3_BIN'] = str(forbidden)
    fingerprints = evaluation_fingerprints()
    summary = {'checker_sha256': sha, 'checker_source': str(checker), 'checker_snapshot': str(snapshot),
               'environment': FLAGS, 'per_primitive_limits': LIMITS, 'width': width, 'alpha_seed': alpha_seed,
               'proof_authority': 'fresh live checker only; reports are audit diagnostics',
               'all_loaded_local_evaluation_source_sha256': fingerprints, 'rows': []}
    started = time.monotonic()
    for doc in cases(width):
        if names and doc['name'] not in names: continue
        role = 'limitation_positive' if doc['name'] in FAMILIES else 'negative'
        if alpha_seed is not None: doc = alpha_rename(doc, alpha_seed)
        directory = out / doc['name']; directory.mkdir()
        payload = (json.dumps(doc, indent=2)+'\n').encode()
        file = directory / 'input.json'; file.write_bytes(payload)
        row = {'name': doc['name'], 'role': role, 'input_sha256': digest(payload), 'input_path': str(file), 'errors': []}
        begin = time.monotonic()
        try:
            command = [str(snapshot), str(file), '--out', str(directory/'output'), '--z3', str(forbidden)]
            row['command'] = command
            process = subprocess.run(command, env=env, capture_output=True, timeout=180)
            (directory/'stdout.txt').write_bytes(process.stdout)
            (directory/'stderr.txt').write_bytes(process.stderr)
            row['exit_code'] = process.returncode
            require(not marker.exists(), 'external solver invoked')
            require(digest(file.read_bytes()) == row['input_sha256'], 'input changed')
            report = read_json(directory/'output/report.json')
            row['status'] = report['status']
            row['accounting'] = audit_report(report, directory/'output')
            row['disposition'] = validate_outcome(row, report, process.returncode)
        except Exception as error:
            row['errors'].append(type(error).__name__ + ': ' + str(error))
        row['wall_seconds'] = time.monotonic()-begin
        row['artifacts_sha256'] = {str(p.relative_to(directory)): digest(p.read_bytes()) for p in sorted(directory.rglob('*')) if p.is_file()}
        summary['rows'].append(row)
        (out/'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
        print(json.dumps({k:v for k,v in row.items() if k not in ('artifacts_sha256','accounting')}), flush=True)
    require(digest(snapshot.read_bytes()) == sha and digest(checker.read_bytes()) == sha, 'checker changed')
    require(evaluation_fingerprints() == fingerprints, 'evaluation source changed')
    summary['wall_seconds'] = time.monotonic()-started
    summary['forbidden_solver_invocations'] = marker.read_text() if marker.exists() else ''
    summary['status'] = 'failed' if any(r['errors'] for r in summary['rows']) else 'passed'
    (out/'summary.json').write_text(json.dumps(summary, indent=2)+'\n')
    return summary


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--checker',type=Path,required=True);p.add_argument('--out',type=Path,required=True)
    p.add_argument('--width',type=int,default=32);p.add_argument('--alpha-seed',type=int)
    p.add_argument('--names',nargs='*',default=[]);p.add_argument('--expected-checker-sha256')
    a=p.parse_args();s=run(a.checker,a.out.resolve(),a.width,a.alpha_seed,a.names,a.expected_checker_sha256)
    raise SystemExit(0 if s['status']=='passed' else 1)

if __name__=='__main__': main()
