#!/usr/bin/env python3
"""Materialize and verify pinned open-source dependencies, then apply named patches."""
import hashlib,json,os,pathlib,shutil,subprocess,tarfile,urllib.request
ROOT=pathlib.Path(__file__).resolve().parent
WORK=ROOT/'work'

def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
def run(*args,cwd=None):subprocess.run(args,cwd=cwd,check=True)

def verify_suite(suite):
    expected=json.loads((ROOT/'upstream-sources.json').read_text())['files']
    actual={str(path.relative_to(suite)):sha(path) for path in suite.rglob('*') if path.is_file()}
    if actual!=expected:raise RuntimeError('pinned original suite source hashes changed; refusing coverage/oracle drift')
    return actual

def main():
    pins=json.loads((ROOT/'dependencies.json').read_text())
    if WORK.exists():
        if not (WORK/'.generated-by-hwverify-proof').is_file():raise RuntimeError('refusing to replace an unowned work directory')
        shutil.rmtree(WORK)
    WORK.mkdir();(WORK/'.generated-by-hwverify-proof').write_text('generated dependency checkout\n')
    celox=WORK/'celox';celox.mkdir();run('git','init','-q',str(celox))
    source=os.environ.get('CELOX_SOURCE_REPOSITORY','https://github.com/celox-sim/celox.git')
    run('git','-C',str(celox),'fetch','--depth=1',source,pins['celox_revision'])
    run('git','-C',str(celox),'-c','advice.detachedHead=false','checkout','--detach','FETCH_HEAD')
    actual=subprocess.check_output(['git','-C',str(celox),'rev-parse','HEAD'],text=True).strip()
    if actual!=pins['celox_revision']:raise RuntimeError('wrong upstream revision')
    verify_suite(celox/'crates/celox-test-suite-veryl')
    name=f'veryl-analyzer-{pins["analyzer_version"]}.crate'
    cargo=pathlib.Path(os.environ.get('CARGO_HOME',pathlib.Path.home()/'.cargo'))
    candidates=sorted((cargo/'registry/cache').glob('*/'+name))
    archive=WORK/name
    if candidates:shutil.copyfile(candidates[0],archive)
    else:
        urllib.request.urlretrieve('https://static.crates.io/crates/veryl-analyzer/'+name,archive)
    if sha(archive)!=pins['analyzer_crate_sha256']:raise RuntimeError('analyzer crate checksum mismatch')
    analyzer=WORK/'analyzer';analyzer.mkdir()
    prefix='veryl-analyzer-'+pins['analyzer_version']
    with tarfile.open(archive,'r:gz') as tar:
        for member in tar.getmembers():
            parts=pathlib.PurePosixPath(member.name).parts
            if not parts or parts[0]!=prefix or any(x in ('.','..') for x in parts):raise RuntimeError('unsafe archive path')
            dest=analyzer.joinpath(*parts[1:])
            if member.isdir():dest.mkdir(parents=True,exist_ok=True)
            elif member.isfile():
                dest.parent.mkdir(parents=True,exist_ok=True)
                with tar.extractfile(member) as src,open(dest,'wb') as target:shutil.copyfileobj(src,target)
            else:raise RuntimeError('unexpected analyzer archive member type')
    for patch in pins['patches']:
        path=ROOT/'patches'/patch['file']
        if sha(path)!=patch['sha256']:raise RuntimeError('dependency patch hash changed: '+path.name)
        destination=analyzer if patch['target']=='analyzer' else celox
        run('git','apply','--check',str(path),cwd=destination)
        run('git','apply',str(path),cwd=destination)
    # Patching the compiler must never change the independent original test suite.
    files=verify_suite(celox/'crates/celox-test-suite-veryl')
    (WORK/'provenance.json').write_text(json.dumps({'dependencies':pins,'original_suite_files':files},indent=2)+'\n')
    print('Pinned dependencies prepared; original suite unchanged')
if __name__=='__main__':main()
