#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
campaign="${COUNTEREXAMPLE_AUDIT_CAMPAIGN_OUT:?Choose a fresh campaign output directory}"
if [[ -e "$campaign" ]]; then
  echo "Refusing to overwrite previous evidence: $campaign" >&2
  exit 1
fi
mkdir -p "$campaign/source"
cp Cargo.toml Cargo.lock "$campaign/source/"
cp -a crates "$campaign/source/"
solver_root="$(cd "$campaign/source" && pwd)"
sha256sum "$solver_root"/crates/solver/src/{finite,z3,quantified}.rs > "$campaign/frozen-source-hashes.txt"
FINITE_AUDIT_SOLVER_ROOT="$solver_root" FINITE_AUDIT_OUT="$campaign/prior-core" \
  audit/finite_speed_independent/run_core.sh
BRANCH_AUDIT_SOLVER_ROOT="$solver_root" BRANCH_AUDIT_OUT="$campaign/mux-alias-core" \
  audit/branch_solver_scaling_independent/run_core.sh
# The audit-owned decomposition copy changes only its final timing-path coverage
# assertion: a completed original-query probe can now legitimately avoid every
# branch. All original formulas, independent verdicts and budget checks remain.
COUNTEREXAMPLE_INCLUDE_DECOMPOSITION=1 COUNTEREXAMPLE_AUDIT_SOLVER_ROOT="$solver_root" COUNTEREXAMPLE_AUDIT_OUT="$campaign/scheduling-core" \
  audit/counterexample_search_independent/run_core.sh
python3 audit/counterexample_search_independent/run_slice_state.py \
  --source "$solver_root" --output "$campaign/slice-state"
for part in mux-alias-core scheduling-core; do
  python3 audit/branch_solver_decomposition_independent/recheck_core_smt.py \
    --input "$campaign/$part" --z3 "${Z3:-../recovered/tools/bin/z3}"
done
sha256sum --check "$campaign/frozen-source-hashes.txt" > "$campaign/frozen-source-hashes-verified.txt"
