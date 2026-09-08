//! Select AArch64 bitfields before allocation so packed values need fewer
//! intermediate registers as well as fewer instructions.

use crate::HashMap;
use crate::mir::{MFunction, MInst, VReg};

fn mask(inst: &MInst) -> Option<(VReg, u64)> {
    match *inst {
        MInst::AndImm { src, imm, .. } => Some((src, imm)),
        MInst::AndImm32 { src, imm, .. } => Some((src, u64::from(imm))),
        _ => None,
    }
}

fn field(mask: u64) -> Option<(u8, u8)> {
    if mask == 0 {
        return None;
    }
    let lsb = mask.trailing_zeros();
    let width = (mask >> lsb).trailing_ones();
    (mask.count_ones() == width).then_some((lsb as u8, width as u8))
}

pub(super) fn run(function: &mut MFunction) {
    let zeros = super::known_bits::known_zeros(function);
    let mut uses = HashMap::<VReg, usize>::default();
    for block in &function.blocks {
        for phi in &block.phis {
            for &(_, source) in &phi.sources {
                *uses.entry(source).or_default() += 1;
            }
        }
        for inst in &block.insts {
            for source in inst.uses() {
                *uses.entry(source).or_default() += 1;
            }
        }
    }
    for block in &mut function.blocks {
        let definitions = block
            .insts
            .iter()
            .filter_map(|inst| inst.def().map(|dst| (dst, inst.clone())))
            .collect::<HashMap<_, _>>();
        for inst in &mut block.insts {
            if let Some((source, low_mask)) = mask(inst)
                && let Some(&MInst::ShrImm { src, imm: lsb, .. }) = definitions.get(&source)
                && (1..64).contains(&lsb)
                && uses[&source] == 1
                && let Some((0, width)) = field(low_mask)
            {
                *inst = MInst::BitExtract {
                    dst: inst.def().unwrap(),
                    src,
                    lsb,
                    width: width.min(64 - lsb),
                };
                continue;
            }
            let MInst::Or { dst, lhs, rhs } = *inst else {
                continue;
            };
            let mut replacement = None;
            for (base, inserted) in [(lhs, rhs), (rhs, lhs)] {
                if let Some((base_source, preserved)) = definitions.get(&base).and_then(mask)
                    && uses[&base] == 1
                    && let Some((lsb, width)) = field(!preserved)
                    && (!zeros[inserted.0 as usize] & preserved) == 0
                {
                    let source = if lsb == 0 {
                        Some(inserted)
                    } else if let Some(&MInst::ShlImm { src, imm, .. }) = definitions.get(&inserted)
                        && imm == lsb
                        && uses[&inserted] == 1
                    {
                        Some(src)
                    } else {
                        None
                    };
                    if let Some(mut src) = source {
                        // BFI itself truncates its source to the field width.
                        // Remove an explicit low mask only if it preserves
                        // every bit consumed by the selected instruction.
                        let low_mask = u64::MAX >> (64 - width);
                        if let Some((unmasked, mask)) = definitions.get(&src).and_then(mask)
                            && mask & low_mask == low_mask
                        {
                            src = unmasked;
                        }
                        replacement = Some(MInst::BitInsert {
                            dst,
                            base: base_source,
                            src,
                            lsb,
                            width,
                        });
                        break;
                    }
                }
            }
            if replacement.is_none() {
                for (unshifted, shifted) in [(lhs, rhs), (rhs, lhs)] {
                    if let Some(&MInst::ShlImm {
                        src, imm: shift, ..
                    }) = definitions.get(&shifted)
                        && (1..64).contains(&shift)
                        && uses[&shifted] == 1
                    {
                        replacement = Some(MInst::OrShifted {
                            dst,
                            lhs: unshifted,
                            rhs: src,
                            shift,
                        });
                        break;
                    }
                }
            }
            if let Some(replacement) = replacement {
                *inst = replacement;
            }
        }
    }
}
