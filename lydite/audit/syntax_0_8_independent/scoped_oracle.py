"""Independent arithmetic oracle for scoped wiring and instance isolation.

Expected traces are calculated directly, not by reusing the elaborator.
"""
import argparse
import copy
import itertools
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
W = lambda n: ['bv', 2, n]
T = {'bv': 2}

def fixture():
    counter = {'inputs': {'amount': T}, 'outputs': {'count': T}, 'state': {'x': T},
               'init': ['eq', 's.x', W(0)], 'invariant': ['eq', 'o.count', 's.x'],
               'operations': {'add': ['and', ['eq', 'n.x', ['add', 's.x', 'i.amount']], ['eq', 'no.count', 'n.x']]}, 'examples': {}}
    def inst(target, **connections):
        return {'target': target, 'connections': connections}
    def pair(instances, outputs=None):
        return {'inputs': {'a': T, 'b': T}, 'outputs': outputs or {'l': T, 'r': T}, 'instances': instances, 'examples': {}}
    direct = pair({'left': inst('Counter', amount='a', count='l'), 'right': inst('Counter', amount='b', count='r')})
    swapped = pair({'pair': inst('Direct', a='b', b='a', l='l', r='r')})
    shared = pair({'left': inst('Counter', amount='a', count='l'), 'right': inst('Counter', amount='b', count='l')}, {'l': T})
    return {'version': 4, 'kind': 'specification', 'name': 'independent scoped wiring oracle',
            'specs': {'Counter': counter}, 'compositions': {'Direct': direct, 'Swapped': swapped, 'Shared': shared}}

def example(a, b, outputs, admitted):
    return {'expect': 'positive' if admitted else 'negative', 'initial': {},
            'trace': [{'operation': 'add', 'inputs': {'a': W(a), 'b': W(b)}, 'observe': {k: W(v) for k, v in outputs.items()}}]}

def populate(doc):
    counts = {'positive': 0, 'negative': 0}
    for name in ['Direct', 'Swapped']:
        cases = doc['compositions'][name]['examples']
        for a, b, l, r in itertools.product(range(4), repeat=4):
            expected = (a, b) if name == 'Direct' else (b, a)
            admitted = (l, r) == expected
            cases[f'a{a}_b{b}_l{l}_r{r}'] = example(a, b, {'l': l, 'r': r}, admitted)
            counts['positive' if admitted else 'negative'] += 1
        # Partial inputs must be existential, not defaulted to zero.
        cases['existential_input'] = {'expect': 'positive', 'initial': {}, 'trace': [{'operation': 'add', 'inputs': {}, 'observe': {'l': W(3), 'r': W(1)}}]}
        counts['positive'] += 1
        # Two logical operations test next-state and nested port remapping.
        cases['wraparound'] = {'expect': 'positive', 'initial': {}, 'trace': [
            {'operation': 'add', 'inputs': {'a': W(3), 'b': W(1)}, 'observe': {}},
            {'operation': 'add', 'inputs': {'a': W(2), 'b': W(1)}, 'observe': {'l': W(1 if name == 'Direct' else 2), 'r': W(2 if name == 'Direct' else 1)}}]}
        counts['positive'] += 1
    for a, b, l in itertools.product(range(4), repeat=3):
        admitted = a == b == l
        doc['compositions']['Shared']['examples'][f'a{a}_b{b}_l{l}'] = example(a, b, {'l': l}, admitted)
        counts['positive' if admitted else 'negative'] += 1
    return counts

def main():
    p = argparse.ArgumentParser()
    p.add_argument('--binary', type=Path, required=True)
    p.add_argument('--z3', required=True)
    p.add_argument('--out', type=Path, default=ROOT / 'results/syntax_0_8_scoped_oracle')
    a = p.parse_args(); a.out.mkdir(parents=True, exist_ok=True)
    doc = fixture(); counts = populate(doc)
    source = a.out / 'scoped_oracle.json'; source.write_text(json.dumps(doc, indent=2) + '\n')
    run = subprocess.run([str(a.binary.resolve()), str(source), '--z3', a.z3, '--out', str(a.out / 'run')], capture_output=True, text=True, timeout=180)
    (a.out / 'run.stdout').write_text(run.stdout); (a.out / 'run.stderr').write_text(run.stderr)
    assert run.returncode == 0, (run.returncode, run.stdout, run.stderr)
    report = json.loads((a.out / 'run/report.json').read_text())
    expected = {(target, name): ex['expect'] == 'positive' for target, comp in doc['compositions'].items() for name, ex in comp['examples'].items()}
    actual = {(ex['target'], ex['example']): ex for ex in report['examples']}
    assert set(actual) == set(expected), (len(actual), len(expected))
    for key, admitted in expected.items():
        assert actual[key]['admitted'] == admitted and actual[key]['status'] == 'passed', (key, actual[key])
    result = {'status': 'pass', 'expected_counts': counts, 'case_count': sum(counts.values()), 'report_status': report['status']}
    (ROOT / 'audit/syntax_0_8_independent/scoped_results.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result))

if __name__ == '__main__':
    main()
