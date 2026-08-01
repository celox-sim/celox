use celox_design::{PortTypeKind, RuntimeEventKind, RuntimeEventSite, StateAddr};
use celox_testbench::{
    AssertMessage as GenericAssertMessage, ClockCount as GenericClockCount, ExprBytecode,
    ExprOpcode as TbOpcode, LoopBound as GenericLoopBound, SemanticArgument, SemanticSignal,
    SemanticStatement, SourceLocation, StateLocation, TestbenchOperator as Op, TestbenchProgram,
    TestbenchStatement as GenericTestbenchStatement,
};
use fxhash::FxHashSet;
use veryl_analyzer::ir::{
    ArrayLiteralItem, AssertKind, CasePattern, Expression, Factor, ForBound, ForRange, Function,
    FunctionCall, Op as VerylOp, Statement, SystemFunctionInput, SystemFunctionKind, TbMethod,
    TbMethodCall, VarId,
};
use veryl_analyzer::value::byte_value_to_string;
use veryl_parser::resource_table::{self, StrId};

use crate::{
    VerylFrontendLookup, VerylTestbenchSource,
    context_width::{
        ValueContext, binary_semantics, cast_semantics, expression_signed, get_expr_width,
    },
};

type UnboundTbOpcode = TbOpcode<StateLocation<StateAddr>>;

const fn get_byte_size(width: usize) -> usize {
    width.div_ceil(8)
}

fn static_string_expr(expr: &Expression) -> Option<String> {
    if !expr.comptime().r#type.is_string() {
        return None;
    }
    let value = expr.comptime().get_value().ok()?;
    byte_value_to_string(value)
}

fn compile_assert_arg(
    input: &SystemFunctionInput,
    ec: &ExprCompiler<'_>,
) -> SemanticArgument<StateAddr> {
    let expr = &input.0;
    let width = {
        let ctx_width = expr.comptime().expr_context.width;
        if ctx_width > 0 {
            ctx_width
        } else if let Some(type_width) = expr.comptime().r#type.total_width() {
            type_width
        } else if let Ok(value) = expr.comptime().get_value() {
            value.width()
        } else {
            0
        }
    };
    SemanticArgument {
        expr: ec.compile(expr),
        width,
        signed: expression_signed(expr),
        is_string: expr.comptime().r#type.is_string(),
    }
}

fn assert_arg_width(input: &SystemFunctionInput) -> usize {
    let expr = &input.0;
    let ctx_width = expr.comptime().expr_context.width;
    if ctx_width > 0 {
        ctx_width
    } else if let Some(type_width) = expr.comptime().r#type.total_width() {
        type_width
    } else if let Ok(value) = expr.comptime().get_value() {
        value.width()
    } else {
        0
    }
}

fn runtime_event_site_for_assert(
    kind: &AssertKind,
    args: &[SystemFunctionInput],
) -> RuntimeEventSite {
    let (template, value_args) = if args
        .first()
        .and_then(|arg| static_string_expr(&arg.0))
        .is_some()
    {
        (
            args.first().and_then(|arg| static_string_expr(&arg.0)),
            &args[1..],
        )
    } else {
        (None, args)
    };
    RuntimeEventSite {
        kind: match kind {
            AssertKind::Fatal => RuntimeEventKind::AssertFatal,
            AssertKind::Continue => RuntimeEventKind::AssertContinue,
        },
        template,
        arg_widths: value_args.iter().map(assert_arg_width).collect(),
        arg_signed: value_args
            .iter()
            .map(|arg| expression_signed(&arg.0))
            .collect(),
        arg_is_string: value_args
            .iter()
            .map(|arg| arg.0.comptime().r#type.is_string())
            .collect(),
    }
}

fn function_body(
    funcs: &fxhash::FxHashMap<VarId, Function>,
    fc: &veryl_analyzer::ir::FunctionCall,
) -> Option<veryl_analyzer::ir::FunctionBody> {
    let func = funcs.get(&fc.id)?;
    if let Some(idx) = &fc.index {
        func.get_function(idx)
    } else {
        func.get_function(&[])
    }
}

fn collect_runtime_event_sites(
    stmts: &[Statement],
    funcs: &fxhash::FxHashMap<VarId, Function>,
    out: &mut Vec<RuntimeEventSite>,
) {
    for stmt in stmts {
        match stmt {
            Statement::SystemFunctionCall(sf) => {
                if let SystemFunctionKind::Assert { kind, args, .. } = &sf.kind {
                    out.push(runtime_event_site_for_assert(kind, args));
                }
            }
            Statement::If(s) => {
                collect_runtime_event_sites(&s.true_side, funcs, out);
                collect_runtime_event_sites(&s.false_side, funcs, out);
            }
            Statement::For(s) => collect_runtime_event_sites(&s.body, funcs, out),
            Statement::FunctionCall(fc) => {
                if let Some(body) = function_body(funcs, fc) {
                    collect_runtime_event_sites(&body.statements, funcs, out);
                }
            }
            _ => {}
        }
    }
}

fn count_assert_statements(
    stmts: &[Statement],
    funcs: &fxhash::FxHashMap<VarId, Function>,
) -> usize {
    let mut count = 0;
    for stmt in stmts {
        match stmt {
            Statement::SystemFunctionCall(sf) => {
                if matches!(&sf.kind, SystemFunctionKind::Assert { .. }) {
                    count += 1;
                }
            }
            Statement::If(s) => {
                count += count_assert_statements(&s.true_side, funcs);
                count += count_assert_statements(&s.false_side, funcs);
            }
            Statement::For(s) => count += count_assert_statements(&s.body, funcs),
            Statement::FunctionCall(fc) => {
                if let Some(body) = function_body(funcs, fc) {
                    count += count_assert_statements(&body.statements, funcs);
                }
            }
            _ => {}
        }
    }
    count
}

pub fn collect_testbench_observability(
    source: &VerylTestbenchSource,
) -> (Vec<RuntimeEventSite>, FxHashSet<VarId>) {
    let Some(stmts) = source.initial_statements.as_ref() else {
        return Default::default();
    };
    let mut sites = Vec::new();
    collect_runtime_event_sites(stmts, &source.functions, &mut sites);
    let mut reads = FxHashSet::default();
    let mut active_functions = FxHashSet::default();
    collect_statement_reads(stmts, &source.functions, &mut active_functions, &mut reads);
    (sites, reads)
}

fn collect_statement_reads(
    stmts: &[Statement],
    funcs: &fxhash::FxHashMap<VarId, Function>,
    active_functions: &mut FxHashSet<VarId>,
    reads: &mut FxHashSet<VarId>,
) {
    for stmt in stmts {
        match stmt {
            Statement::Assign(assign) => {
                collect_expression_reads(&assign.expr, funcs, active_functions, reads);
                for dst in &assign.dst {
                    for expr in &dst.index.0 {
                        collect_expression_reads(expr, funcs, active_functions, reads);
                    }
                    collect_select_reads(&dst.select, funcs, active_functions, reads);
                }
            }
            Statement::If(stmt) => {
                collect_expression_reads(&stmt.cond, funcs, active_functions, reads);
                collect_statement_reads(&stmt.true_side, funcs, active_functions, reads);
                collect_statement_reads(&stmt.false_side, funcs, active_functions, reads);
            }
            Statement::IfReset(stmt) => {
                collect_statement_reads(&stmt.true_side, funcs, active_functions, reads);
                collect_statement_reads(&stmt.false_side, funcs, active_functions, reads);
            }
            Statement::Case(stmt) => {
                collect_expression_reads(&stmt.case_target, funcs, active_functions, reads);
                for arm in &stmt.arms {
                    for pattern in &arm.patterns {
                        match pattern {
                            CasePattern::Eq(expr) => {
                                collect_expression_reads(expr, funcs, active_functions, reads)
                            }
                            CasePattern::Range { lo, hi, .. } => {
                                collect_expression_reads(lo, funcs, active_functions, reads);
                                collect_expression_reads(hi, funcs, active_functions, reads);
                            }
                        }
                    }
                    collect_statement_reads(&arm.body, funcs, active_functions, reads);
                }
                collect_statement_reads(&stmt.default, funcs, active_functions, reads);
            }
            Statement::For(stmt) => {
                collect_for_bound_reads(&stmt.range, funcs, active_functions, reads);
                collect_statement_reads(&stmt.body, funcs, active_functions, reads);
            }
            Statement::SystemFunctionCall(call) => {
                collect_system_function_reads(&call.kind, funcs, active_functions, reads);
            }
            Statement::FunctionCall(call) => {
                collect_function_call_reads(call, funcs, active_functions, reads);
            }
            Statement::TbMethodCall(call) => match &call.method {
                TbMethod::ClockNext { count, period } => {
                    if let Some(expr) = count {
                        collect_expression_reads(expr, funcs, active_functions, reads);
                    }
                    if let Some(expr) = period {
                        collect_expression_reads(expr, funcs, active_functions, reads);
                    }
                }
                TbMethod::ResetAssert { duration, .. } => {
                    if let Some(expr) = duration {
                        collect_expression_reads(expr, funcs, active_functions, reads);
                    }
                }
                TbMethod::FileOpen { name, .. } => {
                    collect_expression_reads(&name.0, funcs, active_functions, reads);
                }
                TbMethod::FileWrite { args } => {
                    for arg in args {
                        collect_expression_reads(&arg.0, funcs, active_functions, reads);
                    }
                }
                TbMethod::FileClose | TbMethod::FileFlush => {}
            },
            Statement::Break | Statement::Unsupported(_) | Statement::Null => {}
        }
    }
}

fn collect_for_bound_reads(
    range: &ForRange,
    funcs: &fxhash::FxHashMap<VarId, Function>,
    active_functions: &mut FxHashSet<VarId>,
    reads: &mut FxHashSet<VarId>,
) {
    let (start, end) = match range {
        ForRange::Forward { start, end, .. }
        | ForRange::Reverse { start, end, .. }
        | ForRange::Stepped { start, end, .. } => (start, end),
    };
    for bound in [start, end] {
        if let ForBound::Expression(expr) = bound {
            collect_expression_reads(expr, funcs, active_functions, reads);
        }
    }
}

fn collect_expression_reads(
    expr: &Expression,
    funcs: &fxhash::FxHashMap<VarId, Function>,
    active_functions: &mut FxHashSet<VarId>,
    reads: &mut FxHashSet<VarId>,
) {
    match expr {
        Expression::Term(factor) => match factor.as_ref() {
            Factor::Variable(var_id, index, select, _) => {
                reads.insert(*var_id);
                for expr in &index.0 {
                    collect_expression_reads(expr, funcs, active_functions, reads);
                }
                collect_select_reads(select, funcs, active_functions, reads);
            }
            Factor::SystemFunctionCall(call) => {
                collect_system_function_reads(&call.kind, funcs, active_functions, reads);
            }
            Factor::FunctionCall(call) => {
                collect_function_call_reads(call, funcs, active_functions, reads);
            }
            Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => {}
        },
        Expression::Unary(_, inner, _) => {
            collect_expression_reads(inner, funcs, active_functions, reads);
        }
        Expression::Binary(lhs, _, rhs, _) => {
            collect_expression_reads(lhs, funcs, active_functions, reads);
            collect_expression_reads(rhs, funcs, active_functions, reads);
        }
        Expression::Ternary(cond, then_expr, else_expr, _) => {
            collect_expression_reads(cond, funcs, active_functions, reads);
            collect_expression_reads(then_expr, funcs, active_functions, reads);
            collect_expression_reads(else_expr, funcs, active_functions, reads);
        }
        Expression::Concatenation(parts, _) => {
            for (value, repeat) in parts {
                collect_expression_reads(value, funcs, active_functions, reads);
                if let Some(repeat) = repeat {
                    collect_expression_reads(repeat, funcs, active_functions, reads);
                }
            }
        }
        Expression::ArrayLiteral(items, _) => {
            for item in items {
                match item {
                    ArrayLiteralItem::Value(value, repeat) => {
                        collect_expression_reads(value, funcs, active_functions, reads);
                        if let Some(repeat) = repeat {
                            collect_expression_reads(repeat, funcs, active_functions, reads);
                        }
                    }
                    ArrayLiteralItem::Defaul(value) => {
                        collect_expression_reads(value, funcs, active_functions, reads);
                    }
                }
            }
        }
        Expression::StructConstructor(_, fields, _) => {
            for (_, value) in fields {
                collect_expression_reads(value, funcs, active_functions, reads);
            }
        }
    }
}

fn collect_select_reads(
    select: &veryl_analyzer::ir::VarSelect,
    funcs: &fxhash::FxHashMap<VarId, Function>,
    active_functions: &mut FxHashSet<VarId>,
    reads: &mut FxHashSet<VarId>,
) {
    for expr in &select.0 {
        collect_expression_reads(expr, funcs, active_functions, reads);
    }
    if let Some((_, expr)) = &select.1 {
        collect_expression_reads(expr, funcs, active_functions, reads);
    }
}

fn collect_system_function_reads(
    kind: &SystemFunctionKind,
    funcs: &fxhash::FxHashMap<VarId, Function>,
    active_functions: &mut FxHashSet<VarId>,
    reads: &mut FxHashSet<VarId>,
) {
    let mut collect_input = |input: &SystemFunctionInput| {
        collect_expression_reads(&input.0, funcs, active_functions, reads);
    };
    match kind {
        SystemFunctionKind::Bits(input)
        | SystemFunctionKind::Size(input)
        | SystemFunctionKind::Clog2(input)
        | SystemFunctionKind::Onehot(input)
        | SystemFunctionKind::Signed(input)
        | SystemFunctionKind::Unsigned(input) => collect_input(input),
        SystemFunctionKind::Readmemh(input, _) => collect_input(input),
        SystemFunctionKind::Display(inputs) | SystemFunctionKind::Write(inputs) => {
            for input in inputs {
                collect_input(input);
            }
        }
        SystemFunctionKind::Assert { cond, args, .. } => {
            collect_input(cond);
            for input in args {
                collect_input(input);
            }
        }
        SystemFunctionKind::Finish => {}
    }
}

fn collect_function_call_reads(
    call: &FunctionCall,
    funcs: &fxhash::FxHashMap<VarId, Function>,
    active_functions: &mut FxHashSet<VarId>,
    reads: &mut FxHashSet<VarId>,
) {
    for expr in call.inputs.values() {
        collect_expression_reads(expr, funcs, active_functions, reads);
    }
    for destinations in call.outputs.values() {
        for dst in destinations {
            for expr in &dst.index.0 {
                collect_expression_reads(expr, funcs, active_functions, reads);
            }
            collect_select_reads(&dst.select, funcs, active_functions, reads);
        }
    }
    if active_functions.insert(call.id) {
        if let Some(body) = function_body(funcs, call) {
            collect_statement_reads(&body.statements, funcs, active_functions, reads);
        }
        active_functions.remove(&call.id);
    }
}

fn compile_assert_message(
    args: &[SystemFunctionInput],
    ec: &ExprCompiler<'_>,
) -> Option<GenericAssertMessage<SemanticArgument<StateAddr>>> {
    if args.is_empty() {
        return None;
    }
    if let Some(template) = static_string_expr(&args[0].0) {
        let compiled_args = args[1..]
            .iter()
            .map(|arg| compile_assert_arg(arg, ec))
            .collect::<Vec<_>>();
        Some(GenericAssertMessage::Formatted {
            template,
            args: compiled_args,
        })
    } else {
        Some(GenericAssertMessage::DynamicArgs(
            args.iter().map(|arg| compile_assert_arg(arg, ec)).collect(),
        ))
    }
}

fn lower_testbench_operator(op: VerylOp) -> Op {
    match op {
        VerylOp::Add => Op::Add,
        VerylOp::Sub => Op::Sub,
        VerylOp::Mul => Op::Mul,
        VerylOp::Div => Op::Div,
        VerylOp::Rem => Op::Rem,
        VerylOp::Pow => Op::Pow,
        VerylOp::BitAnd => Op::BitAnd,
        VerylOp::BitOr => Op::BitOr,
        VerylOp::BitXor => Op::BitXor,
        VerylOp::BitXnor => Op::BitXnor,
        VerylOp::BitNand => Op::BitNand,
        VerylOp::BitNor => Op::BitNor,
        VerylOp::LogicShiftL => Op::LogicShiftL,
        VerylOp::LogicShiftR => Op::LogicShiftR,
        VerylOp::ArithShiftL => Op::ArithShiftL,
        VerylOp::ArithShiftR => Op::ArithShiftR,
        VerylOp::Eq => Op::Eq,
        VerylOp::EqWildcard => Op::EqWildcard,
        VerylOp::Ne => Op::Ne,
        VerylOp::NeWildcard => Op::NeWildcard,
        VerylOp::Less => Op::Less,
        VerylOp::LessEq => Op::LessEq,
        VerylOp::Greater => Op::Greater,
        VerylOp::GreaterEq => Op::GreaterEq,
        VerylOp::LogicAnd => Op::LogicAnd,
        VerylOp::LogicOr => Op::LogicOr,
        VerylOp::LogicNot => Op::LogicNot,
        VerylOp::BitNot => Op::BitNot,
        _ => unreachable!("operator cannot be represented in testbench bytecode: {op:?}"),
    }
}

struct ExprCompiler<'a> {
    lookup: &'a VerylFrontendLookup,
    testbench_source: &'a VerylTestbenchSource,
}

impl ExprCompiler<'_> {
    fn compile(&self, expr: &Expression) -> ExprBytecode<StateLocation<StateAddr>> {
        let mut ops = Vec::new();
        self.emit(expr, &mut ops);
        ExprBytecode::new(ops)
    }

    fn compile_with_width(
        &self,
        expr: &Expression,
        width: usize,
    ) -> ExprBytecode<StateLocation<StateAddr>> {
        let mut ops = Vec::new();
        self.emit_in_context(
            expr,
            &mut ops,
            Some(ValueContext {
                width,
                signed: expression_signed(expr),
            }),
        );
        ExprBytecode::new(ops)
    }

    fn natural_width(&self, expr: &Expression) -> usize {
        get_expr_width(expr)
            .filter(|width| *width != 0)
            .unwrap_or_else(|| self.infer_expr_width(expr).max(1))
    }

    fn root_context(&self, expr: &Expression) -> ValueContext {
        ValueContext {
            width: self
                .natural_width(expr)
                .max(expr.comptime().expr_context.width),
            signed: expression_signed(expr),
        }
    }

    fn emit(&self, expr: &Expression, ops: &mut Vec<UnboundTbOpcode>) {
        self.emit_in_context(expr, ops, Some(self.root_context(expr)));
    }

    fn resize_result(
        &self,
        ops: &mut Vec<UnboundTbOpcode>,
        source: ValueContext,
        target: Option<ValueContext>,
    ) -> ValueContext {
        let Some(target) = target else {
            return source;
        };
        if source.width != target.width {
            ops.push(TbOpcode::Resize {
                source_width: source.width,
                target_width: target.width,
                signed: target.signed,
            });
        }
        target
    }

    fn emit_in_context(
        &self,
        expr: &Expression,
        ops: &mut Vec<UnboundTbOpcode>,
        context: Option<ValueContext>,
    ) -> ValueContext {
        match expr {
            Expression::Term(f) => {
                self.emit_factor(f, ops);
                let source = ValueContext {
                    width: self.natural_width(expr),
                    signed: expression_signed(expr),
                };
                self.resize_result(ops, source, context)
            }
            Expression::Unary(op, inner, _) => {
                let reduction = matches!(
                    op,
                    VerylOp::BitAnd
                        | VerylOp::BitNand
                        | VerylOp::BitOr
                        | VerylOp::BitNor
                        | VerylOp::BitXor
                        | VerylOp::BitXnor
                        | VerylOp::LogicNot
                );
                let natural_width = self.natural_width(inner);
                let operand_context = if reduction {
                    None
                } else {
                    Some(ValueContext {
                        width: natural_width.max(context.map(|x| x.width).unwrap_or(0)),
                        signed: context
                            .map(|x| x.signed)
                            .unwrap_or_else(|| expression_signed(inner)),
                    })
                };
                let operand = self.emit_in_context(inner, ops, operand_context);
                let result_width = if reduction { 1 } else { operand.width };
                match op {
                    VerylOp::Add
                    | VerylOp::Sub
                    | VerylOp::BitNot
                    | VerylOp::BitAnd
                    | VerylOp::BitNand
                    | VerylOp::BitOr
                    | VerylOp::BitNor
                    | VerylOp::BitXor
                    | VerylOp::BitXnor
                    | VerylOp::LogicNot => ops.push(TbOpcode::TypedUnary {
                        op: lower_testbench_operator(*op),
                        operand_width: operand.width,
                        result_width,
                    }),
                    _ => unreachable!("operator is not unary in a testbench expression: {op:?}"),
                }
                let result = ValueContext {
                    width: result_width,
                    signed: if reduction { false } else { operand.signed },
                };
                self.resize_result(ops, result, context)
            }
            Expression::Binary(lhs, op, rhs, _) => {
                if matches!(op, VerylOp::As) {
                    let cast = cast_semantics(lhs, rhs)
                        .expect("analyzed testbench cast must have a concrete target");
                    let source = self.emit_in_context(
                        lhs,
                        ops,
                        Some(ValueContext {
                            width: cast.width,
                            signed: cast.source_signed,
                        }),
                    );
                    let casted = ValueContext {
                        width: source.width,
                        signed: cast.result_signed,
                    };
                    return self.resize_result(ops, casted, context);
                }

                let lhs_width = self.natural_width(lhs);
                let rhs_width = self.natural_width(rhs);
                let semantics = binary_semantics(
                    *op,
                    lhs_width,
                    rhs_width,
                    expression_signed(lhs),
                    expression_signed(rhs),
                    context,
                );
                let lhs = self.emit_in_context(lhs, ops, semantics.lhs_context);
                let rhs = self.emit_in_context(rhs, ops, semantics.rhs_context);
                match op {
                    VerylOp::Pow
                    | VerylOp::Div
                    | VerylOp::Rem
                    | VerylOp::Mul
                    | VerylOp::Add
                    | VerylOp::Sub
                    | VerylOp::ArithShiftL
                    | VerylOp::ArithShiftR
                    | VerylOp::LogicShiftL
                    | VerylOp::LogicShiftR
                    | VerylOp::LessEq
                    | VerylOp::GreaterEq
                    | VerylOp::Less
                    | VerylOp::Greater
                    | VerylOp::Eq
                    | VerylOp::EqWildcard
                    | VerylOp::Ne
                    | VerylOp::NeWildcard
                    | VerylOp::BitAnd
                    | VerylOp::BitOr
                    | VerylOp::BitXor
                    | VerylOp::BitXnor
                    | VerylOp::LogicAnd
                    | VerylOp::LogicOr => ops.push(TbOpcode::TypedBinOp {
                        op: lower_testbench_operator(*op),
                        lhs_width: lhs.width,
                        rhs_width: rhs.width,
                        result_width: semantics.result_width,
                        lhs_signed: semantics.lhs_signed,
                        rhs_signed: semantics.rhs_signed,
                    }),
                    _ => unreachable!("operator is not binary in a testbench expression: {op:?}"),
                }
                let result = ValueContext {
                    width: semantics.result_width,
                    signed: semantics.result_signed,
                };
                self.resize_result(ops, result, context)
            }
            Expression::Ternary(cond, then_expr, else_expr, _) => {
                self.emit_in_context(cond, ops, None);
                let branch_context = ValueContext {
                    width: self
                        .natural_width(then_expr)
                        .max(self.natural_width(else_expr))
                        .max(context.map(|x| x.width).unwrap_or(0)),
                    signed: context.map(|x| x.signed).unwrap_or_else(|| {
                        expression_signed(then_expr) && expression_signed(else_expr)
                    }),
                };
                let mut then_ops = Vec::new();
                self.emit_in_context(then_expr, &mut then_ops, Some(branch_context));
                let mut else_ops = Vec::new();
                self.emit_in_context(else_expr, &mut else_ops, Some(branch_context));
                ops.push(TbOpcode::Ternary {
                    then_len: then_ops.len(),
                    else_len: else_ops.len(),
                });
                ops.extend(then_ops);
                ops.extend(else_ops);
                branch_context
            }
            Expression::Concatenation(parts, _) => {
                ops.push(TbOpcode::ConstU64(0));
                let mut result_width = 0usize;
                for (val_expr, repeat_expr) in parts {
                    let part_width = self.natural_width(val_expr);
                    let repeat = repeat_expr
                        .as_ref()
                        .and_then(Self::try_const_usize)
                        .unwrap_or(1);
                    for _ in 0..repeat {
                        self.emit_in_context(val_expr, ops, None);
                        result_width = result_width.saturating_add(part_width);
                        ops.push(TbOpcode::ConcatPart {
                            part_width,
                            result_width,
                        });
                    }
                }
                self.resize_result(
                    ops,
                    ValueContext {
                        width: result_width.max(1),
                        signed: false,
                    },
                    context,
                )
            }
            Expression::ArrayLiteral(..) | Expression::StructConstructor(..) => {
                unreachable!("aggregate testbench expressions must be lowered before bytecode")
            }
        }
    }

    fn emit_factor(&self, factor: &Factor, ops: &mut Vec<UnboundTbOpcode>) {
        match factor {
            Factor::Variable(var_id, index, select, comptime) => {
                if comptime.is_const
                    && let Ok(value) = comptime.get_value()
                {
                    self.emit_constant_value(value, ops);
                } else if let Some(sig) = self.resolve_var(var_id) {
                    self.emit_var_access(var_id, sig, index, select, ops);
                } else if let Ok(value) = comptime.get_value() {
                    self.emit_constant_value(value, ops);
                } else {
                    unreachable!("unresolved non-constant testbench variable {var_id:?}");
                }
            }
            Factor::Value(comptime) => {
                if let Ok(val) = comptime.get_value() {
                    self.emit_constant_value(val, ops);
                } else {
                    unreachable!("analyzed testbench literal has no value");
                }
            }
            Factor::FunctionCall(fc) => {
                self.emit_function_call(fc, ops);
            }
            Factor::SystemFunctionCall(call) => match &call.kind {
                SystemFunctionKind::Signed(input) | SystemFunctionKind::Unsigned(input) => {
                    self.emit(&input.0, ops);
                }
                _ => {
                    let value = call
                        .comptime
                        .get_value()
                        .expect("testbench system function must be compile-time evaluable");
                    self.emit_constant_value(value, ops);
                }
            },
            Factor::Anonymous(comptime) | Factor::Unknown(comptime) => {
                let value = comptime
                    .get_value()
                    .expect("resolved testbench factor must have a value");
                self.emit_constant_value(value, ops);
            }
        }
    }

    fn emit_constant_value(
        &self,
        value: &veryl_analyzer::value::Value,
        ops: &mut Vec<UnboundTbOpcode>,
    ) {
        if value.width() <= 64 {
            ops.push(TbOpcode::ConstU64(value.payload_u64()));
        } else {
            ops.push(TbOpcode::ConstWide(value.payload().into_owned()));
        }
    }

    /// Emit bytecode for a function call used as an expression value.
    /// Inline-expands: store args → emit body assigns → load return value.
    fn emit_function_call(
        &self,
        fc: &veryl_analyzer::ir::FunctionCall,
        ops: &mut Vec<UnboundTbOpcode>,
    ) {
        let func = match self.testbench_source.functions.get(&fc.id) {
            Some(f) => f,
            None => {
                ops.push(TbOpcode::ConstU64(0));
                return;
            }
        };
        let func_body = match if let Some(idx) = &fc.index {
            func.get_function(idx)
        } else {
            func.get_function(&[])
        } {
            Some(fb) => fb,
            None => {
                ops.push(TbOpcode::ConstU64(0));
                return;
            }
        };

        // 1. Store input arguments into memory
        for (arg_path, arg_expr) in &fc.inputs {
            if let Some(&arg_var_id) = func_body.arg_map.get(arg_path) {
                if let Some(sig) = self.resolve_var(&arg_var_id) {
                    self.emit_in_context(
                        arg_expr,
                        ops,
                        Some(ValueContext {
                            width: sig.width,
                            signed: expression_signed(arg_expr),
                        }),
                    );
                    ops.push(TbOpcode::StoreU64 {
                        location: self.state_location(arg_var_id, 0),
                        byte_size: get_byte_size(sig.width),
                    });
                }
            }
        }

        // 2. Emit body statements as bytecode (only Assign is supported)
        for stmt in &func_body.statements {
            if let veryl_analyzer::ir::Statement::Assign(a) = stmt {
                if let Some(first_dst) = a.dst.first() {
                    if let Some(dst_sig) = self.resolve_var(&first_dst.id) {
                        self.emit_in_context(
                            &a.expr,
                            ops,
                            Some(ValueContext {
                                width: dst_sig.width,
                                signed: expression_signed(&a.expr),
                            }),
                        );
                        ops.push(TbOpcode::StoreU64 {
                            location: self.state_location(first_dst.id, 0),
                            byte_size: get_byte_size(dst_sig.width),
                        });
                    }
                }
            }
            // Non-assign statements (if/for in function body) are skipped.
            // This covers the common case of pure computation functions.
        }

        // 3. Load return value
        if let Some(ret_var_id) = &func_body.ret {
            if let Some(sig) = self.resolve_var(ret_var_id) {
                self.emit_load(*ret_var_id, 0, sig.width, ops);
            } else {
                ops.push(TbOpcode::ConstU64(0));
            }
        } else {
            ops.push(TbOpcode::ConstU64(0));
        }
    }

    /// Emit bytecode for a variable access, handling static and dynamic
    /// array indices and bit selects.
    fn emit_var_access(
        &self,
        var_id: &VarId,
        sig: SemanticSignal<StateAddr>,
        index: &veryl_analyzer::ir::VarIndex,
        select: &veryl_analyzer::ir::VarSelect,
        ops: &mut Vec<UnboundTbOpcode>,
    ) {
        let info = match self.lookup.root_variable(*var_id) {
            Some((_, info)) => info,
            None => {
                self.emit_load(*var_id, 0, sig.width, ops);
                return;
            }
        };

        // No index or select → whole variable
        if index.0.is_empty() && select.0.is_empty() && select.1.is_none() {
            self.emit_load(*var_id, 0, sig.width, ops);
            return;
        }

        let array_total: usize = info.array_dims.iter().product::<usize>().max(1);
        let element_width = info.width / array_total;

        // Compute array strides
        let mut strides_bits = vec![element_width; info.array_dims.len()];
        if !info.array_dims.is_empty() {
            let mut stride = element_width;
            for i in (0..info.array_dims.len()).rev() {
                strides_bits[i] = stride;
                stride *= info.array_dims[i];
            }
        }

        // Process unpacked array indices
        let mut static_bit_offset: usize = 0;
        let mut dynamic_emitted = false;

        for (i, idx_expr) in index.0.iter().enumerate() {
            if i >= info.array_dims.len() {
                break;
            }
            let stride = strides_bits[i];

            if let Some(idx_val) = Self::try_const_usize(idx_expr) {
                // Static index: accumulate into offset
                static_bit_offset += idx_val * stride;
            } else {
                // Dynamic index: emit the index expression, then LoadIndexed
                let base_byte_offset = static_bit_offset / 8;
                let stride_bytes = get_byte_size(stride);
                let elem_byte_size = get_byte_size(element_width);
                self.emit(idx_expr, ops);
                ops.push(TbOpcode::LoadIndexed {
                    location: self.state_location(*var_id, base_byte_offset),
                    stride_bytes,
                    element_byte_size: elem_byte_size,
                    element_width,
                });
                dynamic_emitted = true;
                // After a dynamic index, remaining indices would need chaining.
                // For now, only single dynamic index is supported.
                break;
            }
        }

        if dynamic_emitted {
            // Apply bit select on top of dynamic result if present
            if select.1.is_some() || !select.0.is_empty() {
                self.emit_post_select(select, element_width, ops);
            }
            return;
        }

        // All indices were static — apply bit select
        let accessed_width = if index.0.len() >= info.array_dims.len() {
            element_width
        } else if index.0.is_empty() {
            info.width
        } else {
            strides_bits[index.0.len() - 1]
        };

        if select.0.is_empty() && select.1.is_none() {
            // No bit select, just load the element
            let byte_offset = static_bit_offset / 8;
            let sub = static_bit_offset % 8;
            if sub == 0 {
                self.emit_load(*var_id, byte_offset, accessed_width, ops);
            } else {
                let load_width = accessed_width + sub;
                self.emit_load(*var_id, byte_offset, load_width, ops);
                ops.push(TbOpcode::ConstU64(sub as u64));
                ops.push(TbOpcode::BinOp(Op::LogicShiftR));
                if accessed_width < 64 {
                    ops.push(TbOpcode::ConstU64((1u64 << accessed_width) - 1));
                    ops.push(TbOpcode::BinOp(Op::BitAnd));
                }
            }
            return;
        }

        // Static bit select
        let (sel_lsb, sel_width, is_dynamic_select) = self.resolve_select(select, ops);

        if is_dynamic_select {
            // Dynamic bit select: load full value, shift by dynamic amount, mask
            let byte_offset = static_bit_offset / 8;
            let total_byte_size = get_byte_size(accessed_width);
            ops.push(TbOpcode::LoadBitSelect {
                location: self.state_location(*var_id, byte_offset),
                base_byte_size: total_byte_size,
                select_width: sel_width,
            });
            return;
        }

        let bit_offset = static_bit_offset + sel_lsb;
        let byte_offset = bit_offset / 8;
        let sub = bit_offset % 8;
        if sub == 0 {
            self.emit_load(*var_id, byte_offset, sel_width, ops);
        } else {
            let load_width = sel_width + sub;
            self.emit_load(*var_id, byte_offset, load_width, ops);
            ops.push(TbOpcode::ConstU64(sub as u64));
            ops.push(TbOpcode::BinOp(Op::LogicShiftR));
            if sel_width < 64 {
                ops.push(TbOpcode::ConstU64((1u64 << sel_width) - 1));
                ops.push(TbOpcode::BinOp(Op::BitAnd));
            }
        }
    }

    /// Resolve a VarSelect to `(lsb, width, is_dynamic)`.
    /// If any index is dynamic, emits the dynamic index expression to `ops`
    /// and returns `is_dynamic = true`.
    fn resolve_select(
        &self,
        select: &veryl_analyzer::ir::VarSelect,
        ops: &mut Vec<UnboundTbOpcode>,
    ) -> (usize, usize, bool) {
        if let Some((op, range_expr)) = &select.1 {
            let anchor_expr = select.0.last();
            let anchor = anchor_expr.and_then(Self::try_const_usize);
            let range_val = Self::try_const_usize(range_expr);

            if let (Some(a), Some(v)) = (anchor, range_val) {
                let (lsb, msb) = match op {
                    veryl_analyzer::ir::VarSelectOp::Colon => (v, a),
                    veryl_analyzer::ir::VarSelectOp::PlusColon => (a, a + v - 1),
                    veryl_analyzer::ir::VarSelectOp::MinusColon => (a.saturating_sub(v) + 1, a),
                    veryl_analyzer::ir::VarSelectOp::Step => (a * v, (a + 1) * v - 1),
                };
                return (lsb, msb - lsb + 1, false);
            }

            // Dynamic select: emit the anchor expression
            if let Some(anchor_expr) = anchor_expr {
                self.emit(anchor_expr, ops);
            } else {
                ops.push(TbOpcode::ConstU64(0));
            }
            let width = range_val.unwrap_or(1);
            return (0, width, true);
        }

        // Simple bit index (no range)
        if let Some(first) = select.0.first() {
            if let Some(idx) = Self::try_const_usize(first) {
                return (idx, 1, false);
            }
            // Dynamic single bit select
            self.emit(first, ops);
            return (0, 1, true);
        }

        (0, 0, false)
    }

    /// Emit post-load bit select operations on a value already on the stack
    /// (for dynamic array element access followed by bit select).
    fn emit_post_select(
        &self,
        select: &veryl_analyzer::ir::VarSelect,
        _base_width: usize,
        ops: &mut Vec<UnboundTbOpcode>,
    ) {
        let (lsb, width, is_dynamic) = self.resolve_select(select, ops);
        if is_dynamic {
            // Stack: [value, bit_index]
            ops.push(TbOpcode::BinOp(Op::LogicShiftR));
            if width < 64 {
                ops.push(TbOpcode::ConstU64((1u64 << width) - 1));
                ops.push(TbOpcode::BinOp(Op::BitAnd));
            }
        } else if lsb > 0 || width > 0 {
            if lsb > 0 {
                ops.push(TbOpcode::ConstU64(lsb as u64));
                ops.push(TbOpcode::BinOp(Op::LogicShiftR));
            }
            if width > 0 && width < 64 {
                ops.push(TbOpcode::ConstU64((1u64 << width) - 1));
                ops.push(TbOpcode::BinOp(Op::BitAnd));
            }
        }
    }

    /// Emit a LoadU64 or LoadWide opcode for the given byte offset and bit width.
    fn emit_load(
        &self,
        var_id: VarId,
        byte_offset: usize,
        width: usize,
        ops: &mut Vec<UnboundTbOpcode>,
    ) {
        let byte_size = get_byte_size(width);
        if byte_size <= 8 {
            let mask = if width >= 64 {
                u64::MAX
            } else {
                (1u64 << width) - 1
            };
            ops.push(TbOpcode::LoadU64 {
                location: self.state_location(var_id, byte_offset),
                byte_size,
                mask,
            });
        } else {
            ops.push(TbOpcode::LoadWide {
                location: self.state_location(var_id, byte_offset),
                byte_size,
                width,
            });
        }
    }

    fn state_location(&self, var_id: VarId, byte_offset: usize) -> StateLocation<StateAddr> {
        StateLocation {
            address: self
                .lookup
                .root_variable(var_id)
                .map(|(address, _)| address)
                .expect("frontend state projection is complete"),
            byte_offset,
        }
    }

    /// Resolve VarIndex (unpacked array) and VarSelect (bit select) to
    /// a concrete (byte_offset, bit_width) pair.
    ///
    /// For static indices, adjusts the offset and narrows the width.
    /// Dynamic indices are not supported and fall back to the full variable.
    /// Infer the bit width of an expression. Falls back to comptime if available,
    /// otherwise resolves from VariableInfo for variables.
    fn infer_expr_width(&self, expr: &Expression) -> usize {
        let ctx_width = expr.comptime().expr_context.width;
        if ctx_width > 0 {
            return ctx_width;
        }
        // Try type-level width
        if let Some(w) = expr.comptime().r#type.total_width() {
            if w > 0 {
                return w;
            }
        }
        // For terms, look up variable info
        if let Expression::Term(f) = expr {
            match f.as_ref() {
                Factor::Variable(var_id, _, _, _) => {
                    if let Some((_, info)) = self.lookup.root_variable(*var_id) {
                        return info.width;
                    }
                }
                Factor::Value(c) => {
                    if let Ok(v) = c.get_value() {
                        return v.width();
                    }
                }
                _ => {}
            }
        }
        0
    }

    fn try_const_usize(expr: &Expression) -> Option<usize> {
        match expr {
            Expression::Term(f) => match f.as_ref() {
                Factor::Value(c) => c.get_value().ok().map(|v| v.payload_u64() as usize),
                _ => None,
            },
            _ => None,
        }
    }

    fn resolve_var(&self, var_id: &VarId) -> Option<SemanticSignal<StateAddr>> {
        let (address, info) = self.lookup.root_variable(*var_id)?;
        Some(SemanticSignal {
            address,
            width: info.width,
        })
    }
}

// ── Builder ────────────────────────────────────────────────────────────

struct SemanticTestbenchBuilder<'a> {
    lookup: &'a VerylFrontendLookup,
    testbench_source: &'a VerylTestbenchSource,
    runtime_event_site_count: usize,
    event_map: std::collections::HashMap<StrId, StateAddr>,
    signal_map: std::collections::HashMap<StrId, SemanticSignal<StateAddr>>,
    default_reset_duration: u64,
}

impl<'a> SemanticTestbenchBuilder<'a> {
    fn new(
        lookup: &'a VerylFrontendLookup,
        testbench_source: &'a VerylTestbenchSource,
        runtime_event_site_count: usize,
    ) -> Self {
        Self {
            lookup,
            testbench_source,
            runtime_event_site_count,
            event_map: Default::default(),
            signal_map: Default::default(),
            default_reset_duration: 3,
        }
    }

    fn build_event_map(&mut self, stmts: &[Statement]) {
        let mut clock_insts: Vec<StrId> = Vec::new();
        let mut reset_insts: Vec<StrId> = Vec::new();
        Self::scan_tb_methods(stmts, &mut clock_insts, &mut reset_insts);
        for inst in clock_insts.iter().chain(reset_insts.iter()) {
            if let Some((addr, info)) = self.lookup.root_named_variable(*inst) {
                self.event_map.insert(*inst, addr);
                self.signal_map.insert(
                    *inst,
                    SemanticSignal {
                        address: addr,
                        width: info.width,
                    },
                );
            }
        }
    }

    fn scan_tb_methods(stmts: &[Statement], clks: &mut Vec<StrId>, rsts: &mut Vec<StrId>) {
        for stmt in stmts {
            match stmt {
                Statement::TbMethodCall(tb) => match &tb.method {
                    TbMethod::ClockNext { .. } => {
                        if !clks.contains(&tb.inst) {
                            clks.push(tb.inst);
                        }
                    }
                    TbMethod::ResetAssert { clock, .. } => {
                        if !rsts.contains(&tb.inst) {
                            rsts.push(tb.inst);
                        }
                        if !clks.contains(clock) {
                            clks.push(*clock);
                        }
                    }
                    TbMethod::FileOpen { .. }
                    | TbMethod::FileWrite { .. }
                    | TbMethod::FileClose
                    | TbMethod::FileFlush => {}
                },
                Statement::If(s) => {
                    Self::scan_tb_methods(&s.true_side, clks, rsts);
                    Self::scan_tb_methods(&s.false_side, clks, rsts);
                }
                Statement::For(s) => Self::scan_tb_methods(&s.body, clks, rsts),
                _ => {}
            }
        }
    }

    fn convert(&mut self, stmts: &[Statement]) -> Vec<SemanticStatement<StateAddr>> {
        let ec = ExprCompiler {
            lookup: self.lookup,
            testbench_source: self.testbench_source,
        };
        let site_count = count_assert_statements(stmts, &self.testbench_source.functions) as u32;
        let mut next_assert_site_id = self
            .runtime_event_site_count
            .saturating_sub(site_count as usize) as u32;
        stmts
            .iter()
            .filter_map(|s| self.convert_stmt(s, &ec, &mut next_assert_site_id))
            .collect()
    }

    fn convert_stmt(
        &self,
        stmt: &Statement,
        ec: &ExprCompiler<'_>,
        next_assert_site_id: &mut u32,
    ) -> Option<SemanticStatement<StateAddr>> {
        fn convert_for_bound(
            bound: &ForBound,
            ec: &ExprCompiler<'_>,
        ) -> GenericLoopBound<ExprBytecode<StateLocation<StateAddr>>> {
            match bound {
                ForBound::Const(x) => GenericLoopBound::Static(*x),
                ForBound::Expression(expr) => GenericLoopBound::Dynamic {
                    expr: ec.compile(expr.as_ref()),
                    width: ec.root_context(expr).width,
                    signed: expression_signed(expr),
                },
            }
        }

        match stmt {
            Statement::TbMethodCall(tb) => self.convert_tb_method(tb, ec),
            Statement::SystemFunctionCall(sf) => match &sf.kind {
                SystemFunctionKind::Assert { kind, cond, args } => {
                    let site_id = *next_assert_site_id;
                    *next_assert_site_id = next_assert_site_id.saturating_add(1);
                    Some(GenericTestbenchStatement::Assert {
                        expr: ec.compile(&cond.0),
                        site_id,
                        continue_on_fail: matches!(kind, AssertKind::Continue),
                        message: compile_assert_message(args, ec),
                        location: extract_source_location(&sf.comptime.token),
                    })
                }
                SystemFunctionKind::Finish => Some(GenericTestbenchStatement::Finish),
                SystemFunctionKind::Display(args) => Some(GenericTestbenchStatement::Display {
                    message: compile_assert_message(args, ec),
                    newline: true,
                }),
                SystemFunctionKind::Write(args) => Some(GenericTestbenchStatement::Display {
                    message: compile_assert_message(args, ec),
                    newline: false,
                }),
                _ => None,
            },
            Statement::If(s) => Some(GenericTestbenchStatement::If {
                expr: ec.compile(&s.cond),
                then_block: s
                    .true_side
                    .iter()
                    .filter_map(|s| self.convert_stmt(s, ec, next_assert_site_id))
                    .collect(),
                else_block: s
                    .false_side
                    .iter()
                    .filter_map(|s| self.convert_stmt(s, ec, next_assert_site_id))
                    .collect(),
            }),
            Statement::For(s) => {
                let body: Vec<_> = s
                    .body
                    .iter()
                    .filter_map(|s| self.convert_stmt(s, ec, next_assert_site_id))
                    .collect();
                let lv = self
                    .resolve_loop_var(&s.var_id)
                    .map(|(sig, width)| (sig, width, s.var_type.signed));
                match &s.range {
                    ForRange::Forward {
                        start,
                        end,
                        inclusive,
                        step,
                    } => Some(GenericTestbenchStatement::For {
                        loop_var: lv,
                        start: convert_for_bound(start, ec),
                        end: convert_for_bound(end, ec),
                        inclusive: *inclusive,
                        step: *step,
                        step_op: None,
                        reverse: false,
                        body,
                    }),
                    ForRange::Reverse {
                        start,
                        end,
                        inclusive,
                        step,
                    } => Some(GenericTestbenchStatement::For {
                        loop_var: lv,
                        start: convert_for_bound(start, ec),
                        end: convert_for_bound(end, ec),
                        inclusive: *inclusive,
                        step: *step,
                        step_op: None,
                        reverse: true,
                        body,
                    }),
                    ForRange::Stepped {
                        start,
                        end,
                        inclusive,
                        step,
                        op,
                    } => Some(GenericTestbenchStatement::For {
                        loop_var: lv,
                        start: convert_for_bound(start, ec),
                        end: convert_for_bound(end, ec),
                        inclusive: *inclusive,
                        step: *step,
                        step_op: Some(lower_testbench_operator(*op)),
                        reverse: false,
                        body,
                    }),
                }
            }
            Statement::Assign(a) => a
                .dst
                .first()
                .and_then(|d| ec.resolve_var(&d.id))
                .map(|dst| GenericTestbenchStatement::Assign {
                    dst,
                    expr: ec.compile_with_width(&a.expr, dst.width),
                }),
            Statement::Break => Some(GenericTestbenchStatement::Break),
            Statement::FunctionCall(fc) => self.convert_function_call(fc, ec, next_assert_site_id),
            _ => None,
        }
    }

    /// Inline-expand a function call by binding arguments and converting
    /// the function body's statements.
    fn convert_function_call(
        &self,
        fc: &veryl_analyzer::ir::FunctionCall,
        ec: &ExprCompiler<'_>,
        next_assert_site_id: &mut u32,
    ) -> Option<SemanticStatement<StateAddr>> {
        let func = self.testbench_source.functions.get(&fc.id)?;
        let func_body = if let Some(idx) = &fc.index {
            func.get_function(idx)?
        } else {
            func.get_function(&[])?
        };

        // Build a list of statements: argument assignments + body
        let mut stmts: Vec<SemanticStatement<StateAddr>> = Vec::new();

        // Bind input arguments
        for (arg_path, arg_expr) in &fc.inputs {
            if let Some(&arg_var_id) = func_body.arg_map.get(arg_path) {
                if let Some(sig) = ec.resolve_var(&arg_var_id) {
                    stmts.push(GenericTestbenchStatement::Assign {
                        dst: sig,
                        expr: ec.compile_with_width(arg_expr, sig.width),
                    });
                }
            }
        }

        // Inline body statements
        for stmt in &func_body.statements {
            if let Some(ts) = self.convert_stmt(stmt, ec, next_assert_site_id) {
                stmts.push(ts);
            }
        }

        if stmts.len() == 1 {
            Some(stmts.into_iter().next().unwrap())
        } else {
            // Wrap multiple statements into an If(true) block as a sequence container
            // (there's no "Block" variant in TestbenchStatement)
            // Actually, we can return None and use a different approach:
            // flatten into the parent's statement list.
            // For now, wrap in an always-true If:
            Some(GenericTestbenchStatement::If {
                expr: ExprBytecode::new(vec![TbOpcode::ConstU64(1)]),
                then_block: stmts,
                else_block: Vec::new(),
            })
        }
    }

    fn convert_tb_method(
        &self,
        tb: &TbMethodCall,
        ec: &ExprCompiler<'_>,
    ) -> Option<SemanticStatement<StateAddr>> {
        match &tb.method {
            TbMethod::ClockNext { count, .. } => {
                let ev = self.event_map.get(&tb.inst).copied()?;
                let clock_count = match count {
                    Some(expr) => {
                        if let Some(n) = try_eval_const(expr) {
                            GenericClockCount::Static(n)
                        } else {
                            GenericClockCount::Dynamic(ec.compile(expr))
                        }
                    }
                    None => GenericClockCount::Static(1),
                };
                Some(GenericTestbenchStatement::ClockNext {
                    clock_event: ev,
                    count: clock_count,
                })
            }
            TbMethod::ResetAssert { clock, duration } => {
                let reset_signal = self.signal_map.get(&tb.inst).copied()?;
                let clock_event = self.event_map.get(clock).copied()?;
                let dur = duration
                    .as_ref()
                    .and_then(try_eval_const)
                    .unwrap_or(self.default_reset_duration);
                // Determine reset polarity from the variable's DomainKind
                let (assert_value, deassert_value) = self.resolve_reset_polarity(&tb.inst);
                Some(GenericTestbenchStatement::ResetAssert {
                    reset_signal,
                    clock_event,
                    duration: dur,
                    assert_value,
                    deassert_value,
                })
            }
            TbMethod::FileOpen { .. }
            | TbMethod::FileWrite { .. }
            | TbMethod::FileClose
            | TbMethod::FileFlush => None,
        }
    }

    /// Determine reset assert/deassert values from the variable's PortTypeKind.
    /// PortTypeKind covers all four reset types (async/sync × high/low),
    /// unlike DomainKind which maps sync resets to Other.
    fn resolve_reset_polarity(&self, inst: &StrId) -> (u8, u8) {
        if let Some((_, info)) = self.lookup.root_named_variable(*inst) {
            return match info.type_kind {
                PortTypeKind::ResetAsyncHigh | PortTypeKind::ResetSyncHigh => (1, 0),
                PortTypeKind::ResetAsyncLow | PortTypeKind::ResetSyncLow => (0, 1),
                _ => (0, 1),
            };
        }
        (0, 1)
    }

    fn resolve_loop_var(&self, var_id: &VarId) -> Option<(SemanticSignal<StateAddr>, usize)> {
        let (addr, info) = self.lookup.root_variable(*var_id)?;
        Some((
            SemanticSignal {
                address: addr,
                width: info.width,
            },
            info.width,
        ))
    }
}

fn try_eval_const(expr: &Expression) -> Option<u64> {
    match expr {
        Expression::Term(f) => match f.as_ref() {
            Factor::Value(c) => c.get_value().ok().map(|v| v.payload_u64()),
            Factor::Variable(_, _, _, c) => c.get_value().ok().map(|v| v.payload_u64()),
            _ => None,
        },
        _ => None,
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

fn extract_source_location(
    token: &veryl_parser::token_range::TokenRange,
) -> Option<SourceLocation> {
    let t = &token.beg;
    let file = t
        .source
        .get_path()
        .and_then(resource_table::get_path_value)?;
    Some(SourceLocation {
        file: file.to_string_lossy().into_owned(),
        line: t.line,
        column: t.column,
    })
}

pub fn compile_semantic_testbench(
    lookup: &VerylFrontendLookup,
    source: &VerylTestbenchSource,
    runtime_event_site_count: usize,
) -> Option<TestbenchProgram<StateAddr>> {
    let initial_stmts = source.initial_statements.as_ref()?;
    let mut builder = SemanticTestbenchBuilder::new(lookup, source, runtime_event_site_count);
    builder.build_event_map(initial_stmts);
    Some(TestbenchProgram::new(builder.convert(initial_stmts)))
}
