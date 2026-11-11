//! Immediate encodings, constant materialization, and conditional sets and selects.

use super::*;

pub(super) fn add_sub_immediate(offset: i64) -> Option<(bool, u32, bool)> {
    let (subtract, magnitude) = if offset < 0 {
        (true, offset.unsigned_abs())
    } else {
        (false, offset as u64)
    };
    if magnitude <= 0xfff {
        return Some((subtract, magnitude as u32, false));
    }
    if magnitude.is_multiple_of(0x1000) && magnitude / 0x1000 <= 0xfff {
        return Some((subtract, (magnitude / 0x1000) as u32, true));
    }
    None
}

pub(super) fn add_sub_immediate_pair(offset: i64) -> Option<(i64, i64)> {
    let magnitude = offset.unsigned_abs();
    let high = magnitude & !0xfff;
    let low = magnitude & 0xfff;
    if high == 0 || low == 0 {
        return None;
    }
    let sign = if offset < 0 { -1_i64 } else { 1_i64 };
    let high = i64::try_from(high).ok()?.checked_mul(sign)?;
    let low = i64::try_from(low).ok()?.checked_mul(sign)?;
    (add_sub_immediate(high).is_some() && add_sub_immediate(low).is_some()).then_some((high, low))
}

pub(super) fn add_sub_immediate_encoding(
    destination: u8,
    base: u8,
    offset: i64,
    set_flags: bool,
) -> Option<u32> {
    let (subtract, immediate, shifted) = add_sub_immediate(offset)?;
    // ADD/SUB (immediate): sf=1, op selects SUB, S selects flag-setting,
    // and sh selects an optional <<12 immediate.  Using the same encoder for
    // arithmetic and CMP keeps the immediate selection identical.
    let opcode = if set_flags { 0xb100_0000 } else { 0x9100_0000 };
    let instruction = opcode
        | (u32::from(subtract) << 30)
        | (u32::from(shifted) << 22)
        | (immediate << 10)
        | (u32::from(base) << 5)
        | u32::from(destination);
    debug_assert!(immediate <= 0xfff);
    Some(instruction)
}

pub(super) fn emit_add_sub_immediate(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    base: u8,
    offset: i64,
) -> bool {
    let Some(instruction) = add_sub_immediate_encoding(destination, base, offset, false) else {
        return false;
    };
    ops.push_u32(instruction);
    true
}

pub(super) fn emit_cmp_immediate(
    ops: &mut VecAssembler<Aarch64Relocation>,
    lhs: u8,
    immediate: i32,
) -> bool {
    // CMP lhs, #imm is SUBS lhs, #imm.  A negative immediate is therefore
    // represented by ADDS with its positive magnitude.  The immediate is
    // sign-extended to 64 bits by the MIR contract.
    let Some(instruction) = add_sub_immediate_encoding(31, lhs, -i64::from(immediate), true) else {
        return false;
    };
    ops.push_u32(instruction);
    true
}

pub(super) fn logical_immediate_encoding(
    value: u64,
    width: u32,
    destination: u8,
    source: u8,
    is_and: bool,
) -> Option<u32> {
    debug_assert!(matches!(width, 32 | 64));
    let width_mask = if width == 32 {
        u64::from(u32::MAX)
    } else {
        u64::MAX
    };
    let value = value & width_mask;
    let opcode = match (width, is_and) {
        (32, true) => 0x1200_0000,
        (32, false) => 0x3200_0000,
        (64, true) => 0x9200_0000,
        (64, false) => 0xb200_0000,
        _ => unreachable!(),
    };

    // AArch64 represents a logical immediate as a rotated run of ones in a
    // power-of-two element, replicated to the operand width.  Try the
    // smallest element first; this also selects N=0 for repeated masks and
    // N=1 only for a full 64-bit element.
    for element_size in [2_u32, 4, 8, 16, 32, 64] {
        if element_size > width {
            continue;
        }
        let element_mask = if element_size == 64 {
            u64::MAX
        } else {
            (1_u64 << element_size) - 1
        };
        let pattern = value & element_mask;
        if pattern == 0 || pattern == element_mask {
            continue;
        }
        let mut repeated = 0_u64;
        let mut offset = 0_u32;
        while offset < width {
            repeated |= pattern << offset;
            offset += element_size;
        }
        if repeated != value {
            continue;
        }

        let ones = pattern.count_ones();
        let length = element_size.trailing_zeros();
        let imms_prefix = (!((1_u32 << (length + 1)) - 1)) & 0x3f;
        let imms = imms_prefix | (ones - 1);
        let run = (1_u64 << ones) - 1;
        for rotation in 0..element_size {
            if rotate_right(run, rotation, element_size) != pattern {
                continue;
            }
            let n = u32::from(width == 64 && element_size == 64);
            return Some(
                opcode
                    | (n << 22)
                    | (rotation << 16)
                    | (imms << 10)
                    | (u32::from(source) << 5)
                    | u32::from(destination),
            );
        }
    }
    None
}

pub(super) fn rotate_right(value: u64, amount: u32, width: u32) -> u64 {
    let amount = amount % width;
    let mask = if width == 64 {
        u64::MAX
    } else {
        (1_u64 << width) - 1
    };
    if amount == 0 {
        value & mask
    } else {
        ((value >> amount) | (value << (width - amount))) & mask
    }
}

pub(super) fn emit_logical_immediate(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    source: u8,
    value: u64,
    width: u32,
    is_and: bool,
) -> bool {
    let Some(instruction) = logical_immediate_encoding(value, width, destination, source, is_and)
    else {
        return false;
    };
    ops.push_u32(instruction);
    true
}

pub(crate) fn is_logical_immediate(value: u64, width: u32) -> bool {
    logical_immediate_encoding(value, width, 1, 2, true).is_some()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct MoveWidePlan {
    pub(super) inverted: bool,
    pub(super) first: usize,
    pub(super) instruction_count: usize,
}

pub(super) fn move_wide_plan(value: u64) -> MoveWidePlan {
    let halves = [
        value as u16,
        (value >> 16) as u16,
        (value >> 32) as u16,
        (value >> 48) as u16,
    ];
    let movz_candidates = halves
        .iter()
        .enumerate()
        .filter_map(|(index, &half)| (half != 0).then_some(index))
        .collect::<Vec<_>>();
    let movn_candidates = halves
        .iter()
        .enumerate()
        .filter_map(|(index, &half)| (half != u16::MAX).then_some(index))
        .collect::<Vec<_>>();
    let movz = MoveWidePlan {
        inverted: false,
        first: movz_candidates.first().copied().unwrap_or(0),
        instruction_count: movz_candidates.len().max(1),
    };
    let movn = MoveWidePlan {
        inverted: true,
        first: movn_candidates.first().copied().unwrap_or(0),
        instruction_count: movn_candidates.len().max(1),
    };
    if movn.instruction_count < movz.instruction_count {
        movn
    } else {
        movz
    }
}

pub(crate) fn is_single_instruction_constant(value: u64) -> bool {
    move_wide_plan(value).instruction_count == 1
        || logical_immediate_encoding(value, 64, 1, 31, false).is_some()
}

pub(super) fn emit_load_imm(ops: &mut VecAssembler<Aarch64Relocation>, register: u8, value: u64) {
    let plan = move_wide_plan(value);
    if plan.instruction_count > 1
        && let Some(instruction) = logical_immediate_encoding(value, 64, register, 31, false)
    {
        ops.push_u32(instruction);
        return;
    }
    let halves = [
        value as u16,
        (value >> 16) as u16,
        (value >> 32) as u16,
        (value >> 48) as u16,
    ];
    let first = plan.first;
    let fill = if plan.inverted { u16::MAX } else { 0 };
    let half = if plan.inverted {
        u32::from(!halves[first])
    } else {
        u32::from(halves[first])
    };
    match (plan.inverted, first) {
        (false, 0) => dynasm!(ops ; .arch aarch64 ; movz X(register), half),
        (false, 1) => dynasm!(ops ; .arch aarch64 ; movz X(register), half, LSL #16),
        (false, 2) => dynasm!(ops ; .arch aarch64 ; movz X(register), half, LSL #32),
        (false, 3) => dynasm!(ops ; .arch aarch64 ; movz X(register), half, LSL #48),
        (true, 0) => dynasm!(ops ; .arch aarch64 ; movn X(register), half),
        (true, 1) => dynasm!(ops ; .arch aarch64 ; movn X(register), half, LSL #16),
        (true, 2) => dynasm!(ops ; .arch aarch64 ; movn X(register), half, LSL #32),
        (true, 3) => dynasm!(ops ; .arch aarch64 ; movn X(register), half, LSL #48),
        _ => unreachable!(),
    }
    for (index, half) in halves.into_iter().enumerate() {
        if index == first || half == fill {
            continue;
        }
        let half = u32::from(half);
        match index {
            0 => dynasm!(ops ; .arch aarch64 ; movk X(register), half),
            1 => dynasm!(ops ; .arch aarch64 ; movk X(register), half, LSL #16),
            2 => dynasm!(ops ; .arch aarch64 ; movk X(register), half, LSL #32),
            3 => dynasm!(ops ; .arch aarch64 ; movk X(register), half, LSL #48),
            _ => unreachable!(),
        }
    }
}

pub(super) fn emit_cset(ops: &mut VecAssembler<Aarch64Relocation>, destination: u8, kind: CmpKind) {
    match kind {
        CmpKind::Eq => dynasm!(ops ; .arch aarch64 ; cset X(destination), eq),
        CmpKind::Ne => dynasm!(ops ; .arch aarch64 ; cset X(destination), ne),
        CmpKind::LtU => dynasm!(ops ; .arch aarch64 ; cset X(destination), lo),
        CmpKind::LtS => dynasm!(ops ; .arch aarch64 ; cset X(destination), lt),
        CmpKind::LeU => dynasm!(ops ; .arch aarch64 ; cset X(destination), ls),
        CmpKind::LeS => dynasm!(ops ; .arch aarch64 ; cset X(destination), le),
        CmpKind::GtU => dynasm!(ops ; .arch aarch64 ; cset X(destination), hi),
        CmpKind::GtS => dynasm!(ops ; .arch aarch64 ; cset X(destination), gt),
        CmpKind::GeU => dynasm!(ops ; .arch aarch64 ; cset X(destination), hs),
        CmpKind::GeS => dynasm!(ops ; .arch aarch64 ; cset X(destination), ge),
    }
}

pub(super) fn emit_csel(
    ops: &mut VecAssembler<Aarch64Relocation>,
    destination: u8,
    true_value: u8,
    false_value: u8,
    kind: CmpKind,
) {
    match kind {
        CmpKind::Eq => {
            dynasm!(ops ; .arch aarch64 ; csel X(destination), X(true_value), X(false_value), eq)
        }
        CmpKind::Ne => {
            dynasm!(ops ; .arch aarch64 ; csel X(destination), X(true_value), X(false_value), ne)
        }
        CmpKind::LtU => {
            dynasm!(ops ; .arch aarch64 ; csel X(destination), X(true_value), X(false_value), lo)
        }
        CmpKind::LtS => {
            dynasm!(ops ; .arch aarch64 ; csel X(destination), X(true_value), X(false_value), lt)
        }
        CmpKind::LeU => {
            dynasm!(ops ; .arch aarch64 ; csel X(destination), X(true_value), X(false_value), ls)
        }
        CmpKind::LeS => {
            dynasm!(ops ; .arch aarch64 ; csel X(destination), X(true_value), X(false_value), le)
        }
        CmpKind::GtU => {
            dynasm!(ops ; .arch aarch64 ; csel X(destination), X(true_value), X(false_value), hi)
        }
        CmpKind::GtS => {
            dynasm!(ops ; .arch aarch64 ; csel X(destination), X(true_value), X(false_value), gt)
        }
        CmpKind::GeU => {
            dynasm!(ops ; .arch aarch64 ; csel X(destination), X(true_value), X(false_value), hs)
        }
        CmpKind::GeS => {
            dynasm!(ops ; .arch aarch64 ; csel X(destination), X(true_value), X(false_value), ge)
        }
    }
}
