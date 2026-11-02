//! Capture adjacent value/mask planes together for case comparisons and FF commits.
use super::*;

#[derive(Clone, Copy)]
pub(super) struct PackedPlanes {
    bits: VReg,
    width: usize,
    plane_bits: usize,
}

/// Select paired load/slice chains feeding case comparisons. Both operands
/// must have compatible packed storage; constant or computed operands keep
/// ordinary loads, avoiding an unused packed representation.
pub(super) fn compared_values(
    ctx: &ISelContext,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, usize>,
) -> HashSet<RegisterId> {
    let mut result = HashSet::default();
    if !ctx.four_state {
        return result;
    }
    let trace = |mut current: RegisterId| {
        let mut chain = Vec::new();
        for _ in 0..32 {
            chain.push(current);
            let index = *definitions.get(&current)?;
            match &block.instructions[index] {
                SIRInstruction::Load(dst, address, SIROffset::Static(0), width)
                    if ctx.sir_width(dst) == *width =>
                {
                    return Some((chain, scalar_plane(ctx, address, *width)?));
                }
                SIRInstruction::Slice(_, source, offset, width)
                    if *width > 0
                        && *offset <= ctx.sir_width(source)
                        && *width <= ctx.sir_width(source) - offset =>
                {
                    current = *source;
                }
                _ => return None,
            }
        }
        None
    };
    for instruction in &block.instructions {
        let SIRInstruction::Binary(_, lhs, BinaryOp::EqCase | BinaryOp::NeCase, rhs) = instruction
        else {
            continue;
        };
        if ctx.sir_width(lhs) != ctx.sir_width(rhs) {
            continue;
        }
        if let (Some((a, ap)), Some((b, bp))) = (trace(*lhs), trace(*rhs))
            && ap == bp
        {
            result.extend(a);
            result.extend(b);
        }
    }
    result
}

fn scalar_plane(ctx: &ISelContext, address: &RegionedAbsoluteAddr, width: usize) -> Option<OpSize> {
    if address.region == crate::SPARSE_WORKING_REGION
        || !ctx.is_4state_var(address)
        || ctx
            .layout
            .unpacked_arrays
            .contains_key(&address.absolute_addr())
    {
        return None;
    }
    let size = ctx.full_static_load_size(address, 0, width)?;
    (size.bytes() <= 4 && ctx.layout.plane_size(&address.absolute_addr()) == size.bytes() as usize)
        .then_some(size)
}

pub(super) fn capture(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    instruction: &SIRInstruction<RegionedAbsoluteAddr>,
) -> bool {
    let SIRInstruction::Load(dst, address, SIROffset::Static(0), width) = instruction else {
        return false;
    };
    if !ctx.case_compared_values.contains(dst) || ctx.sir_width(dst) != *width {
        return false;
    }
    let Some(plane) = scalar_plane(ctx, address, *width) else {
        return false;
    };
    let plane_bits = plane.bytes() as usize * 8;
    let raw = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Load {
        dst: raw,
        base: BaseReg::SimState,
        offset: ctx.byte_offset(address, 0),
        size: OpSize::from_bits(plane_bits * 2).unwrap(),
    });
    // Exclude padding in both planes, exactly as the ordinary logical loads do.
    let logical = mask_for_width(*width);
    let packed = ctx.alloc_vreg(SpillDesc::transient());
    ctx.emit_and_imm(block, packed, raw, logical | (logical << plane_bits));
    let value = ctx.reg_map.get(*dst);
    ctx.emit_and_imm(block, value, packed, logical);
    let mask = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::ShrImm {
        dst: mask,
        src: packed,
        imm: plane_bits as u8,
    });
    ctx.known_bits.insert(packed, plane_bits + *width);
    ctx.known_bits.insert(value, *width);
    ctx.known_bits.insert(mask, *width);
    ctx.set_mask(*dst, mask);
    ctx.packed_planes.insert(
        *dst,
        PackedPlanes {
            bits: packed,
            width: *width,
            plane_bits,
        },
    );
    true
}

pub(super) fn slice(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    src: RegisterId,
    offset: usize,
    width: usize,
) {
    if !ctx.case_compared_values.contains(&dst) {
        return;
    }
    let Some(&source) = ctx.packed_planes.get(&src) else {
        return;
    };
    if width == 0 || offset > source.width || width > source.width - offset {
        return;
    }
    if offset == 0 && width == source.width {
        ctx.packed_planes.insert(dst, source);
        return;
    }
    let shifted = if offset == 0 {
        source.bits
    } else {
        let shifted = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::ShrImm {
            dst: shifted,
            src: source.bits,
            imm: offset as u8,
        });
        shifted
    };
    let bits = ctx.alloc_vreg(SpillDesc::transient());
    let logical = mask_for_width(width);
    ctx.emit_and_imm(
        block,
        bits,
        shifted,
        logical | (logical << source.plane_bits),
    );
    ctx.known_bits.insert(bits, source.plane_bits + width);
    ctx.packed_planes.insert(
        dst,
        PackedPlanes {
            bits,
            width,
            plane_bits: source.plane_bits,
        },
    );
}

pub(super) fn compare(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: VReg,
    lhs: RegisterId,
    rhs: RegisterId,
    op: BinaryOp,
) -> bool {
    let Some(&a) = ctx.packed_planes.get(&lhs) else {
        return false;
    };
    let Some(&b) = ctx.packed_planes.get(&rhs) else {
        return false;
    };
    if (a.width, a.plane_bits) != (b.width, b.plane_bits) {
        return false;
    }
    block.push(MInst::Cmp {
        dst,
        lhs: a.bits,
        rhs: b.bits,
        kind: if op == BinaryOp::EqCase {
            CmpKind::Eq
        } else {
            CmpKind::Ne
        },
    });
    true
}

pub(super) fn commit(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    instruction: &SIRInstruction<RegionedAbsoluteAddr>,
) -> bool {
    let SIRInstruction::Commit(src, dst, SIROffset::Static(0), width, _) = instruction else {
        return false;
    };
    let Some(a) = scalar_plane(ctx, src, *width) else {
        return false;
    };
    let Some(b) = scalar_plane(ctx, dst, *width) else {
        return false;
    };
    if a != b {
        return false;
    }
    let size = OpSize::from_bits(a.bytes() as usize * 16).unwrap();
    let source = ctx.byte_offset(src, 0);
    let destination = ctx.byte_offset(dst, 0);
    if source != destination
        && i64::from(source) < i64::from(destination) + i64::from(size.bytes())
        && i64::from(destination) < i64::from(source) + i64::from(size.bytes())
    {
        return false;
    }
    // Whole objects own their padding. This copies the same physical bytes as
    // the two ordinary full-plane commits, without partial-store forwarding.
    let bits = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Load {
        dst: bits,
        base: BaseReg::SimState,
        offset: source,
        size,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: destination,
        src: bits,
        size,
    });
    ctx.trigger_only_seen.clear();
    true
}
