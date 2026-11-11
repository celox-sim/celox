//! Recover control dependence after expression lowering has eagerly
//! materialized a shared mux arm.
//!
//! A single-output mux pass cannot sink a DAG shared by several guarded
//! outputs: every individual root appears to have external uses.  This pass
//! treats all values owned by one already-existing CFG edge as a region.  It
//! distributes stores whose values are selected by the branch condition and
//! moves the closed, pure true-edge region behind that branch.

use super::pass_manager::ExecutionUnitPass;
use super::shared::{def_reg, normalize_branch_condition};
use super::sir_analysis::{UseSite, collect_uses, instruction_uses, predicate_facts};
use crate::PassOptions;
use crate::ir::cfg::SirCfg;
use crate::ir::*;
use crate::{HashMap, HashSet};
use std::collections::{BTreeMap, VecDeque};

mod coupled_stores;
mod dead_control;
mod deferred_values;
mod edge_plan;
mod live_ranges;
mod same_predicate;
use coupled_stores::*;
pub(in crate::optimizer) use dead_control::*;
use deferred_values::*;
use edge_plan::*;
pub(in crate::optimizer) use live_ranges::*;
use same_predicate::*;

pub(in crate::optimizer) struct GuardedRegionSinkingPass;

/// Recover effect/value regions which become visible only after native EUs
/// have been merged into one CFG.
///
/// This runs the coupled-store, closed same-predicate, guarded scan and direct
/// packed-update planners. Replaying the complete source-EU pass after fusion would also
/// perform unrelated edge sinking and repeated CFG repair.
pub(in crate::optimizer) fn recover_merged_effect_regions(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
) {
    if four_state || (cfg!(debug_assertions) && eu.verify_result().is_err()) {
        return;
    }
    form_coupled_store_regions(eu);
    form_same_predicate_regions(eu);
    super::loop_reduction_guard::run(eu);
    super::packed_index_update::run(eu);
}

#[derive(Clone)]
struct DistributedStore {
    index: usize,
    mux_index: usize,
    mux_result: RegisterId,
    true_value: RegisterId,
    false_value: RegisterId,
}

#[derive(Clone)]
struct GuardedRegionPlan {
    block_id: BlockId,
    condition: RegisterId,
    true_target: (BlockId, Vec<RegisterId>),
    false_target: (BlockId, Vec<RegisterId>),
    selected_true: bool,
    moved: HashSet<usize>,
    distributed: Vec<DistributedStore>,
    removable_muxes: HashSet<usize>,
}

/// A pure value-producing diamond whose sole result is consumed in one later
/// control-dependent block. Moving the complete diamond preserves guards such
/// as divide-by-zero handling which cannot be represented by instruction-only
/// scheduling across the result block parameter.
#[derive(Clone, Copy)]
struct DeferredValueDiamondPlan {
    head: BlockId,
    true_arm: BlockId,
    false_arm: BlockId,
    merge: BlockId,
    condition: RegisterId,
    result: RegisterId,
    true_value: RegisterId,
    false_value: RegisterId,
    use_block: BlockId,
}

#[derive(Clone)]
struct DeferredValueRegionPlan {
    head: BlockId,
    merge: BlockId,
    region: HashSet<BlockId>,
    use_block: BlockId,
    moved_head: HashSet<usize>,
}

/// One reverse-if-conversion of a pure, single-block same-predicate region.
///
/// `true_owned` and `false_owned` are closed backwards slices which are used
/// only by the corresponding Mux arms. `cofactor` is the forward slice which
/// depends on at least one removed Mux result. It is rebuilt once on each edge,
/// so only `live_outs` cross the merge instead of every individual Mux result.
#[derive(Clone)]
struct SamePredicatePlan {
    block_id: BlockId,
    segment_start: usize,
    segment_end: usize,
    condition: RegisterId,
    muxes: HashSet<usize>,
    true_muxes: HashSet<usize>,
    false_muxes: HashSet<usize>,
    true_owned: HashSet<usize>,
    false_owned: HashSet<usize>,
    cofactor: HashSet<usize>,
    true_cofactor: HashSet<usize>,
    false_cofactor: HashSet<usize>,
    live_outs: Vec<RegisterId>,
    net_benefit_scaled: u128,
}

/// Reverse if-conversion for several observable stores selected by the same
/// predicate.  Treating each store independently cannot move a shared arm:
/// the other store makes every shared definition appear to escape.  The
/// stores therefore form one effect region and their arm closures are planned
/// together.
#[derive(Clone)]
struct CoupledStorePlan {
    block_id: BlockId,
    last_store: usize,
    stores: Vec<DistributedStore>,
    conditions: Vec<RegisterId>,
    leaf_values: Vec<Vec<RegisterId>>,
    placements: HashMap<usize, CoupledPlacementSite>,
    removable_muxes: HashSet<usize>,
    net_benefit_scaled: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CoupledPlacementSite {
    Decision(usize),
    Leaf(usize),
}

#[derive(Clone, Copy)]
enum ConditionalArm {
    Value(RegisterId),
    Zero,
}

#[derive(Clone, Copy)]
struct ConditionalSource {
    dst: RegisterId,
    condition: RegisterId,
    true_arm: ConditionalArm,
    false_arm: ConditionalArm,
}

impl ConditionalSource {
    fn selected(self, true_arm: bool) -> ConditionalArm {
        if true_arm {
            self.true_arm
        } else {
            self.false_arm
        }
    }
}

fn conditional_source_conditions(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
) -> Vec<RegisterId> {
    match *inst {
        SIRInstruction::Mux(_, condition, _, _) => vec![condition],
        SIRInstruction::Binary(dst, lhs, BinaryOp::And | BinaryOp::LogicAnd, rhs)
            if eu.register_map.get(&dst).map(RegisterType::width) == Some(1)
                && eu.register_map.get(&lhs).map(RegisterType::width) == Some(1)
                && eu.register_map.get(&rhs).map(RegisterType::width) == Some(1) =>
        {
            if lhs == rhs {
                vec![lhs]
            } else {
                vec![lhs, rhs]
            }
        }
        _ => Vec::new(),
    }
}

fn conditional_source(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    condition: RegisterId,
) -> Option<ConditionalSource> {
    match *inst {
        SIRInstruction::Mux(dst, actual, true_value, false_value) if actual == condition => {
            Some(ConditionalSource {
                dst,
                condition,
                true_arm: ConditionalArm::Value(true_value),
                false_arm: ConditionalArm::Value(false_value),
            })
        }
        SIRInstruction::Binary(dst, lhs, BinaryOp::And | BinaryOp::LogicAnd, rhs)
            if eu.register_map.get(&dst).map(RegisterType::width) == Some(1)
                && eu.register_map.get(&lhs).map(RegisterType::width) == Some(1)
                && eu.register_map.get(&rhs).map(RegisterType::width) == Some(1) =>
        {
            let payload = if lhs == condition {
                rhs
            } else if rhs == condition {
                lhs
            } else {
                return None;
            };
            Some(ConditionalSource {
                dst,
                condition,
                true_arm: ConditionalArm::Value(payload),
                false_arm: ConditionalArm::Zero,
            })
        }
        _ => None,
    }
}

impl ExecutionUnitPass for GuardedRegionSinkingPass {
    fn name(&self) -> &'static str {
        "guarded_region_sinking"
    }

    fn handles_calls(&self) -> bool {
        true
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        // A four-state Mux bitwise-merges its arms for an X/Z condition, while
        // control flow selects one edge. No structural proof below authorizes
        // that conversion in four-state mode.
        if options.four_state || (cfg!(debug_assertions) && eu.verify_result().is_err()) {
            return;
        }
        super::loop_reduction_guard::run(eu);
        let verify_stage = |eu: &ExecutionUnit<RegionedAbsoluteAddr>, stage: &'static str| {
            if options.optimize_options.diagnostics.verify_passes
                && let Err(error) = eu.verify_result()
            {
                panic!("after guarded-region substage {stage}: {error}");
            }
        };

        // Recover one branch for coupled state outputs before ordinary Mux
        // lowering fragments the producer DAG with internal value diamonds.
        // This is especially important for RTL blocks which assign `result`
        // and `flags` under the same priority if/else chain.
        form_coupled_store_regions(eu);
        verify_stage(eu, "coupled stores");

        // First recover a branch shared by all Muxes with the same predicate
        // in a pure SIR region. This is deliberately planned from the input
        // CFG and applies at most one best region per input block. Generated
        // blocks are not candidates in this run, so termination needs neither
        // an iteration limit nor a function-size budget.
        form_same_predicate_regions(eu);
        verify_stage(eu, "same-predicate regions");

        // A case rewrite can leave a guarded value diamond on the common path
        // while its phi result is consumed in only one selected leaf. Move the
        // complete diamond, not merely its arm instructions, so operations
        // such as division retain their original guard. Planning uses one
        // CFG/use snapshot and no path enumeration.
        sink_deferred_value_diamonds(eu);
        verify_stage(eu, "deferred value diamonds");

        // Recompute CFG facts after region formation. The existing edge
        // sinking transform remains independent and can consume either an
        // original branch or a branch introduced above.

        let Ok(cfg) = SirCfg::analyze_forward_structure(eu) else {
            return;
        };
        let uses = collect_uses(eu);
        let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
        block_ids.sort_unstable_by_key(|id| id.0);

        // Plans are built only from the input CFG.  Blocks generated below are
        // intentionally not revisited in this run, which makes termination
        // independent of function size or any iteration budget.
        let plans = block_ids
            .into_iter()
            .filter_map(|block_id| plan_block(eu, block_id, &cfg, &uses))
            .collect::<Vec<_>>();
        if !plans.is_empty() {
            // Reserve the complete block-id range before mutating the EU.  An ID
            // overflow therefore leaves the input byte-for-byte unchanged.
            let Some(additional_blocks) = plans.len().checked_mul(2) else {
                return;
            };
            let max_block = eu.blocks.keys().map(|id| id.0).max().unwrap_or(0);
            let Some(first_new_block) = max_block.checked_add(1) else {
                return;
            };
            let Some(last_new_block) = max_block.checked_add(additional_blocks) else {
                return;
            };
            if last_new_block > u32::MAX as usize {
                return;
            }
            if options.optimize_options.diagnostics.pass_timing {
                let moved = plans.iter().map(|plan| plan.moved.len()).sum::<usize>();
                let stores = plans
                    .iter()
                    .map(|plan| plan.distributed.len())
                    .sum::<usize>();
                tracing::debug!(
                    "[guarded-region-sinking] regions={} moved_instructions={moved} distributed_stores={stores}",
                    plans.len(),
                );
            }

            let mut reg_counter = eu.register_map.keys().map(|reg| reg.0).max().unwrap_or(0);
            for (ordinal, plan) in plans.into_iter().enumerate() {
                let true_id = BlockId(first_new_block + ordinal * 2);
                let false_id = BlockId(first_new_block + ordinal * 2 + 1);
                apply_plan(eu, plan, true_id, false_id, &mut reg_counter);
            }

            // Edge sinking can separate two outputs selected by the same RTL
            // priority chain: one Store lives in the new edge block while another
            // is fed by a merge block parameter.  Put prefix phi Stores on their
            // incoming edges, then perform ordinary single-predecessor block
            // merging.  This reconstructs a same-block effect region without
            // guessing instruction order or duplicating any pure computation.
            if distribute_prefix_phi_stores(eu) {
                verify_stage(eu, "prefix phi stores");
                merge_single_predecessor_jump_blocks(eu);
                verify_stage(eu, "single-predecessor merge");
                form_coupled_store_regions(eu);
                verify_stage(eu, "post-merge coupled stores");
                sink_deferred_value_regions(eu);
                verify_stage(eu, "deferred value regions");
            }
        }

        // Values consumed on both mutually exclusive sides otherwise remain
        // live in the branch head. Split their pure cones at the control edge
        // before ordinary dominator placement.
        split_branch_live_ranges(eu);
        verify_stage(eu, "branch live-range splitting");

        // Place pure values at the nearest common dominator of their uses.
        // Priority recovery often leaves the normal arithmetic in the branch
        // head even though every use is in the final normal leaf.  This is
        // ordinary SSA code sinking on the existing CFG: it adds no branch and
        // moves neither memory reads nor observable effects.
        sink_pure_values_to_use_dominators(eu);
        verify_stage(eu, "pure-value dominator sinking");

        debug_assert_eq!(eu.verify_result(), Ok(()));
    }
}

#[cfg(test)]
mod tests;
