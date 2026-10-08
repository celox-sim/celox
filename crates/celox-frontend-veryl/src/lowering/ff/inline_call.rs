//! Inline lowering of FF function calls.
//!
//! FF function calls are normally lowered symbolically: the body is folded
//! into one expression per written variable and substituted at the call site.
//! That representation cannot express a loop whose trip count is only known
//! at runtime, nor runtime effects nested in statement-form calls or in
//! assignment destinations. Calls whose body (transitively) needs one of
//! these are lowered by executing the body as ordinary procedural statements,
//! using blocking call-private working storage for formals and locals.

use super::{Domain, FfParser};
use crate::{
    HashMap, HashSet, ParserError, context_width::expression_signed, function_call_arg,
    resolve_total_width,
};
use celox_design::{VarAtomBase, WORKING_REGION};
use celox_sir::{RegisterId, SIRBuilder, SIRInstruction, SIROffset, SIRValue};
use num_bigint::BigUint;
use veryl_analyzer::ir::{
    ArrayLiteralItem, AssignDestination, CasePattern, Expression, Factor, ForBound, ForRange,
    FunctionBody, FunctionCall, Statement, SystemFunctionCall, SystemFunctionInput,
    SystemFunctionKind, VarId, VarIndex, VarPath, VarSelect,
};
use veryl_analyzer::symbol::Affiliation;
use veryl_parser::token_range::TokenRange;

/// Parser state that belongs to the symbolic lowering of enclosing calls. An
/// inline body cannot reference another function's formals, so this state is
/// hidden while the body is lowered.
struct SymbolicCallFrames {
    args: Vec<HashMap<VarId, Expression>>,
    arg_values: Vec<HashMap<VarId, RegisterId>>,
    expression_values: Vec<HashMap<TokenRange, RegisterId>>,
    event_arg_states: Vec<HashMap<TokenRange, HashMap<VarId, Expression>>>,
    array_views: Vec<HashMap<VarId, super::FunctionArrayView>>,
    array_views_enabled: Vec<bool>,
}

impl<'a> FfParser<'a> {
    fn function_body_for_call(&self, call: &FunctionCall) -> Option<FunctionBody> {
        let function = self.module.functions.get(&call.id)?;
        match &call.index {
            Some(index) => function.get_function(index),
            None => function.get_function(&[]),
        }
    }

    /// Whether the callee's body must be lowered inline.
    pub(super) fn call_requires_inline(&self, call: &FunctionCall) -> bool {
        self.call_requires_inline_inner(call, &mut HashSet::default())
    }

    fn call_requires_inline_inner(
        &self,
        call: &FunctionCall,
        visiting: &mut HashSet<VarId>,
    ) -> bool {
        let Some(body) = self.function_body_for_call(call) else {
            return false;
        };
        if !visiting.insert(call.id) {
            return false;
        }
        let result = self.statements_require_inline(&body.statements, visiting);
        visiting.remove(&call.id);
        result
    }

    fn statements_require_inline(
        &self,
        statements: &[Statement],
        visiting: &mut HashSet<VarId>,
    ) -> bool {
        statements.iter().any(|statement| match statement {
            // Constant-bound loops are unrolled by the analyzer; a remaining
            // loop has a runtime trip count.
            Statement::For(_) => true,
            Statement::Assign(assign) => {
                assign.dst.iter().any(|dst| {
                    self.assignment_destination_needs_eager_evaluation(dst)
                        || self.destination_requires_inline(dst, visiting)
                }) || self.expression_requires_inline(&assign.expr, visiting)
            }
            Statement::If(statement) => {
                self.expression_requires_inline(&statement.cond, visiting)
                    || self.statements_require_inline(&statement.true_side, visiting)
                    || self.statements_require_inline(&statement.false_side, visiting)
            }
            Statement::Case(statement) => {
                self.expression_requires_inline(&statement.case_target, visiting)
                    || statement.arms.iter().any(|arm| {
                        arm.patterns.iter().any(|pattern| match pattern {
                            CasePattern::Eq(expr) => {
                                self.expression_requires_inline(expr, visiting)
                            }
                            CasePattern::Range { lo, hi, .. } => {
                                self.expression_requires_inline(lo, visiting)
                                    || self.expression_requires_inline(hi, visiting)
                            }
                        }) || self.statements_require_inline(&arm.body, visiting)
                    })
                    || self.statements_require_inline(&statement.default, visiting)
            }
            Statement::FunctionCall(call) => {
                self.function_call_has_runtime_effect(call, &mut HashSet::default())
                    || call
                        .outputs
                        .values()
                        .flatten()
                        .any(|dst| self.assignment_destination_needs_eager_evaluation(dst))
                    || self.call_site_requires_inline(call, visiting)
            }
            Statement::SystemFunctionCall(call) => self.system_call_requires_inline(call, visiting),
            Statement::IfReset(_)
            | Statement::TbMethodCall(_)
            | Statement::Break
            | Statement::Unsupported(_)
            | Statement::Null => false,
        })
    }

    fn destination_requires_inline(
        &self,
        dst: &AssignDestination,
        visiting: &mut HashSet<VarId>,
    ) -> bool {
        dst.index
            .expressions()
            .chain(dst.select.0.iter())
            .chain(dst.select.1.iter().map(|(_, expr)| expr))
            .any(|expr| self.expression_requires_inline(expr, visiting))
    }

    fn call_site_requires_inline(
        &self,
        call: &FunctionCall,
        visiting: &mut HashSet<VarId>,
    ) -> bool {
        self.call_requires_inline_inner(call, visiting)
            || call
                .inputs
                .values()
                .any(|expr| self.expression_requires_inline(expr, visiting))
            || call
                .outputs
                .values()
                .flatten()
                .any(|dst| self.destination_requires_inline(dst, visiting))
    }

    fn system_call_requires_inline(
        &self,
        call: &SystemFunctionCall,
        visiting: &mut HashSet<VarId>,
    ) -> bool {
        let mut input =
            |input: &SystemFunctionInput| self.expression_requires_inline(&input.0, visiting);
        match &call.kind {
            SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => {
                args.iter().any(&mut input)
            }
            SystemFunctionKind::Assert { cond, args, .. } => {
                input(cond) || args.iter().any(&mut input)
            }
            SystemFunctionKind::Clog2(arg)
            | SystemFunctionKind::Onehot(arg)
            | SystemFunctionKind::Signed(arg)
            | SystemFunctionKind::Unsigned(arg) => input(arg),
            SystemFunctionKind::Bits(_)
            | SystemFunctionKind::Size(..)
            | SystemFunctionKind::Readmemh(..)
            | SystemFunctionKind::Finish => false,
        }
    }

    fn expression_requires_inline(&self, expr: &Expression, visiting: &mut HashSet<VarId>) -> bool {
        match expr {
            Expression::Term(factor) => match factor.as_ref() {
                Factor::Variable(_, index, select, _) => index
                    .expressions()
                    .chain(select.0.iter())
                    .chain(select.1.iter().map(|(_, expr)| expr))
                    .any(|expr| self.expression_requires_inline(expr, visiting)),
                Factor::HierVariable(reference) => reference
                    .index
                    .expressions()
                    .chain(reference.select.0.iter())
                    .chain(reference.select.1.iter().map(|(_, expr)| expr))
                    .any(|expr| self.expression_requires_inline(expr, visiting)),
                Factor::FunctionCall(call) => self.call_site_requires_inline(call, visiting),
                Factor::SystemFunctionCall(call) => {
                    self.system_call_requires_inline(call, visiting)
                }
                Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => false,
            },
            Expression::Binary(lhs, _, rhs, _) => {
                self.expression_requires_inline(lhs, visiting)
                    || self.expression_requires_inline(rhs, visiting)
            }
            Expression::Unary(_, inner, _) => self.expression_requires_inline(inner, visiting),
            Expression::Ternary(cond, then_expr, else_expr, _) => {
                self.expression_requires_inline(cond, visiting)
                    || self.expression_requires_inline(then_expr, visiting)
                    || self.expression_requires_inline(else_expr, visiting)
            }
            Expression::Concatenation(items, _) => items.iter().any(|(expr, repeat)| {
                self.expression_requires_inline(expr, visiting)
                    || repeat
                        .as_ref()
                        .is_some_and(|repeat| self.expression_requires_inline(repeat, visiting))
            }),
            Expression::ArrayLiteral(items, _) => items.iter().any(|item| match item {
                ArrayLiteralItem::Value(expr, repeat) => {
                    self.expression_requires_inline(expr, visiting)
                        || repeat
                            .as_ref()
                            .is_some_and(|repeat| self.expression_requires_inline(repeat, visiting))
                }
                ArrayLiteralItem::Defaul(expr) => self.expression_requires_inline(expr, visiting),
            }),
            Expression::StructConstructor(_, fields, _) => fields
                .iter()
                .any(|(_, expr)| self.expression_requires_inline(expr, visiting)),
        }
    }

    /// Lower a call by executing the callee body in place.
    ///
    /// Actuals are evaluated left to right before any formal is written, so a
    /// nested call of the same function in a later actual cannot clobber an
    /// earlier binding. Outputs are copied out after the body, in formal
    /// order. The body keeps the analyzer's return lowering (`return.active`
    /// guards and loop breaks), which procedural lowering executes directly.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_inline_function_call<A>(
        &mut self,
        call: &FunctionCall,
        body: &FunctionBody,
        ordered_arg_paths: &[VarPath],
        return_value: bool,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        if return_value && body.ret.is_none() {
            return Err(ParserError::illegal_context(
                "void function call in expression",
                format!("{call}"),
                Some(&call.comptime.token),
            ));
        }

        let mut inputs = Vec::new();
        for arg_path in ordered_arg_paths {
            let Some(&arg_id) = body.arg_map.get(arg_path) else {
                continue;
            };
            let Some(actual) = function_call_arg(&call.inputs, arg_path) else {
                continue;
            };
            let width = resolve_total_width(self.module, &self.module.variables[&arg_id])?;
            let actual = self.coerce_function_input_expression(actual.clone(), arg_id);
            let reg = self.evaluate_inline_input(
                arg_id, &actual, width, targets, domain, convert, sources, ir_builder,
            )?;
            let reg = self.coerce_register_to_variable_type(
                reg,
                arg_id,
                expression_signed(&actual),
                ir_builder,
            )?;
            inputs.push((arg_id, reg));
        }

        let mut storage = HashSet::default();
        collect_statement_variables(&body.statements, &mut storage);
        storage.extend(body.arg_map.values().copied());
        storage.extend(body.ret);
        storage.retain(|id| self.module.variables[id].affiliation == Affiliation::Function);

        // Automatic storage starts uninitialized on every call: X in a
        // four-state variable and zero in a two-state one.
        let input_ids: HashSet<_> = inputs.iter().map(|(id, _)| *id).collect();
        let mut uninitialized: Vec<_> = storage
            .iter()
            .copied()
            .filter(|id| !input_ids.contains(id))
            .collect();
        uninitialized.sort();
        for id in uninitialized {
            let variable = &self.module.variables[&id];
            let width = resolve_total_width(self.module, variable)?;
            let value = if variable.r#type.is_2state() {
                SIRValue::new(0u8)
            } else {
                SIRValue::new_four_state(0u8, (BigUint::from(1u8) << width) - BigUint::from(1u8))
            };
            self.op_constant(value, width, ir_builder);
            let reg = self.stack.pop_back().expect("constant register");
            self.store_inline_function_storage(id, reg, convert, ir_builder)?;
        }
        for (id, reg) in inputs {
            self.store_inline_function_storage(id, reg, convert, ir_builder)?;
        }

        let previous_locals = self.inline_function_locals.clone();
        self.inline_function_locals.extend(storage);
        let frames = self.take_symbolic_call_frames();
        let body_result = self
            .parse_statement_list(
                &body.statements,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            )
            .and_then(|_| {
                let ret = match body.ret.filter(|_| return_value) {
                    Some(ret_id) => {
                        self.op_load(
                            ret_id,
                            &VarIndex::default(),
                            &VarSelect::default(),
                            domain,
                            convert,
                            sources,
                            ir_builder,
                        )?;
                        Some(self.stack.pop_back().expect("function return load"))
                    }
                    None => None,
                };
                let mut outputs = Vec::new();
                for arg_path in ordered_arg_paths {
                    let Some(dsts) = function_call_arg(&call.outputs, arg_path) else {
                        continue;
                    };
                    if dsts.is_empty() {
                        continue;
                    }
                    let Some(&arg_id) = body.arg_map.get(arg_path) else {
                        return Err(ParserError::internal(
                            "function call missing argument",
                            format!("{call}"),
                            Some(&call.comptime.token),
                        ));
                    };
                    self.op_load(
                        arg_id,
                        &VarIndex::default(),
                        &VarSelect::default(),
                        domain,
                        convert,
                        sources,
                        ir_builder,
                    )?;
                    let reg = self.stack.pop_back().expect("function output load");
                    let reg =
                        self.coerce_function_output_to_actual(reg, arg_id, dsts, ir_builder)?;
                    outputs.push((reg, dsts.clone()));
                }
                Ok((ret, outputs))
            });
        self.restore_symbolic_call_frames(frames);
        self.inline_function_locals = previous_locals;
        let (ret, outputs) = body_result?;

        for (reg, dsts) in outputs {
            self.emit_multi_dst_assign(reg, &dsts, targets, domain, convert, sources, ir_builder)?;
        }
        if let Some(ret) = ret {
            self.stack.push_back(ret);
        }
        Ok(())
    }

    /// Evaluate an actual into the formal's whole-value register layout.
    ///
    /// Unpacked-array storage places element 0 at the least-significant end,
    /// while an assignment pattern evaluates as a packed concatenation with
    /// its first item most significant, so its elements are reversed. A
    /// select that leaves an unpacked sub-array is read element by element.
    #[allow(clippy::too_many_arguments)]
    fn evaluate_inline_input<A>(
        &mut self,
        formal_id: VarId,
        actual: &Expression,
        width: usize,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<RegisterId, ParserError> {
        let formal_shape: Vec<usize> = self.module.variables[&formal_id]
            .r#type
            .array
            .iter()
            .map(|dim| dim.unwrap_or(0))
            .collect();
        let element_count: usize = formal_shape.iter().product();
        if formal_shape.is_empty() || element_count == 0 || !width.is_multiple_of(element_count) {
            self.parse_expression(
                actual,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
                Some(width),
            )?;
            return Ok(self
                .stack
                .pop_back()
                .expect("Function input expression evaluation failed"));
        }
        let element_width = width / element_count;

        if let Expression::Term(factor) = actual
            && let Factor::Variable(var_id, index, select, comptime) = factor.as_ref()
            && select.0.is_empty()
            && select.1.is_none()
            && !index.is_range()
            && index.indices.len() < self.module.variables[var_id].r#type.array.iter().count()
        {
            let mut elements = Vec::with_capacity(element_count);
            for element in 0..element_count {
                let mut element_index = index.clone();
                let mut remainder = element;
                let mut suffix = Vec::with_capacity(formal_shape.len());
                for &dim in formal_shape.iter().rev() {
                    suffix.push(remainder % dim);
                    remainder /= dim;
                }
                for position in suffix.into_iter().rev() {
                    element_index.push(Expression::create_value(
                        veryl_analyzer::value::Value::new(position as u64, 32, false),
                        comptime.token,
                    ));
                }
                self.op_load(
                    *var_id,
                    &element_index,
                    select,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
                elements.push((self.stack.pop_back().expect("element load"), element_width));
            }
            // The first concatenated part is most significant.
            elements.reverse();
            return Ok(self.emit_concat_registers(&elements, ir_builder));
        }

        self.parse_expression(
            actual,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
            Some(width),
        )?;
        let value = self
            .stack
            .pop_back()
            .expect("Function input expression evaluation failed");
        if !matches!(actual, Expression::ArrayLiteral(..)) {
            return Ok(value);
        }
        let elements: Vec<_> = (0..element_count)
            .map(|element| {
                let part = ir_builder.alloc_logic(element_width);
                ir_builder.emit(SIRInstruction::Slice(
                    part,
                    value,
                    element * element_width,
                    element_width,
                ));
                (part, element_width)
            })
            .collect();
        Ok(self.emit_concat_registers(&elements, ir_builder))
    }

    fn store_inline_function_storage<A>(
        &mut self,
        id: VarId,
        reg: RegisterId,
        convert: &impl Fn(VarId, u32) -> A,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let variable = &self.module.variables[&id];
        let width = resolve_total_width(self.module, variable)?;
        let element_count = if variable.r#type.array.is_empty() {
            None
        } else {
            variable.r#type.total_array()
        };
        let offset = match element_count {
            Some(count) if count > 0 && width.is_multiple_of(count) => SIROffset::PackedElements {
                bit_offset: 0,
                element_width: width / count,
            },
            _ => SIROffset::Static(0),
        };
        ir_builder.emit(SIRInstruction::Store(
            convert(id, WORKING_REGION),
            offset,
            width,
            reg,
            Vec::new(),
            Vec::new(),
        ));
        Ok(())
    }

    fn take_symbolic_call_frames(&mut self) -> SymbolicCallFrames {
        SymbolicCallFrames {
            args: std::mem::take(&mut self.function_arg_stack),
            arg_values: std::mem::take(&mut self.function_arg_value_stack),
            expression_values: std::mem::take(&mut self.function_expression_value_stack),
            event_arg_states: std::mem::take(&mut self.function_event_arg_state_stack),
            array_views: std::mem::take(&mut self.function_array_view_stack),
            array_views_enabled: std::mem::take(&mut self.function_array_view_enabled_stack),
        }
    }

    fn restore_symbolic_call_frames(&mut self, frames: SymbolicCallFrames) {
        self.function_arg_stack = frames.args;
        self.function_arg_value_stack = frames.arg_values;
        self.function_expression_value_stack = frames.expression_values;
        self.function_event_arg_state_stack = frames.event_arg_states;
        self.function_array_view_stack = frames.array_views;
        self.function_array_view_enabled_stack = frames.array_views_enabled;
    }
}

fn collect_statement_variables(statements: &[Statement], out: &mut HashSet<VarId>) {
    for statement in statements {
        match statement {
            Statement::Assign(assign) => {
                for dst in &assign.dst {
                    collect_destination_variables(dst, out);
                }
                collect_expression_variables(&assign.expr, out);
            }
            Statement::If(statement) => {
                collect_expression_variables(&statement.cond, out);
                collect_statement_variables(&statement.true_side, out);
                collect_statement_variables(&statement.false_side, out);
            }
            Statement::IfReset(statement) => {
                collect_statement_variables(&statement.true_side, out);
                collect_statement_variables(&statement.false_side, out);
            }
            Statement::Case(statement) => {
                collect_expression_variables(&statement.case_target, out);
                for arm in &statement.arms {
                    for pattern in &arm.patterns {
                        match pattern {
                            CasePattern::Eq(expr) => collect_expression_variables(expr, out),
                            CasePattern::Range { lo, hi, .. } => {
                                collect_expression_variables(lo, out);
                                collect_expression_variables(hi, out);
                            }
                        }
                    }
                    collect_statement_variables(&arm.body, out);
                }
                collect_statement_variables(&statement.default, out);
            }
            Statement::For(statement) => {
                out.insert(statement.var_id);
                let (start, end) = match &statement.range {
                    ForRange::Forward { start, end, .. }
                    | ForRange::Reverse { start, end, .. }
                    | ForRange::Stepped { start, end, .. } => (start, end),
                };
                for bound in [start, end] {
                    if let ForBound::Expression(expr) = bound {
                        collect_expression_variables(expr, out);
                    }
                }
                collect_statement_variables(&statement.body, out);
            }
            Statement::SystemFunctionCall(call) => collect_system_call_variables(call, out),
            Statement::FunctionCall(call) => collect_call_variables(call, out),
            Statement::TbMethodCall(_)
            | Statement::Break
            | Statement::Unsupported(_)
            | Statement::Null => {}
        }
    }
}

fn collect_destination_variables(dst: &AssignDestination, out: &mut HashSet<VarId>) {
    out.insert(dst.id);
    for expr in dst
        .index
        .expressions()
        .chain(dst.select.0.iter())
        .chain(dst.select.1.iter().map(|(_, expr)| expr))
    {
        collect_expression_variables(expr, out);
    }
}

fn collect_call_variables(call: &FunctionCall, out: &mut HashSet<VarId>) {
    for expr in call.inputs.values() {
        collect_expression_variables(expr, out);
    }
    for dst in call.outputs.values().flatten() {
        collect_destination_variables(dst, out);
    }
}

fn collect_system_call_variables(call: &SystemFunctionCall, out: &mut HashSet<VarId>) {
    let mut input = |input: &SystemFunctionInput| collect_expression_variables(&input.0, out);
    match &call.kind {
        SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => {
            args.iter().for_each(&mut input);
        }
        SystemFunctionKind::Assert { cond, args, .. } => {
            input(cond);
            args.iter().for_each(&mut input);
        }
        SystemFunctionKind::Bits(arg)
        | SystemFunctionKind::Size(arg, _)
        | SystemFunctionKind::Clog2(arg)
        | SystemFunctionKind::Onehot(arg)
        | SystemFunctionKind::Signed(arg)
        | SystemFunctionKind::Unsigned(arg)
        | SystemFunctionKind::Readmemh(arg, _) => input(arg),
        SystemFunctionKind::Finish => {}
    }
}

fn collect_expression_variables(expr: &Expression, out: &mut HashSet<VarId>) {
    match expr {
        Expression::Term(factor) => match factor.as_ref() {
            Factor::Variable(id, index, select, _) => {
                out.insert(*id);
                for expr in index
                    .expressions()
                    .chain(select.0.iter())
                    .chain(select.1.iter().map(|(_, expr)| expr))
                {
                    collect_expression_variables(expr, out);
                }
            }
            Factor::FunctionCall(call) => collect_call_variables(call, out),
            Factor::SystemFunctionCall(call) => collect_system_call_variables(call, out),
            Factor::HierVariable(_)
            | Factor::Value(_)
            | Factor::Anonymous(_)
            | Factor::Unknown(_) => {}
        },
        Expression::Binary(lhs, _, rhs, _) => {
            collect_expression_variables(lhs, out);
            collect_expression_variables(rhs, out);
        }
        Expression::Unary(_, inner, _) => collect_expression_variables(inner, out),
        Expression::Ternary(cond, then_expr, else_expr, _) => {
            collect_expression_variables(cond, out);
            collect_expression_variables(then_expr, out);
            collect_expression_variables(else_expr, out);
        }
        Expression::Concatenation(items, _) => {
            for (expr, repeat) in items {
                collect_expression_variables(expr, out);
                if let Some(repeat) = repeat {
                    collect_expression_variables(repeat, out);
                }
            }
        }
        Expression::ArrayLiteral(items, _) => {
            for item in items {
                match item {
                    ArrayLiteralItem::Value(expr, repeat) => {
                        collect_expression_variables(expr, out);
                        if let Some(repeat) = repeat {
                            collect_expression_variables(repeat, out);
                        }
                    }
                    ArrayLiteralItem::Defaul(expr) => collect_expression_variables(expr, out),
                }
            }
        }
        Expression::StructConstructor(_, fields, _) => {
            for (_, expr) in fields {
                collect_expression_variables(expr, out);
            }
        }
    }
}
