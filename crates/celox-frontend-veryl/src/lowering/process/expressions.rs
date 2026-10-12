//! Expressions a process evaluates, in their source order.
use super::*;
use veryl_analyzer::ir::{ArrayLiteralItem, CasePattern, Factor};

pub(super) fn system_inputs(kind: &SystemFunctionKind) -> Vec<&Expression> {
    match kind {
        SystemFunctionKind::Bits(input)
        | SystemFunctionKind::Size(input, _)
        | SystemFunctionKind::Clog2(input)
        | SystemFunctionKind::Onehot(input)
        | SystemFunctionKind::Signed(input)
        | SystemFunctionKind::Unsigned(input)
        | SystemFunctionKind::Readmemh(input, _) => vec![&input.0],
        SystemFunctionKind::Display(inputs) | SystemFunctionKind::Write(inputs) => {
            inputs.iter().map(|input| &input.0).collect()
        }
        SystemFunctionKind::Assert { cond, args, .. } => std::iter::once(&cond.0)
            .chain(args.iter().map(|input| &input.0))
            .collect(),
        SystemFunctionKind::Finish => Vec::new(),
    }
}
pub(super) fn selection<'s>(index: &'s VarIndex, select: &'s VarSelect) -> Vec<&'s Expression> {
    index
        .0
        .iter()
        .chain(select.0.iter())
        .chain(select.1.iter().map(|(_, expr)| expr))
        .collect()
}
pub(super) fn children(expr: &Expression) -> Vec<&Expression> {
    match expr {
        Expression::Term(factor) => match factor.as_ref() {
            Factor::Variable(_, index, select, _) => selection(index, select),
            Factor::HierVariable(reference) => selection(&reference.index, &reference.select),
            Factor::FunctionCall(call) => call
                .inputs
                .values()
                .chain(call.outputs.values().flat_map(|destinations| {
                    destinations
                        .iter()
                        .flat_map(|dst| selection(&dst.index, &dst.select))
                }))
                .collect(),
            Factor::SystemFunctionCall(call)
                if !matches!(
                    call.kind,
                    SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(..)
                ) =>
            {
                system_inputs(&call.kind)
            }
            _ => Vec::new(),
        },
        Expression::Unary(_, inner, _) => vec![inner],
        Expression::Binary(lhs, _, rhs, _) => vec![lhs, rhs],
        Expression::Ternary(cond, yes, no, _) => vec![cond, yes, no],
        Expression::Concatenation(parts, _) => parts
            .iter()
            .flat_map(|(value, repeat)| std::iter::once(value).chain(repeat.iter()))
            .collect(),
        Expression::ArrayLiteral(items, _) => items
            .iter()
            .flat_map(|item| match item {
                ArrayLiteralItem::Value(value, repeat) => std::iter::once(value.as_ref())
                    .chain(repeat.iter().map(|expr| expr.as_ref()))
                    .collect::<Vec<_>>(),
                ArrayLiteralItem::Defaul(value) => vec![value.as_ref()],
            })
            .collect(),
        Expression::StructConstructor(_, fields, _) => {
            fields.iter().map(|(_, expr)| expr).collect()
        }
    }
}
pub(super) fn has_call(expr: &Expression) -> bool {
    matches!(expr, Expression::Term(factor) if matches!(factor.as_ref(),Factor::FunctionCall(_)))
        || children(expr).into_iter().any(has_call)
}
pub(super) fn statement_inputs(statement: &Statement) -> Vec<&Expression> {
    match statement {
        Statement::Assign(assign) => std::iter::once(&assign.expr)
            .chain(
                assign
                    .dst
                    .iter()
                    .flat_map(|dst| selection(&dst.index, &dst.select)),
            )
            .collect(),
        Statement::If(stmt) => vec![&stmt.cond],
        Statement::Case(stmt) => std::iter::once(stmt.case_target.as_ref())
            .chain(stmt.arms.iter().flat_map(|arm| {
                arm.patterns.iter().flat_map(|pattern| match pattern {
                    CasePattern::Eq(expr) => vec![expr.as_ref()],
                    CasePattern::Range { lo, hi, .. } => vec![lo.as_ref(), hi.as_ref()],
                })
            }))
            .collect(),
        Statement::For(stmt) => {
            let (start, end) = match &stmt.range {
                ForRange::Forward { start, end, .. }
                | ForRange::Reverse { start, end, .. }
                | ForRange::Stepped { start, end, .. } => (start, end),
            };
            [start, end]
                .into_iter()
                .filter_map(|bound| {
                    if let ForBound::Expression(expr) = bound {
                        Some(expr.as_ref())
                    } else {
                        None
                    }
                })
                .collect()
        }
        Statement::FunctionCall(call) => call
            .inputs
            .values()
            .chain(call.outputs.values().flat_map(|destinations| {
                destinations
                    .iter()
                    .flat_map(|dst| selection(&dst.index, &dst.select))
            }))
            .collect(),
        Statement::SystemFunctionCall(call) => system_inputs(&call.kind),
        Statement::TbMethodCall(tb) => {
            let mut inputs: Vec<&Expression> = match &tb.method {
                TbMethod::ClockNext { count, .. } => count.iter().collect(),
                TbMethod::ResetAssert { duration, .. } => duration.iter().collect(),
                TbMethod::RandomSeed { value, .. } => vec![value],
                TbMethod::RandomGetRange { min, max, .. } => vec![min, max],
                TbMethod::Component { args, .. } => args.iter().map(|arg| &arg.0).collect(),
                _ => Vec::new(),
            };
            if let Some(ret) = tb.ret.as_deref() {
                inputs.extend(selection(&ret.index, &ret.select));
            }
            inputs
        }
        _ => Vec::new(),
    }
}
pub(super) fn statement_bodies(statement: &Statement) -> Vec<&[Statement]> {
    match statement {
        Statement::If(stmt) => match constant_truth(&stmt.cond) {
            Some(true) => vec![&stmt.true_side],
            Some(false) => vec![&stmt.false_side],
            None => vec![&stmt.true_side, &stmt.false_side],
        },
        Statement::IfReset(stmt) => vec![&stmt.true_side, &stmt.false_side],
        Statement::Case(stmt) if selected_case_body(stmt).is_some() => {
            vec![selected_case_body(stmt).unwrap()]
        }
        Statement::Case(stmt) => stmt
            .arms
            .iter()
            .map(|arm| arm.body.as_slice())
            .chain(std::iter::once(stmt.default.as_slice()))
            .collect(),
        Statement::For(stmt) if !constant_empty_range(&stmt.range) => vec![&stmt.body],
        _ => Vec::new(),
    }
}
/// Calls that every evaluation of this expression must reach.
pub(super) fn unconditional_expression_calls<'s>(
    expr: &'s Expression,
    calls: &mut Vec<&'s veryl_analyzer::ir::FunctionCall>,
) {
    let inputs = match expr {
        Expression::Binary(lhs, Op::LogicAnd, rhs, _) => {
            if constant_truth(lhs) == Some(true) {
                vec![lhs.as_ref(), rhs.as_ref()]
            } else {
                vec![lhs.as_ref()]
            }
        }
        Expression::Binary(lhs, Op::LogicOr, rhs, _) => {
            if constant_truth(lhs) == Some(false) {
                vec![lhs.as_ref(), rhs.as_ref()]
            } else {
                vec![lhs.as_ref()]
            }
        }
        Expression::Ternary(cond, yes, no, _) => match constant_truth(cond) {
            Some(true) => vec![cond.as_ref(), yes.as_ref()],
            Some(false) => vec![cond.as_ref(), no.as_ref()],
            None => vec![cond.as_ref()],
        },
        _ => children(expr),
    };
    for child in inputs {
        unconditional_expression_calls(child, calls);
    }
    if let Expression::Term(factor) = expr
        && let Factor::FunctionCall(call) = factor.as_ref()
    {
        calls.push(call);
    }
}

pub(super) fn expression_calls<'s>(
    expr: &'s Expression,
    calls: &mut Vec<&'s veryl_analyzer::ir::FunctionCall>,
) {
    let inputs = match expr {
        Expression::Binary(lhs, Op::LogicAnd, _, _) if constant_truth(lhs) == Some(false) => {
            vec![lhs.as_ref()]
        }
        Expression::Binary(lhs, Op::LogicOr, _, _) if constant_truth(lhs) == Some(true) => {
            vec![lhs.as_ref()]
        }
        Expression::Ternary(cond, yes, no, _) => match constant_truth(cond) {
            Some(true) => vec![cond.as_ref(), yes.as_ref()],
            Some(false) => vec![cond.as_ref(), no.as_ref()],
            None => children(expr),
        },
        _ => children(expr),
    };
    for child in inputs {
        expression_calls(child, calls);
    }
    if let Expression::Term(factor) = expr
        && let Factor::FunctionCall(call) = factor.as_ref()
    {
        calls.push(call);
    }
}
pub(super) fn storage_width(expr: &Expression) -> usize {
    expr.comptime()
        .expr_context
        .width
        .max(crate::context_width::get_expr_width(expr).unwrap_or(0))
        .max(expr.comptime().r#type.total_width().unwrap_or(64))
        .max(
            expr.comptime()
                .get_value()
                .ok()
                .map_or(0, |value| value.width()),
        )
        .max(1)
}

impl BlockLowering<'_> {
    pub(super) fn save_expression(
        &mut self,
        expr: &Expression,
        value: RegisterId,
    ) -> Result<(), ParserError> {
        let (width, id) = self
            .expression_values
            .get(&expr.token_range())
            .copied()
            .ok_or_else(|| {
                self.error(
                    "process expression storage",
                    format!("missing value for {expr}"),
                    Some(&expr.token_range()),
                )
            })?;
        let value = self.parser.cast_reg_width_ext(
            self.kernel.builder(),
            value,
            width,
            expression_signed(expr),
        );
        self.store_scratch(id, width, value);
        Ok(())
    }
    pub(super) fn saved_expression(&mut self, expr: &Expression) -> RegisterId {
        let (width, id) = self.expression_values[&expr.token_range()];
        let builder = self.kernel.builder();
        let value = if expr.comptime().r#type.is_2state() {
            builder.alloc_bit(width, expression_signed(expr))
        } else {
            builder.alloc_logic(width)
        };
        builder.emit(SIRInstruction::Load(
            value,
            RegionedSourceAddr {
                region: STABLE_REGION,
                instance_id: self.instance,
                var_id: id,
            },
            SIROffset::Static(0),
            width,
        ));
        value
    }
    fn parse_with_values(
        &mut self,
        expr: &Expression,
        values: HashMap<TokenRange, RegisterId>,
    ) -> Result<RegisterId, ParserError> {
        self.parser.push_process_expression_values(values);
        let result = self.raw_expression(expr);
        self.parser.pop_process_expression_values();
        result
    }
    fn has_unbound_calls(&self, expr: &Expression) -> bool {
        self.parser
            .process_expression_value(expr.token_range())
            .is_none()
            && (matches!(expr,Expression::Term(factor) if matches!(factor.as_ref(),Factor::FunctionCall(_)))
                || children(expr)
                    .into_iter()
                    .any(|child| self.has_unbound_calls(child)))
    }

    pub(super) fn expression_bindings(
        &mut self,
        inputs: &[&Expression],
    ) -> Result<HashMap<TokenRange, RegisterId>, ParserError> {
        if !inputs.iter().any(|expr| has_call(expr)) {
            return Ok(HashMap::default());
        }
        let inputs = inputs
            .iter()
            .copied()
            .filter(|expr| has_call(expr) || FfParser::static_string_expr(expr).is_none())
            .collect::<Vec<_>>();
        for expr in &inputs {
            let value = self.expression(expr)?;
            self.save_expression(expr, value)?;
        }
        Ok(inputs
            .into_iter()
            .map(|expr| (expr.token_range(), self.saved_expression(expr)))
            .collect())
    }

    pub(super) fn expression(&mut self, expr: &Expression) -> Result<RegisterId, ParserError> {
        self.settle_if_dirty()?;
        if let Some(value) = self.parser.process_expression_value(expr.token_range()) {
            return Ok(value);
        }
        if !self.has_unbound_calls(expr) {
            return self.raw_expression(expr);
        }
        if let Expression::Term(factor) = expr
            && let Factor::FunctionCall(call) = factor.as_ref()
        {
            let body = function_body(&self.ir.functions, call).ok_or_else(|| {
                self.error(
                    "process function expression",
                    format!("{call}"),
                    Some(&call.comptime.token),
                )
            })?;
            if let Flow::Ended = self.lower_function_call(call)? {
                // Keep the syntactic continuation unreachable. The Finish return
                // already stops this kernel and the other ready processes.
                let unreachable = self.kernel.builder().new_block();
                self.kernel.builder().switch_to_block(unreachable);
                return Ok(self.constant(0, storage_width(expr)));
            }
            self.parser.clear_register_cache();
            return self.load_var(body.ret.ok_or_else(|| {
                self.error(
                    "process function expression",
                    "function has no result",
                    Some(&call.comptime.token),
                )
            })?);
        }
        if let Expression::Binary(lhs, op, rhs, _) = expr
            && matches!(op, Op::LogicAnd | Op::LogicOr)
        {
            if let Some(truth) = constant_truth(lhs)
                && ((*op == Op::LogicAnd && !truth) || (*op == Op::LogicOr && truth))
            {
                return Ok(self.constant(u64::from(truth), 1));
            }
            return self.logical_expression(expr, lhs, rhs, *op == Op::LogicAnd);
        }
        if let Expression::Ternary(cond, yes, no, _) = expr {
            return self.conditional_expression(expr, cond, yes, no);
        }
        let inputs = match expr {
            Expression::Binary(lhs, Op::As, _, _) => vec![lhs.as_ref()],
            _ => children(expr),
        };
        for child in &inputs {
            let value = self.expression(child)?;
            self.save_expression(child, value)?;
        }
        let values = inputs
            .into_iter()
            .map(|child| (child.token_range(), self.saved_expression(child)))
            .collect();
        self.parse_with_values(expr, values)
    }
    fn logical_expression(
        &mut self,
        expr: &Expression,
        lhs: &Expression,
        rhs: &Expression,
        is_and: bool,
    ) -> Result<RegisterId, ParserError> {
        let left = self.expression(lhs)?;
        self.save_expression(lhs, left)?;
        let builder = self.kernel.builder();
        let not = builder.alloc_logic(1);
        builder.emit(SIRInstruction::Unary(not, UnaryOp::LogicNot, left));
        let dominant = if is_and {
            not
        } else {
            let truth = builder.alloc_logic(1);
            builder.emit(SIRInstruction::Unary(truth, UnaryOp::LogicNot, not));
            truth
        };
        let shortcut = builder.alloc_bit(1, false);
        builder.emit(SIRInstruction::Unary(
            shortcut,
            UnaryOp::ToTwoState,
            dominant,
        ));
        let short = builder.new_block();
        let full = builder.new_block();
        let join = builder.new_block();
        builder.seal_block(SIRTerminator::Branch {
            cond: shortcut,
            true_block: (short, Vec::new()),
            false_block: (full, Vec::new()),
        });
        let dirty_before = self.dirty;
        self.kernel.builder().switch_to_block(short);
        let value = self.constant(u64::from(!is_and), 1);
        self.save_expression(expr, value)?;
        self.kernel
            .builder()
            .seal_block(SIRTerminator::Jump(join, Vec::new()));
        self.kernel.builder().switch_to_block(full);
        let right = self.expression(rhs)?;
        let left = self.saved_expression(lhs);
        let builder = self.kernel.builder();
        let value = builder.alloc_logic(1);
        builder.emit(SIRInstruction::Binary(
            value,
            left,
            if is_and {
                BinaryOp::LogicAnd
            } else {
                BinaryOp::LogicOr
            },
            right,
        ));
        self.save_expression(expr, value)?;
        self.dirty |= dirty_before;
        self.kernel
            .builder()
            .seal_block(SIRTerminator::Jump(join, Vec::new()));
        self.kernel.builder().switch_to_block(join);
        self.parser.clear_register_cache();
        Ok(self.saved_expression(expr))
    }
    fn conditional_expression(
        &mut self,
        expr: &Expression,
        cond: &Expression,
        yes: &Expression,
        no: &Expression,
    ) -> Result<RegisterId, ParserError> {
        if let Some(truth) = constant_truth(cond) {
            let value = self.expression(if truth { yes } else { no })?;
            self.save_expression(expr, value)?;
            return Ok(self.saved_expression(expr));
        }
        let value = self.expression(cond)?;
        self.save_expression(cond, value)?;
        let truth = self
            .parser
            .lower_procedural_condition(value, self.kernel.builder());
        let builder = self.kernel.builder();
        let yes_block = builder.new_block();
        let decide = builder.new_block();
        let no_block = builder.new_block();
        let unknown = builder.new_block();
        let join = builder.new_block();
        builder.seal_block(SIRTerminator::Branch {
            cond: truth,
            true_block: (yes_block, Vec::new()),
            false_block: (decide, Vec::new()),
        });
        let dirty_before = self.dirty;
        self.kernel.builder().switch_to_block(yes_block);
        let result = self.expression(yes)?;
        self.save_expression(expr, result)?;
        let yes_dirty = self.dirty;
        self.kernel
            .builder()
            .seal_block(SIRTerminator::Jump(join, Vec::new()));
        self.kernel.builder().switch_to_block(decide);
        self.dirty = dirty_before;
        let condition = self.saved_expression(cond);
        let builder = self.kernel.builder();
        let not = builder.alloc_logic(1);
        builder.emit(SIRInstruction::Unary(not, UnaryOp::LogicNot, condition));
        let false_known = builder.alloc_bit(1, false);
        builder.emit(SIRInstruction::Unary(false_known, UnaryOp::ToTwoState, not));
        builder.seal_block(SIRTerminator::Branch {
            cond: false_known,
            true_block: (no_block, Vec::new()),
            false_block: (unknown, Vec::new()),
        });
        self.kernel.builder().switch_to_block(no_block);
        let result = self.expression(no)?;
        self.save_expression(expr, result)?;
        let no_dirty = self.dirty;
        self.kernel
            .builder()
            .seal_block(SIRTerminator::Jump(join, Vec::new()));
        self.kernel.builder().switch_to_block(unknown);
        self.dirty = dirty_before;
        for child in [yes, no] {
            let value = self.expression(child)?;
            self.save_expression(child, value)?;
        }
        let values = [cond, yes, no]
            .into_iter()
            .map(|child| (child.token_range(), self.saved_expression(child)))
            .collect();
        let result = self.parse_with_values(expr, values)?;
        self.save_expression(expr, result)?;
        self.kernel
            .builder()
            .seal_block(SIRTerminator::Jump(join, Vec::new()));
        self.kernel.builder().switch_to_block(join);
        self.dirty |= yes_dirty || no_dirty;
        self.parser.clear_register_cache();
        Ok(self.saved_expression(expr))
    }
}
