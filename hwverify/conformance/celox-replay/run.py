#!/usr/bin/env python3
"""Pinned original Veryl -> reachable violation -> concrete Celox -> saved regression.

The simulator receives source and external stimuli only, never expected outputs.
The independently authored native specification supplies the property oracle.
"""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
PIN = '124a1315096d21b85d9d0d84fd7139363a181cad'
FRONTEND = ROOT / 'conformance/veryl-proof/target/debug/veryl-proof-frontend'
ADAPTER = ROOT / 'conformance/veryl-proof/target/debug/hwverify-celox-replay'
CORE = ROOT / 'target/release/hwverify-replay'
CHECKER = ROOT / 'target/release/hwverify-rs'
LIFTER = ROOT / 'target/release/hwverify-sir-lift'
CASES = {
    'counter_correct': ('counter', None, 'safety', 3, 'bounded_no_failure'),
    'counter_wrong_update': ('counter', ("count = count + 4'd1;", "count = count + 4'd2;"), 'safety', 3, 'reset_reachable_failure'),
    'counter_unreachable_fault': ('counter', ("count = count + 4'd1;", "if fault { count = count + 4'd2; } else { count = count + 4'd1; }"), 'safety', 3, 'bounded_no_failure'),
    'response_correct': ('response', None, 'response_deadline', 5, 'bounded_no_failure'),
    'response_dropped': ('response', ('busy = (busy || accept) && !complete;', 'busy = 0;'), 'response_deadline', 5, 'reset_reachable_failure'),
    'response_deadline': ('response', ("ticks = ticks - 2'd1;", 'ticks = ticks;'), 'response_deadline', 5, 'reset_reachable_failure'),
}

def canonical(v):
    return json.dumps(v, sort_keys=True, separators=(',', ':')).encode()

def sha(v):
    return hashlib.sha256(v).hexdigest()

def write(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')

def invoke(args, request=None, allowed=(0,), timeout=120):
    p = subprocess.run([str(a) for a in args], input=None if request is None else json.dumps(request), text=True, capture_output=True, timeout=timeout, env={**os.environ, 'HWVERIFY_SOLVER': 'finite'})
    if p.returncode not in allowed:
        raise RuntimeError(f'{args[0]} exit {p.returncode}: {p.stderr[-3000:]} {p.stdout[-1500:]}')
    return json.loads(p.stdout)

def core(doc, mode, **kw):
    return invoke([CORE], {'version': 1, 'document': doc, 'mode': mode, **kw})

def prepare_case(name, out):
    family, mutation, goal, depth, expected = CASES[name]
    source = (HERE / 'fixtures' / (family + '.veryl')).read_text()
    if mutation:
        old, new = mutation
        if source.count(old) != 1:
            raise ValueError('mutation site drift')
        source = source.replace(old, new)
    spec_path = HERE / 'fixtures' / (family + '.hwv')
    source_path = out / (family + '.veryl')
    source_path.write_text(source)
    design = {'top': family.title(), 'four_state': False, 'sources': [{'path': family + '.veryl', 'text': source}]}
    design_path = out / 'design.json'
    write(design_path, design)
    compiled = invoke([FRONTEND, design_path])
    if compiled.get('allowed_diagnostics'):
        raise ValueError('replay fixtures must not use frontend semantic waivers')
    compiled_path = out / 'compiled.json'
    write(compiled_path, compiled)
    canonical_path = out / 'canonical.json'
    invoke([CHECKER, spec_path, '--emit-json', canonical_path, '--check', '--out', out / 'parse'])
    doc = json.loads(canonical_path.read_text())
    impl = doc['implementation']
    inputs = {'rst_n': {'name': 'rst', 'type': 'bool', 'expr': ['not', 'i.rst']}}
    for n, ty in doc['inputs'].items():
        if n != 'rst':
            inputs[n] = {'type': ty}
    bindings = {'event': 'clk', 'inputs': inputs, 'state': {n: {'type': ty} for n, ty in impl['state'].items()}, 'outputs': {n: {'signal': n, 'type': 'bool'} for n in (['accept', 'complete'] if family == 'response' else [])}}
    for reset in [False, True]:
        cfg = {**bindings, 'overrides': {'rst': reset}}
        path = out / ('reset-bindings.json' if reset else 'normal-bindings.json')
        write(path, cfg)
        lifted = invoke([LIFTER, compiled_path, path, *(['--inline'] if reset else [])])
        if reset:
            def residual(v):
                return isinstance(v, str) and v.startswith(('s.', 'w.')) or isinstance(v, list) and any(residual(x) for x in v)
            if any(residual(t) for t in lifted['next'].values()):
                raise ValueError('reset remains dependent on arbitrary prestate')
            impl['reset'] = lifted['next']
        else:
            impl['next'] = lifted['next']
            impl['wires'] = {**lifted['wires'], **lifted['outputs']}
    write(out / 'model.json', doc)
    identity = {'celox_revision': PIN, 'source_sha256': sha(source.encode()), 'specification_sha256': sha(spec_path.read_bytes()), 'bindings_sha256': sha(canonical(bindings)), 'document_sha256': sha(canonical(doc)), 'dependency_patches_sha256': sha((ROOT / 'conformance/veryl-proof/dependencies.json').read_bytes())}
    return {'case': name, 'family': family, 'goal': goal, 'depth': depth, 'expected': expected, 'design': design, 'document': doc, 'identity': identity, 'bindings': bindings}

def validate_saved(saved, case):
    if set(saved) != {'version', 'case', 'goal', 'identity', 'inputs'} or saved['version'] != 1 or saved['case'] != case['case'] or saved['goal'] != case['goal'] or saved['identity'] != case['identity']:
        raise ValueError('stale or malformed regression identity')
    if not isinstance(saved['inputs'], list) or not 1 <= len(saved['inputs']) <= 33:
        raise ValueError('invalid saved stimulus length')


def compare_simulation(expected, actual, state_names):
    if actual.get('status') != 'simulated' or actual.get('celox_revision') != PIN or len(actual.get('trace', [])) != len(expected):
        return {'status': 'simulator_divergence', 'reason': 'simulation identity/trace length mismatch'}
    for f, observed in zip(expected, actual['trace']):
        e = f['edge']
        if observed['edge'] != e:
            return {'status': 'simulator_divergence', 'reason': 'edge indexing mismatch'}
        for phase, field in [('after', 'state_after'), ('before', 'state_before')]:
            if field == 'state_before' and e == 0:
                continue  # Power-on state is not part of the reset-established model.
            for name in state_names:
                expected_value = int(f[field][name]['value'])
                if observed[phase].get(name) != str(expected_value):
                    return {'status': 'simulator_divergence', 'edge': e, 'signal': name, 'phase': phase}
        if f['controls'] is not None:
            for name in ['accept', 'complete']:
                if observed['before'].get(name) != str(int(f['controls'][name])):
                    return {'status': 'simulator_divergence', 'edge': e, 'signal': name, 'phase': 'before'}
    return {'status': 'simulation_matches_validated_trace'}


def simulate(case, inputs, out):
    # Recompute the property from the authored spec and original transition model.
    checked = core(case['document'], 'check_stimulus', goal=case['goal'], inputs=inputs)
    expected = checked['trace']
    frames = [{('rst_n' if k == 'rst' else k): (int(not v) if k == 'rst' else int(v) if isinstance(v, bool) else v) for k, v in f['inputs'].items()} for f in expected]
    state_names = list(case['document']['implementation']['state'])
    observe = state_names + (['accept', 'complete'] if case['family'] == 'response' else [])
    request = {'version': 1, 'design': case['design'], 'event': 'clk', 'reset_input': 'rst_n', 'reset_active': 0, 'observe': observe, 'frames': frames}
    actual = invoke([ADAPTER], request)
    comparison = compare_simulation(expected, actual, state_names)
    write(out / 'simulation.json', actual)
    write(out / 'original-replay.json', checked)
    write(out / 'comparison.json', comparison)
    if comparison['status'] != 'simulation_matches_validated_trace':
        raise RuntimeError(f'simulator_divergence: {comparison}')
    return checked['status'], request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--record', action='store_true', help='Explicitly write reviewed mutant stimuli; never used by CI')
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    work = ROOT / 'conformance/veryl-proof/work'
    revision = subprocess.check_output(['git', '-C', str(work / 'celox'), 'rev-parse', 'HEAD'], text=True).strip()
    if revision != PIN:
        raise ValueError('wrong Celox revision')
    provenance = json.loads((work / 'provenance.json').read_text())
    if provenance['dependencies'] != json.loads((ROOT / 'conformance/veryl-proof/dependencies.json').read_text()):
        raise ValueError('dependency provenance mismatch')
    summary = []
    prepared = {}
    for name in CASES:
        out = args.out / name
        out.mkdir()
        case = prepare_case(name, out)
        prepared[name] = case
        result = core(case['document'], 'search', goal=case['goal'], depth=case['depth'])
        write(out / 'search.json', result)
        if result['status'] != case['expected']:
            raise RuntimeError(f'{name}: expected {case["expected"]}, got {result["status"]}: {result.get("reason")}')
        if name == 'counter_unreachable_fault':
            induction = invoke([CHECKER, out / 'model.json', '--out', out / 'induction'], allowed=(0, 1))
            if induction['implementation_binding']['status'] != 'failed' or not any(o['status'] == 'counterexample' and o.get('solver_result') == 'sat' and o.get('finite', {}).get('original_formula_validated') is True for o in induction['implementation_binding']['obligations']):
                raise RuntimeError('unreachable-only control lost its induction countermodel')
            write(out / 'induction.json', induction)
            control = out / 'reset-control'
            control.mkdir()
            if simulate(case, [{'rst': True, 'en': False}, {'rst': False, 'en': True}, {'rst': False, 'en': True}], control)[0] != 'trace_no_failure':
                raise RuntimeError('induction-only countermodel became a reset failure')
        if result['status'] == 'reset_reachable_failure':
            witness = result['witness']
            replayed = core(case['document'], 'replay', witness=witness)
            if replayed['status'] != 'reset_reachable_failure':
                raise RuntimeError('witness failed independent original replay')
            inputs = [f['inputs'] for f in witness['trace']]
            status, request = simulate(case, inputs, out)
            if status != 'reset_reachable_failure':
                raise RuntimeError('new witness did not reproduce actual violation')
            # Actual adapter rejection checks, not mocked simulator responses.
            if name == 'counter_wrong_update':
                for mutation in ['state_write', 'missing_input', 'range', 'reset', 'clock', 'four_state']:
                    bad = copy.deepcopy(request)
                    if mutation == 'state_write': bad['frames'][0]['count'] = 7
                    elif mutation == 'missing_input': bad['frames'][0].pop('en')
                    elif mutation == 'range': bad['frames'][0]['en'] = 2
                    elif mutation == 'reset': bad['frames'][0]['rst_n'] = 1
                    elif mutation == 'clock': bad['event'] = 'missing'
                    else: bad['design']['four_state'] = True
                    rejected = invoke([ADAPTER], bad, allowed=(2,))
                    if rejected['status'] != 'simulation_error':
                        raise RuntimeError('malformed adapter request was not rejected')

            saved_path = HERE / 'fixtures' / (name + '.regression.json')
            if args.record:
                write(saved_path, {'version': 1, 'case': name, 'goal': case['goal'], 'identity': case['identity'], 'inputs': inputs})
            saved = json.loads(saved_path.read_text())
            validate_saved(saved, case)
            saved_out = out / 'saved'
            saved_out.mkdir()
            if simulate(case, saved['inputs'], saved_out)[0] != 'reset_reachable_failure':
                raise RuntimeError('saved regression stopped failing')
            correct = prepared[case['family'] + '_correct']
            correct_out = out / 'correct-counterpart'
            correct_out.mkdir()
            if simulate(correct, saved['inputs'], correct_out)[0] != 'trace_no_failure':
                raise RuntimeError('correct counterpart failed the identical saved inputs')
        summary.append({'case': name, 'search_status': result['status'], 'evidence_kind': 'induction_countermodel_not_reached_within_bound' if name == 'counter_unreachable_fault' else result['status'], 'identity': case['identity']})
        print(json.dumps(summary[-1]), flush=True)
    write(args.out / 'summary.json', summary)

if __name__ == '__main__':
    main()
