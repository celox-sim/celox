#!/usr/bin/env python3
"""Classify every pinned case and gate exact extraction/frontend coverage.

This never blesses a new baseline automatically. --catalog-only emits a candidate
for human review; CI always requires the separately committed golden manifest.
"""
import argparse
from collections import Counter
import copy
import hashlib
import json
from pathlib import Path
import sys
from analyzer_lower import Lower, Unsupported
from check_extracted import analyze, check

ROOT = Path(__file__).resolve().parent
REVISION = '124a1315096d21b85d9d0d84fd7139363a181cad'


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def capture_identity(row):
    # Paths/diagnostic strings can embed a machine-local compiler cache root.
    # Retain exact source and action semantics, not that irrelevant location.
    design = row.get('design') or {}
    sources = [{'sha256': x['sha256']} for x in design.get('sources', [])]
    actions = copy.deepcopy(row.get('actions', row.get('partial_actions', [])))
    for action in actions:
        if 'assertion' in action:
            for field in ('location', 'sample_location'):
                action['assertion'][field].pop('url', None)
    return {'case': row['case'], 'source': row['source'], 'capture': row['status'],
            'assertions': row['assertion_count'], 'action_sha256': digest(actions),
            'design': {'top': design.get('top'), 'four_state': design.get('four_state'), 'sources': sources}}


def relation(document):
    result = copy.deepcopy(document)
    for spec in result['specs'].values():
        spec.pop('examples')
    return result


def oracle_independence(row, ir, document):
    changed = copy.deepcopy(row)
    for action in changed['actions']:
        if action['action'] == 'read':
            action['payload'] = str(int(action['payload']) ^ 1)
            action['assertion']['comparison'] = 'ne' if action['assertion']['comparison'] == 'eq' else 'eq'
    if relation(Lower(ir).build(changed)) != relation(document):
        raise RuntimeError('expected predicates leaked into transition relation')


def classify(data, out):
    if data.get('schema') != 'celox-assertion-capture-v1' or data['provenance']['upstream_commit'] != REVISION:
        raise ValueError('unexpected capture schema/revision')
    rows = data['cases']
    names = [row['case'] for row in rows]
    if len(set(names)) != len(names):
        raise ValueError('duplicate case identifier')
    catalog, supported = [], {}
    for row in sorted(rows, key=lambda row: row['case']):
        entry = capture_identity(row)
        if row['status'] == 'compile_rejection_fixture':
            entry.update(disposition='compile_rejection_fixture', reason='capture_only_no_language_rejection_claim')
        elif row['status'] == 'unsupported':
            codes = sorted(set(reason['code'] for reason in row['reasons']))
            if not codes:
                raise ValueError('unsupported capture without a reason')
            entry.update(disposition='unsupported_capture', reason='+'.join(codes))
        elif row['status'] == 'extracted':
            destination = out / 'catalog' / row['case'].replace('::', '__')
            destination.mkdir(parents=True, exist_ok=False)
            try:
                ir, document = analyze(row, destination)
            except Unsupported as error:
                entry.update(disposition='unsupported_frontend', reason=error.code)
                (destination / 'unsupported.json').write_text(json.dumps({'code': error.code, 'detail': str(error)}, indent=2) + '\n')
            else:
                oracle_independence(row, ir, document)
                frames = document['specs']['Top']['examples']['original_assertions']['trace']
                predicates = [frame.get('ensure') for frame in frames]
                entry.update(disposition='expected_pass', reason='supported_typed_two_state_trace',
                             frames=len(frames), assertion_frames=sum('ensure' in frame for frame in frames),
                             predicate_sha256=digest(predicates))
                supported[row['case']] = (ir, document)
        else:
            raise ValueError(f'unknown capture status: {row["status"]}')
        catalog.append(entry)
    return {'schema': 'veryl-fv-coverage-v1', 'upstream_revision': REVISION,
            'source_hashes': data['provenance']['original_files'], 'cases': catalog}, supported


def validate_golden(actual, expected):
    if actual != expected:
        old = {row['case']: row for row in expected.get('cases', [])}
        new = {row['case']: row for row in actual.get('cases', [])}
        removed, added = sorted(old.keys() - new.keys()), sorted(new.keys() - old.keys())
        changed = sorted(name for name in old.keys() & new.keys() if old[name] != new[name])
        raise ValueError(f'coverage/source manifest drift: missing={removed}, extra={added}, changed={changed}; review actual-coverage.json, never silently skip or bless')
    if not any(row['disposition'] == 'expected_pass' for row in actual['cases']):
        raise ValueError('empty expected-pass coverage')


def validate_results(manifest, results):
    expected = {row['case'] for row in manifest['cases'] if row['disposition'] == 'expected_pass'}
    identities = {row['case']: row for row in manifest['cases'] if row['disposition'] == 'expected_pass'}
    actual = {row['case'] for row in results}
    if len(actual) != len(results) or actual != expected:
        raise ValueError('missing/duplicate/extra finite verification results')
    for result in results:
        if result['status'] != 'passed':
            raise ValueError(f'{result["case"]}: {result["status"]} is not a pass')
        identity = identities[result['case']]
        if result.get('assertions') != identity['assertions'] or result.get('frames') != identity['frames']:
            raise ValueError('verification assertion/operation coverage changed')
        checked = result['normal'].get('cases', [])
        if len(checked) != 1 or checked[0].get('assertion_frames') != identity['assertion_frames']:
            raise ValueError('verification assertion-frame coverage changed')
        if result['normal']['backends'] != sorted(set(result['normal']['backends'])):
            raise ValueError('invalid backend audit')
        if not set(result['normal']['backends']) <= {'finite_bv', 'structural_kernel'}:
            raise ValueError('unexpected solver backend/fallback')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--traces', type=Path, default=ROOT / 'traces.json')
    parser.add_argument('--golden', type=Path, default=ROOT / 'coverage-manifest.json')
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--catalog-only', action='store_true', help='emit candidate catalog for manual review, do not verify or update the golden')
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    report = {'status': 'failed', 'results': []}
    try:
        data = json.loads(args.traces.read_text())
        manifest, supported = classify(data, args.out)
        (args.out / 'actual-coverage.json').write_text(json.dumps(manifest, indent=2, sort_keys=True) + '\n')
        report['coverage'] = dict(Counter(row['disposition'] for row in manifest['cases']))
        report['first_blockers'] = dict(Counter(row['reason'] for row in manifest['cases'] if row['disposition'] != 'expected_pass'))
        report['assertions_extracted'] = sum(row['assertions'] for row in manifest['cases'] if row['capture'] == 'extracted')
        if args.catalog_only:
            report['status'] = 'catalog_only_not_verified'
            print(json.dumps({key: value for key, value in report.items() if key != 'results'}, indent=2))
            return
        validate_golden(manifest, json.loads(args.golden.read_text()))
        for row in data['cases']:
            if row['case'] in supported:
                result = check(row, args.out / 'fv' / row['case'].replace('::', '__'), analyzed=supported[row['case']])
                report['results'].append(result)
                (args.out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
                print(row['case'], result['status'], flush=True)
        validate_results(manifest, report['results'])
        report['status'] = 'passed'
        report['assertions_verified'] = sum(row['assertions'] for row in report['results'])
        report['frames_verified'] = sum(row['frames'] for row in report['results'])
        print(json.dumps({key: value for key, value in report.items() if key != 'results'}, indent=2))
    except Exception as error:
        report['error'] = f'{type(error).__name__}: {error}'
        raise
    finally:
        (args.out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    main()
