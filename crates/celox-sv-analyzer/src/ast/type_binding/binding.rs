//! Datatype semantics scheduled exclusively by authored native handles.
use super::*;
use celox_sv_syntax::{Dimension, DimensionKind, ExprId, ExprKind};

pub(in crate::ast) fn bind(
    root: TypeId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    context: &PackedDimensions,
) -> Converted<Option<Type>> {
    bind_in(root, tree.declarations(), tree, session, context)
}

pub(in crate::ast) fn bind_in(
    root: TypeId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    context: &PackedDimensions,
) -> Converted<Option<Type>> {
    bind_in_mode(root, native, tree, session, context, BindingMode::Use)
}

/// Typedef discovery precedes enum constant resolution, so dependent aliases
/// may still have provisional widths. The initial pass can also wait for
/// enclosing constants before specializing a class default.
pub(in crate::ast) fn bind_alias_in(
    root: TypeId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    context: &PackedDimensions,
    defer_defaults: bool,
) -> Converted<Option<Type>> {
    bind_in_mode(
        root,
        native,
        tree,
        session,
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
        session: &mut Session,
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
                tree, session, generic, arguments, context,
            )
        } else {
            super::super::classes::specializations::specialize(
                tree, session, generic, arguments, context,
            )
            .map(Some)
        }
    }
}

// Keep member-array binding temporaries off the recursive type-binding frame.
fn append_member_dimensions(
    values: &mut Vec<Type>,
    dimensions: &[Dimension],
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    context: &PackedDimensions,
) -> Converted<()> {
    // A dynamic array, queue or associative array member (7.2.1, 7.4): the
    // member is a container, which the structure's flat value holds by its
    // handle.
    if dimensions.first().is_some_and(|dimension| {
        matches!(
            dimension.kind,
            DimensionKind::Unsized | DimensionKind::Queue(_)
        ) || matches!(
            dimension.kind,
            DimensionKind::Index(id) if matches!(
                native.expression(id).kind,
                celox_sv_syntax::ExprKind::Type(_)
            ) || matches!(
                &native.expression(id).kind,
                celox_sv_syntax::ExprKind::Name(name)
                    if context.type_aliases.contains_key(&instances::normalize_identifier(&name.spelling))
            )
        )
    }) {
        let ty = values
            .pop()
            .ok_or_else(|| unsupported("structure member type"))?;
        values.push(containers::with_dimensions(
            ty, dimensions, tree, session, context,
        )?);
        return Ok(());
    }
    let mut bound = |id| {
        constant_binding::expression(
            id,
            native,
            tree,
            session,
            &context.const_env,
            &context.type_aliases,
        )?
        .ok_or_else(|| unsupported("unresolved structure member array dimension"))
    };
    let ranges = dimensions
        .iter()
        .map(|dimension| match dimension.kind {
            DimensionKind::Range { left, right } => {
                Ok(UnpackedRange::new(bound(left)?, bound(right)?))
            }
            DimensionKind::Index(size) => Ok(UnpackedRange::sized(bound(size)?)),
            _ => Err(unsupported("non-fixed structure member array dimension")),
        })
        .collect::<Converted<Vec<_>>>()?;
    types::validate_unpacked_dimension_sizes(&tree.functions, &ranges, &context.const_env)?;
    let ty = values
        .pop()
        .ok_or_else(|| unsupported("structure member type"))?;
    values.push(type_with_unpacked_ranges(ty, ranges)?);
    Ok(())
}

fn bind_in_mode(
    root: TypeId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    context: &PackedDimensions,
    mode: BindingMode,
) -> Converted<Option<Type>> {
    enum Job {
        Enter(TypeId),
        Void,
        MemberDimensions(Vec<celox_sv_syntax::Dimension>),
        Enum(TypeId, Vec<PackedRange>),
        Aggregate {
            owner: Arc<str>,
            scope: Option<Arc<str>>,
            union: bool,
            soft: bool,
            tagged: bool,
            voids: Vec<bool>,
            signed: bool,
            names: Vec<String>,
            defaults: Vec<Option<celox_sv_syntax::ExprId>>,
            ranges: Vec<PackedRange>,
            packed: bool,
        },
    }
    let mut jobs = vec![Job::Enter(root)];
    let mut values = Vec::<Type>::new();
    while let Some(job) = jobs.pop() {
        match job {
            Job::Void => values.push(Type::new(TypeKind::Bit)),
            Job::MemberDimensions(dimensions) => {
                append_member_dimensions(&mut values, &dimensions, native, tree, session, context)?;
            }
            Job::Enter(id) => {
                let data = native.data_type(id);
                let ranges = if matches!(data.kind, DataTypeKind::Struct { .. }) {
                    bind_ranges(&data.packed_dimensions, tree, session, context)?
                } else {
                    packed_ranges(&data.packed_dimensions, tree, session, context)?
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
                        if ty.is_chandle() {
                            session.types.declare_chandle();
                        }
                        if ty.packed_ranges.is_empty() {
                            ty.packed_ranges = ranges;
                        }
                        values.push(ty);
                    }
                    DataTypeKind::Named(path) => {
                        if let Some(ty) =
                            super::super::processes::builtin_type(path, tree, session, context)?
                        {
                            values.push(types::enclose_aliased_packed_type(*ty, ranges));
                            continue;
                        }
                        // A class with a parameter value assignment, or one
                        // whose defaults the name alone refers to (IEEE
                        // 1800-2023 8.25).
                        if let [part] = path.as_slice() {
                            let name = normalize_identifier(&part.name.spelling);
                            if (part.arguments.is_some()
                                || !context.type_aliases.contains_key(&name))
                                && let Some(name) =
                                    super::super::classes::resolve(tree, session, &name)
                                && super::super::classes::is_parameterized(tree, session, &name)
                            {
                                let Some(name) = mode.specialize(
                                    tree,
                                    session,
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
                                tree,
                                &normalize_identifier(&package.name.spelling),
                                &normalize_identifier(&class.name.spelling),
                            );
                            if let Some(class) =
                                class_scope_named(class, tree, session, context, mode, name)?
                            {
                                let member = normalize_identifier(&member.name.spelling);
                                let ty = super::super::classes::specializations::member_type(
                                    tree, session, &class, &member,
                                )
                                .ok_or_else(|| member_type_error(tree, session, &class, &member))?;
                                values.push(types::enclose_aliased_packed_type(ty, ranges));
                                continue;
                            }
                        }
                        // A type that a class declares, `C::T` (IEEE 1800-2023
                        // 8.23, 8.25.1).
                        if let [scope, member] = path.as_slice()
                            && let Some(class) = class_scope(scope, tree, session, context, mode)?
                        {
                            let member = normalize_identifier(&member.name.spelling);
                            let ty = super::super::classes::specializations::member_type(
                                tree, session, &class, &member,
                            )
                            .ok_or_else(|| member_type_error(tree, session, &class, &member))?;
                            values.push(types::enclose_aliased_packed_type(ty, ranges));
                            continue;
                        }
                        if path.iter().any(|part| part.arguments.is_some()) {
                            // A specialization is made where the class is
                            // declared, from its declaration.
                            let imported = scope::imported(tree);
                            let class = match path.as_slice() {
                                [part] => imported
                                    .aliases
                                    .get(&normalize_identifier(&part.name.spelling))
                                    .cloned(),
                                [package, member] => Some(scope::qualified_name(
                                    tree,
                                    &normalize_identifier(&package.name.spelling),
                                    &normalize_identifier(&member.name.spelling),
                                )),
                                _ => None,
                            };
                            if let Some(class) = class
                                .as_ref()
                                .filter(|class| session.foreign.contains(class))
                            {
                                let arguments = path
                                    .last()
                                    .and_then(|part| part.arguments.as_deref())
                                    .unwrap_or_default();
                                let Some(name) =
                                    mode.specialize(tree, session, class, arguments, context)?
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
                            return unresolved_specialization(
                                tree,
                                session,
                                context,
                                path,
                                use_site(native, tree, data),
                            );
                        }
                        let name = match path.as_slice() {
                            [part] => normalize_identifier(&part.name.spelling),
                            [package, member] => scope::qualified_name(
                                tree,
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
                                [_] => super::super::classes::resolve(tree, session, &name),
                                _ => super::super::classes::handle_type(tree, session, &name)
                                    .map(|_| name.clone())
                                    .or_else(|| {
                                        super::super::classes::resolve(tree, session, &name)
                                    }),
                            };
                            if let Some(class) = class.and_then(|class| {
                                super::super::classes::handle_type(tree, session, &class)
                            }) {
                                values.push(class);
                                continue;
                            }
                            // A name nothing visible declares is no type
                            // (IEEE 1800-2023 6.18); one that names a
                            // declaration the binder does not model, such as
                            // a typedef it cannot bind yet, is not known.
                            return unresolved_type_name(
                                tree,
                                context,
                                &name,
                                path,
                                use_site(native, tree, data),
                            );
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
                                validate_enum_initializers(
                                    id, native, tree, session, context, &ty,
                                )?;
                            }
                            ty.enum_domain = enum_values(id, native, tree, session, context, &ty);
                            ty.packed_enum = !native.data_type(id).packed_dimensions.is_empty();
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
                        // Unpacked unions are not supported (IEEE 1800-2023 7.3).
                        if !packed && !soft && (*union || *tagged) {
                            return Ok(None);
                        }
                        let mut names = Vec::new();
                        let mut voids = Vec::new();
                        let mut seen = HashSet::default();
                        for field in fields {
                            if field.random.is_some()
                                || !field.dimensions.is_empty() && (*packed || *soft || *union)
                                || field.default.is_some() && (*packed || *soft || *union)
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
                            scope: types::unit_scope(
                                classes::enclosing_unit(native, data.tokens),
                                session,
                            ),
                            union: *union,
                            soft: *soft,
                            tagged: *tagged,
                            voids: voids.clone(),
                            signed: data.signed.unwrap_or(false),
                            names,
                            defaults: fields.iter().map(|field| field.default).collect(),
                            ranges,
                            packed: *packed || *soft,
                        });
                        for (field, is_void) in fields.iter().zip(voids).rev() {
                            if is_void {
                                jobs.push(Job::Void);
                            } else {
                                if !field.dimensions.is_empty() {
                                    jobs.push(Job::MemberDimensions(field.dimensions.clone()));
                                }
                                jobs.push(Job::Enter(field.data_type));
                            }
                        }
                    }
                    DataTypeKind::TypeOf(expression) => {
                        let inner = match &native.expression(*expression).kind {
                            ExprKind::Type(inner) => Some(*inner),
                            ExprKind::Cast { target, .. }
                                if matches!(
                                    native.expression(*target).kind,
                                    ExprKind::Type(inner)
                                        if matches!(
                                            native.data_type(inner).kind,
                                            DataTypeKind::Builtin(_) | DataTypeKind::Struct { .. }
                                        )
                                ) =>
                            {
                                let ExprKind::Type(inner) = native.expression(*target).kind else {
                                    unreachable!("a cast to a data type")
                                };
                                Some(inner)
                            }
                            // A type alias `type(T)`, or an expression,
                            // including a size or signing cast.
                            _ => {
                                if !push_operand_type(
                                    &mut values,
                                    ranges,
                                    *expression,
                                    native,
                                    tree,
                                    session,
                                    context,
                                ) {
                                    return Ok(None);
                                }
                                continue;
                            }
                        };
                        if let Some(inner) = inner {
                            jobs.push(Job::Enter(inner));
                        }
                    }
                    DataTypeKind::VirtualInterface {
                        name,
                        arguments,
                        modport,
                    } => {
                        // A virtual interface names an interface (IEEE
                        // 1800-2023 25.9).
                        values.push(super::super::virtual_interfaces::bind_type(
                            session,
                            name,
                            arguments.as_deref(),
                            modport.as_ref(),
                        )?);
                    }
                    DataTypeKind::Interface { .. } => return Ok(None),
                }
            }
            Job::Enum(id, ranges) => {
                let Some(mut ty) = values.pop() else {
                    return Err(unsupported("enum base type"));
                };
                if matches!(mode, BindingMode::Use) {
                    validate_enum_initializers(id, native, tree, session, context, &ty)?;
                }
                let mut outer = ranges;
                outer.extend(ty.packed_ranges);
                ty.packed_ranges = outer;
                ty.is_enum = true;
                ty.enum_domain = enum_values(id, native, tree, session, context, &ty);
                ty.packed_enum = !native.data_type(id).packed_dimensions.is_empty();
                values.push(ty);
            }
            Job::Aggregate {
                owner,
                scope,
                union,
                soft,
                tagged,
                voids,
                signed,
                names,
                defaults,
                ranges,
                packed,
            } => {
                let start = values
                    .len()
                    .checked_sub(names.len())
                    .ok_or_else(|| unsupported("aggregate fields"))?;
                if !packed {
                    if !ranges.is_empty() {
                        return Ok(None);
                    }
                    let fields = names
                        .into_iter()
                        .zip(values.split_off(start))
                        .zip(defaults)
                        .map(|((name, ty), default)| (name, ty, default))
                        .collect();
                    let Some(fields) =
                        unpacked_member_defaults(fields, native, tree, session, context, mode)?
                    else {
                        return Ok(None);
                    };
                    let Some(mut ty) = unpacked_structs::native_type(
                        &tree.functions,
                        owner,
                        fields,
                        &context.const_env,
                    ) else {
                        return Ok(None);
                    };
                    if let Some(scope) = scope {
                        ty.spelling.declared_in(scope);
                    }
                    values.push(ty);
                    continue;
                }
                let fields = names
                    .into_iter()
                    .zip(values.split_off(start))
                    .zip(voids)
                    .map(|((name, ty), is_void)| (name, ty, is_void))
                    .collect();
                let Some(mut ty) = packed_structs::native_type(
                    tree,
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
                if !soft && !tagged {
                    ty.spelling =
                        type_names::Spelling::aggregate(if union { "union" } else { "struct" });
                }
                if let Some(scope) = scope {
                    ty.spelling.declared_in(scope);
                }
                values.push(ty);
            }
        }
    }
    // A chandle has its own packed range only; it is no packed array
    // element or enumeration base (IEEE 1800-2023 6.14, 6.19), such as
    // through a typedef. This checks in place, which keeps the recursive
    // frame small.
    if let Some(ty) = values.last()
        && ty.is_chandle()
    {
        if ty.is_enum {
            return types::chandle_enum();
        }
        if ty.packed_ranges.len() > 1 {
            return types::packed_chandles();
        }
    }
    Ok(values.pop())
}

pub(in crate::ast) fn packed_ranges(
    dimensions: &[Dimension],
    tree: &AnalysisContext<'_>,
    session: &mut Session,
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
    bind_ranges(&all, tree, session, context)
}

fn bind_ranges(
    dimensions: &[Dimension],
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    context: &PackedDimensions,
) -> Converted<Vec<PackedRange>> {
    let mut result = Vec::new();
    for dimension in dimensions {
        let DimensionKind::Range { left, right } = dimension.kind else {
            return Err(unsupported("unsized packed dimension"));
        };
        let left = constant_binding::bind(left, tree, session, context)?;
        let right = constant_binding::bind(right, tree, session, context)?;
        if let (Some(left), Some(right)) = (left, right) {
            result.push(PackedRange::new(left, right));
        }
    }
    Ok(result)
}

/// The binding of type name `path`, normalized `name`, that the binder's
/// scoped lookup does not resolve. A single or package-qualified name that
/// no type visible in the scope declares is an error (IEEE 1800-2023 6.18); a declared type
/// without a binding, such as a typedef whose constants are not known yet,
/// has none yet. A type declared in the scope is visible after its
/// declaration, at token `at` of the use. A context that does not record
/// the types visible in its scope tells neither. A tail call, so that the binder's frame, which
/// nested specializations multiply, keeps its size.
#[cold]
#[inline(never)]
fn unresolved_type_name(
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
    name: &str,
    path: &[celox_sv_syntax::TypeSegment],
    at: usize,
) -> Converted<Option<Type>> {
    let Some(declarations) = &context.type_declarations else {
        return Ok(None);
    };
    // `p::T` names a type of package `p`, which the record holds under that
    // qualified name, or under `T` inside `p` itself (IEEE 1800-2023 26.3).
    // `C::T` of a class `C` whose specialization is not known yet names
    // a type of the class, which the record does not hold.
    let spelled = match path {
        [part] => part.name.value().to_owned(),
        [package, member]
            if {
                let package = normalize_identifier(&package.name.spelling);
                scope::imported(tree).package_scopes.contains(&package)
                    && !types::declares_type(declarations, &package, at)
            } =>
        {
            format!("{}::{}", package.name.value(), member.name.value())
        }
        _ => return Ok(None),
    };
    if types::declares_type(declarations, name, at) {
        return Ok(None);
    }
    Err(AnalyzerError::UndeclaredIdentifier {
        name: spelled,
        context: "no type of this name is visible".into(),
    })
}

/// The token of type `data` of `native`, where a declaration of the scope
/// of `tree` must precede it, or past every token when `native` holds
/// another source's declarations.
fn use_site(
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    data: &celox_sv_syntax::DataType,
) -> usize {
    if std::ptr::eq(native, tree.declarations()) {
        data.tokens.start
    } else {
        usize::MAX
    }
}

/// A specialization `C#(...)` no visible class matched: a single name that
/// no visible typedef or class declares is undeclared (IEEE
/// 1800-2023 6.18, 8.25).
#[cold]
#[inline(never)]
fn unresolved_specialization(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    context: &PackedDimensions,
    path: &[celox_sv_syntax::TypeSegment],
    at: usize,
) -> Converted<Option<Type>> {
    if let [part] = path {
        let name = normalize_identifier(&part.name.spelling);
        if !context.type_aliases.contains_key(&name)
            && super::super::classes::resolve(tree, session, &name).is_none()
        {
            return unresolved_type_name(tree, context, &name, path, at);
        }
    }
    Ok(None)
}

/// The error of `C::member` when class `class` has no type `member`: an
/// undeclared name when neither it nor a class it extends declares a
/// typedef or type parameter of that name, and otherwise a type Celox does
/// not support.
#[cold]
#[inline(never)]
fn member_type_error(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    class: &str,
    member: &str,
) -> AnalyzerError {
    use celox_sv_syntax::ParameterKind;
    let native = tree.declarations();
    let mut next = Some(class.to_owned());
    let mut seen = HashSet::default();
    while let Some(name) = next.take() {
        if !seen.insert(name.clone()) {
            break;
        }
        let Some(declaration) = super::super::classes::declaration(tree, session, &name) else {
            return unsupported(format!("type `{member}` of class `{class}`"));
        };
        let declares =
            declaration.type_aliases.iter().any(|&alias| {
                normalize_identifier(&native.type_alias(alias).name.spelling) == member
            }) || declaration
                .parameter_ports
                .iter()
                .chain(&declaration.parameters)
                .flat_map(|&id| &native.parameter_declaration(id).parameters)
                .any(|parameter| {
                    matches!(parameter.kind, ParameterKind::Type { .. })
                        && normalize_identifier(&parameter.name.spelling) == member
                });
        if declares {
            return unsupported(format!("type `{member}` of class `{class}`"));
        }
        next = declaration.base.as_ref().and_then(|base| {
            match &native.data_type(base.data_type).kind {
                celox_sv_syntax::DataTypeKind::Named(path) => path.last().and_then(|part| {
                    super::super::classes::resolve(
                        tree,
                        session,
                        &normalize_identifier(&part.name.spelling),
                    )
                }),
                _ => None,
            }
        });
    }
    AnalyzerError::UndeclaredIdentifier {
        name: member.to_owned(),
        context: format!("class `{class}` declares no type of this name"),
    }
}

/// The class that `scope` of a class-scoped type `C::T` names: inside a
/// parameterized class its name alone refers to the specialization, and
/// elsewhere a parameterized class needs one (IEEE 1800-2023 8.25.1).
fn class_scope(
    scope: &celox_sv_syntax::TypeSegment,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    context: &PackedDimensions,
    mode: BindingMode,
) -> Converted<Option<String>> {
    class_scope_named(
        scope,
        tree,
        session,
        context,
        mode,
        normalize_identifier(&scope.name.spelling),
    )
}

fn class_scope_named(
    scope: &celox_sv_syntax::TypeSegment,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
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
                let class = super::super::classes::resolve(tree, session, &name)?;
                context
                    .type_aliases
                    .get(&super::super::classes::specializations::scope_key(&class))
            })
            .or_else(|| context.type_aliases.get(&name))
            .and_then(|ty| ty.class().map(str::to_owned))
    {
        return Ok(Some(class));
    }
    let Some(class) = super::super::classes::resolve(tree, session, &name) else {
        return Ok(None);
    };
    // Resolve a user package before std's implicit class fallback for this
    // qualified type; explicit std::process remains unambiguous (26.7).
    if class == super::super::processes::CLASS
        && scope::imported(tree).package_scopes.contains(&name)
    {
        return Ok(None);
    }
    let name = class;
    if !super::super::classes::is_parameterized(tree, session, &name) {
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
        if super::super::classes::handle_type(tree, session, &name).is_some()
            && super::super::classes::specializations::lookup(session, &name).is_none()
            && super::super::classes::is_declared(tree, session, &name)
            && mode
                .specialize(tree, session, &name, &[], context)?
                .is_none()
        {
            return Ok(None);
        }
        return Ok(Some(name));
    }
    let Some(arguments) = &scope.arguments else {
        return Err(AnalyzerError::UnspecializedClassScope { class: name });
    };
    mode.specialize(tree, session, &name, arguments, context)
}

pub(in crate::ast) fn expression_type(
    root: ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    context: &PackedDimensions,
) -> Converted<Option<ExprType>> {
    if let ExprKind::Type(id) = tree.declarations().expression(root).kind {
        return Ok(bind(id, tree, session, context)?
            .and_then(|ty| expr_type_from_type(&tree.functions, &ty, &context.const_env)));
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
        let ty = types::enclose_aliased_packed_type(
            ty.clone(),
            packed_ranges(&ranges, tree, session, context)?,
        );
        return Ok(expr_type_from_type(
            &tree.functions,
            &ty,
            &context.const_env,
        ));
    }
    // A fixed-size array property is of its type, though an object holds
    // its elements in a container (see `classes::Property::storage`).
    if let Some((property, _)) =
        expression_binding::object_property(tree, session, root, tree.declarations(), context)?
        && let Some(storage) = &property.storage
    {
        let size = storage.fixed_size().expect("a fixed-size array") as usize;
        return Ok(
            expr_type_from_type(&tree.functions, &storage.element, &context.const_env).map(
                |element| ExprType {
                    width: element.width * size,
                    signed: false,
                },
            ),
        );
    }
    let mut query = context.clone();
    query.expression_signedness.extend(
        context
            .iter()
            .map(|(name, shape)| (name.clone(), shape.signed)),
    );
    query.type_query = true;
    query.functions = Arc::default();
    let value = expression_binding::bind(root, tree, session, &query)?;
    Ok(dimensions::bound_bits_type(tree, &value, &query))
}

/// IEEE 1800-2023 6.19 imposes declaration rules even when no enum member
/// is used. Keep these checks separate from best-effort randomization metadata.
pub(in crate::ast) fn validate_enum_initializers(
    id: TypeId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    context: &PackedDimensions,
    ty: &Type,
) -> Converted<()> {
    let DataTypeKind::Enum { base, items } = &native.data_type(id).kind else {
        return Ok(());
    };
    let Some(shape) = casts::expr_type_from_type(&tree.functions, ty, &context.const_env) else {
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
        let expr =
            constant_binding::expression(value, native, tree, session, &env, &context.type_aliases)
                .ok()
                .flatten()
                .and_then(|expr| {
                    let substituted = constants::substitute_expr_constants_with_parameter_literals(
                        tree,
                        session,
                        const_expr_to_expr(expr),
                        &env,
                        &unknown_literals,
                    );
                    expr_to_const(session, substituted)
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
                &*env,
                &|name| {
                    dimensions::parameter_type_from_const_env(&env, name)
                        .map(|ty| (ty.width, ty.signed))
                },
            )
        });
        previous_unknown = literal
            .as_ref()
            .is_some_and(|value| value.mask != Default::default());
        if previous_unknown
            && (base.is_none() || matches!(ty.kind, TypeKind::Bit | TypeKind::Chandle))
        {
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
            .and_then(|expr| eval_ast_const_expr(&tree.functions, expr, &env))
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
    session: &mut Session,
    context: &PackedDimensions,
    ty: &Type,
) -> Option<Arc<enums::EnumDomain>> {
    let data = native.data_type(id);
    if !data.packed_dimensions.is_empty() {
        return None;
    }
    let DataTypeKind::Enum { items, .. } = &data.kind else {
        return None;
    };
    let shape = casts::expr_type_from_type(&tree.functions, ty, &context.const_env)?;
    // Member values are held as the bits of their base type (6.19), in two's
    // complement in an `i128`; `MemberValue` interprets them.
    if shape.width > 128 {
        return None;
    }
    let mut env = context.const_env.clone();
    let mut next = 0i128;
    let mut values = Vec::new();
    let mut members = Vec::new();
    for item in items {
        if !item.dimensions.is_empty() {
            return None;
        }
        let value = if let Some(id) = item.value {
            let expr = constant_binding::expression(
                id,
                native,
                tree,
                session,
                &env,
                &context.type_aliases,
            )
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
            let value = eval_ast_const_expr(&tree.functions, &expr, &env)?;
            let value_type = parameters::infer_const_expr_type_in(&expr, &env)?;
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
        members.push(enums::EnumMember {
            name: name.clone(),
            value,
        });
        env.insert(name.clone(), value);
        dimensions::insert_parameter_type_markers(&mut env, &name, shape);
        next = value.checked_add(1)?;
    }
    (!members.is_empty()).then(|| {
        Arc::new(enums::EnumDomain {
            members,
            width: shape.width,
            signed: shape.signed,
            four_state: !matches!(ty.kind(), TypeKind::Bit | TypeKind::Chandle),
        })
    })
}

// Keep expression-sized temporaries off the recursive type-binding frame.
fn unpacked_member_defaults(
    fields: Vec<(String, Type, Option<celox_sv_syntax::ExprId>)>,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    context: &PackedDimensions,
    mode: BindingMode,
) -> Converted<Option<Vec<(String, Type, Option<Expr>)>>> {
    let mut bound = Vec::with_capacity(fields.len());
    for (name, ty, default) in fields {
        let default = if let Some(root) = default {
            let Some(value) =
                unpacked_structs::member_default(root, &ty, native, tree, session, context)?
            else {
                // The preliminary alias pass precedes parameter collection.
                if matches!(
                    mode,
                    BindingMode::Alias {
                        defer_defaults: true
                    }
                ) {
                    return Ok(None);
                }
                return Err(unsupported(
                    "nonconstant or unresolved unpacked structure member initial value",
                ));
            };
            Some(value)
        } else {
            None
        };
        bound.push((name, ty, default));
    }
    Ok(Some(bound))
}

/// Push to `values` the type, packed in `ranges`, that a type operator
/// applied to `id` names: that of a type parameter or type alias, `type(T)`,
/// or the self-determined type of an expression, which is not evaluated
/// (IEEE 1800-2023 6.23). Returns whether `id` names one. Kept out of
/// [`bind_in_mode`], whose frame the recursion of class specializations
/// repeats.
#[allow(clippy::too_many_arguments)]
fn push_operand_type(
    values: &mut Vec<Type>,
    ranges: Vec<PackedRange>,
    id: ExprId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    context: &PackedDimensions,
) -> bool {
    let ty = match &native.expression(id).kind {
        ExprKind::Name(name)
            if context
                .type_aliases
                .contains_key(&normalize_identifier(&name.spelling)) =>
        {
            context.type_aliases[&normalize_identifier(&name.spelling)].clone()
        }
        // A variable keeps its declared type, an enumeration or a struct
        // among them (6.23).
        ExprKind::Name(name)
            if context
                .get(&normalize_identifier(&name.spelling))
                .and_then(super::super::type_names::declared_type)
                .is_some() =>
        {
            super::super::type_names::declared_type(
                context
                    .get(&normalize_identifier(&name.spelling))
                    .expect("a declared variable"),
            )
            .expect("a declared type")
            .clone()
        }
        _ => match type_of_expression(id, native, tree, session, context) {
            Some(ty) => ty,
            None => return false,
        },
    };
    values.push(types::enclose_aliased_packed_type(ty, ranges));
    true
}

/// The data type of the self-determined type of value expression `id`, for
/// a type operator applied to it (IEEE 1800-2023 6.23): an integral vector
/// of its width and signing, a real, a shortreal or a string.
fn type_of_expression(
    id: ExprId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    context: &PackedDimensions,
) -> Option<Type> {
    let ty = super::super::expression_types::type_of(tree, session, id, native, context).ok()?;
    Some(match session.types.data(ty) {
        semantic_types::TyData::Packed(packed) => {
            let mut ty = Type::new(if packed.four_state {
                TypeKind::Logic
            } else {
                TypeKind::Bit
            });
            ty.is_signed = packed.signed;
            let bits = packed.bits();
            if bits > 1 {
                ty.packed_ranges = vec![PackedRange::new(
                    ConstExpr::Literal((bits - 1).to_string()),
                    ConstExpr::Literal("0".into()),
                )];
            }
            ty
        }
        semantic_types::TyData::Real => builtin_type("real", None)?,
        semantic_types::TyData::Shortreal => builtin_type("shortreal", None)?,
        semantic_types::TyData::String => builtin_type("string", None)?,
        _ => return None,
    })
}
