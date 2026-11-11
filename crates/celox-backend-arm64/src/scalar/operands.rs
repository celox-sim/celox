//! Memory operand addressing and scalar loads and stores.

use super::*;

pub(super) fn select_memory_base(
    base: BaseReg,
    offset: i64,
    _register: u8,
    size: OpSize,
    store: bool,
    state_pages: StatePageBases,
) -> (u8, i64) {
    if let Some(frame) = state_pages.frame_base(base, offset) {
        return frame;
    }
    let normal = (base_register(base), offset);
    if base != BaseReg::SimState {
        return normal;
    }
    std::iter::once(normal)
        .chain(
            state_pages
                .primary
                .into_iter()
                .map(|page| (STATE_PAGE_REG, offset - page)),
        )
        .chain(
            state_pages
                .secondary
                .iter()
                .flatten()
                .map(|&(register, page)| (register, offset - page)),
        )
        .min_by_key(|&(_, offset)| scalar_memory_offset_cost(offset, size, store))
        .unwrap_or(normal)
}

pub(super) fn select_indexed_memory_base(
    base: BaseReg,
    offset: i64,
    size: OpSize,
    store: bool,
    state_pages: StatePageBases,
) -> (u8, i64) {
    if let Some(frame) = state_pages.frame_base(base, offset) {
        return frame;
    }
    let normal = (base_register(base), offset);
    if base != BaseReg::SimState {
        return normal;
    }
    let candidates = std::iter::once((indexed_access_cost(offset, size, store), normal)).chain(
        state_pages
            .primary
            .into_iter()
            .map(|page| (STATE_PAGE_REG, page))
            .chain(state_pages.secondary.iter().flatten().copied())
            .filter_map(|(page_register, page)| {
                let relative = offset - page;
                indexed_offset_cost(relative, size, store)
                    .map(|cost| (cost, (page_register, relative)))
            }),
    );
    candidates
        .min_by_key(|(cost, _)| *cost)
        .map(|(_, base)| base)
        .unwrap_or(normal)
}

pub(super) fn select_vector_memory_base(
    base: BaseReg,
    offset: i64,
    state_pages: StatePageBases,
) -> (u8, i64) {
    if let Some(frame) = state_pages.frame_base(base, offset) {
        return frame;
    }
    let normal = (base_register(base), offset);
    if base != BaseReg::SimState {
        return normal;
    }
    let candidates = std::iter::once((address_materialization_cost(offset), normal)).chain(
        state_pages
            .primary
            .into_iter()
            .map(|page| (STATE_PAGE_REG, page))
            .chain(state_pages.secondary.iter().flatten().copied())
            .map(|(page_register, page)| {
                let relative = offset - page;
                (
                    address_materialization_cost(relative),
                    (page_register, relative),
                )
            }),
    );
    candidates
        .min_by_key(|(cost, _)| *cost)
        .map(|(_, base)| base)
        .unwrap_or(normal)
}

pub(super) fn address_materialization_cost(offset: i64) -> usize {
    if offset == 0 {
        0
    } else if add_sub_immediate(offset).is_some() {
        1
    } else if add_sub_immediate_pair(offset).is_some() {
        2
    } else {
        move_wide_plan(offset as u64).instruction_count + 1
    }
}

pub(super) fn base_register(base: BaseReg) -> u8 {
    match base {
        BaseReg::SimState => STATE_REG,
        BaseReg::StackFrame => SPILL_REG,
    }
}

pub(super) fn base_offset(base: BaseReg, offset: i32) -> i64 {
    match base {
        BaseReg::SimState | BaseReg::StackFrame => i64::from(offset),
    }
}

pub(super) fn emit_address(ops: &mut VecAssembler<Aarch64Relocation>, base: u8, offset: i64) {
    emit_address_to(ops, SCRATCH0, base, offset);
}

pub(super) fn emit_address_to(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    base: u8,
    offset: i64,
) {
    if offset == 0 {
        dynasm!(ops ; .arch aarch64 ; mov X(destination), X(base));
    } else if !emit_add_sub_immediate(ops, destination, base, offset) {
        if let Some((high, low)) = add_sub_immediate_pair(offset) {
            let _ = emit_add_sub_immediate(ops, destination, base, high);
            let _ = emit_add_sub_immediate(ops, destination, destination, low);
        } else {
            emit_load_imm(ops, SCRATCH1, offset as u64);
            dynasm!(ops ; .arch aarch64 ; add X(destination), X(base), x17);
        }
    }
}

pub(super) fn memory_access_encoding(
    register: u8,
    base: u8,
    offset: i64,
    size: OpSize,
    store: bool,
) -> Option<u32> {
    let (bytes, scaled_opcode, unscaled_opcode) = match (size, store) {
        (OpSize::S8, false) => (1, 0x3940_0000, 0x3840_0000),
        (OpSize::S8, true) => (1, 0x3900_0000, 0x3800_0000),
        (OpSize::S16, false) => (2, 0x7940_0000, 0x7840_0000),
        (OpSize::S16, true) => (2, 0x7900_0000, 0x7800_0000),
        (OpSize::S32, false) => (4, 0xb940_0000, 0xb840_0000),
        (OpSize::S32, true) => (4, 0xb900_0000, 0xb800_0000),
        (OpSize::S64, false) => (8, 0xf940_0000, 0xf840_0000),
        (OpSize::S64, true) => (8, 0xf900_0000, 0xf800_0000),
    };
    let register = u32::from(register);
    let base = u32::from(base);
    let registers = (base << 5) | register;

    if offset >= 0 && offset % bytes == 0 {
        let scaled = offset / bytes;
        if scaled <= 0xfff {
            return Some(scaled_opcode | ((scaled as u32) << 10) | registers);
        }
    }
    if (-256..=255).contains(&offset) {
        let immediate = ((offset as i32) & 0x1ff) as u32;
        return Some(unscaled_opcode | (immediate << 12) | registers);
    }
    None
}

/// Use the load/store immediate for the low page offset, leaving only one
/// ADD/SUB of the high part. A cached state page can then serve nearby pages
/// without rebuilding a full state address from its absolute displacement.
pub(super) fn split_memory_offset(offset: i64, size: OpSize, store: bool) -> Option<(i64, i64)> {
    let low = offset.rem_euclid(STATE_PAGE_BYTES);
    let high = offset.checked_sub(low)?;
    if high == 0 {
        return None;
    }
    add_sub_immediate(high)?;
    memory_access_encoding(SCRATCH0, SCRATCH0, low, size, store)?;
    Some((high, low))
}

pub(super) fn scalar_memory_offset_cost(offset: i64, size: OpSize, store: bool) -> usize {
    if memory_access_encoding(SCRATCH0, SCRATCH0, offset, size, store).is_some() {
        0
    } else if split_memory_offset(offset, size, store).is_some() {
        1
    } else {
        address_materialization_cost(offset)
    }
}

pub(super) fn emit_load_at(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    base: u8,
    offset: i64,
    size: OpSize,
) {
    if let Some(instruction) = memory_access_encoding(destination, base, offset, size, false) {
        ops.push_u32(instruction);
    } else if let Some((high, low)) = split_memory_offset(offset, size, false) {
        let _ = emit_add_sub_immediate(ops, SCRATCH0, base, high);
        ops.push_u32(memory_access_encoding(destination, SCRATCH0, low, size, false).unwrap());
    } else {
        emit_address(ops, base, offset);
        emit_load(ops, destination, SCRATCH0, size);
    }
}

pub(super) fn emit_load_indexed_at(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    base: u8,
    index: u8,
    offset: i64,
    scale: u8,
    size: OpSize,
) {
    if offset == 0
        && let Some(shift) = indexed_address_shift(scale, size)
    {
        match size {
            OpSize::S8 => {
                dynasm!(ops ; .arch aarch64 ; ldrb W(destination), [X(base), X(index), LSL #shift])
            }
            OpSize::S16 => {
                dynasm!(ops ; .arch aarch64 ; ldrh W(destination), [X(base), X(index), LSL #shift])
            }
            OpSize::S32 => {
                dynasm!(ops ; .arch aarch64 ; ldr W(destination), [X(base), X(index), LSL #shift])
            }
            OpSize::S64 => {
                dynasm!(ops ; .arch aarch64 ; ldr X(destination), [X(base), X(index), LSL #shift])
            }
        }
    } else {
        let shift = scale.trailing_zeros();
        dynasm!(ops ; .arch aarch64 ; add x16, X(base), X(index), LSL #shift);
        emit_load_at(ops, destination, SCRATCH0, offset, size);
    }
}

pub(super) fn emit_store_at(
    ops: &mut VecAssembler<Aarch64Relocation>,
    source: u8,
    base: u8,
    offset: i64,
    size: OpSize,
) {
    if let Some(instruction) = memory_access_encoding(source, base, offset, size, true) {
        ops.push_u32(instruction);
    } else if let Some((high, low)) = split_memory_offset(offset, size, true) {
        let _ = emit_add_sub_immediate(ops, SCRATCH0, base, high);
        ops.push_u32(memory_access_encoding(source, SCRATCH0, low, size, true).unwrap());
    } else {
        emit_address(ops, base, offset);
        emit_store(ops, source, SCRATCH0, size);
    }
}

pub(super) fn emit_store_indexed_at(
    ops: &mut VecAssembler<Aarch64Relocation>,
    source: u8,
    base: u8,
    index: u8,
    offset: i64,
    scale: u8,
    size: OpSize,
) {
    if offset == 0
        && let Some(shift) = indexed_address_shift(scale, size)
    {
        match size {
            OpSize::S8 => {
                dynasm!(ops ; .arch aarch64 ; strb W(source), [X(base), X(index), LSL #shift])
            }
            OpSize::S16 => {
                dynasm!(ops ; .arch aarch64 ; strh W(source), [X(base), X(index), LSL #shift])
            }
            OpSize::S32 => {
                dynasm!(ops ; .arch aarch64 ; str W(source), [X(base), X(index), LSL #shift])
            }
            OpSize::S64 => {
                dynasm!(ops ; .arch aarch64 ; str X(source), [X(base), X(index), LSL #shift])
            }
        }
    } else {
        let shift = scale.trailing_zeros();
        dynasm!(ops ; .arch aarch64 ; add x16, X(base), X(index), LSL #shift);
        emit_store_at(ops, source, SCRATCH0, offset, size);
    }
}

pub(super) fn indexed_address_shift(scale: u8, size: OpSize) -> Option<u32> {
    let natural_scale = match size {
        OpSize::S8 => 1,
        OpSize::S16 => 2,
        OpSize::S32 => 4,
        OpSize::S64 => 8,
    };
    if scale == 1 {
        Some(0)
    } else if scale == natural_scale {
        Some(scale.trailing_zeros())
    } else {
        None
    }
}

pub(super) fn emit_add_offset(ops: &mut VecAssembler<Aarch64Relocation>, offset: i64) {
    if offset != 0 {
        if !emit_add_sub_immediate(ops, SCRATCH0, SCRATCH0, offset) {
            if let Some((high, low)) = add_sub_immediate_pair(offset) {
                let _ = emit_add_sub_immediate(ops, SCRATCH0, SCRATCH0, high);
                let _ = emit_add_sub_immediate(ops, SCRATCH0, SCRATCH0, low);
            } else {
                emit_load_imm(ops, SCRATCH1, offset as u64);
                dynasm!(ops ; .arch aarch64 ; add x16, x16, x17);
            }
        }
    }
}

pub(super) fn emit_load(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    address: u8,
    size: OpSize,
) {
    match size {
        OpSize::S8 => dynasm!(ops ; .arch aarch64 ; ldrb W(destination), [X(address)]),
        OpSize::S16 => dynasm!(ops ; .arch aarch64 ; ldrh W(destination), [X(address)]),
        OpSize::S32 => dynasm!(ops ; .arch aarch64 ; ldr W(destination), [X(address)]),
        OpSize::S64 => dynasm!(ops ; .arch aarch64 ; ldr X(destination), [X(address)]),
    }
}

pub(super) fn emit_store(
    ops: &mut VecAssembler<Aarch64Relocation>,
    source: u8,
    address: u8,
    size: OpSize,
) {
    match size {
        OpSize::S8 => dynasm!(ops ; .arch aarch64 ; strb W(source), [X(address)]),
        OpSize::S16 => dynasm!(ops ; .arch aarch64 ; strh W(source), [X(address)]),
        OpSize::S32 => dynasm!(ops ; .arch aarch64 ; str W(source), [X(address)]),
        OpSize::S64 => dynasm!(ops ; .arch aarch64 ; str X(source), [X(address)]),
    }
}

pub(super) fn emit_release_store(
    ops: &mut VecAssembler<Aarch64Relocation>,
    source: u8,
    address: u8,
    size: OpSize,
) {
    match size {
        OpSize::S8 => dynasm!(ops ; .arch aarch64 ; stlrb W(source), [X(address)]),
        OpSize::S16 => dynasm!(ops ; .arch aarch64 ; stlrh W(source), [X(address)]),
        OpSize::S32 => dynasm!(ops ; .arch aarch64 ; stlr W(source), [X(address)]),
        OpSize::S64 => dynasm!(ops ; .arch aarch64 ; stlr X(source), [X(address)]),
    }
}
