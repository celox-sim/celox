#!/usr/bin/env python3
"""Mandatory fixtures use the public project CLI from disposable external projects."""
import argparse
import copy
import json
from pathlib import Path
import re
import sys
import tempfile
from project import HERE, ROOT, ADAPTER, CHECKER, core, invoke, prepare_project, simulate, write

CASES = {
    'counter_correct': ('counter', None, 'safety', 3, 'bounded_no_failure'),
    'counter_wrong_update': ('counter', ("count = count + 4'd1;", "count = count + 4'd2;"), 'safety', 3, 'reset_reachable_failure'),
    'counter_unreachable_fault': ('counter', ("count = count + 4'd1;", "if fault { count = count + 4'd2; } else { count = count + 4'd1; }"), 'safety', 3, 'bounded_no_failure'),
    'response_correct': ('response', None, 'response_deadline', 5, 'bounded_no_failure'),
    'response_dropped': ('response', ('busy = (busy || accept) && !complete;', 'busy = 0;'), 'response_deadline', 5, 'reset_reachable_failure'),
    'response_deadline': ('response', ("ticks = ticks - 2'd1;", 'ticks = ticks;'), 'response_deadline', 5, 'reset_reachable_failure'),
}

def fixture_project(name, root):
    family, mutation, goal, depth, expected = CASES[name]
    source = (HERE / 'fixtures' / (family + '.veryl')).read_text()
    if mutation:
        old, new = mutation
        if source.count(old) != 1:
            raise ValueError('mutation site drift')
        source = source.replace(old, new)
    # Physical names, top and directory layout deliberately differ from native IR.
    pins = ['clk', 'rst_n', 'en', 'count', 'fault'] if family == 'counter' else ['clk', 'rst_n', 'request', 'stall', 'count', 'busy', 'ticks', 'accept', 'complete']
    names = {n: n + '_pin' for n in pins}
    for old, new in names.items():
        source = re.sub(r'\b' + old + r'\b', new, source)
    source = source.replace('module ' + family.title(), 'module UserDut')
    (root / 'rtl').mkdir(parents=True)
    (root / 'contracts').mkdir()
    (root / 'rtl/unit.veryl').write_text(source)
    (root / 'contracts/model.hwv').write_text((HERE / 'fixtures' / (family + '.hwv')).read_text())
    state = ['count', 'fault'] if family == 'counter' else ['count', 'busy', 'ticks']
    inputs = ['en'] if family == 'counter' else ['request', 'stall']
    manifest = {'version': 1, 'name': name, 'sources': ['rtl/unit.veryl'], 'top': 'UserDut', 'specification': 'contracts/model.hwv',
                'clock': names['clk'], 'reset': {'input': 'rst', 'active': 0}, 'inputs': {'rst': names['rst_n'], **{n: names[n] for n in inputs}},
                'state': {n: names[n] for n in state}, 'signals': {n: {'signal': names[n], 'type': 'bool'} for n in (['accept', 'complete'] if family == 'response' else [])}, 'property': goal, 'depth': depth}
    path = root / 'user-project.json'
    write(path, manifest)
    return path

def cli(*args, allowed=(0,)):
    return invoke([sys.executable, HERE / 'project.py', *args], allowed=allowed)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--record', action='store_true', help='Explicitly refresh reviewed fixture regressions, never used by CI')
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    summary = []
    with tempfile.TemporaryDirectory(prefix='hwverify-external-projects-') as temporary:
        external = Path(temporary)
        manifests = {name: fixture_project(name, external / name) for name in CASES}
        for name, (family, _, goal, depth, expected) in CASES.items():
            out = args.out / name
            fresh = external / (name + '.regression.json')
            result = cli('search', manifests[name], '--out', out, '--save-regression', fresh)
            if result['status'] != expected:
                raise RuntimeError(f'{name}: {result}')
            if expected == 'reset_reachable_failure':
                if args.record:
                    write(HERE / 'fixtures' / (name + '.regression.json'), json.loads(fresh.read_text()))
                saved = HERE / 'fixtures' / (name + '.regression.json')
                replay = cli('replay', manifests[name], saved, '--out', out / 'saved')
                if replay['status'] != expected or replay['simulation'] != 'simulation_matches_validated_trace':
                    raise RuntimeError('saved public-CLI regression failed')
                correct_out = out / 'correct-counterpart'
                correct_out.mkdir()
                correct = prepare_project(manifests[family + '_correct'], correct_out)
                if simulate(correct, json.loads(saved.read_text())['inputs'], correct_out)[0] != 'trace_no_failure':
                    raise RuntimeError('correct counterpart failed identical stimuli')
                if name == 'counter_wrong_update':
                    case_out = out / 'negative-controls'
                    case_out.mkdir()
                    project = prepare_project(manifests[name], case_out)
                    _, request = simulate(project, json.loads(fresh.read_text())['inputs'], case_out)
                    for mutation in ['state_write', 'missing_input', 'range', 'reset', 'clock', 'four_state']:
                        bad = copy.deepcopy(request)
                        if mutation == 'state_write': bad['frames'][0]['count_pin'] = 7
                        elif mutation == 'missing_input': bad['frames'][0].pop('en_pin')
                        elif mutation == 'range': bad['frames'][0]['en_pin'] = 2
                        elif mutation == 'reset': bad['frames'][0]['rst_n_pin'] = 1
                        elif mutation == 'clock': bad['event'] = 'missing'
                        else: bad['design']['four_state'] = True
                        if invoke([ADAPTER], bad, allowed=(2,))['status'] != 'simulation_error':
                            raise RuntimeError('malformed adapter request accepted')
                    stale = json.loads(fresh.read_text())
                    stale['identity']['manifest_sha256'] = 'stale'
                    stale_path = external / 'stale.json'
                    write(stale_path, stale)
                    if cli('replay', manifests[name], stale_path, '--out', out / 'stale', allowed=(2,))['status'] != 'project_error':
                        raise RuntimeError('stale manifest accepted')
            elif fresh.exists():
                raise RuntimeError('non-failure search saved a failure regression')
            if name == 'counter_unreachable_fault':
                induction = invoke([CHECKER, out / 'model.json', '--out', out / 'induction'], allowed=(0, 1))
                if not any(o['status'] == 'counterexample' and o.get('finite', {}).get('original_formula_validated') is True for o in induction['implementation_binding']['obligations']):
                    raise RuntimeError('unreachable-only control lost its induction countermodel')
                control = out / 'reset-control'
                control.mkdir()
                project = prepare_project(manifests[name], control)
                if simulate(project, [{'rst': True, 'en': False}, {'rst': False, 'en': True}, {'rst': False, 'en': True}], control)[0] != 'trace_no_failure':
                    raise RuntimeError('induction countermodel became a reset failure')
            summary.append({'case': name, 'search_status': result['status'], 'evidence_kind': 'induction_countermodel_not_reached_within_bound' if name == 'counter_unreachable_fault' else result['status'], 'identity': result['identity']})
            print(json.dumps(summary[-1]), flush=True)
    write(args.out / 'summary.json', summary)

if __name__ == '__main__':
    main()
