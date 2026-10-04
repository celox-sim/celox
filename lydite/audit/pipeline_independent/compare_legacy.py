"""Check default-backend compatibility on prior model families."""
from pathlib import Path
import argparse,json,subprocess,hashlib
ROOT=Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser();p.add_argument('--old',required=True);p.add_argument('--new',required=True);p.add_argument('--z3',required=True);a=p.parse_args();rows=[]
for name in ['cpu.json','auto_array_sum.json','memory_increment.lyd','scoped_dual_operator.lyd','quantified_counter.lyd','quantified_counter_bad_order.lyd','quantified_counter_impossible.lyd']:
 runs=[]
 for label,binary in [('old',a.old),('new',a.new)]:
  dest=ROOT/'results/pipeline_legacy'/label/Path(name).stem;dest.mkdir(parents=True,exist_ok=True)
  r=subprocess.run([binary,str(ROOT/'examples'/name),'--out',str(dest),'--z3',a.z3],capture_output=True,text=True,timeout=120)
  (dest/'run.stdout').write_text(r.stdout);(dest/'run.stderr').write_text(r.stderr)
  report=json.loads((dest/'report.json').read_text());smts={str(f.relative_to(dest)):f.read_bytes() for f in dest.rglob('*.smt2')}
  runs.append((r.returncode,report,smts))
 old,new=runs;assert old[0]==new[0] and old[1]['status']==new[1]['status'],name;assert old[2]==new[2],(name,'SMT changed')
 rows.append({'case':name,'status':new[1]['status'],'smt_files_identical':len(new[2])});print(rows[-1],flush=True)
(ROOT/'audit/pipeline_independent/legacy_results.json').write_text(json.dumps({'status':'pass','cases':rows,'total_identical_smt_files':sum(r['smt_files_identical'] for r in rows)},indent=2)+'\n')
