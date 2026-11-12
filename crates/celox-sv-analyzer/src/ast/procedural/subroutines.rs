//! The functions and tasks of a scope: their arguments and the lowering of their bodies.
use super::*;

/// Run `epilogue` at each `return` in `body`. A returned value is first
/// stored in the result variable `result`, so the epilogue sees the effects
/// of evaluating it.
pub(super) fn before_returns(body: &mut Vec<Stmt>, epilogue: &[Stmt], result: Option<&str>) {
    let mut pending = vec![body];
    while let Some(stmts) = pending.pop() {
        let mut index = 0;
        while index < stmts.len() {
            if let Stmt::Return(value) = &mut stmts[index] {
                let mut exit = Vec::new();
                if let (Some(value), Some(result)) = (value.take(), result) {
                    exit.push(Stmt::Assign {
                        lhs: LValue::Ident(result.to_owned()),
                        rhs: value,
                        nonblocking: false,
                    });
                    stmts[index] = Stmt::Return(Some(Expr::Ident(result.to_owned())));
                }
                exit.extend(epilogue.iter().cloned());
                let count = exit.len();
                stmts.splice(index..index, exit);
                index += count + 1;
                continue;
            }
            index += 1;
        }
        for stmt in stmts.iter_mut() {
            match stmt {
                Stmt::If {
                    then_body,
                    else_body,
                    ..
                } => {
                    pending.push(then_body);
                    pending.push(else_body);
                }
                Stmt::Case { items, default, .. } => {
                    pending.extend(items.iter_mut().map(|item| &mut item.body));
                    pending.extend(default.as_mut());
                }
                Stmt::Loop {
                    init, step, body, ..
                } => {
                    pending.push(init);
                    pending.push(step);
                    pending.push(body);
                }
                Stmt::Block { body, .. } => pending.push(body),
                // A `return` in a forked branch ends only that branch.
                _ => {}
            }
        }
    }
}

/// The declared arguments of a task or function: `(name, direction, type,
/// default)`. An argument without a direction takes the previous one's, and
/// one without a type the previous one's type (IEEE 1800-2023 13.3).
pub(in crate::ast) fn subroutine_param_declarations(
    native: &celox_sv_syntax::Subroutine,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: impl ToShared<i128, ConstEnv>,
    aliases: impl ToShared<Type, TypeAliases>,
) -> Converted<
    Vec<(
        String,
        ParamDirection,
        Type,
        Option<celox_sv_syntax::ExprId>,
    )>,
> {
    type_binding::subroutine_formals(
        native,
        tree,
        session,
        &PackedDimensions::new(HashMap::default(), env, aliases),
    )
}

/// The token that declares formal `source` of `native`, which keys its
/// name (see `BodyBuilder::declare_owned`); a formal a
/// prototype names without declaring is keyed by the subroutine.
fn formal_declaration(native: &celox_sv_syntax::Subroutine, source: &str) -> usize {
    native
        .ports
        .iter()
        .flatten()
        .chain(
            native
                .port_declarations
                .iter()
                .flat_map(|group| &group.ports),
        )
        .find(|port| {
            port.name
                .as_ref()
                .is_some_and(|name| instances::normalize_identifier(&name.spelling) == source)
        })
        .map_or(native.tokens.start, |port| port.tokens.start)
}

/// The return metadata of every function in the active generate items and
/// of the static functions of their classes, as its declaration gives it,
/// keyed by its (qualified) name: what an expression of a constant
/// function's body knows of a function it calls, such as the width a cast of
/// the result keeps (IEEE 1800-2023 6.24.1, 13.4.3), before discovery
/// publishes the scope's signatures. A function whose signature does not
/// bind has none here; it is rejected where its body is.
pub(in crate::ast) fn function_returns(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    type_aliases: &SharedAliases,
    const_env: &ConstEnv,
) -> Converted<HashMap<String, FunctionReturnMetadata>> {
    let mut returns = HashMap::default();
    for item in generate::subroutine_items(tree, session, const_env, type_aliases)? {
        let tree = &item.scoped(tree);
        let item_aliases = item.shared_type_aliases(type_aliases);
        for native_id in subroutine_owners::item(item.id, tree)? {
            let tree = &item.enter(tree);
            if let Some(metadata) =
                declared_return(native_id, tree, session, &item.env, &item_aliases)
            {
                let native = tree.declarations().subroutine(native_id);
                returns.insert(
                    item.name(&instances::normalize_identifier(&native.name.spelling)),
                    metadata,
                );
            }
        }
        for class in classes::item_classes(item.id, tree, session) {
            let tree = &item.enter(tree);
            let aliases = item.type_aliases(type_aliases);
            // A specialization's methods see its parameters (8.25).
            let (env, aliases) =
                classes::specializations::scope(session, &class, &item.env, &aliases);
            let (env, aliases): (SharedEnv, SharedAliases) = (env.into(), aliases.into());
            for (native_id, is_static) in classes::declared_methods(tree, session, &class)? {
                // Only a static method is called without a handle, as a
                // constant function may call one.
                if !is_static {
                    continue;
                }
                if let Some(metadata) = declared_return(native_id, tree, session, &env, &aliases) {
                    let native = tree.declarations().subroutine(native_id);
                    let name = classes::Class::subroutine_name(
                        &class,
                        &instances::normalize_identifier(&native.name.spelling),
                    );
                    returns.insert(item.name(&name), metadata);
                }
            }
        }
    }
    Ok(returns)
}

/// The return metadata of function `native_id` as its declaration gives
/// it, its formals and result bound with constants `env` and types
/// `aliases` of the scope that declares it; `None` for a task, or a function
/// whose signature does not bind.
fn declared_return(
    native_id: celox_sv_syntax::SubroutineId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &SharedEnv,
    aliases: &SharedAliases,
) -> Option<FunctionReturnMetadata> {
    let native = tree.declarations().subroutine(native_id);
    if native.kind != celox_sv_syntax::SubroutineKind::Function {
        return None;
    }
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    let params = type_binding::subroutine_formals(native, tree, session, &context).ok()?;
    let return_type = match native.return_type {
        Some(id)
            if matches!(
                tree.declarations().data_type(id).kind,
                celox_sv_syntax::DataTypeKind::Implicit
            ) =>
        {
            Some(type_binding::implicit_subroutine_type(id, tree, session, env, aliases).ok()?)
        }
        Some(id) => {
            type_binding::procedural_type(id, tree.declarations(), tree, session, env, aliases)
                .ok()?
        }
        None => None,
    };
    let arguments = params
        .iter()
        .map(|(name, direction, _, default)| FunctionArgumentMetadata {
            name: name.clone(),
            direction: *direction,
            has_default: default.is_some(),
        })
        .collect();
    Some(FunctionReturnMetadata::from_type(
        tree,
        session,
        return_type.as_ref(),
        arguments,
        &context,
    ))
}

/// The argument names of every function and task in the active generate
/// items, in declaration order, keyed by their (qualified) names.
pub(in crate::ast) fn subroutine_argument_names(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    const_env: &ConstEnv,
    type_aliases: &SharedAliases,
) -> Result<
    (
        HashMap<String, Vec<String>>,
        HashMap<String, Vec<VariableDimensions>>,
        lifetimes::Directions,
    ),
    AnalyzerError,
> {
    let mut names = HashMap::default();
    let mut shapes = HashMap::default();
    let mut directions = HashMap::default();
    for item in generate::subroutine_items(tree, session, const_env, type_aliases)? {
        let tree = &item.scoped(tree);
        let item_aliases = item.shared_type_aliases(type_aliases);
        for native_id in subroutine_owners::item(item.id, tree)? {
            let native = tree.declarations().subroutine(native_id);
            let tree = &item.enter(tree);
            let params =
                subroutine_param_declarations(native, tree, session, &item.env, &item_aliases)?;
            let qualified = item.name(&instances::normalize_identifier(&native.name.spelling));
            let param_shapes: Vec<VariableDimensions> = params
                .iter()
                .map(|(_, _, r#type, _)| {
                    bound_dimensions_from_type(
                        &scoped_type(r#type.clone(), &item.env),
                        &tree.functions,
                        &mut session.types,
                        &item.env,
                    )
                })
                .collect();
            let param_directions: Vec<ParamDirection> =
                params.iter().map(|(_, direction, ..)| *direction).collect();
            directions.insert(qualified.clone(), param_directions);
            let params: Vec<String> = params.into_iter().map(|(name, ..)| name).collect();
            names.insert(qualified.clone(), params);
            shapes.insert(qualified, param_shapes);
        }
        for class in classes::item_classes(item.id, tree, session) {
            let tree = &item.enter(tree);
            let aliases = item.type_aliases(type_aliases);
            // A specialization's methods see its parameters (8.25).
            let (env, aliases) =
                classes::specializations::scope(session, &class, &item.env, &aliases);
            let handle = classes::handle_type(tree, session, &class)
                .ok_or_else(|| unsupported(format!("class `{class}`")))?;
            let mut constructor = false;
            let this = (
                classes::THIS.to_owned(),
                ParamDirection::Input,
                handle.clone(),
                None,
            );
            let mut methods = classes::declared_methods(tree, session, &class)?
                .into_iter()
                .map(|(native_id, is_static)| {
                    let name = instances::normalize_identifier(
                        &tree.declarations().subroutine(native_id).name.spelling,
                    );
                    (
                        classes::Class::subroutine_name(&class, &name),
                        native_id,
                        is_static,
                    )
                })
                .collect::<Vec<_>>();
            // A class that cannot be laid out reports why where it is
            // lowered.
            let layout = classes::class(tree, session, &class, &env, &aliases).ok();
            if let Some(layout) = &layout {
                methods.extend(layout.dispatched_methods().map(|method| {
                    (
                        classes::Class::dispatcher_name(&class, &method.name),
                        method.native,
                        false,
                    )
                }));
            }
            // The methods bind their formals in one context of the class.
            let context = PackedDimensions::new(HashMap::default(), &env, &aliases);
            for (name, native_id, is_static) in methods {
                let native = tree.declarations().subroutine(native_id);
                let mut params = type_binding::subroutine_formals(native, tree, session, &context)?;
                constructor |= native.kind == celox_sv_syntax::SubroutineKind::Constructor;
                if !is_static {
                    params.insert(0, this.clone());
                }
                let qualified = item.name(&name);
                directions.insert(
                    qualified.clone(),
                    params.iter().map(|(_, direction, ..)| *direction).collect(),
                );
                shapes.insert(
                    qualified.clone(),
                    params
                        .iter()
                        .map(|(_, _, r#type, _)| {
                            bound_dimensions_from_type(
                                &scoped_type(r#type.clone(), &env),
                                &tree.functions,
                                &mut session.types,
                                &env,
                            )
                        })
                        .collect(),
                );
                names.insert(
                    qualified,
                    params.into_iter().map(|(name, ..)| name).collect(),
                );
            }
            for method in ["randomize", "srandom", "rand_mode"] {
                let qualified = item.name(&classes::Class::subroutine_name(&class, method));
                let mut param_names = vec![classes::THIS.to_owned()];
                let mut param_shapes = vec![bound_dimensions_from_type(
                    &handle,
                    &tree.functions,
                    &mut session.types,
                    &env,
                )];
                let (argument, ty) = match method {
                    "randomize" => (
                        "$selection",
                        layout.as_ref().map_or_else(
                            || classes::unsigned_type(1),
                            |layout| layout.selection_type(),
                        ),
                    ),
                    "srandom" => ("seed", int_type()),
                    _ => ("on_off", classes::unsigned_type(1)),
                };
                param_names.push(argument.into());
                param_shapes.push(bound_dimensions_from_type(
                    &ty,
                    &tree.functions,
                    &mut session.types,
                    &env,
                ));
                directions.insert(
                    qualified.clone(),
                    vec![ParamDirection::Input; param_names.len()],
                );
                shapes.insert(qualified.clone(), param_shapes);
                names.insert(qualified, param_names);
            }
            let handle = bound_dimensions_from_type(
                &scoped_type(handle, &env),
                &tree.functions,
                &mut session.types,
                &env,
            );
            if !constructor && !layout.is_some_and(|layout| layout.is_interface) {
                let qualified = item.name(&classes::Class::subroutine_name(&class, "new"));
                directions.insert(qualified.clone(), vec![ParamDirection::Input]);
                shapes.insert(qualified.clone(), vec![handle.clone()]);
                names.insert(qualified, vec![classes::THIS.to_owned()]);
            }
            let qualified = item.name(&classes::Class::cast_name(&class));
            directions.insert(
                qualified.clone(),
                vec![ParamDirection::Inout, ParamDirection::Input],
            );
            shapes.insert(qualified.clone(), vec![handle.clone(), handle]);
            names.insert(
                qualified,
                vec![CAST_DESTINATION.to_owned(), CAST_SOURCE.to_owned()],
            );
        }
    }
    Ok((names, shapes, directions))
}

/// Every function and task of the active generate items, with statement
/// bodies.
pub(in crate::ast) fn subroutines_from_scope(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    const_env: &ConstEnv,
    packed_dimensions: &PackedDimensions,
    parameter_literals: &HashMap<String, Expr>,
    state: &mut BodyState<'_>,
) -> Result<Vec<Subroutine>, AnalyzerError> {
    subroutines_from_scope_with(
        tree,
        session,
        const_env,
        packed_dimensions,
        parameter_literals,
        state,
        None,
    )
}

/// The subroutines of a module. With `rejected`, a subroutine that cannot be
/// lowered is left out and the reason recorded under its name, instead of
/// failing.
pub(in crate::ast) fn subroutines_from_scope_with(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    const_env: &ConstEnv,
    packed_dimensions: &PackedDimensions,
    parameter_literals: &HashMap<String, Expr>,
    state: &mut BodyState<'_>,
    rejected: Option<&mut HashMap<String, AnalyzerError>>,
) -> Result<Vec<Subroutine>, AnalyzerError> {
    subroutines_with_symbolic_parameters(
        tree,
        session,
        const_env,
        packed_dimensions,
        parameter_literals,
        state,
        rejected,
        &HashSet::default(),
        ClassMethods::All,
    )
}

#[allow(clippy::too_many_arguments)]
pub(in crate::ast) fn subroutines_with_symbolic_parameters(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    const_env: &ConstEnv,
    packed_dimensions: &PackedDimensions,
    parameter_literals: &HashMap<String, Expr>,
    state: &mut BodyState<'_>,
    mut rejected: Option<&mut HashMap<String, AnalyzerError>>,
    symbolic_parameters: &HashSet<String>,
    class_methods: ClassMethods,
) -> Result<Vec<Subroutine>, AnalyzerError> {
    let module_type_aliases = &packed_dimensions.type_aliases;
    let mut subroutines: Vec<Subroutine> = Vec::new();
    // The names of `subroutines`.
    let mut names = HashSet::default();
    let active = generate::subroutine_items(tree, session, const_env, module_type_aliases)?;
    let mut views = generate::ScopeViews::with_literals(packed_dimensions, parameter_literals);
    // What the last analysis of the package lowered for its classes, and
    // what this pass lowers for the next.
    let package = reused_package(tree, session, state, symbolic_parameters, class_methods);
    let mut lowered = Lowered::default();
    let mut previous = None;
    if package.is_some() {
        lowered.known = classes::visible_classes(tree, session)
            .into_iter()
            .collect();
    }
    if let Some(last) = package
        .as_deref()
        .and_then(|package| session.foreign.lowered(package))
        && let Some(outdated) = outdated_classes(
            tree,
            session,
            const_env,
            module_type_aliases,
            &last,
            &lowered.known,
        )
    {
        previous = Some((last, outdated));
    }
    for item in &active {
        let tree = &item.scoped(tree);
        if item.is_parameter_declaration(tree.declarations()) {
            continue;
        }
        for native_id in subroutine_owners::item(item.id, tree)? {
            let (item_dimensions, literals) = views.get(item);
            let item_dimensions = if symbolic_parameters.is_empty() {
                std::borrow::Cow::Borrowed(item_dimensions)
            } else {
                let mut dimensions = item_dimensions.clone();
                for parameter in symbolic_parameters {
                    dimensions.const_env.remove(parameter);
                    dimensions.parameter_values.remove(parameter);
                }
                std::borrow::Cow::Owned(dimensions)
            };
            let item_dimensions = &*item_dimensions;

            let declaration_env: SharedEnv = if symbolic_parameters.is_empty() {
                item.env.clone()
            } else {
                item.env
                    .iter()
                    .filter(|(name, _)| !symbolic_parameters.contains(*name))
                    .map(|(name, value)| (name.clone(), *value))
                    .collect()
            };

            let tree = &item.enter(tree);
            let type_aliases = item.shared_type_aliases(module_type_aliases);
            let name = instances::normalize_identifier(
                &tree.declarations().subroutine(native_id).name.spelling,
            );
            let lowered = lower_subroutine(
                native_id,
                tree,
                session,
                &declaration_env,
                &type_aliases,
                item_dimensions,
                literals,
                state,
                item,
                None,
                None,
            );
            let subroutine = match (lowered, rejected.as_deref_mut()) {
                (Ok(subroutine), _) => subroutine,
                (Err(error), Some(rejected)) => {
                    rejected.insert(name, error);
                    continue;
                }
                (Err(error), None) => return Err(error),
            };
            if !names.insert(subroutine.name.clone()) {
                return Err(unsupported(format!(
                    "duplicate function declaration `{}`",
                    subroutine.name
                )));
            }
            subroutines.push(subroutine);
        }
        if class_methods == ClassMethods::None {
            continue;
        }
        for class in classes::item_classes(item.id, tree, session) {
            let taken = previous
                .as_ref()
                .filter(|(_, outdated)| !outdated.contains(&class))
                .and_then(|(last, _)| last.classes.get(&class))
                .cloned();
            let methods = if let Some(taken) = taken {
                taken.replay(session, state);
                let methods = taken.methods.clone();
                lowered.classes.insert(class.clone(), taken);
                methods
            } else {
                #[cfg(test)]
                if class_methods == ClassMethods::All {
                    CLASS_LOWERINGS.with(|count| count.set(count.get() + 1));
                }
                let mark = package.as_ref().map(|_| Mark::of(session, state));
                let (item_dimensions, literals) = views.get(item);
                let tree = &item.enter(tree);
                let type_aliases = item.type_aliases(module_type_aliases);
                let methods = lower_class(
                    &class,
                    tree,
                    session,
                    &item.env,
                    &type_aliases,
                    item_dimensions,
                    literals,
                    state,
                    item,
                    class_methods,
                );
                if let Some(mark) = mark {
                    lowered.classes.insert(
                        class.clone(),
                        std::rc::Rc::new(LoweredClass::since(
                            mark,
                            methods.clone(),
                            session,
                            state,
                        )),
                    );
                }
                methods
            };
            match (methods, rejected.as_deref_mut()) {
                (Ok(methods), _) => {
                    names.extend(methods.iter().map(|method| method.name.clone()));
                    subroutines.extend(methods);
                }
                (Err(error), Some(rejected)) => {
                    rejected.insert(class, error);
                }
                (Err(error), None) => return Err(error),
            }
        }
    }
    if let Some(package) = package {
        lowered.counter = *state.counter;
        session.foreign.record_lowered(package, lowered);
    }
    // A class of the scope, or of another package, that overrides a virtual
    // method of a class that another scope declares is reached through that
    // method's dispatcher, which the declaring scope lowered for the classes
    // it saw: the scope lowers the dispatcher again, over every class it
    // sees, when the dispatcher misses one (IEEE 1800-2023 8.20).
    // Dispatchers and casts take an object, as the methods a pass over
    // static functions leaves out do.
    if class_methods != ClassMethods::All {
        return Ok(subroutines);
    }
    let imported = scope::imported(tree);
    // The first subroutine of each name the scope imports.
    let mut imported_subroutines: HashMap<&str, &Subroutine> = HashMap::default();
    if !imported.classes.is_empty() {
        for subroutine in &imported.subroutines {
            imported_subroutines
                .entry(subroutine.name.as_str())
                .or_insert(subroutine);
        }
    }
    for class in &imported.classes {
        for method in class.dispatched_methods() {
            let name = classes::Class::dispatcher_name(&class.name, &method.name);
            if names.contains(&name) {
                continue;
            }
            let Some(dispatcher) = imported_subroutines.get(name.as_str()).copied() else {
                continue;
            };
            let targets = classes::dispatch_targets(
                tree,
                session,
                &class.name,
                &method.name,
                const_env,
                module_type_aliases,
            )?;
            let covered = case_ids(&dispatcher.body);
            if targets
                .iter()
                .all(|(_, ids)| ids.iter().all(|id| covered.contains(id)))
            {
                continue;
            }
            let mut dispatcher = dispatcher.clone();
            dispatcher.body = vec![dispatch_body(
                &targets,
                &dispatcher.params,
                dispatcher.return_var.as_deref(),
            )];
            names.insert(name);
            subroutines.push(dispatcher);
        }
        // So does a `$cast` to a variable of the class (8.16).
        let name = classes::Class::cast_name(&class.name);
        if names.contains(&name) {
            continue;
        }
        let Some(cast) = imported_subroutines.get(name.as_str()).copied() else {
            continue;
        };
        let targets =
            classes::cast_targets(tree, session, &class.name, const_env, module_type_aliases)?;
        let covered = case_ids(&cast.body);
        if targets.iter().all(|id| covered.contains(id)) {
            continue;
        }
        let mut cast = cast.clone();
        if let Some(Stmt::Case { items, .. }) = cast.body.first_mut()
            && let Some(item) = items.first_mut()
        {
            item.labels = std::iter::once(0)
                .chain(targets)
                .map(|id| CaseLabel::Value(Expr::Literal(format!("32'd{id}"))))
                .collect();
            subroutines.push(cast);
        }
    }
    Ok(subroutines)
}

/// The class identifiers that the case on the class of an object in `body`,
/// the body of a dispatcher or a `$cast` helper, selects.
pub(super) fn case_ids(body: &[Stmt]) -> HashSet<u32> {
    let Some(Stmt::Case { items, .. }) = body.first() else {
        return HashSet::default();
    };
    items
        .iter()
        .flat_map(|item| &item.labels)
        .filter_map(|label| match label {
            CaseLabel::Value(Expr::Literal(literal)) => literal.strip_prefix("32'd")?.parse().ok(),
            _ => None,
        })
        .collect()
}

/// Shared native subroutine lowering for execution and expression-form plans.
/// Native ownership/control is identical in both consumers; typed headers remain
/// borrowed operands until their independent type kernels are migrated.
#[allow(clippy::too_many_arguments)]
pub(in crate::ast) fn lower_subroutine(
    native_id: celox_sv_syntax::SubroutineId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    declaration_env: &SharedEnv,
    type_aliases: &SharedAliases,
    item_dimensions: &PackedDimensions,
    literals: &HashMap<String, Expr>,
    state: &mut BodyState<'_>,
    item: &generate::Item,
    class: Option<&classes::Class>,
    dispatch: Option<&[(String, Vec<u32>)]>,
) -> Converted<Subroutine> {
    let native = tree.declarations().subroutine(native_id);
    let name = instances::normalize_identifier(&native.name.spelling);
    let constructor = native.kind == celox_sv_syntax::SubroutineKind::Constructor;
    let is_static = dispatch.is_none()
        && class.is_some_and(|class| {
            class
                .methods
                .iter()
                // An inherited method of a class of another source may
                // have the same identifier.
                .any(|method| {
                    method.native == native_id
                        && method.owner == class.name
                        && method.qualifiers.is_static
                })
        });
    let handle = class
        .map(|class| {
            classes::handle_type(tree, session, &class.name)
                .ok_or_else(|| unsupported(format!("class `{}`", class.name)))
        })
        .transpose()?;
    let params =
        subroutine_param_declarations(native, tree, session, declaration_env, type_aliases)?;
    let mut formal_names = HashSet::default();
    if let Some((name, ..)) = params
        .iter()
        .find(|(name, ..)| !formal_names.insert(name.as_str()))
    {
        return Err(unsupported(format!("duplicate function argument `{name}`")));
    }
    let return_type = match native.return_type {
        _ if constructor => handle.clone(),
        Some(id)
            if matches!(
                tree.declarations().data_type(id).kind,
                celox_sv_syntax::DataTypeKind::Implicit
            ) =>
        {
            Some(type_binding::implicit_subroutine_type(
                id,
                tree,
                session,
                declaration_env,
                type_aliases,
            )?)
        }
        Some(id) => type_binding::procedural_type(
            id,
            tree.declarations(),
            tree,
            session,
            declaration_env,
            type_aliases,
        )?,
        None => None,
    };
    if return_type
        .as_ref()
        .into_iter()
        .chain(params.iter().map(|(_, _, ty, _)| ty))
        .filter_map(|ty| ty.unpacked_struct())
        .any(|aggregate| aggregate.leaves().iter().any(|leaf| leaf.default.is_some()))
    {
        return Err(unsupported(
            "unpacked structure member initial values in subroutine interfaces",
        ));
    }
    if return_type
        .as_ref()
        .into_iter()
        .chain(params.iter().map(|(_, _, ty, _)| ty))
        .filter_map(|ty| ty.unpacked_struct())
        .any(|aggregate| aggregate.has_array_members())
    {
        return Err(unsupported(
            "unpacked structure array members in subroutine interfaces",
        ));
    }
    // An event control could not tell a call of such a function from a
    // value (IEEE 1800-2023 15.5).
    if return_type
        .as_ref()
        .is_some_and(|ty| ty.kind() == TypeKind::Event)
    {
        return Err(unsupported("function returning an event"));
    }
    let qualified_name = item.name(&match class {
        Some(class) if dispatch.is_some() => classes::Class::dispatcher_name(&class.name, &name),
        Some(class) => classes::Class::subroutine_name(&class.name, &name),
        None => name.clone(),
    });
    let scope = scope_name(tree, session, declaration_env, native.tokens, class, &name);
    let mut builder = BodyBuilder::new(
        tree,
        session,
        item_dimensions,
        state,
        system_functions::Body::Subroutine,
    );
    builder.set_owner(qualified_name.clone());
    builder.dims.scope_name = Some(std::sync::Arc::new(scope));
    builder.task_timing = native.kind == celox_sv_syntax::SubroutineKind::Task;
    let first_local = builder.state.locals.len();
    builder.automatic = match native.lifetime.as_deref() {
        Some("automatic") => true,
        Some("static") => false,
        Some(_) => {
            return Err(AnalyzerError::Parse(
                "unknown native subroutine lifetime".into(),
            ));
        }
        // Methods are automatic (IEEE 1800-2023 8.6).
        None => builder.automatic || class.is_some(),
    };
    if !builder.automatic
        && let Some((argument, ..)) = params
            .iter()
            .find(|(_, direction, ..)| direction.is_reference())
    {
        return Err(AnalyzerError::InvalidRefArgument {
            subroutine: name.clone(),
            argument: argument.clone(),
            detail: "a subroutine with a static lifetime cannot pass arguments by reference \
                     (IEEE 1800-2023 13.5.2)"
                .into(),
        });
    }
    let first_static = builder.state.statics.len();
    builder.push_scope();
    if let Some(class) = class {
        builder.enter_class_scope(class);
        builder.dims.constructor = constructor;
        builder.dims.method_name = Some(name.clone());
        // Method formals and locals may shadow properties, but not another
        // declaration in the method's own lexical scope (8.5, 6.21).
        builder.push_scope();
    }
    // A method that is not static receives the handle of its object first,
    // and a constructor returns the handle of the object it initializes.
    // Its defaults are evaluated in the scope of the method, so they may
    // use the object (IEEE 1800-2023 13.5.3); the frontend replaces the
    // handle with the receiver of a call that uses one.
    let this = handle.clone().filter(|_| !is_static).map(|handle| {
        (
            classes::THIS.to_owned(),
            ParamDirection::Input,
            handle,
            None,
        )
    });
    let mut this_name = this.as_ref().map(|(source, _, r#type, _)| {
        builder.declare_owned(source, r#type.clone(), native.tokens.start)
    });
    // Defaults bind in the enclosing declaration scope, before
    // any formal can shadow those names (IEEE 1800-2023 13.5.3).
    let defaults = params
        .iter()
        .map(|(formal, direction, ty, default)| {
            default
                .map(|root| {
                    // A default of an unpacked array formal is assigned to
                    // it like an actual (IEEE 1800-2023 7.6, 13.5.3).
                    if !ty.unpacked_ranges.is_empty() {
                        array_compatibility::check_native_unpacked_assignment(
                            tree,
                            builder.session,
                            root,
                            &dimensions_from_type(ty),
                            || format!("default of `{formal}`"),
                            &builder.dims,
                        )?;
                    }
                    expression_binding::check_chandle_assignment(
                        ty.is_chandle() && ty.unpacked_ranges.is_empty(),
                        root,
                        tree,
                        tree.declarations(),
                        &builder.dims,
                        builder.session,
                    )?;
                    // That of a class handle formal, a handle its class
                    // takes (8.16).
                    if let Some(class) = ty.class().filter(|_| ty.unpacked_ranges.is_empty()) {
                        expression_binding::check_handle_assignment(
                            tree,
                            builder.session,
                            root,
                            tree.declarations(),
                            &builder.dims,
                            class,
                            || format!("default of `{formal}`"),
                        )?;
                    }
                    // The default of a container input is a container value;
                    // that of a chandle, a chandle.
                    let shape = (ty.tagged_identity().is_some()
                        || ty.unpacked_struct().is_some()
                        || ty.is_chandle() && ty.unpacked_ranges.is_empty()
                        || ty.container.is_some() && *direction == ParamDirection::Input)
                        .then(|| dimensions_from_type(ty));
                    let mut value = expression_binding::bind_value(
                        root,
                        tree.declarations(),
                        tree,
                        builder.session,
                        &builder.dims,
                        shape.as_ref(),
                    )?;
                    builder.names.rename_expr(&mut value);
                    Ok(value)
                })
                .transpose()
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut lowered_params = Vec::new();
    let mut formal_types = HashMap::default();
    let defaults = this
        .iter()
        .map(|_| None)
        .chain(defaults)
        .collect::<Vec<_>>();
    let mut aggregate_formals = Vec::new();
    for ((source, direction, r#type, _), default) in this.into_iter().chain(params).zip(defaults) {
        let name = match this_name.take() {
            Some(name) => name,
            // Named by the subroutine and their declarations, so that every
            // analysis lowering the subroutine names the same ones: those of
            // a static subroutine are static variables (IEEE 1800-2023
            // 13.3.1), and a scope may lower the methods of another's
            // classes, with their formals.
            None => {
                let declaration = formal_declaration(native, &source);
                builder.declare_owned(&source, r#type.clone(), declaration)
            }
        };
        if r#type.unpacked_struct().is_some() {
            aggregate_formals.push((lowered_params.len(), r#type.clone()));
        }
        if direction.is_reference() {
            builder.reference_formals.insert(name.clone());
        }
        formal_types.insert(name.clone(), scoped_type(r#type.clone(), declaration_env));
        lowered_params.push(SubroutineParam {
            name,
            source_name: source,
            direction,
            r#type: super::super::ir_type(
                builder.session,
                &tree.functions,
                scoped_type(r#type, declaration_env),
                &item.env,
            ),
            default,
            leaves: Vec::new(),
        });
    }
    // So is the result of a function, and `this`, declared with the
    // function.
    let return_var = return_type
        .as_ref()
        .map(|r#type| builder.declare_owned(&name, r#type.clone(), native.tokens.start));
    if let (Some(name), Some(ty)) = (&return_var, &return_type) {
        formal_types.insert(name.clone(), scoped_type(ty.clone(), declaration_env));
    }
    // A return statement's value is received by the result as an assignment
    // to it receives its value (IEEE 1800-2023 13.4.1): the declared result
    // type is the target its patterns and other contextual operands take
    // their shape from (10.9).
    builder.return_shape = return_type.as_ref().map(dimensions_from_type);
    // An argument of unpacked structure type is passed as its flat value;
    // the body works on its members (see `unpacked_structs`).
    let mut aggregate_prologue = Vec::new();
    let mut aggregate_epilogue = Vec::new();
    // A result of unpacked structure type assigned through the function's
    // name is assembled from its members when the body ends without
    // `return` (IEEE 1800-2023 13.4.1).
    let mut result_leaves = None;
    if dispatch.is_none()
        && let (Some(result), Some(r#type)) = (&return_var, &return_type)
        && r#type.unpacked_struct().is_some()
        && !constructor
    {
        let leaves = builder
            .declare_aggregate(&name, r#type, None)?
            .into_iter()
            .map(|(leaf, _)| leaf)
            .collect::<Vec<_>>();
        // An automatic result starts each call at its default.
        if builder.automatic {
            aggregate_prologue.extend(leaves.iter().map(|leaf| Stmt::Local {
                name: leaf.clone(),
                init: None,
            }));
        }
        result_leaves = Some((result.clone(), leaves));
    }
    for (index, r#type) in &aggregate_formals {
        let param = &lowered_params[*index];
        let (name, source, direction) = (
            param.name.clone(),
            param.source_name.clone(),
            param.direction,
        );
        // The members of a reference formal are bound to the members of the
        // actual, also in the body that dispatches a virtual method.
        if dispatch.is_some() && !direction.is_reference() {
            continue;
        }
        let leaves = builder
            .declare_aggregate(&source, r#type, None)?
            .into_iter()
            .map(|(leaf, _)| leaf)
            .collect::<Vec<_>>();
        lowered_params[*index].leaves = leaves.clone();
        if direction.is_reference() {
            continue;
        }
        if direction.is_read() {
            aggregate_prologue.push(Stmt::AssignConcat {
                parts: leaves.iter().cloned().map(LValue::Ident).collect(),
                rhs: Expr::Ident(name.clone()),
                nonblocking: false,
            });
        } else if builder.automatic {
            // An automatic output starts each call at its default.
            aggregate_prologue.extend(leaves.iter().map(|leaf| Stmt::Local {
                name: leaf.clone(),
                init: None,
            }));
        }
        if direction.is_written() {
            aggregate_epilogue.push(Stmt::Assign {
                lhs: LValue::Ident(name),
                rhs: Expr::Concat(leaves.into_iter().map(Expr::Ident).collect()),
                nonblocking: false,
            });
        }
    }
    // The members of formals and of the result have the subroutine's
    // lifetime, like the formals themselves.
    builder.return_chandle = return_type
        .as_ref()
        .map(|ty| ty.is_chandle() && ty.unpacked_ranges.is_empty());
    let parameter_locals_end = builder.state.locals.len();
    let mut items = native.items.clone();
    let mut prologue = Vec::new();
    if let (Some(class), Some(result), Some(this)) = (class, &return_var, lowered_params.first())
        && constructor
    {
        // An explicit `super.new` call is made before the property
        // initializers, with the base call the prologue makes.
        let arguments = match (
            super_new(tree.declarations(), &items),
            classes::base_arguments(tree, builder.session, &class.name),
        ) {
            (Some(_), Some(_)) => {
                return Err(unsupported(
                    "`super.new` in a class whose `extends` passes constructor arguments",
                ));
            }
            (Some(_), None) if class.base.is_none() => {
                return Err(unsupported("`super.new` in a class without a base"));
            }
            (Some((index, arguments)), None) => {
                items.remove(index);
                arguments
            }
            (None, arguments) => arguments.unwrap_or_default(),
        };
        prologue = builder.constructor_prologue(class, &this.name, result, arguments)?;
    }
    let mut body = prologue;
    match dispatch {
        Some(targets) => body.push(dispatch_body(
            targets,
            &lowered_params,
            return_var.as_deref(),
        )),
        None => {
            body.extend(aggregate_prologue);
            body.extend(builder.native_body(&items, &native.authored_statements)?);
        }
    }
    if !aggregate_epilogue.is_empty() {
        // Copy the members back on every way out of the body, after the
        // value of a `return` is taken.
        before_returns(&mut body, &aggregate_epilogue, return_var.as_deref());
    }
    if let Some((result, leaves)) = result_leaves {
        body.push(Stmt::Assign {
            lhs: LValue::Ident(result),
            rhs: Expr::Concat(leaves.into_iter().map(Expr::Ident).collect()),
            nonblocking: false,
        });
    }
    body.extend(aggregate_epilogue);
    builder.pop_scope();
    if class.is_some() {
        builder.pop_scope();
    }
    // The locals of the array locator calls of the body are its own, as
    // automatic as the rest of them (IEEE 1800-2023 7.12).
    builder.flush_locator_locals();
    let static_types: HashMap<String, Type> = builder
        .state
        .statics
        .split_off(first_static)
        .into_iter()
        .collect();
    let statics = static_types.keys().cloned().collect::<HashSet<_>>();
    // A constructor's `return;` returns the object (IEEE 1800-2023 8.7).
    if return_var.is_some() && !constructor {
        let mut expressionless = false;
        for stmt in &body {
            stmt.walk(&mut |stmt| expressionless |= matches!(stmt, Stmt::Return(None)));
        }
        if expressionless {
            return Err(unsupported("expressionless function return"));
        }
    }
    let mut subroutine = Subroutine {
        name: qualified_name,
        is_task: native.kind == celox_sv_syntax::SubroutineKind::Task,
        automatic: builder.automatic,
        automatic_locals: builder.state.locals[first_local..]
            .iter()
            .enumerate()
            .filter(|(index, local)| {
                if first_local + index < parameter_locals_end {
                    builder.automatic
                } else {
                    !statics.contains(&local.name)
                }
            })
            .map(|(_, local)| local.name.clone())
            .collect(),
        persistent_locals: Vec::new(),
        return_type: return_type.map(|r#type| {
            super::super::ir_type(
                builder.session,
                &tree.functions,
                scoped_type(r#type, declaration_env),
                &item.env,
            )
        }),
        params: lowered_params,
        return_var,
        body,
        sensitivity_body: std::mem::take(&mut builder.sensitivity_body),
    };
    for stmt in subroutine
        .body
        .iter_mut()
        .chain(&mut subroutine.sensitivity_body)
    {
        qualify_stmt(item, stmt);
        substitute_stmt_constants(tree, builder.session, stmt, declaration_env, literals);
    }
    for param in &mut subroutine.params {
        if let Some(default) = &mut param.default {
            item.expr(default);
            *default = substitute_expr_constants_with_parameter_literals(
                tree,
                builder.session,
                default.clone(),
                &item.env,
                literals,
            );
        }
    }
    // The statics of a task that may suspend are shared by activations
    // that overlap in time (IEEE 1800-2023 6.21, 13.3.1): another call may
    // write one between the declaration and a read, so each keeps its value.
    let mut suspends = false;
    if subroutine.is_task {
        for stmt in &subroutine.body {
            // Blocking scheduler operations, such as `semaphore.get()`, are
            // evaluated calls.
            stmt.walk(&mut |stmt| {
                suspends |= events::blocks(stmt)
                    || matches!(
                        stmt,
                        Stmt::Delay { .. }
                            | Stmt::WaitEvent(_)
                            | Stmt::WaitImplicit { .. }
                            | Stmt::Wait(_)
                            | Stmt::AssertionRegion { .. }
                            | Stmt::WaitFork
                            | Stmt::Fork { .. }
                            | Stmt::Call { .. }
                    );
            });
        }
    }
    if !builder.state.constant_evaluation {
        let retain_result = !builder.automatic
            && lifetimes::check_static_result(
                tree,
                builder.session,
                &builder.dims.const_env,
                builder.state.locals,
                &subroutine,
                builder.state.subroutine_directions,
            )
            .is_err();
        // Keep scratch arrays/bitwise result proofs on the existing temporary path.
        let retained_types = static_types
            .iter()
            .filter(|(name, _)| {
                suspends
                    || lifetimes::check_subroutine_statics(
                        tree,
                        builder.session,
                        &subroutine,
                        &builder.dims.const_env,
                        builder.state.locals,
                        &std::iter::once((*name).clone()).collect(),
                        builder.state.subroutine_directions,
                    )
                    .is_err()
            })
            .map(|(name, ty)| (name.clone(), ty.clone()))
            .collect::<HashMap<_, _>>();
        // The initializer of a static local runs once, before simulation
        // starts, whatever it calls (IEEE 1800-2023 6.8, 6.21), unless it
        // reads a formal or an automatic local.
        let formals = subroutine
            .params
            .iter()
            .map(|param| param.name.clone())
            .chain(subroutine.return_var.clone())
            .collect::<Vec<_>>();
        let (mut kept, mut initializers) = lifetimes::keep_static_locals(
            tree,
            builder.session,
            &retained_types,
            builder.state.subroutine_directions,
            std::iter::once((&mut subroutine.body, false)),
            &statics,
            &formals,
            suspends,
        )?;
        if retain_result {
            kept.extend(subroutine.return_var.clone());
        }
        for param in &subroutine.params {
            if param.direction == ParamDirection::Output
                && !subroutine.automatic_locals.contains(&param.name)
                && lifetimes::output_retains_value(
                    &subroutine,
                    &param.name,
                    builder.state.subroutine_directions,
                )
            {
                kept.push(param.name.clone());
            }
        }
        // Overlapping calls of a task that suspends share its static formals
        // (IEEE 1800-2023 13.3.2). Those of a package or `$unit` task are
        // variables of that scope, shared by every module calling it; a
        // module allocates its own once.
        let shared_scope = !matches!(
            tree.root_unit(),
            Ok(unit) if unit.kind != celox_sv_syntax::UnitKind::Package
        );
        if suspends && shared_scope {
            for param in &subroutine.params {
                if matches!(
                    param.direction,
                    ParamDirection::Input | ParamDirection::Output | ParamDirection::Inout
                ) && !subroutine.automatic_locals.contains(&param.name)
                    && !kept.contains(&param.name)
                    // An unpacked structure or array formal keeps its
                    // members or elements apart, which the scope does not
                    // declare as variables.
                    && param.leaves.is_empty()
                    && param.r#type.structure_leaves().is_empty()
                    && param.r#type.unpacked_ranges().is_empty()
                {
                    kept.push(param.name.clone());
                }
            }
        }
        for initializer in &mut initializers {
            qualify_stmt(item, initializer);
            substitute_stmt_constants(
                tree,
                builder.session,
                initializer,
                declaration_env,
                literals,
            );
        }
        for name in &kept {
            let ty = static_types
                .get(name)
                .or_else(|| formal_types.get(name))
                .expect("function storage has a local type");
            builder.state.statics.push((name.clone(), ty.clone()));
        }
        builder.state.static_initializers.extend(initializers);
        subroutine.persistent_locals = kept;
    }
    Ok(subroutine)
}
