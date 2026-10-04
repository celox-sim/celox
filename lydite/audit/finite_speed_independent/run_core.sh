#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
export FINITE_AUDIT_OUT="${FINITE_AUDIT_OUT:-results/finite_speed_independent/core}"
export FINITE_AUDIT_TARGETED="${FINITE_AUDIT_TARGETED:-1}"
mkdir -p "$FINITE_AUDIT_OUT/audit-source"
cp audit/finite_speed_independent/src/*.rs "$FINITE_AUDIT_OUT/audit-source/"
solver_root="${FINITE_AUDIT_SOLVER_ROOT:-$PWD}"
manifest=audit/finite_speed_independent/Cargo.toml
if [[ "$solver_root" != "$PWD" ]]; then
  mkdir -p "$FINITE_AUDIT_OUT/build-harness"
  python - "$solver_root" "$FINITE_AUDIT_OUT/build-harness" <<'PY'
import json,sys
from pathlib import Path
root=Path(sys.argv[1]).resolve();out=Path(sys.argv[2]);source=Path('audit/finite_speed_independent/src/main.rs').resolve()
s=Path('audit/finite_speed_independent/Cargo.toml').read_text().replace('"../../crates/', json.dumps(str(root/'crates'))[:-1]+'/')
s+='\n[[bin]]\nname="finite-speed-independent-audit"\npath='+json.dumps(str(source))+'\n'
(out/'Cargo.toml').write_text(s)
(out/'Cargo.lock').write_text(Path('audit/finite_speed_independent/Cargo.lock').read_text())
PY
  manifest="$FINITE_AUDIT_OUT/build-harness/Cargo.toml"
  cargo generate-lockfile --offline --manifest-path "$manifest"
fi
sha256sum "$solver_root"/../crates/lydite-solver/src/{finite,z3,quantified}.rs audit/finite_speed_independent/src/*.rs "$manifest" > "$FINITE_AUDIT_OUT/source-hashes.txt"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/lydite-finite-speed-independent-target}"
cargo run --offline --locked --release --manifest-path "$manifest" 2>&1 | tee "$FINITE_AUDIT_OUT/run.log"
sha256sum --check "$FINITE_AUDIT_OUT/source-hashes.txt" > "$FINITE_AUDIT_OUT/source-hashes-verified.txt"
