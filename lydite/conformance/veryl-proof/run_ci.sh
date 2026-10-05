#!/usr/bin/env bash
# Reproducible proof-backed conformance of the in-tree suite; no prebuilt binary or oracle.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
OUT=${1:-"$HERE/reproduced"}
mkdir -p "$(dirname "$OUT")"
if test -e "$OUT"; then echo "Choose a fresh evidence directory: $OUT" >&2; exit 2; fi
mkdir "$OUT"; OUT=$(cd "$OUT" && pwd)
exec > >(tee "$OUT/run.log") 2>&1
finish() {
    status=$?
    if test -e "$OUT/z3-invoked.txt"; then status=1; fi
    python3 - "$OUT" "$status" <<'PY'
import json,pathlib,sys
out=pathlib.Path(sys.argv[1]);code=int(sys.argv[2])
(out/'run-status.json').write_text(json.dumps({'status':'passed' if code==0 else 'failed','exit_code':code,'external_solver_invoked':(out/'z3-invoked.txt').exists()},indent=2)+'\n')
PY
    if test -n "${GITHUB_STEP_SUMMARY:-}"; then
        if test -f "$OUT/coverage/summary.md"; then cat "$OUT/coverage/summary.md" >> "$GITHUB_STEP_SUMMARY"; fi
        printf '\nWhole job exit status: %s\n' "$status" >> "$GITHUB_STEP_SUMMARY"
    fi
    trap - EXIT; exit "$status"
}
trap finish EXIT
python3 - <<'PY'
import sys
if not __debug__:raise SystemExit('PYTHONOPTIMIZE is forbidden: audit assertions must run')
if sys.version_info[:2]!=(3,12):raise SystemExit('Use Python3.12')
PY
rustc --version | tee "$OUT/rustc-version.txt"
cargo --version | tee "$OUT/cargo-version.txt"
python3 --version | tee "$OUT/python-version.txt"
cd "$HERE"
python3 prepare.py
cp work/provenance.json "$OUT/provenance.json"
# The exporter and suite runner build from the in-tree Celox.
cargo build --locked -p lydite-celox
cargo build --release --locked -p lydite --bin lydite-finite-service
cargo fmt --check -p lydite-celox -p lydite
export SIR_EXPORTER_BIN="$REPO/../target/debug/lydite-celox-export"
export PYTHONPATH="$HERE"
export XDG_CACHE_HOME="$HERE/work/cache"
python3 - "$OUT" "$SIR_EXPORTER_BIN" "$REPO/../target/debug/lydite-celox-suite" "$REPO/../target/release/lydite-finite-service" <<'PY'
import hashlib,json,pathlib,sys
out=pathlib.Path(sys.argv[1]);(out/'executables.json').write_text(json.dumps({pathlib.Path(p).name:hashlib.sha256(pathlib.Path(p).read_bytes()).hexdigest() for p in sys.argv[2:]},indent=2)+'\n')
PY
export Z3_TRIPWIRE_MARKER="$OUT/tripwire-smoke.txt"
set +e
"$HERE/tools/z3-tripwire.sh" --version
code=$?
set -e
test "$code" -eq 99; test -s "$Z3_TRIPWIRE_MARKER"
export Z3_TRIPWIRE_MARKER="$OUT/z3-invoked.txt"
mkdir "$OUT/tripwire-bin"; cp "$HERE/tools/z3-tripwire.sh" "$OUT/tripwire-bin/z3"
export PATH="$OUT/tripwire-bin:$PATH"
python3 -m unittest discover -s tests -v 2>&1 | tee "$OUT/unit-tests.log"
python3 coverage.py --out "$OUT/coverage"
python3 source_controls.py --out "$OUT/source-controls"
for matrix in formal_width dimensions numeric_casts; do
    python3 "$REPO/audit/veryl_proof_independent/check_${matrix}.py" --exporter "$SIR_EXPORTER_BIN" --out "$OUT/audit-$matrix"
done
python3 "$REPO/audit/veryl_proof_independent/check_wide_encoding.py" --module-dir "$HERE" --out "$OUT/audit-wide"
python3 replay_audit.py "$OUT/coverage/raw/advanced_interface__test_interface_bidirectional/design-1/proof" | tee "$OUT/query-replay.json"
test ! -e "$Z3_TRIPWIRE_MARKER"
python3 -c "import json,sys;s=json.load(open(sys.argv[1]));print(f\"{s['actual_passes']}/{s['total']} actual passes; {s['raw_failures']} recorded exceptions. Coverage contract satisfied.\")" "$OUT/coverage/coverage-summary.json"
