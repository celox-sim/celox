//! The subroutines of a class: dispatchers, constructors, methods and `$cast` helpers.
use super::*;

/// The body of a dispatcher of a virtual method: a call of the override in
/// `targets` for the class of `this`, the first of `params`, passing the
/// formals on (IEEE 1800-2023 8.20). Every class an object can have is in
/// `targets`, so any other class is that of `null`, whose dereference is an
/// error (8.4).
pub(super) fn dispatch_body(
    targets: &[(String, Vec<u32>)],
    params: &[SubroutineParam],
    result: Option<&str>,
) -> Stmt {
    let this = Expr::Ident(params[0].name.clone());
    let args = params
        .iter()
        .map(|param| {
            Some(
                if param.direction.is_reference() && !param.leaves.is_empty() {
                    Expr::Concat(param.leaves.iter().cloned().map(Expr::Ident).collect())
                } else {
                    Expr::Ident(param.name.clone())
                },
            )
        })
        .collect::<Vec<_>>();
    let items = targets
        .iter()
        .map(|(target, classes)| {
            let call = match result {
                Some(result) => Stmt::Assign {
                    lhs: LValue::Ident(result.to_owned()),
                    rhs: Expr::Call {
                        callee: Callee::Subroutine(target.clone()),
                        args: args.clone(),
                        arg_order: Vec::new(),
                    },
                    nonblocking: false,
                },
                None => Stmt::Call {
                    name: target.clone(),
                    args: args.clone(),
                    arg_order: Vec::new(),
                },
            };
            CaseItemBase {
                labels: classes
                    .iter()
                    .map(|id| CaseLabel::Value(Expr::Literal(format!("32'd{id}"))))
                    .collect(),
                body: vec![call],
            }
        })
        .collect();
    let null_dereference = classes::object_call(
        system_functions::ObjectOp::Load(system_functions::ObjectField {
            offset: 0,
            width: 8,
            signed: false,
            handle: None,
            four_state: false,
            nullable: false,
            real: false,
            key: crate::system_functions::ChangeKey::None,
        }),
        vec![this.clone()],
    );
    Stmt::Case {
        kind: CaseKind::Exact,
        selector: classes::object_call(system_functions::ObjectOp::ClassOf, vec![this]),
        items,
        default: Some(vec![Stmt::Eval(null_dereference)]),
    }
}

/// The `super.new(args)` call that starts constructor body `items`, as its
/// index in `items` and its arguments (IEEE 1800-2023 8.15, 8.17).
pub(super) fn super_new<'a>(
    native: &'a celox_sv_syntax::Declarations,
    items: &[celox_sv_syntax::StatementId],
) -> Option<(usize, &'a [celox_sv_syntax::Argument])> {
    use celox_sv_syntax::{ExprKind, StatementKind};
    let (index, statement) = items.iter().enumerate().find(|(_, id)| {
        !matches!(
            native.statement(**id).kind,
            StatementKind::Declaration(_) | StatementKind::Empty
        )
    })?;
    let StatementKind::Expression(root) = native.statement(*statement).kind else {
        return None;
    };
    let root = expression_binding::ungroup(root, native);
    let (callee, arguments) = match &native.expression(root).kind {
        ExprKind::Call {
            expression,
            arguments,
        } => (*expression, arguments.as_slice()),
        _ => (root, &[][..]),
    };
    let ExprKind::Member {
        expression,
        separator,
        name,
    } = &native.expression(callee).kind
    else {
        return None;
    };
    (separator == "."
        && name.spelling == "new"
        && matches!(&native.expression(*expression).kind,
            ExprKind::Name(name) if name.spelling == "super"))
    .then_some((index, arguments))
}

impl BodyBuilder<'_, '_, '_> {
    /// The statements a constructor of `class` runs before its body, on the
    /// object `this` its caller allocated: return `this` as `result`, call
    /// the constructor of the base with `arguments`, and initialize the
    /// properties the class declares (IEEE 1800-2023 8.7, 8.8, 8.17).
    pub(super) fn constructor_prologue(
        &mut self,
        class: &classes::Class,
        this: &str,
        result: &str,
        arguments: &[celox_sv_syntax::Argument],
    ) -> Converted<Vec<Stmt>> {
        let native = self.tree.declarations();
        let mut body = vec![Stmt::Assign {
            lhs: LValue::Ident(result.to_owned()),
            rhs: Expr::Ident(this.to_owned()),
            nonblocking: false,
        }];
        body.extend(class.program_registration());
        if let Some(base) = &class.base {
            let base = classes::class(
                self.tree,
                self.session,
                base,
                &self.dims.const_env,
                &self.dims.type_aliases,
            )?;
            if let Some(method) = base.method("new") {
                expression_binding::check_member_access(
                    self.tree,
                    self.session,
                    &self.dims,
                    &method.owner,
                    &method.name,
                    method.qualifiers.local,
                    method.qualifiers.protected,
                )?;
            }
            let mut args = vec![Some(Expr::Ident(this.to_owned()))];
            let (actuals, order) = base.constructor_arguments(arguments)?;
            let name = classes::Class::subroutine_name(&base.name, "new");
            for (index, actual) in actuals.into_iter().enumerate() {
                args.push(
                    actual
                        .map(|value| {
                            // The formals start with the object's handle.
                            let mut value = expression_binding::bind_argument(
                                &name,
                                index + 1,
                                value,
                                native,
                                self.tree,
                                &self.dims,
                                self.session,
                            )?;
                            self.rename_expr(&mut value);
                            Ok::<_, AnalyzerError>(value)
                        })
                        .transpose()?,
                );
            }
            body.push(Stmt::Call {
                name,
                args,
                arg_order: std::iter::once(0)
                    .chain(order.into_iter().map(|index| index + 1))
                    .collect(),
            });
        }
        let own = class.base_properties..class.properties.len();
        for (index, property) in own.clone().zip(class.own_properties()) {
            body.extend(self.aggregate_initializers(class, this, index)?);
            // A fixed-size array property holds its elements in a container
            // of its own (see `classes::Property::storage`).
            if let Some(storage) = property.storage.as_deref() {
                let mut value = match property.initializer {
                    Some(initializer) => containers::fixed_assignment(
                        initializer,
                        self.tree,
                        self.session,
                        &self.dims,
                        storage,
                    )?,
                    None => containers::fixed_default(storage),
                };
                self.rename_expr(&mut value);
                body.push(Stmt::Eval(classes::object_call(
                    system_functions::ObjectOp::Store(property.field),
                    vec![Expr::Ident(this.to_owned()), value],
                )));
                continue;
            }
            let Some(initializer) = property.initializer else {
                // Each object has its own synchronization object for each
                // event property (IEEE 1800-2023 15.5).
                if events::is_event(&property.r#type) {
                    body.push(Stmt::Eval(classes::object_call(
                        system_functions::ObjectOp::Store(property.field),
                        vec![Expr::Ident(this.to_owned()), events::new_event()],
                    )));
                }
                continue;
            };
            if let Some(container) = property.r#type.container.as_deref() {
                let mut value = containers::assignment(
                    initializer,
                    self.tree,
                    self.session,
                    &self.dims,
                    container,
                )?;
                self.rename_expr(&mut value);
                body.push(Stmt::Eval(classes::object_call(
                    system_functions::ObjectOp::Store(property.field),
                    vec![Expr::Ident(this.to_owned()), value],
                )));
                continue;
            }
            let mut value = match property.r#type.class() {
                Some(class) => expression_binding::object_constructor(
                    self.tree,
                    self.session,
                    initializer,
                    native,
                    &self.dims,
                    class,
                )?,
                None => None,
            };
            expression_binding::check_chandle_assignment(
                property.r#type.is_chandle() && property.r#type.unpacked_ranges.is_empty(),
                initializer,
                self.tree,
                native,
                &self.dims,
                self.session,
            )?;
            let mut value = match value.take() {
                Some(value) => value,
                None => expression_binding::bind_value(
                    initializer,
                    native,
                    self.tree,
                    self.session,
                    &self.dims,
                    Some(&dimensions_from_type(&property.r#type)),
                )?,
            };
            if property.field.handle == Some(system_functions::HandleProperty::String) {
                value = expression_binding::string_operand(value, &self.dims);
            }
            self.rename_expr(&mut value);
            body.push(Stmt::Eval(classes::object_call(
                system_functions::ObjectOp::Store(property.field),
                vec![Expr::Ident(this.to_owned()), value],
            )));
        }
        body.extend(self.aggregate_initializers(class, this, own.end)?);
        Ok(body)
    }

    /// The initializers of the properties of unpacked structure type that
    /// `class` declares before its property `position`.
    pub(super) fn aggregate_initializers(
        &mut self,
        class: &classes::Class,
        this: &str,
        position: usize,
    ) -> Converted<Vec<Stmt>> {
        let mut body = Vec::new();
        for aggregate in &class.aggregates {
            if aggregate.owner != class.name || aggregate.position != position {
                continue;
            }
            if let Some(initializer) = aggregate.initializer {
                body.extend(self.aggregate_store(
                    aggregate,
                    class,
                    Expr::Ident(this.to_owned()),
                    initializer,
                )?);
            } else if !class.aggregates.iter().any(|parent| {
                parent.owner == aggregate.owner
                    && parent.position == aggregate.position
                    && aggregate.name.starts_with(&format!("{}.", parent.name))
            }) {
                let structure = aggregate
                    .r#type
                    .unpacked_struct()
                    .expect("an aggregate property");
                for leaf in structure.leaves() {
                    let Some(value) = &leaf.default else { continue };
                    let property = class
                        .property(&leaf.name(&aggregate.name))
                        .ok_or_else(|| unsupported("unpacked structure property member"))?;
                    body.push(Stmt::Eval(classes::object_call(
                        system_functions::ObjectOp::Store(property.field),
                        vec![Expr::Ident(this.to_owned()), value.clone()],
                    )));
                }
            }
        }
        Ok(body)
    }
}

pub(super) fn is_function(
    native: &celox_sv_syntax::Declarations,
    id: celox_sv_syntax::SubroutineId,
) -> bool {
    native.subroutine(id).kind == celox_sv_syntax::SubroutineKind::Function
}

/// The methods `methods` selects of class `name` as subroutines; with all
/// of them, an implicit constructor when the class declares none (IEEE
/// 1800-2023 8.7).
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_class(
    name: &str,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    declaration_env: &ConstEnv,
    type_aliases: &TypeAliases,
    item_dimensions: &PackedDimensions,
    literals: &HashMap<String, Expr>,
    state: &mut BodyState<'_>,
    item: &generate::Item,
    methods: ClassMethods,
) -> Converted<Vec<Subroutine>> {
    let static_functions = methods == ClassMethods::StaticFunctions;
    if static_functions {
        let native = tree.declarations();
        if !classes::declared_methods(tree, session, name)?
            .into_iter()
            .any(|(id, is_static)| is_static && is_function(native, id))
        {
            return Ok(Vec::new());
        }
    }
    // A specialization's methods see its parameters (IEEE 1800-2023 8.25).
    let (declaration_env, type_aliases) =
        classes::specializations::scope(session, name, declaration_env, type_aliases);
    let (declaration_env, type_aliases): (SharedEnv, SharedAliases) =
        (declaration_env.into(), type_aliases.into());
    let (item_dimensions, literals) =
        classes::specializations::body_scope(session, name, item_dimensions, literals);
    let (declaration_env, type_aliases, item_dimensions, literals) =
        (&declaration_env, &type_aliases, &item_dimensions, &literals);
    let class = match classes::class(tree, session, name, declaration_env, type_aliases) {
        Ok(class) => class,
        // An interface class that cannot be laid out has no subroutines; a
        // use of its handles reports why (IEEE 1800-2023 8.26).
        Err(AnalyzerError::Unsupported(_)) if classes::is_interface(tree, session, name) => {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error),
    };
    let mut subroutines = Vec::new();
    let mut constructor = false;
    let mut methods = classes::declared_methods(tree, session, name)?;
    // A method whose statics the file names is lowered before the others,
    // which may name them (see `static_references`).
    methods.sort_by_key(|&(id, _)| {
        let name =
            instances::normalize_identifier(&tree.declarations().subroutine(id).name.spelling);
        !static_references::named(tree, &name)
    });
    for (id, is_static) in methods {
        if static_functions && !(is_static && is_function(tree.declarations(), id)) {
            continue;
        }
        constructor |=
            tree.declarations().subroutine(id).kind == celox_sv_syntax::SubroutineKind::Constructor;
        subroutines.push(lower_subroutine(
            id,
            tree,
            session,
            declaration_env,
            type_aliases,
            item_dimensions,
            literals,
            state,
            item,
            Some(&class),
            None,
        )?);
    }
    // The dispatchers, the constructor, `$cast` and the randomization
    // methods take or make an object.
    if static_functions {
        return Ok(subroutines);
    }
    for method in class.dispatched_methods() {
        let targets = classes::dispatch_targets(
            tree,
            session,
            name,
            &method.name,
            declaration_env,
            type_aliases,
        )?;
        subroutines.push(lower_subroutine(
            method.native,
            tree,
            session,
            declaration_env,
            type_aliases,
            item_dimensions,
            literals,
            state,
            item,
            Some(&class),
            Some(&targets),
        )?);
    }
    if !constructor && !class.is_interface {
        let handle = classes::handle_type(tree, session, name)
            .ok_or_else(|| unsupported(format!("class `{name}`")))?;
        let ir_type = super::super::ir_type(
            session,
            &tree.functions,
            scoped_type(handle.clone(), declaration_env),
            &item.env,
        );
        let mut builder = BodyBuilder::new(
            tree,
            session,
            item_dimensions,
            state,
            system_functions::Body::Subroutine,
        );
        builder.automatic = true;
        builder.set_owner(item.name(&classes::Class::subroutine_name(name, "new")));
        let first_local = builder.state.locals.len();
        builder.push_scope();
        builder.enter_class_scope(&class);
        let this = builder.declare(classes::THIS, handle.clone());
        let result = builder.declare("new", handle);
        let base_arguments =
            classes::base_arguments(tree, builder.session, name).unwrap_or_default();
        let body = builder.constructor_prologue(&class, &this, &result, base_arguments)?;
        builder.pop_scope();
        builder.flush_locator_locals();
        let mut subroutine = Subroutine {
            name: item.name(&classes::Class::subroutine_name(name, "new")),
            is_task: false,
            automatic: true,
            automatic_locals: builder.state.locals[first_local..]
                .iter()
                .map(|local| local.name.clone())
                .collect(),
            persistent_locals: Vec::new(),
            return_type: Some(ir_type.clone()),
            params: vec![SubroutineParam {
                name: this,
                source_name: classes::THIS.to_owned(),
                direction: ParamDirection::Input,
                r#type: ir_type,
                default: None,
                leaves: Vec::new(),
            }],
            return_var: Some(result),
            body,
            sensitivity_body: Vec::new(),
        };
        for stmt in &mut subroutine.body {
            qualify_stmt(item, stmt);
            substitute_stmt_constants(tree, builder.session, stmt, declaration_env, literals);
        }
        subroutines.push(subroutine);
    }
    subroutines.push(lower_cast(
        name,
        tree,
        session,
        declaration_env,
        type_aliases,
        item_dimensions,
        state,
        item,
    )?);
    subroutines.extend(randomization::lower(
        &class,
        tree,
        session,
        item_dimensions,
        state,
        item,
    )?);
    Ok(subroutines)
}

/// The formals of [`classes::Class::cast_name`].
pub(in crate::ast) const CAST_DESTINATION: &str = "destination";
pub(in crate::ast) const CAST_SOURCE: &str = "source";

/// The subroutine `$cast` to a variable of class `name` calls: it assigns
/// the source handle to the destination and returns 1 when the source is
/// `null` or its object's class derives from or implements the class, and
/// otherwise returns 0 (IEEE 1800-2023 8.16).
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_cast(
    name: &str,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    declaration_env: &ConstEnv,
    type_aliases: &TypeAliases,
    item_dimensions: &PackedDimensions,
    state: &mut BodyState<'_>,
    item: &generate::Item,
) -> Converted<Subroutine> {
    let handle = classes::handle_type(tree, session, name)
        .ok_or_else(|| unsupported(format!("class `{name}`")))?;
    let ir_handle = super::super::ir_type(
        session,
        &tree.functions,
        scoped_type(handle.clone(), declaration_env),
        &item.env,
    );
    let ir_int = super::super::ir_type(session, &tree.functions, int_type(), &item.env);
    let mut labels = vec![0];
    labels.extend(classes::cast_targets(
        tree,
        session,
        name,
        declaration_env,
        type_aliases,
    )?);
    let mut builder = BodyBuilder::new(
        tree,
        session,
        item_dimensions,
        state,
        system_functions::Body::Subroutine,
    );
    builder.automatic = true;
    builder.set_owner(item.name(&classes::Class::cast_name(name)));
    let first_local = builder.state.locals.len();
    builder.push_scope();
    let destination = builder.declare(CAST_DESTINATION, handle.clone());
    let source = builder.declare(CAST_SOURCE, handle);
    let result = builder.declare("$cast", int_type());
    builder.pop_scope();
    let assign = |lhs: &str, rhs: Expr| Stmt::Assign {
        lhs: LValue::Ident(lhs.to_owned()),
        rhs,
        nonblocking: false,
    };
    let mut subroutine = Subroutine {
        name: item.name(&classes::Class::cast_name(name)),
        is_task: false,
        automatic: true,
        automatic_locals: builder.state.locals[first_local..]
            .iter()
            .map(|local| local.name.clone())
            .collect(),
        persistent_locals: Vec::new(),
        return_type: Some(ir_int),
        params: vec![
            SubroutineParam {
                name: destination.clone(),
                source_name: CAST_DESTINATION.to_owned(),
                direction: ParamDirection::Inout,
                r#type: ir_handle.clone(),
                default: None,
                leaves: Vec::new(),
            },
            SubroutineParam {
                name: source.clone(),
                source_name: CAST_SOURCE.to_owned(),
                direction: ParamDirection::Input,
                r#type: ir_handle,
                default: None,
                leaves: Vec::new(),
            },
        ],
        return_var: Some(result.clone()),
        body: vec![Stmt::Case {
            kind: CaseKind::Exact,
            selector: classes::object_call(
                system_functions::ObjectOp::ClassOf,
                vec![Expr::Ident(source.clone())],
            ),
            items: vec![CaseItemBase {
                labels: labels
                    .iter()
                    .map(|id| CaseLabel::Value(Expr::Literal(format!("32'd{id}"))))
                    .collect(),
                body: vec![
                    assign(&destination, Expr::Ident(source)),
                    assign(&result, Expr::Literal("1".into())),
                ],
            }],
            default: Some(vec![assign(&result, Expr::Literal("0".into()))]),
        }],
        sensitivity_body: Vec::new(),
    };
    for stmt in &mut subroutine.body {
        qualify_stmt(item, stmt);
    }
    Ok(subroutine)
}
