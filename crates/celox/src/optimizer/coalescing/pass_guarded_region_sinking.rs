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
use crate::ir::*;
use crate::optimizer::PassOptions;
use crate::{HashMap, HashSet};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(super) struct GuardedRegionSinkingPass;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UseSite {
    Instruction { block: BlockId, index: usize },
    BranchCondition { block: BlockId },
    TrueEdgeArgument { block: BlockId },
    FalseEdgeArgument { block: BlockId },
    JumpArgument { block: BlockId },
}

impl UseSite {
    fn block(self) -> BlockId {
        match self {
            Self::Instruction { block, .. }
            | Self::BranchCondition { block }
            | Self::TrueEdgeArgument { block }
            | Self::FalseEdgeArgument { block }
            | Self::JumpArgument { block } => block,
        }
    }
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
    moved: HashSet<usize>,
    distributed: Vec<DistributedStore>,
    removable_muxes: HashSet<usize>,
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

        let predecessors = predecessor_map(eu);
        let dominators = Dominators::compute(eu, &predecessors);
        let uses = collect_uses(eu);
        let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
        block_ids.sort_unstable_by_key(|id| id.0);

        // Plans are built only from the input CFG.  Blocks generated below are
        // intentionally not revisited in this run, which makes termination
        // independent of function size or any iteration budget.
        let plans = block_ids
            .into_iter()
            .filter_map(|block_id| plan_block(eu, block_id, &predecessors, &dominators, &uses))
            .collect::<Vec<_>>();
        if plans.is_empty() {
            return;
        }
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
    }
}

fn plan_block(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    predecessors: &BTreeMap<BlockId, BTreeSet<BlockId>>,
    dominators: &Dominators,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
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
    if eu.register_map.get(cond).map(RegisterType::width) != Some(1)
        || true_block.0 == false_block.0
        || true_block.0 == eu.entry_block_id
        || dominators.dominates(true_block.0, block_id)
        || predecessors
            .get(&true_block.0)?
            .iter()
            .copied()
            .ne([block_id])
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
            && !matches!(offset, SIROffset::Dynamic(dynamic) if *dynamic == result)
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
        true_block.0,
        &local_defs,
        uses,
        dominators,
        &distributed,
        &removable_muxes,
        &safe_to_move,
    );

    let mut seeds = VecDeque::new();
    for store in &distributed {
        if local_defs.contains_key(&store.true_value) {
            seeds.push_back(store.true_value);
        }
    }
    seeds.extend(
        true_block
            .1
            .iter()
            .copied()
            .filter(|reg| local_defs.contains_key(reg)),
    );
    for &reg in local_defs.keys() {
        if uses.get(&reg).into_iter().flatten().any(|site| {
            site.block() != block_id && dominators.dominates(true_block.0, site.block())
        }) {
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

    // The false store value and dynamic store offset must remain available on
    // both edges.  The source mux itself is the only store operand removed.
    for store in &distributed {
        if local_defs
            .get(&store.false_value)
            .is_some_and(|index| moved.contains(index))
        {
            return None;
        }
        if let SIRInstruction::Store(_, SIROffset::Dynamic(offset), _, _, _, _) =
            &block.instructions[store.index]
            && local_defs
                .get(offset)
                .is_some_and(|index| moved.contains(index) || removable_muxes.contains(index))
        {
            return None;
        }
    }

    // Recheck the selected closed region explicitly.  This is deliberately
    // redundant with `can_move`: it keeps application independent from the
    // fixed-point implementation and makes every external use proof local to
    // the completed plan.
    for &index in &moved {
        let Some(dst) = def_reg(&block.instructions[index]) else {
            return None;
        };
        if uses.get(&dst).into_iter().flatten().any(|site| {
            !use_is_owned_by_true_edge(
                *site,
                dst,
                block_id,
                true_block.0,
                block,
                &moved,
                &distributed,
                &removable_muxes,
                dominators,
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
    true_target: BlockId,
    local_defs: &HashMap<RegisterId, usize>,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
    dominators: &Dominators,
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
            use_can_follow_true_edge(
                eu,
                *site,
                dst,
                block.id,
                true_target,
                local_defs,
                &result,
                distributed,
                removable_muxes,
                dominators,
            )
        });
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn use_can_follow_true_edge(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    site: UseSite,
    value: RegisterId,
    source_block: BlockId,
    true_target: BlockId,
    _local_defs: &HashMap<RegisterId, usize>,
    moveable: &[bool],
    distributed: &[DistributedStore],
    removable_muxes: &HashSet<usize>,
    dominators: &Dominators,
) -> bool {
    match site {
        UseSite::Instruction { block, index } if block == source_block => {
            if removable_muxes.contains(&index) {
                return removable_mux_true_value(eu, source_block, index) == Some(value);
            }
            def_reg(&eu.blocks[&block].instructions[index])
                .is_some_and(|_| moveable.get(index).copied().unwrap_or(false))
                || distributed.iter().any(|store| {
                    store.index == index && store.true_value == value && store.false_value != value
                })
        }
        UseSite::TrueEdgeArgument { block } if block == source_block => true,
        UseSite::BranchCondition { block }
        | UseSite::FalseEdgeArgument { block }
        | UseSite::JumpArgument { block }
            if block == source_block =>
        {
            false
        }
        _ => dominators.dominates(true_target, site.block()),
    }
}

#[allow(clippy::too_many_arguments)]
fn use_is_owned_by_true_edge(
    site: UseSite,
    value: RegisterId,
    source_block: BlockId,
    true_target: BlockId,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    moved: &HashSet<usize>,
    distributed: &[DistributedStore],
    removable_muxes: &HashSet<usize>,
    dominators: &Dominators,
) -> bool {
    match site {
        UseSite::Instruction {
            block: use_block,
            index,
        } if use_block == source_block => {
            moved.contains(&index)
                || removable_muxes.contains(&index)
                    && removable_mux_true_value_in_block(block, index) == Some(value)
                || distributed.iter().any(|store| {
                    store.index == index && store.true_value == value && store.false_value != value
                })
        }
        UseSite::TrueEdgeArgument { block } if block == source_block => true,
        UseSite::BranchCondition { block }
        | UseSite::FalseEdgeArgument { block }
        | UseSite::JumpArgument { block }
            if block == source_block =>
        {
            false
        }
        _ => dominators.dominates(true_target, site.block()),
    }
}

fn removable_mux_true_value(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: BlockId,
    index: usize,
) -> Option<RegisterId> {
    removable_mux_true_value_in_block(eu.blocks.get(&block)?, index)
}

fn removable_mux_true_value_in_block(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    index: usize,
) -> Option<RegisterId> {
    match block.instructions.get(index)? {
        SIRInstruction::Mux(_, _, true_value, _) => Some(*true_value),
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
            true_instructions.push(inst);
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

fn instruction_uses(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Vec<RegisterId> {
    match inst {
        SIRInstruction::Imm(..) => Vec::new(),
        SIRInstruction::Binary(_, lhs, _, rhs) => vec![*lhs, *rhs],
        SIRInstruction::Unary(_, _, source) | SIRInstruction::Slice(_, source, _, _) => {
            vec![*source]
        }
        SIRInstruction::Load(_, _, SIROffset::Dynamic(offset), _) => vec![*offset],
        SIRInstruction::Load(_, _, SIROffset::Static(_), _) => Vec::new(),
        SIRInstruction::Store(_, SIROffset::Dynamic(offset), _, source, _, _) => {
            vec![*offset, *source]
        }
        SIRInstruction::Store(_, SIROffset::Static(_), _, source, _, _) => vec![*source],
        SIRInstruction::Commit(_, _, SIROffset::Dynamic(offset), _, _) => vec![*offset],
        SIRInstruction::Commit(_, _, SIROffset::Static(_), _, _) => Vec::new(),
        SIRInstruction::Concat(_, args)
        | SIRInstruction::RuntimeEvent { args, .. }
        | SIRInstruction::CombCaptureEvent { args, .. } => args.clone(),
        SIRInstruction::Mux(_, cond, true_value, false_value) => {
            vec![*cond, *true_value, *false_value]
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => vec![*old, *new],
    }
}

fn collect_uses(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> HashMap<RegisterId, Vec<UseSite>> {
    let mut result = HashMap::<RegisterId, Vec<UseSite>>::default();
    for block in eu.blocks.values() {
        for (index, inst) in block.instructions.iter().enumerate() {
            for reg in instruction_uses(inst) {
                result.entry(reg).or_default().push(UseSite::Instruction {
                    block: block.id,
                    index,
                });
            }
        }
        match &block.terminator {
            SIRTerminator::Jump(_, args) => {
                for &reg in args {
                    result
                        .entry(reg)
                        .or_default()
                        .push(UseSite::JumpArgument { block: block.id });
                }
            }
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                result
                    .entry(*cond)
                    .or_default()
                    .push(UseSite::BranchCondition { block: block.id });
                for &reg in &true_block.1 {
                    result
                        .entry(reg)
                        .or_default()
                        .push(UseSite::TrueEdgeArgument { block: block.id });
                }
                for &reg in &false_block.1 {
                    result
                        .entry(reg)
                        .or_default()
                        .push(UseSite::FalseEdgeArgument { block: block.id });
                }
            }
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    result
}

fn predecessor_map<A>(eu: &ExecutionUnit<A>) -> BTreeMap<BlockId, BTreeSet<BlockId>> {
    let mut predecessors = eu
        .blocks
        .keys()
        .copied()
        .map(|id| (id, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    for block in eu.blocks.values() {
        for successor in successors(&block.terminator) {
            if let Some(entries) = predecessors.get_mut(&successor) {
                entries.insert(block.id);
            }
        }
    }
    predecessors
}

fn successors(terminator: &SIRTerminator) -> Vec<BlockId> {
    match terminator {
        SIRTerminator::Jump(target, _) => vec![*target],
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => vec![true_block.0, false_block.0],
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

struct Dominators {
    index: HashMap<BlockId, usize>,
    enter: Vec<usize>,
    exit: Vec<usize>,
}

impl Dominators {
    fn compute<A>(
        eu: &ExecutionUnit<A>,
        predecessors: &BTreeMap<BlockId, BTreeSet<BlockId>>,
    ) -> Self {
        let mut successor_map = eu
            .blocks
            .keys()
            .copied()
            .map(|id| (id, successors(&eu.blocks[&id].terminator)))
            .collect::<BTreeMap<_, _>>();
        for entries in successor_map.values_mut() {
            entries.sort_unstable_by_key(|id| id.0);
        }

        let entry = eu.entry_block_id;
        let mut visited = BTreeSet::new();
        let mut postorder = Vec::with_capacity(eu.blocks.len());
        visited.insert(entry);
        let mut stack = vec![(entry, 0usize)];
        while let Some((block, next)) = stack.last_mut() {
            if *next == successor_map[block].len() {
                postorder.push(*block);
                stack.pop();
                continue;
            }
            let successor = successor_map[block][*next];
            *next += 1;
            if visited.insert(successor) {
                stack.push((successor, 0));
            }
        }
        postorder.reverse();
        let index = postorder
            .iter()
            .enumerate()
            .map(|(index, &block)| (block, index))
            .collect::<HashMap<_, _>>();
        let mut immediate = vec![None; postorder.len()];
        immediate[0] = Some(0);
        let intersect = |mut left: usize, mut right: usize, idom: &[Option<usize>]| {
            while left != right {
                while left > right {
                    left = idom[left].expect("verified predecessor must be processed");
                }
                while right > left {
                    right = idom[right].expect("verified predecessor must be processed");
                }
            }
            left
        };
        loop {
            let mut changed = false;
            for block_index in 1..postorder.len() {
                let block = postorder[block_index];
                let mut processed = predecessors[&block]
                    .iter()
                    .filter_map(|predecessor| index.get(predecessor).copied())
                    .filter(|predecessor| immediate[*predecessor].is_some());
                let Some(first) = processed.next() else {
                    continue;
                };
                let next = processed.fold(first, |current, predecessor| {
                    intersect(current, predecessor, &immediate)
                });
                if immediate[block_index] != Some(next) {
                    immediate[block_index] = Some(next);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        let mut children = vec![Vec::new(); postorder.len()];
        for (block, parent) in immediate.iter().enumerate().skip(1) {
            children[parent.expect("verified reachable block must have an idom")].push(block);
        }
        let mut enter = vec![0usize; postorder.len()];
        let mut exit = vec![0usize; postorder.len()];
        let mut time = 0usize;
        let mut events = vec![(0usize, false)];
        while let Some((block, leaving)) = events.pop() {
            if leaving {
                exit[block] = time;
                time += 1;
            } else {
                enter[block] = time;
                time += 1;
                events.push((block, true));
                events.extend(children[block].iter().rev().map(|child| (*child, false)));
            }
        }
        Self { index, enter, exit }
    }

    fn dominates(&self, dominator: BlockId, block: BlockId) -> bool {
        let (Some(&dominator), Some(&block)) = (self.index.get(&dominator), self.index.get(&block))
        else {
            return false;
        };
        self.enter[dominator] <= self.enter[block] && self.exit[block] <= self.exit[dominator]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{DomainKind, InstanceId, SIRValue, TriggerIdWithKind};
    use veryl_analyzer::ir::VarId;

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

    #[derive(Debug, PartialEq, Eq)]
    struct ExecutionTrace {
        stores: Vec<(RegionedAbsoluteAddr, usize, usize, u64)>,
    }

    fn execute(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        condition: u64,
        input: u64,
    ) -> ExecutionTrace {
        let mut registers = HashMap::default();
        registers.insert(RegisterId(0), condition);
        registers.insert(RegisterId(1), input);
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
                    SIRInstruction::Unary(dst, UnaryOp::Ident, source) => {
                        registers.insert(*dst, registers[source]);
                    }
                    SIRInstruction::Load(dst, addr, SIROffset::Static(offset), _) => {
                        registers.insert(*dst, memory.get(&(*addr, *offset)).copied().unwrap_or(0));
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
        let mut options = PassOptions::default();
        options.four_state = true;

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
    fn malformed_input_is_non_destructive() {
        let mut eu = shared_dag_unit();
        eu.blocks.remove(&BlockId(2));
        assert!(eu.verify_result().is_err());
        let before = eu.clone();

        GuardedRegionSinkingPass.run(&mut eu, &PassOptions::default());

        assert_unchanged(&before, &eu);
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
