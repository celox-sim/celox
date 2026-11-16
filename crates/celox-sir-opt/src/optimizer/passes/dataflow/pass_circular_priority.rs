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

mod apply;
mod packed;
mod recognize_loops;
mod recognize_scans;
use apply::*;
use packed::*;
use recognize_loops::*;
use recognize_scans::*;

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

#[cfg(test)]
mod tests;
