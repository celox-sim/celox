//! The `initial` and `final` processes of a scope and the initializers of its statics.
use super::*;

/// Every `initial` process of the active generate items.
/// The value of the initializer `native` of static property `target` of
/// class `owner`, bound in the scope of the class as its methods are (IEEE
/// 1800-2023 8.9, 8.25): its properties and methods hide the names of the
/// scope around it, a static property names the variable that holds it, and
/// a call of a static method calls that method. `dimensions` is the scope of
/// the class's bodies.
#[allow(clippy::too_many_arguments)]
pub(super) fn static_initializer(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    state: &mut BodyState<'_>,
    item: &generate::Item,
    owner: &str,
    type_aliases: &TypeAliases,
    dimensions: &PackedDimensions,
    native: celox_sv_syntax::ExprId,
    target: &LValue,
) -> Result<Expr, AnalyzerError> {
    let (env, aliases) = classes::specializations::scope(session, owner, &item.env, type_aliases);
    let class = classes::class(tree, session, owner, &env, &aliases)?;
    let mut builder = BodyBuilder::new(
        tree,
        session,
        dimensions,
        state,
        system_functions::Body::Initial,
    );
    builder.push_scope();
    builder.enter_class_scope(&class);
    let mut value = expression_binding::expression(
        native,
        tree.declarations(),
        target,
        tree,
        builder.session,
        &builder.dims,
    )?;
    builder.rename_expr(&mut value);
    builder.pop_scope();
    Ok(value)
}

pub(in crate::ast) fn initial_processes_from_scope(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    const_env: &ConstEnv,
    packed_dimensions: &PackedDimensions,
    parameter_literals: &HashMap<String, Expr>,
    state: &mut BodyState<'_>,
) -> Result<Vec<InitialProcess>, AnalyzerError> {
    let type_aliases = packed_dimensions.type_aliases.clone();
    // Variable declaration initializers run before every `initial` and
    // `always` procedure (IEEE 1800-2023 10.5).
    let mut initializers = Vec::new();
    let mut processes = Vec::new();
    let active = generate::items(tree, session, const_env, &type_aliases)?;
    let mut views = generate::ScopeViews::with_literals(packed_dimensions, parameter_literals);
    for (creation_order, item) in active.iter().enumerate() {
        let tree = &item.scoped(tree);
        if item.is_parameter_declaration(tree.declarations()) {
            continue;
        }
        let tree = &item.enter(tree);
        let mut assignments = Vec::new();
        if let celox_sv_syntax::BodyItemKind::Data(id) = tree.declarations().body_item(item.id).kind
        {
            let declaration = match tree.declarations().data_declaration(id) {
                celox_sv_syntax::DataDeclaration::Variables(value)
                | celox_sv_syntax::DataDeclaration::Constructed(value) => value,
                celox_sv_syntax::DataDeclaration::Unconverted(_) => {
                    return Err(unsupported("opaque declaration initializer"));
                }
            };
            for variable in declaration.variables.iter().filter(|variable| {
                type_binding::ordinary_variable(
                    variable,
                    declaration.data_type,
                    tree.declarations(),
                )
            }) {
                let name = instances::normalize_identifier(&variable.name.spelling);
                if let Some(value) = variable.value {
                    assignments.push((name, Some(value), None));
                } else {
                    assignments.push((name, None, None));
                }
            }
        }
        // Static class properties are initialized like variables, in the
        // scope of the class that declares them (IEEE 1800-2023 8.9, 8.25).
        for property in classes::item_statics(item.id, tree, session, &item.env, &type_aliases) {
            if let Some(value) = property.initializer {
                assignments.push((property.variable, Some(value), Some(property.owner)));
            } else if events::is_event(&property.r#type) {
                assignments.push((property.variable, None, Some(property.owner)));
            }
        }
        if !assignments.is_empty() {
            let (item_dimensions, literals) = views.get(item);
            let mut body = Vec::new();
            for (name, native, owner) in assignments {
                let Some(native) = native else {
                    if let Some(aggregate) = item_dimensions
                        .get(&name)
                        .and_then(|shape| shape.unpacked_struct.as_ref())
                    {
                        body.extend(aggregate.leaves().into_iter().filter_map(|leaf| {
                            let target = leaf.name(&name);
                            leaf.default.map(|rhs| Stmt::Assign {
                                lhs: LValue::Ident(target),
                                rhs,
                                nonblocking: false,
                            })
                        }));
                        continue;
                    }
                    // Each declaration of an event creates a synchronization
                    // object (IEEE 1800-2023 15.5).
                    if owner.is_none() {
                        let Some(shape) = item_dimensions
                            .get(&name)
                            .filter(|shape| shape.class.as_deref() == Some(events::CLASS))
                        else {
                            continue;
                        };
                        if !shape.unpacked.is_empty() || shape.container.is_some() {
                            return Err(unsupported("array of named events"));
                        }
                    }
                    body.push(Stmt::Assign {
                        lhs: LValue::Ident(name),
                        rhs: events::new_event(),
                        nonblocking: false,
                    });
                    continue;
                };
                let scoped = owner.as_ref().map(|owner| {
                    classes::specializations::body_scope(session, owner, item_dimensions, literals)
                });
                let (item_dimensions, literals) = match &scoped {
                    Some((dimensions, literals)) => (dimensions, literals),
                    None => (item_dimensions, &**literals),
                };
                if let Some(target) = selected_unpacked_shape(&name, 0, item_dimensions) {
                    let id = native;
                    array_compatibility::check_native_unpacked_assignment(
                        tree,
                        session,
                        id,
                        &target,
                        || format!("initializer of `{name}`"),
                        item_dimensions,
                    )?;
                }
                expression_binding::check_chandle_assignment(
                    item_dimensions
                        .get(&name)
                        .is_some_and(|shape| shape.chandle && shape.unpacked.is_empty()),
                    native,
                    tree,
                    tree.declarations(),
                    item_dimensions,
                    session,
                )?;
                let lhs = LValue::Ident(name);
                let rhs = match &owner {
                    Some(owner) => static_initializer(
                        tree,
                        session,
                        state,
                        item,
                        owner,
                        &type_aliases,
                        item_dimensions,
                        native,
                        &lhs,
                    )?,
                    None => expression_binding::expression(
                        native,
                        tree.declarations(),
                        &lhs,
                        tree,
                        session,
                        item_dimensions,
                    )?,
                };
                let LValue::Ident(name) = lhs else {
                    unreachable!("an initializer target is a variable")
                };
                // A variable of unpacked structure type initializes its leaves.
                let mut stmt = match unpacked_structs::targets(&name, item_dimensions) {
                    Some(parts) => {
                        unpacked_structs::check_assignment(
                            &name,
                            native,
                            tree,
                            session,
                            item_dimensions,
                        )?;
                        Stmt::AssignConcat {
                            parts,
                            rhs,
                            nonblocking: false,
                        }
                    }
                    None => Stmt::Assign {
                        lhs: LValue::Ident(name),
                        rhs,
                        nonblocking: false,
                    },
                };
                // The parameters of the class are its own constants.
                if scoped.is_some() {
                    substitute_stmt_constants(
                        tree,
                        session,
                        &mut stmt,
                        &item_dimensions.const_env,
                        literals,
                    );
                }
                body.push(stmt);
            }
            let (_, literals) = views.get(item);
            for stmt in &mut body {
                substitute_stmt_constants(tree, session, stmt, &item.env, literals);
                qualify_stmt(item, stmt);
                substitute_stmt_constants(tree, session, stmt, const_env, parameter_literals);
            }
            if !body.is_empty() {
                initializers.push(InitialProcess {
                    creation_order,
                    site: process_site(
                        "declaration initializer",
                        tree.declarations().body_item(item.id).tokens,
                        tree,
                    ),
                    condition: None,
                    body,
                    kind: celox_design::ProcessKind::Initializer,
                    validation_body: Vec::new(),
                    synthetic: false,
                    repeats: false,
                });
            }
            continue;
        }
        let Some(procedure) = item.procedure(tree.declarations())? else {
            continue;
        };
        let construct = match procedure.kind {
            celox_sv_syntax::ProcedureKind::Initial => "initial",
            celox_sv_syntax::ProcedureKind::Final => "final",
            // An `always_ff` that runs as a process keeps its restrictions
            // (IEEE 1800-2023 9.2.2.4): see `timed_tasks::classify`, and
            // the checks of its drivers.
            celox_sv_syntax::ProcedureKind::AlwaysFf => "always_ff",
            _ => "always",
        };
        let repeats = match procedure.kind {
            celox_sv_syntax::ProcedureKind::Initial | celox_sv_syntax::ProcedureKind::Final => {
                false
            }
            celox_sv_syntax::ProcedureKind::AlwaysFf
                if validation::always_kind(procedure, tree.declarations())?
                    == validation::AlwaysKind::Process
                    || ff_process::always_falls_back_to_process(
                        procedure,
                        tree,
                        session,
                        views.get(item).0,
                    )? =>
            {
                true
            }
            celox_sv_syntax::ProcedureKind::Always
                if validation::always_kind(procedure, tree.declarations())?
                    == validation::AlwaysKind::Process
                    || ff_process::always_falls_back_to_process(
                        procedure,
                        tree,
                        session,
                        views.get(item).0,
                    )? =>
            {
                true
            }
            _ => continue,
        };
        let (item_dimensions, literals) = views.get(item);
        let is_final = procedure.kind == celox_sv_syntax::ProcedureKind::Final;
        let mut builder = BodyBuilder::new(
            tree,
            session,
            item_dimensions,
            state,
            if is_final {
                system_functions::Body::Final
            } else if repeats {
                system_functions::Body::Process
            } else {
                system_functions::Body::Initial
            },
        );
        let body = builder.native_body(&[procedure.body], &procedure.authored_statements)?;
        let mut validation_body = if is_final {
            std::mem::take(&mut builder.sensitivity_body)
        } else {
            Vec::new()
        };
        let mut body = if repeats {
            vec![Stmt::Loop {
                kind: LoopKind::Forever,
                init: Vec::new(),
                condition: None,
                step: Vec::new(),
                body,
            }]
        } else {
            body
        };
        for stmt in body.iter_mut().chain(&mut validation_body) {
            substitute_stmt_constants(tree, builder.session, stmt, &item.env, literals);
            qualify_stmt(item, stmt);
            substitute_stmt_constants(tree, builder.session, stmt, const_env, parameter_literals);
        }
        processes.push(InitialProcess {
            creation_order,
            site: process_site(
                construct,
                tree.declarations().body_item(item.id).tokens,
                tree,
            ),
            condition: None,
            body,
            kind: if is_final {
                celox_design::ProcessKind::Final
            } else {
                celox_design::ProcessKind::Regular
            },
            validation_body,
            synthetic: false,
            repeats,
        });
    }
    initializers.extend(processes);
    Ok(initializers)
}

/// The names of the variables written by assignment statements.
pub(in crate::ast) fn written_names<'s>(
    stmts: impl IntoIterator<Item = &'s Stmt>,
) -> HashSet<String> {
    let mut names = HashSet::default();
    for stmt in stmts {
        stmt.walk(&mut |stmt| match stmt {
            Stmt::Assign { lhs, .. } => {
                names.insert(lhs.name().to_string());
            }
            Stmt::AssignConcat { parts, .. } => {
                names.extend(parts.iter().map(|part| part.name().to_string()));
            }
            _ => {}
        });
    }
    names
}

/// Where the first token of a native token range was invoked.
pub(in crate::ast) fn process_site(
    construct: &'static str,
    tokens: celox_sv_syntax::TokenRange,
    tree: &SyntaxTree,
) -> Option<crate::procedural::ProcessSite> {
    let native = tree.declarations();
    let token = native
        .tokens
        .get(tokens.start..tokens.end)?
        .iter()
        .find(|token| !token.is_trivia())?;
    let span = native.sources.invocation(token.origin);
    let file = native.sources.file(span.file);
    let (line, column) = file.line_column(span.start);
    Some(crate::procedural::ProcessSite {
        construct,
        path: file.path.clone(),
        line,
        column,
    })
}
