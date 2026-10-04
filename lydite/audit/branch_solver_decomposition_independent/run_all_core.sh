#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
campaign="${BRANCH_AUDIT_CAMPAIGN_OUT:-results/branch_solver_decomposition_independent/rerun}"
if [[ -e "$campaign" ]]; then
  echo "Choose a fresh BRANCH_AUDIT_CAMPAIGN_OUT; preserving existing evidence: $campaign" >&2
  exit 1
fi
mkdir -p "$campaign/source"
cp Cargo.toml Cargo.lock "$campaign/source/"
cp -a crates "$campaign/source/"
solver_root="$(cd "$campaign/source" && pwd)"
FINITE_AUDIT_SOLVER_ROOT="$solver_root" FINITE_AUDIT_OUT="$campaign/prior-core" \
  audit/finite_speed_independent/run_core.sh
BRANCH_AUDIT_SOLVER_ROOT="$solver_root" BRANCH_AUDIT_OUT="$campaign/mux-alias-core" \
  audit/branch_solver_scaling_independent/run_core.sh
BRANCH_AUDIT_SOLVER_ROOT="$solver_root" BRANCH_AUDIT_OUT="$campaign/decomposition-core" \
  audit/branch_solver_decomposition_independent/run_core.sh
for part in mux-alias-core decomposition-core; do
  python3 audit/branch_solver_decomposition_independent/recheck_core_smt.py \
    --input "$campaign/$part" --z3 "${Z3:-../recovered/tools/bin/z3}"
done
