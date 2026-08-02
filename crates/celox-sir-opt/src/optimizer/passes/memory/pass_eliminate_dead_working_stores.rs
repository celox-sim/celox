use crate::PassOptions;
use crate::ir::{ExecutionUnit, RegionedAbsoluteAddr};

use super::dead_working_stores::eliminate_dead_working_stores;
use super::pass_manager::ExecutionUnitPass;

pub(in crate::optimizer) struct EliminateDeadWorkingStoresPass;

impl ExecutionUnitPass for EliminateDeadWorkingStoresPass {
    fn name(&self) -> &'static str {
        "eliminate_dead_working_stores"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        eliminate_dead_working_stores(eu);
    }
}
