//! Replace a counted circular-priority scan with packed mask operations.
//!
//! A common RTL arbitration idiom scans every lane, forms a one-bit predicate,
//! and retains the matching lane with the smallest `(lane - head) mod N` age.
//! For a power-of-two lane count this is exactly a rotate of the packed
//! predicate followed by CTZ.  The rewrite is discovered from the natural CFG
//! loop and its SSA recurrences; source/layer order is not consulted.

use super::pass_manager::ExecutionUnitPass;
use super::pass_vectorize_concat::remove_dead_definitions;
use super::shared::{def_reg, sir_value_to_u64};
use crate::ir::cfg::SirCfg;
use crate::ir::*;
use crate::optimizer::PassOptions;
use crate::{HashMap, HashSet};

#[derive(Clone, Copy, Debug)]
enum Definition {
    Parameter { block: BlockId },
    Instruction { block: BlockId, index: usize },
}

impl Definition {
    fn block(self) -> BlockId {
        match self {
            Self::Parameter { block } | Self::Instruction { block, .. } => block,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum PackedNode {
    Zero,
    Ones,
    Load(RegionedAbsoluteAddr),
    Broadcast(RegisterId),
    Not(usize),
    And(usize, usize),
    Or(usize, usize),
}

#[derive(Clone, Debug)]
struct PackedExpression {
    nodes: Vec<PackedNode>,
    root: usize,
    invert: bool,
    dynamic_loads: usize,
    value_ops: usize,
}

#[derive(Clone, Debug)]
struct CircularPriorityPlan {
    preheader: BlockId,
    loop_blocks: Vec<BlockId>,
    hoisted_instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    exit: BlockId,
    exit_found_position: usize,
    exit_best_position: usize,
    head: RegisterId,
    lanes: usize,
    found_type: RegisterType,
    best_type: RegisterType,
    predicate: PackedExpression,
}

#[derive(Clone, Default)]
pub(super) struct CircularPriorityPass {
    bit_array_elements: HashMap<AbsoluteAddr, usize>,
}

impl CircularPriorityPass {
    pub(super) fn for_program(program: &Program) -> Self {
        let mut bit_array_elements = HashMap::default();
        for (&instance_id, &module_id) in &program.instance_module {
            for info in program.module_variables[&module_id].values() {
                if info.array_dims.is_empty() {
                    continue;
                }
                let Some(element_count) = info
                    .array_dims
                    .iter()
                    .try_fold(1usize, |count, &dimension| count.checked_mul(dimension))
                else {
                    continue;
                };
                if element_count == 0 || info.width != element_count {
                    continue;
                }
                bit_array_elements.insert(
                    AbsoluteAddr {
                        instance_id,
                        var_id: info.id,
                    },
                    element_count,
                );
            }
        }
        Self { bit_array_elements }
    }
}

impl ExecutionUnitPass for CircularPriorityPass {
    fn name(&self) -> &'static str {
        "circular_priority"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        if options.four_state || eu.blocks.len() < 6 || self.bit_array_elements.is_empty() {
            return;
        }
        let Ok(cfg) = SirCfg::analyze(eu) else {
            return;
        };
        let definitions = collect_definitions(eu);
        let mut constant_cache = HashMap::default();
        let mut plans = Vec::new();

        // Only innermost natural loops are candidates. This keeps discovery
        // linear in the instructions owned by disjoint loop regions rather
        // than repeatedly walking an enclosing loop's complete body.
        let parent_loops = cfg
            .loops
            .iter()
            .filter_map(|natural_loop| natural_loop.parent)
            .collect::<HashSet<_>>();
        for (loop_index, natural_loop) in cfg.loops.iter().enumerate() {
            if parent_loops.contains(&loop_index) {
                continue;
            }
            let loop_blocks = natural_loop
                .blocks
                .iter()
                .map(|&block| cfg.block_ids[block])
                .collect::<HashSet<_>>();
            if let Some(plan) = recognize_loop(
                eu,
                &cfg,
                cfg.block_ids[natural_loop.header],
                &loop_blocks,
                &definitions,
                &mut constant_cache,
                &self.bit_array_elements,
            ) {
                plans.push(plan);
            }
        }

        if plans.is_empty() {
            return;
        }
        plans.sort_unstable_by_key(|plan| plan.preheader);
        let mut occupied = HashSet::default();
        plans.retain(|plan| {
            if plan
                .loop_blocks
                .iter()
                .any(|block| occupied.contains(block))
            {
                return false;
            }
            occupied.extend(plan.loop_blocks.iter().copied());
            true
        });
        prepare_escaping_definitions(eu, &cfg, &definitions, &mut plans);
        if plans.is_empty() || !ids_available(eu, &plans) {
            return;
        }

        let mut next_register = eu.register_map.keys().map(|reg| reg.0).max().unwrap_or(0);
        let mut next_block = eu.blocks.keys().map(|block| block.0).max().unwrap_or(0);
        for plan in plans {
            apply_plan(eu, plan, &mut next_register, &mut next_block);
        }
        remove_dead_definitions(eu);
    }
}

fn collect_definitions(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, Definition> {
    let mut definitions = HashMap::default();
    for (&block_id, block) in &eu.blocks {
        for &parameter in &block.params {
            definitions.insert(parameter, Definition::Parameter { block: block_id });
        }
        for (index, instruction) in block.instructions.iter().enumerate() {
            if let Some(register) = def_reg(instruction) {
                definitions.insert(
                    register,
                    Definition::Instruction {
                        block: block_id,
                        index,
                    },
                );
            }
        }
    }
    definitions
}

fn instruction<'a>(
    eu: &'a ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    register: RegisterId,
) -> Option<&'a SIRInstruction<RegionedAbsoluteAddr>> {
    let Definition::Instruction { block, index } = *definitions.get(&register)? else {
        return None;
    };
    eu.blocks.get(&block)?.instructions.get(index)
}

fn immediate(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    register: RegisterId,
) -> Option<u64> {
    let SIRInstruction::Imm(_, value) = instruction(eu, definitions, register)? else {
        return None;
    };
    sir_value_to_u64(value)
}

fn edge_arguments(terminator: &SIRTerminator, target: BlockId) -> Option<&[RegisterId]> {
    match terminator {
        SIRTerminator::Jump(block, arguments) if *block == target => Some(arguments),
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => match (true_block.0 == target, false_block.0 == target) {
            (true, false) => Some(&true_block.1),
            (false, true) => Some(&false_block.1),
            _ => None,
        },
        _ => None,
    }
}

fn branch_targets(terminator: &SIRTerminator) -> Option<(RegisterId, BlockId, BlockId)> {
    let SIRTerminator::Branch {
        cond,
        true_block,
        false_block,
    } = terminator
    else {
        return None;
    };
    (true_block.0 != false_block.0).then_some((*cond, true_block.0, false_block.0))
}

fn recognize_loop(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    header: BlockId,
    loop_blocks: &HashSet<BlockId>,
    definitions: &HashMap<RegisterId, Definition>,
    constant_cache: &mut HashMap<RegisterId, Option<bool>>,
    bit_array_elements: &HashMap<AbsoluteAddr, usize>,
) -> Option<CircularPriorityPlan> {
    if loop_blocks.len() != 5 {
        return None;
    }
    let header_index = cfg.block_index(header)?;
    let outside_predecessors = cfg.predecessors[header_index]
        .iter()
        .map(|&block| cfg.block_ids[block])
        .filter(|block| !loop_blocks.contains(block))
        .collect::<Vec<_>>();
    let inside_predecessors = cfg.predecessors[header_index]
        .iter()
        .map(|&block| cfg.block_ids[block])
        .filter(|block| loop_blocks.contains(block))
        .collect::<Vec<_>>();
    let [preheader] = outside_predecessors.as_slice() else {
        return None;
    };
    let [latch] = inside_predecessors.as_slice() else {
        return None;
    };
    let preheader = *preheader;
    let latch = *latch;
    let preheader_block = &eu.blocks[&preheader];
    let header_block = &eu.blocks[&header];
    let latch_block = &eu.blocks[&latch];
    let SIRTerminator::Jump(preheader_target, entry_arguments) = &preheader_block.terminator else {
        return None;
    };
    if *preheader_target != header || entry_arguments.len() != header_block.params.len() {
        return None;
    }

    let (latch_condition, latch_true, latch_false) = branch_targets(&latch_block.terminator)?;
    let (exit, loop_when_true) = if latch_true == header && !loop_blocks.contains(&latch_false) {
        (latch_false, true)
    } else if latch_false == header && !loop_blocks.contains(&latch_true) {
        (latch_true, false)
    } else {
        return None;
    };
    let backedge_arguments = edge_arguments(&latch_block.terminator, header)?;
    let exit_arguments = edge_arguments(&latch_block.terminator, exit)?;
    if backedge_arguments.len() != header_block.params.len()
        || exit_arguments.len() != eu.blocks[&exit].params.len()
        || latch_block.params.len() != 2
    {
        return None;
    }

    let (count_position, lanes) = match_count_recurrence(
        eu,
        definitions,
        header_block,
        entry_arguments,
        backedge_arguments,
        latch_condition,
        loop_when_true,
    )?;
    if !(4..=32).contains(&lanes) || !lanes.is_power_of_two() {
        return None;
    }
    let index_position = match_index_recurrence(
        eu,
        definitions,
        header_block,
        entry_arguments,
        backedge_arguments,
        count_position,
    )?;
    let lane_bits = lanes.trailing_zeros() as usize;
    if eu.register_map[&header_block.params[count_position]].width() <= lane_bits
        || eu.register_map[&header_block.params[index_position]].width() < lane_bits
    {
        return None;
    }

    let (header_condition, header_true, header_false) = branch_targets(&header_block.terminator)?;
    for (candidate, skip, invert_predicate) in [
        (header_true, header_false, false),
        (header_false, header_true, true),
    ] {
        let Some(plan) = recognize_orientation(
            eu,
            cfg,
            header,
            candidate,
            skip,
            latch,
            exit,
            loop_blocks,
            definitions,
            constant_cache,
            preheader,
            header_condition,
            invert_predicate,
            lanes,
            index_position,
            count_position,
            entry_arguments,
            backedge_arguments,
            exit_arguments,
            bit_array_elements,
        ) else {
            continue;
        };
        return Some(plan);
    }
    None
}

fn match_count_recurrence(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    header: &BasicBlock<RegionedAbsoluteAddr>,
    entry_arguments: &[RegisterId],
    backedge_arguments: &[RegisterId],
    condition: RegisterId,
    loop_when_true: bool,
) -> Option<(usize, usize)> {
    let SIRInstruction::Binary(_, lhs, operation, rhs) = instruction(eu, definitions, condition)?
    else {
        return None;
    };
    let operation_matches = matches!(operation, BinaryOp::Ne) && loop_when_true
        || matches!(operation, BinaryOp::Eq) && !loop_when_true;
    if !operation_matches {
        return None;
    }
    let next_count = if immediate(eu, definitions, *lhs) == Some(0) {
        *rhs
    } else if immediate(eu, definitions, *rhs) == Some(0) {
        *lhs
    } else {
        return None;
    };
    let position = backedge_arguments
        .iter()
        .position(|&argument| argument == next_count)?;
    let counter = *header.params.get(position)?;
    let SIRInstruction::Binary(_, decrement_lhs, BinaryOp::Sub, decrement_rhs) =
        instruction(eu, definitions, next_count)?
    else {
        return None;
    };
    if *decrement_lhs != counter || immediate(eu, definitions, *decrement_rhs) != Some(1) {
        return None;
    }
    let lanes = usize::try_from(immediate(eu, definitions, entry_arguments[position])?).ok()?;
    (lanes != 0).then_some((position, lanes))
}

fn match_index_recurrence(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    header: &BasicBlock<RegionedAbsoluteAddr>,
    entry_arguments: &[RegisterId],
    backedge_arguments: &[RegisterId],
    count_position: usize,
) -> Option<usize> {
    let mut found = None;
    for position in 0..header.params.len() {
        if position == count_position
            || immediate(eu, definitions, entry_arguments[position]) != Some(0)
        {
            continue;
        }
        let next_index = backedge_arguments[position];
        let Some(SIRInstruction::Binary(_, lhs, BinaryOp::Add, rhs)) =
            instruction(eu, definitions, next_index)
        else {
            continue;
        };
        let index = header.params[position];
        let matches = *lhs == index && immediate(eu, definitions, *rhs) == Some(1)
            || *rhs == index && immediate(eu, definitions, *lhs) == Some(1);
        if !matches || found.replace(position).is_some() {
            return None;
        }
    }
    found
}

#[allow(clippy::too_many_arguments)]
fn recognize_orientation(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    header: BlockId,
    candidate: BlockId,
    skip: BlockId,
    latch: BlockId,
    exit: BlockId,
    loop_blocks: &HashSet<BlockId>,
    definitions: &HashMap<RegisterId, Definition>,
    constant_cache: &mut HashMap<RegisterId, Option<bool>>,
    preheader: BlockId,
    predicate: RegisterId,
    invert_predicate: bool,
    lanes: usize,
    index_position: usize,
    count_position: usize,
    entry_arguments: &[RegisterId],
    backedge_arguments: &[RegisterId],
    exit_arguments: &[RegisterId],
    bit_array_elements: &HashMap<AbsoluteAddr, usize>,
) -> Option<CircularPriorityPlan> {
    if !loop_blocks.contains(&candidate)
        || !loop_blocks.contains(&skip)
        || candidate == skip
        || candidate == latch
        || skip == latch
    {
        return None;
    }
    let header_block = &eu.blocks[&header];
    let candidate_block = &eu.blocks[&candidate];
    let skip_block = &eu.blocks[&skip];
    let latch_block = &eu.blocks[&latch];
    if !skip_block.instructions.is_empty() {
        return None;
    }
    let SIRTerminator::Jump(skip_target, skip_arguments) = &skip_block.terminator else {
        return None;
    };
    if *skip_target != latch || skip_arguments.len() != latch_block.params.len() {
        return None;
    }

    let (update_condition, candidate_true, candidate_false) =
        branch_targets(&candidate_block.terminator)?;
    let orientations = [
        (candidate_true, candidate_false, true),
        (candidate_false, candidate_true, false),
    ];
    for (update, candidate_skip, update_when_true) in orientations {
        if candidate_skip != skip
            || !loop_blocks.contains(&update)
            || update == header
            || update == candidate
            || update == skip
            || update == latch
        {
            continue;
        }
        let update_block = &eu.blocks[&update];
        if !update_block.instructions.is_empty() {
            continue;
        }
        let SIRTerminator::Jump(update_target, update_arguments) = &update_block.terminator else {
            continue;
        };
        if *update_target != latch || update_arguments.len() != latch_block.params.len() {
            continue;
        }
        let expected_blocks = [header, candidate, skip, update, latch]
            .into_iter()
            .collect::<HashSet<_>>();
        if &expected_blocks != loop_blocks {
            continue;
        }

        let state_positions = (0..header_block.params.len())
            .filter(|position| *position != count_position && *position != index_position)
            .collect::<Vec<_>>();
        let [first_state, second_state] = state_positions.as_slice() else {
            continue;
        };
        let mut found_merge = None;
        let mut best_merge = None;
        for merge_position in 0..latch_block.params.len() {
            let Some(header_position) = [*first_state, *second_state]
                .into_iter()
                .find(|&position| header_block.params[position] == skip_arguments[merge_position])
            else {
                found_merge = None;
                best_merge = None;
                break;
            };
            if immediate(eu, definitions, update_arguments[merge_position]) == Some(1)
                && eu.register_map[&header_block.params[header_position]].width() == 1
            {
                if found_merge
                    .replace((merge_position, header_position))
                    .is_some()
                {
                    found_merge = None;
                    break;
                }
            } else if best_merge
                .replace((merge_position, header_position))
                .is_some()
            {
                best_merge = None;
                break;
            }
        }
        let (Some((found_merge, found_position)), Some((best_merge, best_position))) =
            (found_merge, best_merge)
        else {
            continue;
        };
        if immediate(eu, definitions, entry_arguments[found_position]) != Some(0)
            || immediate(eu, definitions, entry_arguments[best_position]) != Some(0)
            || backedge_arguments[found_position] != latch_block.params[found_merge]
            || backedge_arguments[best_position] != latch_block.params[best_merge]
        {
            continue;
        }

        let age = update_arguments[best_merge];
        let found = header_block.params[found_position];
        let best = header_block.params[best_position];
        let Some(head) = match_update_condition(
            eu,
            definitions,
            update_condition,
            update_when_true,
            found,
            best,
            age,
            header_block.params[index_position],
            lanes,
        ) else {
            continue;
        };

        let found_exit = exit_arguments
            .iter()
            .position(|&argument| argument == latch_block.params[found_merge]);
        let best_exit = exit_arguments
            .iter()
            .position(|&argument| argument == latch_block.params[best_merge]);
        let (Some(found_exit), Some(best_exit)) = (found_exit, best_exit) else {
            continue;
        };
        if found_exit == best_exit
            || exit_arguments.len() != 2
            || eu.register_map[&eu.blocks[&exit].params[found_exit]].width() != 1
            || eu.register_map[&eu.blocks[&exit].params[best_exit]].width()
                != lanes.trailing_zeros() as usize
            || eu.register_map[&head].width() != lanes.trailing_zeros() as usize
        {
            continue;
        }

        if !loop_is_pure(eu, loop_blocks) {
            continue;
        }
        let Some(predicate) = build_packed_expression(
            eu,
            cfg,
            definitions,
            constant_cache,
            loop_blocks,
            preheader,
            header_block.params[index_position],
            predicate,
            invert_predicate,
            lanes,
            bit_array_elements,
        ) else {
            continue;
        };
        let old_cost =
            lanes.saturating_mul(predicate.dynamic_loads.saturating_add(predicate.value_ops));
        let new_cost = predicate
            .dynamic_loads
            .saturating_add(predicate.value_ops)
            .saturating_add(8);
        if predicate.dynamic_loads == 0 || old_cost <= new_cost {
            continue;
        }

        let mut loop_blocks = loop_blocks.iter().copied().collect::<Vec<_>>();
        loop_blocks.sort_unstable();
        return Some(CircularPriorityPlan {
            preheader,
            loop_blocks,
            hoisted_instructions: Vec::new(),
            exit,
            exit_found_position: found_exit,
            exit_best_position: best_exit,
            head,
            lanes,
            found_type: eu.register_map[&eu.blocks[&exit].params[found_exit]].clone(),
            best_type: eu.register_map[&eu.blocks[&exit].params[best_exit]].clone(),
            predicate,
        });
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn match_update_condition(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    condition: RegisterId,
    update_when_true: bool,
    found: RegisterId,
    best: RegisterId,
    age: RegisterId,
    index: RegisterId,
    lanes: usize,
) -> Option<RegisterId> {
    let mut condition = strip_boolean_identity(eu, definitions, condition);
    if !update_when_true {
        let SIRInstruction::Unary(_, operation @ (UnaryOp::LogicNot | UnaryOp::BitNot), inner) =
            instruction(eu, definitions, condition)?
        else {
            return None;
        };
        let _ = operation;
        condition = strip_boolean_identity(eu, definitions, *inner);
    }
    let SIRInstruction::Binary(_, lhs, operation @ (BinaryOp::LogicOr | BinaryOp::Or), rhs) =
        instruction(eu, definitions, condition)?
    else {
        return None;
    };
    let _ = operation;
    let matches = |not_found, less| {
        matches_not(eu, definitions, not_found, found)
            && matches!(
                instruction(eu, definitions, less),
                Some(SIRInstruction::Binary(_, less_lhs, BinaryOp::LtU, less_rhs))
                    if *less_lhs == age && *less_rhs == best
            )
    };
    if !matches(*lhs, *rhs) && !matches(*rhs, *lhs) {
        return None;
    }
    match_circular_age(eu, definitions, age, index, lanes)
}

fn matches_not(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    register: RegisterId,
    source: RegisterId,
) -> bool {
    matches!(
        instruction(eu, definitions, strip_boolean_identity(eu, definitions, register)),
        Some(SIRInstruction::Unary(_, UnaryOp::LogicNot | UnaryOp::BitNot, inner))
            if *inner == source
    )
}

fn match_circular_age(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    age: RegisterId,
    index: RegisterId,
    lanes: usize,
) -> Option<RegisterId> {
    let mask = u64::try_from(lanes - 1).ok()?;
    let SIRInstruction::Binary(_, difference, BinaryOp::And, age_mask) =
        instruction(eu, definitions, age)?
    else {
        return None;
    };
    if immediate(eu, definitions, *age_mask) != Some(mask) {
        return None;
    }
    let SIRInstruction::Binary(_, lane, BinaryOp::Sub, head) =
        instruction(eu, definitions, *difference)?
    else {
        return None;
    };
    matches_index_mod(eu, definitions, *lane, index, mask).then_some(*head)
}

fn matches_index_mod(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    mut register: RegisterId,
    index: RegisterId,
    mask: u64,
) -> bool {
    loop {
        if register == index {
            return true;
        }
        match instruction(eu, definitions, register) {
            Some(SIRInstruction::Unary(_, UnaryOp::Ident, inner)) => register = *inner,
            Some(SIRInstruction::Binary(_, source, BinaryOp::Shr, amount))
                if immediate(eu, definitions, *amount) == Some(0) =>
            {
                register = *source;
            }
            Some(SIRInstruction::Binary(_, lhs, BinaryOp::And, rhs))
                if immediate(eu, definitions, *rhs) == Some(mask) =>
            {
                register = *lhs;
            }
            Some(SIRInstruction::Binary(_, lhs, BinaryOp::And, rhs))
                if immediate(eu, definitions, *lhs) == Some(mask) =>
            {
                register = *rhs;
            }
            _ => return false,
        }
    }
}

fn strip_boolean_identity(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    mut register: RegisterId,
) -> RegisterId {
    loop {
        let Some(instruction) = instruction(eu, definitions, register) else {
            return register;
        };
        match instruction {
            SIRInstruction::Unary(_, UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Or, inner)
                if eu.register_map.get(inner).map(RegisterType::width) == Some(1) =>
            {
                register = *inner;
            }
            _ => return register,
        }
    }
}

fn loop_is_pure(eu: &ExecutionUnit<RegionedAbsoluteAddr>, loop_blocks: &HashSet<BlockId>) -> bool {
    loop_blocks.iter().all(|block| {
        eu.blocks[block].instructions.iter().all(|instruction| {
            !matches!(
                instruction,
                SIRInstruction::Store(..)
                    | SIRInstruction::Commit(..)
                    | SIRInstruction::RuntimeEvent { .. }
                    | SIRInstruction::CombCaptureEvent { .. }
                    | SIRInstruction::CombCaptureEnableIfChanged { .. }
            )
        })
    })
}

/// Find loop definitions used elsewhere with one whole-EU use walk, then keep
/// only plans whose escaping values are pure loop invariants.  This is linear
/// in the EU plus the dependency closure of the values that must be hoisted;
/// it does not rescan the complete EU once per natural loop.
fn prepare_escaping_definitions(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    definitions: &HashMap<RegisterId, Definition>,
    plans: &mut Vec<CircularPriorityPlan>,
) {
    let mut block_owner = HashMap::<BlockId, usize>::default();
    let mut register_owner = HashMap::<RegisterId, usize>::default();
    for (plan_index, plan) in plans.iter().enumerate() {
        for &block_id in &plan.loop_blocks {
            block_owner.insert(block_id, plan_index);
            let block = &eu.blocks[&block_id];
            for &parameter in &block.params {
                register_owner.insert(parameter, plan_index);
            }
            for instruction in &block.instructions {
                if let Some(register) = def_reg(instruction) {
                    register_owner.insert(register, plan_index);
                }
            }
        }
    }

    let mut escaping_roots = vec![HashSet::default(); plans.len()];
    let mut uses = Vec::new();
    for (&block_id, block) in &eu.blocks {
        for instruction in &block.instructions {
            uses.clear();
            instruction_uses(instruction, &mut uses);
            for &register in &uses {
                let Some(&owner) = register_owner.get(&register) else {
                    continue;
                };
                if block_owner.get(&block_id) != Some(&owner) {
                    escaping_roots[owner].insert(register);
                }
            }
        }
        uses.clear();
        terminator_uses(&block.terminator, &mut uses);
        for &register in &uses {
            let Some(&owner) = register_owner.get(&register) else {
                continue;
            };
            if block_owner.get(&block_id) != Some(&owner) {
                escaping_roots[owner].insert(register);
            }
        }
    }

    let mut keep = vec![true; plans.len()];
    for (plan_index, plan) in plans.iter_mut().enumerate() {
        let loop_blocks = plan.loop_blocks.iter().copied().collect::<HashSet<_>>();
        let Some(instructions) = hoist_escaping_definitions(
            eu,
            cfg,
            plan.preheader,
            &loop_blocks,
            definitions,
            &escaping_roots[plan_index],
        ) else {
            keep[plan_index] = false;
            continue;
        };
        plan.hoisted_instructions = instructions;
    }

    let old_plans = std::mem::take(plans);
    *plans = old_plans
        .into_iter()
        .zip(keep)
        .filter_map(|(plan, keep)| keep.then_some(plan))
        .collect();
}

fn hoist_escaping_definitions(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    preheader: BlockId,
    loop_blocks: &HashSet<BlockId>,
    definitions: &HashMap<RegisterId, Definition>,
    roots: &HashSet<RegisterId>,
) -> Option<Vec<SIRInstruction<RegionedAbsoluteAddr>>> {
    let mut roots = roots.iter().copied().collect::<Vec<_>>();
    roots.sort_unstable();
    let mut state = HashMap::<RegisterId, bool>::default();
    let mut hoisted = Vec::new();

    for root in roots {
        let mut stack = vec![(root, false)];
        while let Some((register, expanded)) = stack.pop() {
            if expanded {
                if state.get(&register) == Some(&true) {
                    continue;
                }
                let definition = instruction(eu, definitions, register)?.clone();
                hoisted.push(definition);
                state.insert(register, true);
                continue;
            }
            match state.get(&register) {
                Some(true) => continue,
                Some(false) => return None,
                None => {}
            }
            let Definition::Instruction { block, .. } = *definitions.get(&register)? else {
                return None;
            };
            if !loop_blocks.contains(&block) {
                return None;
            }
            let definition = instruction(eu, definitions, register)?;
            if !matches!(
                definition,
                SIRInstruction::Imm(..)
                    | SIRInstruction::Binary(..)
                    | SIRInstruction::Unary(..)
                    | SIRInstruction::Slice(..)
                    | SIRInstruction::Concat(..)
                    | SIRInstruction::Mux(..)
            ) {
                return None;
            }

            state.insert(register, false);
            stack.push((register, true));
            let mut operands = Vec::new();
            instruction_uses(definition, &mut operands);
            for operand in operands.into_iter().rev() {
                let operand_definition = *definitions.get(&operand)?;
                if loop_blocks.contains(&operand_definition.block()) {
                    stack.push((operand, false));
                } else if !cfg.dominates(operand_definition.block(), preheader) {
                    return None;
                }
            }
        }
    }
    Some(hoisted)
}

fn instruction_uses(
    instruction: &SIRInstruction<RegionedAbsoluteAddr>,
    uses: &mut Vec<RegisterId>,
) {
    match instruction {
        SIRInstruction::Imm(..) => {}
        SIRInstruction::Binary(_, lhs, _, rhs) => uses.extend([*lhs, *rhs]),
        SIRInstruction::Unary(_, _, source) | SIRInstruction::Slice(_, source, ..) => {
            uses.push(*source);
        }
        SIRInstruction::Load(_, _, offset, _) => {
            uses.extend(offset.dynamic_registers().into_iter().flatten());
        }
        SIRInstruction::Store(_, offset, _, source, _, _) => {
            uses.push(*source);
            uses.extend(offset.dynamic_registers().into_iter().flatten());
        }
        SIRInstruction::Commit(_, _, offset, _, _) => {
            uses.extend(offset.dynamic_registers().into_iter().flatten());
        }
        SIRInstruction::Concat(_, arguments)
        | SIRInstruction::RuntimeEvent {
            args: arguments, ..
        }
        | SIRInstruction::CombCaptureEvent {
            args: arguments, ..
        } => uses.extend(arguments.iter().copied()),
        SIRInstruction::Mux(_, condition, true_value, false_value) => {
            uses.extend([*condition, *true_value, *false_value]);
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
            uses.extend([*old, *new]);
        }
    }
}

fn terminator_uses(terminator: &SIRTerminator, uses: &mut Vec<RegisterId>) {
    match terminator {
        SIRTerminator::Jump(_, arguments) => uses.extend(arguments.iter().copied()),
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            uses.push(*cond);
            uses.extend(true_block.1.iter().copied());
            uses.extend(false_block.1.iter().copied());
        }
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}

#[derive(Clone, Copy)]
enum PackedOperation {
    Identity(RegisterId),
    Not(RegisterId),
    And(RegisterId, RegisterId),
    Or(RegisterId, RegisterId),
}

#[allow(clippy::too_many_arguments)]
fn build_packed_expression(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    definitions: &HashMap<RegisterId, Definition>,
    constant_cache: &mut HashMap<RegisterId, Option<bool>>,
    loop_blocks: &HashSet<BlockId>,
    preheader: BlockId,
    index: RegisterId,
    root: RegisterId,
    invert: bool,
    lanes: usize,
    bit_array_elements: &HashMap<AbsoluteAddr, usize>,
) -> Option<PackedExpression> {
    let root = strip_boolean_identity(eu, definitions, root);
    let mut nodes = Vec::new();
    let mut interned = HashMap::<PackedNode, usize>::default();
    let mut values = HashMap::<RegisterId, usize>::default();
    let mut operations = HashMap::<RegisterId, PackedOperation>::default();
    let mut stack = vec![(root, false)];

    while let Some((register, expanded)) = stack.pop() {
        if values.contains_key(&register) {
            continue;
        }
        if !expanded {
            if let Some(value) = boolean_constant(eu, definitions, constant_cache, register) {
                let node = if value {
                    PackedNode::Ones
                } else {
                    PackedNode::Zero
                };
                values.insert(register, intern_node(&mut nodes, &mut interned, node));
                continue;
            }
            if eu.register_map.get(&register).map(RegisterType::width) != Some(1) {
                return None;
            }
            let definition = definitions.get(&register).copied()?;
            if !loop_blocks.contains(&definition.block()) {
                if !cfg.dominates(definition.block(), preheader) {
                    return None;
                }
                values.insert(
                    register,
                    intern_node(&mut nodes, &mut interned, PackedNode::Broadcast(register)),
                );
                continue;
            }
            let definition_instruction = instruction(eu, definitions, register)?;
            match definition_instruction {
                SIRInstruction::Load(
                    _,
                    address,
                    SIROffset::Element {
                        index: load_index,
                        element_width: 1,
                        bit_offset: 0,
                        dynamic_bit_offset: None,
                    },
                    1,
                ) if *load_index == index
                    && bit_array_elements
                        .get(&address.absolute_addr())
                        .is_some_and(|&element_count| element_count >= lanes) =>
                {
                    values.insert(
                        register,
                        intern_node(&mut nodes, &mut interned, PackedNode::Load(*address)),
                    );
                }
                SIRInstruction::Unary(
                    _,
                    UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Or,
                    source,
                ) if eu.register_map.get(source).map(RegisterType::width) == Some(1) => {
                    operations.insert(register, PackedOperation::Identity(*source));
                    stack.push((register, true));
                    stack.push((*source, false));
                }
                SIRInstruction::Unary(_, UnaryOp::LogicNot | UnaryOp::BitNot, source) => {
                    operations.insert(register, PackedOperation::Not(*source));
                    stack.push((register, true));
                    stack.push((*source, false));
                }
                SIRInstruction::Binary(_, lhs, BinaryOp::LogicAnd | BinaryOp::And, rhs) => {
                    operations.insert(register, PackedOperation::And(*lhs, *rhs));
                    stack.push((register, true));
                    stack.push((*rhs, false));
                    stack.push((*lhs, false));
                }
                SIRInstruction::Binary(_, lhs, BinaryOp::LogicOr | BinaryOp::Or, rhs) => {
                    operations.insert(register, PackedOperation::Or(*lhs, *rhs));
                    stack.push((register, true));
                    stack.push((*rhs, false));
                    stack.push((*lhs, false));
                }
                SIRInstruction::Binary(_, lhs, BinaryOp::Eq, rhs)
                    if immediate(eu, definitions, *rhs) == Some(1) =>
                {
                    operations.insert(register, PackedOperation::Identity(*lhs));
                    stack.push((register, true));
                    stack.push((*lhs, false));
                }
                SIRInstruction::Binary(_, lhs, BinaryOp::Eq, rhs)
                    if immediate(eu, definitions, *lhs) == Some(1) =>
                {
                    operations.insert(register, PackedOperation::Identity(*rhs));
                    stack.push((register, true));
                    stack.push((*rhs, false));
                }
                SIRInstruction::Binary(_, lhs, BinaryOp::Eq, rhs)
                    if immediate(eu, definitions, *rhs) == Some(0) =>
                {
                    operations.insert(register, PackedOperation::Not(*lhs));
                    stack.push((register, true));
                    stack.push((*lhs, false));
                }
                SIRInstruction::Binary(_, lhs, BinaryOp::Eq, rhs)
                    if immediate(eu, definitions, *lhs) == Some(0) =>
                {
                    operations.insert(register, PackedOperation::Not(*rhs));
                    stack.push((register, true));
                    stack.push((*rhs, false));
                }
                _ => return None,
            }
            continue;
        }

        let operation = operations.get(&register).copied()?;
        let node = match operation {
            PackedOperation::Identity(source) => {
                values.insert(register, *values.get(&source)?);
                continue;
            }
            PackedOperation::Not(source) => simplify_not(&nodes, *values.get(&source)?),
            PackedOperation::And(lhs, rhs) => {
                simplify_and(&nodes, *values.get(&lhs)?, *values.get(&rhs)?)
            }
            PackedOperation::Or(lhs, rhs) => {
                simplify_or(&nodes, *values.get(&lhs)?, *values.get(&rhs)?)
            }
        };
        values.insert(register, intern_node(&mut nodes, &mut interned, node));
    }

    let root = *values.get(&root)?;
    let dynamic_loads = nodes
        .iter()
        .filter(|node| matches!(node, PackedNode::Load(_)))
        .count();
    let value_ops = nodes
        .iter()
        .filter(|node| {
            matches!(
                node,
                PackedNode::Broadcast(_)
                    | PackedNode::Not(_)
                    | PackedNode::And(..)
                    | PackedNode::Or(..)
            )
        })
        .count();
    Some(PackedExpression {
        nodes,
        root,
        invert,
        dynamic_loads,
        value_ops,
    })
}

fn intern_node(
    nodes: &mut Vec<PackedNode>,
    interned: &mut HashMap<PackedNode, usize>,
    node: PackedNode,
) -> usize {
    if let Some(&index) = interned.get(&node) {
        return index;
    }
    let index = nodes.len();
    nodes.push(node.clone());
    interned.insert(node, index);
    index
}

fn simplify_not(nodes: &[PackedNode], source: usize) -> PackedNode {
    match nodes[source] {
        PackedNode::Zero => PackedNode::Ones,
        PackedNode::Ones => PackedNode::Zero,
        PackedNode::Not(inner) => nodes[inner].clone(),
        _ => PackedNode::Not(source),
    }
}

fn simplify_and(nodes: &[PackedNode], lhs: usize, rhs: usize) -> PackedNode {
    if lhs == rhs {
        return nodes[lhs].clone();
    }
    match (&nodes[lhs], &nodes[rhs]) {
        (PackedNode::Zero, _) | (_, PackedNode::Zero) => PackedNode::Zero,
        (PackedNode::Ones, other) | (other, PackedNode::Ones) => other.clone(),
        _ => PackedNode::And(lhs.min(rhs), lhs.max(rhs)),
    }
}

fn simplify_or(nodes: &[PackedNode], lhs: usize, rhs: usize) -> PackedNode {
    if lhs == rhs {
        return nodes[lhs].clone();
    }
    match (&nodes[lhs], &nodes[rhs]) {
        (PackedNode::Ones, _) | (_, PackedNode::Ones) => PackedNode::Ones,
        (PackedNode::Zero, other) | (other, PackedNode::Zero) => other.clone(),
        _ => PackedNode::Or(lhs.min(rhs), lhs.max(rhs)),
    }
}

fn boolean_constant(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    cache: &mut HashMap<RegisterId, Option<bool>>,
    root: RegisterId,
) -> Option<bool> {
    if let Some(value) = cache.get(&root) {
        return *value;
    }
    let mut stack = vec![(root, false)];
    while let Some((register, expanded)) = stack.pop() {
        if cache.contains_key(&register) {
            continue;
        }
        if eu.register_map.get(&register).map(RegisterType::width) != Some(1) {
            cache.insert(register, None);
            continue;
        }
        let Some(definition) = instruction(eu, definitions, register) else {
            cache.insert(register, None);
            continue;
        };
        if !expanded {
            match definition {
                SIRInstruction::Imm(_, value) => {
                    cache.insert(
                        register,
                        sir_value_to_u64(value).map(|value| value & 1 != 0),
                    );
                }
                SIRInstruction::Unary(
                    _,
                    UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Or,
                    source,
                )
                | SIRInstruction::Unary(_, UnaryOp::LogicNot | UnaryOp::BitNot, source) => {
                    stack.push((register, true));
                    stack.push((*source, false));
                }
                SIRInstruction::Binary(
                    _,
                    lhs,
                    BinaryOp::LogicAnd
                    | BinaryOp::And
                    | BinaryOp::LogicOr
                    | BinaryOp::Or
                    | BinaryOp::Eq
                    | BinaryOp::Ne,
                    rhs,
                ) => {
                    stack.push((register, true));
                    stack.push((*rhs, false));
                    stack.push((*lhs, false));
                }
                _ => {
                    cache.insert(register, None);
                }
            }
            continue;
        }

        let value = match definition {
            SIRInstruction::Unary(
                _,
                UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Or,
                source,
            ) => cache.get(source).copied().flatten(),
            SIRInstruction::Unary(_, UnaryOp::LogicNot | UnaryOp::BitNot, source) => {
                cache.get(source).copied().flatten().map(|value| !value)
            }
            SIRInstruction::Binary(_, lhs, BinaryOp::LogicAnd | BinaryOp::And, rhs) => match (
                cache.get(lhs).copied().flatten(),
                cache.get(rhs).copied().flatten(),
            ) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            },
            SIRInstruction::Binary(_, lhs, BinaryOp::LogicOr | BinaryOp::Or, rhs) => match (
                cache.get(lhs).copied().flatten(),
                cache.get(rhs).copied().flatten(),
            ) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            },
            SIRInstruction::Binary(_, lhs, BinaryOp::Eq, rhs) => cache
                .get(lhs)
                .copied()
                .flatten()
                .zip(cache.get(rhs).copied().flatten())
                .map(|(lhs, rhs)| lhs == rhs),
            SIRInstruction::Binary(_, lhs, BinaryOp::Ne, rhs) => cache
                .get(lhs)
                .copied()
                .flatten()
                .zip(cache.get(rhs).copied().flatten())
                .map(|(lhs, rhs)| lhs != rhs),
            _ => None,
        };
        cache.insert(register, value);
    }
    cache.get(&root).copied().flatten()
}

fn ids_available(eu: &ExecutionUnit<RegionedAbsoluteAddr>, plans: &[CircularPriorityPlan]) -> bool {
    let required_registers = plans.iter().try_fold(0usize, |total, plan| {
        total
            .checked_add(plan.predicate.nodes.len())?
            .checked_add(12)
    });
    let required_blocks = plans.len().checked_mul(2);
    let (Some(required_registers), Some(required_blocks)) = (required_registers, required_blocks)
    else {
        return false;
    };
    let max_register = eu
        .register_map
        .keys()
        .map(|register| register.0)
        .max()
        .unwrap_or(0);
    let max_block = eu.blocks.keys().map(|block| block.0).max().unwrap_or(0);
    max_register.checked_add(required_registers).is_some()
        && max_block.checked_add(required_blocks).is_some()
}

fn fresh_register(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    next_register: &mut usize,
    register_type: RegisterType,
) -> RegisterId {
    *next_register += 1;
    let register = RegisterId(*next_register);
    assert!(eu.register_map.insert(register, register_type).is_none());
    register
}

fn fresh_block(next_block: &mut usize) -> BlockId {
    *next_block += 1;
    BlockId(*next_block)
}

fn unsigned_type(width: usize) -> RegisterType {
    RegisterType::Bit {
        width,
        signed: false,
    }
}

#[derive(Clone, Copy)]
enum EmittedValue {
    Constant(bool),
    Register(RegisterId),
}

struct ExpressionEmitter<'a> {
    eu: &'a mut ExecutionUnit<RegionedAbsoluteAddr>,
    next_register: &'a mut usize,
    width: usize,
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    zero: Option<RegisterId>,
    ones: Option<RegisterId>,
}

impl ExpressionEmitter<'_> {
    fn register(&mut self, value: EmittedValue) -> RegisterId {
        match value {
            EmittedValue::Register(register) => register,
            EmittedValue::Constant(false) => {
                if let Some(register) = self.zero {
                    return register;
                }
                let register =
                    fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                self.instructions
                    .push(SIRInstruction::Imm(register, SIRValue::new(0u8)));
                self.zero = Some(register);
                register
            }
            EmittedValue::Constant(true) => {
                if let Some(register) = self.ones {
                    return register;
                }
                let mask = if self.width == 64 {
                    u64::MAX
                } else {
                    (1u64 << self.width) - 1
                };
                let register =
                    fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                self.instructions
                    .push(SIRInstruction::Imm(register, SIRValue::new(mask)));
                self.ones = Some(register);
                register
            }
        }
    }

    fn emit(
        mut self,
        expression: &PackedExpression,
    ) -> (RegisterId, Vec<SIRInstruction<RegionedAbsoluteAddr>>) {
        let mut values = Vec::<EmittedValue>::with_capacity(expression.nodes.len());
        for node in &expression.nodes {
            let value = match *node {
                PackedNode::Zero => EmittedValue::Constant(false),
                PackedNode::Ones => EmittedValue::Constant(true),
                PackedNode::Load(address) => {
                    let register =
                        fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                    self.instructions.push(SIRInstruction::Load(
                        register,
                        address,
                        SIROffset::Static(0),
                        self.width,
                    ));
                    EmittedValue::Register(register)
                }
                PackedNode::Broadcast(condition) => {
                    let zero = self.register(EmittedValue::Constant(false));
                    let ones = self.register(EmittedValue::Constant(true));
                    let register =
                        fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                    self.instructions
                        .push(SIRInstruction::Mux(register, condition, ones, zero));
                    EmittedValue::Register(register)
                }
                PackedNode::Not(source) => match values[source] {
                    EmittedValue::Constant(value) => EmittedValue::Constant(!value),
                    source => {
                        let source = self.register(source);
                        let register =
                            fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                        self.instructions.push(SIRInstruction::Unary(
                            register,
                            UnaryOp::BitNot,
                            source,
                        ));
                        EmittedValue::Register(register)
                    }
                },
                PackedNode::And(lhs, rhs) => match (values[lhs], values[rhs]) {
                    (EmittedValue::Constant(false), _) | (_, EmittedValue::Constant(false)) => {
                        EmittedValue::Constant(false)
                    }
                    (EmittedValue::Constant(true), value)
                    | (value, EmittedValue::Constant(true)) => value,
                    (lhs, rhs) => {
                        let lhs = self.register(lhs);
                        let rhs = self.register(rhs);
                        let register =
                            fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                        self.instructions.push(SIRInstruction::Binary(
                            register,
                            lhs,
                            BinaryOp::And,
                            rhs,
                        ));
                        EmittedValue::Register(register)
                    }
                },
                PackedNode::Or(lhs, rhs) => match (values[lhs], values[rhs]) {
                    (EmittedValue::Constant(true), _) | (_, EmittedValue::Constant(true)) => {
                        EmittedValue::Constant(true)
                    }
                    (EmittedValue::Constant(false), value)
                    | (value, EmittedValue::Constant(false)) => value,
                    (lhs, rhs) => {
                        let lhs = self.register(lhs);
                        let rhs = self.register(rhs);
                        let register =
                            fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                        self.instructions.push(SIRInstruction::Binary(
                            register,
                            lhs,
                            BinaryOp::Or,
                            rhs,
                        ));
                        EmittedValue::Register(register)
                    }
                },
            };
            values.push(value);
        }
        let mut root = values[expression.root];
        if expression.invert {
            root = match root {
                EmittedValue::Constant(value) => EmittedValue::Constant(!value),
                source => {
                    let source = self.register(source);
                    let register =
                        fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                    self.instructions.push(SIRInstruction::Unary(
                        register,
                        UnaryOp::BitNot,
                        source,
                    ));
                    EmittedValue::Register(register)
                }
            };
        }
        let root = self.register(root);
        (root, self.instructions)
    }
}

fn apply_plan(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: CircularPriorityPlan,
    next_register: &mut usize,
    next_block: &mut usize,
) {
    let selected_block = fresh_block(next_block);
    let empty_block = fresh_block(next_block);
    let emitter = ExpressionEmitter {
        eu,
        next_register,
        width: plan.lanes,
        instructions: Vec::new(),
        zero: None,
        ones: None,
    };
    let (mask, predicate_instructions) = emitter.emit(&plan.predicate);
    let mut instructions = plan.hoisted_instructions;
    instructions.extend(predicate_instructions);

    let doubled_width = plan.lanes * 2;
    let doubled = fresh_register(eu, next_register, unsigned_type(doubled_width));
    let shifted = fresh_register(eu, next_register, unsigned_type(doubled_width));
    let rotated = fresh_register(eu, next_register, unsigned_type(plan.lanes));
    let nonempty = fresh_register(eu, next_register, unsigned_type(1));
    instructions.push(SIRInstruction::Concat(doubled, vec![mask, mask]));
    instructions.push(SIRInstruction::Binary(
        shifted,
        doubled,
        BinaryOp::Shr,
        plan.head,
    ));
    instructions.push(SIRInstruction::Slice(rotated, shifted, 0, plan.lanes));
    instructions.push(SIRInstruction::Unary(nonempty, UnaryOp::Or, rotated));

    let preheader = eu.blocks.get_mut(&plan.preheader).unwrap();
    preheader.instructions.extend(instructions);
    preheader.terminator = SIRTerminator::Branch {
        cond: nonempty,
        true_block: (selected_block, Vec::new()),
        false_block: (empty_block, Vec::new()),
    };

    let count_width = UnaryOp::CountTrailingZeros.result_width(plan.lanes);
    let count = fresh_register(eu, next_register, unsigned_type(count_width));
    let selected_age = fresh_register(eu, next_register, plan.best_type.clone());
    let found = fresh_register(eu, next_register, plan.found_type.clone());
    let mut selected_arguments = vec![selected_age; 2];
    selected_arguments[plan.exit_found_position] = found;
    selected_arguments[plan.exit_best_position] = selected_age;
    let selected = BasicBlock {
        id: selected_block,
        params: Vec::new(),
        instructions: vec![
            SIRInstruction::Unary(count, UnaryOp::CountTrailingZeros, rotated),
            SIRInstruction::Slice(selected_age, count, 0, plan.best_type.width()),
            SIRInstruction::Imm(found, SIRValue::new(1u8)),
        ],
        terminator: SIRTerminator::Jump(plan.exit, selected_arguments),
    };

    let not_found = fresh_register(eu, next_register, plan.found_type);
    let no_age = fresh_register(eu, next_register, plan.best_type);
    let mut empty_arguments = vec![no_age; 2];
    empty_arguments[plan.exit_found_position] = not_found;
    empty_arguments[plan.exit_best_position] = no_age;
    let empty = BasicBlock {
        id: empty_block,
        params: Vec::new(),
        instructions: vec![
            SIRInstruction::Imm(not_found, SIRValue::new(0u8)),
            SIRInstruction::Imm(no_age, SIRValue::new(0u8)),
        ],
        terminator: SIRTerminator::Jump(plan.exit, empty_arguments),
    };

    for block in plan.loop_blocks {
        eu.blocks.remove(&block);
    }
    eu.blocks.insert(selected_block, selected);
    eu.blocks.insert(empty_block, empty);
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_traits::ToPrimitive;
    use veryl_analyzer::ir::VarId;

    const LANES: usize = 4;

    fn address(raw: u32) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: STABLE_REGION,
            instance_id: InstanceId(0),
            var_id: VarId::from_raw(raw),
        }
    }

    fn test_pass() -> CircularPriorityPass {
        CircularPriorityPass {
            bit_array_elements: (0..3)
                .map(|raw| (address(raw).absolute_addr(), LANES))
                .collect(),
        }
    }

    struct Builder {
        next: usize,
        types: HashMap<RegisterId, RegisterType>,
    }

    impl Builder {
        fn new() -> Self {
            Self {
                next: 0,
                types: HashMap::default(),
            }
        }

        fn bit(&mut self, width: usize) -> RegisterId {
            let register = RegisterId(self.next);
            self.next += 1;
            self.types.insert(register, unsigned_type(width));
            register
        }

        fn logic(&mut self, width: usize) -> RegisterId {
            let register = RegisterId(self.next);
            self.next += 1;
            self.types.insert(register, RegisterType::Logic { width });
            register
        }

        fn imm(
            &mut self,
            instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
            width: usize,
            value: u64,
        ) -> RegisterId {
            let register = self.bit(width);
            instructions.push(SIRInstruction::Imm(register, SIRValue::new(value)));
            register
        }

        fn logic_imm(
            &mut self,
            instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
            value: u64,
        ) -> RegisterId {
            let register = self.logic(1);
            instructions.push(SIRInstruction::Imm(register, SIRValue::new(value)));
            register
        }

        fn binary(
            &mut self,
            instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
            width: usize,
            lhs: RegisterId,
            operation: BinaryOp,
            rhs: RegisterId,
        ) -> RegisterId {
            let register = self.bit(width);
            instructions.push(SIRInstruction::Binary(register, lhs, operation, rhs));
            register
        }

        fn logic_binary(
            &mut self,
            instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
            lhs: RegisterId,
            operation: BinaryOp,
            rhs: RegisterId,
        ) -> RegisterId {
            let register = self.logic(1);
            instructions.push(SIRInstruction::Binary(register, lhs, operation, rhs));
            register
        }

        fn truth(
            &mut self,
            instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
            source: RegisterId,
        ) -> RegisterId {
            let register = self.bit(1);
            instructions.push(SIRInstruction::Unary(register, UnaryOp::ToTwoState, source));
            register
        }
    }

    fn fixture(index_step: u64, side_effect: bool) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut builder = Builder::new();
        let head = builder.bit(2);

        let mut preheader_instructions = Vec::new();
        let initial_count = builder.imm(&mut preheader_instructions, 3, LANES as u64);
        let initial_index = builder.imm(&mut preheader_instructions, 2, 0);
        let initial_found = builder.logic_imm(&mut preheader_instructions, 0);
        let initial_best = builder.imm(&mut preheader_instructions, 2, 0);
        let one_count = builder.imm(&mut preheader_instructions, 3, 1);
        let one_index = builder.imm(&mut preheader_instructions, 2, index_step);
        let one_found = builder.logic_imm(&mut preheader_instructions, 1);
        let zero_count = builder.imm(&mut preheader_instructions, 3, 0);
        let zero_shift = builder.imm(&mut preheader_instructions, 2, 0);
        let lane_mask = builder.imm(&mut preheader_instructions, 2, 3);
        let preheader = BasicBlock {
            id: BlockId(0),
            params: vec![head],
            instructions: preheader_instructions,
            terminator: SIRTerminator::Jump(
                BlockId(1),
                vec![initial_count, initial_index, initial_found, initial_best],
            ),
        };

        let count = builder.bit(3);
        let index = builder.bit(2);
        let found = builder.logic(1);
        let best = builder.bit(2);
        let mut header_instructions = Vec::new();
        let mut loads = Vec::new();
        for raw in 0..3 {
            let load = builder.logic(1);
            header_instructions.push(SIRInstruction::Load(
                load,
                address(raw),
                SIROffset::Element {
                    index,
                    element_width: 1,
                    bit_offset: 0,
                    dynamic_bit_offset: None,
                },
                1,
            ));
            loads.push(load);
        }
        let either = builder.logic_binary(
            &mut header_instructions,
            loads[1],
            BinaryOp::LogicOr,
            loads[2],
        );
        let predicate = builder.logic_binary(
            &mut header_instructions,
            loads[0],
            BinaryOp::LogicAnd,
            either,
        );
        if side_effect {
            header_instructions.push(SIRInstruction::Store(
                address(9),
                SIROffset::Static(0),
                1,
                predicate,
                Vec::new(),
                Vec::new(),
            ));
        }
        let predicate = builder.truth(&mut header_instructions, predicate);
        let header = BasicBlock {
            id: BlockId(1),
            params: vec![count, index, found, best],
            instructions: header_instructions,
            terminator: SIRTerminator::Branch {
                cond: predicate,
                true_block: (BlockId(2), Vec::new()),
                false_block: (BlockId(4), Vec::new()),
            },
        };

        let mut candidate_instructions = Vec::new();
        let not_found = builder.logic(1);
        candidate_instructions.push(SIRInstruction::Unary(not_found, UnaryOp::LogicNot, found));
        let normalized_index = builder.binary(
            &mut candidate_instructions,
            2,
            index,
            BinaryOp::Shr,
            zero_shift,
        );
        let normalized_index = builder.binary(
            &mut candidate_instructions,
            2,
            normalized_index,
            BinaryOp::And,
            lane_mask,
        );
        let difference = builder.binary(
            &mut candidate_instructions,
            2,
            normalized_index,
            BinaryOp::Sub,
            head,
        );
        let age = builder.binary(
            &mut candidate_instructions,
            2,
            difference,
            BinaryOp::And,
            lane_mask,
        );
        let younger = builder.binary(&mut candidate_instructions, 1, age, BinaryOp::LtU, best);
        let update = builder.logic_binary(
            &mut candidate_instructions,
            not_found,
            BinaryOp::LogicOr,
            younger,
        );
        let update = builder.truth(&mut candidate_instructions, update);
        let candidate = BasicBlock {
            id: BlockId(2),
            params: Vec::new(),
            instructions: candidate_instructions,
            terminator: SIRTerminator::Branch {
                cond: update,
                true_block: (BlockId(3), Vec::new()),
                false_block: (BlockId(4), Vec::new()),
            },
        };
        let update = BasicBlock {
            id: BlockId(3),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(5), vec![one_found, age]),
        };
        let skip = BasicBlock {
            id: BlockId(4),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(5), vec![found, best]),
        };

        let merged_found = builder.logic(1);
        let merged_best = builder.bit(2);
        let mut latch_instructions = Vec::new();
        let next_count =
            builder.binary(&mut latch_instructions, 3, count, BinaryOp::Sub, one_count);
        let keep_looping = builder.binary(
            &mut latch_instructions,
            1,
            next_count,
            BinaryOp::Ne,
            zero_count,
        );
        let next_index =
            builder.binary(&mut latch_instructions, 2, index, BinaryOp::Add, one_index);
        let latch = BasicBlock {
            id: BlockId(5),
            params: vec![merged_found, merged_best],
            instructions: latch_instructions,
            terminator: SIRTerminator::Branch {
                cond: keep_looping,
                true_block: (
                    BlockId(1),
                    vec![next_count, next_index, merged_found, merged_best],
                ),
                false_block: (BlockId(6), vec![merged_found, merged_best]),
            },
        };

        let result_found = builder.logic(1);
        let result_best = builder.bit(2);
        let exit = BasicBlock {
            id: BlockId(6),
            params: vec![result_found, result_best],
            instructions: vec![
                SIRInstruction::Store(
                    address(3),
                    SIROffset::Static(0),
                    1,
                    result_found,
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Store(
                    address(4),
                    SIROffset::Static(0),
                    2,
                    result_best,
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            terminator: SIRTerminator::Return,
        };

        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [preheader, header, candidate, update, skip, latch, exit]
                .into_iter()
                .map(|block| (block.id, block))
                .collect(),
            register_map: builder.types,
        }
    }

    fn execute(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        head: u64,
        valid: u64,
        a: u64,
        b: u64,
    ) -> (u64, u64) {
        let mut registers = HashMap::default();
        registers.insert(eu.blocks[&eu.entry_block_id].params[0], head);
        let mut memory = HashMap::default();
        memory.insert(address(0), valid);
        memory.insert(address(1), a);
        memory.insert(address(2), b);
        let mut block_id = eu.entry_block_id;
        for _ in 0..128 {
            let block = &eu.blocks[&block_id];
            for instruction in &block.instructions {
                match instruction {
                    SIRInstruction::Imm(destination, value) => {
                        registers.insert(*destination, value.payload.to_u64().unwrap_or(0));
                    }
                    SIRInstruction::Binary(destination, lhs, operation, rhs) => {
                        let lhs = registers[lhs];
                        let rhs = registers[rhs];
                        let value = match operation {
                            BinaryOp::Add => lhs.wrapping_add(rhs),
                            BinaryOp::Sub => lhs.wrapping_sub(rhs),
                            BinaryOp::And | BinaryOp::LogicAnd => lhs & rhs,
                            BinaryOp::Or | BinaryOp::LogicOr => lhs | rhs,
                            BinaryOp::Shr => lhs >> rhs,
                            BinaryOp::Ne => u64::from(lhs != rhs),
                            BinaryOp::LtU => u64::from(lhs < rhs),
                            other => panic!("unsupported binary operation {other:?}"),
                        };
                        let width = eu.register_map[destination].width();
                        registers.insert(*destination, value & ((1u64 << width) - 1));
                    }
                    SIRInstruction::Unary(destination, operation, source) => {
                        let source = registers[source];
                        let value = match operation {
                            UnaryOp::ToTwoState | UnaryOp::Ident => source,
                            UnaryOp::LogicNot => u64::from(source == 0),
                            UnaryOp::BitNot => !source,
                            UnaryOp::Or => u64::from(source != 0),
                            UnaryOp::CountTrailingZeros => source.trailing_zeros() as u64,
                            other => panic!("unsupported unary operation {other:?}"),
                        };
                        let width = eu.register_map[destination].width();
                        registers.insert(*destination, value & ((1u64 << width) - 1));
                    }
                    SIRInstruction::Load(destination, address, offset, width) => {
                        let offset = match offset {
                            SIROffset::Static(offset) => *offset,
                            SIROffset::Element { index, .. } => registers[index] as usize,
                            other => panic!("unsupported test offset {other:?}"),
                        };
                        let value = memory.get(address).copied().unwrap_or(0) >> offset;
                        registers.insert(*destination, value & ((1u64 << width) - 1));
                    }
                    SIRInstruction::Store(
                        address,
                        SIROffset::Static(offset),
                        width,
                        source,
                        ..,
                    ) => {
                        let mask = ((1u64 << width) - 1) << offset;
                        let old = memory.get(address).copied().unwrap_or(0);
                        memory.insert(
                            *address,
                            (old & !mask) | ((registers[source] << offset) & mask),
                        );
                    }
                    SIRInstruction::Concat(destination, arguments) => {
                        let value = arguments.iter().fold(0u64, |value, argument| {
                            (value << eu.register_map[argument].width()) | registers[argument]
                        });
                        registers.insert(*destination, value);
                    }
                    SIRInstruction::Slice(destination, source, offset, width) => {
                        registers.insert(
                            *destination,
                            (registers[source] >> offset) & ((1u64 << width) - 1),
                        );
                    }
                    other => panic!("unsupported test instruction {other:?}"),
                }
            }
            let (next, arguments) = match &block.terminator {
                SIRTerminator::Jump(target, arguments) => (*target, arguments),
                SIRTerminator::Branch {
                    cond,
                    true_block,
                    false_block,
                } => {
                    if registers[cond] != 0 {
                        (true_block.0, &true_block.1)
                    } else {
                        (false_block.0, &false_block.1)
                    }
                }
                SIRTerminator::Return => {
                    return (
                        memory.get(&address(3)).copied().unwrap_or(0),
                        memory.get(&address(4)).copied().unwrap_or(0),
                    );
                }
                SIRTerminator::Error(code) => panic!("unexpected error {code}"),
            };
            let values = arguments
                .iter()
                .map(|argument| registers[argument])
                .collect::<Vec<_>>();
            for (&parameter, value) in eu.blocks[&next].params.iter().zip(values) {
                registers.insert(parameter, value);
            }
            block_id = next;
        }
        panic!("test execution did not terminate");
    }

    fn make_lane_mask_escape(unit: &mut ExecutionUnit<RegionedAbsoluteAddr>) -> RegisterId {
        let position = unit.blocks[&BlockId(0)]
            .instructions
            .iter()
            .position(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Imm(register, value)
                        if unit.register_map[register].width() == 2
                            && value.payload.to_u64() == Some(3)
                )
            })
            .unwrap();
        let instruction = unit
            .blocks
            .get_mut(&BlockId(0))
            .unwrap()
            .instructions
            .remove(position);
        let register = def_reg(&instruction).unwrap();
        unit.blocks
            .get_mut(&BlockId(1))
            .unwrap()
            .instructions
            .insert(0, instruction);
        unit.blocks
            .get_mut(&BlockId(6))
            .unwrap()
            .instructions
            .push(SIRInstruction::Store(
                address(5),
                SIROffset::Static(0),
                2,
                register,
                Vec::new(),
                Vec::new(),
            ));
        register
    }

    fn assert_equivalent(
        original: &ExecutionUnit<RegionedAbsoluteAddr>,
        rewritten: &ExecutionUnit<RegionedAbsoluteAddr>,
    ) {
        for head in 0..LANES as u64 {
            for valid in 0..1u64 << LANES {
                for a in 0..1u64 << LANES {
                    for b in 0..1u64 << LANES {
                        assert_eq!(
                            execute(rewritten, head, valid, a, b),
                            execute(original, head, valid, a, b),
                            "head={head} valid={valid:#x} a={a:#x} b={b:#x}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn recovers_packed_circular_priority_and_preserves_all_small_inputs() {
        let mut unit = fixture(1, false);
        unit.verify_result().unwrap();
        let original = unit.clone();
        test_pass().run(&mut unit, &PassOptions::default());
        unit.verify_result().unwrap();

        assert_eq!(unit.blocks.len(), 4);
        assert!(!unit.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Load(_, _, SIROffset::Element { .. }, _)
                )
            })
        }));
        assert_eq!(
            unit.blocks
                .values()
                .flat_map(|block| &block.instructions)
                .filter(|instruction| {
                    matches!(
                        instruction,
                        SIRInstruction::Load(_, _, SIROffset::Static(0), LANES)
                    )
                })
                .count(),
            3
        );
        assert!(unit.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Unary(_, UnaryOp::CountTrailingZeros, _)
                )
            })
        }));

        assert_equivalent(&original, &unit);
    }

    #[test]
    fn hoists_pure_loop_invariants_that_are_reused_after_the_loop() {
        let mut unit = fixture(1, false);
        let lane_mask = make_lane_mask_escape(&mut unit);
        unit.verify_result().unwrap();
        let original = unit.clone();

        test_pass().run(&mut unit, &PassOptions::default());
        unit.verify_result().unwrap();

        assert_eq!(unit.blocks.len(), 4);
        assert!(
            unit.blocks[&BlockId(0)]
                .instructions
                .iter()
                .any(|instruction| def_reg(instruction) == Some(lane_mask))
        );
        assert_equivalent(&original, &unit);
    }

    #[test]
    fn accepts_an_equivalent_unmasked_lane_expression() {
        let mut unit = fixture(1, false);
        let candidate = unit.blocks.get_mut(&BlockId(2)).unwrap();
        let (masked_index, unmasked_index) = match candidate.instructions.remove(2) {
            SIRInstruction::Binary(destination, source, BinaryOp::And, _) => (destination, source),
            other => panic!("unexpected fixture instruction {other:?}"),
        };
        let SIRInstruction::Binary(_, lane, BinaryOp::Sub, _) = &mut candidate.instructions[2]
        else {
            panic!("fixture difference is not a subtraction");
        };
        assert_eq!(*lane, masked_index);
        *lane = unmasked_index;
        unit.verify_result().unwrap();
        let original = unit.clone();

        test_pass().run(&mut unit, &PassOptions::default());
        unit.verify_result().unwrap();

        assert_eq!(unit.blocks.len(), 4);
        assert_equivalent(&original, &unit);
    }

    #[test]
    fn rejects_an_index_too_narrow_to_enumerate_every_lane() {
        let mut unit = fixture(1, false);
        let index = unit.blocks[&BlockId(1)].params[1];
        unit.register_map.insert(index, unsigned_type(1));
        let original = unit.to_string();

        test_pass().run(&mut unit, &PassOptions::default());

        assert_eq!(unit.to_string(), original);
    }

    #[test]
    fn rejects_a_bit_array_shorter_than_the_scan_domain() {
        let mut unit = fixture(1, false);
        let original = unit.to_string();
        let mut pass = test_pass();
        pass.bit_array_elements
            .insert(address(0).absolute_addr(), LANES - 1);

        pass.run(&mut unit, &PassOptions::default());

        assert_eq!(unit.to_string(), original);
    }

    #[test]
    fn rejects_loop_variant_values_reused_after_the_loop() {
        let mut unit = fixture(1, false);
        let index = unit.blocks[&BlockId(1)].params[1];
        unit.blocks
            .get_mut(&BlockId(6))
            .unwrap()
            .instructions
            .push(SIRInstruction::Store(
                address(5),
                SIROffset::Static(0),
                2,
                index,
                Vec::new(),
                Vec::new(),
            ));
        let original = unit.to_string();

        test_pass().run(&mut unit, &PassOptions::default());

        assert_eq!(unit.to_string(), original);
    }

    #[test]
    fn leaves_four_state_mode_unchanged() {
        let mut unit = fixture(1, false);
        let original = unit.to_string();
        test_pass().run(
            &mut unit,
            &PassOptions {
                four_state: true,
                ..PassOptions::default()
            },
        );
        assert_eq!(unit.to_string(), original);
    }

    #[test]
    fn rejects_non_unit_lane_recurrence() {
        let mut unit = fixture(2, false);
        let original = unit.to_string();
        test_pass().run(&mut unit, &PassOptions::default());
        assert_eq!(unit.to_string(), original);
    }

    #[test]
    fn rejects_loop_side_effects() {
        let mut unit = fixture(1, true);
        let original = unit.to_string();
        test_pass().run(&mut unit, &PassOptions::default());
        assert_eq!(unit.to_string(), original);
    }
}
