//! Sequential process collection and event-control parsing.

use super::*;

pub(super) fn ff_processes_from_scope(
    syntax_tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
    packed_dimensions: &PackedDimensions,
    state: &mut procedural::BodyState<'_>,
) -> Result<Vec<FfProcess>, AnalyzerError> {
    let mut processes = Vec::new();
    let active = generate::items(syntax_tree, const_env, &packed_dimensions.type_aliases)?;
    let mut views = generate::ScopeViews::with_literals(packed_dimensions, parameter_literals);
    for (creation_order, item) in active.iter().enumerate() {
        let _interface_scope = item.interface_scope();
        let Some(procedure) = item.procedure(syntax_tree.declarations())? else {
            continue;
        };
        let _constant_functions = item.enter();
        let start = processes.len();
        let (dimensions, literals) = views.get(item);
        if let Some(process) = ff_process_from_procedure(
            procedure,
            syntax_tree,
            &item.env,
            literals,
            dimensions,
            state,
        )? {
            processes.push(process);
        }
        for process in &mut processes[start..] {
            process.creation_order = creation_order;
            for event in &mut process.events {
                event.signal = item.name(&event.signal);
            }
            for stmt in &mut process.body {
                procedural::qualify_stmt(item, stmt);
            }
        }
    }
    Ok(processes)
}

fn ff_process_from_procedure(
    procedure: &celox_sv_syntax::Procedure,
    syntax_tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
    parameter_literals: &HashMap<String, Expr>,
    packed_dimensions: &PackedDimensions,
    state: &mut procedural::BodyState<'_>,
) -> Result<Option<FfProcess>, AnalyzerError> {
    if procedure.kind == celox_sv_syntax::ProcedureKind::Initial {
        return Ok(None);
    }
    if always_kind(procedure, syntax_tree.declarations())? != AlwaysKind::Ff {
        return Ok(None);
    }
    let root = procedure.body;
    let (events, body) = ff_event_control_and_body(root, syntax_tree)?;
    let mut local_dimensions = packed_dimensions.clone();
    local_dimensions.const_env = const_env.clone().into();
    let mut builder = procedural::BodyBuilder::new(
        syntax_tree,
        &local_dimensions,
        state,
        system_functions::Body::Always,
    );
    let mut body = builder.native_body(&[body], &procedure.authored_statements)?;
    for stmt in &mut body {
        procedural::substitute_stmt_constants(
            &syntax_tree.state,
            stmt,
            const_env,
            parameter_literals,
        );
    }
    Ok((!events.is_empty()).then(|| {
        let mut process = FfProcess::new(events, body);
        process.always_ff = procedure.kind == celox_sv_syntax::ProcedureKind::AlwaysFf;
        process.suspendable = procedure.kind == celox_sv_syntax::ProcedureKind::Always;
        process
    }))
}

fn ff_event_control_and_body(
    root: celox_sv_syntax::StatementId,
    tree: &AnalysisContext<'_>,
) -> Result<(Vec<FfEvent>, celox_sv_syntax::StatementId), AnalyzerError> {
    use celox_sv_syntax::{EventControl, StatementKind, TimingControlKind, TimingEdge};
    let unsupported_event = || unsupported("always_ff event expression");
    let native = tree.declarations();
    let StatementKind::Timing { control, body } = native.statement(root).kind else {
        return Err(timing::disagreement());
    };
    let TimingControlKind::Event(EventControl::Items(items)) = &native.timing_control(control).kind
    else {
        return Err(timing::disagreement());
    };
    let mut events = Vec::with_capacity(items.len());
    for item in items {
        if item.iff.is_some() {
            return Err(unsupported("iff-qualified always_ff event"));
        }
        let edge = match item.edge {
            TimingEdge::Pos => FfEdge::Pos,
            TimingEdge::Neg => FfEdge::Neg,
            _ => return Err(unsupported_event()),
        };
        let expression =
            expression_binding::value(item.expression, native, tree, &PackedDimensions::default())?;
        let signal = expr_ident_name(&expression).ok_or_else(unsupported_event)?;
        events.push(FfEvent::new(edge, signal));
    }
    Ok((events, body))
}
