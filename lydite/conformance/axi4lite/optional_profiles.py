"""Source-backed controls for explicit optional response outputs."""
import copy
from pathlib import Path
import shutil
from protocols.axi4lite_project import replay


def run(root, out, good, cli, scenarios):
    results = []
    for width in (32, 64):
        explicit = root / ('profile-explicit-' + str(width)); shutil.copytree(good, explicit)
        if width == 64:
            source = explicit / 'subordinate.veryl'
            source.write_text(source.read_text().replace('bit<32>', 'bit<64>').replace('bit<4>', 'bit<8>'))
            spec = explicit / 'subordinate.lyd'
            spec.write_text(spec.read_text().replace('bv<32>', 'bv<64>').replace('0u32', '0u64').replace('bv<4>', 'bv<8>'))
            project = replay.load_json(explicit / 'project.json')
            for name, bits in [('bus_wdata', 64), ('bus_rdata', 64), ('bus_wstrb', 8)]: project['signals'][name]['type'] = {'bv': bits}
            replay.write(explicit / 'project.json', project)
            binding = replay.load_json(explicit / 'binding.json'); binding['config']['data_width'] = 64
            replay.write(explicit / 'binding.json', binding)
        omitted = root / ('profile-omitted-' + str(width)); shutil.copytree(explicit, omitted)
        source = omitted / 'subordinate.veryl'; text = source.read_text()
        spec = omitted / 'subordinate.lyd'; native = spec.read_text()
        project = replay.load_json(omitted / 'project.json'); binding = replay.load_json(omitted / 'binding.json')
        for name in ('bresp', 'rresp'):
            text = text.replace(', ' + name + ': output bit<2>', '').replace("    assign " + name + " = 2'd0;\n", '')
            native = native.replace('    bus_' + name + ' = 0u2;\n', '')
            del project['signals']['bus_' + name]; del binding['signals'][name]
        source.write_text(text); spec.write_text(native); replay.write(omitted / 'project.json', project)
        binding['version'] = 2
        binding['profile'] = {'name': 'subordinate_no_error_responses',
                              'capabilities': {'supports_exclusive_accesses': False, 'generates_error_responses': False},
                              'omitted': {name: {'port': name, 'type': {'bv': 2}} for name in ('bresp', 'rresp')}}
        replay.write(omitted / 'binding.json', binding)
        for kind, path in [('explicit', explicit), ('omitted', omitted)]:
            result = cli('search', path / 'binding.json', '--out', out / ('profile-' + kind + '-search-' + str(width)))
            if result['status'] != 'bounded_no_failure' or result['capacity_search'] != 'bounded_no_failure': raise RuntimeError(result)
            if kind == 'omitted':
                profile = result['signal_profile']
                if set(profile['defaults']) != {'bresp', 'rresp'} or profile['absence_evidence']['status'] != 'checked_against_frontend_reflection': raise RuntimeError('default provenance missing')
        for name in ('aw_first', 'w_first', 'simultaneous_backpressure', 'continuous'):
            frames, counts = scenarios()[name]; stimulus = root / ('profile-' + name + '.json'); replay.write(stimulus, frames)
            samples = []; states = []
            for kind, path in [('explicit', explicit), ('omitted', omitted)]:
                evidence = out / ('profile-' + kind + '-' + name + '-' + str(width))
                result = cli('stimulus', path / 'binding.json', '--inputs', stimulus, '--out', evidence)
                if result['status'] != 'trace_no_failure' or result['independent']['accepted_transfers'] != counts: raise RuntimeError('optional response counterpart failed')
                samples.append(replay.load_json(evidence / 'axi-samples.json'))
                states.append([frame['state_after'] for frame in replay.load_json(evidence / 'original-replay.json')['trace']])
            if samples[0] != samples[1] or states[0] != states[1]: raise RuntimeError('omitted default differs from explicit OKAY counterpart')
        results.append({'case': 'optional_response_equivalence_' + str(width), 'status': 'passed', 'scope': 'bounded search and four concrete source scenarios'})
        # Omission does not suppress B prerequisites or prevent saved failure replay.
        bad = root / ('profile-bad-response-' + str(width)); shutil.copytree(omitted, bad)
        path = bad / 'subordinate.veryl'; path.write_text(path.read_text().replace('assign bvalid = a_full && d_full;', 'assign bvalid = a_full;'))
        saved = out / ('profile-bad-response-' + str(width) + '.regression.json')
        result = cli('search', bad / 'binding.json', '--out', out / ('profile-bad-response-' + str(width)), '--regression', saved)
        if result['status'] != 'reset_reachable_failure' or 'b_requires_aw_w' not in {v['rule'] for v in result['independent']['guarantee_violations']}: raise RuntimeError('default profile masked protocol failure')
        again = cli('replay', bad / 'binding.json', '--out', out / ('profile-bad-response-replay-' + str(width)), '--regression', saved)
        if again['status'] != 'reset_reachable_failure': raise RuntimeError('profile regression lost')
        changed = copy.deepcopy(binding); changed['profile']['omitted']['bresp']['port'] = 'old_bresp'
        replay.write(bad / 'binding.json', changed)
        stale = cli('replay', bad / 'binding.json', '--out', out / ('profile-stale-' + str(width)), '--regression', saved, allowed=(2,))
        if stale['status'] != 'project_error' or 'identity' not in stale['error']: raise RuntimeError('profile changes did not invalidate saved regression')
        results.append({'case': 'optional_response_mutant_' + str(width), 'status': result['status']})
        if width != 32: continue
        controls = [
            ('missing_valid', lambda b: b['signals'].pop('bvalid'), 'required protocol signal'),
            ('present_as_absent', lambda b: b['profile']['omitted']['bresp'].update(port='bvalid'), 'present in frontend reflection'),
            ('unsupported_profile', lambda b: b['profile'].update(name='arbitrary_defaults'), 'unsupported optional profile'),
            ('manager_role', lambda b: b['config'].update(role='manager'), 'unsupported optional profile'),
            ('error_capability', lambda b: b['profile']['capabilities'].update(generates_error_responses=True), 'explicit false'),
            ('exclusive_capability', lambda b: b['profile']['capabilities'].update(supports_exclusive_accesses=True), 'explicit false'),
            ('implicit_capability', lambda b: b['profile']['capabilities'].pop('generates_error_responses'), 'profile capabilities'),
            ('coerced_capability', lambda b: b['profile']['capabilities'].update(generates_error_responses=0), 'explicit false'),
            ('wrong_width', lambda b: b['profile']['omitted']['rresp'].update(type={'bv': 1}), 'bv<2>'),
            ('float_width', lambda b: b['profile']['omitted']['rresp'].update(type={'bv': 2.0}), 'bv<2>'),
            ('partial_omission', lambda b: b['profile']['omitted'].pop('rresp'), 'omitted response pair'),
            ('duplicate_absent', lambda b: b['profile']['omitted']['rresp'].update(port='bresp'), 'distinct'),
            ('invented_default', lambda b: b['profile']['omitted']['bresp'].update(value=2), 'omitted response declaration'),
            ('wrong_direction', lambda b: b['signals'].update(bvalid=b['signals']['awvalid'], awvalid=b['signals']['bvalid']), 'role/direction mismatch'),
        ]
        for name, change, message in controls:
            altered = copy.deepcopy(binding); change(altered); file = omitted / (name + '.json'); replay.write(file, altered)
            result = cli('search', file, '--out', out / ('profile-reject-' + name), allowed=(2,))
            if result['status'] != 'project_error' or message not in result['error']: raise RuntimeError((name, result))
            results.append({'case': 'profile_reject_' + name, 'status': 'binding_rejected'})
        # Real response ports still present: neither matching nor fake absent names can hide them.
        for hidden in (False, True):
            altered = copy.deepcopy(binding)
            if hidden:
                for name in ('bresp', 'rresp'): altered['profile']['omitted'][name]['port'] = 'absent_' + name
            file = explicit / ('declared-absent-' + str(hidden) + '.json'); replay.write(file, altered)
            result = cli('search', file, '--out', out / ('profile-present-' + str(hidden)), allowed=(2,))
            expected = 'every top-level output mapped' if hidden else 'present in frontend reflection'
            if result['status'] != 'project_error' or expected not in result['error']: raise RuntimeError('existing response output hidden by profile')
            results.append({'case': 'profile_present_' + str(hidden), 'status': 'binding_rejected'})
        missing_profile = copy.deepcopy(binding); missing_profile['version'] = 1; missing_profile.pop('profile')
        file = omitted / 'implicit-defaults.json'; replay.write(file, missing_profile)
        result = cli('search', file, '--out', out / 'profile-implicit-defaults', allowed=(2,))
        if result['status'] != 'project_error' or 'no implicit defaults' not in result['error']: raise RuntimeError('absence silently defaulted')
        results.append({'case': 'profile_implicit_defaults', 'status': 'binding_rejected'})
    return results
