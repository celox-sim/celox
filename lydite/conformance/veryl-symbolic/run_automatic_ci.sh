#!/usr/bin/env bash
# Strict fixed automatic-proof coverage; earlier conformance gates stay mandatory.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
OUT=$(realpath -m "${1:?Choose a fresh evidence directory}")
case "$OUT/" in "$REPO/"*) echo "Evidence must be outside the source repository" >&2; exit 2;; esac
if test -e "$OUT"; then echo "Evidence directory already exists: $OUT" >&2; exit 2; fi
mkdir -p "$OUT"
exec > >(tee "$OUT/run.log") 2>&1
finish() {
    status=$?
    if test -e "$OUT/gate/external-solver-invoked.txt"; then status=1; fi
    python3 - "$OUT" "$status" <<'PY'
import json, pathlib, sys
out = pathlib.Path(sys.argv[1])
(out / 'shell-status.json').write_text(json.dumps({
    'status': 'passed' if sys.argv[2] == '0' else 'failed',
    'exit_code': int(sys.argv[2]),
    'external_solver_invoked': (out / 'gate/external-solver-invoked.txt').exists(),
}, indent=2) + '\n')
PY
    trap - EXIT; exit "$status"
}
trap finish EXIT
cd "$REPO"
cargo build --release --locked -p lydite
cargo test --release --locked -p lydite-solver automatic::tests
python3 -m unittest audit.automatic_proof.test_generate audit.automatic_proof.test_ci
python3 -m audit.automatic_proof.ci --out "$OUT/gate"
