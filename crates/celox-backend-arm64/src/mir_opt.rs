//! Target-owned AArch64 MIR peepholes run before register allocation.

use std::collections::{BTreeMap, BTreeSet};

use crate::HashMap;
use crate::mir::{CmpKind, MFunction, MInst, VReg};

/// Recover the compact immediate and copy forms expected by AArch64 emission.
///
/// Instruction selection deliberately keeps SIR lowering straightforward, so
/// constants can initially appear as ordinary SSA operands. Leaving those
/// values live until allocation both emits unnecessary materializations and
/// creates avoidable register pressure on large simulation kernels.
pub(crate) fn optimize(function: &mut MFunction) {
    fold_constants(function);
    lower_immediate_uses(function);
    fuse_compare_selects(function);
    eliminate_nearby_common_expressions(function);
    propagate_exact_copies(function);
    dead_code_eliminate(function);
    remove_redundant_low_masks(function);
    propagate_exact_copies(function);
    dead_code_eliminate(function);
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum CseOpcode {
    Add,
    Add32,
    Sub,
    Sub32,
    Mul,
    Mul32,
    And,
    And32,
    Or,
    Or32,
    Xor,
    Xor32,
    Shr,
    Shl,
    Sar,
    AndImm,
    AndImm32,
    OrImm,
    ShrImm,
    ShlImm,
    SarImm,
    AddImm,
    SubImm,
    BitNot,
    Neg,
    Popcnt,
    Bsf,
    Bsr,
    BsrOr,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum CseKey {
    Binary(CseOpcode, VReg, VReg),
    Immediate(CseOpcode, VReg, u64),
    Compare(VReg, VReg, CmpKind),
    CompareImmediate(VReg, i32, CmpKind),
    Select(VReg, VReg, VReg),
    CompareSelect(VReg, VReg, CmpKind, VReg, VReg),
    CompareImmediateSelect(VReg, i32, CmpKind, VReg, VReg),
}

fn eliminate_nearby_common_expressions(function: &mut MFunction) {
    const MAX_REUSE_DISTANCE: usize = 32;

    for block in &mut function.blocks {
        let mut available = HashMap::<CseKey, (VReg, usize)>::default();
        let mut aliases = BTreeMap::<VReg, VReg>::new();
        for (index, instruction) in block.insts.iter_mut().enumerate() {
            for operand in instruction.uses() {
                let resolved = resolve_alias(&aliases, operand);
                instruction.rewrite_use(operand, resolved);
            }
            let Some(key) = cse_key(instruction) else {
                continue;
            };
            let destination = instruction
                .def()
                .expect("each common expression must define one value");
            if let Some(&(source, definition_index)) = available.get(&key)
                && index - definition_index <= MAX_REUSE_DISTANCE
            {
                aliases.insert(destination, source);
                *instruction = MInst::Mov {
                    dst: destination,
                    src: source,
                };
            } else {
                available.insert(key, (destination, index));
            }
        }
    }
}

fn resolve_alias(aliases: &BTreeMap<VReg, VReg>, mut value: VReg) -> VReg {
    let mut visited = BTreeSet::new();
    while visited.insert(value) {
        let Some(&next) = aliases.get(&value) else {
            break;
        };
        value = next;
    }
    value
}

fn cse_key(instruction: &MInst) -> Option<CseKey> {
    let commutative = |opcode, lhs, rhs| {
        let (lhs, rhs) = if lhs <= rhs { (lhs, rhs) } else { (rhs, lhs) };
        CseKey::Binary(opcode, lhs, rhs)
    };
    Some(match *instruction {
        MInst::Add { lhs, rhs, .. } => commutative(CseOpcode::Add, lhs, rhs),
        MInst::Add32 { lhs, rhs, .. } => commutative(CseOpcode::Add32, lhs, rhs),
        MInst::Sub { lhs, rhs, .. } => CseKey::Binary(CseOpcode::Sub, lhs, rhs),
        MInst::Sub32 { lhs, rhs, .. } => CseKey::Binary(CseOpcode::Sub32, lhs, rhs),
        MInst::Mul { lhs, rhs, .. } => commutative(CseOpcode::Mul, lhs, rhs),
        MInst::Mul32 { lhs, rhs, .. } => commutative(CseOpcode::Mul32, lhs, rhs),
        MInst::And { lhs, rhs, .. } => commutative(CseOpcode::And, lhs, rhs),
        MInst::And32 { lhs, rhs, .. } => commutative(CseOpcode::And32, lhs, rhs),
        MInst::Or { lhs, rhs, .. } => commutative(CseOpcode::Or, lhs, rhs),
        MInst::Or32 { lhs, rhs, .. } => commutative(CseOpcode::Or32, lhs, rhs),
        MInst::Xor { lhs, rhs, .. } => commutative(CseOpcode::Xor, lhs, rhs),
        MInst::Xor32 { lhs, rhs, .. } => commutative(CseOpcode::Xor32, lhs, rhs),
        MInst::Shr { lhs, rhs, .. } => CseKey::Binary(CseOpcode::Shr, lhs, rhs),
        MInst::Shl { lhs, rhs, .. } => CseKey::Binary(CseOpcode::Shl, lhs, rhs),
        MInst::Sar { lhs, rhs, .. } => CseKey::Binary(CseOpcode::Sar, lhs, rhs),
        MInst::AndImm { src, imm, .. } => CseKey::Immediate(CseOpcode::AndImm, src, imm),
        MInst::AndImm32 { src, imm, .. } => {
            CseKey::Immediate(CseOpcode::AndImm32, src, u64::from(imm))
        }
        MInst::OrImm { src, imm, .. } => CseKey::Immediate(CseOpcode::OrImm, src, imm),
        MInst::ShrImm { src, imm, .. } => CseKey::Immediate(CseOpcode::ShrImm, src, u64::from(imm)),
        MInst::ShlImm { src, imm, .. } => CseKey::Immediate(CseOpcode::ShlImm, src, u64::from(imm)),
        MInst::SarImm { src, imm, .. } => CseKey::Immediate(CseOpcode::SarImm, src, u64::from(imm)),
        MInst::AddImm { src, imm, .. } => {
            CseKey::Immediate(CseOpcode::AddImm, src, imm as i64 as u64)
        }
        MInst::SubImm { src, imm, .. } => {
            CseKey::Immediate(CseOpcode::SubImm, src, imm as i64 as u64)
        }
        MInst::BitNot { src, .. } => CseKey::Immediate(CseOpcode::BitNot, src, 0),
        MInst::Neg { src, .. } => CseKey::Immediate(CseOpcode::Neg, src, 0),
        MInst::Popcnt { src, .. } => CseKey::Immediate(CseOpcode::Popcnt, src, 0),
        MInst::Bsf { src, .. } => CseKey::Immediate(CseOpcode::Bsf, src, 0),
        MInst::Bsr { src, .. } => CseKey::Immediate(CseOpcode::Bsr, src, 0),
        MInst::BsrOr {
            src, zero_value, ..
        } => CseKey::Immediate(CseOpcode::BsrOr, src, u64::from(zero_value)),
        MInst::Cmp { lhs, rhs, kind, .. } => CseKey::Compare(lhs, rhs, kind),
        MInst::CmpImm { lhs, imm, kind, .. } => CseKey::CompareImmediate(lhs, imm, kind),
        MInst::Select {
            cond,
            true_val,
            false_val,
            ..
        } => CseKey::Select(cond, true_val, false_val),
        MInst::CmpSelect {
            lhs,
            rhs,
            kind,
            true_val,
            false_val,
            ..
        } => CseKey::CompareSelect(lhs, rhs, kind, true_val, false_val),
        MInst::CmpImmSelect {
            lhs,
            imm,
            kind,
            true_val,
            false_val,
            ..
        } => CseKey::CompareImmediateSelect(lhs, imm, kind, true_val, false_val),
        _ => return None,
    })
}

fn constants(function: &MFunction) -> BTreeMap<VReg, u64> {
    let mut constants = BTreeMap::new();
    for block in &function.blocks {
        for instruction in &block.insts {
            if let MInst::LoadImm { dst, value } = instruction {
                constants.insert(*dst, *value);
            }
        }
    }
    constants
}

fn fold_constants(function: &mut MFunction) {
    let mut known = constants(function);
    let mut changed = true;
    while changed {
        changed = false;
        for block in &mut function.blocks {
            for instruction in &mut block.insts {
                let folded = match *instruction {
                    MInst::Mov { dst, src } => known.get(&src).copied().map(|value| (dst, value)),
                    MInst::Mov32 { dst, src } => known
                        .get(&src)
                        .copied()
                        .map(|value| (dst, u64::from(value as u32))),
                    MInst::Add { dst, lhs, rhs } => {
                        fold_binary(&known, dst, lhs, rhs, u64::wrapping_add)
                    }
                    MInst::Add32 { dst, lhs, rhs } => {
                        fold_binary32(&known, dst, lhs, rhs, u32::wrapping_add)
                    }
                    MInst::Sub { dst, lhs, rhs } => {
                        fold_binary(&known, dst, lhs, rhs, u64::wrapping_sub)
                    }
                    MInst::Sub32 { dst, lhs, rhs } => {
                        fold_binary32(&known, dst, lhs, rhs, u32::wrapping_sub)
                    }
                    MInst::Mul { dst, lhs, rhs } => {
                        fold_binary(&known, dst, lhs, rhs, u64::wrapping_mul)
                    }
                    MInst::Mul32 { dst, lhs, rhs } => {
                        fold_binary32(&known, dst, lhs, rhs, u32::wrapping_mul)
                    }
                    MInst::And { dst, lhs, rhs } => {
                        fold_binary(&known, dst, lhs, rhs, |left, right| left & right)
                    }
                    MInst::And32 { dst, lhs, rhs } => {
                        fold_binary32(&known, dst, lhs, rhs, |left, right| left & right)
                    }
                    MInst::Or { dst, lhs, rhs } => {
                        fold_binary(&known, dst, lhs, rhs, |left, right| left | right)
                    }
                    MInst::Or32 { dst, lhs, rhs } => {
                        fold_binary32(&known, dst, lhs, rhs, |left, right| left | right)
                    }
                    MInst::Xor { dst, lhs, rhs } => {
                        fold_binary(&known, dst, lhs, rhs, |left, right| left ^ right)
                    }
                    MInst::Xor32 { dst, lhs, rhs } => {
                        fold_binary32(&known, dst, lhs, rhs, |left, right| left ^ right)
                    }
                    MInst::Shl { dst, lhs, rhs } => {
                        fold_binary(&known, dst, lhs, rhs, |value, count| {
                            if count < 64 { value << count } else { 0 }
                        })
                    }
                    MInst::Shr { dst, lhs, rhs } => {
                        fold_binary(&known, dst, lhs, rhs, |value, count| {
                            if count < 64 { value >> count } else { 0 }
                        })
                    }
                    MInst::BitNot { dst, src } => {
                        known.get(&src).copied().map(|value| (dst, !value))
                    }
                    MInst::Neg { dst, src } => known
                        .get(&src)
                        .copied()
                        .map(|value| (dst, value.wrapping_neg())),
                    MInst::Cmp {
                        dst,
                        lhs,
                        rhs,
                        kind,
                    } => match (known.get(&lhs), known.get(&rhs)) {
                        (Some(&left), Some(&right)) => {
                            Some((dst, u64::from(compare(kind, left, right))))
                        }
                        _ => None,
                    },
                    _ => None,
                };
                if let Some((dst, value)) = folded
                    && !matches!(instruction, MInst::LoadImm { value: old, .. } if *old == value)
                {
                    *instruction = MInst::LoadImm { dst, value };
                    known.insert(dst, value);
                    changed = true;
                }
            }
        }
    }
}

fn fold_binary(
    constants: &BTreeMap<VReg, u64>,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    operation: impl Fn(u64, u64) -> u64,
) -> Option<(VReg, u64)> {
    Some((dst, operation(*constants.get(&lhs)?, *constants.get(&rhs)?)))
}

fn fold_binary32(
    constants: &BTreeMap<VReg, u64>,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    operation: impl Fn(u32, u32) -> u32,
) -> Option<(VReg, u64)> {
    Some((
        dst,
        u64::from(operation(
            *constants.get(&lhs)? as u32,
            *constants.get(&rhs)? as u32,
        )),
    ))
}

fn compare(kind: CmpKind, lhs: u64, rhs: u64) -> bool {
    match kind {
        CmpKind::Eq => lhs == rhs,
        CmpKind::Ne => lhs != rhs,
        CmpKind::LtU => lhs < rhs,
        CmpKind::LeU => lhs <= rhs,
        CmpKind::GtU => lhs > rhs,
        CmpKind::GeU => lhs >= rhs,
        CmpKind::LtS => (lhs as i64) < (rhs as i64),
        CmpKind::LeS => (lhs as i64) <= (rhs as i64),
        CmpKind::GtS => (lhs as i64) > (rhs as i64),
        CmpKind::GeS => (lhs as i64) >= (rhs as i64),
    }
}

fn lower_immediate_uses(function: &mut MFunction) {
    let known = constants(function);
    for block in &mut function.blocks {
        for instruction in &mut block.insts {
            for operand in instruction.uses() {
                let Some(&value) = known.get(&operand) else {
                    continue;
                };
                let Some(replacement) = fold_immediate_use(instruction, operand, value) else {
                    continue;
                };
                *instruction = replacement;
                break;
            }
        }
    }
}

fn fold_immediate_use(instruction: &MInst, constant: VReg, value: u64) -> Option<MInst> {
    match *instruction {
        MInst::Cmp {
            dst,
            lhs,
            rhs,
            kind,
        } if rhs == constant => signed_i32(value).map(|imm| MInst::CmpImm {
            dst,
            lhs,
            imm,
            kind,
        }),
        MInst::Add { dst, lhs, rhs } if rhs == constant => {
            signed_i32(value).map(|imm| MInst::AddImm { dst, src: lhs, imm })
        }
        MInst::Add { dst, lhs, rhs } if lhs == constant => {
            signed_i32(value).map(|imm| MInst::AddImm { dst, src: rhs, imm })
        }
        MInst::Sub { dst, lhs, rhs } if rhs == constant => {
            signed_i32(value).map(|imm| MInst::SubImm { dst, src: lhs, imm })
        }
        MInst::And { dst, lhs, rhs } if rhs == constant => Some(MInst::AndImm {
            dst,
            src: lhs,
            imm: value,
        }),
        MInst::And { dst, lhs, rhs } if lhs == constant => Some(MInst::AndImm {
            dst,
            src: rhs,
            imm: value,
        }),
        MInst::And32 { dst, lhs, rhs } if rhs == constant => Some(MInst::AndImm32 {
            dst,
            src: lhs,
            imm: value as u32,
        }),
        MInst::And32 { dst, lhs, rhs } if lhs == constant => Some(MInst::AndImm32 {
            dst,
            src: rhs,
            imm: value as u32,
        }),
        MInst::Or { dst, lhs, rhs } if rhs == constant => Some(MInst::OrImm {
            dst,
            src: lhs,
            imm: value,
        }),
        MInst::Or { dst, lhs, rhs } if lhs == constant => Some(MInst::OrImm {
            dst,
            src: rhs,
            imm: value,
        }),
        MInst::Shl { dst, lhs, rhs } if rhs == constant && value < 64 => Some(MInst::ShlImm {
            dst,
            src: lhs,
            imm: value as u8,
        }),
        MInst::Shr { dst, lhs, rhs } if rhs == constant && value < 64 => Some(MInst::ShrImm {
            dst,
            src: lhs,
            imm: value as u8,
        }),
        MInst::Sar { dst, lhs, rhs } if rhs == constant && value < 64 => Some(MInst::SarImm {
            dst,
            src: lhs,
            imm: value as u8,
        }),
        MInst::Mul { dst, lhs, rhs } if rhs == constant => multiply_by_constant(dst, lhs, value),
        MInst::Mul { dst, lhs, rhs } if lhs == constant => multiply_by_constant(dst, rhs, value),
        MInst::CmpSelect {
            dst,
            lhs,
            rhs,
            kind,
            true_val,
            false_val,
        } if rhs == constant => signed_i32(value).map(|imm| MInst::CmpImmSelect {
            dst,
            lhs,
            imm,
            kind,
            true_val,
            false_val,
        }),
        _ => None,
    }
}

fn multiply_by_constant(dst: VReg, src: VReg, value: u64) -> Option<MInst> {
    match value {
        0 => Some(MInst::LoadImm { dst, value: 0 }),
        1 => Some(MInst::Mov { dst, src }),
        value if value.is_power_of_two() => Some(MInst::ShlImm {
            dst,
            src,
            imm: value.trailing_zeros() as u8,
        }),
        _ => None,
    }
}

fn signed_i32(value: u64) -> Option<i32> {
    let immediate = value as i32;
    ((immediate as i64 as u64) == value).then_some(immediate)
}

fn fuse_compare_selects(function: &mut MFunction) {
    let uses = use_counts(function);
    for block in &mut function.blocks {
        let definitions = block
            .insts
            .iter()
            .enumerate()
            .filter_map(|(index, instruction)| instruction.def().map(|value| (value, index)))
            .collect::<BTreeMap<_, _>>();
        let mut remove = BTreeSet::new();
        let mut replacements = BTreeMap::new();
        for (index, instruction) in block.insts.iter().enumerate() {
            let MInst::Select {
                dst,
                cond,
                true_val,
                false_val,
            } = *instruction
            else {
                continue;
            };
            if uses.get(&cond).copied() != Some(1) {
                continue;
            }
            let Some(&comparison_index) = definitions.get(&cond) else {
                continue;
            };
            let fused = match block.insts[comparison_index] {
                MInst::Cmp { lhs, rhs, kind, .. } => Some(MInst::CmpSelect {
                    dst,
                    lhs,
                    rhs,
                    kind,
                    true_val,
                    false_val,
                }),
                MInst::CmpImm { lhs, imm, kind, .. } => Some(MInst::CmpImmSelect {
                    dst,
                    lhs,
                    imm,
                    kind,
                    true_val,
                    false_val,
                }),
                _ => None,
            };
            if let Some(fused) = fused {
                remove.insert(comparison_index);
                replacements.insert(index, fused);
            }
        }
        rewrite_block(block, &remove, replacements);
    }
}

fn propagate_exact_copies(function: &mut MFunction) {
    let known_u32 = known_u32_values(function);
    let mut aliases = BTreeMap::new();
    for block in &function.blocks {
        for instruction in &block.insts {
            match *instruction {
                MInst::Mov { dst, src } => {
                    aliases.insert(dst, src);
                }
                MInst::Mov32 { dst, src } if known_u32.contains(&src) => {
                    aliases.insert(dst, src);
                }
                _ => {}
            }
        }
    }
    if aliases.is_empty() {
        return;
    }
    let resolve = |mut value: VReg| {
        let mut visited = BTreeSet::new();
        while visited.insert(value) {
            let Some(&next) = aliases.get(&value) else {
                break;
            };
            value = next;
        }
        value
    };
    for block in &mut function.blocks {
        for phi in &mut block.phis {
            for (_, source) in &mut phi.sources {
                *source = resolve(*source);
            }
        }
        for instruction in &mut block.insts {
            let operands = instruction.uses();
            for operand in operands {
                instruction.rewrite_use(operand, resolve(operand));
            }
        }
        block.insts.retain(|instruction| {
            instruction
                .def()
                .is_none_or(|dst| !aliases.contains_key(&dst))
        });
    }
}

/// Values whose upper 32 bits are already known zero. A `Mov32` from one of
/// these values is an exact copy and can be propagated without changing its
/// width-normalization semantics.
fn known_u32_values(function: &MFunction) -> BTreeSet<VReg> {
    let mut known = BTreeSet::new();
    let mut changed = true;
    while changed {
        changed = false;
        for block in &function.blocks {
            for phi in &block.phis {
                if !phi.sources.is_empty()
                    && phi.sources.iter().all(|(_, source)| known.contains(source))
                {
                    changed |= known.insert(phi.dst);
                }
            }
            for instruction in &block.insts {
                let value = match instruction {
                    MInst::LoadImm { dst, value } if *value <= u64::from(u32::MAX) => Some(*dst),
                    MInst::Load { dst, size, .. }
                    | MInst::LoadPtr { dst, size, .. }
                    | MInst::LoadIndexed { dst, size, .. }
                    | MInst::LoadPtrIndexed { dst, size, .. }
                        if *size != crate::mir::OpSize::S64 =>
                    {
                        Some(*dst)
                    }
                    MInst::Mov32 { dst, .. }
                    | MInst::Add32 { dst, .. }
                    | MInst::Sub32 { dst, .. }
                    | MInst::Mul32 { dst, .. }
                    | MInst::And32 { dst, .. }
                    | MInst::Or32 { dst, .. }
                    | MInst::Xor32 { dst, .. }
                    | MInst::AndImm32 { dst, .. }
                    | MInst::Cmp { dst, .. }
                    | MInst::CmpImm { dst, .. }
                    | MInst::Popcnt { dst, .. }
                    | MInst::Bsf { dst, .. }
                    | MInst::Bsr { dst, .. }
                    | MInst::BsrOr { dst, .. } => Some(*dst),
                    MInst::Mov { dst, src } if known.contains(src) => Some(*dst),
                    MInst::AndImm { dst, imm, .. } if *imm <= u64::from(u32::MAX) => Some(*dst),
                    MInst::OrImm { dst, src, imm }
                        if *imm <= u64::from(u32::MAX) && known.contains(src) =>
                    {
                        Some(*dst)
                    }
                    MInst::ShrImm { dst, src, imm } if *imm >= 32 || known.contains(src) => {
                        Some(*dst)
                    }
                    MInst::Select {
                        dst,
                        true_val,
                        false_val,
                        ..
                    }
                    | MInst::CmpSelect {
                        dst,
                        true_val,
                        false_val,
                        ..
                    }
                    | MInst::CmpImmSelect {
                        dst,
                        true_val,
                        false_val,
                        ..
                    }
                    | MInst::GuardedCmpSelect {
                        dst,
                        true_val,
                        false_val,
                        ..
                    } if known.contains(true_val) && known.contains(false_val) => Some(*dst),
                    _ => None,
                };
                if let Some(value) = value {
                    changed |= known.insert(value);
                }
            }
        }
    }
    known
}

fn dead_code_eliminate(function: &mut MFunction) {
    loop {
        let used = use_counts(function).into_keys().collect::<BTreeSet<_>>();
        let mut removed = false;
        for block in &mut function.blocks {
            let before = block.insts.len();
            block.insts.retain(|instruction| {
                instruction
                    .def()
                    .is_none_or(|definition| used.contains(&definition))
            });
            removed |= before != block.insts.len();
            let before = block.phis.len();
            block.phis.retain(|phi| used.contains(&phi.dst));
            removed |= before != block.phis.len();
        }
        if !removed {
            return;
        }
    }
}

fn remove_redundant_low_masks(function: &mut MFunction) {
    let bounds = crate::mir_legalize::value_upper_bounds(function);
    for block in &mut function.blocks {
        for instruction in &mut block.insts {
            let copy = match *instruction {
                MInst::AndImm { dst, src, imm }
                    if is_low_mask(imm) && bounds.get(&src).is_some_and(|&bound| bound <= imm) =>
                {
                    Some((dst, src))
                }
                MInst::AndImm32 { dst, src, imm }
                    if is_low_mask(u64::from(imm))
                        && bounds
                            .get(&src)
                            .is_some_and(|&bound| bound <= u64::from(imm)) =>
                {
                    Some((dst, src))
                }
                _ => None,
            };
            if let Some((dst, src)) = copy {
                *instruction = MInst::Mov { dst, src };
            }
        }
    }
}

fn is_low_mask(value: u64) -> bool {
    value != 0 && value.checked_add(1).is_none_or(u64::is_power_of_two)
}

fn use_counts(function: &MFunction) -> BTreeMap<VReg, usize> {
    let mut uses = BTreeMap::new();
    for block in &function.blocks {
        for phi in &block.phis {
            for &(_, source) in &phi.sources {
                *uses.entry(source).or_default() += 1;
            }
        }
        for instruction in &block.insts {
            for operand in instruction.uses() {
                *uses.entry(operand).or_default() += 1;
            }
        }
    }
    uses
}

fn rewrite_block(
    block: &mut crate::mir::MBlock,
    remove: &BTreeSet<usize>,
    mut replacements: BTreeMap<usize, MInst>,
) {
    let mut rewritten = Vec::with_capacity(block.insts.len());
    for (index, instruction) in std::mem::take(&mut block.insts).into_iter().enumerate() {
        if remove.contains(&index) {
            continue;
        }
        rewritten.push(replacements.remove(&index).unwrap_or(instruction));
    }
    block.insts = rewritten;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::{BlockId, MBlock};

    fn function(instructions: Vec<MInst>) -> MFunction {
        let mut block = MBlock::new(BlockId(0));
        block.insts = instructions;
        MFunction::new(vec![block], Vec::new())
    }

    #[test]
    fn folds_single_use_constants_into_machine_operations() {
        let mut function = function(vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 32,
            },
            MInst::Mul {
                dst: VReg(1),
                lhs: VReg(2),
                rhs: VReg(0),
            },
            MInst::LoadImm {
                dst: VReg(3),
                value: 17,
            },
            MInst::Cmp {
                dst: VReg(4),
                lhs: VReg(1),
                rhs: VReg(3),
                kind: CmpKind::Eq,
            },
            MInst::Store {
                base: crate::mir::BaseReg::SimState,
                offset: 0,
                src: VReg(4),
                size: crate::mir::OpSize::S8,
            },
            MInst::Return,
        ]);

        optimize(&mut function);

        assert!(matches!(
            function.blocks[0].insts[0],
            MInst::ShlImm {
                dst: VReg(1),
                src: VReg(2),
                imm: 5
            }
        ));
        assert!(matches!(
            function.blocks[0].insts[1],
            MInst::CmpImm {
                dst: VReg(4),
                lhs: VReg(1),
                imm: 17,
                kind: CmpKind::Eq
            }
        ));
    }

    #[test]
    fn fuses_single_use_compare_and_select() {
        let mut function = function(vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 7,
            },
            MInst::Cmp {
                dst: VReg(1),
                lhs: VReg(2),
                rhs: VReg(0),
                kind: CmpKind::LtU,
            },
            MInst::Select {
                dst: VReg(3),
                cond: VReg(1),
                true_val: VReg(4),
                false_val: VReg(5),
            },
            MInst::Store {
                base: crate::mir::BaseReg::SimState,
                offset: 0,
                src: VReg(3),
                size: crate::mir::OpSize::S64,
            },
            MInst::Return,
        ]);

        optimize(&mut function);

        assert!(matches!(
            function.blocks[0].insts[0],
            MInst::CmpImmSelect {
                dst: VReg(3),
                lhs: VReg(2),
                imm: 7,
                kind: CmpKind::LtU,
                true_val: VReg(4),
                false_val: VReg(5)
            }
        ));
    }

    #[test]
    fn propagates_mov32_only_when_the_source_is_already_zero_extended() {
        let mut function = function(vec![
            MInst::Load {
                dst: VReg(0),
                base: crate::mir::BaseReg::SimState,
                offset: 0,
                size: crate::mir::OpSize::S32,
            },
            MInst::Mov32 {
                dst: VReg(1),
                src: VReg(0),
            },
            MInst::Load {
                dst: VReg(2),
                base: crate::mir::BaseReg::SimState,
                offset: 8,
                size: crate::mir::OpSize::S64,
            },
            MInst::Mov32 {
                dst: VReg(3),
                src: VReg(2),
            },
            MInst::Store {
                base: crate::mir::BaseReg::SimState,
                offset: 16,
                src: VReg(1),
                size: crate::mir::OpSize::S64,
            },
            MInst::Store {
                base: crate::mir::BaseReg::SimState,
                offset: 24,
                src: VReg(3),
                size: crate::mir::OpSize::S64,
            },
            MInst::Return,
        ]);

        optimize(&mut function);

        assert!(
            !function.blocks[0]
                .insts
                .iter()
                .any(|instruction| matches!(instruction, MInst::Mov32 { dst: VReg(1), .. }))
        );
        assert!(
            function.blocks[0]
                .insts
                .iter()
                .any(|instruction| matches!(instruction, MInst::Mov32 { dst: VReg(3), .. }))
        );
        assert!(function.blocks[0].insts.iter().any(|instruction| matches!(
            instruction,
            MInst::Store {
                src: VReg(0),
                offset: 16,
                ..
            }
        )));
    }

    #[test]
    fn reuses_nearby_common_expressions() {
        let mut function = function(vec![
            MInst::ShlImm {
                dst: VReg(1),
                src: VReg(0),
                imm: 5,
            },
            MInst::AddImm {
                dst: VReg(2),
                src: VReg(1),
                imm: 12,
            },
            MInst::ShlImm {
                dst: VReg(3),
                src: VReg(0),
                imm: 5,
            },
            MInst::AddImm {
                dst: VReg(4),
                src: VReg(3),
                imm: 14,
            },
            MInst::Store {
                base: crate::mir::BaseReg::SimState,
                offset: 0,
                src: VReg(2),
                size: crate::mir::OpSize::S64,
            },
            MInst::Store {
                base: crate::mir::BaseReg::SimState,
                offset: 8,
                src: VReg(4),
                size: crate::mir::OpSize::S64,
            },
            MInst::Return,
        ]);

        optimize(&mut function);

        assert_eq!(
            function.blocks[0]
                .insts
                .iter()
                .filter(|instruction| matches!(instruction, MInst::ShlImm { .. }))
                .count(),
            1
        );
        assert!(function.blocks[0].insts.iter().any(|instruction| matches!(
            instruction,
            MInst::AddImm {
                src: VReg(1),
                imm: 14,
                ..
            }
        )));
    }

    #[test]
    fn removes_only_proven_redundant_low_masks() {
        let mut function = function(vec![
            MInst::Load {
                dst: VReg(0),
                base: crate::mir::BaseReg::SimState,
                offset: 0,
                size: crate::mir::OpSize::S8,
            },
            MInst::AndImm32 {
                dst: VReg(1),
                src: VReg(0),
                imm: 0xff,
            },
            MInst::AndImm32 {
                dst: VReg(2),
                src: VReg(0),
                imm: 0x7f,
            },
            MInst::Store {
                base: crate::mir::BaseReg::SimState,
                offset: 8,
                src: VReg(1),
                size: crate::mir::OpSize::S64,
            },
            MInst::Store {
                base: crate::mir::BaseReg::SimState,
                offset: 16,
                src: VReg(2),
                size: crate::mir::OpSize::S64,
            },
            MInst::Return,
        ]);

        optimize(&mut function);

        assert!(function.blocks[0].insts.iter().any(|instruction| matches!(
            instruction,
            MInst::Store {
                src: VReg(0),
                offset: 8,
                ..
            }
        )));
        assert!(function.blocks[0].insts.iter().any(|instruction| matches!(
            instruction,
            MInst::AndImm32 {
                dst: VReg(2),
                src: VReg(0),
                imm: 0x7f
            }
        )));
    }
}
