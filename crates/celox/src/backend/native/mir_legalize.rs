use std::collections::{HashMap, HashSet};

use super::mir::*;

pub fn legalize(func: &mut MFunction) {
    eliminate_trivial_phis(func);
    legalize_variable_shift_counts(func);
}

/// Make the MIR's non-wrapping shift-count semantics explicit before x86
/// emission. x86 masks variable counts (modulo 64 for a 64-bit operand), while
/// MIR defines logical shifts by counts >= 64 as zero and arithmetic shifts as
/// a sign fill. Every variable shift selects the architectural result only when
/// the unsigned count is below 64; the raw x86 shift is never exposed directly.
pub(crate) fn legalize_variable_shift_counts(func: &mut MFunction) {
    let (blocks, vregs, spill_descs, value_widths) = (
        &mut func.blocks,
        &mut func.vregs,
        &mut func.spill_descs,
        &mut func.value_widths,
    );

    for block in blocks {
        let legalization_count = block
            .insts
            .iter()
            .filter(|inst| {
                matches!(
                    inst,
                    MInst::Shr { .. } | MInst::Shl { .. } | MInst::Sar { .. }
                )
            })
            .count();
        if legalization_count == 0 {
            continue;
        }

        let mut rewritten = Vec::with_capacity(block.insts.len() + legalization_count * 2);
        for inst in std::mem::take(&mut block.insts) {
            match inst {
                MInst::Shr { dst, lhs, rhs } => {
                    let raw = alloc_shift_temp(vregs, spill_descs, value_widths, None, false);
                    let zero = alloc_shift_temp(vregs, spill_descs, value_widths, Some(0), true);
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
                    let raw = alloc_shift_temp(vregs, spill_descs, value_widths, None, false);
                    let zero = alloc_shift_temp(vregs, spill_descs, value_widths, Some(0), true);
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
                    let raw = alloc_shift_temp(vregs, spill_descs, value_widths, None, false);
                    let sign_fill = alloc_shift_temp(vregs, spill_descs, value_widths, None, false);
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
                inst => rewritten.push(inst),
            }
        }
        block.insts = rewritten;
    }
}

fn alloc_shift_temp(
    vregs: &mut VRegAllocator,
    spill_descs: &mut Vec<SpillDesc>,
    value_widths: &mut Vec<Option<u8>>,
    width: Option<u8>,
    rematerialize_zero: bool,
) -> VReg {
    let vreg = vregs.alloc();
    debug_assert_eq!(spill_descs.len(), vreg.0 as usize);
    spill_descs.push(if rematerialize_zero {
        SpillDesc::remat(0)
    } else {
        SpillDesc::transient()
    });
    if !value_widths.is_empty() {
        debug_assert_eq!(value_widths.len(), vreg.0 as usize);
        value_widths.push(width);
    }
    vreg
}

fn eliminate_trivial_phis(func: &mut MFunction) {
    let mut aliases: HashMap<VReg, VReg> = HashMap::new();

    for block in &func.blocks {
        for phi in &block.phis {
            if phi.sources.is_empty() {
                continue;
            }
            let mut unique_src = None;
            let mut trivial = true;
            for (_, src) in &phi.sources {
                match unique_src {
                    None => unique_src = Some(*src),
                    Some(existing) if existing == *src => {}
                    Some(_) => {
                        trivial = false;
                        break;
                    }
                }
            }
            if trivial {
                if let Some(src) = unique_src {
                    if src != phi.dst {
                        aliases.insert(phi.dst, src);
                    }
                }
            }
        }
    }

    if aliases.is_empty() {
        return;
    }

    let mut resolved: HashMap<VReg, VReg> = HashMap::with_capacity(aliases.len());
    for (&dst, &src) in &aliases {
        let mut target = src;
        let mut seen = HashSet::from([dst]);
        let mut cyclic = false;
        while let Some(&next) = aliases.get(&target) {
            if !seen.insert(target) || !seen.insert(next) {
                cyclic = true;
                break;
            }
            target = next;
        }
        if !cyclic {
            resolved.insert(dst, target);
        }
    }
    aliases = resolved;

    for block in &mut func.blocks {
        for inst in &mut block.insts {
            rewrite_uses(inst, &aliases);
        }
        for phi in &mut block.phis {
            for (_, src) in &mut phi.sources {
                if let Some(&alias) = aliases.get(src) {
                    *src = alias;
                }
            }
        }
        block
            .phis
            .retain(|phi| !matches!(aliases.get(&phi.dst), Some(src) if *src != phi.dst));
    }
}

fn rewrite_uses(inst: &mut MInst, aliases: &HashMap<VReg, VReg>) {
    let current_uses = inst.uses();
    for u in current_uses {
        if let Some(&target) = aliases.get(&u) {
            inst.rewrite_use(u, target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eliminates_single_source_phi() {
        let mut vregs = VRegAllocator::new();
        let src = vregs.alloc();
        let dst = vregs.alloc();
        let out = vregs.alloc();
        let spill_descs = vec![
            SpillDesc::transient(),
            SpillDesc::transient(),
            SpillDesc::transient(),
        ];
        let mut func = MFunction::new(vregs, spill_descs);

        let mut block = MBlock::new(BlockId(0));
        block.phis.push(PhiNode {
            dst,
            sources: vec![(BlockId(1), src)],
        });
        block.push(MInst::Mov { dst: out, src: dst });
        block.push(MInst::Return);
        func.push_block(block);

        legalize(&mut func);

        assert!(func.blocks[0].phis.is_empty());
        assert!(matches!(
            func.blocks[0].insts[0],
            MInst::Mov { dst: d, src: s } if d == out && s == src
        ));
    }

    #[test]
    fn leaves_trivial_phi_cycles_intact() {
        let mut vregs = VRegAllocator::new();
        let v0 = vregs.alloc();
        let v1 = vregs.alloc();
        let spill_descs = vec![SpillDesc::transient(), SpillDesc::transient()];
        let mut func = MFunction::new(vregs, spill_descs);

        let mut block = MBlock::new(BlockId(0));
        block.phis.push(PhiNode {
            dst: v0,
            sources: vec![(BlockId(0), v1)],
        });
        block.phis.push(PhiNode {
            dst: v1,
            sources: vec![(BlockId(0), v0)],
        });
        block.push(MInst::Return);
        func.push_block(block);

        legalize(&mut func);

        assert_eq!(func.blocks[0].phis.len(), 2);
        assert_eq!(func.blocks[0].phis[0].sources[0].1, v1);
        assert_eq!(func.blocks[0].phis[1].sources[0].1, v0);
    }

    #[test]
    fn large_variable_shift_counts_are_made_explicit() {
        let mut vregs = VRegAllocator::new();
        let lhs = vregs.alloc();
        let count = vregs.alloc();
        let shl = vregs.alloc();
        let shr = vregs.alloc();
        let sar = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
        func.value_widths = vec![Some(64), Some(7), None, None, None];

        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: lhs,
            value: u64::MAX,
        });
        block.push(MInst::LoadImm {
            dst: count,
            value: 64,
        });
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
        func.push_block(block);

        legalize_variable_shift_counts(&mut func);

        assert_eq!(func.vregs.count(), 11);
        assert_eq!(func.spill_descs.len(), 11);
        assert_eq!(func.value_widths.len(), 11);
        assert_eq!(
            func.blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(inst, MInst::CmpImmSelect { imm: 64, .. }))
                .count(),
            3
        );
        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::SarImm {
                src,
                imm: 63,
                ..
            } if *src == lhs
        )));
        func.verify_result().unwrap();
    }

    #[test]
    fn all_variable_shift_counts_are_legalized() {
        let mut vregs = VRegAllocator::new();
        let lhs = vregs.alloc();
        let count = vregs.alloc();
        let dst = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
        func.value_widths = vec![Some(1), Some(6), None];

        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm { dst: lhs, value: 1 });
        block.push(MInst::LoadImm {
            dst: count,
            value: 63,
        });
        block.push(MInst::Shl {
            dst,
            lhs,
            rhs: count,
        });
        block.push(MInst::Return);
        func.push_block(block);

        legalize_variable_shift_counts(&mut func);

        assert_eq!(func.vregs.count(), 5);
        assert_eq!(func.blocks[0].insts.len(), 6);
        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::CmpImmSelect {
                dst: selected,
                imm: 64,
                ..
            } if *selected == dst
        )));
        func.verify_result().unwrap();
    }
}
