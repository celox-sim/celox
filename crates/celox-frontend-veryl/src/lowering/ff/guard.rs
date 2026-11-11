//! Bounds-guarded access: offset arithmetic, index guards, guarded loads and stores.

use super::*;

pub(super) fn add_offset_term<A>(
    accumulator: &mut Option<RegisterId>,
    term: RegisterId,
    builder: &mut SIRBuilder<A>,
) {
    if let Some(current) = *accumulator {
        let next = builder.alloc_bit(64, false);
        builder.emit(SIRInstruction::Binary(next, current, BinaryOp::Add, term));
        *accumulator = Some(next);
    } else {
        *accumulator = Some(term);
    }
}

pub(super) fn add_offset_constant<A>(
    accumulator: &mut Option<RegisterId>,
    value: u64,
    builder: &mut SIRBuilder<A>,
) {
    if value == 0 {
        return;
    }
    let constant = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Imm(constant, SIRValue::new(value)));
    add_offset_term(accumulator, constant, builder);
}

pub(super) fn scale_offset<A>(
    value: RegisterId,
    scale: usize,
    builder: &mut SIRBuilder<A>,
) -> RegisterId {
    if scale == 1 {
        return value;
    }
    let scale_reg = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Imm(scale_reg, SIRValue::new(scale as u64)));
    let result = builder.alloc_bit(64, false);
    builder.emit(SIRInstruction::Binary(
        result,
        value,
        BinaryOp::Mul,
        scale_reg,
    ));
    result
}

/// The runtime conditions under which a dynamic select addresses only bits
/// of the selected variable.
#[derive(Default)]
pub(super) struct AccessGuard {
    /// Every runtime index, except a part-select anchor, is in range.
    pub(super) indices: Option<RegisterId>,
    /// A runtime part-select anchor, which may be partly in range.
    pub(super) part: Option<PartGuard>,
    /// The raw bits of some index other than a part-select anchor can
    /// address memory outside its dimension.
    pub(super) clamp: bool,
}

/// A part select whose runtime `lsb` selects `elements` consecutive
/// `element_bits`-wide elements of a `dimension_width`-element dimension.
pub(super) struct PartGuard {
    pub(super) lsb: RegisterId,
    pub(super) signed: bool,
    pub(super) elements: usize,
    pub(super) dimension_width: usize,
    pub(super) element_bits: usize,
}

impl AccessGuard {
    /// The condition that the whole access is in range, or `None` when no
    /// runtime index can be out of range.
    pub(super) fn whole<A>(&self, builder: &mut SIRBuilder<A>) -> Option<RegisterId> {
        let mut condition = self.indices;
        if let Some(part) = &self.part {
            and_index_guard(
                &mut condition,
                part.lsb,
                part.signed,
                Some(part.dimension_width + 1 - part.elements),
                builder,
            );
        }
        condition
    }

    /// The condition that element `element` of the part select is in range.
    fn part_element<A>(
        &self,
        part: &PartGuard,
        element: usize,
        builder: &mut SIRBuilder<A>,
    ) -> RegisterId {
        let mut condition = self.indices;
        // Rejects an unknown or negative anchor.
        and_index_guard(&mut condition, part.lsb, part.signed, None, builder);
        let position = builder.alloc_bit(64, false);
        let element_reg = builder.alloc_bit(64, false);
        builder.emit(SIRInstruction::Imm(
            element_reg,
            SIRValue::new(element as u64),
        ));
        builder.emit(SIRInstruction::Binary(
            position,
            part.lsb,
            BinaryOp::Add,
            element_reg,
        ));
        let bound = builder.alloc_bit(64, false);
        builder.emit(SIRInstruction::Imm(
            bound,
            SIRValue::new(part.dimension_width as u64),
        ));
        let in_range = builder.alloc_bit(1, false);
        builder.emit(SIRInstruction::Binary(
            in_range,
            position,
            BinaryOp::LtU,
            bound,
        ));
        and_condition(&mut condition, in_range, builder);
        condition.expect("a part element is always guarded")
    }
}

/// `offset` advanced by a constant number of logical bits.
fn offset_plus_bits<A>(offset: &SIROffset, bits: usize, builder: &mut SIRBuilder<A>) -> SIROffset {
    match offset {
        SIROffset::Static(lsb) => SIROffset::Static(lsb + bits),
        SIROffset::PackedElements {
            bit_offset,
            element_width,
        } => SIROffset::PackedElements {
            bit_offset: bit_offset + bits,
            element_width: *element_width,
        },
        SIROffset::Dynamic(register) => {
            let mut sum = Some(*register);
            add_offset_constant(&mut sum, bits as u64, builder);
            SIROffset::Dynamic(sum.expect("offset register is present"))
        }
        SIROffset::Element {
            index,
            element_width,
            bit_offset,
            dynamic_bit_offset,
        } => SIROffset::Element {
            index: *index,
            element_width: *element_width,
            bit_offset: bit_offset + bits,
            dynamic_bit_offset: *dynamic_bit_offset,
        },
    }
}

/// `offset` with every runtime component replaced by 0 unless `condition`
/// holds. The static components of a validated select address the variable.
fn clamp_offset<A>(
    builder: &mut SIRBuilder<A>,
    offset: &SIROffset,
    condition: RegisterId,
) -> SIROffset {
    let clamp = |builder: &mut SIRBuilder<A>, register: RegisterId| {
        let width = builder.register(&register).width();
        let zero = alloc_like(builder, register, width);
        builder.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
        let clamped = alloc_like(builder, register, width);
        builder.emit(SIRInstruction::Mux(clamped, condition, register, zero));
        clamped
    };
    match offset {
        SIROffset::Static(_) | SIROffset::PackedElements { .. } => offset.clone(),
        SIROffset::Dynamic(register) => SIROffset::Dynamic(clamp(builder, *register)),
        SIROffset::Element {
            index,
            element_width,
            bit_offset,
            dynamic_bit_offset,
        } => SIROffset::Element {
            index: clamp(builder, *index),
            element_width: *element_width,
            bit_offset: *bit_offset,
            dynamic_bit_offset: dynamic_bit_offset.map(|register| clamp(builder, register)),
        },
    }
}

pub(super) fn alloc_like<A>(
    builder: &mut SIRBuilder<A>,
    register: RegisterId,
    width: usize,
) -> RegisterId {
    match builder.register(&register).clone() {
        RegisterType::Bit { signed, .. } => builder.alloc_bit(width, signed),
        RegisterType::Logic { .. } => builder.alloc_logic(width),
    }
}

/// Run `body` only when `condition` holds.
fn emit_when<A>(
    builder: &mut SIRBuilder<A>,
    condition: RegisterId,
    body: impl FnOnce(&mut SIRBuilder<A>),
) {
    let then_block = builder.new_block();
    let merge_block = builder.new_block();
    builder.seal_block(SIRTerminator::Branch {
        cond: condition,
        true_block: (then_block, vec![]),
        false_block: (merge_block, vec![]),
    });
    builder.switch_to_block(then_block);
    body(builder);
    builder.seal_block(SIRTerminator::Jump(merge_block, vec![]));
    builder.switch_to_block(merge_block);
}

/// The value of an invalid read into a register like `like`: X for a
/// four-state register, 0 for a two-state one. A two-state simulation reads X
/// as 0 (`celox_sir::two_state`).
pub(super) fn unknown_like<A>(
    builder: &mut SIRBuilder<A>,
    like: RegisterId,
    width: usize,
) -> RegisterId {
    let register = alloc_like(builder, like, width);
    let value = if matches!(builder.register(&register), RegisterType::Logic { .. }) {
        // X sets both the value and the mask bit.
        let mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
        SIRValue::new_four_state(mask.clone(), mask)
    } else {
        SIRValue::new(0u8)
    };
    builder.emit(SIRInstruction::Imm(register, value));
    register
}

/// `Load` into a fresh register like `like`, or `default` when `condition`
/// is false. The memory is not accessed when `condition` is false.
fn emit_load_when<A>(
    builder: &mut SIRBuilder<A>,
    condition: RegisterId,
    like: RegisterId,
    address: &impl Fn() -> A,
    offset: SIROffset,
    width: usize,
    default: RegisterId,
) -> RegisterId {
    let load_block = builder.new_block();
    let result = alloc_like(builder, like, width);
    let merge_block = builder.new_block_with(vec![result]);
    builder.seal_block(SIRTerminator::Branch {
        cond: condition,
        true_block: (load_block, vec![]),
        false_block: (merge_block, vec![default]),
    });
    builder.switch_to_block(load_block);
    let loaded = alloc_like(builder, like, width);
    builder.emit(SIRInstruction::Load(loaded, address(), offset, width));
    builder.seal_block(SIRTerminator::Jump(merge_block, vec![loaded]));
    builder.switch_to_block(merge_block);
    result
}

/// Emit a store through a guarded dynamic select. A write through an
/// out-of-range index is ignored, and only the in-range bits of a partly
/// out-of-range part select are written (IEEE 1800-2023 7.4.6, 11.5.1).
pub(super) fn emit_guarded_store<A>(
    builder: &mut SIRBuilder<A>,
    guard: &AccessGuard,
    address: impl Fn() -> A,
    offset: SIROffset,
    width: usize,
    value: RegisterId,
) {
    let store = |builder: &mut SIRBuilder<A>, offset, width, value| {
        builder.emit(SIRInstruction::Store(
            address(),
            offset,
            width,
            value,
            Vec::new(),
            Vec::new(),
        ));
    };
    let Some(whole) = guard.whole(builder) else {
        store(builder, offset, width, value);
        return;
    };
    let Some(part) = &guard.part else {
        emit_when(builder, whole, |builder| {
            store(builder, offset, width, value)
        });
        return;
    };
    let store_block = builder.new_block();
    let partial_block = builder.new_block();
    let merge_block = builder.new_block();
    builder.seal_block(SIRTerminator::Branch {
        cond: whole,
        true_block: (store_block, vec![]),
        false_block: (partial_block, vec![]),
    });
    builder.switch_to_block(store_block);
    store(builder, offset.clone(), width, value);
    builder.seal_block(SIRTerminator::Jump(merge_block, vec![]));

    builder.switch_to_block(partial_block);
    for element in 0..part.elements {
        let condition = guard.part_element(part, element, builder);
        emit_when(builder, condition, |builder| {
            let bits = element * part.element_bits;
            let slice = alloc_like(builder, value, part.element_bits);
            builder.emit(SIRInstruction::Slice(slice, value, bits, part.element_bits));
            let offset = offset_plus_bits(&offset, bits, builder);
            store(builder, offset, part.element_bits, slice);
        });
    }
    builder.seal_block(SIRTerminator::Jump(merge_block, vec![]));
    builder.switch_to_block(merge_block);
}

/// Emit a load through a guarded dynamic select into `dest`'s type. An
/// out-of-range index reads X from a four-state register and 0 from a
/// two-state one, per bit for a partly out-of-range part select
/// (IEEE 1800-2023 7.4.6, 11.5.1). The memory is never read out of range.
pub(super) fn emit_guarded_load<A>(
    builder: &mut SIRBuilder<A>,
    guard: &AccessGuard,
    dest: RegisterId,
    address: impl Fn() -> A,
    offset: SIROffset,
    width: usize,
) -> RegisterId {
    let Some(whole) = guard.whole(builder) else {
        builder.emit(SIRInstruction::Load(dest, address(), offset, width));
        return dest;
    };
    let unknown = |builder: &mut SIRBuilder<A>, width: usize| unknown_like(builder, dest, width);
    let Some(part) = &guard.part else {
        // Branch-free: load through an address that is redirected to the
        // first element when an index can leave the variable, then select X.
        let offset = if guard.clamp {
            clamp_offset(builder, &offset, whole)
        } else {
            offset
        };
        let loaded = alloc_like(builder, dest, width);
        builder.emit(SIRInstruction::Load(loaded, address(), offset, width));
        let default = unknown(builder, width);
        let result = alloc_like(builder, dest, width);
        builder.emit(SIRInstruction::Mux(result, whole, loaded, default));
        return result;
    };
    let load_block = builder.new_block();
    let partial_block = builder.new_block();
    let result = alloc_like(builder, dest, width);
    let merge_block = builder.new_block_with(vec![result]);
    builder.seal_block(SIRTerminator::Branch {
        cond: whole,
        true_block: (load_block, vec![]),
        false_block: (partial_block, vec![]),
    });
    builder.switch_to_block(load_block);
    let loaded = alloc_like(builder, dest, width);
    builder.emit(SIRInstruction::Load(
        loaded,
        address(),
        offset.clone(),
        width,
    ));
    builder.seal_block(SIRTerminator::Jump(merge_block, vec![loaded]));

    builder.switch_to_block(partial_block);
    let mut elements = Vec::with_capacity(part.elements);
    for element in 0..part.elements {
        let condition = guard.part_element(part, element, builder);
        let default = unknown(builder, part.element_bits);
        let offset = offset_plus_bits(&offset, element * part.element_bits, builder);
        elements.push(emit_load_when(
            builder,
            condition,
            dest,
            &address,
            offset,
            part.element_bits,
            default,
        ));
    }
    // Concat lists the most significant element first.
    elements.reverse();
    let partial = alloc_like(builder, dest, width);
    builder.emit(SIRInstruction::Concat(partial, elements));
    builder.seal_block(SIRTerminator::Jump(merge_block, vec![partial]));
    builder.switch_to_block(merge_block);
    result
}

/// The raw bits of a `width`-bit index can address position `limit` or above.
pub(super) fn index_bits_exceed(width: usize, limit: usize) -> bool {
    u32::try_from(width)
        .ok()
        .and_then(|bits| 1u64.checked_shl(bits))
        .is_none_or(|values| values > limit as u64)
}

fn and_condition<A>(
    condition: &mut Option<RegisterId>,
    term: RegisterId,
    builder: &mut SIRBuilder<A>,
) {
    *condition = Some(match *condition {
        Some(previous) => {
            let next = builder.alloc_bit(1, false);
            builder.emit(SIRInstruction::Binary(
                next,
                previous,
                BinaryOp::LogicAnd,
                term,
            ));
            next
        }
        None => term,
    });
}

/// AND into `guard` the condition that the runtime `index` addresses one of
/// `limit` positions, or is merely known and non-negative when `limit` is
/// `None`. An index with X/Z bits or a negative value is out of range
/// (IEEE 1800-2023 7.4.6).
pub(super) fn and_index_guard<A>(
    guard: &mut Option<RegisterId>,
    index: RegisterId,
    signed: bool,
    limit: Option<usize>,
    builder: &mut SIRBuilder<A>,
) {
    let register = builder.register(&index).clone();
    let width = register.width();
    let is_logic = matches!(register, RegisterType::Logic { .. });
    // Values the index can take once a negative value has been rejected.
    let magnitude_bits = if signed {
        width.saturating_sub(1)
    } else {
        width
    };
    let bound = limit.filter(|&limit| {
        u32::try_from(magnitude_bits)
            .ok()
            .and_then(|bits| 1u64.checked_shl(bits))
            .is_none_or(|values| values > limit as u64)
    });
    if is_logic {
        let two_state = builder.alloc_bit(width, false);
        builder.emit(SIRInstruction::Unary(two_state, UnaryOp::ToTwoState, index));
        let known = builder.alloc_bit(1, false);
        builder.emit(SIRInstruction::Binary(
            known,
            index,
            BinaryOp::EqCase,
            two_state,
        ));
        and_condition(guard, known, builder);
    }
    if signed && width > 0 {
        let sign = builder.alloc_bit(1, false);
        builder.emit(SIRInstruction::Slice(sign, index, width - 1, 1));
        let non_negative = builder.alloc_bit(1, false);
        builder.emit(SIRInstruction::Unary(non_negative, UnaryOp::LogicNot, sign));
        and_condition(guard, non_negative, builder);
    }
    if let Some(limit) = bound {
        // A needed bound is less than 2^width, so it fits the index width.
        let bound = builder.alloc_bit(width, false);
        builder.emit(SIRInstruction::Imm(bound, SIRValue::new(limit as u64)));
        let in_range = builder.alloc_bit(1, false);
        builder.emit(SIRInstruction::Binary(
            in_range,
            index,
            BinaryOp::LtU,
            bound,
        ));
        and_condition(guard, in_range, builder);
    }
}
