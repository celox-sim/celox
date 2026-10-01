#!/usr/bin/env bash
# One local/CI entrypoint. Generated sources and evidence are never trusted input.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
OUT=${1:-"$REPO/target/veryl-conformance"}
mkdir -p "$(dirname "$OUT")"
if test -e "$OUT"; then echo "Choose a fresh evidence directory: $OUT" >&2; exit 2; fi
mkdir "$OUT"
OUT=$(cd "$OUT" && pwd)
exec > >(tee "$OUT/run.log") 2>&1
finish() {
    status=$?
    if test -e "$OUT/z3-invoked.txt"; then status=1; fi
    python3 - "$OUT" "$status" <<'PY'
import json, pathlib, sys
out = pathlib.Path(sys.argv[1]); code = int(sys.argv[2])
(out/'run-status.json').write_text(json.dumps({'status':'passed' if code == 0 else 'failed', 'exit_code':code, 'external_solver_invoked':(out/'z3-invoked.txt').exists()}, indent=2)+'\n')
PY
    if test -n "${FETCHED_UPSTREAM:-}"; then rm -rf "$FETCHED_UPSTREAM"; fi
    trap - EXIT
    exit "$status"
}
trap finish EXIT
python3 - <<'PY'
import sys
if not __debug__:
    raise SystemExit('Run without PYTHONOPTIMIZE: source mutation checks must execute')
if sys.version_info[:2] != (3, 12):
    raise SystemExit('This conformance run is pinned to Python 3.12')
PY
rustc --version | tee "$OUT/rustc-version.txt"
cargo --version | tee "$OUT/cargo-version.txt"
python3 --version | tee "$OUT/python-version.txt"
if ! rustc --version | grep -q '^rustc 1\.98\.1 '; then
    echo 'Install Rust 1.98.1 or invoke this script with its toolchain on PATH' >&2; exit 2
fi
REV=124a1315096d21b85d9d0d84fd7139363a181cad
if test -z "${CELOX_SUITE_ROOT:-}"; then
    UPSTREAM=$(mktemp -d "${TMPDIR:-/tmp}/hwverify-veryl-upstream.XXXXXXXX")
    FETCHED_UPSTREAM="$UPSTREAM"
    git init -q "$UPSTREAM"
    git -C "$UPSTREAM" -c advice.detachedHead=false fetch --depth=1 https://github.com/celox-sim/celox.git "$REV"
    git -C "$UPSTREAM" -c advice.detachedHead=false checkout --detach FETCH_HEAD
    export CELOX_SUITE_ROOT="$UPSTREAM/crates/celox-test-suite-veryl"
else
    CELOX_SUITE_ROOT=$(cd "$CELOX_SUITE_ROOT" && pwd)
    export CELOX_SUITE_ROOT
    UPSTREAM=$(cd "$CELOX_SUITE_ROOT/../.." && pwd)
fi
test "$(git -C "$UPSTREAM" rev-parse HEAD)" = "$REV"
test -z "$(git -C "$UPSTREAM" status --porcelain --untracked-files=no)"
printf '%s\n' "$REV" > "$OUT/upstream-revision.txt"
# Match the independent analyzer's local metadata patch to the same pinned tree.
python3 - "$CELOX_SUITE_ROOT" "$HERE" <<'PY'
import pathlib, shutil, sys
source=pathlib.Path(sys.argv[1]).parents[1]/'vendor/veryl-metadata'
dest=pathlib.Path(sys.argv[2])/'metadata-snapshot'
if not (source/'Cargo.toml').is_file(): raise SystemExit('missing pinned Veryl metadata patch')
if dest.exists(): shutil.rmtree(dest)
shutil.copytree(source,dest)
PY
cd "$HERE"
python3 generate.py
# --locked forbids resolving newer dependencies. These are independent workspaces.
cargo build --release --locked --manifest-path "$REPO/Cargo.toml" --target-dir "$REPO/target"
cargo build --locked --manifest-path analyzer-probe/Cargo.toml --target-dir "$HERE/analyzer-probe/target"
cargo build --locked --manifest-path Cargo.toml --target-dir "$HERE/target" -p extract-suite
export HWVERIFY_BIN="$REPO/target/release/hwverify-rs"
export VERYL_PROBE_BIN="$HERE/analyzer-probe/target/debug/veryl-analyzer-probe"
python3 - "$OUT" "$HWVERIFY_BIN" "$VERYL_PROBE_BIN" <<'PY'
import hashlib, json, pathlib, sys
out=pathlib.Path(sys.argv[1])
(out/'executables.json').write_text(json.dumps({pathlib.Path(p).name:hashlib.sha256(pathlib.Path(p).read_bytes()).hexdigest() for p in sys.argv[2:]},indent=2)+'\n')
PY
# Exercise the executable tripwire, then arm a separate marker for the real run.
export Z3_TRIPWIRE_MARKER="$OUT/tripwire-smoke.txt"
set +e
"$HERE/tools/z3-tripwire.sh" --version
tripwire_status=$?
set -e
test "$tripwire_status" -eq 99
test -s "$Z3_TRIPWIRE_MARKER"
export Z3_TRIPWIRE_MARKER="$OUT/z3-invoked.txt"
mkdir "$OUT/tripwire-bin"
cp "$HERE/tools/z3-tripwire.sh" "$OUT/tripwire-bin/z3"
export PATH="$OUT/tripwire-bin:$PATH"
export HWVERIFY_SOLVER=finite
python3 extract.py --out "$HERE/traces.json"
cp traces.json "$OUT/traces.json"
cp provenance.json "$OUT/provenance.json"
cargo test --locked --manifest-path Cargo.toml --target-dir "$HERE/target" -p celox-test-suite-veryl --lib
cargo fmt --manifest-path Cargo.toml -p capture-macros -p extract-suite --check
cargo fmt --manifest-path analyzer-probe/Cargo.toml --check
rustfmt --edition 2024 --check --config skip_children=true capture_runtime.rs capture_tests.rs
python3 -m unittest -v test_finite_trace_check test_analyzer_lower test_coverage_gate
python3 catalog.py --out "$OUT/verification"
python3 test_source_controls.py --out "$OUT/source-controls"
test ! -e "$Z3_TRIPWIRE_MARKER"
echo 'All expected Veryl traces and negative controls passed; no external solver invoked'
