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
                        if path.iter().any(|part| part.arguments.is_some()) {
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
                            if let Some(class) = super::super::classes::handle_type(tree, &name) {
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
