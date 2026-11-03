use crate::PassOptions;
use crate::ir::{ExecutionUnit, RegionedAbsoluteAddr};

use super::pass_manager::ExecutionUnitPass;
use super::shared::hoist_common_branch_loads;

pub(in crate::optimizer) struct HoistCommonBranchLoadsPass;

impl ExecutionUnitPass for HoistCommonBranchLoadsPass {
    fn name(&self) -> &'static str {
        "hoist_common_branch_loads"
    }

    fn handles_calls(&self) -> bool {
        true
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        hoist_common_branch_loads(eu);
    }
}
