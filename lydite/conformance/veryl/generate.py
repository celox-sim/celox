#!/usr/bin/env python3
"""Create isolated instrumented corpus copy; do not alter the pinned source."""
import argparse, hashlib, json, os, pathlib, re, shutil
ROOT=pathlib.Path(__file__).resolve().parent
DEFAULT=pathlib.Path(os.environ.get('CELOX_SUITE_ROOT', ROOT/'upstream/crates/celox-test-suite-veryl'))

def generate(source, destination):
    if destination.exists(): shutil.rmtree(destination)
    shutil.copytree(source/'src',destination/'src')
    shutil.copy2(source/'README.md',destination/'README.md')
    (destination/'Cargo.toml').write_text('''[package]
name = "celox-test-suite-veryl"
version = "0.8.2"
edition = "2024"
license = "MIT OR Apache-2.0"
autobins = false
[features]
default = []
emit = []
verilator = []
icarus = []
[dependencies]
num-bigint = "=0.5.1"
num-traits = "0.2"
veryl-std = "=0.21.0"
serde_json = "1"
capture-macros = { path = "../capture-macros" }
''')
    p=destination/'src/cases/mod.rs';s=p.read_text()
    old='                    $($body)*'
    if s.count(old)!=1: raise ValueError('case macro shape changed')
    p.write_text(s.replace(old,'                    capture_macros::capture_body!({ $($body)* });'))
    p=destination/'src/lib.rs';p.write_text(p.read_text()+'\n/// Experimental extraction support, not part of the upstream crate.\npub mod capture;\n')
    p=destination/'src/backend.rs';p.write_text(p.read_text()+'''
// Instrumentation only: this does not call Backend::read or invent an output.
impl Simulator {
    pub fn capture_observation(&mut self, signal: Signal, kind: &str, comparison: &str,
        file: &str, line: u32, column: u32, expression: &str, sample: (&str, u32, u32), actual_form: &str) -> usize {
        let path = self.signals.borrow()[signal.0].clone();
        crate::capture::begin(path, kind, comparison, file, line, column, expression, sample, actual_form)
    }
}
''')
    shutil.copy2(ROOT/'capture_runtime.rs',destination/'src/capture.rs')
    shutil.copy2(ROOT/'capture_tests.rs',destination/'src/capture_tests.rs')
    files={};cases={}
    for p in sorted(source.rglob('*')):
        if not p.is_file(): continue
        rel=str(p.relative_to(source));files[rel]=hashlib.sha256(p.read_bytes()).hexdigest()
        if rel.startswith('src/cases/') and p.suffix=='.rs':
            text=p.read_text();m=re.search(r'cases!\s*\{\s*\w+,\s*"([^"]+)";',text)
            if m:
                for n in re.finditer(r'\bfn\s+(\w+)\s*\(sim\)',text):
                    cases[m[1]+'::'+n[1]]={'file':rel,'line':text.count('\n',0,n.start())+1,'sha256':files[rel]}
    manifest={'upstream_commit':'124a1315096d21b85d9d0d84fd7139363a181cad','source_root':str(source.resolve()),'files':files,'cases':cases}
    (ROOT/'provenance.json').write_text(json.dumps(manifest,indent=2)+'\n')
    return manifest
if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--source',type=pathlib.Path,default=DEFAULT)
    args=parser.parse_args();m=generate(args.source,ROOT/'generated-suite')
    print(f'Copied {len(m["files"])} source files; located {len(m["cases"])} cases')
