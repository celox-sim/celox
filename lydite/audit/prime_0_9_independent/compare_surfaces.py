"""Compare frozen 0.8 surfaces with 0.9 surfaces through public CLI only."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
p = argparse.ArgumentParser()
p.add_argument('--baseline', type=Path, required=True)
p.add_argument('--new', type=Path, required=True)
p.add_argument('--z3', required=True)
p.add_argument('--out', type=Path, default=ROOT / 'results/prime_0_9_independent')
a = p.parse_args()

def stable(value):
    if isinstance(value, dict):
        return {k: stable(v) for k, v in value.items() if k != 'seconds' and not k.endswith('_seconds')}
    if isinstance(value, list):
        return [stable(v) for v in value]
    return value

def run(binary, source, destination):
    destination.mkdir(parents=True, exist_ok=True)
    emit = subprocess.run([str(binary), str(source), '--emit-json', str(destination / 'canonical.json')], capture_output=True, text=True, timeout=30)
    assert emit.returncode == 0, (source, emit.stderr)
    proof = subprocess.run([str(binary), str(source), '--out', str(destination), '--z3', a.z3], capture_output=True, text=True, timeout=120)
    (destination / 'run.stdout').write_text(proof.stdout)
    (destination / 'run.stderr').write_text(proof.stderr)
    return json.loads((destination / 'canonical.json').read_text()), json.loads((destination / 'report.json').read_text()), proof.returncode

rows = []
for source in sorted(a.baseline.glob('*.lyd')):
    old_dir = a.out / 'old' / source.stem
    new_dir = a.out / 'new' / source.stem
    old = run((a.baseline / 'lydite-0.8').resolve(), source.resolve(), old_dir)
    new = run(a.new.resolve(), ROOT / 'examples' / source.name, new_dir)
    assert old[0] == new[0], (source.name, 'canonical JSON changed')
    assert old[2] == new[2], (source.name, 'exit code changed')
    assert stable(old[1]) == stable(new[1]), (source.name, 'non-timing report changed')
    old_smt = {str(f.relative_to(old_dir)): f.read_bytes() for f in old_dir.rglob('*.smt2')}
    new_smt = {str(f.relative_to(new_dir)): f.read_bytes() for f in new_dir.rglob('*.smt2')}
    assert old_smt == new_smt, (source.name, 'SMT bytes changed')
    rows.append({'case': source.name, 'status': new[1]['status'], 'exit_code': new[2], 'smt_files_identical': len(new_smt), 'canonical_json_identical': True, 'non_timing_report_identical': True})
    print(rows[-1], flush=True)
result = {'status': 'pass', 'old_binary_sha256': hashlib.sha256((a.baseline / 'lydite-0.8').read_bytes()).hexdigest(), 'new_binary_sha256': hashlib.sha256(a.new.read_bytes()).hexdigest(), 'cases': rows, 'total_smt_files_identical': sum(r['smt_files_identical'] for r in rows)}
(ROOT / 'audit/prime_0_9_independent/results.json').write_text(json.dumps(result, indent=2) + '\n')
