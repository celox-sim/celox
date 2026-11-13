//! Expression evaluation: an expression goes through SLT into registers,
//! after the loop variables of unrolled loops are substituted and its calls
//! hoisted (see `hoist.rs`).

use super::*;

impl Ff<'_, '_> {
    pub(super) fn lower_slt(
        &mut self,
        arena: &SLTNodeArena<SourceVarId>,
        node: NodeId,
    ) -> Result<RegisterId, sv::AnalyzerError> {
        let mut mapped_arena = SLTNodeArena::<Addr>::new();
        let mut cache = HashMap::default();
        let aliases = &self.aliases.map;
        let working_set = &self.writes.working;
        let variables = &*self.m.variables;
        let direct = self.kernel.is_some() || self.direct;
        let shared = &self.writes.shared;
        let map = |id: &SourceVarId| -> Addr {
            let var = aliases.get(id).unwrap_or(id);
            if direct || shared.contains(var) {
                stable(*var)
            } else if let Some(var) = aliases.get(id) {
                working(*var)
            } else if working_set.contains(id)
                || variables.get(id).is_some_and(SvVariable::is_scratch)
            {
                working(*id)
            } else {
                stable(*id)
            }
        };
        if !self.m.four_state {
            // Node IDs are topological. Cache exact comparisons before mapping
            // their parents, reusing the existing graph/loop remapper.
            for (index, expression) in arena.iter().enumerate() {
                if let SLTNode::Binary(left, op @ (BinaryOp::EqCase | BinaryOp::NeCase), right) =
                    expression
                {
                    let left = arena
                        .get(*left)
                        .map_addr(*left, arena, &mut mapped_arena, &mut cache, &map)
                        .map_err(slt_error)?;
                    let right = arena
                        .get(*right)
                        .map_addr(*right, arena, &mut mapped_arena, &mut cache, &map)
                        .map_err(slt_error)?;
                    let equal =
                        super::super::case_equality::compare(&mut mapped_arena, left, *op, right)
                            .map_err(slt_error)?;
                    cache.insert(NodeId(index), equal);
                }
            }
        }
        let mapped = arena
            .get(node)
            .map_addr(node, arena, &mut mapped_arena, &mut cache, &map)
            .map_err(slt_error)?;
        let mut lowerer = SLTToSIRLowerer::new(self.m.four_state)
            .with_unpacked_input_types(&mapped_arena, &self.element_widths);
        let mut lower_cache = HashMap::default();
        Ok(lowerer.lower(&mut self.b, mapped, &mapped_arena, &mut lower_cache))
    }

    pub(super) fn propagate(&self, expr: &sv::ir::Expr) -> sv::ir::Expr {
        if self.scope.overlay.is_empty() {
            return expr.clone();
        }
        substitute_overlay(expr, &self.scope.overlay)
    }

    pub(super) fn propagate_lvalue(&self, lvalue: &sv::ir::LValue) -> sv::ir::LValue {
        match lvalue {
            sv::ir::LValue::Ident(_) => lvalue.clone(),
            sv::ir::LValue::Select {
                name,
                msb,
                lsb,
                array_slice_width,
                array_slice_reversed,
            } => keep_element_window(
                lvalue,
                sv::ir::LValue::Select {
                    name: name.clone(),
                    msb: substitute_overlay_const(msb, &self.scope.overlay),
                    lsb: substitute_overlay_const(lsb, &self.scope.overlay),
                    array_slice_width: array_slice_width.clone(),
                    array_slice_reversed: *array_slice_reversed,
                },
                self.m.variables,
                self.m.name_to_id,
                self.m.constants,
                self.m.parameter_types,
            ),
        }
    }

    /// Lower an expression to SLT with its calls already executed.
    pub(super) fn expr_slt(
        &mut self,
        expr: &sv::ir::Expr,
        context: Option<(usize, bool)>,
        arena: &mut SLTNodeArena<SourceVarId>,
    ) -> Result<NodeId, sv::AnalyzerError> {
        let expr = expr_for_state_mode(expr, self.m.four_state);
        let expr = if self.m.calls(&expr) {
            std::borrow::Cow::Owned(self.hoist(&expr)?)
        } else {
            expr
        };
        let expr = if self.scope.overlay.is_empty() {
            expr
        } else {
            std::borrow::Cow::Owned(self.propagate(&expr))
        };
        // Before forced reads wrap the vectors it selects.
        let expr = match out_of_range_constant_selects(
            &expr,
            self.m.variables,
            self.m.name_to_id,
            self.m.constants,
            self.m.parameter_types,
            self.m.four_state,
        ) {
            Some(rewritten) => std::borrow::Cow::Owned(rewritten),
            None => expr,
        };
        let expr = if self.m.forceable.is_empty() {
            expr
        } else {
            std::borrow::Cow::Owned(force_reads(&expr, self.m))
        };
        if let (sv::ir::Expr::Literal(literal), Some((width, _))) = (expr.as_ref(), context)
            && let Some(fill) = unbased_fill_literal(literal)
        {
            return lower_unbased_fill_literal_slt(arena, fill, width)
                .ok_or_else(|| unsupported(format!("expression `{literal}`")));
        }
        let (node, _) = lower_expr_with_context(
            &expr,
            self.m.variables,
            self.m.name_to_id,
            self.m.constants,
            self.m.parameter_types,
            arena,
            context.map(|(width, _)| width),
            context.map(|(_, signed)| signed),
        )
        .ok_or_else(|| unsupported("always_ff expression"))?;
        Ok(node)
    }

    pub(super) fn eval(
        &mut self,
        expr: &sv::ir::Expr,
        context: Option<(usize, bool)>,
    ) -> Result<RegisterId, sv::AnalyzerError> {
        let mut arena = SLTNodeArena::new();
        let mut node = self.expr_slt(expr, context, &mut arena)?;
        if let Some((width, signed)) = context {
            node = coerce_node_width(&mut arena, node, Some(width), signed).map_err(slt_error)?;
        }
        self.lower_slt(&arena, node)
    }

    /// The truth of a procedural condition as a one-bit register.
    pub(super) fn eval_truth(
        &mut self,
        expr: &sv::ir::Expr,
    ) -> Result<(RegisterId, Option<bool>), sv::AnalyzerError> {
        let mut arena = SLTNodeArena::new();
        let node = self.expr_slt(
            &floating::condition(expr, self.m.variables, self.m.name_to_id),
            None,
            &mut arena,
        )?;
        let truth = slt_truth(&mut arena, node)?;
        let constant = slt_bool(&arena, &mut ConstCache::default(), truth);
        Ok((self.lower_slt(&arena, truth)?, constant))
    }

    /// Whether an expression is not logically false, as a one-bit register.
    pub(super) fn eval_not_false(
        &mut self,
        expr: &sv::ir::Expr,
    ) -> Result<RegisterId, sv::AnalyzerError> {
        let mut arena = SLTNodeArena::new();
        let node = self.expr_slt(
            &floating::condition(expr, self.m.variables, self.m.name_to_id),
            None,
            &mut arena,
        )?;
        let not_false = slt_not_false(&mut arena, node)?;
        self.lower_slt(&arena, not_false)
    }

    /// The constant value of an expression, if it has one.
    pub(super) fn eval_const(
        &mut self,
        expr: &sv::ir::Expr,
    ) -> Result<Option<(BigUint, usize)>, sv::AnalyzerError> {
        self.eval_const_with_width(expr, None)
    }

    pub(super) fn eval_const_with_width(
        &mut self,
        expr: &sv::ir::Expr,
        width: Option<usize>,
    ) -> Result<Option<(BigUint, usize)>, sv::AnalyzerError> {
        if self.m.calls(expr) {
            return Ok(None);
        }
        let signed = width.map(|_| self.expr_signed(expr));
        let expr = self.propagate(&expr_for_state_mode(expr, self.m.four_state));
        let mut arena = SLTNodeArena::new();
        let Some((node, _)) = lower_expr_with_context(
            &expr,
            self.m.variables,
            self.m.name_to_id,
            self.m.constants,
            self.m.parameter_types,
            &mut arena,
            None,
            None,
        ) else {
            return Ok(None);
        };
        let node = if let Some(signed) = signed {
            coerce_node_width(&mut arena, node, width, signed).map_err(slt_error)?
        } else {
            node
        };
        Ok(slt_const(&arena, &mut ConstCache::default(), node))
    }

    pub(super) fn expr_signed(&self, expr: &sv::ir::Expr) -> bool {
        self.m.expr_signed(expr)
    }

    /// Run `f` where the right operand of `&&` or `||` is evaluated: it is
    /// skipped only when the left one is known false (`&&`) or known true
    /// (`||`) (IEEE 1800-2023 11.4.7), so an unknown left operand evaluates
    /// it.
    pub(super) fn short_circuit<T>(
        &mut self,
        op: sv::ir::BinaryOp,
        left: &sv::ir::Expr,
        f: impl FnOnce(&mut Self) -> Result<T, sv::AnalyzerError>,
    ) -> Result<T, sv::AnalyzerError> {
        let taken = self.b.new_block();
        let join = self.b.new_block();
        let (cond, true_block, false_block) = if op == sv::ir::BinaryOp::LogicAnd {
            (self.eval_not_false(left)?, taken, join)
        } else {
            (self.eval_truth(left)?.0, join, taken)
        };
        self.b.seal_block(SIRTerminator::Branch {
            cond,
            true_block: (true_block, Vec::new()),
            false_block: (false_block, Vec::new()),
        });
        self.b.switch_to_block(taken);
        let result = f(self)?;
        self.b.seal_block(SIRTerminator::Jump(join, Vec::new()));
        self.b.switch_to_block(join);
        Ok(result)
    }

    /// Run `f` in a block entered only where `cond` holds.
    pub(super) fn when<T>(
        &mut self,
        cond: RegisterId,
        f: impl FnOnce(&mut Self) -> Result<T, sv::AnalyzerError>,
    ) -> Result<T, sv::AnalyzerError> {
        let taken = self.b.new_block();
        let join = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond,
            true_block: (taken, Vec::new()),
            false_block: (join, Vec::new()),
        });
        self.b.switch_to_block(taken);
        let result = f(self)?;
        self.b.seal_block(SIRTerminator::Jump(join, Vec::new()));
        self.b.switch_to_block(join);
        Ok(result)
    }

    /// Run `then_f` and `else_f` where the arms of a conditional operator
    /// are evaluated; an ambiguous condition evaluates both (IEEE 1800-2023
    /// 11.4.11). Both arms are passed `state`.
    pub(super) fn mux_arms<S, A, B>(
        &mut self,
        condition: &sv::ir::Expr,
        state: &mut S,
        then_f: impl FnOnce(&mut Self, &mut S) -> Result<A, sv::AnalyzerError>,
        else_f: impl FnOnce(&mut Self, &mut S) -> Result<B, sv::AnalyzerError>,
    ) -> Result<(A, B), sv::AnalyzerError> {
        if self.m.four_state {
            let mut arena = SLTNodeArena::new();
            let mut consts = ConstCache::default();
            let node = self.expr_slt(condition, None, &mut arena)?;
            let truth = slt_truth(&mut arena, node)?;
            let unknown = slt_truth_unknown(&mut arena, &mut consts, node)?;
            let then_cond = slt_or(&mut arena, &mut consts, truth, unknown)?;
            let not_truth = slt_not(&mut arena, &mut consts, truth)?;
            let else_cond = slt_or(&mut arena, &mut consts, not_truth, unknown)?;
            let then_cond = self.lower_slt(&arena, then_cond)?;
            let else_cond = self.lower_slt(&arena, else_cond)?;
            // Whether the else arm runs is decided before the then arm runs,
            // and the then arm's calls may suspend the process or unit and
            // resume it inside the arm. It is kept in a variable (a frame
            // slot in a unit), as no register lives across a resumption.
            let (decided, decided_name) = self.m.temp("else_runs", 1, false, false);
            self.store(decided, SIROffset::Static(0), 1, else_cond);
            let then_result = self.when(then_cond, |this| then_f(this, state))?;
            let (else_cond, _) = self.eval_truth(&sv::ir::Expr::Ident(decided_name))?;
            let else_result = self.when(else_cond, |this| else_f(this, state))?;
            return Ok((then_result, else_result));
        }
        let (truth, _) = self.eval_truth(condition)?;
        let then_block = self.b.new_block();
        let else_block = self.b.new_block();
        let join = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: truth,
            true_block: (then_block, Vec::new()),
            false_block: (else_block, Vec::new()),
        });
        self.b.switch_to_block(then_block);
        let then_result = then_f(self, state)?;
        self.b.seal_block(SIRTerminator::Jump(join, Vec::new()));
        self.b.switch_to_block(else_block);
        let else_result = else_f(self, state)?;
        self.b.seal_block(SIRTerminator::Jump(join, Vec::new()));
        self.b.switch_to_block(join);
        Ok((then_result, else_result))
    }

    /// `expr` converted to a 32-bit `int` or `int unsigned` formal.
    pub(super) fn int_formal(&self, expr: sv::ir::Expr) -> sv::ir::Expr {
        let signed = self.expr_signed(&expr);
        sv::ir::Expr::Resize {
            expr: Box::new(expr),
            width: 32,
            signed,
        }
    }

    /// Heap helpers emit several registers before their call. Materialize
    /// operands first so an operand's synchronous host request cannot leave
    /// those registers live across a kernel return. Preserve formal sizing and
    /// handle roots, including an object receiver captured before side effects.
    pub(super) fn heap_operand(
        &mut self,
        expr: &sv::ir::Expr,
        context: Option<(usize, bool)>,
        kind: Option<celox_design::ValueKind>,
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        let expr = self.hoist(expr)?;
        let value_kind = kind.unwrap_or_else(|| {
            if let sv::ir::Expr::Ident(name) = &expr {
                self.m
                    .id(name)
                    .and_then(|id| self.m.variables.get(&id))
                    .map(|v| v.value_kind)
                    .unwrap_or(celox_design::ValueKind::Bits)
            } else if self.m.expr_is_string(&expr) {
                celox_design::ValueKind::StrRef
            } else {
                celox_design::ValueKind::Bits
            }
        });
        let float = self.m.expr_float_precision(&expr);
        let signed = context.map_or_else(|| self.expr_signed(&expr), |(_, signed)| signed);
        let value = self.eval(&expr, context)?;
        let register = self.b.register(&value);
        let width = register.width();
        let four_state = matches!(register, celox_sir::RegisterType::Logic { .. });
        let (id, name) = self.m.temp("heap_operand", width, signed, four_state);
        let variable = self.m.variables.get_mut(&id).unwrap();
        variable.value_kind = value_kind;
        variable.float = float;
        self.store(id, SIROffset::Static(0), width, value);
        Ok(sv::ir::Expr::Ident(name))
    }
}
