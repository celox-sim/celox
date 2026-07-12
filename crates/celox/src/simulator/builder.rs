use std::path::Path;

use veryl_analyzer::ir::{
    ArrayLiteralItem, Component, Comptime, Declaration, Expression, Factor, ForBound, ForRange,
    Statement, SystemFunctionKind, TbMethod, VarIndex, VarPath, VarSelect,
};
use veryl_analyzer::value::Value;
use veryl_analyzer::{Analyzer, AnalyzerError, Context, attribute_table, ir::Ir, symbol_table};
use veryl_metadata::{ClockType, Metadata, ResetType};
use veryl_parser::Parser;
use veryl_parser::resource_table;
use veryl_parser::token_range::TokenRange;

use crate::parser::BuildConfig;
use crate::{ParserError, SimulatorError, SimulatorErrorKind, ir::Program, parser};

fn token_range_contains_numeric_cast(token: TokenRange) -> bool {
    if token.beg.source != token.end.source {
        return false;
    }
    let text = token.beg.source.get_text();
    let start = token.beg.pos as usize;
    let end = token.end.pos as usize + token.end.length as usize;
    text.get(start..end).is_some_and(|source| {
        source.match_indices("as").any(|(offset, _)| {
            let before = source[..offset].chars().next_back();
            let after_offset = offset + 2;
            let after = source[after_offset..].chars().next();
            let is_boundary =
                |ch: Option<char>| ch.is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_');
            is_boundary(before)
                && is_boundary(after)
                && source[after_offset..]
                    .trim_start()
                    .chars()
                    .next()
                    .is_some_and(|ch| ch.is_ascii_digit())
        })
    })
}

fn visit_var_access(index: &VarIndex, select: &VarSelect, visitor: &mut impl FnMut(&Expression)) {
    for expr in &index.0 {
        visit_expression(expr, visitor);
    }
    for expr in &select.0 {
        visit_expression(expr, visitor);
    }
    if let Some((_, expr)) = &select.1 {
        visit_expression(expr, visitor);
    }
}

fn visit_function_call(
    call: &veryl_analyzer::ir::FunctionCall,
    visitor: &mut impl FnMut(&Expression),
) {
    for expr in call.inputs.values() {
        visit_expression(expr, visitor);
    }
    for destinations in call.outputs.values() {
        for destination in destinations {
            visit_var_access(&destination.index, &destination.select, visitor);
        }
    }
}

fn visit_system_function(
    call: &veryl_analyzer::ir::SystemFunctionCall,
    visitor: &mut impl FnMut(&Expression),
) {
    match &call.kind {
        SystemFunctionKind::Bits(input)
        | SystemFunctionKind::Size(input)
        | SystemFunctionKind::Clog2(input)
        | SystemFunctionKind::Onehot(input)
        | SystemFunctionKind::Signed(input)
        | SystemFunctionKind::Unsigned(input) => visit_expression(&input.0, visitor),
        SystemFunctionKind::Readmemh(input, output) => {
            visit_expression(&input.0, visitor);
            for destination in &output.0 {
                visit_var_access(&destination.index, &destination.select, visitor);
            }
        }
        SystemFunctionKind::Display(inputs) | SystemFunctionKind::Write(inputs) => {
            for input in inputs {
                visit_expression(&input.0, visitor);
            }
        }
        SystemFunctionKind::Assert { cond, args, .. } => {
            visit_expression(&cond.0, visitor);
            for input in args {
                visit_expression(&input.0, visitor);
            }
        }
        SystemFunctionKind::Finish => {}
    }
}

fn visit_expression(expr: &Expression, visitor: &mut impl FnMut(&Expression)) {
    visitor(expr);
    match expr {
        Expression::Term(factor) => match factor.as_ref() {
            Factor::Variable(_, index, select, _) => visit_var_access(index, select, visitor),
            Factor::FunctionCall(call) => visit_function_call(call, visitor),
            Factor::SystemFunctionCall(call) => visit_system_function(call, visitor),
            Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => {}
        },
        Expression::Unary(_, inner, _) => visit_expression(inner, visitor),
        Expression::Binary(lhs, _, rhs, _) => {
            visit_expression(lhs, visitor);
            visit_expression(rhs, visitor);
        }
        Expression::Ternary(cond, then_expr, else_expr, _) => {
            visit_expression(cond, visitor);
            visit_expression(then_expr, visitor);
            visit_expression(else_expr, visitor);
        }
        Expression::Concatenation(items, _) => {
            for (item, repeat) in items {
                visit_expression(item, visitor);
                if let Some(repeat) = repeat {
                    visit_expression(repeat, visitor);
                }
            }
        }
        Expression::ArrayLiteral(items, _) => {
            for item in items {
                match item {
                    ArrayLiteralItem::Value(expr, repeat) => {
                        visit_expression(expr, visitor);
                        if let Some(repeat) = repeat {
                            visit_expression(repeat, visitor);
                        }
                    }
                    ArrayLiteralItem::Defaul(expr) => visit_expression(expr, visitor),
                }
            }
        }
        Expression::StructConstructor(_, fields, _) => {
            for (_, expr) in fields {
                visit_expression(expr, visitor);
            }
        }
    }
}

fn visit_for_range(range: &ForRange, visitor: &mut impl FnMut(&Expression)) {
    let visit_bound = |bound: &ForBound, visitor: &mut _| {
        if let ForBound::Expression(expr) = bound {
            visit_expression(expr, visitor);
        }
    };
    match range {
        ForRange::Forward { start, end, .. }
        | ForRange::Reverse { start, end, .. }
        | ForRange::Stepped { start, end, .. } => {
            visit_bound(start, visitor);
            visit_bound(end, visitor);
        }
    }
}

fn visit_statement(statement: &Statement, visitor: &mut impl FnMut(&Expression)) {
    match statement {
        Statement::Assign(assign) => {
            visit_expression(&assign.expr, visitor);
            for destination in &assign.dst {
                visit_var_access(&destination.index, &destination.select, visitor);
            }
        }
        Statement::If(statement) => {
            visit_expression(&statement.cond, visitor);
            visit_statements(&statement.true_side, visitor);
            visit_statements(&statement.false_side, visitor);
        }
        Statement::IfReset(statement) => {
            visit_statements(&statement.true_side, visitor);
            visit_statements(&statement.false_side, visitor);
        }
        Statement::Case(statement) => {
            visit_expression(&statement.case_target, visitor);
            for arm in &statement.arms {
                for pattern in &arm.patterns {
                    match pattern {
                        veryl_analyzer::ir::CasePattern::Eq(expr) => {
                            visit_expression(expr, visitor)
                        }
                        veryl_analyzer::ir::CasePattern::Range { lo, hi, .. } => {
                            visit_expression(lo, visitor);
                            visit_expression(hi, visitor);
                        }
                    }
                }
                visit_statements(&arm.body, visitor);
            }
            visit_statements(&statement.default, visitor);
        }
        Statement::For(statement) => {
            visit_for_range(&statement.range, visitor);
            visit_statements(&statement.body, visitor);
        }
        Statement::SystemFunctionCall(call) => visit_system_function(call, visitor),
        Statement::FunctionCall(call) => visit_function_call(call, visitor),
        Statement::TbMethodCall(call) => match &call.method {
            TbMethod::ClockNext { count, period } => {
                if let Some(expr) = count {
                    visit_expression(expr, visitor);
                }
                if let Some(expr) = period {
                    visit_expression(expr, visitor);
                }
            }
            TbMethod::ResetAssert { duration, .. } => {
                if let Some(expr) = duration {
                    visit_expression(expr, visitor);
                }
            }
            TbMethod::FileOpen { name, .. } => visit_expression(&name.0, visitor),
            TbMethod::FileWrite { args } => {
                for arg in args {
                    visit_expression(&arg.0, visitor);
                }
            }
            TbMethod::FileClose | TbMethod::FileFlush => {}
        },
        Statement::Break | Statement::Unsupported(_) | Statement::Null => {}
    }
}

fn visit_statements(statements: &[Statement], visitor: &mut impl FnMut(&Expression)) {
    for statement in statements {
        visit_statement(statement, visitor);
    }
}

fn visit_component(component: &Component, visitor: &mut impl FnMut(&Expression)) {
    match component {
        Component::Module(module) => {
            for function in module.functions.values() {
                for body in &function.functions {
                    visit_statements(&body.statements, visitor);
                }
            }
            for declaration in &module.declarations {
                match declaration {
                    Declaration::Comb(declaration) => {
                        visit_statements(&declaration.statements, visitor)
                    }
                    Declaration::Ff(declaration) => {
                        visit_var_access(
                            &declaration.clock.index,
                            &declaration.clock.select,
                            visitor,
                        );
                        if let Some(reset) = &declaration.reset {
                            visit_var_access(&reset.index, &reset.select, visitor);
                        }
                        visit_statements(&declaration.statements, visitor);
                    }
                    Declaration::Inst(declaration) => {
                        for input in &declaration.inputs {
                            visit_expression(&input.expr, visitor);
                        }
                        for output in &declaration.outputs {
                            for destination in &output.dst {
                                visit_var_access(&destination.index, &destination.select, visitor);
                            }
                        }
                        visit_component(declaration.component.as_ref(), visitor);
                    }
                    Declaration::Initial(declaration) => {
                        visit_statements(&declaration.statements, visitor)
                    }
                    Declaration::Final(declaration) => {
                        visit_statements(&declaration.statements, visitor)
                    }
                    Declaration::Unsupported(_) | Declaration::Null => {}
                }
            }
        }
        Component::Interface(interface) => {
            for function in interface.functions.values() {
                for body in &function.functions {
                    visit_statements(&body.statements, visitor);
                }
            }
        }
        Component::SystemVerilog(component) => {
            for destination in &component.connects {
                visit_var_access(&destination.index, &destination.select, visitor);
            }
        }
    }
}

fn visit_ir(ir: &Ir, visitor: &mut impl FnMut(&Expression)) {
    for component in &ir.components {
        visit_component(component, visitor);
    }
}

#[derive(Default)]
struct NumericCastReplacements {
    expressions: fxhash::FxHashMap<TokenRange, Vec<Expression>>,
    next: fxhash::FxHashMap<TokenRange, usize>,
}

impl NumericCastReplacements {
    fn push(&mut self, token: TokenRange, expression: Expression) {
        self.expressions.entry(token).or_default().push(expression);
    }

    fn take(&mut self, token: TokenRange) -> Option<Expression> {
        let next = self.next.entry(token).or_default();
        let expression = self.expressions.get(&token)?.get(*next)?.clone();
        *next += 1;
        Some(expression)
    }
}

fn restore_var_access(
    index: &mut VarIndex,
    select: &mut VarSelect,
    replacements: &mut NumericCastReplacements,
) {
    for expr in &mut index.0 {
        restore_expression(expr, replacements);
    }
    for expr in &mut select.0 {
        restore_expression(expr, replacements);
    }
    if let Some((_, expr)) = &mut select.1 {
        restore_expression(expr, replacements);
    }
}

fn restore_function_call(
    call: &mut veryl_analyzer::ir::FunctionCall,
    replacements: &mut NumericCastReplacements,
) {
    for expr in call.inputs.values_mut() {
        restore_expression(expr, replacements);
    }
    for destinations in call.outputs.values_mut() {
        for destination in destinations {
            restore_var_access(
                &mut destination.index,
                &mut destination.select,
                replacements,
            );
        }
    }
}

fn restore_system_function(
    call: &mut veryl_analyzer::ir::SystemFunctionCall,
    replacements: &mut NumericCastReplacements,
) {
    match &mut call.kind {
        SystemFunctionKind::Bits(input)
        | SystemFunctionKind::Size(input)
        | SystemFunctionKind::Clog2(input)
        | SystemFunctionKind::Onehot(input)
        | SystemFunctionKind::Signed(input)
        | SystemFunctionKind::Unsigned(input) => restore_expression(&mut input.0, replacements),
        SystemFunctionKind::Readmemh(input, output) => {
            restore_expression(&mut input.0, replacements);
            for destination in &mut output.0 {
                restore_var_access(
                    &mut destination.index,
                    &mut destination.select,
                    replacements,
                );
            }
        }
        SystemFunctionKind::Display(inputs) | SystemFunctionKind::Write(inputs) => {
            for input in inputs {
                restore_expression(&mut input.0, replacements);
            }
        }
        SystemFunctionKind::Assert { cond, args, .. } => {
            restore_expression(&mut cond.0, replacements);
            for input in args {
                restore_expression(&mut input.0, replacements);
            }
        }
        SystemFunctionKind::Finish => {}
    }
}

fn restore_expression(expr: &mut Expression, replacements: &mut NumericCastReplacements) {
    if matches!(expr, Expression::Term(factor) if matches!(factor.as_ref(), Factor::Value(_)))
        && let Some(replacement) = replacements.take(expr.token_range())
    {
        *expr = replacement;
        return;
    }

    match expr {
        Expression::Term(factor) => match factor.as_mut() {
            Factor::Variable(_, index, select, _) => {
                restore_var_access(index, select, replacements)
            }
            Factor::FunctionCall(call) => restore_function_call(call, replacements),
            Factor::SystemFunctionCall(call) => restore_system_function(call, replacements),
            Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => {}
        },
        Expression::Unary(_, inner, _) => restore_expression(inner, replacements),
        Expression::Binary(lhs, _, rhs, _) => {
            restore_expression(lhs, replacements);
            restore_expression(rhs, replacements);
        }
        Expression::Ternary(cond, then_expr, else_expr, _) => {
            restore_expression(cond, replacements);
            restore_expression(then_expr, replacements);
            restore_expression(else_expr, replacements);
        }
        Expression::Concatenation(items, _) => {
            for (item, repeat) in items {
                restore_expression(item, replacements);
                if let Some(repeat) = repeat {
                    restore_expression(repeat, replacements);
                }
            }
        }
        Expression::ArrayLiteral(items, _) => {
            for item in items {
                match item {
                    ArrayLiteralItem::Value(expr, repeat) => {
                        restore_expression(expr, replacements);
                        if let Some(repeat) = repeat {
                            restore_expression(repeat, replacements);
                        }
                    }
                    ArrayLiteralItem::Defaul(expr) => restore_expression(expr, replacements),
                }
            }
        }
        Expression::StructConstructor(_, fields, _) => {
            for (_, expr) in fields {
                restore_expression(expr, replacements);
            }
        }
    }
}

fn restore_for_range(range: &mut ForRange, replacements: &mut NumericCastReplacements) {
    let mut restore_bound = |bound: &mut ForBound| {
        if let ForBound::Expression(expr) = bound {
            restore_expression(expr, replacements);
        }
    };
    match range {
        ForRange::Forward { start, end, .. }
        | ForRange::Reverse { start, end, .. }
        | ForRange::Stepped { start, end, .. } => {
            restore_bound(start);
            restore_bound(end);
        }
    }
}

fn restore_statements(statements: &mut [Statement], replacements: &mut NumericCastReplacements) {
    for statement in statements {
        match statement {
            Statement::Assign(assign) => {
                restore_expression(&mut assign.expr, replacements);
                for destination in &mut assign.dst {
                    restore_var_access(
                        &mut destination.index,
                        &mut destination.select,
                        replacements,
                    );
                }
            }
            Statement::If(statement) => {
                restore_expression(&mut statement.cond, replacements);
                restore_statements(&mut statement.true_side, replacements);
                restore_statements(&mut statement.false_side, replacements);
            }
            Statement::IfReset(statement) => {
                restore_statements(&mut statement.true_side, replacements);
                restore_statements(&mut statement.false_side, replacements);
            }
            Statement::Case(statement) => {
                restore_expression(&mut statement.case_target, replacements);
                for arm in &mut statement.arms {
                    for pattern in &mut arm.patterns {
                        match pattern {
                            veryl_analyzer::ir::CasePattern::Eq(expr) => {
                                restore_expression(expr, replacements)
                            }
                            veryl_analyzer::ir::CasePattern::Range { lo, hi, .. } => {
                                restore_expression(lo, replacements);
                                restore_expression(hi, replacements);
                            }
                        }
                    }
                    restore_statements(&mut arm.body, replacements);
                }
                restore_statements(&mut statement.default, replacements);
            }
            Statement::For(statement) => {
                restore_for_range(&mut statement.range, replacements);
                restore_statements(&mut statement.body, replacements);
            }
            Statement::SystemFunctionCall(call) => restore_system_function(call, replacements),
            Statement::FunctionCall(call) => restore_function_call(call, replacements),
            Statement::TbMethodCall(call) => match &mut call.method {
                TbMethod::ClockNext { count, period } => {
                    if let Some(expr) = count {
                        restore_expression(expr, replacements);
                    }
                    if let Some(expr) = period {
                        restore_expression(expr, replacements);
                    }
                }
                TbMethod::ResetAssert { duration, .. } => {
                    if let Some(expr) = duration {
                        restore_expression(expr, replacements);
                    }
                }
                TbMethod::FileOpen { name, .. } => restore_expression(&mut name.0, replacements),
                TbMethod::FileWrite { args } => {
                    for arg in args {
                        restore_expression(&mut arg.0, replacements);
                    }
                }
                TbMethod::FileClose | TbMethod::FileFlush => {}
            },
            Statement::Break | Statement::Unsupported(_) | Statement::Null => {}
        }
    }
}

fn restore_component(component: &mut Component, replacements: &mut NumericCastReplacements) {
    match component {
        Component::Module(module) => {
            for function in module.functions.values_mut() {
                for body in &mut function.functions {
                    restore_statements(&mut body.statements, replacements);
                }
            }
            for declaration in &mut module.declarations {
                match declaration {
                    Declaration::Comb(declaration) => {
                        restore_statements(&mut declaration.statements, replacements)
                    }
                    Declaration::Ff(declaration) => {
                        restore_var_access(
                            &mut declaration.clock.index,
                            &mut declaration.clock.select,
                            replacements,
                        );
                        if let Some(reset) = &mut declaration.reset {
                            restore_var_access(&mut reset.index, &mut reset.select, replacements);
                        }
                        restore_statements(&mut declaration.statements, replacements);
                    }
                    Declaration::Inst(declaration) => {
                        for input in &mut declaration.inputs {
                            restore_expression(&mut input.expr, replacements);
                        }
                        for output in &mut declaration.outputs {
                            for destination in &mut output.dst {
                                restore_var_access(
                                    &mut destination.index,
                                    &mut destination.select,
                                    replacements,
                                );
                            }
                        }
                        restore_component(
                            std::sync::Arc::make_mut(&mut declaration.component),
                            replacements,
                        );
                    }
                    Declaration::Initial(declaration) => {
                        restore_statements(&mut declaration.statements, replacements)
                    }
                    Declaration::Final(declaration) => {
                        restore_statements(&mut declaration.statements, replacements)
                    }
                    Declaration::Unsupported(_) | Declaration::Null => {}
                }
            }
        }
        Component::Interface(interface) => {
            for function in interface.functions.values_mut() {
                for body in &mut function.functions {
                    restore_statements(&mut body.statements, replacements);
                }
            }
        }
        Component::SystemVerilog(component) => {
            for destination in &mut component.connects {
                restore_var_access(
                    &mut destination.index,
                    &mut destination.select,
                    replacements,
                );
            }
        }
    }
}

fn restore_ir(ir: &mut Ir, replacements: &mut NumericCastReplacements) {
    for component in &mut ir.components {
        restore_component(component, replacements);
    }
}

fn create_analyzer_context(param_overrides: &[(String, u64)], disable_const_opt: bool) -> Context {
    let mut context = Context::default();
    context.disalbe_const_opt = disable_const_opt;
    if !param_overrides.is_empty() {
        let mut override_map = fxhash::FxHashMap::default();
        let token = TokenRange::default();
        for (name, value) in param_overrides {
            let name_id = resource_table::insert_str(name);
            let path = VarPath::new(name_id);
            let val = Value::new(*value, 64, false);
            let comptime = Comptime::create_value(val.clone(), token);
            let expr = Expression::create_value(val, token);
            override_map.insert(path, (comptime, expr));
        }
        context.push_override(override_map);
    }
    context
}

fn analyze(
    sources: &[(&str, &Path)],
    top: &str,
    ignored_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )],
    true_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )],
    four_state: bool,
    trace_opts: &crate::debug::TraceOptions,
    trace_out: Option<&mut crate::debug::CompilationTrace>,
    metadata: Option<Metadata>,
    clock_type: Option<ClockType>,
    reset_type: Option<ResetType>,
    param_overrides: &[(String, u64)],
    optimize_options: &crate::optimizer::OptimizeOptions,
) -> (Result<Program, ParserError>, Vec<AnalyzerError>) {
    symbol_table::clear();
    attribute_table::clear();

    let metadata = metadata.unwrap_or_else(|| Metadata::create_default("prj").unwrap());
    let analyzer = Analyzer::new(&metadata);

    // Per-file: parse + pass1
    let mut parsers = Vec::new();
    let mut errors = vec![];
    for (code, path) in sources {
        let parsed = Parser::parse(code, path).unwrap();
        errors.append(&mut analyzer.analyze_pass1("prj", &parsed.veryl));
        parsers.push(parsed);
    }
    let loop_sources =
        parser::loop_provenance::LoopSourceTable::collect(parsers.iter().map(|x| &x.veryl));

    // Global post-pass1
    errors.append(&mut Analyzer::analyze_post_pass1());

    // The primary pass keeps Veryl's normal constant folding enabled. Generate
    // conditions and parameterized declarations depend on that behavior.
    let mut context = create_analyzer_context(param_overrides, false);

    let mut ir = Ir::default();

    for parsed in &parsers {
        errors.append(&mut analyzer.analyze_pass2(
            "prj",
            &parsed.veryl,
            &mut context,
            Some(&mut ir),
        ));
    }
    errors.append(&mut Analyzer::analyze_post_pass2(&ir));

    // Veryl 0.20.2 folds numeric size casts as unsigned. Re-run pass2 only
    // when such a folded source range actually exists, then restore just those
    // expression trees into the otherwise-normal IR. This keeps generate
    // elaboration authoritative and avoids a second pass for unaffected code.
    let mut folded_cast_ranges = fxhash::FxHashSet::default();
    visit_ir(&ir, &mut |expr| {
        if matches!(expr, Expression::Term(factor) if matches!(factor.as_ref(), Factor::Value(_)))
            && token_range_contains_numeric_cast(expr.token_range())
        {
            folded_cast_ranges.insert(expr.token_range());
        }
    });
    if !folded_cast_ranges.is_empty() {
        let mut structural_context = create_analyzer_context(param_overrides, true);
        let mut structural_ir = Ir::default();
        for parsed in &parsers {
            let _ = analyzer.analyze_pass2(
                "prj",
                &parsed.veryl,
                &mut structural_context,
                Some(&mut structural_ir),
            );
        }
        let mut replacements = NumericCastReplacements::default();
        visit_ir(&structural_ir, &mut |expr| {
            let token = expr.token_range();
            if folded_cast_ranges.contains(&token)
                && crate::context_width::contains_numeric_width_cast(expr)
            {
                replacements.push(token, expr.clone());
            }
        });
        restore_ir(&mut ir, &mut replacements);
    }
    let loop_provenance = loop_sources.match_unrolled(&ir);

    let top = veryl_parser::resource_table::insert_str(top);
    let mut build_config = BuildConfig::from(&metadata.build);
    if let Some(ct) = clock_type {
        build_config.clock_type = ct;
    }
    if let Some(rt) = reset_type {
        build_config.reset_type = rt;
    }
    let sir = parser::parse(
        &top,
        &ir,
        &loop_provenance,
        &build_config,
        ignored_loops,
        true_loops,
        four_state,
        trace_opts,
        trace_out,
        optimize_options,
    );
    (sir, errors)
}

/// Compile Veryl source code to the SIR (Simulation IR) representation.
///
/// This is the shared compilation pipeline used by all backends.
/// Returns the compiled Program and any analyzer warnings on success.
pub fn compile_to_sir(
    sources: &[(&str, &Path)],
    top: &str,
    ignored_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )],
    true_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )],
    four_state: bool,
    trace_opts: &crate::debug::TraceOptions,
    trace_out: Option<&mut crate::debug::CompilationTrace>,
    metadata: Option<Metadata>,
    clock_type: Option<ClockType>,
    reset_type: Option<ResetType>,
    param_overrides: &[(String, u64)],
    optimize_options: &crate::optimizer::OptimizeOptions,
) -> Result<(Program, Vec<AnalyzerError>), SimulatorError> {
    let (sir, errors) = analyze(
        sources,
        top,
        ignored_loops,
        true_loops,
        four_state,
        trace_opts,
        trace_out,
        metadata,
        clock_type,
        reset_type,
        param_overrides,
        optimize_options,
    );
    let (real_errors, warnings): (Vec<_>, Vec<_>) = errors.into_iter().partition(|e| e.is_error());
    if !real_errors.is_empty() {
        return Err(
            SimulatorError::new(SimulatorErrorKind::Analyzer(real_errors)).with_warnings(warnings),
        );
    }
    match sir {
        Ok(p) => Ok((p, warnings)),
        Err(e) => Err(SimulatorError::from(e).with_warnings(warnings)),
    }
}

// ── JIT-specific types and builders (native only) ────────────────────

#[cfg(not(target_arch = "wasm32"))]
use super::Simulator;
#[cfg(not(target_arch = "wasm32"))]
use crate::backend::JitBackend;

/// Controls which stores the dead store elimination pass preserves.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeadStorePolicy {
    /// Keep all stores (no dead store elimination). Default for user-facing builds.
    #[default]
    Off,
    /// Eliminate stores except those explicitly marked live via `live_signal()`
    /// and those loaded by execution units.
    PreserveListedSignals,
    /// Eliminate stores except those to top-module ports and those loaded by EUs.
    PreserveTopPorts,
    /// Eliminate stores except those to ports of *all* instances and those loaded by EUs.
    PreserveAllPorts,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone)]
pub struct SimulatorOptions {
    pub four_state: bool,
    /// Per-pass SIRT optimizer flags.
    pub optimize_options: crate::optimizer::OptimizeOptions,
    /// Fine-grained Cranelift backend options.
    pub cranelift_options: crate::optimizer::CraneliftOptions,
    pub trace: crate::debug::TraceOptions,
    /// When true, JIT-compiled functions emit trigger detection code for
    /// edge-based event discovery. Only needed by [`crate::Simulation`].
    pub emit_triggers: bool,
    /// Dead store elimination policy.
    pub dead_store_policy: DeadStorePolicy,
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for SimulatorOptions {
    fn default() -> Self {
        let opt = crate::optimizer::OptimizeOptions::default();
        let cranelift = opt.opt_level().default_cranelift_options();
        Self {
            four_state: false,
            optimize_options: opt,
            cranelift_options: cranelift,
            trace: Default::default(),
            emit_triggers: false,
            dead_store_policy: DeadStorePolicy::Off,
        }
    }
}

/// A fluent builder for configuring and initializing a [`Simulator`] or
/// [`Simulation`](crate::Simulation).
///
/// Use [`Simulator::builder()`] or [`Simulation::builder()`](crate::Simulation::builder)
/// to obtain the appropriate variant. Both share the same configuration methods;
/// only `.build()` differs in return type.
#[cfg(not(target_arch = "wasm32"))]
pub struct SimulatorBuilder<'a, Target = Simulator> {
    sources: Vec<(&'a str, &'a Path)>,
    top: &'a str,
    ignored_loops: Vec<(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )>,
    true_loops: Vec<(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )>,
    options: SimulatorOptions,
    vcd_path: Option<std::path::PathBuf>,
    metadata: Option<Metadata>,
    clock_type: Option<ClockType>,
    reset_type: Option<ResetType>,
    param_overrides: Vec<(String, u64)>,
    live_signals: Vec<(Vec<(String, usize)>, Vec<String>)>,
    _marker: std::marker::PhantomData<Target>,
}

/// Configuration methods shared by all builder variants.
#[cfg(not(target_arch = "wasm32"))]
impl<'a, Target> SimulatorBuilder<'a, Target> {
    /// Returns the source files passed to this builder.
    pub fn sources(&self) -> &[(&'a str, &'a Path)] {
        &self.sources
    }

    /// Returns the top module name.
    pub fn top(&self) -> &'a str {
        self.top
    }

    /// Supply project metadata (clock/reset settings, etc.) instead of defaults.
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Override the clock type (posedge/negedge) from metadata or defaults.
    pub fn clock_type(mut self, clock_type: ClockType) -> Self {
        self.clock_type = Some(clock_type);
        self
    }

    /// Override the reset type (async_high/async_low/sync_high/sync_low) from metadata or defaults.
    pub fn reset_type(mut self, reset_type: ResetType) -> Self {
        self.reset_type = Some(reset_type);
        self
    }

    /// Override a top-level module parameter value.
    pub fn param(mut self, name: &str, value: u64) -> Self {
        self.param_overrides.push((name.to_string(), value));
        self
    }

    /// Enable VCD dumping to the specified file.
    pub fn vcd<P: AsRef<std::path::Path>>(mut self, path: P) -> Self {
        self.vcd_path = Some(path.as_ref().to_path_buf());
        self
    }

    /// Enable 4-state (0, 1, X, Z) simulation mode.
    pub fn four_state(mut self, enable: bool) -> Self {
        self.options.four_state = enable;
        self
    }

    /// Set the overall optimization level. Sets defaults for SIR passes,
    /// Cranelift options, and DSE policy. Per-pass overrides can be applied after.
    pub fn opt_level(mut self, level: crate::optimizer::OptLevel) -> Self {
        self.options.optimize_options = crate::optimizer::OptimizeOptions::new(level);
        self.options.cranelift_options = level.default_cranelift_options();
        self.options.dead_store_policy = match level {
            crate::optimizer::OptLevel::O2 => DeadStorePolicy::PreserveTopPorts,
            _ => DeadStorePolicy::Off,
        };
        self
    }

    /// Enable a specific SIR pass, overriding the OptLevel default.
    pub fn enable_pass(mut self, pass: crate::optimizer::SirPass) -> Self {
        self.options.optimize_options = self.options.optimize_options.enable(pass);
        self
    }

    /// Disable a specific SIR pass, overriding the OptLevel default.
    pub fn disable_pass(mut self, pass: crate::optimizer::SirPass) -> Self {
        self.options.optimize_options = self.options.optimize_options.disable(pass);
        self
    }

    /// Enable or disable all SIRT optimization passes at once.
    /// Shorthand: `true` → `OptLevel::O1`, `false` → `OptLevel::O0`.
    pub fn optimize(mut self, enable: bool) -> Self {
        self.options.optimize_options = if enable {
            crate::optimizer::OptimizeOptions::all()
        } else {
            crate::optimizer::OptimizeOptions::none()
        };
        self
    }

    /// Set per-pass optimizer flags directly.
    pub fn optimize_options(mut self, options: crate::optimizer::OptimizeOptions) -> Self {
        self.options.optimize_options = options;
        self
    }

    /// Set fine-grained Cranelift backend options.
    pub fn cranelift_options(mut self, options: crate::optimizer::CraneliftOptions) -> Self {
        self.options.cranelift_options = options;
        self
    }

    /// Set the register allocator algorithm.
    pub fn regalloc_algorithm(mut self, algo: crate::optimizer::RegallocAlgorithm) -> Self {
        self.options.cranelift_options.regalloc_algorithm = algo;
        self
    }

    /// Enable or disable alias analysis in the Cranelift egraph pass.
    pub fn enable_alias_analysis(mut self, enable: bool) -> Self {
        self.options.cranelift_options.enable_alias_analysis = enable;
        self
    }

    /// Enable or disable the Cranelift IR verifier.
    pub fn enable_verifier(mut self, enable: bool) -> Self {
        self.options.cranelift_options.enable_verifier = enable;
        self
    }

    /// Set the dead store elimination policy.
    pub fn dead_store_policy(mut self, policy: DeadStorePolicy) -> Self {
        self.options.dead_store_policy = policy;
        self
    }

    /// Mark a signal as externally observable (live) for dead store elimination.
    pub fn live_signal(
        mut self,
        instance_path: Vec<(String, usize)>,
        var_path: Vec<String>,
    ) -> Self {
        self.live_signals.push((instance_path, var_path));
        self
    }

    /// Configure compilation tracing options.
    pub fn trace(mut self, trace: crate::debug::TraceOptions) -> Self {
        self.options.trace = trace;
        self
    }

    pub fn trace_sim_modules(mut self) -> Self {
        self.options.trace.sim_modules = true;
        self
    }

    pub fn trace_pre_atomized_comb_blocks(mut self) -> Self {
        self.options.trace.pre_atomized_comb_blocks = true;
        self
    }

    pub fn trace_atomized_comb_blocks(mut self) -> Self {
        self.options.trace.atomized_comb_blocks = true;
        self
    }

    pub fn trace_flattened_comb_blocks(mut self) -> Self {
        self.options.trace.flattened_comb_blocks = true;
        self
    }

    pub fn trace_scheduled_units(mut self) -> Self {
        self.options.trace.scheduled_units = true;
        self
    }

    pub fn trace_pre_optimized_sir(mut self) -> Self {
        self.options.trace.pre_optimized_sir = true;
        self
    }

    pub fn trace_post_optimized_sir(mut self) -> Self {
        self.options.trace.post_optimized_sir = true;
        self
    }

    pub fn trace_analyzer_ir(mut self) -> Self {
        self.options.trace.analyzer_ir = true;
        self
    }

    pub fn trace_pre_optimized_clif(mut self) -> Self {
        self.options.trace.pre_optimized_clif = true;
        self
    }

    pub fn trace_post_optimized_clif(mut self) -> Self {
        self.options.trace.post_optimized_clif = true;
        self
    }

    pub fn trace_native(mut self) -> Self {
        self.options.trace.native = true;
        self
    }

    pub fn trace_mir(mut self) -> Self {
        self.options.trace.mir = true;
        self
    }

    pub fn trace_on_build(mut self) -> Self {
        self.options.trace.output_to_stdout = true;
        self
    }

    /// Explicitly ignore a dependency between two signals.
    pub fn false_loop(
        mut self,
        from: (Vec<(String, usize)>, Vec<String>),
        to: (Vec<(String, usize)>, Vec<String>),
    ) -> Self {
        self.ignored_loops.push((from, to));
        self
    }

    /// Mark a dependency as a "true loop" and specify its convergence limit.
    pub fn true_loop(
        mut self,
        from: (Vec<(String, usize)>, Vec<String>),
        to: (Vec<(String, usize)>, Vec<String>),
        max_iter: usize,
    ) -> Self {
        self.true_loops.push((from, to, max_iter));
        self
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<'a> SimulatorBuilder<'a, Simulator> {
    pub fn new(code: &'a str, top: &'a str) -> Self {
        Self {
            sources: vec![(code, Path::new(""))],
            top,
            ignored_loops: Vec::new(),
            true_loops: Vec::new(),
            options: SimulatorOptions::default(),
            vcd_path: None,
            metadata: None,
            clock_type: None,
            reset_type: None,
            param_overrides: Vec::new(),
            live_signals: Vec::new(),
            _marker: std::marker::PhantomData,
        }
    }

    pub fn from_sources(sources: Vec<(&'a str, &'a Path)>, top: &'a str) -> Self {
        Self {
            sources,
            top,
            ignored_loops: Vec::new(),
            true_loops: Vec::new(),
            options: SimulatorOptions::default(),
            vcd_path: None,
            metadata: None,
            clock_type: None,
            reset_type: None,
            param_overrides: Vec::new(),
            live_signals: Vec::new(),
            _marker: std::marker::PhantomData,
        }
    }

    /// Compile SIR and return it along with the remaining builder state.
    /// Consumes self.
    fn into_sir(
        self,
    ) -> Result<
        (
            crate::ir::Program,
            Vec<veryl_analyzer::AnalyzerError>,
            SimulatorOptions,
            Option<std::path::PathBuf>,
        ),
        SimulatorError,
    > {
        let phase_timing = std::env::var_os("CELOX_PHASE_TIMING").is_some();
        let compile_start = phase_timing.then(crate::timing::now);
        let (mut program, warnings) = compile_to_sir(
            &self.sources,
            self.top,
            &self.ignored_loops,
            &self.true_loops,
            self.options.four_state,
            &self.options.trace,
            None,
            self.metadata,
            self.clock_type,
            self.reset_type,
            &self.param_overrides,
            &self.options.optimize_options,
        )?;
        if let Some(start) = compile_start {
            eprintln!("[phase-timing] compile_to_sir: {:?}", start.elapsed());
        }

        // Register testbench runtime-event sites before layout fixes the ring geometry.
        let runtime_sites_start = phase_timing.then(crate::timing::now);
        crate::testbench::register_runtime_event_sites(&mut program);
        if let Some(start) = runtime_sites_start {
            eprintln!(
                "[phase-timing] register_runtime_event_sites: {:?} runtime_event_sites={} comb_observers={}",
                start.elapsed(),
                program.runtime_event_sites.len(),
                program.comb_observers.len()
            );
        }

        // Build memory layout (consumes address_aliases for offset sharing)
        let layout_start = phase_timing.then(crate::timing::now);
        program.build_layout(self.options.four_state);
        if let Some(start) = layout_start {
            eprintln!("[phase-timing] build_layout: {:?}", start.elapsed());
        }

        if self.options.dead_store_policy != DeadStorePolicy::Off {
            let dse_start = phase_timing.then(crate::timing::now);
            run_dead_store_elimination(
                &mut program,
                &self.live_signals,
                self.options.dead_store_policy,
            );
            if let Some(start) = dse_start {
                eprintln!(
                    "[phase-timing] dead_store_elimination: {:?}",
                    start.elapsed()
                );
            }
        }

        Ok((program, warnings, self.options, self.vcd_path))
    }

    /// Compiles the Veryl source and constructs the simulator.
    /// Uses the native x86-64 backend on x86-64, Cranelift elsewhere.
    pub fn build(self) -> Result<Simulator<crate::DefaultBackend>, SimulatorError> {
        #[cfg(target_arch = "x86_64")]
        {
            self.build_native()
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            self.build_cranelift()
        }
    }

    /// Compiles using the Cranelift JIT backend.
    pub fn build_cranelift(self) -> Result<Simulator<JitBackend>, SimulatorError> {
        let phase_timing = std::env::var("CELOX_PHASE_TIMING").is_ok();
        let phase_start = phase_timing.then(crate::timing::now);

        let (program, warnings, options, vcd_path) = self.into_sir()?;

        if let Some(s) = phase_start {
            eprintln!("[phase-timing] compile_to_sir (total): {:?}", s.elapsed());
        }

        let jit_start = phase_timing.then(crate::timing::now);
        let backend = JitBackend::new(&program, &options, None)?;
        if let Some(s) = jit_start {
            eprintln!("[phase-timing] jit_backend: {:?}", s.elapsed());
        }

        let mut sim = Simulator::with_backend_and_program(backend, program, warnings);
        if let Some(path) = vcd_path {
            let descs = sim.build_vcd_descs(options.four_state);
            let vcd_writer = crate::vcd::VcdWriter::new(path, &descs)
                .map_err(|_| SimulatorError::from(crate::RuntimeErrorCode::InternalError))?;
            sim.vcd_writer = Some(vcd_writer);
        }
        sim.apply_initial_values();
        sim.modify(|_| {}).map_err(SimulatorError::from)?;
        Ok(sim)
    }

    /// Compiles using the native x86-64 backend.
    #[cfg(target_arch = "x86_64")]
    pub fn build_native(
        self,
    ) -> Result<Simulator<crate::backend::native::NativeBackend>, SimulatorError> {
        let phase_timing = std::env::var_os("CELOX_PHASE_TIMING").is_some();
        let sir_start = phase_timing.then(crate::timing::now);
        let (program, warnings, options, vcd_path) = self.into_sir()?;
        if let Some(start) = sir_start {
            eprintln!("[phase-timing] into_sir total: {:?}", start.elapsed());
        }
        let backend_start = phase_timing.then(crate::timing::now);
        let backend = crate::backend::native::NativeBackend::new(&program, &options)?;
        if let Some(start) = backend_start {
            eprintln!("[phase-timing] native_backend: {:?}", start.elapsed());
        }
        let mut sim = Simulator::with_backend_and_program(backend, program, warnings);
        if let Some(path) = vcd_path {
            let descs = sim.build_vcd_descs(options.four_state);
            let vcd_writer = crate::vcd::VcdWriter::new(path, &descs)
                .map_err(|_| SimulatorError::from(crate::RuntimeErrorCode::InternalError))?;
            sim.vcd_writer = Some(vcd_writer);
        }
        let apply_initial_start = phase_timing.then(crate::timing::now);
        sim.apply_initial_values();
        if let Some(start) = apply_initial_start {
            eprintln!("[phase-timing] apply_initial_values: {:?}", start.elapsed());
        }
        let settle_start = phase_timing.then(crate::timing::now);
        sim.modify(|_| {}).map_err(SimulatorError::from)?;
        if let Some(start) = settle_start {
            eprintln!("[phase-timing] initial_settle: {:?}", start.elapsed());
        }
        Ok(sim)
    }

    /// Compiles using the Wasmtime WASM backend.
    pub fn build_wasm(
        self,
    ) -> Result<Simulator<crate::backend::wasm_runtime::WasmBackend>, SimulatorError> {
        let (program, warnings, options, vcd_path) = self.into_sir()?;
        let backend = crate::backend::wasm_runtime::WasmBackend::new(&program, &options)?;
        let mut sim = Simulator::with_backend_and_program(backend, program, warnings);
        if let Some(path) = vcd_path {
            let descs = sim.build_vcd_descs(options.four_state);
            let vcd_writer = crate::vcd::VcdWriter::new(path, &descs)
                .map_err(|_| SimulatorError::from(crate::RuntimeErrorCode::InternalError))?;
            sim.vcd_writer = Some(vcd_writer);
        }
        sim.apply_initial_values();
        sim.modify(|_| {}).map_err(SimulatorError::from)?;
        Ok(sim)
    }

    /// Compiles and runs a native testbench (`#[test]` module).
    pub fn run_test(self) -> Result<crate::testbench::TestResult, SimulatorError> {
        run_test_with_sim(self.build()?)
    }

    /// Compiles and runs a testbench using the Cranelift JIT backend.
    pub fn run_test_cranelift(self) -> Result<crate::testbench::TestResult, SimulatorError> {
        run_test_with_sim(self.build_cranelift()?)
    }

    /// Compiles and runs a testbench using the custom native backend.
    #[cfg(target_arch = "x86_64")]
    pub fn run_test_native(self) -> Result<crate::testbench::TestResult, SimulatorError> {
        run_test_with_sim(self.build_native()?)
    }

    /// Compiles and runs a native testbench, returning assertion results
    /// observed before the test finishes or stops on a fatal failure.
    pub fn run_test_detailed(self) -> Result<crate::testbench::TestResultDetailed, SimulatorError> {
        let mut sim = self.build()?;
        let initial_stmts = sim.program().initial_statements.clone().ok_or_else(|| {
            SimulatorError::new(SimulatorErrorKind::Codegen(
                "no initial block found — this module is not a native testbench".into(),
            ))
        })?;
        let mut tb_builder = crate::testbench::TestbenchBuilder::new(&sim);
        tb_builder.build_event_map(&initial_stmts);
        let tb_stmts = tb_builder.convert(&initial_stmts);
        Ok(crate::testbench::run_testbench_detailed(
            &mut sim, &tb_stmts,
        ))
    }

    /// Compiles the Veryl source and constructs the core logic simulator,
    /// while capturing compilation trace data as configured by TraceOptions.
    pub fn build_with_trace(self) -> crate::debug::CompilationTraceResult {
        let mut trace = crate::debug::CompilationTrace::default();
        let program_res = compile_to_sir(
            &self.sources,
            self.top,
            &self.ignored_loops,
            &self.true_loops,
            self.options.four_state,
            &self.options.trace,
            Some(&mut trace),
            self.metadata,
            self.clock_type,
            self.reset_type,
            &self.param_overrides,
            &self.options.optimize_options,
        );

        let sim_res = program_res.and_then(|(mut program, warnings)| {
            // Register testbench runtime-event sites before layout fixes the ring geometry.
            crate::testbench::register_runtime_event_sites(&mut program);

            program.build_layout(self.options.four_state);

            if self.options.dead_store_policy != DeadStorePolicy::Off {
                run_dead_store_elimination(
                    &mut program,
                    &self.live_signals,
                    self.options.dead_store_policy,
                );
            }

            // Run MIR trace if requested (generates MIR output before/after optimization + regalloc)
            #[cfg(target_arch = "x86_64")]
            if self.options.trace.mir {
                use crate::backend::native::{emit, isel, mir_opt, regalloc};
                let layout = program
                    .layout
                    .as_ref()
                    .expect("layout must be built before MIR trace");
                let mut mir_output = String::new();

                mir_output.push_str("=== MIR (eval_comb) ===\n");
                for (idx, eu) in program.eval_comb.iter().enumerate() {
                    let mut mfunc = isel::lower_execution_unit(eu, layout, self.options.four_state);
                    crate::backend::native::mir_legalize::legalize(&mut mfunc);
                    mir_opt::optimize(&mut mfunc);
                    mir_output.push_str(&format!("Execution Unit {idx} (before regalloc):\n"));
                    mir_output.push_str(&format!("{mfunc}\n"));
                    let ra = regalloc::run_regalloc(&mut mfunc)
                        .map_err(|error| SimulatorError::from(error.to_string()))?;
                    mir_output.push_str(&format!("Execution Unit {idx} (after regalloc):\n"));
                    mir_output.push_str(&format!("{mfunc}"));
                    mir_output.push_str("  Register assignment:\n");
                    for (vreg, preg) in ra.assignment.sorted_entries() {
                        mir_output.push_str(&format!("    {vreg} -> {preg}\n"));
                    }
                    if let Ok(result) = emit::emit(&mfunc, &ra.assignment, ra.spill_frame_size) {
                        mir_output.push_str("  x86-64 disassembly:\n");
                        mir_output.push_str(&emit::disassemble(&result.code, 0));
                    }
                    mir_output.push('\n');
                }
                trace.mir = Some(mir_output);
            }

            #[cfg(target_arch = "x86_64")]
            let backend = crate::backend::native::NativeBackend::new(&program, &self.options)?;
            #[cfg(not(target_arch = "x86_64"))]
            let backend = JitBackend::new(&program, &self.options, None)?;

            let mut sim = Simulator::with_backend_and_program(backend, program, warnings);
            sim.apply_initial_values();
            sim.modify(|_| {}).map_err(SimulatorError::from)?;
            Ok(sim)
        });

        if self.options.trace.output_to_stdout {
            trace.print();
        }

        crate::debug::CompilationTraceResult {
            res: sim_res,
            trace,
        }
    }
}

fn run_test_with_sim<B: crate::backend::SimBackend>(
    mut sim: Simulator<B>,
) -> Result<crate::testbench::TestResult, SimulatorError> {
    let initial_stmts = sim.program().initial_statements.clone().ok_or_else(|| {
        SimulatorError::new(SimulatorErrorKind::Codegen(
            "no initial block found — this module is not a native testbench".into(),
        ))
    })?;
    let mut tb_builder = crate::testbench::TestbenchBuilder::new(&sim);
    tb_builder.build_event_map(&initial_stmts);
    let tb_stmts = tb_builder.convert(&initial_stmts);
    Ok(crate::testbench::run_testbench(&mut sim, &tb_stmts))
}

#[cfg(not(target_arch = "wasm32"))]
impl<'a> SimulatorBuilder<'a, crate::Simulation> {
    pub(crate) fn new(code: &'a str, top: &'a str) -> Self {
        Self {
            sources: vec![(code, Path::new(""))],
            top,
            ignored_loops: Vec::new(),
            true_loops: Vec::new(),
            options: SimulatorOptions::default(),
            vcd_path: None,
            metadata: None,
            clock_type: None,
            reset_type: None,
            param_overrides: Vec::new(),
            live_signals: Vec::new(),
            _marker: std::marker::PhantomData,
        }
    }

    pub(crate) fn from_sources(sources: Vec<(&'a str, &'a Path)>, top: &'a str) -> Self {
        Self {
            sources,
            top,
            ignored_loops: Vec::new(),
            true_loops: Vec::new(),
            options: SimulatorOptions::default(),
            vcd_path: None,
            metadata: None,
            clock_type: None,
            reset_type: None,
            param_overrides: Vec::new(),
            live_signals: Vec::new(),
            _marker: std::marker::PhantomData,
        }
    }

    /// Compiles the Veryl source and constructs the timed simulation wrapper.
    pub fn build(mut self) -> Result<crate::Simulation, SimulatorError> {
        self.options.emit_triggers = true;
        let (mut program, warnings) = compile_to_sir(
            &self.sources,
            self.top,
            &self.ignored_loops,
            &self.true_loops,
            self.options.four_state,
            &self.options.trace,
            None,
            self.metadata,
            self.clock_type,
            self.reset_type,
            &self.param_overrides,
            &self.options.optimize_options,
        )?;
        program.build_layout(self.options.four_state);

        if self.options.dead_store_policy != DeadStorePolicy::Off {
            run_dead_store_elimination(
                &mut program,
                &self.live_signals,
                self.options.dead_store_policy,
            );
        }
        #[cfg(target_arch = "x86_64")]
        let backend = crate::backend::native::NativeBackend::new(&program, &self.options)?;
        #[cfg(not(target_arch = "x86_64"))]
        let backend = crate::backend::JitBackend::new(&program, &self.options, None)?;

        let mut sim = Simulator::with_backend_and_program(backend, program, warnings);
        if let Some(path) = self.vcd_path {
            let descs = sim.build_vcd_descs(self.options.four_state);
            let vcd_writer = crate::vcd::VcdWriter::new(path, &descs)
                .map_err(|_| SimulatorError::from(crate::RuntimeErrorCode::InternalError))?;
            sim.vcd_writer = Some(vcd_writer);
        }
        sim.apply_initial_values();
        sim.modify(|_| {}).map_err(SimulatorError::from)?;
        Ok(crate::Simulation::new(sim))
    }
}

/// Resolve user-specified `(instance_path, var_path)` to `AbsoluteAddr` and run DSE.
#[cfg(not(target_arch = "wasm32"))]
fn run_dead_store_elimination(
    program: &mut Program,
    live_signals: &[(Vec<(String, usize)>, Vec<String>)],
    policy: DeadStorePolicy,
) {
    use crate::HashSet;
    use crate::ir::{AbsoluteAddr, InstancePath};
    let mut externally_live = HashSet::default();

    // User-specified live signals
    for (inst_path, var_path) in live_signals {
        let inst_refs: Vec<(&str, usize)> =
            inst_path.iter().map(|(s, i)| (s.as_str(), *i)).collect();
        let var_refs: Vec<&str> = var_path.iter().map(|s| s.as_str()).collect();
        let addr = program.get_addr(&inst_refs, &var_refs).unwrap();
        externally_live.insert(addr);
    }

    // PreserveTopPorts: auto-collect top module port addresses
    if policy == DeadStorePolicy::PreserveTopPorts {
        if let Some(&top_instance_id) = program.instance_ids.get(&InstancePath(vec![])) {
            if let Some(&top_module_id) = program.instance_module.get(&top_instance_id) {
                if let Some(top_vars) = program.module_variables.get(&top_module_id) {
                    for info in top_vars.values() {
                        if info.var_kind.is_port() {
                            externally_live.insert(AbsoluteAddr {
                                instance_id: top_instance_id,
                                var_id: info.id,
                            });
                        }
                    }
                }
            }
        }
    }

    // PreserveAllPorts: collect port addresses from every instance
    if policy == DeadStorePolicy::PreserveAllPorts {
        for (&instance_id, &module_id) in &program.instance_module {
            if let Some(vars) = program.module_variables.get(&module_id) {
                for info in vars.values() {
                    if info.var_kind.is_port() {
                        externally_live.insert(AbsoluteAddr {
                            instance_id,
                            var_id: info.id,
                        });
                    }
                }
            }
        }
    }

    crate::optimizer::coalescing::pass_dead_store_elimination::eliminate_dead_stores(
        program,
        &externally_live,
    );
}
