"""Independent concrete semantics and deliberate kernel-mutation rejection.
Requires rustc (or --rustc); does not use Z3. Copies to temporary build directory.
"""
from pathlib import Path
import argparse,json,re,subprocess,tempfile,time
HERE=Path(__file__).resolve().parent;ROOT=HERE.parents[1]
p=argparse.ArgumentParser();p.add_argument('--rustc',default='rustc');p.add_argument('--out',type=Path,default=HERE/'kernel_results.json');a=p.parse_args()
rows=[]
with tempfile.TemporaryDirectory(prefix='hwverify-kernel-audit-') as temp:
    t=Path(temp);source=(HERE/'check_kernel.rs').read_text().replace('../../crates/ir/src/term.rs',str(ROOT/'crates/ir/src/term.rs')).replace('../../crates/solver/src/kernel.rs',str(t/'kernel.rs'))
    (t/'check.rs').write_text(source);original=(ROOT/'crates/solver/src/kernel.rs').read_text()
    pattern=r'("bvsub"\s+if\s+a\[0\]\s*==\s*a\[1\]\s*=>\s*result\s*=\s*Some\(bv\(w,\s*)0(\)\))'
    mutant,count=re.subn(pattern,lambda m:m[1]+'1'+m[2],original)
    assert count==1,('mutation site drift',count)
    for name,body,expected_ok in [('original',original,True),('wrong_self_subtraction',mutant,False)]:
        (t/'kernel.rs').write_text(body);start=time.monotonic()
        build=subprocess.run([a.rustc,'--edition=2021','-O',str(t/'check.rs'),'-o',str(t/'check')],capture_output=True,text=True,timeout=90)
        assert build.returncode==0,build.stderr
        run=subprocess.run([str(t/'check')],capture_output=True,text=True,timeout=120)
        assert (run.returncode==0)==expected_ok,(name,run.returncode,run.stdout,run.stderr)
        rows.append({'variant':name,'exit_code':run.returncode,'seconds_including_compile':time.monotonic()-start,'stdout':run.stdout,'stderr':run.stderr})
        print(name,'PASS' if expected_ok else 'MUTATION REJECTED',flush=True)
a.out.parent.mkdir(parents=True,exist_ok=True);a.out.write_text(json.dumps({'status':'pass','checks':rows},indent=2)+'\n')
