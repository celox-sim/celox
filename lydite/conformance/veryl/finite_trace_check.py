#!/usr/bin/env python3
"""Nonvacuous fixed-trace forall via existing QF positive/negative checks.

R must be feasible. Each expected frame predicate P_i is checked by UNSAT
of R AND NOT(P_i), retaining the entire input/event schedule in every query.
Expected outputs never become premises, including expectations on other frames.
No lydite core modifications or external solver fallback are involved.
"""
import argparse
import copy
import json
import os
from pathlib import Path
import subprocess


def conjunction(xs):
    if not xs:
        return True
    if len(xs) == 1:
        return xs[0]
    middle = len(xs) // 2
    return ['and', conjunction(xs[:middle]), conjunction(xs[middle:])]


def lower_fixed_forall(document):
    doc = copy.deepcopy(document)
    if doc.get('version') != 4 or doc.get('kind') != 'specification':
        raise ValueError('only validated scoped specification v4 input is supported')
    if doc.get('implementation') is not None or doc.get('compositions'):
        raise ValueError('this experiment supports leaf specs only, not bindings/compositions')
    manifest = []
    for target, spec in doc.get('specs', {}).items():
        converted = {}
        for index, (name, original) in enumerate(spec.get('examples', {}).items()):
            if original.get('expect') != 'forall' or original.get('quantifiers', []) != []:
                raise ValueError('only innermost forall with no outer input quantifiers is supported')
            base = copy.deepcopy(original)
            base.pop('quantifiers', None)
            predicates = []
            for time, frame in enumerate(base['trace']):
                ps = [['eq', 'o.' + key, value] for key, value in frame.get('observe', {}).items()]
                if 'ensure' in frame:
                    ps.append(frame.pop('ensure'))
                predicates.append((time, conjunction(ps)) if ps else None)
                frame['observe'] = {}
            feasible_name = f'trace_{index}_feasible'
            base['expect'] = 'positive'
            converted[feasible_name] = base
            assertions = []
            for entry in predicates:
                if entry is None:
                    continue
                time, predicate = entry
                assertion_name = f'trace_{index}_frame_{time}_no_violation'
                assertion = copy.deepcopy(base)
                assertion['expect'] = 'negative'
                assertion['trace'][time]['ensure'] = ['not', predicate]
                converted[assertion_name] = assertion
                assertions.append({'name': assertion_name, 'frame': time, 'predicate': predicate})
            manifest.append({'target': target, 'example': name, 'feasibility': feasible_name,
                             'assertions': assertions, 'steps': len(base['trace'])})
        spec['examples'] = converted
    if not manifest:
        raise ValueError('no finite examples were supplied')
    return doc, manifest


def run(document, binary, out, z3_path='/definitely/not-present/veryl-fv-z3'):
    out = Path(out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    if (out / 'solver').exists():
        raise FileExistsError('choose a fresh output directory; refusing to reuse old solver evidence')
    doc, manifest = lower_fixed_forall(document)
    source = out / 'qf-traces.json'
    source.write_text(json.dumps(doc, indent=2) + '\n')
    (out / 'query-map.json').write_text(json.dumps(manifest, indent=2) + '\n')
    env = dict(os.environ, LYDITE_SOLVER='finite', LYDITE_FINITE_SEARCH_HINT='query')
    process = subprocess.run([str(Path(binary).resolve()), str(source), '--out', str(out / 'solver'),
                              '--finite-search-hint', 'query', '--z3', str(z3_path)],
                             env=env, capture_output=True, text=True, timeout=120)
    (out / 'stdout.txt').write_text(process.stdout)
    (out / 'stderr.txt').write_text(process.stderr)
    report_path = out / 'solver/report.json'
    if not report_path.exists():
        raise RuntimeError(f'checker did not return a report (exit {process.returncode}): {process.stderr}')
    if process.returncode not in (0, 1, 3):
        raise RuntimeError(f'checker failed (exit {process.returncode}): {process.stderr}')
    report = json.loads(report_path.read_text())
    actual = {(x['target'], x['example']): x for x in report['examples']}
    expected_keys = {(m['target'], n) for m in manifest for n in [m['feasibility'], *[a['name'] for a in m['assertions']]]}
    if len(actual) != len(report['examples']) or set(actual) != expected_keys:
        raise RuntimeError('missing or extra query results')
    if any(not any(x.get('admitted') is allowed for allowed in (True, False, None)) for x in actual.values()):
        raise RuntimeError('invalid solver admission verdict')
    rows = []
    for m in manifest:
        f = actual[(m['target'], m['feasibility'])]
        checks = [actual[(m['target'], a['name'])] for a in m['assertions']]
        feasible = f.get('admitted')
        # Explicitly false whenever infeasible or a real counterexample exists;
        # otherwise unresolved checks remain unknown, never a vacuous success.
        if feasible is False or any(c.get('admitted') is True for c in checks):
            valid = False
        elif feasible is None or any(c.get('admitted') is None for c in checks):
            valid = None
        else:
            valid = True
        rows.append({'target': m['target'], 'example': m['example'], 'valid': valid,
                     'feasible': feasible, 'assertion_frames': len(checks), 'steps': m['steps'],
                     'violation_queries': [{'name': c['example'], 'sat': c.get('admitted'),
                                            'status': c['status']} for c in checks]})
    backends = set()
    def inspect(value):
        if isinstance(value, dict):
            if 'backend' in value:
                backends.add(value['backend'])
            for v in value.values():
                inspect(v)
        elif isinstance(value, list):
            for v in value:
                inspect(v)
    inspect(report)
    if not backends <= {'finite_bv', 'structural_kernel'}:
        raise RuntimeError('unexpected external-solver backend or fallback')
    summary = {'status': 'passed' if all(r['valid'] is True for r in rows) else
               ('failed' if any(r['valid'] is False for r in rows) else 'unknown'),
               'backend_mode': 'finite', 'backends': sorted(backends), 'cases': rows,
               'claim': 'Nonvacuous forall over these complete finite traces; no arbitrary input prefix, liveness, or language-conformance proof',
               'external_solver_path': str(z3_path), 'checker_exit_code': process.returncode}
    (out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    return summary


def main():
    p = argparse.ArgumentParser()
    p.add_argument('source', type=Path)
    p.add_argument('--binary', default=os.environ.get('LYDITE_BIN'), required=not bool(os.environ.get('LYDITE_BIN')))
    p.add_argument('--out', type=Path, required=True)
    p.add_argument('--z3-tripwire', default='/definitely/not-present/veryl-fv-z3')
    a = p.parse_args()
    result = run(json.loads(a.source.read_text()), a.binary, a.out, a.z3_tripwire)
    print(json.dumps(result, indent=2))
    raise SystemExit(0 if result['status'] == 'passed' else 1)

if __name__ == '__main__':
    main()
