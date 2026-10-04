"""Required candidate-interface tests; no saved result can authorize a proof."""
import argparse
import json
from pathlib import Path
import subprocess
import sys


def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--out',type=Path,required=True)
    p.add_argument('--checker',type=Path,default=Path('../target/release/lydite'))
    a=p.parse_args();a.out.mkdir(parents=True,exist_ok=False)
    commands=[['cargo','test','--release','--locked','-p','lydite-verify','--test','lemma_candidates','--test','native_lemmas'],
              ['cargo','test','--release','--locked','-p','lydite-verify','proof_program::tests'],
              [sys.executable,'-m','unittest','audit.lemma_candidates.test_migrate']]
    for i,command in enumerate(commands):
        with (a.out/f'check-{i}.log').open('w') as log:
            subprocess.run(command,stdout=log,stderr=subprocess.STDOUT,check=True)
    checker=str(a.checker.resolve())
    source=Path('audit/lemma_candidates/counter.lyd')
    with (a.out/'native-cli.log').open('w') as log:
        subprocess.run([checker,str(source),'--check','--out',str(a.out/'native-check'),'--emit-json',str(a.out/'counter.json')],stdout=log,stderr=subprocess.STDOUT,check=True)
        bad=a.out/'wrong-width.lyd'
        bad.write_text(source.read_text().replace("impl.x' == spec.x';","impl.x' == 0u16;"))
        rejected=subprocess.run([checker,str(bad),'--check','--out',str(a.out/'rejected-check')],capture_output=True,text=True)
        log.write(rejected.stdout+rejected.stderr)
        if rejected.returncode==0 or 'wrong-width.lyd:' not in rejected.stdout+rejected.stderr:
            raise ValueError('native CLI failed to reject/source-link a width error before solving')
    (a.out/'summary.json').write_text(json.dumps({'status':'passed','checks':commands,
        'scope':'native CLI and typed candidate interface, live sequent boundaries and lossless migration; selected RV gate attaches native RV delivery/guard-use source and exercises all six migrated manual programs',
        'saved_results_authorize_proofs':False},indent=2)+'\n')

if __name__=='__main__':main()
