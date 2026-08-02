use super::*;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

mod builder;
mod cache;
mod diagnostics;
mod late;
pub(in crate::optimizer) mod pass_manager;
mod runner;

use pass_manager::ExecutionUnitPassManager;

pub(crate) fn run(program: &mut OptimizationContext<'_>, options: &PassOptions) {
    runner::optimize_with_options(
        program,
        options.max_inflight_loads,
        options.four_state,
        &options.optimize_options,
        options.preserve_element_storage_layout,
    );
}
