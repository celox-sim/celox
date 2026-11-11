//! Addresses of state memory and the slots of call frames.

use super::*;

/// Where the addresses of a memory access start from.
#[derive(Clone, Copy)]
pub(super) enum Base {
    /// Offsets are absolute addresses into linear memory.
    Absolute,
    /// Offsets are relative to the address held by an `i32` local.
    I32(u32),
    /// Offsets are relative to the address held by an `i64` local.
    I64(u32),
}

/// Push the address `offset` bytes from `base`.
pub(super) fn emit_address(base: Base, offset: usize, instrs: &mut Vec<Instruction<'static>>) {
    match base {
        Base::Absolute => {
            instrs.push(Instruction::I32Const(offset as i32));
            return;
        }
        Base::I32(local) => instrs.push(Instruction::LocalGet(local)),
        Base::I64(local) => {
            instrs.push(Instruction::LocalGet(local));
            instrs.push(Instruction::I32WrapI64);
        }
    }
    if offset != 0 {
        instrs.push(Instruction::I32Const(offset as i32));
        instrs.push(Instruction::I32Add);
    }
}

/// The bit offset of a frame slot access when it is static; a slot holding
/// an unpacked array is also accessed at offsets computed at run time.
pub(super) fn static_bits(offset: &SIROffset) -> Option<usize> {
    match offset {
        SIROffset::Static(bits)
        | SIROffset::PackedElements {
            bit_offset: bits, ..
        } => Some(*bits),
        SIROffset::Dynamic(_) | SIROffset::Element { .. } => None,
    }
}

/// A local holding the absolute bit address of the access at `offset`, in
/// its array's logical packed layout, of `slot` of the frame based at the
/// address in `frame_base`.
pub(super) fn frame_bit_address(
    frame_base: u32,
    slot: &FrameSlot<RegionedAbsoluteAddr>,
    offset: &SIROffset,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) -> u32 {
    let logical = compile_logical_bit_offset(offset, locals, instrs);
    instrs.push(Instruction::LocalGet(frame_base));
    instrs.push(Instruction::I64ExtendI32U);
    instrs.push(Instruction::I64Const(slot.offset as i64));
    instrs.push(Instruction::I64Add);
    instrs.push(Instruction::I64Const(3));
    instrs.push(Instruction::I64Shl);
    instrs.push(Instruction::LocalGet(logical));
    instrs.push(Instruction::I64Add);
    instrs.push(Instruction::LocalSet(logical));
    logical
}

/// [`load_slot`] at an offset computed at run time.
#[allow(clippy::too_many_arguments)]
pub(super) fn load_slot_dynamic(
    dst: &RegLocal,
    frame_base: u32,
    slot: &FrameSlot<RegionedAbsoluteAddr>,
    offset: &SIROffset,
    op_width: usize,
    four_state: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let bits = frame_bit_address(frame_base, slot, offset, locals, instrs);
    let value = RegLocal {
        value_idx: dst.value_idx,
        num_chunks: dst.num_chunks,
        mask_idx: None,
    };
    load_frame_pieces(&value, 0, bits, op_width, locals, instrs);
    let Some(mask_idx) = dst.mask_idx.filter(|_| four_state) else {
        return;
    };
    let mask = RegLocal {
        value_idx: mask_idx,
        num_chunks: dst.num_chunks,
        mask_idx: None,
    };
    if slot.four_state {
        load_frame_pieces(&mask, slot.plane_size(), bits, op_width, locals, instrs);
    } else {
        for c in 0..dst.num_chunks {
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(mask_idx + c as u32));
        }
    }
}

/// [`store_slot`] at an offset computed at run time.
#[allow(clippy::too_many_arguments)]
pub(super) fn store_slot_dynamic(
    src: &RegLocal,
    frame_base: u32,
    slot: &FrameSlot<RegionedAbsoluteAddr>,
    offset: &SIROffset,
    op_width: usize,
    four_state: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    if op_width == 0 {
        return;
    }
    let bits = frame_bit_address(frame_base, slot, offset, locals, instrs);
    store_frame_pieces(src, 0, bits, op_width, locals, instrs);
    if !four_state || !slot.four_state {
        return;
    }
    let mask = match src.mask_idx {
        Some(mask_idx) => RegLocal {
            value_idx: mask_idx,
            num_chunks: src.num_chunks,
            mask_idx: None,
        },
        None => {
            // A two-state source gets a zero mask.
            let zero = locals.alloc(src.num_chunks);
            for c in 0..src.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(zero + c as u32));
            }
            RegLocal {
                value_idx: zero,
                num_chunks: src.num_chunks,
                mask_idx: None,
            }
        }
    };
    store_frame_pieces(&mask, slot.plane_size(), bits, op_width, locals, instrs);
}

/// The widest piece of a frame access at a bit address computed at run
/// time that one word of linear memory holds whatever the address's bit
/// shift (at most 7).
pub(super) const FRAME_PIECE_BITS: usize = 56;

/// A local holding `bits + delta`.
pub(super) fn bit_address_plus(
    bits: u32,
    delta: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) -> u32 {
    if delta == 0 {
        return bits;
    }
    let local = locals.alloc(1);
    instrs.push(Instruction::LocalGet(bits));
    instrs.push(Instruction::I64Const(delta as i64));
    instrs.push(Instruction::I64Add);
    instrs.push(Instruction::LocalSet(local));
    local
}

/// Load the `op_width` bits at bit address `bits` from `base_offset` into
/// the value plane of `dst`, a piece of at most [`FRAME_PIECE_BITS`] bits
/// at a time, so that every piece keeps the address's bit shift.
pub(super) fn load_frame_pieces(
    dst: &RegLocal,
    base_offset: usize,
    bits: u32,
    op_width: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    if op_width <= FRAME_PIECE_BITS {
        compile_load_dynamic(dst, base_offset, bits, op_width, instrs);
        return;
    }
    for c in 0..dst.num_chunks {
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::LocalSet(dst.value_idx + c as u32));
    }
    let piece = RegLocal {
        value_idx: locals.alloc(1),
        num_chunks: 1,
        mask_idx: None,
    };
    for start in (0..op_width).step_by(FRAME_PIECE_BITS) {
        let width = FRAME_PIECE_BITS.min(op_width - start);
        let at = bit_address_plus(bits, start, locals, instrs);
        compile_load_dynamic(&piece, base_offset, at, width, instrs);
        // Keep only the piece's bits, then place them at `start`.
        instrs.push(Instruction::LocalGet(piece.value_idx));
        instrs.push(Instruction::I64Const(((1u64 << width) - 1) as i64));
        instrs.push(Instruction::I64And);
        instrs.push(Instruction::LocalSet(piece.value_idx));
        let (chunk, shift) = (start / 64, start % 64);
        for (into, amount, right) in [(chunk, shift, false), (chunk + 1, 64 - shift, true)] {
            if into >= dst.num_chunks || (right && shift + width <= 64) {
                continue;
            }
            let into = dst.value_idx + into as u32;
            instrs.push(Instruction::LocalGet(into));
            instrs.push(Instruction::LocalGet(piece.value_idx));
            if amount != 0 {
                instrs.push(Instruction::I64Const(amount as i64));
                instrs.push(if right {
                    Instruction::I64ShrU
                } else {
                    Instruction::I64Shl
                });
            }
            instrs.push(Instruction::I64Or);
            instrs.push(Instruction::LocalSet(into));
        }
    }
}

/// Store the `op_width` bits of the value plane of `src` at bit address
/// `bits` from `base_offset`, a piece of at most [`FRAME_PIECE_BITS`] bits
/// at a time, so that every piece keeps the address's bit shift and no
/// bit outside the access changes.
pub(super) fn store_frame_pieces(
    src: &RegLocal,
    base_offset: usize,
    bits: u32,
    op_width: usize,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let value = RegLocal {
        value_idx: src.value_idx,
        num_chunks: src.num_chunks,
        mask_idx: None,
    };
    if op_width <= FRAME_PIECE_BITS {
        compile_store_dynamic(&value, base_offset, bits, op_width, locals, instrs);
        return;
    }
    let piece = RegLocal {
        value_idx: locals.alloc(1),
        num_chunks: 1,
        mask_idx: None,
    };
    for start in (0..op_width).step_by(FRAME_PIECE_BITS) {
        let width = FRAME_PIECE_BITS.min(op_width - start);
        let (chunk, shift) = (start / 64, start % 64);
        // The piece's bits, from one chunk or from two.
        instrs.push(Instruction::LocalGet(src.value_idx + chunk as u32));
        if shift != 0 {
            instrs.push(Instruction::I64Const(shift as i64));
            instrs.push(Instruction::I64ShrU);
        }
        if shift + width > 64 && chunk + 1 < src.num_chunks {
            instrs.push(Instruction::LocalGet(src.value_idx + chunk as u32 + 1));
            instrs.push(Instruction::I64Const((64 - shift) as i64));
            instrs.push(Instruction::I64Shl);
            instrs.push(Instruction::I64Or);
        }
        instrs.push(Instruction::LocalSet(piece.value_idx));
        let at = bit_address_plus(bits, start, locals, instrs);
        compile_store_dynamic(&piece, base_offset, at, width, locals, instrs);
    }
}

/// Load `op_width` bits `bits` into `slot`, of the frame at `base`, into
/// `dst`.
pub(super) fn load_slot(
    dst: &RegLocal,
    base: Base,
    slot: &FrameSlot<RegionedAbsoluteAddr>,
    bits: usize,
    op_width: usize,
    four_state: bool,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let (byte, shift) = (slot.offset + bits / 8, bits % 8);
    compile_load_at_offset_based(base, dst, byte, shift, op_width, instrs);
    let Some(mask_idx) = dst.mask_idx.filter(|_| four_state) else {
        return;
    };
    if slot.four_state {
        let mask = RegLocal {
            value_idx: mask_idx,
            num_chunks: dst.num_chunks,
            mask_idx: None,
        };
        compile_load_at_offset_based(
            base,
            &mask,
            byte + slot.plane_size(),
            shift,
            op_width,
            instrs,
        );
    } else {
        for c in 0..dst.num_chunks {
            instrs.push(Instruction::I64Const(0));
            instrs.push(Instruction::LocalSet(mask_idx + c as u32));
        }
    }
}

/// Store `op_width` bits of `src` `bits` into `slot`, of the frame at
/// `base`.
#[allow(clippy::too_many_arguments)]
pub(super) fn store_slot(
    src: &RegLocal,
    base: Base,
    slot: &FrameSlot<RegionedAbsoluteAddr>,
    bits: usize,
    op_width: usize,
    four_state: bool,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    if op_width == 0 {
        return;
    }
    let (byte, shift) = (slot.offset + bits / 8, bits % 8);
    compile_store_at_offset_based(base, src, byte, shift, op_width, locals, instrs);
    if !four_state || !slot.four_state {
        return;
    }
    let mask = match src.mask_idx {
        Some(mask_idx) => RegLocal {
            value_idx: mask_idx,
            num_chunks: src.num_chunks,
            mask_idx: None,
        },
        None => {
            // A two-state source gets a zero mask.
            let zero = locals.alloc(src.num_chunks);
            for c in 0..src.num_chunks {
                instrs.push(Instruction::I64Const(0));
                instrs.push(Instruction::LocalSet(zero + c as u32));
            }
            RegLocal {
                value_idx: zero,
                num_chunks: src.num_chunks,
                mask_idx: None,
            }
        }
    };
    compile_store_at_offset_based(
        base,
        &mask,
        byte + slot.plane_size(),
        shift,
        op_width,
        locals,
        instrs,
    );
}
