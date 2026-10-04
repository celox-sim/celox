#!/usr/bin/env python3
"""Caller routing and malformed option checks against one frozen binary."""
import argparse,hashlib,json,os,subprocess
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def evidence(obj):
 if isinstance(obj,dict):
  if 'solver_result' in obj and 'backend' in obj and 'evidence' in obj:yield obj
  else:
   for v in obj.values():yield from evidence(v)
 elif isinstance(obj,list):
  for v in obj:yield from evidence(v)
def main():
 p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args();out=a.output.resolve();out.mkdir(parents=True,exist_ok=False);binary=a.binary.resolve();bh=sha(binary)
 tripwire=out/'z3_tripwire.py';marker=out/'Z3_WAS_CALLED';tripwire.write_text('#!/usr/bin/env python3\nfrom pathlib import Path\nPath('+repr(str(marker))+').write_text("external solver called")\nraise SystemExit(97)\n');tripwire.chmod(0o755)
 rows=[];smts={};query_results={};sources={}
 def run(label,file,args=(),setting=None,good=True):
  sources[str(file)]=sha(file);dest=out/label;env=dict(os.environ,LYDITE_SOLVER='finite');env.pop('LYDITE_FINITE_SEARCH_HINT',None);env.pop('LYDITE_KERNEL',None)
  if setting is not None:env['LYDITE_FINITE_SEARCH_HINT']=setting
  cmd=[str(binary),str(file),'--out',str(dest),'--z3',str(tripwire),*args]
  proc=subprocess.run(cmd,env=env,text=True,capture_output=True,timeout=180);report=json.loads(proc.stdout);(out/(label+'.stdout.json')).write_text(proc.stdout);(out/(label+'.stderr.log')).write_text(proc.stderr)
  assert not marker.exists()
  if not good:
   assert proc.returncode==2 and report['status']=='invalid_or_tool_error',(label,report)
   assert not list(dest.glob('*.smt2')),'invalid hint executed queries'
  else:
   assert report['status']!='invalid_or_tool_error',(label,report)
   eqs=list(evidence(report));assert eqs,(label,report)
   override=args[-1] if args else setting;override=override if override in ('sat','unsat') else None
   for q in eqs:
    assert q['backend'] in ('finite_bv','structural_kernel'),q
    logical=q['logical_expectation'];assert logical in ('sat','unsat');declared=override or logical
    assert q.get('finite_search_hint',q.get('finite',{}).get('search_hint'))==declared,(label,q)
    if q['backend']=='finite_bv' and 'probe_work' in q.get('finite',{}):
     f=q['finite'];assert f['search_hint']==declared
     if declared=='unsat':assert f['probe_work']==0 and f['probe_result'] is None
     if q['solver_result']=='sat':assert f['original_formula_validated'] and q['concrete_model']
     if q['solver_result']=='unknown':assert not f['original_formula_validated']
    original=(dest/q['evidence']).read_text().split('(check-sat)')[0];key=(str(file),q['evidence'])
    if key in smts:assert smts[key]==original,('hint changed original assertion',key)
    else:smts[key]=original
    if key in query_results:assert query_results[key]==q['solver_result'],('hint changed decided result',key)
    else:query_results[key]=q['solver_result']
   rows.append({'case':label,'file':str(file.relative_to(ROOT)),'setting':setting,'cli':list(args),'status':report['status'],'queries':[{'name':q['name'],'logical_expectation':q['logical_expectation'],'declared_hint':q.get('finite_search_hint',q.get('finite',{}).get('search_hint')),'result':q['solver_result'],'source':q['search_hint_source']} for q in eqs]})
   if file.name=='expectation_counter.lyd':
    for e in report['examples']:assert e['evidence']['logical_expectation']==('sat' if e['expect']=='positive' else 'unsat')
   if file.name=='quantified_counter.json':
    assert report['status']=='unknown';assert all(q['solver_result']=='unknown' for q in eqs)
  return report
 for file in [ROOT/'examples/pipeline.json',ROOT/'examples/pipeline_bad_forward.json',ROOT/'examples/expectation_counter.lyd',ROOT/'examples/quantified_counter.json']:
  for label,args,setting in [('default',(),None),('query',('--finite-search-hint','query'),None),('sat',('--finite-search-hint','sat'),None),('unsat',('--finite-search-hint','unsat'),None),('cli_over_env',('--finite-search-hint','unsat'),'sat'),('query_over_env',('--finite-search-hint','query'),'unsat'),('env_sat',(),'sat'),('env_unsat',(),'unsat'),('cli_over_bad_env',('--finite-search-hint','sat'),'bad')]:
   run(file.stem+'_'+label,file,args,setting)
 invalid=[]
 for label,args,setting in [('missing',('--finite-search-hint',),None),('empty',('--finite-search-hint',''),None),('typo',('--finite-search-hint','satt'),None),('wrong_case',('--finite-search-hint','SAT'),None),('unknown_value',('--finite-search-hint','unknown'),None),('duplicate',('--finite-search-hint','sat','--finite-search-hint','unsat'),None),('env_bad',(),'bad'),('env_empty',(),''),('env_case',(),'Unsat')]:
  r=run('invalid_'+label,ROOT/'examples/pipeline.json',args,setting,False);invalid.append({'case':label,'error':r['error']})
 assert sha(binary)==bh;assert all(sha(Path(p))==h for p,h in sources.items())
 (out/'summary.json').write_text(json.dumps({'status':'pass','binary_sha256':bh,'valid_calls':len(rows),'malformed_rejected':len(invalid),'unique_original_queries_unchanged':len(smts),'z3_tripwire_invocations':0,'rows':rows,'malformed':invalid,'fixtures_sha256':sources},indent=2)+'\n')
 print(json.dumps({'status':'pass','valid_calls':len(rows),'malformed_rejected':len(invalid),'unique_original_queries':len(smts)}))
if __name__=='__main__':main()
