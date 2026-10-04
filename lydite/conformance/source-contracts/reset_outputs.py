"""Reset-mux controls for state-only source contract output bindings."""
import re
import shutil
from protocols.axi4lite_project import replay


def run(root, good, cli, axi, out):
    results = []
    # All these normal-phase outputs are unchanged. The reset mux alone changes
    # actual pins. VALID controls expose false init claims; data/READY controls
    # enforce representability without inventing protocol reset-value rules.
    cases = [
        ('manager', 'awvalid', 'launch', 'write-offer.json', 'bool'),
        ('manager', 'bready', 'resource', 'write-offer.json', 'bool'),
        ('read', 'rvalid', 'response', 'read-contract.json', 'bool'),
        ('read', 'rvalid', 'stall', 'read-contract.json', 'bool'),
        ('read', 'rvalid', 'storage', 'read-contract.json', 'bool'),
        ('read', 'rvalid', 'capacity', 'read-contract.json', 'bool'),
        ('read', 'rdata', 'response', 'read-contract.json', 'bv'),
        ('read', 'arready', 'storage', 'read-contract.json', 'bool'),
    ]
    for active in (0, 1):
        for kind, signal, obligation, binding, ty in cases:
            name = f'reset-mux-{active}-{signal}-{obligation}'
            path = root / name; shutil.copytree(good, path)
            source = path / ('manager.veryl' if kind == 'manager' else 'ordered_read.veryl')
            manifest_path = path / ('manager-project.json' if kind == 'manager' else 'read-project.json')
            manifest = replay.load_json(manifest_path)
            header, body = source.read_text().split(') {', 1)
            private = signal + '_internal'; body = re.sub(r'\b' + signal + r'\b', private, body)
            if active == 1: body = body.replace('!rst_n', 'rst_n')
            reset = '!rst_n' if active == 0 else 'rst_n'
            # READY is normally high at reset-established state; force it low.
            expression = ('!' + '(' + reset + ') && ' + private) if signal.endswith('ready') else (reset + ' || ' + private)
            if ty == 'bv':
                connection = 'always_comb { if ' + reset + ' { ' + signal + " = 32'd99; } else { " + signal + ' = ' + private + '; } }'
            else: connection = 'assign ' + signal + ' = ' + expression + ';'
            source.write_text(header + ') {\n    var ' + private + ': ' + ('bit<32>' if ty == 'bv' else 'bit') + ';\n    ' + connection + '\n' + body)
            if signal in manifest['state']: manifest['state'][signal] = private
            manifest['reset']['active'] = active; replay.write(manifest_path, manifest)
            result = cli(name, 'search', path / binding, obligation, allowed=(2,))
            if result['status'] != 'project_error' or 'unsupported reset-dependent output binding' not in result['error']:
                raise RuntimeError(('reset mux silently accepted', name, result))
            if kind == 'read' and signal == 'rvalid':
                flags = {'rst', 'awvalid', 'wvalid', 'arvalid', 'bready', 'rready'}
                row = {k: False if k in flags else 0 for k in manifest['inputs']}
                inputs = path / 'reset-idle.json'; replay.write(inputs, [{**row, 'rst': True}, row])
                concrete = cli(name + '-stimulus', 'stimulus', path / binding, obligation, '--inputs', inputs, allowed=(2,))
                if concrete['status'] != 'project_error' or 'reset-dependent output binding' not in concrete['error']: raise RuntimeError('reset-idle stimulus silently accepted')
                if obligation == 'response':
                    bus = axi(name + '-axi', 'stimulus', path / 'axi-binding.json', '--inputs', inputs)
                    if bus['status'] != 'reset_reachable_failure' or 'subordinate_reset_valid' not in {v['rule'] for v in bus['independent']['guarantee_violations']}: raise RuntimeError('AXI reset control failed')
            results.append({'case': name, 'status': 'passed', 'disposition': 'unsupported_binding_rejected'})
        # A reset-gated VALID whose actual reset value still equals the abstraction
        # is supported. Validate both the native phases and Celox's actual pins.
        name = f'reset-compatible-{active}'
        path = root / name; shutil.copytree(good, path)
        source = path / 'manager.veryl'; header, body = source.read_text().split(') {', 1)
        body = re.sub(r'\bawvalid\b', 'awvalid_internal', body)
        if active == 1: body = body.replace('!rst_n', 'rst_n')
        released = 'rst_n' if active == 0 else '!rst_n'
        source.write_text(header + ') {\n    var awvalid_internal: bit;\n    assign awvalid = ' + released + ' && awvalid_internal;\n' + body)
        manifest_path = path / 'manager-project.json'; manifest = replay.load_json(manifest_path)
        manifest['state']['awvalid'] = 'awvalid_internal'; manifest['reset']['active'] = active; replay.write(manifest_path, manifest)
        row = {n: False if n not in ('bresp', 'rresp', 'rdata') else 0 for n in manifest['inputs']}
        inputs = path / 'inputs.json'; replay.write(inputs, [{**row, 'rst': True}, {**row, 'start_write': True}, row])
        result = cli(name, 'stimulus', path / 'write-offer.json', 'launch', '--inputs', inputs)
        if result['status'] != 'trace_no_failure': raise RuntimeError(result)
        compared = replay.load_json(out / name / 'reset-output-comparison.json')
        actual = replay.load_json(out / name / 'simulation.json')['trace'][0]['after']
        if compared['status'] != 'simulation_matches_reset_output_binding' or actual['awvalid'] != '0': raise RuntimeError('reset source comparison missing')
        # Mutation of the actual simulator response tests the new comparison at
        # edge zero, which the generic source replay intentionally does not check.
        from unittest.mock import patch
        from protocols.source_contract_project import prepare, simulate
        project_out = out / (name + '-comparison-control'); project_out.mkdir()
        project = prepare(path / 'write-offer.json', project_out, 'launch')
        shutil.copyfile(out / name / 'simulation.json', project_out / 'simulation.json')
        tampered = replay.load_json(project_out / 'simulation.json'); tampered['trace'][0]['after']['awvalid'] = '1'; replay.write(project_out / 'simulation.json', tampered)
        with patch.object(replay, 'simulate', return_value=('trace_no_failure', {})):
            try: simulate(project, [], project_out)
            except replay.SimulationDivergence: pass
            else: raise RuntimeError('reset sample divergence was ignored')
        results.append({'case': name, 'status': 'passed', 'disposition': 'phase_equivalence_and_actual_reset_sample_checked'})
    return results
