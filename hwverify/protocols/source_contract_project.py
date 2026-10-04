#!/usr/bin/env python3
"""Bind reusable source contracts to an external scalar Veryl project.

Runs each named obligation separately through the existing native safety engine.
No reference function, offer meaning, or application capability is inferred.
"""
import argparse
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from protocols.axi4lite_project import replay, expand
from protocols.source_contracts import fifo_read, idle_offer_step, memory_write


def prepare(path, out, obligation):
    config = replay.load_json(path)
    replay.exact(config, ['version', 'project', 'contract'], 'source contract binding')
    if type(config['version']) is not int or config['version'] != 1: raise ValueError('source contract version must be 1')
    project = replay.prepare_project(replay.project_file(path.resolve().parent, config['project']), out)
    if project['goal'] != 'safety': raise ValueError('source contract requires safety project')
    doc = project['document']; imp = doc['implementation']; contract = config['contract']; manifest = project['manifest']
    # Native state bindings have no phase-dependent output expression. Validate
    # their settled reset values against a separate reset=True source lowering.
    reset_lift = replay.invoke([replay.LIFTER, out / 'compiled.json', out / 'reset-bindings.json', '--inline'])
    reset_impl = {'wires': reset_lift['wires'], 'reset': imp['reset']}
    reset_outputs = {}
    def state(alias, ty):
        if not isinstance(alias, str) or imp['state'].get(alias) != ty: raise ValueError('missing/wrong-width source state: ' + str(alias))
        return 's.' + alias
    def inp(alias, ty):
        if not isinstance(alias, str) or doc['inputs'].get(alias) != ty or alias == imp['reset_input']: raise ValueError('missing/wrong-width contract input: ' + str(alias))
        return 'i.' + alias
    def output(alias, ty, require_port=True):
        entry = manifest['signals'].get(alias)
        if entry is None or entry['type'] != ty: raise ValueError('missing/wrong-width source output: ' + str(alias))
        compiled = replay.load_json(out / 'compiled.json')
        top = {s['path'][0]: s for s in compiled['signals'] if not s['instances'] and len(s['path']) == 1}
        if require_port and top[entry['signal']]['kind'] != 'Output': raise ValueError('contract output must map an actual output port')
        if top[entry['signal']]['kind'] == 'Input': raise ValueError('effect signal must not map an external input')
        term = expand('w.' + alias, imp)
        def state_only(v):
            if isinstance(v, str) and not v.startswith('s.'): raise ValueError('contract output must be state-only')
            if isinstance(v, list):
                for x in v[1:]: state_only(x)
        state_only(term)
        normal_at_reset = expand(term, imp, reset=True)
        actual_reset = expand(reset_lift['outputs'][alias], reset_impl, reset=True)
        def closed(v):
            if isinstance(v, str): raise ValueError('unsupported input-dependent reset output binding: ' + alias)
            if isinstance(v, list):
                for x in v[1:]: closed(x)
        closed(normal_at_reset); closed(actual_reset)
        reset_outputs[alias] = {'type': ty, 'normal': normal_at_reset, 'actual': actual_reset, 'port': entry['signal']}
        return term
    if contract['kind'] == 'fifo_read':
        replay.exact(contract, ['kind', 'name', 'capacity', 'request_width', 'response_width', 'count', 'slots', 'request_valid', 'request_payload', 'request_ready', 'response_valid', 'response_data', 'response_ready', 'response_function'], 'FIFO source contract')
        cap = contract['capacity']; aw = contract['request_width']; dw = contract['response_width']
        if type(cap) is not int or not 1 <= cap <= 16: raise ValueError('FIFO capacity must be 1..16')
        if not isinstance(contract['slots'], list) or len(contract['slots']) != cap or len(set(contract['slots'])) != cap: raise ValueError('map distinct FIFO storage slots')
        bindings = {'count': state(contract['count'], {'bv': cap.bit_length()}),
                    **{'slot_' + str(i): state(alias, {'bv': aw}) for i, alias in enumerate(contract['slots'])},
                    'ready': output(contract['request_ready'], 'bool'), 'valid': output(contract['response_valid'], 'bool'),
                    'data': output(contract['response_data'], {'bv': dw})}
        documents = fifo_read(doc, contract['name'], cap, aw, dw, bindings,
                              inp(contract['request_valid'], 'bool'), inp(contract['request_payload'], {'bv': aw}),
                              inp(contract['response_ready'], 'bool'), contract['response_function'])
        evidence = {'kind': 'explicit_source_fifo_refinement', 'request_semantics': contract['response_function'],
                    'origin_evidence': 'actual source storage transitions and output correspondence; no monitor-assigned response tags',
                    'limits': 'bounded safety; no eventual response; equal response values do not distinguish physical origins; function is user-supplied semantics'}
    elif contract['kind'] == 'idle_offer_step':
        replay.exact(contract, ['kind', 'name', 'busy', 'valids', 'offer', 'completion_valid', 'completion_ready'], 'offer source contract')
        if not isinstance(contract['valids'], list) or not contract['valids'] or len(set(contract['valids'])) != len(contract['valids']): raise ValueError('map distinct offered VALID outputs')
        bindings = {'busy': state(contract['busy'], 'bool'), **{'valid_' + str(i): output(alias, 'bool') for i, alias in enumerate(contract['valids'])}}
        physical_valids = [manifest['signals'][alias]['signal'] for alias in contract['valids']]
        if len(set(physical_valids)) != len(physical_valids):
            raise ValueError('offered VALID aliases must resolve to distinct physical output ports')
        # Equal normal expressions on distinct physical ports are allowed.
        # Completion output is state-only and is not an assumed environment guarantee.
        complete = ['and', inp(contract['completion_valid'], 'bool'), output(contract['completion_ready'], 'bool')]
        documents = idle_offer_step(doc, contract['name'], bindings, inp(contract['offer'], 'bool'), complete)
        evidence = {'kind': 'explicit_application_idle_offer', 'availability': 'not busy and no pending bound VALID',
                    'limits': 'one-step application contract, not AXI normative latency or global READY-history noninterference; application offer meaning is declared'}
    elif contract['kind'] == 'memory_write':
        replay.exact(contract, ['kind','name','address_width','data_width','locations','state','signals','inputs'], 'memory source contract')
        aw = contract['address_width']; dw = contract['data_width']
        if type(dw) is not int or dw not in (32,64): raise ValueError('memory data width must be 32 or 64')
        if not isinstance(contract['locations'],list): raise ValueError('declare explicit memory locations')
        cells = []; addresses = []; initial = []
        for location in contract['locations']:
            replay.exact(location,['address','state','initial'],'memory location')
            addresses.append(location['address']); initial.append(location['initial']); cells.append(location['state'])
        if len(set(cells)) != len(cells): raise ValueError('memory cells must map distinct actual state registers')
        state_types = {'aw_pending':'bool','w_pending':'bool','applied':'bool','address':{'bv':aw},'data':{'bv':dw},'strobes':{'bv':dw//8}}
        signal_types = {'apply':'bool','aw_ready':'bool','w_ready':'bool','b_valid':'bool','b_response':{'bv':2},'read_ready':'bool','read_valid':'bool','read_data':{'bv':dw},'read_response':{'bv':2}}
        input_types = {'aw_valid':'bool','aw_address':{'bv':aw},'w_valid':'bool','w_data':{'bv':dw},'w_strobes':{'bv':dw//8},'b_ready':'bool','ar_valid':'bool','ar_address':{'bv':aw},'r_ready':'bool'}
        for group, types in [('state',state_types),('signals',signal_types),('inputs',input_types)]: replay.exact(contract[group],types,'memory '+group)
        bindings = {name:state(contract['state'][name],ty) for name,ty in state_types.items()}
        bindings.update({'cell_'+str(i):state(alias,{'bv':dw}) for i,alias in enumerate(cells)})
        bindings.update({name:output(contract['signals'][name],ty,require_port=name!='apply') for name,ty in signal_types.items()})
        inputs = {name:inp(contract['inputs'][name],ty) for name,ty in input_types.items()}
        documents = memory_write(doc,contract['name'],aw,dw,addresses,initial,bindings,inputs)
        evidence = {'kind':'explicit_source_memory_effects','semantics':'little-endian byte strobes, aligned word decode, declared reset contents; unmapped writes have no effect and return DECERR; readback is one-edge read-before-write',
                    'origin_evidence':'actual accepted AW/W storage, actual apply/applied signals, actual memory-register transitions and response pins; no monitor response identities',
                    'limits':'single outstanding paired write; no eventual application/response, no general peripheral side-effect claim; source semantics and effect-event meaning are declared'}
    else: raise ValueError('unsupported source contract kind')
    if obligation not in documents: raise ValueError('choose obligation: ' + ', '.join(documents))
    project['reset_output_binding'] = validate_reset_outputs(reset_outputs, out)
    project['document'] = documents[obligation]
    project['identity'].update({'source_contract_sha256': replay.sha(replay.canonical(config)),
                                'source_contract_library_sha256': replay.sha((ROOT / 'protocols/source_contracts.py').read_bytes()),
                                'source_contract_driver_sha256': replay.sha(Path(__file__).read_bytes()),
                                'source_expansion_sha256': replay.sha((ROOT / 'protocols/axi4lite_project.py').read_bytes()),
                                'native_expression_library_sha256': replay.sha((ROOT / 'protocols/axi4lite.py').read_bytes()),
                                'source_project_driver_sha256': replay.sha(Path(replay.__file__).read_bytes()),
                                'obligation_sha256': replay.sha(obligation.encode()),
                                'document_sha256': replay.sha(replay.canonical(project['document']))})
    project['contract_evidence'] = {**evidence, 'reset_output_binding': project['reset_output_binding'], 'checked_scope': 'original specification plus selected obligation; a failure may originate in either', 'obligation': obligation, 'independent_obligations': list(documents), 'binding': contract}
    replay.write(out / 'model.json', project['document']); replay.write(out / 'project-identity.json', project['identity'])
    replay.write(out / 'contract-evidence.json', project['contract_evidence'])
    return project


def validate_reset_outputs(outputs, out):
    """Use the native typed evaluator, not Python expression equivalence rules.

    Only closed settled reset values are admitted. Different reset/normal values
    cannot be represented by a single state-only binding, even when the differing
    payload is irrelevant to a particular protocol rule. Reject rather than claim
    an initialization property about a substituted output.
    """
    state = {}; values = {}; checks = []
    for index, entry in enumerate(outputs.values()):
        for phase in ('normal', 'actual'):
            name = phase + '_' + str(index); state[name] = entry['type']; values[name] = entry[phase]
        checks.append(['eq', 's.normal_' + str(index), 's.actual_' + str(index)])
    predicate = True
    for check in checks: predicate = ['and', predicate, check]
    document = {'version': 3, 'kind': 'specification', 'name': 'Source output reset-phase binding',
                'inputs': {'rst': 'bool'}, 'observations': {}, 'operations': {'tick': {}},
                'components': {'ResetBinding': {'state': {'matches': 'bool'}, 'init': 's.matches', 'invariant': True, 'steps': {'tick': True}, 'examples': {}}},
                'compositions': {'ResetChecked': {'members': ['ResetBinding'], 'examples': {}}},
                'implementation': {'composition': 'ResetChecked', 'reset_input': 'rst', 'state': state, 'reset': values,
                                   'next': {name: 's.' + name for name in state}, 'wires': {}, 'operations': {'tick': True},
                                   'binding': {'states': {'ResetBinding': {'matches': predicate}}, 'observations': {}}}}
    replay.write(out / 'reset-output-bindings.json', outputs)
    replay.write(out / 'reset-output-model.json', document)
    checked = replay.core(document, 'check_stimulus', goal='safety', inputs=[{'rst': True}])
    replay.write(out / 'reset-output-check.json', checked)
    after = checked['trace'][0]['state_after']
    if checked['status'] != 'trace_no_failure':
        mismatches = [alias for index, alias in enumerate(outputs) if after['normal_' + str(index)]['value'] != after['actual_' + str(index)]['value']]
        raise ValueError('unsupported reset-dependent output binding: ' + ', '.join(mismatches) + '; normal state abstraction differs from actual reset lowering')
    return {'status': 'checked_against_separate_reset_lowering', 'scope': 'settled post-reset-edge values only; no waveform or asynchronous timing claim',
            'outputs': {alias: {'port': entry['port'], 'type': entry['type'], 'value': after['actual_' + str(index)]['value']}
                        for index, (alias, entry) in enumerate(outputs.items())}}


def simulate(project, inputs, out):
    result = replay.simulate(project, inputs, out)
    actual = replay.load_json(out / 'simulation.json')['trace'][0]['after']
    for alias, entry in project['reset_output_binding']['outputs'].items():
        if actual.get(entry['port']) != str(int(entry['value'])):
            comparison = {'status': 'simulator_divergence', 'edge': 0, 'phase': 'after', 'signal': entry['port'], 'alias': alias,
                          'reason': 'reset output differs from separately validated source binding'}
            replay.write(out / 'comparison.json', comparison)
            raise replay.SimulationDivergence(json.dumps(comparison))
    replay.write(out / 'reset-output-comparison.json', {'status': 'simulation_matches_reset_output_binding', 'edge': 0,
                                                     'outputs': project['reset_output_binding']['outputs']})
    return result


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('mode', choices=['search', 'stimulus', 'replay']); p.add_argument('binding', type=Path)
    p.add_argument('--obligation', required=True); p.add_argument('--out', type=Path, required=True)
    p.add_argument('--inputs', type=Path); p.add_argument('--regression', type=Path)
    args = p.parse_args()
    try:
        replay.check_dependencies(); args.out.mkdir(parents=True, exist_ok=False)
        project = prepare(args.binding, args.out, args.obligation)
        result = {'identity': project['identity'], 'contract': project['contract_evidence']}
        if args.mode == 'search':
            checked = replay.core(project['document'], 'search', goal='safety', depth=project['depth'])
            replay.write(args.out / 'search.json', checked); result['status'] = checked['status']; result['depth'] = project['depth']
            if checked.get('reason') is not None: result['reason'] = checked['reason']
            if checked['status'] == 'reset_reachable_failure':
                replay.core(project['document'], 'replay', witness=checked['witness'])
                inputs = [f['inputs'] for f in checked['witness']['trace']]
                if simulate(project, inputs, args.out)[0] != 'reset_reachable_failure': raise ValueError('failure did not reproduce')
                result['simulation'] = 'simulation_matches_validated_trace'
                if args.regression:
                    saved = {'version': 2, 'project': project['name'], 'goal': project['goal'], 'depth': project['depth'], 'identity': project['identity'], 'inputs': inputs}
                    with args.regression.open('x') as file: file.write(json.dumps(saved, indent=2) + '\n')
        else:
            if args.mode == 'replay':
                if args.regression is None: raise ValueError('replay requires --regression')
                saved = replay.load_json(args.regression); replay.validate_saved(saved, project); inputs = saved['inputs']
            else:
                if args.inputs is None: raise ValueError('stimulus requires --inputs')
                inputs = replay.load_json(args.inputs)
            result['status'], _ = simulate(project, inputs, args.out)
            result['simulation'] = 'simulation_matches_validated_trace'
            if args.mode == 'replay' and result['status'] != 'reset_reachable_failure': raise ValueError('saved failure did not reproduce')
        replay.write(args.out / 'result.json', result); print(json.dumps(result))
    except (ValueError, KeyError, TypeError, OSError, RuntimeError, subprocess.SubprocessError) as error:
        print(json.dumps({'status': 'simulator_divergence' if isinstance(error, replay.SimulationDivergence) else 'project_error', 'error': str(error)})); return 2
    return 0

if __name__ == '__main__': sys.exit(main())
