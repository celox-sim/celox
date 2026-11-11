//! Symbolic state of a function body: state values, coercion, merging, assignment and substitution.

use super::*;

impl<'a> FfParser<'a> {
    fn function_state_comptime(&self, id: VarId) -> Comptime {
        let token = TokenRange::default();
        let mut comptime = Comptime::create_unknown(token);
        if let Some(variable) = self.module.variables.get(&id) {
            comptime.r#type = variable.r#type.clone();
            comptime.expr_context.width = variable.r#type.total_width().unwrap_or_default();
            comptime.expr_context.signed = variable.r#type.signed;
        }
        comptime
    }

    pub(super) fn function_state_merge_comptime(&self, id: VarId) -> Comptime {
        let mut comptime = self.function_state_comptime(id);
        // Token::default() allocates a fresh token id, so equality with a
        // default TokenRange cannot identify synthetic expressions reliably.
        // Source lines cannot use this value; keep it as an internal marker
        // for procedural state merges that may be lowered into guarded writes.
        comptime.token.beg.line = u32::MAX;
        comptime.token.end.line = u32::MAX;
        comptime
    }

    fn function_state_base_comptime(&self, id: VarId) -> Comptime {
        let mut comptime = self.function_state_comptime(id);
        Self::mark_function_state_base_comptime(&mut comptime);
        comptime
    }

    fn mark_function_state_base_comptime(comptime: &mut Comptime) {
        comptime.token.beg.line = u32::MAX - 2;
        comptime.token.end.line = u32::MAX - 2;
    }

    pub(super) fn is_function_state_base(comptime: &Comptime) -> bool {
        comptime.token.beg.line == u32::MAX - 2 && comptime.token.end.line == u32::MAX - 2
    }

    pub(super) fn is_function_state_merge(comptime: &Comptime) -> bool {
        comptime.token.beg.line == u32::MAX && comptime.token.end.line == u32::MAX
    }

    pub(super) fn mark_emitted_function_state(expr: &mut Expression) {
        expr.comptime_mut().token.beg.line = u32::MAX - 1;
        expr.comptime_mut().token.end.line = u32::MAX - 1;
    }

    pub(super) fn is_emitted_function_state(expr: &Expression) -> bool {
        expr.comptime().token.beg.line == u32::MAX - 1
            && expr.comptime().token.end.line == u32::MAX - 1
    }

    pub(super) fn state_value_expr(
        &self,
        id: VarId,
        state: &HashMap<VarId, Expression>,
    ) -> Expression {
        state.get(&id).cloned().unwrap_or_else(|| {
            Expression::Term(Box::new(Factor::Variable(
                id,
                VarIndex::default(),
                VarSelect::default(),
                self.function_state_base_comptime(id),
            )))
        })
    }

    pub(super) fn coerce_function_state_assignment(
        &self,
        expr: Expression,
        dst: &AssignDestination,
    ) -> Result<Expression, ParserError> {
        let variable = &self.module.variables[&dst.id];
        let is_whole_var =
            dst.index.0.is_empty() && dst.select.0.is_empty() && dst.select.1.is_none();
        // Unpacked arrays are shape-checked and lowered element-wise elsewhere;
        // a scalar `as` cast cannot represent their assignment conversion.
        if !variable.r#type.array.is_empty() && is_whole_var {
            return Ok(expr);
        }

        let target_type = if is_whole_var {
            variable.r#type.clone()
        } else {
            let width = get_access_width(self.module, dst.id, &dst.index, &dst.select)?;
            let mut ty = Type::new(if variable.r#type.is_2state() {
                TypeKind::Bit
            } else {
                TypeKind::Logic
            });
            // Selecting only an unpacked array element preserves the packed
            // element's signedness. Packed selections are always unsigned.
            ty.signed = variable.r#type.signed && dst.select.0.is_empty() && dst.select.1.is_none();
            ty.set_concrete_width(Shape::new(vec![Some(width)]));
            ty
        };

        Ok(Self::coerce_function_expression_to_type(expr, &target_type))
    }

    fn coerce_function_expression_to_type(expr: Expression, target_type: &Type) -> Expression {
        if crate::float::conversion(&expr.comptime().r#type, target_type).is_some() {
            return crate::float::converted(&expr, target_type).into_owned();
        }
        let source_type = &expr.comptime().r#type;
        if source_type.total_width() == target_type.total_width()
            && source_type.signed == target_type.signed
            && source_type.is_2state() == target_type.is_2state()
        {
            return expr;
        }

        let token = expr.token_range();
        let cast_target = Expression::Term(Box::new(Factor::Value(Comptime {
            value: ValueVariant::Type(target_type.clone()),
            r#type: Type::new(TypeKind::Type),
            is_const: true,
            is_global: true,
            token,
            ..Default::default()
        })));
        let mut comptime = Comptime::create_unknown(token);
        comptime.r#type = target_type.clone();
        comptime.expr_context.width = target_type.total_width().unwrap_or_default();
        comptime.expr_context.signed = target_type.signed;
        Expression::Binary(
            Box::new(expr),
            Op::As,
            Box::new(cast_target),
            Box::new(comptime),
        )
    }

    pub(super) fn coerce_function_input_expression(
        &self,
        expr: Expression,
        formal_id: VarId,
    ) -> Expression {
        let formal = &self.module.variables[&formal_id];
        if formal.r#type.array.is_empty() {
            Self::coerce_function_expression_to_type(expr, &formal.r#type)
        } else {
            expr
        }
    }

    pub(super) fn merge_expression_states(
        &self,
        condition: &Expression,
        base: &HashMap<VarId, Expression>,
        then_state: &HashMap<VarId, Expression>,
        else_state: &HashMap<VarId, Expression>,
    ) -> HashMap<VarId, Expression> {
        let ids: HashSet<VarId> = base
            .keys()
            .chain(then_state.keys())
            .chain(else_state.keys())
            .copied()
            .collect();
        ids.into_iter()
            .map(|id| {
                let then_expr = then_state
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| self.state_value_expr(id, base));
                let else_expr = else_state
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| self.state_value_expr(id, base));
                (
                    id,
                    Expression::Ternary(
                        Box::new(Self::normalize_function_control_condition(
                            condition.clone(),
                        )),
                        Box::new(then_expr),
                        Box::new(else_expr),
                        Box::new(self.function_state_merge_comptime(id)),
                    ),
                )
            })
            .collect()
    }

    pub(super) fn apply_state_transition_on_path(
        &self,
        path: &FunctionPathCondition,
        base: &HashMap<VarId, Expression>,
        transitioned: HashMap<VarId, Expression>,
    ) -> HashMap<VarId, Expression> {
        match path {
            FunctionPathCondition::Always => transitioned,
            FunctionPathCondition::Never => base.clone(),
            FunctionPathCondition::Conditional(condition) => {
                self.merge_expression_states(condition, base, &transitioned, base)
            }
        }
    }

    /// Apply `dsts = rhs` to a symbolic function state.
    ///
    /// `rhs` must already be resolved against `state`; destination selects are
    /// resolved here against the same pre-assignment state. Concatenated
    /// destinations receive consecutive slices of `rhs`, the last destination
    /// taking the least-significant bits.
    pub(super) fn assign_function_state(
        &self,
        dsts: &[AssignDestination],
        rhs: Expression,
        state: &HashMap<VarId, Expression>,
    ) -> Result<HashMap<VarId, Expression>, ParserError> {
        self.assign_function_state_with_selects(dsts, rhs, state, state)
    }

    /// Like [`Self::assign_function_state`], but resolves destination selects
    /// against `select_state` instead of the state being updated.
    pub(super) fn assign_function_state_with_selects(
        &self,
        dsts: &[AssignDestination],
        rhs: Expression,
        select_state: &HashMap<VarId, Expression>,
        state: &HashMap<VarId, Expression>,
    ) -> Result<HashMap<VarId, Expression>, ParserError> {
        let dsts: Vec<_> = dsts
            .iter()
            .map(|dst| self.substitute_assignment_destination(dst, select_state))
            .collect();
        let mut next = state.clone();
        if let [dst] = dsts.as_slice() {
            let rhs = self.coerce_function_state_assignment(rhs, dst)?;
            self.store_function_state(dst, rhs, &mut next)?;
            return Ok(next);
        }

        let widths = dsts
            .iter()
            .map(|dst| get_access_width(self.module, dst.id, &dst.index, &dst.select))
            .collect::<Result<Vec<_>, _>>()?;
        let total_width: usize = widths.iter().sum();
        let is_2state = rhs.comptime().r#type.is_2state();
        let rhs = Self::coerce_function_expression_to_type(
            rhs,
            &Self::unsigned_packed_type(total_width, is_2state),
        );
        let mut offset = 0usize;
        for (dst, &width) in dsts.iter().zip(&widths).rev() {
            let token = TokenRange::default();
            let shifted = if offset == 0 {
                rhs.clone()
            } else {
                Expression::Binary(
                    Box::new(rhs.clone()),
                    Op::LogicShiftR,
                    Box::new(Expression::create_value(
                        Value::new(offset as u64, 64, false),
                        token,
                    )),
                    Box::new(Comptime::create_unknown(token)),
                )
            };
            let part = Self::coerce_function_expression_to_type(
                shifted,
                &Self::unsigned_packed_type(width, is_2state),
            );
            let part = self.coerce_function_state_assignment(part, dst)?;
            self.store_function_state(dst, part, &mut next)?;
            offset += width;
        }
        Ok(next)
    }

    fn unsigned_packed_type(width: usize, is_2state: bool) -> Type {
        let mut ty = Type::new(if is_2state {
            TypeKind::Bit
        } else {
            TypeKind::Logic
        });
        ty.set_concrete_width(Shape::new(vec![Some(width)]));
        ty
    }

    /// Store an already coerced value into one resolved destination.
    fn store_function_state(
        &self,
        dst: &AssignDestination,
        rhs: Expression,
        state: &mut HashMap<VarId, Expression>,
    ) -> Result<(), ParserError> {
        let is_whole_var =
            dst.index.0.is_empty() && dst.select.0.is_empty() && dst.select.1.is_none();
        let value = if is_whole_var {
            rhs
        } else {
            let old_value = self.state_value_expr(dst.id, state);
            if is_static_access(&dst.index, &dst.select) {
                build_partial_assign_expr(self.module, dst, rhs, old_value)?
            } else {
                let merged =
                    build_dynamic_partial_assign_expr(self.module, dst, rhs, old_value.clone())?;
                self.guard_dynamic_element_write(dst, merged, old_value)?
            }
        };
        state.insert(dst.id, value);
        Ok(())
    }

    /// Keep the old value when a dynamic unpacked-array or packed-dimension
    /// index is out of range. A flat bit offset alone would otherwise let an
    /// oversized inner index spill into a neighbouring element.
    fn guard_dynamic_element_write(
        &self,
        dst: &AssignDestination,
        merged: Expression,
        old_value: Expression,
    ) -> Result<Expression, ParserError> {
        Ok(
            match self.dynamic_index_in_range(dst.id, &dst.index, &dst.select)? {
                Some(condition) => Expression::Ternary(
                    Box::new(condition),
                    Box::new(merged),
                    Box::new(old_value),
                    Box::new(self.function_state_comptime(dst.id)),
                ),
                None => merged,
            },
        )
    }

    /// The condition under which every runtime index of a select addresses an
    /// existing element, or `None` when no index can be out of range.
    fn dynamic_index_in_range(
        &self,
        var_id: VarId,
        index: &VarIndex,
        select: &VarSelect,
    ) -> Result<Option<Expression>, ParserError> {
        let geometry = crate::bitaccess::select_geometry(self.module, var_id, index, select)?;
        let token = TokenRange::default();
        let mut condition: Option<Expression> = None;
        for (dimension, index) in index
            .0
            .iter()
            .chain(select.0.iter())
            .take(geometry.dimension_count)
            .enumerate()
        {
            if index.comptime().is_const {
                continue;
            }
            let outer = if dimension == 0 {
                geometry.total_width
            } else {
                geometry.strides[dimension - 1]
            };
            let size = outer / geometry.strides[dimension].max(1);
            let index_width = index.comptime().r#type.total_width().unwrap_or(usize::MAX);
            let always_in_range = !index.comptime().r#type.signed
                && index_width < usize::BITS as usize
                && (1usize << index_width) <= size;
            if always_in_range {
                continue;
            }
            let in_range = Expression::Binary(
                Box::new(index.clone()),
                Op::Less,
                Box::new(Expression::create_value(
                    Value::new(size as u64, 64, false),
                    token,
                )),
                Box::new(Comptime::create_unknown(token)),
            );
            condition = Some(match condition {
                Some(previous) => Expression::Binary(
                    Box::new(previous),
                    Op::LogicAnd,
                    Box::new(in_range),
                    Box::new(Comptime::create_unknown(token)),
                ),
                None => in_range,
            });
        }
        Ok(condition)
    }

    /// Select `index`/`select` from the symbolic whole value of `var_id`.
    ///
    /// An out-of-range runtime index reads X in a four-state variable and 0 in
    /// a two-state one (IEEE 1800-2023 7.4.6).
    fn select_from_state_value(
        &self,
        var_id: VarId,
        index: &VarIndex,
        select: &VarSelect,
        whole: Expression,
    ) -> Result<Expression, ParserError> {
        let (offset, geometry) =
            crate::bitaccess::select_offset_expr(self.module, var_id, index, select)?;
        let token = TokenRange::default();
        let shifted = Expression::Binary(
            Box::new(whole),
            Op::LogicShiftR,
            Box::new(offset),
            Box::new(Comptime::create_unknown(token)),
        );
        let variable = &self.module.variables[&var_id];
        let mut ty =
            Self::unsigned_packed_type(geometry.selected_width, variable.r#type.is_2state());
        // Selecting only unpacked array elements preserves the packed
        // element's signedness. Packed selections are always unsigned.
        ty.signed = variable.r#type.signed && select.0.is_empty() && select.1.is_none();
        let selected = Self::coerce_function_expression_to_type(shifted, &ty);
        let Some(condition) = self.dynamic_index_in_range(var_id, index, select)? else {
            return Ok(selected);
        };
        let out_of_range = if variable.r#type.is_2state() {
            Value::new(0, geometry.selected_width, ty.signed)
        } else {
            Value::new_x(geometry.selected_width, ty.signed)
        };
        let mut comptime = Comptime::create_unknown(token);
        comptime.r#type = ty.clone();
        comptime.expr_context.width = geometry.selected_width;
        comptime.expr_context.signed = ty.signed;
        Ok(Expression::Ternary(
            Box::new(condition),
            Box::new(selected),
            Box::new(Self::coerce_function_expression_to_type(
                Expression::create_value(out_of_range, token),
                &ty,
            )),
            Box::new(comptime),
        ))
    }

    pub(super) fn substitute_assignment_destination(
        &self,
        dst: &AssignDestination,
        defs: &HashMap<VarId, Expression>,
    ) -> AssignDestination {
        let mut dst = dst.clone();
        for expr in &mut dst.index.0 {
            *expr = self.substitute_function_expr(expr, defs);
        }
        for expr in &mut dst.select.0 {
            *expr = self.substitute_function_expr(expr, defs);
        }
        if let Some((_, expr)) = &mut dst.select.1 {
            *expr = self.substitute_function_expr(expr, defs);
        }
        dst
    }

    pub(super) fn substitute_function_expr(
        &self,
        expr: &Expression,
        defs: &HashMap<VarId, Expression>,
    ) -> Expression {
        self.substitute_function_expr_inner(expr, defs, &mut HashSet::default())
    }

    fn substitute_function_expr_inner(
        &self,
        expr: &Expression,
        defs: &HashMap<VarId, Expression>,
        expanding: &mut HashSet<VarId>,
    ) -> Expression {
        if self
            .get_bound_function_expression_value(expr.token_range())
            .is_some()
        {
            return expr.clone();
        }
        match expr {
            Expression::Term(factor) => match factor.as_ref() {
                Factor::Variable(var_id, index, select, comptime) => {
                    if Self::is_function_state_base(comptime) {
                        return expr.clone();
                    }
                    let is_whole = index.0.is_empty() && select.0.is_empty() && select.1.is_none();
                    if let Some(bound) = defs.get(var_id)
                        && expanding.insert(*var_id)
                    {
                        // State entries already hold the value captured at assignment.
                        // Re-expanding against later definitions would reread variables
                        // that were still unassigned then, such as mutable locals or formals.
                        let whole = bound.clone();
                        expanding.remove(var_id);
                        if is_whole {
                            return whole;
                        }
                        // A partial read of a variable with symbolic state
                        // selects from that state; its storage is stale.
                        let mut index = index.clone();
                        for expr in &mut index.0 {
                            *expr = self.substitute_function_expr_inner(expr, defs, expanding);
                        }
                        let mut select = select.clone();
                        for expr in &mut select.0 {
                            *expr = self.substitute_function_expr_inner(expr, defs, expanding);
                        }
                        if let Some((_, expr)) = &mut select.1 {
                            *expr = self.substitute_function_expr_inner(expr, defs, expanding);
                        }
                        // Select validation errors are reported when the
                        // original access is lowered, so keep it unchanged.
                        if let Ok(selected) =
                            self.select_from_state_value(*var_id, &index, &select, whole)
                        {
                            return selected;
                        }
                    }
                    if !defs.contains_key(var_id)
                        && self.module.variables[var_id].affiliation != Affiliation::Function
                    {
                        let mut comptime = comptime.clone();
                        Self::mark_function_state_base_comptime(&mut comptime);
                        return Expression::Term(Box::new(Factor::Variable(
                            *var_id,
                            index.clone(),
                            select.clone(),
                            comptime,
                        )));
                    }
                    expr.clone()
                }
                Factor::FunctionCall(call) => {
                    let mut call = call.clone();
                    for input_expr in call.inputs.values_mut() {
                        *input_expr =
                            self.substitute_function_expr_inner(input_expr, defs, expanding);
                    }
                    Expression::Term(Box::new(Factor::FunctionCall(call)))
                }
                Factor::SystemFunctionCall(call) => {
                    let mut call = call.clone();
                    match &mut call.kind {
                        // These functions evaluate their operand, so carry the
                        // assignment-time symbolic state through them. `$bits`
                        // and `$size` inspect a type/shape and intentionally keep
                        // their operand intact.
                        SystemFunctionKind::Clog2(input)
                        | SystemFunctionKind::Onehot(input)
                        | SystemFunctionKind::Signed(input)
                        | SystemFunctionKind::Unsigned(input) => {
                            input.0 =
                                self.substitute_function_expr_inner(&input.0, defs, expanding);
                        }
                        _ => {}
                    }
                    Expression::Term(Box::new(Factor::SystemFunctionCall(call)))
                }
                _ => expr.clone(),
            },
            Expression::Binary(lhs, op, rhs, comptime) => Expression::Binary(
                Box::new(self.substitute_function_expr_inner(lhs, defs, expanding)),
                *op,
                Box::new(self.substitute_function_expr_inner(rhs, defs, expanding)),
                comptime.clone(),
            ),
            Expression::Unary(op, inner, comptime) => Expression::Unary(
                *op,
                Box::new(self.substitute_function_expr_inner(inner, defs, expanding)),
                comptime.clone(),
            ),
            Expression::Ternary(cond, then_expr, else_expr, comptime) => Expression::Ternary(
                Box::new(self.substitute_function_expr_inner(cond, defs, expanding)),
                Box::new(self.substitute_function_expr_inner(then_expr, defs, expanding)),
                Box::new(self.substitute_function_expr_inner(else_expr, defs, expanding)),
                comptime.clone(),
            ),
            Expression::Concatenation(parts, comptime) => Expression::Concatenation(
                parts
                    .iter()
                    .map(|(x, rep)| {
                        (
                            self.substitute_function_expr_inner(x, defs, expanding),
                            rep.as_ref()
                                .map(|r| self.substitute_function_expr_inner(r, defs, expanding)),
                        )
                    })
                    .collect(),
                comptime.clone(),
            ),
            Expression::ArrayLiteral(items, comptime) => Expression::ArrayLiteral(
                items
                    .iter()
                    .map(|item| match item {
                        ArrayLiteralItem::Value(x, rep) => ArrayLiteralItem::Value(
                            Box::new(self.substitute_function_expr_inner(x, defs, expanding)),
                            rep.as_ref().map(|r| {
                                Box::new(self.substitute_function_expr_inner(r, defs, expanding))
                            }),
                        ),
                        ArrayLiteralItem::Defaul(x) => ArrayLiteralItem::Defaul(Box::new(
                            self.substitute_function_expr_inner(x, defs, expanding),
                        )),
                    })
                    .collect(),
                comptime.clone(),
            ),
            Expression::StructConstructor(ty, fields, comptime) => Expression::StructConstructor(
                ty.clone(),
                fields
                    .iter()
                    .map(|(name, x)| {
                        (
                            *name,
                            self.substitute_function_expr_inner(x, defs, expanding),
                        )
                    })
                    .collect(),
                comptime.clone(),
            ),
        }
    }
}
