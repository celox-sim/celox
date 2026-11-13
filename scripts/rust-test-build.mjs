// The Cargo build of CI's Rust tests. scripts/run-vtest.py builds the vtest
// executor with the same profile, packages and features, so the workspace
// crates compile once for both (scripts/ci-workflow.test.mjs checks that the
// two agree).
export const runtimeCasesFeature = "celox-test-suite/runtime-cases";
// CI's full Rust test build: workspace members and features of the
// "Run workspace tests with CI features" step and its cocotb rerun.
export const workspaceFeatures = "celox/cranelift,celox/wasm-runtime,celox/wasm-component,celox-bench/cranelift,celox-test-suite/verilator,celox-test-suite/icarus,celox-test-suite/xezim,celox-test-suite/runtime-cases";
export const testProfile = "vtest";
