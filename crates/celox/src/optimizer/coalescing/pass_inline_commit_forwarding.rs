use crate::ir::{ExecutionUnit, RegionedAbsoluteAddr};
use crate::optimizer::PassOptions;

use super::commit_ops::inline_commit_forwarding;
use super::pass_manager::ExecutionUnitPass;

pub(super) struct InlineCommitForwardingPass;

impl ExecutionUnitPass for InlineCommitForwardingPass {
    fn name(&self) -> &'static str {
        "inline_commit_forwarding"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        inline_commit_forwarding(eu);
    }
}
