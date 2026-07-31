//! Recover one dynamically indexed unpacked-array store from a lowered CFG.
//!
//! Analyzer lowering can turn an update such as `array[index] = value` into
//! one exact equality, one store arm, and one empty arm for every possible
//! index.  When both arms rejoin before the next equality, native lowering
//! otherwise emits an O(element_count) branch ladder for an operation whose
//! source semantics and machine implementation are both O(1).
//!
//! This pass proves the complete shape before changing it.  In particular,
//! the equality keys must cover the selector's entire two-state domain, each
//! key must address the corresponding declared unpacked-array element, and
//! every selected arm must contain the same alpha-equivalent pure value DAG
//! followed by exactly one unobserved store.  CFG construction, chain
//! discovery, validation, and rewriting are all linear in blocks plus SIR
//! instructions; no path enumeration or pairwise block relation is used.

use super::pass_manager::ExecutionUnitPass;
use super::shared::def_reg;
use crate::ir::cfg::SirCfg;
use crate::ir::*;
use crate::optimizer::PassOptions;
use crate::{HashMap, HashSet};
use num_bigint::BigUint;
use num_traits::{One, ToPrimitive, Zero};

const MIN_PROFITABLE_ELEMENTS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ArrayShape {
    element_width: usize,
    element_count: usize,
}

pub(super) struct IndexedStoreRecoveryPass {
    arrays: HashMap<AbsoluteAddr, ArrayShape>,
}

impl IndexedStoreRecoveryPass {
    pub(super) fn for_program(program: &Program) -> Self {
        let mut arrays = HashMap::default();
        for (&address, info) in &program.design.state_objects {
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
            if element_count == 0 || info.width % element_count != 0 {
                continue;
            }
            arrays.insert(
                address,
                ArrayShape {
                    element_width: info.width / element_count,
                    element_count,
                },
            );
        }
        Self { arrays }
    }
}

impl ExecutionUnitPass for IndexedStoreRecoveryPass {
    fn name(&self) -> &'static str {
        "indexed_store_recovery"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        // In four-state mode an X/Z selector makes every procedural equality
        // false, whereas a raw dynamic element offset has no such no-op value.
        if options.four_state || eu.verify_result().is_err() {
            return;
        }

        // Recognized chains are disjoint.  Prepare every replacement from one
        // CFG snapshot, then rewrite and clean up once so many independent
        // arrays do not turn this pass into repeated whole-function scans.
        let plans = find_plans(eu, &self.arrays);
        if !plans.is_empty() {
            apply_plans(eu, plans);
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct DefinitionSite {
    block: BlockId,
    index: usize,
}

#[derive(Clone, Copy, Debug)]
struct RawStage {
    decision: BlockId,
    matched_arm: BlockId,
    empty_arm: BlockId,
    next: BlockId,
    selector: RegisterId,
    key: usize,
    destination: RegionedAbsoluteAddr,
    static_offset: usize,
    width: usize,
    shape: ArrayShape,
}

#[derive(Clone, Copy, Debug)]
struct IndexedStorePlan {
    head: BlockId,
    template_arm: BlockId,
    continuation: BlockId,
    selector: RegisterId,
    destination: RegionedAbsoluteAddr,
    element_width: usize,
}

fn find_plans(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    arrays: &HashMap<AbsoluteAddr, ArrayShape>,
) -> Vec<IndexedStorePlan> {
    let Ok(cfg) = SirCfg::analyze(eu) else {
        return Vec::new();
    };
    let definitions = definition_sites(eu);
    let use_blocks = register_use_blocks(eu);
    let mut exact_constants = HashMap::default();

    let mut stages = Vec::new();
    let mut stage_for_block = HashMap::default();
    for (block_index, &block_id) in cfg.block_ids.iter().enumerate() {
        let Some(stage) = recognize_stage(
            eu,
            &cfg,
            block_index,
            &definitions,
            &mut exact_constants,
            arrays,
        ) else {
            continue;
        };
        stage_for_block.insert(block_id, stages.len());
        stages.push(stage);
    }
    if stages.is_empty() {
        return Vec::new();
    }

    // Connect only an unambiguous same-selector continuation.  Requiring the
    // next decision's complete predecessor set to be the preceding two arms
    // prevents an external entry from being silently bypassed by the rewrite.
    let mut next = vec![None; stages.len()];
    let mut incoming = vec![0usize; stages.len()];
    for (stage_index, stage) in stages.iter().enumerate() {
        let Some(&successor) = stage_for_block.get(&stage.next) else {
            continue;
        };
        if stages[successor].selector != stage.selector
            || !is_exclusive_stage_join(&cfg, stage, stages[successor].decision)
        {
            continue;
        }
        next[stage_index] = Some(successor);
        incoming[successor] = incoming[successor].saturating_add(1);
    }

    let mut visited = vec![false; stages.len()];
    let mut plans = Vec::new();
    for start in 0..stages.len() {
        if incoming[start] == 1 || visited[start] {
            continue;
        }
        let chain = collect_chain(start, &next, &incoming, &mut visited);
        if let Some(plan) = plan_complete_chain(eu, &stages, &chain, &use_blocks) {
            plans.push(plan);
        }
    }
    plans.sort_unstable_by_key(|plan| plan.head.0);
    plans
}

fn collect_chain(
    start: usize,
    next: &[Option<usize>],
    incoming: &[usize],
    visited: &mut [bool],
) -> Vec<usize> {
    let mut chain = Vec::new();
    let mut current = start;
    loop {
        if visited[current] {
            break;
        }
        visited[current] = true;
        chain.push(current);
        let Some(successor) = next[current] else {
            break;
        };
        if incoming[successor] != 1 {
            break;
        }
        current = successor;
    }
    chain
}

fn recognize_stage(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    decision_index: usize,
    definitions: &HashMap<RegisterId, DefinitionSite>,
    exact_constants: &mut HashMap<RegisterId, Option<BigUint>>,
    arrays: &HashMap<AbsoluteAddr, ArrayShape>,
) -> Option<RawStage> {
    if cfg.sccs[cfg.scc_for_block[decision_index]].cyclic {
        return None;
    }
    let decision_id = cfg.block_ids[decision_index];
    let decision = &eu.blocks[&decision_id];
    if !decision.instructions.iter().all(is_cloneable_pure) {
        return None;
    }
    let SIRTerminator::Branch {
        cond,
        true_block,
        false_block,
    } = &decision.terminator
    else {
        return None;
    };
    if !true_block.1.is_empty() || !false_block.1.is_empty() {
        return None;
    }
    let predicate = exact_equality_predicate(eu, definitions, exact_constants, *cond)?;
    let (matched_arm, empty_arm) = if predicate.equal_when_true {
        (true_block.0, false_block.0)
    } else {
        (false_block.0, true_block.0)
    };
    if matched_arm == empty_arm {
        return None;
    }
    let matched_index = cfg.block_index(matched_arm)?;
    let empty_index = cfg.block_index(empty_arm)?;
    if cfg.sccs[cfg.scc_for_block[matched_index]].cyclic
        || cfg.sccs[cfg.scc_for_block[empty_index]].cyclic
        || !has_only_predecessor(cfg, matched_index, decision_index)
        || !has_only_predecessor(cfg, empty_index, decision_index)
    {
        return None;
    }
    let matched = &eu.blocks[&matched_arm];
    let empty = &eu.blocks[&empty_arm];
    if !matched.params.is_empty() || !empty.params.is_empty() || !empty.instructions.is_empty() {
        return None;
    }
    let SIRTerminator::Jump(matched_next, matched_args) = &matched.terminator else {
        return None;
    };
    let SIRTerminator::Jump(empty_next, empty_args) = &empty.terminator else {
        return None;
    };
    if matched_next != empty_next || !matched_args.is_empty() || !empty_args.is_empty() {
        return None;
    }
    if !valid_value_arm(eu, matched) {
        return None;
    }
    let (store, _) = matched.instructions.split_last()?;
    let SIRInstruction::Store(
        destination,
        SIROffset::Static(static_offset),
        width,
        source,
        triggers,
        capture_sites,
    ) = store
    else {
        return None;
    };
    if !triggers.is_empty() || !capture_sites.is_empty() || *width == 0 {
        return None;
    }
    let shape = *arrays.get(&destination.absolute_addr())?;
    if *width != shape.element_width
        || eu.register_map.get(source)?.width() != shape.element_width
        || shape.element_count < MIN_PROFITABLE_ELEMENTS
    {
        return None;
    }
    let selector_type = eu.register_map.get(&predicate.selector)?;
    let selector_width = selector_type.width();
    if selector_width == 0
        || selector_width >= usize::BITS as usize
        || selector_type.is_signed()
        || 1usize.checked_shl(selector_width as u32)? != shape.element_count
    {
        return None;
    }
    let key = predicate.key.to_usize()?;
    if key >= shape.element_count || key.checked_mul(shape.element_width)? != *static_offset {
        return None;
    }

    Some(RawStage {
        decision: decision_id,
        matched_arm,
        empty_arm,
        next: *matched_next,
        selector: predicate.selector,
        key,
        destination: *destination,
        static_offset: *static_offset,
        width: *width,
        shape,
    })
}

fn has_only_predecessor(cfg: &SirCfg, block: usize, predecessor: usize) -> bool {
    cfg.predecessors[block].len() == 1 && cfg.predecessors[block][0] == predecessor
}

fn is_exclusive_stage_join(cfg: &SirCfg, stage: &RawStage, next: BlockId) -> bool {
    let Some(next) = cfg.block_index(next) else {
        return false;
    };
    let Some(matched) = cfg.block_index(stage.matched_arm) else {
        return false;
    };
    let Some(empty) = cfg.block_index(stage.empty_arm) else {
        return false;
    };
    cfg.predecessors[next].len() == 2
        && cfg.predecessors[next].contains(&matched)
        && cfg.predecessors[next].contains(&empty)
}

fn plan_complete_chain(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    stages: &[RawStage],
    chain: &[usize],
    use_blocks: &HashMap<RegisterId, HashSet<BlockId>>,
) -> Option<IndexedStorePlan> {
    let &first_index = chain.first()?;
    let first = stages[first_index];
    if chain.len() != first.shape.element_count {
        return None;
    }

    let mut seen = vec![false; first.shape.element_count];
    for &stage_index in chain {
        let stage = stages[stage_index];
        if stage.selector != first.selector
            || stage.destination != first.destination
            || stage.width != first.width
            || stage.shape != first.shape
            || stage.static_offset != stage.key.checked_mul(stage.width)?
            || seen.get(stage.key).copied() != Some(false)
            || !value_arms_are_alpha_equivalent(eu, first.matched_arm, stage.matched_arm)
        {
            return None;
        }
        seen[stage.key] = true;
    }
    if seen.iter().any(|present| !present) {
        return None;
    }

    // The rewrite keeps the first decision block and removes every arm plus
    // every later decision.  A value defined there must not escape that set;
    // otherwise bypassing the blocks would invalidate SSA and observable data.
    let mut removed = HashSet::default();
    for (position, &stage_index) in chain.iter().enumerate() {
        let stage = stages[stage_index];
        if position != 0 {
            removed.insert(stage.decision);
        }
        removed.insert(stage.matched_arm);
        removed.insert(stage.empty_arm);
    }
    for &block_id in &removed {
        for instruction in &eu.blocks[&block_id].instructions {
            let Some(destination) = def_reg(instruction) else {
                continue;
            };
            if use_blocks
                .get(&destination)
                .into_iter()
                .flatten()
                .any(|use_block| !removed.contains(use_block))
            {
                return None;
            }
        }
    }

    let last = stages[*chain.last()?];
    Some(IndexedStorePlan {
        head: first.decision,
        template_arm: first.matched_arm,
        continuation: last.next,
        selector: first.selector,
        destination: first.destination,
        element_width: first.width,
    })
}

fn valid_value_arm(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
) -> bool {
    let Some((store, pure)) = block.instructions.split_last() else {
        return false;
    };
    if !matches!(store, SIRInstruction::Store(..)) || !pure.iter().all(is_cloneable_pure) {
        return false;
    }

    // Explicitly verify local topological order.  This also makes cloning
    // independent of any verifier implementation detail.
    let local = pure.iter().filter_map(def_reg).collect::<HashSet<_>>();
    let mut available = HashSet::default();
    for instruction in pure {
        if instruction_uses(instruction)
            .into_iter()
            .any(|operand| local.contains(&operand) && !available.contains(&operand))
        {
            return false;
        }
        let Some(destination) = def_reg(instruction) else {
            return false;
        };
        if !available.insert(destination) || !eu.register_map.contains_key(&destination) {
            return false;
        }
    }
    instruction_uses(store)
        .into_iter()
        .all(|operand| !local.contains(&operand) || available.contains(&operand))
}

fn is_cloneable_pure(instruction: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    matches!(
        instruction,
        SIRInstruction::Imm(..)
            | SIRInstruction::Binary(..)
            | SIRInstruction::Unary(..)
            | SIRInstruction::Load(..)
            | SIRInstruction::Concat(..)
            | SIRInstruction::Slice(..)
            | SIRInstruction::Mux(..)
    )
}

fn value_arms_are_alpha_equivalent(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    template: BlockId,
    candidate: BlockId,
) -> bool {
    if template == candidate {
        return true;
    }
    let template = &eu.blocks[&template];
    let candidate = &eu.blocks[&candidate];
    let Some((template_store, template_pure)) = template.instructions.split_last() else {
        return false;
    };
    let Some((candidate_store, candidate_pure)) = candidate.instructions.split_last() else {
        return false;
    };
    if template_pure.len() != candidate_pure.len() {
        return false;
    }

    let mut candidate_to_template = HashMap::default();
    for (template_instruction, candidate_instruction) in template_pure.iter().zip(candidate_pure) {
        let Some(template_destination) = def_reg(template_instruction) else {
            return false;
        };
        let Some(candidate_destination) = def_reg(candidate_instruction) else {
            return false;
        };
        if eu.register_map.get(&template_destination) != eu.register_map.get(&candidate_destination)
            || !same_pure_instruction(
                template_instruction,
                candidate_instruction,
                &candidate_to_template,
            )
            || candidate_to_template
                .insert(candidate_destination, template_destination)
                .is_some()
        {
            return false;
        }
    }

    let SIRInstruction::Store(_, _, _, template_source, _, _) = template_store else {
        return false;
    };
    let SIRInstruction::Store(_, _, _, candidate_source, _, _) = candidate_store else {
        return false;
    };
    same_register(*template_source, *candidate_source, &candidate_to_template)
}

fn same_pure_instruction(
    template: &SIRInstruction<RegionedAbsoluteAddr>,
    candidate: &SIRInstruction<RegionedAbsoluteAddr>,
    mapping: &HashMap<RegisterId, RegisterId>,
) -> bool {
    match (template, candidate) {
        (SIRInstruction::Imm(_, left), SIRInstruction::Imm(_, right)) => left == right,
        (
            SIRInstruction::Binary(_, left_lhs, left_op, left_rhs),
            SIRInstruction::Binary(_, right_lhs, right_op, right_rhs),
        ) => {
            left_op == right_op
                && same_register(*left_lhs, *right_lhs, mapping)
                && same_register(*left_rhs, *right_rhs, mapping)
        }
        (
            SIRInstruction::Unary(_, left_op, left_source),
            SIRInstruction::Unary(_, right_op, right_source),
        ) => left_op == right_op && same_register(*left_source, *right_source, mapping),
        (
            SIRInstruction::Load(_, left_address, left_offset, left_width),
            SIRInstruction::Load(_, right_address, right_offset, right_width),
        ) => {
            left_address == right_address
                && left_width == right_width
                && same_offset(left_offset, right_offset, mapping)
        }
        (SIRInstruction::Concat(_, left_sources), SIRInstruction::Concat(_, right_sources)) => {
            left_sources.len() == right_sources.len()
                && left_sources
                    .iter()
                    .zip(right_sources)
                    .all(|(&left, &right)| same_register(left, right, mapping))
        }
        (
            SIRInstruction::Slice(_, left_source, left_offset, left_width),
            SIRInstruction::Slice(_, right_source, right_offset, right_width),
        ) => {
            left_offset == right_offset
                && left_width == right_width
                && same_register(*left_source, *right_source, mapping)
        }
        (
            SIRInstruction::Mux(_, left_cond, left_true, left_false),
            SIRInstruction::Mux(_, right_cond, right_true, right_false),
        ) => {
            same_register(*left_cond, *right_cond, mapping)
                && same_register(*left_true, *right_true, mapping)
                && same_register(*left_false, *right_false, mapping)
        }
        _ => false,
    }
}

fn same_register(
    template: RegisterId,
    candidate: RegisterId,
    mapping: &HashMap<RegisterId, RegisterId>,
) -> bool {
    mapping.get(&candidate).copied().unwrap_or(candidate) == template
}

fn same_offset(
    template: &SIROffset,
    candidate: &SIROffset,
    mapping: &HashMap<RegisterId, RegisterId>,
) -> bool {
    match (template, candidate) {
        (SIROffset::Static(left), SIROffset::Static(right)) => left == right,
        (SIROffset::Dynamic(left), SIROffset::Dynamic(right)) => {
            same_register(*left, *right, mapping)
        }
        (
            SIROffset::Element {
                index: left_index,
                element_width: left_width,
                bit_offset: left_bit,
                dynamic_bit_offset: left_dynamic,
            },
            SIROffset::Element {
                index: right_index,
                element_width: right_width,
                bit_offset: right_bit,
                dynamic_bit_offset: right_dynamic,
            },
        ) => {
            left_width == right_width
                && left_bit == right_bit
                && same_register(*left_index, *right_index, mapping)
                && match (left_dynamic, right_dynamic) {
                    (None, None) => true,
                    (Some(left), Some(right)) => same_register(*left, *right, mapping),
                    _ => false,
                }
        }
        _ => false,
    }
}

#[derive(Clone, Debug)]
struct ExactEqualityPredicate {
    selector: RegisterId,
    key: BigUint,
    equal_when_true: bool,
}

fn exact_equality_predicate(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, DefinitionSite>,
    exact_constants: &mut HashMap<RegisterId, Option<BigUint>>,
    mut condition: RegisterId,
) -> Option<ExactEqualityPredicate> {
    let mut inverted = false;
    let mut seen = HashSet::default();
    let (lhs, op, rhs) = loop {
        if !seen.insert(condition) {
            return None;
        }
        match defining_instruction(eu, definitions, condition)? {
            SIRInstruction::Unary(_, UnaryOp::LogicNot, source) => {
                condition = *source;
                inverted = !inverted;
            }
            SIRInstruction::Unary(destination, UnaryOp::Ident | UnaryOp::ToTwoState, source)
                if eu.register_map.get(destination)?.width() == 1
                    && eu.register_map.get(source)?.width() == 1 =>
            {
                condition = *source;
            }
            SIRInstruction::Unary(destination, UnaryOp::Or, source)
                if eu.register_map.get(destination)?.width() == 1
                    && eu.register_map.get(source)?.width() == 1 =>
            {
                condition = *source;
            }
            SIRInstruction::Binary(
                destination,
                lhs,
                op @ (BinaryOp::Eq | BinaryOp::Ne | BinaryOp::EqWildcard | BinaryOp::NeWildcard),
                rhs,
            ) if eu.register_map.get(destination)?.width() == 1 => {
                break (*lhs, *op, *rhs);
            }
            _ => return None,
        }
    };

    let left_constant = exact_constant(eu, definitions, exact_constants, lhs);
    let right_constant = exact_constant(eu, definitions, exact_constants, rhs);
    let (selector, key_register, key) = match (left_constant, right_constant) {
        (None, Some(key)) => (lhs, rhs, key),
        (Some(key), None) => (rhs, lhs, key),
        _ => return None,
    };
    let compare_width = eu.register_map.get(&selector)?.width();
    if compare_width == 0 || eu.register_map.get(&key_register)?.width() != compare_width {
        return None;
    }
    let selector = canonical_selector(eu, definitions, selector);
    let selector_width = eu.register_map.get(&selector)?.width();
    let key = truncate(key, compare_width);
    if selector_width != compare_width || !fits_width(&key, selector_width) {
        return None;
    }
    Some(ExactEqualityPredicate {
        selector,
        key,
        equal_when_true: matches!(op, BinaryOp::Eq | BinaryOp::EqWildcard) ^ inverted,
    })
}

fn canonical_selector(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, DefinitionSite>,
    mut register: RegisterId,
) -> RegisterId {
    let mut seen = HashSet::default();
    while seen.insert(register) {
        let Some(SIRInstruction::Unary(destination, UnaryOp::Ident, source)) =
            defining_instruction(eu, definitions, register)
        else {
            break;
        };
        if eu.register_map.get(destination) != eu.register_map.get(source) {
            break;
        }
        register = *source;
    }
    register
}

fn exact_constant(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, DefinitionSite>,
    cache: &mut HashMap<RegisterId, Option<BigUint>>,
    register: RegisterId,
) -> Option<BigUint> {
    if let Some(value) = cache.get(&register) {
        return value.clone();
    }
    let value = exact_constant_inner(eu, definitions, cache, register, &mut HashSet::default());
    cache.insert(register, value.clone());
    value
}

fn exact_constant_inner(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, DefinitionSite>,
    cache: &mut HashMap<RegisterId, Option<BigUint>>,
    register: RegisterId,
    active: &mut HashSet<RegisterId>,
) -> Option<BigUint> {
    if let Some(value) = cache.get(&register) {
        return value.clone();
    }
    if !active.insert(register) {
        return None;
    }
    let width = eu.register_map.get(&register)?.width();
    if width == 0 {
        return None;
    }
    let result = match defining_instruction(eu, definitions, register)? {
        SIRInstruction::Imm(_, value) if value.mask.is_zero() => {
            Some(truncate(value.payload.clone(), width))
        }
        SIRInstruction::Unary(_, UnaryOp::Ident | UnaryOp::ToTwoState, source) => {
            exact_constant_inner(eu, definitions, cache, *source, active)
                .map(|value| truncate(value, width))
        }
        SIRInstruction::Binary(_, lhs, op, rhs) => {
            let lhs = exact_constant_inner(eu, definitions, cache, *lhs, active)?;
            let rhs = exact_constant_inner(eu, definitions, cache, *rhs, active)?;
            let value = match op {
                BinaryOp::Add => lhs + rhs,
                BinaryOp::Sub => {
                    let modulus = BigUint::one() << width;
                    (truncate(lhs, width) + &modulus - truncate(rhs, width)) % modulus
                }
                BinaryOp::Mul => lhs * rhs,
                BinaryOp::And => lhs & rhs,
                BinaryOp::Or => lhs | rhs,
                BinaryOp::Xor => lhs ^ rhs,
                BinaryOp::Shl => lhs << rhs.to_usize()?,
                BinaryOp::Shr => lhs >> rhs.to_usize()?,
                _ => return None,
            };
            Some(truncate(value, width))
        }
        SIRInstruction::Slice(_, source, offset, slice_width) => {
            let source = exact_constant_inner(eu, definitions, cache, *source, active)?;
            Some(truncate(source >> offset, *slice_width))
        }
        SIRInstruction::Concat(_, parts) => {
            let mut value = BigUint::zero();
            for part in parts {
                let part_width = eu.register_map.get(part)?.width();
                value = (value << part_width)
                    | truncate(
                        exact_constant_inner(eu, definitions, cache, *part, active)?,
                        part_width,
                    );
            }
            Some(truncate(value, width))
        }
        _ => None,
    };
    active.remove(&register);
    cache.insert(register, result.clone());
    result
}

struct PreparedRewrite {
    head: BlockId,
    continuation: BlockId,
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    register_types: Vec<(RegisterId, RegisterType)>,
}

fn apply_plans(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, plans: Vec<IndexedStorePlan>) -> bool {
    let mut next_register = eu
        .register_map
        .keys()
        .map(|register| register.0)
        .max()
        .unwrap_or(0);
    let mut rewrites = Vec::with_capacity(plans.len());
    for plan in plans {
        let Some(rewrite) = prepare_plan(eu, plan, &mut next_register) else {
            return false;
        };
        rewrites.push(rewrite);
    }

    for rewrite in rewrites {
        for (register, register_type) in rewrite.register_types {
            eu.register_map.insert(register, register_type);
        }
        let head = eu
            .blocks
            .get_mut(&rewrite.head)
            .expect("a prepared indexed-store head must remain present");
        head.instructions.extend(rewrite.instructions);
        head.terminator = SIRTerminator::Jump(rewrite.continuation, Vec::new());
    }

    remove_unreachable_blocks(eu);
    super::pass_vectorize_concat::remove_dead_definitions(eu);
    trim_register_types(eu);
    debug_assert_eq!(eu.verify_result(), Ok(()));
    true
}

fn prepare_plan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    plan: IndexedStorePlan,
    next_register: &mut usize,
) -> Option<PreparedRewrite> {
    let template = eu.blocks.get(&plan.template_arm)?;
    let (template_store, pure) = template.instructions.split_last()?;
    let SIRInstruction::Store(_, _, _, template_source, _, _) = template_store else {
        return None;
    };

    let mut mapping = HashMap::default();
    let mut cloned = Vec::with_capacity(pure.len() + 1);
    let mut new_types = Vec::with_capacity(pure.len());
    for instruction in pure {
        let old_destination = def_reg(instruction)?;
        let new_id = next_register.checked_add(1)?;
        *next_register = new_id;
        let destination = RegisterId(new_id);
        let register_type = eu.register_map.get(&old_destination).cloned()?;
        mapping.insert(old_destination, destination);
        let instruction = clone_with_mapping(instruction, &mapping)?;
        new_types.push((destination, register_type));
        cloned.push(instruction);
    }
    let source = mapping
        .get(template_source)
        .copied()
        .unwrap_or(*template_source);
    cloned.push(SIRInstruction::Store(
        plan.destination,
        SIROffset::Element {
            index: plan.selector,
            element_width: plan.element_width,
            bit_offset: 0,
            dynamic_bit_offset: None,
        },
        plan.element_width,
        source,
        Vec::new(),
        Vec::new(),
    ));

    Some(PreparedRewrite {
        head: plan.head,
        continuation: plan.continuation,
        instructions: cloned,
        register_types: new_types,
    })
}

fn clone_with_mapping(
    instruction: &SIRInstruction<RegionedAbsoluteAddr>,
    mapping: &HashMap<RegisterId, RegisterId>,
) -> Option<SIRInstruction<RegionedAbsoluteAddr>> {
    let register = |value: RegisterId| mapping.get(&value).copied().unwrap_or(value);
    let offset = |value: &SIROffset| remap_offset(value, mapping);
    Some(match instruction {
        SIRInstruction::Imm(destination, value) => {
            SIRInstruction::Imm(register(*destination), value.clone())
        }
        SIRInstruction::Binary(destination, lhs, op, rhs) => {
            SIRInstruction::Binary(register(*destination), register(*lhs), *op, register(*rhs))
        }
        SIRInstruction::Unary(destination, op, source) => {
            SIRInstruction::Unary(register(*destination), *op, register(*source))
        }
        SIRInstruction::Load(destination, address, memory_offset, width) => SIRInstruction::Load(
            register(*destination),
            *address,
            offset(memory_offset),
            *width,
        ),
        SIRInstruction::Concat(destination, sources) => SIRInstruction::Concat(
            register(*destination),
            sources.iter().copied().map(register).collect(),
        ),
        SIRInstruction::Slice(destination, source, bit_offset, width) => SIRInstruction::Slice(
            register(*destination),
            register(*source),
            *bit_offset,
            *width,
        ),
        SIRInstruction::Mux(destination, condition, true_value, false_value) => {
            SIRInstruction::Mux(
                register(*destination),
                register(*condition),
                register(*true_value),
                register(*false_value),
            )
        }
        _ => return None,
    })
}

fn remap_offset(offset: &SIROffset, mapping: &HashMap<RegisterId, RegisterId>) -> SIROffset {
    let register = |value: RegisterId| mapping.get(&value).copied().unwrap_or(value);
    match offset {
        SIROffset::Static(value) => SIROffset::Static(*value),
        SIROffset::Dynamic(value) => SIROffset::Dynamic(register(*value)),
        SIROffset::Element {
            index,
            element_width,
            bit_offset,
            dynamic_bit_offset,
        } => SIROffset::Element {
            index: register(*index),
            element_width: *element_width,
            bit_offset: *bit_offset,
            dynamic_bit_offset: dynamic_bit_offset.map(register),
        },
        SIROffset::PackedElements {
            bit_offset,
            element_width,
        } => SIROffset::PackedElements {
            bit_offset: *bit_offset,
            element_width: *element_width,
        },
    }
}

fn remove_unreachable_blocks(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let mut reachable = HashSet::default();
    let mut work = vec![eu.entry_block_id];
    while let Some(block_id) = work.pop() {
        if !reachable.insert(block_id) {
            continue;
        }
        let Some(block) = eu.blocks.get(&block_id) else {
            continue;
        };
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
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    eu.blocks.retain(|block, _| reachable.contains(block));
}

fn trim_register_types(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let mut retained = HashSet::default();
    for block in eu.blocks.values() {
        retained.extend(block.params.iter().copied());
        for instruction in &block.instructions {
            if let Some(destination) = def_reg(instruction) {
                retained.insert(destination);
            }
            retained.extend(instruction_uses(instruction));
        }
        retained.extend(terminator_uses(&block.terminator));
    }
    eu.register_map
        .retain(|register, _| retained.contains(register));
}

fn definition_sites(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, DefinitionSite> {
    eu.blocks
        .values()
        .flat_map(|block| {
            block
                .instructions
                .iter()
                .enumerate()
                .filter_map(move |(index, instruction)| {
                    def_reg(instruction).map(|register| {
                        (
                            register,
                            DefinitionSite {
                                block: block.id,
                                index,
                            },
                        )
                    })
                })
        })
        .collect()
}

fn defining_instruction<'a>(
    eu: &'a ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, DefinitionSite>,
    register: RegisterId,
) -> Option<&'a SIRInstruction<RegionedAbsoluteAddr>> {
    let site = definitions.get(&register)?;
    eu.blocks.get(&site.block)?.instructions.get(site.index)
}

fn register_use_blocks(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, HashSet<BlockId>> {
    let mut uses = HashMap::<RegisterId, HashSet<BlockId>>::default();
    for block in eu.blocks.values() {
        for instruction in &block.instructions {
            for register in instruction_uses(instruction) {
                uses.entry(register).or_default().insert(block.id);
            }
        }
        for register in terminator_uses(&block.terminator) {
            uses.entry(register).or_default().insert(block.id);
        }
    }
    uses
}

fn instruction_uses(instruction: &SIRInstruction<RegionedAbsoluteAddr>) -> Vec<RegisterId> {
    match instruction {
        SIRInstruction::Imm(..) => Vec::new(),
        SIRInstruction::Binary(_, lhs, _, rhs) => vec![*lhs, *rhs],
        SIRInstruction::Unary(_, _, source) | SIRInstruction::Slice(_, source, ..) => vec![*source],
        SIRInstruction::Load(_, _, offset, _) => {
            offset.dynamic_registers().into_iter().flatten().collect()
        }
        SIRInstruction::Store(_, offset, _, source, _, _) => offset
            .dynamic_registers()
            .into_iter()
            .flatten()
            .chain(std::iter::once(*source))
            .collect(),
        SIRInstruction::Commit(_, _, offset, _, _) => {
            offset.dynamic_registers().into_iter().flatten().collect()
        }
        SIRInstruction::Concat(_, sources)
        | SIRInstruction::RuntimeEvent { args: sources, .. }
        | SIRInstruction::CombCaptureEvent { args: sources, .. } => sources.clone(),
        SIRInstruction::Mux(_, condition, true_value, false_value) => {
            vec![*condition, *true_value, *false_value]
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => vec![*old, *new],
    }
}

fn terminator_uses(terminator: &SIRTerminator) -> Vec<RegisterId> {
    match terminator {
        SIRTerminator::Jump(_, arguments) => arguments.clone(),
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            let mut uses = vec![*cond];
            uses.extend(true_block.1.iter().copied());
            uses.extend(false_block.1.iter().copied());
            uses
        }
        SIRTerminator::Switch { selector, .. } => vec![*selector],
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

fn low_mask(width: usize) -> BigUint {
    (BigUint::one() << width) - BigUint::one()
}

fn truncate(value: BigUint, width: usize) -> BigUint {
    value & low_mask(width)
}

fn fits_width(value: &BigUint, width: usize) -> bool {
    value.is_zero() || value.bits() <= width as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{InstanceId, STABLE_REGION, WORKING_REGION};
    use veryl_analyzer::ir::VarId;

    const SELECTOR_WIDTH: usize = 3;
    const ELEMENT_WIDTH: usize = 64;
    const ELEMENT_COUNT: usize = 1 << SELECTOR_WIDTH;

    fn address(region: u32, raw: u32) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region,
            instance_id: InstanceId(0),
            var_id: VarId::from_raw(raw),
        }
    }

    fn destination() -> RegionedAbsoluteAddr {
        address(WORKING_REGION, 1)
    }

    fn data_address() -> RegionedAbsoluteAddr {
        address(STABLE_REGION, 2)
    }

    fn bit(width: usize) -> RegisterType {
        RegisterType::Bit {
            width,
            signed: false,
        }
    }

    fn logic(width: usize) -> RegisterType {
        RegisterType::Logic { width }
    }

    struct Fixture {
        eu: ExecutionUnit<RegionedAbsoluteAddr>,
        selector: RegisterId,
        arms: Vec<BlockId>,
    }

    struct Registers {
        next: usize,
        types: HashMap<RegisterId, RegisterType>,
    }

    impl Registers {
        fn new() -> Self {
            Self {
                next: 0,
                types: HashMap::default(),
            }
        }

        fn alloc(&mut self, register_type: RegisterType) -> RegisterId {
            let register = RegisterId(self.next);
            self.next += 1;
            self.types.insert(register, register_type);
            register
        }
    }

    fn fixture(stage_count: usize) -> Fixture {
        fixture_ladders(&[stage_count])
    }

    fn fixture_ladders(stage_counts: &[usize]) -> Fixture {
        assert!(!stage_counts.is_empty());
        assert!(stage_counts.iter().all(|&count| count <= ELEMENT_COUNT));
        let mut registers = Registers::new();
        let selector = registers.alloc(logic(SELECTOR_WIDTH));
        let mask = registers.alloc(logic(SELECTOR_WIDTH));
        let mut blocks = HashMap::default();
        let mut arms = Vec::new();
        let mut block_base = 0usize;

        for (ladder_index, &stage_count) in stage_counts.iter().enumerate() {
            let final_block = BlockId(block_base + stage_count * 3);
            for key_value in 0..stage_count {
                let decision = BlockId(block_base + key_value * 3);
                let arm = BlockId(block_base + key_value * 3 + 1);
                let empty = BlockId(block_base + key_value * 3 + 2);
                let next = if key_value + 1 == stage_count {
                    final_block
                } else {
                    BlockId(block_base + (key_value + 1) * 3)
                };
                arms.push(arm);

                let key = registers.alloc(logic(SELECTOR_WIDTH));
                let masked = registers.alloc(logic(SELECTOR_WIDTH));
                let alias = registers.alloc(logic(SELECTOR_WIDTH));
                let equality = registers.alloc(logic(1));
                let condition = registers.alloc(bit(1));
                let mut decision_instructions = Vec::new();
                if ladder_index == 0 && key_value == 0 {
                    decision_instructions.push(SIRInstruction::Imm(
                        mask,
                        SIRValue::new((ELEMENT_COUNT - 1) as u64),
                    ));
                }
                decision_instructions.extend([
                    SIRInstruction::Imm(key, SIRValue::new(key_value as u64)),
                    SIRInstruction::Binary(masked, key, BinaryOp::And, mask),
                    SIRInstruction::Unary(alias, UnaryOp::Ident, masked),
                    SIRInstruction::Binary(equality, selector, BinaryOp::Eq, alias),
                    SIRInstruction::Unary(condition, UnaryOp::ToTwoState, equality),
                ]);
                blocks.insert(
                    decision,
                    BasicBlock {
                        id: decision,
                        params: if ladder_index == 0 && key_value == 0 {
                            vec![selector]
                        } else {
                            Vec::new()
                        },
                        instructions: decision_instructions,
                        terminator: SIRTerminator::Branch {
                            cond: condition,
                            true_block: (arm, Vec::new()),
                            false_block: (empty, Vec::new()),
                        },
                    },
                );

                let value = registers.alloc(logic(ELEMENT_WIDTH));
                blocks.insert(
                    arm,
                    BasicBlock {
                        id: arm,
                        params: Vec::new(),
                        instructions: vec![
                            SIRInstruction::Load(
                                value,
                                data_address(),
                                SIROffset::Static(0),
                                ELEMENT_WIDTH,
                            ),
                            SIRInstruction::Store(
                                destination(),
                                SIROffset::Static(key_value * ELEMENT_WIDTH),
                                ELEMENT_WIDTH,
                                value,
                                Vec::new(),
                                Vec::new(),
                            ),
                        ],
                        terminator: SIRTerminator::Jump(next, Vec::new()),
                    },
                );
                blocks.insert(
                    empty,
                    BasicBlock {
                        id: empty,
                        params: Vec::new(),
                        instructions: Vec::new(),
                        terminator: SIRTerminator::Jump(next, Vec::new()),
                    },
                );
            }
            let next_ladder = BlockId(final_block.0 + 1);
            blocks.insert(
                final_block,
                BasicBlock {
                    id: final_block,
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: if ladder_index + 1 == stage_counts.len() {
                        SIRTerminator::Return
                    } else {
                        SIRTerminator::Jump(next_ladder, Vec::new())
                    },
                },
            );
            block_base = next_ladder.0;
        }

        let eu = ExecutionUnit {
            blocks,
            entry_block_id: BlockId(0),
            register_map: registers.types,
        };
        eu.verify_result().unwrap();
        Fixture { eu, selector, arms }
    }

    fn pass() -> IndexedStoreRecoveryPass {
        IndexedStoreRecoveryPass {
            arrays: [(
                destination().absolute_addr(),
                ArrayShape {
                    element_width: ELEMENT_WIDTH,
                    element_count: ELEMENT_COUNT,
                },
            )]
            .into_iter()
            .collect(),
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Write {
        address: RegionedAbsoluteAddr,
        offset: usize,
        width: usize,
        value: u64,
    }

    fn execute(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        selector: RegisterId,
        value: u64,
    ) -> Vec<Write> {
        let mut registers = HashMap::default();
        registers.insert(selector, value);
        let mut writes = Vec::new();
        let mut current = eu.entry_block_id;
        let mut steps = 0usize;
        loop {
            steps += 1;
            assert!(steps <= eu.blocks.len() + 1, "test SIR did not terminate");
            let block = &eu.blocks[&current];
            for instruction in &block.instructions {
                match instruction {
                    SIRInstruction::Imm(destination, constant) => {
                        registers.insert(*destination, constant.payload.to_u64().unwrap_or(0));
                    }
                    SIRInstruction::Binary(destination, lhs, operation, rhs) => {
                        let lhs = registers[lhs];
                        let rhs = registers[rhs];
                        let result = match operation {
                            BinaryOp::And => lhs & rhs,
                            BinaryOp::Eq => u64::from(lhs == rhs),
                            other => panic!("unsupported test binary operation {other:?}"),
                        };
                        registers.insert(*destination, result);
                    }
                    SIRInstruction::Unary(destination, operation, source) => {
                        assert!(matches!(operation, UnaryOp::Ident | UnaryOp::ToTwoState));
                        registers.insert(*destination, registers[source]);
                    }
                    SIRInstruction::Load(destination, _, SIROffset::Static(0), width) => {
                        assert_eq!(*width, ELEMENT_WIDTH);
                        registers.insert(*destination, 0x5a5a_1234_dead_beef);
                    }
                    SIRInstruction::Store(address, offset, width, source, triggers, captures) => {
                        assert!(triggers.is_empty() && captures.is_empty());
                        let offset = match offset {
                            SIROffset::Static(offset) => *offset,
                            SIROffset::Element {
                                index,
                                element_width,
                                bit_offset,
                                dynamic_bit_offset: None,
                            } => registers[index] as usize * element_width + bit_offset,
                            other => panic!("unsupported test store offset {other:?}"),
                        };
                        writes.push(Write {
                            address: *address,
                            offset,
                            width: *width,
                            value: registers[source],
                        });
                    }
                    other => panic!("unsupported test instruction {other:?}"),
                }
            }
            match &block.terminator {
                SIRTerminator::Jump(target, arguments) => {
                    assert!(arguments.is_empty());
                    current = *target;
                }
                SIRTerminator::Branch {
                    cond,
                    true_block,
                    false_block,
                } => {
                    let target = if registers[cond] == 0 {
                        false_block
                    } else {
                        true_block
                    };
                    assert!(target.1.is_empty());
                    current = target.0;
                }
                SIRTerminator::Switch { .. } => {
                    panic!("unexpected Switch in indexed-store test")
                }
                SIRTerminator::Return => return writes,
                SIRTerminator::Error(code) => panic!("unexpected Error({code})"),
            }
        }
    }

    #[test]
    fn recovers_full_domain_unpacked_array_store_and_preserves_every_selector() {
        let fixture = fixture(ELEMENT_COUNT);
        let original = fixture.eu.clone();
        let mut optimized = fixture.eu;

        pass().run(&mut optimized, &PassOptions::default());

        optimized.verify_result().unwrap();
        for selector_value in 0..ELEMENT_COUNT as u64 {
            assert_eq!(
                execute(&original, fixture.selector, selector_value),
                execute(&optimized, fixture.selector, selector_value),
                "selector={selector_value}"
            );
        }
        assert_eq!(optimized.blocks.len(), 2);
        assert_eq!(
            optimized
                .blocks
                .values()
                .filter(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
                .count(),
            0
        );
        let stores = optimized
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| match instruction {
                SIRInstruction::Store(address, offset, width, _, _, _) => {
                    Some((*address, offset, *width))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(stores.len(), 1);
        assert_eq!(stores[0].0, destination());
        assert_eq!(stores[0].2, ELEMENT_WIDTH);
        assert!(matches!(
            stores[0].1,
            SIROffset::Element {
                index,
                element_width: ELEMENT_WIDTH,
                bit_offset: 0,
                dynamic_bit_offset: None,
            } if *index == fixture.selector
        ));
    }

    #[test]
    fn recovers_multiple_disjoint_ladders_from_one_cfg_analysis() {
        let fixture = fixture_ladders(&[ELEMENT_COUNT, ELEMENT_COUNT]);
        let original = fixture.eu.clone();
        let mut optimized = fixture.eu;

        pass().run(&mut optimized, &PassOptions::default());

        optimized.verify_result().unwrap();
        for selector_value in 0..ELEMENT_COUNT as u64 {
            assert_eq!(
                execute(&original, fixture.selector, selector_value),
                execute(&optimized, fixture.selector, selector_value),
                "selector={selector_value}"
            );
        }
        assert_eq!(
            optimized
                .blocks
                .values()
                .filter(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
                .count(),
            0
        );
        assert_eq!(
            optimized
                .blocks
                .values()
                .flat_map(|block| &block.instructions)
                .filter(|instruction| matches!(instruction, SIRInstruction::Store(..)))
                .count(),
            2
        );
    }

    #[test]
    fn leaves_an_incomplete_selector_domain_unchanged() {
        let mut fixture = fixture(ELEMENT_COUNT - 1);
        let original_blocks = fixture.eu.blocks.len();

        pass().run(&mut fixture.eu, &PassOptions::default());

        fixture.eu.verify_result().unwrap();
        assert_eq!(fixture.eu.blocks.len(), original_blocks);
        assert!(
            fixture
                .eu
                .blocks
                .values()
                .any(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
        );
    }

    #[test]
    fn leaves_non_equivalent_value_arms_unchanged() {
        let mut fixture = fixture(ELEMENT_COUNT);
        let arm = fixture.arms[3];
        let SIRInstruction::Load(_, source, _, _) =
            &mut fixture.eu.blocks.get_mut(&arm).unwrap().instructions[0]
        else {
            unreachable!();
        };
        *source = address(STABLE_REGION, 99);
        let original_blocks = fixture.eu.blocks.len();

        pass().run(&mut fixture.eu, &PassOptions::default());

        fixture.eu.verify_result().unwrap();
        assert_eq!(fixture.eu.blocks.len(), original_blocks);
    }

    #[test]
    fn leaves_an_observable_arm_effect_unchanged() {
        let mut fixture = fixture(ELEMENT_COUNT);
        let arm = fixture.arms[4];
        let value = match fixture.eu.blocks[&arm].instructions[0] {
            SIRInstruction::Load(value, ..) => value,
            _ => unreachable!(),
        };
        fixture
            .eu
            .blocks
            .get_mut(&arm)
            .unwrap()
            .instructions
            .insert(
                1,
                SIRInstruction::RuntimeEvent {
                    site_id: 17,
                    args: vec![value],
                },
            );
        let original_blocks = fixture.eu.blocks.len();

        pass().run(&mut fixture.eu, &PassOptions::default());

        fixture.eu.verify_result().unwrap();
        assert_eq!(fixture.eu.blocks.len(), original_blocks);
    }

    #[test]
    fn leaves_four_state_equality_ladder_unchanged() {
        let mut fixture = fixture(ELEMENT_COUNT);
        let original_blocks = fixture.eu.blocks.len();
        let options = PassOptions {
            four_state: true,
            ..PassOptions::default()
        };

        pass().run(&mut fixture.eu, &options);

        fixture.eu.verify_result().unwrap();
        assert_eq!(fixture.eu.blocks.len(), original_blocks);
    }
}
