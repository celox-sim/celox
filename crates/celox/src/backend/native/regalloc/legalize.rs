//! Machine constraints expressed as short-lived SSA values.

use crate::backend::native::mir::{MFunction, MInst, SpillDesc, VReg};

use super::assignment::{RegConstraint, use_constraints};

/// Isolate every fixed-register use behind a fresh one-use SSA copy.
///
/// Coloring a long-lived value directly into a fixed register changes its
/// location for the whole live range.  A fresh copy makes the machine
/// constraint local to one instruction and keeps `AssignmentMap` a true
/// function-wide VReg-to-register mapping.
pub(super) fn isolate_fixed_uses(func: &mut MFunction) {
    let (vregs, spill_descs, value_widths, blocks) = (
        &mut func.vregs,
        &mut func.spill_descs,
        &mut func.value_widths,
        &mut func.blocks,
    );

    for block in blocks {
        let mut rewritten = Vec::with_capacity(block.insts.len());
        for mut inst in std::mem::take(&mut block.insts) {
            let uses = inst.uses();
            let constraints = use_constraints(&inst);
            for (source, constraint) in uses.into_iter().zip(constraints) {
                if !matches!(constraint, RegConstraint::Fixed(_)) {
                    continue;
                }
                let fresh = alloc_copy(vregs, spill_descs, value_widths, source);
                rewritten.push(MInst::Mov {
                    dst: fresh,
                    src: source,
                });
                inst.rewrite_use(source, fresh);
            }
            rewritten.push(inst);
        }
        block.insts = rewritten;
    }
}

fn alloc_copy(
    vregs: &mut crate::backend::native::mir::VRegAllocator,
    spill_descs: &mut Vec<SpillDesc>,
    value_widths: &mut Vec<Option<u8>>,
    source: VReg,
) -> VReg {
    let desc = spill_descs
        .get(source.0 as usize)
        .map(SpillDesc::copy_for_snapshot)
        .unwrap_or_else(SpillDesc::transient);
    let width = value_widths.get(source.0 as usize).copied().flatten();
    let fresh = vregs.alloc();
    assert_eq!(fresh.0 as usize, spill_descs.len());
    spill_descs.push(desc);
    if !value_widths.is_empty() {
        assert_eq!(fresh.0 as usize, value_widths.len());
        value_widths.push(width);
    }
    fresh
}
