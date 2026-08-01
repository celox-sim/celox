use crate::ir::{ExecutionUnit, RegionedAbsoluteAddr};
use crate::optimizer::PassOptions;

use super::block_opt::schedule_instructions;
use super::pass_manager::ExecutionUnitPass;

pub(super) struct ReschedulePass;

impl ExecutionUnitPass for ReschedulePass {
    fn name(&self) -> &'static str {
        "reschedule"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        for block in eu.blocks.values_mut() {
            schedule_instructions(&mut block.instructions, options.max_inflight_loads);
        }
    }
}
