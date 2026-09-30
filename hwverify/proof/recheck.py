#!/usr/bin/env python3
"""Check Lean soundness, actual Rust rewrites, and intentionally wrong copies."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
parser = argparse.ArgumentParser()
parser.add_argument('--lean', default=os.environ.get('LEAN', 'lean'))
parser.add_argument('--rustc', default=os.environ.get('RUSTC', 'rustc'))
parser.add_argument('--out', type=Path, default=HERE / 'results.json')
args = parser.parse_args()
rows = []

def run(name, command, expected=0):
    start = time.perf_counter()
    p = subprocess.run(command, text=True, capture_output=True, timeout=120)
    row = dict(name=name, seconds=time.perf_counter()-start, returncode=p.returncode,
               stdout=p.stdout, stderr=p.stderr)
    rows.append(row)
    if (expected == 0 and p.returncode != 0) or (expected != 0 and p.returncode == 0):
        raise RuntimeError(f'{name}: unexpected result\n{p.stdout}\n{p.stderr}')
    return p

lean_source = (HERE / 'MemoryRules.lean').read_text()
rust_source = (ROOT / 'src/ir.rs').read_text()
with tempfile.TemporaryDirectory(prefix='hwverify-memory-proof-') as td:
    td = Path(td)
    checked = run('lean_soundness', [args.lean, str(HERE / 'MemoryRules.lean')])
    assert 'sorryAx' not in checked.stdout
    program_checked = run('lean_program_rule', [args.lean, str(HERE / 'ProgramRules.lean')])
    assert 'sorryAx' not in program_checked.stdout
    for label, source in [
        ('no_alias', lean_source.replace('.branch a b v (memoryRead old a n)', '.read old a')),
        ('first_write_wins', lean_source.replace('if b = a then .store old a v else .store m a v', 'if b = a then m else .store m a v')),
    ]:
        assert source != lean_source
        f = td / f'{label}.lean'
        f.write_text(source)
        p = run('reject_lean_' + label, [args.lean, str(f)], expected=1)
        assert 'unsolved goals' in p.stdout or 'error:' in p.stdout
    (td / 'src').mkdir()
    (td / 'proof').mkdir()
    shutil.copyfile(HERE / 'check_rust_rules.rs', td / 'proof/check_rust_rules.rs')
    for label, source in [
        ('actual', rust_source),
        ('no_alias', rust_source.replace('return ite(\n            eq(a.clone(), v[1].clone()),\n            v[2].clone(),\n            memory_read(v[0].clone(), a, budget - 1, rules),\n        );', 'return memory_read(v[0].clone(), a, budget - 1, rules);')),
        ('first_write_wins', rust_source.replace('m = m.0.args[0].clone();', 'return m;')),
    ]:
        if label != 'actual':
            assert source != rust_source, 'Rust shape changed: update mutation deliberately'
        (td / 'src/ir.rs').write_text(source)
        binary = td / ('check_' + label)
        run('compile_rust_' + label, [args.rustc, '--edition=2021', '-O', str(td / 'proof/check_rust_rules.rs'), '-o', str(binary)])
        p = run('rust_' + label, [str(binary)], expected=0 if label == 'actual' else 1)
        if label != 'actual':
            assert 'mismatch' in p.stderr, 'failure must be a semantic mismatch'
report = dict(status='pass', caveat='Lean proves a typed model; Rust correspondence is tested, not proved.',
              files={str(p.relative_to(ROOT)): hashlib.sha256(p.read_bytes()).hexdigest()
                     for p in [HERE / 'MemoryRules.lean', HERE / 'ProgramRules.lean', HERE / 'check_rust_rules.rs', ROOT / 'src/ir.rs']},
              checks=rows)
args.out.write_text(json.dumps(report, indent=2, ensure_ascii=False)+'\n')
print(json.dumps({'status': report['status'], 'checks': len(rows), 'out': str(args.out)}))
