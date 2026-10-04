#!/usr/bin/env bash
# Mandatory source simulation and property replay gate; no optional simulator skip.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
OUT=${1:?provide a fresh evidence directory}
test ! -e "$OUT"
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
exec > >(tee "$OUT/run.log") 2>&1
finish() {
  status=$?
  python3 - "$OUT" "$status" <<'PY'
import json,pathlib,sys
pathlib.Path(sys.argv[1],'run-status.json').write_text(json.dumps({'status':'passed' if sys.argv[2]=='0' else 'failed','exit_code':int(sys.argv[2])},indent=2)+'\n')
PY
  trap - EXIT
  exit "$status"
}
trap finish EXIT
cd "$REPO"
python3 conformance/veryl-proof/prepare.py
cargo build --locked -p lydite-celox
cargo fmt --check -p lydite-celox
cargo build --release --locked -p lydite -p lydite-celox
cargo test --release --locked -p lydite-verify reachable::tests
cargo test --release --locked -p lydite-verify --test inductive_safety
python3 "$HERE/test_replay.py"
python3 - "$OUT" <<'PY'
import hashlib,json,pathlib,subprocess,sys
out=pathlib.Path(sys.argv[1]); paths=[pathlib.Path(p) for p in ['conformance/veryl-proof/work/provenance.json','../Cargo.lock','../target/debug/lydite-celox-export','../target/debug/lydite-celox-replay','../target/release/lydite-replay','../target/release/lydite-celox-lift','../target/release/lydite-structure']]
(out/'executables-and-pins.json').write_text(json.dumps({str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in paths},indent=2)+'\n')
(out/'head.txt').write_text(subprocess.check_output(['git','rev-parse','HEAD'],text=True))
PY
python3 "$HERE/run.py" --out "$OUT/cases"
