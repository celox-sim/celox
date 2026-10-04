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
cargo build --release --locked -p lydite -p lydite-sir
cargo fmt --check -p lydite-ir -p lydite -p lydite-sir -p lydite-solver -p lydite-syntax -p lydite-verify
python3 - "$OUT" "$REPO" "$FRONTEND" <<'PYHASH'
import hashlib, json, pathlib, subprocess, sys
out, root, frontend = map(pathlib.Path, sys.argv[1:])
# The Rust crates live in the repository-root Cargo workspace.
repo = root.parent
files = [repo/'Cargo.toml', repo/'Cargo.lock', frontend,
         root/'../target/release/lydite', root/'../target/release/lydite-sir-lift']
for directory in ('crates/lydite-ir', 'crates/lydite-solver', 'crates/lydite-verify', 'crates/lydite-syntax',
                  'crates/lydite', 'crates/lydite-sir', 'lydite/conformance/veryl-symbolic'):
    files.extend(p for p in (repo/directory).rglob('*') if p.is_file()
                 and '__pycache__' not in p.parts and p.suffix != '.pyc')
files.extend((root/'audit/veryl_scaling').glob('*.py'))
files.extend(root/p for p in ('examples/build_pipeline.py', 'examples/build_branch_pipeline.py', 'audit/interpreter.py'))
(out/'tested-files.json').write_text(json.dumps({str(p.relative_to(repo)): hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(files)}, indent=2)+'\n')
(out/'base-commit.txt').write_text(subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True))
PYHASH
export Z3_TRIPWIRE_MARKER="$OUT/z3-invoked.txt"
mkdir "$OUT/tripwire-bin"
cp conformance/veryl-proof/tools/z3-tripwire.sh "$OUT/tripwire-bin/z3"
export PATH="$OUT/tripwire-bin:$PATH"
export Z3_BIN="$OUT/tripwire-bin/z3"
export LYDITE_SOLVER=finite
cargo test --release --locked -p lydite-sir -p lydite-solver
python3 -m unittest discover -s "$HERE/tests" -v
python3 "$HERE/run.py" --out "$OUT/cases" --frontend "$FRONTEND" --lifter "$REPO/../target/release/lydite-sir-lift"
python3 "$REPO/audit/veryl_scaling/measure.py" --out "$OUT/scaling-regression" --sizes 16 32 64 --require-success
python3 "$REPO/audit/veryl_scaling/cpu_capacity.py" --out "$OUT/cpu-capacity-regression" --sizes 8 16 32 64 --faults no_flush wrong_target no_forward no_interlock wrong_add missing_reset --require-success
python3 "$REPO/audit/veryl_scaling/cpu_registers.py" --out "$OUT/register-scaling-regression" --sizes 8 16 32 --require-success
python3 "$REPO/audit/veryl_scaling/test_cpu_registers.py" --evidence "$OUT/register-scaling-regression"
python3 "$REPO/audit/veryl_scaling/cpu_memory_contract.py" --out "$OUT/memory-contract-regression" --require-success
python3 "$REPO/audit/veryl_scaling/test_cpu_memory_contract.py" --evidence "$OUT/memory-contract-regression"
python3 -m unittest audit.veryl_scaling.test_cpu_memory_reuse
python3 "$REPO/audit/veryl_scaling/test_readonly_array_differential.py" --root "$REPO" --out "$OUT/readonly-array-differential"
python3 "$REPO/audit/veryl_scaling/cpu_memory_reuse.py" --out "$OUT/memory-reuse-regression" --negative-controls
test ! -e "$Z3_TRIPWIRE_MARKER"
# STORE milestone: retain all prior gates, add writable array/CPU/memory proofs.
python3 -m unittest audit.veryl_scaling.test_cpu_store_reuse audit.veryl_scaling.test_cpu_writable_memory
python3 "$REPO/audit/veryl_scaling/test_array_store_differential.py" --root "$REPO" --out "$OUT/array-store-differential"
python3 "$REPO/audit/veryl_scaling/cpu_store_reuse.py" --out "$OUT/store-memory-reuse-regression" --negative-controls
python3 "$REPO/audit/veryl_scaling/test_cpu_writable_memory.py" --evidence "$OUT/store-memory-reuse-regression"
test ! -e "$Z3_TRIPWIRE_MARKER"
