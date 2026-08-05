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
    // TailCallSplit is a backend selector, not a SIR transform. Skip the
    // optional hashing/cloning pipeline when no actual SIR pass is active,
    // but retain correctness-required canonicalization.
    if !options.optimize_options.any_enabled() {
        runner::canonicalize_required(program);
        return;
    }
    runner::optimize_with_options(
        program,
        options.max_inflight_loads,
        options.four_state,
        &options.optimize_options,
        options.preserve_element_storage_layout,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OptLevel, OptimizeOptions};
    use celox_design::StateObjectId;

    fn unit(
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut blocks = crate::HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Return,
            },
        );
        ExecutionUnit {
            blocks,
            entry_block_id: BlockId(0),
            register_map: crate::HashMap::default(),
        }
    }

    #[test]
    fn o0_keeps_sparse_commit_after_all_event_evaluators() {
        let event = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: StateObjectId(0),
        };
        let sparse = RegionedAbsoluteAddr::from_absolute_addr(SPARSE_WORKING_REGION, event);
        let stable = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, event);
        let commit = SIRInstruction::Commit(sparse, stable, SIROffset::Static(0), 8, Vec::new());
        let mut sir = SirProgram {
            eval_comb: Vec::new(),
            eval_apply_ffs: [(event, vec![unit(vec![commit.clone()]), unit(Vec::new())])]
                .into_iter()
                .collect(),
            eval_comb_apply_ffs: crate::HashMap::default(),
            eval_only_ffs: crate::HashMap::default(),
            apply_ffs: crate::HashMap::default(),
        };
        let design = celox_design::ElaboratedDesign::default();
        let runtime_schema = celox_design::RuntimeSchema::default();
        let mut layout_requirements = celox_state_layout::LayoutRequirements::default();
        let mut context = OptimizationContext {
            sir: &mut sir,
            design: &design,
            runtime_schema: &runtime_schema,
            layout_requirements: &mut layout_requirements,
        };

        run(
            &mut context,
            &PassOptions {
                optimize_options: OptimizeOptions::new(OptLevel::O0),
                ..PassOptions::default()
            },
        );

        let units = &context.sir.eval_apply_ffs[&event];
        assert_eq!(units.len(), 3);
        assert!(units[..2].iter().all(|unit| {
            unit.blocks
                .values()
                .all(|block| !block.instructions.contains(&commit))
        }));
        assert_eq!(units[2].blocks[&BlockId(0)].instructions, vec![commit]);
    }
}
