//! Blocking assignments: stores into the region a variable's writes go to,
//! whole, at a constant range or at a run-time element of an unpacked array.

use super::*;

/// Read-modify-write views of a variable's working value.
pub(super) struct Aliases {
    /// alias -> var.
    pub(super) map: HashMap<SourceVarId, SourceVarId>,
    pub(super) next: u32,
}

impl Aliases {
    /// No view yet; the ids of the aliases count down from `u32::MAX`.
    pub(super) fn new() -> Self {
        Self {
            map: HashMap::default(),
            next: u32::MAX,
        }
    }
}

/// The blocking assignments a process kernel publishes to its waits before
/// the next statement (see [`Ff::observe_assignment`]), and the writes it
/// could not publish.
#[derive(Default)]
pub(super) struct Observation {
    pub(super) unobserved_function_writes: HashSet<SourceVarId>,
    pub(super) observe_blocking_assignments: bool,
    pub(super) implicit_write_depth: usize,
}

impl Ff<'_, '_> {
    /// The store `store` of a place, a class property or a container
    /// element, published as a store of a variable is (see
    /// [`Self::observe_assignment`]): every write of a place, by an
    /// assignment, an output copy-out or a scan, goes through here. A store
    /// to an element of a container variable is a write of the variable; a
    /// property of an object is no variable a wait can watch.
    pub(super) fn store_place(&mut self, store: &sv::ir::Expr) -> Result<(), sv::AnalyzerError> {
        self.eval(store, None)?;
        if let Some(target) = place_store_target(store) {
            self.observe_assignment(&[sv::ir::LValue::Ident(target)])?;
        }
        Ok(())
    }

    pub(super) fn store(
        &mut self,
        id: SourceVarId,
        offset: SIROffset,
        width: usize,
        value: RegisterId,
    ) {
        // Intrinsic and subroutine expression copy-outs, including seed and
        // plusargs actuals, bypass statement lowering. They cannot introduce
        // observation yields while caller registers are live. Record the
        // resolved destination here so a later probe cannot silently miss it.
        if self.observation.observe_blocking_assignments
            && self.kernel.is_some()
            && (self.observation.implicit_write_depth != 0
                || self.call_stack.active_calls.iter().any(|name| {
                    self.m
                        .subroutine(name)
                        .is_some_and(|subroutine| !subroutine.is_task)
                }))
        {
            self.observation.unobserved_function_writes.insert(id);
        }
        let value = if let Some((enabled, forced)) = self.m.force_controls(id) {
            let mut condition = self.b.alloc_bit(1, false);
            self.b.emit(SIRInstruction::Load(
                condition,
                stable(enabled),
                SIROffset::Static(0),
                1,
            ));
            // A released target holds the forced value until the procedure
            // writing it executes again (IEEE 1800-2023 10.6.2); this is
            // that procedure, but its write before the hold ends is not.
            if let Some(&hold) = self.m.force_holds.get(&id) {
                let held = self.b.alloc_bit(1, false);
                self.b.emit(SIRInstruction::Load(
                    held,
                    stable(hold),
                    SIROffset::Static(0),
                    1,
                ));
                let either = self.b.alloc_bit(1, false);
                self.b.emit(SIRInstruction::Binary(
                    either,
                    condition,
                    BinaryOp::Or,
                    held,
                ));
                condition = either;
            }
            let replacement = self.b.alloc_reg(self.b.register(&value).clone());
            self.b.emit(SIRInstruction::Load(
                replacement,
                stable(forced),
                offset.clone(),
                width,
            ));
            let selected = self.b.alloc_reg(self.b.register(&value).clone());
            self.b
                .emit(SIRInstruction::Mux(selected, condition, replacement, value));
            selected
        } else {
            value
        };
        let target = self.target(id);
        self.b.emit(SIRInstruction::Store(
            target,
            offset,
            width,
            value,
            Vec::new(),
            Vec::new(),
        ));
    }

    pub(super) fn alias(&mut self, id: SourceVarId) -> SourceVarId {
        let alias = SourceVarId(self.aliases.next);
        self.aliases.next -= 1;
        self.aliases.map.insert(alias, id);
        alias
    }

    pub(super) fn default_reg(&mut self, id: SourceVarId) -> RegisterId {
        let var = self.m.var(id);
        let width = var.width;
        let unknown = var.is_4state && self.m.four_state;
        let reg = if unknown {
            self.b.alloc_logic(width)
        } else {
            self.b.alloc_bit(width, false)
        };
        let mask = if unknown {
            (BigUint::from(1u8) << width) - BigUint::from(1u8)
        } else {
            BigUint::zero()
        };
        self.b.emit(SIRInstruction::Imm(
            reg,
            SIRValue::new_four_state(BigUint::zero(), mask),
        ));
        reg
    }

    pub(super) fn init_default(&mut self, id: SourceVarId) {
        let width = self.m.var(id).width;
        let value = self.default_reg(id);
        let offset = sv_memory_offset(self.m.var(id), 0, width);
        self.store(id, offset, width, value);
    }

    pub(super) fn assign(
        &mut self,
        lhs: &sv::ir::LValue,
        rhs: &sv::ir::Expr,
    ) -> Result<(), sv::AnalyzerError> {
        self.assign_value(lhs, rhs, true)
    }

    /// Assign `rhs` to `lhs`; `convert` turns an integral value assigned to
    /// a string variable into its bytes. The parts of a concatenation target
    /// take the bits of the value instead: a part that is a string is a
    /// member of an unpacked structure, and its bits are a handle.
    pub(super) fn assign_value(
        &mut self,
        lhs: &sv::ir::LValue,
        rhs: &sv::ir::Expr,
        convert: bool,
    ) -> Result<(), sv::AnalyzerError> {
        let lhs = self.propagate_lvalue(lhs);
        let lhs = self.hoist_lvalue(&lhs)?;
        let converted = match self.m.id(lhs.name()) {
            Some(id) if convert && matches!(lhs, sv::ir::LValue::Ident(_)) => {
                self.string_conversion(id, rhs.clone())
            }
            _ => rhs.clone(),
        };
        let rhs = &converted;
        let owned_rhs = if self.m.calls(rhs) {
            self.hoist(rhs)?
        } else {
            rhs.clone()
        };
        let rhs = &owned_rhs;
        let width = self.lvalue_width(&lhs)?;
        let signed = self.expr_signed(rhs);
        let mut arena = SLTNodeArena::new();
        let from = floating::precision(rhs, self.m.variables, self.m.name_to_id);
        let target_var = self.m.id(lhs.name()).map(|id| self.m.var(id));
        let to = target_var.and_then(|v| v.float);
        let target_signed = target_var.is_some_and(|v| v.signed);
        let node = self.expr_slt(
            rhs,
            if from.is_some() || to.is_some() {
                None
            } else {
                Some((width, signed))
            },
            &mut arena,
        )?;
        let node = floating::convert(
            &mut arena,
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
        self.write(&lhs, &mut arena, node, signed, rhs)
    }

    pub(super) fn lvalue_width(&self, lvalue: &sv::ir::LValue) -> Result<usize, sv::AnalyzerError> {
        let id = self
            .m
            .id(lvalue.name())
            .ok_or_else(|| unsupported(format!("assignment target `{}`", lvalue.name())))?;
        let variable = self.m.var(id);
        let constants = self.m.constants;
        let parameter_types = self.m.parameter_types;
        match lvalue {
            sv::ir::LValue::Ident(_) => Ok(variable.width),
            sv::ir::LValue::Select { msb, lsb, .. } => {
                if let (Some(msb), Some(lsb)) = (
                    sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types),
                    sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types),
                ) {
                    return Ok(usize::try_from(msb.abs_diff(lsb)).unwrap_or(0) + 1);
                }
                if let Some((_, _, _, access)) = dynamic_array_element_lvalue(
                    lvalue,
                    self.m.variables,
                    self.m.name_to_id,
                    constants,
                    parameter_types,
                ) {
                    return Ok(access.msb - access.lsb + 1);
                }
                if let Some(write) = dynamic_packed_write(
                    lvalue,
                    self.m.variables,
                    self.m.name_to_id,
                    constants,
                    parameter_types,
                ) {
                    return Ok(write.select_width);
                }
                Err(unsupported(format!(
                    "assignment target `{}`",
                    lvalue.name()
                )))
            }
        }
    }

    /// Store the SLT value `node` into `lhs`.
    pub(super) fn write(
        &mut self,
        lhs: &sv::ir::LValue,
        arena: &mut SLTNodeArena<SourceVarId>,
        node: NodeId,
        signed: bool,
        rhs: &sv::ir::Expr,
    ) -> Result<(), sv::AnalyzerError> {
        let id = self
            .m
            .id(lhs.name())
            .ok_or_else(|| unsupported(format!("assignment target `{}`", lhs.name())))?;
        let width = self.lvalue_width(lhs)?;
        let mut node = coerce_node_width(arena, node, Some(width), signed).map_err(slt_error)?;
        let (var_width, is_4state) = {
            let var = self.m.var(id);
            (var.width, var.is_4state)
        };
        if !is_4state || (!self.m.four_state && expr_is_unknown_literal(rhs)) {
            node = arena
                .alloc(SLTNode::Unary(UnaryOp::ToTwoState, node))
                .map_err(slt_error)?;
        }
        let constants = self.m.constants;
        let parameter_types = self.m.parameter_types;
        match lhs {
            sv::ir::LValue::Ident(_) => {
                let value = self.lower_slt(arena, node)?;
                let offset = sv_memory_offset(self.m.var(id), 0, var_width);
                self.store(id, offset, var_width, value);
                Ok(())
            }
            sv::ir::LValue::Select { msb, lsb, .. } => {
                if let (Some(msb), Some(lsb)) = (
                    sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types),
                    sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types),
                ) {
                    let (access, drop) = constant_select_write(self.m.var(id), msb, lsb)
                        .ok_or_else(|| unsupported("assignment position"))?;
                    let Some(access) = access else {
                        return Ok(());
                    };
                    let mut node = permute_reversed_lvalue_rhs_slt(
                        lhs,
                        node,
                        width,
                        constants,
                        parameter_types,
                        arena,
                    )
                    .ok_or_else(|| unsupported("assignment lvalue order"))?;
                    let written = access.msb - access.lsb + 1;
                    if written != width {
                        node = arena
                            .alloc(SLTNode::Slice {
                                expr: node,
                                access: BitAccess::new(drop, drop + written - 1),
                            })
                            .map_err(slt_error)?;
                    }
                    let value = self.lower_slt(arena, node)?;
                    let offset = sv_memory_offset(self.m.var(id), access.lsb, written);
                    self.store(id, offset, written, value);
                    return Ok(());
                }
                if let Some((_, element_width, offset, access)) = dynamic_array_element_lvalue(
                    lhs,
                    self.m.variables,
                    self.m.name_to_id,
                    constants,
                    parameter_types,
                ) {
                    return self.write_element(id, element_width, &offset, access, arena, node);
                }
                if let Some(write) = dynamic_packed_write(
                    lhs,
                    self.m.variables,
                    self.m.name_to_id,
                    constants,
                    parameter_types,
                ) {
                    let (up, _) = lower_expr_with_context(
                        &self.propagate(&write.up),
                        self.m.variables,
                        self.m.name_to_id,
                        constants,
                        parameter_types,
                        arena,
                        None,
                        Some(false),
                    )
                    .ok_or_else(|| unsupported("dynamic assignment position"))?;
                    let (down, _) = lower_expr_with_context(
                        &self.propagate(&write.down),
                        self.m.variables,
                        self.m.name_to_id,
                        constants,
                        parameter_types,
                        arena,
                        None,
                        Some(false),
                    )
                    .ok_or_else(|| unsupported("dynamic assignment position"))?;
                    // A select within one array element reads and writes only
                    // that element, which is all its process commits.
                    let window = write
                        .window
                        .unwrap_or_else(|| BitAccess::new(0, var_width - 1));
                    let alias = self.alias(id);
                    let current = arena
                        .alloc(SLTNode::Input {
                            variable: alias,
                            signed: false,
                            index: Vec::new(),
                            access: window,
                        })
                        .map_err(slt_error)?;
                    let wide = var_width
                        .max(celox_slt::get_width(up, arena))
                        .max(celox_slt::get_width(down, arena));
                    let mask = slt_constant(
                        arena,
                        (BigUint::from(1u8) << width) - BigUint::from(1u8),
                        wide,
                        false,
                    )?;
                    let coerce = |arena: &mut SLTNodeArena<SourceVarId>, node| {
                        coerce_node_width(arena, node, Some(wide), false).map_err(slt_error)
                    };
                    let value = coerce(arena, node)?;
                    let current = coerce(arena, current)?;
                    let current = if window.lsb == 0 {
                        current
                    } else {
                        let bottom = slt_constant(arena, BigUint::from(window.lsb), wide, false)?;
                        arena
                            .alloc(SLTNode::Binary(current, BinaryOp::Shl, bottom))
                            .map_err(slt_error)?
                    };
                    let up = coerce(arena, up)?;
                    let down = coerce(arena, down)?;
                    let place = |arena: &mut SLTNodeArena<SourceVarId>,
                                 value|
                     -> Result<NodeId, sv::AnalyzerError> {
                        let raised = arena
                            .alloc(SLTNode::Binary(value, BinaryOp::Shl, up))
                            .map_err(slt_error)?;
                        arena
                            .alloc(SLTNode::Binary(raised, BinaryOp::Shr, down))
                            .map_err(slt_error)
                    };
                    let placed_mask = place(arena, mask)?;
                    let mut updated = if self.m.four_state && self.m.var(id).is_4state {
                        masked::dynamic_four_state(arena, current, value, placed_mask, up, down)?
                    } else {
                        let placed_value = place(arena, value)?;
                        let placed_value = arena
                            .alloc(SLTNode::Binary(placed_value, BinaryOp::And, placed_mask))
                            .map_err(slt_error)?;
                        let keep = arena
                            .alloc(SLTNode::Unary(UnaryOp::BitNot, placed_mask))
                            .map_err(slt_error)?;
                        let kept = arena
                            .alloc(SLTNode::Binary(current, BinaryOp::And, keep))
                            .map_err(slt_error)?;
                        arena
                            .alloc(SLTNode::Binary(kept, BinaryOp::Or, placed_value))
                            .map_err(slt_error)?
                    };
                    if self.m.four_state {
                        // A write at an unknown position is ignored (IEEE 1800-2023 11.5.1).
                        let mut known = None;
                        for position in [up, down] {
                            let two_state = arena
                                .alloc(SLTNode::Unary(UnaryOp::ToTwoState, position))
                                .map_err(slt_error)?;
                            let same = arena
                                .alloc(SLTNode::Binary(position, BinaryOp::EqCase, two_state))
                                .map_err(slt_error)?;
                            known = Some(match known {
                                None => same,
                                Some(other) => {
                                    slt_and(arena, &mut ConstCache::default(), other, same)?
                                }
                            });
                        }
                        if let Some(known) = known {
                            updated = arena
                                .alloc(SLTNode::Mux {
                                    cond: known,
                                    then_expr: updated,
                                    else_expr: current,
                                })
                                .map_err(slt_error)?;
                        }
                    }
                    let window_width = window.msb - window.lsb + 1;
                    let updated = if window.lsb == 0 && window_width == wide {
                        updated
                    } else {
                        arena
                            .alloc(SLTNode::Slice {
                                expr: updated,
                                access: window,
                            })
                            .map_err(slt_error)?
                    };
                    let value = self.lower_slt(arena, updated)?;
                    let offset = sv_memory_offset(self.m.var(id), window.lsb, window_width);
                    self.store(id, offset, window_width, value);
                    return Ok(());
                }
                Err(unsupported(format!("assignment target `{}`", lhs.name())))
            }
        }
    }

    /// Write one element (or a part of it) of an unpacked array at a run-time
    /// index. An unknown or out-of-range index writes nothing.
    pub(super) fn write_element(
        &mut self,
        id: SourceVarId,
        element_width: usize,
        offset: &sv::ir::ConstExpr,
        access: BitAccess,
        arena: &mut SLTNodeArena<SourceVarId>,
        node: NodeId,
    ) -> Result<(), sv::AnalyzerError> {
        let target_width = access.msb - access.lsb + 1;
        let offset =
            expr_from_const_expr(offset).ok_or_else(|| unsupported("dynamic assignment index"))?;
        let offset = self.propagate(&offset);
        let (bit_offset, _) = lower_expr_with_context(
            &offset,
            self.m.variables,
            self.m.name_to_id,
            self.m.constants,
            self.m.parameter_types,
            arena,
            None,
            None,
        )
        .ok_or_else(|| unsupported("dynamic assignment index"))?;
        let bit_offset =
            coerce_node_width(arena, bit_offset, Some(64), false).map_err(slt_error)?;
        let stride = slt_constant(arena, BigUint::from(element_width), 64, false)?;
        let index = arena
            .alloc(SLTNode::Binary(bit_offset, BinaryOp::DivU, stride))
            .map_err(slt_error)?;
        let element_count = self.m.var(id).width / element_width.max(1);
        let (index, valid) = dynamic_array_index_guard_slt(arena, index, element_count)
            .ok_or_else(|| unsupported("dynamic assignment index"))?;
        let packed_element_width = unpacked_element_width(self.m.var(id))
            .ok_or_else(|| unsupported("dynamic assignment"))?;
        let index = self.lower_slt(arena, index)?;
        let valid = self.lower_slt(arena, valid)?;
        let value = self.lower_slt(arena, node)?;
        if element_width != packed_element_width {
            // A whole multi-dimensional element: one store per inner element.
            if access.lsb != 0 || target_width != element_width {
                return Err(unsupported(
                    "partial write of a multi-dimensional array element",
                ));
            }
            let inner_count = element_width / packed_element_width;
            let inner = self.b.alloc_bit(64, false);
            self.b.emit(SIRInstruction::Imm(
                inner,
                SIRValue::new(inner_count as u64),
            ));
            let scaled = self.b.alloc_bit(64, false);
            self.b
                .emit(SIRInstruction::Binary(scaled, index, BinaryOp::Mul, inner));
            for inner_index in 0..inner_count {
                let element_index = if inner_index == 0 {
                    scaled
                } else {
                    let step = self.b.alloc_bit(64, false);
                    self.b
                        .emit(SIRInstruction::Imm(step, SIRValue::new(inner_index as u64)));
                    let element_index = self.b.alloc_bit(64, false);
                    self.b.emit(SIRInstruction::Binary(
                        element_index,
                        scaled,
                        BinaryOp::Add,
                        step,
                    ));
                    element_index
                };
                let offset = SIROffset::Element {
                    index: element_index,
                    element_width: packed_element_width,
                    bit_offset: 0,
                    dynamic_bit_offset: None,
                };
                if self.writes.sparse_memories.contains(&id) {
                    let part = self.b.alloc_logic(packed_element_width);
                    self.b.emit(SIRInstruction::Slice(
                        part,
                        value,
                        inner_index * packed_element_width,
                        packed_element_width,
                    ));
                    self.when(valid, |this| {
                        this.store(id, offset, packed_element_width, part);
                        Ok(())
                    })?;
                    continue;
                }
                let old = self.b.alloc_logic(packed_element_width);
                let target = self.target(id);
                self.b.emit(SIRInstruction::Load(
                    old,
                    target,
                    offset.clone(),
                    packed_element_width,
                ));
                let part = self.b.alloc_logic(packed_element_width);
                self.b.emit(SIRInstruction::Slice(
                    part,
                    value,
                    inner_index * packed_element_width,
                    packed_element_width,
                ));
                let stored = self.b.alloc_logic(packed_element_width);
                self.b.emit(SIRInstruction::Mux(stored, valid, part, old));
                self.store(id, offset, packed_element_width, stored);
            }
            return Ok(());
        }
        let offset = SIROffset::Element {
            index,
            element_width,
            bit_offset: access.lsb,
            dynamic_bit_offset: None,
        };
        if self.writes.sparse_memories.contains(&id) {
            return self.when(valid, |this| {
                this.store(id, offset, target_width, value);
                Ok(())
            });
        }
        let old = self.b.alloc_logic(target_width);
        let target = self.target(id);
        self.b.emit(SIRInstruction::Load(
            old,
            target,
            offset.clone(),
            target_width,
        ));
        let stored = self.b.alloc_logic(target_width);
        self.b.emit(SIRInstruction::Mux(stored, valid, value, old));
        self.store(id, offset, target_width, stored);
        Ok(())
    }

    pub(super) fn assign_concat(
        &mut self,
        parts: &[sv::ir::LValue],
        rhs: &sv::ir::Expr,
    ) -> Result<(), sv::AnalyzerError> {
        // The positions of the parts are evaluated before any part is written.
        let mut evaluated = Vec::with_capacity(parts.len());
        for part in parts {
            let part = self.propagate_lvalue(part);
            let part = self.hoist_lvalue(&part)?;
            evaluated.push(if parts.len() > 1 {
                self.freeze_lvalue(part)?
            } else {
                part
            });
        }
        let parts = evaluated;
        let widths = parts
            .iter()
            .map(|part| self.lvalue_width(part))
            .collect::<Result<Vec<_>, _>>()?;
        let total: usize = widths.iter().sum();
        let signed = self.expr_signed(rhs);
        let mut arena = SLTNodeArena::new();
        let node = self.expr_slt(rhs, Some((total, signed)), &mut arena)?;
        let node = coerce_node_width(&mut arena, node, Some(total), signed).map_err(slt_error)?;
        // Capture the value before any part is written, in a temporary of
        // the value's state kind.
        let value = self.lower_slt(&arena, node)?;
        let four_state = matches!(self.b.register(&value), RegisterType::Logic { .. });
        let (temp, temp_name) = self.m.temp("concat", total, false, four_state);
        self.store(temp, SIROffset::Static(0), total, value);
        let mut lsb = total;
        for (part, width) in parts.iter().zip(widths) {
            lsb -= width;
            let slice = sv::ir::Expr::Select {
                expr: Box::new(sv::ir::Expr::Ident(temp_name.clone())),
                msb: sv::ir::ConstExpr::Literal((lsb + width - 1).to_string()),
                lsb: sv::ir::ConstExpr::Literal(lsb.to_string()),
                signed: false,
            };
            self.assign_value(part, &slice, false)?;
        }
        Ok(())
    }

    /// Publish each blocking assignment before the next statement, without
    /// switching the writer or releasing deferred join_none children.
    pub(super) fn observe_assignment(
        &mut self,
        targets: &[sv::ir::LValue],
    ) -> Result<(), sv::AnalyzerError> {
        if !self.activation.forked
            || !self.observation.observe_blocking_assignments
            || self.kernel.is_none()
        {
            return Ok(());
        }
        let in_function = self.call_stack.active_calls.iter().any(|name| {
            self.m
                .subroutine(name)
                .is_some_and(|subroutine| !subroutine.is_task)
        });
        if in_function || self.observation.implicit_write_depth != 0 {
            self.observation
                .unobserved_function_writes
                .extend(targets.iter().filter_map(|target| self.m.id(target.name())));
        } else {
            self.with_kernel(ProcessKernelBuilder::yield_now)
                .map_err(kernel_error)?;
        }
        Ok(())
    }
}

/// The store that copies `value` back into the class property that `actual`
/// reads through the object of a reference formal (see
/// [`references::Alias`]), when it is such a read.
/// The container variable that place store `store` writes, a container
/// element set on a variable, if it is one.
pub(super) fn place_store_target(store: &sv::ir::Expr) -> Option<String> {
    match store {
        sv::ir::Expr::Call {
            callee: sv::ir::Callee::Object(op),
            args,
            ..
        } if matches!(
            **op,
            sv::ir::ObjectOp::Container {
                op: sv::ir::ContainerOp::Set,
                ..
            }
        ) =>
        {
            match args.first() {
                Some(Some(sv::ir::Expr::Ident(name))) => Some(name.clone()),
                _ => None,
            }
        }
        _ => None,
    }
}
