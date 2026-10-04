#!/usr/bin/env python3
"""External Z3 check of independent harness's saved ORIGINAL IR queries."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

p = argparse.ArgumentParser()
p.add_argument('--input', type=Path, required=True)
p.add_argument('--z3', type=Path, required=True)
a = p.parse_args()
summary = json.loads((a.input/'summary.json').read_text())
rows = []
for item in summary['oracle_cases']:
    source = a.input/item['file']
    proc = subprocess.run([str(a.z3), '-smt2', str(source)], text=True, capture_output=True, timeout=30)
    source.with_suffix('.z3.out').write_text(proc.stdout)
    source.with_suffix('.z3.err').write_text(proc.stderr)
    answers = [x for x in proc.stdout.splitlines() if x in ('sat','unsat','unknown')]
    assert proc.returncode == 0 and len(answers) == 1, (source, proc.stdout, proc.stderr)
    assert answers[0] == item['finite_result'], (source, answers, item['finite_result'])
    rows.append({'file':item['file'], 'label':item['label'], 'sha256':hashlib.sha256(source.read_bytes()).hexdigest(), 'z3_result':answers[0], 'finite_result':item['finite_result']})
result = {'status':'pass', 'checks':len(rows), 'z3_sha256':hashlib.sha256(a.z3.read_bytes()).hexdigest(), 'z3_version':subprocess.check_output([str(a.z3), '-version'], text=True).strip(), 'rows':rows}
(a.input/'z3-original-query-summary.json').write_text(json.dumps(result, indent=2)+'\n')
print(json.dumps({k:v for k,v in result.items() if k != 'rows'}))
