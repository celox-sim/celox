#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
mkdir -p results/finite_solver_independent
sha256sum ../crates/hwverify-solver/src/finite.rs ../crates/hwverify-solver/src/z3.rs ../crates/hwverify-solver/src/quantified.rs audit/finite_solver_independent/src/*.rs > results/finite_solver_independent/source-hashes.txt
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/hwverify-finite-independent-target}"
cargo run --offline --locked --release --manifest-path audit/finite_solver_independent/Cargo.toml 2>&1 | tee results/finite_solver_independent/run.log
sha256sum --check results/finite_solver_independent/source-hashes.txt > results/finite_solver_independent/source-hashes-verified.txt
