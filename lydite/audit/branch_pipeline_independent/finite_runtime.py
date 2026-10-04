#!/usr/bin/env python3
"""Independent finite invocation with an executable Z3-call tripwire.

The tripwire would record any attempted fallback and fail. This run checks final
binary identity before and after all invocations; it is correctness evidence,
not a performance benchmark.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

ROOT=Path(__file__).resolve().parents[2]


def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()


def main():
    p=argparse.ArgumentParser()
    p.add_argument('--binary',type=Path,required=True)
    p.add_argument('--output',type=Path,default=ROOT/'results/branch_pipeline_independent/finite')
    a=p.parse_args()
    binary=a.binary.resolve();folder=a.output.resolve();folder.mkdir(parents=True,exist_ok=True)
    marker=folder/'Z3_WAS_CALLED'
    assert not marker.exists(),'old_tripwire_marker_exists'
    tripwire=folder/'z3_tripwire.py'
    tripwire.write_text('#!/usr/bin/env python3\nfrom pathlib import Path\nPath('+repr(str(marker))+').write_text("Unexpected Z3 invocation\\n")\nraise SystemExit(97)\n')
    tripwire.chmod(0o755)
    sources=[ROOT/f'examples/branch_pipeline_w{w}.json' for w in (4,8,16,32)]+sorted((ROOT/'results/branch_pipeline_independent/extra_mutants').glob('*.json'))
    initial={'binary':sha(binary),'sources':{str(s.relative_to(ROOT)):sha(s) for s in sources}}
    (folder/'frozen_run_inputs.json').write_text(json.dumps(initial,indent=2)+'\n')
    rows=[]
    for source in sources:
        dest=folder/source.stem
        env=dict(os.environ);env['LYDITE_SOLVER']='finite'
        cmd=[str(binary),str(source),'--out',str(dest),'--z3',str(tripwire)]
        started=time.perf_counter()
        proc=subprocess.run(cmd,env=env,text=True,capture_output=True,timeout=120)
        elapsed=time.perf_counter()-started
        report=json.loads(proc.stdout)
        (dest/'stdout.json').write_text(proc.stdout)
        (dest/'stderr.log').write_text(proc.stderr)
        assert not marker.exists(),('z3_fallback_attempt',source)
        assert report['engine_summary']['z3_queries']==0,('z3_reported',source)
        assert all(q['backend'] in ('finite_bv','structural_kernel') for q in report['obligations'])
        expected='counterexample' if '_bad_' in source.stem else ('unknown' if source.stem.endswith(('w16','w32')) else 'stuttering_refinement_verified')
        assert report['status'] in ({'counterexample','unknown'} if '_bad_' in source.stem else {expected}),(source,report['status'],expected)
        assert all(q['solver_result'] in ('sat','unsat','unknown') for q in report['obligations'])
        row={'model':source.stem,'source':str(source.relative_to(ROOT)),'source_sha256':sha(source),'status':report['status'],'returncode':proc.returncode,'seconds_not_benchmark':elapsed,'obligations':[{'name':q['name'],'backend':q['backend'],'solver_result':q['solver_result'],'status':q['status']} for q in report['obligations']]}
        rows.append(row);print(json.dumps(row),flush=True)
    assert sha(binary)==initial['binary'],'binary_changed_during_run'
    assert all(sha(ROOT/path)==digest for path,digest in initial['sources'].items()),'source_changed_during_run'
    result={'status':'pass','z3_tripwire_invocations':0,'binary_sha256':initial['binary'],'models':len(rows),'rows':rows}
    (folder/'runtime_summary.json').write_text(json.dumps(result,indent=2)+'\n')

if __name__=='__main__':main()
