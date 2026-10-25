//! Target-independent building blocks shared by native machine-code backends.

pub mod regalloc;

pub mod cranelift_options;

/// The compile label of a generated derived-edge unit, which runs periods
/// of its event in the tick loop.
pub const DERIVED_EDGE_LABEL: &str = "derived_edge";

/// Whether a function compiled under `label` runs in the tick loop when the
/// backend's native tick loop is on: the fused tick (`eval_comb_apply_ff`)
/// and the derived-edge units.
pub fn loops_ticks(label: &str) -> bool {
    label == "eval_comb_apply_ff" || label == DERIVED_EDGE_LABEL
}
