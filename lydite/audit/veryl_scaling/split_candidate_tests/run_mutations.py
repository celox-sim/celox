#!/usr/bin/env python3
"""Negative sensitivity controls on disposable SV copies, never source edits."""
import argparse, hashlib, json, subprocess, sys
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--sv',type=Path,required=True);p.add_argument('--out',type=Path,required=True);p.add_argument('--tools',type=Path,required=True);a=p.parse_args();a.out.mkdir(parents=True,exist_ok=True)
s=a.sv.read_text();results=[]
forward_old,forward_new=('operand1 = m_result;','operand1 = w_result;') if 'operand1 = m_result;' in s else ('(m_result & m_mask1)','(w_result & m_mask1)')
mutations=[('bad_m_forward',forward_old,forward_new),('no_hazard','hazard   = d_valid',"hazard   = 1'b0 && d_valid")]
if 'd_sel2' in s:mutations.append(('wrong_selector_capture',"32'd1 << imem_response[24:20]","32'd1 << imem_response[19:15]"))
for name,old,new in mutations:
 assert s.count(old)==1,(name,'unexpected mutation target count')
 target=a.out/(name+'.sv');target.write_text(s.replace(old,new))
 cp=subprocess.run([sys.executable,str(Path(__file__).with_name('run_sv.py')),'--sv',str(target),'--out',str(a.out/name),'--tools',str(a.tools)],capture_output=True,text=True)
 (a.out/(name+'.log')).write_text(cp.stdout+cp.stderr)
 report=json.loads((a.out/name/'report.json').read_text())
 log=(a.out/name/'simulation.log').read_text()
 assert cp.returncode!=0 and report['returncode']!=0 and 'Assertion failed' in log,(name,'mutant not rejected by simulation assertion')
 results.append(dict(name=name,source_sha256=hashlib.sha256(target.read_bytes()).hexdigest(),detected=True,failure=next(l for l in log.splitlines() if 'Assertion failed' in l)))
report=dict(original_sv=str(a.sv),original_sha256=hashlib.sha256(a.sv.read_bytes()).hexdigest(),mutations=results)
(a.out/'mutations-report.json').write_text(json.dumps(report,indent=2));print(json.dumps(report,indent=2))
