use super::pass_manager::ExecutionUnitPass;
use super::shared::{def_reg, normalize_branch_condition};
use crate::ir::{
    BasicBlock, BlockId, ExecutionUnit, RegionedAbsoluteAddr, RegisterId, SIRInstruction,
    SIROffset, SIRTerminator,
};
use crate::optimizer::PassOptions;
use crate::{HashMap, HashSet};
use std::collections::{BTreeMap, VecDeque};

pub(super) struct BranchifyMuxPass;

#[derive(Clone)]
struct BranchifyPlan {
    block_id: BlockId,
    mux_idx: usize,
    dst: RegisterId,
    cond: RegisterId,
    true_val: RegisterId,
    false_val: RegisterId,
    true_defs: Vec<usize>,
    false_defs: Vec<usize>,
    distributed_store: Option<DistributedStore>,
    preserve_result: bool,
}

#[derive(Clone)]
struct DistributedStore {
    idx: usize,
    true_inst: SIRInstruction<RegionedAbsoluteAddr>,
    false_inst: SIRInstruction<RegionedAbsoluteAddr>,
}

/// CFG facts used by BranchifyMux.  The old implementation looked only at the
/// block containing a Mux, which made it blind to the normal SSA shape
/// produced by lowering:
///
/// ```text
///             branch p
///             /       \
///       compute t   compute f
///             \       /
///              join: Mux(p, t, f)
/// ```
///
/// In that shape the arm work is already control-dependent, but the Mux still
/// survives as a branchless select.  The analysis below is deliberately
/// function-wide: it uses the complete predecessor graph, dominators and a
/// post-dominator tree to identify the controlled join in linear-ish time.
struct CfgAnalysis {
    dominators: super::pass_guarded_region_sinking::Dominators,
    postdominators: PostDominatorTree,
    path_facts: PathFacts,
}

#[derive(Clone)]
struct BranchInfo {
    source: BlockId,
    true_target: BlockId,
    false_target: BlockId,
}

#[derive(Clone)]
struct ControlledMuxPlan {
    join: BlockId,
    mux_idx: usize,
    dst: RegisterId,
    true_val: RegisterId,
    false_val: RegisterId,
    /// Each incoming edge is classified by the original branch's truth value.
    incoming: Vec<ControlledIncomingEdge>,
}

#[derive(Clone, Copy)]
struct ControlledIncomingEdge {
    predecessor: BlockId,
    select_true: bool,
    /// `Some(true)`/`Some(false)` identifies a branch edge. `None` is a jump.
    edge_truth: Option<bool>,
}

struct PathFacts {
    entry_facts: HashMap<BlockId, HashMap<PathFactKey, bool>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum PathFactKey {
    Register(RegisterId),
    Predicate(PredicateKey),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PredicateKey {
    lhs: RegisterId,
    kind: PredicateKind,
    rhs: PredicateRhs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum PredicateKind {
    Equal,
    NotEqual,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum PredicateRhs {
    Register(RegisterId),
    Constant(Vec<u64>, Vec<u64>),
}

#[derive(Clone)]
struct LocatedInstruction {
    block: BlockId,
    index: usize,
    instruction: SIRInstruction<RegionedAbsoluteAddr>,
}

#[derive(Clone)]
struct CrossBlockBranchifyPlan {
    block_id: BlockId,
    mux_idx: usize,
    dst: RegisterId,
    cond: RegisterId,
    condition_defs: Vec<LocatedInstruction>,
    true_val: RegisterId,
    false_val: RegisterId,
    true_defs: Vec<LocatedInstruction>,
    false_defs: Vec<LocatedInstruction>,
}

#[derive(Clone)]
struct PriorityChainMux {
    mux_idx: usize,
    dst: RegisterId,
    cond: RegisterId,
    true_val: RegisterId,
    false_val: RegisterId,
}

#[derive(Clone)]
struct CrossBlockPriorityChainPlan {
    block_id: BlockId,
    first_mux_idx: usize,
    muxes: Vec<PriorityChainMux>,
    condition_defs: Vec<Vec<LocatedInstruction>>,
}

#[derive(Clone)]
struct CrossGroupMux {
    mux_idx: usize,
    dst: RegisterId,
    true_val: RegisterId,
    false_val: RegisterId,
    condition_inverted: bool,
}

#[derive(Clone)]
struct CrossBlockGroupBranchifyPlan {
    block_id: BlockId,
    first_mux_idx: usize,
    branch_cond: RegisterId,
    muxes: Vec<CrossGroupMux>,
    true_defs: Vec<LocatedInstruction>,
    false_defs: Vec<LocatedInstruction>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct UseLocation {
    block: BlockId,
    instruction: Option<usize>,
}

impl ExecutionUnitPass for BranchifyMuxPass {
    fn name(&self) -> &'static str {
        "branchify_mux"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        // A four-state Mux bitwise-merges its arms for an X/Z condition.
        // Control flow selects only one arm, so branchification cannot preserve
        // that behavior.
        if options.four_state {
            return;
        }
        // First consume Muxes whose arms are already guarded by an existing
        // branch.  This is the CFG case the old block-local pass missed: no
        // new control flow is needed, so the selected value can be carried as
        // a block parameter and the branchless Mux can be deleted outright.
        // Plan all such rewrites from one CFG snapshot; do not repeatedly
        // rescan the whole function for each Mux.
        eliminate_controlled_join_muxes(eu);

        let stats = std::env::var_os("CELOX_BRANCHIFY_STATS").is_some();
        let stats_start = stats.then(crate::timing::now);
        let trace_reg = std::env::var("CELOX_BRANCHIFY_TRACE_REG")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .map(RegisterId);
        let mut use_counts = count_uses(eu);
        let mut def_blocks = instruction_def_blocks(eu);
        let mut next_block_id = eu.blocks.keys().map(|id| id.0).max().unwrap_or(0) + 1;
        let mut reg_counter = eu.register_map.keys().map(|reg| reg.0).max().unwrap_or(0);
        let mut applied = 0usize;

        // A priority spine is one short-circuit expression, not a collection
        // of independent selects.  Handle the whole spine before the
        // single-Mux motion below so later conditions and their pure compare
        // DAGs are evaluated only on the fall-through path.
        while let Some(plan) = find_cross_block_priority_chain_plan(eu, &use_counts) {
            apply_cross_block_priority_chain(eu, plan, &mut next_block_id, &mut reg_counter);
            applied += 1;
            use_counts = count_uses(eu);
            def_blocks = instruction_def_blocks(eu);
        }

        // The local transform below can only move definitions from one basic
        // block.  Before using it, repeatedly consume a conservative
        // cross-block plan: every moved instruction must be pure, its defining
        // block must dominate the Mux block, and every moved definition must
        // have exactly one use in the selected arm.  This is the useful CFG
        // region case when lowering left a straight-line DAG split over
        // several basic blocks.
        while let Some(plan) = find_cross_block_group_branchify_plan(eu) {
            apply_cross_block_group_branchify(eu, plan, &mut next_block_id, &mut reg_counter);
            applied += 1;
            use_counts = count_uses(eu);
            def_blocks = instruction_def_blocks(eu);
        }
        while let Some(plan) = find_cross_block_branchify_plan(eu, &use_counts) {
            apply_cross_block_branchify(eu, plan, &mut next_block_id, &mut reg_counter);
            applied += 1;
            use_counts = count_uses(eu);
            def_blocks = instruction_def_blocks(eu);
        }
        let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
        block_ids.sort_by_key(|id| id.0);
        let mut worklist = VecDeque::from(block_ids);
        let mut queued = HashSet::default();
        queued.extend(worklist.iter().copied());
        while let Some(block_id) = worklist.pop_front() {
            queued.remove(&block_id);
            if !eu.blocks.contains_key(&block_id) {
                continue;
            }
            while let Some(plan) =
                find_branchify_mux_in_block(eu, block_id, &use_counts, &def_blocks)
            {
                let new_blocks = apply_branchify_mux(
                    eu,
                    plan,
                    &mut use_counts,
                    &mut def_blocks,
                    &mut next_block_id,
                    &mut reg_counter,
                    trace_reg,
                );
                applied += 1;
                if stats && applied.is_multiple_of(1000) {
                    let insts = eu
                        .blocks
                        .values()
                        .map(|block| block.instructions.len())
                        .sum::<usize>();
                    eprintln!(
                        "[branchify-stats] applied={applied} blocks={} insts={} worklist={} elapsed={:?}",
                        eu.blocks.len(),
                        insts,
                        worklist.len(),
                        stats_start.unwrap().elapsed()
                    );
                }
                for new_block in new_blocks {
                    if queued.insert(new_block) {
                        worklist.push_back(new_block);
                    }
                }
            }
        }
        if stats {
            eprintln!(
                "[branchify-stats] before_pre_repair_inline applied={applied} blocks={} elapsed={:?}",
                eu.blocks.len(),
                stats_start.unwrap().elapsed()
            );
        }
        inline_param_only_jump_blocks(eu);
        inline_param_only_jump_blocks(eu);
        if stats {
            let insts = eu
                .blocks
                .values()
                .map(|block| block.instructions.len())
                .sum::<usize>();
            eprintln!(
                "[branchify-stats] done applied={applied} blocks={} insts={} elapsed={:?}",
                eu.blocks.len(),
                insts,
                stats_start.unwrap().elapsed()
            );
        }
        if std::env::var_os("CELOX_BRANCHIFY_VERIFY").is_some() {
            verify_all_uses_have_defs(eu);
        }
    }
}

fn find_cross_block_priority_chain_plan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    use_counts: &HashMap<RegisterId, usize>,
) -> Option<CrossBlockPriorityChainPlan> {
    let cfg = CfgAnalysis::compute(eu);
    let locations = instruction_def_locations(eu);
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable_by_key(|id| id.0);

    for block_id in block_ids {
        let block = &eu.blocks[&block_id];
        for first_mux_idx in 0..block.instructions.len() {
            let SIRInstruction::Mux(dst, cond, true_val, false_val) =
                &block.instructions[first_mux_idx]
            else {
                continue;
            };
            let mut muxes = vec![PriorityChainMux {
                mux_idx: first_mux_idx,
                dst: *dst,
                cond: *cond,
                true_val: *true_val,
                false_val: *false_val,
            }];
            while let Some(index) = first_mux_idx.checked_add(muxes.len()) {
                let Some(SIRInstruction::Mux(dst, cond, true_val, false_val)) =
                    block.instructions.get(index)
                else {
                    break;
                };
                if *false_val != muxes.last().expect("chain has a first mux").dst {
                    break;
                }
                muxes.push(PriorityChainMux {
                    mux_idx: index,
                    dst: *dst,
                    cond: *cond,
                    true_val: *true_val,
                    false_val: *false_val,
                });
            }
            if muxes.len() < 2
                || muxes
                    .iter()
                    .take(muxes.len() - 1)
                    .any(|mux| use_counts.get(&mux.dst).copied().unwrap_or(0) != 1)
            {
                continue;
            }

            let mut condition_defs = Vec::with_capacity(muxes.len());
            let mut moved_locations = HashSet::default();
            let mut valid = true;
            for mux in &muxes {
                let mut seen = HashSet::default();
                let Some(defs) = collect_cross_arm_defs(
                    eu,
                    &cfg,
                    use_counts,
                    &locations,
                    block_id,
                    first_mux_idx,
                    mux.cond,
                    true,
                    &mut seen,
                ) else {
                    valid = false;
                    break;
                };
                let defs = defs
                    .into_iter()
                    .filter(|def| def.block != block_id)
                    .collect::<Vec<_>>();
                for def in &defs {
                    if !moved_locations.insert((def.block, def.index)) {
                        valid = false;
                        break;
                    }
                }
                if !valid {
                    break;
                }
                condition_defs.push(defs);
            }
            if !valid || moved_locations.is_empty() || condition_defs.iter().all(Vec::is_empty) {
                continue;
            }

            let head = block
                .instructions
                .iter()
                .enumerate()
                .take(first_mux_idx)
                .map(|(_, instruction)| instruction.clone())
                .collect::<Vec<_>>();
            let outer_condition_defs = condition_defs.last().expect("chain has an outer condition");
            if moved_defs_insertion_index(&head, outer_condition_defs).is_none() {
                continue;
            }

            let condition_cost = condition_defs
                .iter()
                .flatten()
                .map(|def| branchified_instruction_cost(&def.instruction, &eu.register_map))
                .sum::<u128>();
            let removed_mux_cost = muxes
                .iter()
                .map(|mux| {
                    branchified_instruction_cost(&block.instructions[mux.mux_idx], &eu.register_map)
                })
                .sum::<u128>();
            let suffix = block
                .instructions
                .iter()
                .skip(muxes.last().expect("chain has a first mux").mux_idx + 1)
                .cloned()
                .collect::<Vec<_>>();
            let live_through = block_live_ins(&suffix, &terminator_uses(&block.terminator));
            let chunks_for = |value: RegisterId| {
                eu.register_map
                    .get(&value)
                    .map(|register| register.width().div_ceil(64).max(1))
                    .unwrap_or(1) as u128
            };
            let live_through_cost = live_through
                .into_iter()
                .filter(|value| *value != muxes.last().expect("chain has a first mux").dst)
                .map(chunks_for)
                .sum::<u128>();
            let introduced_cost = (muxes.len() as u128)
                .saturating_mul(BRANCH_CONTROL_COST)
                .saturating_add(
                    chunks_for(muxes.last().expect("chain has a first mux").dst)
                        .saturating_mul(PHI_COPY_COST_PER_CHUNK),
                )
                .saturating_add(live_through_cost);
            if condition_cost.saturating_add(removed_mux_cost) <= introduced_cost {
                continue;
            }

            return Some(CrossBlockPriorityChainPlan {
                block_id,
                first_mux_idx,
                muxes,
                condition_defs,
            });
        }
    }
    None
}

fn apply_cross_block_priority_chain(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: CrossBlockPriorityChainPlan,
    next_block_id: &mut usize,
    reg_counter: &mut usize,
) {
    let true_leaf_id = BlockId(*next_block_id + plan.muxes.len() - 1);
    let merge_id = BlockId(*next_block_id + plan.muxes.len());
    let decision_ids = (0..plan.muxes.len() - 1)
        .map(|index| BlockId(*next_block_id + index))
        .collect::<Vec<_>>();
    *next_block_id += plan.muxes.len() + 1;

    let removed_locations = plan
        .condition_defs
        .iter()
        .flatten()
        .map(located_instruction_key)
        .chain(plan.muxes.iter().map(|mux| (plan.block_id, mux.mux_idx)))
        .collect::<HashSet<_>>();
    let original = eu
        .blocks
        .remove(&plan.block_id)
        .expect("priority chain target block must exist");
    for block in eu.blocks.values_mut() {
        block.instructions = block
            .instructions
            .iter()
            .enumerate()
            .filter(|(index, _)| !removed_locations.contains(&(block.id, *index)))
            .map(|(_, instruction)| instruction.clone())
            .collect();
    }

    let outer_index = plan.muxes.len() - 1;
    let outer = &plan.muxes[outer_index];
    let mut head_insts = original
        .instructions
        .iter()
        .enumerate()
        .take(plan.first_mux_idx)
        .filter(|(index, _)| !removed_locations.contains(&(plan.block_id, *index)))
        .map(|(_, instruction)| instruction.clone())
        .collect::<Vec<_>>();
    let insertion = moved_defs_insertion_index(&head_insts, &plan.condition_defs[outer_index])
        .expect("priority-chain condition definitions must have an SSA insertion point");
    head_insts.splice(
        insertion..insertion,
        plan.condition_defs[outer_index]
            .iter()
            .map(|def| def.instruction.clone()),
    );
    let head_cond = normalize_branch_condition(
        &mut eu.register_map,
        &mut head_insts,
        outer.cond,
        reg_counter,
    );
    let head_false = if outer_index == 0 {
        (merge_id, vec![outer.false_val])
    } else {
        (decision_ids[outer_index - 1], Vec::new())
    };
    let head = BasicBlock {
        id: plan.block_id,
        params: original.params,
        instructions: head_insts,
        terminator: SIRTerminator::Branch {
            cond: head_cond,
            true_block: (merge_id, vec![outer.true_val]),
            false_block: head_false,
        },
    };
    eu.blocks.insert(plan.block_id, head);

    for index in (0..outer_index).rev() {
        let mux = &plan.muxes[index];
        let mut instructions = plan.condition_defs[index]
            .iter()
            .map(|def| def.instruction.clone())
            .collect::<Vec<_>>();
        let cond = normalize_branch_condition(
            &mut eu.register_map,
            &mut instructions,
            mux.cond,
            reg_counter,
        );
        let false_target = if index == 0 {
            (merge_id, vec![mux.false_val])
        } else {
            (decision_ids[index - 1], Vec::new())
        };
        let true_target = if index == 0 {
            (true_leaf_id, Vec::new())
        } else {
            (merge_id, vec![mux.true_val])
        };
        eu.blocks.insert(
            decision_ids[index],
            BasicBlock {
                id: decision_ids[index],
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Branch {
                    cond,
                    true_block: true_target,
                    false_block: false_target,
                },
            },
        );
    }

    let final_mux = &plan.muxes[0];
    eu.blocks.insert(
        true_leaf_id,
        BasicBlock {
            id: true_leaf_id,
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(merge_id, vec![final_mux.true_val]),
        },
    );

    let suffix = original
        .instructions
        .iter()
        .enumerate()
        .skip(plan.muxes.last().expect("chain has a first mux").mux_idx + 1)
        .filter(|(index, _)| !removed_locations.contains(&(plan.block_id, *index)))
        .map(|(_, instruction)| instruction.clone())
        .collect::<Vec<_>>();
    eu.blocks.insert(
        merge_id,
        BasicBlock {
            id: merge_id,
            params: vec![outer.dst],
            instructions: suffix,
            terminator: original.terminator,
        },
    );
}

fn find_cross_block_branchify_plan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    use_counts: &HashMap<RegisterId, usize>,
) -> Option<CrossBlockBranchifyPlan> {
    let cfg = CfgAnalysis::compute(eu);
    let def_locations = instruction_def_locations(eu);
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable_by_key(|id| id.0);

    for block_id in block_ids {
        let block = &eu.blocks[&block_id];
        for (mux_idx, inst) in block.instructions.iter().enumerate() {
            let SIRInstruction::Mux(dst, cond, true_val, false_val) = inst else {
                continue;
            };
            let mut condition_seen = HashSet::default();
            let Some(condition_defs) = collect_cross_arm_defs(
                eu,
                &cfg,
                use_counts,
                &def_locations,
                block_id,
                mux_idx,
                *cond,
                true,
                &mut condition_seen,
            ) else {
                continue;
            };
            // Definitions already in the Mux block are kept in their original
            // order.  Only a dominating cross-block slice is actually moved
            // into the new branch head.
            let condition_defs = condition_defs
                .into_iter()
                .filter(|def| def.block != block_id)
                .collect::<Vec<_>>();
            let head = block
                .instructions
                .iter()
                .enumerate()
                .take(mux_idx)
                .map(|(_, instruction)| instruction.clone())
                .collect::<Vec<_>>();
            if moved_defs_insertion_index(&head, &condition_defs).is_none() {
                continue;
            }
            let mut true_seen = HashSet::default();
            let mut false_seen = HashSet::default();
            let Some(true_defs) = collect_cross_arm_defs(
                eu,
                &cfg,
                use_counts,
                &def_locations,
                block_id,
                mux_idx,
                *true_val,
                true,
                &mut true_seen,
            ) else {
                continue;
            };
            let Some(false_defs) = collect_cross_arm_defs(
                eu,
                &cfg,
                use_counts,
                &def_locations,
                block_id,
                mux_idx,
                *false_val,
                true,
                &mut false_seen,
            ) else {
                continue;
            };
            if condition_defs.is_empty() && true_defs.is_empty() && false_defs.is_empty() {
                continue;
            }
            let condition_locations = condition_defs
                .iter()
                .map(|def| (def.block, def.index))
                .collect::<HashSet<_>>();
            let true_locations = true_defs
                .iter()
                .map(|def| (def.block, def.index))
                .collect::<HashSet<_>>();
            if condition_locations
                .intersection(&true_locations)
                .next()
                .is_some()
                || condition_locations
                    .intersection(
                        &false_defs
                            .iter()
                            .map(|def| (def.block, def.index))
                            .collect::<HashSet<_>>(),
                    )
                    .next()
                    .is_some()
            {
                continue;
            }
            if false_defs
                .iter()
                .any(|def| true_locations.contains(&(def.block, def.index)))
            {
                continue;
            }
            if !condition_defs
                .iter()
                .chain(true_defs.iter())
                .chain(false_defs.iter())
                .any(|def| def.block != block_id)
            {
                // The existing block-local planner has a more precise memory
                // and live-through model for this case.
                continue;
            }

            let plan = CrossBlockBranchifyPlan {
                block_id,
                mux_idx,
                dst: *dst,
                cond: *cond,
                condition_defs,
                true_val: *true_val,
                false_val: *false_val,
                true_defs,
                false_defs,
            };
            if cross_block_branch_is_profitable(eu, &plan) {
                return Some(plan);
            }
        }
    }
    None
}

/// Return the only insertion point that keeps a moved condition DAG in SSA
/// order.  Its external operands must already be defined in the target head,
/// while every use of a moved result must remain after the inserted DAG.
fn moved_defs_insertion_index(
    head: &[SIRInstruction<RegionedAbsoluteAddr>],
    moved: &[LocatedInstruction],
) -> Option<usize> {
    if moved.is_empty() {
        return Some(head.len());
    }
    let moved_registers = moved
        .iter()
        .filter_map(|def| def_reg(&def.instruction))
        .collect::<HashSet<_>>();
    if moved_registers.len() != moved.len() {
        return None;
    }

    let first_use = head
        .iter()
        .position(|instruction| {
            inst_uses(instruction)
                .iter()
                .any(|reg| moved_registers.contains(reg))
        })
        .unwrap_or(head.len());
    let mut insertion = 0usize;
    for definition in moved {
        for operand in inst_uses(&definition.instruction) {
            if moved_registers.contains(&operand) {
                continue;
            }
            if let Some(index) = head
                .iter()
                .position(|instruction| def_reg(instruction) == Some(operand))
            {
                insertion = insertion.max(index + 1);
            }
        }
    }
    (insertion <= first_use).then_some(insertion)
}

/// Find a group of selects driven by the same predicate.  Treating each Mux
/// independently misses the important case where several selected values
/// share one arm DAG:
///
/// ```text
///   t = expensive(...)
///   a = Mux(p, t, a0)
///   b = Mux(p, t, b0)
/// ```
///
/// `t` has two uses, so a single-use walk rejects it even though it is safe to
/// compute it once in the true arm and pass both selected results through one
/// merge.  The group analysis below classifies all uses of the candidate DAG,
/// so a definition is moved only when every use is on the same arm or is
/// another Mux in this group.
fn find_cross_block_group_branchify_plan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> Option<CrossBlockGroupBranchifyPlan> {
    let cfg = CfgAnalysis::compute(eu);
    let def_locations = instruction_def_locations(eu);
    let def_blocks = all_def_blocks(eu);
    let use_locations = register_use_locations(eu);
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable_by_key(|id| id.0);

    for block_id in block_ids {
        let block = &eu.blocks[&block_id];
        let mut groups = HashMap::<(RegisterId, bool), Vec<CrossGroupMux>>::default();
        for (mux_idx, inst) in block.instructions.iter().enumerate() {
            let SIRInstruction::Mux(dst, condition, true_val, false_val) = inst else {
                continue;
            };
            let (root, condition_inverted) = resolve_boolean_alias(eu, &def_locations, *condition);
            groups
                .entry((root, condition_inverted))
                .or_default()
                .push(CrossGroupMux {
                    mux_idx,
                    dst: *dst,
                    true_val: *true_val,
                    false_val: *false_val,
                    condition_inverted,
                });
        }

        let mut groups = groups
            .into_iter()
            .filter(|(_, muxes)| muxes.len() >= 2)
            .collect::<Vec<_>>();
        groups.sort_unstable_by_key(|((root, inverted), muxes)| {
            (muxes[0].mux_idx, root.0, *inverted as u8)
        });

        for ((branch_cond, _), muxes) in groups {
            let first_mux_idx = muxes[0].mux_idx;
            if !cross_group_value_available(
                &cfg,
                &def_blocks,
                &def_locations,
                block_id,
                first_mux_idx,
                branch_cond,
                &HashSet::default(),
            ) {
                continue;
            }

            let true_roots = muxes.iter().map(|mux| mux.true_val).collect::<Vec<_>>();
            let false_roots = muxes.iter().map(|mux| mux.false_val).collect::<Vec<_>>();
            let true_all = collect_cross_group_defs(
                eu,
                &cfg,
                &def_locations,
                block_id,
                first_mux_idx,
                &true_roots,
            );
            let false_all = collect_cross_group_defs(
                eu,
                &cfg,
                &def_locations,
                block_id,
                first_mux_idx,
                &false_roots,
            );
            if true_all.is_empty() && false_all.is_empty() {
                continue;
            }

            let true_all_locations = instruction_locations(&true_all);
            let false_all_locations = instruction_locations(&false_all);
            let true_movable = filter_cross_group_defs(
                eu,
                block_id,
                &true_all,
                &false_all_locations,
                true,
                &muxes,
                &use_locations,
            );
            let false_movable = filter_cross_group_defs(
                eu,
                block_id,
                &false_all,
                &true_all_locations,
                false,
                &muxes,
                &use_locations,
            );
            if true_movable.is_empty() && false_movable.is_empty() {
                continue;
            }

            let true_defs = true_all
                .into_iter()
                .filter(|def| true_movable.contains(&located_instruction_key(def)))
                .collect::<Vec<_>>();
            let false_defs = false_all
                .into_iter()
                .filter(|def| false_movable.contains(&located_instruction_key(def)))
                .collect::<Vec<_>>();

            if muxes.iter().any(|mux| {
                !cross_group_value_available(
                    &cfg,
                    &def_blocks,
                    &def_locations,
                    block_id,
                    first_mux_idx,
                    mux.true_val,
                    &true_movable,
                ) || !cross_group_value_available(
                    &cfg,
                    &def_blocks,
                    &def_locations,
                    block_id,
                    first_mux_idx,
                    mux.false_val,
                    &false_movable,
                )
            }) {
                continue;
            }

            let plan = CrossBlockGroupBranchifyPlan {
                block_id,
                first_mux_idx,
                branch_cond,
                muxes,
                true_defs,
                false_defs,
            };
            if cross_group_branch_is_profitable(eu, &plan) {
                return Some(plan);
            }
        }
    }
    None
}

fn register_use_locations(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, Vec<UseLocation>> {
    let mut uses = HashMap::<RegisterId, Vec<UseLocation>>::default();
    for block in eu.blocks.values() {
        for (index, instruction) in block.instructions.iter().enumerate() {
            for register in inst_uses(instruction) {
                uses.entry(register).or_default().push(UseLocation {
                    block: block.id,
                    instruction: Some(index),
                });
            }
        }
        for register in terminator_uses(&block.terminator) {
            uses.entry(register).or_default().push(UseLocation {
                block: block.id,
                instruction: None,
            });
        }
    }
    uses
}

fn instruction_locations(instructions: &[LocatedInstruction]) -> HashSet<(BlockId, usize)> {
    instructions.iter().map(located_instruction_key).collect()
}

fn located_instruction_key(instruction: &LocatedInstruction) -> (BlockId, usize) {
    (instruction.block, instruction.index)
}

fn collect_cross_group_defs(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &CfgAnalysis,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    mux_block: BlockId,
    first_mux_idx: usize,
    roots: &[RegisterId],
) -> Vec<LocatedInstruction> {
    fn visit(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        cfg: &CfgAnalysis,
        locations: &HashMap<RegisterId, (BlockId, usize)>,
        mux_block: BlockId,
        first_mux_idx: usize,
        register: RegisterId,
        seen: &mut HashSet<(BlockId, usize)>,
        result: &mut Vec<LocatedInstruction>,
    ) {
        let Some(&(block, index)) = locations.get(&register) else {
            return;
        };
        if block == mux_block && index >= first_mux_idx {
            return;
        }
        if !cfg.dominators.dominates(block, mux_block) {
            return;
        }
        let instruction = eu.blocks[&block].instructions[index].clone();
        if !is_cross_block_sinkable_input(&instruction) || !seen.insert((block, index)) {
            return;
        }
        for operand in inst_uses(&instruction) {
            visit(
                eu,
                cfg,
                locations,
                mux_block,
                first_mux_idx,
                operand,
                seen,
                result,
            );
        }
        result.push(LocatedInstruction {
            block,
            index,
            instruction,
        });
    }

    let mut seen = HashSet::default();
    let mut result = Vec::new();
    for &root in roots {
        visit(
            eu,
            cfg,
            locations,
            mux_block,
            first_mux_idx,
            root,
            &mut seen,
            &mut result,
        );
    }
    result
}

fn filter_cross_group_defs(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    mux_block: BlockId,
    candidates: &[LocatedInstruction],
    other_side: &HashSet<(BlockId, usize)>,
    true_side: bool,
    muxes: &[CrossGroupMux],
    use_locations: &HashMap<RegisterId, Vec<UseLocation>>,
) -> HashSet<(BlockId, usize)> {
    let mut movable = candidates
        .iter()
        .map(located_instruction_key)
        .filter(|location| !other_side.contains(location))
        .collect::<HashSet<_>>();

    loop {
        let remove = movable
            .iter()
            .copied()
            .filter(|location| {
                let instruction = &eu.blocks[&location.0].instructions[location.1];
                let Some(definition) = def_reg(instruction) else {
                    return true;
                };
                use_locations
                    .get(&definition)
                    .into_iter()
                    .flatten()
                    .any(|use_location| {
                        if use_location
                            .instruction
                            .is_some_and(|index| movable.contains(&(use_location.block, index)))
                        {
                            return false;
                        }
                        let Some(index) = use_location.instruction else {
                            return true;
                        };
                        if use_location.block != mux_block {
                            return true;
                        }
                        let Some(_mux) = muxes.iter().find(|mux| mux.mux_idx == index) else {
                            return true;
                        };
                        let SIRInstruction::Mux(_, condition, true_val, false_val) =
                            &eu.blocks[&mux_block].instructions[index]
                        else {
                            return true;
                        };
                        if *condition == definition {
                            return true;
                        }
                        if true_side {
                            *true_val != definition || *false_val == definition
                        } else {
                            *false_val != definition || *true_val == definition
                        }
                    })
            })
            .collect::<Vec<_>>();
        if remove.is_empty() {
            break;
        }
        for location in remove {
            movable.remove(&location);
        }
    }
    movable
}

fn cross_group_value_available(
    cfg: &CfgAnalysis,
    def_blocks: &HashMap<RegisterId, BlockId>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    mux_block: BlockId,
    first_mux_idx: usize,
    register: RegisterId,
    moved: &HashSet<(BlockId, usize)>,
) -> bool {
    if let Some(&(block, index)) = def_locations.get(&register) {
        if moved.contains(&(block, index)) {
            return true;
        }
        return cfg.dominators.dominates(block, mux_block)
            && (block != mux_block || index < first_mux_idx);
    }
    def_blocks
        .get(&register)
        .is_some_and(|block| cfg.dominators.dominates(*block, mux_block))
}

fn cross_group_branch_is_profitable(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    plan: &CrossBlockGroupBranchifyPlan,
) -> bool {
    let block = &eu.blocks[&plan.block_id];
    let true_arm_cost = plan
        .true_defs
        .iter()
        .map(|def| branchified_instruction_cost(&def.instruction, &eu.register_map))
        .sum::<u128>();
    let false_arm_cost = plan
        .false_defs
        .iter()
        .map(|def| branchified_instruction_cost(&def.instruction, &eu.register_map))
        .sum::<u128>();
    let group_indices = plan
        .muxes
        .iter()
        .map(|mux| mux.mux_idx)
        .collect::<HashSet<_>>();
    let moved = plan
        .true_defs
        .iter()
        .chain(plan.false_defs.iter())
        .map(located_instruction_key)
        .collect::<HashSet<_>>();
    let suffix = block
        .instructions
        .iter()
        .enumerate()
        .skip(plan.first_mux_idx + 1)
        .filter(|(index, _)| {
            !group_indices.contains(index) && !moved.contains(&(plan.block_id, *index))
        })
        .map(|(_, instruction)| instruction.clone())
        .collect::<Vec<_>>();
    let mut live_through = block_live_ins(&suffix, &terminator_uses(&block.terminator));
    live_through.retain(|value| !plan.muxes.iter().any(|mux| mux.dst == *value));
    live_through.sort_unstable();
    live_through.dedup();
    let chunks_for = |value: RegisterId| {
        eu.register_map
            .get(&value)
            .map(|register| register.width().div_ceil(64).max(1))
            .unwrap_or(1) as u128
    };
    let phi_copy_cost = plan
        .muxes
        .iter()
        .map(|mux| chunks_for(mux.dst).saturating_mul(PHI_COPY_COST_PER_CHUNK))
        .sum::<u128>();
    let live_through_cost = live_through
        .into_iter()
        .map(chunks_for)
        .sum::<u128>()
        .saturating_mul(LIVE_THROUGH_COST_PER_CHUNK);
    let removed_mux_cost = plan
        .muxes
        .iter()
        .map(|mux| block.instructions[mux.mux_idx].clone())
        .map(|instruction| branchified_instruction_cost(&instruction, &eu.register_map))
        .sum::<u128>();
    BranchProfitability {
        true_arm_cost,
        false_arm_cost,
        removed_mux_cost,
        probability: StaticBranchProbability::EVEN,
        control_cost: BRANCH_CONTROL_COST,
        phi_copy_cost,
        live_through_cost,
    }
    .proves_expected_benefit()
}

/// Collect a closed, pure, single-use slice which can be delayed from a
/// dominating block until the Mux's branch arm. A non-movable operand is kept
/// as a live-in; SSA dominance guarantees that it is available at the Mux.
fn collect_cross_arm_defs(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &CfgAnalysis,
    use_counts: &HashMap<RegisterId, usize>,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    mux_block: BlockId,
    mux_idx: usize,
    root: RegisterId,
    root_required: bool,
    seen: &mut HashSet<(BlockId, usize)>,
) -> Option<Vec<LocatedInstruction>> {
    let Some(&(block_id, index)) = locations.get(&root) else {
        return root_required.then(Vec::new);
    };
    if block_id == mux_block && index >= mux_idx {
        return None;
    }
    if !cfg.dominators.dominates(block_id, mux_block) {
        return None;
    }
    if use_counts.get(&root).copied().unwrap_or(0) != 1 {
        return root_required.then(Vec::new);
    }
    let instruction = eu.blocks[&block_id].instructions[index].clone();
    if !is_cross_block_sinkable_input(&instruction) {
        return root_required.then(Vec::new);
    }
    if !seen.insert((block_id, index)) {
        return Some(Vec::new());
    }

    let mut result = Vec::new();
    for operand in inst_uses(&instruction) {
        let can_attempt_move =
            locations
                .get(&operand)
                .is_some_and(|&(operand_block, operand_idx)| {
                    (operand_block != mux_block || operand_idx < mux_idx)
                        && cfg.dominators.dominates(operand_block, mux_block)
                });
        if can_attempt_move
            && use_counts.get(&operand).copied().unwrap_or(0) == 1
            && let Some(operand_defs) = collect_cross_arm_defs(
                eu, cfg, use_counts, locations, mux_block, mux_idx, operand, false, seen,
            )
        {
            result.extend(operand_defs);
        }
    }
    result.push(LocatedInstruction {
        block: block_id,
        index,
        instruction,
    });
    Some(result)
}

fn is_cross_block_sinkable_input(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
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

fn cross_block_branch_is_profitable(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    plan: &CrossBlockBranchifyPlan,
) -> bool {
    let block = &eu.blocks[&plan.block_id];
    let true_arm_cost = plan
        .true_defs
        .iter()
        .map(|def| branchified_instruction_cost(&def.instruction, &eu.register_map))
        .sum::<u128>();
    let false_arm_cost = plan
        .false_defs
        .iter()
        .map(|def| branchified_instruction_cost(&def.instruction, &eu.register_map))
        .sum::<u128>();
    let suffix = block
        .instructions
        .iter()
        .skip(plan.mux_idx + 1)
        .cloned()
        .collect::<Vec<_>>();
    let mut live_through = block_live_ins(&suffix, &terminator_uses(&block.terminator));
    live_through.retain(|value| *value != plan.dst);
    live_through.sort_unstable();
    live_through.dedup();
    let chunks_for = |value: RegisterId| {
        eu.register_map
            .get(&value)
            .map(|register| register.width().div_ceil(64).max(1))
            .unwrap_or(1) as u128
    };
    let phi_copy_cost = chunks_for(plan.dst).saturating_mul(PHI_COPY_COST_PER_CHUNK);
    let live_through_cost = live_through
        .into_iter()
        .map(chunks_for)
        .sum::<u128>()
        .saturating_mul(LIVE_THROUGH_COST_PER_CHUNK);
    BranchProfitability {
        true_arm_cost,
        false_arm_cost,
        removed_mux_cost: branchified_instruction_cost(
            &block.instructions[plan.mux_idx],
            &eu.register_map,
        ),
        probability: StaticBranchProbability::EVEN,
        control_cost: BRANCH_CONTROL_COST,
        phi_copy_cost,
        live_through_cost,
    }
    .proves_expected_benefit()
}

fn apply_cross_block_group_branchify(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: CrossBlockGroupBranchifyPlan,
    next_block_id: &mut usize,
    reg_counter: &mut usize,
) {
    let true_id = BlockId(*next_block_id);
    let false_id = BlockId(*next_block_id + 1);
    let merge_id = BlockId(*next_block_id + 2);
    *next_block_id += 3;

    let mux_indices = plan
        .muxes
        .iter()
        .map(|mux| mux.mux_idx)
        .collect::<HashSet<_>>();
    let removed_locations = plan
        .true_defs
        .iter()
        .chain(plan.false_defs.iter())
        .map(located_instruction_key)
        .chain(plan.muxes.iter().map(|mux| (plan.block_id, mux.mux_idx)))
        .collect::<HashSet<_>>();
    let original = eu
        .blocks
        .remove(&plan.block_id)
        .expect("cross-group branchify target block must exist");
    for block in eu.blocks.values_mut() {
        block.instructions = block
            .instructions
            .iter()
            .enumerate()
            .filter(|(index, _)| !removed_locations.contains(&(block.id, *index)))
            .map(|(_, instruction)| instruction.clone())
            .collect();
    }

    let mut head_insts = original
        .instructions
        .iter()
        .enumerate()
        .take(plan.first_mux_idx)
        .filter(|(index, _)| !removed_locations.contains(&(plan.block_id, *index)))
        .map(|(_, instruction)| instruction.clone())
        .collect::<Vec<_>>();
    let branch_cond = normalize_branch_condition(
        &mut eu.register_map,
        &mut head_insts,
        plan.branch_cond,
        reg_counter,
    );
    let suffix = original
        .instructions
        .iter()
        .enumerate()
        .skip(plan.first_mux_idx + 1)
        .filter(|(index, _)| {
            !mux_indices.contains(index) && !removed_locations.contains(&(plan.block_id, *index))
        })
        .map(|(_, instruction)| instruction.clone())
        .collect::<Vec<_>>();
    let true_insts = plan
        .true_defs
        .iter()
        .map(|def| def.instruction.clone())
        .collect::<Vec<_>>();
    let false_insts = plan
        .false_defs
        .iter()
        .map(|def| def.instruction.clone())
        .collect::<Vec<_>>();
    let true_args = plan
        .muxes
        .iter()
        .map(|mux| {
            if mux.condition_inverted {
                mux.false_val
            } else {
                mux.true_val
            }
        })
        .collect::<Vec<_>>();
    let false_args = plan
        .muxes
        .iter()
        .map(|mux| {
            if mux.condition_inverted {
                mux.true_val
            } else {
                mux.false_val
            }
        })
        .collect::<Vec<_>>();
    let merge_params = plan.muxes.iter().map(|mux| mux.dst).collect::<Vec<_>>();

    let head = BasicBlock {
        id: plan.block_id,
        params: original.params,
        instructions: head_insts,
        terminator: SIRTerminator::Branch {
            cond: branch_cond,
            true_block: (true_id, Vec::new()),
            false_block: (false_id, Vec::new()),
        },
    };
    let true_block = BasicBlock {
        id: true_id,
        params: Vec::new(),
        instructions: true_insts,
        terminator: SIRTerminator::Jump(merge_id, true_args),
    };
    let false_block = BasicBlock {
        id: false_id,
        params: Vec::new(),
        instructions: false_insts,
        terminator: SIRTerminator::Jump(merge_id, false_args),
    };
    let merge_block = BasicBlock {
        id: merge_id,
        params: merge_params,
        instructions: suffix,
        terminator: original.terminator,
    };
    eu.blocks.insert(plan.block_id, head);
    eu.blocks.insert(true_id, true_block);
    eu.blocks.insert(false_id, false_block);
    eu.blocks.insert(merge_id, merge_block);
}

fn apply_cross_block_branchify(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: CrossBlockBranchifyPlan,
    next_block_id: &mut usize,
    reg_counter: &mut usize,
) {
    let true_id = BlockId(*next_block_id);
    let false_id = BlockId(*next_block_id + 1);
    let merge_id = BlockId(*next_block_id + 2);
    *next_block_id += 3;

    let removed_locations = plan
        .condition_defs
        .iter()
        .chain(plan.true_defs.iter())
        .chain(plan.false_defs.iter())
        .map(|def| (def.block, def.index))
        .collect::<HashSet<_>>();
    let original = eu
        .blocks
        .remove(&plan.block_id)
        .expect("cross-block branchify target block must exist");
    for block in eu.blocks.values_mut() {
        block.instructions = block
            .instructions
            .iter()
            .enumerate()
            .filter(|(index, _)| !removed_locations.contains(&(block.id, *index)))
            .map(|(_, instruction)| instruction.clone())
            .collect();
    }

    let mut head_insts = original
        .instructions
        .iter()
        .enumerate()
        .take(plan.mux_idx)
        .filter(|(index, _)| !removed_locations.contains(&(plan.block_id, *index)))
        .map(|(_, instruction)| instruction.clone())
        .collect::<Vec<_>>();
    let insertion = moved_defs_insertion_index(&head_insts, &plan.condition_defs)
        .expect("cross-block condition definitions must have an SSA insertion point");
    head_insts.splice(
        insertion..insertion,
        plan.condition_defs
            .iter()
            .map(|def| def.instruction.clone()),
    );
    let branch_cond = normalize_branch_condition(
        &mut eu.register_map,
        &mut head_insts,
        plan.cond,
        reg_counter,
    );
    let suffix = original
        .instructions
        .iter()
        .enumerate()
        .skip(plan.mux_idx + 1)
        .filter(|(index, _)| !removed_locations.contains(&(plan.block_id, *index)))
        .map(|(_, instruction)| instruction.clone())
        .collect::<Vec<_>>();
    let true_insts = plan
        .true_defs
        .iter()
        .map(|def| def.instruction.clone())
        .collect::<Vec<_>>();
    let false_insts = plan
        .false_defs
        .iter()
        .map(|def| def.instruction.clone())
        .collect::<Vec<_>>();

    let head = BasicBlock {
        id: plan.block_id,
        params: original.params,
        instructions: head_insts,
        terminator: SIRTerminator::Branch {
            cond: branch_cond,
            true_block: (true_id, Vec::new()),
            false_block: (false_id, Vec::new()),
        },
    };
    let true_block = BasicBlock {
        id: true_id,
        params: Vec::new(),
        instructions: true_insts,
        terminator: SIRTerminator::Jump(merge_id, vec![plan.true_val]),
    };
    let false_block = BasicBlock {
        id: false_id,
        params: Vec::new(),
        instructions: false_insts,
        terminator: SIRTerminator::Jump(merge_id, vec![plan.false_val]),
    };
    let merge_block = BasicBlock {
        id: merge_id,
        params: vec![plan.dst],
        instructions: suffix,
        terminator: original.terminator,
    };
    eu.blocks.insert(plan.block_id, head);
    eu.blocks.insert(true_id, true_block);
    eu.blocks.insert(false_id, false_block);
    eu.blocks.insert(merge_id, merge_block);
}

fn eliminate_controlled_join_muxes(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let cfg = CfgAnalysis::compute(eu);
    let def_blocks = all_def_blocks(eu);
    let def_locations = instruction_def_locations(eu);
    let mut branches_by_root = HashMap::<RegisterId, Vec<BranchInfo>>::default();

    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable_by_key(|id| id.0);
    for block_id in block_ids.iter().copied() {
        let block = &eu.blocks[&block_id];
        let SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } = &block.terminator
        else {
            continue;
        };
        let (root, _) = resolve_boolean_alias(eu, &def_locations, *cond);
        branches_by_root.entry(root).or_default().push(BranchInfo {
            source: block_id,
            true_target: true_block.0,
            false_target: false_block.0,
        });
    }
    if branches_by_root.is_empty() {
        return;
    }

    let mut plans = Vec::new();
    for block_id in block_ids {
        let block = &eu.blocks[&block_id];
        for (mux_idx, inst) in block.instructions.iter().enumerate() {
            let SIRInstruction::Mux(dst, condition, true_val, false_val) = inst else {
                continue;
            };
            let (root, _) = resolve_boolean_alias(eu, &def_locations, *condition);
            let plan = branches_by_root
                .get(&root)
                .into_iter()
                .flatten()
                .find_map(|branch| {
                    plan_controlled_join_mux(
                        eu,
                        &cfg,
                        &def_blocks,
                        &def_locations,
                        branch,
                        block_id,
                        mux_idx,
                        *condition,
                        *dst,
                        *true_val,
                        *false_val,
                    )
                })
                .or_else(|| {
                    plan_path_conditioned_join_mux(
                        eu,
                        &cfg,
                        &def_blocks,
                        &def_locations,
                        block_id,
                        mux_idx,
                        *dst,
                        *true_val,
                        *false_val,
                    )
                });
            let Some(plan) = plan else {
                continue;
            };
            plans.push(plan);
        }
    }

    // Removing instructions changes indices, so apply plans in descending
    // order within each join.  Edge arguments are appended in the same order
    // as the new block parameters and therefore remain type/arity-correct.
    plans.sort_unstable_by_key(|plan| (plan.join.0, std::cmp::Reverse(plan.mux_idx)));
    for plan in plans {
        let Some(join) = eu.blocks.get_mut(&plan.join) else {
            continue;
        };
        if plan.mux_idx >= join.instructions.len()
            || !matches!(
                join.instructions[plan.mux_idx],
                SIRInstruction::Mux(dst, ..) if dst == plan.dst
            )
        {
            continue;
        }
        join.instructions.remove(plan.mux_idx);
        join.params.push(plan.dst);

        for edge in plan.incoming {
            let value = if edge.select_true {
                plan.true_val
            } else {
                plan.false_val
            };
            append_controlled_edge_argument(
                eu,
                edge.predecessor,
                plan.join,
                edge.edge_truth,
                value,
            );
        }
    }
}

fn plan_controlled_join_mux(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &CfgAnalysis,
    def_blocks: &HashMap<RegisterId, BlockId>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    branch: &BranchInfo,
    join: BlockId,
    mux_idx: usize,
    condition: RegisterId,
    dst: RegisterId,
    true_val: RegisterId,
    false_val: RegisterId,
) -> Option<ControlledMuxPlan> {
    if branch.source == join
        || !cfg.dominators.dominates(branch.source, join)
        || !cfg.postdominators.postdominates(join, branch.true_target)
        || !cfg.postdominators.postdominates(join, branch.false_target)
    {
        return None;
    }

    let block = eu.blocks.get(&join)?;
    if block.params.contains(&dst)
        || !matches!(
            block.instructions.get(mux_idx),
            Some(SIRInstruction::Mux(..))
        )
    {
        return None;
    }

    let incoming_edges = incoming_edges_to(eu, join);
    if incoming_edges.is_empty() {
        return None;
    }

    let mut incoming = Vec::with_capacity(incoming_edges.len());
    let mut seen_predecessors = HashSet::default();
    for (predecessor, edge_truth) in incoming_edges {
        // A block with two edges to the same join has no unambiguous edge
        // classification for this transform.  Leave it to the general
        // branchifier instead of guessing.
        if !seen_predecessors.insert(predecessor) || predecessor == join {
            return None;
        }

        // A predicate can be branched on repeatedly.  Dominance alone cannot
        // identify which occurrence controls this edge: a CFG-only walk sees
        // infeasible paths that flip the same SSA boolean later.  Derive the
        // Mux's truth value from the actual incoming edge facts instead.
        let facts =
            cfg.path_facts
                .facts_on_edge(eu, def_locations, predecessor, join, edge_truth)?;
        let selected = known_condition_truth(eu, def_locations, &facts, condition)?;
        let selected_value = if selected { true_val } else { false_val };
        let def_block = def_blocks.get(&selected_value)?;
        if !cfg.dominators.dominates(*def_block, predecessor) {
            return None;
        }

        incoming.push(ControlledIncomingEdge {
            predecessor,
            select_true: selected,
            edge_truth,
        });
    }

    Some(ControlledMuxPlan {
        join,
        mux_idx,
        dst,
        true_val,
        false_val,
        incoming,
    })
}

fn plan_path_conditioned_join_mux(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &CfgAnalysis,
    def_blocks: &HashMap<RegisterId, BlockId>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    join: BlockId,
    mux_idx: usize,
    dst: RegisterId,
    true_val: RegisterId,
    false_val: RegisterId,
) -> Option<ControlledMuxPlan> {
    let block = eu.blocks.get(&join)?;
    if block.params.contains(&dst)
        || !matches!(
            block.instructions.get(mux_idx),
            Some(SIRInstruction::Mux(..))
        )
    {
        return None;
    }
    let incoming_edges = incoming_edges_to(eu, join);
    if incoming_edges.is_empty() {
        return None;
    }
    let Some(SIRInstruction::Mux(_, condition, ..)) = block.instructions.get(mux_idx) else {
        return None;
    };
    let (_, condition_inverted) = resolve_boolean_alias(eu, def_locations, *condition);
    let mut incoming = Vec::with_capacity(incoming_edges.len());
    let mut seen_predecessors = HashSet::default();
    for (predecessor, edge_truth) in incoming_edges {
        if !seen_predecessors.insert(predecessor) || predecessor == join {
            return None;
        }
        let facts =
            cfg.path_facts
                .facts_on_edge(eu, def_locations, predecessor, join, edge_truth)?;
        let condition_truth = known_condition_truth(eu, def_locations, &facts, *condition)?;
        let select_true = condition_truth ^ condition_inverted;
        let selected_value = if select_true { true_val } else { false_val };
        let def_block = def_blocks.get(&selected_value)?;
        if !cfg.dominators.dominates(*def_block, predecessor) {
            return None;
        }
        incoming.push(ControlledIncomingEdge {
            predecessor,
            select_true,
            edge_truth,
        });
    }
    Some(ControlledMuxPlan {
        join,
        mux_idx,
        dst,
        true_val,
        false_val,
        incoming,
    })
}

fn append_controlled_edge_argument(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    predecessor: BlockId,
    target: BlockId,
    edge_truth: Option<bool>,
    value: RegisterId,
) {
    let Some(block) = eu.blocks.get_mut(&predecessor) else {
        return;
    };
    match &mut block.terminator {
        SIRTerminator::Jump(destination, args) if *destination == target => args.push(value),
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => match edge_truth {
            Some(true) if true_block.0 == target => true_block.1.push(value),
            Some(false) if false_block.0 == target => false_block.1.push(value),
            _ => {}
        },
        _ => {}
    }
}

fn incoming_edges_to(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    target: BlockId,
) -> Vec<(BlockId, Option<bool>)> {
    let mut edges = Vec::new();
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable_by_key(|id| id.0);
    for block_id in block_ids {
        match &eu.blocks[&block_id].terminator {
            SIRTerminator::Jump(destination, _) if *destination == target => {
                edges.push((block_id, None));
            }
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => {
                if true_block.0 == target {
                    edges.push((block_id, Some(true)));
                }
                if false_block.0 == target {
                    edges.push((block_id, Some(false)));
                }
            }
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
            _ => {}
        }
    }
    edges
}

fn all_def_blocks(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> HashMap<RegisterId, BlockId> {
    let mut defs = HashMap::default();
    for block in eu.blocks.values() {
        for &param in &block.params {
            defs.insert(param, block.id);
        }
        for inst in &block.instructions {
            if let Some(def) = def_reg(inst) {
                defs.insert(def, block.id);
            }
        }
    }
    defs
}

fn instruction_def_locations(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, (BlockId, usize)> {
    let mut defs = HashMap::default();
    for block in eu.blocks.values() {
        for (idx, inst) in block.instructions.iter().enumerate() {
            if let Some(def) = def_reg(inst) {
                defs.insert(def, (block.id, idx));
            }
        }
    }
    defs
}

fn resolve_boolean_alias(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    mut register: RegisterId,
) -> (RegisterId, bool) {
    let mut inverted = false;
    let mut seen = HashSet::default();
    while seen.insert(register) {
        let Some(&(block_id, idx)) = locations.get(&register) else {
            break;
        };
        match &eu.blocks[&block_id].instructions[idx] {
            SIRInstruction::Unary(
                _,
                crate::ir::UnaryOp::Ident | crate::ir::UnaryOp::ToTwoState,
                source,
            ) => {
                register = *source;
            }
            SIRInstruction::Unary(_, crate::ir::UnaryOp::LogicNot, source) => {
                register = *source;
                inverted = !inverted;
            }
            _ => break,
        }
    }
    (register, inverted)
}

impl CfgAnalysis {
    fn compute(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> Self {
        let predecessors = super::pass_guarded_region_sinking::predecessor_map(eu);
        let dominators = super::pass_guarded_region_sinking::Dominators::compute(eu, &predecessors);
        let def_locations = instruction_def_locations(eu);
        let mut successors = BTreeMap::<BlockId, Vec<BlockId>>::new();
        for (&block_id, block) in &eu.blocks {
            let mut outgoing = match &block.terminator {
                SIRTerminator::Jump(target, _) => vec![*target],
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => vec![true_block.0, false_block.0],
                SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
            };
            outgoing.sort_unstable_by_key(|id| id.0);
            outgoing.dedup();
            successors.insert(block_id, outgoing);
        }

        let virtual_exit = BlockId(
            eu.blocks
                .keys()
                .map(|id| id.0)
                .max()
                .unwrap_or(0)
                .saturating_add(1),
        );
        let exits = successors
            .iter()
            .filter_map(|(&block, outgoing)| outgoing.is_empty().then_some(block))
            .collect::<Vec<_>>();
        let mut reverse_successors = BTreeMap::<BlockId, Vec<BlockId>>::new();
        reverse_successors.insert(virtual_exit, exits);
        for (&block, outgoing) in &successors {
            reverse_successors.entry(block).or_default();
            for &successor in outgoing {
                reverse_successors.entry(successor).or_default().push(block);
            }
        }
        let postdominators = PostDominatorTree::compute(virtual_exit, reverse_successors);
        let path_facts = PathFacts::compute(eu, &def_locations);

        Self {
            dominators,
            postdominators,
            path_facts,
        }
    }
}

impl PathFacts {
    fn compute(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    ) -> Self {
        let reachable = reachable_block_ids(eu);
        let mut entry_facts = HashMap::<BlockId, HashMap<PathFactKey, bool>>::default();
        for block_id in &reachable {
            entry_facts.insert(*block_id, HashMap::default());
        }

        let mut incoming = HashMap::<BlockId, Vec<(BlockId, Option<bool>)>>::default();
        let mut successors = HashMap::<BlockId, Vec<BlockId>>::default();
        for &block_id in &reachable {
            let edges = incoming_edges_to(eu, block_id)
                .into_iter()
                .filter(|(predecessor, _)| reachable.contains(predecessor))
                .collect::<Vec<_>>();
            incoming.insert(block_id, edges);
            successors.entry(block_id).or_default();
        }
        for (&target, edges) in &incoming {
            for &(predecessor, _) in edges {
                successors.entry(predecessor).or_default().push(target);
            }
        }

        let mut worklist = VecDeque::from_iter(reachable.iter().copied());
        while let Some(predecessor) = worklist.pop_front() {
            let targets = successors.get(&predecessor).cloned().unwrap_or_default();
            for target in targets {
                if target == eu.entry_block_id {
                    continue;
                }
                let mut intersection: Option<HashMap<PathFactKey, bool>> = None;
                for &(edge_predecessor, edge_truth) in &incoming[&target] {
                    let facts = &entry_facts[&edge_predecessor];
                    let Some(edge_facts) = facts_on_edge(
                        eu,
                        def_locations,
                        edge_predecessor,
                        target,
                        edge_truth,
                        facts,
                    ) else {
                        continue;
                    };
                    if let Some(current) = intersection.as_mut() {
                        current.retain(|register, value| edge_facts.get(register) == Some(value));
                    } else {
                        intersection = Some(edge_facts);
                    }
                }
                let Some(next) = intersection else {
                    continue;
                };
                if entry_facts[&target] != next {
                    entry_facts.insert(target, next);
                    worklist.push_back(target);
                }
            }
        }

        Self { entry_facts }
    }

    fn facts_on_edge(
        &self,
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        def_locations: &HashMap<RegisterId, (BlockId, usize)>,
        predecessor: BlockId,
        target: BlockId,
        edge_truth: Option<bool>,
    ) -> Option<HashMap<PathFactKey, bool>> {
        let facts = self.entry_facts.get(&predecessor)?;
        facts_on_edge(eu, def_locations, predecessor, target, edge_truth, facts)
    }
}

fn facts_on_edge(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    predecessor: BlockId,
    target: BlockId,
    edge_truth: Option<bool>,
    facts: &HashMap<PathFactKey, bool>,
) -> Option<HashMap<PathFactKey, bool>> {
    let mut result = facts.clone();
    let block = eu.blocks.get(&predecessor)?;
    match (&block.terminator, edge_truth) {
        (SIRTerminator::Jump(destination, _), None) if *destination == target => {}
        (
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            },
            Some(truth),
        ) if (truth && true_block.0 == target) || (!truth && false_block.0 == target) => {
            let (root, inverted) = resolve_boolean_alias(eu, def_locations, *cond);
            let root_truth = truth ^ inverted;
            let register_key = PathFactKey::Register(root);
            if result
                .get(&register_key)
                .is_some_and(|known| *known != root_truth)
            {
                return None;
            }
            result.insert(register_key, root_truth);
            if let Some((predicate, predicate_inverted)) = predicate_key(eu, def_locations, *cond) {
                let predicate_truth = truth ^ predicate_inverted;
                let key = PathFactKey::Predicate(predicate);
                if result
                    .get(&key)
                    .is_some_and(|known| *known != predicate_truth)
                {
                    return None;
                }
                result.insert(key, predicate_truth);
            }
        }
        _ => return None,
    }
    Some(result)
}

fn known_condition_truth(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    facts: &HashMap<PathFactKey, bool>,
    condition: RegisterId,
) -> Option<bool> {
    let (root, inverted) = resolve_boolean_alias(eu, def_locations, condition);
    if let Some(value) = facts.get(&PathFactKey::Register(root)) {
        return Some(*value ^ inverted);
    }
    let (predicate, predicate_inverted) = predicate_key(eu, def_locations, condition)?;
    let value = known_predicate_truth(facts, &predicate)?;
    Some(value ^ predicate_inverted)
}

fn known_predicate_truth(facts: &HashMap<PathFactKey, bool>, query: &PredicateKey) -> Option<bool> {
    if let Some(value) = facts.get(&PathFactKey::Predicate(query.clone())) {
        return Some(*value);
    }
    let same_lhs = |key: &PredicateKey| key.lhs == query.lhs;
    for (fact, &value) in facts {
        let PathFactKey::Predicate(fact) = fact else {
            continue;
        };
        if !same_lhs(fact) {
            continue;
        }
        if fact.kind == query.kind
            && fact.kind == PredicateKind::Equal
            && different_constants(&fact.rhs, &query.rhs)
            && value
        {
            return Some(false);
        }
        if fact.rhs == query.rhs && fact.kind != query.kind && value {
            return Some(false);
        }
        if fact.rhs == query.rhs && fact.kind != query.kind && !value {
            return Some(true);
        }
    }
    None
}

fn different_constants(left: &PredicateRhs, right: &PredicateRhs) -> bool {
    match (left, right) {
        (
            PredicateRhs::Constant(left_payload, left_mask),
            PredicateRhs::Constant(right_payload, right_mask),
        ) => {
            left_mask.iter().all(|word| *word == 0)
                && right_mask.iter().all(|word| *word == 0)
                && left_payload != right_payload
        }
        _ => false,
    }
}

fn predicate_key(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    mut register: RegisterId,
) -> Option<(PredicateKey, bool)> {
    let mut inverted = false;
    let mut seen = HashSet::default();
    while seen.insert(register) {
        let &(block, index) = locations.get(&register)?;
        match &eu.blocks[&block].instructions[index] {
            SIRInstruction::Unary(_, crate::ir::UnaryOp::LogicNot, source) => {
                register = *source;
                inverted = !inverted;
            }
            SIRInstruction::Unary(
                _,
                crate::ir::UnaryOp::Ident | crate::ir::UnaryOp::ToTwoState,
                source,
            ) => register = *source,
            SIRInstruction::Binary(_, lhs, op, rhs)
                if matches!(
                    op,
                    crate::ir::BinaryOp::Eq
                        | crate::ir::BinaryOp::EqWildcard
                        | crate::ir::BinaryOp::Ne
                        | crate::ir::BinaryOp::NeWildcard
                ) =>
            {
                let kind = match op {
                    crate::ir::BinaryOp::Eq | crate::ir::BinaryOp::EqWildcard => {
                        PredicateKind::Equal
                    }
                    crate::ir::BinaryOp::Ne | crate::ir::BinaryOp::NeWildcard => {
                        PredicateKind::NotEqual
                    }
                    _ => unreachable!(),
                };
                let lhs = canonical_identity_register(eu, locations, *lhs);
                let rhs = if let Some(value) = immediate_value(eu, locations, *rhs) {
                    PredicateRhs::Constant(value.0, value.1)
                } else {
                    PredicateRhs::Register(canonical_identity_register(eu, locations, *rhs))
                };
                return Some((PredicateKey { lhs, kind, rhs }, inverted));
            }
            _ => return None,
        }
    }
    None
}

fn canonical_identity_register(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    mut register: RegisterId,
) -> RegisterId {
    let mut seen = HashSet::default();
    while seen.insert(register) {
        let Some(&(block, index)) = locations.get(&register) else {
            break;
        };
        match &eu.blocks[&block].instructions[index] {
            SIRInstruction::Unary(
                _,
                crate::ir::UnaryOp::Ident | crate::ir::UnaryOp::ToTwoState,
                source,
            ) => register = *source,
            _ => break,
        }
    }
    register
}

fn immediate_value(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    mut register: RegisterId,
) -> Option<(Vec<u64>, Vec<u64>)> {
    let mut seen = HashSet::default();
    while seen.insert(register) {
        let &(block, index) = locations.get(&register)?;
        match &eu.blocks[&block].instructions[index] {
            SIRInstruction::Imm(_, value) => {
                return Some((value.payload.to_u64_digits(), value.mask.to_u64_digits()));
            }
            SIRInstruction::Unary(
                _,
                crate::ir::UnaryOp::Ident | crate::ir::UnaryOp::ToTwoState,
                source,
            ) => register = *source,
            _ => return None,
        }
    }
    None
}

fn reachable_block_ids(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> HashSet<BlockId> {
    let mut reachable = HashSet::default();
    let mut worklist = vec![eu.entry_block_id];
    while let Some(block_id) = worklist.pop() {
        if !reachable.insert(block_id) {
            continue;
        }
        let Some(block) = eu.blocks.get(&block_id) else {
            continue;
        };
        match &block.terminator {
            SIRTerminator::Jump(target, _) => worklist.push(*target),
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => {
                worklist.push(true_block.0);
                worklist.push(false_block.0);
            }
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    reachable
}

struct PostDominatorTree {
    virtual_exit: BlockId,
    tree: SimpleDominatorTree,
}

impl PostDominatorTree {
    fn compute(entry: BlockId, successors: BTreeMap<BlockId, Vec<BlockId>>) -> Self {
        let mut predecessors = BTreeMap::<BlockId, Vec<BlockId>>::new();
        for (&block, outgoing) in &successors {
            predecessors.entry(block).or_default();
            for &successor in outgoing {
                predecessors.entry(successor).or_default().push(block);
            }
        }
        let tree = SimpleDominatorTree::compute(entry, successors, predecessors);
        Self {
            virtual_exit: entry,
            tree,
        }
    }

    fn postdominates(&self, postdominator: BlockId, block: BlockId) -> bool {
        self.tree.dominates(postdominator, block)
    }

    #[allow(dead_code)]
    fn common_postdominator(&self, left: BlockId, right: BlockId) -> Option<BlockId> {
        let candidate = self.tree.lca(left, right)?;
        (candidate != self.virtual_exit).then_some(candidate)
    }
}

struct SimpleDominatorTree {
    idom: HashMap<BlockId, BlockId>,
    rpo_index: HashMap<BlockId, usize>,
    depth: HashMap<BlockId, usize>,
}

impl SimpleDominatorTree {
    fn compute(
        entry: BlockId,
        successors: BTreeMap<BlockId, Vec<BlockId>>,
        predecessors: BTreeMap<BlockId, Vec<BlockId>>,
    ) -> Self {
        let mut postorder = Vec::new();
        let mut visited = HashSet::default();
        let mut stack = vec![(entry, 0usize)];
        visited.insert(entry);
        while let Some((block, next_successor)) = stack.last_mut() {
            let outgoing = successors.get(block).map(Vec::as_slice).unwrap_or(&[]);
            if *next_successor == outgoing.len() {
                postorder.push(*block);
                stack.pop();
                continue;
            }
            let successor = outgoing[*next_successor];
            *next_successor += 1;
            if visited.insert(successor) {
                stack.push((successor, 0));
            }
        }
        postorder.reverse();
        let rpo_index = postorder
            .iter()
            .enumerate()
            .map(|(index, &block)| (block, index))
            .collect::<HashMap<_, _>>();
        let mut idom = HashMap::default();
        idom.insert(entry, entry);

        loop {
            let mut changed = false;
            for &block in postorder.iter().skip(1) {
                let mut processed = predecessors
                    .get(&block)
                    .into_iter()
                    .flatten()
                    .filter(|predecessor| idom.contains_key(predecessor));
                let Some(first) = processed.next().copied() else {
                    continue;
                };
                let next = processed.fold(first, |current, predecessor| {
                    intersect_idoms(current, *predecessor, &idom, &rpo_index)
                });
                if idom.insert(block, next) != Some(next) {
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        let mut depth = HashMap::default();
        depth.insert(entry, 0);
        for &block in postorder.iter().skip(1) {
            let mut current = block;
            let mut distance = 0usize;
            while current != entry {
                let Some(&parent) = idom.get(&current) else {
                    break;
                };
                current = parent;
                distance += 1;
            }
            depth.insert(block, distance);
        }

        Self {
            idom,
            rpo_index,
            depth,
        }
    }

    fn dominates(&self, dominator: BlockId, block: BlockId) -> bool {
        if !self.rpo_index.contains_key(&dominator) || !self.idom.contains_key(&block) {
            return false;
        }
        let mut candidate = block;
        loop {
            if candidate == dominator {
                return true;
            }
            let Some(&parent) = self.idom.get(&candidate) else {
                return false;
            };
            if parent == candidate {
                return false;
            }
            candidate = parent;
        }
    }

    fn lca(&self, left: BlockId, right: BlockId) -> Option<BlockId> {
        if !self.depth.contains_key(&left) || !self.depth.contains_key(&right) {
            return None;
        }
        let mut left = left;
        let mut right = right;
        while self.depth[&left] > self.depth[&right] {
            left = *self.idom.get(&left)?;
        }
        while self.depth[&right] > self.depth[&left] {
            right = *self.idom.get(&right)?;
        }
        while left != right {
            left = *self.idom.get(&left)?;
            right = *self.idom.get(&right)?;
        }
        Some(left)
    }
}

fn intersect_idoms(
    mut left: BlockId,
    mut right: BlockId,
    idom: &HashMap<BlockId, BlockId>,
    rpo_index: &HashMap<BlockId, usize>,
) -> BlockId {
    while left != right {
        while rpo_index[&left] > rpo_index[&right] {
            left = idom[&left];
        }
        while rpo_index[&right] > rpo_index[&left] {
            right = idom[&right];
        }
    }
    left
}

fn find_branchify_mux_in_block(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    use_counts: &HashMap<RegisterId, usize>,
    def_blocks: &HashMap<RegisterId, BlockId>,
) -> Option<BranchifyPlan> {
    let block = eu.blocks.get(&block_id)?;
    let mut def_pos = HashMap::default();
    for (idx, inst) in block.instructions.iter().enumerate() {
        if let Some(def) = def_reg(inst) {
            def_pos.insert(def, idx);
        }
    }

    for (mux_idx, inst) in block.instructions.iter().enumerate() {
        let SIRInstruction::Mux(dst, cond, true_val, false_val) = inst else {
            continue;
        };

        if use_counts.get(dst).copied().unwrap_or(0) > block_use_count(block, *dst) {
            continue;
        }

        let immediate_store = find_distributed_store(block, mux_idx, *dst, *true_val, *false_val);
        let preserve_result =
            immediate_store.is_none() || use_counts.get(dst).copied().unwrap_or(0) > 1;
        let memory_barrier_idx = if preserve_result {
            mux_idx
        } else {
            immediate_store
                .as_ref()
                .expect("single-use store mux should have a store")
                .idx
                + 1
        };

        let mut true_defs = HashSet::default();
        let mut false_defs = HashSet::default();
        collect_sinkable_defs(
            block,
            &def_pos,
            use_counts,
            mux_idx,
            memory_barrier_idx,
            *true_val,
            &mut true_defs,
        );
        collect_sinkable_defs(
            block,
            &def_pos,
            use_counts,
            mux_idx,
            memory_barrier_idx,
            *false_val,
            &mut false_defs,
        );
        if !true_defs.is_disjoint(&false_defs) {
            continue;
        }
        if !terminator_uses(&block.terminator).contains(dst)
            && true_defs
                .iter()
                .chain(false_defs.iter())
                .all(|idx| is_trivial_select_input(&block.instructions[*idx]))
        {
            continue;
        }

        let mut true_defs = true_defs.into_iter().collect::<Vec<_>>();
        let mut false_defs = false_defs.into_iter().collect::<Vec<_>>();
        true_defs.sort_unstable();
        false_defs.sort_unstable();
        let plan = BranchifyPlan {
            block_id,
            mux_idx,
            dst: *dst,
            cond: *cond,
            true_val: *true_val,
            false_val: *false_val,
            true_defs,
            false_defs,
            distributed_store: if preserve_result {
                None
            } else {
                immediate_store
            },
            preserve_result,
        };
        if !branch_is_profitable(eu, block, &plan, def_blocks, &def_pos) {
            continue;
        }
        return Some(plan);
    }

    None
}

// Native and Cranelift both eventually turn a SIR branch into a conditional
// transfer, an executed arm-to-merge transfer, and (when the mux result is
// preserved) phi copies. With no profile, equality-to-constant decoder tests
// use the same 20/80 prior as cost-directed SLT lowering and other conditions
// use 50/50. A modern x86 misprediction is roughly 16 cycles.
//
// This is a local proof of expected benefit, not an iteration or function-size
// budget: the work expected to be skipped must strictly exceed every modeled
// downstream cost introduced by this particular transformation.
const BRANCH_CONTROL_COST: u128 = 3;
const MISPREDICT_COST: u128 = 16;
const PHI_COPY_COST_PER_CHUNK: u128 = 2;
const LIVE_THROUGH_COST_PER_CHUNK: u128 = 1;
// Cross-block motion adds three CFG blocks and extends the live range to a
// join.  The profitability proof below accounts for that cost directly; do
// not impose a separate compile-time or arbitrary work threshold here.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StaticBranchProbability {
    true_weight: u128,
    total_weight: u128,
}

impl StaticBranchProbability {
    const EVEN: Self = Self {
        true_weight: 1,
        total_weight: 2,
    };

    const EQUALITY_TO_CONSTANT: Self = Self {
        true_weight: 1,
        total_weight: 5,
    };

    fn inverted(self) -> Self {
        Self {
            true_weight: self.total_weight - self.true_weight,
            total_weight: self.total_weight,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BranchProfitability {
    true_arm_cost: u128,
    false_arm_cost: u128,
    removed_mux_cost: u128,
    probability: StaticBranchProbability,
    control_cost: u128,
    phi_copy_cost: u128,
    live_through_cost: u128,
}

impl BranchProfitability {
    fn expected_saved_scaled(self) -> u128 {
        let false_weight = self.probability.total_weight - self.probability.true_weight;
        false_weight
            .saturating_mul(self.true_arm_cost)
            .saturating_add(
                self.probability
                    .true_weight
                    .saturating_mul(self.false_arm_cost),
            )
            .saturating_add(
                self.probability
                    .total_weight
                    .saturating_mul(self.removed_mux_cost),
            )
    }

    fn introduced_cost_scaled(self) -> u128 {
        let false_weight = self.probability.total_weight - self.probability.true_weight;
        self.probability
            .total_weight
            .saturating_mul(
                self.control_cost
                    .saturating_add(self.phi_copy_cost)
                    .saturating_add(self.live_through_cost),
            )
            .saturating_add(
                self.probability
                    .true_weight
                    .min(false_weight)
                    .saturating_mul(MISPREDICT_COST),
            )
    }

    fn proves_expected_benefit(self) -> bool {
        self.expected_saved_scaled() > self.introduced_cost_scaled()
    }
}

fn branch_is_profitable(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &BranchifyPlan,
    def_blocks: &HashMap<RegisterId, BlockId>,
    def_pos: &HashMap<RegisterId, usize>,
) -> bool {
    branch_profitability(eu, block, plan, def_blocks, def_pos).proves_expected_benefit()
}

fn branch_profitability(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &BranchifyPlan,
    def_blocks: &HashMap<RegisterId, BlockId>,
    def_pos: &HashMap<RegisterId, usize>,
) -> BranchProfitability {
    let remove_defs = removable_defs_after_head_restore(block, plan, def_blocks);
    let arm_cost = |defs: &[usize]| {
        defs.iter()
            .filter(|idx| remove_defs.contains(idx))
            .map(|&idx| branchified_instruction_cost(&block.instructions[idx], &eu.register_map))
            .sum::<u128>()
    };
    let suffix = block
        .instructions
        .iter()
        .enumerate()
        .skip(plan.mux_idx + 1)
        .filter(|(idx, _)| !remove_defs.contains(idx))
        .map(|(_, inst)| inst.clone())
        .collect::<Vec<_>>();
    let mut live_through = block_live_ins(&suffix, &terminator_uses(&block.terminator));
    live_through.retain(|value| *value != plan.dst);
    live_through.sort_unstable();
    live_through.dedup();

    let chunks_for = |value: RegisterId| {
        eu.register_map
            .get(&value)
            .map(|register| register.width().div_ceil(64).max(1))
            .unwrap_or(1) as u128
    };
    let result_chunks = if plan.preserve_result {
        chunks_for(plan.dst)
    } else {
        0
    };
    let live_through_chunks = live_through.into_iter().map(chunks_for).sum::<u128>();

    BranchProfitability {
        true_arm_cost: arm_cost(&plan.true_defs),
        false_arm_cost: arm_cost(&plan.false_defs),
        removed_mux_cost: branchified_instruction_cost(
            &block.instructions[plan.mux_idx],
            &eu.register_map,
        ),
        probability: static_true_probability(block, def_pos, plan.cond),
        control_cost: BRANCH_CONTROL_COST,
        phi_copy_cost: result_chunks.saturating_mul(PHI_COPY_COST_PER_CHUNK),
        live_through_cost: live_through_chunks.saturating_mul(LIVE_THROUGH_COST_PER_CHUNK),
    }
}

fn static_true_probability(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    def_pos: &HashMap<RegisterId, usize>,
    cond: RegisterId,
) -> StaticBranchProbability {
    let mut current = cond;
    let mut inverted = false;
    let mut seen = HashSet::default();

    while seen.insert(current) {
        let Some(&idx) = def_pos.get(&current) else {
            break;
        };
        match &block.instructions[idx] {
            SIRInstruction::Unary(_, crate::ir::UnaryOp::LogicNot, inner) => {
                inverted = !inverted;
                current = *inner;
            }
            SIRInstruction::Unary(_, crate::ir::UnaryOp::Ident, inner) => {
                current = *inner;
            }
            SIRInstruction::Binary(
                _,
                lhs,
                op @ (crate::ir::BinaryOp::Eq
                | crate::ir::BinaryOp::Ne
                | crate::ir::BinaryOp::EqWildcard
                | crate::ir::BinaryOp::NeWildcard),
                rhs,
            ) if register_is_immediate(block, def_pos, *lhs)
                || register_is_immediate(block, def_pos, *rhs) =>
            {
                let equality = matches!(
                    op,
                    crate::ir::BinaryOp::Eq | crate::ir::BinaryOp::EqWildcard
                );
                let probability = if equality != inverted {
                    StaticBranchProbability::EQUALITY_TO_CONSTANT
                } else {
                    StaticBranchProbability::EQUALITY_TO_CONSTANT.inverted()
                };
                return probability;
            }
            _ => break,
        }
    }

    if inverted {
        StaticBranchProbability::EVEN.inverted()
    } else {
        StaticBranchProbability::EVEN
    }
}

fn register_is_immediate(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    def_pos: &HashMap<RegisterId, usize>,
    register: RegisterId,
) -> bool {
    let mut current = register;
    let mut seen = HashSet::default();
    while seen.insert(current) {
        let Some(&idx) = def_pos.get(&current) else {
            return false;
        };
        match &block.instructions[idx] {
            SIRInstruction::Imm(..) => return true,
            SIRInstruction::Unary(_, crate::ir::UnaryOp::Ident, inner) => current = *inner,
            _ => return false,
        }
    }
    false
}

/// Estimated dynamic target work for an instruction that can be moved into a
/// branch arm.  This deliberately follows the same width/chunk model as
/// cost-directed SLT mux lowering instead of the CLIF-size estimator: the
/// decision is about runtime work skipped, not compiler IR expansion.
fn branchified_instruction_cost(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    register_map: &HashMap<RegisterId, crate::ir::RegisterType>,
) -> u128 {
    let register_width = |register: RegisterId| {
        register_map
            .get(&register)
            .map(crate::ir::RegisterType::width)
            .unwrap_or(64)
    };
    let chunks = |width: usize| width.div_ceil(64).max(1) as u128;

    match inst {
        SIRInstruction::Imm(dst, _) => chunks(register_width(*dst)),
        SIRInstruction::Binary(dst, lhs, op, rhs) => {
            let operand_chunks = chunks(
                register_width(*dst)
                    .max(register_width(*lhs))
                    .max(register_width(*rhs)),
            );
            match op {
                crate::ir::BinaryOp::And
                | crate::ir::BinaryOp::Or
                | crate::ir::BinaryOp::Xor
                | crate::ir::BinaryOp::LogicAnd
                | crate::ir::BinaryOp::LogicOr => operand_chunks,
                crate::ir::BinaryOp::Add | crate::ir::BinaryOp::Sub => 3 * operand_chunks,
                crate::ir::BinaryOp::Mul => 5 * operand_chunks.saturating_mul(operand_chunks),
                crate::ir::BinaryOp::DivU
                | crate::ir::BinaryOp::DivS
                | crate::ir::BinaryOp::RemU
                | crate::ir::BinaryOp::RemS => 12 * operand_chunks.saturating_mul(operand_chunks),
                crate::ir::BinaryOp::Shl | crate::ir::BinaryOp::Shr | crate::ir::BinaryOp::Sar => {
                    4 * operand_chunks
                }
                crate::ir::BinaryOp::Eq
                | crate::ir::BinaryOp::Ne
                | crate::ir::BinaryOp::EqWildcard
                | crate::ir::BinaryOp::NeWildcard
                | crate::ir::BinaryOp::LtU
                | crate::ir::BinaryOp::LtS
                | crate::ir::BinaryOp::LeU
                | crate::ir::BinaryOp::LeS
                | crate::ir::BinaryOp::GtU
                | crate::ir::BinaryOp::GtS
                | crate::ir::BinaryOp::GeU
                | crate::ir::BinaryOp::GeS => 3 * operand_chunks,
            }
        }
        SIRInstruction::Unary(dst, op, src) => {
            let operand_chunks = chunks(register_width(*dst).max(register_width(*src)));
            match op {
                crate::ir::UnaryOp::PopCount => 2 * operand_chunks + 1,
                crate::ir::UnaryOp::CountLeadingZeros | crate::ir::UnaryOp::CountTrailingZeros => {
                    3 * operand_chunks + 1
                }
                _ => 2 * operand_chunks,
            }
        }
        SIRInstruction::Load(_, _, offset, width) => {
            3 * chunks(*width) + 3 * u128::from(offset.is_dynamic())
        }
        SIRInstruction::Concat(dst, args) => chunks(register_width(*dst)) + args.len() as u128,
        SIRInstruction::Slice(dst, _, _, _) => 2 * chunks(register_width(*dst)),
        SIRInstruction::Mux(dst, _, true_value, false_value) => chunks(
            register_width(*dst)
                .max(register_width(*true_value))
                .max(register_width(*false_value)),
        ),
        SIRInstruction::Store(..)
        | SIRInstruction::Commit(..)
        | SIRInstruction::RuntimeEvent { .. }
        | SIRInstruction::CombCaptureEvent { .. }
        | SIRInstruction::CombCaptureEnableIfChanged { .. } => 0,
    }
}

fn find_distributed_store(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    mux_idx: usize,
    dst: RegisterId,
    true_val: RegisterId,
    false_val: RegisterId,
) -> Option<DistributedStore> {
    let store_idx = mux_idx + 1;
    let store = block.instructions.get(store_idx)?;
    match store {
        SIRInstruction::Store(addr, offset, width, src, triggers, sites) if *src == dst => {
            Some(DistributedStore {
                idx: store_idx,
                true_inst: SIRInstruction::Store(
                    *addr,
                    offset.clone(),
                    *width,
                    true_val,
                    triggers.clone(),
                    sites.clone(),
                ),
                false_inst: SIRInstruction::Store(
                    *addr,
                    offset.clone(),
                    *width,
                    false_val,
                    triggers.clone(),
                    sites.clone(),
                ),
            })
        }
        _ => None,
    }
}

fn collect_sinkable_defs(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    def_pos: &HashMap<RegisterId, usize>,
    use_counts: &HashMap<RegisterId, usize>,
    user_idx: usize,
    memory_barrier_idx: usize,
    root: RegisterId,
    defs: &mut HashSet<usize>,
) {
    if use_counts.get(&root).copied().unwrap_or(0) != 1 {
        return;
    }
    let Some(&idx) = def_pos.get(&root) else {
        return;
    };
    if idx >= user_idx || defs.contains(&idx) {
        return;
    }
    let inst = &block.instructions[idx];
    if !is_sinkable_input(inst) {
        return;
    }
    if let Some(load) = memory_read(inst)
        && has_intervening_memory_conflict(block, idx + 1, memory_barrier_idx, load)
    {
        return;
    }

    defs.insert(idx);
    for use_reg in inst_uses(inst) {
        collect_sinkable_defs(
            block,
            def_pos,
            use_counts,
            idx,
            memory_barrier_idx,
            use_reg,
            defs,
        );
    }
}

fn is_sinkable_input(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    matches!(
        inst,
        SIRInstruction::Imm(..)
            | SIRInstruction::Binary(..)
            | SIRInstruction::Unary(..)
            | SIRInstruction::Load(..)
            | SIRInstruction::Concat(..)
            | SIRInstruction::Slice(..)
            | SIRInstruction::Mux(..)
    )
}

fn is_trivial_select_input(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    matches!(inst, SIRInstruction::Imm(..))
}

#[derive(Clone, Copy)]
struct MemAccess<'a> {
    addr: &'a RegionedAbsoluteAddr,
    offset: Option<usize>,
    width: usize,
}

fn memory_read(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Option<MemAccess<'_>> {
    match inst {
        SIRInstruction::Load(_, addr, offset, width) => Some(MemAccess {
            addr,
            offset: offset_static(offset),
            width: *width,
        }),
        _ => None,
    }
}

fn memory_write(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Option<MemAccess<'_>> {
    match inst {
        SIRInstruction::Store(addr, offset, width, _, _, _) => Some(MemAccess {
            addr,
            offset: offset_static(offset),
            width: *width,
        }),
        SIRInstruction::Commit(_, dst, offset, width, _) => Some(MemAccess {
            addr: dst,
            offset: offset_static(offset),
            width: *width,
        }),
        _ => None,
    }
}

fn offset_static(offset: &SIROffset) -> Option<usize> {
    match offset {
        SIROffset::Static(offset) => Some(*offset),
        SIROffset::Dynamic(_) | SIROffset::Element { .. } => None,
    }
}

fn has_intervening_memory_conflict(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    start: usize,
    end: usize,
    read: MemAccess<'_>,
) -> bool {
    block.instructions[start..end].iter().any(|inst| {
        is_memory_barrier(inst)
            || memory_write(inst).is_some_and(|write| mem_may_alias(read, write))
    })
}

fn is_memory_barrier(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    matches!(
        inst,
        SIRInstruction::RuntimeEvent { .. }
            | SIRInstruction::CombCaptureEvent { .. }
            | SIRInstruction::CombCaptureEnableIfChanged { .. }
    )
}

fn mem_may_alias(a: MemAccess<'_>, b: MemAccess<'_>) -> bool {
    if a.addr != b.addr {
        return false;
    }
    match (a.offset, b.offset) {
        (Some(a_off), Some(b_off)) => a_off < b_off + b.width && b_off < a_off + a.width,
        _ => true,
    }
}

fn apply_branchify_mux(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: BranchifyPlan,
    use_counts: &mut HashMap<RegisterId, usize>,
    def_blocks: &mut HashMap<RegisterId, BlockId>,
    next_block_id: &mut usize,
    reg_counter: &mut usize,
    trace_reg: Option<RegisterId>,
) -> [BlockId; 3] {
    let true_id = BlockId(*next_block_id);
    let false_id = BlockId(*next_block_id + 1);
    let merge_id = BlockId(*next_block_id + 2);
    *next_block_id += 3;

    let original = eu
        .blocks
        .remove(&plan.block_id)
        .expect("branchify target block must exist");
    if let Some(reg) = trace_reg {
        trace_reg_in_original(&original, &plan, reg);
    }
    remove_block_uses(use_counts, &original);
    let remove_defs = removable_defs_after_head_restore(&original, &plan, def_blocks);
    if let Some(reg) = trace_reg {
        trace_reg_branchify_plan(&original, &plan, &remove_defs, reg);
    }

    let mut head_insts = Vec::new();
    for (idx, inst) in original.instructions.iter().enumerate().take(plan.mux_idx) {
        if !remove_defs.contains(&idx) {
            head_insts.push(inst.clone());
        }
    }
    let branch_cond = normalize_branch_condition(
        &mut eu.register_map,
        &mut head_insts,
        plan.cond,
        reg_counter,
    );
    let mut suffix = Vec::new();
    for (idx, inst) in original
        .instructions
        .iter()
        .enumerate()
        .skip(plan.mux_idx + 1)
    {
        if !remove_defs.contains(&idx) {
            suffix.push(inst.clone());
        }
    }

    let mut true_insts = plan
        .true_defs
        .iter()
        .filter(|idx| remove_defs.contains(idx))
        .map(|&idx| original.instructions[idx].clone())
        .collect::<Vec<_>>();
    let mut false_insts = plan
        .false_defs
        .iter()
        .filter(|idx| remove_defs.contains(idx))
        .map(|&idx| original.instructions[idx].clone())
        .collect::<Vec<_>>();
    if let Some(store) = &plan.distributed_store {
        true_insts.push(store.true_inst.clone());
        false_insts.push(store.false_inst.clone());
    }
    let true_args = if plan.preserve_result {
        vec![plan.true_val]
    } else {
        Vec::new()
    };
    let false_args = if plan.preserve_result {
        vec![plan.false_val]
    } else {
        Vec::new()
    };
    let merge_params = if plan.preserve_result {
        vec![plan.dst]
    } else {
        Vec::new()
    };

    let merge_terminator = original.terminator;

    let head = BasicBlock {
        id: plan.block_id,
        params: original.params,
        instructions: head_insts,
        terminator: SIRTerminator::Branch {
            cond: branch_cond,
            true_block: (true_id, Vec::new()),
            false_block: (false_id, Vec::new()),
        },
    };
    let true_block = BasicBlock {
        id: true_id,
        params: Vec::new(),
        instructions: true_insts,
        terminator: SIRTerminator::Jump(merge_id, true_args),
    };
    let false_block = BasicBlock {
        id: false_id,
        params: Vec::new(),
        instructions: false_insts,
        terminator: SIRTerminator::Jump(merge_id, false_args),
    };
    let merge_block = BasicBlock {
        id: merge_id,
        params: merge_params,
        instructions: suffix,
        terminator: merge_terminator,
    };

    add_block_uses(use_counts, &head);
    add_block_uses(use_counts, &true_block);
    add_block_uses(use_counts, &false_block);
    add_block_uses(use_counts, &merge_block);

    eu.blocks.insert(plan.block_id, head);
    eu.blocks.insert(true_id, true_block);
    eu.blocks.insert(false_id, false_block);
    eu.blocks.insert(merge_id, merge_block);

    for block_id in [plan.block_id, true_id, false_id, merge_id] {
        for inst in &eu.blocks[&block_id].instructions {
            if let Some(def) = def_reg(inst) {
                def_blocks.insert(def, block_id);
            }
        }
    }

    if let Some(reg) = trace_reg {
        for block_id in [plan.block_id, true_id, false_id, merge_id] {
            if let Some(block) = eu.blocks.get(&block_id) {
                trace_reg_in_new_block(block, reg);
            }
        }
    }

    [true_id, false_id, merge_id]
}

fn removable_defs_after_head_restore(
    original: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &BranchifyPlan,
    def_blocks: &HashMap<RegisterId, BlockId>,
) -> HashSet<usize> {
    let mut remove_defs = plan
        .true_defs
        .iter()
        .chain(plan.false_defs.iter())
        .copied()
        .collect::<HashSet<_>>();
    remove_defs.insert(plan.mux_idx);
    if let Some(store) = &plan.distributed_store {
        remove_defs.insert(store.idx);
    }
    let restore_defs = head_restore_defs(original, plan, &remove_defs, def_blocks);
    for idx in restore_defs {
        remove_defs.remove(&idx);
    }
    remove_defs
}

fn trace_reg_in_original(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &BranchifyPlan,
    reg: RegisterId,
) {
    let defines = block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(idx, inst)| (def_reg(inst) == Some(reg)).then_some((idx, inst)))
        .collect::<Vec<_>>();
    let inst_uses = block
        .instructions
        .iter()
        .enumerate()
        .filter(|(_, inst)| inst_uses(inst).contains(&reg))
        .collect::<Vec<_>>();
    let term_uses = terminator_uses(&block.terminator).contains(&reg);
    if defines.is_empty() && inst_uses.is_empty() && !term_uses && !block.params.contains(&reg) {
        return;
    }
    eprintln!(
        "[branchify-trace] original block=b{} mux_idx={} dst=r{} cond=r{} true=r{} false=r{} params={} term_uses={} true_defs={:?} false_defs={:?}",
        block.id.0,
        plan.mux_idx,
        plan.dst.0,
        plan.cond.0,
        plan.true_val.0,
        plan.false_val.0,
        block.params.contains(&reg),
        term_uses,
        plan.true_defs,
        plan.false_defs
    );
    for (idx, inst) in defines {
        eprintln!(
            "[branchify-trace] original defines r{} at inst {idx}: {inst}",
            reg.0
        );
    }
    for (idx, inst) in inst_uses {
        eprintln!(
            "[branchify-trace] original uses r{} at inst {idx}: {inst}",
            reg.0
        );
    }
    if term_uses {
        eprintln!(
            "[branchify-trace] original terminator uses r{}: {}",
            reg.0, block.terminator
        );
    }
}

fn trace_reg_branchify_plan(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &BranchifyPlan,
    remove_defs: &HashSet<usize>,
    reg: RegisterId,
) {
    for (idx, inst) in block.instructions.iter().enumerate() {
        if def_reg(inst) == Some(reg) {
            eprintln!(
                "[branchify-trace] after restore decision block=b{} r{} def_idx={idx} removed={} inst={inst}",
                block.id.0,
                reg.0,
                remove_defs.contains(&idx)
            );
        }
    }
    if plan.cond == reg || plan.true_val == reg || plan.false_val == reg || plan.dst == reg {
        eprintln!(
            "[branchify-trace] plan directly references r{} block=b{} mux_idx={} dst=r{} cond=r{} true=r{} false=r{}",
            reg.0,
            block.id.0,
            plan.mux_idx,
            plan.dst.0,
            plan.cond.0,
            plan.true_val.0,
            plan.false_val.0
        );
    }
}

fn trace_reg_in_new_block(block: &BasicBlock<RegionedAbsoluteAddr>, reg: RegisterId) {
    let term_uses = terminator_uses(&block.terminator).contains(&reg);
    let inst_uses = block
        .instructions
        .iter()
        .enumerate()
        .filter(|(_, inst)| inst_uses(inst).contains(&reg))
        .collect::<Vec<_>>();
    let defines = block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(idx, inst)| (def_reg(inst) == Some(reg)).then_some((idx, inst)))
        .collect::<Vec<_>>();
    if !block.params.contains(&reg) && !term_uses && inst_uses.is_empty() && defines.is_empty() {
        return;
    }
    eprintln!(
        "[branchify-trace] new block=b{} params={} term_uses={} insts={} defs={}",
        block.id.0,
        block.params.contains(&reg),
        term_uses,
        inst_uses.len(),
        defines.len()
    );
    for (idx, inst) in defines {
        eprintln!(
            "[branchify-trace] new defines r{} at inst {idx}: {inst}",
            reg.0
        );
    }
    for (idx, inst) in inst_uses {
        eprintln!(
            "[branchify-trace] new uses r{} at inst {idx}: {inst}",
            reg.0
        );
    }
    if term_uses {
        eprintln!(
            "[branchify-trace] new terminator uses r{}: {}",
            reg.0, block.terminator
        );
    }
}

fn instruction_defs_in(head_insts: &[SIRInstruction<RegionedAbsoluteAddr>]) -> HashSet<RegisterId> {
    let mut defs = HashSet::default();
    for inst in head_insts {
        if let Some(def) = def_reg(inst) {
            defs.insert(def);
        }
    }
    defs
}

fn instruction_def_blocks(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, BlockId> {
    let mut defs = HashMap::default();
    for block in eu.blocks.values() {
        for inst in &block.instructions {
            if let Some(def) = def_reg(inst) {
                defs.insert(def, block.id);
            }
        }
    }
    defs
}

fn head_restore_defs(
    original: &BasicBlock<RegionedAbsoluteAddr>,
    plan: &BranchifyPlan,
    remove_defs: &HashSet<usize>,
    def_blocks: &HashMap<RegisterId, BlockId>,
) -> HashSet<usize> {
    let mut head_insts = Vec::new();
    for (idx, inst) in original.instructions.iter().enumerate().take(plan.mux_idx) {
        if !remove_defs.contains(&idx) {
            head_insts.push(inst.clone());
        }
    }
    let head_defs = instruction_defs_in(&head_insts);

    let mut suffix = Vec::new();
    for (idx, inst) in original
        .instructions
        .iter()
        .enumerate()
        .skip(plan.mux_idx + 1)
    {
        if !remove_defs.contains(&idx) {
            suffix.push(inst.clone());
        }
    }

    let mut merge_live_ins = block_live_ins(&suffix, &terminator_uses(&original.terminator));
    if plan.preserve_result {
        merge_live_ins.retain(|reg| *reg != plan.dst);
    }
    merge_live_ins.retain(|reg| {
        !head_defs.contains(reg)
            && def_blocks
                .get(reg)
                .is_none_or(|def_block| *def_block >= plan.block_id)
    });

    let mut true_args = if plan.preserve_result {
        vec![plan.true_val]
    } else {
        Vec::new()
    };
    true_args.extend(merge_live_ins.iter().copied());
    let mut false_args = if plan.preserve_result {
        vec![plan.false_val]
    } else {
        Vec::new()
    };
    false_args.extend(merge_live_ins.iter().copied());

    let true_insts = plan
        .true_defs
        .iter()
        .filter(|idx| remove_defs.contains(idx))
        .map(|&idx| original.instructions[idx].clone())
        .collect::<Vec<_>>();
    let false_insts = plan
        .false_defs
        .iter()
        .filter(|idx| remove_defs.contains(idx))
        .map(|&idx| original.instructions[idx].clone())
        .collect::<Vec<_>>();
    let true_live_ins = block_live_ins(&true_insts, &true_args);
    let false_live_ins = block_live_ins(&false_insts, &false_args);

    let mut needed = HashSet::default();
    needed.insert(plan.cond);
    needed.extend(true_live_ins);
    needed.extend(false_live_ins);
    collect_removed_defs_needed_by_head(original, remove_defs, needed)
}

fn collect_removed_defs_needed_by_head(
    original: &BasicBlock<RegionedAbsoluteAddr>,
    remove_defs: &HashSet<usize>,
    needed: HashSet<RegisterId>,
) -> HashSet<usize> {
    let mut removed_def_pos = HashMap::default();
    for &idx in remove_defs {
        if let Some(def) = def_reg(&original.instructions[idx]) {
            removed_def_pos.insert(def, idx);
        }
    }

    let mut restore = HashSet::default();
    let mut queue = VecDeque::from_iter(needed);
    let mut seen = HashSet::default();
    while let Some(reg) = queue.pop_front() {
        if !seen.insert(reg) {
            continue;
        }
        let Some(&idx) = removed_def_pos.get(&reg) else {
            continue;
        };
        if restore.insert(idx) {
            for use_reg in inst_uses(&original.instructions[idx]) {
                queue.push_back(use_reg);
            }
        }
    }
    restore
}

fn block_live_ins(
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
    terminator_args: &[RegisterId],
) -> Vec<RegisterId> {
    let mut defs = HashSet::default();
    let mut live_ins = Vec::new();
    let mut seen = HashSet::default();

    for inst in instructions {
        for reg in inst_uses(inst) {
            if !defs.contains(&reg) && seen.insert(reg) {
                live_ins.push(reg);
            }
        }
        if let Some(def) = def_reg(inst) {
            defs.insert(def);
        }
    }
    for &reg in terminator_args {
        if !defs.contains(&reg) && seen.insert(reg) {
            live_ins.push(reg);
        }
    }

    live_ins
}

fn inline_param_only_jump_blocks(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    loop {
        let (pred_counts, jump_preds) = predecessor_info(eu);
        let use_blocks = register_use_blocks(eu);
        let mut eligible = eu
            .blocks
            .keys()
            .copied()
            .filter(|&block_id| block_id != eu.entry_block_id)
            .filter(|block_id| param_only_replacement(eu, *block_id, &use_blocks).is_some())
            .filter(|block_id| {
                let jump_count = jump_preds.get(block_id).map_or(0, Vec::len);
                jump_count > 0 && pred_counts.get(block_id).copied().unwrap_or(0) == jump_count
            })
            .collect::<Vec<_>>();
        eligible.sort();

        if eligible.is_empty() {
            break;
        }

        for block_id in eligible {
            if !eu.blocks.contains_key(&block_id) {
                continue;
            }
            let Some(replacement) = param_only_replacement(eu, block_id, &use_blocks) else {
                continue;
            };
            let Some(preds) = jump_preds.get(&block_id) else {
                continue;
            };
            let params = eu.blocks[&block_id].params.clone();
            for &pred_id in preds {
                if !eu.blocks.contains_key(&pred_id) {
                    continue;
                }
                let pred_args = match &eu.blocks[&pred_id].terminator {
                    SIRTerminator::Jump(target, args) if *target == block_id => args.clone(),
                    _ => continue,
                };
                let map = params
                    .iter()
                    .copied()
                    .zip(pred_args)
                    .collect::<HashMap<_, _>>();
                eu.blocks.get_mut(&pred_id).unwrap().terminator =
                    substitute_terminator(&replacement, &map);
            }
            eu.blocks.remove(&block_id);
        }
    }
}

fn param_only_replacement(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    use_blocks: &HashMap<RegisterId, HashSet<BlockId>>,
) -> Option<SIRTerminator> {
    let block = eu.blocks.get(&block_id)?;
    if !block.instructions.is_empty() || block.params.is_empty() {
        return None;
    }
    if block.params.iter().any(|param| {
        use_blocks
            .get(param)
            .is_some_and(|uses| uses.iter().any(|use_block| *use_block != block_id))
    }) {
        return None;
    }
    match &block.terminator {
        SIRTerminator::Jump(_, _) | SIRTerminator::Branch { .. } => Some(block.terminator.clone()),
        SIRTerminator::Return | SIRTerminator::Error(_) => None,
    }
}

fn register_use_blocks(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, HashSet<BlockId>> {
    let mut result = HashMap::<RegisterId, HashSet<BlockId>>::default();
    for block in eu.blocks.values() {
        for inst in &block.instructions {
            for value in inst_uses(inst) {
                result.entry(value).or_default().insert(block.id);
            }
        }
        for value in terminator_uses(&block.terminator) {
            result.entry(value).or_default().insert(block.id);
        }
    }
    result
}

fn predecessor_info(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> (HashMap<BlockId, usize>, HashMap<BlockId, Vec<BlockId>>) {
    let mut pred_counts = HashMap::default();
    let mut jump_preds: HashMap<BlockId, Vec<BlockId>> = HashMap::default();
    for block in eu.blocks.values() {
        match &block.terminator {
            SIRTerminator::Jump(dst, _) => {
                *pred_counts.entry(*dst).or_default() += 1;
                jump_preds.entry(*dst).or_default().push(block.id);
            }
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => {
                *pred_counts.entry(true_block.0).or_default() += 1;
                *pred_counts.entry(false_block.0).or_default() += 1;
            }
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    for preds in jump_preds.values_mut() {
        preds.sort();
    }
    (pred_counts, jump_preds)
}

fn substitute_terminator(
    term: &SIRTerminator,
    map: &HashMap<RegisterId, RegisterId>,
) -> SIRTerminator {
    let replace = |reg: RegisterId| map.get(&reg).copied().unwrap_or(reg);
    match term {
        SIRTerminator::Jump(target, args) => {
            SIRTerminator::Jump(*target, args.iter().copied().map(replace).collect())
        }
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => SIRTerminator::Branch {
            cond: replace(*cond),
            true_block: (
                true_block.0,
                true_block.1.iter().copied().map(replace).collect(),
            ),
            false_block: (
                false_block.0,
                false_block.1.iter().copied().map(replace).collect(),
            ),
        },
        SIRTerminator::Return => SIRTerminator::Return,
        SIRTerminator::Error(code) => SIRTerminator::Error(*code),
    }
}

fn verify_all_uses_have_defs(eu: &ExecutionUnit<RegionedAbsoluteAddr>) {
    let mut defs = HashSet::default();
    for block in eu.blocks.values() {
        defs.extend(block.params.iter().copied());
        for inst in &block.instructions {
            if let Some(def) = def_reg(inst) {
                defs.insert(def);
            }
        }
    }

    for block in eu.blocks.values() {
        for (idx, inst) in block.instructions.iter().enumerate() {
            for reg in inst_uses(inst) {
                assert!(
                    defs.contains(&reg),
                    "branchify verify: r{} used without def/param in b{} inst {}: {}",
                    reg.0,
                    block.id.0,
                    idx,
                    inst
                );
            }
        }
        for reg in terminator_uses(&block.terminator) {
            assert!(
                defs.contains(&reg),
                "branchify verify: r{} used without def/param in b{} terminator: {}",
                reg.0,
                block.id.0,
                block.terminator
            );
        }
    }
}

fn count_uses(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> HashMap<RegisterId, usize> {
    let mut counts = HashMap::default();
    for block in eu.blocks.values() {
        add_block_uses(&mut counts, block);
    }
    counts
}

fn block_use_count(block: &BasicBlock<RegionedAbsoluteAddr>, reg: RegisterId) -> usize {
    let inst_uses = block
        .instructions
        .iter()
        .map(|inst| {
            inst_uses(inst)
                .into_iter()
                .filter(|use_reg| *use_reg == reg)
                .count()
        })
        .sum::<usize>();
    let term_uses = terminator_uses(&block.terminator)
        .into_iter()
        .filter(|use_reg| *use_reg == reg)
        .count();
    inst_uses + term_uses
}

fn add_block_uses(
    counts: &mut HashMap<RegisterId, usize>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
) {
    for inst in &block.instructions {
        for reg in inst_uses(inst) {
            *counts.entry(reg).or_default() += 1;
        }
    }
    for reg in terminator_uses(&block.terminator) {
        *counts.entry(reg).or_default() += 1;
    }
}

fn remove_block_uses(
    counts: &mut HashMap<RegisterId, usize>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
) {
    for inst in &block.instructions {
        for reg in inst_uses(inst) {
            decrement_use(counts, reg);
        }
    }
    for reg in terminator_uses(&block.terminator) {
        decrement_use(counts, reg);
    }
}

fn decrement_use(counts: &mut HashMap<RegisterId, usize>, reg: RegisterId) {
    let Some(count) = counts.get_mut(&reg) else {
        return;
    };
    *count -= 1;
    if *count == 0 {
        counts.remove(&reg);
    }
}

fn inst_uses(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Vec<RegisterId> {
    match inst {
        SIRInstruction::Imm(_, _) => Vec::new(),
        SIRInstruction::Binary(_, lhs, _, rhs) => vec![*lhs, *rhs],
        SIRInstruction::Unary(_, _, src) => vec![*src],
        SIRInstruction::Load(_, _, offset, _) => {
            offset.dynamic_registers().into_iter().flatten().collect()
        }
        SIRInstruction::Store(_, offset, _, src, _, _) => offset
            .dynamic_registers()
            .into_iter()
            .flatten()
            .chain(std::iter::once(*src))
            .collect(),
        SIRInstruction::Commit(_, _, offset, _, _) => {
            offset.dynamic_registers().into_iter().flatten().collect()
        }
        SIRInstruction::Concat(_, args) => args.clone(),
        SIRInstruction::Slice(_, src, _, _) => vec![*src],
        SIRInstruction::Mux(_, cond, true_val, false_val) => vec![*cond, *true_val, *false_val],
        SIRInstruction::RuntimeEvent { args, .. }
        | SIRInstruction::CombCaptureEvent { args, .. } => args.clone(),
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => vec![*old, *new],
    }
}

fn terminator_uses(term: &SIRTerminator) -> Vec<RegisterId> {
    match term {
        SIRTerminator::Jump(_, args) => args.clone(),
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            let mut uses = Vec::with_capacity(1 + true_block.1.len() + false_block.1.len());
            uses.push(*cond);
            uses.extend(true_block.1.iter().copied());
            uses.extend(false_block.1.iter().copied());
            uses
        }
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{InstanceId, RegisterType, SIRValue};
    use num_bigint::BigUint;
    use veryl_analyzer::ir::VarId;

    fn addr(id: usize) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: 0,
            instance_id: InstanceId(id),
            var_id: VarId::default(),
        }
    }

    fn unit(
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut register_map = HashMap::default();
        for reg in 0..26 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
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
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    }

    fn imm(dst: usize, value: u64) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Imm(RegisterId(dst), SIRValue::new(BigUint::from(value)))
    }

    fn append_mul_chain(
        instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        initial: usize,
        factor: usize,
        outputs: &[usize],
    ) {
        let mut lhs = RegisterId(initial);
        for &output in outputs {
            instructions.push(SIRInstruction::Binary(
                RegisterId(output),
                lhs,
                crate::ir::BinaryOp::Mul,
                RegisterId(factor),
            ));
            lhs = RegisterId(output);
        }
    }

    fn profitability(
        true_arm_cost: u128,
        false_arm_cost: u128,
        phi_copy_cost: u128,
        live_through_cost: u128,
    ) -> BranchProfitability {
        profitability_with_probability(
            true_arm_cost,
            false_arm_cost,
            phi_copy_cost,
            live_through_cost,
            StaticBranchProbability::EVEN,
        )
    }

    fn profitability_with_probability(
        true_arm_cost: u128,
        false_arm_cost: u128,
        phi_copy_cost: u128,
        live_through_cost: u128,
        probability: StaticBranchProbability,
    ) -> BranchProfitability {
        BranchProfitability {
            true_arm_cost,
            false_arm_cost,
            removed_mux_cost: 1,
            probability,
            control_cost: BRANCH_CONTROL_COST,
            phi_copy_cost,
            live_through_cost,
        }
    }

    #[test]
    fn one_expensive_arm_must_pay_for_its_unselected_half() {
        // Expected savings: 24 / 2 + 1 = 13. Introduced cost: 11 + 2 = 13.
        // Equality is deliberately rejected because it does not prove a win.
        assert!(!profitability(24, 0, 2, 0).proves_expected_benefit());
    }

    #[test]
    fn work_on_both_arms_can_prove_expected_benefit() {
        // Expected savings: (20 + 20) / 2 + 1 = 21. Introduced cost: 13.
        assert!(profitability(20, 20, 2, 0).proves_expected_benefit());
    }

    #[test]
    fn live_through_cost_can_turn_a_candidate_into_a_rejection() {
        assert!(profitability(20, 10, 2, 0).proves_expected_benefit());
        // Expected savings and introduced cost are now both 16.
        assert!(!profitability(20, 10, 2, 3).proves_expected_benefit());
    }

    #[test]
    fn decoder_probability_can_prove_a_local_expected_win() {
        assert!(!profitability(10, 0, 0, 0).proves_expected_benefit());
        assert!(
            profitability_with_probability(
                10,
                0,
                0,
                0,
                StaticBranchProbability::EQUALITY_TO_CONSTANT,
            )
            .proves_expected_benefit()
        );
    }

    #[test]
    fn static_probability_tracks_constant_equality_and_inversion() {
        let eu = unit(vec![
            imm(1, 7),
            SIRInstruction::Unary(RegisterId(5), crate::ir::UnaryOp::Ident, RegisterId(1)),
            SIRInstruction::Binary(
                RegisterId(2),
                RegisterId(0),
                crate::ir::BinaryOp::EqWildcard,
                RegisterId(5),
            ),
            SIRInstruction::Unary(RegisterId(3), crate::ir::UnaryOp::LogicNot, RegisterId(2)),
            SIRInstruction::Binary(
                RegisterId(4),
                RegisterId(0),
                crate::ir::BinaryOp::Ne,
                RegisterId(1),
            ),
        ]);
        let block = &eu.blocks[&BlockId(0)];
        let def_pos = block
            .instructions
            .iter()
            .enumerate()
            .filter_map(|(idx, inst)| def_reg(inst).map(|register| (register, idx)))
            .collect::<HashMap<_, _>>();

        assert_eq!(
            static_true_probability(block, &def_pos, RegisterId(2)),
            StaticBranchProbability::EQUALITY_TO_CONSTANT,
        );
        assert_eq!(
            static_true_probability(block, &def_pos, RegisterId(3)),
            StaticBranchProbability::EQUALITY_TO_CONSTANT.inverted(),
        );
        assert_eq!(
            static_true_probability(block, &def_pos, RegisterId(4)),
            StaticBranchProbability::EQUALITY_TO_CONSTANT.inverted(),
        );
        assert_eq!(
            static_true_probability(block, &def_pos, RegisterId(0)),
            StaticBranchProbability::EVEN,
        );
    }

    #[test]
    fn runtime_work_cost_scales_with_width_and_operation() {
        let mut register_map = HashMap::default();
        for register in [RegisterId(1), RegisterId(2)] {
            register_map.insert(
                register,
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
        }
        let mul = SIRInstruction::Binary(
            RegisterId(2),
            RegisterId(1),
            crate::ir::BinaryOp::Mul,
            RegisterId(1),
        );
        let div = SIRInstruction::Binary(
            RegisterId(2),
            RegisterId(1),
            crate::ir::BinaryOp::DivU,
            RegisterId(1),
        );
        assert_eq!(branchified_instruction_cost(&mul, &register_map), 5);
        assert_eq!(branchified_instruction_cost(&div, &register_map), 12);

        for register in [RegisterId(1), RegisterId(2)] {
            register_map.insert(
                register,
                RegisterType::Bit {
                    width: 128,
                    signed: false,
                },
            );
        }
        assert_eq!(branchified_instruction_cost(&mul, &register_map), 20);
        assert_eq!(branchified_instruction_cost(&div, &register_map), 48);
    }

    #[test]
    fn branchifies_single_use_mux_arm_work_when_expected_savings_pay_cost() {
        let mut eu = unit(vec![
            imm(1, 3),
            imm(4, 5),
            SIRInstruction::Binary(
                RegisterId(5),
                RegisterId(1),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Binary(
                RegisterId(6),
                RegisterId(5),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Binary(
                RegisterId(7),
                RegisterId(6),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Binary(
                RegisterId(2),
                RegisterId(7),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Store(
                addr(0),
                SIROffset::Static(0),
                64,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ]);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        let head = &eu.blocks[&BlockId(0)];
        assert!(matches!(head.terminator, SIRTerminator::Branch { .. }));
        assert!(eu.blocks.values().any(|block| {
            block.params.is_empty() && matches!(block.terminator, SIRTerminator::Return)
        }));
        assert!(eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Store(_, _, 64, RegisterId(2), _, _)))
        }));
        let SIRTerminator::Branch { false_block, .. } = &head.terminator else {
            panic!("expected mux to become branch");
        };
        assert!(false_block.1.is_empty());
        let false_block = &eu.blocks[&false_block.0];
        assert!(
            false_block.instructions.iter().any(|inst| {
                matches!(inst, SIRInstruction::Store(_, _, 64, RegisterId(4), _, _))
            })
        );
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(RegisterId(2), _, crate::ir::BinaryOp::Mul, _)
                )
            })
        }));
        assert!(!head.instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(RegisterId(2), _, crate::ir::BinaryOp::Mul, _)
            )
        }));
    }

    #[test]
    fn keeps_a_single_cheap_mul_arm_as_a_mux() {
        let mut eu = unit(vec![
            imm(1, 3),
            imm(4, 5),
            SIRInstruction::Binary(
                RegisterId(2),
                RegisterId(1),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Store(
                addr(0),
                SIROffset::Static(0),
                64,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ]);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks.len(), 1);
        assert!(eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4))
            )
        }));
    }

    #[test]
    fn branchifies_a_decoder_biased_arm_with_expected_benefit() {
        let mut instructions = vec![
            imm(1, 3),
            imm(4, 5),
            imm(13, 7),
            SIRInstruction::Binary(
                RegisterId(14),
                RegisterId(0),
                crate::ir::BinaryOp::Eq,
                RegisterId(13),
            ),
        ];
        append_mul_chain(&mut instructions, 1, 1, &[5, 2]);
        instructions.extend([
            SIRInstruction::Mux(RegisterId(3), RegisterId(14), RegisterId(2), RegisterId(4)),
            SIRInstruction::Store(
                addr(0),
                SIROffset::Static(0),
                64,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ]);
        let mut eu = unit(instructions);
        eu.register_map.insert(
            RegisterId(14),
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert!(matches!(
            eu.blocks[&BlockId(0)].terminator,
            SIRTerminator::Branch {
                cond: RegisterId(14),
                ..
            }
        ));
    }

    #[test]
    fn keeps_muxes_in_four_state_mode() {
        let mut instructions = vec![imm(1, 3), imm(4, 5)];
        append_mul_chain(&mut instructions, 1, 1, &[5, 6, 7, 2]);
        instructions.extend([
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Store(
                addr(0),
                SIROffset::Static(0),
                64,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ]);
        let mut eu = unit(instructions);
        let options = PassOptions {
            four_state: true,
            ..Default::default()
        };

        BranchifyMuxPass.run(&mut eu, &options);

        assert_eq!(eu.blocks.len(), 1);
        assert!(
            eu.blocks[&BlockId(0)]
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Mux(RegisterId(3), _, _, _)))
        );
    }

    #[test]
    fn keeps_shared_mux_input_hoisted() {
        let mut eu = unit(vec![
            imm(1, 3),
            SIRInstruction::Binary(
                RegisterId(2),
                RegisterId(1),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(2)),
        ]);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks.len(), 1);
    }

    #[test]
    fn keeps_cheap_select_as_mux() {
        let mut eu = unit(vec![
            imm(1, 3),
            SIRInstruction::Unary(RegisterId(2), crate::ir::UnaryOp::BitNot, RegisterId(1)),
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
        ]);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks.len(), 1);
        assert!(eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4))
            )
        }));
    }

    #[test]
    fn branchifies_non_store_mux_with_arm_work() {
        let mut instructions = vec![imm(1, 3)];
        append_mul_chain(&mut instructions, 1, 1, &[8, 10, 2]);
        append_mul_chain(&mut instructions, 1, 1, &[9, 4]);
        instructions.extend([
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Unary(RegisterId(5), crate::ir::UnaryOp::BitNot, RegisterId(3)),
        ]);
        let mut eu = unit(instructions);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks.len(), 4);
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Mux(RegisterId(3), _, _, _)))
        }));
        assert!(
            eu.blocks
                .values()
                .any(|block| block.params == vec![RegisterId(3)])
        );
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(RegisterId(2), _, crate::ir::BinaryOp::Mul, _)
                )
            })
        }));
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(RegisterId(4), _, crate::ir::BinaryOp::Mul, _)
                )
            })
        }));
    }

    #[test]
    fn does_not_branchify_mux_with_external_uses() {
        let mut eu = unit(vec![
            imm(1, 3),
            SIRInstruction::Binary(
                RegisterId(2),
                RegisterId(1),
                crate::ir::BinaryOp::Mul,
                RegisterId(1),
            ),
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
        ]);
        eu.blocks.get_mut(&BlockId(0)).unwrap().terminator =
            SIRTerminator::Jump(BlockId(1), Vec::new());
        eu.blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Unary(
                    RegisterId(5),
                    crate::ir::UnaryOp::BitNot,
                    RegisterId(3),
                )],
                terminator: SIRTerminator::Return,
            },
        );

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks.len(), 2);
        assert!(eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4))
            )
        }));
    }

    #[test]
    fn removes_mux_at_cfg_controlled_join() {
        let mut register_map = HashMap::default();
        for reg in 0..8 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: if reg == 0 { 1 } else { 64 },
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0)],
                instructions: vec![imm(1, 3), imm(2, 5)],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Binary(
                    RegisterId(3),
                    RegisterId(1),
                    crate::ir::BinaryOp::Mul,
                    RegisterId(1),
                )],
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
        );
        blocks.insert(
            BlockId(2),
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Binary(
                    RegisterId(4),
                    RegisterId(2),
                    crate::ir::BinaryOp::Mul,
                    RegisterId(2),
                )],
                terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
            },
        );
        blocks.insert(
            BlockId(3),
            BasicBlock {
                id: BlockId(3),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Mux(RegisterId(5), RegisterId(0), RegisterId(3), RegisterId(4)),
                    SIRInstruction::Unary(RegisterId(6), crate::ir::UnaryOp::BitNot, RegisterId(5)),
                ],
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.verify_result(), Ok(()));
        assert_eq!(eu.blocks[&BlockId(3)].params, vec![RegisterId(5)]);
        assert!(
            !eu.blocks[&BlockId(3)]
                .instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Mux(RegisterId(5), ..)) })
        );
        assert!(matches!(
            &eu.blocks[&BlockId(1)].terminator,
            SIRTerminator::Jump(BlockId(3), args) if args == &vec![RegisterId(3)]
        ));
        assert!(matches!(
            &eu.blocks[&BlockId(2)].terminator,
            SIRTerminator::Jump(BlockId(3), args) if args == &vec![RegisterId(4)]
        ));
    }

    #[test]
    fn uses_per_edge_path_facts_for_reconvergent_mux() {
        let mut register_map = HashMap::default();
        for reg in 0..6 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: if reg < 2 { 1 } else { 64 },
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1)],
                instructions: vec![imm(2, 3), imm(3, 5)],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(1),
                    true_block: (BlockId(3), Vec::new()),
                    false_block: (BlockId(4), Vec::new()),
                },
            },
        );
        blocks.insert(
            BlockId(2),
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(1),
                    true_block: (BlockId(3), Vec::new()),
                    false_block: (BlockId(5), Vec::new()),
                },
            },
        );
        blocks.insert(
            BlockId(3),
            BasicBlock {
                id: BlockId(3),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Mux(
                    RegisterId(4),
                    RegisterId(1),
                    RegisterId(2),
                    RegisterId(3),
                )],
                terminator: SIRTerminator::Return,
            },
        );
        for block_id in [BlockId(4), BlockId(5)] {
            blocks.insert(
                block_id,
                BasicBlock {
                    id: block_id,
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Return,
                },
            );
        }
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.verify_result(), Ok(()));
        assert_eq!(eu.blocks[&BlockId(3)].params, vec![RegisterId(4)]);
        assert!(
            !eu.blocks[&BlockId(3)]
                .instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Mux(RegisterId(4), ..)) })
        );
        assert!(matches!(
            &eu.blocks[&BlockId(1)].terminator,
            SIRTerminator::Branch { true_block: (_, args), .. } if args == &vec![RegisterId(2)]
        ));
        assert!(matches!(
            &eu.blocks[&BlockId(2)].terminator,
            SIRTerminator::Branch { true_block: (_, args), .. } if args == &vec![RegisterId(2)]
        ));
    }

    #[test]
    fn does_not_use_an_ancestor_branch_for_a_repeated_mux_predicate() {
        // `b0` and `b2` branch on the same SSA predicate.  Structurally, b2
        // dominates b3, so an ancestor-only arm classification would label
        // b3 as b0's false arm.  But b3 is reached on b2's true edge, where
        // the Mux must select r6.  r6 is defined in the join itself and
        // therefore cannot be passed on b3's incoming edge.
        let mut register_map = HashMap::default();
        for reg in 0..8 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: if reg == 0 { 1 } else { 64 },
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(BlockId(2), Vec::new()),
            },
        );
        blocks.insert(
            BlockId(2),
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(3), Vec::new()),
                    false_block: (BlockId(4), Vec::new()),
                },
            },
        );
        for block_id in [BlockId(3), BlockId(4)] {
            blocks.insert(
                block_id,
                BasicBlock {
                    id: block_id,
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Jump(BlockId(5), Vec::new()),
                },
            );
        }
        blocks.insert(
            BlockId(5),
            BasicBlock {
                id: BlockId(5),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Unary(RegisterId(6), crate::ir::UnaryOp::BitNot, RegisterId(1)),
                    SIRInstruction::Mux(RegisterId(7), RegisterId(0), RegisterId(6), RegisterId(1)),
                    SIRInstruction::Store(
                        addr(0),
                        SIROffset::Static(0),
                        64,
                        RegisterId(7),
                        Vec::new(),
                        Vec::new(),
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        eliminate_controlled_join_muxes(&mut eu);

        assert_eq!(eu.verify_result(), Ok(()));
        assert!(eu.blocks[&BlockId(5)].params.is_empty());
        assert!(
            eu.blocks[&BlockId(5)]
                .instructions
                .iter()
                .any(|instruction| {
                    matches!(instruction, SIRInstruction::Mux(RegisterId(7), ..))
                })
        );
        assert!(matches!(
            &eu.blocks[&BlockId(3)].terminator,
            SIRTerminator::Jump(BlockId(5), args) if args.is_empty()
        ));
    }

    #[test]
    fn short_circuits_a_cross_block_priority_chain() {
        let mut register_map = HashMap::default();
        for reg in 0..18 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: if matches!(reg, 6 | 8 | 10) { 1 } else { 64 },
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![
                    RegisterId(0),
                    RegisterId(1),
                    RegisterId(2),
                    RegisterId(3),
                    RegisterId(12),
                ],
                instructions: vec![
                    imm(5, 1),
                    SIRInstruction::Binary(
                        RegisterId(6),
                        RegisterId(0),
                        crate::ir::BinaryOp::Eq,
                        RegisterId(5),
                    ),
                    imm(7, 2),
                    SIRInstruction::Binary(
                        RegisterId(8),
                        RegisterId(0),
                        crate::ir::BinaryOp::Eq,
                        RegisterId(7),
                    ),
                    imm(9, 3),
                    SIRInstruction::Binary(
                        RegisterId(10),
                        RegisterId(0),
                        crate::ir::BinaryOp::Eq,
                        RegisterId(9),
                    ),
                ],
                terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Mux(
                        RegisterId(13),
                        RegisterId(6),
                        RegisterId(1),
                        RegisterId(12),
                    ),
                    SIRInstruction::Mux(
                        RegisterId(14),
                        RegisterId(8),
                        RegisterId(2),
                        RegisterId(13),
                    ),
                    SIRInstruction::Mux(
                        RegisterId(15),
                        RegisterId(10),
                        RegisterId(3),
                        RegisterId(14),
                    ),
                    SIRInstruction::Unary(
                        RegisterId(16),
                        crate::ir::UnaryOp::BitNot,
                        RegisterId(15),
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.verify_result(), Ok(()));
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Mux(..)))
        }));
        assert!(eu.blocks.values().any(|block| {
            matches!(block.terminator, SIRTerminator::Branch { .. })
                && block
                    .instructions
                    .iter()
                    .any(|inst| matches!(inst, SIRInstruction::Binary(RegisterId(10), ..)))
        }));
        assert!(!eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(RegisterId(6) | RegisterId(8) | RegisterId(10), ..)
            )
        }));
    }

    #[test]
    fn moves_pure_arm_dags_from_dominating_blocks() {
        let mut register_map = HashMap::default();
        for reg in 0..26 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: if reg == 0 { 1 } else { 64 },
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
        let mut preheader_insts = vec![imm(1, 3), imm(2, 5)];
        append_mul_chain(
            &mut preheader_insts,
            1,
            1,
            &[3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
        );
        append_mul_chain(
            &mut preheader_insts,
            2,
            2,
            &[13, 14, 15, 16, 17, 18, 19, 20, 21, 22],
        );
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0)],
                instructions: preheader_insts,
                terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Mux(
                        RegisterId(23),
                        RegisterId(0),
                        RegisterId(12),
                        RegisterId(22),
                    ),
                    SIRInstruction::Unary(
                        RegisterId(24),
                        crate::ir::UnaryOp::BitNot,
                        RegisterId(23),
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.verify_result(), Ok(()));
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Mux(RegisterId(23), ..)))
        }));
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(RegisterId(12), _, crate::ir::BinaryOp::Mul, _)
                )
            })
        }));
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(RegisterId(22), _, crate::ir::BinaryOp::Mul, _)
                )
            })
        }));
        assert!(
            eu.blocks
                .values()
                .any(|block| { block.params == vec![RegisterId(23)] })
        );
        assert!(!eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(
                    RegisterId(12) | RegisterId(22),
                    _,
                    crate::ir::BinaryOp::Mul,
                    _
                )
            )
        }));
    }

    #[test]
    fn branches_once_for_multiple_muxes_sharing_an_arm_dag() {
        let mut register_map = HashMap::default();
        for reg in 0..26 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: if reg == 0 { 1 } else { 64 },
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
        let mut preheader_insts = vec![imm(1, 3), imm(2, 5)];
        append_mul_chain(
            &mut preheader_insts,
            1,
            1,
            &[3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
        );
        append_mul_chain(
            &mut preheader_insts,
            2,
            2,
            &[13, 14, 15, 16, 17, 18, 19, 20, 21, 22],
        );
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0)],
                instructions: preheader_insts,
                terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![
                    SIRInstruction::Mux(
                        RegisterId(23),
                        RegisterId(0),
                        RegisterId(12),
                        RegisterId(22),
                    ),
                    SIRInstruction::Mux(
                        RegisterId(24),
                        RegisterId(0),
                        RegisterId(12),
                        RegisterId(22),
                    ),
                    SIRInstruction::Binary(
                        RegisterId(25),
                        RegisterId(23),
                        crate::ir::BinaryOp::Add,
                        RegisterId(24),
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.verify_result(), Ok(()));
        assert!(!eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Mux(RegisterId(23) | RegisterId(24), ..)
                )
            })
        }));
        assert!(
            eu.blocks
                .values()
                .any(|block| { block.params == vec![RegisterId(23), RegisterId(24)] })
        );
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(RegisterId(12), _, crate::ir::BinaryOp::Mul, _)
                )
            })
        }));
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(RegisterId(22), _, crate::ir::BinaryOp::Mul, _)
                )
            })
        }));
        assert!(!eu.blocks[&BlockId(0)].instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(
                    RegisterId(12) | RegisterId(22),
                    _,
                    crate::ir::BinaryOp::Mul,
                    _
                )
            )
        }));
    }

    #[test]
    fn does_not_sink_load_across_aliasing_store() {
        let mut instructions = vec![
            SIRInstruction::Load(RegisterId(1), addr(0), SIROffset::Static(0), 64),
            imm(9, 3),
            SIRInstruction::Store(
                addr(0),
                SIROffset::Static(0),
                64,
                RegisterId(4),
                Vec::new(),
                Vec::new(),
            ),
        ];
        append_mul_chain(&mut instructions, 1, 9, &[6, 7, 8, 10, 2]);
        instructions.extend([
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(5)),
            SIRInstruction::Store(
                addr(1),
                SIROffset::Static(0),
                64,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ]);
        let mut eu = unit(instructions);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        let head = &eu.blocks[&BlockId(0)];
        assert!(matches!(head.terminator, SIRTerminator::Branch { .. }));
        assert!(
            head.instructions
                .iter()
                .any(|inst| { matches!(inst, SIRInstruction::Load(RegisterId(1), _, _, _)) })
        );
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(RegisterId(2), _, crate::ir::BinaryOp::Mul, _)
                )
            })
        }));
    }

    #[test]
    fn sunk_arm_uses_dominating_live_in_directly() {
        let mut instructions = vec![imm(1, 3), imm(4, 5)];
        append_mul_chain(&mut instructions, 7, 1, &[5, 6, 8, 2]);
        instructions.extend([
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Store(
                addr(0),
                SIROffset::Static(0),
                64,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ]);
        let mut eu = unit(instructions);
        eu.blocks.get_mut(&BlockId(0)).unwrap().params = vec![RegisterId(7)];

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        let head = &eu.blocks[&BlockId(0)];
        let SIRTerminator::Branch {
            true_block: true_edge,
            false_block: false_edge,
            ..
        } = &head.terminator
        else {
            panic!("expected mux to become branch");
        };
        let true_block = &eu.blocks[&true_edge.0];
        let false_block = &eu.blocks[&false_edge.0];
        assert!(true_edge.1.is_empty());
        assert!(false_edge.1.is_empty());
        assert!(true_block.params.is_empty());
        assert!(false_block.params.is_empty());
        assert!(true_block.instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(dst, lhs, crate::ir::BinaryOp::Mul, _)
                    if *dst == RegisterId(5) && *lhs == RegisterId(7)
            )
        }));
    }

    #[test]
    fn branchifies_when_suffix_uses_dominating_live_in() {
        let mut instructions = vec![imm(1, 3), imm(6, 11)];
        append_mul_chain(&mut instructions, 1, 1, &[7, 8, 9, 10, 11, 2]);
        instructions.extend([
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Binary(
                RegisterId(5),
                RegisterId(6),
                crate::ir::BinaryOp::Add,
                RegisterId(3),
            ),
        ]);
        let mut eu = unit(instructions);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks.len(), 4);
        assert!(eu.blocks.values().any(|block| {
            block.instructions.iter().any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::Binary(
                        RegisterId(5),
                        RegisterId(6),
                        crate::ir::BinaryOp::Add,
                        RegisterId(3)
                    )
                )
            })
        }));
    }

    #[test]
    fn merge_uses_dominating_param_directly() {
        let mut instructions = vec![imm(1, 3)];
        append_mul_chain(&mut instructions, 1, 1, &[8, 9, 10, 11, 12, 2]);
        instructions.extend([
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Binary(
                RegisterId(5),
                RegisterId(7),
                crate::ir::BinaryOp::Add,
                RegisterId(3),
            ),
        ]);
        let mut eu = unit(instructions);
        eu.blocks.get_mut(&BlockId(0)).unwrap().params = vec![RegisterId(7)];

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        let merge = eu
            .blocks
            .values()
            .find(|block| {
                block
                    .params
                    .first()
                    .is_some_and(|param| *param == RegisterId(3))
            })
            .expect("expected merge block with mux result param");
        assert_eq!(merge.params, vec![RegisterId(3)]);
        assert!(merge.instructions.iter().any(|inst| {
            matches!(
                inst,
                SIRInstruction::Binary(RegisterId(5), lhs, crate::ir::BinaryOp::Add, RegisterId(3))
                    if *lhs == RegisterId(7)
            )
        }));
        assert!(eu.blocks.values().any(|block| {
            matches!(
                &block.terminator,
                SIRTerminator::Jump(target, args)
                    if *target == merge.id && args.len() == 1
            )
        }));
    }

    #[test]
    fn inlines_param_only_branch_blocks_from_jump_predecessors() {
        let mut register_map = HashMap::default();
        for reg in 0..8 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions: vec![imm(1, 3)],
                terminator: SIRTerminator::Jump(BlockId(1), vec![RegisterId(1)]),
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: vec![RegisterId(2)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(2), vec![RegisterId(2)]),
                    false_block: (BlockId(3), vec![RegisterId(2)]),
                },
            },
        );
        blocks.insert(
            BlockId(2),
            BasicBlock {
                id: BlockId(2),
                params: vec![RegisterId(4)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        );
        blocks.insert(
            BlockId(3),
            BasicBlock {
                id: BlockId(3),
                params: vec![RegisterId(5)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        inline_param_only_jump_blocks(&mut eu);

        assert!(!eu.blocks.contains_key(&BlockId(1)));
        assert!(matches!(
            &eu.blocks[&BlockId(0)].terminator,
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } if true_block.1 == vec![RegisterId(1)] && false_block.1 == vec![RegisterId(1)]
        ));
    }

    #[test]
    fn keeps_param_only_branch_when_descendant_uses_parameter_directly() {
        let mut register_map = HashMap::default();
        for reg in 0..6 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
        }
        register_map.insert(
            RegisterId(5),
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions: vec![imm(1, 3)],
                terminator: SIRTerminator::Jump(BlockId(1), vec![RegisterId(1)]),
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: vec![RegisterId(2)],
                instructions: vec![SIRInstruction::Imm(RegisterId(5), SIRValue::new(1u8))],
                terminator: SIRTerminator::Branch {
                    cond: RegisterId(5),
                    true_block: (BlockId(2), Vec::new()),
                    false_block: (BlockId(3), Vec::new()),
                },
            },
        );
        blocks.insert(
            BlockId(2),
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Unary(
                    RegisterId(4),
                    crate::ir::UnaryOp::BitNot,
                    RegisterId(2),
                )],
                terminator: SIRTerminator::Return,
            },
        );
        blocks.insert(
            BlockId(3),
            BasicBlock {
                id: BlockId(3),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        inline_param_only_jump_blocks(&mut eu);

        assert!(eu.blocks.contains_key(&BlockId(1)));
        eu.verify();
    }

    #[test]
    fn keeps_cheap_mux_feeding_jump_args() {
        let mut register_map = HashMap::default();
        for reg in 0..8 {
            register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
        }
        let mut blocks = HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions: vec![imm(1, 1), imm(2, 2), imm(3, 3)],
                terminator: SIRTerminator::Jump(
                    BlockId(1),
                    vec![RegisterId(1), RegisterId(2), RegisterId(3)],
                ),
            },
        );
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: vec![RegisterId(4), RegisterId(5), RegisterId(6)],
                instructions: vec![SIRInstruction::Mux(
                    RegisterId(7),
                    RegisterId(4),
                    RegisterId(5),
                    RegisterId(6),
                )],
                terminator: SIRTerminator::Jump(BlockId(2), vec![RegisterId(7)]),
            },
        );
        blocks.insert(
            BlockId(2),
            BasicBlock {
                id: BlockId(2),
                params: vec![RegisterId(7)],
                instructions: Vec::new(),
                terminator: SIRTerminator::Return,
            },
        );
        let mut eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert!(eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Mux(RegisterId(7), _, _, _)))
        }));
    }

    #[test]
    fn preserves_mux_result_through_merge_when_used_after_store() {
        let mut instructions = vec![imm(1, 3)];
        append_mul_chain(&mut instructions, 1, 1, &[5, 6, 7, 8, 2]);
        instructions.extend([
            SIRInstruction::Mux(RegisterId(3), RegisterId(0), RegisterId(2), RegisterId(4)),
            SIRInstruction::Store(
                addr(0),
                SIROffset::Static(0),
                64,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Store(
                addr(1),
                SIROffset::Static(0),
                64,
                RegisterId(3),
                Vec::new(),
                Vec::new(),
            ),
        ]);
        let mut eu = unit(instructions);

        BranchifyMuxPass.run(&mut eu, &PassOptions::default());

        assert_eq!(eu.blocks.len(), 4);
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Mux(RegisterId(3), _, _, _)))
        }));
        assert!(
            eu.blocks
                .values()
                .any(|block| block.params == vec![RegisterId(3)])
        );
        assert!(eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Store(_, _, 64, RegisterId(3), _, _)))
        }));
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Store(_, _, 64, RegisterId(2), _, _)))
        }));
        assert!(!eu.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|inst| matches!(inst, SIRInstruction::Store(_, _, 64, RegisterId(4), _, _)))
        }));
    }
}
