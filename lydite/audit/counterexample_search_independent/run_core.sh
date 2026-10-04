#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
export COUNTEREXAMPLE_AUDIT_OUT="${COUNTEREXAMPLE_AUDIT_OUT:?Choose a fresh output directory}"
solver_root="${COUNTEREXAMPLE_AUDIT_SOLVER_ROOT:-$PWD}"
if [[ -e "$COUNTEREXAMPLE_AUDIT_OUT" ]]; then
  echo "Refusing to overwrite previous evidence: $COUNTEREXAMPLE_AUDIT_OUT" >&2
  exit 1
fi
mkdir -p "$COUNTEREXAMPLE_AUDIT_OUT/audit-source" "$COUNTEREXAMPLE_AUDIT_OUT/build-harness"
cp audit/counterexample_search_independent/src/*.rs "$COUNTEREXAMPLE_AUDIT_OUT/audit-source/"
python3 - "$solver_root" "$COUNTEREXAMPLE_AUDIT_OUT/build-harness" <<'PY'
import json,sys
from pathlib import Path
root=Path(sys.argv[1]).resolve();out=Path(sys.argv[2])
source=(out.parent/'audit-source/main.rs').resolve()
s=Path('audit/counterexample_search_independent/Cargo.toml').read_text().replace('"../../crates/',json.dumps(str(root/'crates'))[:-1]+'/')
s+='\n[[bin]]\nname="counterexample-search-independent-audit"\npath='+json.dumps(str(source))+'\n'
(out/'Cargo.toml').write_text(s)
PY
manifest="$COUNTEREXAMPLE_AUDIT_OUT/build-harness/Cargo.toml"
cargo generate-lockfile --offline --manifest-path "$manifest"
sha256sum "$solver_root"/../crates/lydite-solver/src/{finite,z3,quantified}.rs "$COUNTEREXAMPLE_AUDIT_OUT"/audit-source/*.rs "$manifest" > "$COUNTEREXAMPLE_AUDIT_OUT/source-hashes.txt"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/lydite-counterexample-independent-target}"
cargo run --offline --locked --release --manifest-path "$manifest" 2>&1 | tee "$COUNTEREXAMPLE_AUDIT_OUT/run.log"
sha256sum --check "$COUNTEREXAMPLE_AUDIT_OUT/source-hashes.txt" > "$COUNTEREXAMPLE_AUDIT_OUT/source-hashes-verified.txt"
