//! Recover bit-count idioms after the Veryl analyzer has expanded procedural
//! loops.  The source loop is gone at this point, but its recurrence remains
//! explicit in SIR and can be replaced without guessing source intent.

use super::pass_manager::ExecutionUnitPass;
use super::shared::{def_reg, sir_value_to_u64};
use crate::PassOptions;
use crate::ir::*;
use crate::{HashMap, HashSet};

const MIN_CHAIN_LEN: usize = 4;

pub(in crate::optimizer) struct LoopIdiomPass;

impl ExecutionUnitPass for LoopIdiomPass {
    fn name(&self) -> &'static str {
        "loop_idiom"
    }

    fn handles_calls(&self) -> bool {
        true
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        // The recovered predicates are two-state booleans.  Four-state mux
        // merging has additional X/Z semantics and is deliberately left
        // untouched until those semantics are represented by the idiom op.
        if options.four_state {
            return;
        }

        let mut next_reg = eu.register_map.keys().map(|reg| reg.0).max().unwrap_or(0);
        for block in eu.blocks.values_mut() {
            recover_block(&mut block.instructions, &mut eu.register_map, &mut next_reg);
            reuse_growing_or_reductions(&mut block.instructions, &eu.register_map);
        }
        prune_dead_pure_instructions(eu);
    }
}

/// Reuse an already-computed reduction when vectorization exposes a growing
/// predicate prefix:
///
/// ```text
/// previous = Or(Concat([p[n-1], ..., p[0]]))
/// current  = Or(Concat([p[n], p[n-1], ..., p[0]]))
/// ```
///
/// In two-state mode `current` is exactly `p[n] | previous`.  Procedural
/// accumulator loops otherwise rebuild every preceding predicate at every
/// step, turning a linear reduction into quadratic generated work.
fn reuse_growing_or_reductions(
    instructions: &mut [SIRInstruction<RegionedAbsoluteAddr>],
    register_map: &HashMap<RegisterId, RegisterType>,
) -> bool {
    let defs = instruction_defs(instructions);
    let mut reductions = HashMap::<Vec<RegisterId>, RegisterId>::default();
    let mut replacements = Vec::new();

    for (index, inst) in instructions.iter().enumerate() {
        let SIRInstruction::Unary(dst, UnaryOp::Or, concat) = inst else {
            continue;
        };
        if register_map.get(dst).map(RegisterType::width) != Some(1) {
            continue;
        }
        let Some(&concat_index) = defs.get(concat) else {
            continue;
        };
        let SIRInstruction::Concat(_, parts) = &instructions[concat_index] else {
            continue;
        };
        if parts.len() >= 2
            && register_map.get(&parts[0]).map(RegisterType::width) == Some(1)
            && let Some(&previous) = reductions.get(&parts[1..])
            && register_map.get(&previous).map(RegisterType::width) == Some(1)
        {
            replacements.push((index, *dst, parts[0], previous));
        }
        reductions.insert(parts.clone(), *dst);
    }

    for (index, dst, new_predicate, previous) in replacements.iter().copied() {
        instructions[index] = SIRInstruction::Binary(dst, new_predicate, BinaryOp::Or, previous);
    }
    !replacements.is_empty()
}

#[derive(Clone, Copy)]
struct BitTerm {
    predicate: RegisterId,
    source: Option<(RegisterId, usize)>,
}

struct CountReplacement {
    root_idx: usize,
    dst: RegisterId,
    op: UnaryOp,
    input: CountInput,
}

enum CountInput {
    Register(RegisterId),
    Predicates(Vec<RegisterId>),
}

fn recover_block(
    instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    register_map: &mut HashMap<RegisterId, RegisterType>,
    next_reg: &mut usize,
) -> bool {
    let defs = instruction_defs(instructions);
    let accumulator_children = accumulator_children(instructions, &defs);
    let mut replacements = Vec::new();

    for (root_idx, inst) in instructions.iter().enumerate() {
        let Some(dst) = def_reg(inst) else {
            continue;
        };
        if accumulator_children.contains(&dst) {
            continue;
        }

        if matches!(inst, SIRInstruction::Mux(..))
            && let Some(replacement) =
                match_priority_count(instructions, &defs, register_map, root_idx, dst)
        {
            replacements.push(replacement);
            continue;
        }

        if matches!(
            inst,
            SIRInstruction::Mux(..) | SIRInstruction::Binary(_, _, BinaryOp::Add, _)
        ) && let Some(replacement) =
            match_popcount(instructions, &defs, register_map, root_idx, dst)
        {
            replacements.push(replacement);
        }
    }

    if replacements.is_empty() {
        return false;
    }

    replacements.sort_unstable_by_key(|replacement| replacement.root_idx);
    for replacement in replacements.into_iter().rev() {
        let inserts_concat = matches!(&replacement.input, CountInput::Predicates(_));
        let source = match replacement.input {
            CountInput::Register(source) => source,
            CountInput::Predicates(predicates) => {
                *next_reg += 1;
                while register_map.contains_key(&RegisterId(*next_reg)) {
                    *next_reg += 1;
                }
                let concat = RegisterId(*next_reg);
                register_map.insert(
                    concat,
                    RegisterType::Bit {
                        width: predicates.len(),
                        signed: false,
                    },
                );
                instructions.insert(
                    replacement.root_idx,
                    SIRInstruction::Concat(concat, predicates),
                );
                concat
            }
        };

        let root_idx = if inserts_concat {
            replacement.root_idx + 1
        } else {
            replacement.root_idx
        };
        instructions[root_idx] = SIRInstruction::Unary(replacement.dst, replacement.op, source);
    }
    true
}

fn instruction_defs(
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
) -> HashMap<RegisterId, usize> {
    instructions
        .iter()
        .enumerate()
        .filter_map(|(idx, inst)| def_reg(inst).map(|dst| (dst, idx)))
        .collect()
}

fn accumulator_children(
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
    defs: &HashMap<RegisterId, usize>,
) -> HashSet<RegisterId> {
    let mut children = HashSet::default();
    for inst in instructions {
        match inst {
            SIRInstruction::Mux(_, _, then_value, else_value) => {
                if defs
                    .get(else_value)
                    .is_some_and(|&idx| matches!(instructions[idx], SIRInstruction::Mux(..)))
                {
                    children.insert(*else_value);
                }
                if defs
                    .get(then_value)
                    .is_some_and(|&idx| matches!(instructions[idx], SIRInstruction::Mux(..)))
                {
                    children.insert(*then_value);
                }
            }
            SIRInstruction::Binary(_, lhs, BinaryOp::Add, rhs) => {
                if defs.get(lhs).is_some_and(|&idx| {
                    matches!(
                        instructions[idx],
                        SIRInstruction::Binary(_, _, BinaryOp::Add, _)
                    )
                }) {
                    children.insert(*lhs);
                }
                if defs.get(rhs).is_some_and(|&idx| {
                    matches!(
                        instructions[idx],
                        SIRInstruction::Binary(_, _, BinaryOp::Add, _)
                    )
                }) {
                    children.insert(*rhs);
                }
            }
            _ => {}
        }
    }
    children
}

fn match_popcount(
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
    defs: &HashMap<RegisterId, usize>,
    register_map: &HashMap<RegisterId, RegisterType>,
    root_idx: usize,
    root: RegisterId,
) -> Option<CountReplacement> {
    let root_width = register_map.get(&root)?.width();
    let terms = match instructions.get(root_idx)? {
        SIRInstruction::Mux(..) => {
            collect_conditional_increments(instructions, defs, register_map, root, root_width)?
        }
        SIRInstruction::Binary(_, _, BinaryOp::Add, _) => {
            collect_additive_bits(instructions, defs, register_map, root, root_width)?
        }
        _ => return None,
    };
    if terms.len() < MIN_CHAIN_LEN || !width_can_represent(root_width, terms.len()) {
        return None;
    }

    let input = common_complete_source(&terms, register_map)
        .map(CountInput::Register)
        .unwrap_or_else(|| {
            CountInput::Predicates(terms.into_iter().map(|term| term.predicate).collect())
        });
    Some(CountReplacement {
        root_idx,
        dst: root,
        op: UnaryOp::PopCount,
        input,
    })
}

fn collect_conditional_increments(
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
    defs: &HashMap<RegisterId, usize>,
    register_map: &HashMap<RegisterId, RegisterType>,
    mut cursor: RegisterId,
    accumulator_width: usize,
) -> Option<Vec<BitTerm>> {
    let mut terms = Vec::new();
    loop {
        let &idx = defs.get(&cursor)?;
        let SIRInstruction::Mux(dst, cond, then_value, else_value) = instructions[idx] else {
            return None;
        };
        if dst != cursor || register_map.get(&dst)?.width() != accumulator_width {
            return None;
        }
        match_increment(instructions, defs, then_value, else_value)?;
        if register_map.get(&cond)?.width() != 1 {
            return None;
        }
        terms.push(BitTerm {
            predicate: cond,
            source: resolve_bit_source(instructions, defs, register_map, cond),
        });
        cursor = else_value;
        if is_zero_of_width(instructions, defs, register_map, cursor, accumulator_width) {
            break;
        }
    }
    Some(terms)
}

fn match_increment(
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
    defs: &HashMap<RegisterId, usize>,
    value: RegisterId,
    accumulator: RegisterId,
) -> Option<()> {
    let &idx = defs.get(&value)?;
    let SIRInstruction::Binary(_, lhs, BinaryOp::Add, rhs) = instructions[idx] else {
        return None;
    };
    if lhs == accumulator && imm_u64(instructions, defs, rhs) == Some(1)
        || rhs == accumulator && imm_u64(instructions, defs, lhs) == Some(1)
    {
        Some(())
    } else {
        None
    }
}

fn collect_additive_bits(
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
    defs: &HashMap<RegisterId, usize>,
    register_map: &HashMap<RegisterId, RegisterType>,
    mut cursor: RegisterId,
    accumulator_width: usize,
) -> Option<Vec<BitTerm>> {
    let mut terms = Vec::new();
    loop {
        if is_zero_of_width(instructions, defs, register_map, cursor, accumulator_width) {
            break;
        }
        let &idx = defs.get(&cursor)?;
        let SIRInstruction::Binary(dst, lhs, BinaryOp::Add, rhs) = instructions[idx] else {
            return None;
        };
        if dst != cursor || register_map.get(&dst)?.width() != accumulator_width {
            return None;
        }
        let lhs_term = resolve_extended_bit(instructions, defs, register_map, lhs);
        let rhs_term = resolve_extended_bit(instructions, defs, register_map, rhs);
        match (lhs_term, rhs_term) {
            (Some(term), None) => {
                terms.push(term);
                cursor = rhs;
            }
            (None, Some(term)) => {
                terms.push(term);
                cursor = lhs;
            }
            _ => return None,
        }
    }
    Some(terms)
}

fn resolve_extended_bit(
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
    defs: &HashMap<RegisterId, usize>,
    register_map: &HashMap<RegisterId, RegisterType>,
    reg: RegisterId,
) -> Option<BitTerm> {
    if register_map.get(&reg)?.width() == 1 {
        return Some(BitTerm {
            predicate: reg,
            source: resolve_bit_source(instructions, defs, register_map, reg),
        });
    }
    let &idx = defs.get(&reg)?;
    match &instructions[idx] {
        SIRInstruction::Unary(_, UnaryOp::Ident, inner) => {
            resolve_extended_bit(instructions, defs, register_map, *inner)
        }
        SIRInstruction::Concat(_, args) => {
            let mut term = None;
            for &arg in args {
                if is_zero(instructions, defs, arg) {
                    continue;
                }
                let candidate = resolve_extended_bit(instructions, defs, register_map, arg)?;
                if term.is_some() {
                    return None;
                }
                term = Some(candidate);
            }
            term
        }
        _ => None,
    }
}

fn match_priority_count(
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
    defs: &HashMap<RegisterId, usize>,
    register_map: &HashMap<RegisterId, RegisterType>,
    root_idx: usize,
    root: RegisterId,
) -> Option<CountReplacement> {
    let mut cursor = root;
    let mut items = Vec::new();
    let mut default_reg = None;
    let mut default_value = None;
    let mut guarded = None;

    loop {
        let &mux_idx = defs.get(&cursor)?;
        let SIRInstruction::Mux(dst, cond, then_value, else_value) = instructions[mux_idx] else {
            return None;
        };
        if dst != cursor {
            return None;
        }
        let (is_guarded, guard, matched_default) =
            split_priority_condition(instructions, defs, register_map, cond, else_value);
        if let Some(previous) = guarded {
            if previous != is_guarded {
                return None;
            }
        } else {
            guarded = Some(is_guarded);
        }
        if let Some(matched_default) = matched_default {
            if let Some(previous) = default_reg {
                if previous != matched_default {
                    return None;
                }
            } else {
                default_reg = Some(matched_default);
                default_value = imm_u64(instructions, defs, matched_default);
            }
        }

        let then_value = imm_u64(instructions, defs, then_value)? as usize;
        let (source, bit_index) = resolve_bit_source(instructions, defs, register_map, guard)?;
        items.push((then_value, source, bit_index));
        cursor = else_value;
        if !matches!(
            defs.get(&cursor).map(|idx| &instructions[*idx]),
            Some(SIRInstruction::Mux(..))
        ) {
            break;
        }
    }

    if guarded == Some(false) {
        default_reg = Some(cursor);
        default_value = imm_u64(instructions, defs, cursor);
    }
    let width = default_value? as usize;
    if items.len() < MIN_CHAIN_LEN
        || items.len() != width
        || Some(cursor) != default_reg
        || !width_can_represent(register_map.get(&root)?.width(), width)
    {
        return None;
    }
    let source = items.first()?.1;
    if register_map.get(&source)?.width() != width || items.iter().any(|item| item.1 != source) {
        return None;
    }

    // Items are collected from the final mux back toward the initial state.
    // Match the exact stage order; accepting only the set of bit/value pairs
    // would silently change priority for a permuted chain.
    let op = if guarded == Some(true)
        && items
            .iter()
            .enumerate()
            .all(|(j, &(value, _, bit))| value == width - 1 - j && bit == j)
    {
        UnaryOp::CountLeadingZeros
    } else if guarded == Some(true)
        && items
            .iter()
            .enumerate()
            .all(|(j, &(value, _, bit))| value == width - 1 - j && bit == width - 1 - j)
    {
        UnaryOp::CountTrailingZeros
    } else if guarded == Some(false)
        && items
            .iter()
            .enumerate()
            .all(|(j, &(value, _, bit))| value == j && bit == width - 1 - j)
    {
        UnaryOp::CountLeadingZeros
    } else if guarded == Some(false)
        && items
            .iter()
            .enumerate()
            .all(|(j, &(value, _, bit))| value == j && bit == j)
    {
        UnaryOp::CountTrailingZeros
    } else {
        return None;
    };

    Some(CountReplacement {
        root_idx,
        dst: root,
        op,
        input: CountInput::Register(source),
    })
}

fn split_priority_condition(
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
    defs: &HashMap<RegisterId, usize>,
    register_map: &HashMap<RegisterId, RegisterType>,
    cond: RegisterId,
    accumulator: RegisterId,
) -> (bool, RegisterId, Option<RegisterId>) {
    let cond = unwrap_one_bit_procedural_truth(instructions, defs, register_map, cond);
    let Some(&idx) = defs.get(&cond) else {
        return (false, cond, None);
    };
    let SIRInstruction::Binary(_, lhs, op @ (BinaryOp::And | BinaryOp::LogicAnd), rhs) =
        instructions[idx]
    else {
        return (false, cond, None);
    };
    let _ = op;
    if let Some(default) = match_accumulator_default(instructions, defs, lhs, accumulator) {
        (true, rhs, Some(default))
    } else if let Some(default) = match_accumulator_default(instructions, defs, rhs, accumulator) {
        (true, lhs, Some(default))
    } else {
        (false, cond, None)
    }
}

fn match_accumulator_default(
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
    defs: &HashMap<RegisterId, usize>,
    candidate: RegisterId,
    accumulator: RegisterId,
) -> Option<RegisterId> {
    let &idx = defs.get(&candidate)?;
    let SIRInstruction::Binary(_, lhs, BinaryOp::Eq, rhs) = instructions[idx] else {
        return None;
    };
    if lhs == accumulator && imm_u64(instructions, defs, rhs).is_some() {
        Some(rhs)
    } else if rhs == accumulator && imm_u64(instructions, defs, lhs).is_some() {
        Some(lhs)
    } else {
        None
    }
}

fn resolve_bit_source(
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
    defs: &HashMap<RegisterId, usize>,
    register_map: &HashMap<RegisterId, RegisterType>,
    reg: RegisterId,
) -> Option<(RegisterId, usize)> {
    let reg = unwrap_one_bit_procedural_truth(instructions, defs, register_map, reg);
    let &idx = defs.get(&reg)?;
    match instructions[idx] {
        SIRInstruction::Slice(_, source, offset, 1) => Some((source, offset)),
        SIRInstruction::Unary(_, UnaryOp::Ident, inner) => {
            resolve_bit_source(instructions, defs, register_map, inner)
        }
        SIRInstruction::Binary(_, lhs, BinaryOp::Eq, rhs) => {
            if imm_u64(instructions, defs, lhs) == Some(1) {
                resolve_bit_source(instructions, defs, register_map, rhs)
            } else if imm_u64(instructions, defs, rhs) == Some(1) {
                resolve_bit_source(instructions, defs, register_map, lhs)
            } else {
                None
            }
        }
        SIRInstruction::Binary(_, lhs, BinaryOp::And, rhs) => {
            let shifted = if imm_u64(instructions, defs, lhs) == Some(1) {
                rhs
            } else if imm_u64(instructions, defs, rhs) == Some(1) {
                lhs
            } else {
                return None;
            };
            let Some(&shift_idx) = defs.get(&shifted) else {
                return (register_map.get(&shifted)?.width() == 1).then_some((shifted, 0));
            };
            match instructions[shift_idx] {
                SIRInstruction::Binary(_, source, BinaryOp::Shr, amount) => {
                    Some((source, imm_u64(instructions, defs, amount)? as usize))
                }
                _ if register_map.get(&shifted)?.width() == 1 => Some((shifted, 0)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// In two-state lowering, procedural control represents a condition as
/// `ToTwoState(Or(cond))`. The loop-idiom pass is disabled in four-state mode,
/// so this exact pair is an identity when `cond` is already one bit. A wider
/// reduction must remain visible because it is not lane-wise.
fn unwrap_one_bit_procedural_truth(
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
    defs: &HashMap<RegisterId, usize>,
    register_map: &HashMap<RegisterId, RegisterType>,
    reg: RegisterId,
) -> RegisterId {
    let Some(&to_two_state_idx) = defs.get(&reg) else {
        return reg;
    };
    let SIRInstruction::Unary(_, UnaryOp::ToTwoState, truth) = instructions[to_two_state_idx]
    else {
        return reg;
    };
    let Some(&truth_idx) = defs.get(&truth) else {
        return reg;
    };
    let SIRInstruction::Unary(_, UnaryOp::Or, inner) = instructions[truth_idx] else {
        return reg;
    };
    if register_map.get(&inner).map(RegisterType::width) == Some(1) {
        inner
    } else {
        reg
    }
}

fn common_complete_source(
    terms: &[BitTerm],
    register_map: &HashMap<RegisterId, RegisterType>,
) -> Option<RegisterId> {
    let (source, _) = terms.first()?.source?;
    let width = register_map.get(&source)?.width();
    if width != terms.len() {
        return None;
    }
    let mut seen = vec![false; width];
    for term in terms {
        let (term_source, bit) = term.source?;
        if term_source != source || bit >= width || seen[bit] {
            return None;
        }
        seen[bit] = true;
    }
    Some(source)
}

fn width_can_represent(width: usize, maximum: usize) -> bool {
    width >= usize::BITS as usize || maximum < (1usize << width)
}

fn is_zero_of_width(
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
    defs: &HashMap<RegisterId, usize>,
    register_map: &HashMap<RegisterId, RegisterType>,
    reg: RegisterId,
    width: usize,
) -> bool {
    register_map.get(&reg).is_some_and(|ty| ty.width() == width) && is_zero(instructions, defs, reg)
}

fn is_zero(
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
    defs: &HashMap<RegisterId, usize>,
    reg: RegisterId,
) -> bool {
    imm_u64(instructions, defs, reg) == Some(0)
}

fn imm_u64(
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
    defs: &HashMap<RegisterId, usize>,
    reg: RegisterId,
) -> Option<u64> {
    let &idx = defs.get(&reg)?;
    let SIRInstruction::Imm(_, value) = &instructions[idx] else {
        return None;
    };
    sir_value_to_u64(value)
}

fn instruction_uses(inst: &SIRInstruction<RegionedAbsoluteAddr>, out: &mut Vec<RegisterId>) {
    inst.for_each_use(|register| out.push(register));
}

fn terminator_uses(terminator: &SIRTerminator, out: &mut Vec<RegisterId>) {
    match terminator {
        SIRTerminator::Jump(_, args) => out.extend(args.iter().copied()),
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            out.push(*cond);
            out.extend(true_block.1.iter().copied());
            out.extend(false_block.1.iter().copied());
        }
        SIRTerminator::Switch { selector, .. } => out.push(*selector),
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}

fn instruction_has_side_effect(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    inst.has_side_effects()
}

fn prune_dead_pure_instructions(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let mut defs = HashMap::default();
    for (block_id, block) in &eu.blocks {
        for (idx, inst) in block.instructions.iter().enumerate() {
            if let Some(dst) = def_reg(inst) {
                defs.insert(dst, (*block_id, idx));
            }
        }
    }

    let mut work = Vec::new();
    for block in eu.blocks.values() {
        terminator_uses(&block.terminator, &mut work);
        for inst in &block.instructions {
            if instruction_has_side_effect(inst) {
                instruction_uses(inst, &mut work);
            }
        }
    }

    let mut live = HashSet::default();
    while let Some(reg) = work.pop() {
        if !live.insert(reg) {
            continue;
        }
        if let Some(&(block_id, idx)) = defs.get(&reg) {
            instruction_uses(&eu.blocks[&block_id].instructions[idx], &mut work);
        }
    }

    for block in eu.blocks.values_mut() {
        block.instructions.retain(|inst| {
            instruction_has_side_effect(inst) || def_reg(inst).is_none_or(|dst| live.contains(&dst))
        });
    }
}

#[cfg(test)]
mod tests;
