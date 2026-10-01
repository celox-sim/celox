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
        let dst = def_reg(inst)?; // Reject stores, commits and observer effects.
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
        let (param, guard) = preserved(*output, &block.params, &defs, &local, &eu.register_map, 0)?;
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
            let guard = false_guard(*condition, defs, local, types, 0)?;
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
    depth: usize,
) -> Option<RegisterId> {
    if depth > 32 || types.get(&value)?.width() != 1 {
        return None;
    }
    if !local.contains(&value) {
        return Some(value);
    }
    match defs.get(&value)? {
        SIRInstruction::Unary(_, UnaryOp::Ident | UnaryOp::ToTwoState, source) => {
            false_guard(*source, defs, local, types, depth + 1)
        }
        SIRInstruction::Binary(_, lhs, BinaryOp::LogicAnd | BinaryOp::And, rhs) => {
            false_guard(*lhs, defs, local, types, depth + 1)
                .or_else(|| false_guard(*rhs, defs, local, types, depth + 1))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{InstanceId, STABLE_REGION};
    use celox_design::StateObjectId;

    fn addr() -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: STABLE_REGION,
            instance_id: InstanceId(0),
            var_id: StateObjectId(0),
        }
    }

    fn unit() -> ExecutionUnit<RegionedAbsoluteAddr> {
        let r = RegisterId;
        let mut types = HashMap::default();
        for id in 0..38 {
            types.insert(
                r(id),
                RegisterType::Bit {
                    width: if matches!(id, 5 | 6 | 13..=31 | 35) {
                        1
                    } else {
                        8
                    },
                    signed: false,
                },
            );
        }
        let entry = BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions: vec![
                SIRInstruction::Imm(r(0), SIRValue::new(0u8)),
                SIRInstruction::Imm(r(1), SIRValue::new(1u8)),
                SIRInstruction::Imm(r(2), SIRValue::new(32u8)),
                SIRInstruction::Load(r(5), addr(), SIROffset::Static(0), 1),
                SIRInstruction::Load(r(6), addr(), SIROffset::Static(1), 1),
                SIRInstruction::Load(r(7), addr(), SIROffset::Static(8), 8),
                SIRInstruction::Load(r(8), addr(), SIROffset::Static(16), 8),
            ],
            terminator: SIRTerminator::Jump(BlockId(1), vec![r(2), r(7), r(8)]),
        };
        let mut instructions = vec![SIRInstruction::Load(
            r(13),
            addr(),
            SIROffset::Dynamic(r(10)),
            1,
        )];
        for id in 14..30 {
            instructions.push(SIRInstruction::Unary(r(id), UnaryOp::Ident, r(id - 1)));
        }
        instructions.extend([
            SIRInstruction::Binary(r(30), r(5), BinaryOp::LogicAnd, r(29)),
            SIRInstruction::Binary(r(31), r(6), BinaryOp::And, r(29)),
            SIRInstruction::Mux(r(32), r(30), r(10), r(11)),
            SIRInstruction::Mux(r(33), r(31), r(2), r(12)),
            SIRInstruction::Binary(r(34), r(10), BinaryOp::Sub, r(1)),
            SIRInstruction::Binary(r(35), r(34), BinaryOp::Ne, r(0)),
        ]);
        let scan = BasicBlock {
            id: BlockId(1),
            params: vec![r(10), r(11), r(12)],
            instructions,
            terminator: SIRTerminator::Branch {
                cond: r(35),
                true_block: (BlockId(1), vec![r(34), r(32), r(33)]),
                false_block: (BlockId(2), vec![r(32), r(33)]),
            },
        };
        let exit = BasicBlock {
            id: BlockId(2),
            params: vec![r(36), r(37)],
            instructions: vec![
                SIRInstruction::Store(addr(), SIROffset::Static(32), 8, r(36), vec![], vec![]),
                SIRInstruction::Store(addr(), SIROffset::Static(40), 8, r(37), vec![], vec![]),
            ],
            terminator: SIRTerminator::Return,
        };
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [entry, scan, exit].into_iter().map(|b| (b.id, b)).collect(),
            register_map: types,
        }
    }

    #[test]
    fn guards_all_results_and_preserves_nonzero_initial_values() {
        let mut eu = unit();
        eu.verify();
        assert_eq!(run(&mut eu), 1);
        eu.verify();
        let preheader = &eu.blocks[&BlockId(0)];
        assert!(matches!(
            preheader.instructions.last(),
            Some(SIRInstruction::Binary(
                _,
                RegisterId(5),
                BinaryOp::LogicOr,
                RegisterId(6)
            ))
        ));
        let SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } = &preheader.terminator
        else {
            panic!("missing loop guard")
        };
        assert_eq!(
            true_block,
            &(
                BlockId(1),
                vec![RegisterId(2), RegisterId(7), RegisterId(8)]
            )
        );
        assert_eq!(
            false_block,
            &(BlockId(2), vec![RegisterId(7), RegisterId(8)])
        );
        assert_eq!(
            run(&mut eu),
            0,
            "do not guard an already guarded incoming edge"
        );
    }

    #[test]
    fn rejects_effects_uncovered_conditions_and_external_definitions() {
        let mut variants = vec![unit(), unit(), unit(), unit()];
        variants[0]
            .blocks
            .get_mut(&BlockId(1))
            .unwrap()
            .instructions
            .push(SIRInstruction::RuntimeEvent {
                site_id: 0,
                args: vec![],
            });
        variants[1]
            .blocks
            .get_mut(&BlockId(1))
            .unwrap()
            .instructions[17] = SIRInstruction::Binary(
            RegisterId(30),
            RegisterId(5),
            BinaryOp::LogicOr,
            RegisterId(29),
        );
        variants[2]
            .blocks
            .get_mut(&BlockId(2))
            .unwrap()
            .instructions
            .push(SIRInstruction::Store(
                addr(),
                SIROffset::Static(48),
                8,
                RegisterId(34),
                vec![],
                vec![],
            ));
        if let SIRTerminator::Branch { true_block, .. } =
            &mut variants[3].blocks.get_mut(&BlockId(1)).unwrap().terminator
        {
            true_block.1[1] = RegisterId(10); // The next iteration overwrites the accumulator without its guard.
        }
        for mut eu in variants {
            eu.verify();
            let before = eu.clone();
            assert_eq!(run(&mut eu), 0);
            assert_eq!(eu, before);
        }
    }

    #[test]
    fn requires_a_finite_unit_stride_counter() {
        for step in [0u8, 2] {
            let mut eu = unit();
            eu.blocks.get_mut(&BlockId(0)).unwrap().instructions[1] =
                SIRInstruction::Imm(RegisterId(1), SIRValue::new(step));
            eu.verify();
            assert_eq!(run(&mut eu), 0);
        }
    }

    #[test]
    fn four_state_pipeline_keeps_the_scan() {
        use super::super::pass_guarded_region_sinking::recover_merged_effect_regions;
        let mut eu = unit();
        let before = eu.clone();
        recover_merged_effect_regions(&mut eu, true);
        assert_eq!(eu, before);
    }

    #[test]
    fn resizing_identity_is_not_a_preserved_value() {
        let param = RegisterId(0);
        let result = RegisterId(1);
        let inst = SIRInstruction::Unary(result, UnaryOp::Ident, param);
        let defs = [(result, &inst)].into_iter().collect();
        let local = [param, result].into_iter().collect();
        let types = [
            (
                param,
                RegisterType::Bit {
                    width: 8,
                    signed: false,
                },
            ),
            (
                result,
                RegisterType::Bit {
                    width: 4,
                    signed: false,
                },
            ),
        ]
        .into_iter()
        .collect();
        assert_eq!(preserved(result, &[param], &defs, &local, &types, 0), None);
    }
}
