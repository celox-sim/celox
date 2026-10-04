"""Fail-closed fresh 18-row bank/mux control gate; every positive must verify."""
import argparse
import json
from pathlib import Path
from audit.equality_sharing.ci import digest, read_json, require, validate_outcome
from audit.word_frontier.generate import FAMILIES, alpha_rename, cases
from audit.word_frontier.run import run

GROUPS=((32,None),(24,None),(24,18073))
MANIFEST=Path(__file__).with_name('input-sha256.json')


def matrix():
    rows=[]
    for width,seed in GROUPS:
        for doc in cases(width):
            if seed is not None: doc=alpha_rename(doc,seed)
            rows.append({'key':f'{width}:{seed}:{doc["name"]}', 'input_sha256':digest((json.dumps(doc,indent=2)+'\n').encode())})
    require(len(rows)==18 and len({r['key'] for r in rows})==18,'invalid matrix')
    require({r['key']:r['input_sha256'] for r in rows}==read_json(MANIFEST),'frozen input matrix changed')
    return rows


def validate_results(summaries):
    expected=matrix();actual=[];positive=negative=0
    require(len(summaries)==3,'missing run group')
    for (width,seed),s in zip(GROUPS,summaries):
        require(s.get('width')==width and s.get('alpha_seed')==seed,'run group mismatch')
        require(s.get('status')=='passed' and s.get('forbidden_solver_invocations')=='','run failed or solver invoked')
        require(len(s.get('rows',[]))==6,'missing case')
        for r in s['rows']:
            require(not r.get('errors'),'case audit failed')
            actual.append({'key':f'{width}:{seed}:{r["name"]}', 'input_sha256':r['input_sha256']})
            if r['role']=='negative':
                require(r.get('disposition')=='validated_original_counterexample','negative lacks original SAT')
                negative+=1
            else:
                require(r.get('status')=='stuttering_refinement_verified' and r.get('disposition')=='limitation_improved_verified','control positive is not verified')
                positive+=1
    require(actual==expected,'missing, duplicate, reordered or changed matrix row')
    require(positive==6 and negative==12,'incomplete outcomes')
    require(len({s['checker_sha256'] for s in summaries})==1,'mixed checker hashes')
    return {'positive_controls_verified':positive,'original_formula_sat_mutants':negative}


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--checker',type=Path,required=True);p.add_argument('--out',type=Path,required=True)
    p.add_argument('--expected-checker-sha256',required=True)
    a=p.parse_args();matrix();a.out.mkdir(parents=True,exist_ok=False)
    results=[]
    for width,seed in GROUPS:
        results.append(run(a.checker,a.out.resolve()/f'width{width}-alpha{seed}',width,seed,expected_sha256=a.expected_checker_sha256))
    summary={'status':'passed','checker_sha256':a.expected_checker_sha256,**validate_results(results),
             'runs':[str((a.out/f'width{w}-alpha{s}'/'summary.json').resolve()) for w,s in GROUPS],
             'input_manifest_sha256':digest(MANIFEST.read_bytes())}
    (a.out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
    print(json.dumps(summary,indent=2))

if __name__=='__main__':main()
