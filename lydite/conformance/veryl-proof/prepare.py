#!/usr/bin/env python3
"""Record the in-tree Celox revision and reusable suite this run tests."""
import hashlib,json,pathlib,shutil,subprocess
ROOT=pathlib.Path(__file__).resolve().parent
REPO=ROOT.parents[2]
WORK=ROOT/'work'
SUITE=REPO/'crates/celox-test-suite-veryl'

def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
def git(*args):return subprocess.check_output(['git',*args],cwd=REPO,text=True)

def verify_suite(suite=SUITE):
    """Hash every tracked suite file, so callers can prove a run left it unchanged."""
    names=git('ls-files','-z','--',str(suite.relative_to(REPO))).split('\0')
    return {str((REPO/name).relative_to(suite)):sha(REPO/name) for name in names if name}

def main():
    if WORK.exists():
        if not (WORK/'.generated-by-lydite-proof').is_file():raise RuntimeError('refusing to replace an unowned work directory')
        shutil.rmtree(WORK)
    WORK.mkdir();(WORK/'.generated-by-lydite-proof').write_text('generated run state\n')
    provenance={'celox_revision':git('rev-parse','HEAD').strip(),
                # Uncommitted compiler or suite edits are part of what this run tests.
                'uncommitted_paths':sorted(line[3:] for line in git('status','--porcelain','--','crates','Cargo.lock').splitlines()),
                'suite_files':verify_suite()}
    (WORK/'provenance.json').write_text(json.dumps(provenance,indent=2)+'\n')
    print('Recorded in-tree Celox',provenance['celox_revision'])
if __name__=='__main__':main()
