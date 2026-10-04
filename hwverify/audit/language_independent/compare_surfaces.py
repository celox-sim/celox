"""Persist CLI evidence for DSL/JSON equivalence; uses only real public CLI paths."""
from pathlib import Path
import argparse,hashlib,json,subprocess
ROOT=Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser();p.add_argument('--binary',required=True,type=Path);p.add_argument('--z3',required=True);p.add_argument('--out',type=Path,default=ROOT/'results/language_0_6_surfaces');a=p.parse_args()
PAIRS=[('array_sum','examples/array_sum.hwv','examples/array_sum.json'),('auto_array_sum','examples/auto_array_sum.hwv','examples/auto_array_sum.json'),('memory_increment','examples/memory_increment.hwv','audit/structural_solver/memory_increment_good.json'),('memory_increment_readable','examples/memory_increment_readable.hwv','audit/structural_solver/memory_increment_good.json')]
rows=[]
for name,dsl,js in PAIRS:
 dirs={};reports={}
 for mode,source in [('dsl',dsl),('json',js)]:
  out=a.out/name/mode;out.mkdir(parents=True,exist_ok=True);dirs[mode]=out
  proc=subprocess.run([str(a.binary.resolve()),str(ROOT/source),'--out',str(out),'--z3',a.z3],capture_output=True,text=True,timeout=90)
  assert proc.returncode==0,(source,proc.stdout,proc.stderr)
  (out/'run.stdout').write_text(proc.stdout);(out/'run.stderr').write_text(proc.stderr);reports[mode]=json.loads((out/'report.json').read_text())
 emit=dirs['dsl']/'canonical.json';validate=dirs['dsl']/'validation'
 p2=subprocess.run([str(a.binary.resolve()),str(ROOT/dsl),'--out',str(validate),'--emit-json',str(emit)],capture_output=True,text=True,timeout=30)
 assert p2.returncode==0,(dsl,p2.stdout,p2.stderr);assert json.loads(emit.read_text())==json.loads((ROOT/js).read_text())
 lhs,rhs=reports['dsl'],reports['json'];assert lhs['status']==rhs['status']=='program_and_refinement_verified';assert lhs['normalization']==rhs['normalization']
 assert len(lhs['obligations'])==len(rhs['obligations']);proofs=[]
 for x,y in zip(lhs['obligations'],rhs['obligations']):
  for field in ['name','status','solver_result','backend','context_symbols']:assert x.get(field)==y.get(field),(name,x['name'],field)
  d=(dirs['dsl']/x['evidence']).read_bytes();j=(dirs['json']/y['evidence']).read_bytes();assert d==j,(name,x['name'])
  proofs.append({'name':x['name'],'sha256':hashlib.sha256(d).hexdigest()})
 for report in reports.values():report.get('partition_plan',{}).pop('scoring_seconds',None)
 assert lhs['partition_plan']==rhs['partition_plan']
 rows.append({'name':name,'dsl':dsl,'json':js,'status':lhs['status'],'canonical_equal':True,'obligations':proofs});print(name,len(proofs),'identical',flush=True)
(ROOT/'audit/language_independent/surface_results.json').write_text(json.dumps({'status':'pass','binary_sha256':hashlib.sha256(a.binary.read_bytes()).hexdigest(),'pairs':rows,'total_obligations':sum(len(x['obligations']) for x in rows)},indent=2)+'\n')
