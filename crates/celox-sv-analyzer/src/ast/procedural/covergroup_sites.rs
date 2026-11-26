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
//!
//! An instance that any other `new` constructs, a declaration of a
//! package or the compilation unit, or of a type of another scope,
//! included, is sampled by the process that `N::$spawn` starts (see
//! `covergroup_subroutines::spawn`).

use super::*;
use crate::ast::covergroups::{self, Covergroup};
use celox_sv_syntax::ExprKind;

/// A declaration that samples the instance it constructs, or whose
/// instance reads the variables its `ref` actuals name.
pub(super) struct Site {
    model: Arc<Covergroup>,
    /// The hidden variable of the scope that holds the instance, of a type
    /// with a clocking event.
    handle: Option<String>,
    creation_order: usize,
    tokens: celox_sv_syntax::TokenRange,
    /// Where the statements of the initializer start among those of the
    /// initializers of its item.
    at: usize,
    /// The actuals of the `ref` formals of the type, bound and qualified as
    /// the statements of the initializer are (see [`Site::bind`]).
    references: Vec<Expr>,
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
    /// The sites of the types with `ref` formals, whose `sample()` through
    /// the declared variable passes the actuals of the declaration (see
    /// [`pass_references`]).
    pub references: Vec<ReferenceSite>,
}

/// A declaration that constructs an instance of a covergroup with `ref`
/// formals (IEEE 1800-2023 19.3): `cg c = new(a);`. The instance reads the
/// variables of its actuals at each sample, which `N::$sample` takes as
/// `const ref` formals: the sampler of the declaration passes them, and so
/// does `c.sample()`, which the declared variable alone may call, as
/// nothing but the initializer may write it (see [`check_site_variables`]).
#[derive(Debug, Clone)]
pub(in crate::ast) struct ReferenceSite {
    /// The declared variable, as the bodies of the scope name it.
    variable: String,
    /// The sampling subroutine of the type.
    sample: String,
    /// The subroutine that constructs an instance of the type, which only
    /// the initializer of a declaration calls.
    construct: String,
    /// The hidden variable of the instance, if the type has a clocking
    /// event.
    handle: Option<String>,
    /// The actuals of the `ref` formals.
    actuals: Vec<Expr>,
    /// The names of the `ref` formals and the name of the type, which
    /// messages show.
    formals: Vec<String>,
    display: String,
}

impl Site {
    /// The actuals of the `ref` formals and the declared variable, from
    /// `body`, the statements of the initializers of the item of the site
    /// as they are bound and qualified: the initializer of the site starts
    /// at [`Self::at`]. `None` for a type without `ref` formals.
    pub(super) fn bind(&mut self, body: &[Stmt]) -> Option<ReferenceSite> {
        if !self.model.has_references() {
            return None;
        }
        let Some(Stmt::Assign {
            rhs: Expr::Call { args, .. },
            ..
        }) = body.get(self.at)
        else {
            unreachable!("a site constructs its instance first");
        };
        self.references = self
            .model
            .references()
            .map(|(index, _)| args[index].clone().expect("a site passes every ref actual"))
            .collect();
        let Some(Stmt::Assign { lhs, .. }) = body.get(self.at + usize::from(self.handle.is_some()))
        else {
            unreachable!("a site assigns its variable");
        };
        Some(ReferenceSite {
            variable: lhs.name().to_owned(),
            sample: self.model.sample_subroutine(),
            construct: self.model.new_subroutine(),
            handle: self.handle.clone(),
            actuals: self.references.clone(),
            formals: self
                .model
                .references()
                .map(|(_, formal)| formal.name.clone())
                .collect(),
            display: self.model.display.clone(),
        })
    }

    /// Whether the type samples the instance at a clocking event.
    pub(super) fn clocked(&self) -> bool {
        self.model.event.is_some()
    }
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
/// of that variable to `name`; for a type without a clocking event, whose
/// `new` has `ref` formals, the construction into `name`. `sites` gets the
/// site, whose statements start at `at` among those of the item, and
/// `signals` its variable. `None` for any other initializer, which the
/// ordinary binding of the initializer checks and reports.
#[allow(clippy::too_many_arguments)]
pub(super) fn initializer(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    item_dimensions: &PackedDimensions,
    item: &generate::Item,
    creation_order: usize,
    name: &str,
    native: celox_sv_syntax::ExprId,
    at: usize,
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
    // A site is a declaration of a module or a program of a type of its
    // own: the type of a package or the compilation unit is sampled where
    // that scope binds its event (see `N::$spawn`), and a package has no
    // process to sample with.
    let scope_unit = tree.root_unit().ok().map(|unit| unit.kind);
    if !covergroups::is_class(&class)
        || !session.covergroups.contains_key(&class)
        || scope_unit.is_none_or(|kind| kind == celox_sv_syntax::UnitKind::Package)
    {
        return Ok(None);
    }
    let Ok(model) = covergroups::model(tree, session, item_dimensions, &class, None) else {
        return Ok(None);
    };
    if model.rejection.is_some() || (model.event.is_none() && !model.has_references()) {
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
    let tokens = declarations.body_item(item.id).tokens;
    if model.event.is_none() {
        sites.push(Site {
            model,
            handle: None,
            creation_order,
            tokens,
            at,
            references: Vec::new(),
        });
        return Ok(Some(vec![Stmt::Assign {
            lhs: LValue::Ident(name.to_owned()),
            rhs: construction,
            nonblocking: false,
        }]));
    }
    let handle = format!("%covergroup_site{}", signals.len());
    signals.push((
        handle.clone(),
        classes::handle(&mut session.types, &model.name),
    ));
    sites.push(Site {
        model,
        handle: Some(handle.clone()),
        creation_order,
        tokens,
        at,
        references: Vec::new(),
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
fn edge_procedure(wait: &[Stmt], one_bit: bool, sample: &[Stmt]) -> Option<FfProcess> {
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
            then_body: sample.to_vec(),
            else_body: Vec::new(),
        }],
        None => sample.to_vec(),
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
    let handle = site
        .handle
        .clone()
        .expect("a site of a type with a clocking event has a hidden variable");
    // The variables of the `ref` actuals of the declaration (19.3), which
    // its initializer bound and qualified (see `Site::bind`).
    let args = std::iter::once(Expr::Ident(handle))
        .chain(site.references.iter().cloned())
        .map(Some)
        .collect::<Vec<_>>();
    let call = Stmt::Call {
        name: site.model.sample_subroutine(),
        arg_order: (0..args.len()).collect(),
        args,
    };
    sampler_of(
        tree,
        session,
        state,
        active,
        views,
        const_env,
        parameter_literals,
        &site.model,
        site.creation_order,
        site.tokens,
        |_| vec![call],
    )
}

/// The procedure that runs the statements `sample` makes at the clocking
/// event of `model`, bound and qualified where the covergroup is declared
/// (see [`sampler`]), and that has the site of item `tokens` and
/// `creation_order`.
#[allow(clippy::too_many_arguments)]
fn sampler_of<'a>(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    state: &mut BodyState<'_>,
    active: &'a [generate::Item],
    views: &mut generate::ScopeViews<'a>,
    const_env: &ConstEnv,
    parameter_literals: &HashMap<String, Expr>,
    model: &Covergroup,
    creation_order: usize,
    tokens: celox_sv_syntax::TokenRange,
    sample: impl FnOnce(&mut BodyBuilder<'_, '_, '_>) -> Vec<Stmt>,
) -> Converted<Sampler> {
    let event = model.event.expect("a sampler samples at an event");
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
    let mut sample = sample(&mut builder);
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
    let program = tree
        .root_unit()
        .is_ok_and(|unit| unit.kind == celox_sv_syntax::UnitKind::Program);
    if !program && let Some(mut procedure) = edge_procedure(&body, bit, &sample) {
        procedure.creation_order = creation_order;
        procedure.site = process_site("covergroup sampling", tokens, tree);
        return Ok(Sampler::Edge {
            covergroup: model.name.clone(),
            procedure,
        });
    }
    body.append(&mut sample);
    Ok(Sampler::Process(InitialProcess {
        creation_order,
        site: process_site("covergroup sampling", tokens, tree),
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
        assertion_actions: false,
    }))
}

/// The instances of a covergroup type of a module or a program that `new`s
/// other than the initializers of declarations construct (19.3): the
/// first [`INLINE_INSTANCES`] in hidden handle variables of the scope, any
/// more in a hidden queue, with a hidden count. `N::$spawn` adds an
/// instance, and the sampler of the type walks them at its event, so that
/// no process is started for an instance and the sampler is a procedure the
/// compiled scheduler runs: at one edge of a one-bit variable the flip-flop
/// shaped procedure of the type (see [`SiteParts::add`]), at any other event
/// one process of the type. The handles keep the instances for the rest of
/// the simulation.
#[derive(Debug, Clone)]
pub(in crate::ast) struct InstanceList {
    /// The variables of the first instances, in the order of construction.
    pub slots: Vec<String>,
    /// The number of instances.
    pub count: String,
    /// The queue of the instances after the first [`INLINE_INSTANCES`].
    pub overflow: String,
    pub overflow_layout: system_functions::ContainerLayout,
}

/// How many instances of a type an [`InstanceList`] holds in variables of
/// their own before it uses its queue, whose operations are calls of the
/// heap.
pub(in crate::ast) const INLINE_INSTANCES: usize = 4;

fn count_literal(value: usize) -> Expr {
    Expr::Literal(format!("32'd{value}"))
}

impl InstanceList {
    /// The statements of `N::$spawn` that add instance `handle`.
    pub(in crate::ast) fn add(&self, handle: &Expr) -> Vec<Stmt> {
        let count = || Expr::Ident(self.count.clone());
        let mut add = vec![Stmt::Eval(containers::call(
            system_functions::ContainerOp::PushBack,
            self.overflow_layout.clone(),
            vec![Expr::Ident(self.overflow.clone()), handle.clone()],
        ))];
        for (index, slot) in self.slots.iter().enumerate().rev() {
            add = vec![Stmt::If {
                condition: Expr::Binary {
                    left: Box::new(count()),
                    op: BinaryOp::Eq,
                    right: Box::new(count_literal(index)),
                },
                then_body: vec![Stmt::Assign {
                    lhs: LValue::Ident(slot.clone()),
                    rhs: handle.clone(),
                    nonblocking: false,
                }],
                else_body: add,
            }];
        }
        add.push(Stmt::Assign {
            lhs: LValue::Ident(self.count.clone()),
            rhs: Expr::Binary {
                left: Box::new(count()),
                op: BinaryOp::Add,
                right: Box::new(count_literal(1)),
            },
            nonblocking: false,
        });
        add
    }

    /// Whether the list holds more than `than` instances.
    fn more(&self, than: usize) -> Expr {
        Expr::Binary {
            left: Box::new(Expr::Ident(self.count.clone())),
            op: BinaryOp::Gt,
            right: Box::new(count_literal(than)),
        }
    }

    /// The statements that sample the instances of the variables of the
    /// list with `sample`, the sampling subroutine of the type.
    fn walk_slots(&self, sample: &str) -> Vec<Stmt> {
        self.slots
            .iter()
            .enumerate()
            .map(|(index, slot)| Stmt::If {
                condition: self.more(index),
                then_body: vec![sample_call(sample, Expr::Ident(slot.clone()))],
                else_body: Vec::new(),
            })
            .collect()
    }

    /// The statement that samples the instances of the queue of the list
    /// with `sample`, with `index` the variable of the position in it.
    fn walk_overflow(&self, sample: &str, index: &str) -> Stmt {
        let queue = || Expr::Ident(self.overflow.clone());
        let position = || Expr::Ident(index.to_owned());
        Stmt::If {
            condition: self.more(self.slots.len()),
            then_body: vec![Stmt::Loop {
                kind: LoopKind::For,
                init: vec![Stmt::Local {
                    name: index.to_owned(),
                    init: Some(Expr::Literal("0".into())),
                }],
                condition: Some(Expr::Binary {
                    left: Box::new(position()),
                    op: BinaryOp::Lt,
                    right: Box::new(containers::call(
                        system_functions::ContainerOp::Size,
                        self.overflow_layout.clone(),
                        vec![queue()],
                    )),
                }),
                step: vec![Stmt::Assign {
                    lhs: LValue::Ident(index.to_owned()),
                    rhs: Expr::Binary {
                        left: Box::new(position()),
                        op: BinaryOp::Add,
                        right: Box::new(Expr::Literal("1".into())),
                    },
                    nonblocking: false,
                }],
                body: vec![sample_call(
                    sample,
                    containers::call(
                        system_functions::ContainerOp::Get,
                        self.overflow_layout.clone(),
                        vec![queue(), position()],
                    ),
                )],
            }],
            else_body: Vec::new(),
        }
    }
}

/// `sample()` of the covergroups with `ref` formals in the bodies of a
/// scope, through the variable of declaration `sites` (IEEE 1800-2023 19.3,
/// 19.8): the calls of `N::$sample` that `covergroups::sample_statement`
/// made without the `ref` actuals, which it leaves out, get those of the
/// declaration whose variable is their handle. A call through any other
/// variable, such as a local of a subroutine with the name of the declared
/// one, is rejected. `references` gives the number of `ref` formals of the
/// type each sampling subroutine samples, if it has any.
pub(in crate::ast) fn pass_references<'a>(
    sites: &[ReferenceSite],
    references: &dyn Fn(&str) -> Option<usize>,
    bodies: impl Iterator<Item = &'a mut Vec<Stmt>>,
) -> Converted<()> {
    let mut result = Ok(());
    for body in bodies {
        for stmt in body.iter_mut() {
            stmt.walk_mut(&mut |stmt| {
                let Stmt::Call { name, args, .. } = stmt else {
                    return;
                };
                if result.is_err() || !covergroups::is_sampler(name) {
                    return;
                }
                let Some(count) = references(name).filter(|count| *count != 0) else {
                    return;
                };
                // A sampler of a declaration passes its actuals already.
                if !args.get(1).is_some_and(Option::is_none) {
                    return;
                }
                let site = match args.first() {
                    Some(Some(Expr::Ident(variable))) => sites
                        .iter()
                        .find(|site| site.variable == *variable && site.sample == *name),
                    _ => None,
                };
                let Some(site) = site else {
                    result = Err(covergroups::sampled_elsewhere());
                    return;
                };
                debug_assert_eq!(site.actuals.len(), count);
                for (arg, actual) in args[1..=count].iter_mut().zip(&site.actuals) {
                    *arg = Some(actual.clone());
                }
            });
        }
    }
    result
}

/// The error of a `ref` actual of a declaration of `sites` that names a net
/// of `nets` or selects from one: "Nets and selects into nets shall not be
/// passed by reference" (IEEE 1800-2023 13.5.2).
pub(in crate::ast) fn check_site_actuals(
    sites: &[ReferenceSite],
    nets: &HashSet<&str>,
) -> Converted<()> {
    fn root(actual: &Expr) -> Option<&str> {
        match actual {
            Expr::Ident(name) => Some(name),
            Expr::Select { expr, .. } | Expr::Resize { expr, .. } => root(expr),
            _ => None,
        }
    }
    // An actual the declaration names once: a variable, or an element at
    // constant indices. A sample reads the actual as the declaration bound
    // it, which for a class property or an index that a variable gives
    // would read through the handle or the index at the time of the
    // sample instead.
    fn fixed(actual: &Expr) -> bool {
        // An index the substitution of the parameters and genvars of the
        // declaration left reading no variable and calling nothing, as its
        // range checks do not.
        fn constant(index: &ConstExpr) -> bool {
            match index {
                ConstExpr::Literal(_) => true,
                ConstExpr::Ident(_) | ConstExpr::Function { .. } => false,
                ConstExpr::Select { expr, bit } => constant(expr) && constant(bit),
                ConstExpr::SelectRange { expr, msb, lsb, .. } => {
                    constant(expr) && constant(msb) && constant(lsb)
                }
                ConstExpr::Unary { expr, .. } | ConstExpr::Resize { expr, .. } => constant(expr),
                ConstExpr::Binary { left, right, .. } => constant(left) && constant(right),
                ConstExpr::Mux {
                    condition,
                    then_expr,
                    else_expr,
                } => constant(condition) && constant(then_expr) && constant(else_expr),
            }
        }
        match actual {
            Expr::Ident(_) => true,
            Expr::Select { expr, msb, lsb, .. } => fixed(expr) && constant(msb) && constant(lsb),
            _ => false,
        }
    }
    for site in sites {
        for (actual, formal) in site.actuals.iter().zip(&site.formals) {
            if !fixed(actual) {
                return Err(unsupported(
                    "covergroup ref argument bound to a class property or to an element at an index that is not constant (IEEE 1800-2023 13.5.2, 19.3)",
                ));
            }
            if root(actual).is_some_and(|name| nets.contains(name)) {
                return Err(AnalyzerError::InvalidCovergroup(format!(
                    "the actual of ref argument `{formal}` of covergroup `{}` is a net, which cannot be passed by reference (IEEE 1800-2023 13.5.2, 19.3)",
                    site.display
                )));
            }
        }
    }
    Ok(())
}

/// Rejects a write to the variable of a declaration of `sites` but for its
/// initializer (IEEE 1800-2023 19.3): the samples of the instance through
/// the variable pass the actuals of the declaration (see
/// [`pass_references`]), which another instance it held would read.
#[allow(clippy::too_many_arguments)]
pub(in crate::ast) fn check_site_variables<'a>(
    sites: &[ReferenceSite],
    subroutines: impl Iterator<Item = &'a Subroutine>,
    comb: &[CombProcess],
    ff: &[FfProcess],
    processes: &[InitialProcess],
    directions: &lifetimes::Directions,
) -> Converted<()> {
    if sites.is_empty() {
        return Ok(());
    }
    let variables = sites
        .iter()
        .map(|site| site.variable.as_str())
        .collect::<HashSet<_>>();
    // The initializer of a declaration assigns the instance from the hidden
    // variable of the site, or from the construction itself, which no
    // other `new` of such a type is.
    let initializer = |stmt: &Stmt| {
        let Stmt::Assign { lhs, rhs, .. } = stmt else {
            return false;
        };
        sites.iter().any(|site| {
            site.variable == lhs.name()
                && match rhs {
                    Expr::Ident(handle) => site.handle.as_deref() == Some(handle.as_str()),
                    Expr::Call {
                        callee: Callee::Subroutine(name),
                        ..
                    } => *name == site.construct,
                    _ => false,
                }
        })
    };
    match super::references::written_variable(
        &variables,
        &initializer,
        subroutines,
        comb,
        ff,
        processes,
        directions,
    ) {
        Some(variable) => Err(unsupported(format!(
            "covergroup ref argument of an instance whose variable `{variable}` is written outside its declaration (IEEE 1800-2023 19.3)"
        ))),
        None => Ok(()),
    }
}

/// The call of sampling subroutine `sample` with `handle`.
fn sample_call(sample: &str, handle: Expr) -> Stmt {
    Stmt::Call {
        name: sample.to_owned(),
        args: vec![Some(handle)],
        arg_order: vec![0],
    }
}

/// The instance lists and their samplers of the covergroup types of the
/// scope that a `new` other than the initializer of a declaration
/// constructs (see [`InstanceList`]), which `session` has recorded while
/// the bodies of the scope were bound: the lists go to `session` for the
/// `N::$spawn` the scope makes later, their variables to `parts`, and
/// each sampler to `parts` (at an edge, merged with the procedure of the
/// declarations of the type, after them) or to `processes`.
#[allow(clippy::too_many_arguments)]
pub(super) fn constructed<'a>(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    state: &mut BodyState<'_>,
    active: &'a [generate::Item],
    views: &mut generate::ScopeViews<'a>,
    const_env: &ConstEnv,
    parameter_literals: &HashMap<String, Expr>,
    dimensions: &PackedDimensions,
    parts: &mut SiteParts,
    processes: &mut Vec<InitialProcess>,
) -> Converted<()> {
    let own = tree.root_unit().is_ok_and(|unit| {
        matches!(
            unit.kind,
            celox_sv_syntax::UnitKind::Module | celox_sv_syntax::UnitKind::Program
        )
    });
    if !own {
        return Ok(());
    }
    for name in session.covergroup_spawn_requests.clone() {
        if session.covergroup_instance_lists.contains_key(&name) {
            continue;
        }
        let Some(model) = session.covergroups.get(&name).cloned() else {
            continue;
        };
        if model.rejection.is_some() || model.event.is_none() || model.owner.is_some() {
            continue;
        }
        let number = session.covergroup_instance_lists.len();
        let handle = classes::handle(&mut session.types, &model.name);
        let slots = (0..INLINE_INSTANCES)
            .map(|slot| format!("%covergroup_instance{number}_{slot}"))
            .collect::<Vec<_>>();
        let count = format!("%covergroup_instance_count{number}");
        let overflow = format!("%covergroup_instances{number}");
        let queue = containers::queue_of(handle.clone(), tree, session, dimensions)?;
        let overflow_layout = queue
            .container()
            .expect("a queue is a container")
            .layout
            .clone();
        for slot in &slots {
            parts.signals.push((slot.clone(), handle.clone()));
        }
        parts
            .signals
            .push((count.clone(), classes::unsigned_type(32)));
        parts.signals.push((overflow.clone(), queue));
        let list = InstanceList {
            slots,
            count,
            overflow,
            overflow_layout,
        };
        let declaration = tree.declarations().covergroup(model.declaration);
        let creation_order = active
            .iter()
            .position(|item| {
                matches!(tree.declarations().body_item(item.id).kind,
                    celox_sv_syntax::BodyItemKind::Covergroup(id) if id == model.declaration)
            })
            .unwrap_or(0);
        let sample = model.sample_subroutine();
        let mut overflow = None;
        let sampler = sampler_of(
            tree,
            session,
            state,
            active,
            views,
            const_env,
            parameter_literals,
            &model,
            creation_order,
            declaration.tokens,
            |builder| {
                let index = builder.declare("$covergroup_instance", int_type());
                let mut walk = list.walk_slots(&sample);
                overflow = Some(list.walk_overflow(&sample, &index));
                walk.extend(overflow.clone());
                walk
            },
        )?;
        match sampler {
            // The flip-flop shaped procedure walks the queue too: it runs
            // the loop only when the queue holds instances, at little cost
            // otherwise.
            Sampler::Edge {
                covergroup,
                procedure,
            } => parts.add(covergroup, procedure),
            // A process pays for a loop at each run, used or not: the queue
            // has a process of its own, which waits until it holds an
            // instance.
            Sampler::Process(mut process) => {
                let overflow = overflow.expect("the walk was made");
                let slots = list.walk_slots(&sample);
                let mut queued = process.clone();
                let Some(Stmt::Loop { body, .. }) = process.body.first_mut() else {
                    unreachable!("a sampler process is a loop");
                };
                body.retain(|stmt| *stmt != overflow);
                let Some(Stmt::Loop { body, .. }) = queued.body.first_mut() else {
                    unreachable!("a sampler process is a loop");
                };
                body.retain(|stmt| !slots.contains(stmt));
                queued
                    .body
                    .insert(0, Stmt::Wait(list.more(list.slots.len())));
                processes.push(process);
                processes.push(queued);
            }
        }
        session.covergroup_instance_lists.insert(name, list);
    }
    Ok(())
}
