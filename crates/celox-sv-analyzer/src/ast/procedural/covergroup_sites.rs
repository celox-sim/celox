//! The declarations of a module or a program that construct an instance of
//! a covergroup with a clocking event, `cg c = new;` (IEEE 1800-2023 19.3):
//! the instance is sampled at the event of its type, as if the process that
//! triggers the event called `sample()`.
//!
//! The initializer of such a declaration keeps the handle it constructs in
//! a hidden variable of the scope as well, `%covergroup_site{n}`, which no
//! statement of a source names, and one process per declaration waits for
//! the event and samples the instance that variable holds: assigning the
//! declared variable later neither stops the sampling nor adds to it.
//!
//! An event that is one edge of a one-bit variable (`@(posedge clk)`,
//! `@(negedge clk iff en)`) gets a flip-flop shaped procedure, `always
//! @(posedge clk) N::$sample(handle);`, one for all the sites of the type,
//! which runs with the flip-flops of the edge, before the nonblocking
//! assignments they make, also when the edge itself is a nonblocking
//! assignment of another flip-flop (a divided clock). Any other
//! event, and every event of a program, whose processes run in the Reactive
//! region set (24.3.1), gets an `initial` whose body is `forever begin
//! @(event); N::$sample(handle); end`, with the wait at the top of its loop
//! so that it can be a compiled kernel. Neither runs before the first event
//! or draws a seed of its own.

use super::*;
use crate::ast::covergroups::{self, Covergroup};
use celox_sv_syntax::ExprKind;

/// A declaration that samples the instance it constructs.
pub(super) struct Site {
    model: Arc<Covergroup>,
    /// The hidden variable of the scope that holds the instance.
    handle: String,
    creation_order: usize,
    tokens: celox_sv_syntax::TokenRange,
}

/// The hidden variables of the sites of a scope, which the scope declares
/// as signals of the handle types of their covergroups.
pub(in crate::ast) type SiteSignals = Vec<(String, Type)>;

/// What the sites of a scope add to it: the hidden variables, and the
/// flip-flop shaped procedures that sample at edges.
#[derive(Default)]
pub(in crate::ast) struct SiteParts {
    pub signals: SiteSignals,
    pub edge_procedures: Vec<FfProcess>,
    /// The covergroup type each of `edge_procedures` samples.
    covergroups: Vec<String>,
}

impl SiteParts {
    /// Adds edge procedure `procedure` of a site of covergroup type
    /// `covergroup`. The sites of one type share its event, and one
    /// procedure samples them all, in the order of their declarations: two
    /// edge procedures that call the sampling subroutine of one type would
    /// run as processes, which see the nonblocking assignments of their
    /// edge.
    pub(super) fn add(&mut self, covergroup: String, procedure: FfProcess) {
        match self
            .covergroups
            .iter()
            .position(|other| *other == covergroup)
        {
            Some(index) => self.edge_procedures[index].body.extend(procedure.body),
            None => {
                self.covergroups.push(covergroup);
                self.edge_procedures.push(procedure);
            }
        }
    }
}

/// The statements of the initializer `native` of variable `name`, which
/// `item` declares at `creation_order`, when the declaration is a site: the
/// construction of the instance into a hidden variable, and the assignment
/// of that variable to `name`. `sites` gets the site and `signals` its
/// variable. `None` for any other initializer, which the ordinary binding
/// of the initializer checks and reports.
#[allow(clippy::too_many_arguments)]
pub(super) fn initializer(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    item_dimensions: &PackedDimensions,
    item: &generate::Item,
    creation_order: usize,
    name: &str,
    native: celox_sv_syntax::ExprId,
    sites: &mut Vec<Site>,
    signals: &mut SiteSignals,
) -> Converted<Option<Vec<Stmt>>> {
    let declarations = tree.declarations();
    let arguments: &[celox_sv_syntax::Argument] = match &declarations.expression(native).kind {
        ExprKind::Name(new) if new.spelling == "new" => &[],
        ExprKind::Call {
            expression,
            arguments,
        } if matches!(&declarations.expression(*expression).kind,
            ExprKind::Name(new) if new.spelling == "new") =>
        {
            arguments
        }
        _ => return Ok(None),
    };
    let Some(shape) = item_dimensions.get(name) else {
        return Ok(None);
    };
    if !shape.unpacked.is_empty() || shape.container().is_some() {
        return Ok(None);
    }
    let Some(class) =
        expression_binding::objects::shape_class(shape, tree, session, item_dimensions)
    else {
        return Ok(None);
    };
    if !covergroups::is_class(&class) {
        return Ok(None);
    }
    let Ok(model) = covergroups::model(tree, session, item_dimensions, &class, None) else {
        return Ok(None);
    };
    if model.rejection.is_some() || model.event.is_none() {
        return Ok(None);
    }
    let construction = covergroups::construct_at_site(
        tree,
        session,
        arguments,
        declarations,
        item_dimensions,
        &class,
    )?;
    let handle = format!("%covergroup_site{}", signals.len());
    signals.push((
        handle.clone(),
        classes::handle(&mut session.types, &model.name),
    ));
    sites.push(Site {
        model,
        handle: handle.clone(),
        creation_order,
        tokens: declarations.body_item(item.id).tokens,
    });
    Ok(Some(vec![
        Stmt::Assign {
            lhs: LValue::Ident(handle.clone()),
            rhs: construction,
            nonblocking: false,
        },
        Stmt::Assign {
            lhs: LValue::Ident(name.to_owned()),
            rhs: Expr::Ident(handle),
            nonblocking: false,
        },
    ]))
}

/// What samples a site at its event: a flip-flop shaped `always`, which
/// the scope merges with the one of every other site of the type (see
/// [`SiteParts::add`]), or a process.
pub(super) enum Sampler {
    Edge {
        covergroup: String,
        procedure: FfProcess,
    },
    Process(InitialProcess),
}

/// Whether variable `name` of `dimensions` is one bit of an integral type,
/// whose edges a flip-flop shaped procedure can wait for.
fn one_bit(
    name: &str,
    session: &Session,
    dimensions: &PackedDimensions,
    functions: &crate::ast::const_functions::ConstantFunctions,
) -> bool {
    let Some(shape) = dimensions.get(name) else {
        return false;
    };
    let table = &session.types;
    !(shape.string
        || shape.real
        || shape.chandle
        || shape.class_in(table).is_some()
        || shape.container().is_some()
        || !shape.unpacked.is_empty()
        || shape.is_unpacked_struct_in(table))
        && lvalue_expr_type(functions, &LValue::Ident(name.to_owned()), dimensions)
            .is_some_and(|ty| ty.width == 1)
}

/// The flip-flop shaped procedure that runs `call` at event `wait`, bound
/// and qualified, when the event is one `posedge` or `negedge` of a one-bit
/// variable (`one_bit` says whether it is), with or without an `iff`, which
/// the procedure checks when it runs at the edge, as the guard is evaluated
/// at the event (9.4.2.3). An event of several terms, or `edge`, is waited
/// for by a process: the frontend runs two edge procedures that call the
/// sampling subroutine of one type as processes, which see the nonblocking
/// assignments of their edge.
fn edge_procedure(wait: &[Stmt], one_bit: bool, call: &Stmt) -> Option<FfProcess> {
    let [Stmt::WaitEvent(items)] = wait else {
        return None;
    };
    let [item] = items.as_slice() else {
        return None;
    };
    let Expr::Ident(signal) = &item.expr else {
        return None;
    };
    let edge = match item.edge {
        EventEdge::Pos => FfEdge::Pos,
        EventEdge::Neg => FfEdge::Neg,
        EventEdge::Both | EventEdge::Any => return None,
    };
    if !one_bit {
        return None;
    }
    let body = match &item.iff {
        Some(condition) => vec![Stmt::If {
            condition: condition.clone(),
            then_body: vec![call.clone()],
            else_body: Vec::new(),
        }],
        None => vec![call.clone()],
    };
    let mut process = FfProcess::new(vec![FfEvent::new(edge, signal.clone())], body);
    // As `always @(posedge clk) c.sample();` is.
    process.suspendable = true;
    // A procedure of the analysis, which draws no seed (18.14.1): the
    // random stimulus of the design stays that of the design without the
    // clocking event.
    process.synthetic = true;
    Some(process)
}

/// The procedure that samples the instance of `site` at the clocking event of
/// its covergroup. The event is bound where the covergroup is declared, at
/// the item of `active` that declares it, as its coverpoints are: the names
/// it reads are those of that scope, whatever the generate block of the
/// declaration of the instance declares.
#[allow(clippy::too_many_arguments)]
pub(super) fn sampler<'a>(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    state: &mut BodyState<'_>,
    active: &'a [generate::Item],
    views: &mut generate::ScopeViews<'a>,
    const_env: &ConstEnv,
    parameter_literals: &HashMap<String, Expr>,
    site: &Site,
) -> Converted<Sampler> {
    let model = &site.model;
    let event = model.event.expect("a site samples at an event");
    let Some(declaring) = active.iter().find(|item| {
        matches!(tree.declarations().body_item(item.id).kind,
            celox_sv_syntax::BodyItemKind::Covergroup(id) if id == model.declaration
                && covergroups::scope_names(tree, session, id, Some(item.scope_name())).class
                    == model.name)
    }) else {
        return Err(unsupported(
            "covergroup sampling event of an instance declared outside the scope of its covergroup (IEEE 1800-2023 19.3)",
        ));
    };
    let scoped = &declaring.scoped(tree);
    let scoped = &declaring.enter(scoped);
    let (dimensions, literals) = views.get(declaring);
    let mut builder = BodyBuilder::new(
        scoped,
        session,
        dimensions,
        state,
        system_functions::Body::Initial,
    );
    // What the analysis does not support of the event is rejected as a
    // covergroup construct, which the tracking issue of covergroups keys on.
    let mut body = builder
        .timing_control_statements(event)
        .map_err(|error| match error {
            AnalyzerError::Unsupported(text) if !text.starts_with("covergroup ") => {
                unsupported(format!("covergroup sampling event ({text})"))
            }
            error => error,
        })?;
    drop(builder);
    let bit = matches!(body.as_slice(), [Stmt::WaitEvent(items)]
        if matches!(items.as_slice(), [item]
            if matches!(&item.expr, Expr::Ident(name)
                if one_bit(name, session, dimensions, &scoped.functions))));
    for stmt in &mut body {
        substitute_stmt_constants(scoped, session, stmt, &declaring.env, literals);
        qualify_stmt(declaring, stmt);
        substitute_stmt_constants(scoped, session, stmt, const_env, parameter_literals);
    }
    let call = Stmt::Call {
        name: model.sample_subroutine(),
        args: vec![Some(Expr::Ident(site.handle.clone()))],
        arg_order: vec![0],
    };
    let program = tree
        .root_unit()
        .is_ok_and(|unit| unit.kind == celox_sv_syntax::UnitKind::Program);
    if !program && let Some(mut procedure) = edge_procedure(&body, bit, &call) {
        procedure.creation_order = site.creation_order;
        procedure.site = process_site("covergroup sampling", site.tokens, tree);
        return Ok(Sampler::Edge {
            covergroup: model.name.clone(),
            procedure,
        });
    }
    body.push(call);
    Ok(Sampler::Process(InitialProcess {
        creation_order: site.creation_order,
        site: process_site("covergroup sampling", site.tokens, tree),
        condition: None,
        body: vec![Stmt::Loop {
            kind: LoopKind::Forever,
            init: Vec::new(),
            condition: None,
            step: Vec::new(),
            body,
        }],
        kind: celox_design::ProcessKind::Regular,
        validation_body: Vec::new(),
        // A process of the analysis, which draws no seed (18.14.1): the
        // random stimulus of the design stays that of the design without
        // the covergroup.
        synthetic: true,
        // An `initial forever`, not an `always`: a level event list would
        // otherwise make it a level-sensitive procedure, which two-state
        // simulation runs once at time zero (see `InitialProcess::repeats`).
        repeats: false,
    }))
}
