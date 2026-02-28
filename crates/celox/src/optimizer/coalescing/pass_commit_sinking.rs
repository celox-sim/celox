use crate::ir::{ExecutionUnit, RegionedAbsoluteAddr};
use crate::optimizer::PassOptions;

use super::commit_ops::optimize_commit_sinking;
use super::pass_manager::ExecutionUnitPass;

pub(super) struct CommitSinkingPass;

impl ExecutionUnitPass for CommitSinkingPass {
    fn name(&self) -> &'static str {
        "optimize_commit_sinking"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        optimize_commit_sinking(eu);
    }
}
