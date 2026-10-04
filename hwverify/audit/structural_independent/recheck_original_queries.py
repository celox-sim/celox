"""Replay original obligations custom-closed by the kernel through Z3.
This is differential validation, not a certificate for the Rust implementation.
"""
from pathlib import Path
import argparse,concurrent.futures,hashlib,json,re,subprocess,time
p=argparse.ArgumentParser();p.add_argument('roots',nargs='+',type=Path);p.add_argument('--z3',required=True);p.add_argument('--out',required=True,type=Path);a=p.parse_args()
a.out.mkdir(parents=True,exist_ok=True);grouped={};count=0
for root in a.roots:
    for report_path in sorted(root.rglob('report.json')):
        report=json.loads(report_path.read_text())
        for ob in report.get('obligations',[]):
            if ob.get('backend')!='structural_kernel':continue
            count+=1;path=report_path.parent/ob['evidence'];script=path.read_text();digest=hashlib.sha256(script.encode()).hexdigest()
            item=grouped.setdefault(digest,{'sha256':digest,'script':script,'occurrences':[]})
            item['occurrences'].append(str(path))
def run(item):
    start=time.monotonic();script=re.sub(r'\(set-option :timeout \d+\)','(set-option :timeout 10000)',item['script'])
    completed=subprocess.run([a.z3,'-in','-smt2'],input=script,capture_output=True,text=True,timeout=20)
    verdict=completed.stdout.strip();(a.out/(item['sha256']+'.out')).write_text(completed.stdout+completed.stderr)
    return {k:v for k,v in item.items() if k!='script'}|{'verdict':verdict,'exit_code':completed.returncode,'seconds':time.monotonic()-start}
with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool: rows=list(pool.map(run,grouped.values()))
passed=count>0 and all(r['verdict']=='unsat' and r['exit_code']==0 for r in rows)
result={'status':'pass' if passed else 'unconfirmed_or_disagreement','custom_closures':count,'unique_original_queries':len(rows),'results':rows}
(a.out/'results.json').write_text(json.dumps(result,indent=2)+'\n');print(result['status'],count,'custom closures',len(rows),'unique originals',flush=True)
raise SystemExit(0 if passed else 1)
