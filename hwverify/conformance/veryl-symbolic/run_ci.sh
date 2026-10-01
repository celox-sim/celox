#!/usr/bin/env bash
# Run after veryl-proof/run_ci.sh, reusing its checksum-pinned frontend build.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
OUT=${1:?Choose a fresh evidence directory}
if test -e "$OUT"; then echo "Evidence directory already exists: $OUT" >&2; exit 2; fi
mkdir -p "$OUT"; OUT=$(cd "$OUT" && pwd)
exec > >(tee "$OUT/run.log") 2>&1
finish() {
    status=$?
    if test -e "$OUT/z3-invoked.txt"; then status=1; fi
    python3 - "$OUT" "$status" <<'PY'
import json, pathlib, sys
out = pathlib.Path(sys.argv[1])
(out/'run-status.json').write_text(json.dumps({'status': 'passed' if sys.argv[2]=='0' else 'failed', 'exit_code': int(sys.argv[2]), 'external_solver_invoked': (out/'z3-invoked.txt').exists()}, indent=2)+'\n')
PY
    trap - EXIT; exit "$status"
}
trap finish EXIT
cd "$REPO"
FRONTEND="$REPO/conformance/veryl-proof/target/debug/veryl-proof-frontend"
test -x "$FRONTEND"
cp conformance/veryl-proof/work/provenance.json "$OUT/frontend-provenance.json"
cargo build --release --locked -p hwverify-rs -p hwverify-sir
cargo fmt --all --check
python3 - "$OUT" "$REPO" "$FRONTEND" <<'PYHASH'
import hashlib, json, pathlib, subprocess, sys
out, root, frontend = map(pathlib.Path, sys.argv[1:])
files = [root/'Cargo.toml', root/'Cargo.lock', frontend,
         root/'target/release/hwverify-rs', root/'target/release/hwverify-sir-lift']
for directory in ('crates/sir', 'conformance/veryl-symbolic'):
    files.extend(p for p in (root/directory).rglob('*') if p.is_file()
                 and '__pycache__' not in p.parts and p.suffix != '.pyc')
(out/'tested-files.json').write_text(json.dumps({str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(files)}, indent=2)+'\n')
(out/'base-commit.txt').write_text(subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True))
PYHASH
export Z3_TRIPWIRE_MARKER="$OUT/z3-invoked.txt"
mkdir "$OUT/tripwire-bin"
cp conformance/veryl-proof/tools/z3-tripwire.sh "$OUT/tripwire-bin/z3"
export PATH="$OUT/tripwire-bin:$PATH"
export Z3_BIN="$OUT/tripwire-bin/z3"
export HWVERIFY_SOLVER=finite
cargo test --release --locked -p hwverify-sir
python3 -m unittest discover -s "$HERE/tests" -v
python3 "$HERE/run.py" --out "$OUT/cases" --frontend "$FRONTEND" --lifter "$REPO/target/release/hwverify-sir-lift"
test ! -e "$Z3_TRIPWIRE_MARKER"
