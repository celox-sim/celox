//! The design state a testbench reads.

use super::*;

pub fn collect_testbench_observability(
    lookup: &FrontendLookup,
    source: &VerylTestbenchSource,
) -> Result<(Vec<RuntimeEventSite>, FxHashSet<StateAddr>), ParserError> {
    let mut sites = Vec::new();
    let mut reads = FxHashSet::default();
    for source in source.sources() {
        let (local_sites, local_reads) = collect_instance_testbench_observability(lookup, source)?;
        sites.extend(local_sites);
        reads.extend(local_reads);
    }
    Ok((sites, reads))
}

pub(super) fn collect_instance_testbench_observability(
    lookup: &FrontendLookup,
    source: &VerylTestbenchSource,
) -> Result<(Vec<RuntimeEventSite>, FxHashSet<StateAddr>), ParserError> {
    let stmts = source.initial_statements.as_deref().unwrap_or(&[]);
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
                let root_instance = source.base_instance(lookup);
                if let Some(source_id) = source.id_map.instance_var(lookup, root_instance, var_id)
                    && let Some((address, _)) = lookup.instance_variable(root_instance, source_id)
                {
                    reads.insert(address);
                }
            }
            TestbenchRead::Hierarchical(reference) => {
                let (address, _) = resolve_hierarchical_reference_from(
                    lookup,
                    source.base_instance(lookup),
                    &reference,
                )?;
                reads.insert(address);
            }
        }
    }
    for component in &source.component_bindings {
        let mut component_reads = Vec::new();
        for connection in &component.connections {
            if let Some(input) = &connection.input {
                collect_expression_reads(
                    input,
                    &component.functions,
                    &mut active_functions,
                    &mut component_reads,
                );
            }
            if let Some(output) = &connection.output {
                collect_target_reads(
                    output,
                    &component.functions,
                    &mut active_functions,
                    &mut component_reads,
                );
            }
        }
        for reference in component_reads {
            let address = match reference {
                TestbenchRead::Root(var_id) => source
                    .id_map
                    .instance_var(lookup, component.parent_instance, var_id)
                    .and_then(|source_id| {
                        lookup
                            .instance_variable(component.parent_instance, source_id)
                            .map(|(address, _)| address)
                    }),
                TestbenchRead::Hierarchical(reference) => resolve_hierarchical_reference_from(
                    lookup,
                    component.parent_instance,
                    &reference,
                )
                .ok()
                .map(|(address, _)| address),
            }
            .ok_or_else(|| {
                ParserError::illegal_context(
                    "testbench component elaboration",
                    format!(
                        "component `{}` input has no runtime state binding",
                        component.instance
                    ),
                    None,
                )
            })?;
            reads.insert(address);
        }
    }
    Ok((sites, reads))
}

pub(super) fn collect_statement_reads(
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
                    collect_target_reads(dst, funcs, active_functions, reads);
                }
                if let Some(destination) = &assign.hier_dst {
                    // Keep the destination address and any read-modify-write
                    // state alive even when its value is never asserted.
                    reads.push(TestbenchRead::Hierarchical(Box::new(
                        hierarchical_destination_reference(destination),
                    )));
                    for index in &destination.index.0 {
                        collect_expression_reads(index, funcs, active_functions, reads);
                    }
                    collect_select_reads(&destination.select, funcs, active_functions, reads);
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
            Statement::TbMethodCall(call) => {
                // A selected random return is a read-modify-write in the
                // host-side testbench runtime.  Keep both its dynamic
                // addressing expressions and the old root value observable.
                if let Some(ret) = call.ret.as_deref() {
                    collect_target_reads(ret, funcs, active_functions, reads);
                }
                match &call.method {
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
                }
            }
            Statement::Break | Statement::Unsupported(_) | Statement::Null => {}
        }
    }
}

pub(super) fn collect_target_reads(
    target: &veryl_analyzer::ir::AssignDestination,
    funcs: &fxhash::FxHashMap<VarId, Function>,
    active_functions: &mut FxHashSet<VarId>,
    reads: &mut Vec<TestbenchRead>,
) {
    for expr in &target.index.0 {
        collect_expression_reads(expr, funcs, active_functions, reads);
    }
    collect_select_reads(&target.select, funcs, active_functions, reads);

    // The native selected-target path reads the complete root signal before
    // merging the new slice.  Model that read as a DSE root; a whole-root
    // assignment does not need this extra liveness edge.
    if !target.index.0.is_empty() || !target.select.0.is_empty() || target.select.1.is_some() {
        reads.push(TestbenchRead::Root(target.id));
    }
}

pub(super) fn collect_for_bound_reads(
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

pub(super) fn collect_expression_reads(
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

pub(super) fn collect_select_reads(
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

pub(super) fn collect_system_function_reads(
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
        | SystemFunctionKind::Size(input, _)
        | SystemFunctionKind::Clog2(input)
        | SystemFunctionKind::Onehot(input)
        | SystemFunctionKind::Signed(input)
        | SystemFunctionKind::Unsigned(input) => collect_input(input),
        SystemFunctionKind::Readmemh(input, output) => {
            collect_input(input);
            if let SystemFunctionOutput::Hier(reference) = output {
                reads.push(TestbenchRead::Hierarchical(reference.clone()));
            }
        }
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

pub(super) fn collect_function_call_reads(
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
