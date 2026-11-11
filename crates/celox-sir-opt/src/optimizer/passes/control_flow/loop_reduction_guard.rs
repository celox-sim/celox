//! Skip a pure scan when invariant predicates prove every result unchanged.
//!
//! RTL often reduces a guarded per-entry predicate across an array. Keeping
//! the guard inside a lowered loop needlessly scans the array on idle cycles.
//! This recovers a guard at the loop's incoming edge from the SSA recurrence.

use super::shared::{def_reg, sir_value_to_u64};
use super::sir_analysis::instruction_uses;
use crate::ir::*;
use crate::{HashMap, HashSet};

pub(super) fn run(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) -> usize {
    let mut ids = eu
        .blocks
        .values()
        .filter(|block| {
            block.instructions.len() >= 16
                && matches!(&block.terminator, SIRTerminator::Branch { true_block, false_block, .. }
                if true_block.0 == block.id && false_block.0 != block.id)
        })
        .map(|block| block.id)
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return 0;
    }
    ids.sort_unstable();
    // Most EUs have no candidate scan. For those that do, record a single use
    // block or None for cross-block uses without allocating one set per value.
    let mut users = HashMap::<RegisterId, Option<BlockId>>::default();
    let mut constants = HashMap::default();
    for block in eu.blocks.values() {
        for inst in &block.instructions {
            if let SIRInstruction::Imm(reg, value) = inst
                && let Some(value) = sir_value_to_u64(value)
            {
                constants.insert(*reg, value);
            }
            for reg in instruction_uses(inst) {
                users
                    .entry(reg)
                    .and_modify(|owner| {
                        if *owner != Some(block.id) {
                            *owner = None;
                        }
                    })
                    .or_insert(Some(block.id));
            }
        }
        celox_sir::analysis::visit_terminator_uses(&block.terminator, |reg| {
            users
                .entry(reg)
                .and_modify(|owner| {
                    if *owner != Some(block.id) {
                        *owner = None;
                    }
                })
                .or_insert(Some(block.id));
        });
    }
    let mut applied = 0;
    for id in ids {
        let Some(plan) = plan(eu, id, &users, &constants) else {
            continue;
        };
        let mut active = plan.guards[0];
        let mut next_reg = eu.register_map.keys().map(|r| r.0).max().unwrap_or(0);
        let preheader = eu.blocks.get_mut(&plan.preheader).unwrap();
        for guard in &plan.guards[1..] {
            next_reg += 1;
            let dst = RegisterId(next_reg);
            eu.register_map.insert(
                dst,
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
            preheader.instructions.push(SIRInstruction::Binary(
                dst,
                active,
                BinaryOp::LogicOr,
                *guard,
            ));
            active = dst;
        }
        if eu.register_map.get(&active)
            != Some(&RegisterType::Bit {
                width: 1,
                signed: false,
            })
        {
            next_reg += 1;
            let dst = RegisterId(next_reg);
            eu.register_map.insert(
                dst,
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
            preheader
                .instructions
                .push(SIRInstruction::Unary(dst, UnaryOp::ToTwoState, active));
            active = dst;
        }
        preheader.terminator = SIRTerminator::Branch {
            cond: active,
            true_block: (id, plan.initial),
            false_block: (plan.exit, plan.skipped),
        };
        applied += 1;
    }
    if applied != 0 {
        tracing::debug!("guarded reduction loops: {applied}");
    }
    applied
}

struct Plan {
    preheader: BlockId,
    exit: BlockId,
    initial: Vec<RegisterId>,
    skipped: Vec<RegisterId>,
    guards: Vec<RegisterId>,
}

fn plan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    id: BlockId,
    users: &HashMap<RegisterId, Option<BlockId>>,
    constants: &HashMap<RegisterId, u64>,
) -> Option<Plan> {
    let block = &eu.blocks[&id];
    if id == eu.entry_block_id || block.instructions.len() < 16 {
        return None;
    }
    let SIRTerminator::Branch {
        cond,
        true_block,
        false_block,
    } = &block.terminator
    else {
        return None;
    };
    // A nonzero test after a unit-stride update reaches zero for any initial
    // value of a finite-width counter. Do not bypass possibly infinite loops.
    if true_block.0 != id || false_block.0 == id {
        return None;
    }
    let mut defs = HashMap::default();
    let mut local = block.params.iter().copied().collect::<HashSet<_>>();
    for inst in &block.instructions {
        // Reject stores, commits and observer effects, and calls, which can
        // define a result and still write state.
        if inst.has_side_effects() {
            return None;
        }
        let dst = def_reg(inst)?;
        defs.insert(dst, inst);
        local.insert(dst);
    }
    let SIRInstruction::Binary(_, counter, BinaryOp::Ne, zero) = defs.get(cond)? else {
        return None;
    };
    if constants.get(zero) != Some(&0) {
        return None;
    }
    let SIRInstruction::Binary(_, previous, BinaryOp::Add | BinaryOp::Sub, one) =
        defs.get(counter)?
    else {
        return None;
    };
    let counter_index = block.params.iter().position(|r| r == previous)?;
    if constants.get(one) != Some(&1)
        || true_block.1.get(counter_index) != Some(counter)
        || eu.register_map.get(previous) != eu.register_map.get(counter)
        || !(1..=64).contains(&eu.register_map.get(counter)?.width())
    {
        return None;
    }
    // All observable results must cross the explicit exit edge. A direct use
    // of a loop-local SSA definition would not exist on the bypass path.
    if local
        .iter()
        .any(|r| users.get(r).is_some_and(|owner| *owner != Some(id)))
    {
        return None;
    }
    let mut incoming = Vec::new();
    for predecessor in eu.blocks.values().filter(|b| b.id != id) {
        match &predecessor.terminator {
            SIRTerminator::Jump(target, args) if *target == id => {
                incoming.push((predecessor.id, args))
            }
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } if true_block.0 == id || false_block.0 == id => return None,
            SIRTerminator::Switch { cases, default, .. }
                if *default == id || cases.iter().any(|case| case.target == id) =>
            {
                return None;
            }
            _ => {}
        }
    }
    let [(preheader, initial)] = incoming.as_slice() else {
        return None;
    };
    let mut guards = Vec::new();
    let mut skipped = Vec::new();
    for output in &false_block.1 {
        if !local.contains(output) {
            skipped.push(*output);
            continue;
        }
        let (param, guard) = preserved(*output, &block.params, &defs, &local, &eu.register_map, 0)
            .or_else(|| {
                block.params.iter().enumerate().find_map(|(index, param)| {
                    if constants.get(initial.get(index)?) != Some(&0)
                        || true_block.1.get(index) != Some(output)
                        || eu.register_map.get(param) != eu.register_map.get(output)
                    {
                        return None;
                    }
                    zero_guard(*output, *param, &defs, &local, &eu.register_map, constants)
                        .map(|guard| (*param, Some(guard)))
                })
            })?;
        let index = block.params.iter().position(|r| *r == param)?;
        // Prove the recurrence, not just the final iteration's result.
        if true_block.1.get(index) != Some(output) && true_block.1.get(index) != Some(&param) {
            return None;
        }
        if let Some(guard) = guard {
            guards.push(guard);
        }
        skipped.push(*initial.get(index)?);
    }
    guards.sort_unstable();
    guards.dedup();
    if guards.is_empty() {
        return None;
    }
    Some(Plan {
        preheader: *preheader,
        exit: false_block.0,
        initial: (*initial).clone(),
        skipped,
        guards,
    })
}

fn preserved(
    value: RegisterId,
    params: &[RegisterId],
    defs: &HashMap<RegisterId, &SIRInstruction<RegionedAbsoluteAddr>>,
    local: &HashSet<RegisterId>,
    types: &HashMap<RegisterId, RegisterType>,
    depth: usize,
) -> Option<(RegisterId, Option<RegisterId>)> {
    if params.contains(&value) {
        return Some((value, None));
    }
    if depth > 32 {
        return None;
    }
    match defs.get(&value)? {
        SIRInstruction::Unary(_, UnaryOp::Ident, source)
            if types.get(&value) == types.get(source) =>
        {
            preserved(*source, params, defs, local, types, depth + 1)
        }
        SIRInstruction::Mux(_, condition, _, otherwise)
            if types.get(&value) == types.get(otherwise) =>
        {
            let guard = false_guard(*condition, defs, local, types)?;
            let (param, nested) = preserved(*otherwise, params, defs, local, types, depth + 1)?;
            // One invariant guard must cover the whole recurrence.
            if nested.is_some_and(|nested| nested != guard) {
                return None;
            }
            Some((param, Some(guard)))
        }
        _ => None,
    }
}

fn false_guard(
    value: RegisterId,
    defs: &HashMap<RegisterId, &SIRInstruction<RegionedAbsoluteAddr>>,
    local: &HashSet<RegisterId>,
    types: &HashMap<RegisterId, RegisterType>,
) -> Option<RegisterId> {
    let mut work = vec![value];
    let mut visited = HashSet::default();
    // Predicate expressions are DAGs. Visiting each node once prevents shared
    // all-local conjunctions from causing exponential recursive searches.
    while let Some(value) = work.pop() {
        if !visited.insert(value) || types.get(&value)?.width() != 1 {
            continue;
        }
        if !local.contains(&value) {
            return Some(value);
        }
        match defs.get(&value) {
            Some(SIRInstruction::Unary(_, UnaryOp::Ident | UnaryOp::ToTwoState, source)) => {
                work.push(*source)
            }
            Some(SIRInstruction::Binary(_, lhs, BinaryOp::LogicAnd | BinaryOp::And, rhs)) => {
                work.push(*rhs);
                work.push(*lhs);
            }
            _ => {}
        }
    }
    None
}

// Packed RTL results often clear and then replace one bit per iteration.
// An inactive predicate clears that bit rather than preserving an arbitrary
// accumulator, but a zero seed stays zero throughout the entire scan.
fn zero_guard(
    output: RegisterId,
    param: RegisterId,
    defs: &HashMap<RegisterId, &SIRInstruction<RegionedAbsoluteAddr>>,
    local: &HashSet<RegisterId>,
    types: &HashMap<RegisterId, RegisterType>,
    constants: &HashMap<RegisterId, u64>,
) -> Option<RegisterId> {
    let mut work = vec![output];
    let mut visited = HashSet::default();
    let mut candidates = Vec::new();
    while let Some(value) = work.pop() {
        if !visited.insert(value) {
            continue;
        }
        if visited.len() > 4096 || candidates.len() > 32 {
            return None;
        }
        if !local.contains(&value) {
            if types.get(&value)?.width() == 1 && !constants.contains_key(&value) {
                candidates.push(value);
            }
        } else if let Some(inst) = defs.get(&value) {
            work.extend(instruction_uses(inst));
        }
    }
    candidates.into_iter().find(|guard| {
        zero_when_inactive(
            output,
            param,
            *guard,
            defs,
            constants,
            &mut HashMap::default(),
            0,
        )
    })
}

fn zero_when_inactive(
    value: RegisterId,
    param: RegisterId,
    guard: RegisterId,
    defs: &HashMap<RegisterId, &SIRInstruction<RegionedAbsoluteAddr>>,
    constants: &HashMap<RegisterId, u64>,
    memo: &mut HashMap<RegisterId, bool>,
    depth: usize,
) -> bool {
    if value == param || value == guard || constants.get(&value) == Some(&0) {
        return true;
    }
    if let Some(result) = memo.get(&value) {
        return *result;
    }
    if depth > 64 || memo.len() > 4096 {
        return false;
    }
    let mut zero =
        |source| zero_when_inactive(source, param, guard, defs, constants, memo, depth + 1);
    let result = match defs.get(&value) {
        Some(SIRInstruction::Unary(_, UnaryOp::Ident | UnaryOp::ToTwoState, source)) => {
            zero(*source)
        }
        Some(SIRInstruction::Binary(_, lhs, BinaryOp::And | BinaryOp::LogicAnd, rhs)) => {
            zero(*lhs) || zero(*rhs)
        }
        Some(SIRInstruction::Binary(
            _,
            lhs,
            BinaryOp::Or | BinaryOp::LogicOr | BinaryOp::Xor,
            rhs,
        )) => zero(*lhs) && zero(*rhs),
        Some(SIRInstruction::Binary(_, lhs, BinaryOp::Shl, _)) => zero(*lhs),
        Some(SIRInstruction::Concat(_, sources)) => sources.iter().all(|source| zero(*source)),
        Some(SIRInstruction::Mux(_, _, lhs, rhs)) => zero(*lhs) && zero(*rhs),
        _ => false,
    };
    memo.insert(value, result);
    result
}

#[cfg(test)]
mod tests;
