//! AArch64 MIR legalization required before register allocation.

use std::collections::BTreeMap;

use crate::mir::{CmpKind, MFunction, MInst, SpillDesc, VReg, VRegAllocator};

/// Make the MIR's non-wrapping shift-count semantics explicit.
///
/// AArch64 masks variable shift counts to their low six bits. MIR instead
/// defines logical shifts by counts greater than or equal to 64 as zero and
/// arithmetic shifts as a sign fill, so select the architectural result only
/// for in-range counts.
pub(crate) fn legalize_variable_shift_counts(function: &mut MFunction) {
    let upper_bounds = value_upper_bounds(function);
    let (blocks, vregs, spill_descs) = (
        &mut function.blocks,
        &mut function.vregs,
        &mut function.spill_descs,
    );

    for block in blocks {
        let legalization_count = block
            .insts
            .iter()
            .filter(|instruction| {
                let rhs = match instruction {
                    MInst::Shr { rhs, .. } | MInst::Shl { rhs, .. } | MInst::Sar { rhs, .. } => {
                        *rhs
                    }
                    _ => return false,
                };
                upper_bounds.get(&rhs).is_none_or(|&bound| bound >= 64)
            })
            .count();
        if legalization_count == 0 {
            continue;
        }

        let mut rewritten = Vec::with_capacity(block.insts.len() + legalization_count * 2);
        for instruction in std::mem::take(&mut block.insts) {
            let in_range = match &instruction {
                MInst::Shr { rhs, .. } | MInst::Shl { rhs, .. } | MInst::Sar { rhs, .. } => {
                    upper_bounds.get(rhs).is_some_and(|&bound| bound < 64)
                }
                _ => false,
            };
            if in_range {
                rewritten.push(instruction);
                continue;
            }
            match instruction {
                MInst::Shr { dst, lhs, rhs } => {
                    let raw = alloc_shift_temp(vregs, spill_descs, false);
                    let zero = alloc_shift_temp(vregs, spill_descs, true);
                    rewritten.push(MInst::Shr { dst: raw, lhs, rhs });
                    rewritten.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    rewritten.push(MInst::CmpImmSelect {
                        dst,
                        lhs: rhs,
                        imm: 64,
                        kind: CmpKind::LtU,
                        true_val: raw,
                        false_val: zero,
                    });
                }
                MInst::Shl { dst, lhs, rhs } => {
                    let raw = alloc_shift_temp(vregs, spill_descs, false);
                    let zero = alloc_shift_temp(vregs, spill_descs, true);
                    rewritten.push(MInst::Shl { dst: raw, lhs, rhs });
                    rewritten.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    rewritten.push(MInst::CmpImmSelect {
                        dst,
                        lhs: rhs,
                        imm: 64,
                        kind: CmpKind::LtU,
                        true_val: raw,
                        false_val: zero,
                    });
                }
                MInst::Sar { dst, lhs, rhs } => {
                    let raw = alloc_shift_temp(vregs, spill_descs, false);
                    let sign_fill = alloc_shift_temp(vregs, spill_descs, false);
                    rewritten.push(MInst::Sar { dst: raw, lhs, rhs });
                    rewritten.push(MInst::SarImm {
                        dst: sign_fill,
                        src: lhs,
                        imm: 63,
                    });
                    rewritten.push(MInst::CmpImmSelect {
                        dst,
                        lhs: rhs,
                        imm: 64,
                        kind: CmpKind::LtU,
                        true_val: raw,
                        false_val: sign_fill,
                    });
                }
                instruction => rewritten.push(instruction),
            }
        }
        block.insts = rewritten;
    }
}

pub(crate) fn value_upper_bounds(function: &MFunction) -> BTreeMap<VReg, u64> {
    let mut bounds = BTreeMap::new();
    let mut changed = true;
    while changed {
        changed = false;
        for block in &function.blocks {
            for phi in &block.phis {
                let bound = phi
                    .sources
                    .iter()
                    .map(|(_, source)| bounds.get(source).copied())
                    .collect::<Option<Vec<_>>>()
                    .and_then(|bounds| bounds.into_iter().max());
                if let Some(bound) = bound {
                    changed |= insert_tighter_bound(&mut bounds, phi.dst, bound);
                }
            }
            for instruction in &block.insts {
                let bounded = match instruction {
                    MInst::LoadImm { dst, value } => Some((*dst, *value)),
                    MInst::Mov { dst, src } => bounds.get(src).copied().map(|bound| (*dst, bound)),
                    MInst::Mov32 { dst, src } => Some((
                        *dst,
                        bounds
                            .get(src)
                            .copied()
                            .unwrap_or(u64::from(u32::MAX))
                            .min(u64::from(u32::MAX)),
                    )),
                    MInst::Load { dst, size, .. }
                    | MInst::LoadPtr { dst, size, .. }
                    | MInst::LoadIndexed { dst, size, .. }
                    | MInst::LoadPtrIndexed { dst, size, .. } => Some((*dst, size_mask(*size))),
                    MInst::AndImm { dst, src, imm } => {
                        Some((*dst, bounds.get(src).copied().unwrap_or(u64::MAX).min(*imm)))
                    }
                    MInst::AndImm32 { dst, src, imm } => Some((
                        *dst,
                        bounds
                            .get(src)
                            .copied()
                            .unwrap_or(u64::from(u32::MAX))
                            .min(u64::from(*imm)),
                    )),
                    MInst::And { dst, lhs, rhs } => match (bounds.get(lhs), bounds.get(rhs)) {
                        (Some(&left), Some(&right)) => Some((*dst, left.min(right))),
                        (Some(&bound), None) | (None, Some(&bound)) => Some((*dst, bound)),
                        _ => None,
                    },
                    MInst::And32 { dst, lhs, rhs } => {
                        let left = bounds.get(lhs).copied().unwrap_or(u64::from(u32::MAX));
                        let right = bounds.get(rhs).copied().unwrap_or(u64::from(u32::MAX));
                        Some((*dst, left.min(right)))
                    }
                    MInst::Or { dst, lhs, rhs }
                    | MInst::Or32 { dst, lhs, rhs }
                    | MInst::Xor { dst, lhs, rhs }
                    | MInst::Xor32 { dst, lhs, rhs }
                        if bounds.contains_key(lhs) && bounds.contains_key(rhs) =>
                    {
                        Some((*dst, enclosing_low_mask(bounds[lhs].max(bounds[rhs]))))
                    }
                    MInst::OrImm { dst, src, imm } => bounds
                        .get(src)
                        .copied()
                        .map(|bound| (*dst, enclosing_low_mask(bound.max(*imm)))),
                    MInst::Add { dst, lhs, rhs } => match (bounds.get(lhs), bounds.get(rhs)) {
                        (Some(&left), Some(&right)) => {
                            left.checked_add(right).map(|bound| (*dst, bound))
                        }
                        _ => None,
                    },
                    MInst::Mul { dst, lhs, rhs } => match (bounds.get(lhs), bounds.get(rhs)) {
                        (Some(&left), Some(&right)) => {
                            left.checked_mul(right).map(|bound| (*dst, bound))
                        }
                        _ => None,
                    },
                    MInst::AddImm { dst, src, imm } if *imm >= 0 => bounds
                        .get(src)
                        .and_then(|bound| bound.checked_add(*imm as u64))
                        .map(|bound| (*dst, bound)),
                    MInst::Or32 { dst, .. }
                    | MInst::Xor32 { dst, .. }
                    | MInst::Add32 { dst, .. }
                    | MInst::Sub32 { dst, .. }
                    | MInst::Mul32 { dst, .. } => Some((*dst, u64::from(u32::MAX))),
                    MInst::Cmp { dst, .. } | MInst::CmpImm { dst, .. } => Some((*dst, 1)),
                    MInst::ShrImm { dst, src, imm } => Some((
                        *dst,
                        bounds.get(src).copied().unwrap_or(u64::MAX) >> u32::from(*imm),
                    )),
                    MInst::ShlImm { dst, src, imm } => bounds
                        .get(src)
                        .and_then(|&bound| {
                            let shift = u32::from(*imm);
                            let largest = u64::MAX.checked_shr(shift)?;
                            (bound <= largest).then_some(bound << shift)
                        })
                        .map(|bound| (*dst, bound)),
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
                    } => match (bounds.get(true_val), bounds.get(false_val)) {
                        (Some(&left), Some(&right)) => Some((*dst, left.max(right))),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some((value, bound)) = bounded {
                    changed |= insert_tighter_bound(&mut bounds, value, bound);
                }
            }
        }
    }
    bounds
}

fn enclosing_low_mask(value: u64) -> u64 {
    if value == 0 {
        0
    } else {
        u64::MAX >> value.leading_zeros()
    }
}

fn insert_tighter_bound(bounds: &mut BTreeMap<VReg, u64>, value: VReg, bound: u64) -> bool {
    match bounds.get_mut(&value) {
        Some(current) if bound < *current => {
            *current = bound;
            true
        }
        Some(_) => false,
        None => {
            bounds.insert(value, bound);
            true
        }
    }
}

fn size_mask(size: crate::mir::OpSize) -> u64 {
    match size {
        crate::mir::OpSize::S8 => u64::from(u8::MAX),
        crate::mir::OpSize::S16 => u64::from(u16::MAX),
        crate::mir::OpSize::S32 => u64::from(u32::MAX),
        crate::mir::OpSize::S64 => u64::MAX,
    }
}

fn alloc_shift_temp(
    vregs: &mut VRegAllocator,
    spill_descs: &mut Vec<SpillDesc>,
    rematerialize_zero: bool,
) -> VReg {
    let vreg = vregs.alloc();
    debug_assert_eq!(spill_descs.len(), vreg.0 as usize);
    spill_descs.push(if rematerialize_zero {
        SpillDesc::remat(0)
    } else {
        SpillDesc::transient()
    });
    vreg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::{BlockId, MBlock};

    #[test]
    fn legalizes_all_variable_shift_kinds() {
        let mut vregs = VRegAllocator::new();
        let lhs = vregs.alloc();
        let count = vregs.alloc();
        let shl = vregs.alloc();
        let shr = vregs.alloc();
        let sar = vregs.alloc();
        let mut function = MFunction::for_isel(vregs, vec![SpillDesc::transient(); 5]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Shl {
            dst: shl,
            lhs,
            rhs: count,
        });
        block.push(MInst::Shr {
            dst: shr,
            lhs,
            rhs: count,
        });
        block.push(MInst::Sar {
            dst: sar,
            lhs,
            rhs: count,
        });
        block.push(MInst::Return);
        function.blocks.push(block);

        legalize_variable_shift_counts(&mut function);

        assert_eq!(function.vregs.count(), 11);
        assert_eq!(function.spill_descs.len(), 11);
        assert_eq!(
            function.blocks[0]
                .insts
                .iter()
                .filter(|instruction| matches!(instruction, MInst::CmpImmSelect { imm: 64, .. }))
                .count(),
            3
        );
        assert!(function.blocks[0].insts.iter().any(|instruction| matches!(
            instruction,
            MInst::SarImm {
                src,
                imm: 63,
                ..
            } if *src == lhs
        )));
    }

    #[test]
    fn leaves_proven_in_range_shift_counts_native() {
        let mut vregs = VRegAllocator::new();
        let lhs = vregs.alloc();
        let unbounded = vregs.alloc();
        let count = vregs.alloc();
        let shifted = vregs.alloc();
        let mut function = MFunction::for_isel(vregs, vec![SpillDesc::transient(); 4]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::AndImm32 {
            dst: count,
            src: unbounded,
            imm: 7,
        });
        block.push(MInst::Shr {
            dst: shifted,
            lhs,
            rhs: count,
        });
        block.push(MInst::Return);
        function.blocks.push(block);

        legalize_variable_shift_counts(&mut function);

        assert_eq!(function.vregs.count(), 4);
        assert_eq!(function.blocks[0].insts.len(), 3);
        assert!(matches!(function.blocks[0].insts[1], MInst::Shr { .. }));
    }
}
