#!/usr/bin/env python3
"""Read-only solver comparison; Z3 only independently rechecks saved original SMT."""
import argparse, hashlib, json, os, subprocess, time
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
CASES=('pipeline','pipeline_bad_forward','pipeline_bad_priority','pipeline_bad_interlock','pipeline_bad_retire','pipeline_bad_stall')
def sha(p): return hashlib.sha256(p.read_bytes()).hexdigest()
def main():
    ap=argparse.ArgumentParser();ap.add_argument('--baseline',type=Path,required=True);ap.add_argument('--current',type=Path,required=True);ap.add_argument('--z3',type=Path,required=True);ap.add_argument('--out',type=Path,default=ROOT/'results/finite_speed_independent/pipeline');args=ap.parse_args()
    out=args.out.resolve();out.mkdir(parents=True,exist_ok=True)
    bins={k:getattr(args,k).resolve() for k in ('baseline','current')};z3=args.z3.resolve()
    hashes={k:sha(p) for k,p in bins.items()};rows=[];refs=[];search_identity=[]
    source_files=[ROOT.parent/'crates/lydite-solver/src'/n for n in ('finite.rs','z3.rs','quantified.rs')]+[ROOT/'examples'/f'{n}.json' for n in CASES]
    source_hashes={str(p.relative_to(ROOT.parent)):sha(p) for p in source_files}
    (out/'input_hashes.json').write_text(json.dumps({'binaries':hashes,'sources':source_hashes},indent=2)+'\n')
    for label in ('baseline','current'):
      for case in CASES:
        dest=out/label/case;env=dict(os.environ,LYDITE_SOLVER='finite');env.pop('LYDITE_KERNEL',None)
        cmd=[str(bins[label]),str(ROOT/'examples'/f'{case}.json'),'--out',str(dest),'--z3','/finite-speed-audit-external-solver-prohibited']
        start=time.perf_counter();p=subprocess.run(cmd,env=env,capture_output=True,text=True,timeout=90);wall=time.perf_counter()-start
        dest.mkdir(parents=True,exist_ok=True);(dest/'stdout.json').write_text(p.stdout);(dest/'stderr.txt').write_text(p.stderr)
        report=json.loads(p.stdout);expect='stuttering_refinement_verified' if case=='pipeline' else 'counterexample'
        assert report['status']==expect,(label,case,report)
        assert report['engine_summary']['z3_queries']==0
        assert all(q['backend']!='z3' for q in report['obligations'])
        assert len(report['obligations'])==8
        for q in report['obligations']:
          assertion=(dest/q['evidence']).read_text().split('(check-sat)')[0]
          if label=='current':
            old=(out/'baseline'/case/q['evidence']).read_text().split('(check-sat)')[0]
            assert old==assertion,(case,q['name'],'original SMT assertion changed')
            z=subprocess.run([str(z3),'-in','-smt2'],input=assertion+'(check-sat)\n',capture_output=True,text=True,timeout=30,check=True)
            assert z.stdout.strip()==q['solver_result'],(case,q['name'],z.stdout,q['solver_result'])
            refs.append({'case':case,'obligation':q['name'],'finite_result':q['solver_result'],'z3_result':z.stdout.strip(),'assertion_sha256':hashlib.sha256(assertion.encode()).hexdigest()})
        if label=='current':
          for evidence in sorted(dest.glob('*.finite.json')):
            current=json.loads(evidence.read_text());previous=json.loads((out/'baseline'/case/evidence.name).read_text())
            fields=('solver_result','variables','clauses','decisions','conflicts','assignments','context_values','original_formula_validated')
            assert all(current[k]==previous[k] for k in fields),(case,evidence.name,'logical/search trace summary changed')
            search_identity.append({'case':case,'query':evidence.stem.removesuffix('.finite'),'matched_fields':list(fields),'baseline_work':previous['work'],'current_work':current['work']})
        rows.append({'label':label,'case':case,'status':report['status'],'exit_code':p.returncode,'wall_seconds_in_correctness_run':wall,'engine_summary':report['engine_summary'],'obligations':[{k:q[k] for k in ('name','backend','status','solver_result')} for q in report['obligations']]})
        print(label,case,report['status'],flush=True)
    for k,p in bins.items():assert sha(p)==hashes[k],('binary changed',p)
    for p in source_files:assert sha(p)==source_hashes[str(p.relative_to(ROOT.parent))],('source changed',p)
    result={'status':'PASS','runs':rows,'original_smt_z3_rechecks':refs,'original_assertions_match_baseline':len(refs),'binary_hashes':hashes,'same_search_and_witness_summaries':search_identity,'source_hashes_unchanged':True,'timing_note':'Correctness run wall times are diagnostic only; no isolated speed claim.'}
    (out/'summary.json').write_text(json.dumps(result,indent=2)+'\n');print('All',len(refs),'original SMT assertions unchanged and independently rechecked')
if __name__=='__main__':main()
