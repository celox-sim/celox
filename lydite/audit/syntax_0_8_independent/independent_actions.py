"""Independent expected-value checks for action products and bridge mutants."""
import argparse
import copy
import itertools
import json
from pathlib import Path
import subprocess
from scoped_oracle import fixture, W, T

ROOT = Path(__file__).resolve().parents[2]

def action_fixture():
    doc = fixture()
    doc['specs']['Counter']['operations']['clear'] = ['eq', 'n.x', W(0)]
    for name in ['Direct', 'Shared']:
        doc['compositions'][name]['operations'] = {
            'left': ['left.add'], 'right': ['right.add'],
            'left_clear': ['left.clear'], 'right_clear': ['right.clear'],
            'both': ['left.add', 'right.add']}
    doc['compositions']['Swapped']['operations'] = {'left': ['pair.left'], 'right': ['pair.right']}
    expected = {}
    for target in ['Direct', 'Swapped', 'Shared']:
        cases = doc['compositions'][target]['examples']
        for mask, a, b in itertools.product(range(4), range(4), range(4)):
            actions = [name for bit, name in [(1, 'left'), (2, 'right')] if mask & bit]
            left = (a if target != 'Swapped' else b) if mask & 1 else 0
            right = (b if target != 'Swapped' else a) if mask & 2 else 0
            outputs = [{'l': l} for l in range(4)] if target == 'Shared' else [{'l': l, 'r': r} for l, r in itertools.product(range(4), repeat=2)]
            for output in outputs:
                admitted = (left == right == output['l']) if target == 'Shared' else (left, right) == (output['l'], output['r'])
                name = f'm{mask}_a{a}_b{b}_o' + '_'.join(str(v) for v in output.values())
                cases[name] = {'expect': 'positive' if admitted else 'negative', 'initial': {}, 'trace': [{'actions': actions, 'inputs': {'a': W(a), 'b': W(b)}, 'observe': {k: W(v) for k, v in output.items()}}]}
                expected[(target, name)] = admitted
    extra = {
        'overlapping_groups_apply_once': (True, ['both', 'left'], {'l': W(1), 'r': W(2)}),
        'distinct_ops_conflict': (False, ['left', 'left_clear'], {}),
        'cross_instance_ops_compatible': (True, ['left', 'right_clear'], {'l': W(1), 'r': W(0)}),
    }
    for name, (admitted, actions, observe) in extra.items():
        doc['compositions']['Direct']['examples'][name] = {'expect': 'positive' if admitted else 'negative', 'initial': {}, 'trace': [{'actions': actions, 'inputs': {'a': W(1), 'b': W(2)}, 'observe': observe}]}
        expected[('Direct', name)] = admitted
    return doc, expected

def implementation():
    return {'composition': 'Direct', 'inputs': {'a': T, 'b': T, 'go_a': 'bool', 'go_b': 'bool', 'rst': 'bool'}, 'reset_input': 'rst',
            'state': {'l': T, 'r': T}, 'reset': {'l': W(0), 'r': W(0)},
            'next': {'l': ['ite', 'i.go_a', ['add', 's.l', 'i.a'], 's.l'], 'r': ['ite', 'i.go_b', ['add', 's.r', 'i.b'], 's.r']},
            'operations': {'left': 'i.go_a', 'right': 'i.go_b', 'left_clear': False, 'right_clear': False, 'both': False},
            'binding': {'states': {'left': {'x': 's.l'}, 'right': {'x': 's.r'}}, 'outputs': {'l': 's.l', 'r': 's.r'}}}

def invoke(binary, z3, doc, directory):
    directory.mkdir(parents=True, exist_ok=True)
    source = directory / 'input.json'; source.write_text(json.dumps(doc, indent=2) + '\n')
    run = subprocess.run([str(binary.resolve()), str(source), '--z3', z3, '--out', str(directory / 'run')], capture_output=True, text=True, timeout=300)
    (directory / 'run.stdout').write_text(run.stdout); (directory / 'run.stderr').write_text(run.stderr)
    path = directory / 'run/report.json'
    assert path.exists(), (run.returncode, run.stdout, run.stderr)
    return run.returncode, json.loads(path.read_text())

def main():
    p = argparse.ArgumentParser(); p.add_argument('--binary', required=True, type=Path); p.add_argument('--z3', required=True)
    p.add_argument('--out', type=Path, default=ROOT / 'results/syntax_0_8_action_oracle'); a = p.parse_args()
    doc, expected = action_fixture()
    code, report = invoke(a.binary, a.z3, doc, a.out / 'traces')
    assert code == 0, report
    actual = {(ex['target'], ex['example']): ex for ex in report['examples']}
    assert set(actual) == set(expected), (len(actual), len(expected))
    for key, admitted in expected.items():
        assert actual[key]['admitted'] == admitted and actual[key]['status'] == 'passed', (key, actual[key])
    for target in list(doc['specs'].values()) + list(doc['compositions'].values()): target['examples'] = {}
    variants = {'good': implementation()}
    for name in ['inactive_state_changes', 'swapped_private_binding', 'missed_simultaneous_update', 'conflicting_selectors']:
        variants[name] = copy.deepcopy(implementation())
    variants['inactive_state_changes']['next']['l'] = ['add', 's.l', 'i.a']
    variants['swapped_private_binding']['binding']['states'] = {'left': {'x': 's.r'}, 'right': {'x': 's.l'}}
    variants['missed_simultaneous_update']['next']['r'] = ['ite', ['and', 'i.go_a', 'i.go_b'], 's.r', variants['good']['next']['r']]
    variants['conflicting_selectors']['operations']['left_clear'] = 'i.go_a'
    rows = []
    for name, impl in variants.items():
        doc['implementation'] = impl
        code, report = invoke(a.binary, a.z3, doc, a.out / name)
        assert (code == 0) == (name == 'good'), (name, code, report)
        rows.append({'variant': name, 'exit_code': code, 'status': report['status']})
    result = {'status': 'pass', 'trace_count': len(expected), 'positive': sum(expected.values()), 'negative': len(expected) - sum(expected.values()), 'bridge_variants': rows}
    (ROOT / 'audit/syntax_0_8_independent/action_results.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result), flush=True)

if __name__ == '__main__': main()
