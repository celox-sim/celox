"""Compare published qualified source with bare-name source through public CLIs."""
import argparse, hashlib, json, subprocess
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--old',required=True,type=Path);p.add_argument('--new',required=True,type=Path);p.add_argument('--edited',required=True,type=Path);p.add_argument('--baseline',required=True,type=Path);p.add_argument('--z3',required=True);a=p.parse_args()
root=Path(__file__).resolve().parents[2]; out=root/'results/bare_quantifiers_independent'
def stable(x):
 if isinstance(x,dict):return {k:stable(v) for k,v in x.items() if k!='seconds' and not k.endswith('_seconds')}
 if isinstance(x,list):return [stable(v) for v in x]
 return x
rows=[]
for name in ['quantified_counter','quantified_counter_bad_order','quantified_counter_impossible']:
 runs=[]
 for label,binary,base in [('old',a.old,a.baseline),('new',a.new,a.edited)]:
  dest=out/label/name;dest.mkdir(parents=True,exist_ok=True)
  source=base/'examples'/f'{name}.lyd'
  parse=subprocess.run([str(binary),str(source),'--emit-json',str(dest/'canonical.json')],capture_output=True,text=True,timeout=30)
  assert parse.returncode==0,parse.stderr
  r=subprocess.run([str(binary),str(source),'--out',str(dest),'--z3',a.z3],capture_output=True,text=True,timeout=90)
  (dest/'run.stdout').write_text(r.stdout);(dest/'run.stderr').write_text(r.stderr)
  runs.append((dest,json.loads((dest/'canonical.json').read_text()),json.loads((dest/'report.json').read_text()),r.returncode))
 x,y=runs
 assert x[1]==y[1],(name,'canonical JSON')
 assert stable(x[2])==stable(y[2]),(name,'non-timing report')
 assert x[3]==y[3],(name,'exit code')
 smts=[{str(f.relative_to(z[0])):f.read_bytes() for f in z[0].rglob('*.smt2')} for z in runs]
 assert smts[0]==smts[1],(name,'SMT')
 rows.append({'case':name,'status':y[2]['status'],'exit_code':y[3],'identical_smt_files':len(smts[1]),'canonical_json_identical':True,'non_timing_report_identical':True});print(rows[-1],flush=True)
result={'status':'pass','old_binary_sha256':hashlib.sha256(a.old.read_bytes()).hexdigest(),'new_binary_sha256':hashlib.sha256(a.new.read_bytes()).hexdigest(),'cases':rows}
(root/'audit/bare_quantifiers_independent/results.json').write_text(json.dumps(result,indent=2)+'\n')
