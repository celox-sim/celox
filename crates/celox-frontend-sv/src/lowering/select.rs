//! Packed selects and array element accesses at constant and run-time positions.

use super::*;

/// A write to a packed vector whose selected position is a runtime value.
pub(super) struct DynamicPackedWrite {
    /// Number of bits written.
    pub(super) select_width: usize,
    /// Where the selected bits sit; see [`RuntimePosition`].
    pub(super) up: sv::ir::Expr,
    pub(super) down: sv::ir::Expr,
    /// For a run-time select inside a constant element of an array, the
    /// bits of that element: only they are written.
    pub(super) window: Option<BitAccess>,
}

/// Recover a constant outer element for a checked dynamic bit select of a
/// one-bit packed element. Its only valid inner index is its declared bound.
/// An arbitrary dynamic unpacked element permits more than one index and must
/// retain its dynamic element addressing.
fn single_bit_select_window(
    lsb: &sv::ir::ConstExpr,
    width: usize,
    constants: &HashMap<String, i128>,
    types: &HashMap<String, (usize, bool)>,
) -> Option<BitAccess> {
    use sv::ir::{BinaryOp, ConstExpr};
    fn selected<'a>(
        mut expr: &'a ConstExpr,
        constants: &HashMap<String, i128>,
        types: &HashMap<String, (usize, bool)>,
    ) -> &'a ConstExpr {
        while let ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } = expr
        {
            match sv::typecheck::eval_const_expr_with_types(condition, constants, types) {
                Some(0) => expr = else_expr,
                Some(_) => expr = then_expr,
                None => break,
            }
        }
        expr
    }
    let (base, offset) = split_dynamic_array_offset(lsb, constants, types)?;
    let ConstExpr::Mux {
        condition,
        then_expr,
        else_expr,
    } = selected(base, constants, types)
    else {
        return None;
    };
    let ConstExpr::Binary {
        left,
        op: BinaryOp::LogicAnd,
        right,
    } = selected(condition, constants, types)
    else {
        return None;
    };
    let bound = |expr, expected| {
        let ConstExpr::Binary { left, op, right } = selected(expr, constants, types) else {
            return None;
        };
        let ConstExpr::Ident(name) = left.as_ref() else {
            return None;
        };
        if *op != expected {
            return None;
        }
        Some((
            name,
            sv::typecheck::eval_const_expr_with_types(right, constants, types)?,
        ))
    };
    let lower = bound(left, BinaryOp::Ge)?;
    let upper = bound(right, BinaryOp::Le)?;
    if lower != upper {
        return None;
    }
    let invalid = sv::typecheck::eval_const_expr_with_types(else_expr, constants, types)?
        .checked_add(offset)?;
    let width = i128::try_from(width).ok()?;
    if (0..width).contains(&invalid) {
        return None;
    }
    let mut checked_index = constants.clone();
    checked_index.insert(lower.0.clone(), lower.1);
    let position = sv::typecheck::eval_const_expr_with_types(then_expr, &checked_index, types)?
        .checked_add(offset)?;
    if !(0..width).contains(&position) {
        return None;
    }
    let position = usize::try_from(position).ok()?;
    Some(BitAccess::new(position, position))
}

/// For a run-time select inside one element of an unpacked array, such as
/// `mem[1][i]`, the bits of that element: the constant part of the flattened
/// position names the element, and the run-time part moves within it.
pub(super) fn runtime_select_window(
    lsb: &sv::ir::ConstExpr,
    element_width: usize,
    width: usize,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<BitAccess> {
    // A checked bit select inside a one-bit element can only write its zero
    // bit. Preserve the constant outer element in both driver analysis and
    // lowering; an arbitrary run-time array index remains unconstrained.
    if element_width == 1 {
        return single_bit_select_window(lsb, width, constants, parameter_types);
    }
    if element_width == 0 || element_width == width {
        return None;
    }
    let (base, offset) = split_dynamic_array_offset(lsb, constants, parameter_types)?;
    // A run-time element index can reach every element.
    if dynamic_array_base_steps_elements(
        base,
        i128::try_from(element_width).ok()?,
        constants,
        parameter_types,
    ) {
        return None;
    }
    let element = usize::try_from(offset).ok()? / element_width;
    let lsb = element.checked_mul(element_width)?;
    let msb = lsb.checked_add(element_width - 1)?;
    (msb < width).then(|| BitAccess::new(lsb, msb))
}

pub(super) fn dynamic_packed_write(
    lvalue: &sv::ir::LValue,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<DynamicPackedWrite> {
    let sv::ir::LValue::Select {
        name,
        msb,
        lsb,
        array_slice_width: None,
        ..
    } = lvalue
    else {
        return None;
    };
    let id = *name_to_id.get(name)?;
    let variable = variables.get(&id)?;
    // Callers try the element lowering of an array first.
    let position = runtime_select_position(
        name,
        msb,
        lsb,
        variables,
        name_to_id,
        constants,
        parameter_types,
        true,
        false,
    )?;
    let window = (!variable.is_unpacked_struct)
        .then(|| unpacked_element_width(variable))
        .flatten()
        .and_then(|element_width| {
            runtime_select_window(
                lsb,
                element_width,
                variable.width,
                constants,
                parameter_types,
            )
        });
    (position.width <= variable.width).then_some(DynamicPackedWrite {
        select_width: position.width,
        up: position.up,
        down: position.down,
        window,
    })
}

pub(super) fn permute_reversed_lvalue_rhs_slt(
    lvalue: &sv::ir::LValue,
    expr: NodeId,
    target_width: usize,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    arena: &mut SLTNodeArena<SourceVarId>,
) -> Option<NodeId> {
    let sv::ir::LValue::Select {
        array_slice_width: Some(array_slice_width),
        array_slice_reversed: true,
        ..
    } = lvalue
    else {
        return Some(expr);
    };
    let element_width = usize::try_from(sv::typecheck::eval_const_expr_with_types(
        array_slice_width,
        constants,
        parameter_types,
    )?)
    .ok()
    .filter(|width| *width != 0)?;
    if !target_width.is_multiple_of(element_width) {
        return None;
    }
    let element_count = target_width / element_width;
    if element_count <= 1 {
        return Some(expr);
    }
    let mut parts = Vec::with_capacity(element_count);
    for lsb in (0..target_width).step_by(element_width) {
        let msb = lsb.checked_add(element_width)?.checked_sub(1)?;
        let part = arena
            .alloc(SLTNode::Slice {
                expr,
                access: BitAccess::new(lsb, msb),
            })
            .ok()?;
        parts.push((part, element_width));
    }
    arena.alloc(SLTNode::Concat(parts)).ok()
}

pub(super) fn packed_index_offset(variable: &SvVariable, index: i128) -> Option<usize> {
    if !variable.array_dims.is_empty() {
        return usize::try_from(index)
            .ok()
            .filter(|offset| *offset < variable.width);
    }
    let offset = match variable.packed_ranges.as_slice() {
        [(left, right)] if left >= right => index.checked_sub(*right)?,
        [(_, right)] => right.checked_sub(index)?,
        _ => index,
    };
    usize::try_from(offset)
        .ok()
        .filter(|offset| *offset < variable.width)
}

/// The bit position of declared index `index` of `variable`, which may lie
/// outside it; see [`packed_index_offset`].
pub(super) fn packed_index_position(variable: &SvVariable, index: i128) -> Option<i128> {
    if !variable.array_dims.is_empty() {
        return Some(index);
    }
    match variable.packed_ranges.as_slice() {
        [(left, right)] if left >= right => index.checked_sub(*right),
        [(_, right)] => right.checked_sub(index),
        _ => Some(index),
    }
}

pub(super) fn unpacked_element_width(variable: &SvVariable) -> Option<usize> {
    if variable.array_dims.is_empty() {
        return None;
    }
    let element_count = variable
        .array_dims
        .iter()
        .copied()
        .try_fold(1usize, usize::checked_mul)?;
    (element_count != 0).then(|| variable.width.checked_div(element_count))?
}

fn dynamic_array_selection_width(
    variable: &SvVariable,
    access: BitAccess,
    packed_element_width: usize,
) -> Option<usize> {
    let access_width = access.msb.checked_sub(access.lsb)?.checked_add(1)?;
    let element_width = packed_element_width.max(access_width);
    (element_width.is_multiple_of(packed_element_width)
        && variable.width.is_multiple_of(element_width)
        && access.msb < element_width)
        .then_some(element_width)
}

fn dynamic_array_index_kind(variable: &SvVariable, element_width: usize) -> SLTIndexKind {
    if unpacked_element_width(variable) == Some(element_width) {
        SLTIndexKind::Unpacked { element_width }
    } else {
        SLTIndexKind::Packed
    }
}

pub(super) fn lower_dynamic_array_selection_slt<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    variable: A,
    signed: bool,
    index: NodeId,
    access: BitAccess,
    element_width: usize,
    variable_info: &SvVariable,
) -> Option<NodeId> {
    let packed_element_width = unpacked_element_width(variable_info)?;
    if element_width == packed_element_width {
        return dynamic_array_input_slt(
            arena,
            variable,
            signed,
            SLTIndex {
                node: index,
                stride: element_width,
                kind: dynamic_array_index_kind(variable_info, element_width),
            },
            access,
            variable_info.width,
        );
    }
    if access.lsb != 0 || access.msb.checked_add(1)? != element_width {
        return None;
    }
    let inner_count = element_width.checked_div(packed_element_width)?;
    let inner_count_literal = arena
        .alloc(SLTNode::Constant(
            BigUint::from(inner_count),
            BigUint::default(),
            64,
            false,
        ))
        .ok()?;
    let scaled_index = arena
        .alloc(SLTNode::Binary(index, BinaryOp::Mul, inner_count_literal))
        .ok()?;
    let mut nodes = Vec::with_capacity(inner_count);
    for inner_index in (0..inner_count).rev() {
        let node = if inner_index == 0 {
            scaled_index
        } else {
            let inner_index_literal = arena
                .alloc(SLTNode::Constant(
                    BigUint::from(inner_index),
                    BigUint::default(),
                    64,
                    false,
                ))
                .ok()?;
            arena
                .alloc(SLTNode::Binary(
                    scaled_index,
                    BinaryOp::Add,
                    inner_index_literal,
                ))
                .ok()?
        };
        let node = dynamic_array_input_slt(
            arena,
            variable.clone(),
            signed,
            SLTIndex {
                node,
                stride: packed_element_width,
                kind: SLTIndexKind::Unpacked {
                    element_width: packed_element_width,
                },
            },
            BitAccess::new(0, packed_element_width - 1),
            variable_info.width,
        )?;
        nodes.push((node, packed_element_width));
    }
    arena.alloc(SLTNode::Concat(nodes)).ok()
}

/// Bits `access` of the element `index` selects. The input itself spans the
/// whole variable, because shared dependency analysis takes an indexed input's
/// footprint from its access range: a run-time index may reach any element.
/// Lowering composes the outer slice into the indexed load, so the selected
/// element is still loaded directly.
fn dynamic_array_input_slt<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    variable: A,
    signed: bool,
    index: SLTIndex,
    access: BitAccess,
    variable_width: usize,
) -> Option<NodeId> {
    let input = arena
        .alloc(SLTNode::Input {
            variable,
            signed,
            index: vec![index],
            access: BitAccess::new(0, variable_width.checked_sub(1)?),
        })
        .ok()?;
    arena
        .alloc(SLTNode::Slice {
            expr: input,
            access,
        })
        .ok()
}

pub(super) fn sv_memory_offset(
    variable: &SvVariable,
    bit_offset: usize,
    width: usize,
) -> SIROffset {
    match unpacked_element_width(variable) {
        Some(element_width)
            if element_width != 0
                && width > element_width
                && bit_offset.is_multiple_of(element_width)
                && width.is_multiple_of(element_width) =>
        {
            SIROffset::PackedElements {
                bit_offset,
                element_width,
            }
        }
        _ => SIROffset::Static(bit_offset),
    }
}

/// The bits of `variable` that a constant select from declared index `msb`
/// to `lsb` writes, and the number of low bits of the assigned value that
/// fall below the variable. A select partly outside the variable writes only
/// the bits inside, one wholly outside writes none (IEEE 1800-2023 11.5.1).
pub(super) fn constant_select_write(
    variable: &SvVariable,
    msb: i128,
    lsb: i128,
) -> Option<(Option<BitAccess>, usize)> {
    let (msb, lsb) = (
        packed_index_position(variable, msb)?,
        packed_index_position(variable, lsb)?,
    );
    let (low, high) = (msb.min(lsb), msb.max(lsb));
    let top = i128::try_from(variable.width).ok()? - 1;
    let (start, end) = (low.max(0), high.min(top));
    if start > end {
        return Some((None, 0));
    }
    Some((
        Some(BitAccess::new(
            usize::try_from(start).ok()?,
            usize::try_from(end).ok()?,
        )),
        usize::try_from(start - low).ok()?,
    ))
}

/// `folded`, the select `original` with the values its position reads
/// substituted, unless that turned a run-time select within one array
/// element into a constant one reaching past the element: the run-time form
/// keeps its write within the element (see `dynamic_packed_write`), while
/// the constant position counts from bit 0 of the whole array.
pub(super) fn keep_element_window(
    original: &sv::ir::LValue,
    folded: sv::ir::LValue,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> sv::ir::LValue {
    let constant =
        |expr| sv::typecheck::eval_const_expr_with_types(expr, constants, parameter_types);
    let (
        sv::ir::LValue::Select {
            msb: original_msb,
            lsb: original_lsb,
            ..
        },
        sv::ir::LValue::Select { msb, lsb, .. },
    ) = (original, &folded)
    else {
        return folded;
    };
    if constant(original_msb).is_some() && constant(original_lsb).is_some() {
        return folded;
    }
    let (Some(msb), Some(lsb)) = (constant(msb), constant(lsb)) else {
        return folded;
    };
    let Some(window) =
        dynamic_packed_write(original, variables, name_to_id, constants, parameter_types)
            .and_then(|write| write.window)
    else {
        return folded;
    };
    let inside = |position: i128| {
        usize::try_from(position)
            .is_ok_and(|position| (window.lsb..=window.msb).contains(&position))
    };
    if inside(msb) && inside(lsb) {
        folded
    } else {
        original.clone()
    }
}

pub(super) fn packed_expr_select_offsets(
    expr: &sv::ir::Expr,
    msb: i128,
    lsb: i128,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
) -> Option<(usize, usize)> {
    if let sv::ir::Expr::Ident(name) = expr {
        if let Some(variable) = name_to_id.get(name).and_then(|id| variables.get(id)) {
            return Some((
                packed_index_offset(variable, msb)?,
                packed_index_offset(variable, lsb)?,
            ));
        }
    }
    Some((usize::try_from(msb).ok()?, usize::try_from(lsb).ok()?))
}

/// Width of a packed select whose bounds depend on a runtime value. `None`
/// when both bounds are constants.
pub(super) fn runtime_select_width(
    msb: &sv::ir::ConstExpr,
    lsb: &sv::ir::ConstExpr,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<usize> {
    let constant = |bound: &sv::ir::ConstExpr| {
        sv::typecheck::eval_const_expr_with_types(bound, constants, parameter_types)
    };
    if constant(msb).is_some() && constant(lsb).is_some() {
        return None;
    }
    // A bit select has width one regardless of its runtime position. Sampling
    // both bounds evaluates the same (potentially inlined) expression twice,
    // and the zero sample need not even be a valid index computation.
    if msb == lsb {
        return Some(1);
    }
    // The width is the same for every value of the runtime operands.
    let mut sample = constants.clone();
    for name in name_to_id.keys() {
        sample.entry(name.clone()).or_insert(0);
    }
    let sampled = |bound: &sv::ir::ConstExpr| {
        sv::typecheck::eval_const_expr_with_types(bound, &sample, parameter_types)
    };
    usize::try_from(sampled(msb)?.abs_diff(sampled(lsb)?))
        .ok()?
        .checked_add(1)
}

/// How far a runtime-positioned select sits from bit 0 of its vector.
///
/// The lowest selected bit lies at position `low` (it is negative when the
/// select hangs over the bottom of the vector). `up` is `low` when it is
/// non-negative and zero otherwise; `down` is `-low` when it is negative and
/// zero otherwise. A write places a value with `(value << up) >> down`; a read
/// brings the selected bits back to bit 0 with `(value << down) >> up`.
struct RuntimePosition {
    width: usize,
    vector_width: usize,
    up: sv::ir::Expr,
    down: sv::ir::Expr,
    /// The bits of the array element the select stays within, when the
    /// position is measured from its bottom rather than from bit 0.
    window: Option<BitAccess>,
}

/// The select width and runtime position of the `lsb` index for a packed
/// select of `name` whose bounds depend on a runtime value. A variable keeps
/// its declared range; a parameter is a zero-based vector. With
/// `element_window`, a select within one array element is measured within
/// that element, so a position outside it selects no bits.
#[allow(clippy::too_many_arguments)]
fn runtime_select_position(
    name: &str,
    msb: &sv::ir::ConstExpr,
    lsb: &sv::ir::ConstExpr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    flat_arrays: bool,
    element_window: bool,
) -> Option<RuntimePosition> {
    let mut window = None;
    let mut whole_array = false;
    let (left, right) = match name_to_id.get(name).and_then(|id| variables.get(id)) {
        Some(variable) if variable.is_unpacked_struct => {
            // Whole structures cannot be bit selected in authored SV. These
            // coordinates come from member selection, which guards each
            // member's own window before flattening its array indices.
            if !flat_arrays {
                return None;
            }
            (i128::try_from(variable.width).ok()?.checked_sub(1)?, 0)
        }
        Some(variable) if !variable.array_dims.is_empty() => {
            // A select the element lowering cannot express, such as a
            // run-time bit of a constant element, addresses the flattened
            // bits of the array.
            if !flat_arrays {
                return None;
            }
            // The analyzer moves a bit select whose inner index is out of
            // range past the whole array, so its flattened position never
            // reaches a neighbouring element. Within a constant element, the
            // position can be measured within it.
            // A write stays in its constant element through its own window
            // (see `dynamic_packed_write`).
            let element = unpacked_element_width(variable).and_then(|element_width| {
                runtime_select_window(
                    lsb,
                    element_width,
                    variable.width,
                    constants,
                    parameter_types,
                )
            });
            // A single element spans the whole array.
            whole_array =
                element.is_none() && unpacked_element_width(variable) != Some(variable.width);
            window = element.filter(|_| element_window);
            match window {
                Some(window) => (
                    i128::try_from(window.msb).ok()?,
                    i128::try_from(window.lsb).ok()?,
                ),
                None => (i128::try_from(variable.width).ok()?.checked_sub(1)?, 0),
            }
        }
        Some(variable) => {
            match variable.packed_ranges.as_slice() {
                [range] => *range,
                // The analyzer flattens the selects of several packed
                // dimensions into bit offsets from bit 0.
                [_, _, ..] => (i128::try_from(variable.width).ok()?.checked_sub(1)?, 0),
                [] => return None,
            }
        }
        None => {
            constants.get(name)?;
            let (width, _) = parameter_types.get(name).copied()?;
            (i128::try_from(width).ok()?.checked_sub(1)?, 0)
        }
    };
    runtime_select_position_in_range(
        left,
        right,
        window,
        whole_array,
        msb,
        lsb,
        variables,
        name_to_id,
        constants,
        parameter_types,
    )
}

fn runtime_select_position_in_range(
    left: i128,
    right: i128,
    window: Option<BitAccess>,
    whole_array: bool,
    msb: &sv::ir::ConstExpr,
    lsb: &sv::ir::ConstExpr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<RuntimePosition> {
    if right < 0 {
        return None;
    }
    let width = runtime_select_width(msb, lsb, name_to_id, constants, parameter_types)?;
    // Only a bit select (the same position for both bounds) is moved past
    // the whole array when out of range; a part-select, even of one bit,
    // could reach a neighbouring element.
    if whole_array && (width != 1 || msb != lsb) {
        return None;
    }
    let index = expr_from_const_expr(&fold_constant_calls(lsb, constants, parameter_types))?;
    // An index such as `i - 1` wraps when it should be negative. Widen it with
    // its sign so that "hangs over the bottom" can be tested as a comparison.
    let index_is_wide =
        sv_expr_natural_width(&index, variables, name_to_id, constants, parameter_types)
            .is_some_and(|width| width >= 32);
    let index = sv::ir::Expr::Resize {
        expr: Box::new(index),
        width: 64,
        signed: index_is_wide,
    };
    let descending = left >= right;
    let boundary = || sv::ir::Expr::Literal(right.to_string());
    let zero = || sv::ir::Expr::Literal("0".to_string());
    let binary = |left, op, right| sv::ir::Expr::Binary {
        left: Box::new(left),
        op,
        right: Box::new(right),
    };
    let select = |condition, then_expr, else_expr| sv::ir::Expr::Mux {
        condition: Box::new(condition),
        then_expr: Box::new(then_expr),
        else_expr: Box::new(else_expr),
    };
    // `above` is the index's distance past the declared bit 0; it is
    // negative when the select hangs over the bottom of the vector.
    let (hangs_over, above, below) = if descending {
        (
            binary(index.clone(), sv::ir::BinaryOp::Lt, boundary()),
            binary(index.clone(), sv::ir::BinaryOp::Sub, boundary()),
            binary(boundary(), sv::ir::BinaryOp::Sub, index),
        )
    } else {
        (
            binary(index.clone(), sv::ir::BinaryOp::Gt, boundary()),
            binary(boundary(), sv::ir::BinaryOp::Sub, index.clone()),
            binary(index, sv::ir::BinaryOp::Sub, boundary()),
        )
    };
    Some(RuntimePosition {
        width,
        vector_width: usize::try_from(left.abs_diff(right)).ok()?.checked_add(1)?,
        up: select(hangs_over.clone(), zero(), above),
        down: select(hangs_over, below, zero()),
        window,
    })
}

/// Rewrite `v[msb:lsb]` of a packed vector, whose bounds depend on a runtime
/// value, as `(v >> low)[width-1:0]`, where `low` is the bit position of the
/// `lsb` index. Positions outside a four-state vector read as X (IEEE
/// 1800-2023 11.5.1); a two-state vector, or simulation, reads zero.
pub(super) fn runtime_select_as_shift(
    expr: &sv::ir::Expr,
    msb: &sv::ir::ConstExpr,
    lsb: &sv::ir::ConstExpr,
    signed: bool,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    flat_arrays: bool,
) -> Option<sv::ir::Expr> {
    let position = if let sv::ir::Expr::Ident(name) = expr {
        runtime_select_position(
            name,
            msb,
            lsb,
            variables,
            name_to_id,
            constants,
            parameter_types,
            flat_arrays,
            flat_arrays,
        )?
    } else {
        let width = sv_expr_natural_width(expr, variables, name_to_id, constants, parameter_types)?;
        runtime_select_position_in_range(
            i128::try_from(width).ok()?.checked_sub(1)?,
            0,
            None,
            false,
            msb,
            lsb,
            variables,
            name_to_id,
            constants,
            parameter_types,
        )?
    };
    let shift = |value: sv::ir::Expr, op, amount: sv::ir::Expr| sv::ir::Expr::Binary {
        left: Box::new(value),
        op,
        right: Box::new(amount),
    };
    // Bring the selection down to bit 0: right by `up`, or left by `down`
    // when it hangs over the bottom.
    let move_down = |value: sv::ir::Expr| sv::ir::Expr::Select {
        expr: Box::new(shift(
            shift(value, sv::ir::BinaryOp::Shl, position.down.clone()),
            sv::ir::BinaryOp::Shr,
            position.up.clone(),
        )),
        msb: sv::ir::ConstExpr::Literal((position.width - 1).to_string()),
        lsb: sv::ir::ConstExpr::Literal("0".to_string()),
        signed: false,
    };
    // Within an array element, only the element's bits can be selected.
    let vector = match position.window {
        Some(window) => sv::ir::Expr::Select {
            expr: Box::new(expr.clone()),
            msb: sv::ir::ConstExpr::Literal(window.msb.to_string()),
            lsb: sv::ir::ConstExpr::Literal(window.lsb.to_string()),
            signed: false,
        },
        None => expr.clone(),
    };
    let moved = move_down(vector);
    // Shifting fills the missing bits with 0, which is what a two-state
    // vector (or a parameter) reads.
    let four_state = match expr {
        sv::ir::Expr::Ident(name) => name_to_id
            .get(name)
            .and_then(|id| variables.get(id))
            .is_some_and(|variable| variable.is_4state),
        _ => true,
    };
    if !four_state {
        return Some(sv::ir::Expr::Select {
            expr: Box::new(moved),
            msb: sv::ir::ConstExpr::Literal((position.width - 1).to_string()),
            lsb: sv::ir::ConstExpr::Literal("0".to_string()),
            signed,
        });
    }
    // Moving an all-ones vector the same way marks the selected bits that
    // exist; the others read X.
    let literal = |digit: &str, width: usize| {
        sv::ir::Expr::Literal(format!("{width}'b{}", digit.repeat(width)))
    };
    let in_vector = move_down(literal("1", position.vector_width));
    let binary = |left, op, right| sv::ir::Expr::Binary {
        left: Box::new(left),
        op,
        right: Box::new(right),
    };
    let outside = sv::ir::Expr::Unary {
        op: sv::ir::UnaryOp::BitNot,
        expr: Box::new(in_vector.clone()),
    };
    let selected = binary(
        binary(moved, sv::ir::BinaryOp::BitAnd, in_vector),
        sv::ir::BinaryOp::BitOr,
        binary(
            literal("x", position.width),
            sv::ir::BinaryOp::BitAnd,
            outside,
        ),
    );
    Some(sv::ir::Expr::Select {
        expr: Box::new(selected),
        msb: sv::ir::ConstExpr::Literal((position.width - 1).to_string()),
        lsb: sv::ir::ConstExpr::Literal("0".to_string()),
        signed,
    })
}

/// The expression of select position `expr`, its calls of system
/// functions that have a constant value, such as the `$clog2(N)` of an
/// element width, replaced by their values (see [`fold_constant_calls`]).
pub(super) fn expr_from_select_position(
    expr: &sv::ir::ConstExpr,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<sv::ir::Expr> {
    expr_from_const_expr(&fold_constant_calls(expr, constants, parameter_types))
}

/// `expr` with each call of a system function that has a constant value,
/// such as the `$clog2(N)` of an element width in a select position,
/// replaced by that value: an `integer` (IEEE 1800-2023 20.8.1).
fn fold_constant_calls(
    expr: &sv::ir::ConstExpr,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> sv::ir::ConstExpr {
    use sv::ir::ConstExpr;
    let fold = |expr: &ConstExpr| Box::new(fold_constant_calls(expr, constants, parameter_types));
    match expr {
        ConstExpr::Literal(_) | ConstExpr::Ident(_) => expr.clone(),
        ConstExpr::Function {
            callee: sv::ir::Callee::System(sv::ir::SystemFunction::Clog2),
            ..
        } => match sv::typecheck::eval_const_expr_with_types(expr, constants, parameter_types) {
            Some(value) => ConstExpr::Literal(value.to_string()),
            None => expr.clone(),
        },
        ConstExpr::Function { .. } => expr.clone(),
        ConstExpr::SelectRange {
            expr,
            msb,
            lsb,
            signed,
        } => ConstExpr::SelectRange {
            expr: fold(expr),
            msb: fold(msb),
            lsb: fold(lsb),
            signed: *signed,
        },
        ConstExpr::Select { expr, bit } => ConstExpr::Select {
            expr: fold(expr),
            bit: fold(bit),
        },
        ConstExpr::Resize {
            expr,
            width,
            signed,
        } => ConstExpr::Resize {
            expr: fold(expr),
            width: *width,
            signed: *signed,
        },
        ConstExpr::Unary { op, expr } => ConstExpr::Unary {
            op: *op,
            expr: fold(expr),
        },
        ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
            left: fold(left),
            op: *op,
            right: fold(right),
        },
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => ConstExpr::Mux {
            condition: fold(condition),
            then_expr: fold(then_expr),
            else_expr: fold(else_expr),
        },
    }
}

pub(super) fn expr_from_const_expr(expr: &sv::ir::ConstExpr) -> Option<sv::ir::Expr> {
    expr_from_const_expr_with_calls(expr, false)
}

/// Reconstruct a select operand, optionally retaining nested runtime calls.
pub(super) fn expr_from_const_expr_with_calls(
    expr: &sv::ir::ConstExpr,
    allow_calls: bool,
) -> Option<sv::ir::Expr> {
    let convert = |expr| expr_from_const_expr_with_calls(expr, allow_calls);
    Some(match expr {
        sv::ir::ConstExpr::Literal(value) => sv::ir::Expr::Literal(value.clone()),
        sv::ir::ConstExpr::Ident(name) => sv::ir::Expr::Ident(name.clone()),
        sv::ir::ConstExpr::SelectRange {
            expr,
            msb,
            lsb,
            signed,
        } => sv::ir::Expr::Select {
            expr: Box::new(convert(expr)?),
            msb: (**msb).clone(),
            lsb: (**lsb).clone(),
            signed: *signed,
        },
        sv::ir::ConstExpr::Select { expr, bit } => sv::ir::Expr::Select {
            expr: Box::new(convert(expr)?),
            msb: (**bit).clone(),
            lsb: (**bit).clone(),
            signed: false,
        },
        sv::ir::ConstExpr::Resize {
            expr,
            width,
            signed,
        } => sv::ir::Expr::Resize {
            expr: Box::new(convert(expr)?),
            width: *width,
            signed: *signed,
        },
        sv::ir::ConstExpr::Function { callee, args, .. } if allow_calls => sv::ir::Expr::Call {
            callee: callee.clone(),
            arg_order: Vec::new(),
            args: args
                .iter()
                .map(|arg| convert(arg).map(Some))
                .collect::<Option<_>>()?,
        },
        sv::ir::ConstExpr::Function { .. } => return None,
        sv::ir::ConstExpr::Unary { op, expr } => sv::ir::Expr::Unary {
            op: *op,
            expr: Box::new(convert(expr)?),
        },
        sv::ir::ConstExpr::Binary { left, op, right } => sv::ir::Expr::Binary {
            left: Box::new(convert(left)?),
            op: *op,
            right: Box::new(convert(right)?),
        },
        sv::ir::ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => sv::ir::Expr::Mux {
            condition: Box::new(convert(condition)?),
            then_expr: Box::new(convert(then_expr)?),
            else_expr: Box::new(convert(else_expr)?),
        },
    })
}

pub(super) fn dynamic_array_element_subselection(
    expr: &sv::ir::Expr,
    msb: &sv::ir::ConstExpr,
    lsb: &sv::ir::ConstExpr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<(SourceVarId, usize, BitAccess)> {
    let sv::ir::Expr::Ident(name) = expr else {
        return None;
    };
    let id = *name_to_id.get(name)?;
    let variable = variables.get(&id)?;
    let packed_element_width = unpacked_element_width(variable).filter(|width| *width != 0)?;
    let is_dynamic = sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)
        .is_none()
        || sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types).is_none();
    if !is_dynamic {
        return None;
    }
    // This is a dynamic bit inside one constant one-bit element, rather
    // than a dynamic element of the whole array. Let the packed lowering
    // retain the checked element window.
    if packed_element_width == 1
        && single_bit_select_window(lsb, variable.width, constants, parameter_types).is_some()
    {
        return None;
    }
    let (msb_base, msb_offset) = split_dynamic_array_offset(msb, constants, parameter_types)?;
    let (lsb_base, lsb_offset) = split_dynamic_array_offset(lsb, constants, parameter_types)?;
    if msb_base != lsb_base {
        return None;
    }
    let msb = usize::try_from(msb_offset).ok()?;
    let lsb = usize::try_from(lsb_offset).ok()?;
    let access = BitAccess::new(msb.min(lsb), msb.max(lsb));
    let element_width = dynamic_array_selection_width(variable, access, packed_element_width)?;
    if element_width > 1
        && !dynamic_array_base_has_stride(
            msb_base,
            i128::try_from(element_width).ok()?,
            constants,
            parameter_types,
        )
    {
        return None;
    }
    Some((id, element_width, access))
}

fn split_dynamic_array_offset<'a>(
    expr: &'a sv::ir::ConstExpr,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<(&'a sv::ir::ConstExpr, i128)> {
    if sv::typecheck::eval_const_expr_with_types(expr, constants, parameter_types).is_some() {
        return None;
    }
    if let sv::ir::ConstExpr::Binary { left, op, right } = expr
        && *op == sv::ir::BinaryOp::Add
    {
        if let Some(offset) =
            sv::typecheck::eval_const_expr_with_types(right, constants, parameter_types)
            && sv::typecheck::eval_const_expr_with_types(left, constants, parameter_types).is_none()
        {
            return Some((left, offset));
        }
        if let Some(offset) =
            sv::typecheck::eval_const_expr_with_types(left, constants, parameter_types)
            && sv::typecheck::eval_const_expr_with_types(right, constants, parameter_types)
                .is_none()
        {
            return Some((right, offset));
        }
    }
    Some((expr, 0))
}

/// Whether some run-time term of the base steps by whole elements, as a
/// run-time element index does.
fn dynamic_array_base_steps_elements(
    expr: &sv::ir::ConstExpr,
    element_width: i128,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> bool {
    let steps =
        |expr| dynamic_array_base_steps_elements(expr, element_width, constants, parameter_types);
    let constant =
        |expr| sv::typecheck::eval_const_expr_with_types(expr, constants, parameter_types);
    match expr {
        sv::ir::ConstExpr::Binary { left, op, right } => {
            if *op == sv::ir::BinaryOp::Mul {
                let (left_value, right_value) = (constant(left), constant(right));
                if left_value.is_some_and(|value| value > 0 && value % element_width == 0)
                    && right_value.is_none()
                    || right_value.is_some_and(|value| value > 0 && value % element_width == 0)
                        && left_value.is_none()
                {
                    return true;
                }
            }
            steps(left) || steps(right)
        }
        sv::ir::ConstExpr::Mux {
            then_expr,
            else_expr,
            ..
        } => steps(then_expr) || steps(else_expr),
        _ => false,
    }
}

fn dynamic_array_base_has_stride(
    expr: &sv::ir::ConstExpr,
    element_width: i128,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> bool {
    // Every value of the base must be a multiple of the element width: a
    // run-time term without the stride, such as an inner bit index, moves
    // within an element.
    if let Some(value) = sv::typecheck::eval_const_expr_with_types(expr, constants, parameter_types)
    {
        return value % element_width == 0;
    }
    let has_stride =
        |expr| dynamic_array_base_has_stride(expr, element_width, constants, parameter_types);
    match expr {
        sv::ir::ConstExpr::Binary { left, op, right } => match op {
            sv::ir::BinaryOp::Mul => has_stride(left) || has_stride(right),
            sv::ir::BinaryOp::Add | sv::ir::BinaryOp::Sub => has_stride(left) && has_stride(right),
            _ => false,
        },
        sv::ir::ConstExpr::Mux {
            then_expr,
            else_expr,
            ..
        } => has_stride(then_expr) && has_stride(else_expr),
        _ => false,
    }
}

pub(super) fn dynamic_array_element_lvalue(
    lvalue: &sv::ir::LValue,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<(SourceVarId, usize, sv::ir::ConstExpr, BitAccess)> {
    let sv::ir::LValue::Select { name, msb, lsb, .. } = lvalue else {
        return None;
    };
    let id = *name_to_id.get(name)?;
    let variable = variables.get(&id)?;
    let packed_element_width = unpacked_element_width(variable).filter(|width| *width != 0)?;
    let is_dynamic = sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)
        .is_none()
        || sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types).is_none();
    if !is_dynamic {
        return None;
    }
    // This is a dynamic bit inside one constant one-bit element, rather
    // than a dynamic element of the whole array. Let the packed lowering
    // retain the checked element window.
    if packed_element_width == 1
        && single_bit_select_window(lsb, variable.width, constants, parameter_types).is_some()
    {
        return None;
    }
    let (msb_base, msb_offset) = split_dynamic_array_offset(msb, constants, parameter_types)?;
    let (lsb_base, lsb_offset) = split_dynamic_array_offset(lsb, constants, parameter_types)?;
    if msb_base != lsb_base {
        return None;
    }
    let offset = lsb.clone();
    let msb = usize::try_from(msb_offset).ok()?;
    let lsb = usize::try_from(lsb_offset).ok()?;
    let access = BitAccess::new(msb.min(lsb), msb.max(lsb));
    let element_width = dynamic_array_selection_width(variable, access, packed_element_width)?;
    if element_width > 1
        && !dynamic_array_base_has_stride(
            msb_base,
            i128::try_from(element_width).ok()?,
            constants,
            parameter_types,
        )
    {
        return None;
    }
    (access.msb < element_width).then_some((id, element_width, offset, access))
}

pub(super) fn lower_dynamic_array_element_index_slt(
    offset: &sv::ir::ConstExpr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    arena: &mut SLTNodeArena<SourceVarId>,
    element_width: usize,
) -> Option<(celox_slt::NodeId, HashSet<VarAtomBase<SourceVarId>>)> {
    let offset_expr = expr_from_select_position(offset, constants, parameter_types)?;
    let (offset, sources) = lower_expr_with_context(
        &offset_expr,
        variables,
        name_to_id,
        constants,
        parameter_types,
        arena,
        None,
        None,
    )?;
    let element_index = if element_width == 1 {
        offset
    } else {
        let divisor = arena
            .alloc(SLTNode::Constant(
                BigUint::from(element_width),
                BigUint::default(),
                64,
                false,
            ))
            .ok()?;
        arena
            .alloc(SLTNode::Binary(offset, BinaryOp::DivU, divisor))
            .ok()?
    };
    Some((element_index, sources))
}

pub(super) fn dynamic_array_index_guard_slt<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    index: NodeId,
    element_count: usize,
) -> Option<(NodeId, NodeId)> {
    let element_count = BigUint::from(element_count);
    let two_state_index = arena
        .alloc(SLTNode::Unary(UnaryOp::ToTwoState, index))
        .ok()?;
    let known = arena
        .alloc(SLTNode::Binary(index, BinaryOp::EqCase, two_state_index))
        .ok()?;
    let bound = arena
        .alloc(SLTNode::Constant(
            element_count,
            BigUint::default(),
            64,
            false,
        ))
        .ok()?;
    let in_range = arena
        .alloc(SLTNode::Binary(index, BinaryOp::LtU, bound))
        .ok()?;
    let valid = arena
        .alloc(SLTNode::Binary(known, BinaryOp::LogicAnd, in_range))
        .ok()?;
    let zero = arena
        .alloc(SLTNode::Constant(
            BigUint::default(),
            BigUint::default(),
            64,
            false,
        ))
        .ok()?;
    let safe_index = arena
        .alloc(SLTNode::Mux {
            cond: valid,
            then_expr: index,
            else_expr: zero,
        })
        .ok()?;
    Some((safe_index, valid))
}

pub(super) fn guard_dynamic_array_read_slt<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    valid: NodeId,
    value: NodeId,
    value_width: usize,
    is_4state: bool,
) -> Option<NodeId> {
    let unknown_mask = if is_4state {
        (BigUint::from(1u8) << value_width) - BigUint::from(1u8)
    } else {
        BigUint::default()
    };
    // An invalid index reads X: both the value and the mask bit are set
    // (IEEE 1800-2023 7.4.6).
    let unknown = arena
        .alloc(SLTNode::Constant(
            unknown_mask.clone(),
            unknown_mask,
            value_width,
            false,
        ))
        .ok()?;
    arena
        .alloc(SLTNode::Mux {
            cond: valid,
            then_expr: value,
            else_expr: unknown,
        })
        .ok()
}

/// `expr` with each constant select of a packed vector variable that lies
/// partly or wholly outside the vector replaced by the bits it reads: those
/// outside read as X, or 0 for a two-state value (IEEE 1800-2023 11.5.1).
/// Such a select can arise where a loop is unrolled, as in the branch of
/// `(i != N - 1) ? v[i + 1] : 1'b1` that the last iteration does not take.
/// `None` when `expr` has none.
pub(super) fn out_of_range_constant_selects(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    four_state: bool,
) -> Option<sv::ir::Expr> {
    use sv::ir::Expr;
    let rewrite = |expr: &Expr| {
        out_of_range_constant_selects(
            expr,
            variables,
            name_to_id,
            constants,
            parameter_types,
            four_state,
        )
    };
    // The operands, rewritten where any of them is.
    let rewrite_all = |exprs: &[&Expr]| -> Option<Vec<Expr>> {
        let rewritten: Vec<_> = exprs.iter().map(|expr| rewrite(expr)).collect();
        rewritten.iter().any(Option::is_some).then(|| {
            rewritten
                .into_iter()
                .zip(exprs)
                .map(|(rewritten, expr)| rewritten.unwrap_or_else(|| (*expr).clone()))
                .collect()
        })
    };
    match expr {
        Expr::Ident(_) | Expr::Literal(_) => None,
        // The calls left in an expression when it is lowered are pure, such
        // as `$countones`; their arguments are read as any operand.
        Expr::Call {
            callee,
            args,
            arg_order,
        } => {
            let present: Vec<&Expr> = args.iter().flatten().collect();
            rewrite_all(&present).map(|rewritten| {
                let mut rewritten = rewritten.into_iter();
                Expr::Call {
                    callee: callee.clone(),
                    args: args
                        .iter()
                        .map(|arg| arg.as_ref().map(|_| rewritten.next().expect("an argument")))
                        .collect(),
                    arg_order: arg_order.clone(),
                }
            })
        }
        Expr::Inside { expr, items } => {
            let mut operands = vec![&**expr];
            for item in items {
                operands.extend(item.exprs());
            }
            rewrite_all(&operands).map(|operands| {
                let mut operands = operands.into_iter();
                let expr = operands.next().expect("the operand");
                let items = items
                    .iter()
                    .map(|item| item.map(&mut |_| operands.next().expect("an item operand")))
                    .collect();
                Expr::Inside {
                    expr: Box::new(expr),
                    items,
                }
            })
        }
        Expr::Select {
            expr: expr_of_select,
            msb,
            lsb,
            signed,
        } => {
            let inner = expr_of_select;
            if let Expr::Ident(name) = &**inner
                && let Some(variable) = name_to_id.get(name).and_then(|id| variables.get(id))
                && variable.array_dims.is_empty()
                && let Some(msb) =
                    sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)
                && let Some(lsb) =
                    sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types)
                && let (Some(left), Some(right)) = (
                    packed_index_position(variable, msb),
                    packed_index_position(variable, lsb),
                )
            {
                let (low, high) = (left.min(right), left.max(right));
                let width = i128::try_from(variable.width).ok()?;
                if low >= 0 && high < width {
                    return None;
                }
                let fill = if four_state && variable.is_4state {
                    'x'
                } else {
                    '0'
                };
                let fill = |bits: i128| Expr::Literal(format!("{bits}'b{fill}"));
                // The declared index of bit position `position`.
                let index = |position: i128| match variable.packed_ranges.as_slice() {
                    [(left, right)] if left >= right => position + right,
                    [(_, right)] => right - position,
                    _ => position,
                };
                let literal = |value: i128| sv::ir::ConstExpr::Literal(value.to_string());
                // Most significant part first.
                let mut parts = Vec::new();
                if high >= width {
                    parts.push(fill(high - width.max(low) + 1));
                }
                if high >= 0 && low < width {
                    parts.push(Expr::Select {
                        expr: inner.clone(),
                        msb: literal(index(high.min(width - 1))),
                        lsb: literal(index(low.max(0))),
                        signed: *signed,
                    });
                }
                if low < 0 {
                    parts.push(fill(high.min(-1) - low + 1));
                }
                return Some(assembled(parts, high - low + 1, *signed));
            }
            let inner = rewrite(expr_of_select);
            let base = inner.as_ref().unwrap_or(&**expr_of_select);
            // A select of a subexpression, such as `a[1][i + 4]` once `a[1]`
            // is a part of `a`, counts from bit 0 of the subexpression.
            if !matches!(base, Expr::Ident(_))
                && let Some(msb) =
                    sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)
                && let Some(lsb) =
                    sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types)
                && let Some(width) =
                    sv_expr_natural_width(base, variables, name_to_id, constants, parameter_types)
                && let Ok(width) = i128::try_from(width)
                && (msb.min(lsb) < 0 || msb.max(lsb) >= width)
            {
                let (low, high) = (msb.min(lsb), msb.max(lsb));
                let unknown = four_state && base_is_four_state(base, variables, name_to_id);
                let fill = if unknown { 'x' } else { '0' };
                let fill = |bits: i128| Expr::Literal(format!("{bits}'b{fill}"));
                let literal = |value: i128| sv::ir::ConstExpr::Literal(value.to_string());
                let mut parts = Vec::new();
                if high >= width {
                    parts.push(fill(high - width.max(low) + 1));
                }
                if high >= 0 && low < width {
                    parts.push(Expr::Select {
                        expr: Box::new(base.clone()),
                        msb: literal(high.min(width - 1)),
                        lsb: literal(low.max(0)),
                        signed: *signed,
                    });
                }
                if low < 0 {
                    parts.push(fill(high.min(-1) - low + 1));
                }
                return Some(assembled(parts, high - low + 1, *signed));
            }
            // A run-time index may read past a vector too, as in
            // `data[addr[i + 1]]`.
            let bound = |bound: &sv::ir::ConstExpr| {
                out_of_range_index_selects(
                    bound,
                    variables,
                    name_to_id,
                    constants,
                    parameter_types,
                    four_state,
                )
            };
            let (new_msb, new_lsb) = (bound(msb), bound(lsb));
            if inner.is_none() && new_msb.is_none() && new_lsb.is_none() {
                return None;
            }
            Some(Expr::Select {
                expr: Box::new(inner.unwrap_or_else(|| (**expr_of_select).clone())),
                msb: new_msb.unwrap_or_else(|| msb.clone()),
                lsb: new_lsb.unwrap_or_else(|| lsb.clone()),
                signed: *signed,
            })
        }
        Expr::Concat(parts) => rewrite_all(&parts.iter().collect::<Vec<_>>()).map(Expr::Concat),
        Expr::RepeatConcat { count, parts } => {
            rewrite_all(&parts.iter().collect::<Vec<_>>()).map(|parts| Expr::RepeatConcat {
                count: count.clone(),
                parts,
            })
        }
        Expr::Resize {
            expr,
            width,
            signed,
        } => rewrite(expr).map(|expr| Expr::Resize {
            expr: Box::new(expr),
            width: *width,
            signed: *signed,
        }),
        Expr::Unary { op, expr } => rewrite(expr).map(|expr| Expr::Unary {
            op: *op,
            expr: Box::new(expr),
        }),
        Expr::Binary { left, op, right } => rewrite_all(&[left, right]).map(|mut operands| {
            let right = operands.pop().expect("two operands");
            let left = operands.pop().expect("two operands");
            Expr::Binary {
                left: Box::new(left),
                op: *op,
                right: Box::new(right),
            }
        }),
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => rewrite_all(&[condition, then_expr, else_expr]).map(|mut operands| {
            let else_expr = operands.pop().expect("three operands");
            let then_expr = operands.pop().expect("three operands");
            let condition = operands.pop().expect("three operands");
            Expr::Mux {
                condition: Box::new(condition),
                then_expr: Box::new(then_expr),
                else_expr: Box::new(else_expr),
            }
        }),
    }
}

/// The concatenation of `parts`, `width` bits, with the signedness of the
/// select they replace (IEEE 1800-2023 11.8.1).
fn assembled(parts: Vec<sv::ir::Expr>, width: i128, signed: bool) -> sv::ir::Expr {
    let concat = sv::ir::Expr::Concat(parts);
    if !signed {
        return concat;
    }
    sv::ir::Expr::Select {
        expr: Box::new(concat),
        msb: sv::ir::ConstExpr::Literal((width - 1).to_string()),
        lsb: sv::ir::ConstExpr::Literal("0".into()),
        signed: true,
    }
}

/// Index expression `expr` with each constant select of a packed vector
/// variable that lies partly or wholly outside the vector replaced by the
/// bits it reads: those outside read X, or 0 for a two-state value (IEEE
/// 1800-2023 11.5.1). `None` when it has none.
fn out_of_range_index_selects(
    expr: &sv::ir::ConstExpr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    four_state: bool,
) -> Option<sv::ir::ConstExpr> {
    use sv::ir::ConstExpr;
    let rewrite = |expr: &ConstExpr| {
        out_of_range_index_selects(
            expr,
            variables,
            name_to_id,
            constants,
            parameter_types,
            four_state,
        )
    };
    let rewrite_all = |exprs: &[&ConstExpr]| -> Option<Vec<ConstExpr>> {
        let rewritten: Vec<_> = exprs.iter().map(|expr| rewrite(expr)).collect();
        rewritten.iter().any(Option::is_some).then(|| {
            rewritten
                .into_iter()
                .zip(exprs)
                .map(|(rewritten, expr)| rewritten.unwrap_or_else(|| (*expr).clone()))
                .collect()
        })
    };
    // The bits that a select of `[msb:lsb]` of variable `base` reads when
    // they lie wholly outside it.
    let outside = |base: &ConstExpr, msb: &ConstExpr, lsb: &ConstExpr, signed: bool| {
        let ConstExpr::Ident(name) = base else {
            return None;
        };
        let variable = name_to_id.get(name).and_then(|id| variables.get(id))?;
        if !variable.array_dims.is_empty() {
            return None;
        }
        let msb = sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types)?;
        let lsb = sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types)?;
        let left = packed_index_position(variable, msb)?;
        let right = packed_index_position(variable, lsb)?;
        let width = i128::try_from(variable.width).ok()?;
        let (low, high) = (left.min(right), left.max(right));
        if low >= 0 && high < width {
            return None;
        }
        let fill = if four_state && variable.is_4state {
            'x'
        } else {
            '0'
        };
        let bits = usize::try_from(high - low + 1).ok()?;
        // Bit `k` of the select is the vector's bit at position `low + k`;
        // those inside the vector keep their value, the others read `fill`.
        let (inside_low, inside_high) = (low.max(0), high.min(width - 1));
        let fill_bits = (low..=high)
            .rev()
            .map(|position| {
                if (inside_low..=inside_high).contains(&position) {
                    '0'
                } else {
                    fill
                }
            })
            .collect::<String>();
        let fill_bits = ConstExpr::Literal(format!("{bits}'b{fill_bits}"));
        let value = if inside_low > inside_high {
            fill_bits
        } else {
            // The declared index of bit position `position`.
            let index = |position: i128| match variable.packed_ranges.as_slice() {
                [(left, right)] if left >= right => position + right,
                [(_, right)] => right - position,
                _ => position,
            };
            let literal = |value: i128| Box::new(ConstExpr::Literal(value.to_string()));
            let inside = ConstExpr::Binary {
                left: Box::new(ConstExpr::Resize {
                    expr: Box::new(ConstExpr::SelectRange {
                        expr: Box::new(base.clone()),
                        msb: literal(index(inside_high)),
                        lsb: literal(index(inside_low)),
                        signed: false,
                    }),
                    width: bits,
                    signed: false,
                }),
                op: sv::ir::BinaryOp::Shl,
                right: literal(inside_low - low),
            };
            // An unknown bit ORed with 0 stays unknown (11.4.8).
            if fill == '0' {
                inside
            } else {
                ConstExpr::Binary {
                    left: Box::new(inside),
                    op: sv::ir::BinaryOp::BitOr,
                    right: Box::new(fill_bits),
                }
            }
        };
        Some(if signed {
            ConstExpr::Resize {
                expr: Box::new(value),
                width: bits,
                signed: true,
            }
        } else {
            value
        })
    };
    match expr {
        ConstExpr::Literal(_) | ConstExpr::Ident(_) => None,
        ConstExpr::Select { expr, bit } => outside(expr, bit, bit, false).or_else(|| {
            rewrite_all(&[expr, bit]).map(|mut operands| {
                let bit = operands.pop().expect("two operands");
                let expr = operands.pop().expect("two operands");
                ConstExpr::Select {
                    expr: Box::new(expr),
                    bit: Box::new(bit),
                }
            })
        }),
        ConstExpr::SelectRange {
            expr,
            msb,
            lsb,
            signed,
        } => outside(expr, msb, lsb, *signed).or_else(|| {
            rewrite_all(&[expr, msb, lsb]).map(|mut operands| {
                let lsb = operands.pop().expect("three operands");
                let msb = operands.pop().expect("three operands");
                let expr = operands.pop().expect("three operands");
                ConstExpr::SelectRange {
                    expr: Box::new(expr),
                    msb: Box::new(msb),
                    lsb: Box::new(lsb),
                    signed: *signed,
                }
            })
        }),
        ConstExpr::Resize {
            expr,
            width,
            signed,
        } => rewrite(expr).map(|expr| ConstExpr::Resize {
            expr: Box::new(expr),
            width: *width,
            signed: *signed,
        }),
        ConstExpr::Function { callee, args, site } => rewrite_all(&args.iter().collect::<Vec<_>>())
            .map(|args| ConstExpr::Function {
                callee: callee.clone(),
                args,
                site: *site,
            }),
        ConstExpr::Unary { op, expr } => rewrite(expr).map(|expr| ConstExpr::Unary {
            op: *op,
            expr: Box::new(expr),
        }),
        ConstExpr::Binary { left, op, right } => rewrite_all(&[left, right]).map(|mut operands| {
            let right = operands.pop().expect("two operands");
            let left = operands.pop().expect("two operands");
            ConstExpr::Binary {
                left: Box::new(left),
                op: *op,
                right: Box::new(right),
            }
        }),
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => rewrite_all(&[condition, then_expr, else_expr]).map(|mut operands| {
            let else_expr = operands.pop().expect("three operands");
            let then_expr = operands.pop().expect("three operands");
            let condition = operands.pop().expect("three operands");
            ConstExpr::Mux {
                condition: Box::new(condition),
                then_expr: Box::new(then_expr),
                else_expr: Box::new(else_expr),
            }
        }),
    }
}

/// Whether the vectors `expr` reads are four-state: an unknown bit past a
/// subexpression reads as X only where its bits can be X.
fn base_is_four_state(
    expr: &sv::ir::Expr,
    variables: &HashMap<SourceVarId, SvVariable>,
    name_to_id: &HashMap<String, SourceVarId>,
) -> bool {
    use sv::ir::Expr;
    let any = |exprs: &[&Expr]| {
        exprs
            .iter()
            .any(|expr| base_is_four_state(expr, variables, name_to_id))
    };
    match expr {
        Expr::Ident(name) => name_to_id
            .get(name)
            .and_then(|id| variables.get(id))
            .is_some_and(|variable| variable.is_4state),
        // A conversion to two states, such as a cast to `bit`, reads no X.
        Expr::Unary {
            op: sv::ir::UnaryOp::ToTwoState,
            ..
        } => false,
        Expr::Literal(text) => text
            .chars()
            .any(|digit| matches!(digit, 'x' | 'X' | 'z' | 'Z' | '?')),
        Expr::Select { expr, .. } | Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => {
            base_is_four_state(expr, variables, name_to_id)
        }
        Expr::Concat(parts) | Expr::RepeatConcat { parts, .. } => {
            any(&parts.iter().collect::<Vec<_>>())
        }
        Expr::Binary { left, right, .. } => any(&[left, right]),
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => any(&[condition, then_expr, else_expr]),
        Expr::Inside { .. } | Expr::Call { .. } => true,
    }
}
