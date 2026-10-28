//! Datatype semantics scheduled exclusively by authored native handles.
use super::*;
use celox_sv_syntax::{Dimension, DimensionKind, ExprId, ExprKind};

pub(in crate::ast) fn bind(
    root: TypeId,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<Option<Type>> {
    bind_in(root, tree.declarations(), tree, context)
}

pub(in crate::ast) fn bind_in(
    root: TypeId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<Option<Type>> {
    bind_in_mode(root, native, tree, context, BindingMode::Use)
}

/// Typedef discovery precedes enum constant resolution, so dependent aliases
/// may still have provisional widths. The initial pass can also wait for
/// enclosing constants before specializing a class default.
pub(in crate::ast) fn bind_alias_in(
    root: TypeId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
    defer_defaults: bool,
) -> Converted<Option<Type>> {
    bind_in_mode(
        root,
        native,
        tree,
        context,
        BindingMode::Alias { defer_defaults },
    )
}

#[derive(Clone, Copy)]
enum BindingMode {
    Use,
    Alias { defer_defaults: bool },
}

impl BindingMode {
    fn specialize(
        self,
        tree: &AnalysisContext<'_>,
        generic: &str,
        arguments: &[celox_sv_syntax::Argument],
        context: &PackedDimensions,
    ) -> Converted<Option<String>> {
        if matches!(
            self,
            Self::Alias {
                defer_defaults: true
            }
        ) {
            super::super::classes::specializations::specialize_if_ready(
                tree, generic, arguments, context,
            )
        } else {
            super::super::classes::specializations::specialize(tree, generic, arguments, context)
                .map(Some)
        }
    }
}

fn bind_in_mode(
    root: TypeId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
    mode: BindingMode,
) -> Converted<Option<Type>> {
    enum Job {
        Enter(TypeId),
        Void,
        Enum(TypeId, Vec<PackedRange>),
        Aggregate {
            owner: Arc<str>,
            union: bool,
            soft: bool,
            tagged: bool,
            voids: Vec<bool>,
            signed: bool,
            names: Vec<String>,
            ranges: Vec<PackedRange>,
        },
    }
    let mut jobs = vec![Job::Enter(root)];
    let mut values = Vec::<Type>::new();
    while let Some(job) = jobs.pop() {
        match job {
            Job::Void => values.push(Type::new(TypeKind::Bit)),
            Job::Enter(id) => {
                let data = native.data_type(id);
                let ranges = if matches!(data.kind, DataTypeKind::Struct { .. }) {
                    bind_ranges(&data.packed_dimensions, tree, context)?
                } else {
                    packed_ranges(&data.packed_dimensions, tree, context)?
                };
                match &data.kind {
                    DataTypeKind::Implicit => {
                        let mut ty = Type::implicit();
                        ty.is_signed = data.signed.unwrap_or(false);
                        ty.packed_ranges = ranges;
                        values.push(ty);
                    }
                    DataTypeKind::Builtin(name) => {
                        let Some(mut ty) = builtin_type(name, data.signed) else {
                            return Ok(None);
                        };
                        if ty.packed_ranges.is_empty() {
                            ty.packed_ranges = ranges;
                        }
                        values.push(ty);
                    }
                    DataTypeKind::Named(path) => {
                        if let Some(ty) = super::super::processes::qualified_type(path) {
                            values.push(types::enclose_aliased_packed_type(ty, ranges));
                            continue;
                        }
                        // A class with a parameter value assignment, or one
                        // whose defaults the name alone refers to (IEEE
                        // 1800-2023 8.25).
                        if let [part] = path.as_slice() {
                            let name = normalize_identifier(&part.name.spelling);
                            if (part.arguments.is_some()
                                || !context.type_aliases.contains_key(&name))
                                && let Some(name) = super::super::classes::resolve(tree, &name)
                                && super::super::classes::is_parameterized(tree, &name)
                            {
                                let Some(name) = mode.specialize(
                                    tree,
                                    &name,
                                    part.arguments.as_deref().unwrap_or_default(),
                                    context,
                                )?
                                else {
                                    return Ok(None);
                                };
                                values.push(super::super::classes::handle(&name));
                                continue;
                            }
                        }
                        if let [package, class, member] = path.as_slice() {
                            let name = scope::qualified_name(
                                &tree.state,
                                &normalize_identifier(&package.name.spelling),
                                &normalize_identifier(&class.name.spelling),
                            );
                            if let Some(class) =
                                class_scope_named(class, tree, context, mode, name)?
                            {
                                let member = normalize_identifier(&member.name.spelling);
                                let ty = super::super::classes::specializations::member_type(
                                    tree, &class, &member,
                                )
                                .ok_or_else(|| {
                                    unsupported(format!("type `{member}` of class `{class}`"))
                                })?;
                                values.push(types::enclose_aliased_packed_type(ty, ranges));
                                continue;
                            }
                        }
                        // A type that a class declares, `C::T` (IEEE 1800-2023
                        // 8.23, 8.25.1).
                        if let [scope, member] = path.as_slice()
                            && let Some(class) = class_scope(scope, tree, context, mode)?
                        {
                            let member = normalize_identifier(&member.name.spelling);
                            let ty = super::super::classes::specializations::member_type(
                                tree, &class, &member,
                            )
                            .ok_or_else(|| {
                                unsupported(format!("type `{member}` of class `{class}`"))
                            })?;
                            values.push(types::enclose_aliased_packed_type(ty, ranges));
                            continue;
                        }
                        if path.iter().any(|part| part.arguments.is_some()) {
                            // A specialization is made where the class is
                            // declared, from its declaration.
                            let imported = scope::imported(&tree.state);
                            let class = match path.as_slice() {
                                [part] => imported
                                    .aliases
                                    .get(&normalize_identifier(&part.name.spelling))
                                    .cloned(),
                                [package, member] => Some(scope::qualified_name(
                                    &tree.state,
                                    &normalize_identifier(&package.name.spelling),
                                    &normalize_identifier(&member.name.spelling),
                                )),
                                _ => None,
                            };
                            if let Some(class) = class
                                .as_ref()
                                .filter(|class| tree.state.foreign.contains(class))
                            {
                                let arguments = path
                                    .last()
                                    .and_then(|part| part.arguments.as_deref())
                                    .unwrap_or_default();
                                let Some(name) =
                                    mode.specialize(tree, class, arguments, context)?
                                else {
                                    return Ok(None);
                                };
                                values.push(types::enclose_aliased_packed_type(
                                    super::super::classes::handle(&name),
                                    ranges,
                                ));
                                continue;
                            }
                            if let Some(class) = class.filter(|class| {
                                imported
                                    .classes
                                    .iter()
                                    .any(|known| known.name.starts_with(&format!("{class}#(")))
                                    || imported.aliases.values().any(|known| known == class)
                            }) {
                                return Err(unsupported(format!(
                                    "specialization of parameterized class `{class}` outside the scope that declares it"
                                )));
                            }
                            return Ok(None);
                        }
                        let name = match path.as_slice() {
                            [part] => normalize_identifier(&part.name.spelling),
                            [package, member] => scope::qualified_name(
                                &tree.state,
                                &normalize_identifier(&package.name.spelling),
                                &normalize_identifier(&member.name.spelling),
                            ),
                            _ => return Ok(None),
                        };
                        let Some(ty) = context.type_aliases.get(&name) else {
                            // Inside its package, `p::C` is a name of the
                            // package itself, whose classes are declared
                            // under their qualified names.
                            let class = match path.as_slice() {
                                [_] => super::super::classes::resolve(tree, &name),
                                _ => super::super::classes::handle_type(tree, &name)
                                    .map(|_| name.clone())
                                    .or_else(|| super::super::classes::resolve(tree, &name)),
                            };
                            if let Some(class) = class
                                .and_then(|class| super::super::classes::handle_type(tree, &class))
                            {
                                values.push(class);
                                continue;
                            }
                            return Ok(None);
                        };
                        values.push(types::enclose_aliased_packed_type(ty.clone(), ranges));
                    }
                    DataTypeKind::Enum { base, .. } => {
                        if let Some(base) = base {
                            jobs.push(Job::Enum(id, ranges));
                            jobs.push(Job::Enter(*base));
                        } else {
                            let mut ty = builtin_type("int", None).unwrap();
                            ty.is_enum = true;
                            if matches!(mode, BindingMode::Use) {
                                validate_enum_initializers(id, native, tree, context, &ty)?;
                            }
                            ty.enum_values = enum_values(id, native, tree, context, &ty);
                            ty.packed_ranges = ranges.into_iter().chain(ty.packed_ranges).collect();
                            values.push(ty);
                        }
                    }
                    DataTypeKind::Struct {
                        union,
                        soft,
                        tagged,
                        packed,
                        fields,
                    } => {
                        if !packed && !soft {
                            return Ok(None);
                        }
                        let mut names = Vec::new();
                        let mut voids = Vec::new();
                        let mut seen = HashSet::default();
                        for field in fields {
                            if field.random.is_some()
                                || !field.dimensions.is_empty()
                                || field.default.is_some()
                            {
                                return Ok(None);
                            }
                            let name = normalize_identifier(&field.name.spelling);
                            if !seen.insert(name.clone()) {
                                return Err(unsupported(format!(
                                    "duplicate aggregate field `{name}`"
                                )));
                            }
                            let is_void = matches!(&native.data_type(field.data_type).kind, DataTypeKind::Builtin(name) if name == "void");
                            if is_void && !tagged {
                                return Ok(None);
                            }
                            voids.push(is_void);
                            names.push(name);
                        }
                        let token = &native.tokens[data.tokens.start];
                        let span = token.origin.spelling;
                        let owner = Arc::from(format!(
                            "{}:{}:{}:{}",
                            tree.syntax_tree.root_name().unwrap_or_default(),
                            native.sources.file(span.file).path.display(),
                            span.start,
                            data.tokens.start
                        ));
                        jobs.push(Job::Aggregate {
                            owner,
                            union: *union,
                            soft: *soft,
                            tagged: *tagged,
                            voids: voids.clone(),
                            signed: data.signed.unwrap_or(false),
                            names,
                            ranges,
                        });
                        jobs.extend(fields.iter().zip(voids).rev().map(|(field, is_void)| {
                            if is_void {
                                Job::Void
                            } else {
                                Job::Enter(field.data_type)
                            }
                        }));
                    }
                    DataTypeKind::TypeOf(expression) => {
                        // The existing query kernel recognizes an explicit
                        // nested datatype; it does not infer arbitrary values.
                        let inner = match native.expression(*expression).kind {
                            ExprKind::Type(inner) => inner,
                            ExprKind::Cast { target, .. } => match native.expression(target).kind {
                                ExprKind::Type(inner)
                                    if matches!(
                                        native.data_type(inner).kind,
                                        DataTypeKind::Builtin(_) | DataTypeKind::Struct { .. }
                                    ) =>
                                {
                                    inner
                                }
                                _ => return Ok(None),
                            },
                            _ => return Ok(None),
                        };
                        jobs.push(Job::Enter(inner));
                    }
                    DataTypeKind::Interface { .. } | DataTypeKind::VirtualInterface { .. } => {
                        return Ok(None);
                    }
                }
            }
            Job::Enum(id, ranges) => {
                let Some(mut ty) = values.pop() else {
                    return Err(unsupported("enum base type"));
                };
                if matches!(mode, BindingMode::Use) {
                    validate_enum_initializers(id, native, tree, context, &ty)?;
                }
                let mut outer = ranges;
                outer.extend(ty.packed_ranges);
                ty.packed_ranges = outer;
                ty.is_enum = true;
                ty.enum_values = enum_values(id, native, tree, context, &ty);
                values.push(ty);
            }
            Job::Aggregate {
                owner,
                union,
                soft,
                tagged,
                voids,
                signed,
                names,
                ranges,
            } => {
                let start = values
                    .len()
                    .checked_sub(names.len())
                    .ok_or_else(|| unsupported("aggregate fields"))?;
                let fields = names
                    .into_iter()
                    .zip(values.split_off(start))
                    .zip(voids)
                    .map(|((name, ty), is_void)| (name, ty, is_void))
                    .collect();
                let Some(ty) = packed_structs::native_type(
                    &tree.state,
                    if tagged {
                        packed_structs::AggregateKind::TaggedUnion
                    } else if soft {
                        packed_structs::AggregateKind::SoftUnion
                    } else if union {
                        packed_structs::AggregateKind::HardUnion
                    } else {
                        packed_structs::AggregateKind::Struct
                    },
                    signed,
                    owner,
                    fields,
                    ranges,
                    &context.const_env,
                ) else {
                    return Ok(None);
                };
                values.push(ty);
            }
        }
    }
    Ok(values.pop())
}

pub(in crate::ast) fn packed_ranges(
    dimensions: &[Dimension],
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<Vec<PackedRange>> {
    if dimensions.is_empty() {
        return Ok(Vec::new());
    }
    // Preserve the existing width contract for datatype-bearing bounds: the
    // historical primitive/alias kernel includes nested datatype dimensions.
    // Aggregate fields use their own independently scheduled type ranges.
    let mut all = dimensions.to_vec();
    let bounds = dimensions
        .iter()
        .map(|dimension| dimension.tokens)
        .collect::<Vec<_>>();
    for id in tree.spans().types.within(&bounds) {
        let data = tree.declarations().data_type(id);
        if dimensions.iter().any(|bound| {
            bound.tokens.start <= data.tokens.start && data.tokens.end <= bound.tokens.end
        }) {
            all.extend(data.packed_dimensions.iter().cloned());
        }
    }
    all.sort_by_key(|dimension| dimension.tokens.start);
    all.dedup_by_key(|dimension| dimension.tokens);
    bind_ranges(&all, tree, context)
}

fn bind_ranges(
    dimensions: &[Dimension],
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<Vec<PackedRange>> {
    let mut result = Vec::new();
    for dimension in dimensions {
        let DimensionKind::Range { left, right } = dimension.kind else {
            return Err(unsupported("unsized packed dimension"));
        };
        let left = constant_binding::bind(left, tree, context)?;
        let right = constant_binding::bind(right, tree, context)?;
        if let (Some(left), Some(right)) = (left, right) {
            result.push(PackedRange::new(left, right));
        }
    }
    Ok(result)
}

/// The class that `scope` of a class-scoped type `C::T` names: inside a
/// parameterized class its name alone refers to the specialization, and
/// elsewhere a parameterized class needs one (IEEE 1800-2023 8.25.1).
fn class_scope(
    scope: &celox_sv_syntax::TypeSegment,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
    mode: BindingMode,
) -> Converted<Option<String>> {
    class_scope_named(
        scope,
        tree,
        context,
        mode,
        normalize_identifier(&scope.name.spelling),
    )
}

fn class_scope_named(
    scope: &celox_sv_syntax::TypeSegment,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
    mode: BindingMode,
    name: String,
) -> Converted<Option<String>> {
    if scope.arguments.is_none()
        && let Some(class) = context
            .type_aliases
            .get(&super::super::classes::specializations::scope_key(&name))
            .or_else(|| {
                // A class of a package is declared under its qualified name.
                let class = super::super::classes::resolve(tree, &name)?;
                context
                    .type_aliases
                    .get(&super::super::classes::specializations::scope_key(&class))
            })
            .or_else(|| context.type_aliases.get(&name))
            .and_then(|ty| ty.class().map(str::to_owned))
    {
        return Ok(Some(class));
    }
    let Some(class) = super::super::classes::resolve(tree, &name) else {
        return Ok(None);
    };
    // Resolve a user package before std's implicit class fallback for this
    // qualified type; explicit std::process remains unambiguous (26.7).
    if class == super::super::processes::CLASS
        && scope::imported(&tree.state).package_scopes.contains(&name)
    {
        return Ok(None);
    }
    let name = class;
    if !super::super::classes::is_parameterized(tree, &name) {
        if scope
            .arguments
            .as_ref()
            .is_some_and(|arguments| !arguments.is_empty())
        {
            return Err(unsupported(format!(
                "parameter value assignment of class `{name}`, which has no parameters"
            )));
        }
        // Its members are those of its one specialization.
        if super::super::classes::handle_type(tree, &name).is_some()
            && super::super::classes::specializations::lookup(tree, &name).is_none()
            && super::super::classes::is_declared(tree, &name)
            && mode.specialize(tree, &name, &[], context)?.is_none()
        {
            return Ok(None);
        }
        return Ok(Some(name));
    }
    let Some(arguments) = &scope.arguments else {
        return Err(AnalyzerError::UnspecializedClassScope { class: name });
    };
    mode.specialize(tree, &name, arguments, context)
}

pub(in crate::ast) fn expression_type(
    root: ExprId,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<Option<ExprType>> {
    if let ExprKind::Type(id) = tree.declarations().expression(root).kind {
        return Ok(bind(id, tree, context)?
            .and_then(|ty| expr_type_from_type(&tree.state, &ty, &context.const_env)));
    }
    if let Some((name, ranges)) = argument_alias(root, tree.declarations())
        && context.get(&name).is_none()
        && !context.const_env.contains_key(&name)
        && !context
            .const_env
            .contains_key(&parameter_width_marker(&name))
        && !context.parameter_values.contains_key(&name)
        && let Some(ty) = context.type_aliases.get(&name)
    {
        let ty =
            types::enclose_aliased_packed_type(ty.clone(), packed_ranges(&ranges, tree, context)?);
        return Ok(expr_type_from_type(&tree.state, &ty, &context.const_env));
    }
    let mut query = context.clone();
    query.expression_signedness.extend(
        context
            .iter()
            .map(|(name, shape)| (name.clone(), shape.signed)),
    );
    query.type_query = true;
    query.functions = Arc::default();
    let value = expression_binding::bind(root, tree, &query)?;
    Ok(dimensions::bound_bits_type(&tree.state, &value, &query))
}

/// IEEE 1800-2023 6.19 imposes declaration rules even when no enum member
/// is used. Keep these checks separate from best-effort randomization metadata.
pub(in crate::ast) fn validate_enum_initializers(
    id: TypeId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
    ty: &Type,
) -> Converted<()> {
    let DataTypeKind::Enum { base, items } = &native.data_type(id).kind else {
        return Ok(());
    };
    let Some(shape) = casts::expr_type_from_type(&tree.state, ty, &context.const_env) else {
        return Ok(());
    };
    let mut env = context.const_env.clone();
    let mut unknown_literals = HashMap::default();
    let mut previous_unknown = false;
    let mut next = Some(0i128);
    for item in items {
        let name = normalize_identifier(&item.name.spelling);
        let Some(value) = item.value else {
            if previous_unknown {
                return Err(unsupported(format!(
                    "enum member `{name}` cannot implicitly increment an X/Z value"
                )));
            }
            if !item.dimensions.is_empty() {
                next = None;
                continue;
            }
            if let Some(value) = next {
                env.insert(name.clone(), value);
                dimensions::insert_parameter_type_markers(&mut env, &name, shape);
                next = value.checked_add(1);
            }
            continue;
        };
        // Check authored syntax, not a folded expression: a parameter, cast,
        // or arithmetic expression is not a sized literal constant.
        let mut literal_id = value;
        while let ExprKind::Group(inner) = native.expression(literal_id).kind {
            literal_id = inner;
        }
        if let ExprKind::Literal(text) = &native.expression(literal_id).kind
            && let Some((size, _)) = text.split_once('\'')
            && !size.is_empty()
            && let Ok(width) = size.replace('_', "").parse::<usize>()
            && width != shape.width
        {
            return Err(unsupported(format!(
                "enum member `{name}` sized literal width {width} differs from base width {}",
                shape.width
            )));
        }
        // During signature discovery some constants are still unresolved.
        // Leave their resolution to the ordinary constant-binding pass.
        let expr = constant_binding::expression(value, native, tree, &env, &context.type_aliases)
            .ok()
            .flatten()
            .and_then(|expr| {
                expr_to_const(
                    &tree.state,
                    constants::substitute_expr_constants_with_parameter_literals(
                        &tree.state,
                        const_expr_to_expr(expr),
                        &env,
                        &unknown_literals,
                    ),
                )
            });
        let expr = expr.map(|expr| match expr {
            ConstExpr::Literal(text) => ConstExpr::Literal(
                resize_unbased_fill_literal_for_cast(&text, shape.width, shape.signed)
                    .unwrap_or(text),
            ),
            expr => expr,
        });
        let literal = expr.as_ref().and_then(|expr| {
            typecheck::eval_const_integral_literal_with_lookup(
                &expr.clone().into(),
                &env,
                &|name| {
                    dimensions::parameter_type_from_const_env(&env, name)
                        .map(|ty| (ty.width, ty.signed))
                },
            )
        });
        previous_unknown = literal
            .as_ref()
            .is_some_and(|value| value.mask != Default::default());
        if previous_unknown && (base.is_none() || ty.kind == TypeKind::Bit) {
            return Err(unsupported(format!(
                "enum member `{name}` X/Z value requires an explicit four-state base type"
            )));
        }
        if !item.dimensions.is_empty() {
            next = None;
            continue;
        }
        if previous_unknown {
            // Preserve masks when a later enumerator refers to this member.
            // Numeric constant environments cannot represent X/Z values.
            unknown_literals.insert(
                name.clone(),
                Expr::Literal(resize_integral_literal_for_cast(
                    literal.unwrap(),
                    shape.width,
                    shape.signed,
                )),
            );
            env.remove(&name);
        }
        next = expr
            .as_ref()
            .and_then(|expr| eval_ast_const_expr(&tree.state, expr, &env))
            .map(|value| {
                parameters::coerce_const_parameter_value(value, shape.width, shape.signed)
            });
        if let Some(value) = next {
            env.insert(name.clone(), value);
            dimensions::insert_parameter_type_markers(&mut env, &name, shape);
            next = value.checked_add(1);
        }
    }
    Ok(())
}

/// Domain metadata is best-effort during signature discovery. Only a caller
/// requesting randomization requires a resolved domain; unused types must not
/// start reporting new errors merely because this metadata is collected.
fn enum_values(
    id: TypeId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
    ty: &Type,
) -> Option<Vec<i128>> {
    let data = native.data_type(id);
    if !data.packed_dimensions.is_empty() {
        return None;
    }
    let DataTypeKind::Enum { items, .. } = &data.kind else {
        return None;
    };
    let shape = casts::expr_type_from_type(&tree.state, ty, &context.const_env)?;
    if shape.width > 64 {
        return None;
    }
    let mut env = context.const_env.clone();
    let mut next = 0i128;
    let mut values = Vec::new();
    for item in items {
        if !item.dimensions.is_empty() {
            return None;
        }
        let value = if let Some(id) = item.value {
            let expr = constant_binding::expression(id, native, tree, &env, &context.type_aliases)
                .ok()??;
            // Unbased fills take the enum base's width and signedness, just
            // as they do when resolving the declared enum constants (5.7.1).
            let expr = match expr {
                ConstExpr::Literal(literal) => ConstExpr::Literal(
                    resize_unbased_fill_literal_for_cast(&literal, shape.width, shape.signed)
                        .unwrap_or(literal),
                ),
                expr => expr,
            };
            let value = eval_ast_const_expr(&tree.state, &expr, &env)?;
            let value_type = infer_const_expr_type(&expr, &parameter_types_from_const_env(&env))?;
            if !parameters::enum_initializer_fits_base_type(value, value_type, shape) {
                return None;
            }
            value
        } else {
            if parameters::coerce_const_parameter_value(next, shape.width, shape.signed) != next {
                return None;
            }
            next
        };
        let value = parameters::coerce_const_parameter_value(value, shape.width, shape.signed);
        if values.contains(&value) {
            return None;
        }
        values.push(value);
        let name = normalize_identifier(&item.name.spelling);
        env.insert(name.clone(), value);
        dimensions::insert_parameter_type_markers(&mut env, &name, shape);
        next = value.checked_add(1)?;
    }
    (!values.is_empty()).then_some(values)
}
