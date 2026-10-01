#!/usr/bin/env python3
"""Ask independent Z3 about ORIGINAL SMT obligations, never kernel residuals.

This is an offline audit, not a runtime fallback. Each source query is copied
byte-for-byte and hashed. Solver output is retained separately from original
runtime evidence. Sequential single-process checks avoid load amplification.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import time


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--input', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--z3', type=Path, required=True)
    p.add_argument('--timeout', type=int, default=60)
    a = p.parse_args()
    a.output.mkdir(parents=True, exist_ok=True)
    rows = []
    for report_file in sorted(a.input.glob('*/report.json')):
        report = json.loads(report_file.read_text())
        if not report['name'].startswith('branch_pipeline_'):
            continue
        for obligation in report['obligations']:
            source = report_file.parent/obligation['evidence']
            assert source.name.endswith('.smt2') and 'kernel-residual' not in source.name
            dest = a.output/report['name']/source.name
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(source.read_bytes())
            started = time.perf_counter()
            try:
                process = subprocess.run([str(a.z3), '-smt2', str(dest)], text=True, capture_output=True, timeout=a.timeout)
                stdout, stderr, returncode = process.stdout, process.stderr, process.returncode
                answers = [line.strip() for line in stdout.splitlines() if line.strip() in ('sat','unsat','unknown')]
                actual = answers[0] if answers else 'error'
            except subprocess.TimeoutExpired as error:
                stdout = (error.stdout or b'').decode() if isinstance(error.stdout, bytes) else error.stdout or ''
                stderr = (error.stderr or b'').decode() if isinstance(error.stderr, bytes) else error.stderr or ''
                actual, returncode = 'timeout', None
            elapsed = time.perf_counter() - started
            dest.with_suffix('.z3.out').write_text(stdout)
            dest.with_suffix('.z3.err').write_text(stderr)
            expected = obligation['solver_result']
            row = {'model':report['name'], 'query':obligation['name'], 'original_sha256':hashlib.sha256(source.read_bytes()).hexdigest(), 'runtime_backend':obligation['backend'], 'runtime_result':expected, 'z3_result':actual, 'match':actual==expected, 'decisive_runtime_disagreement':expected in ('sat','unsat') and actual in ('sat','unsat') and actual != expected, 'runtime_unknown_resolved':expected=='unknown' and actual in ('sat','unsat'), 'returncode':returncode, 'seconds':elapsed}
            rows.append(row)
            print(json.dumps(row), flush=True)
    assert rows, 'no_obligations'
    result = {'status':'pass' if all(r['match'] or r['runtime_unknown_resolved'] for r in rows) else 'incomplete_or_mismatch', 'queries':len(rows), 'matches':sum(r['match'] for r in rows), 'runtime_unknown_resolved':sum(r['runtime_unknown_resolved'] for r in rows), 'z3_sha256':hashlib.sha256(a.z3.read_bytes()).hexdigest(), 'z3_version':subprocess.check_output([str(a.z3), '-version'], text=True).strip(), 'rows':rows}
    (a.output/'summary.json').write_text(json.dumps(result, indent=2)+'\n')
    assert result['status'] == 'pass', result['status']

if __name__ == '__main__':
    main()
