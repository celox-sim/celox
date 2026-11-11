//! Recover a direct packed array update from a full-domain equality scan.
//!
//! A packed array assignment can become a pure scan that selects one element
//! with `index == selector` and carries an otherwise unchanged packed value.
//! Prove that a finite unit-stride loop visits every value of an unsigned
//! selector exactly once. Then perform its masked update at `selector * width`
//! directly, retaining all other bits and shared literal definitions.
//! The native caller excludes four-state mode before invoking this rewrite.
use super::shared::{def_reg, sir_value_to_u64};
use super::sir_analysis::instruction_uses;
use crate::ir::*;
use crate::{HashMap, HashSet};

pub(super) fn run(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) -> usize {
    let mut ids = eu.blocks.values().filter(|block| {
        block.params.len() == 3 && matches!(&block.terminator,
            SIRTerminator::Branch { true_block, false_block, .. }
            if true_block.0 == block.id && false_block.0 != block.id && false_block.1.len() == 1
        )
    }).map(|block| block.id).collect::<Vec<_>>();
    if ids.is_empty() {
        return 0;
    }
    ids.sort_unstable();
    let mut constants = HashMap::default();
    let mut users = HashMap::<RegisterId, Option<BlockId>>::default();
    for block in eu.blocks.values() {
        for inst in &block.instructions {
            if let SIRInstruction::Imm(dst, value) = inst
                && let Some(value) = sir_value_to_u64(value)
            {
                constants.insert(*dst, value);
            }
            for source in instruction_uses(inst) {
                users
                    .entry(source)
                    .and_modify(|owner| {
                        if *owner != Some(block.id) {
                            *owner = None;
                        }
                    })
                    .or_insert(Some(block.id));
            }
        }
        celox_sir::analysis::visit_terminator_uses(&block.terminator, |source| {
            users
                .entry(source)
                .and_modify(|owner| {
                    if *owner != Some(block.id) {
                        *owner = None;
                    }
                })
                .or_insert(Some(block.id));
        });
    }
    let mut count = 0;
    for id in ids {
        let Some(plan) = plan(eu, id, &constants, &users) else {
            continue;
        };
        let mut next = eu.register_map.keys().map(|id| id.0).max().unwrap_or(0) + 1;
        let mut fresh = |ty: RegisterType| {
            let reg = RegisterId(next);
            next += 1;
            eu.register_map.insert(reg, ty);
            reg
        };
        let shift = fresh(plan.shift_type);
        let mask = fresh(plan.value_type.clone());
        let inverse = fresh(plan.value_type.clone());
        let cleared = fresh(plan.value_type.clone());
        let shifted = fresh(plan.value_type.clone());
        let inserted = fresh(plan.value_type.clone());
        let result = fresh(plan.value_type);
        let mut literals = Vec::new();
        eu.blocks.get_mut(&id).unwrap().instructions.retain(|inst| {
            if matches!(inst, SIRInstruction::Imm(..)) {
                literals.push(inst.clone());
                false
            } else {
                true
            }
        });
        let incoming = eu.blocks.get_mut(&plan.incoming).unwrap();
        incoming.instructions.extend(literals);
        incoming.instructions.extend([
            SIRInstruction::Binary(shift, plan.selector, BinaryOp::Mul, plan.element_width),
            SIRInstruction::Binary(mask, plan.element_mask, BinaryOp::Shl, shift),
            SIRInstruction::Unary(inverse, UnaryOp::BitNot, mask),
            SIRInstruction::Binary(cleared, plan.initial, BinaryOp::And, inverse),
            SIRInstruction::Binary(shifted, plan.payload, BinaryOp::Shl, shift),
            SIRInstruction::Binary(inserted, shifted, BinaryOp::And, mask),
            SIRInstruction::Binary(result, cleared, BinaryOp::Or, inserted),
        ]);
        incoming.terminator = SIRTerminator::Jump(plan.exit, vec![result]);
        eu.blocks.remove(&id);
        count += 1;
    }
    if count != 0 {
        tracing::debug!("direct packed index updates: {count}");
    }
    count
}

struct Plan {
    incoming: BlockId,
    exit: BlockId,
    initial: RegisterId,
    selector: RegisterId,
    payload: RegisterId,
    element_mask: RegisterId,
    element_width: RegisterId,
    value_type: RegisterType,
    shift_type: RegisterType,
}

fn plan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    id: BlockId,
    constants: &HashMap<RegisterId, u64>,
    users: &HashMap<RegisterId, Option<BlockId>>,
) -> Option<Plan> {
    let block = &eu.blocks[&id];
    let [counter, index, accumulator] = block.params.as_slice() else {
        return None;
    };
    let SIRTerminator::Branch {
        cond,
        true_block,
        false_block,
    } = &block.terminator
    else {
        return None;
    };
    if true_block.0 != id
        || false_block.0 == id
        || true_block.1.len() != 3
        || false_block.1.len() != 1
    {
        return None;
    }
    let mut defs = HashMap::default();
    let mut local = block.params.iter().copied().collect::<HashSet<_>>();
    for inst in &block.instructions {
        if !matches!(
            inst,
            SIRInstruction::Imm(..)
                | SIRInstruction::Binary(..)
                | SIRInstruction::Unary(..)
                | SIRInstruction::Concat(..)
                | SIRInstruction::Slice(..)
                | SIRInstruction::Mux(..)
        ) {
            return None;
        }
        let dst = def_reg(inst)?;
        defs.insert(dst, inst);
        local.insert(dst);
        if !matches!(inst, SIRInstruction::Imm(..))
            && users.get(&dst).is_some_and(|owner| *owner != Some(id))
        {
            return None;
        }
    }
    if block
        .params
        .iter()
        .any(|reg| users.get(reg).is_some_and(|owner| *owner != Some(id)))
    {
        return None;
    }
    let SIRInstruction::Binary(_, next_counter, BinaryOp::Ne, zero) = defs.get(cond)? else {
        return None;
    };
    let SIRInstruction::Binary(_, previous_counter, BinaryOp::Sub, one) = defs.get(next_counter)?
    else {
        return None;
    };
    let SIRInstruction::Binary(_, previous_index, BinaryOp::Add, index_one) =
        defs.get(&true_block.1[1])?
    else {
        return None;
    };
    if previous_counter != counter
        || previous_index != index
        || constants.get(zero) != Some(&0)
        || constants.get(one) != Some(&1)
        || constants.get(index_one) != Some(&1)
        || true_block.1[0] != *next_counter
        || [one, index_one].into_iter().any(|reg| {
            matches!(
                eu.register_map.get(reg),
                Some(RegisterType::Bit {
                    width: 1,
                    signed: true
                })
            )
        })
    {
        return None;
    }
    let mut incoming = None;
    for other in eu.blocks.values().filter(|b| b.id != id) {
        let targets_loop = match &other.terminator {
            SIRTerminator::Jump(target, _) => *target == id,
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => true_block.0 == id || false_block.0 == id,
            SIRTerminator::Switch { cases, default, .. } => {
                *default == id || cases.iter().any(|case| case.target == id)
            }
            _ => false,
        };
        if !targets_loop {
            continue;
        }
        let SIRTerminator::Jump(_, args) = &other.terminator else {
            return None;
        };
        if incoming.is_some() || args.len() != 3 {
            return None;
        }
        incoming = Some((other.id, args));
    }
    let (incoming, initial) = incoming?;
    let iterations = *constants.get(&initial[0])?;
    if !(8..=256).contains(&iterations)
        || !iterations.is_power_of_two()
        || constants.get(&initial[1]) != Some(&0)
    {
        return None;
    }
    let key_width = iterations.trailing_zeros() as usize;
    if eu.register_map[counter].width() <= key_width
        || eu.register_map[index].width() < key_width
        || eu.register_map.get(counter) != eu.register_map.get(next_counter)
        || eu.register_map.get(index) != eu.register_map.get(&true_block.1[1])
        || eu.register_map.get(counter) != eu.register_map.get(&initial[0])
        || eu.register_map.get(index) != eu.register_map.get(&initial[1])
        || matches!(eu.register_map.get(index), Some(RegisterType::Bit { width, signed: true }) if *width <= key_width)
    {
        return None;
    }
    let output = false_block.1[0];
    if true_block.1[2] != output {
        return None;
    }
    let SIRInstruction::Mux(_, condition, updated, otherwise) = defs.get(&output)? else {
        return None;
    };
    if otherwise != accumulator {
        return None;
    }
    let mut condition = *condition;
    for _ in 0..4 {
        if let Some(SIRInstruction::Unary(_, UnaryOp::Ident | UnaryOp::ToTwoState, inner)) =
            defs.get(&condition)
        {
            condition = *inner;
        } else {
            break;
        }
    }
    let SIRInstruction::Binary(_, lhs, BinaryOp::Eq, rhs) = defs.get(&condition)? else {
        return None;
    };
    let (indexed, selector) = if local.contains(lhs) && !local.contains(rhs) {
        (*lhs, *rhs)
    } else if local.contains(rhs) && !local.contains(lhs) {
        (*rhs, *lhs)
    } else {
        return None;
    };
    if !matches!(eu.register_map.get(&selector), Some(RegisterType::Bit { width, signed: false } | RegisterType::Logic { width }) if *width == key_width)
    {
        return None;
    }
    let mut indexed = indexed;
    if eu.register_map.get(&indexed)?.width() < key_width {
        return None;
    }
    if let Some(SIRInstruction::Binary(_, inner, BinaryOp::And, mask)) = defs.get(&indexed) {
        if constants.get(mask) != Some(&(iterations - 1)) {
            return None;
        }
        indexed = *inner;
        if eu.register_map.get(&indexed)?.width() < key_width
            || eu.register_map.get(mask)?.width() < key_width
        {
            return None;
        }
    }
    if let Some(SIRInstruction::Binary(_, inner, BinaryOp::Shr, zero)) = defs.get(&indexed) {
        if constants.get(zero) != Some(&0) {
            return None;
        }
        indexed = *inner;
        if eu.register_map.get(&indexed)?.width() < key_width {
            return None;
        }
    }
    if indexed != *index {
        return None;
    }
    let SIRInstruction::Binary(_, cleared, BinaryOp::Or, inserted) = defs.get(updated)? else {
        return None;
    };
    let SIRInstruction::Binary(_, previous, BinaryOp::And, inverse) = defs.get(cleared)? else {
        return None;
    };
    if previous != accumulator {
        return None;
    }
    let SIRInstruction::Unary(_, UnaryOp::BitNot, mask) = defs.get(inverse)? else {
        return None;
    };
    let SIRInstruction::Binary(_, shifted, BinaryOp::And, same_mask) = defs.get(inserted)? else {
        return None;
    };
    if mask != same_mask {
        return None;
    }
    let SIRInstruction::Binary(_, element_mask, BinaryOp::Shl, shift) = defs.get(mask)? else {
        return None;
    };
    let SIRInstruction::Binary(_, payload, BinaryOp::Shl, same_shift) = defs.get(shifted)? else {
        return None;
    };
    if shift != same_shift || local.contains(payload) {
        return None;
    }
    let SIRInstruction::Binary(_, shifting_index, BinaryOp::Mul, element_width) =
        defs.get(shift)?
    else {
        return None;
    };
    if shifting_index != index {
        return None;
    }
    let width = *constants.get(element_width)?;
    if !(1..=63).contains(&width) || constants.get(element_mask) != Some(&((1u64 << width) - 1)) {
        return None;
    }
    let value_type = eu.register_map.get(accumulator)?.clone();
    if value_type.width() < iterations as usize * width as usize
        || eu.register_map.get(&initial[2]) != Some(&value_type)
        || eu.register_map.get(payload) != Some(&value_type)
    {
        return None;
    }
    for reg in [
        output,
        *updated,
        *cleared,
        *inverse,
        *mask,
        *inserted,
        *shifted,
        *element_mask,
    ] {
        if eu.register_map.get(&reg)?.width() != value_type.width() {
            return None;
        }
    }
    // A two-state native offset may still have a logic type in merged SIR.
    // Both inputs are nonnegative in the proven domain and the product fits
    // an unsigned 64-bit offset without truncation or sign extension.
    let is_offset = |reg| {
        matches!(
            eu.register_map.get(reg),
            Some(
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                } | RegisterType::Logic { width: 64 }
            )
        )
    };
    if !is_offset(shift)
        || !is_offset(element_width)
        || local.contains(element_width)
            && !matches!(defs.get(element_width), Some(SIRInstruction::Imm(..)))
    {
        return None;
    }
    Some(Plan {
        incoming,
        exit: false_block.0,
        initial: initial[2],
        selector,
        payload: *payload,
        element_mask: *element_mask,
        element_width: *element_width,
        value_type,
        shift_type: RegisterType::Bit {
            width: 64,
            signed: false,
        },
    })
}

#[cfg(test)]
mod tests;
