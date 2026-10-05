#!/usr/bin/env python3
"""User-owned scalar Veryl project: bounded failure search and concrete regression replay."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
FRONTEND = ROOT / '../target/debug/lydite-celox-export'
ADAPTER = ROOT / '../target/debug/lydite-celox-replay'
CORE = ROOT / '../target/release/lydite-replay'
CHECKER = ROOT / '../target/release/lydite'
LIFTER = ROOT / '../target/release/lydite-celox-lift'

def canonical(v):
    return json.dumps(v, sort_keys=True, separators=(',', ':')).encode()

def sha(v):
    return hashlib.sha256(v).hexdigest()

def write(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')

def invoke(args, request=None, allowed=(0,), timeout=120):
    p = subprocess.run([str(a) for a in args], input=None if request is None else json.dumps(request), text=True, capture_output=True, timeout=timeout, env={**os.environ, 'LYDITE_SOLVER': 'finite'})
    if p.returncode not in allowed:
        raise RuntimeError(f'{args[0]} exit {p.returncode}: {p.stderr[-3000:]} {p.stdout[-1500:]}')
    return json.loads(p.stdout)

def core(doc, mode, **kw):
    return invoke([CORE], {'version': 1, 'document': doc, 'mode': mode, **kw})


def exact(value, fields, label):
    if not isinstance(value, dict) or set(value) != set(fields):
        raise ValueError(f'{label}: expected exactly {", ".join(fields)}')

def identifier(value):
    if not isinstance(value, str) or not re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]*', value):
        raise ValueError('mapping names must be plain top-level identifiers')
    return value

def scalar_width(ty):
    if ty == 'bool':
        return 1
    if isinstance(ty, dict) and set(ty) == {'bv'} and type(ty['bv']) is int and 1 <= ty['bv'] <= 64:
        return ty['bv']
    raise ValueError('only Bool/BV 1..64 scalar mappings supported')

def project_file(root, relative):
    if not isinstance(relative, str) or not relative or Path(relative).is_absolute() or '..' in Path(relative).parts:
        raise ValueError('project files must use relative paths inside the manifest directory')
    path = (root / relative).resolve()
    if not path.is_relative_to(root) or not path.is_file():
        raise ValueError(f'missing or escaping project file: {relative}')
    return path

def load_json(path):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f'duplicate JSON field: {key}')
            result[key] = value
        return result
    return json.loads(path.read_text(), object_pairs_hook=unique)

def load_manifest(path):
    root = path.resolve().parent
    manifest = load_json(path)
    exact(manifest, ['version', 'name', 'sources', 'top', 'specification', 'clock', 'reset', 'inputs', 'state', 'signals', 'property', 'depth'], 'manifest')
    if type(manifest['version']) is not int or manifest['version'] != 1 or type(manifest['depth']) is not int or not 1 <= manifest['depth'] <= 32:
        raise ValueError('manifest version must be 1 and depth must be 1..32')
    for key in ['name', 'top', 'clock']:
        identifier(manifest[key])
    if manifest['property'] not in ['safety', 'response_deadline']:
        raise ValueError('property must be safety or response_deadline')
    exact(manifest['reset'], ['input', 'active'], 'reset')
    identifier(manifest['reset']['input'])
    if type(manifest['reset']['active']) is not int or manifest['reset']['active'] not in [0, 1]:
        raise ValueError('reset active must be integer 0 or 1')
    for key in ['inputs', 'state', 'signals']:
        if not isinstance(manifest[key], dict):
            raise ValueError(f'{key} must be a mapping')
        for name, entry in manifest[key].items():
            identifier(name)
            if key == 'signals':
                exact(entry, ['signal', 'type'], 'sampled DUT signal')
                identifier(entry['signal'])
                scalar_width(entry['type'])
            else:
                identifier(entry)
        if key != 'signals' and len(set(manifest[key].values())) != len(manifest[key]):
            raise ValueError(f'duplicate physical {key} mapping')
    if set(manifest['inputs'].values()) & set(manifest['state'].values()):
        raise ValueError('input/state mappings overlap')
    if manifest['clock'] in set(manifest['inputs'].values()) | set(manifest['state'].values()):
        raise ValueError('clock cannot be driven or mapped as data/state')
    sources = manifest['sources']
    if not isinstance(sources, list) or not sources or any(not isinstance(s, str) for s in sources) or len(set(sources)) != len(sources):
        raise ValueError('sources must be a nonempty list of distinct relative filenames')
    resolved = [project_file(root, s) for s in sources]
    if len(set(resolved)) != len(resolved):
        raise ValueError('duplicate resolved source paths')
    design = {'top': manifest['top'], 'four_state': False, 'sources': [{'path': s, 'text': p.read_text()} for s, p in zip(sources, resolved)]}
    return manifest, design, project_file(root, manifest['specification'])

def check_dependencies():
    # The tools are built from the in-tree Celox; replay needs only their binaries.
    missing = [str(p) for p in (FRONTEND, ADAPTER, CORE, CHECKER, LIFTER) if not p.is_file()]
    if missing:
        raise ValueError('missing replay tools; build them first: ' + ', '.join(missing))

def mappings(manifest, doc, compiled):
    if doc.get('version') != 3 or doc.get('kind') != 'specification' or not isinstance(doc.get('implementation'), dict):
        raise ValueError('project requires a native v3 specification with implementation binding')
    impl = doc['implementation']
    if set(manifest['inputs']) != set(doc['inputs']) or set(manifest['state']) != set(impl['state']):
        raise ValueError('map every declared input and implementation state exactly once')
    if manifest['reset']['input'] != impl['reset_input'] or doc['inputs'].get(impl['reset_input']) != 'bool':
        raise ValueError('reset input must match the specification boolean reset_input')
    if set(manifest['signals']) != set(impl.get('wires', {})):
        raise ValueError('signals must map every implementation wire alias; source expressions replace those placeholders')
    if manifest['property'] == 'response_deadline' and len(impl.get('responses', {})) != 1:
        raise ValueError('response_deadline requires exactly one response contract')
    top = {}
    for signal in compiled['signals']:
        meta = signal['metadata']
        if meta['type_kind'].startswith('Reset') or signal['kind'] == 'Inout':
            raise ValueError('typed reset and inout signals are unsupported')
        if not signal['instances'] and len(signal['path']) == 1:
            name = signal['path'][0]
            if name in top:
                raise ValueError('ambiguous top-level signal')
            top[name] = signal
    clocks = [s for s in compiled['signals'] if s['metadata']['type_kind'] == 'Clock']
    if len(clocks) != 1 or clocks[0]['path'] != [manifest['clock']] or clocks[0]['instances'] or clocks[0]['metadata']['kind'] != 'ClockPosedge':
        raise ValueError('exactly one top-level positive-edge clock is supported')
    physical_inputs = {n for n, s in top.items() if s['kind'] == 'Input'} - {manifest['clock']}
    if set(manifest['inputs'].values()) != physical_inputs:
        raise ValueError('map every external nonclock input; no unknown/input-to-state mappings')
    def signal(name, ty, state=False):
        row = top.get(name)
        if row is None or row['metadata']['array_dims'] or row['metadata']['width'] != scalar_width(ty) or row['metadata']['type_kind'] not in ['Bit', 'Logic']:
            raise ValueError(f'unsupported signal/width/array mapping: {name}')
        if state and row['kind'] == 'Input':
            raise ValueError('state cannot map an input')
        return row
    for n, physical in manifest['inputs'].items():
        signal(physical, doc['inputs'][n])
    for n, physical in manifest['state'].items():
        signal(physical, impl['state'][n], state=True)
    for entry in manifest['signals'].values():
        signal(entry['signal'], entry['type'])
    # A selected register must not be overwritten by the combinational phase.
    comb_writes = set()
    def walk(value):
        if isinstance(value, dict):
            for op, args in value.items():
                if op in ['Store', 'Commit'] and isinstance(args, list) and args and isinstance(args[0], dict) and args[0].get('region', 0) == 0:
                    addr = args[0]
                    comb_writes.add((addr.get('instance_id'), addr.get('var_id')))
                walk(args)
        elif isinstance(value, list):
            for item in value:
                walk(item)
    walk(compiled['sir']['eval_comb'])
    for physical in manifest['state'].values():
        addr = top[physical]['address']
        if (addr['instance_id'], addr['var_id']) in comb_writes:
            raise ValueError('state mapping selects a combinationally assigned signal')
    inputs = {n: {'name': n, 'signal': physical, 'type': doc['inputs'][n]} for n, physical in manifest['inputs'].items()}
    reset = manifest['reset']['input']
    inputs[reset]['expr'] = ['not', 'i.' + reset] if manifest['reset']['active'] == 0 else 'i.' + reset
    return {'event': manifest['clock'], 'inputs': inputs, 'state': {n: {'signal': physical, 'type': impl['state'][n]} for n, physical in manifest['state'].items()}, 'outputs': manifest['signals']}

def prepare_project(manifest_path, out):
    manifest, design, spec_path = load_manifest(manifest_path)
    design_path = out / 'design.json'
    write(design_path, design)
    canonical_path = out / 'canonical.json'
    invoke([CHECKER, spec_path, '--emit-json', canonical_path, '--check', '--out', out / 'parse'])
    doc = json.loads(canonical_path.read_text())
    compiled = invoke([FRONTEND, design_path])
    if compiled.get('allowed_diagnostics'):
        raise ValueError('source replay does not permit frontend diagnostic waivers')
    compiled_path = out / 'compiled.json'
    write(compiled_path, compiled)
    bindings = mappings(manifest, doc, compiled)
    impl = doc['implementation']
    for reset in [False, True]:
        cfg = {**bindings, 'overrides': {manifest['reset']['input']: reset}}
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
            if set(lifted['wires']) & set(lifted['outputs']):
                raise ValueError('source wire alias collides with generated lifting wire')
            impl['next'] = lifted['next']
            impl['wires'] = {**lifted['wires'], **lifted['outputs']}
    write(out / 'model.json', doc)
    identity = {'manifest_sha256': sha(canonical(manifest)), 'sources_sha256': sha(canonical(design)), 'specification_sha256': sha(spec_path.read_bytes()), 'bindings_sha256': sha(canonical(bindings)), 'document_sha256': sha(canonical(doc))}
    project = {'name': manifest['name'], 'goal': manifest['property'], 'depth': manifest['depth'], 'manifest': manifest, 'design': design, 'document': doc, 'identity': identity}
    write(out / 'project-identity.json', identity)
    return project

def validate_saved(saved, project):
    exact(saved, ['version', 'project', 'goal', 'depth', 'identity', 'inputs'], 'regression')
    if type(saved['version']) is not int or saved['version'] != 2 or type(saved['depth']) is not int or saved['project'] != project['name'] or saved['goal'] != project['goal'] or saved['depth'] != project['depth'] or saved['identity'] != project['identity']:
        raise ValueError('stale or malformed regression identity')
    if not isinstance(saved['inputs'], list) or not 1 <= len(saved['inputs']) <= project['depth'] + 1:
        raise ValueError('invalid saved stimulus length')

class SimulationDivergence(ValueError):
    pass

def compare_simulation(expected, actual, state_mapping, signal_mapping=None):
    if isinstance(state_mapping, list):
        state_mapping = {n: n for n in state_mapping}
    if actual.get('status') != 'simulated' or len(actual.get('trace', [])) != len(expected):
        return {'status': 'simulator_divergence', 'reason': 'simulation identity/trace length mismatch'}
    for f, observed in zip(expected, actual['trace']):
        e = f['edge']
        if observed['edge'] != e:
            return {'status': 'simulator_divergence', 'reason': 'edge indexing mismatch'}
        for phase, field in [('after', 'state_after'), ('before', 'state_before')]:
            if field == 'state_before' and e == 0:
                continue
            for name, physical in state_mapping.items():
                if observed[phase].get(physical) != str(int(f[field][name]['value'])):
                    return {'status': 'simulator_divergence', 'edge': e, 'signal': physical, 'phase': phase}
        if e > 0:
            for name, entry in (signal_mapping or {}).items():
                if observed['before'].get(entry['signal']) != str(int(f['signal_samples'][name]['value'])):
                    return {'status': 'simulator_divergence', 'edge': e, 'signal': entry['signal'], 'phase': 'before'}
    return {'status': 'simulation_matches_validated_trace'}

def simulate(project, inputs, out):
    manifest = project['manifest']
    checked = core(project['document'], 'check_stimulus', goal=project['goal'], inputs=inputs, signals=list(manifest['signals']))
    expected = checked['trace']
    reset = manifest['reset']['input']
    frames = []
    for f in expected:
        row = {}
        for n, physical in manifest['inputs'].items():
            v = f['inputs'][n]
            row[physical] = int(not v) if n == reset and manifest['reset']['active'] == 0 else int(v)
        frames.append(row)
    observe = sorted(set(manifest['state'].values()) | {v['signal'] for v in manifest['signals'].values()})
    request = {'version': 1, 'design': project['design'], 'event': manifest['clock'], 'reset_input': manifest['inputs'][reset], 'reset_active': manifest['reset']['active'], 'observe': observe, 'frames': frames}
    actual = invoke([ADAPTER], request)
    comparison = compare_simulation(expected, actual, manifest['state'], manifest['signals'])
    write(out / 'simulation.json', actual)
    write(out / 'original-replay.json', checked)
    write(out / 'comparison.json', comparison)
    if comparison['status'] != 'simulation_matches_validated_trace':
        raise SimulationDivergence(json.dumps(comparison))
    return checked['status'], request

def search_project(manifest, out, save=None):
    project = prepare_project(manifest, out)
    result = core(project['document'], 'search', goal=project['goal'], depth=project['depth'])
    write(out / 'search.json', result)
    summary = {'status': result['status'], 'project': project['name'], 'identity': project['identity'], 'regression': None}
    if result.get('reason') is not None:
        summary['reason'] = result['reason']
    if result['status'] == 'reset_reachable_failure':
        witness = result['witness']
        core(project['document'], 'replay', witness=witness)
        inputs = [f['inputs'] for f in witness['trace']]
        if simulate(project, inputs, out)[0] != 'reset_reachable_failure':
            raise ValueError('searched failure did not reproduce')
        summary['simulation'] = 'simulation_matches_validated_trace'
        if save is not None:
            regression = {'version': 2, 'project': project['name'], 'goal': project['goal'], 'depth': project['depth'], 'identity': project['identity'], 'inputs': inputs}
            # Explicit destination; never overwrite an existing reviewed regression.
            with save.open('x') as file:
                file.write(json.dumps(regression, indent=2) + '\n')
            summary['regression'] = str(save)
    write(out / 'result.json', summary)
    return summary

def replay_project(manifest, regression, out):
    project = prepare_project(manifest, out)
    saved = load_json(regression)
    validate_saved(saved, project)
    status, _ = simulate(project, saved['inputs'], out)
    if status != 'reset_reachable_failure':
        raise ValueError('saved regression does not reproduce a property failure')
    summary = {'status': status, 'project': project['name'], 'identity': project['identity'], 'simulation': 'simulation_matches_validated_trace'}
    write(out / 'result.json', summary)
    return summary

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    search = sub.add_parser('search')
    search.add_argument('manifest', type=Path)
    search.add_argument('--out', type=Path, required=True)
    search.add_argument('--save-regression', type=Path)
    replay = sub.add_parser('replay')
    replay.add_argument('manifest', type=Path)
    replay.add_argument('regression', type=Path)
    replay.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    try:
        check_dependencies()
        args.out.mkdir(parents=True, exist_ok=False)
        result = search_project(args.manifest, args.out, args.save_regression) if args.command == 'search' else replay_project(args.manifest, args.regression, args.out)
        print(json.dumps(result))
    except (ValueError, KeyError, TypeError, OSError, RuntimeError, subprocess.SubprocessError) as error:
        print(json.dumps({'status': 'simulator_divergence' if isinstance(error, SimulationDivergence) else 'project_error', 'error': str(error)}))
        return 2
    return 0

if __name__ == '__main__':
    sys.exit(main())
