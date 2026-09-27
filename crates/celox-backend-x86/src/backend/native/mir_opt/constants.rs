//! Constant evaluation and deduplication.

use super::*;

// ────────────────────────────────────────────────────────────────
// Phase 1A: Constant folding
// ────────────────────────────────────────────────────────────────

/// Constant folding: evaluate operations with constant operands at compile time.
pub(super) fn constant_fold(func: &mut MFunction) {
    // Build def map: VReg → LoadImm value
    let mut consts: HashMap<VReg, u64> = HashMap::default();
    for block in &func.blocks {
        for inst in &block.insts {
            if let MInst::LoadImm { dst, value } = inst {
                consts.insert(*dst, *value);
            }
        }
    }
    if consts.is_empty() {
        return;
    }

    let mut changed = true;
    while changed {
        changed = false;
        for block in &mut func.blocks {
            for inst in &mut block.insts {
                let folded = match inst {
                    // Binary reg-reg with both constant
                    MInst::Add { dst, lhs, rhs } => {
                        fold_bin(&consts, *dst, *lhs, *rhs, u64::wrapping_add)
                    }
                    MInst::Add32 { dst, lhs, rhs } => {
                        fold_bin32(&consts, *dst, *lhs, *rhs, u32::wrapping_add)
                    }
                    MInst::Sub { dst, lhs, rhs } => {
                        fold_bin(&consts, *dst, *lhs, *rhs, u64::wrapping_sub)
                    }
                    MInst::Sub32 { dst, lhs, rhs } => {
                        fold_bin32(&consts, *dst, *lhs, *rhs, u32::wrapping_sub)
                    }
                    MInst::Mul { dst, lhs, rhs } => {
                        fold_bin(&consts, *dst, *lhs, *rhs, u64::wrapping_mul)
                    }
                    MInst::Mul32 { dst, lhs, rhs } => {
                        fold_bin32(&consts, *dst, *lhs, *rhs, u32::wrapping_mul)
                    }
                    MInst::MulImm { dst, src, imm } => consts
                        .get(src)
                        .map(|value| (*dst, value.wrapping_mul(*imm as u64))),
                    MInst::MulImm32 { dst, src, imm } => consts
                        .get(src)
                        .map(|value| (*dst, u64::from((*value as u32).wrapping_mul(*imm as u32)))),
                    MInst::And { dst, lhs, rhs } => {
                        fold_bin(&consts, *dst, *lhs, *rhs, |a, b| a & b)
                    }
                    MInst::And32 { dst, lhs, rhs } => {
                        fold_bin32(&consts, *dst, *lhs, *rhs, |a, b| a & b)
                    }
                    MInst::Or { dst, lhs, rhs } => {
                        fold_bin(&consts, *dst, *lhs, *rhs, |a, b| a | b)
                    }
                    MInst::Or32 { dst, lhs, rhs } => {
                        fold_bin32(&consts, *dst, *lhs, *rhs, |a, b| a | b)
                    }
                    MInst::Xor { dst, lhs, rhs } => {
                        fold_bin(&consts, *dst, *lhs, *rhs, |a, b| a ^ b)
                    }
                    MInst::Xor32 { dst, lhs, rhs } => {
                        fold_bin32(&consts, *dst, *lhs, *rhs, |a, b| a ^ b)
                    }
                    MInst::Shr { dst, lhs, rhs } => {
                        fold_bin(
                            &consts,
                            *dst,
                            *lhs,
                            *rhs,
                            |a, b| {
                                if b >= 64 { 0 } else { a >> b }
                            },
                        )
                    }
                    MInst::Shl { dst, lhs, rhs } => {
                        fold_bin(
                            &consts,
                            *dst,
                            *lhs,
                            *rhs,
                            |a, b| {
                                if b >= 64 { 0 } else { a << b }
                            },
                        )
                    }
                    MInst::Sar { dst, lhs, rhs } => fold_bin(&consts, *dst, *lhs, *rhs, |a, b| {
                        if b >= 64 {
                            ((a as i64) >> 63) as u64
                        } else {
                            ((a as i64) >> b) as u64
                        }
                    }),
                    // Binary imm with constant src
                    MInst::AndImm { dst, src, imm } => consts.get(src).map(|&v| (*dst, v & *imm)),
                    MInst::AndImm32 { dst, src, imm } => consts
                        .get(src)
                        .map(|&v| (*dst, u64::from((v as u32) & *imm))),
                    MInst::OrImm { dst, src, imm } => consts.get(src).map(|&v| (*dst, v | *imm)),
                    MInst::ShrImm { dst, src, imm } => consts
                        .get(src)
                        .map(|&v| (*dst, if *imm >= 64 { 0 } else { v >> *imm })),
                    MInst::ShlImm { dst, src, imm } => consts
                        .get(src)
                        .map(|&v| (*dst, if *imm >= 64 { 0 } else { v << *imm })),
                    MInst::SarImm { dst, src, imm } => consts.get(src).map(|&v| {
                        (
                            *dst,
                            if *imm >= 64 {
                                ((v as i64) >> 63) as u64
                            } else {
                                ((v as i64) >> *imm) as u64
                            },
                        )
                    }),
                    // Unary with constant src
                    MInst::BitNot { dst, src } => consts.get(src).map(|&v| (*dst, !v)),
                    MInst::Neg { dst, src } => consts.get(src).map(|&v| (*dst, v.wrapping_neg())),
                    MInst::Popcnt { dst, src } => {
                        consts.get(src).map(|&v| (*dst, v.count_ones() as u64))
                    }
                    MInst::Bsf { dst, src } => consts
                        .get(src)
                        .and_then(|&v| (v != 0).then_some((*dst, v.trailing_zeros() as u64))),
                    MInst::Bsr { dst, src } => consts
                        .get(src)
                        .and_then(|&v| (v != 0).then_some((*dst, 63 - v.leading_zeros() as u64))),
                    MInst::BsrOr {
                        dst,
                        src,
                        zero_value,
                    } => consts.get(src).map(|&v| {
                        (
                            *dst,
                            if v == 0 {
                                *zero_value as u64
                            } else {
                                63 - v.leading_zeros() as u64
                            },
                        )
                    }),
                    // Comparison with both constant
                    MInst::Cmp {
                        dst,
                        lhs,
                        rhs,
                        kind,
                    } => {
                        if let (Some(&l), Some(&r)) = (consts.get(lhs), consts.get(rhs)) {
                            let result = match kind {
                                CmpKind::Eq => l == r,
                                CmpKind::Ne => l != r,
                                CmpKind::LtU => l < r,
                                CmpKind::LeU => l <= r,
                                CmpKind::GtU => l > r,
                                CmpKind::GeU => l >= r,
                                CmpKind::LtS => (l as i64) < (r as i64),
                                CmpKind::LeS => (l as i64) <= (r as i64),
                                CmpKind::GtS => (l as i64) > (r as i64),
                                CmpKind::GeS => (l as i64) >= (r as i64),
                            };
                            Some((*dst, result as u64))
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some((dst, value)) = folded {
                    *inst = MInst::LoadImm { dst, value };
                    consts.insert(dst, value);
                    changed = true;
                }
            }
        }
    }
}

fn fold_bin(
    consts: &HashMap<VReg, u64>,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    op: impl Fn(u64, u64) -> u64,
) -> Option<(VReg, u64)> {
    if let (Some(&l), Some(&r)) = (consts.get(&lhs), consts.get(&rhs)) {
        Some((dst, op(l, r)))
    } else {
        None
    }
}

fn fold_bin32(
    consts: &HashMap<VReg, u64>,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    op: impl Fn(u32, u32) -> u32,
) -> Option<(VReg, u64)> {
    if let (Some(&l), Some(&r)) = (consts.get(&lhs), consts.get(&rhs)) {
        Some((dst, u64::from(op(l as u32, r as u32))))
    } else {
        None
    }
}

/// Constant deduplication: merge LoadImm instructions with the same value
/// into a single VReg. Reduces register pressure and instruction count.
pub(super) fn constant_dedup(func: &mut MFunction) {
    let mut aliases: HashMap<VReg, VReg> = HashMap::default();
    // Map from constant value → canonical VReg
    let mut const_map: HashMap<u64, VReg> = HashMap::default();

    for block in &func.blocks {
        const_map.clear(); // per-block to avoid cross-block live range extension
        for inst in &block.insts {
            if let MInst::LoadImm { dst, value } = inst {
                if let Some(&canonical) = const_map.get(value) {
                    aliases.insert(*dst, canonical);
                } else {
                    const_map.insert(*value, *dst);
                }
            }
        }
    }

    if aliases.is_empty() {
        return;
    }

    // Apply aliases
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            let current_uses = inst.uses();
            for u in current_uses {
                if let Some(&target) = aliases.get(&u) {
                    inst.rewrite_use(u, target);
                }
            }
        }
        for phi in &mut block.phis {
            for (_, src) in &mut phi.sources {
                if let Some(&a) = aliases.get(src) {
                    *src = a;
                }
            }
        }
    }

    // Remove duplicated LoadImm
    for block in &mut func.blocks {
        block.insts.retain(|inst| {
            if let MInst::LoadImm { dst, .. } = inst {
                !aliases.contains_key(dst)
            } else {
                true
            }
        });
    }
}
