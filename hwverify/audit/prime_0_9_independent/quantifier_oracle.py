"""Finite Python oracle for ordered input binders and execution modes.

The expected Boolean values come from explicit Python quantifier evaluation,
not from Z3 or the Rust formula builder.
"""
import argparse
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
T = {'bv': 2}
W = lambda n: ['bv', 2, n]

def quantify(prefix, predicate, environment=None):
    env = {} if environment is None else environment
    if not prefix: return predicate(env)
    kind, name = prefix[0]
    values = [quantify(prefix[1:], predicate, env | {name: v}) for v in range(4)]
    return all(values) if kind == 'forall' else any(values)

def fixture():
    specs, expected = {}, {}
    relations = {'Echo': ['eq', 'no.y', 'i.x'],
                 'Choice': ['or', ['eq', 'no.y', 'i.x'], ['eq', 'no.y', W(0)]],
                 'Restricted': ['and', ['not', ['eq', 'i.x', W(3)]], ['eq', 'no.y', 'i.x']],
                 'Dead': False}
    def outputs(kind, x):
        if kind == 'Dead' or (kind == 'Restricted' and x == 3): return set()
        return {x, 0} if kind == 'Choice' else {x}
    prefixes = {
        'forall_a_exists_b': [('forall', 'a'), ('exists', 'b')],
        'exists_b_forall_a': [('exists', 'b'), ('forall', 'a')],
        'forall_a_forall_b': [('forall', 'a'), ('forall', 'b')],
        'exists_a_exists_b': [('exists', 'a'), ('exists', 'b')],
        'forall_a': [('forall', 'a')],
        'exists_a': [('exists', 'a')],
    }
    for kind, relation in relations.items():
        examples = {}
        for prefix_name, prefix in prefixes.items():
            has_b = any(name == 'b' for _, name in prefix)
            for mode in ['exists', 'not_exists', 'forall']:
                for predicate in ['equals', 'differs', 'allowed']:
                    rhs = 'q.b' if has_b else 'q.a'
                    equal = ['eq', 'o.y', rhs]
                    ensure = equal if predicate == 'equals' else (['not', equal] if predicate == 'differs' else ['or', ['eq', 'o.y', 'q.a'], ['eq', 'o.y', W(0)]])
                    def accepts(env, y):
                        v = env['b'] if has_b else env['a']
                        return y == v if predicate == 'equals' else (y != v if predicate == 'differs' else y in {env['a'], 0})
                    def valid(env):
                        ys = outputs(kind, env['a']); matches = [accepts(env, y) for y in ys]
                        if mode == 'exists': return any(matches)
                        if mode == 'not_exists': return not any(matches)
                        return bool(ys) and all(matches)
                    name = prefix_name + '_' + mode + '_' + predicate
                    result = {'valid': quantify(prefix, valid), 'feasible': quantify(prefix, lambda env: bool(outputs(kind, env['a'])))}
                    if mode == 'not_exists':
                        result['nonvacuous'] = quantify(prefix, lambda env: bool(outputs(kind, env['a'])) and valid(env))
                    expected[(kind, name)] = result
                    examples[name] = {'expect': mode, 'quantifiers': [{'kind': quantifier, 'variables': {variable: T}} for quantifier, variable in prefix],
                                      'initial': {}, 'trace': [{'operation': 'emit', 'inputs': {'x': 'q.a'}, 'observe': {}, 'ensure': ensure}]}
        specs[kind] = {'inputs': {'x': T}, 'outputs': {'y': T}, 'state': {}, 'init': True, 'invariant': True, 'operations': {'emit': relation}, 'examples': examples}
    # An earlier expected observation must not become an assumption for the
    # later result. Choice can return 1 on the first frame, violating P.
    for mode in ['exists', 'not_exists', 'forall']:
        name = 'earlier_result_remains_assertion_' + mode
        specs['Choice']['examples'][name] = {'expect': mode, 'quantifiers': [], 'initial': {}, 'trace': [
            {'operation': 'emit', 'inputs': {'x': W(1)}, 'observe': {}, 'ensure': ['eq', 'o.y', W(0)]},
            {'operation': 'emit', 'inputs': {'x': W(2)}, 'observe': {}, 'ensure': True}]}
        expected[('Choice', name)] = {'valid': mode == 'exists', 'feasible': True}
        if mode == 'not_exists': expected[('Choice', name)]['nonvacuous'] = False
    return {'version': 4, 'kind': 'specification', 'name': 'Independent ordered quantifier oracle', 'specs': specs, 'compositions': {}}, expected

if __name__ == '__main__':
    p = argparse.ArgumentParser(); p.add_argument('--binary', type=Path, required=True); p.add_argument('--z3', required=True)
    p.add_argument('--out', type=Path, default=ROOT / 'results/prime_0_9_quantifier_oracle'); a = p.parse_args(); a.out.mkdir(parents=True, exist_ok=True)
    doc, expected = fixture(); source = a.out / 'quantifier_oracle.json'; source.write_text(json.dumps(doc, indent=2) + '\n')
    (a.out / 'expected.json').write_text(json.dumps([{'target': t, 'example': n, **v} for (t, n), v in expected.items()], indent=2) + '\n')
    run = subprocess.run([str(a.binary.resolve()), str(source), '--z3', a.z3, '--out', str(a.out / 'run')], capture_output=True, text=True, timeout=300)
    (a.out / 'run.stdout').write_text(run.stdout); (a.out / 'run.stderr').write_text(run.stderr)
    path = a.out / 'run/report.json'; assert path.exists(), (run.returncode, run.stdout, run.stderr)
    report = json.loads(path.read_text())
    actual = {(ex['target'], ex['example']): ex for ex in report['examples']}
    assert set(actual) == set(expected), (len(actual), len(expected))
    for key, value in expected.items():
        found = actual[key]
        assert found['valid'] == value['valid'], (key, 'valid', value, found)
        assert found['status'] == ('passed' if value['valid'] else 'failed'), (key, 'status', found)
        assert found['feasibility']['holds'] == value['feasible'], (key, 'feasibility', value, found)
        if 'nonvacuous' in value:
            assert found['nonvacuous']['holds'] == value['nonvacuous'], (key, 'nonvacuous', value, found)
    result = {'status': 'pass', 'cases': len(expected), 'expected_valid': sum(v['valid'] for v in expected.values()), 'expected_invalid': sum(not v['valid'] for v in expected.values()), 'exit_code': run.returncode, 'report_status': report['status']}
    (ROOT / 'audit/prime_0_9_independent/quantifier_results.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result))
