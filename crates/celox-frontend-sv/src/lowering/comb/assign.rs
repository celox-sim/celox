//! Blocking assignments: the bits a target names and the store of a value
//! into them, also under a run-time position.

use super::*;

impl Comb<'_, '_> {
    pub(super) fn target_width(
        &mut self,
        store: &Store,
        lvalue: &sv::ir::LValue,
    ) -> Result<usize, sv::AnalyzerError> {
        Ok(match self.lvalue_target(store, lvalue)? {
            Target::Static { width, .. } | Target::Dynamic { width, .. } => width,
        })
    }

    /// The bits that continuous assignment `body` writes, for each part of
    /// a concatenated target too: a variable or a constant select of one
    /// writes its bits, a select at a run-time position any bit of its
    /// variable.
    pub fn continuous_target_bits(
        &mut self,
        body: &[sv::ir::Stmt],
    ) -> Option<Vec<(SourceVarId, BitAccess)>> {
        let parts = match body {
            [sv::ir::Stmt::Assign { lhs, .. }] => std::slice::from_ref(lhs),
            [sv::ir::Stmt::AssignConcat { parts, .. }] => parts.as_slice(),
            _ => return None,
        };
        parts
            .iter()
            .map(
                |lhs| match self.lvalue_target(&Store::default(), lhs).ok()? {
                    Target::Static { id, access, .. } => Some((id, access)),
                    Target::Dynamic { id, .. } => Some((id, full(self.m.var(id).width))),
                },
            )
            .collect()
    }

    pub(super) fn lvalue_target(
        &mut self,
        store: &Store,
        lvalue: &sv::ir::LValue,
    ) -> Result<Target, sv::AnalyzerError> {
        let lvalue = self.propagate_lvalue(store, lvalue);
        let id = self
            .m
            .id(lvalue.name())
            .ok_or_else(|| unsupported(format!("assignment target `{}`", lvalue.name())))?;
        let var_width = self.m.var(id).width;
        let constants = self.m.constants;
        let parameter_types = self.m.parameter_types;
        match &lvalue {
            sv::ir::LValue::Ident(_) => Ok(Target::Static {
                id,
                access: full(var_width),
                width: var_width,
                drop: 0,
                lvalue,
            }),
            sv::ir::LValue::Select { msb, lsb, .. } => {
                let msb_value =
                    sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types);
                let lsb_value =
                    sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types);
                if let (Some(msb), Some(lsb)) = (msb_value, lsb_value) {
                    let width = usize::try_from(msb.abs_diff(lsb)).unwrap_or(0) + 1;
                    let (access, drop) = constant_select_write(self.m.var(id), msb, lsb)
                        .ok_or_else(|| unsupported("assignment position"))?;
                    let Some(access) = access else {
                        // A constant select entirely outside the vector writes nothing.
                        return Ok(Target::Dynamic {
                            id,
                            offset: Offset::Nothing,
                            width,
                        });
                    };
                    return Ok(Target::Static {
                        id,
                        access,
                        width,
                        drop,
                        lvalue: lvalue.clone(),
                    });
                }
                if let Some((_, element_width, offset, access)) = dynamic_array_element_lvalue(
                    &lvalue,
                    self.m.variables,
                    self.m.name_to_id,
                    constants,
                    parameter_types,
                ) {
                    let element_count = var_width / element_width.max(1);
                    return Ok(Target::Dynamic {
                        id,
                        offset: Offset::Element {
                            offset,
                            element_width,
                            element_count,
                            access,
                        },
                        width: access.msb - access.lsb + 1,
                    });
                }
                if let Some(write) = dynamic_packed_write(
                    &lvalue,
                    self.m.variables,
                    self.m.name_to_id,
                    constants,
                    parameter_types,
                ) {
                    return Ok(Target::Dynamic {
                        id,
                        offset: Offset::Packed {
                            up: Box::new(write.up),
                            down: Box::new(write.down),
                            window: write.window,
                        },
                        width: write.select_width,
                    });
                }
                Err(unsupported(format!(
                    "assignment target `{}`",
                    lvalue.name()
                )))
            }
        }
    }

    /// Assign `rhs` to `lhs`.
    pub(super) fn assign(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        lhs: &sv::ir::LValue,
        rhs: &sv::ir::Expr,
    ) -> Result<(), sv::AnalyzerError> {
        let prepared_rhs = if self.m.calls(rhs) {
            Cow::Owned(self.hoist(store, frames, rhs)?)
        } else {
            Cow::Borrowed(rhs)
        };
        let rhs = prepared_rhs.as_ref();
        let prepared_lhs = if self.m.lvalue_calls(lhs) {
            Cow::Owned(self.hoist_lvalue(store, frames, lhs)?)
        } else {
            Cow::Borrowed(lhs)
        };
        let lhs = prepared_lhs.as_ref();
        let target = self.lvalue_target(store, lhs)?;
        let width = match &target {
            Target::Static { width, .. } | Target::Dynamic { width, .. } => *width,
        };
        if self.continuous
            && matches!(
                &target,
                Target::Dynamic { id, offset, .. }
                    if !self.m.is_hidden(*id) && !matches!(offset, Offset::Nothing)
            )
        {
            return Err(unsupported(format!(
                "combinational assignment target `{}` at a run-time position",
                lhs.name()
            )));
        }
        let signed = self.expr_signed(rhs);
        let from = floating::precision(rhs, self.m.variables, self.m.name_to_id);
        let target_var = self.m.id(lhs.name()).map(|id| self.m.var(id));
        let to = target_var.and_then(|v| v.float);
        let target_signed = target_var.is_some_and(|v| v.signed);
        let (mut node, sources) = self
            .eval(
                store,
                frames,
                rhs,
                if from.is_some() || to.is_some() {
                    None
                } else {
                    Some((width, signed))
                },
            )
            .map_err(|error| match error {
                sv::AnalyzerError::Unsupported(construct)
                    if construct == "combinational expression" =>
                {
                    unsupported(format!(
                        "combinational expression assigned to `{}`",
                        lhs.name()
                    ))
                }
                error => error,
            })?;
        node = floating::convert(
            self.arena,
            node,
            from,
            to,
            width,
            if from.is_none() {
                signed
            } else {
                target_signed
            },
            true,
        )
        .ok_or_else(|| unsupported("real assignment conversion"))?;
        self.store_value(store, frames, target, (node, sources), signed, rhs)
    }

    pub(super) fn store_value(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        target: Target,
        value: Value,
        signed: bool,
        rhs: &sv::ir::Expr,
    ) -> Result<(), sv::AnalyzerError> {
        let (id, width) = match &target {
            Target::Static { id, width, .. } | Target::Dynamic { id, width, .. } => (*id, *width),
        };
        let (mut node, mut sources) = value;
        node = coerce_node_width(self.arena, node, Some(width), signed).map_err(slt_error)?;
        let target_two_state = !self.m.var(id).is_4state;
        if target_two_state || (!self.m.four_state && expr_is_unknown_literal(rhs)) {
            node = self.alloc(SLTNode::Unary(UnaryOp::ToTwoState, node))?;
        }
        match target {
            Target::Static {
                id,
                access,
                drop,
                lvalue,
                ..
            } => {
                let mut node = permute_reversed_lvalue_rhs_slt(
                    &lvalue,
                    node,
                    width,
                    self.m.constants,
                    self.m.parameter_types,
                    self.arena,
                )
                .ok_or_else(|| unsupported("assignment lvalue order"))?;
                let written = access.msb - access.lsb + 1;
                if written != width {
                    node = self.alloc(SLTNode::Slice {
                        expr: node,
                        access: BitAccess::new(drop, drop + written - 1),
                    })?;
                }
                self.write(store, id, access, (node, sources))
            }
            Target::Dynamic { id, offset, width } => {
                let var_width = self.m.var(id).width;
                let (current, current_sources) = self.read(store, id, full(var_width))?;
                // A partial dynamic update keeps the other bits; they are not a
                // self-dependency of the variable.
                sources.extend(current_sources.into_iter().filter(|source| source.id != id));
                let mut window = None;
                let (up, down) = match offset {
                    Offset::Nothing => return Ok(()),
                    Offset::Element {
                        offset,
                        element_width,
                        element_count,
                        access,
                    } => {
                        let offset_expr = expr_from_select_position(
                            &offset,
                            self.m.constants,
                            self.m.parameter_types,
                        )
                        .ok_or_else(|| unsupported("dynamic assignment index"))?;
                        let (bit_offset, offset_sources) =
                            self.eval(store, frames, &offset_expr, None)?;
                        sources.extend(offset_sources);
                        let wide = 64.max(var_width);
                        let bit_offset = coerce_node_width(self.arena, bit_offset, Some(64), false)
                            .map_err(slt_error)?;
                        // The element index; an unknown or out-of-range one writes nothing.
                        let stride = self.constant(element_width as u64, 64)?;
                        let index =
                            self.alloc(SLTNode::Binary(bit_offset, BinaryOp::DivU, stride))?;
                        let (_, valid) =
                            dynamic_array_index_guard_slt(self.arena, index, element_count)
                                .ok_or_else(|| unsupported("dynamic assignment index"))?;
                        let element_base =
                            self.alloc(SLTNode::Binary(index, BinaryOp::Mul, stride))?;
                        let lsb = self.constant(access.lsb as u64, 64)?;
                        let position =
                            self.alloc(SLTNode::Binary(element_base, BinaryOp::Add, lsb))?;
                        let outside = self.constant(var_width as u64, 64)?;
                        let position = self.alloc(SLTNode::Mux {
                            cond: valid,
                            then_expr: position,
                            else_expr: outside,
                        })?;
                        let position = coerce_node_width(self.arena, position, Some(wide), false)
                            .map_err(slt_error)?;
                        let zero = self.constant(0, wide)?;
                        (position, zero)
                    }
                    Offset::Packed {
                        up,
                        down,
                        window: packed_window,
                    } => {
                        window = packed_window;
                        let (up, up_sources) = self.eval(store, frames, &up, None)?;
                        let (down, down_sources) = self.eval(store, frames, &down, None)?;
                        sources.extend(up_sources);
                        sources.extend(down_sources);
                        (up, down)
                    }
                };
                let wide = var_width.max(self.width(up)).max(self.width(down));
                let mask_value = (BigUint::from(1u8) << width) - BigUint::from(1u8);
                let mask = slt_constant(self.arena, mask_value, wide, false)?;
                let value =
                    coerce_node_width(self.arena, node, Some(wide), false).map_err(slt_error)?;
                let current =
                    coerce_node_width(self.arena, current, Some(wide), false).map_err(slt_error)?;
                let up = coerce_node_width(self.arena, up, Some(wide), false).map_err(slt_error)?;
                let down =
                    coerce_node_width(self.arena, down, Some(wide), false).map_err(slt_error)?;
                let place = |this: &mut Self, value| -> Result<NodeId, sv::AnalyzerError> {
                    let raised = this.alloc(SLTNode::Binary(value, BinaryOp::Shl, up))?;
                    this.alloc(SLTNode::Binary(raised, BinaryOp::Shr, down))
                };
                let placed_mask = place(self, mask)?;
                let mut updated = if self.m.four_state && self.m.var(id).is_4state {
                    masked::dynamic_four_state(self.arena, current, value, placed_mask, up, down)?
                } else {
                    let placed_value = place(self, value)?;
                    let placed_value =
                        self.alloc(SLTNode::Binary(placed_value, BinaryOp::And, placed_mask))?;
                    let keep = self.alloc(SLTNode::Unary(UnaryOp::BitNot, placed_mask))?;
                    let kept = self.alloc(SLTNode::Binary(current, BinaryOp::And, keep))?;
                    self.alloc(SLTNode::Binary(kept, BinaryOp::Or, placed_value))?
                };
                if self.m.four_state {
                    // A write at an unknown position is ignored (IEEE 1800-2023 11.5.1).
                    let mut known = None;
                    for position in [up, down] {
                        let two_state =
                            self.alloc(SLTNode::Unary(UnaryOp::ToTwoState, position))?;
                        let same =
                            self.alloc(SLTNode::Binary(position, BinaryOp::EqCase, two_state))?;
                        known = Some(match known {
                            None => same,
                            Some(other) => slt_and(self.arena, &mut self.consts, other, same)?,
                        });
                    }
                    if let Some(known) = known
                        && slt_bool(self.arena, &mut self.consts, known) != Some(true)
                    {
                        updated = self.alloc(SLTNode::Mux {
                            cond: known,
                            then_expr: updated,
                            else_expr: current,
                        })?;
                    }
                }
                let written = window.unwrap_or(full(var_width));
                let updated = self.slice(updated, written)?;
                self.write(store, id, written, (updated, sources))
            }
        }
    }

    pub(super) fn assign_concat(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        parts: &[sv::ir::LValue],
        rhs: &sv::ir::Expr,
    ) -> Result<(), sv::AnalyzerError> {
        // The positions of all parts are evaluated before any part is written.
        let mut targets = Vec::with_capacity(parts.len());
        let mut widths = Vec::with_capacity(parts.len());
        for part in parts {
            let part = self.hoist_lvalue(store, frames, part)?;
            let part = if parts.len() > 1 {
                self.freeze_lvalue(store, frames, part)?
            } else {
                part
            };
            widths.push(self.target_width(store, &part)?);
            targets.push(self.lvalue_target(store, &part)?);
        }
        let total: usize = widths.iter().sum();
        let signed = self.expr_signed(rhs);
        let (node, sources) = self.eval(store, frames, rhs, Some((total, signed)))?;
        let node = coerce_node_width(self.arena, node, Some(total), signed).map_err(slt_error)?;
        let mut lsb = total;
        for (target, width) in targets.into_iter().zip(widths) {
            lsb -= width;
            let slice = self.slice(node, BitAccess::new(lsb, lsb + width - 1))?;
            self.store_value(store, frames, target, (slice, sources.clone()), false, rhs)?;
        }
        Ok(())
    }

    /// Write `value` (already evaluated) to an output-argument lvalue.
    pub(super) fn write_back(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        lvalues: &[sv::ir::LValue],
        value: Value,
        value_signed: bool,
    ) -> Result<(), sv::AnalyzerError> {
        // The positions of all parts are evaluated, at copy-out, before any
        // part is written.
        let mut targets = Vec::with_capacity(lvalues.len());
        let mut widths = Vec::with_capacity(lvalues.len());
        for part in lvalues {
            let part = self.hoist_lvalue(store, frames, part)?;
            let part = if lvalues.len() > 1 {
                self.freeze_lvalue(store, frames, part)?
            } else {
                part
            };
            widths.push(self.target_width(store, &part)?);
            targets.push(self.lvalue_target(store, &part)?);
        }
        let total: usize = widths.iter().sum();
        let node =
            coerce_node_width(self.arena, value.0, Some(total), value_signed).map_err(slt_error)?;
        let mut lsb = total;
        let placeholder = sv::ir::Expr::Literal("0".to_string());
        for (target, width) in targets.into_iter().zip(widths) {
            lsb -= width;
            let slice = self.slice(node, BitAccess::new(lsb, lsb + width - 1))?;
            self.store_value(
                store,
                frames,
                target,
                (slice, value.1.clone()),
                value_signed,
                &placeholder,
            )?;
        }
        Ok(())
    }
}

pub(super) enum Target {
    /// A constant select: the select's `width` bits of the value from
    /// `drop` up go to the bits `access` of the variable, the part of the
    /// select inside it.
    Static {
        id: SourceVarId,
        access: BitAccess,
        width: usize,
        drop: usize,
        lvalue: sv::ir::LValue,
    },
    Dynamic {
        id: SourceVarId,
        offset: Offset,
        width: usize,
    },
}

pub(super) enum Offset {
    /// A constant select outside the vector.
    Nothing,
    /// An element of an unpacked array at a run-time index.
    Element {
        offset: sv::ir::ConstExpr,
        element_width: usize,
        element_count: usize,
        access: BitAccess,
    },
    /// A run-time position in a packed vector; see [`RuntimePosition`].
    /// With a window, only those bits are written.
    Packed {
        up: Box<sv::ir::Expr>,
        down: Box<sv::ir::Expr>,
        window: Option<BitAccess>,
    },
}
