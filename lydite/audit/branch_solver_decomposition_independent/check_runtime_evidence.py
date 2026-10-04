#!/usr/bin/env python3
"""Independently replay original JSON and check all saved original SMT in Z3."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('old_independent_replay', ROOT/'audit/branch_pipeline_independent/replay_finite.py')
replay = importlib.util.module_from_spec(spec)
spec.loader.exec_module(replay)

def main():
    p = argparse.ArgumentParser()
    p.add_argument('--input', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--z3', type=Path, required=True)
    a = p.parse_args()
    a.output.mkdir(parents=True, exist_ok=True)
    rows, witnesses = [], []
    for report_file in sorted(a.input.glob('*/report.json')):
        report = json.loads(report_file.read_text())
        name = report['name']
        source = ROOT/'examples'/f'{name}.json'
        if not source.exists():
            source = ROOT/'results/branch_pipeline_independent/extra_mutants'/f'{name}.json'
        doc = json.loads(source.read_text())
        for evidence_file in sorted(report_file.parent.glob('*.finite.json')):
            evidence = json.loads(evidence_file.read_text())
            if evidence['solver_result'] == 'sat':
                query = evidence_file.name.removesuffix('.finite.json')
                witnesses.append({'case':name,'query':query,'evidence_sha256':hashlib.sha256(evidence_file.read_bytes()).hexdigest(),**replay.replay(doc,evidence,query)})
        for q in report['obligations']:
            original = report_file.parent/q['evidence']
            assert original.suffix == '.smt2' and 'kernel-residual' not in original.name
            dest = a.output/name/original.name
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(original.read_bytes())
            # Recheck the unchanged original assertion, omitting only model/value
            # requests that are irrelevant (or invalid) after an UNSAT answer.
            assertion = original.read_text().split('(check-sat)')[0]+'(check-sat)\n'
            proc = subprocess.run([str(a.z3),'-in','-smt2'],input=assertion,text=True,capture_output=True,timeout=60)
            dest.with_suffix('.z3.out').write_text(proc.stdout)
            dest.with_suffix('.z3.err').write_text(proc.stderr)
            actual = proc.stdout.strip()
            assert proc.returncode == 0 and actual in ('sat','unsat'), (name,q['name'],proc.stdout,proc.stderr)
            runtime = q['solver_result']
            assert runtime == actual or runtime == 'unknown', (name,q['name'],runtime,actual)
            rows.append({'case':name,'query':q['name'],'original_sha256':hashlib.sha256(original.read_bytes()).hexdigest(),'runtime_result':runtime,'z3_result':actual,'decisive_agreement':runtime==actual,'finite_unknown':runtime=='unknown'})
        print(name, 'original obligations checked; SAT witnesses replayed', flush=True)
    assert rows and witnesses
    result = {'status':'pass','original_smt_checks':len(rows),'decisive_agreements':sum(r['decisive_agreement'] for r in rows),'finite_unknowns_not_counted_as_agreement':sum(r['finite_unknown'] for r in rows),'independent_original_json_sat_replays':len(witnesses),'context_values_replayed':sum(w['context_values_replayed'] for w in witnesses),'z3_sha256':hashlib.sha256(a.z3.read_bytes()).hexdigest(),'rows':rows,'witnesses':witnesses}
    (a.output/'summary.json').write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps({k:v for k,v in result.items() if k not in ('rows','witnesses')}))

if __name__ == '__main__':
    main()
