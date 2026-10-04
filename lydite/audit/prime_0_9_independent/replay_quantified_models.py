"""Replay concrete diagnostics for the independent finite quantifier fixture."""
import argparse
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'audit'))
from interpreter import BV, evaluate
from replay_program_counterexamples import context

def allowed(kind, x):
    if kind == 'Dead' or (kind == 'Restricted' and x == 3): return set()
    return {x, 0} if kind == 'Choice' else {x}

if __name__ == '__main__':
    p = argparse.ArgumentParser(); p.add_argument('--results', type=Path, required=True); a = p.parse_args()
    doc = json.loads((a.results / 'quantifier_oracle.json').read_text())
    report = json.loads((a.results / 'run/report.json').read_text()); rows = []
    for ex in report['examples']:
        witness = ex.get('concrete_witness')
        if not witness: continue
        evidence = witness['evidence']; assert evidence['concrete_model'] and evidence['solver_result'] == 'sat'
        ctx = context(a.results / 'run', evidence)
        source = doc['specs'][ex['target']]['examples'][ex['example']]
        q = {name: value for name, value in ctx.items() if name.startswith('q.')}
        choices, tests = [], []
        for step in source['trace']:
            x = evaluate(step['inputs']['x'], q).v
            ys = allowed(ex['target'], x); choices.append(ys)
            tests.append({y for y in ys if evaluate(step['ensure'], q | {'o.y': BV(2, y)})})
        feasible = all(choices)
        good_exists = all(tests)
        every_good = feasible and all(good == ys for good, ys in zip(tests, choices))
        mode = source['expect']
        claim = good_exists if mode == 'exists' else (not good_exists if mode == 'not_exists' else every_good)
        purpose = witness['purpose']
        if purpose == 'satisfying_input_choice': assert claim
        elif purpose == 'input_without_feasible_execution': assert not feasible
        elif purpose == 'input_without_satisfying_execution': assert not good_exists
        elif purpose in ['satisfying_execution', 'excluded_execution_counterexample', 'postcondition_counterexample']:
            actual_good = True
            for index, step in enumerate(source['trace']):
                x = evaluate(step['inputs']['x'], q)
                assert ctx[f'step{index}.i.x'] == x
                y = ctx[f'frame{index + 1}.o.y'].v
                assert y in choices[index]
                actual_good &= y in tests[index]
            assert actual_good == (purpose != 'postcondition_counterexample')
        else: raise AssertionError(purpose)
        rows.append({'target': ex['target'], 'example': ex['example'], 'purpose': purpose, 'replayed': True})
    assert rows
    result = {'status': 'pass', 'models_replayed': len(rows), 'models': rows}
    (ROOT / 'audit/prime_0_9_independent/model_replay_results.json').write_text(json.dumps(result, indent=2) + '\n')
    print(len(rows), 'quantified-query diagnostic models replayed')
