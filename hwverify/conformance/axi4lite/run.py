#!/usr/bin/env python3
"""Real source tests through the public AXI project API, with explicit expectations."""
import argparse
import copy
import json
from pathlib import Path
import shutil
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from protocols.axi4lite_project import replay

CLI = ROOT / 'protocols/axi4lite_project.py'
EXAMPLE = ROOT / 'examples/axi4lite'
MUTANTS = {
    'b_reset_comb': ('assign bvalid = a_full && d_full;', 'assign bvalid = !rst_n || (a_full && d_full);', 'subordinate_reset_valid'),
    'b_aw_only': ('assign bvalid = a_full && d_full;', 'assign bvalid = a_full;', 'b_requires_aw_w'),
    'b_w_only': ('assign bvalid = a_full && d_full;', 'assign bvalid = d_full;', 'b_requires_aw_w'),
    'b_drop_stalled': ('if bvalid && bready {', 'if bvalid {', 'b_valid_stable'),
    'b_duplicate': ('if bvalid && bready { a_full = 0; d_full = 0; }', 'if bvalid && bready { a_full = 1; d_full = 1; }', 'b_requires_aw_w'),
    'r_duplicate': ('if rvalid && rready { r_full = 0; }', 'if rvalid && rready { r_full = 1; }', 'r_requires_ar'),
    'r_unstable_data': ('if rvalid && rready { r_full = 0; }', "if rvalid && rready { r_full = 0; }\n            if rvalid { rdata = rdata + 32'd1; }", 'r_payload_stable'),
    'b_exokay': ("assign bresp = 2'd0;", "assign bresp = 2'd1;", 'b_response_code'),
    'r_exokay': ("assign rresp = 2'd0;", "assign rresp = 2'd1;", 'r_response_code'),
    'r_unsolicited': ('assign rvalid = r_full;', 'assign rvalid = !r_full;', 'subordinate_reset_valid'),
}

def cli(*args, allowed=(0,)):
    return replay.invoke([sys.executable, CLI, *args], allowed=allowed, timeout=120)

def inputs(**kw):
    manifest = json.loads((EXAMPLE / 'project.json').read_text())
    values = {n: False if n in ('rst', 'awvalid', 'wvalid', 'arvalid', 'bready', 'rready') else 0 for n in manifest['inputs']}
    return {**values, **kw}

def scenarios():
    z = inputs(rst=True)
    return {
        'reset_idle': ([z, inputs(), inputs()], {'aw': 0, 'w': 0, 'b': 0, 'ar': 0, 'r': 0}),
        'aw_first': ([z, inputs(awvalid=True), inputs(), inputs(wvalid=True, wdata=7, wstrb=15), inputs(), inputs(), inputs(bready=True)], {'aw': 1, 'w': 1, 'b': 1, 'ar': 0, 'r': 0}),
        'w_first': ([z, inputs(wvalid=True, wdata=7, wstrb=0), inputs(), inputs(awvalid=True), inputs(), inputs(), inputs(bready=True)], {'aw': 1, 'w': 1, 'b': 1, 'ar': 0, 'r': 0}),
        'simultaneous_backpressure': ([z, inputs(awvalid=True, wvalid=True, arvalid=True), inputs(), inputs(), inputs(bready=True, rready=True)], {'aw': 1, 'w': 1, 'b': 1, 'ar': 1, 'r': 1}),
        'continuous': ([z] + [inputs(awvalid=True, wvalid=True, arvalid=True, bready=True, rready=True)] * 6, {'aw': 3, 'w': 3, 'b': 3, 'ar': 3, 'r': 3}),
        'pending_without_deadline': ([z, inputs(awvalid=True)] + [inputs()] * 12, {'aw': 1, 'w': 0, 'b': 0, 'ar': 0, 'r': 0}),
    }

def main():
    parser = argparse.ArgumentParser(description=__doc__); parser.add_argument('--out', type=Path, required=True); args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=False)
    results = []
    with tempfile.TemporaryDirectory(prefix='external AXI projects ') as temp:
        root = Path(temp)
        # Actual copied user-owned project, not a built-in fixture name path.
        good = root / 'correct'; shutil.copytree(EXAMPLE, good)
        result = cli('search', good / 'binding.json', '--out', args.out / 'correct-search')
        if result['status'] != 'bounded_no_failure' or result['capacity_search'] != 'bounded_no_failure': raise RuntimeError(result)
        results.append({'case': 'correct_bounded', 'status': result['status'], 'depth': result['depth']})
        for name, (frames, expected_counts) in scenarios().items():
            file = root / (name + '.json'); replay.write(file, frames)
            result = cli('stimulus', good / 'binding.json', '--inputs', file, '--out', args.out / name)
            if result['status'] != 'trace_no_failure' or result['independent']['status'] != 'sampled_prefix_passed' or result['independent']['accepted_transfers'] != expected_counts:
                raise RuntimeError((name, result))
            results.append({'case': name, 'status': result['status'], 'accepted_transfers': expected_counts})
        for name, (old, new, rule) in MUTANTS.items():
            bad = root / name; shutil.copytree(EXAMPLE, bad)
            source = bad / 'subordinate.veryl'; text = source.read_text()
            if text.count(old) != 1: raise RuntimeError('mutation site drift')
            source.write_text(text.replace(old, new))
            saved = args.out / (name + '.regression.json')
            result = cli('search', bad / 'binding.json', '--out', args.out / name, '--regression', saved)
            if result['status'] != 'reset_reachable_failure' or rule not in {v['rule'] for v in result['independent']['guarantee_violations']}:
                raise RuntimeError((name, result))
            if result['independent']['environment_violations'] or result['independent']['capacity_exceeded']: raise RuntimeError('mutant evidence uses illegal environment or out-of-scope trace')
            again = cli('replay', bad / 'binding.json', '--regression', saved, '--out', args.out / (name + '-replay'))
            if again['status'] != 'reset_reachable_failure': raise RuntimeError('saved failure lost')
            # Same concrete external inputs on the good RTL: independently legal, no protocol violation.
            stimuli = root / (name + '-stimulus.json'); replay.write(stimuli, replay.load_json(saved)['inputs'])
            counterpart = cli('stimulus', good / 'binding.json', '--inputs', stimuli, '--out', args.out / (name + '-good'))
            if counterpart['status'] != 'trace_no_failure' or counterpart['independent']['status'] != 'sampled_prefix_passed': raise RuntimeError('good counterpart failed mutant stimuli')
            source.write_text(source.read_text() + '\n// stale source\n')
            stale = cli('replay', bad / 'binding.json', '--regression', saved, '--out', args.out / (name + '-stale'), allowed=(2,))
            if stale['status'] != 'project_error' or 'identity' not in stale['error']: raise RuntimeError('stale identity accepted')
            results.append({'case': name, 'status': result['status'], 'required_rule': rule})
            print(json.dumps(results[-1]), flush=True)
        result = cli('search', good / 'manager-binding.json', '--out', args.out / 'manager-search')
        if result['status'] != 'bounded_no_failure' or result['capacity_search'] != 'bounded_no_failure': raise RuntimeError(result)
        results.append({'case': 'manager_bounded', 'status': result['status']})
        manager_inputs = replay.load_json(good / 'manager-project.json')['inputs']
        def manager_row(**values):
            flags = {'rst', 'start_write', 'start_read', 'awready', 'wready', 'bvalid', 'arready', 'rvalid'}
            return {**{n: False if n in flags else 0 for n in manager_inputs}, **values}
        frames = [manager_row(rst=True), manager_row(start_write=True, start_read=True),
                  manager_row(awready=True), manager_row(), manager_row(wready=True, arready=True),
                  manager_row(bvalid=True, rvalid=True, rdata=42)]
        file = root / 'manager-legal.json'; replay.write(file, frames)
        result = cli('stimulus', good / 'manager-binding.json', '--inputs', file, '--out', args.out / 'manager-legal-counterpart')
        if result['status'] != 'trace_no_failure' or result['independent']['status'] != 'sampled_prefix_passed' or result['independent']['accepted_transfers'] != {'aw': 1, 'w': 1, 'b': 1, 'ar': 1, 'r': 1}:
            raise RuntimeError(result)
        results.append({'case': 'manager_legal_counterpart', 'status': result['status']})
        for channel in ('aw', 'w', 'ar'):
            name = 'manager_drop_' + channel
            bad = root / name; shutil.copytree(EXAMPLE, bad)
            path = bad / 'manager.veryl'; text = path.read_text()
            old = f'if {channel}valid && {channel}ready {{'; new = f'if {channel}valid {{'
            if text.count(old) != 1: raise RuntimeError('manager mutation drift')
            path.write_text(text.replace(old, new)); saved = args.out / (name + '.regression.json')
            result = cli('search', bad / 'manager-binding.json', '--out', args.out / name, '--regression', saved)
            if result['status'] != 'reset_reachable_failure' or channel + '_valid_stable' not in {v['rule'] for v in result['independent']['guarantee_violations']}:
                raise RuntimeError(result)
            if result['independent']['environment_violations'] or result['independent']['capacity_exceeded']: raise RuntimeError('manager failure needs legal counterpart')
            again = cli('replay', bad / 'manager-binding.json', '--regression', saved, '--out', args.out / (name + '-replay'))
            if again['status'] != 'reset_reachable_failure': raise RuntimeError('manager regression lost')
            stimuli = root / (name + '.json'); replay.write(stimuli, replay.load_json(saved)['inputs'])
            counterpart = cli('stimulus', good / 'manager-binding.json', '--inputs', stimuli, '--out', args.out / (name + '-good'))
            if counterpart['status'] != 'trace_no_failure' or counterpart['independent']['status'] != 'sampled_prefix_passed': raise RuntimeError('correct manager failed')
            results.append({'case': name, 'status': result['status']})
        beyond = root / 'capacity'; shutil.copytree(EXAMPLE, beyond)
        source = beyond / 'subordinate.veryl'
        source.write_text(source.read_text().replace('assign awready = !a_full;', 'assign awready = 1;'))
        unsaved = args.out / 'must-not-save-capacity-as-bug.json'
        capacity = cli('search', beyond / 'binding.json', '--out', args.out / 'capacity-control', '--regression', unsaved)
        if capacity['status'] != 'scope_exceeded' or not capacity['independent']['capacity_exceeded'] or unsaved.exists():
            raise RuntimeError('capacity overflow was hidden or mislabeled a protocol violation')
        results.append({'case': 'capacity_control', 'status': capacity['status']})
        # Role/pin direction and type validation reject swapped/proxy mappings.
        binding = replay.load_json(good / 'binding.json')
        for name, change in [('role', lambda b: b['config'].update(role='manager')),
                             ('alias', lambda b: b['signals'].update(bvalid=b['signals']['awvalid'])),
                             ('width', lambda b: b['config'].update(data_width=64))]:
            wrong = copy.deepcopy(binding); change(wrong); path = good / ('bad-' + name + '.json'); replay.write(path, wrong)
            rejected = cli('search', path, '--out', args.out / ('invalid-' + name), allowed=(2,))
            if rejected['status'] != 'project_error': raise RuntimeError('invalid binding accepted')
    replay.write(args.out / 'summary.json', results)
    print(json.dumps({'status': 'passed', 'cases': len(results)}))

if __name__ == '__main__': main()
