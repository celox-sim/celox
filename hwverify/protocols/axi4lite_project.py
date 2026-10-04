#!/usr/bin/env python3
"""Bind AXI4-Lite contracts to a user Veryl project; search and replay with Celox."""
import argparse
import json
from pathlib import Path
import sys
import subprocess

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(ROOT / 'conformance/celox-replay'))
import project as replay
import structure_project
from protocols.axi4lite import bind, parameters, signal_types, rules, UNCHECKED, SOURCE
from protocols.axi4lite_reference import check_trace


def expand(term, impl, reset=False, visiting=None):
    visiting = set() if visiting is None else visiting
    if isinstance(term, list): return [expand(x, impl, reset, visiting) for x in term]
    if not isinstance(term, str): return term
    if term.startswith('w.'):
        if term in visiting: raise ValueError('cyclic source wire')
        return expand(impl['wires'][term[2:]], impl, reset, visiting | {term})
    if reset and term.startswith('s.'):
        return expand(impl['reset'][term[2:]], impl, False)
    return term


def prepare(path, out):
    binding = replay.load_json(path)
    replay.exact(binding, ['version', 'project', 'config', 'signals'], 'AXI binding')
    if type(binding['version']) is not int or binding['version'] != 1: raise ValueError('AXI binding version must be 1')
    config = parameters(binding['config']); types = signal_types(config)
    if config['role'] == 'link': raise ValueError('source binding needs manager or subordinate role; link is for complete sampled traces')
    if not isinstance(binding['signals'], dict) or set(binding['signals']) != set(types) or any(not isinstance(n, str) for n in binding['signals'].values()) or len(set(binding['signals'].values())) != len(types):
        raise ValueError('map each protocol signal to a distinct project wire alias')
    project_path = replay.project_file(path.resolve().parent, binding['project'])
    project = replay.prepare_project(project_path, out)
    if project['goal'] != 'safety': raise ValueError('AXI protocol guarantees use safety; declare optional response contracts separately')
    impl = project['document']['implementation']
    compiled = json.loads((out / 'compiled.json').read_text())
    top = {s['path'][0]: s for s in compiled['signals'] if not s['instances'] and len(s['path']) == 1}
    manager_outputs = {'awvalid', 'awaddr', 'awprot', 'wvalid', 'wdata', 'wstrb', 'arvalid', 'araddr', 'arprot', 'bready', 'rready'}
    physical = []
    for name, alias in binding['signals'].items():
        mapped = project['manifest']['signals'].get(alias)
        if mapped is None or mapped['type'] != types[name]: raise ValueError('AXI signal alias missing or wrong type: ' + name)
        physical.append(mapped['signal'])
        dut_output = (name in manager_outputs) == (config['role'] == 'manager')
        if top[mapped['signal']]['kind'] != ('Output' if dut_output else 'Input'):
            raise ValueError('AXI role/direction mismatch: ' + name)
    if len(set(physical)) != len(physical): raise ValueError('AXI physical mappings alias each other')
    signals = {n: expand('w.' + alias, impl) for n, alias in binding['signals'].items()}
    # Reset combinational outputs must come from the reset lowering, not the
    # normal lowering (which has already substituted reset=False).
    reset_lift = replay.invoke([replay.LIFTER, out / 'compiled.json', out / 'reset-bindings.json', '--inline'])
    reset_impl = {'wires': reset_lift['wires'], 'reset': impl['reset']}
    reset_signals = {n: expand(reset_lift['outputs'][alias], reset_impl, reset=True) for n, alias in binding['signals'].items()}
    # Generic native structural obligations; no AXI-specific graph checker.
    structural = json.loads(json.dumps(project['document']))
    structural['components']['AxiStructure'] = {'state': {}, 'init': True, 'invariant': True, 'steps': {'tick': True}, 'examples': {}, 'structure': {'no_comb_path': {}}}
    structural['compositions']['AxiSourceStructure'] = {'members': [impl['composition'], 'AxiStructure'], 'examples': {}}
    structural['implementation']['composition'] = 'AxiSourceStructure'
    structural['implementation']['binding']['states']['AxiStructure'] = {}
    endpoints = structural['implementation'].setdefault('endpoints', {})
    inputs = {}; outputs = {}
    inverse_inputs = {physical: name for name, physical in project['manifest']['inputs'].items()}
    for protocol, alias in binding['signals'].items():
        physical = project['manifest']['signals'][alias]['signal']
        if top[physical]['kind'] == 'Input':
            logical = 'i.' + inverse_inputs[physical]; inputs[protocol] = logical
        else:
            name = 'axi_port_' + protocol
            if name in structural['observations']: raise ValueError('reserved structural observation collision')
            structural['observations'][name] = types[protocol]
            structural['implementation']['binding']['observations'][name] = False if types[protocol] == 'bool' else ['bv', types[protocol]['bv'], 0]
            logical = 'o.' + name; outputs[protocol] = logical
        if logical in endpoints and endpoints[logical] != physical: raise ValueError('conflicting structural endpoint binding')
        endpoints[logical] = physical
    for source, fr in inputs.items():
        for target, to in outputs.items():
            structural['components']['AxiStructure']['structure']['no_comb_path'][source + '_to_' + target] = {'from': fr, 'to': to}
    replay.write(out / 'structure-model.json', structural)
    project['structural'] = structure_project.check(structural, project['design'], compiled, out)
    project['identity']['structural_extractor_sha256'] = project['structural']['trusted_extractor_sha256']
    base = project['document']
    project['document'] = bind(base, config, signals, reset_signals)
    project['scope_document'] = bind(base, config, signals, reset_signals, 'scope')
    project['axi'] = binding
    project['identity']['axi_binding_sha256'] = replay.sha(replay.canonical(binding))
    project['identity']['sampled_phase_library_sha256'] = replay.sha((ROOT / 'protocols/sampled_phase.py').read_bytes())
    project['identity']['axi_library_sha256'] = replay.sha((ROOT / 'protocols/axi4lite.py').read_bytes())
    project['identity']['axi_replay_sha256'] = replay.sha(Path(__file__).read_bytes())
    project['identity']['axi_reference_sha256'] = replay.sha((ROOT / 'protocols/axi4lite_reference.py').read_bytes())
    project['identity']['document_sha256'] = replay.sha(replay.canonical(project['document']))
    replay.write(out / 'model.json', project['document'])
    replay.write(out / 'project-identity.json', project['identity'])
    replay.write(out / 'axi-contract.json', {'config': config, 'rules': rules(), 'normative_source': SOURCE, 'unchecked': UNCHECKED,
                 'assumptions': 'Only counterpart protocol rules on this prefix; no DUT guarantees, READY fairness or deadlines assumed',
                 'capacity': 'tool bound, not a protocol rule; overflow is reported separately'})
    return project


def simulate(project, inputs, out):
    status, request = replay.simulate(project, inputs, out)
    actual = json.loads((out / 'simulation.json').read_text())
    rows = []
    for frame in actual['trace']:
        edge = frame['edge']; sample = frame['after'] if edge == 0 else frame['before']
        row = {'rst': edge == 0}
        for name, alias in project['axi']['signals'].items():
            value = int(sample[project['manifest']['signals'][alias]['signal']])
            row[name] = bool(value) if signal_types(project['axi']['config'])[name] == 'bool' else value
        rows.append(row)
    independent = check_trace(rows, project['axi']['config'])
    independent['reset_release']['source_phases'] = [
        {'edge': frame['edge'], 'reset_active': frame['edge'] == 0,
         **{phase: {ch: bool(int(frame[phase][project['manifest']['signals'][project['axi']['signals'][ch + 'valid']]['signal']])) for ch in ('aw', 'w', 'ar', 'b', 'r')} for phase in ('before', 'after')}}
        for frame in actual['trace'][:2]]
    independent['reset_release']['scope'] = 'one initial reset: settled before/after samples from original Celox source; no physical timing or repeated-reset claim'
    # Source-independent native specs may fail too. AXI-only failure must agree.
    checked = json.loads((out / 'original-replay.json').read_text())
    flags = any(v['value'] for n, v in checked['trace'][-1]['state_after'].items() if n.startswith('axi_bad_') and
                (project['axi']['config']['role'] == 'link' or rules()[n[len('axi_bad_'):]]['owner'] == project['axi']['config']['role']))
    if flags != bool(independent['guarantee_violations']):
        raise replay.SimulationDivergence('independent AXI oracle disagrees with generated monitor')
    replay.write(out / 'axi-samples.json', rows)
    replay.write(out / 'axi-independent.json', independent)
    return status, independent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['search', 'replay', 'stimulus'])
    parser.add_argument('binding', type=Path)
    parser.add_argument('--inputs', type=Path, help='Complete canonical external-input frames, initial reset then nonreset')
    parser.add_argument('--regression', type=Path)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    try:
        replay.check_dependencies()
        args.out.mkdir(parents=True, exist_ok=False)
        project = prepare(args.binding, args.out)
        if args.mode == 'search':
            result = replay.core(project['document'], 'search', goal='safety', depth=project['depth'])
            scope = replay.core(project['scope_document'], 'search', goal='safety', depth=project['depth'])
            replay.write(args.out / 'search.json', result); replay.write(args.out / 'scope-search.json', scope)
            summary = {'status': result['status'], 'capacity_search': scope['status'], 'depth': project['depth'], 'identity': project['identity'], 'unchecked': UNCHECKED}
            if result['status'] == 'reset_reachable_failure':
                replay.core(project['document'], 'replay', witness=result['witness'])
                inputs = [f['inputs'] for f in result['witness']['trace']]
                status, independent = simulate(project, inputs, args.out)
                if status != result['status']: raise ValueError('failure not reproduced')
                summary['independent'] = independent
                if args.regression:
                    saved = {'version': 2, 'project': project['name'], 'goal': project['goal'], 'depth': project['depth'], 'identity': project['identity'], 'inputs': inputs}
                    with args.regression.open('x') as file: file.write(json.dumps(saved, indent=2) + '\n')
            elif scope['status'] != 'bounded_no_failure':
                summary['status'] = 'scope_exceeded' if scope['status'] == 'reset_reachable_failure' else 'unknown'
                if scope['status'] == 'reset_reachable_failure':
                    replay.core(project['scope_document'], 'replay', witness=scope['witness'])
                    inputs = [f['inputs'] for f in scope['witness']['trace']]
                    _, independent = simulate(project, inputs, args.out)
                    if not independent['capacity_exceeded'] or independent['environment_violations']:
                        raise replay.SimulationDivergence('capacity witness does not reproduce on a legal counterpart prefix')
                    summary['independent'] = independent
        else:
            if args.mode == 'replay':
                if args.regression is None: raise ValueError('replay requires --regression')
                saved = replay.load_json(args.regression); replay.validate_saved(saved, project); inputs = saved['inputs']
            else:
                if args.inputs is None: raise ValueError('stimulus requires --inputs')
                inputs = replay.load_json(args.inputs)
            status, independent = simulate(project, inputs, args.out)
            if args.mode == 'replay' and status != 'reset_reachable_failure': raise ValueError('saved failure did not reproduce')
            summary = {'status': (independent['status'] if status == 'trace_no_failure' and independent['status'] != 'sampled_prefix_passed' else status), 'independent': independent, 'identity': project['identity'], 'unchecked': UNCHECKED}
        summary['write_pairing'] = summary.get('independent', {}).get('write_pairing', {'status': 'not_established_by_bounded_search', 'scope': 'Known-pair safety only; counterpart arrival and completion are not established'})
        summary['reset_release'] = summary.get('independent', {}).get('reset_release', {'status': 'included_in_bounded_conditional_checks', 'scope': 'one initial reset; manager pre-edge obligation only; no physical timing claim'})
        summary['structural'] = project['structural']
        if project['structural']['status'] == 'verified':
            summary['unchecked'] = ['synthesized-netlist/physical combinational paths'] + [item for item in UNCHECKED if item != 'input-to-output combinational paths']
        if project['structural']['status'] == 'violated':
            summary['status'] = 'structural_violation'
        elif project['structural']['status'] != 'verified' and summary['status'] in ('bounded_no_failure', 'trace_no_failure'):
            summary['status'] = 'unknown'
        summary['environment_nonvacuity'] = ('Not established by search; provide a legal positive stimulus or separate cover' if args.mode == 'search' else 'Concrete prefix only; inspect independent environment violations and accepted transfer counts')
        summary['claim'] = 'Bounded sampled safety conditional on a legal counterpart prefix and declared capacity; not complete AXI compliance'
        replay.write(args.out / 'result.json', summary)
        print(json.dumps(summary))
    except (ValueError, KeyError, TypeError, OSError, RuntimeError, subprocess.SubprocessError) as error:
        # Tool/project failure is never a verification success.
        print(json.dumps({'status': 'simulator_divergence' if isinstance(error, replay.SimulationDivergence) else 'project_error', 'error': str(error)})); return 2
    return 0

if __name__ == '__main__': sys.exit(main())
