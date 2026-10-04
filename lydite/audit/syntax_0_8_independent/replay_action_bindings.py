"""Replay actual bridge SAT witnesses independently of Rust elaboration."""
import argparse
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'audit'))
from interpreter import evaluate, evaluate_record
from replay_program_counterexamples import context

def replay(doc, report, out):
    machine = doc['implementation']; rows = []
    for ob in report['implementation_binding']['obligations']:
        if ob['status'] != 'counterexample': continue
        ctx = context(out, ob)
        def values(prefix): return {k[len(prefix):]: v for k, v in ctx.items() if k.startswith(prefix)}
        state, inputs = values('impl.'), values('i.')
        assert not inputs['rst']
        nxt = evaluate_record(machine, 'next', state, inputs)
        assert nxt == values('impl_next.')
        env = {'s.' + k: v for k, v in state.items()} | {'i.' + k: v for k, v in inputs.items()}
        next_env = {'s.' + k: v for k, v in nxt.items()} | {'i.' + k: v for k, v in inputs.items()}
        output = {k: evaluate(v, env) for k, v in machine['binding']['outputs'].items()}
        next_output = {k: evaluate(v, next_env) for k, v in machine['binding']['outputs'].items()}
        private = {k: evaluate(v['x'], env) for k, v in machine['binding']['states'].items()}
        next_private = {k: evaluate(v['x'], next_env) for k, v in machine['binding']['states'].items()}
        assert private['left'] == output['l'] and private['right'] == output['r']
        actions = {k for k, expr in machine['operations'].items() if evaluate(expr, env)}
        local = {'left': set(), 'right': set()}
        for action in actions:
            for ref in doc['compositions']['Direct']['operations'][action]:
                leaf, op = ref.split('.')
                local[leaf].add(op)
        if ob['name'].startswith('binding_leaf_exclusive_'):
            assert any(len(ops) > 1 for ops in local.values())
        elif ob['name'] == 'binding_product_preservation':
            okay = True
            for leaf, port, inp in [('left', 'l', 'a'), ('right', 'r', 'b')]:
                ops = local[leaf]
                okay &= next_private[leaf] == next_output[port]
                if not ops: okay &= next_private[leaf] == private[leaf]
                if 'add' in ops: okay &= next_private[leaf] == evaluate(['add', machine['binding']['states'][leaf]['x'], 'i.' + inp], env)
                if 'clear' in ops: okay &= next_private[leaf].v == 0
                okay &= len(ops) <= 1
            assert not okay, (ob['name'], ctx)
        else: raise AssertionError(ob['name'])
        rows.append({'obligation': ob['name'], 'replayed': True})
    return rows

if __name__ == '__main__':
    p = argparse.ArgumentParser(); p.add_argument('--results', required=True, type=Path); a = p.parse_args()
    rows = []
    for path in sorted(a.results.glob('*/run/report.json')):
        doc = json.loads((path.parent.parent / 'input.json').read_text()); report = json.loads(path.read_text())
        if 'implementation' not in doc: continue
        items = replay(doc, report, path.parent)
        if items: rows.append({'variant': path.parent.parent.name, 'witnesses': items})
    assert len(rows) == 4, rows
    result = {'status': 'pass', 'cases': rows, 'counterexamples_replayed': sum(len(row['witnesses']) for row in rows)}
    (ROOT / 'audit/syntax_0_8_independent/action_replay_results.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result))
