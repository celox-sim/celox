//! Expression evaluation: an expression is lowered to SLT and its reads of
//! module state are resolved to the values the store holds; the guards of
//! short-circuit and conditional operands.

use super::*;

impl Comb<'_, '_> {
    /// Replace each local whose value is a known constant by a literal, so
    /// selects and loop conditions over it fold.
    pub(super) fn propagate<'e>(
        &mut self,
        store: &Store,
        expr: &'e sv::ir::Expr,
    ) -> Cow<'e, sv::ir::Expr> {
        let literals = self.known_literals(store, expr);
        if literals.is_empty() {
            return Cow::Borrowed(expr);
        }
        Cow::Owned(substitute_literals(expr, &literals))
    }

    /// The constant values the store holds for the variables `expr` names,
    /// as typed literals and numbers.
    fn known_literals(
        &mut self,
        store: &Store,
        expr: &sv::ir::Expr,
    ) -> HashMap<String, (String, i128)> {
        let mut names = HashSet::default();
        expr_idents(expr, &mut names);
        let mut literals: HashMap<String, (String, i128)> = HashMap::default();
        for name in names {
            let Some(id) = self.m.id(&name) else { continue };
            let Some(range) = store.get(&id) else {
                continue;
            };
            let var = self.m.var(id);
            if var.float.is_some() {
                continue;
            }
            let (width, signed) = (var.width, var.signed);
            let Ok(parts) = range.get_parts(full(width)) else {
                continue;
            };
            let [(Some((node, _)), relative)] = parts.as_slice() else {
                continue;
            };
            if relative.lsb != 0 || relative.msb + 1 != width || self.width(*node) != width {
                continue;
            }
            let Some((value, _)) = slt_const(self.arena, &mut self.consts, *node) else {
                continue;
            };
            let numeric = if signed && width > 0 && value.bit(width as u64 - 1) {
                (num_bigint::BigInt::from(value.clone()) - (num_bigint::BigInt::from(1u8) << width))
                    .to_i128()
            } else {
                value.to_i128()
            };
            let Some(numeric) = numeric else { continue };
            literals.insert(name, (typed_literal(&value, width, signed), numeric));
        }
        literals
    }

    pub(super) fn propagate_lvalue(
        &mut self,
        store: &Store,
        lvalue: &sv::ir::LValue,
    ) -> sv::ir::LValue {
        match lvalue {
            sv::ir::LValue::Ident(_) => lvalue.clone(),
            sv::ir::LValue::Select {
                name,
                msb,
                lsb,
                array_slice_width,
                array_slice_reversed,
                window,
            } => {
                let probe = sv::ir::Expr::Select {
                    expr: Box::new(sv::ir::Expr::Literal("0".to_string())),
                    msb: msb.clone(),
                    lsb: lsb.clone(),
                    signed: false,
                };
                let sv::ir::Expr::Select { msb, lsb, .. } =
                    self.propagate(store, &probe).into_owned()
                else {
                    unreachable!()
                };
                keep_element_window(
                    lvalue,
                    sv::ir::LValue::Select {
                        name: name.clone(),
                        msb,
                        lsb,
                        array_slice_width: array_slice_width.clone(),
                        array_slice_reversed: *array_slice_reversed,
                        window: *window,
                    },
                    self.m.variables,
                    self.m.name_to_id,
                    self.m.constants,
                    self.m.parameter_types,
                )
            }
        }
    }

    /// Evaluate an expression in the current store.
    pub(super) fn eval(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        expr: &sv::ir::Expr,
        context: Option<(usize, bool)>,
    ) -> Result<Value, sv::AnalyzerError> {
        self.eval_mode(store, frames, expr, context, false)
    }

    pub(super) fn eval_mode(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        expr: &sv::ir::Expr,
        context: Option<(usize, bool)>,
        preserve_literals: bool,
    ) -> Result<Value, sv::AnalyzerError> {
        let expr = expr_for_state_mode(expr, self.m.four_state || preserve_literals);
        let expr = if self.m.calls(&expr) {
            Cow::Owned(self.hoist(store, frames, &expr)?)
        } else {
            expr
        };
        // A select an unrolled loop takes past a vector (see the guarded
        // unit's lowering). Its positions take the store's known values,
        // such as the loop's index, but its base stays the variable it
        // names: the bits outside a four-state variable read X whatever
        // the store knows of the variable (IEEE 1800-2023 11.5.1).
        let literals = self.known_literals(store, &expr);
        let bounded = if literals.is_empty() {
            Cow::Borrowed(expr.as_ref())
        } else {
            Cow::Owned(substitute_select_bounds(&expr, &literals))
        };
        let expr = match out_of_range_constant_selects(
            &bounded,
            self.m.variables,
            self.m.name_to_id,
            self.m.constants,
            self.m.parameter_types,
            self.m.four_state,
        ) {
            Some(rewritten) => Cow::Owned(rewritten),
            None => expr,
        };
        let expr = match expr {
            Cow::Borrowed(expr) => self.propagate(store, expr),
            Cow::Owned(expr) => Cow::Owned(self.propagate(store, &expr).into_owned()),
        };
        let (node, _) = if let (sv::ir::Expr::Literal(literal), Some((width, _))) =
            (expr.as_ref(), context)
            && let Some(fill) = unbased_fill_literal(literal)
        {
            (
                lower_unbased_fill_literal_slt(self.arena, fill, width)
                    .ok_or_else(|| unsupported(format!("expression `{literal}`")))?,
                Sources::default(),
            )
        } else {
            lower_expr_with_context(
                &expr,
                self.m.variables,
                self.m.name_to_id,
                self.m.constants,
                self.m.parameter_types,
                self.arena,
                context.map(|(width, _)| width),
                context.map(|(_, signed)| signed),
            )
            .ok_or_else(|| unsupported("combinational expression"))?
        };
        let mut memo = HashMap::default();
        self.resolve(store, node, &mut memo)
    }

    pub(super) fn expr_signed(&self, expr: &sv::ir::Expr) -> bool {
        self.m.expr_signed(expr)
    }

    /// Replace bits absorbed by a constant zero mask by literal zeros before
    /// dependency analysis (IEEE 1800-2023 Table 11-11, including X/Z inputs).
    pub(super) fn masked_and_operand(
        &mut self,
        store: &Store,
        operand: NodeId,
        constant: NodeId,
    ) -> Result<Option<Value>, sv::AnalyzerError> {
        let Some((value, mask, width)) = slt_const4(self.arena, &mut self.consts, constant) else {
            return Ok(None);
        };
        if self.width(operand) != width {
            return Ok(None);
        }
        self.masked_input(store, operand, &(value | mask))
    }

    pub(super) fn masked_input(
        &mut self,
        store: &Store,
        node: NodeId,
        active: &BigUint,
    ) -> Result<Option<Value>, sv::AnalyzerError> {
        if active.is_zero() {
            return Ok(Some((
                self.constant(0, self.width(node))?,
                Sources::default(),
            )));
        }
        match self.arena.get(node).clone() {
            SLTNode::Constant(..) => Ok(Some((node, Sources::default()))),
            SLTNode::Input {
                variable,
                index,
                access,
                ..
            } if index.is_empty() => {
                let mut sources = Sources::default();
                let mut parts = Vec::new();
                let width = access.msb - access.lsb + 1;
                let mut bit = 0;
                while bit < width {
                    let selected = active.bit(bit as u64);
                    let first = bit;
                    while bit < width && active.bit(bit as u64) == selected {
                        bit += 1;
                    }
                    let part = if selected {
                        let (part, read_sources) = self.read(
                            store,
                            variable,
                            BitAccess::new(access.lsb + first, access.lsb + bit - 1),
                        )?;
                        sources.extend(read_sources);
                        part
                    } else {
                        self.constant(0, bit - first)?
                    };
                    parts.push((part, bit - first));
                }
                Ok(Some((self.concat_lsb_first(parts)?, sources)))
            }
            SLTNode::Concat(parts) => {
                let mut sources = Sources::default();
                let mut replacement = Vec::new();
                let mut offset = 0usize;
                for (part, width) in parts.into_iter().rev() {
                    let selected =
                        (active >> offset) & ((BigUint::from(1u8) << width) - BigUint::from(1u8));
                    if self.width(part) != width {
                        return Ok(None);
                    }
                    let Some((part, part_sources)) = self.masked_input(store, part, &selected)?
                    else {
                        return Ok(None);
                    };
                    sources.extend(part_sources);
                    replacement.push((part, width));
                    offset += width;
                }
                Ok(Some((self.concat_lsb_first(replacement)?, sources)))
            }
            SLTNode::Slice { expr, access } => {
                let Some((expr, sources)) =
                    self.masked_input(store, expr, &(active << access.lsb))?
                else {
                    return Ok(None);
                };
                Ok(Some((self.slice(expr, access)?, sources)))
            }
            _ => Ok(None),
        }
    }

    /// Rewrite the module-state reads of a freshly lowered tree to the values
    /// the store holds.
    pub(super) fn resolve(
        &mut self,
        store: &Store,
        node: NodeId,
        memo: &mut HashMap<NodeId, Value>,
    ) -> Result<Value, sv::AnalyzerError> {
        if let Some(value) = memo.get(&node) {
            return Ok(value.clone());
        }
        let value = match self.arena.get(node).clone() {
            SLTNode::Input {
                variable,
                signed,
                index,
                access,
            } => {
                if index.is_empty() {
                    if self.m.force_controls(variable).is_some()
                        || store.get(&variable).is_some_and(|range| {
                            range
                                .get_parts_ref(access)
                                .is_ok_and(|parts| parts.iter().any(|(value, _)| value.is_some()))
                        })
                    {
                        let (value, sources) = self.read(store, variable, access)?;
                        let _ = signed;
                        (value, sources)
                    } else {
                        (
                            node,
                            [VarAtomBase::new(variable, access.lsb, access.msb)]
                                .into_iter()
                                .collect(),
                        )
                    }
                } else {
                    let mut sources = Sources::default();
                    let mut resolved_index = Vec::with_capacity(index.len());
                    for entry in &index {
                        let (index_node, index_sources) = self.resolve(store, entry.node, memo)?;
                        sources.extend(index_sources);
                        resolved_index.push(SLTIndex {
                            node: index_node,
                            stride: entry.stride,
                            kind: entry.kind,
                        });
                    }
                    let var_width = self.m.var(variable).width;
                    let written = self.m.force_controls(variable).is_some()
                        || store.get(&variable).is_some_and(|range| {
                            range.ranges.values().any(|(value, _, _)| value.is_some())
                        });
                    if written {
                        // Read the selected bits out of the current value.
                        let (whole, whole_sources) = self.read(store, variable, full(var_width))?;
                        sources.extend(whole_sources);
                        let offset_width = 64;
                        let mut offset = self.constant(access.lsb as u64, offset_width)?;
                        for entry in &resolved_index {
                            let index_node = coerce_node_width(
                                self.arena,
                                entry.node,
                                Some(offset_width),
                                false,
                            )
                            .map_err(slt_error)?;
                            let stride = self.constant(entry.stride as u64, offset_width)?;
                            let scaled =
                                self.alloc(SLTNode::Binary(index_node, BinaryOp::Mul, stride))?;
                            offset = self.alloc(SLTNode::Binary(offset, BinaryOp::Add, scaled))?;
                        }
                        let offset = coerce_node_width(
                            self.arena,
                            offset,
                            Some(var_width.max(offset_width)),
                            false,
                        )
                        .map_err(slt_error)?;
                        let whole = coerce_node_width(
                            self.arena,
                            whole,
                            Some(var_width.max(offset_width)),
                            false,
                        )
                        .map_err(slt_error)?;
                        let shifted = self.alloc(SLTNode::Binary(whole, BinaryOp::Shr, offset))?;
                        let value =
                            self.slice(shifted, BitAccess::new(0, access.msb - access.lsb))?;
                        (value, sources)
                    } else {
                        sources.insert(VarAtomBase::new(variable, 0, var_width.saturating_sub(1)));
                        let rebuilt = if resolved_index
                            .iter()
                            .zip(&index)
                            .all(|(new, old)| new.node == old.node)
                        {
                            node
                        } else {
                            self.alloc(SLTNode::Input {
                                variable,
                                signed,
                                index: resolved_index,
                                access,
                            })?
                        };
                        (rebuilt, sources)
                    }
                }
            }
            SLTNode::Constant(..) => (node, Sources::default()),
            SLTNode::Binary(left, op, right) => {
                let (mut l, mut sources) = self.resolve(store, left, memo)?;
                let (mut r, mut r_sources) = self.resolve(store, right, memo)?;
                if op == BinaryOp::And
                    && !(node_is_signed(self.arena, l) && node_is_signed(self.arena, r))
                {
                    // A procedural observer still wakes on the original reads,
                    // even when a masked bit cannot affect the assigned value.
                    self.events
                        .effect_sensitivity
                        .extend(sources.iter().copied());
                    self.events
                        .effect_sensitivity
                        .extend(r_sources.iter().copied());
                    if let Some((masked, read_sources)) = self.masked_and_operand(store, left, r)? {
                        l = masked;
                        sources = read_sources;
                    }
                    if let Some((masked, read_sources)) =
                        self.masked_and_operand(store, right, l)?
                    {
                        r = masked;
                        r_sources = read_sources;
                    }
                }
                sources.extend(r_sources);
                let rebuilt = if !self.m.four_state
                    && matches!(op, BinaryOp::EqCase | BinaryOp::NeCase)
                {
                    super::super::case_equality::compare(self.arena, l, op, r).map_err(slt_error)?
                } else if l == left && r == right {
                    node
                } else {
                    self.alloc(SLTNode::Binary(l, op, r))?
                };
                (rebuilt, sources)
            }
            SLTNode::Unary(op, inner) => {
                let (i, sources) = self.resolve(store, inner, memo)?;
                let rebuilt = if i == inner {
                    node
                } else {
                    self.alloc(SLTNode::Unary(op, i))?
                };
                (rebuilt, sources)
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                let (c, mut sources) = self.resolve(store, cond, memo)?;
                let (t, t_sources) = self.resolve(store, then_expr, memo)?;
                let (e, e_sources) = self.resolve(store, else_expr, memo)?;
                sources.extend(t_sources);
                sources.extend(e_sources);
                let rebuilt = if c == cond && t == then_expr && e == else_expr {
                    node
                } else {
                    self.alloc(SLTNode::Mux {
                        cond: c,
                        then_expr: t,
                        else_expr: e,
                    })?
                };
                (rebuilt, sources)
            }
            SLTNode::Concat(parts) => {
                let mut sources = Sources::default();
                let mut changed = false;
                let mut resolved = Vec::with_capacity(parts.len());
                for (part, width) in &parts {
                    let (p, part_sources) = self.resolve(store, *part, memo)?;
                    changed |= p != *part;
                    sources.extend(part_sources);
                    resolved.push((p, *width));
                }
                let rebuilt = if changed {
                    self.alloc(SLTNode::Concat(resolved))?
                } else {
                    node
                };
                (rebuilt, sources)
            }
            SLTNode::Slice { expr, access } => {
                // A constant select of a variable reads only the selected bits.
                if let SLTNode::Input {
                    variable,
                    index,
                    access: input_access,
                    ..
                } = self.arena.get(expr).clone()
                    && index.is_empty()
                    && access.msb + input_access.lsb <= input_access.msb
                {
                    let absolute = BitAccess::new(
                        input_access.lsb + access.lsb,
                        input_access.lsb + access.msb,
                    );
                    let value = self.read(store, variable, absolute)?;
                    memo.insert(node, value.clone());
                    return Ok(value);
                }
                let (e, sources) = self.resolve(store, expr, memo)?;
                let rebuilt = if e == expr {
                    node
                } else {
                    self.alloc(SLTNode::Slice { expr: e, access })?
                };
                (rebuilt, sources)
            }
            SLTNode::Capture { expr, key } => {
                let (e, sources) = self.resolve(store, expr, memo)?;
                let rebuilt = if e == expr {
                    node
                } else {
                    self.alloc(SLTNode::Capture { expr: e, key })?
                };
                (rebuilt, sources)
            }
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => {
                return Err(unsupported("loop fold in an expression"));
            }
        };
        memo.insert(node, value.clone());
        Ok(value)
    }

    /// The guard under which the right operand of `&&` or `||` is evaluated:
    /// it is skipped only when the left one is known false (`&&`) or known
    /// true (`||`) (IEEE 1800-2023 11.4.7), so an unknown left operand
    /// evaluates it.
    pub(super) fn short_circuit_guard(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        op: sv::ir::BinaryOp,
        left: &sv::ir::Expr,
    ) -> Result<Value, sv::AnalyzerError> {
        let left_value = self.eval(
            store,
            frames,
            &floating::condition(left, self.m.variables, self.m.name_to_id),
            None,
        )?;
        let guard = if op == sv::ir::BinaryOp::LogicAnd {
            slt_not_false(self.arena, left_value.0)?
        } else {
            let truth = slt_truth(self.arena, left_value.0)?;
            slt_not(self.arena, &mut self.consts, truth)?
        };
        Ok((guard, left_value.1))
    }

    /// The guards under which the arms of a conditional operator are
    /// evaluated; an unknown condition evaluates both (IEEE 1800-2023
    /// 11.4.11).
    pub(super) fn mux_guards(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        condition: &sv::ir::Expr,
    ) -> Result<(Value, Value), sv::AnalyzerError> {
        let condition_value = self.eval(
            store,
            frames,
            &floating::condition(condition, self.m.variables, self.m.name_to_id),
            None,
        )?;
        let truth = slt_truth(self.arena, condition_value.0)?;
        let unknown = if self.m.four_state {
            Some(slt_truth_unknown(
                self.arena,
                &mut self.consts,
                condition_value.0,
            )?)
        } else {
            None
        };
        let then_guard = match unknown {
            Some(unknown) => slt_or(self.arena, &mut self.consts, truth, unknown)?,
            None => truth,
        };
        let not_truth = slt_not(self.arena, &mut self.consts, truth)?;
        let else_guard = match unknown {
            Some(unknown) => slt_or(self.arena, &mut self.consts, not_truth, unknown)?,
            None => not_truth,
        };
        Ok((
            (then_guard, condition_value.1.clone()),
            (else_guard, condition_value.1),
        ))
    }

    /// Run `f` on a branch of `store` taken where `guard` holds.
    pub(super) fn guarded<T>(
        &mut self,
        store: &mut Store,
        guard: Value,
        f: impl FnOnce(&mut Self, &mut Store) -> Result<T, sv::AnalyzerError>,
    ) -> Result<T, sv::AnalyzerError> {
        let mut taken = store.fork();
        self.path.guards.push(guard.clone());
        let result = f(self, &mut taken);
        self.path.guards.pop();
        let result = result?;
        *store = self.merge(&guard, &taken, store)?;
        Ok(result)
    }
}
