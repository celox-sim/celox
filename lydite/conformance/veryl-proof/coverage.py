#!/usr/bin/env python3
"""Fail-closed whole-suite gate. Golden contains identity/counts, never expected values."""
import argparse, collections, gzip, hashlib, json, pathlib, subprocess, sys
from prepare import ROOT, verify_suite
SUITE_BIN = ROOT/'../../../target/debug/lydite-celox-suite'
# Reviewed cases that fail for a recorded reason outside the proof engine:
# Veryl language restrictions, suite features the proof backend lacks, and
# known Celox frontend failures that Celox's own tests also ignore, and
# constructs the Celox frontend reports as typed Unsupported errors.
EXCEPTION_KINDS = {'veryl_language_restriction', 'unsupported_by_proof_backend', 'known_celox_failure', 'celox_unsupported'}
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
def check_exceptions(exceptions,catalog):
 require(isinstance(exceptions,dict),'exceptions must be a mapping')
 for name,entry in exceptions.items():
  require(name in catalog,name+': exception for an unknown case')
  require(set(entry)=={'kind','reason','panic'} and entry['kind'] in EXCEPTION_KINDS,name+': malformed exception')
  require(isinstance(entry['reason'],str) and entry['reason'] and isinstance(entry['panic'],str) and entry['panic'],name+': exception needs a reason and the exact failure')
def check_queries(name,design,record):
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
 control=None
 if record['reads']:
  control=record['negative_control'];require(control and control['status']=='passed',name+': missing poisoned observation control')
  q=queries[control['query']];require(q['solver_result']=='sat' and q['original_formula_validated'] is True and q['encoded_extra'] is not True,name+': poisoned observation lacks counterexample')
 return len(queries),control is not None
def source_identity(meta):
 """Keep diagnostic positions in the catalogue, never in the reviewed contract."""
 script=meta['script_identity']
 require(isinstance(script,str) and script,meta['case']+': missing parsed script identity')
 return {'file':meta['source']['file'],'script_sha256':hashlib.sha256(script.encode()).hexdigest()}
def collect(raw,listed,exceptions,raw_exit):
 catalog=indexed(listed,'catalog'); rows=indexed(read(raw/'summary.json'),'raw results')
 require(set(rows)==set(catalog),'missing/extra cases; every suite case must execute')
 check_exceptions(exceptions,catalog)
 require(raw_exit==(1 if exceptions else 0),'raw engine exit code must report exactly the recorded exceptions')
 result=[];totals=collections.Counter();queries_total=0
 for name in sorted(catalog):
  row=rows[name];meta=catalog[name];exception=exceptions.get(name)
  require(row['expectation']==meta['expectation'],name+': expectation changed')
  require(row['errors']==[],name+': sticky backend/transport errors')
  if exception:
   require(row['status']=='failed' and row['panic']==exception['panic'],name+': recorded exception no longer reproduces exactly; review case-exceptions.json')
  else:
   require(row['status']=='passed' and row['panic'] is None,name+': unexpected failure: '+str(row['panic'] or row['status']))
  root=raw/name.replace('::','__');dirs=sorted(root.glob('design-*'))
  require(len(dirs)==row['designs'],name+': missing/extra designs')
  require(dirs or exception,name+': no design was compiled')
  designs=[];all_reads=0;unsupported_designs=0
  for design in dirs:
   record=read(design/'backend-result.json');rejected=record['compilation_rejected']
   diag=diagnostic_identity(record['diagnostic'])
   if record.get('frontend_unsupported',False):
    # A typed Celox Unsupported is never a source rejection or a pass.
    require(exception is not None and exception['kind']=='celox_unsupported',name+': frontend Unsupported needs a reviewed celox_unsupported exception')
    require(not rejected and record['status']=='failed' and record['reads']==0 and diag is not None,name+': malformed frontend Unsupported record')
    require(not (design/'proof').exists(),name+': unsupported design unexpectedly reached proof execution')
    unsupported_designs+=1
   if exception is None:
    rejection_expected=meta['expectation']=='CompilationError'
    require(record['status']==('failed' if rejection_expected else 'passed'),name+': backend close verdict')
    require(rejected==rejection_expected,name+': expected genuine typed compilation rejection')
    require((record['error'] is not None)==rejection_expected,name+': backend error state')
   if rejected:
    require(diag is not None and record['reads']==0,name+': rejection lacks diagnostics')
    require(not (design/'proof').exists(),name+': rejected design unexpectedly reached proof execution')
   elif record['status']=='passed':
    require(diag is None,name+': successful design has compiler diagnostic')
    count,controlled=check_queries(name,design,record);queries_total+=count;totals['negative_controls']+=controlled
   all_reads+=record['reads']
   designs.append({key:record[key] for key in ('design_sha256','protocol_sha256','reads','commands','operations')}|{'diagnostic':diag})
  if exception and exception['kind']=='celox_unsupported':
   require(unsupported_designs>0,name+': celox_unsupported exception without a frontend Unsupported design')
  disposition=exception['kind'] if exception else 'expected_compilation_rejection' if meta['expectation']=='CompilationError' else 'observation_verified' if all_reads else 'smoke_only'
  totals[disposition]+=1;totals['reads']+=all_reads
  source=source_identity(meta)
  result.append({'case':name,'expectation':meta['expectation'],'category':meta['category'],'source':source,'disposition':disposition,'designs':designs})
 passes=len(catalog)-len(exceptions)
 return {'schema':3,'cases':result},dict(totals)|{'total':len(catalog),'actual_passes':passes,'raw_failures':len(exceptions),'queries':queries_total}
def compare(actual,golden):
 require(actual==golden,'golden coverage/identity mismatch: case sources, design/protocol hashes, dispositions and operation/read counts must match; do not auto-bless')
def main():
 p=argparse.ArgumentParser();p.add_argument('--out',type=pathlib.Path,required=True);p.add_argument('--raw',type=pathlib.Path);p.add_argument('--candidate-only',action='store_true');p.add_argument('--raw-exit',type=int);args=p.parse_args()
 args.out.mkdir(parents=True,exist_ok=True)
 try:
  verify_suite()
  listed=json.loads(subprocess.check_output([str(SUITE_BIN),'--list'],text=True))
  (args.out/'catalog.json').write_text(json.dumps(listed,indent=2)+'\n')
  raw=args.raw or args.out/'raw';code=args.raw_exit
  if args.raw is None:
   with (args.out/'suite.log').open('w') as log:
    run=subprocess.run([str(SUITE_BIN),str(raw)],stdout=log,stderr=subprocess.STDOUT,timeout=3600)
   code=run.returncode
  exceptions=read(ROOT/'case-exceptions.json')
  candidate,counts=collect(raw,listed,exceptions,code)
  (args.out/'coverage-candidate.json').write_text(json.dumps(candidate,indent=2)+'\n')
  if not args.candidate_only:compare(candidate,read(ROOT/'coverage-manifest.json'))
  summary={'status':'candidate_only' if args.candidate_only else 'coverage_contract_passed',**counts,'exceptions':{k:v['kind'] for k,v in sorted(exceptions.items())},'raw_exit_code':code}
  (args.out/'coverage-summary.json').write_text(json.dumps(summary,indent=2)+'\n')
  kinds=collections.Counter(v['kind'] for v in exceptions.values())
  (args.out/'summary.md').write_text(f"## Veryl proof-backed conformance: {counts['actual_passes']}/{counts['total']} actual passes\n\n{counts['total']-counts.get('expected_compilation_rejection',0)} positive cases + {counts.get('expected_compilation_rejection',0)} genuine expected compiler rejections. Recorded exceptions: "+', '.join(f'{n} {k}' for k,n in sorted(kinds.items()))+f".\n\nObserved-read cases: {counts.get('observation_verified',0)}; smoke-only cases: {counts.get('smoke_only',0)}; proved reads: {counts['reads']}; finite queries: {counts['queries']}; poisoned observation controls: {counts.get('negative_controls',0)}.\n\nCoverage contract: {summary['status']}. No external solver is used. Query audits are replayable evidence, not independently checked UNSAT certificates.\n")
  print(json.dumps(summary))
 except Exception as error:
  (args.out/'gate-error.json').write_text(json.dumps({'status':'failed','error':str(error)},indent=2)+'\n');raise
if __name__=='__main__':main()
