"""Independent subprocess checks of missing-case/UNKNOWN/fallback CI gates.

The deliberately fake solver reports are failure-injection controls, not FV
results. The real typed analyzer still processes a real pinned source case.
"""
import copy,json,os,subprocess,sys
from pathlib import Path
REPO=Path(__file__).resolve().parents[2];ADAPTER=REPO/'conformance/veryl'
SOURCE=Path(sys.argv[1]);OUT=Path(sys.argv[2]);OUT.mkdir(parents=True,exist_ok=False)
data=json.loads(SOURCE.read_text());golden=json.loads((ADAPTER/'coverage-manifest.json').read_text())
name='operators::test_bitwise_operations'
row=next(x for x in data['cases'] if x['case']==name)
subset=copy.deepcopy(data);subset['cases']=[row]
(OUT/'one-case.json').write_text(json.dumps(subset))
small=copy.deepcopy(golden);small['cases']=[x for x in golden['cases'] if x['case']==name]
(OUT/'one-case-golden.json').write_text(json.dumps(small))
probe=ADAPTER/'analyzer-probe/target/debug/veryl-analyzer-probe'
rows=[]
def invoke(label, manifest, fake=None):
 env=dict(os.environ,VERYL_PROBE_BIN=str(probe))
 if fake:env['LYDITE_BIN']=str(fake)
 dest=OUT/label
 p=subprocess.run([sys.executable,str(ADAPTER/'catalog.py'),'--traces',str(OUT/'one-case.json'),'--golden',str(manifest),'--out',str(dest)],env=env,capture_output=True,text=True,timeout=60)
 (OUT/(label+'.stdout')).write_text(p.stdout);(OUT/(label+'.stderr')).write_text(p.stderr)
 report=json.loads((dest/'report.json').read_text())
 assert p.returncode!=0 and report['status']=='failed',(label,p.returncode,report)
 rows.append({'control':label,'exit_code':p.returncode,'status':report['status'],'error':report['error']})
invoke('missing_664_cases',ADAPTER/'coverage-manifest.json')
for label,backend in [('unknown_result','finite_bv'),('external_backend','z3')]:
 fake=OUT/(label+'-solver')
 fake.write_text('''#!/usr/bin/env python3
import json,sys,pathlib
source=json.loads(pathlib.Path(sys.argv[1]).read_text());out=pathlib.Path(sys.argv[sys.argv.index('--out')+1]);out.mkdir(parents=True)
rows=[{'target':target,'example':name,'status':'unknown','admitted':None,'evidence':{'backend':BACKEND}} for target,spec in source['specs'].items() for name in spec['examples']]
(out/'report.json').write_text(json.dumps({'status':'unknown','examples':rows}));sys.exit(3)
'''.replace('BACKEND',repr(backend)))
 fake.chmod(0o700);invoke(label,OUT/'one-case-golden.json',fake)
(OUT/'summary.json').write_text(json.dumps({'status':'pass','controls':rows},indent=2)+'\n');print(json.dumps(rows,indent=2))
