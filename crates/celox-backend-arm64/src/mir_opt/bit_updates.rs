//! Fold a clear followed by setting the same bits into a single OR.

use super::*;

pub(crate) fn run(function: &mut MFunction) {
    let mut definitions = vec![None; function.value_count()];
    for block in &function.blocks {
        for inst in &block.insts {
            if let Some(dst) = inst.def() {
                definitions[dst.0 as usize] = Some(inst);
            }
        }
    }
    let definition = |value: VReg| definitions[value.0 as usize];
    let same_value = |left: VReg, right: VReg| {
        left == right
            || matches!((definition(left), definition(right)),
                (Some(MInst::LoadImm { value: left, .. }),
                 Some(MInst::LoadImm { value: right, .. })) if left == right)
    };
    let same_mask = |left: VReg, right: VReg| {
        same_value(left, right)
            || matches!((definition(left), definition(right)),
                (Some(MInst::Shl { lhs: a, rhs: b, .. }),
                 Some(MInst::Shl { lhs: c, rhs: d, .. }))
                    if same_value(*a, *c) && same_value(*b, *d))
    };
    let mut replacements = Vec::new();
    for (block_at, block) in function.blocks.iter().enumerate() {
        for (inst_at, inst) in block.insts.iter().enumerate() {
            let (dst, left, right, narrow) = match *inst {
                MInst::Or { dst, lhs, rhs } => (dst, lhs, rhs, false),
                MInst::Or32 { dst, lhs, rhs } => (dst, lhs, rhs, true),
                _ => continue,
            };
            let replacement =
                [(left, right), (right, left)]
                    .into_iter()
                    .find_map(|(cleared, mask)| {
                        let (left, right) = match definition(cleared)? {
                            MInst::And { lhs, rhs, .. } => (*lhs, *rhs),
                            MInst::And32 { lhs, rhs, .. } if narrow => (*lhs, *rhs),
                            _ => return None,
                        };
                        [(left, right), (right, left)]
                            .into_iter()
                            .find_map(|(base, inverted)| {
                                let MInst::BitNot { src, .. } = definition(inverted)? else {
                                    return None;
                                };
                                same_mask(*src, mask).then_some(if narrow {
                                    MInst::Or32 {
                                        dst,
                                        lhs: base,
                                        rhs: mask,
                                    }
                                } else {
                                    MInst::Or {
                                        dst,
                                        lhs: base,
                                        rhs: mask,
                                    }
                                })
                            })
                    });
            if let Some(replacement) = replacement {
                replacements.push((block_at, inst_at, replacement));
            }
        }
    }
    for (block, inst, replacement) in replacements {
        function.blocks[block].insts[inst] = replacement;
    }
}
