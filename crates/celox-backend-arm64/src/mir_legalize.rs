//! AArch64 MIR legalization required before register allocation.

use crate::mir::{CmpKind, MFunction, MInst, SpillDesc, VReg, VRegAllocator};

/// Make the MIR's non-wrapping shift-count semantics explicit.
///
/// AArch64 masks variable shift counts to their low six bits. MIR instead
/// defines logical shifts by counts greater than or equal to 64 as zero and
/// arithmetic shifts as a sign fill, so select the architectural result only
/// for in-range counts.
pub(crate) fn legalize_variable_shift_counts(function: &mut MFunction) {
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
                matches!(
                    instruction,
                    MInst::Shr { .. } | MInst::Shl { .. } | MInst::Sar { .. }
                )
            })
            .count();
        if legalization_count == 0 {
            continue;
        }

        let mut rewritten = Vec::with_capacity(block.insts.len() + legalization_count * 2);
        for instruction in std::mem::take(&mut block.insts) {
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
}
