#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
export BRANCH_AUDIT_OUT="${BRANCH_AUDIT_OUT:-results/branch_solver_decomposition_independent/current-core}"
solver_root="${BRANCH_AUDIT_SOLVER_ROOT:-$PWD}"
mkdir -p "$BRANCH_AUDIT_OUT/audit-source"
cp audit/branch_solver_decomposition_independent/src/*.rs "$BRANCH_AUDIT_OUT/audit-source/"
cp audit/finite_speed_independent/src/semantics.rs "$BRANCH_AUDIT_OUT/audit-source/"
manifest=audit/branch_solver_decomposition_independent/Cargo.toml
if [[ "$solver_root" != "$PWD" ]]; then
  mkdir -p "$BRANCH_AUDIT_OUT/build-harness"
  python3 - "$solver_root" "$BRANCH_AUDIT_OUT/build-harness" <<'PY'
import json,sys
from pathlib import Path
root=Path(sys.argv[1]).resolve();out=Path(sys.argv[2]);source=Path('audit/branch_solver_decomposition_independent/src/main.rs').resolve()
s=Path('audit/branch_solver_decomposition_independent/Cargo.toml').read_text().replace('"../../crates/', json.dumps(str(root/'crates'))[:-1]+'/')
s+='\n[[bin]]\nname="branch-solver-decomposition-independent-audit"\npath='+json.dumps(str(source))+'\n'
(out/'Cargo.toml').write_text(s)
(out/'Cargo.lock').write_text(Path('audit/branch_solver_decomposition_independent/Cargo.lock').read_text())
PY
  manifest="$BRANCH_AUDIT_OUT/build-harness/Cargo.toml"
  cargo generate-lockfile --offline --manifest-path "$manifest"
fi
sha256sum "$solver_root"/crates/solver/src/{finite,z3,quantified}.rs audit/branch_solver_decomposition_independent/src/*.rs audit/finite_speed_independent/src/semantics.rs "$manifest" > "$BRANCH_AUDIT_OUT/source-hashes.txt"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/hwverify-branch-decomposition-independent-target}"
cargo run --offline --locked --release --manifest-path "$manifest" 2>&1 | tee "$BRANCH_AUDIT_OUT/run.log"
sha256sum --check "$BRANCH_AUDIT_OUT/source-hashes.txt" > "$BRANCH_AUDIT_OUT/source-hashes-verified.txt"
