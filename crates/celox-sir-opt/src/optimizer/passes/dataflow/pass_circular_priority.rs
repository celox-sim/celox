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
use crate::PassOptions;
use crate::ir::cfg::SirCfg;
use crate::ir::*;
use crate::{HashMap, HashSet, OptimizationContext};
use num_bigint::BigUint;

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
    Load {
        address: RegionedAbsoluteAddr,
        unpacked_element_width: Option<usize>,
    },
    Broadcast(RegisterId),
    /// Lanes whose zero-based index is smaller than the scalar bound.
    Prefix(RegisterId),
    /// Compare every logical element field with one loop-invariant scalar and
    /// return one compact predicate bit per lane.
    LaneEq {
        address: RegionedAbsoluteAddr,
        element_width: usize,
        bit_offset: usize,
        field_width: usize,
        value: RegisterId,
    },
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

#[derive(Clone, Debug)]
struct LinearBitmapScanPlan {
    preheader: BlockId,
    loop_blocks: Vec<BlockId>,
    exit: BlockId,
    exit_count_position: usize,
    exit_best_position: usize,
    exit_found_position: usize,
    exit_arguments: Vec<RegisterId>,
    lanes: usize,
    count_type: RegisterType,
    best_type: RegisterType,
    found_type: RegisterType,
    no_match_best: RegisterId,
    no_match_found: RegisterId,
    predicate: PackedExpression,
}

#[derive(Clone, Debug)]
struct PayloadField {
    exit_position: usize,
    address: RegionedAbsoluteAddr,
    element_width: usize,
    bit_offset: usize,
    width: usize,
    value_type: RegisterType,
    no_match: RegisterId,
}

#[derive(Clone, Debug)]
struct LastPayloadScanPlan {
    preheader: BlockId,
    loop_blocks: Vec<BlockId>,
    hoisted_instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    exit: BlockId,
    exit_arguments: Vec<RegisterId>,
    exit_found_position: usize,
    found_type: RegisterType,
    no_match_found: RegisterId,
    lanes: usize,
    predicate: PackedExpression,
    payloads: Vec<PayloadField>,
}

#[derive(Clone, Debug)]
struct SparseBitmapLoopPlan {
    preheader: BlockId,
    header: BlockId,
    exit: BlockId,
    lanes: usize,
    count_position: usize,
    index_position: usize,
    entry_arguments: Vec<RegisterId>,
    backedge_arguments: Vec<RegisterId>,
    exit_arguments: Vec<RegisterId>,
    bypass_arguments: Vec<RegisterId>,
    common_predicate: RegisterId,
    predicate: PackedExpression,
}

#[derive(Clone, Debug)]
struct BitMapLoopPlan {
    preheader: BlockId,
    header: BlockId,
    exit: BlockId,
    lanes: usize,
    accumulator_type: RegisterType,
    initial_accumulator: RegisterId,
    index: RegisterId,
    bit: RegisterId,
    dependency_indices: Vec<usize>,
    exit_arguments: Vec<RegisterId>,
    exit_result_position: usize,
}

#[derive(Clone, Copy, Debug)]
struct ArrayShape {
    element_width: usize,
    element_count: usize,
}

#[derive(Clone, Default)]
pub(in crate::optimizer) struct CircularPriorityPass {
    bit_array_elements: HashMap<AbsoluteAddr, usize>,
    array_shapes: HashMap<AbsoluteAddr, ArrayShape>,
}

impl CircularPriorityPass {
    pub(in crate::optimizer) fn for_program(program: &OptimizationContext) -> Self {
        let mut bit_array_elements = HashMap::default();
        let mut array_shapes = HashMap::default();
        for (&address, info) in &program.design.state_objects {
            let element_count = if info.array_dims.is_empty() {
                info.width
            } else {
                let Some(element_count) = info
                    .array_dims
                    .iter()
                    .try_fold(1usize, |count, &dimension| count.checked_mul(dimension))
                else {
                    continue;
                };
                if info.width != element_count {
                    let Some(element_width) = info.width.checked_div(element_count) else {
                        continue;
                    };
                    if element_width == 0 || element_width * element_count != info.width {
                        continue;
                    }
                    array_shapes.insert(
                        address,
                        ArrayShape {
                            element_width,
                            element_count,
                        },
                    );
                    continue;
                }
                element_count
            };
            if element_count == 0 {
                continue;
            }
            bit_array_elements.insert(address, element_count);
            if !info.array_dims.is_empty() {
                array_shapes.insert(
                    address,
                    ArrayShape {
                        element_width: 1,
                        element_count,
                    },
                );
            }
        }
        Self {
            bit_array_elements,
            array_shapes,
        }
    }
}

impl ExecutionUnitPass for CircularPriorityPass {
    fn name(&self) -> &'static str {
        "circular_priority"
    }

    fn handles_calls(&self) -> bool {
        true
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        if options.four_state || self.bit_array_elements.is_empty() && self.array_shapes.is_empty()
        {
            return;
        }
        let Ok(cfg) = SirCfg::analyze(eu) else {
            return;
        };
        let definitions = collect_definitions(eu);
        let use_blocks = collect_use_blocks(eu);
        let mut constant_cache = HashMap::default();
        let mut plans = Vec::new();
        let mut linear_plans = Vec::new();
        let mut payload_plans = Vec::new();
        let mut sparse_plans = Vec::new();
        let mut bit_map_plans = Vec::new();

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
                &self.array_shapes,
            ) {
                plans.push(plan);
            }
            if let Some(plan) = recognize_linear_bitmap_scan(
                eu,
                &cfg,
                cfg.block_ids[natural_loop.header],
                &loop_blocks,
                &definitions,
                &use_blocks,
                &mut constant_cache,
                &self.bit_array_elements,
                &self.array_shapes,
            ) {
                linear_plans.push(plan);
            }
            if let Some(plan) = recognize_last_payload_scan(
                eu,
                &cfg,
                cfg.block_ids[natural_loop.header],
                &loop_blocks,
                &definitions,
                &use_blocks,
                &mut constant_cache,
                &self.bit_array_elements,
                &self.array_shapes,
            ) {
                payload_plans.push(plan);
            }
            if let Some(plan) = recognize_sparse_bitmap_loop(
                eu,
                &cfg,
                cfg.block_ids[natural_loop.header],
                &loop_blocks,
                &definitions,
                &use_blocks,
                &mut constant_cache,
                &self.bit_array_elements,
                &self.array_shapes,
            ) {
                sparse_plans.push(plan);
            }
            if let Some(plan) = recognize_bit_map_loop(
                eu,
                &cfg,
                cfg.block_ids[natural_loop.header],
                &loop_blocks,
                &definitions,
                &use_blocks,
            ) {
                bit_map_plans.push(plan);
            }
        }

        if plans.is_empty()
            && linear_plans.is_empty()
            && payload_plans.is_empty()
            && sparse_plans.is_empty()
            && bit_map_plans.is_empty()
        {
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
        linear_plans.sort_unstable_by_key(|plan| plan.preheader);
        linear_plans.retain(|plan| {
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
        payload_plans.sort_unstable_by_key(|plan| plan.preheader);
        payload_plans.retain(|plan| {
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
        sparse_plans.sort_unstable_by_key(|plan| plan.preheader);
        sparse_plans.retain(|plan| occupied.insert(plan.header));
        bit_map_plans.sort_unstable_by_key(|plan| plan.preheader);
        bit_map_plans.retain(|plan| occupied.insert(plan.header));
        prepare_escaping_definitions(eu, &cfg, &definitions, &mut plans);
        if (plans.is_empty()
            && linear_plans.is_empty()
            && payload_plans.is_empty()
            && sparse_plans.is_empty()
            && bit_map_plans.is_empty())
            || !ids_available(eu, &plans, &linear_plans, &payload_plans, &sparse_plans)
        {
            return;
        }

        let mut next_register = eu.register_map.keys().map(|reg| reg.0).max().unwrap_or(0);
        let mut next_block = eu.blocks.keys().map(|block| block.0).max().unwrap_or(0);
        for plan in plans {
            apply_plan(eu, plan, &mut next_register, &mut next_block);
        }
        for plan in linear_plans {
            apply_linear_bitmap_scan(eu, plan, &mut next_register);
        }
        for plan in payload_plans {
            apply_last_payload_scan(eu, plan, &mut next_register, &mut next_block);
        }
        for plan in sparse_plans {
            apply_sparse_bitmap_loop(eu, plan, &mut next_register);
        }
        for plan in bit_map_plans {
            apply_bit_map_loop(eu, plan, &mut next_register);
        }
        remove_dead_definitions(eu);
    }
}

/// Recover fixed lane-to-bit maps after native EU merging and jump inlining.
///
/// This entry point deliberately needs no `OptimizationContext` metadata: unlike the
/// broader circular-priority patterns, the bit-map proof is entirely in the
/// merged CFG and SSA recurrence. It runs once at the final native SIR
/// boundary, where branch-expanded predecessors have been inlined.
pub(in crate::optimizer) fn recover_native_fixed_bit_map_loops(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
) -> usize {
    let Ok(cfg) = SirCfg::analyze(eu) else {
        return 0;
    };
    let definitions = collect_definitions(eu);
    let use_blocks = collect_use_blocks(eu);
    let mut planned_headers = HashSet::default();
    let mut plans = Vec::new();
    for natural_loop in &cfg.loops {
        let header = cfg.block_ids[natural_loop.header];
        if !planned_headers.insert(header) {
            continue;
        }
        let loop_blocks = natural_loop
            .blocks
            .iter()
            .map(|&block| cfg.block_ids[block])
            .collect::<HashSet<_>>();
        if let Some(plan) =
            recognize_bit_map_loop(eu, &cfg, header, &loop_blocks, &definitions, &use_blocks)
        {
            plans.push(plan);
        }
    }
    plans.sort_unstable_by_key(|plan| plan.preheader);
    let plan_count = plans.len();
    if plan_count == 0 {
        return 0;
    }
    let mut next_register = eu.register_map.keys().map(|reg| reg.0).max().unwrap_or(0);
    for plan in plans {
        apply_bit_map_loop(eu, plan, &mut next_register);
    }
    remove_dead_definitions(eu);
    plan_count
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

fn collect_use_blocks(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, HashSet<BlockId>> {
    let mut result = HashMap::<RegisterId, HashSet<BlockId>>::default();
    let mut uses = Vec::new();
    for (&block_id, block) in &eu.blocks {
        for instruction in &block.instructions {
            uses.clear();
            instruction_uses(instruction, &mut uses);
            for &value in &uses {
                result.entry(value).or_default().insert(block_id);
            }
        }
        uses.clear();
        terminator_uses(&block.terminator, &mut uses);
        for &value in &uses {
            result.entry(value).or_default().insert(block_id);
        }
    }
    result
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

/// Recognize a fixed-trip scan which counts a packed predicate and remembers
/// its first matching lane.  The source CFG deliberately remains the proof:
/// no source-order or scheduler-order assumption is used.
#[allow(clippy::too_many_arguments)]
fn recognize_linear_bitmap_scan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    header: BlockId,
    loop_blocks: &HashSet<BlockId>,
    definitions: &HashMap<RegisterId, Definition>,
    use_blocks: &HashMap<RegisterId, HashSet<BlockId>>,
    constant_cache: &mut HashMap<RegisterId, Option<bool>>,
    bit_array_elements: &HashMap<AbsoluteAddr, usize>,
    array_shapes: &HashMap<AbsoluteAddr, ArrayShape>,
) -> Option<LinearBitmapScanPlan> {
    if loop_blocks.len() != 4 || !loop_is_pure(eu, loop_blocks) {
        return None;
    }
    if loop_blocks.iter().any(|block| {
        eu.blocks[block]
            .params
            .iter()
            .copied()
            .chain(eu.blocks[block].instructions.iter().filter_map(def_reg))
            .any(|value| {
                use_blocks
                    .get(&value)
                    .is_some_and(|users| users.iter().any(|user| !loop_blocks.contains(user)))
            })
    }) {
        return None;
    }
    let header_index = cfg.block_index(header)?;
    let outside_predecessors = cfg.predecessors[header_index]
        .iter()
        .map(|&block| cfg.block_ids[block])
        .filter(|block| !loop_blocks.contains(block))
        .collect::<Vec<_>>();
    let [preheader] = outside_predecessors.as_slice() else {
        return None;
    };
    let preheader = *preheader;
    let header_block = &eu.blocks[&header];
    let SIRTerminator::Jump(target, entry_arguments) = &eu.blocks[&preheader].terminator else {
        return None;
    };
    if *target != header || entry_arguments.len() != header_block.params.len() {
        return None;
    }

    let (header_condition, first_arm, carry_arm) = branch_targets(&header_block.terminator)?;
    let first_jump = edge_arguments(
        &eu.blocks[&first_arm].terminator,
        common_jump_target(eu, first_arm)?,
    )?;
    let body = common_jump_target(eu, first_arm)?;
    if common_jump_target(eu, carry_arm)? != body || !loop_blocks.contains(&body) {
        return None;
    }
    let carry_jump = edge_arguments(&eu.blocks[&carry_arm].terminator, body)?;
    let body_block = &eu.blocks[&body];
    if body_block.params.len() != 2 || first_jump.len() != 2 || carry_jump.len() != 2 {
        return None;
    }

    let (latch_condition, latch_true, latch_false) = branch_targets(&body_block.terminator)?;
    let (exit, loop_when_true) = if latch_true == header && !loop_blocks.contains(&latch_false) {
        (latch_false, true)
    } else if latch_false == header && !loop_blocks.contains(&latch_true) {
        (latch_true, false)
    } else {
        return None;
    };
    let backedge_arguments = edge_arguments(&body_block.terminator, header)?;
    let exit_arguments = edge_arguments(&body_block.terminator, exit)?;
    if backedge_arguments.len() != header_block.params.len()
        || exit_arguments.len() != eu.blocks[&exit].params.len()
    {
        return None;
    }
    let (trip_position, lanes) = match_count_recurrence(
        eu,
        definitions,
        header_block,
        entry_arguments,
        backedge_arguments,
        latch_condition,
        loop_when_true,
    )?;
    if lanes < 4 {
        return None;
    }
    let index_position = match_index_recurrence(
        eu,
        definitions,
        header_block,
        entry_arguments,
        backedge_arguments,
        trip_position,
    )?;
    let index = header_block.params[index_position];

    // The header chooses `(zero_extend(index), true)` until `found`, then
    // carries `(best, found)`.  Normalize an inverted branch orientation.
    let mut found_position = None;
    let mut best_position = None;
    for (position, (&parameter, &entry_argument)) in
        header_block.params.iter().zip(entry_arguments).enumerate()
    {
        if position == trip_position || position == index_position {
            continue;
        }
        if immediate(eu, definitions, entry_argument) == Some(0)
            && matches_not(eu, definitions, header_condition, parameter)
        {
            found_position = Some(position);
        }
    }
    let found_position = found_position?;
    let found = header_block.params[found_position];
    for position in 0..header_block.params.len() {
        if position == trip_position || position == index_position || position == found_position {
            continue;
        }
        let best = header_block.params[position];
        let first_matches = is_zero_extended_index(eu, definitions, first_jump[0], index)
            && immediate(eu, definitions, first_jump[1]) == Some(1)
            && carry_jump == [best, found];
        if first_matches {
            best_position = Some(position);
            break;
        }
    }
    let best_position = best_position?;
    let best = header_block.params[best_position];

    let mut count_position = None;
    let mut predicate = None;
    let mut count_update = None;
    let mut best_update = None;
    let mut found_update = None;
    for position in 0..header_block.params.len() {
        if position == trip_position || position == index_position {
            continue;
        }
        let current = header_block.params[position];
        let update = backedge_arguments[position];
        let Some(SIRInstruction::Mux(_, condition, on_true, on_false)) =
            instruction(eu, definitions, update)
        else {
            continue;
        };
        if *on_false != current {
            continue;
        }
        if position == best_position && *on_true == body_block.params[0] {
            predicate.get_or_insert(*condition);
            if predicate != Some(*condition) {
                return None;
            }
            best_update = Some(update);
        } else if position == found_position && *on_true == body_block.params[1] {
            predicate.get_or_insert(*condition);
            if predicate != Some(*condition) {
                return None;
            }
            found_update = Some(update);
        } else if immediate(eu, definitions, entry_arguments[position]) == Some(0)
            && is_add_one(eu, definitions, *on_true, current)
        {
            predicate.get_or_insert(*condition);
            if predicate != Some(*condition) || count_position.replace(position).is_some() {
                return None;
            }
            count_update = Some(update);
        }
    }
    let count_position = count_position?;
    let predicate_register = predicate?;
    let count_update = count_update?;
    let best_update = best_update?;
    let found_update = found_update?;
    let exit_count_position = exit_arguments
        .iter()
        .position(|&value| value == count_update)?;
    let exit_best_position = exit_arguments
        .iter()
        .position(|&value| value == best_update)?;
    let exit_found_position = exit_arguments
        .iter()
        .position(|&value| value == found_update)?;
    if [exit_count_position, exit_best_position, exit_found_position]
        .into_iter()
        .collect::<HashSet<_>>()
        .len()
        != 3
    {
        return None;
    }

    let predicate = build_packed_expression(
        eu,
        cfg,
        definitions,
        constant_cache,
        loop_blocks,
        preheader,
        index,
        predicate_register,
        false,
        lanes,
        bit_array_elements,
        array_shapes,
        &HashSet::default(),
    )?;
    if predicate.dynamic_loads == 0 {
        return None;
    }

    let count_type = eu.register_map[&header_block.params[count_position]].clone();
    let best_type = eu.register_map[&best].clone();
    let found_type = eu.register_map[&found].clone();
    let bit_count_width = UnaryOp::PopCount.result_width(lanes);
    if count_type.width() != bit_count_width
        || best_type.width() < bit_count_width
        || found_type.width() != 1
    {
        return None;
    }

    let mut loop_blocks = loop_blocks.iter().copied().collect::<Vec<_>>();
    loop_blocks.sort_unstable();
    Some(LinearBitmapScanPlan {
        preheader,
        loop_blocks,
        exit,
        exit_count_position,
        exit_best_position,
        exit_found_position,
        exit_arguments: exit_arguments.to_vec(),
        lanes,
        count_type,
        best_type,
        found_type,
        no_match_best: entry_arguments[best_position],
        no_match_found: entry_arguments[found_position],
        predicate,
    })
}

/// Recognize an ascending fixed-trip search which retains the payload from
/// every matching lane.  Since each match overwrites the previous payload,
/// the final state is exactly the payload of the highest set predicate bit.
#[allow(clippy::too_many_arguments)]
fn recognize_last_payload_scan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    header: BlockId,
    loop_blocks: &HashSet<BlockId>,
    definitions: &HashMap<RegisterId, Definition>,
    use_blocks: &HashMap<RegisterId, HashSet<BlockId>>,
    constant_cache: &mut HashMap<RegisterId, Option<bool>>,
    bit_array_elements: &HashMap<AbsoluteAddr, usize>,
    array_shapes: &HashMap<AbsoluteAddr, ArrayShape>,
) -> Option<LastPayloadScanPlan> {
    if loop_blocks.len() != 4 || !loop_is_pure(eu, loop_blocks) {
        return None;
    }
    let header_index = cfg.block_index(header)?;
    let outside_predecessors = cfg.predecessors[header_index]
        .iter()
        .map(|&block| cfg.block_ids[block])
        .filter(|block| !loop_blocks.contains(block))
        .collect::<Vec<_>>();
    let [preheader] = outside_predecessors.as_slice() else {
        return None;
    };
    let preheader = *preheader;
    let escaping_roots = loop_blocks
        .iter()
        .flat_map(|block| {
            eu.blocks[block]
                .params
                .iter()
                .copied()
                .chain(eu.blocks[block].instructions.iter().filter_map(def_reg))
        })
        .filter(|value| {
            use_blocks
                .get(value)
                .is_some_and(|users| users.iter().any(|user| !loop_blocks.contains(user)))
        })
        .collect::<HashSet<_>>();
    let hoisted_instructions = hoist_escaping_definitions(
        eu,
        cfg,
        preheader,
        loop_blocks,
        definitions,
        &escaping_roots,
    )?;
    let header_block = &eu.blocks[&header];
    let SIRTerminator::Jump(target, entry_arguments) = &eu.blocks[&preheader].terminator else {
        return None;
    };
    if *target != header || entry_arguments.len() != header_block.params.len() {
        return None;
    }
    let (predicate_register, selected_arm, carry_arm) = branch_targets(&header_block.terminator)?;
    let latch = common_jump_target(eu, selected_arm)?;
    if common_jump_target(eu, carry_arm)? != latch || !loop_blocks.contains(&latch) {
        return None;
    }
    let selected_arguments = edge_arguments(&eu.blocks[&selected_arm].terminator, latch)?;
    let carry_arguments = edge_arguments(&eu.blocks[&carry_arm].terminator, latch)?;
    let latch_block = &eu.blocks[&latch];
    if selected_arguments.len() != latch_block.params.len()
        || carry_arguments.len() != latch_block.params.len()
        || latch_block.params.is_empty()
    {
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
    {
        return None;
    }
    let (trip_position, lanes) = match_count_recurrence(
        eu,
        definitions,
        header_block,
        entry_arguments,
        backedge_arguments,
        latch_condition,
        loop_when_true,
    )?;
    if lanes < 4 {
        return None;
    }
    let index_position = match_index_recurrence(
        eu,
        definitions,
        header_block,
        entry_arguments,
        backedge_arguments,
        trip_position,
    )?;
    let index = header_block.params[index_position];

    let found_position = (0..header_block.params.len()).find(|&position| {
        if position == trip_position || position == index_position {
            return false;
        }
        let current = header_block.params[position];
        matches!(
            instruction(eu, definitions, backedge_arguments[position]),
            Some(SIRInstruction::Mux(_, condition, on_true, on_false))
                if *condition == predicate_register
                    && immediate(eu, definitions, *on_true) == Some(1)
                    && *on_false == current
                    && immediate(eu, definitions, entry_arguments[position]) == Some(0)
        )
    })?;
    let found_update = backedge_arguments[found_position];
    let exit_found_position = exit_arguments
        .iter()
        .position(|&argument| argument == found_update)?;

    let mut mapped_positions = HashSet::default();
    mapped_positions.extend([trip_position, index_position, found_position]);
    let mut used_exit_positions = HashSet::default();
    used_exit_positions.insert(exit_found_position);
    let mut payloads = Vec::with_capacity(latch_block.params.len());
    for (merge_position, &merge_parameter) in latch_block.params.iter().enumerate() {
        let header_position = (0..header_block.params.len()).find(|&position| {
            !mapped_positions.contains(&position)
                && backedge_arguments[position] == merge_parameter
                && carry_arguments[merge_position] == header_block.params[position]
        })?;
        let selected = selected_arguments[merge_position];
        let SIRInstruction::Load(
            _,
            address,
            SIROffset::Element {
                index: load_index,
                element_width,
                bit_offset,
                dynamic_bit_offset: None,
            },
            width,
        ) = instruction(eu, definitions, selected)?
        else {
            return None;
        };
        let shape = array_shapes.get(&address.absolute_addr())?;
        if *load_index != index
            || shape.element_width != *element_width
            || shape.element_count < lanes
            || *width == 0
            || bit_offset.checked_add(*width)? > *element_width
        {
            return None;
        }
        let exit_position = exit_arguments
            .iter()
            .position(|&argument| argument == merge_parameter)?;
        if !used_exit_positions.insert(exit_position) {
            return None;
        }
        let value_type = eu.register_map.get(&selected)?.clone();
        if eu.register_map.get(&header_block.params[header_position]) != Some(&value_type)
            || eu.register_map.get(&eu.blocks[&exit].params[exit_position]) != Some(&value_type)
        {
            return None;
        }
        mapped_positions.insert(header_position);
        payloads.push(PayloadField {
            exit_position,
            address: *address,
            element_width: *element_width,
            bit_offset: *bit_offset,
            width: *width,
            value_type,
            no_match: entry_arguments[header_position],
        });
    }
    if mapped_positions.len() != header_block.params.len() {
        return None;
    }

    let hoisted_values = hoisted_instructions
        .iter()
        .filter_map(def_reg)
        .collect::<HashSet<_>>();
    let predicate = build_packed_expression(
        eu,
        cfg,
        definitions,
        constant_cache,
        loop_blocks,
        preheader,
        index,
        predicate_register,
        false,
        lanes,
        bit_array_elements,
        array_shapes,
        &hoisted_values,
    )?;
    if predicate.dynamic_loads == 0
        || eu.register_map[&header_block.params[found_position]].width() != 1
    {
        return None;
    }
    let mut loop_blocks = loop_blocks.iter().copied().collect::<Vec<_>>();
    loop_blocks.sort_unstable();
    Some(LastPayloadScanPlan {
        preheader,
        loop_blocks,
        hoisted_instructions,
        exit,
        exit_arguments: exit_arguments.to_vec(),
        exit_found_position,
        found_type: eu.register_map[&header_block.params[found_position]].clone(),
        no_match_found: entry_arguments[found_position],
        lanes,
        predicate,
        payloads,
    })
}

fn conjunction_nodes(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    root: RegisterId,
) -> HashSet<RegisterId> {
    let mut result = HashSet::default();
    let mut stack = vec![root];
    while let Some(value) = stack.pop() {
        if !result.insert(value) {
            continue;
        }
        match instruction(eu, definitions, value) {
            Some(SIRInstruction::Unary(
                _,
                UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Or,
                source,
            )) if eu.register_map.get(source).map(RegisterType::width) == Some(1) => {
                stack.push(*source);
            }
            Some(SIRInstruction::Binary(_, lhs, BinaryOp::LogicAnd | BinaryOp::And, rhs)) => {
                stack.extend([*lhs, *rhs]);
            }
            _ => {}
        }
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn recognize_sparse_bitmap_loop(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    header: BlockId,
    loop_blocks: &HashSet<BlockId>,
    definitions: &HashMap<RegisterId, Definition>,
    use_blocks: &HashMap<RegisterId, HashSet<BlockId>>,
    constant_cache: &mut HashMap<RegisterId, Option<bool>>,
    bit_array_elements: &HashMap<AbsoluteAddr, usize>,
    array_shapes: &HashMap<AbsoluteAddr, ArrayShape>,
) -> Option<SparseBitmapLoopPlan> {
    if loop_blocks.len() != 1 || !loop_is_pure(eu, loop_blocks) {
        return None;
    }
    let header_block = &eu.blocks[&header];
    // The rewrite lets the preheader branch straight to the exit, so the loop
    // no longer dominates it: values defined in the loop may leave it only as
    // exit arguments.
    if header_block
        .params
        .iter()
        .copied()
        .chain(header_block.instructions.iter().filter_map(def_reg))
        .any(|value| {
            use_blocks
                .get(&value)
                .is_some_and(|users| users.iter().any(|user| !loop_blocks.contains(user)))
        })
    {
        return None;
    }
    let header_index = cfg.block_index(header)?;
    let outside = cfg.predecessors[header_index]
        .iter()
        .map(|&index| cfg.block_ids[index])
        .filter(|block| *block != header)
        .collect::<Vec<_>>();
    let [preheader] = outside.as_slice() else {
        return None;
    };
    let preheader = *preheader;
    let SIRTerminator::Jump(target, entry_arguments) = &eu.blocks[&preheader].terminator else {
        return None;
    };
    if *target != header || entry_arguments.len() != header_block.params.len() {
        return None;
    }
    let (condition, true_target, false_target) = branch_targets(&header_block.terminator)?;
    let (exit, loop_when_true) = if true_target == header && false_target != header {
        (false_target, true)
    } else if false_target == header && true_target != header {
        (true_target, false)
    } else {
        return None;
    };
    let backedge_arguments = edge_arguments(&header_block.terminator, header)?;
    let exit_arguments = edge_arguments(&header_block.terminator, exit)?;
    if backedge_arguments.len() != header_block.params.len()
        || exit_arguments.len() != eu.blocks[&exit].params.len()
    {
        return None;
    }
    let (count_position, lanes) = match_count_recurrence(
        eu,
        definitions,
        header_block,
        entry_arguments,
        backedge_arguments,
        condition,
        loop_when_true,
    )?;
    if !(16..=64).contains(&lanes) || !lanes.is_power_of_two() {
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
    let index = header_block.params[index_position];

    let mut update_conditions = Vec::new();
    let mut bypass_arguments = exit_arguments.to_vec();
    for position in 0..header_block.params.len() {
        if position == count_position || position == index_position {
            continue;
        }
        let current = header_block.params[position];
        let update = backedge_arguments[position];
        let SIRInstruction::Mux(_, update_condition, on_true, on_false) =
            instruction(eu, definitions, update)?
        else {
            return None;
        };
        if *on_false != current
            || immediate(eu, definitions, *on_true) != Some(1)
            || eu.register_map.get(&current).map(RegisterType::width) != Some(1)
        {
            return None;
        }
        update_conditions.push(*update_condition);
        let exit_position = exit_arguments
            .iter()
            .position(|&argument| argument == update)?;
        bypass_arguments[exit_position] = entry_arguments[position];
    }
    if update_conditions.is_empty()
        || bypass_arguments
            .iter()
            .zip(exit_arguments)
            .any(|(&bypass, &normal)| bypass == normal)
    {
        return None;
    }

    let mut common = conjunction_nodes(eu, definitions, update_conditions[0]);
    for &update_condition in &update_conditions[1..] {
        let nodes = conjunction_nodes(eu, definitions, update_condition);
        common.retain(|node| nodes.contains(node));
    }
    let mut best = None;
    for candidate in common {
        if !loop_blocks.contains(&definitions.get(&candidate)?.block()) {
            continue;
        }
        let Some(predicate) = build_packed_expression(
            eu,
            cfg,
            definitions,
            constant_cache,
            loop_blocks,
            preheader,
            index,
            candidate,
            false,
            lanes,
            bit_array_elements,
            array_shapes,
            &HashSet::default(),
        ) else {
            continue;
        };
        if predicate.dynamic_loads == 0 {
            continue;
        }
        let score = (predicate.dynamic_loads, predicate.value_ops);
        if best
            .as_ref()
            .is_none_or(|(_, best_predicate): &(RegisterId, PackedExpression)| {
                score > (best_predicate.dynamic_loads, best_predicate.value_ops)
            })
        {
            best = Some((candidate, predicate));
        }
    }
    let (common_predicate, predicate) = best?;
    Some(SparseBitmapLoopPlan {
        preheader,
        header,
        exit,
        lanes,
        count_position,
        index_position,
        entry_arguments: entry_arguments.to_vec(),
        backedge_arguments: backedge_arguments.to_vec(),
        exit_arguments: exit_arguments.to_vec(),
        bypass_arguments,
        common_predicate,
        predicate,
    })
}

fn binary_definition(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    value: RegisterId,
    op: BinaryOp,
) -> Option<(RegisterId, RegisterId)> {
    let SIRInstruction::Binary(_, lhs, actual, rhs) = instruction(eu, definitions, value)? else {
        return None;
    };
    (*actual == op).then_some((*lhs, *rhs))
}

fn commutative_operand(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    value: RegisterId,
    op: BinaryOp,
    operand: RegisterId,
) -> Option<RegisterId> {
    let (lhs, rhs) = binary_definition(eu, definitions, value, op)?;
    if lhs == operand {
        Some(rhs)
    } else if rhs == operand {
        Some(lhs)
    } else {
        None
    }
}

fn match_inserted_bit(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    current: RegisterId,
    update: RegisterId,
) -> Option<(RegisterId, RegisterId)> {
    let (outer_lhs, outer_rhs) = binary_definition(eu, definitions, update, BinaryOp::Or)?;
    for (cleared, inserted) in [(outer_lhs, outer_rhs), (outer_rhs, outer_lhs)] {
        let (clear_lhs, clear_rhs) = binary_definition(eu, definitions, cleared, BinaryOp::And)?;
        let inverted = if clear_lhs == current {
            clear_rhs
        } else if clear_rhs == current {
            clear_lhs
        } else {
            continue;
        };
        let SIRInstruction::Unary(_, UnaryOp::BitNot, onehot) =
            instruction(eu, definitions, inverted)?
        else {
            continue;
        };
        let shifted = commutative_operand(eu, definitions, inserted, BinaryOp::And, *onehot)?;
        let (one, shift) = binary_definition(eu, definitions, *onehot, BinaryOp::Shl)?;
        if immediate(eu, definitions, one) != Some(1) {
            continue;
        }
        let (extended, inserted_shift) =
            binary_definition(eu, definitions, shifted, BinaryOp::Shl)?;
        if inserted_shift != shift {
            continue;
        }
        let bit = if eu.register_map.get(&extended).map(RegisterType::width) == Some(1) {
            extended
        } else {
            let SIRInstruction::Concat(_, parts) = instruction(eu, definitions, extended)? else {
                continue;
            };
            let (&bit, padding) = parts.split_last()?;
            if eu.register_map.get(&bit).map(RegisterType::width) != Some(1)
                || !padding
                    .iter()
                    .all(|&part| immediate(eu, definitions, part) == Some(0))
            {
                continue;
            }
            bit
        };
        return Some((shift, bit));
    }
    None
}

fn shift_is_index(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    shift: RegisterId,
    index: RegisterId,
) -> bool {
    if shift == index || is_zero_extended_index(eu, definitions, shift, index) {
        return true;
    }
    binary_definition(eu, definitions, shift, BinaryOp::Mul).is_some_and(|(lhs, rhs)| {
        (lhs == index && immediate(eu, definitions, rhs) == Some(1))
            || (rhs == index && immediate(eu, definitions, lhs) == Some(1))
    })
}

fn collect_bit_dependencies(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    definitions: &HashMap<RegisterId, Definition>,
    header: BlockId,
    preheader: BlockId,
    root: RegisterId,
    allowed_parameter: RegisterId,
    forbidden: &[RegisterId],
) -> Option<Vec<usize>> {
    let mut seen = HashSet::default();
    let mut stack = vec![root];
    while let Some(value) = stack.pop() {
        if forbidden.contains(&value) {
            return None;
        }
        if !seen.insert(value) {
            continue;
        }
        let definition = *definitions.get(&value)?;
        if definition.block() != header {
            if !cfg.dominates(definition.block(), preheader) {
                return None;
            }
            continue;
        }
        if value == allowed_parameter && matches!(definition, Definition::Parameter { .. }) {
            continue;
        }
        let Definition::Instruction { index, .. } = definition else {
            return None;
        };
        let instruction = &eu.blocks[&header].instructions[index];
        if instruction.has_side_effects() {
            return None;
        }
        let mut operands = Vec::new();
        instruction_uses(instruction, &mut operands);
        stack.extend(operands);
    }
    let mut indices = seen
        .into_iter()
        .filter_map(|value| match definitions.get(&value) {
            Some(Definition::Instruction { block, index }) if *block == header => Some(*index),
            _ => None,
        })
        .collect::<Vec<_>>();
    indices.sort_unstable();
    Some(indices)
}

fn recognize_bit_map_loop(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    header: BlockId,
    loop_blocks: &HashSet<BlockId>,
    definitions: &HashMap<RegisterId, Definition>,
    use_blocks: &HashMap<RegisterId, HashSet<BlockId>>,
) -> Option<BitMapLoopPlan> {
    let block = &eu.blocks[&header];
    if block.params.len() != 3 {
        return None;
    }
    if block
        .params
        .iter()
        .copied()
        .chain(block.instructions.iter().filter_map(def_reg))
        .any(|value| {
            use_blocks
                .get(&value)
                .is_some_and(|users| users.iter().any(|user| !loop_blocks.contains(user)))
        })
    {
        return None;
    }
    if block
        .instructions
        .iter()
        .any(|instruction| instruction.has_side_effects())
    {
        return None;
    }
    let header_index = cfg.block_index(header)?;
    let outside = cfg.predecessors[header_index]
        .iter()
        .map(|&index| cfg.block_ids[index])
        .filter(|block| *block != header)
        .collect::<Vec<_>>();
    let [preheader] = outside.as_slice() else {
        return None;
    };
    let preheader = *preheader;
    let SIRTerminator::Jump(target, entry_arguments) = &eu.blocks[&preheader].terminator else {
        return None;
    };
    if *target != header || entry_arguments.len() != block.params.len() {
        return None;
    }
    let (condition, true_target, false_target) = branch_targets(&block.terminator)?;
    let (exit, loop_when_true) = if true_target == header && false_target != header {
        (false_target, true)
    } else if false_target == header && true_target != header {
        (true_target, false)
    } else {
        return None;
    };
    let backedge = edge_arguments(&block.terminator, header)?;
    let exit_arguments = edge_arguments(&block.terminator, exit)?;
    let (count_position, lanes) = match_count_recurrence(
        eu,
        definitions,
        block,
        entry_arguments,
        backedge,
        condition,
        loop_when_true,
    )?;
    if lanes != 16 {
        return None;
    }
    let index_position = match_index_recurrence(
        eu,
        definitions,
        block,
        entry_arguments,
        backedge,
        count_position,
    )?;
    let accumulator_position = (0..block.params.len())
        .find(|position| *position != count_position && *position != index_position)?;
    let current = block.params[accumulator_position];
    let accumulator_type = eu.register_map.get(&current)?.clone();
    if accumulator_type.width() < lanes {
        return None;
    }
    let update = backedge[accumulator_position];
    let (shift, bit) = match_inserted_bit(eu, definitions, current, update)?;
    let index = block.params[index_position];
    if !shift_is_index(eu, definitions, shift, index) {
        return None;
    }
    let exit_result_position = exit_arguments.iter().position(|&value| value == update)?;
    if exit_arguments.iter().enumerate().any(|(position, value)| {
        position != exit_result_position
            && definitions
                .get(value)
                .is_some_and(|definition| definition.block() == header)
    }) {
        return None;
    }
    let dependencies = collect_bit_dependencies(
        eu,
        cfg,
        definitions,
        header,
        preheader,
        bit,
        index,
        &[block.params[count_position], current],
    )?;
    Some(BitMapLoopPlan {
        preheader,
        header,
        exit,
        lanes,
        accumulator_type,
        initial_accumulator: entry_arguments[accumulator_position],
        index,
        bit,
        dependency_indices: dependencies,
        exit_arguments: exit_arguments.to_vec(),
        exit_result_position,
    })
}

fn common_jump_target(eu: &ExecutionUnit<RegionedAbsoluteAddr>, block: BlockId) -> Option<BlockId> {
    let SIRTerminator::Jump(target, _) = &eu.blocks.get(&block)?.terminator else {
        return None;
    };
    Some(*target)
}

fn is_add_one(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    value: RegisterId,
    source: RegisterId,
) -> bool {
    matches!(
        instruction(eu, definitions, value),
        Some(SIRInstruction::Binary(_, lhs, BinaryOp::Add, rhs))
            if (*lhs == source && immediate(eu, definitions, *rhs) == Some(1))
                || (*rhs == source && immediate(eu, definitions, *lhs) == Some(1))
    )
}

fn is_zero_extended_index(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    value: RegisterId,
    index: RegisterId,
) -> bool {
    if value == index {
        return true;
    }
    let Some(SIRInstruction::Concat(_, parts)) = instruction(eu, definitions, value) else {
        return false;
    };
    parts.last() == Some(&index)
        && parts[..parts.len().saturating_sub(1)]
            .iter()
            .all(|&part| immediate(eu, definitions, part) == Some(0))
}

fn recognize_loop(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    header: BlockId,
    loop_blocks: &HashSet<BlockId>,
    definitions: &HashMap<RegisterId, Definition>,
    constant_cache: &mut HashMap<RegisterId, Option<bool>>,
    bit_array_elements: &HashMap<AbsoluteAddr, usize>,
    array_shapes: &HashMap<AbsoluteAddr, ArrayShape>,
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
            array_shapes,
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
    array_shapes: &HashMap<AbsoluteAddr, ArrayShape>,
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
            array_shapes,
            &HashSet::default(),
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
        eu.blocks[block]
            .instructions
            .iter()
            .all(|instruction| !instruction.has_side_effects())
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
    instruction.for_each_use(|register| uses.push(register));
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
        SIRTerminator::Switch { selector, .. } => uses.push(*selector),
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
    array_shapes: &HashMap<AbsoluteAddr, ArrayShape>,
    hoisted_values: &HashSet<RegisterId>,
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
                SIRInstruction::Load(_, address, offset, 1)
                    if is_lane_bit_offset(offset, index)
                        && bit_array_elements
                            .get(&address.absolute_addr())
                            .is_some_and(|&element_count| element_count >= lanes) =>
                {
                    values.insert(
                        register,
                        intern_node(
                            &mut nodes,
                            &mut interned,
                            PackedNode::Load {
                                address: *address,
                                unpacked_element_width: array_shapes
                                    .get(&address.absolute_addr())
                                    .map(|shape| shape.element_width),
                            },
                        ),
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
                SIRInstruction::Binary(_, lhs, BinaryOp::Eq | BinaryOp::EqWildcard, rhs)
                    if match_lane_eq(
                        eu,
                        cfg,
                        definitions,
                        loop_blocks,
                        preheader,
                        index,
                        lanes,
                        array_shapes,
                        hoisted_values,
                        *lhs,
                        *rhs,
                    )
                    .is_some() =>
                {
                    let node = match_lane_eq(
                        eu,
                        cfg,
                        definitions,
                        loop_blocks,
                        preheader,
                        index,
                        lanes,
                        array_shapes,
                        hoisted_values,
                        *lhs,
                        *rhs,
                    )?;
                    values.insert(register, intern_node(&mut nodes, &mut interned, node));
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
                SIRInstruction::Binary(_, lhs, BinaryOp::LtU, rhs)
                    if index_prefix_lhs(eu, definitions, *lhs, index, lanes)
                        && definitions.get(rhs).is_some_and(|definition| {
                            cfg.dominates(definition.block(), preheader)
                        }) =>
                {
                    values.insert(
                        register,
                        intern_node(&mut nodes, &mut interned, PackedNode::Prefix(*rhs)),
                    );
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
        .filter(|node| matches!(node, PackedNode::Load { .. } | PackedNode::LaneEq { .. }))
        .count();
    let value_ops = nodes
        .iter()
        .filter(|node| {
            matches!(
                node,
                PackedNode::Broadcast(_)
                    | PackedNode::Prefix(_)
                    | PackedNode::LaneEq { .. }
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

#[allow(clippy::too_many_arguments)]
fn match_lane_eq(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    definitions: &HashMap<RegisterId, Definition>,
    loop_blocks: &HashSet<BlockId>,
    preheader: BlockId,
    index: RegisterId,
    lanes: usize,
    array_shapes: &HashMap<AbsoluteAddr, ArrayShape>,
    hoisted_values: &HashSet<RegisterId>,
    lhs: RegisterId,
    rhs: RegisterId,
) -> Option<PackedNode> {
    for (loaded, value) in [(lhs, rhs), (rhs, lhs)] {
        let SIRInstruction::Load(
            _,
            address,
            SIROffset::Element {
                index: load_index,
                element_width,
                bit_offset,
                dynamic_bit_offset,
            },
            field_width,
        ) = instruction(eu, definitions, loaded)?
        else {
            continue;
        };
        let dynamic_bit_offset = if let Some(offset) = dynamic_bit_offset {
            immediate(eu, definitions, *offset)?
        } else {
            0
        };
        let dynamic_bit_offset = usize::try_from(dynamic_bit_offset).ok()?;
        let bit_offset = bit_offset.checked_add(dynamic_bit_offset)?;
        if *load_index != index
            || *field_width == 0
            || bit_offset.checked_add(*field_width)? > *element_width
            || eu.register_map.get(&value).map(RegisterType::width) != Some(*field_width)
        {
            continue;
        }
        let shape = array_shapes.get(&address.absolute_addr())?;
        if shape.element_width != *element_width || shape.element_count < lanes {
            continue;
        }
        let value_definition = definitions.get(&value).copied()?;
        let defined_in_loop = loop_blocks.contains(&value_definition.block());
        if defined_in_loop && !hoisted_values.contains(&value)
            || !defined_in_loop && !cfg.dominates(value_definition.block(), preheader)
        {
            continue;
        }
        element_width.checked_mul(lanes)?;
        field_width.checked_mul(lanes)?;
        return Some(PackedNode::LaneEq {
            address: *address,
            element_width: *element_width,
            bit_offset,
            field_width: *field_width,
            value,
        });
    }
    None
}

fn is_lane_bit_offset(offset: &SIROffset, index: RegisterId) -> bool {
    matches!(offset, SIROffset::Dynamic(load_index) if *load_index == index)
        || matches!(
            offset,
            SIROffset::Element {
                index: load_index,
                element_width: 1,
                bit_offset: 0,
                dynamic_bit_offset: None,
            } if *load_index == index
        )
}

fn index_prefix_lhs(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    mut value: RegisterId,
    index: RegisterId,
    lanes: usize,
) -> bool {
    let required_mask = lanes
        .checked_next_power_of_two()
        .and_then(|width| width.checked_sub(1))
        .and_then(|mask| u64::try_from(mask).ok());
    loop {
        if value == index {
            return true;
        }
        match instruction(eu, definitions, value) {
            Some(SIRInstruction::Unary(_, UnaryOp::Ident | UnaryOp::ToTwoState, source)) => {
                value = *source
            }
            Some(SIRInstruction::Binary(_, lhs, BinaryOp::Shr, rhs))
                if immediate(eu, definitions, *rhs) == Some(0) =>
            {
                value = *lhs;
            }
            Some(SIRInstruction::Binary(_, lhs, BinaryOp::And, rhs))
                if immediate(eu, definitions, *rhs)
                    .zip(required_mask)
                    .is_some_and(|(mask, required)| mask & required == required) =>
            {
                value = *lhs;
            }
            Some(SIRInstruction::Binary(_, lhs, BinaryOp::And, rhs))
                if immediate(eu, definitions, *lhs)
                    .zip(required_mask)
                    .is_some_and(|(mask, required)| mask & required == required) =>
            {
                value = *rhs;
            }
            Some(SIRInstruction::Concat(_, parts))
                if parts.last() == Some(&index)
                    && parts[..parts.len().saturating_sub(1)]
                        .iter()
                        .all(|&part| immediate(eu, definitions, part) == Some(0)) =>
            {
                value = index;
            }
            _ => return false,
        }
    }
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

fn ids_available(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    plans: &[CircularPriorityPlan],
    linear_plans: &[LinearBitmapScanPlan],
    payload_plans: &[LastPayloadScanPlan],
    sparse_plans: &[SparseBitmapLoopPlan],
) -> bool {
    // A packed node normally emits one register. Prefix and Broadcast may emit
    // three, and materializing a constant root/inversion needs at most two
    // more. Keep this bound independent of the expression shape so ID-space
    // exhaustion is rejected before mutating the EU.
    let expression_registers = |expression: &PackedExpression, lanes: usize| {
        expression.nodes.iter().try_fold(2usize, |total, node| {
            let registers = if matches!(node, PackedNode::LaneEq { .. }) {
                lanes.checked_mul(2)?.checked_add(1)?
            } else {
                3
            };
            total.checked_add(registers)
        })
    };
    let required_registers = plans
        .iter()
        .try_fold(0usize, |total, plan| {
            total
                .checked_add(expression_registers(&plan.predicate, plan.lanes)?)?
                .checked_add(9)
        })
        .and_then(|total| {
            linear_plans.iter().try_fold(total, |total, plan| {
                total
                    .checked_add(expression_registers(&plan.predicate, plan.lanes)?)?
                    .checked_add(8)
            })
        })
        .and_then(|total| {
            payload_plans.iter().try_fold(total, |total, plan| {
                total
                    .checked_add(expression_registers(&plan.predicate, plan.lanes)?)?
                    .checked_add(plan.payloads.len())?
                    .checked_add(5)
            })
        })
        .and_then(|total| {
            sparse_plans.iter().try_fold(total, |total, plan| {
                total
                    .checked_add(expression_registers(&plan.predicate, plan.lanes)?)?
                    // CTZ may need one extra zero-extension register when the
                    // original loop index is wider than its result.
                    .checked_add(10)
            })
        });
    let required_blocks = plans
        .len()
        .checked_add(payload_plans.len())
        .and_then(|count| count.checked_mul(2));
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
                let mask = (BigUint::from(1u8) << self.width) - BigUint::from(1u8);
                let register =
                    fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                self.instructions
                    .push(SIRInstruction::Imm(register, SIRValue::new(mask)));
                self.ones = Some(register);
                register
            }
        }
    }

    fn emit_lane_eq(
        &mut self,
        address: RegionedAbsoluteAddr,
        element_width: usize,
        bit_offset: usize,
        field_width: usize,
        value: RegisterId,
    ) -> RegisterId {
        let mut predicates = Vec::with_capacity(self.width);
        for lane in (0..self.width).rev() {
            let loaded = fresh_register(self.eu, self.next_register, unsigned_type(field_width));
            self.instructions.push(SIRInstruction::Load(
                loaded,
                address,
                SIROffset::Static(lane * element_width + bit_offset),
                field_width,
            ));
            let predicate = fresh_register(self.eu, self.next_register, unsigned_type(1));
            self.instructions.push(SIRInstruction::Binary(
                predicate,
                loaded,
                BinaryOp::Eq,
                value,
            ));
            predicates.push(predicate);
        }
        let compact = fresh_register(self.eu, self.next_register, unsigned_type(self.width));
        self.instructions
            .push(SIRInstruction::Concat(compact, predicates));
        compact
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
                PackedNode::Load {
                    address,
                    unpacked_element_width,
                } => {
                    let register =
                        fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                    self.instructions.push(SIRInstruction::Load(
                        register,
                        address,
                        unpacked_element_width.map_or(SIROffset::Static(0), |element_width| {
                            SIROffset::PackedElements {
                                bit_offset: 0,
                                element_width,
                            }
                        }),
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
                PackedNode::Prefix(bound) => {
                    let one =
                        fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                    self.instructions
                        .push(SIRInstruction::Imm(one, SIRValue::new(1u8)));
                    let shifted =
                        fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                    self.instructions.push(SIRInstruction::Binary(
                        shifted,
                        one,
                        BinaryOp::Shl,
                        bound,
                    ));
                    let prefix =
                        fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                    self.instructions.push(SIRInstruction::Binary(
                        prefix,
                        shifted,
                        BinaryOp::Sub,
                        one,
                    ));
                    EmittedValue::Register(prefix)
                }
                PackedNode::LaneEq {
                    address,
                    element_width,
                    bit_offset,
                    field_width,
                    value,
                } => EmittedValue::Register(self.emit_lane_eq(
                    address,
                    element_width,
                    bit_offset,
                    field_width,
                    value,
                )),
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

fn apply_linear_bitmap_scan(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: LinearBitmapScanPlan,
    next_register: &mut usize,
) {
    let emitter = ExpressionEmitter {
        eu,
        next_register,
        width: plan.lanes,
        instructions: Vec::new(),
        zero: None,
        ones: None,
    };
    let (mask, predicate_instructions) = emitter.emit(&plan.predicate);
    let preheader = eu.blocks.get_mut(&plan.preheader).unwrap();
    preheader.instructions.extend(predicate_instructions);

    let count = fresh_register(eu, next_register, plan.count_type);
    let first_narrow = fresh_register(
        eu,
        next_register,
        unsigned_type(UnaryOp::CountTrailingZeros.result_width(plan.lanes)),
    );
    let nonempty = fresh_register(eu, next_register, unsigned_type(1));
    eu.blocks
        .get_mut(&plan.preheader)
        .unwrap()
        .instructions
        .extend([
            SIRInstruction::Unary(count, UnaryOp::PopCount, mask),
            SIRInstruction::Unary(first_narrow, UnaryOp::CountTrailingZeros, mask),
            SIRInstruction::Unary(nonempty, UnaryOp::Or, mask),
        ]);

    let first = if plan.best_type.width() == eu.register_map[&first_narrow].width() {
        first_narrow
    } else {
        let padding = fresh_register(
            eu,
            next_register,
            unsigned_type(plan.best_type.width() - eu.register_map[&first_narrow].width()),
        );
        let widened = fresh_register(eu, next_register, plan.best_type.clone());
        eu.blocks
            .get_mut(&plan.preheader)
            .unwrap()
            .instructions
            .extend([
                SIRInstruction::Imm(padding, SIRValue::new(0u8)),
                SIRInstruction::Concat(widened, vec![padding, first_narrow]),
            ]);
        widened
    };
    let one = fresh_register(eu, next_register, plan.found_type.clone());
    let best = fresh_register(eu, next_register, plan.best_type);
    let found = fresh_register(eu, next_register, plan.found_type);
    let preheader = eu.blocks.get_mut(&plan.preheader).unwrap();
    preheader.instructions.extend([
        SIRInstruction::Imm(one, SIRValue::new(1u8)),
        SIRInstruction::Mux(best, nonempty, first, plan.no_match_best),
        SIRInstruction::Mux(found, nonempty, one, plan.no_match_found),
    ]);
    let mut arguments = plan.exit_arguments;
    arguments[plan.exit_count_position] = count;
    arguments[plan.exit_best_position] = best;
    arguments[plan.exit_found_position] = found;
    preheader.terminator = SIRTerminator::Jump(plan.exit, arguments);

    for block in plan.loop_blocks {
        eu.blocks.remove(&block);
    }
}

fn replace_sparse_loop_use(
    instruction: &mut SIRInstruction<RegionedAbsoluteAddr>,
    old: RegisterId,
    new: RegisterId,
) {
    instruction.for_each_use_mut(|register| {
        if *register == old {
            *register = new;
        }
    });
}

fn replace_instruction_definition(
    instruction: &mut SIRInstruction<RegionedAbsoluteAddr>,
    definition: RegisterId,
) {
    *instruction
        .defined_register_mut()
        .expect("only an instruction that defines a register is renamed") = definition;
}

fn is_lane_comparison(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::LtU
            | BinaryOp::LtS
            | BinaryOp::LeU
            | BinaryOp::LeS
            | BinaryOp::GtU
            | BinaryOp::GtS
            | BinaryOp::GeU
            | BinaryOp::GeS
    )
}

#[allow(clippy::too_many_arguments)]
fn emit_boolean_mux(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    next_register: &mut usize,
    emitted: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    register_type: RegisterType,
    destination: RegisterId,
    condition: RegisterId,
    true_value: RegisterId,
    false_value: RegisterId,
) {
    let inverted = fresh_register(eu, next_register, register_type.clone());
    let on_true = fresh_register(eu, next_register, register_type.clone());
    let on_false = fresh_register(eu, next_register, register_type);
    emitted.extend([
        SIRInstruction::Unary(inverted, UnaryOp::BitNot, condition),
        SIRInstruction::Binary(on_true, condition, BinaryOp::And, true_value),
        SIRInstruction::Binary(on_false, inverted, BinaryOp::And, false_value),
        SIRInstruction::Binary(destination, on_true, BinaryOp::Or, on_false),
    ]);
}

fn apply_bit_map_loop(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: BitMapLoopPlan,
    next_register: &mut usize,
) {
    let source_instructions = eu.blocks[&plan.header].instructions.clone();
    let source_definitions = source_instructions
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| def_reg(instruction).map(|register| (register, index)))
        .collect::<HashMap<_, _>>();
    let index_type = eu.register_map[&plan.index].clone();
    let mut emitted = Vec::new();
    let mut lane_bits = Vec::with_capacity(plan.lanes);
    for lane in 0..plan.lanes {
        let lane_index = fresh_register(eu, next_register, index_type.clone());
        emitted.push(SIRInstruction::Imm(lane_index, SIRValue::new(lane)));
        let mut replacements = HashMap::default();
        replacements.insert(plan.index, lane_index);
        for &instruction_index in &plan.dependency_indices {
            let source = &source_instructions[instruction_index];
            let old_definition = def_reg(source).expect("bit-map dependency defines a value");
            let definition_type = eu.register_map[&old_definition].clone();
            let new_definition = fresh_register(eu, next_register, definition_type.clone());
            let mapped =
                |register: RegisterId| replacements.get(&register).copied().unwrap_or(register);
            if let SIRInstruction::Binary(_, lhs, operation, rhs) = source
                && is_lane_comparison(*operation)
            {
                let mux_operand = [(*lhs, *rhs, true), (*rhs, *lhs, false)]
                    .into_iter()
                    .find_map(|(candidate, other, mux_is_lhs)| {
                        let index = *source_definitions.get(&candidate)?;
                        let SIRInstruction::Mux(_, condition, on_true, on_false) =
                            &source_instructions[index]
                        else {
                            return None;
                        };
                        Some((*condition, *on_true, *on_false, other, mux_is_lhs))
                    });
                if let Some((condition, on_true, on_false, other, mux_is_lhs)) = mux_operand {
                    let true_compare = fresh_register(eu, next_register, definition_type.clone());
                    let false_compare = fresh_register(eu, next_register, definition_type.clone());
                    let (true_lhs, true_rhs, false_lhs, false_rhs) = if mux_is_lhs {
                        (
                            mapped(on_true),
                            mapped(other),
                            mapped(on_false),
                            mapped(other),
                        )
                    } else {
                        (
                            mapped(other),
                            mapped(on_true),
                            mapped(other),
                            mapped(on_false),
                        )
                    };
                    emitted.extend([
                        SIRInstruction::Binary(true_compare, true_lhs, *operation, true_rhs),
                        SIRInstruction::Binary(false_compare, false_lhs, *operation, false_rhs),
                    ]);
                    emit_boolean_mux(
                        eu,
                        next_register,
                        &mut emitted,
                        definition_type,
                        new_definition,
                        mapped(condition),
                        true_compare,
                        false_compare,
                    );
                    replacements.insert(old_definition, new_definition);
                    continue;
                }
            }
            let mut cloned = source.clone();
            for (&old, &new) in &replacements {
                replace_sparse_loop_use(&mut cloned, old, new);
            }
            replace_instruction_definition(&mut cloned, new_definition);
            if let SIRInstruction::Mux(_, condition, true_value, false_value) = &cloned
                && definition_type.width() == 1
            {
                emit_boolean_mux(
                    eu,
                    next_register,
                    &mut emitted,
                    definition_type,
                    new_definition,
                    *condition,
                    *true_value,
                    *false_value,
                );
                replacements.insert(old_definition, new_definition);
                continue;
            }
            replacements.insert(old_definition, new_definition);
            emitted.push(cloned);
        }
        lane_bits.push(replacements[&plan.bit]);
    }
    lane_bits.reverse();
    let packed = fresh_register(eu, next_register, unsigned_type(plan.lanes));
    emitted.push(SIRInstruction::Concat(packed, lane_bits));
    let accumulator_width = plan.accumulator_type.width();
    let result = if accumulator_width == plan.lanes {
        packed
    } else {
        let padding = fresh_register(
            eu,
            next_register,
            unsigned_type(accumulator_width - plan.lanes),
        );
        emitted.push(SIRInstruction::Imm(padding, SIRValue::new(0u8)));
        let extended = fresh_register(eu, next_register, plan.accumulator_type.clone());
        emitted.push(SIRInstruction::Concat(extended, vec![padding, packed]));
        let low_mask = (BigUint::from(1u8) << plan.lanes) - BigUint::from(1u8);
        let mask = fresh_register(eu, next_register, plan.accumulator_type.clone());
        emitted.push(SIRInstruction::Imm(mask, SIRValue::new(low_mask)));
        let inverted_mask = fresh_register(eu, next_register, plan.accumulator_type.clone());
        emitted.push(SIRInstruction::Unary(inverted_mask, UnaryOp::BitNot, mask));
        let preserved = fresh_register(eu, next_register, plan.accumulator_type.clone());
        emitted.push(SIRInstruction::Binary(
            preserved,
            plan.initial_accumulator,
            BinaryOp::And,
            inverted_mask,
        ));
        let merged = fresh_register(eu, next_register, plan.accumulator_type);
        emitted.push(SIRInstruction::Binary(
            merged,
            preserved,
            BinaryOp::Or,
            extended,
        ));
        merged
    };
    let mut exit_arguments = plan.exit_arguments;
    exit_arguments[plan.exit_result_position] = result;
    let preheader = eu.blocks.get_mut(&plan.preheader).unwrap();
    preheader.instructions.extend(emitted);
    preheader.terminator = SIRTerminator::Jump(plan.exit, exit_arguments);
    eu.blocks.remove(&plan.header);
}

fn apply_sparse_bitmap_loop(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: SparseBitmapLoopPlan,
    next_register: &mut usize,
) {
    let emitter = ExpressionEmitter {
        eu,
        next_register,
        width: plan.lanes,
        instructions: Vec::new(),
        zero: None,
        ones: None,
    };
    let (mask, predicate_instructions) = emitter.emit(&plan.predicate);
    let nonempty = fresh_register(eu, next_register, unsigned_type(1));
    let index_type = eu.register_map[&eu.blocks[&plan.header].params[plan.index_position]].clone();
    let index_width = index_type.width();
    let zero_index = fresh_register(eu, next_register, index_type.clone());
    let mut entry_arguments = plan.entry_arguments.clone();
    entry_arguments[plan.count_position] = mask;
    entry_arguments[plan.index_position] = zero_index;
    let preheader = eu.blocks.get_mut(&plan.preheader).unwrap();
    preheader.instructions.extend(predicate_instructions);
    preheader.instructions.extend([
        SIRInstruction::Unary(nonempty, UnaryOp::Or, mask),
        SIRInstruction::Imm(zero_index, SIRValue::new(0u8)),
    ]);
    preheader.terminator = SIRTerminator::Branch {
        cond: nonempty,
        true_block: (plan.header, entry_arguments),
        false_block: (plan.exit, plan.bypass_arguments.clone()),
    };

    let header_params = eu.blocks[&plan.header].params.clone();
    let remaining = header_params[plan.count_position];
    let old_index = header_params[plan.index_position];
    eu.register_map.insert(remaining, unsigned_type(plan.lanes));
    let trailing_width = UnaryOp::CountTrailingZeros.result_width(plan.lanes);
    let index_wide = fresh_register(eu, next_register, unsigned_type(trailing_width));
    let index = fresh_register(eu, next_register, index_type);
    let index_padding = (index_width > trailing_width).then(|| {
        fresh_register(
            eu,
            next_register,
            unsigned_type(index_width - trailing_width),
        )
    });
    let known_true = fresh_register(eu, next_register, unsigned_type(1));
    let one = fresh_register(eu, next_register, unsigned_type(plan.lanes));
    let remaining_minus_one = fresh_register(eu, next_register, unsigned_type(plan.lanes));
    let next_remaining = fresh_register(eu, next_register, unsigned_type(plan.lanes));
    let more = fresh_register(eu, next_register, unsigned_type(1));

    let mut old_instructions =
        std::mem::take(&mut eu.blocks.get_mut(&plan.header).unwrap().instructions);
    for instruction in &mut old_instructions {
        replace_sparse_loop_use(instruction, old_index, index);
        replace_sparse_loop_use(instruction, plan.common_predicate, known_true);
    }
    let header = eu.blocks.get_mut(&plan.header).unwrap();
    header.instructions.push(SIRInstruction::Unary(
        index_wide,
        UnaryOp::CountTrailingZeros,
        remaining,
    ));
    if let Some(padding) = index_padding {
        header
            .instructions
            .push(SIRInstruction::Imm(padding, SIRValue::new(0u8)));
        header
            .instructions
            .push(SIRInstruction::Concat(index, vec![padding, index_wide]));
    } else if index_width == trailing_width {
        header
            .instructions
            .push(SIRInstruction::Unary(index, UnaryOp::Ident, index_wide));
    } else {
        header
            .instructions
            .push(SIRInstruction::Slice(index, index_wide, 0, index_width));
    }
    header
        .instructions
        .push(SIRInstruction::Imm(known_true, SIRValue::new(1u8)));
    header.instructions.append(&mut old_instructions);
    header.instructions.extend([
        SIRInstruction::Imm(one, SIRValue::new(1u8)),
        SIRInstruction::Binary(remaining_minus_one, remaining, BinaryOp::Sub, one),
        SIRInstruction::Binary(
            next_remaining,
            remaining,
            BinaryOp::And,
            remaining_minus_one,
        ),
        SIRInstruction::Unary(more, UnaryOp::Or, next_remaining),
    ]);
    let mut backedge = plan.backedge_arguments;
    backedge[plan.count_position] = next_remaining;
    backedge[plan.index_position] = zero_index;
    header.terminator = SIRTerminator::Branch {
        cond: more,
        true_block: (plan.header, backedge),
        false_block: (plan.exit, plan.exit_arguments),
    };
}

fn apply_last_payload_scan(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: LastPayloadScanPlan,
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
    let nonempty = fresh_register(eu, next_register, unsigned_type(1));
    let preheader = eu.blocks.get_mut(&plan.preheader).unwrap();
    preheader.instructions.extend(plan.hoisted_instructions);
    preheader.instructions.extend(predicate_instructions);
    preheader
        .instructions
        .push(SIRInstruction::Unary(nonempty, UnaryOp::Or, mask));
    preheader.terminator = SIRTerminator::Branch {
        cond: nonempty,
        true_block: (selected_block, Vec::new()),
        false_block: (empty_block, Vec::new()),
    };

    let count_width = UnaryOp::CountLeadingZeros.result_width(plan.lanes);
    let leading_zeros = fresh_register(eu, next_register, unsigned_type(count_width));
    let last_lane = fresh_register(eu, next_register, unsigned_type(count_width));
    let last_lane_number = fresh_register(eu, next_register, unsigned_type(count_width));
    let found = fresh_register(eu, next_register, plan.found_type.clone());
    let mut selected_instructions = vec![
        SIRInstruction::Unary(leading_zeros, UnaryOp::CountLeadingZeros, mask),
        SIRInstruction::Imm(last_lane, SIRValue::new(plan.lanes - 1)),
        SIRInstruction::Binary(last_lane_number, last_lane, BinaryOp::Sub, leading_zeros),
        SIRInstruction::Imm(found, SIRValue::new(1u8)),
    ];
    let mut selected_arguments = plan.exit_arguments.clone();
    selected_arguments[plan.exit_found_position] = found;
    for payload in &plan.payloads {
        let value = fresh_register(eu, next_register, payload.value_type.clone());
        selected_instructions.push(SIRInstruction::Load(
            value,
            payload.address,
            SIROffset::Element {
                index: last_lane_number,
                element_width: payload.element_width,
                bit_offset: payload.bit_offset,
                dynamic_bit_offset: None,
            },
            payload.width,
        ));
        selected_arguments[payload.exit_position] = value;
    }
    let selected = BasicBlock {
        id: selected_block,
        params: Vec::new(),
        instructions: selected_instructions,
        terminator: SIRTerminator::Jump(plan.exit, selected_arguments),
    };

    let mut empty_arguments = plan.exit_arguments;
    empty_arguments[plan.exit_found_position] = plan.no_match_found;
    for payload in &plan.payloads {
        empty_arguments[payload.exit_position] = payload.no_match;
    }
    let empty = BasicBlock {
        id: empty_block,
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Jump(plan.exit, empty_arguments),
    };

    for block in plan.loop_blocks {
        eu.blocks.remove(&block);
    }
    eu.blocks.insert(selected_block, selected);
    eu.blocks.insert(empty_block, empty);
}

#[cfg(test)]
mod tests;
