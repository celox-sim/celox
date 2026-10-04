#!/usr/bin/env python3
"""Immutable snapshot plus audit-only adapters for historical API checks.

Historical files are read, copied and retained without alteration in their original
paths. A transparent generated adapter changes only the public solve invocation
so exactly the same formulas and truth assertions run under each declared hint.
"""
import argparse, hashlib, json, os, re, shutil, subprocess
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
AUDIT=Path(__file__).resolve().parent

def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def run(cmd,env,log):
 p=subprocess.run(cmd,env=env,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
 log.write_text(p.stdout); print(p.stdout,flush=True); assert p.returncode==0,(cmd,p.returncode)
def main():
 p=argparse.ArgumentParser();p.add_argument('--source',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args();source=a.source.resolve();out=a.output.resolve();out.mkdir(parents=True,exist_ok=False)
 frozen={str(f.relative_to(source)):sha(f) for f in sorted((source/'crates').rglob('*.rs'))}
 (out/'source-hashes.json').write_text(json.dumps(frozen,indent=2)+'\n')
 env=dict(os.environ,CARGO_TARGET_DIR=str(out/'build-target'),FINITE_AUDIT_TARGETED='1')
 env.pop('HWVERIFY_FINITE_SEARCH_HINT',None);env.pop('HWVERIFY_SOLVER',None)
 campaigns=[('finite_speed_independent','FINITE_AUDIT_OUT','prior-core'),('branch_solver_scaling_independent','BRANCH_AUDIT_OUT','mux-alias-core'),('counterexample_search_independent','COUNTEREXAMPLE_AUDIT_OUT','decomposition-core')]
 for old,output_var,label in campaigns+[('expected_result_search_independent','EXPECTED_AUDIT_OUT','cross-hint-core')]:
  dest=out/label;dest.mkdir();src=dest/'audit-source';shutil.copytree(ROOT/'audit'/old/'src',src)
  for f in src.glob('*.rs'):
   text=f.read_text();text=re.sub(r'#\[path = "[^"]*semantics.rs"\]\n','',text)
   if old!='expected_result_search_independent':
    text=text.replace('finite::solve(', 'audit_solve(')
    if f.name=='main.rs':text+='\nfn audit_solve(t:&hwverify_ir::Term,c:&hwverify_ir::Env,l:hwverify_solver::finite::Limits)->hwverify_solver::finite::Outcome { hwverify_solver::finite::solve_with_hint(t,c,l,match std::env::var("AUDIT_DECLARED_HINT").unwrap().as_str(){"sat"=>hwverify_solver::finite::SearchHint::Sat,"unsat"=>hwverify_solver::finite::SearchHint::Unsat,_=>panic!("bad audit hint")}) }\n'
    if old=='counterexample_search_independent' and f.name=='main.rs':text=text.replace('    scheduling_cases::run(&mut a);','    if std::env::var("AUDIT_DECLARED_HINT").unwrap() == "sat" { scheduling_cases::run(&mut a); }')
   f.write_text(text)
  if not (src/'semantics.rs').exists():shutil.copy(ROOT/'audit/finite_speed_independent/src/semantics.rs',src/'semantics.rs')
  cargo='[package]\nname="expected-audit-'+label+'"\nversion="0.1.0"\nedition="2021"\npublish=false\n[workspace]\n[[bin]]\nname="expected-audit-'+label+'"\npath='+json.dumps(str(src/'main.rs'))+'\n[dependencies]\nhwverify-ir={path='+json.dumps(str(source/'../crates/hwverify-ir'))+'}\nhwverify-solver={path='+json.dumps(str(source/'../crates/hwverify-solver'))+'}\nserde_json="1"\n'
  (dest/'Cargo.toml').write_text(cargo);manifest=str(dest/'Cargo.toml')
  run(['cargo','generate-lockfile','--offline','--manifest-path',manifest],env,dest/'lock.log')
  run(['cargo','build','--offline','--locked','--release','--manifest-path',manifest],env,dest/'build.log')
  for hint in (['both'] if old=='expected_result_search_independent' else ['sat','unsat']):
   runout=dest/hint;runout.mkdir();runenv=dict(env,AUDIT_DECLARED_HINT=hint,COUNTEREXAMPLE_INCLUDE_DECOMPOSITION='1');runenv[output_var]=str(runout)
   run([str(out/'build-target/release'/('expected-audit-'+label))],runenv,runout/'run.log')
   if old in ('branch_solver_scaling_independent','counterexample_search_independent'):
    run(['python3',str(ROOT/'audit/branch_solver_decomposition_independent/recheck_core_smt.py'),'--input',str(runout),'--z3',str(ROOT.parent/'recovered/tools/bin/z3')],env,runout/'oracle.log')
 run(['python3',str(ROOT/'audit/counterexample_search_independent/run_slice_state.py'),'--source',str(source),'--output',str(out/'slice-state')],env,out/'slice-state.log')
 assert frozen=={str(f.relative_to(source)):sha(f) for f in sorted((source/'crates').rglob('*.rs'))}
 (out/'source-hashes-verified.json').write_text(json.dumps({'status':'pass','sha256':frozen},indent=2)+'\n')
 summaries={str(p.relative_to(out)):json.loads(p.read_text()) for p in out.glob('*/*/summary.json') if 'audit-source' not in str(p)}
 (out/'summary-index.json').write_text(json.dumps({p:{k:v for k,v in s.items() if k in ('status','counts','phases','exhaustive_or_pinned_assignment_rows')} for p,s in summaries.items()},indent=2)+'\n')
if __name__=='__main__':main()
