//! Sequential process collection and event-control parsing.

use super::*;

pub(super) fn ff_processes_from_module_node(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
    packed_dimensions: &PackedDimensions,
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
        )?;
        for process in &mut processes[start..] {
            for event in &mut process.events {
                event.signal = item.name(&event.signal);
            }
            for assignment in &mut process.assignments {
                if let Some(condition) = &mut assignment.condition {
                    item.expr(condition);
                }
                item.assignment(&mut assignment.assignment);
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
) -> Result<(), AnalyzerError> {
    if let sv_parser::ModuleOrGenerateItem::ModuleItem(item) = item {
        ff_processes_from_module_common_item(
            &item.nodes.1,
            syntax_tree,
            const_env,
            parameter_literals,
            packed_dimensions,
            processes,
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
) -> Result<(), AnalyzerError> {
    if let sv_parser::ModuleCommonItem::AlwaysConstruct(always) = item {
        if let Some(process) = ff_process_from_always_construct(
            always,
            syntax_tree,
            const_env,
            parameter_literals,
            packed_dimensions,
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
) -> Result<Option<FfProcess>, AnalyzerError> {
    if !matches!(always.nodes.0, sv_parser::AlwaysKeyword::AlwaysFf(_)) {
        return Ok(None);
    }
    let Some((events, body)) = ff_event_control_and_body(&always.nodes.1, syntax_tree) else {
        return Err(AnalyzerError::Unsupported(
            "always_ff event expression".to_string(),
        ));
    };
    let mut assignments = Vec::new();
    conditional_assignments_from_statement_or_null(
        body,
        None,
        false,
        false,
        syntax_tree,
        const_env,
        packed_dimensions,
        &mut assignments,
    )?;
    let assignments = assignments
        .into_iter()
        .map(|assignment| {
            ConditionalAssignment::new(
                assignment.condition.map(|condition| {
                    substitute_expr_constants_with_parameter_literals(
                        condition,
                        const_env,
                        parameter_literals,
                    )
                }),
                substitute_assignment_constants_with_parameter_literals(
                    assignment.assignment,
                    const_env,
                    parameter_literals,
                ),
            )
        })
        .collect::<Vec<_>>();
    Ok(
        (!events.is_empty() && !assignments.is_empty())
            .then(|| FfProcess::new(events, assignments)),
    )
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
            let signal = expr_ident_name(&expr_from_expression(&expr.nodes.1, syntax_tree)?);
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
