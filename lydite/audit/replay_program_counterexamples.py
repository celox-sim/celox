"""Replay program witnesses with an independent concrete evaluator (no Z3)."""
from pathlib import Path
import itertools
import json
from interpreter import evaluate, evaluate_record, outputs, related
from replay_counterexamples import sexprs, val
ROOT = Path(__file__).resolve().parents[1]

def expand_lets(expr, env=None):
    env = {} if env is None else env
    if isinstance(expr, str): return env.get(expr, expr)
    if expr and expr[0] == "let":
        local = {**env, **{name: expand_lets(value,env) for name,value in expr[1]}}
        return expand_lets(expr[2],local)
    return [expand_lets(x,env) for x in expr]

def context(out, obligation):
    roots = sexprs((out / obligation['solver_output']).read_text())
    assert roots[0] == 'sat'
    values = {symbol: val(expand_lets(value)) for symbol, value in roots[-1]}
    return {name: values[symbol] for name, symbol in obligation['context_symbols'].items()}

def replay(doc, out, obligation):
    ctx = context(out, obligation)
    rec = lambda prefix: {k[len(prefix):]: v for k,v in ctx.items() if k.startswith(prefix)}
    name = obligation['name']
    i = rec('i.')
    if name.startswith('program_'):
        c = doc['program_contract']
        s, ns = rec('s.'), rec('next.')
        p = {k:v for k,v in ctx.items() if k.startswith('p.')}
        assert evaluate_record(doc['spec'], 'next', s, i) == ns
        env = {**{'s.'+k:v for k,v in s.items()}, **p}
        next_env = {**{'s.'+k:v for k,v in ns.items()}, **p}
        ev = lambda key: evaluate(c[key],env)
        inv, terminal = ev('invariant'), ev('terminal')
        active = inv and not terminal
        if name == 'program_initialization':
            assert evaluate(c['precondition'], {**{'i.'+k:v for k,v in i.items()}, **p})
            reset = evaluate_record(doc['spec'],'reset',{},i)
            assert not evaluate(c['invariant'], {**{'s.'+k:v for k,v in reset.items()}, **p})
        elif name.startswith('program_invariant_preserved'):
            assert active and not evaluate(c['invariant'], next_env)
            suffix = name.removeprefix('program_invariant_preserved_')
            if 'cases' in c: assert evaluate(c['cases'][suffix],env)
            if 'split' in c:
                choices = []
                for key,hint in sorted(c['split'].items()):
                    value = evaluate(hint['expr'],env).v
                    choices.append(f'{key}_{value}' if hint['min'] <= value <= hint['max'] else f'{key}_other')
                assert suffix == '_'.join(choices)
        elif name == 'program_rank_decreases':
            assert active and evaluate(c['rank'],next_env).v >= ev('rank').v
        elif name == 'program_postcondition': assert inv and terminal and not ev('postcondition')
        elif name == 'program_step_available': assert active and not outputs(doc['spec'],s,i)[doc['can_step']]
        elif name == 'program_terminal_quiescent': assert inv and terminal and outputs(doc['spec'],s,i)[doc['can_step']]
        elif name == 'program_cases_cover': assert active and not any(evaluate(g,env) for g in c['cases'].values())
        else: raise AssertionError('unhandled obligation '+name)
        return {'obligation':name,'replayed':True,'pc':s.get('pc').v if 'pc' in s else None}
    # Refinement witness on extended CPU: do not use the legacy ISA oracle.
    s,t = rec('spec.'),rec('impl.')
    ns = evaluate_record(doc['spec'],'reset',{},i) if i[doc['reset_input']] else evaluate_record(doc['spec'],'next',s,i) if ctx['commit'] else s
    nt = evaluate_record(doc['impl'],'reset',{},i) if i[doc['reset_input']] else evaluate_record(doc['impl'],'next',t,i)
    assert ns == rec('spec_next.') and nt == rec('impl_next.')
    relation = related(doc,s,t)
    if name == 'microstep_refinement': assert relation and not related(doc,ns,nt)
    elif name == 'commit_eligible': assert relation and not i[doc['reset_input']] and ctx['commit'] and not outputs(doc['spec'],s,i)[doc['can_step']]
    elif name == 'reset_binding': assert not related(doc,evaluate_record(doc['spec'],'reset',{},i),evaluate_record(doc['impl'],'reset',{},i))
    elif name == 'noncommit_rank_decreases': assert relation and not i[doc['reset_input']] and ctx['progress_enabled'] and not ctx['commit'] and not ctx['rank_next'].v < ctx['rank'].v
    else: raise AssertionError('unhandled refinement '+name)
    return {'obligation':name,'replayed':True,'pc':s['pc'].v}

if __name__ == '__main__':
    import argparse
    parser=argparse.ArgumentParser()
    parser.add_argument('--results',type=Path,default=ROOT/'results')
    parser.add_argument('--out',type=Path,default=ROOT/'audit/program_replay_results.json')
    args=parser.parse_args()
    rows=[]
    for reportpath in sorted(args.results.glob('array_sum*/report.json')):
        report=json.loads(reportpath.read_text())
        witnesses=[o for o in report.get('obligations',[]) if o['status']=='counterexample']
        if not witnesses: continue
        docpath=ROOT/'examples'/(reportpath.parent.name+'.json')
        if not docpath.exists(): continue
        doc=json.loads(docpath.read_text())
        rows.append({'case':reportpath.parent.name,'witnesses':[replay(doc,reportpath.parent,o) for o in witnesses]})
    assert rows, 'no counterexamples found'
    args.out.write_text(json.dumps(rows,indent=2)+'\n')
    print({'cases':len(rows),'witnesses':sum(len(r['witnesses']) for r in rows),'all_replayed':True})
