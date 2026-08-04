use celox_design::{BitAccess, PortTypeKind, RuntimeEventKind, RuntimeEventSite, StateAddr};
use celox_testbench::{
    AssertMessage as GenericAssertMessage, ClockCount as GenericClockCount, ExprBytecode,
    ExprOpcode as TbOpcode, LoopBound as GenericLoopBound, SemanticArgument, SemanticSignal,
    SemanticStatement, SourceLocation, StateLocation, TestbenchOperator as Op, TestbenchProgram,
    TestbenchStatement as GenericTestbenchStatement,
};
use fxhash::FxHashSet;
use num_traits::ToPrimitive as _;
use veryl_analyzer::ir::{
    ArrayLiteralItem, AssertKind, CasePattern, Expression, Factor, ForBound, ForRange, Function,
    FunctionCall, HierVarRef, Op as VerylOp, Statement, SystemFunctionInput, SystemFunctionKind,
    TbMethod, TbMethodCall, VarId, VarIndex, VarSelect, VarSelectOp,
};
use veryl_analyzer::value::byte_value_to_string;
use veryl_parser::resource_table::{self, StrId};

use crate::{
    AbsoluteAddr, InstancePath, LoweringPhase, ParserError, VariableInfo, VerylFrontendLookup,
    VerylTestbenchSource,
    bitaccess::eval_constexpr,
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
    SemanticArgument {
        expr: ec.compile(expr),
        width: ec.natural_width(expr),
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

fn source_name(id: StrId) -> String {
    resource_table::get_str_value(id).unwrap_or_else(|| format!("{id}"))
}

fn hierarchical_reference_name(reference: &HierVarRef) -> String {
    reference
        .inst_path
        .iter()
        .copied()
        .chain(reference.var_path.0.iter().copied())
        .map(source_name)
        .collect::<Vec<_>>()
        .join(".")
}

fn invalid_hierarchical_reference(
    reference: &HierVarRef,
    detail: impl Into<String>,
) -> ParserError {
    ParserError::illegal_context(
        "hierarchical variable reference",
        format!(
            "`{}`: {}",
            hierarchical_reference_name(reference),
            detail.into()
        ),
        Some(&reference.comptime.token),
    )
}

pub(crate) fn resolve_hierarchical_reference<'a>(
    lookup: &'a VerylFrontendLookup,
    reference: &HierVarRef,
) -> Result<(StateAddr, &'a VariableInfo), ParserError> {
    let mut resolved_path = Vec::with_capacity(reference.inst_path.len());
    for &segment in &reference.inst_path {
        let candidates = lookup
            .instance_ids
            .keys()
            .filter(|candidate| {
                candidate.0.len() == resolved_path.len() + 1
                    && candidate.0.starts_with(&resolved_path)
                    && candidate.0[resolved_path.len()].0 == segment
            })
            .map(|candidate| candidate.0[resolved_path.len()])
            .collect::<Vec<_>>();
        match candidates.as_slice() {
            [part] => resolved_path.push(*part),
            [] => {
                return Err(invalid_hierarchical_reference(
                    reference,
                    format!("instance `{}` was not found", source_name(segment)),
                ));
            }
            _ => {
                return Err(invalid_hierarchical_reference(
                    reference,
                    format!(
                        "instance `{}` is ambiguous because it elaborates to multiple array elements",
                        source_name(segment)
                    ),
                ));
            }
        }
    }

    let instance_id = lookup
        .instance_ids
        .get(&InstancePath(resolved_path))
        .copied()
        .ok_or_else(|| {
            invalid_hierarchical_reference(reference, "the elaborated instance path was not found")
        })?;
    let module_id = lookup
        .instance_module
        .get(&instance_id)
        .copied()
        .ok_or_else(|| {
            invalid_hierarchical_reference(reference, "the target instance has no module")
        })?;
    let var_id = match lookup
        .module_var_path_index
        .get(&module_id)
        .and_then(|variables| variables.get(&reference.var_path))
    {
        Some(Some(var_id)) => *var_id,
        Some(None) => {
            return Err(invalid_hierarchical_reference(
                reference,
                format!("variable `{}` is ambiguous", reference.var_path),
            ));
        }
        None => {
            return Err(invalid_hierarchical_reference(
                reference,
                format!("variable `{}` was not found", reference.var_path),
            ));
        }
    };
    let info = lookup
        .module_variables
        .get(&module_id)
        .and_then(|variables| variables.get(&var_id))
        .ok_or_else(|| {
            invalid_hierarchical_reference(reference, "the target variable has no metadata")
        })?;
    let address = lookup
        .state_address(&AbsoluteAddr {
            instance_id,
            var_id,
        })
        .ok_or_else(|| {
            invalid_hierarchical_reference(
                reference,
                "the target variable has no elaborated state address",
            )
        })?;
    Ok((address, info))
}

/// Resolve the flattened state bits read by a hierarchical reference.
///
/// Static unpacked and packed indices retain their precise range. A dynamic
/// index conservatively covers the remaining slice at that dimension.
pub(crate) fn hierarchical_reference_bits(
    info: &VariableInfo,
    reference: &HierVarRef,
) -> Result<BitAccess, ParserError> {
    let invalid = |detail| invalid_hierarchical_reference(reference, detail);
    let array_total = info
        .array_dims
        .iter()
        .try_fold(1usize, |total, dimension| total.checked_mul(*dimension))
        .ok_or_else(|| invalid("array dimensions overflow usize"))?;
    if array_total == 0 || info.width == 0 || !info.width.is_multiple_of(array_total) {
        return Err(invalid(
            "the target variable has invalid flattened dimensions",
        ));
    }
    let element_width = info.width / array_total;
    let mut strides = vec![element_width; info.array_dims.len()];
    let mut stride = element_width;
    for (dimension, size) in info.array_dims.iter().enumerate().rev() {
        strides[dimension] = stride;
        stride = stride
            .checked_mul(*size)
            .ok_or_else(|| invalid("array stride overflows usize"))?;
    }

    let mut base = 0usize;
    let mut consumed = 0usize;
    for (dimension, expression) in reference.index.0.iter().enumerate() {
        let Some(&dimension_stride) = strides.get(dimension) else {
            return Err(invalid("too many unpacked array indices"));
        };
        let Some(index) = eval_constexpr(expression).and_then(|value| value.to_usize()) else {
            let width = if dimension == 0 {
                info.width
            } else {
                strides[dimension - 1]
            };
            return checked_reference_bits(reference, base, width, info.width);
        };
        if index >= info.array_dims[dimension] {
            return Err(invalid_hierarchical_reference(
                reference,
                format!(
                    "array index {index} is outside dimension width {}",
                    info.array_dims[dimension]
                ),
            ));
        }
        base = base
            .checked_add(
                index
                    .checked_mul(dimension_stride)
                    .ok_or_else(|| invalid("array index offset overflows usize"))?,
            )
            .ok_or_else(|| invalid("array index offset overflows usize"))?;
        consumed += 1;
    }

    let accessed_width = if consumed == 0 {
        info.width
    } else if consumed == info.array_dims.len() {
        element_width
    } else {
        strides[consumed - 1]
    };
    if reference.select.0.is_empty() && reference.select.1.is_none() {
        return checked_reference_bits(reference, base, accessed_width, info.width);
    }

    let packed_dimensions = if info.packed_dims.is_empty() {
        vec![element_width]
    } else {
        info.packed_dims.clone()
    };
    let mut packed_strides = vec![1usize; packed_dimensions.len()];
    let mut packed_stride = 1usize;
    for (dimension, size) in packed_dimensions.iter().enumerate().rev() {
        packed_strides[dimension] = packed_stride;
        packed_stride = packed_stride
            .checked_mul(*size)
            .ok_or_else(|| invalid("packed stride overflows usize"))?;
    }

    let prefix_count = if reference.select.1.is_some() {
        reference.select.0.len().saturating_sub(1)
    } else {
        reference.select.0.len()
    };
    for (dimension, expression) in reference.select.0[..prefix_count].iter().enumerate() {
        let Some(&scale) = packed_strides.get(dimension) else {
            return Err(invalid("too many packed indices"));
        };
        let Some(index) = eval_constexpr(expression).and_then(|value| value.to_usize()) else {
            let width = if dimension == 0 {
                accessed_width
            } else {
                packed_strides[dimension - 1]
            };
            return checked_reference_bits(reference, base, width, info.width);
        };
        base = base
            .checked_add(
                index
                    .checked_mul(scale)
                    .ok_or_else(|| invalid("packed index offset overflows usize"))?,
            )
            .ok_or_else(|| invalid("packed index offset overflows usize"))?;
    }

    let selected_width = if let Some((op, range_expression)) = &reference.select.1 {
        let anchor_expression = reference
            .select
            .0
            .last()
            .ok_or_else(|| invalid("part select is missing its anchor"))?;
        let current_width = if prefix_count == 0 {
            accessed_width
        } else {
            packed_strides[prefix_count - 1]
        };
        let Some(anchor) = eval_constexpr(anchor_expression).and_then(|value| value.to_usize())
        else {
            return checked_reference_bits(reference, base, current_width, info.width);
        };
        let Some(range) = eval_constexpr(range_expression).and_then(|value| value.to_usize())
        else {
            return checked_reference_bits(reference, base, current_width, info.width);
        };
        let scale = *packed_strides
            .get(prefix_count)
            .ok_or_else(|| invalid("part select exceeds packed dimensions"))?;
        let (relative_lsb, elements) = match op {
            veryl_analyzer::ir::VarSelectOp::Colon => {
                let elements = anchor
                    .checked_sub(range)
                    .and_then(|width| width.checked_add(1))
                    .ok_or_else(|| invalid("part-select range underflows"))?;
                (range, elements)
            }
            veryl_analyzer::ir::VarSelectOp::PlusColon => (anchor, range),
            veryl_analyzer::ir::VarSelectOp::MinusColon => {
                let lsb = anchor
                    .checked_add(1)
                    .and_then(|end| end.checked_sub(range))
                    .ok_or_else(|| invalid("part-select range underflows"))?;
                (lsb, range)
            }
            veryl_analyzer::ir::VarSelectOp::Step => {
                let lsb = anchor
                    .checked_mul(range)
                    .ok_or_else(|| invalid("part-select offset overflows usize"))?;
                (lsb, range)
            }
        };
        base = base
            .checked_add(
                relative_lsb
                    .checked_mul(scale)
                    .ok_or_else(|| invalid("part-select offset overflows usize"))?,
            )
            .ok_or_else(|| invalid("part-select offset overflows usize"))?;
        elements
            .checked_mul(scale)
            .ok_or_else(|| invalid("part-select width overflows usize"))?
    } else {
        *packed_strides
            .get(reference.select.0.len() - 1)
            .ok_or_else(|| invalid("packed select exceeds variable dimensions"))?
    };
    checked_reference_bits(reference, base, selected_width, info.width)
}

fn checked_reference_bits(
    reference: &HierVarRef,
    lsb: usize,
    width: usize,
    total_width: usize,
) -> Result<BitAccess, ParserError> {
    let msb = lsb
        .checked_add(width.checked_sub(1).ok_or_else(|| {
            invalid_hierarchical_reference(reference, "selected width must be nonzero")
        })?)
        .ok_or_else(|| invalid_hierarchical_reference(reference, "bit range overflows usize"))?;
    if msb >= total_width {
        return Err(invalid_hierarchical_reference(
            reference,
            "selected bit range is outside the target variable",
        ));
    }
    Ok(BitAccess::new(lsb, msb))
}

fn variable_access_width(
    info: &VariableInfo,
    index: &VarIndex,
    select: &VarSelect,
) -> Option<usize> {
    let array_total = info
        .array_dims
        .iter()
        .try_fold(1usize, |total, dimension| total.checked_mul(*dimension))?;
    if array_total == 0 || !info.width.is_multiple_of(array_total) {
        return None;
    }
    let consumed = index.0.len();
    if consumed > info.array_dims.len() {
        return None;
    }
    let element_width = info.width / array_total;
    let accessed_width = info.array_dims[consumed..]
        .iter()
        .try_fold(element_width, |width, dimension| {
            width.checked_mul(*dimension)
        })?;

    let packed_dimensions = if info.packed_dims.is_empty() {
        vec![element_width]
    } else {
        info.packed_dims.clone()
    };
    let mut packed_strides = vec![1usize; packed_dimensions.len()];
    let mut stride = 1usize;
    for (dimension, size) in packed_dimensions.iter().enumerate().rev() {
        packed_strides[dimension] = stride;
        stride = stride.checked_mul(*size)?;
    }

    if let Some((op, range_expression)) = &select.1 {
        let range = eval_constexpr(range_expression)?.to_usize()?;
        let dimension = select.0.len().checked_sub(1)?;
        let stride = *packed_strides.get(dimension)?;
        let elements = match op {
            VarSelectOp::Colon => {
                let anchor = eval_constexpr(select.0.last()?)?.to_usize()?;
                anchor.checked_sub(range)?.checked_add(1)
            }
            VarSelectOp::PlusColon | VarSelectOp::MinusColon | VarSelectOp::Step => Some(range),
        }?;
        return elements.checked_mul(stride);
    }
    if select.0.is_empty() {
        Some(accessed_width)
    } else {
        packed_strides.get(select.0.len() - 1).copied()
    }
}

enum TestbenchRead {
    Root(VarId),
    Hierarchical(Box<HierVarRef>),
}

pub fn collect_testbench_observability(
    lookup: &VerylFrontendLookup,
    source: &VerylTestbenchSource,
) -> Result<(Vec<RuntimeEventSite>, FxHashSet<StateAddr>), ParserError> {
    let Some(stmts) = source.initial_statements.as_ref() else {
        return Ok(Default::default());
    };
    let mut sites = Vec::new();
    collect_runtime_event_sites(stmts, &source.functions, &mut sites);
    let mut read_references = Vec::new();
    let mut active_functions = FxHashSet::default();
    collect_statement_reads(
        stmts,
        &source.functions,
        &mut active_functions,
        &mut read_references,
    );
    let mut reads = FxHashSet::default();
    for reference in read_references {
        match reference {
            TestbenchRead::Root(var_id) => {
                if let Some((address, _)) = lookup.root_variable(var_id) {
                    reads.insert(address);
                }
            }
            TestbenchRead::Hierarchical(reference) => {
                let (address, _) = resolve_hierarchical_reference(lookup, &reference)?;
                reads.insert(address);
            }
        }
    }
    Ok((sites, reads))
}

fn collect_statement_reads(
    stmts: &[Statement],
    funcs: &fxhash::FxHashMap<VarId, Function>,
    active_functions: &mut FxHashSet<VarId>,
    reads: &mut Vec<TestbenchRead>,
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
                TbMethod::Component { args, .. } => {
                    for arg in args {
                        collect_expression_reads(&arg.0, funcs, active_functions, reads);
                    }
                }
                TbMethod::RandomSeed { value } => {
                    collect_expression_reads(value, funcs, active_functions, reads);
                }
                TbMethod::RandomGetRange { min, max, .. } => {
                    collect_expression_reads(min, funcs, active_functions, reads);
                    collect_expression_reads(max, funcs, active_functions, reads);
                }
                TbMethod::RandomGet { .. } | TbMethod::RandomGetSeed => {}
            },
            Statement::Break | Statement::Unsupported(_) | Statement::Null => {}
        }
    }
}

fn collect_for_bound_reads(
    range: &ForRange,
    funcs: &fxhash::FxHashMap<VarId, Function>,
    active_functions: &mut FxHashSet<VarId>,
    reads: &mut Vec<TestbenchRead>,
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
    reads: &mut Vec<TestbenchRead>,
) {
    match expr {
        Expression::Term(factor) => match factor.as_ref() {
            Factor::Variable(var_id, index, select, _) => {
                reads.push(TestbenchRead::Root(*var_id));
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
            Factor::HierVariable(reference) => {
                reads.push(TestbenchRead::Hierarchical(reference.clone()));
                for expr in &reference.index.0 {
                    collect_expression_reads(expr, funcs, active_functions, reads);
                }
                collect_select_reads(&reference.select, funcs, active_functions, reads);
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
    reads: &mut Vec<TestbenchRead>,
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
    reads: &mut Vec<TestbenchRead>,
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
    reads: &mut Vec<TestbenchRead>,
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
        self.resolved_variable_access_width(expr)
            .or_else(|| get_expr_width(expr))
            .filter(|width| *width != 0)
            .unwrap_or_else(|| self.infer_expr_width(expr).max(1))
    }

    fn resolved_variable_access_width(&self, expr: &Expression) -> Option<usize> {
        let Expression::Term(factor) = expr else {
            return None;
        };
        match factor.as_ref() {
            Factor::Variable(var_id, index, select, _) => {
                let (_, info) = self.lookup.root_variable(*var_id)?;
                Some(variable_access_width(info, index, select).unwrap_or(info.width))
            }
            Factor::HierVariable(reference) => {
                let (_, info) = resolve_hierarchical_reference(self.lookup, reference).ok()?;
                Some(
                    variable_access_width(info, &reference.index, &reference.select)
                        .unwrap_or(info.width),
                )
            }
            _ => None,
        }
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
                } else if let Some((address, info)) = self.lookup.root_variable(*var_id) {
                    self.emit_var_access(address, info, index, select, ops);
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
            Factor::HierVariable(reference) => {
                let (address, info) = resolve_hierarchical_reference(self.lookup, reference)
                    .expect("hierarchical testbench references are validated before emission");
                self.emit_var_access(address, info, &reference.index, &reference.select, ops);
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
        address: StateAddr,
        info: &VariableInfo,
        index: &veryl_analyzer::ir::VarIndex,
        select: &veryl_analyzer::ir::VarSelect,
        ops: &mut Vec<UnboundTbOpcode>,
    ) {
        // No index or select → whole variable
        if index.0.is_empty() && select.0.is_empty() && select.1.is_none() {
            self.emit_load_at(address, 0, info.width, ops);
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

        // Build one flattened bit offset for all unpacked and packed indices.
        // Dynamic terms remain on the expression stack until the final load.
        let mut static_bit_offset: usize = 0;
        let mut dynamic_terms = 0usize;

        for (i, idx_expr) in index.0.iter().enumerate() {
            if i >= info.array_dims.len() {
                break;
            }
            let stride = strides_bits[i];

            if let Some(idx_val) = Self::try_const_usize(idx_expr) {
                // Static index: accumulate into offset
                static_bit_offset += idx_val * stride;
            } else {
                // Keep the dynamic flattened bit offset on the stack. All
                // dimensions are consumed before the selected subarray is
                // loaded, so a dynamic outer index does not discard inner
                // indices or force the result down to one scalar element.
                self.emit(idx_expr, ops);
                Self::finish_dynamic_offset_term(stride, ops, &mut dynamic_terms);
            }
        }

        let accessed_width = if index.0.len() >= info.array_dims.len() {
            element_width
        } else if index.0.is_empty() {
            info.width
        } else {
            strides_bits[index.0.len() - 1]
        };

        let (select_offset, selected_width) =
            self.emit_select_offset(info, select, accessed_width, ops, &mut dynamic_terms);
        static_bit_offset += select_offset;

        if dynamic_terms != 0 {
            ops.push(TbOpcode::LoadIndexed {
                location: Self::state_location_at(address, 0),
                stride_bits: 1,
                base_bit_offset: static_bit_offset,
                element_width: selected_width,
            });
            return;
        }

        let bit_offset = static_bit_offset;
        let byte_offset = bit_offset / 8;
        let sub = bit_offset % 8;
        if sub == 0 {
            self.emit_load_at(address, byte_offset, selected_width, ops);
        } else {
            let load_width = selected_width + sub;
            self.emit_load_at(address, byte_offset, load_width, ops);
            ops.push(TbOpcode::ConstU64(sub as u64));
            ops.push(TbOpcode::BinOp(Op::LogicShiftR));
            if selected_width < 64 {
                ops.push(TbOpcode::ConstU64((1u64 << selected_width) - 1));
                ops.push(TbOpcode::BinOp(Op::BitAnd));
            }
        }
    }

    fn finish_dynamic_offset_term(
        multiplier: usize,
        ops: &mut Vec<UnboundTbOpcode>,
        dynamic_terms: &mut usize,
    ) {
        if multiplier != 1 {
            ops.push(TbOpcode::ConstU64(multiplier as u64));
            ops.push(TbOpcode::BinOp(Op::Mul));
        }
        if *dynamic_terms != 0 {
            ops.push(TbOpcode::BinOp(Op::Add));
        }
        *dynamic_terms += 1;
    }

    /// Append packed indices to the flattened bit offset and return the
    /// remaining static offset plus the selected result width.
    fn emit_select_offset(
        &self,
        info: &VariableInfo,
        select: &VarSelect,
        accessed_width: usize,
        ops: &mut Vec<UnboundTbOpcode>,
        dynamic_terms: &mut usize,
    ) -> (usize, usize) {
        if select.0.is_empty() && select.1.is_none() {
            return (0, accessed_width);
        }

        let packed_dimensions = if info.packed_dims.is_empty() {
            vec![accessed_width]
        } else {
            info.packed_dims.clone()
        };
        let mut strides = vec![1usize; packed_dimensions.len()];
        let mut stride = 1usize;
        for (dimension, size) in packed_dimensions.iter().enumerate().rev() {
            strides[dimension] = stride;
            stride = stride.saturating_mul(*size);
        }

        let prefix_count = if select.1.is_some() {
            select.0.len().saturating_sub(1)
        } else {
            select.0.len()
        };
        let mut static_offset = 0usize;
        for (dimension, expression) in select.0[..prefix_count].iter().enumerate() {
            let scale = strides[dimension];
            if let Some(index) = Self::try_const_usize(expression) {
                static_offset += index * scale;
            } else {
                self.emit(expression, ops);
                Self::finish_dynamic_offset_term(scale, ops, dynamic_terms);
            }
        }

        let Some((op, range_expression)) = &select.1 else {
            let selected_width = strides[select.0.len() - 1];
            return (static_offset, selected_width);
        };
        let anchor_expression = select
            .0
            .last()
            .expect("validated part select has an anchor");
        let range = Self::try_const_usize(range_expression)
            .expect("validated part-select range is compile-time constant");
        let scale = strides[prefix_count];
        let elements = match op {
            VarSelectOp::Colon => {
                let anchor = Self::try_const_usize(anchor_expression)
                    .expect("validated colon-select anchor is compile-time constant");
                static_offset += range * scale;
                anchor - range + 1
            }
            VarSelectOp::PlusColon => {
                if let Some(anchor) = Self::try_const_usize(anchor_expression) {
                    static_offset += anchor * scale;
                } else {
                    self.emit(anchor_expression, ops);
                    Self::finish_dynamic_offset_term(scale, ops, dynamic_terms);
                }
                range
            }
            VarSelectOp::MinusColon => {
                if let Some(anchor) = Self::try_const_usize(anchor_expression) {
                    static_offset += (anchor + 1 - range) * scale;
                } else {
                    self.emit(anchor_expression, ops);
                    ops.push(TbOpcode::ConstU64(1));
                    ops.push(TbOpcode::BinOp(Op::Add));
                    ops.push(TbOpcode::ConstU64(range as u64));
                    ops.push(TbOpcode::BinOp(Op::Sub));
                    Self::finish_dynamic_offset_term(scale, ops, dynamic_terms);
                }
                range
            }
            VarSelectOp::Step => {
                let multiplier = range.saturating_mul(scale);
                if let Some(anchor) = Self::try_const_usize(anchor_expression) {
                    static_offset += anchor * multiplier;
                } else {
                    self.emit(anchor_expression, ops);
                    Self::finish_dynamic_offset_term(multiplier, ops, dynamic_terms);
                }
                range
            }
        };
        (static_offset, elements * scale)
    }

    /// Emit a LoadU64 or LoadWide opcode for the given byte offset and bit width.
    fn emit_load(
        &self,
        var_id: VarId,
        byte_offset: usize,
        width: usize,
        ops: &mut Vec<UnboundTbOpcode>,
    ) {
        let address = self
            .lookup
            .root_variable(var_id)
            .map(|(address, _)| address)
            .expect("frontend state projection is complete");
        self.emit_load_at(address, byte_offset, width, ops);
    }

    fn emit_load_at(
        &self,
        address: StateAddr,
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
                location: Self::state_location_at(address, byte_offset),
                byte_size,
                mask,
            });
        } else {
            ops.push(TbOpcode::LoadWide {
                location: Self::state_location_at(address, byte_offset),
                byte_size,
                width,
            });
        }
    }

    fn state_location(&self, var_id: VarId, byte_offset: usize) -> StateLocation<StateAddr> {
        let address = self
            .lookup
            .root_variable(var_id)
            .map(|(address, _)| address)
            .expect("frontend state projection is complete");
        Self::state_location_at(address, byte_offset)
    }

    fn state_location_at(address: StateAddr, byte_offset: usize) -> StateLocation<StateAddr> {
        StateLocation {
            address,
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
        if let Some(width) = self.resolved_variable_access_width(expr) {
            return width;
        }
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
        // For terms, use constant value widths as a final fallback.
        if let Expression::Term(f) = expr {
            if let Factor::Value(c) = f.as_ref()
                && let Ok(v) = c.get_value()
            {
                return v.width();
            }
        }
        0
    }

    fn try_const_usize(expr: &Expression) -> Option<usize> {
        eval_constexpr(expr).and_then(|value| value.to_usize())
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
        let mut active_functions = FxHashSet::default();
        self.scan_tb_methods(
            stmts,
            &mut clock_insts,
            &mut reset_insts,
            &mut active_functions,
        );
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

    fn scan_tb_methods(
        &self,
        stmts: &[Statement],
        clks: &mut Vec<StrId>,
        rsts: &mut Vec<StrId>,
        active_functions: &mut FxHashSet<VarId>,
    ) {
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
                    | TbMethod::FileFlush
                    | TbMethod::Component { .. }
                    | TbMethod::RandomSeed { .. }
                    | TbMethod::RandomGet { .. }
                    | TbMethod::RandomGetRange { .. }
                    | TbMethod::RandomGetSeed => {}
                },
                Statement::If(s) => {
                    self.scan_tb_methods(&s.true_side, clks, rsts, active_functions);
                    self.scan_tb_methods(&s.false_side, clks, rsts, active_functions);
                }
                Statement::For(s) => {
                    self.scan_tb_methods(&s.body, clks, rsts, active_functions);
                }
                Statement::FunctionCall(call) if active_functions.insert(call.id) => {
                    if let Some(body) = function_body(&self.testbench_source.functions, call) {
                        self.scan_tb_methods(&body.statements, clks, rsts, active_functions);
                    }
                    active_functions.remove(&call.id);
                }
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
                let duration = match duration {
                    Some(expr) => match try_eval_const(expr) {
                        Some(duration) => GenericClockCount::Static(duration),
                        None => GenericClockCount::Dynamic(ec.compile(expr)),
                    },
                    None => GenericClockCount::Static(self.default_reset_duration),
                };
                // Determine reset polarity from the variable's DomainKind
                let (assert_value, deassert_value) = self.resolve_reset_polarity(&tb.inst);
                Some(GenericTestbenchStatement::ResetAssert {
                    reset_signal,
                    clock_event,
                    duration,
                    assert_value,
                    deassert_value,
                })
            }
            TbMethod::RandomSeed { value } => Some(GenericTestbenchStatement::RandomSeed {
                handle: resource_table::get_str_value(tb.inst).unwrap_or_default(),
                value: ec.compile(value),
            }),
            TbMethod::RandomGet { width, signed } => {
                let ret = match tb.ret.as_deref() {
                    Some(dst) => Some(ec.resolve_var(&dst.id)?),
                    None => None,
                };
                Some(GenericTestbenchStatement::RandomGet {
                    handle: resource_table::get_str_value(tb.inst).unwrap_or_default(),
                    width: *width,
                    signed: *signed,
                    ret,
                })
            }
            TbMethod::RandomGetRange {
                min,
                max,
                width,
                signed,
            } => {
                let ret = match tb.ret.as_deref() {
                    Some(dst) => Some(ec.resolve_var(&dst.id)?),
                    None => None,
                };
                Some(GenericTestbenchStatement::RandomGetRange {
                    handle: resource_table::get_str_value(tb.inst).unwrap_or_default(),
                    min: ec.compile(min),
                    max: ec.compile(max),
                    width: *width,
                    signed: *signed,
                    ret,
                })
            }
            TbMethod::RandomGetSeed => {
                let ret = match tb.ret.as_deref() {
                    Some(dst) => Some(ec.resolve_var(&dst.id)?),
                    None => None,
                };
                Some(GenericTestbenchStatement::RandomGetSeed {
                    handle: resource_table::get_str_value(tb.inst).unwrap_or_default(),
                    ret,
                })
            }
            TbMethod::FileOpen { .. }
            | TbMethod::FileWrite { .. }
            | TbMethod::FileClose
            | TbMethod::FileFlush
            | TbMethod::Component { .. } => None,
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
            _ if expr.comptime().is_const => expr
                .comptime()
                .get_value()
                .ok()
                .map(|value| value.payload_u64()),
            _ => None,
        },
        _ if expr.comptime().is_const => expr
            .comptime()
            .get_value()
            .ok()
            .map(|value| value.payload_u64()),
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

fn validate_testbench_function_call(
    call: &FunctionCall,
    source: &VerylTestbenchSource,
    active_functions: &mut FxHashSet<(VarId, Option<Vec<usize>>)>,
) -> Result<(), ParserError> {
    for expression in call.inputs.values() {
        validate_testbench_expression(expression, source, active_functions)?;
    }

    let key = (call.id, call.index.clone());
    if !active_functions.insert(key.clone()) {
        return Ok(());
    }
    let result = if let Some(body) = function_body(&source.functions, call) {
        validate_testbench_statements(&body.statements, source, active_functions)
    } else {
        Ok(())
    };
    active_functions.remove(&key);
    result
}

fn validate_testbench_expression(
    expression: &Expression,
    source: &VerylTestbenchSource,
    active_functions: &mut FxHashSet<(VarId, Option<Vec<usize>>)>,
) -> Result<(), ParserError> {
    match expression {
        Expression::Term(factor) => match factor.as_ref() {
            Factor::HierVariable(reference) => {
                for expression in reference.index.0.iter().chain(reference.select.0.iter()) {
                    validate_testbench_expression(expression, source, active_functions)?;
                }
                if let Some((_, expression)) = &reference.select.1 {
                    validate_testbench_expression(expression, source, active_functions)?;
                }
                Ok(())
            }
            Factor::Variable(_, index, select, _) => {
                for expression in index.0.iter().chain(select.0.iter()) {
                    validate_testbench_expression(expression, source, active_functions)?;
                }
                if let Some((_, expression)) = &select.1 {
                    validate_testbench_expression(expression, source, active_functions)?;
                }
                Ok(())
            }
            Factor::FunctionCall(call) => {
                validate_testbench_function_call(call, source, active_functions)
            }
            Factor::SystemFunctionCall(call) => {
                validate_testbench_system_function(&call.kind, source, active_functions)
            }
            Factor::Anonymous(comptime) | Factor::Unknown(comptime)
                if comptime.get_value().is_err() =>
            {
                let token = &comptime.token;
                let source = token.source().get_text();
                let start = token.beg.pos as usize;
                let end = (token.end.pos + token.end.length) as usize;
                let factor_text = source.get(start..end).unwrap_or_default();
                if factor_text.contains('.') {
                    Err(ParserError::illegal_context(
                        "hierarchical variable reference",
                        format!("`{factor_text}` was not resolved by the analyzer"),
                        Some(token),
                    ))
                } else {
                    Err(ParserError::unsupported(
                        67,
                        LoweringPhase::SimulatorParser,
                        "unresolved factor in testbench expression",
                        format!("{factor:?}"),
                        Some(token),
                    ))
                }
            }
            Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => Ok(()),
        },
        Expression::Unary(_, inner, _) => {
            validate_testbench_expression(inner, source, active_functions)
        }
        Expression::Binary(lhs, _, rhs, _) => {
            validate_testbench_expression(lhs, source, active_functions)?;
            validate_testbench_expression(rhs, source, active_functions)
        }
        Expression::Ternary(condition, then_expression, else_expression, _) => {
            validate_testbench_expression(condition, source, active_functions)?;
            validate_testbench_expression(then_expression, source, active_functions)?;
            validate_testbench_expression(else_expression, source, active_functions)
        }
        Expression::Concatenation(items, _) => {
            for (expression, repeat) in items {
                validate_testbench_expression(expression, source, active_functions)?;
                if let Some(repeat) = repeat {
                    validate_testbench_expression(repeat, source, active_functions)?;
                }
            }
            Ok(())
        }
        Expression::ArrayLiteral(items, _) => {
            for item in items {
                match item {
                    ArrayLiteralItem::Value(expression, repeat) => {
                        validate_testbench_expression(expression, source, active_functions)?;
                        if let Some(repeat) = repeat {
                            validate_testbench_expression(repeat, source, active_functions)?;
                        }
                    }
                    ArrayLiteralItem::Defaul(expression) => {
                        validate_testbench_expression(expression, source, active_functions)?;
                    }
                }
            }
            Ok(())
        }
        Expression::StructConstructor(_, fields, _) => {
            for (_, expression) in fields {
                validate_testbench_expression(expression, source, active_functions)?;
            }
            Ok(())
        }
    }
}

fn validate_testbench_system_function(
    kind: &SystemFunctionKind,
    source: &VerylTestbenchSource,
    active_functions: &mut FxHashSet<(VarId, Option<Vec<usize>>)>,
) -> Result<(), ParserError> {
    let mut validate = |input: &SystemFunctionInput| {
        validate_testbench_expression(&input.0, source, active_functions)
    };
    match kind {
        SystemFunctionKind::Bits(input)
        | SystemFunctionKind::Size(input)
        | SystemFunctionKind::Clog2(input)
        | SystemFunctionKind::Onehot(input)
        | SystemFunctionKind::Signed(input)
        | SystemFunctionKind::Unsigned(input) => validate(input),
        SystemFunctionKind::Display(inputs) | SystemFunctionKind::Write(inputs) => {
            for input in inputs {
                validate(input)?;
            }
            Ok(())
        }
        SystemFunctionKind::Assert { cond, args, .. } => {
            validate(cond)?;
            for input in args {
                validate(input)?;
            }
            Ok(())
        }
        SystemFunctionKind::Readmemh(input, _) => validate(input),
        SystemFunctionKind::Finish => Ok(()),
    }
}

fn validate_testbench_destination(
    destination: &veryl_analyzer::ir::AssignDestination,
) -> Result<(), ParserError> {
    if !destination.index.0.is_empty()
        || !destination.select.0.is_empty()
        || destination.select.1.is_some()
    {
        return Err(ParserError::unsupported(
            478,
            LoweringPhase::SimulatorParser,
            "selected native testbench assignment",
            "selected destinations are not represented by the testbench AIR",
            Some(&destination.token),
        ));
    }
    Ok(())
}

fn validate_testbench_statements(
    statements: &[Statement],
    source: &VerylTestbenchSource,
    active_functions: &mut FxHashSet<(VarId, Option<Vec<usize>>)>,
) -> Result<(), ParserError> {
    for statement in statements {
        match statement {
            Statement::Assign(statement) => {
                for destination in &statement.dst {
                    validate_testbench_destination(destination)?;
                    for expression in &destination.index.0 {
                        validate_testbench_expression(expression, source, active_functions)?;
                    }
                    for expression in &destination.select.0 {
                        validate_testbench_expression(expression, source, active_functions)?;
                    }
                    if let Some((_, expression)) = &destination.select.1 {
                        validate_testbench_expression(expression, source, active_functions)?;
                    }
                }
                validate_testbench_expression(&statement.expr, source, active_functions)?
            }
            Statement::If(statement) => {
                validate_testbench_expression(&statement.cond, source, active_functions)?;
                validate_testbench_statements(&statement.true_side, source, active_functions)?;
                validate_testbench_statements(&statement.false_side, source, active_functions)?;
            }
            Statement::IfReset(statement) => {
                validate_testbench_statements(&statement.true_side, source, active_functions)?;
                validate_testbench_statements(&statement.false_side, source, active_functions)?;
            }
            Statement::Case(statement) => {
                validate_testbench_expression(&statement.case_target, source, active_functions)?;
                for arm in &statement.arms {
                    for pattern in &arm.patterns {
                        match pattern {
                            CasePattern::Eq(expression) => {
                                validate_testbench_expression(
                                    expression,
                                    source,
                                    active_functions,
                                )?;
                            }
                            CasePattern::Range { lo, hi, .. } => {
                                validate_testbench_expression(lo, source, active_functions)?;
                                validate_testbench_expression(hi, source, active_functions)?;
                            }
                        }
                    }
                    validate_testbench_statements(&arm.body, source, active_functions)?;
                }
                validate_testbench_statements(&statement.default, source, active_functions)?;
            }
            Statement::For(statement) => {
                let (start, end) = match &statement.range {
                    ForRange::Forward { start, end, .. }
                    | ForRange::Reverse { start, end, .. }
                    | ForRange::Stepped { start, end, .. } => (start, end),
                };
                for bound in [start, end] {
                    if let ForBound::Expression(expression) = bound {
                        validate_testbench_expression(expression, source, active_functions)?;
                    }
                }
                validate_testbench_statements(&statement.body, source, active_functions)?;
            }
            Statement::SystemFunctionCall(call) => {
                validate_testbench_system_function(&call.kind, source, active_functions)?;
            }
            Statement::FunctionCall(call) => {
                validate_testbench_function_call(call, source, active_functions)?;
            }
            Statement::TbMethodCall(call) => {
                if let Some(destination) = call.ret.as_deref() {
                    validate_testbench_destination(destination)?;
                }
                match &call.method {
                    TbMethod::Component { .. } => {
                        return Err(ParserError::unsupported(
                            468,
                            LoweringPhase::SimulatorParser,
                            "testbench component method",
                            "component runtime integration is not implemented",
                            None,
                        ));
                    }
                    TbMethod::RandomSeed { value } => {
                        validate_testbench_expression(value, source, active_functions)?;
                    }
                    TbMethod::RandomGetRange { min, max, .. } => {
                        validate_testbench_expression(min, source, active_functions)?;
                        validate_testbench_expression(max, source, active_functions)?;
                    }
                    TbMethod::RandomGet { .. } | TbMethod::RandomGetSeed => {}
                    TbMethod::ClockNext { count, period } => {
                        if let Some(expression) = count {
                            validate_testbench_expression(expression, source, active_functions)?;
                        }
                        if let Some(expression) = period {
                            validate_testbench_expression(expression, source, active_functions)?;
                        }
                    }
                    TbMethod::ResetAssert { duration, .. } => {
                        if let Some(expression) = duration {
                            validate_testbench_expression(expression, source, active_functions)?;
                        }
                    }
                    TbMethod::FileOpen { name, .. } => {
                        validate_testbench_expression(&name.0, source, active_functions)?
                    }
                    TbMethod::FileWrite { args } => {
                        for argument in args {
                            validate_testbench_expression(&argument.0, source, active_functions)?;
                        }
                    }
                    TbMethod::FileClose | TbMethod::FileFlush => {}
                }
            }
            Statement::Break | Statement::Unsupported(_) | Statement::Null => {}
        }
    }
    Ok(())
}

pub fn compile_semantic_testbench(
    lookup: &VerylFrontendLookup,
    source: &VerylTestbenchSource,
    runtime_event_site_count: usize,
    random_seed: Option<u64>,
) -> Result<Option<TestbenchProgram<StateAddr>>, ParserError> {
    let Some(initial_stmts) = source.initial_statements.as_ref() else {
        return Ok(None);
    };
    // Resolve every hierarchical read before the infallible bytecode emitter
    // runs. The same walk also guarantees direct callers get path diagnostics,
    // even when observability projection is not invoked separately.
    let _ = collect_testbench_observability(lookup, source)?;
    validate_testbench_statements(initial_stmts, source, &mut FxHashSet::default())?;
    let mut builder = SemanticTestbenchBuilder::new(lookup, source, runtime_event_site_count);
    builder.build_event_map(initial_stmts);
    Ok(Some(
        TestbenchProgram::new(builder.convert(initial_stmts)).with_random_seed_option(random_seed),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use celox_design::{
        DomainKind, InstanceId, ModuleId, PortTypeKind, StateObjectId, VariableMetadata,
    };
    use veryl_analyzer::ir::{Comptime, VarIndex, VarKind, VarPath, VarSelect};

    fn reference(inst_path: Vec<StrId>, var_path: VarPath) -> HierVarRef {
        HierVarRef {
            inst_path,
            var_path,
            index: VarIndex::default(),
            select: VarSelect::default(),
            comptime: Comptime::default(),
        }
    }

    fn lookup_with_child(child_name: StrId, variable_name: StrId) -> VerylFrontendLookup {
        let root_instance = InstanceId(0);
        let child_instance = InstanceId(1);
        let root_module = ModuleId(0);
        let child_module = ModuleId(1);
        let var_id = VarId::from_raw(0);
        let source_address = AbsoluteAddr {
            instance_id: child_instance,
            var_id,
        };
        let state_address = StateAddr {
            instance_id: child_instance,
            var_id: StateObjectId(0),
        };
        let path = VarPath(vec![variable_name]);
        let info = VariableInfo {
            id: var_id,
            path: path.clone(),
            var_kind: VarKind::Variable,
            metadata: VariableMetadata {
                width: 8,
                is_4state: true,
                kind: DomainKind::Other,
                type_kind: PortTypeKind::Logic,
                array_dims: Vec::new(),
                packed_dims: vec![8],
            },
        };

        let mut lookup = VerylFrontendLookup::default();
        lookup
            .instance_ids
            .insert(InstancePath(Vec::new()), root_instance);
        lookup
            .instance_ids
            .insert(InstancePath(vec![(child_name, 0)]), child_instance);
        lookup.instance_module.insert(root_instance, root_module);
        lookup.instance_module.insert(child_instance, child_module);
        lookup
            .module_variables
            .entry(child_module)
            .or_default()
            .insert(var_id, info);
        lookup
            .module_var_path_index
            .entry(child_module)
            .or_default()
            .insert(path, Some(var_id));
        lookup.source_to_state.insert(source_address, state_address);
        lookup.state_to_source.insert(state_address, source_address);
        lookup
    }

    #[test]
    fn hierarchical_reference_reports_missing_instance_segment() {
        let dut = resource_table::insert_str("dut");
        let missing = resource_table::insert_str("missing");
        let q = resource_table::insert_str("q");
        let lookup = lookup_with_child(dut, q);
        let reference = reference(vec![missing], VarPath(vec![q]));

        let error = resolve_hierarchical_reference(&lookup, &reference).unwrap_err();
        let ParserError::IllegalContext {
            feature, detail, ..
        } = error
        else {
            panic!("expected invalid hierarchical path diagnostic");
        };
        assert_eq!(feature, "hierarchical variable reference");
        assert!(detail.contains("instance `missing` was not found"));
    }

    #[test]
    fn hierarchical_reference_reports_missing_target_variable() {
        let dut = resource_table::insert_str("dut");
        let q = resource_table::insert_str("q");
        let missing = resource_table::insert_str("missing");
        let lookup = lookup_with_child(dut, q);
        let reference = reference(vec![dut], VarPath(vec![missing]));

        let error = resolve_hierarchical_reference(&lookup, &reference).unwrap_err();
        let ParserError::IllegalContext {
            feature, detail, ..
        } = error
        else {
            panic!("expected invalid hierarchical variable diagnostic");
        };
        assert_eq!(feature, "hierarchical variable reference");
        assert!(detail.contains("variable `missing` was not found"));
    }

    #[test]
    fn hierarchical_message_argument_width_uses_resolved_variable_metadata() {
        let dut = resource_table::insert_str("dut");
        let q = resource_table::insert_str("q");
        let lookup = lookup_with_child(dut, q);
        let source = VerylTestbenchSource::default();
        let compiler = ExprCompiler {
            lookup: &lookup,
            testbench_source: &source,
        };
        let input = SystemFunctionInput(Expression::Term(Box::new(Factor::HierVariable(
            Box::new(reference(vec![dut], VarPath(vec![q]))),
        ))));

        assert_eq!(compile_assert_arg(&input, &compiler).width, 8);
    }
}
