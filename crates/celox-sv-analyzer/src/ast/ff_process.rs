//! Sequential process collection and event-control parsing.

use super::*;

pub(super) fn ff_processes_from_module_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
    packed_dimensions: &PackedDimensions,
    state: &mut procedural::BodyState<'_>,
) -> Result<Vec<FfProcess>, AnalyzerError> {
    let mut processes = Vec::new();
    for item in generate::items(
        node,
        syntax_tree,
        const_env,
        &packed_dimensions.type_aliases,
    )? {
        let start = processes.len();
        let dimensions = item.dimensions(packed_dimensions);
        let literals = item.parameter_literals(parameter_literals);
        ff_processes_from_module_or_generate_item(
            item.node,
            syntax_tree,
            &item.env,
            &literals,
            &dimensions,
            &mut processes,
            state,
        )?;
        for process in &mut processes[start..] {
            for event in &mut process.events {
                event.signal = item.name(&event.signal);
            }
            for stmt in &mut process.body {
                procedural::qualify_stmt(&item, stmt);
            }
        }
    }
    Ok(processes)
}

fn ff_processes_from_module_or_generate_item(
    item: &sv_parser::ModuleOrGenerateItem,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
    packed_dimensions: &PackedDimensions,
    processes: &mut Vec<FfProcess>,
    state: &mut procedural::BodyState<'_>,
) -> Result<(), AnalyzerError> {
    if let sv_parser::ModuleOrGenerateItem::ModuleItem(item) = item {
        ff_processes_from_module_common_item(
            &item.nodes.1,
            syntax_tree,
            const_env,
            parameter_literals,
            packed_dimensions,
            processes,
            state,
        )?;
    }
    Ok(())
}

fn ff_processes_from_module_common_item(
    item: &sv_parser::ModuleCommonItem,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
    packed_dimensions: &PackedDimensions,
    processes: &mut Vec<FfProcess>,
    state: &mut procedural::BodyState<'_>,
) -> Result<(), AnalyzerError> {
    if let sv_parser::ModuleCommonItem::AlwaysConstruct(always) = item {
        if let Some(process) = ff_process_from_always_construct(
            always,
            syntax_tree,
            const_env,
            parameter_literals,
            packed_dimensions,
            state,
        )? {
            processes.push(process);
        }
    }
    Ok(())
}

fn ff_process_from_always_construct(
    always: &sv_parser::AlwaysConstruct,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
    packed_dimensions: &PackedDimensions,
    state: &mut procedural::BodyState<'_>,
) -> Result<Option<FfProcess>, AnalyzerError> {
    if always_kind(always) != AlwaysKind::Ff {
        return Ok(None);
    }
    let Some((events, body)) = ff_event_control_and_body(&always.nodes.1, syntax_tree) else {
        return Err(AnalyzerError::Unsupported(
            "always_ff event expression".to_string(),
        ));
    };
    let mut local_dimensions = packed_dimensions.clone();
    local_dimensions.const_env = const_env.clone();
    let mut builder = procedural::BodyBuilder::new(
        syntax_tree,
        &local_dimensions,
        state,
        system_functions::Body::Always,
    );
    let mut body = builder.statement_or_null(body)?;
    for stmt in &mut body {
        procedural::substitute_stmt_constants(stmt, const_env, parameter_literals);
    }
    Ok((!events.is_empty()).then(|| FfProcess::new(events, body)))
}

fn ff_event_control_and_body<'a>(
    stmt: &'a sv_parser::Statement,
    syntax_tree: &SyntaxTree,
) -> Option<(Vec<FfEvent>, &'a sv_parser::StatementOrNull)> {
    let sv_parser::StatementItem::ProceduralTimingControlStatement(timing) = &stmt.nodes.2 else {
        return None;
    };
    let sv_parser::ProceduralTimingControl::EventControl(event_control) = &timing.nodes.0 else {
        return None;
    };
    Some((
        ff_events_from_event_control(event_control, syntax_tree)?,
        &timing.nodes.1,
    ))
}

fn ff_events_from_event_control(
    control: &sv_parser::EventControl,
    syntax_tree: &SyntaxTree,
) -> Option<Vec<FfEvent>> {
    let sv_parser::EventControl::EventExpression(control) = control else {
        return None;
    };
    ff_events_from_event_expression(&control.nodes.1.nodes.1, syntax_tree)
}

fn ff_events_from_event_expression(
    expr: &sv_parser::EventExpression,
    syntax_tree: &SyntaxTree,
) -> Option<Vec<FfEvent>> {
    match expr {
        sv_parser::EventExpression::Expression(expr) => {
            let edge = match expr.nodes.0.as_ref()? {
                sv_parser::EdgeIdentifier::Posedge(_) => FfEdge::Pos,
                sv_parser::EdgeIdentifier::Negedge(_) => FfEdge::Neg,
                sv_parser::EdgeIdentifier::Edge(_) => return None,
            };
            let signal = expr_ident_name(&expr_from_expression(&expr.nodes.1, syntax_tree).ok()?);
            signal.map(|signal| vec![FfEvent::new(edge, signal)])
        }
        sv_parser::EventExpression::Or(expr) => {
            let mut left = ff_events_from_event_expression(&expr.nodes.0, syntax_tree)?;
            left.extend(ff_events_from_event_expression(&expr.nodes.2, syntax_tree)?);
            Some(left)
        }
        sv_parser::EventExpression::Comma(expr) => {
            let mut left = ff_events_from_event_expression(&expr.nodes.0, syntax_tree)?;
            left.extend(ff_events_from_event_expression(&expr.nodes.2, syntax_tree)?);
            Some(left)
        }
        sv_parser::EventExpression::Paren(expr) => {
            ff_events_from_event_expression(&expr.nodes.0.nodes.1, syntax_tree)
        }
        _ => None,
    }
}
