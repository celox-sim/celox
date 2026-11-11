//! Dynamic index and part selects, and assignments through them.

use super::*;

pub(super) struct DynamicSelectOffset {
    pub(super) node: NodeId,
    pub(super) indices: Vec<SLTIndex>,
    pub(super) prefix_access: BitAccess,
    pub(super) selected_width: usize,
    pub(super) sources: HashSet<VarAtomBase<VarId>>,
    pub(super) boundaries: BoundaryMap<VarId>,
    /// Every runtime index is known, non-negative and inside its dimension,
    /// except that a part select may lie partly outside its dimension.
    /// `None` when no index can be invalid.
    pub(super) indices_valid: Option<NodeId>,
    /// The whole selection is inside the variable. `None` when it always is.
    pub(super) valid: Option<NodeId>,
    /// The raw bits of some index can address memory outside its dimension,
    /// so a load must replace an invalid index before addressing memory.
    /// Otherwise only the loaded value needs to be replaced by X.
    pub(super) clamp: bool,
    /// A runtime part select that may lie partly outside its dimension.
    pub(super) partial: Option<DynamicPartSelect>,
}

/// A runtime `+:`, `-:` or step part select whose elements may partly fall
/// outside their dimension (IEEE 1800-2023 11.5.1).
///
/// Its lowest element is `position - pad`. For `-:` that can be negative, so
/// positions are kept relative to `pad` elements below the dimension.
pub(super) struct DynamicPartSelect {
    /// Logical bit offset of the dimension ("row") containing the selection.
    pub(super) row_offset: NodeId,
    pub(super) row_bits: usize,
    /// `lowest selected element + pad`, which is never negative.
    pub(super) position: NodeId,
    pub(super) pad: usize,
    pub(super) element_bits: usize,
}

/// AND `term` into `condition`.
pub(super) fn and_slt_condition(
    arena: &mut SLTNodeArena<VarId>,
    condition: Option<NodeId>,
    term: NodeId,
) -> Result<NodeId, ParserError> {
    Ok(match condition {
        Some(previous) => arena.alloc(SLTNode::Binary(previous, BinaryOp::LogicAnd, term))?,
        None => term,
    })
}

/// Records the value range of a loop variable while its body is lowered, so
/// an index such as `values[i * 8 +: 8]` in `for i in 0..16` needs no range
/// check. The body lowers through the scope, which derefs to the arena.
pub(in crate::lowering) struct LoopRangeScope<'a> {
    pub(super) arena: &'a mut SLTNodeArena<VarId>,
    pub(super) variable: VarId,
    pub(super) previous: Option<Option<BigUint>>,
}

impl<'a> LoopRangeScope<'a> {
    pub(in crate::lowering) fn enter(
        arena: &'a mut SLTNodeArena<VarId>,
        for_stmt: &ForStatement,
    ) -> Self {
        let (start, end, inclusive) = match &for_stmt.range {
            ForRange::Forward {
                start,
                end,
                inclusive,
                ..
            }
            | ForRange::Reverse {
                start,
                end,
                inclusive,
                ..
            }
            | ForRange::Stepped {
                start,
                end,
                inclusive,
                ..
            } => (start, end, *inclusive),
        };
        // Every iteration value lies in `start..end` (or `..=end`); a
        // constant bound is never negative.
        let max = match (start, end) {
            (ForBound::Const(..), ForBound::Const(end, _)) => {
                if inclusive {
                    Some(BigUint::from(*end))
                } else {
                    end.checked_sub(1).map(BigUint::from)
                }
            }
            _ => None,
        };
        match max {
            Some(max) => Self::with_bound(arena, for_stmt.var_id, max),
            None => Self {
                arena,
                variable: for_stmt.var_id,
                previous: None,
            },
        }
    }

    /// Record that `variable` takes only values in `0..=max`.
    pub(in crate::lowering) fn with_max(
        arena: &'a mut SLTNodeArena<VarId>,
        variable: VarId,
        max: usize,
    ) -> Self {
        Self::with_bound(arena, variable, BigUint::from(max))
    }

    pub(super) fn with_bound(
        arena: &'a mut SLTNodeArena<VarId>,
        variable: VarId,
        max: BigUint,
    ) -> Self {
        let previous = arena.loop_ranges.insert(variable, max);
        Self {
            arena,
            variable,
            previous: Some(previous),
        }
    }
}

impl std::ops::Deref for LoopRangeScope<'_> {
    type Target = SLTNodeArena<VarId>;
    fn deref(&self) -> &Self::Target {
        self.arena
    }
}

impl std::ops::DerefMut for LoopRangeScope<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.arena
    }
}

impl Drop for LoopRangeScope<'_> {
    fn drop(&mut self) {
        let Some(previous) = self.previous.take() else {
            return;
        };
        let ranges = &mut self.arena.loop_ranges;
        match previous {
            Some(previous) => ranges.insert(self.variable, previous),
            None => ranges.remove(&self.variable),
        };
    }
}

/// A conservative upper bound of the unsigned value of `node`.
///
/// Index expressions are often computed at a wider width than their value
/// range, e.g. `offset * 8` for a 2-bit `offset`; the width alone would make
/// such an index look as if it could leave its dimension.
pub(super) fn slt_max_value(arena: &SLTNodeArena<VarId>, node: NodeId) -> BigUint {
    let width = expr::get_width(node, arena);
    let all_ones = (BigUint::from(1u8) << width) - BigUint::from(1u8);
    let bit_length_ones =
        |value: &BigUint| (BigUint::from(1u8) << value.bits()) - BigUint::from(1u8);
    let max = match arena.get(node) {
        SLTNode::Constant(value, mask, _, _) if mask.is_zero() => value.clone(),
        SLTNode::Input {
            variable,
            index,
            access,
            ..
        } if index.is_empty() && access.lsb == 0 => arena
            .loop_ranges
            .get(variable)
            .cloned()
            .unwrap_or_else(|| all_ones.clone()),
        SLTNode::Binary(lhs, op, rhs) => {
            let (lhs, rhs) = (*lhs, *rhs);
            match op {
                BinaryOp::Add => slt_max_value(arena, lhs) + slt_max_value(arena, rhs),
                BinaryOp::Mul => slt_max_value(arena, lhs) * slt_max_value(arena, rhs),
                BinaryOp::Shl => match arena.get(rhs) {
                    SLTNode::Constant(amount, mask, _, _) if mask.is_zero() => amount
                        .to_usize()
                        .filter(|&amount| amount < width)
                        .map_or(all_ones.clone(), |amount| {
                            slt_max_value(arena, lhs) << amount
                        }),
                    _ => all_ones.clone(),
                },
                BinaryOp::Shr => slt_max_value(arena, lhs),
                BinaryOp::And => slt_max_value(arena, lhs).min(slt_max_value(arena, rhs)),
                BinaryOp::Or | BinaryOp::Xor => {
                    bit_length_ones(&slt_max_value(arena, lhs).max(slt_max_value(arena, rhs)))
                }
                _ => all_ones.clone(),
            }
        }
        SLTNode::Slice { expr, access } if access.lsb == 0 => slt_max_value(arena, *expr),
        SLTNode::Mux {
            then_expr,
            else_expr,
            ..
        } => slt_max_value(arena, *then_expr).max(slt_max_value(arena, *else_expr)),
        SLTNode::Concat(parts) => {
            // The first part is the most significant.
            let mut max = BigUint::zero();
            let mut shift = 0;
            for (part, part_width) in parts.iter().rev() {
                max += slt_max_value(arena, *part) << shift;
                shift += part_width;
            }
            max
        }
        _ => all_ones.clone(),
    };
    max.min(all_ones)
}

/// The raw value of `node` can address position `limit` or above.
pub(super) fn slt_index_exceeds(arena: &SLTNodeArena<VarId>, node: NodeId, limit: usize) -> bool {
    slt_max_value(arena, node) >= BigUint::from(limit)
}

/// A signed index whose sign bit can be set. Scaling sign-extends it, so its
/// raw bits do not bound the address it reaches.
pub(super) fn slt_index_may_be_negative(
    arena: &SLTNodeArena<VarId>,
    node: NodeId,
    expression: &Expression,
) -> bool {
    let width = expr::get_width(node, arena);
    expression.comptime().r#type.signed
        && width > 0
        && slt_max_value(arena, node) >> (width - 1) != BigUint::zero()
}

/// The condition that a runtime index is known, non-negative and less than
/// `limit` (`None`: unbounded), or `None` when it always is
/// (IEEE 1800-2023 7.4.6).
pub(super) fn slt_index_guard(
    arena: &mut SLTNodeArena<VarId>,
    node: NodeId,
    expression: &Expression,
    limit: Option<usize>,
) -> Result<Option<NodeId>, ParserError> {
    let ty = &expression.comptime().r#type;
    let width = expr::get_width(node, arena);
    // An index whose sign bit is never set cannot be negative.
    let signed =
        ty.signed && width > 0 && slt_max_value(arena, node) >> (width - 1) != BigUint::zero();
    let mut condition = None;
    if !ty.is_2state() {
        let two_state = arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, node))?;
        let known = arena.alloc(SLTNode::Binary(node, BinaryOp::EqCase, two_state))?;
        condition = Some(and_slt_condition(arena, condition, known)?);
    }
    if signed && width > 0 {
        let sign = arena.alloc(SLTNode::Slice {
            expr: node,
            access: BitAccess::new(width - 1, width - 1),
        })?;
        let non_negative = arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, sign))?;
        condition = Some(and_slt_condition(arena, condition, non_negative)?);
    }
    let bound = limit.filter(|&limit| {
        if signed {
            // Values the index can take once a negative value is rejected.
            u32::try_from(width.saturating_sub(1))
                .ok()
                .and_then(|bits| 1u64.checked_shl(bits))
                .is_none_or(|values| values > limit as u64)
        } else {
            slt_index_exceeds(arena, node, limit)
        }
    });
    if let Some(limit) = bound {
        // A needed bound is less than 2^width, so it fits the index width.
        let bound = arena.alloc(SLTNode::Constant(
            BigUint::from(limit),
            BigUint::from(0u8),
            width,
            false,
        ))?;
        let in_range = arena.alloc(SLTNode::Binary(node, BinaryOp::LtU, bound))?;
        condition = Some(and_slt_condition(arena, condition, in_range)?);
    }
    Ok(condition)
}

/// The `width`-bit value of an invalid read: X from a four-state variable, 0
/// from a two-state one (IEEE 1800-2023 7.4.6, Table 7-1).
pub(super) fn slt_unknown(
    arena: &mut SLTNodeArena<VarId>,
    width: usize,
    two_state: bool,
) -> Result<NodeId, ParserError> {
    let mask = if two_state {
        BigUint::zero()
    } else {
        (BigUint::from(1u8) << width) - BigUint::from(1u8)
    };
    Ok(arena.alloc(SLTNode::Constant(mask.clone(), mask, width, false))?)
}

/// `value` when `condition` holds, the value of an invalid read otherwise.
pub(super) fn slt_or_unknown(
    arena: &mut SLTNodeArena<VarId>,
    condition: Option<NodeId>,
    value: NodeId,
    width: usize,
    two_state: bool,
) -> Result<NodeId, ParserError> {
    let Some(condition) = condition else {
        return Ok(value);
    };
    let unknown = slt_unknown(arena, width, two_state)?;
    Ok(arena.alloc(SLTNode::Mux {
        cond: condition,
        then_expr: value,
        else_expr: unknown,
    })?)
}

/// Where a partly out-of-range part select is read from.
pub(super) struct PartWindow {
    /// First element of a `selected_width`-wide window inside the row.
    pub(super) start: NodeId,
    /// Bits by which the selection lies above (`up`) or below (`down`) the
    /// window; at most one is nonzero.
    pub(super) up: NodeId,
    pub(super) down: NodeId,
}

impl DynamicPartSelect {
    pub(super) fn constant(
        arena: &mut SLTNodeArena<VarId>,
        value: usize,
    ) -> Result<NodeId, ParserError> {
        Ok(arena.alloc(SLTNode::Constant(
            BigUint::from(value),
            BigUint::from(0u8),
            64,
            false,
        ))?)
    }

    /// Clamp the selection into the row, so a narrow load of the window
    /// addresses only the row.
    pub(super) fn window(
        &self,
        arena: &mut SLTNodeArena<VarId>,
        selected_width: usize,
    ) -> Result<PartWindow, ParserError> {
        let elements = selected_width / self.element_bits;
        let row_elements = self.row_bits / self.element_bits;
        // `position` is the lowest element plus `pad`; it lies in `low..=high`
        // when the whole selection is inside the row.
        let low = Self::constant(arena, self.pad)?;
        let high = Self::constant(arena, row_elements - elements + self.pad)?;
        let zero = Self::constant(arena, 0)?;
        let element_bits = Self::constant(arena, self.element_bits)?;
        // Compare and select at the 64-bit width of the constants.
        let position = arena.alloc(SLTNode::Binary(self.position, BinaryOp::Add, zero))?;
        let above = arena.alloc(SLTNode::Binary(position, BinaryOp::GtU, high))?;
        let below = arena.alloc(SLTNode::Binary(position, BinaryOp::LtU, low))?;
        let inside = arena.alloc(SLTNode::Mux {
            cond: below,
            then_expr: low,
            else_expr: position,
        })?;
        let clamped = arena.alloc(SLTNode::Mux {
            cond: above,
            then_expr: high,
            else_expr: inside,
        })?;
        let start = arena.alloc(SLTNode::Binary(clamped, BinaryOp::Sub, low))?;
        let distance = |arena: &mut SLTNodeArena<VarId>,
                        condition: NodeId,
                        from: NodeId,
                        to: NodeId|
         -> Result<NodeId, ParserError> {
            let elements = arena.alloc(SLTNode::Binary(from, BinaryOp::Sub, to))?;
            let bits = arena.alloc(SLTNode::Binary(elements, BinaryOp::Mul, element_bits))?;
            Ok(arena.alloc(SLTNode::Mux {
                cond: condition,
                then_expr: bits,
                else_expr: zero,
            })?)
        };
        let up = distance(arena, above, position, high)?;
        let down = distance(arena, below, low, position)?;
        Ok(PartWindow { start, up, down })
    }

    /// Align the selection from the loaded `window`. Its bits outside the
    /// row read X, or 0 from a two-state variable.
    pub(super) fn align(
        &self,
        arena: &mut SLTNodeArena<VarId>,
        window: NodeId,
        placement: &PartWindow,
        selected_width: usize,
        two_state: bool,
    ) -> Result<NodeId, ParserError> {
        let shift = |arena: &mut SLTNodeArena<VarId>, value: NodeId| {
            let raised = arena.alloc(SLTNode::Binary(value, BinaryOp::Shl, placement.down))?;
            let moved = arena.alloc(SLTNode::Binary(raised, BinaryOp::Shr, placement.up))?;
            Ok::<_, ParserError>(arena.alloc(SLTNode::Slice {
                expr: moved,
                access: BitAccess::new(0, selected_width - 1),
            })?)
        };
        let value = shift(arena, window)?;
        let ones = arena.alloc(SLTNode::Constant(
            (BigUint::from(1u8) << selected_width) - BigUint::from(1u8),
            BigUint::from(0u8),
            selected_width,
            false,
        ))?;
        let in_row = shift(arena, ones)?;
        // Bitwise select: in-row bits keep `value`, the rest read X.
        let kept = arena.alloc(SLTNode::Binary(value, BinaryOp::And, in_row))?;
        let outside = arena.alloc(SLTNode::Unary(UnaryOp::BitNot, in_row))?;
        let unknown = slt_unknown(arena, selected_width, two_state)?;
        let unknown = arena.alloc(SLTNode::Binary(unknown, BinaryOp::And, outside))?;
        Ok(arena.alloc(SLTNode::Binary(kept, BinaryOp::Or, unknown))?)
    }
}

/// Replace every runtime index with 0 unless `valid` holds, so a load always
/// addresses memory inside the variable. Nothing is replaced unless `clamp`
/// says that the raw index bits can leave the variable.
pub(super) fn safe_slt_indices(
    arena: &mut SLTNodeArena<VarId>,
    mut indices: Vec<SLTIndex>,
    valid: Option<NodeId>,
    clamp: bool,
) -> Result<Vec<SLTIndex>, ParserError> {
    let Some(valid) = valid.filter(|_| clamp) else {
        return Ok(indices);
    };
    for entry in &mut indices {
        let width = expr::get_width(entry.node, arena);
        let zero = arena.alloc(SLTNode::Constant(
            BigUint::from(0u8),
            BigUint::from(0u8),
            width,
            false,
        ))?;
        entry.node = arena.alloc(SLTNode::Mux {
            cond: valid,
            then_expr: entry.node,
            else_expr: zero,
        })?;
    }
    Ok(indices)
}

/// Build the effective LSB for a dynamic access from validated select
/// geometry.  The returned `indices` and arithmetic `node` encode the same
/// offset so direct dynamic loads and read-modify-write paths cannot diverge.
pub(super) fn eval_dynamic_select_offset(
    module: &Module,
    store: &mut expr::ExpressionStore<'_>,
    var_id: VarId,
    index: &VarIndex,
    select: &VarSelect,
    arena: &mut SLTNodeArena<VarId>,
    token: Option<&TokenRange>,
) -> Result<DynamicSelectOffset, ParserError> {
    let geometry = select_geometry(module, var_id, index, select)?;
    let array_dimension_count = module.variables[&var_id].r#type.array.iter().count();
    let array_element_width = if array_dimension_count == 0 {
        None
    } else {
        geometry.strides.get(array_dimension_count - 1).copied()
    };
    let mut offset = arena.alloc(SLTNode::Constant(
        BigUint::from(0u8),
        BigUint::from(0u8),
        64,
        false,
    ))?;
    let mut indices =
        Vec::with_capacity(geometry.dimension_count + usize::from(geometry.part.is_some()));
    let mut sources = HashSet::default();
    let mut boundaries = BoundaryMap::default();
    let mut indices_valid = None;
    let mut clamp = false;
    let dimension_width = |dimension: usize| {
        let outer = if dimension == 0 {
            geometry.total_width
        } else {
            geometry.strides[dimension - 1]
        };
        outer / geometry.strides[dimension].max(1)
    };

    for (dimension, expression) in index
        .0
        .iter()
        .chain(&select.0)
        .take(geometry.dimension_count)
        .enumerate()
    {
        let ((node, node_sources), node_boundaries) =
            expr::eval_expression_in_context(module, store, expression, arena, None)?;
        sources.extend(node_sources);
        boundaries = merge_boundaries(boundaries, node_boundaries);
        let stride = geometry.strides.get(dimension).copied().ok_or_else(|| {
            ParserError::illegal_context(
                "dynamic variable select",
                format!(
                    "index dimension {dimension} is outside the {}-entry stride table",
                    geometry.strides.len()
                ),
                token,
            )
        })?;
        clamp |= slt_index_may_be_negative(arena, node, expression)
            || slt_index_exceeds(arena, node, dimension_width(dimension));
        if let Some(guard) =
            slt_index_guard(arena, node, expression, Some(dimension_width(dimension)))?
        {
            indices_valid = Some(and_slt_condition(arena, indices_valid, guard)?);
        }
        let kind = if dimension < array_dimension_count {
            SLTIndexKind::Unpacked {
                element_width: array_element_width.expect("unpacked array has an element width"),
            }
        } else {
            SLTIndexKind::Packed
        };
        indices.push(SLTIndex { node, stride, kind });
        let stride_node = arena.alloc(SLTNode::Constant(
            BigUint::from(stride),
            BigUint::from(0u8),
            64,
            false,
        ))?;
        let term = arena.alloc(SLTNode::Binary(node, BinaryOp::Mul, stride_node))?;
        offset = arena.alloc(SLTNode::Binary(offset, BinaryOp::Add, term))?;
    }

    let mut valid = indices_valid;
    let mut partial = None;
    if let Some(part) = geometry.part {
        let row_offset = offset;
        let stride = geometry
            .strides
            .get(geometry.dimension_count)
            .copied()
            .ok_or_else(|| {
                ParserError::illegal_context(
                    "dynamic variable select",
                    format!(
                        "part-select dimension {} is outside the {}-entry stride table",
                        geometry.dimension_count,
                        geometry.strides.len()
                    ),
                    token,
                )
            })?;
        let start = match part {
            PartSelectGeometry::Colon { lsb, .. } => arena.alloc(SLTNode::Constant(
                BigUint::from(lsb),
                BigUint::from(0u8),
                64,
                false,
            ))?,
            PartSelectGeometry::PlusColon { elements }
            | PartSelectGeometry::MinusColon { elements }
            | PartSelectGeometry::Step { elements } => {
                let anchor_expression = select.0.last().ok_or_else(|| {
                    ParserError::illegal_context(
                        "dynamic variable select",
                        "part select is missing its anchor expression",
                        token,
                    )
                })?;
                let ((anchor, anchor_sources), anchor_boundaries) =
                    expr::eval_expression_in_context(
                        module,
                        store,
                        anchor_expression,
                        arena,
                        None,
                    )?;
                sources.extend(anchor_sources);
                boundaries = merge_boundaries(boundaries, anchor_boundaries);
                let anchor_guard = slt_index_guard(arena, anchor, anchor_expression, None)?;
                if let Some(guard) = anchor_guard {
                    indices_valid = Some(and_slt_condition(arena, indices_valid, guard)?);
                }
                let width = dimension_width(geometry.dimension_count);
                // A possibly negative anchor takes the clamped partial path.
                let anchor_max = (!slt_index_may_be_negative(arena, anchor, anchor_expression))
                    .then(|| slt_max_value(arena, anchor).to_u64())
                    .flatten();
                // `position` is the lowest element plus `pad`; it is never
                // negative once the anchor is.
                let (start, position, pad, may_be_partial) = match part {
                    PartSelectGeometry::PlusColon { .. } => (
                        anchor,
                        anchor,
                        0,
                        anchor_max.is_none_or(|max| max + elements as u64 > width as u64),
                    ),
                    PartSelectGeometry::MinusColon { .. } => {
                        let decrement = elements.checked_sub(1).ok_or_else(|| {
                            ParserError::illegal_context(
                                "dynamic variable select",
                                "minus-colon width underflows",
                                token,
                            )
                        })?;
                        let decrement = arena.alloc(SLTNode::Constant(
                            BigUint::from(decrement),
                            BigUint::from(0u8),
                            64,
                            false,
                        ))?;
                        let start =
                            arena.alloc(SLTNode::Binary(anchor, BinaryOp::Sub, decrement))?;
                        (
                            start,
                            anchor,
                            elements - 1,
                            elements > 1 || anchor_max.is_none_or(|max| max >= width as u64),
                        )
                    }
                    PartSelectGeometry::Step { .. } => {
                        let elements_node = arena.alloc(SLTNode::Constant(
                            BigUint::from(elements),
                            BigUint::from(0u8),
                            64,
                            false,
                        ))?;
                        let start =
                            arena.alloc(SLTNode::Binary(anchor, BinaryOp::Mul, elements_node))?;
                        (
                            start,
                            start,
                            0,
                            anchor_max.is_none_or(|max| {
                                max.checked_add(1)
                                    .and_then(|count| count.checked_mul(elements as u64))
                                    .is_none_or(|end| end > width as u64)
                            }),
                        )
                    }
                    PartSelectGeometry::Colon { .. } => unreachable!("handled above"),
                };
                if may_be_partial {
                    // The whole selection is in range when its lowest element
                    // lies in `pad..=width - elements + pad`.
                    let mut whole = anchor_guard;
                    if pad > 0 {
                        let low = arena.alloc(SLTNode::Constant(
                            BigUint::from(pad),
                            BigUint::from(0u8),
                            64,
                            false,
                        ))?;
                        let above = arena.alloc(SLTNode::Binary(position, BinaryOp::GeU, low))?;
                        whole = Some(and_slt_condition(arena, whole, above)?);
                    }
                    let high = arena.alloc(SLTNode::Constant(
                        BigUint::from(width - elements + pad),
                        BigUint::from(0u8),
                        64,
                        false,
                    ))?;
                    let below = arena.alloc(SLTNode::Binary(position, BinaryOp::LeU, high))?;
                    whole = Some(and_slt_condition(arena, whole, below)?);
                    if let Some(whole) = whole {
                        valid = Some(and_slt_condition(arena, valid, whole)?);
                    }
                    partial = Some(DynamicPartSelect {
                        row_offset,
                        row_bits: width * stride,
                        position,
                        pad,
                        element_bits: stride,
                    });
                } else if let Some(guard) = anchor_guard {
                    valid = Some(and_slt_condition(arena, valid, guard)?);
                }
                start
            }
        };
        indices.push(SLTIndex {
            node: start,
            stride,
            kind: SLTIndexKind::Packed,
        });
        let stride_node = arena.alloc(SLTNode::Constant(
            BigUint::from(stride),
            BigUint::from(0u8),
            64,
            false,
        ))?;
        let term = arena.alloc(SLTNode::Binary(start, BinaryOp::Mul, stride_node))?;
        offset = arena.alloc(SLTNode::Binary(offset, BinaryOp::Add, term))?;
    }

    let prefix_access = eval_var_select_with_geometry(index, select, &geometry)?;
    Ok(DynamicSelectOffset {
        node: offset,
        indices,
        prefix_access,
        selected_width: geometry.selected_width,
        sources,
        boundaries,
        indices_valid,
        valid,
        clamp,
        partial,
    })
}

pub(super) fn eval_dynamic_assign(
    module: &Module,
    mut store: SymbolicStore<VarId>,
    mut boundaries: BoundaryMap<VarId>,
    dst: &veryl_analyzer::ir::AssignDestination,
    rhs_expr: NodeId,
    rhs_sources: HashSet<VarAtomBase<VarId>>,
    source_is_2state: bool,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(SymbolicStore<VarId>, BoundaryMap<VarId>), ParserError> {
    let mut all_sources = rhs_sources;
    let select_offset = {
        let mut expression_store = expr::ExpressionStore::Effectful(&mut store);
        eval_dynamic_select_offset(
            module,
            &mut expression_store,
            dst.id,
            &dst.index,
            &dst.select,
            arena,
            Some(&dst.token),
        )?
    };
    let access_width = select_offset.selected_width;
    let prefix_access = select_offset.prefix_access;
    boundaries = merge_boundaries(boundaries, select_offset.boundaries);
    all_sources.extend(select_offset.sources);
    let offset_node = select_offset.node;
    // An invalid index makes the write a no-op (IEEE 1800-2023 7.4.6); a
    // partly out-of-range part select writes only its in-range bits (11.5.1).
    let (write_condition, partial) = match select_offset.partial {
        Some(partial) => (select_offset.indices_valid, Some(partial)),
        None => (select_offset.valid, None),
    };

    let var = &module.variables[&dst.id];
    let width = resolve_total_width(module, var)?;
    if width == 0 || access_width == 0 || access_width > width {
        return Err(ParserError::illegal_context(
            "dynamic assignment",
            format!("destination width {access_width} must be in 1..={width}"),
            Some(&dst.token),
        ));
    }

    let access_full = BitAccess::new(0, width - 1);
    let range_store = store
        .entry(dst.id)
        .or_insert_with(|| RangeStore::new(None, width));

    // Evaluate the variable's current state.
    // Sub-ranges that haven't been assigned yet will fall back to their initial input state.
    let old_parts = range_store
        .get_parts_ref(access_full)
        .map_err(|error| range_store_error("dynamic assignment", error, Some(&dst.token)))?;
    let (old_val, old_sources) = combine_parts_with_default(dst.id, 0, old_parts, arena)?;
    // Note: Partial dynamic updates are not treated as self-dependencies (latches)
    // to maintain consistency with existing test expectations and Verilog semantics.
    for source in old_sources {
        if source.id != dst.id {
            all_sources.insert(source);
        }
    }

    // A `-:` part select may start below bit 0 of its row. Place it in a
    // value widened by `pad_bits` below bit 0, then drop those bits.
    let pad_bits = partial
        .as_ref()
        .map_or(0, |partial| partial.pad * partial.element_bits);
    let wide_width = width + pad_bits;
    let shift = if pad_bits == 0 {
        offset_node
    } else {
        let pad = arena.alloc(SLTNode::Constant(
            BigUint::from(pad_bits),
            BigUint::from(0u8),
            64,
            false,
        ))?;
        arena.alloc(SLTNode::Binary(offset_node, BinaryOp::Add, pad))?
    };
    // An invalid index shifts the write mask out of the variable, so the
    // update keeps every old bit without a select over the whole variable.
    let shift = match write_condition {
        Some(condition) => {
            let past_end = arena.alloc(SLTNode::Constant(
                BigUint::from(wide_width),
                BigUint::from(0u8),
                64,
                false,
            ))?;
            arena.alloc(SLTNode::Mux {
                cond: condition,
                then_expr: shift,
                else_expr: past_end,
            })?
        }
        None => shift,
    };
    let place = |arena: &mut SLTNodeArena<VarId>, value: NodeId| -> Result<NodeId, ParserError> {
        let shifted = arena.alloc(SLTNode::Binary(value, BinaryOp::Shl, shift))?;
        if pad_bits == 0 {
            Ok(shifted)
        } else {
            Ok(arena.alloc(SLTNode::Slice {
                expr: shifted,
                access: BitAccess::new(pad_bits, pad_bits + width - 1),
            })?)
        }
    };

    // Compute the bitmask to isolate the target range: mask = !(( (1<<access_width) - 1 ) << offset)
    let mask_base = (BigUint::from(1u32) << access_width) - BigUint::from(1u32);
    // Ensure width consistency; using the full variable width for safety.
    let mask_constant = arena.alloc(SLTNode::Constant(
        mask_base,
        BigUint::from(0u32),
        wide_width,
        false,
    ))?;

    let mut mask_shifted = place(arena, mask_constant)?;
    if let Some(partial) = &partial {
        // Bits past the end of the row belong to the next row.
        let row_ones = (BigUint::from(1u32) << partial.row_bits) - BigUint::from(1u32);
        let row_ones = arena.alloc(SLTNode::Constant(
            row_ones,
            BigUint::from(0u32),
            width,
            false,
        ))?;
        let row_mask = arena.alloc(SLTNode::Binary(row_ones, BinaryOp::Shl, partial.row_offset))?;
        mask_shifted = arena.alloc(SLTNode::Binary(mask_shifted, BinaryOp::And, row_mask))?;
    }
    let mask_node = arena.alloc(SLTNode::Unary(UnaryOp::BitNot, mask_shifted))?;

    // Apply assignment coercion before embedding the value in the full
    // destination.  Otherwise discarded high RHS bits can corrupt neighbours.
    let rhs_signed = expr::is_signed(module, rhs_expr, arena);
    let rhs_expr = coerce_node_width(arena, rhs_expr, Some(access_width), rhs_signed)?;
    let rhs_expr = if var.r#type.is_2state() && !source_is_2state {
        arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, rhs_expr))?
    } else {
        rhs_expr
    };
    let rhs_widened = if access_width < wide_width {
        let padding = wide_width - access_width;
        let zero = arena.alloc(SLTNode::Constant(
            BigUint::from(0u32),
            BigUint::from(0u32),
            padding,
            false,
        ))?;
        // Concatenate zero padding to match variable width: {padding'b0, rhs_expr}
        arena.alloc(SLTNode::Concat(vec![
            (zero, padding),
            (rhs_expr, access_width),
        ]))?
    } else {
        rhs_expr
    };
    let new_val_term = place(arena, rhs_widened)?;
    let new_val_term = arena.alloc(SLTNode::Binary(new_val_term, BinaryOp::And, mask_shifted))?;

    // Apply the update: final_val = (old_val & mask) | new_val_term
    let new_val_masked = arena.alloc(SLTNode::Binary(old_val, BinaryOp::And, mask_node))?;
    let final_val = arena.alloc(SLTNode::Binary(new_val_masked, BinaryOp::Or, new_val_term))?;

    let stored_expr = if prefix_access.lsb == 0 && prefix_access.msb == width - 1 {
        final_val
    } else {
        arena.alloc(SLTNode::Slice {
            expr: final_val,
            access: prefix_access,
        })?
    };
    range_store
        .update(prefix_access, Some((stored_expr, all_sources)))
        .map_err(|error| range_store_error("dynamic assignment", error, Some(&dst.token)))?;

    Ok((store, boundaries))
}
