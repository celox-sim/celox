//! Packed lane comparison encoding with NEON.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_packed_lane_compare(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    rhs: PackedLaneCompareRhs,
    kind: CmpKind,
    offset: i32,
    lane_count: u8,
    element_stride: u8,
    bit_offset: u8,
    field_width: u8,
    assignment: &Assignment<VReg>,
    state_pages: StatePageBases,
) -> Result<(), EmitError> {
    let size = match element_stride {
        1 => OpSize::S8,
        2 => OpSize::S16,
        4 => OpSize::S32,
        _ => return Err(EmitError::Range("packed lane stride must be 1, 2, or 4")),
    };
    if usize::from(lane_count) * usize::from(element_stride) % 16 == 0 {
        emit_packed_lane_compare_vector(
            ops,
            destination,
            rhs,
            kind,
            offset,
            lane_count,
            element_stride,
            bit_offset,
            field_width,
            assignment,
            state_pages,
        )?;
        return Ok(());
    }
    let scalar_rhs = match rhs {
        PackedLaneCompareRhs::Scalar(value) => Some(resolve(assignment, value)?),
        PackedLaneCompareRhs::Memory { .. } => None,
    };
    dynasm!(ops ; .arch aarch64 ; mov x30, xzr);
    let field_mask = if field_width == 64 {
        u64::MAX
    } else {
        (1_u64 << field_width) - 1
    };
    for lane in 0..lane_count {
        let lane_delta = i32::from(lane)
            .checked_mul(i32::from(element_stride))
            .and_then(|delta| offset.checked_add(delta))
            .ok_or(EmitError::Range("packed lane offset overflow"))?;
        let (base, offset) = select_memory_base(
            BaseReg::SimState,
            i64::from(lane_delta),
            SCRATCH0,
            size,
            false,
            state_pages,
        );
        emit_load_at(ops, SCRATCH0, base, offset, size);
        match rhs {
            PackedLaneCompareRhs::Scalar(_) => {
                dynasm!(ops ; .arch aarch64 ; mov x17, X(scalar_rhs.unwrap()));
            }
            PackedLaneCompareRhs::Memory { offset, .. } => {
                let rhs_offset = i32::from(lane)
                    .checked_mul(i32::from(element_stride))
                    .and_then(|delta| offset.checked_add(delta))
                    .ok_or(EmitError::Range("packed lane RHS offset overflow"))?;
                let (base, offset) = select_memory_base(
                    BaseReg::SimState,
                    i64::from(rhs_offset),
                    SCRATCH1,
                    size,
                    false,
                    state_pages,
                );
                let direct = memory_access_encoding(SCRATCH1, base, offset, size, false).is_some();
                if !direct {
                    dynasm!(ops ; .arch aarch64 ; fmov d7, x16);
                }
                emit_load_at(ops, SCRATCH1, base, offset, size);
                if !direct {
                    dynasm!(ops ; .arch aarch64 ; fmov x16, d7);
                }
            }
        }
        if bit_offset != 0 {
            let shift = u32::from(bit_offset);
            dynasm!(ops
                ; .arch aarch64
                ; lsr x16, x16, shift
                ; lsr x17, x17, shift
            );
        }
        let storage_width = element_stride * 8;
        if field_width != storage_width {
            if logical_immediate_encoding(field_mask, 64, SCRATCH0, SCRATCH0, true).is_some() {
                emit_logical_immediate(ops, SCRATCH0, SCRATCH0, field_mask, 64, true);
                emit_logical_immediate(ops, SCRATCH1, SCRATCH1, field_mask, 64, true);
            } else {
                dynasm!(ops ; .arch aarch64 ; fmov d5, x30);
                emit_load_imm(ops, 30, field_mask);
                dynasm!(ops
                    ; .arch aarch64
                    ; and x16, x16, x30
                    ; and x17, x17, x30
                    ; fmov x30, d5
                );
            }
        }
        if matches!(
            kind,
            CmpKind::LtS | CmpKind::LeS | CmpKind::GtS | CmpKind::GeS
        ) {
            match storage_width {
                8 => dynasm!(ops ; .arch aarch64 ; sxtb x16, w16 ; sxtb x17, w17),
                16 => dynasm!(ops ; .arch aarch64 ; sxth x16, w16 ; sxth x17, w17),
                32 => dynasm!(ops ; .arch aarch64 ; sxtw x16, w16 ; sxtw x17, w17),
                _ => unreachable!(),
            }
        }
        dynasm!(ops ; .arch aarch64 ; cmp x16, x17);
        emit_cset(ops, SCRATCH0, kind);
        if lane != 0 {
            let shift = u32::from(lane);
            dynasm!(ops ; .arch aarch64 ; lsl x16, x16, shift);
        }
        dynasm!(ops ; .arch aarch64 ; orr x30, x30, x16);
    }
    dynasm!(ops ; .arch aarch64 ; mov X(destination), x30);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_packed_lane_compare_vector(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    rhs: PackedLaneCompareRhs,
    kind: CmpKind,
    offset: i32,
    lane_count: u8,
    element_stride: u8,
    bit_offset: u8,
    field_width: u8,
    assignment: &Assignment<VReg>,
    state_pages: StatePageBases,
) -> Result<(), EmitError> {
    let lanes_per_vector = 16 / usize::from(element_stride);
    let storage_width = element_stride * 8;
    let scalar_rhs = match rhs {
        PackedLaneCompareRhs::Scalar(value) => Some(resolve(assignment, value)?),
        PackedLaneCompareRhs::Memory { .. } => None,
    };
    let needs_mask = field_width != storage_width;
    let field_mask = if field_width == 64 {
        u64::MAX
    } else {
        (1_u64 << field_width) - 1
    };

    if needs_mask {
        emit_load_imm(ops, SCRATCH1, field_mask);
        match element_stride {
            1 => dynasm!(ops ; .arch aarch64 ; dup v4.b16, W(SCRATCH1)),
            2 => dynasm!(ops ; .arch aarch64 ; dup v4.h8, W(SCRATCH1)),
            4 => dynasm!(ops ; .arch aarch64 ; dup v4.s4, W(SCRATCH1)),
            _ => unreachable!(),
        }
    }
    dynasm!(ops ; .arch aarch64 ; mov x30, xzr);

    for lane_base in (0..usize::from(lane_count)).step_by(lanes_per_vector) {
        let chunk_delta = i32::try_from(lane_base * usize::from(element_stride))
            .map_err(|_| EmitError::Range("packed lane offset overflow"))?;
        let lhs_offset = offset
            .checked_add(chunk_delta)
            .ok_or(EmitError::Range("packed lane offset overflow"))?;
        let (lhs_base, lhs_offset) =
            select_vector_memory_base(BaseReg::SimState, i64::from(lhs_offset), state_pages);
        emit_vector_load(ops, 0, lhs_base, lhs_offset);

        if let PackedLaneCompareRhs::Memory {
            offset: rhs_offset, ..
        } = rhs
        {
            let rhs_offset = rhs_offset
                .checked_add(chunk_delta)
                .ok_or(EmitError::Range("packed lane RHS offset overflow"))?;
            let (rhs_base, rhs_offset) =
                select_vector_memory_base(BaseReg::SimState, i64::from(rhs_offset), state_pages);
            emit_vector_load(ops, 1, rhs_base, rhs_offset);
        } else if let Some(rhs) = scalar_rhs {
            match element_stride {
                1 => dynasm!(ops ; .arch aarch64 ; dup v1.b16, W(rhs)),
                2 => dynasm!(ops ; .arch aarch64 ; dup v1.h8, W(rhs)),
                4 => dynasm!(ops ; .arch aarch64 ; dup v1.s4, W(rhs)),
                _ => unreachable!(),
            }
        }

        if bit_offset != 0 {
            let shift = u32::from(bit_offset);
            match element_stride {
                1 => dynasm!(ops ; .arch aarch64 ; ushr v0.b16, v0.b16, #shift),
                2 => dynasm!(ops ; .arch aarch64 ; ushr v0.h8, v0.h8, #shift),
                4 => dynasm!(ops ; .arch aarch64 ; ushr v0.s4, v0.s4, #shift),
                _ => unreachable!(),
            }
            match element_stride {
                1 => dynasm!(ops ; .arch aarch64 ; ushr v1.b16, v1.b16, #shift),
                2 => dynasm!(ops ; .arch aarch64 ; ushr v1.h8, v1.h8, #shift),
                4 => dynasm!(ops ; .arch aarch64 ; ushr v1.s4, v1.s4, #shift),
                _ => unreachable!(),
            }
        }
        if needs_mask {
            match element_stride {
                1 => dynasm!(ops
                    ; .arch aarch64
                    ; and v0.b16, v0.b16, v4.b16
                    ; and v1.b16, v1.b16, v4.b16
                ),
                2 => dynasm!(ops
                    ; .arch aarch64
                    ; and v0.b16, v0.b16, v4.b16
                    ; and v1.b16, v1.b16, v4.b16
                ),
                4 => dynasm!(ops
                    ; .arch aarch64
                    ; and v0.b16, v0.b16, v4.b16
                    ; and v1.b16, v1.b16, v4.b16
                ),
                _ => unreachable!(),
            }
        }

        emit_packed_vector_compare(ops, element_stride, kind);
        emit_packed_vector_mask(ops, element_stride);
        if lane_base != 0 {
            let shift = u32::try_from(lane_base)
                .map_err(|_| EmitError::Range("packed lane result shift overflow"))?;
            dynasm!(ops ; .arch aarch64 ; lsl x16, x16, #shift);
        }
        dynasm!(ops ; .arch aarch64 ; orr x30, x30, x16);
    }
    dynasm!(ops ; .arch aarch64 ; mov X(destination), x30);
    Ok(())
}

pub(super) fn emit_vector_load(
    ops: &mut VecAssembler<Aarch64Relocation>,
    vector: u8,
    base: u8,
    offset: i64,
) {
    emit_address_to(ops, SCRATCH0, base, offset);
    match vector {
        0 => dynasm!(ops ; .arch aarch64 ; ldr q0, [x16]),
        1 => dynasm!(ops ; .arch aarch64 ; ldr q1, [x16]),
        _ => unreachable!(),
    }
}

pub(super) fn emit_packed_vector_compare(
    ops: &mut VecAssembler<Aarch64Relocation>,
    element_stride: u8,
    kind: CmpKind,
) {
    macro_rules! compare {
        ($instruction:ident, $shape:ident) => {
            dynasm!(ops ; .arch aarch64 ; $instruction v0.$shape, v0.$shape, v1.$shape)
        };
    }
    macro_rules! compare_swapped {
        ($instruction:ident, $shape:ident) => {
            dynasm!(ops ; .arch aarch64 ; $instruction v0.$shape, v1.$shape, v0.$shape)
        };
    }
    macro_rules! invert {
        ($shape:ident) => {
            dynasm!(ops ; .arch aarch64 ; mvn v0.b16, v0.b16)
        };
    }

    match (element_stride, kind) {
        (1, CmpKind::Eq) => compare!(cmeq, b16),
        (2, CmpKind::Eq) => compare!(cmeq, h8),
        (4, CmpKind::Eq) => compare!(cmeq, s4),
        (1, CmpKind::Ne) => {
            compare!(cmeq, b16);
            invert!(b16);
        }
        (2, CmpKind::Ne) => {
            compare!(cmeq, h8);
            invert!(h8);
        }
        (4, CmpKind::Ne) => {
            compare!(cmeq, s4);
            invert!(s4);
        }
        (1, CmpKind::LtU | CmpKind::GeU) => {
            compare_swapped!(cmhi, b16);
            if matches!(kind, CmpKind::GeU) {
                invert!(b16);
            }
        }
        (2, CmpKind::LtU | CmpKind::GeU) => {
            compare_swapped!(cmhi, h8);
            if matches!(kind, CmpKind::GeU) {
                invert!(h8);
            }
        }
        (4, CmpKind::LtU | CmpKind::GeU) => {
            compare_swapped!(cmhi, s4);
            if matches!(kind, CmpKind::GeU) {
                invert!(s4);
            }
        }
        (1, CmpKind::GtU | CmpKind::LeU) => {
            compare!(cmhi, b16);
            if matches!(kind, CmpKind::LeU) {
                invert!(b16);
            }
        }
        (2, CmpKind::GtU | CmpKind::LeU) => {
            compare!(cmhi, h8);
            if matches!(kind, CmpKind::LeU) {
                invert!(h8);
            }
        }
        (4, CmpKind::GtU | CmpKind::LeU) => {
            compare!(cmhi, s4);
            if matches!(kind, CmpKind::LeU) {
                invert!(s4);
            }
        }
        (1, CmpKind::LtS | CmpKind::GeS) => {
            compare_swapped!(cmgt, b16);
            if matches!(kind, CmpKind::GeS) {
                invert!(b16);
            }
        }
        (2, CmpKind::LtS | CmpKind::GeS) => {
            compare_swapped!(cmgt, h8);
            if matches!(kind, CmpKind::GeS) {
                invert!(h8);
            }
        }
        (4, CmpKind::LtS | CmpKind::GeS) => {
            compare_swapped!(cmgt, s4);
            if matches!(kind, CmpKind::GeS) {
                invert!(s4);
            }
        }
        (1, CmpKind::GtS | CmpKind::LeS) => {
            compare!(cmgt, b16);
            if matches!(kind, CmpKind::LeS) {
                invert!(b16);
            }
        }
        (2, CmpKind::GtS | CmpKind::LeS) => {
            compare!(cmgt, h8);
            if matches!(kind, CmpKind::LeS) {
                invert!(h8);
            }
        }
        (4, CmpKind::GtS | CmpKind::LeS) => {
            compare!(cmgt, s4);
            if matches!(kind, CmpKind::LeS) {
                invert!(s4);
            }
        }
        _ => unreachable!(),
    }
}

pub(super) fn emit_packed_vector_mask(
    ops: &mut VecAssembler<Aarch64Relocation>,
    element_stride: u8,
) {
    match element_stride {
        1 => {
            emit_load_imm(ops, SCRATCH1, 0x8040_2010_0804_0201);
            dynasm!(ops
                ; .arch aarch64
                ; ushr v0.b16, v0.b16, #7
                ; fmov d6, x17
                ; dup v6.d2, v6.d[0]
                ; mul v0.b16, v0.b16, v6.b16
                ; addv b5, v0.b8
                ; umov W(SCRATCH0), v5.b[0]
                ; ext v7.b16, v0.b16, v0.b16, #8
                ; addv b5, v7.b8
                ; umov W(SCRATCH1), v5.b[0]
                ; lsl x17, x17, #8
                ; orr x16, x16, x17
            );
        }
        2 => {
            emit_load_imm(ops, SCRATCH1, 0x0008_0004_0002_0001);
            dynasm!(ops ; .arch aarch64 ; fmov d6, x17);
            emit_load_imm(ops, SCRATCH1, 0x0080_0040_0020_0010);
            dynasm!(ops
                ; .arch aarch64
                ; fmov d7, x17
                ; ins v6.d[1], v7.d[0]
                ; ushr v0.h8, v0.h8, #15
                ; mul v0.h8, v0.h8, v6.h8
                ; addv h5, v0.h8
                ; umov W(SCRATCH0), v5.h[0]
            );
        }
        4 => {
            emit_load_imm(ops, SCRATCH1, 0x0000_0002_0000_0001);
            dynasm!(ops ; .arch aarch64 ; fmov d6, x17);
            emit_load_imm(ops, SCRATCH1, 0x0000_0008_0000_0004);
            dynasm!(ops
                ; .arch aarch64
                ; fmov d7, x17
                ; ins v6.d[1], v7.d[0]
                ; ushr v0.s4, v0.s4, #31
                ; mul v0.s4, v0.s4, v6.s4
                ; addv s5, v0.s4
                ; umov W(SCRATCH0), v5.s[0]
            );
        }
        _ => unreachable!(),
    }
}

pub(super) fn emit_packed_byte_affine_compare(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    base: u8,
    rhs: u8,
    kind: CmpKind,
) {
    // This pseudo always compares exactly sixteen byte lanes.  Keep the
    // affine sequence in NEON registers, then turn each 0xff/0x00 predicate
    // into one bit with a byte-weighted horizontal sum.  The scalar fallback
    // used to materialize and compare every lane independently.
    emit_load_imm(ops, 30, 0x0706_0504_0302_0100);
    dynasm!(ops ; .arch aarch64 ; fmov d2, x30);
    emit_load_imm(ops, 30, 0x0f0e_0d0c_0b0a_0908);
    dynasm!(ops
        ; .arch aarch64
        ; fmov d3, x30
        ; ins v2.d[1], v3.d[0]
        ; dup v4.b16, W(base)
        ; dup v1.b16, W(rhs)
        ; add v0.b16, v2.b16, v4.b16
        ; movi v5.b16, #0
    );
    match kind {
        CmpKind::Eq => {
            dynasm!(ops ; .arch aarch64 ; cmeq v0.b16, v0.b16, v1.b16);
        }
        CmpKind::Ne => {
            dynasm!(ops
                ; .arch aarch64
                ; cmeq v0.b16, v0.b16, v1.b16
                ; mvn v0.b16, v0.b16
            );
        }
        CmpKind::LtU | CmpKind::GeU => {
            dynasm!(ops
                ; .arch aarch64
                ; uqsub v2.b16, v1.b16, v0.b16
                ; cmeq v0.b16, v2.b16, v5.b16
            );
            if matches!(kind, CmpKind::LtU) {
                dynasm!(ops ; .arch aarch64 ; mvn v0.b16, v0.b16);
            }
        }
        CmpKind::GtU | CmpKind::LeU => {
            dynasm!(ops
                ; .arch aarch64
                ; uqsub v2.b16, v0.b16, v1.b16
                ; cmeq v0.b16, v2.b16, v5.b16
            );
            if matches!(kind, CmpKind::GtU) {
                dynasm!(ops ; .arch aarch64 ; mvn v0.b16, v0.b16);
            }
        }
        CmpKind::LtS | CmpKind::GeS => {
            dynasm!(ops ; .arch aarch64 ; cmgt v0.b16, v1.b16, v0.b16);
            if matches!(kind, CmpKind::GeS) {
                dynasm!(ops ; .arch aarch64 ; mvn v0.b16, v0.b16);
            }
        }
        CmpKind::GtS | CmpKind::LeS => {
            dynasm!(ops ; .arch aarch64 ; cmgt v0.b16, v0.b16, v1.b16);
            if matches!(kind, CmpKind::LeS) {
                dynasm!(ops ; .arch aarch64 ; mvn v0.b16, v0.b16);
            }
        }
    }
    emit_load_imm(ops, 30, 0x8040_2010_0804_0201);
    dynasm!(ops
        ; .arch aarch64
        ; ushr v0.b16, v0.b16, #7
        ; fmov d6, x30
        ; dup v6.d2, v6.d[0]
        ; mul v0.b16, v0.b16, v6.b16
        ; addv b5, v0.b8
        ; umov W(SCRATCH0), v5.b[0]
        ; ext v7.b16, v0.b16, v0.b16, #8
        ; addv b5, v7.b8
        ; umov W(SCRATCH1), v5.b[0]
        ; lsl x17, x17, #8
        ; orr x30, x16, x17
        ; mov X(destination), x30
    );
}
