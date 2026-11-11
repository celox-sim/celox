//! Packed lane comparison encoding: one vector compare per chunk of lanes,
//! reduced to a bit per lane.

use super::*;

/// Emit `PackedLaneCompare`: compare packed lanes in state memory with a
/// scalar or with other lanes, one result bit per lane.
pub(super) fn emit_packed_lane_compare(
    asm: &mut EmitAssembler,
    assignment: &AssignmentMap,
    context: EmissionContext,
    dst: &VReg,
    rhs: &PackedLaneCompareRhs,
    kind: &CmpKind,
    offset: &i32,
    lane_count: &u8,
    element_stride: &u8,
    bit_offset: &u8,
    field_width: &u8,
) -> Result<(), IcedError> {
    let d64 = preg_to_reg64(resolve(assignment, *dst));
    let d32 = preg_to_reg32(resolve(assignment, *dst));
    let stride = usize::from(*element_stride);
    debug_assert!(matches!(stride, 1 | 2 | 4));
    debug_assert_eq!(usize::from(*lane_count) * stride % 16, 0);

    // XMM registers are outside the GPR allocator. The pseudo is one
    // indivisible emission unit, so these temporaries cannot overlap
    // another generated operation.
    if let PackedLaneCompareRhs::Scalar(value) = rhs {
        let scalar = preg_to_reg32(resolve(assignment, *value));
        asm.movd(xmm1, scalar)?;
        match stride {
            1 => {
                asm.punpcklbw(xmm1, xmm1)?;
                asm.punpcklwd(xmm1, xmm1)?;
                asm.pshufd(xmm1, xmm1, 0)?;
            }
            2 => {
                asm.pshuflw(xmm1, xmm1, 0)?;
                asm.pshufd(xmm1, xmm1, 0)?;
            }
            4 => asm.pshufd(xmm1, xmm1, 0)?,
            _ => unreachable!(),
        }
        asm.movdqa(xmm3, xmm1)?;
    }
    let storage_width = stride * 8;
    let needs_mask = usize::from(*field_width) != storage_width;
    debug_assert!(
        matches!(kind, CmpKind::Eq | CmpKind::Ne) || !needs_mask,
        "ordered packed comparisons require a full physical lane"
    );
    if needs_mask {
        let mask = if *field_width == 32 {
            u32::MAX
        } else {
            (1u32 << *field_width) - 1
        };
        asm.mov(d32, mask)?;
        asm.movd(xmm4, d32)?;
        match stride {
            1 => {
                asm.punpcklbw(xmm4, xmm4)?;
                asm.punpcklwd(xmm4, xmm4)?;
                asm.pshufd(xmm4, xmm4, 0)?;
            }
            2 => {
                asm.pshuflw(xmm4, xmm4, 0)?;
                asm.pshufd(xmm4, xmm4, 0)?;
            }
            4 => asm.pshufd(xmm4, xmm4, 0)?,
            _ => unreachable!(),
        }
    }
    let ordered = !matches!(kind, CmpKind::Eq | CmpKind::Ne);
    let unsigned = matches!(
        kind,
        CmpKind::LtU | CmpKind::LeU | CmpKind::GtU | CmpKind::GeU
    );
    let invert = matches!(
        kind,
        CmpKind::Ne | CmpKind::LeU | CmpKind::LeS | CmpKind::GeU | CmpKind::GeS
    );
    let swap = matches!(
        kind,
        CmpKind::LtU | CmpKind::LtS | CmpKind::GeU | CmpKind::GeS
    );
    if ordered && unsigned {
        asm.mov(d32, 1u32 << (storage_width - 1))?;
        asm.movd(xmm5, d32)?;
        match stride {
            1 => {
                asm.punpcklbw(xmm5, xmm5)?;
                asm.punpcklwd(xmm5, xmm5)?;
                asm.pshufd(xmm5, xmm5, 0)?;
            }
            2 => {
                asm.pshuflw(xmm5, xmm5, 0)?;
                asm.pshufd(xmm5, xmm5, 0)?;
            }
            4 => asm.pshufd(xmm5, xmm5, 0)?,
            _ => unreachable!(),
        }
    }
    asm.pxor(xmm2, xmm2)?;
    let lanes_per_chunk = 16 / stride;
    for lane_base in (0..usize::from(*lane_count)).step_by(lanes_per_chunk) {
        let chunk_offset = offset
            .checked_add((lane_base * stride) as i32)
            .expect("packed lane compare offset must fit i32");
        asm.movdqu(
            xmm0,
            xmmword_ptr(mem_operand(&context, BaseReg::SimState, chunk_offset)),
        )?;
        if let PackedLaneCompareRhs::Memory { offset, .. } = rhs {
            let rhs_chunk_offset = offset
                .checked_add((lane_base * stride) as i32)
                .expect("packed lane compare RHS offset must fit i32");
            asm.movdqu(
                xmm1,
                xmmword_ptr(mem_operand(&context, BaseReg::SimState, rhs_chunk_offset)),
            )?;
        } else {
            asm.movdqa(xmm1, xmm3)?;
        }
        if *bit_offset != 0 {
            match stride {
                2 => {
                    asm.psrlw(xmm0, u32::from(*bit_offset))?;
                    if matches!(rhs, PackedLaneCompareRhs::Memory { .. }) {
                        asm.psrlw(xmm1, u32::from(*bit_offset))?;
                    }
                }
                4 => {
                    asm.psrld(xmm0, u32::from(*bit_offset))?;
                    if matches!(rhs, PackedLaneCompareRhs::Memory { .. }) {
                        asm.psrld(xmm1, u32::from(*bit_offset))?;
                    }
                }
                _ => unreachable!("byte-lane shifts are rejected by ISel"),
            }
        }
        if needs_mask {
            asm.pand(xmm0, xmm4)?;
            asm.pand(xmm1, xmm4)?;
        }
        let result = if ordered {
            if unsigned {
                asm.pxor(xmm0, xmm5)?;
                asm.pxor(xmm1, xmm5)?;
            }
            match (stride, swap) {
                (1, false) => asm.pcmpgtb(xmm0, xmm1)?,
                (1, true) => asm.pcmpgtb(xmm1, xmm0)?,
                (2, false) => asm.pcmpgtw(xmm0, xmm1)?,
                (2, true) => asm.pcmpgtw(xmm1, xmm0)?,
                (4, false) => asm.pcmpgtd(xmm0, xmm1)?,
                (4, true) => asm.pcmpgtd(xmm1, xmm0)?,
                _ => unreachable!(),
            }
            if swap { xmm1 } else { xmm0 }
        } else {
            match stride {
                1 => asm.pcmpeqb(xmm0, xmm1)?,
                2 => asm.pcmpeqw(xmm0, xmm1)?,
                4 => asm.pcmpeqd(xmm0, xmm1)?,
                _ => unreachable!(),
            }
            xmm0
        };
        if invert {
            let inverse_temp = if ordered && swap { xmm0 } else { xmm1 };
            asm.pcmpeqd(inverse_temp, inverse_temp)?;
            asm.pxor(result, inverse_temp)?;
        }
        match stride {
            1 => asm.pmovmskb(d32, result)?,
            2 => {
                asm.packsswb(result, result)?;
                asm.pmovmskb(d32, result)?;
                asm.and(d32, 0xff)?;
            }
            4 => asm.movmskps(d32, result)?,
            _ => unreachable!(),
        }
        if lane_base != 0 {
            asm.shl(d64, lane_base as u32)?;
        }
        asm.movd(result, d32)?;
        asm.por(xmm2, result)?;
    }
    asm.movq(d64, xmm2)?;
    Ok(())
}

/// Emit `PackedByteAffineCompare`: compare the sixteen bytes `base + i`
/// with one byte, one result bit per byte.
pub(super) fn emit_packed_byte_affine_compare(
    asm: &mut EmitAssembler,
    assignment: &AssignmentMap,
    dst: &VReg,
    base: &VReg,
    rhs: &VReg,
    kind: &CmpKind,
) -> Result<(), IcedError> {
    let d64 = preg_to_reg64(resolve(assignment, *dst));
    let d32 = preg_to_reg32(resolve(assignment, *dst));
    let base32 = preg_to_reg32(resolve(assignment, *base));
    let rhs32 = preg_to_reg32(resolve(assignment, *rhs));

    // Read both inputs before using the output register as scratch:
    // allocation may coalesce a dying input with this definition.
    asm.movd(xmm3, base32)?;
    asm.punpcklbw(xmm3, xmm3)?;
    asm.punpcklwd(xmm3, xmm3)?;
    asm.pshufd(xmm3, xmm3, 0)?;
    asm.movd(xmm4, rhs32)?;
    asm.punpcklbw(xmm4, xmm4)?;
    asm.punpcklwd(xmm4, xmm4)?;
    asm.pshufd(xmm4, xmm4, 0)?;

    asm.mov(d64, 0x0706_0504_0302_0100_i64)?;
    asm.movq(xmm0, d64)?;
    asm.mov(d64, 0x0f0e_0d0c_0b0a_0908_i64)?;
    asm.movq(xmm2, d64)?;
    asm.punpcklqdq(xmm0, xmm2)?;
    asm.paddb(xmm0, xmm3)?;

    let invert = match kind {
        CmpKind::Eq => {
            asm.pcmpeqb(xmm0, xmm4)?;
            false
        }
        CmpKind::Ne => {
            asm.pcmpeqb(xmm0, xmm4)?;
            true
        }
        CmpKind::LtU | CmpKind::GeU => {
            // Saturated rhs-lhs is nonzero exactly when lhs < rhs.
            asm.movdqa(xmm2, xmm4)?;
            asm.psubusb(xmm2, xmm0)?;
            asm.pxor(xmm1, xmm1)?;
            asm.pcmpeqb(xmm2, xmm1)?;
            asm.movdqa(xmm0, xmm2)?;
            matches!(kind, CmpKind::LtU)
        }
        CmpKind::GtU | CmpKind::LeU => {
            // Saturated lhs-rhs is nonzero exactly when lhs > rhs.
            asm.movdqa(xmm2, xmm0)?;
            asm.psubusb(xmm2, xmm4)?;
            asm.pxor(xmm1, xmm1)?;
            asm.pcmpeqb(xmm2, xmm1)?;
            asm.movdqa(xmm0, xmm2)?;
            matches!(kind, CmpKind::GtU)
        }
        CmpKind::LtS | CmpKind::GeS => {
            asm.movdqa(xmm2, xmm4)?;
            asm.pcmpgtb(xmm2, xmm0)?;
            asm.movdqa(xmm0, xmm2)?;
            matches!(kind, CmpKind::GeS)
        }
        CmpKind::GtS | CmpKind::LeS => {
            asm.pcmpgtb(xmm0, xmm4)?;
            matches!(kind, CmpKind::LeS)
        }
    };
    asm.pmovmskb(d32, xmm0)?;
    if invert {
        asm.xor(d32, 0xffff)?;
    }
    Ok(())
}
