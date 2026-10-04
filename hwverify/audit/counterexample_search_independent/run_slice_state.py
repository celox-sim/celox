#!/usr/bin/env python3
"""Build test-only state instrumentation beside an immutable source snapshot."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[2]
AUDIT = Path(__file__).resolve().parent
p = argparse.ArgumentParser()
p.add_argument('--source', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
source = a.source.resolve()
out = a.output.resolve()
out.mkdir(parents=True, exist_ok=False)
production = source/'../crates/hwverify-solver/src/finite.rs'
raw = production.read_bytes()
checks = (AUDIT/'src/slice_state_tests.rs').read_bytes()
(out/'finite.production.rs').write_bytes(raw)
(out/'slice_state_tests.rs').write_bytes(checks)
(out/'finite.rs').write_bytes(raw+b'\n#[cfg(test)]\n#[path = "slice_state_tests.rs"]\nmod independent_slice_state;\n')
(out/'lib.rs').write_text('pub mod finite;\n')
(out/'Cargo.toml').write_text('''[package]
name = "counterexample-slice-state-independent"
version = "0.1.0"
edition = "2021"
publish = false
[workspace]
[lib]
path = "lib.rs"
[dependencies]
hwverify-ir = { path = '''+json.dumps(str(source/'../crates/hwverify-ir'))+''' }
serde_json = "1"
''')
env = dict(os.environ, CARGO_TARGET_DIR='/tmp/hwverify-counterexample-slice-audit-target')
subprocess.run(['cargo','generate-lockfile','--offline','--manifest-path',str(out/'Cargo.toml')],env=env,check=True)
proc = subprocess.run(['cargo','test','--offline','--locked','--release','--manifest-path',str(out/'Cargo.toml'),
                       'independent_slice_state','--','--nocapture','--test-threads=1'],env=env,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT)
(out/'run.log').write_text(proc.stdout)
print(proc.stdout)
assert production.read_bytes() == raw, 'production source changed during snapshot test'
result = {'status':'pass' if proc.returncode == 0 else 'FAIL','exit_code':proc.returncode,
          'production_sha256':hashlib.sha256(raw).hexdigest(), 'test_sha256':hashlib.sha256(checks).hexdigest(),
          'instrumentation':'Only appended cfg(test) child module; byte-identical production prefix preserved',
          'semantics':'Random CNFs checked by exhaustive truth assignments; exact internal state compared after every completed sliced run'}
(out/'summary.json').write_text(json.dumps(result,indent=2)+'\n')
raise SystemExit(proc.returncode)
