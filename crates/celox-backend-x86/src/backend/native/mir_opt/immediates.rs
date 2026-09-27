//! Immediate operand selection and late constant-mask folding.

use super::*;

pub(super) fn fold_imm_use(inst: &MInst, imm_vreg: VReg, value: u64) -> Option<MInst> {
    match inst {
        MInst::Mul { dst, lhs, rhs } if *rhs == imm_vreg || *lhs == imm_vreg => {
            sign_extended_i32(value).map(|imm| MInst::MulImm {
                dst: *dst,
                src: if *rhs == imm_vreg { *lhs } else { *rhs },
                imm,
            })
        }
        MInst::Mul32 { dst, lhs, rhs } if *rhs == imm_vreg || *lhs == imm_vreg => {
            Some(MInst::MulImm32 {
                dst: *dst,
                src: if *rhs == imm_vreg { *lhs } else { *rhs },
                imm: value as i32,
            })
        }
        MInst::Cmp {
            dst,
            lhs,
            rhs,
            kind,
        } if *rhs == imm_vreg => sign_extended_i32(value).map(|imm| MInst::CmpImm {
            dst: *dst,
            lhs: *lhs,
            imm,
            kind: *kind,
        }),
        MInst::Add { dst, lhs, rhs } if *rhs == imm_vreg => {
            sign_extended_i32(value).map(|imm| MInst::AddImm {
                dst: *dst,
                src: *lhs,
                imm,
            })
        }
        MInst::Add { dst, lhs, rhs } if *lhs == imm_vreg => {
            sign_extended_i32(value).map(|imm| MInst::AddImm {
                dst: *dst,
                src: *rhs,
                imm,
            })
        }
        MInst::Sub { dst, lhs, rhs } if *rhs == imm_vreg => {
            sign_extended_i32(value).map(|imm| MInst::SubImm {
                dst: *dst,
                src: *lhs,
                imm,
            })
        }
        MInst::And { dst, lhs, rhs } if *rhs == imm_vreg => {
            and_imm_ok(value).then_some(MInst::AndImm {
                dst: *dst,
                src: *lhs,
                imm: value,
            })
        }
        MInst::And { dst, lhs, rhs } if *lhs == imm_vreg => {
            and_imm_ok(value).then_some(MInst::AndImm {
                dst: *dst,
                src: *rhs,
                imm: value,
            })
        }
        MInst::And32 { dst, lhs, rhs } if *rhs == imm_vreg => Some(MInst::AndImm32 {
            dst: *dst,
            src: *lhs,
            imm: value as u32,
        }),
        MInst::And32 { dst, lhs, rhs } if *lhs == imm_vreg => Some(MInst::AndImm32 {
            dst: *dst,
            src: *rhs,
            imm: value as u32,
        }),
        MInst::Or { dst, lhs, rhs } if *rhs == imm_vreg => {
            sign_extended_i32(value).map(|imm| MInst::OrImm {
                dst: *dst,
                src: *lhs,
                imm: imm as u64,
            })
        }
        MInst::Or { dst, lhs, rhs } if *lhs == imm_vreg => {
            sign_extended_i32(value).map(|imm| MInst::OrImm {
                dst: *dst,
                src: *rhs,
                imm: imm as u64,
            })
        }
        MInst::Shr { dst, lhs, rhs } if *rhs == imm_vreg && value < 64 => Some(MInst::ShrImm {
            dst: *dst,
            src: *lhs,
            imm: value as u8,
        }),
        MInst::Shl { dst, lhs, rhs } if *rhs == imm_vreg && value < 64 => Some(MInst::ShlImm {
            dst: *dst,
            src: *lhs,
            imm: value as u8,
        }),
        MInst::Sar { dst, lhs, rhs } if *rhs == imm_vreg && value < 64 => Some(MInst::SarImm {
            dst: *dst,
            src: *lhs,
            imm: value as u8,
        }),
        MInst::LoadIndexed {
            dst,
            base,
            offset,
            index,
            scale,
            size,
            ..
        } if *index == imm_vreg => sign_extended_i32(value)
            .and_then(|index| index.checked_mul(i32::from(*scale)))
            .and_then(|index| offset.checked_add(index))
            .map(|offset| MInst::Load {
                dst: *dst,
                base: *base,
                offset,
                size: *size,
            }),
        MInst::StoreIndexed {
            base,
            offset,
            index,
            src,
            size,
            ..
        } if *index == imm_vreg => sign_extended_i32(value)
            .and_then(|index| offset.checked_add(index))
            .map(|offset| MInst::Store {
                base: *base,
                offset,
                src: *src,
                size: *size,
            }),
        MInst::LoadPtrIndexed {
            dst,
            ptr,
            offset,
            index,
            size,
        } if *index == imm_vreg => sign_extended_i32(value)
            .and_then(|index| offset.checked_add(index))
            .map(|offset| MInst::LoadPtr {
                dst: *dst,
                ptr: *ptr,
                offset,
                size: *size,
            }),
        MInst::StorePtrIndexed {
            ptr,
            offset,
            index,
            src,
            size,
        } if *index == imm_vreg => sign_extended_i32(value)
            .and_then(|index| offset.checked_add(index))
            .map(|offset| MInst::StorePtr {
                ptr: *ptr,
                offset,
                src: *src,
                size: *size,
            }),
        MInst::ReleaseStorePtrIndexed {
            ptr,
            offset,
            index,
            src,
            size,
        } if *index == imm_vreg => sign_extended_i32(value)
            .and_then(|index| offset.checked_add(index))
            .map(|offset| MInst::ReleaseStorePtr {
                ptr: *ptr,
                offset,
                src: *src,
                size: *size,
            }),
        _ => None,
    }
}

pub(super) fn sign_extended_i32(value: u64) -> Option<i32> {
    let imm = value as i32;
    ((imm as i64 as u64) == value).then_some(imm)
}

pub(super) fn and_imm_ok(value: u64) -> bool {
    sign_extended_i32(value).is_some() || value <= u32::MAX as u64
}

// ────────────────────────────────────────────────────────────────
// Immediate-form lowering
// ────────────────────────────────────────────────────────────────

/// Convert operations with constant operands into immediate-form MIR.
/// This runs late (after CSE/constant fold) to maximize opportunities.
pub(super) fn lower_to_imm_forms(func: &mut MFunction) {
    // Collect constants
    let mut consts: HashMap<VReg, u64> = HashMap::default();
    for block in &func.blocks {
        for inst in &block.insts {
            if let MInst::LoadImm { dst, value } = inst {
                consts.insert(*dst, *value);
            }
        }
    }

    for block in &mut func.blocks {
        for inst in &mut block.insts {
            for use_vreg in inst.uses() {
                let Some(&value) = consts.get(&use_vreg) else {
                    continue;
                };
                let Some(folded) = fold_imm_use(inst, use_vreg, value) else {
                    continue;
                };
                *inst = folded;
                break;
            }
        }
    }
}

/// Close serial immediate-mask chains after all target bit-pack rewrites.
///
/// Some late bit-range folds reconstruct an And64/And32 chain after the main
/// algebraic pass. The 32-bit operation is a zero-extending machine
/// operation, so mixed-width composition must retain an And32 result.
pub(super) fn fold_late_serial_and_immediates(func: &mut MFunction) {
    let mut and64 = HashMap::<VReg, (VReg, u64)>::default();
    let mut and32 = HashMap::<VReg, (VReg, u32)>::default();
    for block in &func.blocks {
        for instruction in &block.insts {
            match instruction {
                MInst::AndImm { dst, src, imm } => {
                    and64.insert(*dst, (*src, *imm));
                }
                MInst::AndImm32 { dst, src, imm } => {
                    and32.insert(*dst, (*src, *imm));
                }
                _ => {}
            }
        }
    }
    for block in &mut func.blocks {
        for instruction in &mut block.insts {
            let replacement = match *instruction {
                MInst::AndImm { dst, src, imm } => {
                    if let Some(&(original, previous)) = and64.get(&src) {
                        let combined = previous & imm;
                        Some(if combined == 0 {
                            MInst::LoadImm { dst, value: 0 }
                        } else {
                            MInst::AndImm {
                                dst,
                                src: original,
                                imm: combined,
                            }
                        })
                    } else if let Some(&(original, previous)) = and32.get(&src) {
                        let combined = previous & (imm as u32);
                        Some(if combined == 0 {
                            MInst::LoadImm { dst, value: 0 }
                        } else {
                            MInst::AndImm32 {
                                dst,
                                src: original,
                                imm: combined,
                            }
                        })
                    } else {
                        None
                    }
                }
                MInst::AndImm32 { dst, src, imm } => {
                    let source = and32
                        .get(&src)
                        .copied()
                        .or_else(|| and64.get(&src).map(|&(source, mask)| (source, mask as u32)));
                    source.map(|(original, previous)| {
                        let combined = previous & imm;
                        if combined == 0 {
                            MInst::LoadImm { dst, value: 0 }
                        } else {
                            MInst::AndImm32 {
                                dst,
                                src: original,
                                imm: combined,
                            }
                        }
                    })
                }
                _ => None,
            };
            if let Some(replacement) = replacement {
                *instruction = replacement;
            }
        }
    }
}
