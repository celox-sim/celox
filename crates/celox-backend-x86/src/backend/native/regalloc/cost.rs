//! Shared target costs for scheduling and spilling.
//!
//! `SpillDesc` supplies the target's persistent stack/state costs, while
//! `PlanningRecipes` may prove a cheaper globally or point-valid
//! reconstruction.  Keep that interpretation in one place so scheduling and
//! spill placement cannot silently assign different prices to the same
//! machine value.

use crate::native::mir::{MFunction, VReg};

use super::reload::{EdgeUse, PlanningRecipes, PointUse};

pub(super) struct MachineSpillCosts<'a> {
    func: &'a MFunction,
    recipes: Option<&'a PlanningRecipes>,
}

impl<'a> MachineSpillCosts<'a> {
    pub(super) fn from_descriptors(func: &'a MFunction) -> Self {
        Self {
            func,
            recipes: None,
        }
    }

    pub(super) fn with_recipes(func: &'a MFunction, recipes: &'a PlanningRecipes) -> Self {
        Self {
            func,
            recipes: Some(recipes),
        }
    }

    pub(super) fn spill(&self, value: VReg) -> u16 {
        self.func
            .spill_desc(value)
            .map_or(u16::MAX, |desc| u16::from(desc.spill_cost))
    }

    pub(super) fn persistent_reload(&self, value: VReg) -> u16 {
        self.recipes
            .and_then(|recipes| recipes.global_materialization_cost(value))
            .unwrap_or_else(|| {
                self.func
                    .spill_desc(value)
                    .map_or(u16::MAX, |desc| u16::from(desc.reload_cost))
            })
    }

    pub(super) fn reload_at_point(&self, point: PointUse) -> u16 {
        self.recipes
            .and_then(|recipes| recipes.materialization_cost_at_point(point))
            .unwrap_or_else(|| self.persistent_reload(point.value))
    }

    pub(super) fn reload_on_edge(&self, edge: EdgeUse) -> u16 {
        self.recipes
            .and_then(|recipes| recipes.materialization_cost_on_edge(edge))
            .unwrap_or_else(|| self.persistent_reload(edge.value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::mir::{SpillDesc, VRegAllocator};

    #[test]
    fn descriptors_and_global_recipes_share_one_cost_model() {
        let descriptors = vec![
            SpillDesc::transient(),
            SpillDesc::remat(7),
            SpillDesc::transient(),
        ];
        let mut vregs = VRegAllocator::new();
        for _ in 0..3 {
            vregs.alloc();
        }
        let func = MFunction::new(vregs, descriptors);
        let base = MachineSpillCosts::from_descriptors(&func);

        assert_eq!(base.spill(VReg(0)), 2);
        assert_eq!(base.persistent_reload(VReg(0)), 2);
        assert_eq!(base.spill(VReg(1)), 0);
        assert_eq!(base.persistent_reload(VReg(1)), 1);

        let recipes = PlanningRecipes::with_global_costs(vec![None, None, Some(1)]);
        let planned = MachineSpillCosts::with_recipes(&func, &recipes);
        assert_eq!(planned.persistent_reload(VReg(2)), 1);
        assert_eq!(planned.spill(VReg(2)), 2);
    }
}
