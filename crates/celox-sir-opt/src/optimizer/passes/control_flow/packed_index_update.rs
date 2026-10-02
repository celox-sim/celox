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
mod tests {
    use super::*;
    use num_bigint::BigUint;
    use num_traits::{ToPrimitive, Zero};

    fn fixture(
        lanes: usize,
        element_width: usize,
        word_width: usize,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let r = RegisterId;
        let key_width = lanes.trailing_zeros() as usize;
        let bit = |width| RegisterType::Bit {
            width,
            signed: false,
        };
        let index_type = RegisterType::Bit {
            width: 32,
            signed: true,
        };
        let value_type = RegisterType::Logic { width: word_width };
        let register_map = (0..31)
            .map(|id| {
                let ty = match id {
                    0 | 9 | 16 => bit(key_width),
                    3 | 5 | 7 | 12 | 27 => bit(key_width + 1),
                    4 | 6 | 13 | 29 => index_type.clone(),
                    8 | 10 => bit(64),
                    19 => RegisterType::Logic { width: 64 },
                    15 => bit(32),
                    17 => RegisterType::Logic { width: 1 },
                    18 | 28 => bit(1),
                    11 | 20 | 21 => bit(word_width),
                    _ => value_type.clone(),
                };
                (r(id), ty)
            })
            .collect();
        let entry = BasicBlock {
            id: BlockId(0),
            params: vec![r(0), r(1), r(2)],
            instructions: vec![
                SIRInstruction::Imm(r(3), SIRValue::new(lanes as u64)),
                SIRInstruction::Imm(r(4), SIRValue::new(0u8)),
                SIRInstruction::Imm(r(5), SIRValue::new(1u8)),
                SIRInstruction::Imm(r(6), SIRValue::new(1u8)),
                SIRInstruction::Imm(r(7), SIRValue::new(0u8)),
                SIRInstruction::Imm(r(8), SIRValue::new(0u8)),
                SIRInstruction::Imm(r(9), SIRValue::new((lanes - 1) as u64)),
                SIRInstruction::Imm(r(10), SIRValue::new(element_width as u64)),
            ],
            terminator: SIRTerminator::Jump(BlockId(1), vec![r(3), r(4), r(2)]),
        };
        let scan = BasicBlock {
            id: BlockId(1),
            params: vec![r(12), r(13), r(14)],
            instructions: vec![
                SIRInstruction::Imm(r(11), SIRValue::new((1u64 << element_width) - 1)),
                SIRInstruction::Binary(r(15), r(13), BinaryOp::Shr, r(8)),
                SIRInstruction::Binary(r(16), r(15), BinaryOp::And, r(9)),
                SIRInstruction::Binary(r(17), r(0), BinaryOp::Eq, r(16)),
                SIRInstruction::Unary(r(18), UnaryOp::ToTwoState, r(17)),
                SIRInstruction::Binary(r(19), r(13), BinaryOp::Mul, r(10)),
                SIRInstruction::Binary(r(20), r(11), BinaryOp::Shl, r(19)),
                SIRInstruction::Unary(r(21), UnaryOp::BitNot, r(20)),
                SIRInstruction::Binary(r(22), r(14), BinaryOp::And, r(21)),
                SIRInstruction::Binary(r(23), r(1), BinaryOp::Shl, r(19)),
                SIRInstruction::Binary(r(24), r(23), BinaryOp::And, r(20)),
                SIRInstruction::Binary(r(25), r(22), BinaryOp::Or, r(24)),
                SIRInstruction::Mux(r(26), r(18), r(25), r(14)),
                SIRInstruction::Binary(r(27), r(12), BinaryOp::Sub, r(5)),
                SIRInstruction::Binary(r(28), r(27), BinaryOp::Ne, r(7)),
                SIRInstruction::Binary(r(29), r(13), BinaryOp::Add, r(6)),
            ],
            terminator: SIRTerminator::Branch {
                cond: r(28),
                true_block: (BlockId(1), vec![r(27), r(29), r(26)]),
                false_block: (BlockId(2), vec![r(26)]),
            },
        };
        let exit = BasicBlock {
            id: BlockId(2),
            params: vec![r(30)],
            instructions: vec![SIRInstruction::RuntimeEvent {
                site_id: 0,
                args: vec![r(30)],
            }],
            terminator: SIRTerminator::Return,
        };
        ExecutionUnit {
            entry_block_id: BlockId(0),
            register_map,
            blocks: [entry, scan, exit].into_iter().map(|b| (b.id, b)).collect(),
        }
    }

    fn execute(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        selector: u64,
        payload: &BigUint,
        initial: &BigUint,
    ) -> Vec<BigUint> {
        let mut values = HashMap::from_iter([
            (RegisterId(0), BigUint::from(selector)),
            (RegisterId(1), payload.clone()),
            (RegisterId(2), initial.clone()),
        ]);
        let mut current = eu.entry_block_id;
        for _ in 0..300 {
            let block = &eu.blocks[&current];
            for inst in &block.instructions {
                let (dst, value) = match inst {
                    SIRInstruction::Imm(dst, value) => (*dst, value.payload.clone()),
                    SIRInstruction::Binary(dst, lhs, op, rhs) => {
                        let (lhs, rhs) = (&values[lhs], &values[rhs]);
                        let value = match op {
                            BinaryOp::Add => lhs + rhs,
                            BinaryOp::Sub => lhs - rhs,
                            BinaryOp::Mul => lhs * rhs,
                            BinaryOp::And => lhs & rhs,
                            BinaryOp::Or => lhs | rhs,
                            BinaryOp::Shr => lhs >> rhs.to_usize().unwrap(),
                            BinaryOp::Shl => lhs << rhs.to_usize().unwrap(),
                            BinaryOp::Eq => BigUint::from(u8::from(lhs == rhs)),
                            BinaryOp::Ne => BigUint::from(u8::from(lhs != rhs)),
                            _ => panic!("unsupported fixture operation {op:?}"),
                        };
                        (*dst, value)
                    }
                    SIRInstruction::Unary(dst, op, input) => {
                        let value = match op {
                            UnaryOp::BitNot => {
                                ((BigUint::from(1u8) << eu.register_map[dst].width()) - 1u8)
                                    ^ &values[input]
                            }
                            UnaryOp::Ident | UnaryOp::ToTwoState => values[input].clone(),
                            _ => panic!("unsupported fixture operation {op:?}"),
                        };
                        (*dst, value)
                    }
                    SIRInstruction::Mux(dst, condition, yes, no) => (
                        *dst,
                        values[if values[condition].is_zero() { no } else { yes }].clone(),
                    ),
                    SIRInstruction::RuntimeEvent { args, .. } => {
                        return args.iter().map(|reg| values[reg].clone()).collect();
                    }
                    _ => panic!("unsupported fixture instruction {inst:?}"),
                };
                let mask = (BigUint::from(1u8) << eu.register_map[&dst].width()) - 1u8;
                values.insert(dst, value & mask);
            }
            let (target, args) = match &block.terminator {
                SIRTerminator::Jump(target, args) => (*target, args),
                SIRTerminator::Branch {
                    cond,
                    true_block,
                    false_block,
                } => {
                    let target = if values[cond].is_zero() {
                        false_block
                    } else {
                        true_block
                    };
                    (target.0, &target.1)
                }
                _ => panic!("fixture did not publish a result"),
            };
            let args = args
                .iter()
                .map(|reg| values[reg].clone())
                .collect::<Vec<_>>();
            for (&param, value) in eu.blocks[&target].params.iter().zip(args) {
                values.insert(param, value);
            }
            current = target;
        }
        panic!("fixture did not terminate")
    }

    #[test]
    fn updates_every_selector_and_preserves_bits_across_word_boundaries() {
        for (lanes, element_width, word_width) in [(8, 3, 32), (32, 6, 192)] {
            let original = fixture(lanes, element_width, word_width);
            original.verify();
            let mut rewritten = original.clone();
            assert_eq!(run(&mut rewritten), 1);
            rewritten.verify();
            assert!(!rewritten.blocks.contains_key(&BlockId(1)));
            assert_eq!(run(&mut rewritten), 0);
            let word_mask = (BigUint::from(1u8) << word_width) - 1u8;
            let element_mask = (BigUint::from(1u8) << element_width) - 1u8;
            for selector in 0..lanes {
                for initial in [
                    BigUint::zero(),
                    word_mask.clone(),
                    (BigUint::from(1u8) << (word_width - 1)) | BigUint::from(0x12345678u32),
                ] {
                    for payload in [
                        BigUint::zero(),
                        element_mask.clone(),
                        BigUint::from(0xabcdefu32),
                    ] {
                        let shift = selector * element_width;
                        let mask = &element_mask << shift;
                        let expected = (&initial & (&word_mask ^ &mask))
                            | ((&payload & &element_mask) << shift);
                        assert_eq!(
                            execute(&original, selector as u64, &payload, &initial),
                            vec![expected.clone()]
                        );
                        assert_eq!(
                            execute(&rewritten, selector as u64, &payload, &initial),
                            vec![expected]
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn keeps_shared_literals_available_after_removing_the_scan() {
        let mut original = fixture(8, 3, 32);
        original.blocks.get_mut(&BlockId(2)).unwrap().instructions[0] =
            SIRInstruction::RuntimeEvent {
                site_id: 0,
                args: vec![RegisterId(30), RegisterId(11)],
            };
        original.verify();
        let mut rewritten = original.clone();
        assert_eq!(run(&mut rewritten), 1);
        rewritten.verify();
        assert_eq!(
            execute(&original, 3, &BigUint::from(2u8), &BigUint::from(0xffu8)),
            execute(&rewritten, 3, &BigUint::from(2u8), &BigUint::from(0xffu8))
        );
    }

    #[test]
    fn native_four_state_recovery_retains_the_scan() {
        let mut eu = fixture(32, 6, 192);
        let original = eu.clone();
        super::super::pass_guarded_region_sinking::recover_merged_effect_regions(&mut eu, true);
        assert_eq!(eu, original);
    }

    #[test]
    fn rejects_incomplete_domains_effects_and_escaping_loop_values() {
        let mut variants = (0..12).map(|_| fixture(8, 3, 32)).collect::<Vec<_>>();
        variants[0]
            .blocks
            .get_mut(&BlockId(0))
            .unwrap()
            .instructions[0] = SIRInstruction::Imm(RegisterId(3), SIRValue::new(4u8));
        variants[1]
            .blocks
            .get_mut(&BlockId(0))
            .unwrap()
            .instructions[1] = SIRInstruction::Imm(RegisterId(4), SIRValue::new(1u8));
        variants[2]
            .blocks
            .get_mut(&BlockId(0))
            .unwrap()
            .instructions[3] = SIRInstruction::Imm(RegisterId(6), SIRValue::new(2u8));
        variants[3]
            .blocks
            .get_mut(&BlockId(0))
            .unwrap()
            .instructions[6] = SIRInstruction::Imm(RegisterId(9), SIRValue::new(3u8));
        variants[4]
            .blocks
            .get_mut(&BlockId(0))
            .unwrap()
            .instructions[5] = SIRInstruction::Imm(RegisterId(8), SIRValue::new(1u8));
        variants[5]
            .blocks
            .get_mut(&BlockId(1))
            .unwrap()
            .instructions
            .push(SIRInstruction::RuntimeEvent {
                site_id: 1,
                args: vec![],
            });
        variants[6]
            .blocks
            .get_mut(&BlockId(2))
            .unwrap()
            .instructions[0] = SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![RegisterId(30), RegisterId(27)],
        };
        for reg in [0, 9, 16] {
            variants[7].register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 4,
                    signed: false,
                },
            );
        }
        // A signed one-bit step represents -1, and a narrow signed index
        // becomes negative before completing the unsigned selector domain.
        variants[8].register_map.insert(
            RegisterId(6),
            RegisterType::Bit {
                width: 1,
                signed: true,
            },
        );
        for reg in [4, 6, 13, 15, 29] {
            variants[9].register_map.insert(
                RegisterId(reg),
                RegisterType::Bit {
                    width: 3,
                    signed: true,
                },
            );
        }
        variants[10]
            .register_map
            .insert(RegisterId(19), RegisterType::Logic { width: 4 });
        variants[11].register_map.insert(
            RegisterId(10),
            RegisterType::Bit {
                width: 64,
                signed: true,
            },
        );
        for mut eu in variants {
            eu.verify();
            let before = eu.clone();
            assert_eq!(run(&mut eu), 0);
            assert_eq!(eu, before);
        }
    }
}
