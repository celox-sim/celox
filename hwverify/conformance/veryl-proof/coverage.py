#!/usr/bin/env python3
"""Fail-closed whole-corpus gate. Golden contains identity/counts, never expected values."""
import argparse, collections, gzip, hashlib, json, pathlib, subprocess, sys
from prepare import ROOT, verify_suite
BLOCKED = {
 'flip_flop::test_ff_constant_signed_bounds_in_unrolled_loops',
 'synth_dynamic_loop::test_constant_signed_bounds_in_unrolled_synth_loops',
}
class GateError(ValueError): pass
def require(test,message):
 if not test: raise GateError(message)
def read(path):
 with (gzip.open(path,'rt') if path.suffix=='.gz' else path.open()) as file: return json.load(file)
def diagnostic_identity(diagnostic):
 if diagnostic is None:return None
 if diagnostic.get('stage')=='analyzer':
  return {'stage':'analyzer','diagnostics':diagnostic['diagnostics']}
 return {'stage':diagnostic['stage'],'detail_sha256':hashlib.sha256(diagnostic['detail'].encode()).hexdigest()}
def indexed(rows,label):
 require(isinstance(rows,list),label+' must be an array')
 names=[r['case'] for r in rows];require(len(names)==len(set(names)),label+' duplicate cases')
 return {r['case']:r for r in rows}
def collect(raw,listed,sources,raw_exit):
 catalog=indexed(listed,'catalog'); rows=indexed(read(raw/'summary.json'),'raw results'); origin=indexed(sources['cases'],'source catalog')
 require(len(catalog)==665 and set(rows)==set(catalog)==set(origin),'missing/extra cases; exactly all665 must execute')
 require(raw_exit==1,'raw engine must report two positive fixtures failed (exit1)')
 result=[];totals=collections.Counter();queries_total=0
 for name in sorted(catalog):
  row=rows[name];meta=catalog[name];blocked=name in BLOCKED
  require(row['expectation']==meta['expectation'],name+': expectation changed')
  require(row['status']==('failed' if blocked else 'passed'),name+': unexpected raw verdict')
  require(row['errors']==[],name+': sticky backend/transport errors')
  require((row['panic'] is not None)==blocked,name+': unexpected assertion/panic')
  root=raw/name.replace('::','__');dirs=sorted(root.glob('design-*'))
  require(len(dirs)==row['designs'] and len(dirs)>0,name+': missing/extra designs')
  designs=[];all_reads=0
  for design in dirs:
   record=read(design/'backend-result.json');rejected=record['compilation_rejected']
   rejection_expected=meta['expectation']=='CompilationError' or blocked
   require(record['status']==('failed' if rejection_expected else 'passed'),name+': backend close verdict')
   require(rejected==rejection_expected,name+': expected genuine typed compilation rejection')
   require((record['error'] is not None)==rejection_expected,name+': backend error state')
   diag=diagnostic_identity(record['diagnostic'])
   if blocked:
    require(meta['expectation']=='Simulation',name+': positive fixture misclassified')
    require(diag and diag['stage']=='analyzer' and diag['diagnostics'] and all(d=={'code':'InvalidForRange','kind':'NegativeBound'} for d in diag['diagnostics']),name+': blocked diagnostic must be exactly InvalidForRange::NegativeBound')
    require(record['reads']==0 and record['operations']==0,name+': blocked fixture partially ran')
   if rejection_expected:
    require(diag is not None and record['reads']==0,name+': rejection lacks diagnostics')
    require(not (design/'proof').exists(),name+': rejected design unexpectedly reached proof execution')
   else:
    require(diag is None,name+': successful design has compiler diagnostic')
    queries=read(design/'proof/proof-audit.json.gz');require(queries,name+': missing query audit')
    for index,q in enumerate(queries):
     require(q['query']==index,name+': query sequence corrupt')
     require(q['solver_result'] in ('sat','unsat'),name+': UNKNOWN or invalid query verdict')
     require(q.get('kind')=='bounded bit-blast/CDCL diagnostics; not an independently checkable proof certificate',name+': external/fallback solver result')
     if q['solver_result']=='sat':require(q['original_formula_validated'] is True,name+': unvalidated SAT model')
    sample_count=sum(q.get('purpose','').startswith('sample') and q['solver_result']=='unsat' for q in queries)
    design_input=read(design/'design.json')
    require(sample_count==record['reads']*(2 if design_input['four_state'] else 1),name+': observed payload/mask reads lack uniqueness proofs')
    require(queries[-1]['solver_result']=='sat' and queries[-1]['encoded_extra'] is True and queries[-1]['context']=={},name+': missing final prefix feasibility query')
    if record['reads']:
     control=record['negative_control'];require(control and control['status']=='passed',name+': missing poisoned observation control')
     q=queries[control['query']];require(q['solver_result']=='sat' and q['original_formula_validated'] is True and q['encoded_extra'] is not True,name+': poisoned observation lacks counterexample')
     totals['negative_controls']+=1
    queries_total+=len(queries)
   all_reads+=record['reads']
   designs.append({key:record[key] for key in ('design_sha256','protocol_sha256','reads','commands','operations')}|{'diagnostic':diag})
  disposition='blocked_language_restriction' if blocked else 'expected_compilation_rejection' if meta['expectation']=='CompilationError' else 'observation_verified' if all_reads else 'smoke_only'
  totals[disposition]+=1;totals['reads']+=all_reads
  result.append({'case':name,'expectation':meta['expectation'],'category':meta['category'],'source':origin[name]['source'],'disposition':disposition,'designs':designs})
 require(totals['blocked_language_restriction']==2 and totals['expected_compilation_rejection']==8 and totals['smoke_only']==3,'case disposition totals changed')
 return {'schema':1,'upstream_revision':sources['revision'],'cases':result},dict(totals)|{'total':665,'actual_passes':663,'raw_failures':2,'queries':queries_total}
def compare(actual,golden):
 require(actual==golden,'golden coverage/identity mismatch: original sources, design/protocol hashes, dispositions and operation/read counts must match; do not auto-bless')
def main():
 p=argparse.ArgumentParser();p.add_argument('--out',type=pathlib.Path,required=True);p.add_argument('--raw',type=pathlib.Path);p.add_argument('--candidate-only',action='store_true');p.add_argument('--raw-exit',type=int);args=p.parse_args()
 args.out.mkdir(parents=True,exist_ok=True)
 try:
  verify_suite(ROOT/'work/celox/crates/celox-test-suite-veryl')
  listed=json.loads(subprocess.check_output([str(ROOT/'target/debug/veryl-proof-suite'),'--list'],text=True))
  (args.out/'catalog.json').write_text(json.dumps(listed,indent=2)+'\n')
  raw=args.raw or args.out/'raw';code=args.raw_exit
  if args.raw is None:
   with (args.out/'suite.log').open('w') as log:
    run=subprocess.run([str(ROOT/'target/debug/veryl-proof-suite'),str(raw)],stdout=log,stderr=subprocess.STDOUT,timeout=1800)
   code=run.returncode
  candidate,counts=collect(raw,listed,read(ROOT/'upstream-sources.json'),code)
  (args.out/'coverage-candidate.json').write_text(json.dumps(candidate,indent=2)+'\n')
  if not args.candidate_only:compare(candidate,read(ROOT/'coverage-manifest.json'))
  summary={'status':'candidate_only' if args.candidate_only else 'coverage_contract_passed',**counts,'blocked_cases':sorted(BLOCKED),'raw_exit_code':code}
  (args.out/'coverage-summary.json').write_text(json.dumps(summary,indent=2)+'\n')
  (args.out/'summary.md').write_text(f"## Veryl proof-backed conformance: 663/665 actual passes\n\n655 executable designs + 8 genuine expected compiler rejections. Two positive fixtures remain blocked by the pinned language's negative constant range restriction and remain failed in the raw engine.\n\nObserved-read cases: {counts['observation_verified']}; smoke-only cases: 3; proved reads: {counts['reads']}; finite queries: {counts['queries']}; poisoned observation controls: {counts['negative_controls']}.\n\nCoverage contract: {summary['status']}. No external solver is used. Query audits are replayable evidence, not independently checked UNSAT certificates.\n")
  print(json.dumps(summary))
 except Exception as error:
  (args.out/'gate-error.json').write_text(json.dumps({'status':'failed','error':str(error)},indent=2)+'\n');raise
if __name__=='__main__':main()
