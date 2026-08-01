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

pub(super) struct GuardedRegionSinkingPass;

/// Recover effect/value regions which become visible only after native EUs
/// have been merged into one CFG.
///
/// This deliberately runs only the coupled-store and closed same-predicate
/// planners. Replaying the complete source-EU pass after fusion would also
/// perform unrelated edge sinking and repeated CFG repair.
pub(super) fn recover_merged_effect_regions(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
) {
    if four_state || eu.verify_result().is_err() {
        return;
    }
    form_coupled_store_regions(eu);
    form_same_predicate_regions(eu);
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

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        // A four-state Mux bitwise-merges its arms for an X/Z condition, while
        // control flow selects one edge. No structural proof below authorizes
        // that conversion in four-state mode.
        if options.four_state || eu.verify_result().is_err() {
            return;
        }
        let verify_stage = |eu: &ExecutionUnit<RegionedAbsoluteAddr>, stage: &'static str| {
            if std::env::var_os("CELOX_SIR_VERIFY_PASSES").is_some()
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

        let Ok(cfg) = SirCfg::analyze(eu) else {
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
            if std::env::var_os("CELOX_PASS_TIMING").is_some() {
                let moved = plans.iter().map(|plan| plan.moved.len()).sum::<usize>();
                let stores = plans
                    .iter()
                    .map(|plan| plan.distributed.len())
                    .sum::<usize>();
                eprintln!(
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

#[derive(Clone)]
struct BranchLiveRangeSplitPlan {
    source: BlockId,
    arms: [BlockId; 2],
    placements: Vec<u8>,
}

/// Split a pure cone which is live from a branch head into both exclusive
/// successor regions.
///
/// Both successors must have the head as their sole predecessor. Therefore an
/// edge-local copy executes exactly once whenever that edge is taken, even if
/// the branch is inside a loop. Loads and effects stay in the head and form
/// the materialization frontier.
fn split_branch_live_ranges(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let Ok(cfg) = SirCfg::analyze_forward_structure(eu) else {
        return;
    };
    let uses = collect_uses(eu);
    let mut plans = Vec::new();

    for &source_id in &cfg.block_ids {
        let source = &eu.blocks[&source_id];
        let SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } = &source.terminator
        else {
            continue;
        };
        let arms = [true_block.0, false_block.0];
        let (Some(source_index), Some(true_index), Some(false_index)) = (
            cfg.block_index(source_id),
            cfg.block_index(arms[0]),
            cfg.block_index(arms[1]),
        ) else {
            continue;
        };
        if arms[0] == arms[1]
            || cfg.predecessors[true_index].as_slice() != [source_index]
            || cfg.predecessors[false_index].as_slice() != [source_index]
        {
            continue;
        }

        // Bit 0 is the true edge and bit 1 is the false edge. Reverse order
        // lets a producer inherit the exact edge set of a local pure user.
        let mut placements = vec![0u8; source.instructions.len()];
        for (index, instruction) in source.instructions.iter().enumerate().rev() {
            if !instruction_is_movable(instruction) {
                continue;
            }
            let Some(definition) = def_reg(instruction) else {
                continue;
            };
            let Some(value_uses) = uses.get(&definition).filter(|uses| !uses.is_empty()) else {
                continue;
            };
            let mut edges = 0u8;
            let mut legal = true;
            for site in value_uses {
                let use_edges = match *site {
                    UseSite::Instruction { block, index: user } if block == source_id => {
                        placements.get(user).copied().unwrap_or(0)
                    }
                    _ if site.block() == source_id => 0,
                    _ => {
                        let block = site.block();
                        match (cfg.dominates(arms[0], block), cfg.dominates(arms[1], block)) {
                            (true, false) => 1,
                            (false, true) => 2,
                            _ => 0,
                        }
                    }
                };
                if use_edges == 0 {
                    legal = false;
                    break;
                }
                edges |= use_edges;
            }
            if legal {
                placements[index] = edges;
            }
        }

        // One-edge values are already handled by ordinary sinking. This pass
        // only applies when it removes a genuinely shared branch live range.
        if placements.contains(&3) {
            plans.push(BranchLiveRangeSplitPlan {
                source: source_id,
                arms,
                placements,
            });
        }
    }

    if plans.is_empty() {
        return;
    }
    // Inner branches first. An outer split may rewrite their operands, but it
    // cannot invalidate their source instruction indices.
    plans.sort_unstable_by_key(|plan| {
        std::cmp::Reverse(dominator_depth(&cfg, cfg.block_index(plan.source).unwrap()))
    });

    let mut reg_counter = eu.register_map.keys().map(|reg| reg.0).max().unwrap_or(0);
    let Some(additional_registers) = plans.iter().try_fold(0usize, |total, plan| {
        plan.placements.iter().try_fold(total, |total, placement| {
            total.checked_add(placement.count_ones() as usize)
        })
    }) else {
        return;
    };
    if reg_counter.checked_add(additional_registers).is_none() {
        return;
    }

    for plan in plans {
        let original = eu.blocks[&plan.source].instructions.clone();
        let mut replacements = [HashMap::default(), HashMap::default()];
        let mut cloned = [Vec::new(), Vec::new()];

        for (index, instruction) in original.iter().enumerate() {
            let placement = plan.placements[index];
            if placement == 0 {
                continue;
            }
            let old = def_reg(instruction).expect("split plan contains only value definitions");
            for arm in 0..2 {
                if placement & (1 << arm) == 0 {
                    continue;
                }
                reg_counter += 1;
                let new = RegisterId(reg_counter);
                eu.register_map.insert(new, eu.register_map[&old].clone());
                cloned[arm].push(
                    clone_pure_instruction(instruction, new, &replacements[arm])
                        .expect("split plan contains only pure instructions"),
                );
                replacements[arm].insert(old, new);
            }
        }

        for arm in 0..2 {
            for &block_id in &cfg.block_ids {
                if !cfg.dominates(plan.arms[arm], block_id) {
                    continue;
                }
                let block = eu.blocks.get_mut(&block_id).unwrap();
                for (&old, &new) in &replacements[arm] {
                    for instruction in &mut block.instructions {
                        replace_register_uses_in_instruction(instruction, old, new);
                    }
                    replace_register_uses_in_terminator(&mut block.terminator, old, new);
                }
            }
            cloned[arm].append(&mut eu.blocks.get_mut(&plan.arms[arm]).unwrap().instructions);
            eu.blocks.get_mut(&plan.arms[arm]).unwrap().instructions =
                std::mem::take(&mut cloned[arm]);
        }

        let source = eu.blocks.get_mut(&plan.source).unwrap();
        source.instructions = std::mem::take(&mut source.instructions)
            .into_iter()
            .enumerate()
            .filter_map(|(index, instruction)| (plan.placements[index] == 0).then_some(instruction))
            .collect();
        for (index, instruction) in original.iter().enumerate() {
            if plan.placements[index] != 0
                && let Some(old) = def_reg(instruction)
            {
                eu.register_map.remove(&old);
            }
        }
    }

    debug_assert_eq!(eu.verify_result(), Ok(()));
}

/// Sink pure SSA definitions to the nearest common dominator of all uses.
///
/// Definitions are considered in reverse instruction order, so a producer can
/// follow a consumer which was itself sunk out of the source block.  The
/// destination must remain in an acyclic region: sinking a loop-invariant
/// value into a loop would trade one evaluation for one per iteration.
fn sink_pure_values_to_use_dominators(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let Ok(cfg) = SirCfg::analyze(eu) else {
        return;
    };
    let uses = collect_uses(eu);
    let mut placements = HashMap::<(BlockId, usize), BlockId>::default();

    for &source_id in &cfg.block_ids {
        let source = &eu.blocks[&source_id];
        for (index, instruction) in source.instructions.iter().enumerate().rev() {
            if !instruction_is_movable(instruction) {
                continue;
            }
            let Some(definition) = def_reg(instruction) else {
                continue;
            };
            let Some(definition_uses) = uses.get(&definition).filter(|uses| !uses.is_empty())
            else {
                continue;
            };

            let mut destination = None;
            for site in definition_uses {
                let use_block = match *site {
                    UseSite::Instruction {
                        block,
                        index: use_index,
                    } if block == source_id => placements
                        .get(&(source_id, use_index))
                        .copied()
                        .unwrap_or(source_id),
                    _ => site.block(),
                };
                let Some(use_index) = cfg.block_index(use_block) else {
                    destination = None;
                    break;
                };
                destination = Some(match destination {
                    None => use_index,
                    Some(current) => cfg.dominators.lca(current, use_index).unwrap_or(0),
                });
            }

            let Some(destination) = destination.map(|index| cfg.block_ids[index]) else {
                continue;
            };
            let destination_index = cfg.block_index(destination).unwrap();
            if destination != source_id
                && cfg.dominates(source_id, destination)
                && !cfg.sccs[cfg.scc_for_block[destination_index]].cyclic
            {
                placements.insert((source_id, index), destination);
            }
        }
    }

    if placements.is_empty() {
        return;
    }

    // Source blocks are in RPO, hence definitions from an outer dominator are
    // prepended before definitions from an inner dominator when both arrive at
    // the same destination.
    let mut incoming = HashMap::<BlockId, Vec<SIRInstruction<RegionedAbsoluteAddr>>>::default();
    for &source_id in &cfg.block_ids {
        let source = eu.blocks.get_mut(&source_id).unwrap();
        let mut retained = Vec::with_capacity(source.instructions.len());
        for (index, instruction) in std::mem::take(&mut source.instructions)
            .into_iter()
            .enumerate()
        {
            if let Some(&destination) = placements.get(&(source_id, index)) {
                incoming.entry(destination).or_default().push(instruction);
            } else {
                retained.push(instruction);
            }
        }
        source.instructions = retained;
    }
    for (destination, mut instructions) in incoming {
        let block = eu.blocks.get_mut(&destination).unwrap();
        instructions.append(&mut block.instructions);
        block.instructions = instructions;
    }
}

pub(super) fn sink_pure_values_with_predicate_repair(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    sink_pure_values_to_use_dominators(eu);
    loop {
        let pruned = prune_control_dead_phi_operands(eu);
        let repaired = repair_predicated_live_outs(eu);
        if !pruned && !repaired {
            break;
        }
        sink_pure_values_to_use_dominators(eu);
    }
}

#[derive(Clone)]
struct PredicatedLiveOutPlan {
    value: RegisterId,
    source: BlockId,
    candidate: BlockId,
    merge: BlockId,
    facts: Vec<(RegisterId, bool)>,
}

/// Repair SSA for a value which is available under the same predicate facts
/// in two separate priority regions.  A plain dominator LCA places such a
/// value before both regions.  Passing it through the first region's merge
/// makes its conditional availability explicit, after which ordinary sinking
/// can place the complete producer DAG below the shared guards.
fn repair_predicated_live_outs(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) -> bool {
    let Ok(cfg) = SirCfg::analyze(eu) else {
        return false;
    };
    let uses = collect_uses(eu);
    let facts = predicate_facts(eu, &cfg);
    let mut definitions = HashMap::default();
    for &block_id in &cfg.block_ids {
        for instruction in &eu.blocks[&block_id].instructions {
            if instruction_is_movable(instruction)
                && let Some(value) = def_reg(instruction)
            {
                definitions.insert(value, block_id);
            }
        }
    }

    let mut plans = Vec::new();
    let mut claimed = HashSet::default();
    for (&value, &source) in &definitions {
        let Some(value_uses) = uses.get(&value).filter(|uses| uses.len() >= 2) else {
            continue;
        };
        let mut use_blocks = value_uses
            .iter()
            .map(|site| site.block())
            .collect::<Vec<_>>();
        use_blocks.sort_unstable();
        use_blocks.dedup();
        if use_blocks.len() < 2 {
            continue;
        }
        let mut common_facts = facts[cfg.block_index(use_blocks[0]).unwrap()].clone();
        common_facts.retain(|fact| {
            use_blocks
                .iter()
                .skip(1)
                .all(|block| facts[cfg.block_index(*block).unwrap()].contains(fact))
        });
        if common_facts.is_empty() {
            continue;
        }

        let mut candidates = HashSet::default();
        for &use_block in &use_blocks {
            let mut block = cfg.block_index(use_block).unwrap();
            while cfg.block_ids[block] != source {
                candidates.insert(block);
                let Some(parent) = cfg.dominators.idom[block] else {
                    break;
                };
                block = parent;
            }
        }
        let mut best = None;
        for candidate_index in candidates {
            let candidate = cfg.block_ids[candidate_index];
            let candidate_facts = &facts[candidate_index];
            if candidate_facts.is_empty()
                || candidate_facts
                    .iter()
                    .any(|fact| !common_facts.contains(fact) || fact.0 == value)
                || !cfg.dominates(source, candidate)
                || cfg.sccs[cfg.scc_for_block[candidate_index]].cyclic
            {
                continue;
            }
            let dominated_uses = use_blocks
                .iter()
                .filter(|&&block| cfg.dominates(candidate, block))
                .count();
            if dominated_uses == 0 || dominated_uses == use_blocks.len() {
                continue;
            }
            let Some(merge_index) = cfg.immediate_postdominator(candidate_index) else {
                continue;
            };
            let merge = cfg.block_ids[merge_index];
            if cfg.dominates(candidate, merge)
                || cfg.sccs[cfg.scc_for_block[merge_index]].cyclic
                || use_blocks.iter().any(|&block| {
                    !cfg.dominates(candidate, block)
                        && (!cfg.dominates(merge, block)
                            || candidate_facts
                                .iter()
                                .any(|fact| !facts[cfg.block_index(block).unwrap()].contains(fact)))
                })
                || !merge_accepts_phi(&cfg, eu, merge)
            {
                continue;
            }
            let score = (
                candidate_facts.len(),
                dominator_depth(&cfg, candidate_index),
            );
            if best.as_ref().is_none_or(
                |(best_score, _, _): &((usize, usize), BlockId, BlockId)| score > *best_score,
            ) {
                best = Some((score, candidate, merge));
            }
        }
        let Some((_, candidate, merge)) = best else {
            continue;
        };
        if claimed.insert(value) {
            plans.push(PredicatedLiveOutPlan {
                value,
                source,
                candidate,
                merge,
                facts: facts[cfg.block_index(candidate).unwrap()].clone(),
            });
        }
    }
    if plans.is_empty() {
        return false;
    }
    plans.sort_unstable_by_key(|plan| (plan.merge.0, plan.value.0));

    let mut next_register = eu.register_map.keys().map(|reg| reg.0).max().unwrap_or(0);
    let Some(_) = plans
        .len()
        .checked_mul(2)
        .and_then(|additional| next_register.checked_add(additional))
    else {
        return false;
    };
    for plan in plans {
        let dummy_id = next_register + 1;
        let phi_id = dummy_id + 1;
        next_register = phi_id;
        let Some(ty) = eu.register_map.get(&plan.value).cloned() else {
            continue;
        };
        let dummy = RegisterId(dummy_id);
        let phi = RegisterId(phi_id);
        eu.register_map.insert(dummy, ty.clone());
        eu.register_map.insert(phi, ty);
        eu.blocks
            .get_mut(&plan.source)
            .unwrap()
            .instructions
            .push(SIRInstruction::Imm(dummy, SIRValue::new(0u8)));

        for &block_id in &cfg.block_ids {
            if cfg.dominates(plan.merge, block_id)
                && !cfg.dominates(plan.candidate, block_id)
                && plan
                    .facts
                    .iter()
                    .all(|fact| facts[cfg.block_index(block_id).unwrap()].contains(fact))
            {
                let block = eu.blocks.get_mut(&block_id).unwrap();
                for instruction in &mut block.instructions {
                    replace_register_uses_in_instruction(instruction, plan.value, phi);
                }
                replace_register_uses_in_terminator(&mut block.terminator, plan.value, phi);
            }
        }

        let merge_index = cfg.block_index(plan.merge).unwrap();
        for &predecessor_index in &cfg.predecessors[merge_index] {
            let predecessor = cfg.block_ids[predecessor_index];
            let argument = if cfg.dominates(plan.candidate, predecessor) {
                plan.value
            } else {
                dummy
            };
            append_edge_argument(
                &mut eu.blocks.get_mut(&predecessor).unwrap().terminator,
                plan.merge,
                argument,
            );
        }
        eu.blocks.get_mut(&plan.merge).unwrap().params.push(phi);
    }
    debug_assert_eq!(eu.verify_result(), Ok(()));
    true
}

fn dominator_depth(cfg: &SirCfg, mut block: usize) -> usize {
    let mut depth = 0;
    while let Some(parent) = cfg.dominators.idom[block] {
        depth += 1;
        block = parent;
    }
    depth
}

fn merge_accepts_phi(
    cfg: &SirCfg,
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    merge: BlockId,
) -> bool {
    let Some(merge_index) = cfg.block_index(merge) else {
        return false;
    };
    cfg.predecessors[merge_index].iter().all(|&predecessor| {
        let predecessor = cfg.block_ids[predecessor];
        match &eu.blocks[&predecessor].terminator {
            SIRTerminator::Jump(target, _) => *target == merge,
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => true_block.0 == merge || false_block.0 == merge,
            SIRTerminator::Switch { .. } => false,
            SIRTerminator::Return | SIRTerminator::Error(_) => false,
        }
    })
}

fn append_edge_argument(terminator: &mut SIRTerminator, target: BlockId, argument: RegisterId) {
    match terminator {
        SIRTerminator::Jump(actual, arguments) if *actual == target => arguments.push(argument),
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => {
            if true_block.0 == target {
                true_block.1.push(argument);
            }
            if false_block.0 == target {
                false_block.1.push(argument);
            }
        }
        _ => unreachable!("validated merge predecessor must target the merge"),
    }
}

#[derive(Clone, Copy)]
enum IncomingEdgeKind {
    Jump,
    True,
    False,
}

#[derive(Clone)]
struct IncomingPhiEdge {
    predecessor: BlockId,
    kind: IncomingEdgeKind,
    arguments: Vec<RegisterId>,
    facts: Vec<(RegisterId, bool)>,
}

/// Remove phi operands which cannot reach any use of that phi result.
///
/// Priority regions commonly merge both their selected result and the
/// predicates needed by a later priority region.  On an early-exit edge the
/// later payload is unobservable, but ordinary SSA liveness still keeps its
/// producer alive.  The branch facts on the consuming side and the facts on
/// each incoming edge are sufficient to prove those operands dead without
/// enumerating paths.
fn prune_control_dead_phi_operands(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) -> bool {
    let Ok(cfg) = SirCfg::analyze(eu) else {
        return false;
    };
    let uses = collect_uses(eu);
    let block_facts = predicate_facts(eu, &cfg);
    let mut constants = HashMap::<RegisterId, bool>::default();
    for block in eu.blocks.values() {
        for instruction in &block.instructions {
            if let SIRInstruction::Imm(dst, value) = instruction
                && value.mask.to_u64_digits().is_empty()
            {
                constants.insert(*dst, !value.payload.to_u64_digits().is_empty());
            }
        }
    }

    let mut replacements = Vec::<(BlockId, IncomingEdgeKind, usize, RegisterId)>::new();
    let mut replacement_params = HashSet::<RegisterId>::default();
    for &merge_id in &cfg.block_ids {
        let merge = &eu.blocks[&merge_id];
        if merge.params.is_empty() {
            continue;
        }
        let mut incoming = Vec::new();
        let merge_index = cfg.block_index(merge_id).unwrap();
        for &predecessor_index in &cfg.predecessors[merge_index] {
            let predecessor_id = cfg.block_ids[predecessor_index];
            let predecessor = &eu.blocks[&predecessor_id];
            match &predecessor.terminator {
                SIRTerminator::Jump(target, arguments) if *target == merge_id => {
                    incoming.push(IncomingPhiEdge {
                        predecessor: predecessor_id,
                        kind: IncomingEdgeKind::Jump,
                        arguments: arguments.clone(),
                        facts: block_facts[predecessor_index].clone(),
                    });
                }
                SIRTerminator::Branch {
                    cond,
                    true_block,
                    false_block,
                } => {
                    if true_block.0 == merge_id {
                        let mut facts = block_facts[predecessor_index].clone();
                        facts.push((*cond, true));
                        incoming.push(IncomingPhiEdge {
                            predecessor: predecessor_id,
                            kind: IncomingEdgeKind::True,
                            arguments: true_block.1.clone(),
                            facts,
                        });
                    }
                    if false_block.0 == merge_id {
                        let mut facts = block_facts[predecessor_index].clone();
                        facts.push((*cond, false));
                        incoming.push(IncomingPhiEdge {
                            predecessor: predecessor_id,
                            kind: IncomingEdgeKind::False,
                            arguments: false_block.1.clone(),
                            facts,
                        });
                    }
                }
                _ => {}
            }
        }
        if incoming.len() < 2
            || incoming
                .iter()
                .any(|edge| edge.arguments.len() != merge.params.len())
        {
            continue;
        }
        let parameter_indices = merge
            .params
            .iter()
            .enumerate()
            .map(|(index, &parameter)| (parameter, index))
            .collect::<HashMap<_, _>>();

        for edge in incoming {
            let known_on_edge = |register: RegisterId| {
                edge.facts
                    .iter()
                    .rev()
                    .find_map(|&(condition, value)| (condition == register).then_some(value))
                    .or_else(|| constants.get(&register).copied())
            };
            for (parameter_index, &parameter) in merge.params.iter().enumerate() {
                if constants.contains_key(&edge.arguments[parameter_index]) {
                    continue;
                }
                let Some(parameter_uses) = uses.get(&parameter) else {
                    continue;
                };
                let all_uses_unreachable = parameter_uses.iter().all(|site| {
                    let use_block = site.block();
                    if use_block == merge_id || !cfg.dominates(merge_id, use_block) {
                        return false;
                    }
                    block_facts[cfg.block_index(use_block).unwrap()].iter().any(
                        |&(condition, required)| {
                            let Some(&condition_index) = parameter_indices.get(&condition) else {
                                return false;
                            };
                            known_on_edge(edge.arguments[condition_index])
                                .is_some_and(|actual| actual != required)
                        },
                    )
                });
                if all_uses_unreachable {
                    replacements.push((edge.predecessor, edge.kind, parameter_index, parameter));
                    replacement_params.insert(parameter);
                }
            }
        }
    }
    if replacements.is_empty() {
        return false;
    }

    let mut next_register = eu.register_map.keys().map(|reg| reg.0).max().unwrap_or(0);
    let Some(last_register) = next_register.checked_add(replacement_params.len()) else {
        return false;
    };
    let entry = cfg.block_ids[0];
    let mut zero_for_param = HashMap::default();
    let mut params = replacement_params.into_iter().collect::<Vec<_>>();
    params.sort_unstable();
    for parameter in params {
        next_register += 1;
        let zero = RegisterId(next_register);
        eu.register_map
            .insert(zero, eu.register_map[&parameter].clone());
        eu.blocks
            .get_mut(&entry)
            .unwrap()
            .instructions
            .push(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
        zero_for_param.insert(parameter, zero);
    }
    debug_assert_eq!(next_register, last_register);

    for (predecessor, kind, parameter_index, parameter) in replacements {
        let zero = zero_for_param[&parameter];
        match (
            &mut eu.blocks.get_mut(&predecessor).unwrap().terminator,
            kind,
        ) {
            (SIRTerminator::Jump(_, arguments), IncomingEdgeKind::Jump) => {
                arguments[parameter_index] = zero;
            }
            (SIRTerminator::Branch { true_block, .. }, IncomingEdgeKind::True) => {
                true_block.1[parameter_index] = zero;
            }
            (SIRTerminator::Branch { false_block, .. }, IncomingEdgeKind::False) => {
                false_block.1[parameter_index] = zero;
            }
            _ => unreachable!(),
        }
    }
    debug_assert_eq!(eu.verify_result(), Ok(()));
    true
}

/// Remove control dependence which has no remaining value or effect after
/// rooted dead-store elimination. This is the CFG half of ADCE: first prune
/// dead phi parameters, then bypass pure SESE regions whose merge carries no
/// live value.
pub(super) fn eliminate_dead_control_regions(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    loop {
        prune_dead_block_parameters(eu);
        super::pass_vectorize_concat::remove_dead_definitions(eu);
        let Ok(cfg) = SirCfg::analyze(eu) else {
            return;
        };
        let uses = collect_uses(eu);
        let mut candidates = Vec::new();
        for &head_id in &cfg.block_ids {
            let SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } = &eu.blocks[&head_id].terminator
            else {
                continue;
            };
            let Some(merge_id) = cfg.common_postdominator(true_block.0, false_block.0) else {
                continue;
            };
            if merge_id == head_id || !eu.blocks[&merge_id].params.is_empty() {
                continue;
            }
            let mut region = HashSet::default();
            let mut work = vec![true_block.0, false_block.0];
            let mut valid = true;
            while let Some(block_id) = work.pop() {
                if block_id == merge_id || !region.insert(block_id) {
                    continue;
                }
                let index = cfg.block_index(block_id).unwrap();
                let block = &eu.blocks[&block_id];
                if cfg.sccs[cfg.scc_for_block[index]].cyclic
                    || block.instructions.iter().any(instruction_has_effect)
                {
                    valid = false;
                    break;
                }
                match &block.terminator {
                    SIRTerminator::Jump(target, arguments) => {
                        if *target == merge_id && !arguments.is_empty() {
                            valid = false;
                            break;
                        }
                        work.push(*target);
                    }
                    SIRTerminator::Branch {
                        true_block,
                        false_block,
                        ..
                    } => {
                        work.push(true_block.0);
                        work.push(false_block.0);
                    }
                    SIRTerminator::Switch { cases, default, .. } => {
                        work.extend(cases.iter().map(|case| case.target));
                        work.push(*default);
                    }
                    SIRTerminator::Return | SIRTerminator::Error(_) => {
                        valid = false;
                        break;
                    }
                }
            }
            if !valid || region.is_empty() {
                continue;
            }
            for &block_id in &region {
                let index = cfg.block_index(block_id).unwrap();
                if cfg.predecessors[index].iter().any(|&predecessor| {
                    let predecessor = cfg.block_ids[predecessor];
                    predecessor != head_id && !region.contains(&predecessor)
                }) || eu.blocks[&block_id].instructions.iter().any(|instruction| {
                    def_reg(instruction).is_some_and(|definition| {
                        uses.get(&definition)
                            .into_iter()
                            .flatten()
                            .any(|site| !region.contains(&site.block()))
                    })
                }) {
                    valid = false;
                    break;
                }
            }
            if valid {
                candidates.push((head_id, merge_id, region));
            }
        }
        if candidates.is_empty() {
            break;
        }

        // One CFG/use analysis can prove several independent dead regions.
        // Prefer the largest region when candidates nest, and reserve its
        // head, body, and merge so no other rewrite in this batch can mutate
        // or remove one of those blocks. Re-analyze only after the maximal
        // non-overlapping batch has been removed.
        candidates.sort_unstable_by(|lhs, rhs| {
            rhs.2
                .len()
                .cmp(&lhs.2.len())
                .then_with(|| lhs.0.0.cmp(&rhs.0.0))
        });
        let mut claimed = HashSet::default();
        let mut selected = Vec::new();
        for (head, merge, region) in candidates {
            if claimed.contains(&head)
                || claimed.contains(&merge)
                || region.iter().any(|block| claimed.contains(block))
            {
                continue;
            }
            claimed.insert(head);
            claimed.insert(merge);
            claimed.extend(region.iter().copied());
            selected.push((head, merge, region));
        }
        debug_assert!(!selected.is_empty());
        for (head, merge, region) in selected {
            eu.blocks.get_mut(&head).unwrap().terminator = SIRTerminator::Jump(merge, Vec::new());
            for block in region {
                eu.blocks.remove(&block);
            }
        }
    }
    prune_dead_block_parameters(eu);
    super::pass_vectorize_concat::remove_dead_definitions(eu);
    debug_assert_eq!(eu.verify_result(), Ok(()));
}

fn prune_dead_block_parameters(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    loop {
        let uses = collect_uses(eu);
        let mut dead = BTreeMap::<BlockId, Vec<usize>>::new();
        for block in eu.blocks.values() {
            for (index, parameter) in block.params.iter().enumerate() {
                if !uses.contains_key(parameter) {
                    dead.entry(block.id).or_default().push(index);
                }
            }
        }
        if dead.is_empty() {
            break;
        }
        for (&block, indices) in &dead {
            for &index in indices.iter().rev() {
                eu.blocks.get_mut(&block).unwrap().params.remove(index);
            }
        }
        for predecessor in eu.blocks.values_mut() {
            remove_dead_edge_arguments(&mut predecessor.terminator, &dead);
        }
    }
}

fn remove_dead_edge_arguments(
    terminator: &mut SIRTerminator,
    dead: &BTreeMap<BlockId, Vec<usize>>,
) {
    let remove = |target: BlockId, arguments: &mut Vec<RegisterId>| {
        let Some(indices) = dead.get(&target) else {
            return;
        };
        for &index in indices.iter().rev() {
            arguments.remove(index);
        }
    };
    match terminator {
        SIRTerminator::Jump(target, arguments) => {
            remove(*target, arguments);
        }
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => {
            remove(true_block.0, &mut true_block.1);
            remove(false_block.0, &mut false_block.1);
        }
        _ => {}
    }
}

/// Move an acyclic pure SESE decision region to the only block which consumes
/// its merge value.  This is the multi-block form of deferred code motion: a
/// scheduler may have placed normal-path normalization before exception
/// priority checks even though the whole decision region and its phi are only
/// needed by the final normal leaf.
fn sink_deferred_value_regions(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let Ok(cfg) = SirCfg::analyze(eu) else {
        return;
    };
    let uses = collect_uses(eu);
    let mut plans = Vec::new();
    let mut claimed = HashSet::default();

    for &head_id in &cfg.block_ids {
        let head = &eu.blocks[&head_id];
        let SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } = &head.terminator
        else {
            continue;
        };
        if !true_block.1.is_empty() || !false_block.1.is_empty() || true_block.0 == false_block.0 {
            continue;
        }
        let Some(merge_id) = cfg.common_postdominator(true_block.0, false_block.0) else {
            continue;
        };
        let Some(merge) = eu.blocks.get(&merge_id) else {
            continue;
        };
        if merge.params.is_empty() || merge_id == head_id {
            continue;
        }

        let mut region = HashSet::default();
        let mut work = vec![true_block.0, false_block.0];
        let mut valid = true;
        while let Some(block_id) = work.pop() {
            if block_id == merge_id || !region.insert(block_id) {
                continue;
            }
            let Some(index) = cfg.block_index(block_id) else {
                valid = false;
                break;
            };
            let block = &eu.blocks[&block_id];
            if cfg.sccs[cfg.scc_for_block[index]].cyclic
                || block
                    .instructions
                    .iter()
                    .any(|instruction| !instruction_is_movable(instruction))
            {
                valid = false;
                break;
            }
            match &block.terminator {
                SIRTerminator::Jump(target, _) => work.push(*target),
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => {
                    work.push(true_block.0);
                    work.push(false_block.0);
                }
                SIRTerminator::Switch { cases, default, .. } => {
                    work.extend(cases.iter().map(|case| case.target));
                    work.push(*default);
                }
                SIRTerminator::Return | SIRTerminator::Error(_) => {
                    valid = false;
                    break;
                }
            }
        }
        if !valid || region.is_empty() || region.contains(&head_id) {
            continue;
        }
        let merge_index = cfg.block_index(merge_id).unwrap();
        if cfg.predecessors[merge_index]
            .iter()
            .any(|&predecessor| !region.contains(&cfg.block_ids[predecessor]))
        {
            continue;
        }
        for &block_id in &region {
            let index = cfg.block_index(block_id).unwrap();
            if cfg.predecessors[index].iter().any(|&predecessor| {
                let predecessor = cfg.block_ids[predecessor];
                predecessor != head_id && !region.contains(&predecessor)
            }) || eu.blocks[&block_id].instructions.iter().any(|instruction| {
                def_reg(instruction).is_some_and(|definition| {
                    uses.get(&definition)
                        .into_iter()
                        .flatten()
                        .any(|site| !region.contains(&site.block()))
                })
            }) {
                valid = false;
                break;
            }
            match &eu.blocks[&block_id].terminator {
                SIRTerminator::Jump(target, arguments) if *target == merge_id => {
                    valid &= arguments.len() == merge.params.len();
                }
                SIRTerminator::Jump(target, _) => valid &= region.contains(target),
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => {
                    valid &= region.contains(&true_block.0) && region.contains(&false_block.0);
                }
                SIRTerminator::Switch { cases, default, .. } => {
                    valid &= cases.iter().all(|case| region.contains(&case.target))
                        && region.contains(default);
                }
                SIRTerminator::Return | SIRTerminator::Error(_) => valid = false,
            }
        }
        if !valid {
            continue;
        }

        let mut use_block = None;
        for &param in &merge.params {
            let Some(param_uses) = uses.get(&param) else {
                valid = false;
                break;
            };
            for site in param_uses {
                let block = site.block();
                if [head_id, merge_id].contains(&block) || region.contains(&block) {
                    valid = false;
                    break;
                }
                if use_block.is_some_and(|expected| expected != block) {
                    valid = false;
                    break;
                }
                use_block = Some(block);
            }
        }
        let Some(use_block) = use_block.filter(|_| valid) else {
            continue;
        };
        if !cfg.dominates(merge_id, use_block)
            || cfg.postdominates(use_block, merge_id)
            || claimed.contains(&head_id)
            || claimed.contains(&merge_id)
            || claimed.contains(&use_block)
            || region.iter().any(|block| claimed.contains(block))
        {
            continue;
        }

        let mut moved_head = HashSet::default();
        for index in (0..head.instructions.len()).rev() {
            let Some(dst) = def_reg(&head.instructions[index]) else {
                continue;
            };
            if !instruction_is_movable(&head.instructions[index]) {
                continue;
            }
            let owned = uses
                .get(&dst)
                .into_iter()
                .flatten()
                .all(|site| match *site {
                    UseSite::Instruction {
                        block,
                        index: use_index,
                    } if block == head_id => moved_head.contains(&use_index),
                    UseSite::BranchCondition { block } if block == head_id => true,
                    _ => region.contains(&site.block()) || site.block() == use_block,
                });
            if owned {
                moved_head.insert(index);
            }
        }
        if moved_head.is_empty() {
            continue;
        }

        claimed.insert(head_id);
        claimed.insert(merge_id);
        claimed.insert(use_block);
        claimed.extend(region.iter().copied());
        plans.push(DeferredValueRegionPlan {
            head: head_id,
            merge: merge_id,
            region,
            use_block,
            moved_head,
        });
    }

    let max_block = eu.blocks.keys().map(|block| block.0).max().unwrap_or(0);
    let Some(first_new_block) = max_block.checked_add(1) else {
        return;
    };
    let Some(last_new_block) = max_block.checked_add(plans.len()) else {
        return;
    };
    if last_new_block > u32::MAX as usize {
        return;
    }
    for (ordinal, plan) in plans.into_iter().enumerate() {
        apply_deferred_value_region(eu, plan, BlockId(first_new_block + ordinal));
    }
}

fn apply_deferred_value_region(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: DeferredValueRegionPlan,
    continuation_id: BlockId,
) {
    let original_use = eu
        .blocks
        .remove(&plan.use_block)
        .expect("planned deferred region use must exist");
    let head = eu
        .blocks
        .get_mut(&plan.head)
        .expect("planned deferred region head must exist");
    let original_branch = std::mem::replace(
        &mut head.terminator,
        SIRTerminator::Jump(plan.merge, Vec::new()),
    );
    let mut moved = Vec::new();
    let mut retained = Vec::new();
    for (index, instruction) in std::mem::take(&mut head.instructions)
        .into_iter()
        .enumerate()
    {
        if plan.moved_head.contains(&index) {
            moved.push(instruction);
        } else {
            retained.push(instruction);
        }
    }
    head.instructions = retained;

    let params = std::mem::take(
        &mut eu
            .blocks
            .get_mut(&plan.merge)
            .expect("planned deferred region merge must exist")
            .params,
    );
    for &block_id in &plan.region {
        let block = eu.blocks.get_mut(&block_id).unwrap();
        if let SIRTerminator::Jump(target, _) = &mut block.terminator
            && *target == plan.merge
        {
            *target = continuation_id;
        }
    }
    eu.blocks.insert(
        plan.use_block,
        BasicBlock {
            id: plan.use_block,
            params: original_use.params,
            instructions: moved,
            terminator: original_branch,
        },
    );
    eu.blocks.insert(
        continuation_id,
        BasicBlock {
            id: continuation_id,
            params,
            instructions: original_use.instructions,
            terminator: original_use.terminator,
        },
    );
}

/// Distribute a merge block's leading Stores when their operands are block
/// parameters.  Every accepted predecessor is an unconditional incoming edge,
/// so each dynamic execution still performs each Store exactly once and in the
/// same order.  Backedges are excluded: this normalization is for acyclic CFG
/// joins, not loop-header effect motion.
fn distribute_prefix_phi_stores(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) -> bool {
    #[derive(Clone)]
    struct Plan {
        merge: BlockId,
        count: usize,
        incoming: Vec<(BlockId, Vec<RegisterId>)>,
    }

    let Ok(cfg) = SirCfg::analyze(eu) else {
        return false;
    };
    let mut plans = Vec::new();
    for &merge_id in &cfg.block_ids {
        let merge = &eu.blocks[&merge_id];
        if merge.params.is_empty() {
            continue;
        }
        let params = merge.params.iter().copied().collect::<HashSet<_>>();
        let count = merge
            .instructions
            .iter()
            .take_while(|inst| {
                matches!(inst, SIRInstruction::Store(_, _, _, source, _, _)
                    if params.contains(source))
            })
            .count();
        if count == 0 {
            continue;
        }

        let Some(merge_index) = cfg.block_index(merge_id) else {
            continue;
        };
        let mut incoming = Vec::new();
        let mut valid = cfg.predecessors[merge_index].len() >= 2;
        for &pred_index in &cfg.predecessors[merge_index] {
            let pred_id = cfg.block_ids[pred_index];
            if cfg.dominates(merge_id, pred_id) {
                valid = false;
                break;
            }
            match &eu.blocks[&pred_id].terminator {
                SIRTerminator::Jump(target, args)
                    if *target == merge_id && args.len() == merge.params.len() =>
                {
                    incoming.push((pred_id, args.clone()));
                }
                _ => {
                    valid = false;
                    break;
                }
            }
        }
        if valid {
            plans.push(Plan {
                merge: merge_id,
                count,
                incoming,
            });
        }
    }

    for plan in &plans {
        let merge = &eu.blocks[&plan.merge];
        let params = merge.params.clone();
        let stores = merge.instructions[..plan.count].to_vec();
        for (pred_id, args) in &plan.incoming {
            let mut edge_stores = stores.clone();
            for (&param, &argument) in params.iter().zip(args) {
                for store in &mut edge_stores {
                    replace_register_uses_in_instruction(store, param, argument);
                }
            }
            eu.blocks
                .get_mut(pred_id)
                .expect("planned predecessor must still exist")
                .instructions
                .extend(edge_stores);
        }
        eu.blocks
            .get_mut(&plan.merge)
            .expect("planned merge must still exist")
            .instructions
            .drain(..plan.count);
    }
    let uses = collect_uses(eu);
    for plan in &plans {
        let dead = eu.blocks[&plan.merge]
            .params
            .iter()
            .enumerate()
            .filter_map(|(index, param)| (!uses.contains_key(param)).then_some(index))
            .collect::<Vec<_>>();
        for &index in dead.iter().rev() {
            eu.blocks
                .get_mut(&plan.merge)
                .expect("planned merge must still exist")
                .params
                .remove(index);
            for (pred_id, _) in &plan.incoming {
                let SIRTerminator::Jump(target, arguments) =
                    &mut eu.blocks.get_mut(pred_id).unwrap().terminator
                else {
                    unreachable!("planned incoming edge must remain a Jump")
                };
                debug_assert_eq!(*target, plan.merge);
                arguments.remove(index);
            }
        }
    }
    !plans.is_empty()
}

/// Merge one disjoint layer of `pred -> successor` pairs where successor has
/// exactly one predecessor.  One layer is intentional: the edge blocks just
/// introduced by this pass are the desired predecessors, and a global fixed
/// point would needlessly reshape unrelated CFG.
fn merge_single_predecessor_jump_blocks(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let Ok(cfg) = SirCfg::analyze(eu) else {
        return;
    };
    let mut claimed = HashSet::default();
    let mut pairs = Vec::new();
    for &pred_id in &cfg.block_ids {
        let SIRTerminator::Jump(successor, arguments) = &eu.blocks[&pred_id].terminator else {
            continue;
        };
        let Some(successor_index) = cfg.block_index(*successor) else {
            continue;
        };
        if *successor == eu.entry_block_id
            || cfg.predecessors[successor_index].as_slice()
                != [cfg.block_index(pred_id).expect("CFG contains predecessor")]
            || arguments.len() != eu.blocks[successor].params.len()
            || claimed.contains(&pred_id)
            || claimed.contains(successor)
        {
            continue;
        }
        claimed.insert(pred_id);
        claimed.insert(*successor);
        pairs.push((pred_id, *successor, arguments.clone()));
    }

    for (pred_id, successor_id, arguments) in pairs {
        let mut successor = eu
            .blocks
            .remove(&successor_id)
            .expect("planned successor must still exist");
        for (&param, &argument) in successor.params.iter().zip(&arguments) {
            for instruction in &mut successor.instructions {
                replace_register_uses_in_instruction(instruction, param, argument);
            }
            replace_register_uses_in_terminator(&mut successor.terminator, param, argument);
            replace_register_uses(eu, param, argument);
        }
        let pred = eu
            .blocks
            .get_mut(&pred_id)
            .expect("planned predecessor must still exist");
        pred.instructions.extend(successor.instructions);
        pred.terminator = successor.terminator;
    }
}

fn replace_register_uses(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    old: RegisterId,
    new: RegisterId,
) {
    for block in eu.blocks.values_mut() {
        for instruction in &mut block.instructions {
            replace_register_uses_in_instruction(instruction, old, new);
        }
        replace_register_uses_in_terminator(&mut block.terminator, old, new);
    }
}

fn replace_register_uses_in_instruction(
    instruction: &mut SIRInstruction<RegionedAbsoluteAddr>,
    old: RegisterId,
    new: RegisterId,
) {
    let replace = |register: &mut RegisterId| {
        if *register == old {
            *register = new;
        }
    };
    let replace_offset = |offset: &mut SIROffset| match offset {
        SIROffset::Static(_) | SIROffset::PackedElements { .. } => {}
        SIROffset::Dynamic(register) => replace(register),
        SIROffset::Element {
            index,
            dynamic_bit_offset,
            ..
        } => {
            replace(index);
            if let Some(offset) = dynamic_bit_offset {
                replace(offset);
            }
        }
    };
    match instruction {
        SIRInstruction::Imm(..) => {}
        SIRInstruction::Binary(_, lhs, _, rhs) => {
            replace(lhs);
            replace(rhs);
        }
        SIRInstruction::Unary(_, _, source) | SIRInstruction::Slice(_, source, ..) => {
            replace(source)
        }
        SIRInstruction::Load(_, _, offset, _) => replace_offset(offset),
        SIRInstruction::Store(_, offset, _, source, _, _) => {
            replace_offset(offset);
            replace(source);
        }
        SIRInstruction::Commit(_, _, offset, _, _) => replace_offset(offset),
        SIRInstruction::Concat(_, args)
        | SIRInstruction::RuntimeEvent { args, .. }
        | SIRInstruction::CombCaptureEvent { args, .. } => {
            for arg in args {
                replace(arg);
            }
        }
        SIRInstruction::Mux(_, condition, true_value, false_value) => {
            replace(condition);
            replace(true_value);
            replace(false_value);
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
            replace(old);
            replace(new);
        }
    }
}

fn replace_register_uses_in_terminator(
    terminator: &mut SIRTerminator,
    old: RegisterId,
    new: RegisterId,
) {
    let replace = |register: &mut RegisterId| {
        if *register == old {
            *register = new;
        }
    };
    match terminator {
        SIRTerminator::Jump(_, args) => {
            for arg in args {
                replace(arg);
            }
        }
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            replace(cond);
            for arg in true_block.1.iter_mut().chain(&mut false_block.1) {
                replace(arg);
            }
        }
        SIRTerminator::Switch { selector, .. } => replace(selector),
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}

fn sink_deferred_value_diamonds(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let Ok(cfg) = SirCfg::analyze(eu) else {
        return;
    };
    let uses = collect_uses(eu);
    let mut plans = Vec::<DeferredValueDiamondPlan>::new();
    let mut removed_arms = HashSet::<BlockId>::default();
    let mut selected_use_blocks = HashSet::<BlockId>::default();
    let mut selected_boundaries = HashSet::<BlockId>::default();

    for &head_id in &cfg.block_ids {
        let head = &eu.blocks[&head_id];
        let SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } = &head.terminator
        else {
            continue;
        };
        if !true_block.1.is_empty() || !false_block.1.is_empty() || true_block.0 == false_block.0 {
            continue;
        }
        let (true_id, false_id) = (true_block.0, false_block.0);
        let (Some(true_arm), Some(false_arm)) = (eu.blocks.get(&true_id), eu.blocks.get(&false_id))
        else {
            continue;
        };
        if !true_arm.params.is_empty()
            || !false_arm.params.is_empty()
            || true_arm
                .instructions
                .iter()
                .any(|inst| !instruction_is_movable(inst))
            || false_arm
                .instructions
                .iter()
                .any(|inst| !instruction_is_movable(inst))
        {
            continue;
        }
        let (
            SIRTerminator::Jump(true_merge, true_args),
            SIRTerminator::Jump(false_merge, false_args),
        ) = (&true_arm.terminator, &false_arm.terminator)
        else {
            continue;
        };
        if true_merge != false_merge || true_args.len() != 1 || false_args.len() != 1 {
            continue;
        }
        let merge_id = *true_merge;
        let Some(merge) = eu.blocks.get(&merge_id) else {
            continue;
        };
        if merge.params.len() != 1
            || !has_exact_predecessors(&cfg, true_id, &[head_id])
            || !has_exact_predecessors(&cfg, false_id, &[head_id])
            || !has_exact_predecessors(&cfg, merge_id, &[true_id, false_id])
        {
            continue;
        }

        let result = merge.params[0];
        let Some(result_uses) = uses.get(&result) else {
            continue;
        };
        let Some(use_block) = result_uses.first().map(|site| site.block()) else {
            continue;
        };
        if result_uses.iter().any(|site| site.block() != use_block)
            || [head_id, true_id, false_id, merge_id].contains(&use_block)
        {
            continue;
        }
        let Some(use_body) = eu.blocks.get(&use_block) else {
            continue;
        };
        if !use_body.params.is_empty()
            || use_body.instructions.iter().any(instruction_has_effect)
            || !cfg.dominates(merge_id, use_block)
            // A postdominating use executes whenever the original diamond
            // does, so moving the branch would save no dynamic work.
            || cfg.postdominates(use_block, merge_id)
        {
            continue;
        }
        let (Some(head_index), Some(merge_index), Some(use_index)) = (
            cfg.block_index(head_id),
            cfg.block_index(merge_id),
            cfg.block_index(use_block),
        ) else {
            continue;
        };
        if cfg.sccs[cfg.scc_for_block[head_index]].cyclic
            || cfg.sccs[cfg.scc_for_block[merge_index]].cyclic
            || cfg.sccs[cfg.scc_for_block[use_index]].cyclic
            || removed_arms.contains(&true_id)
            || removed_arms.contains(&false_id)
            || removed_arms.contains(&use_block)
            || selected_boundaries.contains(&use_block)
            || selected_use_blocks.contains(&use_block)
            || selected_use_blocks.contains(&true_id)
            || selected_use_blocks.contains(&false_id)
            || selected_use_blocks.contains(&head_id)
            || selected_use_blocks.contains(&merge_id)
        {
            continue;
        }

        // Serial diamonds may share a boundary (`previous.merge ==
        // next.head`), but their arm blocks and selected leaves are disjoint.
        // This admits DivS/DivU/RemS/RemU sequences in one linear plan.
        removed_arms.insert(true_id);
        removed_arms.insert(false_id);
        selected_use_blocks.insert(use_block);
        selected_boundaries.insert(head_id);
        selected_boundaries.insert(merge_id);
        plans.push(DeferredValueDiamondPlan {
            head: head_id,
            true_arm: true_id,
            false_arm: false_id,
            merge: merge_id,
            condition: *cond,
            result,
            true_value: true_args[0],
            false_value: false_args[0],
            use_block,
        });
    }

    if plans.is_empty() {
        return;
    }
    let Some(additional_blocks) = plans.len().checked_mul(3) else {
        return;
    };
    let max_block = eu.blocks.keys().map(|id| id.0).max().unwrap_or(0);
    let Some(first_new_block) = max_block.checked_add(1) else {
        return;
    };
    let Some(end_sentinel) = first_new_block.checked_add(additional_blocks) else {
        return;
    };
    if end_sentinel > u32::MAX as usize {
        return;
    }

    for (ordinal, plan) in plans.into_iter().enumerate() {
        let true_id = BlockId(first_new_block + ordinal * 3);
        let false_id = BlockId(first_new_block + ordinal * 3 + 1);
        let continuation_id = BlockId(first_new_block + ordinal * 3 + 2);
        apply_deferred_value_diamond(eu, plan, true_id, false_id, continuation_id);
    }
    debug_assert_eq!(eu.verify_result(), Ok(()));
}

fn has_exact_predecessors(cfg: &SirCfg, block: BlockId, expected: &[BlockId]) -> bool {
    let Some(block) = cfg.block_index(block) else {
        return false;
    };
    let mut actual = cfg.predecessors[block]
        .iter()
        .map(|&predecessor| cfg.block_ids[predecessor])
        .collect::<Vec<_>>();
    let mut expected = expected.to_vec();
    actual.sort_unstable();
    expected.sort_unstable();
    actual == expected
}

fn apply_deferred_value_diamond(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: DeferredValueDiamondPlan,
    true_id: BlockId,
    false_id: BlockId,
    continuation_id: BlockId,
) {
    let true_arm = eu
        .blocks
        .remove(&plan.true_arm)
        .expect("planned deferred true arm must exist");
    let false_arm = eu
        .blocks
        .remove(&plan.false_arm)
        .expect("planned deferred false arm must exist");
    let original_use = eu
        .blocks
        .remove(&plan.use_block)
        .expect("planned deferred use block must exist");

    eu.blocks
        .get_mut(&plan.head)
        .expect("planned deferred head must exist")
        .terminator = SIRTerminator::Jump(plan.merge, Vec::new());
    eu.blocks
        .get_mut(&plan.merge)
        .expect("planned deferred merge must exist")
        .params
        .clear();

    eu.blocks.insert(
        plan.use_block,
        BasicBlock {
            id: plan.use_block,
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Branch {
                cond: plan.condition,
                true_block: (true_id, Vec::new()),
                false_block: (false_id, Vec::new()),
            },
        },
    );
    eu.blocks.insert(
        true_id,
        BasicBlock {
            id: true_id,
            params: Vec::new(),
            instructions: true_arm.instructions,
            terminator: SIRTerminator::Jump(continuation_id, vec![plan.true_value]),
        },
    );
    eu.blocks.insert(
        false_id,
        BasicBlock {
            id: false_id,
            params: Vec::new(),
            instructions: false_arm.instructions,
            terminator: SIRTerminator::Jump(continuation_id, vec![plan.false_value]),
        },
    );
    eu.blocks.insert(
        continuation_id,
        BasicBlock {
            id: continuation_id,
            params: vec![plan.result],
            instructions: original_use.instructions,
            terminator: original_use.terminator,
        },
    );
}

fn form_coupled_store_regions(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let uses = collect_uses(eu);
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable_by_key(|id| id.0);
    let plans = block_ids
        .into_iter()
        .filter_map(|block| best_coupled_store_plan(eu, block, &uses))
        .collect::<Vec<_>>();
    if plans.is_empty() {
        return;
    }

    let Some(additional_blocks) = plans.iter().try_fold(0usize, |total, plan| {
        total.checked_add(plan.conditions.len().checked_mul(2)?.checked_add(1)?)
    }) else {
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
    let max_register = eu.register_map.keys().map(|id| id.0).max().unwrap_or(0);
    let Some(additional_registers) = plans.iter().try_fold(0usize, |total, plan| {
        total.checked_add(plan.conditions.len().checked_mul(2)?)
    }) else {
        return;
    };
    if max_register.checked_add(additional_registers).is_none() {
        return;
    }

    if std::env::var_os("CELOX_PASS_TIMING").is_some() {
        for plan in &plans {
            eprintln!(
                "[coupled-store-region] block={} depth={} stores={} owned={} benefit_scaled={}",
                plan.block_id.0,
                plan.conditions.len(),
                plan.stores.len(),
                plan.placements.len(),
                plan.net_benefit_scaled,
            );
        }
    }

    let mut next_block = first_new_block;
    let mut reg_counter = max_register;
    for plan in plans {
        let consumed = plan.conditions.len() * 2 + 1;
        apply_coupled_store_plan(eu, plan, next_block, &mut reg_counter);
        next_block += consumed;
    }
}

fn best_coupled_store_plan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
) -> Option<CoupledStorePlan> {
    let block = eu.blocks.get(&block_id)?;
    let local_defs = block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| def_reg(instruction).map(|value| (value, index)))
        .collect::<HashMap<_, _>>();
    let mut groups = BTreeMap::<RegisterId, Vec<DistributedStore>>::new();
    for (index, instruction) in block.instructions.iter().enumerate() {
        let SIRInstruction::Store(_, offset, width, source, _, _) = instruction else {
            continue;
        };
        let Some(&mux_index) = local_defs.get(source) else {
            continue;
        };
        let SIRInstruction::Mux(result, condition, true_value, false_value) =
            block.instructions[mux_index]
        else {
            continue;
        };
        if result != *source
            || mux_index >= index
            || *width == 0
            || eu.register_map.get(&condition).map(RegisterType::width) != Some(1)
            || eu
                .register_map
                .get(&true_value)
                .is_none_or(|ty| ty.width() < *width)
            || eu
                .register_map
                .get(&false_value)
                .is_none_or(|ty| ty.width() < *width)
            || offset
                .dynamic_registers()
                .into_iter()
                .flatten()
                .any(|dynamic| dynamic == result)
        {
            continue;
        }
        groups.entry(condition).or_default().push(DistributedStore {
            index,
            mux_index,
            mux_result: result,
            true_value,
            false_value,
        });
    }

    let mut best: Option<CoupledStorePlan> = None;
    for (condition, stores) in groups {
        if stores.len() < 2 {
            continue;
        }
        let Some(candidate) =
            plan_coupled_store_region(eu, block_id, condition, stores, &local_defs, uses)
        else {
            continue;
        };
        if best.as_ref().is_none_or(|current| {
            candidate.net_benefit_scaled > current.net_benefit_scaled
                || candidate.net_benefit_scaled == current.net_benefit_scaled
                    && candidate.conditions < current.conditions
        }) {
            best = Some(candidate);
        }
    }
    best
}

fn plan_coupled_store_region(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    condition: RegisterId,
    mut stores: Vec<DistributedStore>,
    local_defs: &HashMap<RegisterId, usize>,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
) -> Option<CoupledStorePlan> {
    let block = eu.blocks.get(&block_id)?;
    stores.sort_unstable_by_key(|store| store.index);
    let first_store = stores.first()?.index;
    let last_store = stores.last()?.index;
    let store_indices = stores
        .iter()
        .map(|store| store.index)
        .collect::<HashSet<_>>();
    if block
        .instructions
        .iter()
        .enumerate()
        .take(last_store + 1)
        .skip(first_store + 1)
        .any(|(index, instruction)| {
            matches!(instruction, SIRInstruction::Load(..))
                || instruction_has_effect(instruction) && !store_indices.contains(&index)
        })
    {
        return None;
    }

    let output_values = stores
        .iter()
        .map(|store| store.mux_result)
        .collect::<HashSet<_>>();
    if output_values.len() != stores.len() {
        return None;
    }
    for store in &stores {
        if uses
            .get(&store.mux_result)
            .into_iter()
            .flatten()
            .any(|site| {
                !matches!(
                    site,
                    UseSite::Instruction { block, index }
                        if *block == block_id
                            && store_indices.contains(index)
                            && store_source_is(
                                &eu.blocks[block].instructions[*index],
                                store.mux_result,
                            )
                )
            })
        {
            return None;
        }
    }

    // Peel the common outer-to-inner priority spine from all store values at
    // once.  Every selected leaf is a tuple, so a definition shared by result
    // and flags remains owned by that leaf instead of escaping through the
    // other output.
    let mut roots = stores
        .iter()
        .map(|store| store.mux_result)
        .collect::<Vec<_>>();
    let mut conditions = Vec::new();
    let mut leaf_values = Vec::new();
    let mut removable_muxes = HashSet::default();
    loop {
        let mut level_condition = None;
        let mut selected = Vec::with_capacity(roots.len());
        let mut fallthrough = Vec::with_capacity(roots.len());
        let mut level_muxes = Vec::with_capacity(roots.len());
        for root in &roots {
            let Some(&index) = local_defs.get(root) else {
                level_muxes.clear();
                break;
            };
            let SIRInstruction::Mux(dst, mux_condition, true_value, false_value) =
                block.instructions[index]
            else {
                level_muxes.clear();
                break;
            };
            if dst != *root
                || eu.register_map.get(&mux_condition).map(RegisterType::width) != Some(1)
                || level_condition.is_some_and(|expected| expected != mux_condition)
            {
                level_muxes.clear();
                break;
            }
            level_condition = Some(mux_condition);
            level_muxes.push(index);
            selected.push(true_value);
            fallthrough.push(false_value);
        }
        if level_muxes.len() != roots.len() {
            break;
        }
        let level_condition = level_condition?;
        if conditions.is_empty() && level_condition != condition {
            return None;
        }
        conditions.push(level_condition);
        leaf_values.push(selected);
        removable_muxes.extend(level_muxes);
        roots = fallthrough;
    }
    if conditions.is_empty() {
        return None;
    }
    leaf_values.push(roots);

    if removable_muxes.iter().any(|index| {
        let Some(value) = def_reg(&block.instructions[*index]) else {
            return true;
        };
        uses.get(&value).into_iter().flatten().any(|site| {
            !matches!(
                site,
                UseSite::Instruction { block, index }
                    if *block == block_id
                        && (removable_muxes.contains(index)
                            || store_indices.contains(index))
            )
        })
    }) {
        return None;
    }

    let mut masks = HashMap::<usize, Vec<bool>>::default();
    for (leaf, values) in leaf_values.iter().enumerate() {
        mark_coupled_store_leaf_defs(
            values,
            leaf..leaf + 1,
            leaf_values.len(),
            last_store,
            local_defs,
            block,
            &removable_muxes,
            &mut masks,
        );
    }
    // Every store template is emitted in every leaf.  Its dynamic address
    // operands therefore have to dominate the complete region, even when the
    // same calculation is also reachable from one leaf's value cone.  Without
    // this all-leaf mask, that shared calculation could be sunk into only that
    // leaf and leave the duplicated stores in the other leaves with a
    // non-dominating operand.
    for store in &stores {
        let SIRInstruction::Store(_, offset, _, _, _, _) = &block.instructions[store.index] else {
            unreachable!("coupled-store plan must refer to a Store")
        };
        let dynamic_offsets = offset
            .dynamic_registers()
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        mark_coupled_store_leaf_defs(
            &dynamic_offsets,
            0..leaf_values.len(),
            leaf_values.len(),
            last_store,
            local_defs,
            block,
            &removable_muxes,
            &mut masks,
        );
    }
    // A condition at level `n` executes on every path which reaches that
    // decision, namely leaves n..=depth.  Include its defining slice in the
    // same path masks as data values so the nearest-common-dominator
    // placement does not incorrectly classify it as a final-leaf value and
    // then reject the entire dependency chain as a forward branch use.
    for (level, condition) in conditions.iter().copied().enumerate().skip(1) {
        mark_coupled_store_leaf_defs(
            std::slice::from_ref(&condition),
            level..leaf_values.len(),
            leaf_values.len(),
            last_store,
            local_defs,
            block,
            &removable_muxes,
            &mut masks,
        );
    }
    let mut placements = HashMap::default();
    for (index, mask) in masks {
        let mut leaves = mask
            .iter()
            .enumerate()
            .filter_map(|(leaf, needed)| needed.then_some(leaf));
        let Some(leaf) = leaves.next() else {
            continue;
        };
        let site = if leaves.next().is_none() {
            CoupledPlacementSite::Leaf(leaf)
        } else if leaf > 0 {
            // A priority chain's decision `leaf` is the nearest common
            // dominator of every remaining leaf.  Placing the definition
            // there avoids evaluating it on earlier exits without cloning it.
            CoupledPlacementSite::Decision(leaf)
        } else {
            // Every path can need this definition, so it belongs in the head.
            continue;
        };
        placements.insert(index, site);
    }
    close_coupled_store_placements(
        eu,
        block_id,
        &conditions,
        &mut placements,
        &removable_muxes,
        uses,
    );
    if placements.is_empty() {
        return None;
    }

    let mut plan = CoupledStorePlan {
        block_id,
        last_store,
        stores,
        conditions,
        leaf_values,
        placements,
        removable_muxes,
        net_benefit_scaled: 0,
    };
    plan.net_benefit_scaled = coupled_store_net_benefit(eu, block, &plan)?;
    Some(plan)
}

fn coupled_site_dominates(
    definition: CoupledPlacementSite,
    use_site: CoupledPlacementSite,
) -> bool {
    match (definition, use_site) {
        (CoupledPlacementSite::Decision(def), CoupledPlacementSite::Decision(use_)) => use_ >= def,
        (CoupledPlacementSite::Decision(def), CoupledPlacementSite::Leaf(use_)) => use_ >= def,
        (CoupledPlacementSite::Leaf(def), CoupledPlacementSite::Leaf(use_)) => def == use_,
        (CoupledPlacementSite::Leaf(_), CoupledPlacementSite::Decision(_)) => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn mark_coupled_store_leaf_defs(
    roots: &[RegisterId],
    needed_leaves: std::ops::Range<usize>,
    leaf_count: usize,
    last_store: usize,
    local_defs: &HashMap<RegisterId, usize>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    removable_muxes: &HashSet<usize>,
    masks: &mut HashMap<usize, Vec<bool>>,
) {
    let mut visited = HashSet::default();
    let mut worklist = roots.to_vec();
    while let Some(value) = worklist.pop() {
        if !visited.insert(value) {
            continue;
        }
        let Some(&index) = local_defs.get(&value) else {
            continue;
        };
        if index > last_store
            || removable_muxes.contains(&index)
            || !instruction_is_movable(&block.instructions[index])
        {
            continue;
        }
        let mask = masks
            .entry(index)
            .or_insert_with(|| vec![false; leaf_count]);
        mask[needed_leaves.clone()].fill(true);
        worklist.extend(instruction_uses(&block.instructions[index]));
    }
}

fn close_coupled_store_placements(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    conditions: &[RegisterId],
    placements: &mut HashMap<usize, CoupledPlacementSite>,
    removable_muxes: &HashSet<usize>,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
) {
    let block = &eu.blocks[&block_id];
    let mut condition_levels = HashMap::default();
    for (level, condition) in conditions.iter().copied().enumerate() {
        condition_levels
            .entry(condition)
            .and_modify(|earliest: &mut usize| *earliest = (*earliest).min(level))
            .or_insert(level);
    }
    loop {
        let mut rejected = Vec::new();
        for (&index, &definition_site) in placements.iter() {
            let Some(value) = def_reg(&block.instructions[index]) else {
                rejected.push(index);
                continue;
            };
            let escapes = uses
                .get(&value)
                .into_iter()
                .flatten()
                .any(|site| match *site {
                    UseSite::Instruction {
                        block: use_block,
                        index: use_index,
                    } if use_block == block_id => {
                        if removable_muxes.contains(&use_index) {
                            match &block.instructions[use_index] {
                                SIRInstruction::Mux(_, condition, _, _) if *condition == value => {
                                    condition_levels.get(condition).is_none_or(|&level| {
                                        !coupled_site_dominates(
                                            definition_site,
                                            CoupledPlacementSite::Decision(level),
                                        )
                                    })
                                }
                                _ => false,
                            }
                        } else if let Some(&use_site) = placements.get(&use_index) {
                            !coupled_site_dominates(definition_site, use_site)
                        } else {
                            true
                        }
                    }
                    _ => true,
                });
            if escapes {
                rejected.push(index);
            }
        }
        if rejected.is_empty() {
            break;
        }
        for index in rejected {
            placements.remove(&index);
        }
    }
}

fn coupled_store_net_benefit(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &CoupledStorePlan,
) -> Option<u128> {
    const BRANCH_CONTROL_COST: u128 = 3;
    const MISPREDICT_COST: u128 = 16;
    let cost = |index: usize| {
        super::cost_model::estimate_clif_cost(&block.instructions[index], &eu.register_map, false)
            as u128
    };
    let arm_cost = plan
        .placements
        .keys()
        .copied()
        .map(cost)
        .fold(0u128, u128::saturating_add);
    let mux_cost = plan
        .removable_muxes
        .iter()
        .copied()
        .map(cost)
        .fold(0u128, u128::saturating_add);
    let saved_scaled = arm_cost.saturating_add(mux_cost.saturating_mul(2));
    let introduced_scaled = BRANCH_CONTROL_COST
        .saturating_mul(plan.conditions.len() as u128)
        .saturating_mul(2)
        .saturating_add(MISPREDICT_COST.saturating_mul(plan.conditions.len() as u128));
    (saved_scaled > introduced_scaled).then(|| saved_scaled - introduced_scaled)
}

fn apply_coupled_store_plan(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: CoupledStorePlan,
    first_new_block: usize,
    reg_counter: &mut usize,
) {
    let original = eu
        .blocks
        .remove(&plan.block_id)
        .expect("planned coupled-store block must remain present");
    let dead_mux_values = plan
        .removable_muxes
        .iter()
        .filter_map(|index| def_reg(&original.instructions[*index]))
        .collect::<Vec<_>>();
    let store_templates = plan
        .stores
        .iter()
        .map(|store| original.instructions[store.index].clone())
        .collect::<Vec<_>>();
    let mut head = Vec::new();
    let mut leaves = vec![Vec::new(); plan.leaf_values.len()];
    let mut decisions = vec![Vec::new(); plan.conditions.len()];
    let mut continuation = Vec::new();
    for (index, instruction) in original.instructions.into_iter().enumerate() {
        if index > plan.last_store {
            continuation.push(instruction);
        } else if let Some(site) = plan.placements.get(&index) {
            match *site {
                CoupledPlacementSite::Decision(level) => decisions[level].push(instruction),
                CoupledPlacementSite::Leaf(leaf) => leaves[leaf].push(instruction),
            }
        } else if !plan.stores.iter().any(|store| store.index == index)
            && !plan.removable_muxes.contains(&index)
        {
            head.push(instruction);
        }
    }
    for (leaf, values) in leaves.iter_mut().zip(&plan.leaf_values) {
        leaf.extend(
            store_templates
                .iter()
                .zip(values)
                .map(|(store, value)| store_with_source(store, *value)),
        );
    }
    let mut conditions = Vec::with_capacity(plan.conditions.len());
    for (level, condition) in plan.conditions.iter().copied().enumerate() {
        let instructions = if level == 0 {
            &mut head
        } else {
            &mut decisions[level]
        };
        conditions.push(normalize_branch_condition(
            &mut eu.register_map,
            instructions,
            condition,
            reg_counter,
        ));
    }

    let depth = conditions.len();
    let decision_ids = std::iter::once(plan.block_id)
        .chain((0..depth.saturating_sub(1)).map(|index| BlockId(first_new_block + index)))
        .collect::<Vec<_>>();
    let leaf_base = first_new_block + depth.saturating_sub(1);
    let leaf_ids = (0..=depth)
        .map(|index| BlockId(leaf_base + index))
        .collect::<Vec<_>>();
    let merge_id = BlockId(leaf_base + depth + 1);

    eu.blocks.insert(
        plan.block_id,
        BasicBlock {
            id: plan.block_id,
            params: original.params,
            instructions: head,
            terminator: SIRTerminator::Branch {
                cond: conditions[0],
                true_block: (leaf_ids[0], Vec::new()),
                false_block: if depth == 1 {
                    (leaf_ids[1], Vec::new())
                } else {
                    (decision_ids[1], Vec::new())
                },
            },
        },
    );
    for level in 1..depth {
        eu.blocks.insert(
            decision_ids[level],
            BasicBlock {
                id: decision_ids[level],
                params: Vec::new(),
                instructions: std::mem::take(&mut decisions[level]),
                terminator: SIRTerminator::Branch {
                    cond: conditions[level],
                    true_block: (leaf_ids[level], Vec::new()),
                    false_block: if level + 1 == depth {
                        (leaf_ids[depth], Vec::new())
                    } else {
                        (decision_ids[level + 1], Vec::new())
                    },
                },
            },
        );
    }
    for (leaf, instructions) in leaves.into_iter().enumerate() {
        eu.blocks.insert(
            leaf_ids[leaf],
            BasicBlock {
                id: leaf_ids[leaf],
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Jump(merge_id, Vec::new()),
            },
        );
    }
    eu.blocks.insert(
        merge_id,
        BasicBlock {
            id: merge_id,
            params: Vec::new(),
            instructions: continuation,
            terminator: original.terminator,
        },
    );
    for value in dead_mux_values {
        eu.register_map.remove(&value);
    }
    debug_assert_eq!(eu.verify_result(), Ok(()));
}

fn form_same_predicate_regions(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let uses = collect_uses(eu);
    let mut input_blocks = eu.blocks.keys().copied().collect::<Vec<_>>();
    input_blocks.sort_unstable_by_key(|id| id.0);
    let plans = input_blocks
        .into_iter()
        .filter_map(|block| best_same_predicate_plan(eu, block, &uses))
        .collect::<Vec<_>>();
    if plans.is_empty() {
        return;
    }

    // A region adds true, false, and merge blocks. Reserve all IDs before
    // changing the EU so overflow leaves the input untouched.
    let Some(additional_blocks) = plans.len().checked_mul(3) else {
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

    // Every moved or cofactored instruction is rebuilt with one edge-local
    // destination. Branch normalization can allocate at most two more values
    // per plan. Check the complete register range before applying any plan.
    let additional_registers = plans.iter().try_fold(0usize, |total, plan| {
        total
            .checked_add(plan.true_owned.len())?
            .checked_add(plan.false_owned.len())?
            .checked_add(plan.true_cofactor.len())?
            .checked_add(plan.false_cofactor.len())?
            // One false-edge zero is shared by every implicit gated-And in a
            // region. Explicit-Mux-only regions leave this reservation unused.
            .checked_add(1)?
            .checked_add(2)
    });
    let Some(additional_registers) = additional_registers else {
        return;
    };
    let max_register = eu.register_map.keys().map(|id| id.0).max().unwrap_or(0);
    if max_register.checked_add(additional_registers).is_none() {
        return;
    }

    if std::env::var_os("CELOX_PASS_TIMING").is_some() {
        let muxes = plans.iter().map(|plan| plan.muxes.len()).sum::<usize>();
        let true_owned = plans
            .iter()
            .map(|plan| plan.true_owned.len())
            .sum::<usize>();
        let false_owned = plans
            .iter()
            .map(|plan| plan.false_owned.len())
            .sum::<usize>();
        let live_outs = plans.iter().map(|plan| plan.live_outs.len()).sum::<usize>();
        eprintln!(
            "[same-predicate-regions] regions={} muxes={muxes} true_owned={true_owned} false_owned={false_owned} live_outs={live_outs}",
            plans.len(),
        );
        for plan in &plans {
            eprintln!(
                "[same-predicate-region] block={} cond=r{} segment={}..{} muxes={} true_owned={} false_owned={} true_cofactor={} false_cofactor={} live_outs={} benefit_scaled={}",
                plan.block_id.0,
                plan.condition.0,
                plan.segment_start,
                plan.segment_end,
                plan.muxes.len(),
                plan.true_owned.len(),
                plan.false_owned.len(),
                plan.true_cofactor.len(),
                plan.false_cofactor.len(),
                plan.live_outs.len(),
                plan.net_benefit_scaled,
            );
        }
    }

    let mut reg_counter = max_register;
    for (ordinal, plan) in plans.into_iter().enumerate() {
        let true_id = BlockId(first_new_block + ordinal * 3);
        let false_id = BlockId(first_new_block + ordinal * 3 + 1);
        let merge_id = BlockId(first_new_block + ordinal * 3 + 2);
        apply_same_predicate_plan(eu, plan, true_id, false_id, merge_id, &mut reg_counter);
    }
}

fn best_same_predicate_plan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
) -> Option<SamePredicatePlan> {
    let block = eu.blocks.get(&block_id)?;
    let mut best: Option<SamePredicatePlan> = None;
    let mut segment_start = 0usize;
    while segment_start < block.instructions.len() {
        while segment_start < block.instructions.len()
            && !instruction_is_same_predicate_region_value(&block.instructions[segment_start])
        {
            segment_start += 1;
        }
        if segment_start == block.instructions.len() {
            break;
        }
        let mut segment_end = segment_start;
        while segment_end < block.instructions.len()
            && instruction_is_same_predicate_region_value(&block.instructions[segment_end])
        {
            segment_end += 1;
        }

        let mut groups = BTreeMap::<RegisterId, Vec<usize>>::new();
        for index in segment_start..segment_end {
            for condition in conditional_source_conditions(eu, &block.instructions[index]) {
                groups.entry(condition).or_default().push(index);
            }
        }
        for (condition, muxes) in groups {
            // One Mux is already handled by ordinary cost-directed lowering;
            // this transform exists to share one branch across a region.
            if muxes.len() < 2 {
                continue;
            }
            let Some(candidate) = plan_same_predicate_region(
                eu,
                block_id,
                segment_start,
                segment_end,
                condition,
                &muxes,
                uses,
            ) else {
                continue;
            };
            let replace = best.as_ref().is_none_or(|current| {
                candidate.net_benefit_scaled > current.net_benefit_scaled
                    || candidate.net_benefit_scaled == current.net_benefit_scaled
                        && (candidate.segment_start, candidate.condition)
                            < (current.segment_start, current.condition)
            });
            if replace {
                best = Some(candidate);
            }
        }
        segment_start = segment_end.saturating_add(1);
    }
    best
}

#[allow(clippy::too_many_arguments)]
fn plan_same_predicate_region(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    segment_start: usize,
    segment_end: usize,
    condition: RegisterId,
    mux_indices: &[usize],
    uses: &HashMap<RegisterId, Vec<UseSite>>,
) -> Option<SamePredicatePlan> {
    let block = eu.blocks.get(&block_id)?;
    if eu.register_map.get(&condition).map(RegisterType::width) != Some(1) {
        return None;
    }
    let local_defs = block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(index, inst)| def_reg(inst).map(|reg| (reg, index)))
        .collect::<HashMap<_, _>>();
    let mut muxes = mux_indices.iter().copied().collect::<HashSet<_>>();

    // Forward-cofactor every pure value which depends on a group Mux. This is
    // what turns many scalar Mux outputs which later reconverge in a Concat
    // into a small live-out frontier instead of one block parameter per Mux.
    let mut affected = HashSet::<RegisterId>::default();
    let mut cofactor = HashSet::<usize>::default();
    for index in segment_start..segment_end {
        let inst = &block.instructions[index];
        let dst = def_reg(inst)?;
        if muxes.contains(&index) {
            affected.insert(dst);
        } else if instruction_uses(inst)
            .into_iter()
            .any(|operand| affected.contains(&operand))
        {
            affected.insert(dst);
            cofactor.insert(index);
        }
    }

    let mut live_outs = affected
        .iter()
        .copied()
        .filter(|value| {
            uses.get(value).into_iter().flatten().any(|site| {
                !matches!(
                    site,
                    UseSite::Instruction { block, index }
                        if *block == block_id
                            && (muxes.contains(index) || cofactor.contains(index))
                )
            })
        })
        .collect::<Vec<_>>();
    live_outs.sort_unstable();
    live_outs.dedup();
    if live_outs.is_empty() {
        return None;
    }

    // Specialize the forward slice separately on each edge. Besides explicit
    // Muxes, a one-bit `condition & payload` is an implicit
    // `Mux(condition, payload, 0)` in two-state mode. Recognizing it here
    // recovers the control dependence without first materializing another
    // select instruction.
    let (true_muxes, true_cofactor) = specialize_region_needed(
        eu,
        condition,
        &live_outs,
        true,
        &muxes,
        &cofactor,
        &local_defs,
        block,
    );
    let (false_muxes, false_cofactor) = specialize_region_needed(
        eu,
        condition,
        &live_outs,
        false,
        &muxes,
        &cofactor,
        &local_defs,
        block,
    );
    muxes = true_muxes
        .union(&false_muxes)
        .copied()
        .collect::<HashSet<_>>();
    cofactor = true_cofactor
        .union(&false_cofactor)
        .copied()
        .collect::<HashSet<_>>();
    if muxes.len() < 2 {
        return None;
    }

    let mut true_roots = Vec::with_capacity(true_muxes.len());
    let mut false_roots = Vec::with_capacity(false_muxes.len());
    for &index in &muxes {
        let source = conditional_source(eu, &block.instructions[index], condition)?;
        if source.condition != condition {
            return None;
        }
        if true_muxes.contains(&index) {
            if let ConditionalArm::Value(value) = source.true_arm {
                true_roots.push(value);
            }
        }
        if false_muxes.contains(&index) {
            if let ConditionalArm::Value(value) = source.false_arm {
                false_roots.push(value);
            }
        }
    }
    let true_reachable = collect_region_reachable_defs(
        &true_roots,
        segment_start,
        segment_end,
        &local_defs,
        block,
        &muxes,
    );
    let false_reachable = collect_region_reachable_defs(
        &false_roots,
        segment_start,
        segment_end,
        &local_defs,
        block,
        &muxes,
    );
    let shared = true_reachable
        .intersection(&false_reachable)
        .copied()
        .collect::<HashSet<_>>();
    let removed_forward = muxes
        .iter()
        .chain(cofactor.iter())
        .copied()
        .collect::<HashSet<_>>();
    let mut true_owned = true_reachable
        .difference(&shared)
        .copied()
        .filter(|index| !removed_forward.contains(index))
        .collect::<HashSet<_>>();
    let mut false_owned = false_reachable
        .difference(&shared)
        .copied()
        .filter(|index| !removed_forward.contains(index))
        .collect::<HashSet<_>>();
    close_region_arm(
        eu,
        condition,
        &mut true_owned,
        true,
        block_id,
        block,
        &true_muxes,
        uses,
    );
    close_region_arm(
        eu,
        condition,
        &mut false_owned,
        false,
        block_id,
        block,
        &false_muxes,
        uses,
    );
    if !true_owned.is_disjoint(&false_owned) {
        return None;
    }

    let plan = SamePredicatePlan {
        block_id,
        segment_start,
        segment_end,
        condition,
        muxes,
        true_muxes,
        false_muxes,
        true_owned,
        false_owned,
        cofactor,
        true_cofactor,
        false_cofactor,
        live_outs,
        net_benefit_scaled: 0,
    };
    if !same_predicate_arm_is_closed(eu, block, &plan, true)
        || !same_predicate_arm_is_closed(eu, block, &plan, false)
    {
        return None;
    }
    let net_benefit_scaled = same_predicate_net_benefit(eu, block, &plan)?;
    Some(SamePredicatePlan {
        net_benefit_scaled,
        ..plan
    })
}

fn collect_region_reachable_defs(
    roots: &[RegisterId],
    segment_start: usize,
    segment_end: usize,
    local_defs: &HashMap<RegisterId, usize>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    source_muxes: &HashSet<usize>,
) -> HashSet<usize> {
    let mut result = HashSet::default();
    let mut work = roots.to_vec();
    let mut visited = HashSet::default();
    while let Some(value) = work.pop() {
        if !visited.insert(value) {
            continue;
        }
        let Some(&index) = local_defs.get(&value) else {
            continue;
        };
        if index < segment_start || index >= segment_end || source_muxes.contains(&index) {
            continue;
        }
        if result.insert(index) {
            work.extend(instruction_uses(&block.instructions[index]));
        }
    }
    result
}

fn specialize_region_needed(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    condition: RegisterId,
    live_outs: &[RegisterId],
    true_arm: bool,
    source_muxes: &HashSet<usize>,
    cofactor: &HashSet<usize>,
    local_defs: &HashMap<RegisterId, usize>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
) -> (HashSet<usize>, HashSet<usize>) {
    let mut needed_muxes = HashSet::default();
    let mut needed_cofactor = HashSet::default();
    let mut work = live_outs.to_vec();
    let mut visited = HashSet::default();
    while let Some(value) = work.pop() {
        if !visited.insert(value) {
            continue;
        }
        let Some(&index) = local_defs.get(&value) else {
            continue;
        };
        if source_muxes.contains(&index) {
            needed_muxes.insert(index);
            let Some(source) = conditional_source(eu, &block.instructions[index], condition) else {
                continue;
            };
            if let ConditionalArm::Value(value) = source.selected(true_arm) {
                work.push(value);
            }
        } else if cofactor.contains(&index) && needed_cofactor.insert(index) {
            work.extend(instruction_uses(&block.instructions[index]));
        }
    }
    (needed_muxes, needed_cofactor)
}

fn close_region_arm(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    condition: RegisterId,
    owned: &mut HashSet<usize>,
    true_arm: bool,
    block_id: BlockId,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    source_muxes: &HashSet<usize>,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
) {
    loop {
        let rejected = owned
            .iter()
            .copied()
            .filter(|index| {
                let Some(dst) = def_reg(&block.instructions[*index]) else {
                    return true;
                };
                uses.get(&dst)
                    .into_iter()
                    .flatten()
                    .any(|site| match *site {
                        UseSite::Instruction {
                            block: use_block,
                            index: use_index,
                        } if use_block == block_id => {
                            if owned.contains(&use_index) {
                                return false;
                            }
                            if !source_muxes.contains(&use_index) {
                                return true;
                            }
                            let Some(source) = conditional_source(
                                eu,
                                &block.instructions[use_index],
                                condition,
                            ) else {
                                return true;
                            };
                            source.condition == dst
                                || !matches!(source.selected(true_arm), ConditionalArm::Value(value) if value == dst)
                        }
                        _ => true,
                    })
            })
            .collect::<Vec<_>>();
        if rejected.is_empty() {
            break;
        }
        for index in rejected {
            owned.remove(&index);
        }
    }
}

fn same_predicate_arm_is_closed(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &SamePredicatePlan,
    true_arm: bool,
) -> bool {
    let owned = if true_arm {
        &plan.true_owned
    } else {
        &plan.false_owned
    };
    let arm_muxes = if true_arm {
        &plan.true_muxes
    } else {
        &plan.false_muxes
    };
    let arm_cofactor = if true_arm {
        &plan.true_cofactor
    } else {
        &plan.false_cofactor
    };
    let removed = plan
        .true_owned
        .iter()
        .chain(plan.false_owned.iter())
        .chain(plan.muxes.iter())
        .chain(plan.cofactor.iter())
        .filter_map(|index| def_reg(&block.instructions[*index]))
        .collect::<HashSet<_>>();
    let mut mapped = HashSet::default();
    for index in plan.segment_start..plan.segment_end {
        if owned.contains(&index) || arm_cofactor.contains(&index) {
            if instruction_uses(&block.instructions[index])
                .into_iter()
                .any(|operand| removed.contains(&operand) && !mapped.contains(&operand))
            {
                return false;
            }
            if let Some(dst) = def_reg(&block.instructions[index]) {
                mapped.insert(dst);
            }
        } else if arm_muxes.contains(&index) {
            let Some(source) = conditional_source(eu, &block.instructions[index], plan.condition)
            else {
                return false;
            };
            if let ConditionalArm::Value(selected) = source.selected(true_arm) {
                if removed.contains(&selected) && !mapped.contains(&selected) {
                    return false;
                }
            }
            mapped.insert(source.dst);
        }
    }
    plan.live_outs.iter().all(|value| mapped.contains(value))
}

fn register_chunks(register_map: &HashMap<RegisterId, RegisterType>, value: RegisterId) -> u128 {
    register_map
        .get(&value)
        .map(|ty| ty.width().div_ceil(64).max(1) as u128)
        .unwrap_or(1)
}

fn same_predicate_net_benefit(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &SamePredicatePlan,
) -> Option<u128> {
    const BRANCH_CONTROL_COST: u128 = 3;
    const MISPREDICT_COST: u128 = 16;
    const PHI_COPY_COST_PER_CHUNK: u128 = 2;
    const LIVE_THROUGH_COST_PER_CHUNK: u128 = 1;

    let instruction_cost = |index: usize| {
        super::cost_model::estimate_clif_cost(&block.instructions[index], &eu.register_map, false)
            as u128
    };
    let true_cost = plan
        .true_owned
        .iter()
        .copied()
        .map(instruction_cost)
        .fold(0u128, u128::saturating_add);
    let false_cost = plan
        .false_owned
        .iter()
        .copied()
        .map(instruction_cost)
        .fold(0u128, u128::saturating_add);
    let mux_cost = plan
        .muxes
        .iter()
        .copied()
        .map(instruction_cost)
        .fold(0u128, u128::saturating_add);

    let region_defs = plan
        .true_owned
        .iter()
        .chain(plan.false_owned.iter())
        .chain(plan.muxes.iter())
        .chain(plan.cofactor.iter())
        .filter_map(|index| def_reg(&block.instructions[*index]))
        .collect::<HashSet<_>>();
    let mut live_through = HashSet::default();
    for &index in plan
        .true_owned
        .iter()
        .chain(plan.false_owned.iter())
        .chain(plan.cofactor.iter())
    {
        for operand in instruction_uses(&block.instructions[index]) {
            if !region_defs.contains(&operand) {
                live_through.insert(operand);
            }
        }
    }
    let live_through_chunks = live_through
        .into_iter()
        .map(|value| register_chunks(&eu.register_map, value))
        .fold(0u128, u128::saturating_add);
    let phi_chunks = plan
        .live_outs
        .iter()
        .copied()
        .map(|value| register_chunks(&eu.register_map, value))
        .fold(0u128, u128::saturating_add);

    // Exact 50/50 integer expected-cost comparison. All values are scaled by
    // two: each arm is skipped on one edge, every removed Mux is saved on both,
    // and one of the two equally-likely outcomes pays the modeled miss.
    let saved_scaled = true_cost
        .saturating_add(false_cost)
        .saturating_add(mux_cost.saturating_mul(2));
    let introduced_scaled = BRANCH_CONTROL_COST
        .saturating_add(phi_chunks.saturating_mul(PHI_COPY_COST_PER_CHUNK))
        .saturating_add(live_through_chunks.saturating_mul(LIVE_THROUGH_COST_PER_CHUNK))
        .saturating_mul(2)
        .saturating_add(MISPREDICT_COST);
    (saved_scaled > introduced_scaled).then(|| saved_scaled - introduced_scaled)
}

fn apply_same_predicate_plan(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: SamePredicatePlan,
    true_id: BlockId,
    false_id: BlockId,
    merge_id: BlockId,
    reg_counter: &mut usize,
) {
    let original = eu
        .blocks
        .remove(&plan.block_id)
        .expect("planned same-predicate block must remain present");
    let mut head_instructions = original.instructions[..plan.segment_start].to_vec();
    for index in plan.segment_start..plan.segment_end {
        if !plan.true_owned.contains(&index)
            && !plan.false_owned.contains(&index)
            && !plan.muxes.contains(&index)
            && !plan.cofactor.contains(&index)
        {
            head_instructions.push(original.instructions[index].clone());
        }
    }
    let merge_instructions = original.instructions[plan.segment_end..].to_vec();

    // These original SSA definitions are replaced by edge-local clones or by
    // merge parameters. Keeping the dead IDs in register_map is not benign:
    // native isel allocates a VReg for every entry before it sees any use.
    let live_outs = plan.live_outs.iter().copied().collect::<HashSet<_>>();
    let dead_originals = plan
        .true_owned
        .iter()
        .chain(plan.false_owned.iter())
        .chain(plan.muxes.iter())
        .chain(plan.cofactor.iter())
        .filter_map(|index| def_reg(&original.instructions[*index]))
        .filter(|value| !live_outs.contains(value))
        .collect::<HashSet<_>>();

    let (true_instructions, true_arguments) =
        build_same_predicate_arm(eu, &original, &plan, true, reg_counter);
    let (false_instructions, false_arguments) =
        build_same_predicate_arm(eu, &original, &plan, false, reg_counter);
    let branch_condition = normalize_branch_condition(
        &mut eu.register_map,
        &mut head_instructions,
        plan.condition,
        reg_counter,
    );

    eu.blocks.insert(
        plan.block_id,
        BasicBlock {
            id: plan.block_id,
            params: original.params,
            instructions: head_instructions,
            terminator: SIRTerminator::Branch {
                cond: branch_condition,
                true_block: (true_id, Vec::new()),
                false_block: (false_id, Vec::new()),
            },
        },
    );
    eu.blocks.insert(
        true_id,
        BasicBlock {
            id: true_id,
            params: Vec::new(),
            instructions: true_instructions,
            terminator: SIRTerminator::Jump(merge_id, true_arguments),
        },
    );
    eu.blocks.insert(
        false_id,
        BasicBlock {
            id: false_id,
            params: Vec::new(),
            instructions: false_instructions,
            terminator: SIRTerminator::Jump(merge_id, false_arguments),
        },
    );
    eu.blocks.insert(
        merge_id,
        BasicBlock {
            id: merge_id,
            params: plan.live_outs,
            instructions: merge_instructions,
            terminator: original.terminator,
        },
    );
    for value in dead_originals {
        eu.register_map.remove(&value);
    }
}

fn build_same_predicate_arm(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    original: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &SamePredicatePlan,
    true_arm: bool,
    reg_counter: &mut usize,
) -> (Vec<SIRInstruction<RegionedAbsoluteAddr>>, Vec<RegisterId>) {
    let owned = if true_arm {
        &plan.true_owned
    } else {
        &plan.false_owned
    };
    let arm_muxes = if true_arm {
        &plan.true_muxes
    } else {
        &plan.false_muxes
    };
    let arm_cofactor = if true_arm {
        &plan.true_cofactor
    } else {
        &plan.false_cofactor
    };
    let mut instructions = Vec::new();
    let mut replacements = HashMap::<RegisterId, RegisterId>::default();
    let mut zero = None;
    for index in plan.segment_start..plan.segment_end {
        let inst = &original.instructions[index];
        if owned.contains(&index) || arm_cofactor.contains(&index) {
            let old_dst = def_reg(inst).expect("pure region instruction must define a value");
            *reg_counter += 1;
            let new_dst = RegisterId(*reg_counter);
            eu.register_map
                .insert(new_dst, eu.register_map[&old_dst].clone());
            instructions.push(
                clone_pure_instruction(inst, new_dst, &replacements)
                    .expect("planned region contains only pure instructions"),
            );
            replacements.insert(old_dst, new_dst);
        } else if arm_muxes.contains(&index) {
            let source = conditional_source(eu, inst, plan.condition)
                .expect("same-predicate source must remain conditional");
            let selected = match source.selected(true_arm) {
                ConditionalArm::Value(value) => replacements.get(&value).copied().unwrap_or(value),
                ConditionalArm::Zero => *zero.get_or_insert_with(|| {
                    *reg_counter += 1;
                    let value = RegisterId(*reg_counter);
                    eu.register_map
                        .insert(value, eu.register_map[&source.dst].clone());
                    instructions.push(SIRInstruction::Imm(value, SIRValue::new(0u8)));
                    value
                }),
            };
            replacements.insert(source.dst, selected);
        }
    }
    let arguments = plan
        .live_outs
        .iter()
        .map(|value| {
            *replacements
                .get(value)
                .expect("closed cofactor must define every live-out on both edges")
        })
        .collect();
    (instructions, arguments)
}

fn clone_pure_instruction(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    dst: RegisterId,
    replacements: &HashMap<RegisterId, RegisterId>,
) -> Option<SIRInstruction<RegionedAbsoluteAddr>> {
    let mapped = |value: RegisterId| replacements.get(&value).copied().unwrap_or(value);
    Some(match inst {
        SIRInstruction::Imm(_, value) => SIRInstruction::Imm(dst, value.clone()),
        SIRInstruction::Binary(_, lhs, op, rhs) => {
            SIRInstruction::Binary(dst, mapped(*lhs), *op, mapped(*rhs))
        }
        SIRInstruction::Unary(_, op, source) => SIRInstruction::Unary(dst, *op, mapped(*source)),
        SIRInstruction::Concat(_, args) => {
            SIRInstruction::Concat(dst, args.iter().copied().map(mapped).collect())
        }
        SIRInstruction::Slice(_, source, lsb, width) => {
            SIRInstruction::Slice(dst, mapped(*source), *lsb, *width)
        }
        SIRInstruction::Mux(_, condition, true_value, false_value) => SIRInstruction::Mux(
            dst,
            mapped(*condition),
            mapped(*true_value),
            mapped(*false_value),
        ),
        SIRInstruction::Load(_, address, offset, width) => SIRInstruction::Load(
            dst,
            *address,
            match offset {
                SIROffset::Static(offset) => SIROffset::Static(*offset),
                SIROffset::Dynamic(offset) => SIROffset::Dynamic(mapped(*offset)),
                SIROffset::Element {
                    index,
                    element_width,
                    bit_offset,
                    dynamic_bit_offset,
                } => SIROffset::Element {
                    index: mapped(*index),
                    element_width: *element_width,
                    bit_offset: *bit_offset,
                    dynamic_bit_offset: dynamic_bit_offset.map(mapped),
                },
                SIROffset::PackedElements {
                    bit_offset,
                    element_width,
                } => SIROffset::PackedElements {
                    bit_offset: *bit_offset,
                    element_width: *element_width,
                },
            },
            *width,
        ),
        SIRInstruction::Store(..)
        | SIRInstruction::Commit(..)
        | SIRInstruction::RuntimeEvent { .. }
        | SIRInstruction::CombCaptureEvent { .. }
        | SIRInstruction::CombCaptureEnableIfChanged { .. } => return None,
    })
}

fn instruction_is_same_predicate_region_value(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    instruction_is_movable(inst) || matches!(inst, SIRInstruction::Load(..))
}

fn plan_block(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    cfg: &SirCfg,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
) -> Option<GuardedRegionPlan> {
    let block = &eu.blocks[&block_id];
    let cost = |plan: &GuardedRegionPlan| {
        plan.moved
            .iter()
            .map(|&index| {
                super::cost_model::estimate_clif_cost(
                    &block.instructions[index],
                    &eu.register_map,
                    false,
                ) as u128
            })
            .fold(0u128, u128::saturating_add)
    };
    // Every rewrite inserts one edge block on the executed path.  A lone
    // constant/copy cannot repay that control transfer even though it forms a
    // technically closed region.
    let true_plan =
        plan_block_for_edge(eu, block_id, cfg, uses, true).filter(|plan| cost(plan) > 1);
    let false_plan =
        plan_block_for_edge(eu, block_id, cfg, uses, false).filter(|plan| cost(plan) > 1);
    match (true_plan, false_plan) {
        (Some(true_plan), Some(false_plan)) => {
            if cost(&false_plan) > cost(&true_plan) {
                Some(false_plan)
            } else {
                Some(true_plan)
            }
        }
        (Some(plan), None) | (None, Some(plan)) => Some(plan),
        (None, None) => None,
    }
}

fn plan_block_for_edge(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    cfg: &SirCfg,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
    selected_true: bool,
) -> Option<GuardedRegionPlan> {
    let block = eu.blocks.get(&block_id)?;
    let SIRTerminator::Branch {
        cond,
        true_block,
        false_block,
    } = &block.terminator
    else {
        return None;
    };
    let block_index = cfg.block_index(block_id)?;
    let selected_target = if selected_true {
        true_block
    } else {
        false_block
    };
    let selected_index = cfg.block_index(selected_target.0)?;
    if eu.register_map.get(cond).map(RegisterType::width) != Some(1)
        || true_block.0 == false_block.0
        || selected_target.0 == eu.entry_block_id
        || cfg.dominates(selected_target.0, block_id)
        || cfg.predecessors[selected_index].as_slice() != [block_index]
    {
        return None;
    }

    let local_defs = block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(index, inst)| def_reg(inst).map(|reg| (reg, index)))
        .collect::<HashMap<_, _>>();
    let mut distributed = Vec::new();
    for (index, inst) in block.instructions.iter().enumerate() {
        let SIRInstruction::Store(_, offset, width, source, _, _) = inst else {
            continue;
        };
        let Some(&mux_index) = local_defs.get(source) else {
            continue;
        };
        let SIRInstruction::Mux(result, mux_cond, true_value, false_value) =
            block.instructions[mux_index]
        else {
            continue;
        };
        if result == *source
            && mux_cond == *cond
            && mux_index < index
            && *width != 0
            && eu
                .register_map
                .get(&true_value)
                .is_some_and(|ty| ty.width() >= *width)
            && eu
                .register_map
                .get(&false_value)
                .is_some_and(|ty| ty.width() >= *width)
            && !offset
                .dynamic_registers()
                .into_iter()
                .flatten()
                .any(|dynamic| dynamic == result)
        {
            distributed.push(DistributedStore {
                index,
                mux_index,
                mux_result: result,
                true_value,
                false_value,
            });
        }
    }
    if distributed.is_empty() {
        return None;
    }
    distributed.sort_unstable_by_key(|store| store.index);
    let distributed_indices = distributed
        .iter()
        .map(|store| store.index)
        .collect::<HashSet<_>>();
    let first_distributed = distributed.first()?.index;
    if block
        .instructions
        .iter()
        .enumerate()
        .skip(first_distributed + 1)
        .any(|(index, inst)| {
            matches!(inst, SIRInstruction::Load(..))
                || instruction_has_effect(inst) && !distributed_indices.contains(&index)
        })
    {
        return None;
    }

    let removable_muxes = distributed
        .iter()
        .map(|store| store.mux_index)
        .collect::<HashSet<_>>();
    for store in &distributed {
        if uses
            .get(&store.mux_result)
            .into_iter()
            .flatten()
            .any(|site| {
                !matches!(
                    site,
                    UseSite::Instruction { block, index }
                        if *block == block_id
                            && distributed_indices.contains(index)
                            && store_source_is(
                                &eu.blocks[block].instructions[*index],
                                store.mux_result,
                            )
                )
            })
        {
            return None;
        }
    }

    let safe_to_move = block
        .instructions
        .iter()
        .map(instruction_is_movable)
        .collect::<Vec<_>>();
    let can_move = compute_moveable_definitions(
        eu,
        block,
        *cond,
        selected_target.0,
        selected_true,
        &local_defs,
        uses,
        cfg,
        &distributed,
        &removable_muxes,
        &safe_to_move,
    );

    let mut seeds = VecDeque::new();
    for store in &distributed {
        let selected_value = if selected_true {
            store.true_value
        } else {
            store.false_value
        };
        if local_defs.contains_key(&selected_value) {
            seeds.push_back(selected_value);
        }
    }
    seeds.extend(
        selected_target
            .1
            .iter()
            .copied()
            .filter(|reg| local_defs.contains_key(reg)),
    );
    for &reg in local_defs.keys() {
        if uses
            .get(&reg)
            .into_iter()
            .flatten()
            .any(|site| site.block() != block_id && cfg.dominates(selected_target.0, site.block()))
        {
            seeds.push_back(reg);
        }
    }

    let mut moved = HashSet::default();
    while let Some(reg) = seeds.pop_front() {
        if reg == *cond {
            continue;
        }
        let Some(&index) = local_defs.get(&reg) else {
            continue;
        };
        if removable_muxes.contains(&index) || !can_move[index] || !moved.insert(index) {
            continue;
        }

        for operand in instruction_uses(&block.instructions[index]) {
            if operand != *cond
                && let Some(&operand_index) = local_defs.get(&operand)
                && can_move[operand_index]
            {
                seeds.push_back(operand);
            }
        }
        for site in uses.get(&reg).into_iter().flatten() {
            if let UseSite::Instruction {
                block: use_block,
                index: use_index,
            } = *site
                && use_block == block_id
                && !removable_muxes.contains(&use_index)
                && let Some(user) = def_reg(&block.instructions[use_index])
                && can_move[use_index]
            {
                seeds.push_back(user);
            }
        }
    }
    if moved.is_empty() {
        return None;
    }

    // The unselected store value and dynamic store offset must remain available on
    // both edges.  The source mux itself is the only store operand removed.
    for store in &distributed {
        let unselected_value = if selected_true {
            store.false_value
        } else {
            store.true_value
        };
        if local_defs
            .get(&unselected_value)
            .is_some_and(|index| moved.contains(index))
        {
            return None;
        }
        if let SIRInstruction::Store(_, offset, _, _, _, _) = &block.instructions[store.index] {
            for offset in offset.dynamic_registers().into_iter().flatten() {
                if local_defs
                    .get(&offset)
                    .is_some_and(|index| moved.contains(index) || removable_muxes.contains(index))
                {
                    return None;
                }
            }
        }
    }

    // Recheck the selected closed region explicitly.  This is deliberately
    // redundant with `can_move`: it keeps application independent from the
    // fixed-point implementation and makes every external use proof local to
    // the completed plan.
    for &index in &moved {
        let dst = def_reg(&block.instructions[index])?;
        if uses.get(&dst).into_iter().flatten().any(|site| {
            !use_is_owned_by_selected_edge(
                *site,
                dst,
                block_id,
                selected_target.0,
                selected_true,
                block,
                &moved,
                &distributed,
                &removable_muxes,
                cfg,
            )
        }) {
            return None;
        }
    }

    Some(GuardedRegionPlan {
        block_id,
        condition: *cond,
        true_target: true_block.clone(),
        false_target: false_block.clone(),
        selected_true,
        moved,
        distributed,
        removable_muxes,
    })
}

#[allow(clippy::too_many_arguments)]
fn compute_moveable_definitions(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    condition: RegisterId,
    selected_target: BlockId,
    selected_true: bool,
    local_defs: &HashMap<RegisterId, usize>,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
    cfg: &SirCfg,
    distributed: &[DistributedStore],
    removable_muxes: &HashSet<usize>,
    safe_to_move: &[bool],
) -> Vec<bool> {
    let mut result = vec![false; block.instructions.len()];
    for index in (0..block.instructions.len()).rev() {
        let Some(dst) = def_reg(&block.instructions[index]) else {
            continue;
        };
        if dst == condition || removable_muxes.contains(&index) || !safe_to_move[index] {
            continue;
        }
        result[index] = uses.get(&dst).into_iter().flatten().all(|site| {
            use_can_follow_selected_edge(
                eu,
                *site,
                dst,
                block.id,
                selected_target,
                selected_true,
                local_defs,
                &result,
                distributed,
                removable_muxes,
                cfg,
            )
        });
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn use_can_follow_selected_edge(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    site: UseSite,
    value: RegisterId,
    source_block: BlockId,
    selected_target: BlockId,
    selected_true: bool,
    _local_defs: &HashMap<RegisterId, usize>,
    moveable: &[bool],
    distributed: &[DistributedStore],
    removable_muxes: &HashSet<usize>,
    cfg: &SirCfg,
) -> bool {
    match site {
        UseSite::Instruction { block, index } if block == source_block => {
            if removable_muxes.contains(&index) {
                return removable_mux_selected_value(eu, source_block, index, selected_true)
                    == Some(value);
            }
            def_reg(&eu.blocks[&block].instructions[index])
                .is_some_and(|_| moveable.get(index).copied().unwrap_or(false))
                || distributed.iter().any(|store| {
                    let selected_value = if selected_true {
                        store.true_value
                    } else {
                        store.false_value
                    };
                    let unselected_value = if selected_true {
                        store.false_value
                    } else {
                        store.true_value
                    };
                    store.index == index && selected_value == value && unselected_value != value
                })
        }
        UseSite::TrueEdgeArgument { block } if block == source_block => selected_true,
        UseSite::FalseEdgeArgument { block } if block == source_block => !selected_true,
        UseSite::BranchCondition { block } | UseSite::JumpArgument { block }
            if block == source_block =>
        {
            false
        }
        _ => cfg.dominates(selected_target, site.block()),
    }
}

#[allow(clippy::too_many_arguments)]
fn use_is_owned_by_selected_edge(
    site: UseSite,
    value: RegisterId,
    source_block: BlockId,
    selected_target: BlockId,
    selected_true: bool,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    moved: &HashSet<usize>,
    distributed: &[DistributedStore],
    removable_muxes: &HashSet<usize>,
    cfg: &SirCfg,
) -> bool {
    match site {
        UseSite::Instruction {
            block: use_block,
            index,
        } if use_block == source_block => {
            moved.contains(&index)
                || removable_muxes.contains(&index)
                    && removable_mux_selected_value_in_block(block, index, selected_true)
                        == Some(value)
                || distributed.iter().any(|store| {
                    let selected_value = if selected_true {
                        store.true_value
                    } else {
                        store.false_value
                    };
                    let unselected_value = if selected_true {
                        store.false_value
                    } else {
                        store.true_value
                    };
                    store.index == index && selected_value == value && unselected_value != value
                })
        }
        UseSite::TrueEdgeArgument { block } if block == source_block => selected_true,
        UseSite::FalseEdgeArgument { block } if block == source_block => !selected_true,
        UseSite::BranchCondition { block } | UseSite::JumpArgument { block }
            if block == source_block =>
        {
            false
        }
        _ => cfg.dominates(selected_target, site.block()),
    }
}

fn removable_mux_selected_value(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: BlockId,
    index: usize,
    selected_true: bool,
) -> Option<RegisterId> {
    removable_mux_selected_value_in_block(eu.blocks.get(&block)?, index, selected_true)
}

fn removable_mux_selected_value_in_block(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    index: usize,
    selected_true: bool,
) -> Option<RegisterId> {
    match block.instructions.get(index)? {
        SIRInstruction::Mux(_, _, true_value, false_value) => Some(if selected_true {
            *true_value
        } else {
            *false_value
        }),
        _ => None,
    }
}

fn apply_plan(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: GuardedRegionPlan,
    true_id: BlockId,
    false_id: BlockId,
    reg_counter: &mut usize,
) {
    let original = eu
        .blocks
        .remove(&plan.block_id)
        .expect("verified guarded-region source block must exist");
    let distributed = plan
        .distributed
        .iter()
        .map(|store| (store.index, store))
        .collect::<HashMap<_, _>>();
    let mut head_instructions = Vec::new();
    let mut true_instructions = Vec::new();
    let mut false_instructions = Vec::new();

    for (index, inst) in original.instructions.into_iter().enumerate() {
        if plan.moved.contains(&index) {
            if plan.selected_true {
                true_instructions.push(inst);
            } else {
                false_instructions.push(inst);
            }
        } else if let Some(store) = distributed.get(&index) {
            true_instructions.push(store_with_source(&inst, store.true_value));
            false_instructions.push(store_with_source(&inst, store.false_value));
        } else if !plan.removable_muxes.contains(&index) {
            head_instructions.push(inst);
        }
    }
    let branch_condition = normalize_branch_condition(
        &mut eu.register_map,
        &mut head_instructions,
        plan.condition,
        reg_counter,
    );

    eu.blocks.insert(
        plan.block_id,
        BasicBlock {
            id: plan.block_id,
            params: original.params,
            instructions: head_instructions,
            terminator: SIRTerminator::Branch {
                cond: branch_condition,
                true_block: (true_id, Vec::new()),
                false_block: (false_id, Vec::new()),
            },
        },
    );
    eu.blocks.insert(
        true_id,
        BasicBlock {
            id: true_id,
            params: Vec::new(),
            instructions: true_instructions,
            terminator: SIRTerminator::Jump(plan.true_target.0, plan.true_target.1),
        },
    );
    eu.blocks.insert(
        false_id,
        BasicBlock {
            id: false_id,
            params: Vec::new(),
            instructions: false_instructions,
            terminator: SIRTerminator::Jump(plan.false_target.0, plan.false_target.1),
        },
    );
}

fn store_with_source(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    source: RegisterId,
) -> SIRInstruction<RegionedAbsoluteAddr> {
    let SIRInstruction::Store(addr, offset, width, _, triggers, capture_sites) = inst else {
        unreachable!("distributed store plan refers to a non-store")
    };
    SIRInstruction::Store(
        *addr,
        offset.clone(),
        *width,
        source,
        triggers.clone(),
        capture_sites.clone(),
    )
}

fn store_source_is(inst: &SIRInstruction<RegionedAbsoluteAddr>, source: RegisterId) -> bool {
    matches!(inst, SIRInstruction::Store(_, _, _, actual, _, _) if *actual == source)
}

fn instruction_is_movable(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    matches!(
        inst,
        SIRInstruction::Imm(..)
            | SIRInstruction::Binary(..)
            | SIRInstruction::Unary(..)
            | SIRInstruction::Concat(..)
            | SIRInstruction::Slice(..)
            | SIRInstruction::Mux(..)
    )
}

fn instruction_has_effect(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    matches!(
        inst,
        SIRInstruction::Store(..)
            | SIRInstruction::Commit(..)
            | SIRInstruction::RuntimeEvent { .. }
            | SIRInstruction::CombCaptureEvent { .. }
            | SIRInstruction::CombCaptureEnableIfChanged { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{DomainKind, InstanceId, SIRValue, TriggerIdWithKind};
    use celox_design::StateObjectId as VarId;

    fn address(id: usize) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: 0,
            instance_id: InstanceId(id),
            var_id: VarId::default(),
        }
    }

    fn bit(width: usize) -> RegisterType {
        RegisterType::Bit {
            width,
            signed: false,
        }
    }

    fn insert_block(
        blocks: &mut HashMap<BlockId, BasicBlock<RegionedAbsoluteAddr>>,
        id: usize,
        params: Vec<RegisterId>,
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        terminator: SIRTerminator,
    ) {
        blocks.insert(
            BlockId(id),
            BasicBlock {
                id: BlockId(id),
                params,
                instructions,
                terminator,
            },
        );
    }

    fn shared_dag_unit() -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut register_map = HashMap::default();
        register_map.insert(RegisterId(0), bit(1));
        for reg in 1..=8 {
            register_map.insert(RegisterId(reg), bit(8));
        }
        let mut blocks = HashMap::default();
        insert_block(
            &mut blocks,
            0,
            vec![RegisterId(0), RegisterId(1)],
            vec![
                SIRInstruction::Imm(RegisterId(2), SIRValue::new(0u8)),
                SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Add, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(4), RegisterId(3), BinaryOp::Mul, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(5), RegisterId(3), BinaryOp::Or, RegisterId(4)),
                SIRInstruction::Mux(RegisterId(6), RegisterId(0), RegisterId(5), RegisterId(2)),
                SIRInstruction::Store(
                    address(10),
                    SIROffset::Static(0),
                    8,
                    RegisterId(6),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(1), vec![RegisterId(5)]),
                false_block: (BlockId(2), vec![RegisterId(2)]),
            },
        );
        insert_block(
            &mut blocks,
            1,
            vec![RegisterId(7)],
            vec![SIRInstruction::Store(
                address(11),
                SIROffset::Static(0),
                8,
                RegisterId(7),
                Vec::new(),
                Vec::new(),
            )],
            SIRTerminator::Jump(BlockId(3), Vec::new()),
        );
        insert_block(
            &mut blocks,
            2,
            vec![RegisterId(8)],
            vec![SIRInstruction::Store(
                address(12),
                SIROffset::Static(0),
                8,
                RegisterId(8),
                Vec::new(),
                Vec::new(),
            )],
            SIRTerminator::Jump(BlockId(3), Vec::new()),
        );
        insert_block(
            &mut blocks,
            3,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Return,
        );
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    }

    fn repeated_predicate_unit() -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut register_map = HashMap::default();
        register_map.insert(RegisterId(0), bit(1));
        for reg in 1..=11 {
            register_map.insert(RegisterId(reg), bit(8));
        }
        let mut blocks = HashMap::default();
        insert_block(
            &mut blocks,
            0,
            vec![RegisterId(0), RegisterId(1), RegisterId(2)],
            vec![
                // Two closed arm DAGs. The selected values are postprocessed
                // and reconverge before their one external Store.
                SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Mul, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(4), RegisterId(2), BinaryOp::Add, RegisterId(2)),
                SIRInstruction::Mux(RegisterId(5), RegisterId(0), RegisterId(3), RegisterId(4)),
                SIRInstruction::Binary(RegisterId(6), RegisterId(5), BinaryOp::And, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(7), RegisterId(3), BinaryOp::Mul, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(8), RegisterId(4), BinaryOp::Add, RegisterId(2)),
                SIRInstruction::Mux(RegisterId(9), RegisterId(0), RegisterId(7), RegisterId(8)),
                SIRInstruction::Binary(RegisterId(10), RegisterId(9), BinaryOp::Or, RegisterId(2)),
                SIRInstruction::Binary(RegisterId(11), RegisterId(6), BinaryOp::Or, RegisterId(10)),
                SIRInstruction::Store(
                    address(40),
                    SIROffset::Static(0),
                    8,
                    RegisterId(11),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            SIRTerminator::Return,
        );
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    }

    fn repeated_gated_and_unit() -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut register_map = HashMap::default();
        register_map.insert(RegisterId(0), bit(1));
        register_map.insert(RegisterId(1), bit(8));
        register_map.insert(RegisterId(2), bit(8));
        for reg in 3..=12 {
            register_map.insert(RegisterId(reg), bit(8));
        }
        for reg in 13..=17 {
            register_map.insert(RegisterId(reg), bit(1));
        }
        let mut blocks = HashMap::default();
        insert_block(
            &mut blocks,
            0,
            vec![RegisterId(0), RegisterId(1), RegisterId(2)],
            vec![
                SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Mul, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(4), RegisterId(3), BinaryOp::Mul, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(5), RegisterId(4), BinaryOp::Mul, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(6), RegisterId(5), BinaryOp::Mul, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(7), RegisterId(6), BinaryOp::Mul, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(13), RegisterId(7), BinaryOp::Eq, RegisterId(2)),
                SIRInstruction::Binary(
                    RegisterId(14),
                    RegisterId(0),
                    BinaryOp::LogicAnd,
                    RegisterId(13),
                ),
                SIRInstruction::Binary(RegisterId(8), RegisterId(2), BinaryOp::Mul, RegisterId(2)),
                SIRInstruction::Binary(RegisterId(9), RegisterId(8), BinaryOp::Mul, RegisterId(2)),
                SIRInstruction::Binary(RegisterId(10), RegisterId(9), BinaryOp::Mul, RegisterId(2)),
                SIRInstruction::Binary(
                    RegisterId(11),
                    RegisterId(10),
                    BinaryOp::Mul,
                    RegisterId(2),
                ),
                SIRInstruction::Binary(
                    RegisterId(12),
                    RegisterId(11),
                    BinaryOp::Mul,
                    RegisterId(2),
                ),
                SIRInstruction::Binary(RegisterId(15), RegisterId(12), BinaryOp::Eq, RegisterId(1)),
                SIRInstruction::Binary(
                    RegisterId(16),
                    RegisterId(15),
                    BinaryOp::And,
                    RegisterId(0),
                ),
                SIRInstruction::Binary(
                    RegisterId(17),
                    RegisterId(14),
                    BinaryOp::LogicOr,
                    RegisterId(16),
                ),
                SIRInstruction::Store(
                    address(41),
                    SIROffset::Static(0),
                    1,
                    RegisterId(17),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            SIRTerminator::Return,
        );
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct ExecutionTrace {
        stores: Vec<(RegionedAbsoluteAddr, usize, usize, u64)>,
    }

    fn execute(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        condition: u64,
        input: u64,
    ) -> ExecutionTrace {
        execute_with_inputs(eu, condition, input, input)
    }

    fn execute_with_inputs(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        condition: u64,
        input: u64,
        second_input: u64,
    ) -> ExecutionTrace {
        let mut registers = HashMap::default();
        registers.insert(RegisterId(0), condition);
        registers.insert(RegisterId(1), input);
        registers.insert(RegisterId(2), second_input);
        registers.insert(RegisterId(3), input);
        let mut memory = HashMap::<(RegionedAbsoluteAddr, usize), u64>::default();
        let mut stores = Vec::new();
        let mut current = eu.entry_block_id;
        let mut entered = HashSet::default();
        loop {
            assert!(
                entered.insert(current),
                "test fixture unexpectedly contains a loop"
            );
            let block = &eu.blocks[&current];
            for inst in &block.instructions {
                match inst {
                    SIRInstruction::Imm(dst, value) => {
                        let digits = value.payload.to_u64_digits();
                        registers.insert(*dst, digits.first().copied().unwrap_or(0));
                    }
                    SIRInstruction::Binary(dst, lhs, op, rhs) => {
                        let lhs = registers[lhs];
                        let rhs = registers[rhs];
                        let value = match op {
                            BinaryOp::Add => lhs.wrapping_add(rhs),
                            BinaryOp::Mul => lhs.wrapping_mul(rhs),
                            BinaryOp::Eq => u64::from(lhs == rhs),
                            BinaryOp::Or | BinaryOp::LogicOr => lhs | rhs,
                            BinaryOp::And | BinaryOp::LogicAnd => lhs & rhs,
                            other => panic!("unsupported test binary op {other:?}"),
                        };
                        let width = eu.register_map[dst].width();
                        let mask = if width >= 64 {
                            u64::MAX
                        } else {
                            (1u64 << width) - 1
                        };
                        registers.insert(*dst, value & mask);
                    }
                    SIRInstruction::Unary(dst, op, source) => {
                        let width = eu.register_map[dst].width();
                        let mask = if width >= 64 {
                            u64::MAX
                        } else {
                            (1u64 << width) - 1
                        };
                        let value = match op {
                            UnaryOp::Ident => registers[source],
                            UnaryOp::BitNot => !registers[source],
                            UnaryOp::Minus => registers[source].wrapping_neg(),
                            other => panic!("unsupported test unary op {other:?}"),
                        };
                        registers.insert(*dst, value & mask);
                    }
                    SIRInstruction::Load(dst, addr, offset, _) => {
                        let offset = match offset {
                            SIROffset::Static(offset)
                            | SIROffset::PackedElements {
                                bit_offset: offset, ..
                            } => *offset,
                            SIROffset::Dynamic(offset) => registers[offset] as usize,
                            SIROffset::Element {
                                index,
                                element_width,
                                bit_offset,
                                dynamic_bit_offset,
                            } => {
                                registers[index] as usize * element_width
                                    + bit_offset
                                    + dynamic_bit_offset
                                        .map(|register| registers[&register] as usize)
                                        .unwrap_or(0)
                            }
                        };
                        registers.insert(*dst, memory.get(&(*addr, offset)).copied().unwrap_or(0));
                    }
                    SIRInstruction::Mux(dst, cond, true_value, false_value) => {
                        let selected = if registers[cond] != 0 {
                            registers[true_value]
                        } else {
                            registers[false_value]
                        };
                        registers.insert(*dst, selected);
                    }
                    SIRInstruction::Store(addr, SIROffset::Static(offset), width, source, _, _) => {
                        let value = registers[source];
                        memory.insert((*addr, *offset), value);
                        stores.push((*addr, *offset, *width, value));
                    }
                    other => panic!("unsupported test instruction {other:?}"),
                }
            }
            let (target, arguments) = match &block.terminator {
                SIRTerminator::Jump(target, args) => (*target, args.clone()),
                SIRTerminator::Branch {
                    cond,
                    true_block,
                    false_block,
                } => {
                    if registers[cond] != 0 {
                        (true_block.0, true_block.1.clone())
                    } else {
                        (false_block.0, false_block.1.clone())
                    }
                }
                SIRTerminator::Switch { .. } => {
                    panic!("unexpected Switch in guarded-region test")
                }
                SIRTerminator::Return => return ExecutionTrace { stores },
                SIRTerminator::Error(code) => panic!("unexpected test error {code}"),
            };
            let values = arguments
                .iter()
                .map(|argument| registers[argument])
                .collect::<Vec<_>>();
            for (&param, value) in eu.blocks[&target].params.iter().zip(values) {
                registers.insert(param, value);
            }
            current = target;
        }
    }

    fn assert_unchanged(
        before: &ExecutionUnit<RegionedAbsoluteAddr>,
        after: &ExecutionUnit<RegionedAbsoluteAddr>,
    ) {
        assert_eq!(after.entry_block_id, before.entry_block_id);
        assert_eq!(after.register_map, before.register_map);
        assert_eq!(after.blocks, before.blocks);
    }

    #[test]
    fn coupled_stores_recover_the_complete_shared_priority_spine() {
        let mut register_map = HashMap::default();
        register_map.insert(RegisterId(0), bit(1));
        register_map.insert(RegisterId(1), bit(1));
        register_map.insert(RegisterId(2), bit(1));
        for register in 3..=22 {
            register_map.insert(RegisterId(register), bit(8));
        }
        register_map.insert(RegisterId(21), bit(1));
        register_map.insert(RegisterId(22), bit(1));
        let mut instructions = Vec::new();
        for register in 4..=10 {
            instructions.push(SIRInstruction::Binary(
                RegisterId(register),
                RegisterId(register - 1),
                BinaryOp::Mul,
                RegisterId(3),
            ));
        }
        instructions.extend([
            SIRInstruction::Imm(RegisterId(11), SIRValue::new(0x55u8)),
            SIRInstruction::Imm(RegisterId(12), SIRValue::new(0xaau8)),
            SIRInstruction::Binary(RegisterId(21), RegisterId(10), BinaryOp::Eq, RegisterId(11)),
            SIRInstruction::Binary(
                RegisterId(22),
                RegisterId(2),
                BinaryOp::LogicAnd,
                RegisterId(21),
            ),
            SIRInstruction::Binary(RegisterId(13), RegisterId(10), BinaryOp::Add, RegisterId(3)),
            SIRInstruction::Mux(
                RegisterId(14),
                RegisterId(22),
                RegisterId(11),
                RegisterId(13),
            ),
            SIRInstruction::Mux(
                RegisterId(15),
                RegisterId(1),
                RegisterId(11),
                RegisterId(14),
            ),
            SIRInstruction::Mux(
                RegisterId(16),
                RegisterId(0),
                RegisterId(12),
                RegisterId(15),
            ),
            SIRInstruction::Store(
                address(60),
                SIROffset::Static(0),
                8,
                RegisterId(16),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Mux(
                RegisterId(18),
                RegisterId(22),
                RegisterId(12),
                RegisterId(10),
            ),
            SIRInstruction::Mux(
                RegisterId(19),
                RegisterId(1),
                RegisterId(12),
                RegisterId(18),
            ),
            SIRInstruction::Mux(
                RegisterId(20),
                RegisterId(0),
                RegisterId(11),
                RegisterId(19),
            ),
            SIRInstruction::Store(
                address(61),
                SIROffset::Static(0),
                8,
                RegisterId(20),
                Vec::new(),
                Vec::new(),
            ),
        ]);
        let mut blocks = HashMap::default();
        insert_block(
            &mut blocks,
            0,
            vec![RegisterId(0), RegisterId(1), RegisterId(2), RegisterId(3)],
            instructions,
            SIRTerminator::Return,
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };
        eu.verify_result().unwrap();
        let before = eu.clone();

        form_coupled_store_regions(&mut eu);

        eu.verify_result().unwrap();
        for outer in [0, 1] {
            for inner in [0, 1] {
                for final_condition in [0, 1] {
                    assert_eq!(
                        execute_with_inputs(&before, outer, inner, final_condition),
                        execute_with_inputs(&eu, outer, inner, final_condition)
                    );
                }
            }
        }
        assert_eq!(
            eu.blocks
                .values()
                .flat_map(|block| &block.instructions)
                .filter(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
                .count(),
            0
        );
        assert_eq!(
            eu.blocks
                .values()
                .filter(|block| block.instructions.iter().any(|instruction| {
                    matches!(instruction, SIRInstruction::Binary(_, _, BinaryOp::Mul, _))
                }))
                .count(),
            1
        );
        assert!(
            !eu.blocks[&eu.entry_block_id]
                .instructions
                .iter()
                .any(|instruction| matches!(
                    instruction,
                    SIRInstruction::Binary(_, _, BinaryOp::Mul, _)
                ))
        );
        let multiplication_block = eu
            .blocks
            .values()
            .find(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(instruction, SIRInstruction::Binary(_, _, BinaryOp::Mul, _))
                })
            })
            .unwrap();
        assert!(matches!(
            multiplication_block.terminator,
            SIRTerminator::Branch { .. }
        ));

        // Store templates are cloned into every priority leaf.  A dynamic
        // offset which is also part of a late leaf's value cone must therefore
        // stay above the region instead of following that value cone.
        let mut dynamic_offset_eu = before;
        dynamic_offset_eu
            .register_map
            .insert(RegisterId(22), RegisterType::Logic { width: 1 });
        for instruction in &mut dynamic_offset_eu
            .blocks
            .get_mut(&BlockId(0))
            .unwrap()
            .instructions
        {
            if let SIRInstruction::Store(_, offset, _, _, _, _) = instruction {
                *offset = SIROffset::Dynamic(RegisterId(10));
            }
        }
        dynamic_offset_eu.verify_result().unwrap();
        form_coupled_store_regions(&mut dynamic_offset_eu);
        dynamic_offset_eu.verify_result().unwrap();
        assert!(
            dynamic_offset_eu.blocks[&dynamic_offset_eu.entry_block_id]
                .instructions
                .iter()
                .any(|instruction| matches!(
                    instruction,
                    SIRInstruction::Binary(RegisterId(10), ..)
                ))
        );
    }

    #[test]
    fn sinks_a_shared_true_edge_dag_and_preserves_both_guard_outcomes() {
        let mut eu = shared_dag_unit();
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert_eq!(execute(&before, 0, 7), execute(&eu, 0, 7));
        assert_eq!(execute(&before, 1, 7), execute(&eu, 1, 7));
        let SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } = &eu.blocks[&BlockId(0)].terminator
        else {
            panic!("guard must remain a branch");
        };
        let head = &eu.blocks[&BlockId(0)];
        assert!(!head.instructions.iter().any(|inst| {
            matches!(
                def_reg(inst),
                Some(RegisterId(3) | RegisterId(4) | RegisterId(5))
            )
        }));
        let true_shim = &eu.blocks[&true_block.0];
        assert!(
            true_shim
                .instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Binary(RegisterId(3), ..)) })
        );
        assert!(
            true_shim
                .instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Binary(RegisterId(4), ..)) })
        );
        assert!(
            true_shim
                .instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Binary(RegisterId(5), ..)) })
        );
        assert!(
            true_shim.instructions.iter().any(|inst| {
                matches!(inst, SIRInstruction::Store(_, _, 8, RegisterId(5), _, _))
            })
        );
        let false_shim = &eu.blocks[&false_block.0];
        assert!(
            false_shim.instructions.iter().any(|inst| {
                matches!(inst, SIRInstruction::Store(_, _, 8, RegisterId(2), _, _))
            })
        );
    }

    #[test]
    fn sinks_the_more_expensive_false_edge_dag() {
        let mut eu = shared_dag_unit();
        let head = eu.blocks.get_mut(&BlockId(0)).unwrap();
        let SIRInstruction::Mux(_, _, true_value, false_value) = &mut head.instructions[4] else {
            panic!("fixture must select the shared DAG");
        };
        std::mem::swap(true_value, false_value);
        let SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } = &mut head.terminator
        else {
            panic!("fixture must branch on the same predicate");
        };
        std::mem::swap(&mut true_block.1, &mut false_block.1);
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert_eq!(execute(&before, 0, 7), execute(&eu, 0, 7));
        assert_eq!(execute(&before, 1, 7), execute(&eu, 1, 7));
        assert!(!eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
            matches!(
                def_reg(inst),
                Some(RegisterId(3) | RegisterId(4) | RegisterId(5))
            )
        }));
        let SIRTerminator::Branch { false_block, .. } = &eu.blocks[&BlockId(0)].terminator else {
            panic!("guard must remain a branch");
        };
        let false_shim = &eu.blocks[&false_block.0];
        assert!(
            false_shim
                .instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Binary(RegisterId(5), ..)) })
        );
        assert!(
            false_shim.instructions.iter().any(|inst| {
                matches!(inst, SIRInstruction::Store(_, _, 8, RegisterId(5), _, _))
            })
        );
    }

    #[test]
    fn phi_store_distribution_recovers_the_remaining_priority_chain() {
        let mut register_map = HashMap::default();
        register_map.insert(RegisterId(0), bit(1));
        register_map.insert(RegisterId(1), bit(1));
        for register in 2..=20 {
            register_map.insert(RegisterId(register), bit(8));
        }

        let mut blocks = HashMap::default();
        insert_block(
            &mut blocks,
            0,
            vec![RegisterId(0), RegisterId(1), RegisterId(2)],
            vec![
                SIRInstruction::Binary(RegisterId(4), RegisterId(2), BinaryOp::Mul, RegisterId(2)),
                SIRInstruction::Binary(RegisterId(5), RegisterId(4), BinaryOp::Mul, RegisterId(2)),
                SIRInstruction::Binary(RegisterId(15), RegisterId(5), BinaryOp::Mul, RegisterId(2)),
                SIRInstruction::Binary(
                    RegisterId(16),
                    RegisterId(15),
                    BinaryOp::Mul,
                    RegisterId(2),
                ),
                SIRInstruction::Binary(
                    RegisterId(17),
                    RegisterId(16),
                    BinaryOp::Mul,
                    RegisterId(2),
                ),
                SIRInstruction::Binary(
                    RegisterId(18),
                    RegisterId(17),
                    BinaryOp::Mul,
                    RegisterId(2),
                ),
                SIRInstruction::Binary(
                    RegisterId(19),
                    RegisterId(18),
                    BinaryOp::Mul,
                    RegisterId(2),
                ),
                SIRInstruction::Binary(
                    RegisterId(20),
                    RegisterId(19),
                    BinaryOp::Mul,
                    RegisterId(2),
                ),
                SIRInstruction::Imm(RegisterId(7), SIRValue::new(0x70u8)),
                SIRInstruction::Imm(RegisterId(8), SIRValue::new(0x80u8)),
                SIRInstruction::Imm(RegisterId(11), SIRValue::new(0x11u8)),
                SIRInstruction::Imm(RegisterId(13), SIRValue::new(0x13u8)),
                SIRInstruction::Mux(RegisterId(14), RegisterId(1), RegisterId(8), RegisterId(20)),
                SIRInstruction::Mux(RegisterId(6), RegisterId(0), RegisterId(7), RegisterId(14)),
                SIRInstruction::Store(
                    address(70),
                    SIROffset::Static(0),
                    8,
                    RegisterId(6),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(1), Vec::new()),
                false_block: (BlockId(2), Vec::new()),
            },
        );
        insert_block(
            &mut blocks,
            1,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Jump(BlockId(3), vec![RegisterId(13)]),
        );
        insert_block(
            &mut blocks,
            2,
            Vec::new(),
            vec![SIRInstruction::Mux(
                RegisterId(12),
                RegisterId(1),
                RegisterId(8),
                RegisterId(11),
            )],
            SIRTerminator::Jump(BlockId(3), vec![RegisterId(12)]),
        );
        insert_block(
            &mut blocks,
            3,
            vec![RegisterId(9)],
            vec![SIRInstruction::Store(
                address(71),
                SIROffset::Static(0),
                8,
                RegisterId(9),
                Vec::new(),
                Vec::new(),
            )],
            SIRTerminator::Return,
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        for outer in [0, 1] {
            for inner in [0, 1] {
                assert_eq!(execute(&before, outer, inner), execute(&eu, outer, inner));
            }
        }
        assert!(eu.blocks[&BlockId(3)].instructions.is_empty());
        assert!(
            !eu.blocks[&BlockId(0)]
                .instructions
                .iter()
                .any(|instruction| {
                    matches!(instruction, SIRInstruction::Binary(_, _, BinaryOp::Mul, _))
                })
        );
        let multiplication_block = eu
            .blocks
            .values()
            .find(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(instruction, SIRInstruction::Binary(_, _, BinaryOp::Mul, _))
                })
            })
            .expect("normal leaf must retain the expensive DAG");
        let cfg = SirCfg::analyze(&eu).unwrap();
        assert!(cfg.dominates(BlockId(0), multiplication_block.id));
        assert!(
            eu.blocks.values().any(|block| {
                matches!(
                    block.terminator,
                    SIRTerminator::Branch {
                        cond: RegisterId(1),
                        ..
                    }
                ) && cfg.dominates(block.id, multiplication_block.id)
            }),
            "{eu:#?}"
        );
    }

    #[test]
    fn one_branch_specializes_a_repeated_predicate_and_merges_only_the_live_out() {
        let mut eu = repeated_predicate_unit();
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        for condition in [0, 1] {
            for input in [3, 5, 7, 11] {
                assert_eq!(
                    execute(&before, condition, input),
                    execute(&eu, condition, input),
                );
            }
        }
        let head = &eu.blocks[&BlockId(0)];
        let SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } = &head.terminator
        else {
            panic!("the repeated predicate must form one shared branch");
        };
        assert!(
            eu.blocks[&true_block.0]
                .instructions
                .iter()
                .all(|inst| !matches!(inst, SIRInstruction::Mux(_, RegisterId(0), ..)))
        );
        assert!(
            eu.blocks[&false_block.0]
                .instructions
                .iter()
                .all(|inst| !matches!(inst, SIRInstruction::Mux(_, RegisterId(0), ..)))
        );
        let merge = match &eu.blocks[&true_block.0].terminator {
            SIRTerminator::Jump(merge, args) => {
                assert_eq!(
                    args.len(),
                    1,
                    "only the reconverged result crosses the edge"
                );
                *merge
            }
            other => panic!("unexpected true-edge terminator: {other:?}"),
        };
        assert_eq!(eu.blocks[&merge].params, vec![RegisterId(11)]);
        assert!(matches!(
            eu.blocks[&merge].instructions.as_slice(),
            [SIRInstruction::Store(_, _, 8, RegisterId(11), _, _)]
        ));
        for dead in 3..=10 {
            assert!(
                !eu.register_map.contains_key(&RegisterId(dead)),
                "replaced original r{dead} must not allocate a native VReg"
            );
        }
        assert!(
            eu.register_map.contains_key(&RegisterId(11)),
            "the merge parameter keeps its original register type"
        );
    }

    #[test]
    fn one_branch_skips_repeated_gated_and_payloads_and_merges_one_bit() {
        let mut eu = repeated_gated_and_unit();
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        for condition in [0, 1] {
            for input in [0, 1, 3, 7, 11, 255] {
                assert_eq!(
                    execute(&before, condition, input),
                    execute(&eu, condition, input),
                );
            }
        }
        let SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } = &eu.blocks[&BlockId(0)].terminator
        else {
            panic!("the repeated gate must form one shared branch");
        };
        assert!(eu.blocks[&BlockId(0)].instructions.is_empty());
        assert!(
            eu.blocks[&true_block.0]
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Binary(_, _, BinaryOp::Mul, _)))
        );
        assert!(
            eu.blocks[&true_block.0].instructions.iter().all(|inst| {
                !matches!(inst, SIRInstruction::Binary(_, _, BinaryOp::LogicAnd, _))
            })
        );
        assert!(eu.blocks[&false_block.0].instructions.iter().all(|inst| {
            matches!(
                inst,
                SIRInstruction::Imm(_, _) | SIRInstruction::Binary(_, _, BinaryOp::LogicOr, _)
            )
        }));
        let merge = match &eu.blocks[&true_block.0].terminator {
            SIRTerminator::Jump(merge, args) => {
                assert_eq!(args.len(), 1);
                *merge
            }
            other => panic!("unexpected true-edge terminator: {other:?}"),
        };
        assert_eq!(eu.blocks[&merge].params, vec![RegisterId(17)]);
        assert!(matches!(
            eu.blocks[&merge].instructions.as_slice(),
            [SIRInstruction::Store(_, _, 1, RegisterId(17), _, _)]
        ));
    }

    #[test]
    fn same_predicate_region_moves_static_and_dynamic_loads_with_their_arms() {
        let mut eu = repeated_predicate_unit();
        eu.register_map.insert(RegisterId(12), bit(8));
        eu.register_map.insert(RegisterId(13), bit(8));
        let block = eu.blocks.get_mut(&BlockId(0)).unwrap();
        block.instructions.insert(
            0,
            SIRInstruction::Load(RegisterId(12), address(50), SIROffset::Static(0), 8),
        );
        block.instructions.insert(
            1,
            SIRInstruction::Load(
                RegisterId(13),
                address(51),
                SIROffset::Dynamic(RegisterId(2)),
                8,
            ),
        );
        let SIRInstruction::Binary(_, true_lhs, _, _) = &mut block.instructions[2] else {
            panic!("fixture true arm must start with a binary operation");
        };
        *true_lhs = RegisterId(12);
        let SIRInstruction::Binary(_, false_lhs, _, _) = &mut block.instructions[3] else {
            panic!("fixture false arm must start with a binary operation");
        };
        *false_lhs = RegisterId(13);
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        for condition in [0, 1] {
            assert_eq!(execute(&before, condition, 7), execute(&eu, condition, 7));
        }
        let SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } = &eu.blocks[&BlockId(0)].terminator
        else {
            panic!("load-bearing repeated predicate must branch");
        };
        assert!(
            eu.blocks[&true_block.0]
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Load(_, _, SIROffset::Static(0), 8)))
        );
        assert!(
            eu.blocks[&false_block.0].instructions.iter().any(|inst| {
                matches!(inst, SIRInstruction::Load(_, _, SIROffset::Dynamic(_), 8))
            })
        );
    }

    #[test]
    fn nested_same_predicate_mux_drops_the_unselected_cofactor() {
        let mut eu = repeated_predicate_unit();
        for reg in 12..=14 {
            eu.register_map.insert(RegisterId(reg), bit(8));
        }
        let block = eu.blocks.get_mut(&BlockId(0)).unwrap();
        let store = block.instructions.pop().unwrap();
        block.instructions.extend([
            SIRInstruction::Unary(RegisterId(12), UnaryOp::BitNot, RegisterId(11)),
            SIRInstruction::Unary(RegisterId(13), UnaryOp::Minus, RegisterId(11)),
            SIRInstruction::Mux(
                RegisterId(14),
                RegisterId(0),
                RegisterId(12),
                RegisterId(13),
            ),
        ]);
        let SIRInstruction::Store(address, offset, width, _, triggers, sites) = store else {
            panic!("fixture must end in a Store");
        };
        block.instructions.push(SIRInstruction::Store(
            address,
            offset,
            width,
            RegisterId(14),
            triggers,
            sites,
        ));
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        for condition in [0, 1] {
            assert_eq!(execute(&before, condition, 9), execute(&eu, condition, 9));
        }
        let SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } = &eu.blocks[&BlockId(0)].terminator
        else {
            panic!("nested repeated predicate must branch");
        };
        assert!(
            eu.blocks[&true_block.0]
                .instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Unary(_, UnaryOp::BitNot, _)) })
        );
        assert!(
            !eu.blocks[&true_block.0]
                .instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Unary(_, UnaryOp::Minus, _)) })
        );
        assert!(
            eu.blocks[&false_block.0]
                .instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Unary(_, UnaryOp::Minus, _)) })
        );
        assert!(
            !eu.blocks[&false_block.0]
                .instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Unary(_, UnaryOp::BitNot, _)) })
        );
    }

    #[test]
    fn keeps_a_locally_defined_predicate_before_the_recovered_branch() {
        let mut register_map = HashMap::default();
        for reg in 0..=14 {
            register_map.insert(RegisterId(reg), bit(1));
        }
        let mut blocks = HashMap::default();
        insert_block(
            &mut blocks,
            0,
            vec![RegisterId(0), RegisterId(1), RegisterId(2)],
            vec![
                SIRInstruction::Unary(RegisterId(3), UnaryOp::Ident, RegisterId(0)),
                SIRInstruction::Binary(RegisterId(4), RegisterId(1), BinaryOp::Mul, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(5), RegisterId(4), BinaryOp::Mul, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(6), RegisterId(5), BinaryOp::Mul, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(7), RegisterId(6), BinaryOp::Mul, RegisterId(1)),
                SIRInstruction::Binary(RegisterId(8), RegisterId(2), BinaryOp::Mul, RegisterId(2)),
                SIRInstruction::Binary(RegisterId(9), RegisterId(8), BinaryOp::Mul, RegisterId(2)),
                SIRInstruction::Binary(RegisterId(10), RegisterId(9), BinaryOp::Mul, RegisterId(2)),
                SIRInstruction::Binary(
                    RegisterId(11),
                    RegisterId(10),
                    BinaryOp::Mul,
                    RegisterId(2),
                ),
                // The predicate is also selected as data. Moving r3 into the
                // true arm would leave the newly introduced Branch undefined.
                SIRInstruction::Mux(RegisterId(12), RegisterId(3), RegisterId(3), RegisterId(7)),
                SIRInstruction::Mux(RegisterId(13), RegisterId(3), RegisterId(3), RegisterId(11)),
                SIRInstruction::Binary(
                    RegisterId(14),
                    RegisterId(12),
                    BinaryOp::Or,
                    RegisterId(13),
                ),
                SIRInstruction::Store(
                    address(42),
                    SIROffset::Static(0),
                    1,
                    RegisterId(14),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            SIRTerminator::Return,
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        for condition in [0, 1] {
            for input in [0, 1] {
                assert_eq!(
                    execute(&before, condition, input),
                    execute(&eu, condition, input),
                );
            }
        }
        let head = &eu.blocks[&BlockId(0)];
        assert!(head.instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Unary(RegisterId(3), UnaryOp::Ident, RegisterId(0))
            )
        }));
        assert!(matches!(
            head.terminator,
            SIRTerminator::Branch {
                cond: RegisterId(3),
                ..
            }
        ));
    }

    #[test]
    fn rejects_a_value_used_from_the_false_region() {
        let mut eu = shared_dag_unit();
        eu.blocks
            .get_mut(&BlockId(2))
            .unwrap()
            .instructions
            .push(SIRInstruction::Store(
                address(13),
                SIROffset::Static(0),
                8,
                RegisterId(5),
                Vec::new(),
                Vec::new(),
            ));
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&before, &eu);
    }

    #[test]
    fn an_aliasing_write_keeps_the_load_before_the_guard() {
        let mut eu = shared_dag_unit();
        let head = eu.blocks.get_mut(&BlockId(0)).unwrap();
        head.instructions = vec![
            SIRInstruction::Imm(RegisterId(2), SIRValue::new(0u8)),
            SIRInstruction::Load(RegisterId(5), address(20), SIROffset::Static(0), 8),
            SIRInstruction::Store(
                address(20),
                SIROffset::Static(0),
                8,
                RegisterId(1),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Mux(RegisterId(6), RegisterId(0), RegisterId(5), RegisterId(2)),
            SIRInstruction::Store(
                address(10),
                SIROffset::Static(0),
                8,
                RegisterId(6),
                Vec::new(),
                Vec::new(),
            ),
        ];
        if let SIRTerminator::Branch { true_block, .. } = &mut head.terminator {
            true_block.1 = vec![RegisterId(5)];
        }
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&before, &eu);
    }

    #[test]
    fn rejects_a_load_after_the_first_distributed_store() {
        let mut eu = shared_dag_unit();
        eu.register_map.insert(RegisterId(9), bit(8));
        eu.blocks
            .get_mut(&BlockId(0))
            .unwrap()
            .instructions
            .push(SIRInstruction::Load(
                RegisterId(9),
                address(30),
                SIROffset::Static(0),
                8,
            ));
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&before, &eu);
    }

    #[test]
    fn rejects_an_effect_after_the_first_distributed_store() {
        let mut eu = shared_dag_unit();
        eu.blocks
            .get_mut(&BlockId(0))
            .unwrap()
            .instructions
            .push(SIRInstruction::RuntimeEvent {
                site_id: 7,
                args: vec![RegisterId(1)],
            });
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&before, &eu);
    }

    #[test]
    fn preserves_multiple_distributed_store_order() {
        let mut eu = shared_dag_unit();
        eu.register_map.insert(RegisterId(9), bit(8));
        let head = eu.blocks.get_mut(&BlockId(0)).unwrap();
        head.instructions.push(SIRInstruction::Mux(
            RegisterId(9),
            RegisterId(0),
            RegisterId(4),
            RegisterId(2),
        ));
        head.instructions.push(SIRInstruction::Store(
            address(14),
            SIROffset::Static(0),
            8,
            RegisterId(9),
            Vec::new(),
            Vec::new(),
        ));
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert_eq!(execute(&before, 0, 3), execute(&eu, 0, 3));
        assert_eq!(execute(&before, 1, 3), execute(&eu, 1, 3));
        let SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } = &eu.blocks[&BlockId(0)].terminator
        else {
            panic!("expected guard branch");
        };
        let addresses = |block: &BasicBlock<RegionedAbsoluteAddr>| {
            block
                .instructions
                .iter()
                .filter_map(|inst| match inst {
                    SIRInstruction::Store(address, ..) => Some(address.instance_id),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            addresses(&eu.blocks[&true_block.0]),
            vec![InstanceId(10), InstanceId(14)]
        );
        assert_eq!(
            addresses(&eu.blocks[&false_block.0]),
            vec![InstanceId(10), InstanceId(14)]
        );
    }

    #[test]
    fn four_state_mode_is_non_destructive() {
        let mut eu = shared_dag_unit();
        let before = eu.clone();
        let options = PassOptions {
            four_state: true,
            ..Default::default()
        };

        GuardedRegionSinkingPass.run(&mut eu, &options);

        assert_unchanged(&before, &eu);
    }

    #[test]
    fn trigger_only_zero_width_store_is_non_destructive() {
        let mut eu = shared_dag_unit();
        let head = eu.blocks.get_mut(&BlockId(0)).unwrap();
        let SIRInstruction::Store(_, _, width, _, triggers, _) = &mut head.instructions[5] else {
            panic!("fixture must end in a store");
        };
        *width = 0;
        *triggers = vec![TriggerIdWithKind {
            kind: DomainKind::ClockPosedge,
            id: 9,
        }];
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&before, &eu);
    }

    #[test]
    fn a_second_run_does_not_rewrite_generated_regions() {
        let mut eu = shared_dag_unit();
        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());
        eu.verify_result().unwrap();
        let once = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&once, &eu);
    }

    #[test]
    fn multi_bit_branch_guard_is_rejected() {
        let mut eu = shared_dag_unit();
        eu.register_map.insert(RegisterId(0), bit(8));
        assert_eq!(
            eu.verify_result().unwrap_err().invariant,
            "TYPE.BRANCH_CONDITION"
        );
    }

    #[test]
    fn narrow_mux_arm_is_not_connected_directly_to_a_wider_store() {
        let mut eu = shared_dag_unit();
        eu.register_map.insert(RegisterId(2), bit(4));
        eu.register_map.insert(RegisterId(8), bit(4));
        eu.register_map.insert(RegisterId(9), bit(8));
        let head = eu.blocks.get_mut(&BlockId(0)).unwrap();
        head.instructions.insert(
            4,
            SIRInstruction::Unary(RegisterId(9), UnaryOp::Ident, RegisterId(2)),
        );
        let SIRInstruction::Mux(_, _, _, false_value) = &mut head.instructions[5] else {
            panic!("fixture must contain the distributed mux");
        };
        *false_value = RegisterId(9);
        let false_block = eu.blocks.get_mut(&BlockId(2)).unwrap();
        let SIRInstruction::Store(_, _, width, _, _, _) = &mut false_block.instructions[0] else {
            panic!("fixture false block must store its parameter");
        };
        *width = 4;
        eu.verify_result().unwrap();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert!(
            eu.blocks
                .values()
                .flat_map(|block| &block.instructions)
                .all(|instruction| {
                    let SIRInstruction::Store(_, _, width, value, _, _) = instruction else {
                        return true;
                    };
                    *width == eu.register_map[value].width()
                })
        );
    }

    #[test]
    fn removable_mux_cannot_also_supply_a_dynamic_store_offset() {
        let mut eu = shared_dag_unit();
        let head = eu.blocks.get_mut(&BlockId(0)).unwrap();
        let SIRInstruction::Store(_, offset, _, _, _, _) = &mut head.instructions[5] else {
            panic!("fixture must end in the distributed store");
        };
        *offset = SIROffset::Dynamic(RegisterId(6));
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&before, &eu);
    }

    #[test]
    fn one_removable_mux_cannot_supply_another_store_offset() {
        let mut eu = shared_dag_unit();
        eu.register_map.insert(RegisterId(9), bit(8));
        let head = eu.blocks.get_mut(&BlockId(0)).unwrap();
        head.instructions.insert(
            5,
            SIRInstruction::Mux(RegisterId(9), RegisterId(0), RegisterId(4), RegisterId(2)),
        );
        let SIRInstruction::Store(_, first_offset, _, _, _, _) = &mut head.instructions[6] else {
            panic!("fixture first store must remain a store");
        };
        *first_offset = SIROffset::Dynamic(RegisterId(9));
        head.instructions.push(SIRInstruction::Store(
            address(14),
            SIROffset::Static(0),
            8,
            RegisterId(9),
            Vec::new(),
            Vec::new(),
        ));
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&before, &eu);
    }

    #[test]
    fn moves_a_guarded_value_diamond_under_its_selected_use() {
        let mut register_map = HashMap::default();
        register_map.insert(RegisterId(0), bit(1));
        register_map.insert(RegisterId(1), bit(1));
        for register in 2..=9 {
            register_map.insert(RegisterId(register), bit(64));
        }
        let mut blocks = HashMap::default();
        insert_block(
            &mut blocks,
            0,
            vec![
                RegisterId(0),
                RegisterId(1),
                RegisterId(2),
                RegisterId(3),
                RegisterId(4),
                RegisterId(8),
            ],
            Vec::new(),
            SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(1), Vec::new()),
                false_block: (BlockId(2), Vec::new()),
            },
        );
        insert_block(
            &mut blocks,
            1,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Jump(BlockId(3), vec![RegisterId(4)]),
        );
        insert_block(
            &mut blocks,
            2,
            Vec::new(),
            vec![SIRInstruction::Binary(
                RegisterId(5),
                RegisterId(2),
                BinaryOp::DivU,
                RegisterId(3),
            )],
            SIRTerminator::Jump(BlockId(3), vec![RegisterId(5)]),
        );
        insert_block(
            &mut blocks,
            3,
            vec![RegisterId(6)],
            Vec::new(),
            SIRTerminator::Branch {
                cond: RegisterId(1),
                true_block: (BlockId(4), Vec::new()),
                false_block: (BlockId(5), Vec::new()),
            },
        );
        insert_block(
            &mut blocks,
            4,
            Vec::new(),
            vec![SIRInstruction::Unary(
                RegisterId(7),
                UnaryOp::BitNot,
                RegisterId(6),
            )],
            SIRTerminator::Jump(BlockId(6), vec![RegisterId(7)]),
        );
        insert_block(
            &mut blocks,
            5,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Jump(BlockId(6), vec![RegisterId(8)]),
        );
        insert_block(
            &mut blocks,
            6,
            vec![RegisterId(9)],
            vec![SIRInstruction::Store(
                address(99),
                SIROffset::Static(0),
                64,
                RegisterId(9),
                Vec::new(),
                Vec::new(),
            )],
            SIRTerminator::Return,
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };
        eu.verify_result().unwrap();

        sink_deferred_value_diamonds(&mut eu);

        eu.verify_result().unwrap();
        assert!(!eu.blocks.contains_key(&BlockId(1)));
        assert!(!eu.blocks.contains_key(&BlockId(2)));
        assert!(matches!(
            eu.blocks[&BlockId(0)].terminator,
            SIRTerminator::Jump(BlockId(3), ref args) if args.is_empty()
        ));
        assert!(eu.blocks[&BlockId(3)].params.is_empty());
        assert!(matches!(
            eu.blocks[&BlockId(4)].terminator,
            SIRTerminator::Branch {
                cond: RegisterId(0),
                ..
            }
        ));
        let divide_block = eu
            .blocks
            .values()
            .find(|block| {
                block.instructions.iter().any(|instruction| {
                    matches!(
                        instruction,
                        SIRInstruction::Binary(RegisterId(5), _, BinaryOp::DivU, _)
                    )
                })
            })
            .expect("the guarded divide must retain one definition");
        let SIRTerminator::Branch { false_block, .. } = &eu.blocks[&BlockId(4)].terminator else {
            unreachable!()
        };
        assert_eq!(false_block.0, divide_block.id);
        let continuation = eu
            .blocks
            .values()
            .find(|block| block.params == vec![RegisterId(6)])
            .expect("the selected leaf must merge the guarded value locally");
        assert!(continuation.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                SIRInstruction::Unary(RegisterId(7), UnaryOp::BitNot, RegisterId(6))
            )
        }));
    }

    #[test]
    fn sinks_a_pure_multi_block_region_to_its_only_phi_use() {
        let mut register_map = HashMap::default();
        register_map.insert(RegisterId(0), bit(1));
        register_map.insert(RegisterId(1), bit(1));
        for register in 2..=21 {
            register_map.insert(RegisterId(register), bit(8));
        }
        let mut blocks = HashMap::default();
        insert_block(
            &mut blocks,
            0,
            vec![RegisterId(0), RegisterId(1), RegisterId(2)],
            vec![
                SIRInstruction::Store(
                    address(80),
                    SIROffset::Static(0),
                    8,
                    RegisterId(2),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Binary(RegisterId(4), RegisterId(2), BinaryOp::Mul, RegisterId(2)),
            ],
            SIRTerminator::Branch {
                cond: RegisterId(1),
                true_block: (BlockId(1), Vec::new()),
                false_block: (BlockId(2), Vec::new()),
            },
        );
        insert_block(
            &mut blocks,
            1,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Branch {
                cond: RegisterId(1),
                true_block: (BlockId(3), Vec::new()),
                false_block: (BlockId(4), Vec::new()),
            },
        );
        insert_block(
            &mut blocks,
            2,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Branch {
                cond: RegisterId(1),
                true_block: (BlockId(5), Vec::new()),
                false_block: (BlockId(6), Vec::new()),
            },
        );
        for (block, value, register) in [
            (3, None, 4),
            (4, Some(4), 10),
            (5, Some(5), 11),
            (6, Some(6), 12),
        ] {
            insert_block(
                &mut blocks,
                block,
                Vec::new(),
                value
                    .map(|value| {
                        vec![SIRInstruction::Imm(
                            RegisterId(register),
                            SIRValue::new(value as u8),
                        )]
                    })
                    .unwrap_or_default(),
                SIRTerminator::Jump(BlockId(7), vec![RegisterId(register)]),
            );
        }
        insert_block(
            &mut blocks,
            7,
            vec![RegisterId(20)],
            Vec::new(),
            SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(8), Vec::new()),
                false_block: (BlockId(9), vec![RegisterId(2)]),
            },
        );
        insert_block(
            &mut blocks,
            8,
            Vec::new(),
            vec![SIRInstruction::Store(
                address(81),
                SIROffset::Static(0),
                8,
                RegisterId(2),
                Vec::new(),
                Vec::new(),
            )],
            SIRTerminator::Return,
        );
        insert_block(
            &mut blocks,
            9,
            vec![RegisterId(21)],
            vec![SIRInstruction::Store(
                address(82),
                SIROffset::Static(0),
                8,
                RegisterId(20),
                Vec::new(),
                Vec::new(),
            )],
            SIRTerminator::Return,
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };
        eu.verify_result().unwrap();
        let before = eu.clone();

        sink_deferred_value_regions(&mut eu);

        eu.verify_result().unwrap();
        for exception in [0, 1] {
            for input in [0, 1] {
                assert_eq!(
                    execute(&before, exception, input),
                    execute(&eu, exception, input)
                );
            }
        }
        assert!(matches!(
            eu.blocks[&BlockId(0)].terminator,
            SIRTerminator::Jump(BlockId(7), ref arguments) if arguments.is_empty()
        ));
        assert!(eu.blocks[&BlockId(7)].params.is_empty());
        assert_eq!(eu.blocks[&BlockId(9)].params, vec![RegisterId(21)]);
        assert!(
            !eu.blocks[&BlockId(0)]
                .instructions
                .iter()
                .any(|instruction| def_reg(instruction) == Some(RegisterId(4)))
        );
        assert!(
            eu.blocks[&BlockId(9)]
                .instructions
                .iter()
                .any(|instruction| def_reg(instruction) == Some(RegisterId(4)))
        );
    }

    #[test]
    fn moves_a_serial_sequence_of_guarded_values_in_one_cfg_plan() {
        let mut register_map = HashMap::default();
        for register in 0..=3 {
            register_map.insert(RegisterId(register), bit(1));
        }
        for register in 4..=17 {
            register_map.insert(RegisterId(register), bit(64));
        }
        let mut blocks = HashMap::default();
        insert_block(
            &mut blocks,
            0,
            vec![
                RegisterId(0),
                RegisterId(1),
                RegisterId(2),
                RegisterId(3),
                RegisterId(4),
                RegisterId(5),
                RegisterId(6),
                RegisterId(9),
                RegisterId(10),
                RegisterId(11),
                RegisterId(16),
            ],
            Vec::new(),
            SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(1), Vec::new()),
                false_block: (BlockId(2), Vec::new()),
            },
        );
        insert_block(
            &mut blocks,
            1,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Jump(BlockId(3), vec![RegisterId(6)]),
        );
        insert_block(
            &mut blocks,
            2,
            Vec::new(),
            vec![SIRInstruction::Binary(
                RegisterId(7),
                RegisterId(4),
                BinaryOp::DivS,
                RegisterId(5),
            )],
            SIRTerminator::Jump(BlockId(3), vec![RegisterId(7)]),
        );
        insert_block(
            &mut blocks,
            3,
            vec![RegisterId(8)],
            Vec::new(),
            SIRTerminator::Branch {
                cond: RegisterId(1),
                true_block: (BlockId(4), Vec::new()),
                false_block: (BlockId(5), Vec::new()),
            },
        );
        insert_block(
            &mut blocks,
            4,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Jump(BlockId(6), vec![RegisterId(11)]),
        );
        insert_block(
            &mut blocks,
            5,
            Vec::new(),
            vec![SIRInstruction::Binary(
                RegisterId(12),
                RegisterId(9),
                BinaryOp::RemU,
                RegisterId(10),
            )],
            SIRTerminator::Jump(BlockId(6), vec![RegisterId(12)]),
        );
        insert_block(
            &mut blocks,
            6,
            vec![RegisterId(13)],
            Vec::new(),
            SIRTerminator::Branch {
                cond: RegisterId(2),
                true_block: (BlockId(7), Vec::new()),
                false_block: (BlockId(8), Vec::new()),
            },
        );
        insert_block(
            &mut blocks,
            7,
            Vec::new(),
            vec![SIRInstruction::Unary(
                RegisterId(14),
                UnaryOp::BitNot,
                RegisterId(8),
            )],
            SIRTerminator::Jump(BlockId(11), vec![RegisterId(14)]),
        );
        insert_block(
            &mut blocks,
            8,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Branch {
                cond: RegisterId(3),
                true_block: (BlockId(9), Vec::new()),
                false_block: (BlockId(10), Vec::new()),
            },
        );
        insert_block(
            &mut blocks,
            9,
            Vec::new(),
            vec![SIRInstruction::Unary(
                RegisterId(15),
                UnaryOp::BitNot,
                RegisterId(13),
            )],
            SIRTerminator::Jump(BlockId(11), vec![RegisterId(15)]),
        );
        insert_block(
            &mut blocks,
            10,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Jump(BlockId(11), vec![RegisterId(16)]),
        );
        insert_block(
            &mut blocks,
            11,
            vec![RegisterId(17)],
            vec![SIRInstruction::Store(
                address(100),
                SIROffset::Static(0),
                64,
                RegisterId(17),
                Vec::new(),
                Vec::new(),
            )],
            SIRTerminator::Return,
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };
        eu.verify_result().unwrap();

        sink_deferred_value_diamonds(&mut eu);

        eu.verify_result().unwrap();
        for removed in [1, 2, 4, 5] {
            assert!(!eu.blocks.contains_key(&BlockId(removed)));
        }
        assert!(matches!(
            eu.blocks[&BlockId(0)].terminator,
            SIRTerminator::Jump(BlockId(3), ref args) if args.is_empty()
        ));
        assert!(matches!(
            eu.blocks[&BlockId(3)].terminator,
            SIRTerminator::Jump(BlockId(6), ref args) if args.is_empty()
        ));
        assert!(eu.blocks[&BlockId(3)].params.is_empty());
        assert!(eu.blocks[&BlockId(6)].params.is_empty());
        assert!(matches!(
            eu.blocks[&BlockId(7)].terminator,
            SIRTerminator::Branch {
                cond: RegisterId(0),
                ..
            }
        ));
        assert!(matches!(
            eu.blocks[&BlockId(9)].terminator,
            SIRTerminator::Branch {
                cond: RegisterId(1),
                ..
            }
        ));
    }

    #[test]
    fn malformed_input_is_non_destructive() {
        let mut eu = shared_dag_unit();
        eu.blocks.remove(&BlockId(2));
        assert!(eu.verify_result().is_err());
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&before, &eu);
    }

    #[test]
    fn sinks_normal_arithmetic_to_the_final_priority_leaf() {
        let mut register_map = HashMap::default();
        register_map.insert(RegisterId(0), bit(1));
        register_map.insert(RegisterId(1), bit(1));
        for reg in 2..=5 {
            register_map.insert(RegisterId(reg), bit(8));
        }
        let mut blocks = HashMap::default();
        insert_block(
            &mut blocks,
            0,
            vec![RegisterId(0), RegisterId(1), RegisterId(2)],
            vec![
                SIRInstruction::Binary(RegisterId(3), RegisterId(2), BinaryOp::Mul, RegisterId(2)),
                SIRInstruction::Binary(RegisterId(4), RegisterId(3), BinaryOp::Add, RegisterId(2)),
                SIRInstruction::Imm(RegisterId(5), SIRValue::new(0u8)),
            ],
            SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(1), Vec::new()),
                false_block: (BlockId(2), Vec::new()),
            },
        );
        insert_block(
            &mut blocks,
            1,
            Vec::new(),
            vec![SIRInstruction::Store(
                address(70),
                SIROffset::Static(0),
                8,
                RegisterId(5),
                Vec::new(),
                Vec::new(),
            )],
            SIRTerminator::Jump(BlockId(5), Vec::new()),
        );
        insert_block(
            &mut blocks,
            2,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Branch {
                cond: RegisterId(1),
                true_block: (BlockId(3), Vec::new()),
                false_block: (BlockId(4), Vec::new()),
            },
        );
        insert_block(
            &mut blocks,
            3,
            Vec::new(),
            vec![SIRInstruction::Store(
                address(70),
                SIROffset::Static(0),
                8,
                RegisterId(2),
                Vec::new(),
                Vec::new(),
            )],
            SIRTerminator::Jump(BlockId(5), Vec::new()),
        );
        insert_block(
            &mut blocks,
            4,
            Vec::new(),
            vec![SIRInstruction::Store(
                address(70),
                SIROffset::Static(0),
                8,
                RegisterId(4),
                Vec::new(),
                Vec::new(),
            )],
            SIRTerminator::Jump(BlockId(5), Vec::new()),
        );
        insert_block(
            &mut blocks,
            5,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Return,
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };
        eu.verify_result().unwrap();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        eu.verify_result().unwrap();
        assert!(
            !eu.blocks[&BlockId(0)]
                .instructions
                .iter()
                .any(|instruction| {
                    matches!(def_reg(instruction), Some(RegisterId(3) | RegisterId(4)))
                })
        );
        assert!(
            eu.blocks[&BlockId(4)]
                .instructions
                .iter()
                .any(|instruction| {
                    matches!(instruction, SIRInstruction::Binary(RegisterId(3), ..))
                })
        );
        assert!(
            eu.blocks[&BlockId(4)]
                .instructions
                .iter()
                .any(|instruction| {
                    matches!(instruction, SIRInstruction::Binary(RegisterId(4), ..))
                })
        );
        assert!(
            eu.blocks[&BlockId(1)]
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, SIRInstruction::Imm(RegisterId(5), ..)))
        );
    }

    #[test]
    fn repairs_conditional_availability_across_repeated_priority_regions() {
        let mut register_map = HashMap::default();
        register_map.insert(RegisterId(0), bit(1));
        for reg in 1..=3 {
            register_map.insert(RegisterId(reg), bit(8));
        }
        let mut blocks = HashMap::default();
        insert_block(
            &mut blocks,
            0,
            vec![RegisterId(0), RegisterId(1)],
            vec![
                SIRInstruction::Binary(RegisterId(2), RegisterId(1), BinaryOp::Mul, RegisterId(1)),
                SIRInstruction::Imm(RegisterId(3), SIRValue::new(0u8)),
            ],
            SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(1), Vec::new()),
                false_block: (BlockId(2), Vec::new()),
            },
        );
        for (id, source) in [(1, RegisterId(3)), (2, RegisterId(2))] {
            insert_block(
                &mut blocks,
                id,
                Vec::new(),
                vec![SIRInstruction::Store(
                    address(80),
                    SIROffset::Static(0),
                    8,
                    source,
                    Vec::new(),
                    Vec::new(),
                )],
                SIRTerminator::Jump(BlockId(3), Vec::new()),
            );
        }
        insert_block(
            &mut blocks,
            3,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(4), Vec::new()),
                false_block: (BlockId(5), Vec::new()),
            },
        );
        for (id, source) in [(4, RegisterId(3)), (5, RegisterId(2))] {
            insert_block(
                &mut blocks,
                id,
                Vec::new(),
                vec![SIRInstruction::Store(
                    address(81),
                    SIROffset::Static(0),
                    8,
                    source,
                    Vec::new(),
                    Vec::new(),
                )],
                SIRTerminator::Jump(BlockId(6), Vec::new()),
            );
        }
        insert_block(
            &mut blocks,
            6,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Return,
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };
        eu.verify_result().unwrap();
        let before = eu.clone();

        sink_pure_values_with_predicate_repair(&mut eu);

        eu.verify_result().unwrap();
        assert_eq!(execute(&before, 0, 7), execute(&eu, 0, 7));
        assert_eq!(execute(&before, 1, 7), execute(&eu, 1, 7));
        assert!(
            !eu.blocks[&BlockId(0)]
                .instructions
                .iter()
                .any(|instruction| matches!(
                    instruction,
                    SIRInstruction::Binary(RegisterId(2), ..)
                ))
        );
        assert!(
            eu.blocks[&BlockId(2)]
                .instructions
                .iter()
                .any(|instruction| matches!(
                    instruction,
                    SIRInstruction::Binary(RegisterId(2), ..)
                ))
        );
        assert_eq!(eu.blocks[&BlockId(3)].params.len(), 2);
        assert!(matches!(
            eu.blocks[&BlockId(5)].instructions.last(),
            Some(SIRInstruction::Store(_, _, 8, source, _, _)) if *source != RegisterId(2)
        ));
    }

    #[test]
    fn prunes_priority_phi_payloads_on_early_exit_edges() {
        let mut register_map = HashMap::default();
        for reg in [0, 1, 4, 5] {
            register_map.insert(RegisterId(reg), bit(1));
        }
        for reg in [2, 3, 6] {
            register_map.insert(RegisterId(reg), bit(8));
        }
        let mut blocks = HashMap::default();
        insert_block(
            &mut blocks,
            0,
            vec![RegisterId(0), RegisterId(1), RegisterId(2)],
            vec![SIRInstruction::Binary(
                RegisterId(3),
                RegisterId(2),
                BinaryOp::Mul,
                RegisterId(2),
            )],
            SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(1), Vec::new()),
                false_block: (BlockId(2), Vec::new()),
            },
        );
        insert_block(
            &mut blocks,
            1,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Jump(
                BlockId(5),
                vec![RegisterId(0), RegisterId(1), RegisterId(3)],
            ),
        );
        insert_block(
            &mut blocks,
            2,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Branch {
                cond: RegisterId(1),
                true_block: (BlockId(3), Vec::new()),
                false_block: (BlockId(4), Vec::new()),
            },
        );
        for id in [3, 4] {
            insert_block(
                &mut blocks,
                id,
                Vec::new(),
                Vec::new(),
                SIRTerminator::Jump(
                    BlockId(5),
                    vec![RegisterId(0), RegisterId(1), RegisterId(3)],
                ),
            );
        }
        insert_block(
            &mut blocks,
            5,
            vec![RegisterId(4), RegisterId(5), RegisterId(6)],
            Vec::new(),
            SIRTerminator::Branch {
                cond: RegisterId(4),
                true_block: (BlockId(6), Vec::new()),
                false_block: (BlockId(7), Vec::new()),
            },
        );
        insert_block(
            &mut blocks,
            6,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Jump(BlockId(9), Vec::new()),
        );
        insert_block(
            &mut blocks,
            7,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Branch {
                cond: RegisterId(5),
                true_block: (BlockId(8), Vec::new()),
                false_block: (BlockId(10), Vec::new()),
            },
        );
        insert_block(
            &mut blocks,
            8,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Jump(BlockId(9), Vec::new()),
        );
        insert_block(
            &mut blocks,
            10,
            Vec::new(),
            vec![SIRInstruction::Store(
                address(82),
                SIROffset::Static(0),
                8,
                RegisterId(6),
                Vec::new(),
                Vec::new(),
            )],
            SIRTerminator::Jump(BlockId(9), Vec::new()),
        );
        insert_block(
            &mut blocks,
            9,
            Vec::new(),
            Vec::new(),
            SIRTerminator::Return,
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };
        eu.verify_result().unwrap();

        sink_pure_values_with_predicate_repair(&mut eu);

        eu.verify_result().unwrap();
        assert!(
            !eu.blocks[&BlockId(0)]
                .instructions
                .iter()
                .any(|instruction| def_reg(instruction) == Some(RegisterId(3)))
        );
        assert!(
            eu.blocks[&BlockId(4)]
                .instructions
                .iter()
                .any(|instruction| def_reg(instruction) == Some(RegisterId(3)))
        );
        for early_exit in [BlockId(1), BlockId(3)] {
            let SIRTerminator::Jump(_, arguments) = &eu.blocks[&early_exit].terminator else {
                unreachable!()
            };
            assert_ne!(arguments[2], RegisterId(3));
        }
        let SIRTerminator::Jump(_, arguments) = &eu.blocks[&BlockId(4)].terminator else {
            unreachable!()
        };
        assert_eq!(arguments[2], RegisterId(3));
    }

    #[test]
    fn block_id_overflow_is_non_destructive() {
        let mut eu = shared_dag_unit();
        let terminal = eu.blocks.remove(&BlockId(3)).unwrap();
        let max = BlockId(usize::MAX);
        eu.blocks.insert(
            max,
            BasicBlock {
                id: max,
                ..terminal
            },
        );
        for id in [BlockId(1), BlockId(2)] {
            eu.blocks.get_mut(&id).unwrap().terminator = SIRTerminator::Jump(max, Vec::new());
        }
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&before, &eu);
    }

    #[test]
    fn native_block_id_overflow_is_non_destructive() {
        let mut eu = shared_dag_unit();
        let terminal = eu.blocks.remove(&BlockId(3)).unwrap();
        let max = BlockId(u32::MAX as usize);
        eu.blocks.insert(
            max,
            BasicBlock {
                id: max,
                ..terminal
            },
        );
        for id in [BlockId(1), BlockId(2)] {
            eu.blocks.get_mut(&id).unwrap().terminator = SIRTerminator::Jump(max, Vec::new());
        }
        eu.verify_result().unwrap();
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&before, &eu);
    }
}
