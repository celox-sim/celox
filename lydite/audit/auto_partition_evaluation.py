"""Reproducible complete evaluation; retain all failures, no cherry-picked runs."""
import subprocess,time,json,os
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
os.chdir(ROOT)
names=['auto_array_sum','auto_array_sum_renamed','auto_array_sum_reordered','auto_array_sum_unsplit','auto_array_sum_bad_program','auto_array_sum_false_invariant','auto_array_sum_false_pre','auto_array_sum_bad_rank','auto_array_sum_input_invariant','auto_array_sum_wrong_indexed_load','array_sum']
r=[]
for n in names:
 out=Path('results/auto_partition_0_4')/n;out.mkdir(parents=True,exist_ok=True)
 start=time.monotonic();p=subprocess.run(['../target/debug/lydite',f'examples/{n}.json','--out',str(out),'--z3',os.environ.get('Z3_BIN','../proof-binding-study/.venv/bin/z3')],stdout=subprocess.DEVNULL)
 d=json.loads((out/'report.json').read_text());row=dict(name=n,status=d['status'],exit_code=p.returncode,wall_seconds=time.monotonic()-start,queries=len(d.get('obligations',[])),failures=[{'name':o['name'],'status':o['status']} for o in d.get('obligations',[]) if o['status']!='passed'],plan=d.get('partition_plan'),error=d.get('error'));r.append(row);print(n,row['status'],row['wall_seconds'],row['queries'],flush=True)
 Path('audit/auto_partition_results.json').write_text(json.dumps(r,indent=2)+'\n')
