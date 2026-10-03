#!/usr/bin/env bash
# Additional acceptance gate. The existing run_ci.sh remains mandatory.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
OUT=${1:?Choose a fresh evidence directory}
OUT=$(realpath -m "$OUT")
case "$OUT/" in "$REPO/"*) echo "Evidence must be outside the source repository" >&2; exit 2;; esac
if test -e "$OUT"; then echo "Evidence directory already exists: $OUT" >&2; exit 2; fi
mkdir -p "$OUT"; OUT=$(cd "$OUT" && pwd)
exec > >(tee "$OUT/run.log") 2>&1
finish() {
    status=$?
    if test -e "$OUT/z3-invoked.txt"; then status=1; fi
    python3 - "$OUT" "$status" <<'PY'
import json, pathlib, sys
out=pathlib.Path(sys.argv[1])
(out/'shell-status.json').write_text(json.dumps({'status':'passed' if sys.argv[2]=='0' else 'failed','exit_code':int(sys.argv[2]),'external_solver_invoked':(out/'z3-invoked.txt').exists()},indent=2)+'\n')
PY
    trap - EXIT; exit "$status"
}
trap finish EXIT
cd "$REPO"
test -x conformance/veryl-proof/target/debug/veryl-proof-frontend
cp conformance/veryl-proof/work/provenance.json "$OUT/frontend-provenance.json"
cargo build --release --locked -p hwverify-rs -p hwverify-sir
export Z3_TRIPWIRE_MARKER="$OUT/z3-invoked.txt"
mkdir "$OUT/tripwire-bin"
cp conformance/veryl-proof/tools/z3-tripwire.sh "$OUT/tripwire-bin/z3"
export Z3_BIN="$OUT/tripwire-bin/z3"
export PATH="$OUT/tripwire-bin:$PATH"
export HWVERIFY_SOLVER=finite
python3 -m unittest audit.veryl_scaling.test_rv32i_isa audit.veryl_scaling.test_rv32i_spec audit.veryl_scaling.test_rv32i_normalization audit.veryl_scaling.test_rv32i_split_refinement audit.veryl_scaling.test_rv32i_selected_ci audit.veryl_scaling.test_rv32i_memory.ContractTests audit.veryl_scaling.test_rv32i_latency audit.veryl_scaling.test_rv32i_latency_onehot_candidate audit.veryl_scaling.test_rv32i_latency_shared
python3 -m audit.veryl_scaling.rv32i_selected_ci --out "$OUT/selected-proof"
test ! -e "$Z3_TRIPWIRE_MARKER"
