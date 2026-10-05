#!/usr/bin/env python3
"""Change HDL before compilation; the unchanged suite assertions must fail."""
import argparse,hashlib,json,os,pathlib,subprocess,sys
from prepare import ROOT,verify_suite
CASE='operators::test_bitwise_operations'
def main():
 p=argparse.ArgumentParser();p.add_argument('--out',required=True,type=pathlib.Path);args=p.parse_args();args.out.mkdir(parents=True,exist_ok=False)
 summary=[];baseline=verify_suite()
 for name,after in [('wrong_operator','o_or  = a & b;'),('invalid_hdl','THIS_IS_INTENTIONALLY_INVALID_VERYL;')]:
  dest=args.out/name;dest.mkdir();wrapper=dest/'mutated_frontend.py'
  wrapper.write_text('#!'+sys.executable+'\n'+'''import json,pathlib,subprocess,sys
p=pathlib.Path(sys.argv[1]);d=json.loads(p.read_text());before='o_or  = a | b;';found=0
for source in d['sources']:
 if before in source['text']:
  found+=source['text'].count(before);source['text']=source['text'].replace(before,AFTER)
if found!=1:raise RuntimeError('source mutation anchor changed')
mut=p.with_name('mutated-design.json');mut.write_text(json.dumps(d))
raise SystemExit(subprocess.call([EXPORTER,str(mut)]))
'''.replace('AFTER',repr(after)).replace('EXPORTER',repr(str(ROOT/'../../../target/debug/lydite-celox-export'))));wrapper.chmod(0o700)
  with (dest/'run.log').open('w') as log:
   run=subprocess.run([str(ROOT/'../../../target/debug/lydite-celox-suite'),str(dest/'raw'),CASE],env=dict(os.environ,SIR_EXPORTER_BIN=str(wrapper)),stdout=log,stderr=subprocess.STDOUT,timeout=60)
  rows=json.loads((dest/'raw/summary.json').read_text())
  if run.returncode!=1 or len(rows)!=1 or rows[0]['status']!='failed':raise RuntimeError('source mutation did not fail original test: '+name)
  design=dest/'raw'/CASE.replace('::','__')/'design-1';record=json.loads((design/'backend-result.json').read_text())
  if name=='wrong_operator':
   if record['status']!='passed' or not record['reads'] or rows[0]['panic'] is None or rows[0]['errors']:raise RuntimeError('mutation did not reach the original Rust assertion')
  elif not record['compilation_rejected']:raise RuntimeError('invalid HDL was not genuinely rejected')
  summary.append({'control':name,'status':'passed','original_case':CASE,'raw_case_status':rows[0]['status'],'mutation':{'before':'o_or  = a | b;','after':after},'original_source_hashes_unchanged':True})
 if baseline!=verify_suite():raise RuntimeError('original suite changed')
 (args.out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary))
if __name__=='__main__':main()
