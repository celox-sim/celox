"""Required candidate-interface tests; no saved result can authorize a proof."""
import argparse
import json
from pathlib import Path
import subprocess
import sys


def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--out',type=Path,required=True)
    a=p.parse_args();a.out.mkdir(parents=True,exist_ok=False)
    commands=[['cargo','test','--release','--locked','-p','hwverify-verify','--test','lemma_candidates'],
              ['cargo','test','--release','--locked','-p','hwverify-verify','proof_program::tests'],
              [sys.executable,'-m','unittest','audit.lemma_candidates.test_migrate']]
    for i,command in enumerate(commands):
        with (a.out/f'check-{i}.log').open('w') as log:
            subprocess.run(command,stdout=log,stderr=subprocess.STDOUT,check=True)
    (a.out/'summary.json').write_text(json.dumps({'status':'passed','checks':commands,
        'scope':'typed candidate interface, live sequent boundaries and lossless migration; selected RV gate exercises all six migrated manual programs',
        'saved_results_authorize_proofs':False},indent=2)+'\n')

if __name__=='__main__':main()
