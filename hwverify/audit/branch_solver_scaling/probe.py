#!/usr/bin/env python3
"""Development probes; use benchmark.py for final repeated wall-time claims."""
import argparse,json,os,subprocess,time
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--label',required=True);p.add_argument('--all',action='store_true');args=p.parse_args()
cases=[f'branch_pipeline_w{w}' for w in (4,8,16,32)]+['branch_pipeline_w32_bad_branch_forward','branch_pipeline_w32_bad_branch_interlock','pipeline']
if args.all:
 cases=[f'branch_pipeline_w{w}{s}' for w in (4,8,16,32) for s in ('','_bad_no_flush','_bad_wrong_kill','_bad_wrong_target','_bad_stall_branch','_bad_branch_forward','_bad_branch_interlock','_bad_branch_write')]+['pipeline'+s for s in ('','_bad_forward','_bad_priority','_bad_interlock','_bad_retire','_bad_stall')]
out=ROOT/'results/branch_solver_scaling'/args.label
assert not out.exists(),out
rows=[]
for case in cases:
 d=out/case;start=time.perf_counter();r=subprocess.run([str(args.binary.resolve()),str(ROOT/'examples'/f'{case}.json'),'--out',str(d),'--z3','/definitely/absent/z3'],env=dict(os.environ,HWVERIFY_SOLVER='finite'),capture_output=True,text=True,timeout=180)
 wall=time.perf_counter()-start;report=json.loads(r.stdout);(d/'stderr.log').write_text(r.stderr);q=next(x for x in report['obligations'] if x['name']=='microstep_refinement');f=q.get('finite',{});row=dict(case=case,status=report['status'],microstep=q['solver_result'],wall_seconds=wall,finite={k:f.get(k) for k in ('variables','clauses','decisions','conflicts','work')},profiles=[json.loads(x[8:]) for x in r.stderr.splitlines() if x.startswith('PROFILE ')]);rows.append(row);print(json.dumps(row),flush=True)
(out/'summary.json').write_text(json.dumps(rows,indent=2)+'\n')
