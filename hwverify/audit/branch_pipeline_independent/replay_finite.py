#!/usr/bin/env python3
"""Replay finite SAT assignments against original JSON with independent Python semantics."""
import argparse
import hashlib
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'audit'))
from interpreter import BV, evaluate, evaluate_record, outputs, related


def decode(x):
    return x['value'] if x['sort'] == 'Bool' else BV(x['width'], x['value'])


def record(prefix, types, values):
    # Symbols omitted by simplification do not influence the formula. Zero is an
    # explicit completion, not an inferred value from the pipeline invariant.
    return {k: decode(values[prefix+k]) if prefix+k in values else (False if t == 'bool' else BV(t['bv'], 0)) for k,t in types.items()}


def replay(doc, evidence, name):
    values = evidence['assignments']
    s = record('spec_', doc['spec']['state'], values)
    t = record('impl_', doc['impl']['state'], values)
    i = record('input_', doc['inputs'], values)
    env = {**{'spec.'+k:v for k,v in s.items()}, **{'impl.'+k:v for k,v in t.items()}, **{'i.'+k:v for k,v in i.items()}}
    before = related(doc, s, t)
    commit = outputs(doc['impl'], t, i)['commit']
    can = outputs(doc['spec'], s, i)['can_step']
    sn = evaluate_record(doc['spec'], 'reset' if i['rst'] else 'next', s, i) if i['rst'] or commit else s
    tn = evaluate_record(doc['impl'], 'reset' if i['rst'] else 'next', t, i)
    after = related(doc, sn, tn)
    enabled = evaluate(doc['progress']['enabled'], env)
    rank = evaluate(doc['progress']['rank'], env)
    rankn = evaluate(doc['progress']['rank'], {**{'spec.'+k:v for k,v in sn.items()}, **{'impl.'+k:v for k,v in tn.items()}})
    formulas = {
        'binding_nonempty': before,
        'reset_binding': not related(doc, evaluate_record(doc['spec'], 'reset', s, i), evaluate_record(doc['impl'], 'reset', t, i)),
        'progress_nonvacuity': before and not i['rst'] and enabled,
        'commit_reachable_in_relation': before and not i['rst'] and enabled and commit,
        'microstep_refinement': before and not after,
        'commit_eligible': before and not i['rst'] and commit and not can,
        'hold_contract': before and not i['rst'] and evaluate(doc['hold_when'], env) and t != tn,
        'noncommit_rank_decreases': before and not i['rst'] and enabled and not commit and not rankn.v < rank.v,
    }
    assert name in formulas and formulas[name], ('witness_not_valid', doc['name'], name)
    expected = env | {'commit':commit, 'binding_before':before, 'binding_after':after, 'rank':rank, 'rank_next':rankn, 'progress_enabled':enabled} | {'spec_next.'+k:v for k,v in sn.items()} | {'impl_next.'+k:v for k,v in tn.items()}
    for key, value in evidence['context_values'].items():
        assert expected[key] == decode(value), ('context_mismatch', doc['name'], name, key, expected[key], value)
    return {'context_values_replayed':len(evidence['context_values']), 'assignment_symbols':len(values), 'binding_before':before, 'binding_after':after, 'reset':i['rst'], 'commit':commit}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--input', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.input = args.input.resolve()
    rows, reports = [], []
    for report_file in sorted(args.input.glob('*/report.json')):
        report = json.loads(report_file.read_text())
        name = report['name']
        if not name.startswith('branch_pipeline_'):
            continue
        source = ROOT/'examples'/f'{name}.json'
        if not source.exists():
            source = ROOT/'results/branch_pipeline_independent/extra_mutants'/f'{name}.json'
        doc = json.loads(source.read_text())
        assert report['engine_summary']['z3_queries'] == 0, ('finite_used_z3', report_file)
        assert all(o['backend'] in ('finite_bv','structural_kernel') for o in report['obligations']), ('unexpected_backend', report_file)
        reports.append({'path':str(report_file.relative_to(ROOT)), 'model':name, 'status':report['status'], 'source_sha256':hashlib.sha256(source.read_bytes()).hexdigest()})
        for file in sorted(report_file.parent.glob('*.finite.json')):
            evidence = json.loads(file.read_text())
            if evidence['solver_result'] != 'sat':
                continue
            query = file.name.removesuffix('.finite.json')
            row = replay(doc, evidence, query)
            rows.append({'model':name, 'query':query, 'evidence':str(file.relative_to(ROOT)), 'evidence_sha256':hashlib.sha256(file.read_bytes()).hexdigest(), **row})
    assert reports and rows, 'no_evidence'
    result = {'status':'pass', 'reports_checked':len(reports), 'sat_witnesses_replayed':len(rows), 'context_values_replayed':sum(r['context_values_replayed'] for r in rows), 'reports':reports, 'rows':rows}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2)+'\n')
    print(json.dumps({k:v for k,v in result.items() if k not in ('rows','reports')}))

if __name__ == '__main__':
    main()
