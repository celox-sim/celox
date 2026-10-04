#!/usr/bin/env python3
"""Repeat identical full AXI queries under unchanged finite-solver defaults."""
import argparse
import hashlib
import json
from pathlib import Path
import statistics
import subprocess
import sys
import time
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from protocols.axi4lite_project import prepare, replay


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--out', type=Path, required=True); p.add_argument('--baseline-binary', type=Path)
    p.add_argument('--runs', type=int, default=3); a = p.parse_args()
    if not 1 <= a.runs <= 10: p.error('runs must be 1..10')
    a.out.mkdir(parents=True, exist_ok=False); setup = a.out / 'prepare'; setup.mkdir()
    project = prepare(ROOT / 'examples/source-contracts/axi-binding.json', setup)
    binaries = {'current': replay.CORE}
    if a.baseline_binary: binaries['baseline'] = a.baseline_binary.resolve()
    report = {'depth': project['depth'], 'identity': project['identity'], 'limits': 'unchanged native defaults; no external solver', 'queries': {}}
    for goal, doc in [('guarantees', project['document']), ('capacity', project['scope_document'])]:
        request = {'version': 1, 'document': doc, 'mode': 'search', 'goal': 'safety', 'depth': project['depth']}
        encoded = replay.canonical(request); replay.write(a.out / (goal + '-request.json'), request)
        results = {'request_sha256': replay.sha(encoded), 'state_fields': len(doc['implementation']['state'])}
        for label, binary in binaries.items():
            runs = []
            for index in range(a.runs):
                start = time.perf_counter(); process = subprocess.run([str(binary)], input=encoded, capture_output=True, check=True)
                elapsed = time.perf_counter() - start; result = json.loads(process.stdout)
                replay.write(a.out / f'{goal}-{label}-{index}.json', result)
                runs.append({'seconds': elapsed, 'status': result['status'], 'reason': result.get('reason'),
                             'solver': {k: result['solver'][k] for k in ('terms','variables','clauses','work','decisions','conflicts')}})
            results[label] = {'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'median_seconds': statistics.median(r['seconds'] for r in runs), 'runs': runs}
        if any(r['status'] != 'bounded_no_failure' for r in results['current']['runs']): raise RuntimeError(results)
        report['queries'][goal] = results
    replay.write(a.out / 'measurements.json', report); print(json.dumps(report))
if __name__ == '__main__': main()
