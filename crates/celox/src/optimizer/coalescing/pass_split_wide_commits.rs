use crate::ir::{ExecutionUnit, RegionedAbsoluteAddr};
use crate::optimizer::PassOptions;

use super::commit_ops::split_wide_commits;
use super::pass_manager::ExecutionUnitPass;

pub(super) struct SplitWideCommitsPass;

impl ExecutionUnitPass for SplitWideCommitsPass {
    fn name(&self) -> &'static str {
        "split_wide_commits"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        split_wide_commits(eu);
    }
}
