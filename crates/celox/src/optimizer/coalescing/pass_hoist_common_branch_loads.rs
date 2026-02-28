use crate::ir::{ExecutionUnit, RegionedAbsoluteAddr};
use crate::optimizer::PassOptions;

use super::pass_manager::ExecutionUnitPass;
use super::shared::hoist_common_branch_loads;

pub(super) struct HoistCommonBranchLoadsPass;

impl ExecutionUnitPass for HoistCommonBranchLoadsPass {
    fn name(&self) -> &'static str {
        "hoist_common_branch_loads"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        hoist_common_branch_loads(eu);
    }
}
