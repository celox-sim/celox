"""Compare v0.6 and workspace CLI on unchanged JSON inputs; timings not comparable unless same profile."""
from pathlib import Path
import argparse,hashlib,json,subprocess,time
ROOT=Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser();p.add_argument('--old',required=True,type=Path);p.add_argument('--new',required=True,type=Path);p.add_argument('--z3',required=True);p.add_argument('--out',type=Path,default=ROOT/'results/composition_0_7_legacy_v2');a=p.parse_args()
CASES=['examples/auto_array_sum.json','examples/cpu.json','examples/wrong_load.json','examples/hang_fetch.json','audit/partition_independent/multiplier_good.json','audit/structural_solver/memory_increment_good.json','audit/structural_solver/memory_increment_bad_alias.json']
rows=[]
for case in CASES:
 results=[]
 for label,binary in [('v0.6',a.old),('workspace',a.new)]:
  out=a.out/label/Path(case).stem;out.mkdir(parents=True,exist_ok=True);start=time.monotonic()
  run=subprocess.run([str(binary.resolve()),str(ROOT/case),'--out',str(out),'--z3',a.z3],capture_output=True,text=True,timeout=90)
  (out/'run.stdout').write_text(run.stdout);(out/'run.stderr').write_text(run.stderr)
  report=json.loads((out/'report.json').read_text());results.append((out,report,run.returncode))
 (old_dir,old,old_exit),(new_dir,new,new_exit)=results
 assert old['status']==new['status'] and old_exit==new_exit,(case,old['status'],new['status'])
 assert old['normalization']==new['normalization'],case
 assert len(old['obligations'])==len(new['obligations']),case
 evidence=[]
 for x,y in zip(old['obligations'],new['obligations']):
  for key in ['name','status','solver_result','backend','context_symbols']:assert x.get(key)==y.get(key),(case,x['name'],key)
  lhs=(old_dir/x['evidence']).read_bytes();rhs=(new_dir/y['evidence']).read_bytes();assert lhs==rhs,(case,x['name'],'SMT bytes')
  evidence.append({'name':x['name'],'sha256':hashlib.sha256(rhs).hexdigest()})
 for r in [old,new]:
  if isinstance(r.get('partition_plan'),dict):r['partition_plan'].pop('scoring_seconds',None)
 assert old.get('partition_plan')==new.get('partition_plan'),case
 rows.append({'case':case,'status':new['status'],'exit_code':new_exit,'obligations':evidence});print(case,new['status'],len(evidence),'identical',flush=True)
result={'status':'pass','baseline_sha256':hashlib.sha256(a.old.read_bytes()).hexdigest(),'workspace_sha256':hashlib.sha256(a.new.read_bytes()).hexdigest(),'cases':rows,'total_obligations':sum(len(r['obligations']) for r in rows)}
(ROOT/'audit/composition_independent/legacy_v2_results.json').write_text(json.dumps(result,indent=2)+'\n')
