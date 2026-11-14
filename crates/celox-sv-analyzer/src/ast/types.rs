//! Native type declarations and syntax-independent type/range operations.
use super::*;
use celox_sv_syntax::{BodyItemKind, ParameterKind, TypeAliasTarget};

pub(super) fn type_aliases_from_scope(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
) -> Converted<TypeAliases> {
    // Constants of this scope are not available until parameter collection.
    // The subsequent pass with an environment requires complete defaults.
    type_aliases_in_scope(
        tree,
        session,
        &ConstEnv::default(),
        &TypeAliases::default(),
        true,
        None,
        true,
        None,
    )
}
pub(super) fn type_aliases_from_scope_with_env(
    syntax_tree: &AnalysisContext<'_>,
    session: &mut Session,
    const_env: &ConstEnv,
) -> Converted<TypeAliases> {
    type_aliases_in_scope(
        syntax_tree,
        session,
        const_env,
        &TypeAliases::default(),
        true,
        None,
        false,
        None,
    )
}

/// Resolve a selected body while retaining its enclosing lexical type
/// bindings. `generate_scope` is the path of the elaborated generate scope
/// of the body, if it is one, whose typedefs are its own (see
/// [`type_aliases_of_generate_scope`]).
pub(super) fn type_aliases_from_body_with_env(
    syntax_tree: &AnalysisContext<'_>,
    session: &mut Session,
    items: &[celox_sv_syntax::BodyItemId],
    const_env: &ConstEnv,
    inherited: &TypeAliases,
    generate_scope: Option<&str>,
) -> Converted<TypeAliases> {
    type_aliases_in_scope(
        syntax_tree,
        session,
        const_env,
        inherited,
        false,
        Some(items),
        false,
        generate_scope,
    )
}

/// The typedefs of elaborated generate scope `path`, such as `g[1]`, whose
/// enumeration, struct and union types are their own in each scope, also
/// in each iteration of a loop (IEEE 1800-2023 27.4).
pub(super) fn type_aliases_of_generate_scope(
    syntax_tree: &AnalysisContext<'_>,
    session: &mut Session,
    items: &[celox_sv_syntax::BodyItemId],
    const_env: &ConstEnv,
    inherited: &TypeAliases,
    path: &str,
) -> Converted<TypeAliases> {
    type_aliases_in_scope(
        syntax_tree,
        session,
        const_env,
        inherited,
        false,
        Some(items),
        false,
        Some(path),
    )
}

fn type_aliases_in_scope(
    syntax_tree: &AnalysisContext<'_>,
    session: &mut Session,
    const_env: &ConstEnv,
    inherited: &TypeAliases,
    include_header: bool,
    items: Option<&[celox_sv_syntax::BodyItemId]>,
    defer_class_defaults: bool,
    generate_scope: Option<&str>,
) -> Converted<TypeAliases> {
    // Each alias binds in a context that shares the tables; it is dropped
    // before the next alias is added, so the table grows without copies.
    let const_env: SharedEnv = const_env.clone().into();
    let const_env = &const_env;
    let imported = scope::imported(syntax_tree);
    let mut aliases: SharedAliases = imported.type_aliases.clone().into();
    aliases.extend(
        inherited
            .iter()
            .map(|(name, ty)| (name.clone(), ty.clone())),
    );
    // The scope records every type visible in it, its own, those of the
    // scopes around it and those it imports, so that its typedefs tell a
    // type without a binding from a name no type has.
    let declarations = Some(match items {
        None => scope_type_declarations(syntax_tree)?,
        Some(items) => nested_type_declarations(syntax_tree, items, inherited)?,
    });
    if include_header && syntax_tree.scope_handle() != crate::source_tree::ScopeHandle::Compilation
    {
        for parameter in syntax_tree.root_unit()?.parameters.iter().flatten() {
            add_type_parameter(
                parameter,
                parameter.id,
                syntax_tree,
                session,
                const_env,
                &mut aliases,
                declarations.as_ref(),
            )?;
        }
    }
    let members = match items {
        Some(items) => items.to_vec(),
        None => syntax_tree.body_membership()?,
    };
    let scope_items = members.clone();
    // The forward typedefs, which the scope's typedefs complete once bound.
    let mut forwards = Vec::new();
    // The first typedef of a type Celox does not support, which does not
    // keep a later typedef that makes the scope illegal from being checked.
    let mut unsupported_typedef = None;

    let mut pending = members.into_iter().rev().collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        match &syntax_tree.declarations().body_item(id).kind {
            BodyItemKind::Region(items) => pending.extend(items.iter().rev()),
            BodyItemKind::Parameter(id) => {
                for parameter in syntax_tree
                    .declarations()
                    .parameter_declaration(*id)
                    .parameters
                    .iter()
                {
                    add_type_parameter(
                        parameter,
                        parameter.id,
                        syntax_tree,
                        session,
                        const_env,
                        &mut aliases,
                        declarations.as_ref(),
                    )?;
                }
            }
            BodyItemKind::TypeAlias(id) => {
                let alias = syntax_tree.declarations().type_alias(*id);
                let bound = match bind_alias(
                    syntax_tree,
                    session,
                    alias,
                    const_env,
                    &aliases,
                    declarations.as_ref(),
                    const_env,
                    defer_class_defaults,
                    || declaration_name(syntax_tree, alias, generate_scope),
                ) {
                    Err(error) if error.is_unsupported() => {
                        unsupported_typedef.get_or_insert(error);
                        continue;
                    }
                    bound => bound?,
                };
                let Some(ty) = bound else {
                    if matches!(alias.target, TypeAliasTarget::Forward(_)) {
                        forwards.push(alias);
                    }
                    continue;
                };
                aliases.insert(instances::normalize_identifier(&alias.name.spelling), ty);
            }
            _ => {}
        }
    }
    for alias in forwards {
        check_forward_typedef(syntax_tree, session, alias, &scope_items, &aliases)?;
    }
    if let Some(error) = unsupported_typedef {
        return Err(error);
    }
    // Alias dimensions belong to their definition scope, not the scope that
    // later declares a signal of that type. Rebuilt for each specialization.
    // A non-negative bound is written as a plain decimal so that it does not
    // widen the index arithmetic of a run-time select to 128 bits.
    for (_, ty) in aliases.iter_mut() {
        freeze_bounds(ty, &syntax_tree.functions, const_env);
    }
    Ok(aliases.into_inner())
}

/// The types visible in a scope, by name, each with the token after which
/// its declaration makes it visible there (IEEE 1800-2023 6.18), or `None`
/// when it is visible throughout: an imported type or a type parameter of
/// the header.
pub(super) type TypeDeclarations = HashMap<String, Option<usize>>;

/// Whether `declarations` has a type `name` visible at token `at`.
pub(super) fn declares_type(declarations: &TypeDeclarations, name: &str, at: usize) -> bool {
    declarations
        .get(name)
        .is_some_and(|visible| visible.is_none_or(|from| from <= at))
}

/// The types visible in the scope of `tree`, a design element or
/// compilation unit: those it declares and those it imports, which tell a
/// type without a binding from a name that no type has.
pub(super) fn scope_type_declarations(
    tree: &AnalysisContext<'_>,
) -> Converted<Arc<TypeDeclarations>> {
    let imported = scope::imported(tree);
    let mut declarations = declared_types(tree)?;
    for name in imported
        .type_declarations
        .iter()
        .chain(imported.type_aliases.keys())
    {
        declarations.insert(name.clone(), None);
    }
    Ok(Arc::new(declarations))
}

/// The types visible in a scope nested in the design element or
/// compilation unit of `tree`, such as a generate block, whose own items
/// are `items`: those of the enclosing scopes, `inherited` among them, and
/// those the items declare.
pub(super) fn nested_type_declarations(
    tree: &AnalysisContext<'_>,
    items: &[celox_sv_syntax::BodyItemId],
    inherited: &TypeAliases,
) -> Converted<Arc<TypeDeclarations>> {
    let mut declarations = (*scope_type_declarations(tree)?).clone();
    for name in inherited.keys() {
        declare(&mut declarations, name.clone(), None);
    }
    declare_items(tree.declarations(), items, &mut declarations);
    Ok(Arc::new(declarations))
}

/// The names of the types that the scope of `tree` declares (see
/// [`declared_types`]).
pub(super) fn declared_type_names(tree: &AnalysisContext<'_>) -> Converted<HashSet<String>> {
    Ok(declared_types(tree)?.into_keys().collect())
}

/// The types that the scope of `tree`, a design element or compilation
/// unit, declares in itself, not in a nested scope (see [`declare_items`]),
/// and the type parameters of its header, visible throughout. Every item of
/// the design element counts, also outside a selection that the scope
/// analyzes now.
fn declared_types(tree: &AnalysisContext<'_>) -> Converted<TypeDeclarations> {
    let native = tree.declarations();
    let element = tree
        .scope_handle()
        .unit(native)
        .filter(|unit| !unit.external);
    let mut declarations = TypeDeclarations::default();
    let items = match element {
        Some(unit) => {
            declare_parameters(unit.parameters.iter().flatten(), true, &mut declarations);
            unit.items.clone()
        }
        None => tree.body_membership()?,
    };
    declare_items(native, &items, &mut declarations);
    Ok(declarations)
}

/// Record that type `name` is visible from token `from`, or throughout with
/// `None`. The first declaration of a name, such as a forward typedef,
/// makes it visible.
fn declare(declarations: &mut TypeDeclarations, name: String, from: Option<usize>) {
    let visible = declarations.entry(name).or_insert(from);
    *visible = visible.zip(from).map(|(known, from)| known.min(from));
}

/// Record the type parameters among `parameters`, each visible after its
/// own declaration, or throughout for those of a `header` (IEEE 1800-2023
/// 6.20.3).
pub(super) fn declare_parameters<'a>(
    parameters: impl IntoIterator<Item = &'a celox_sv_syntax::Parameter>,
    header: bool,
    declarations: &mut TypeDeclarations,
) {
    for parameter in parameters {
        if matches!(parameter.kind, ParameterKind::Type { .. }) {
            declare(
                declarations,
                instances::normalize_identifier(&parameter.name.spelling),
                (!header).then_some(parameter.tokens.end),
            );
        }
    }
}

/// Record the types that `items` of one scope declare, each visible after
/// its declaration, not those of the scopes nested in it: typedefs, also
/// forward ones, type parameters, and covergroups and user-defined
/// nettypes, which Celox keeps unconverted (IEEE 1800-2023 6.18, 6.20.3,
/// 19.3, 6.6.7).
fn declare_items(
    native: &celox_sv_syntax::Declarations,
    items: &[celox_sv_syntax::BodyItemId],
    declarations: &mut TypeDeclarations,
) {
    let name = |name: &celox_sv_syntax::Name| instances::normalize_identifier(&name.spelling);
    let mut pending = items.to_vec();
    while let Some(id) = pending.pop() {
        let item = native.body_item(id);
        match &item.kind {
            BodyItemKind::Region(items) => pending.extend(items.iter().copied()),
            BodyItemKind::TypeAlias(alias) => {
                let alias = native.type_alias(*alias);
                declare(declarations, name(&alias.name), Some(alias.tokens.end));
            }
            BodyItemKind::Parameter(declaration) => declare_parameters(
                &native.parameter_declaration(*declaration).parameters,
                false,
                declarations,
            ),
            BodyItemKind::Unconverted { category, .. }
                if matches!(category.as_str(), "covergroup" | "nettype") =>
            {
                let tokens = item.tokens.range();
                for (declared, range) in native
                    .covergroups
                    .iter()
                    .map(|covergroup| (&covergroup.name, covergroup.tokens))
                    .chain(
                        native
                            .nettypes
                            .iter()
                            .map(|nettype| (&nettype.name, nettype.tokens)),
                    )
                    .filter(|(_, declared)| tokens.contains(&declared.start))
                {
                    declare(declarations, name(declared), Some(range.end));
                }
            }
            _ => {}
        }
    }
}

/// Record the types of class `class`: its type parameters and typedefs,
/// each visible after its declaration in the class's `own` body, and
/// throughout the body of a class that extends it (IEEE 1800-2023 8.13,
/// 8.25).
pub(super) fn declare_class(
    native: &celox_sv_syntax::Declarations,
    class: &celox_sv_syntax::ClassDeclaration,
    own: bool,
    declarations: &mut TypeDeclarations,
) {
    if let Some(ports) = class.parameter_ports {
        declare_parameters(
            &native.parameter_declaration(ports).parameters,
            true,
            declarations,
        );
    }
    for &id in &class.parameters {
        declare_parameters(
            &native.parameter_declaration(id).parameters,
            !own,
            declarations,
        );
    }
    for &id in &class.type_aliases {
        let alias = native.type_alias(id);
        declare(
            declarations,
            instances::normalize_identifier(&alias.name.spelling),
            own.then_some(alias.tokens.end),
        );
    }
}

/// Reject forward typedef `alias` when its scope does not define its name
/// as a type of its kind (IEEE 1800-2023 6.18): a typedef of an enum, struct
/// or union among `scope_items`, or a class or interface class that the
/// typedef's scope resolves.
fn check_forward_typedef(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    alias: &celox_sv_syntax::TypeAlias,
    scope_items: &[celox_sv_syntax::BodyItemId],
    aliases: &TypeAliases,
) -> Converted<()> {
    use celox_sv_syntax::{DataTypeKind, ForwardType};
    let TypeAliasTarget::Forward(kind) = alias.target else {
        return Ok(());
    };
    let native = tree.declarations();
    let name = alias.name.value();
    let mut pending = scope_items.to_vec();
    while let Some(id) = pending.pop() {
        match &native.body_item(id).kind {
            BodyItemKind::Region(items) => pending.extend(items.iter().copied()),
            BodyItemKind::TypeAlias(other) => {
                let other = native.type_alias(*other);
                let TypeAliasTarget::DataType(data) = other.target else {
                    continue;
                };
                if other.name.value() == name
                    && match (kind, &native.data_type(data).kind) {
                        (None, _) => true,
                        (Some(ForwardType::Enum), DataTypeKind::Enum { .. }) => true,
                        (Some(ForwardType::Struct), DataTypeKind::Struct { union: false, .. }) => {
                            true
                        }
                        (Some(ForwardType::Union), DataTypeKind::Struct { union: true, .. }) => {
                            true
                        }
                        // A typedef that names a class completes a class
                        // forward of its kind, once bound to that class.
                        (
                            Some(forward @ (ForwardType::Class | ForwardType::InterfaceClass)),
                            DataTypeKind::Named(_),
                        ) => aliases
                            .get(&instances::normalize_identifier(name))
                            .and_then(|ty| ty.class())
                            .and_then(|class| super::classes::declaration(tree, session, class))
                            .is_some_and(|class| {
                                class.interface == (forward == ForwardType::InterfaceClass)
                            }),
                        _ => false,
                    }
                {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
    let class = super::classes::resolve(tree, session, &instances::normalize_identifier(name))
        .and_then(|class| super::classes::declaration(tree, session, &class));
    let defined = class.is_some_and(|class| match kind {
        None | Some(ForwardType::Class) => !class.interface,
        Some(ForwardType::InterfaceClass) => class.interface,
        Some(_) => false,
    });
    if defined {
        return Ok(());
    }
    Err(AnalyzerError::UndeclaredIdentifier {
        name: name.to_owned(),
        context: format!(
            "a forward typedef whose scope does not define it{} (IEEE 1800-2023 6.18)",
            match kind {
                None => "",
                Some(ForwardType::Enum) => " as an enum",
                Some(ForwardType::Struct) => " as a struct",
                Some(ForwardType::Union) => " as a union",
                Some(ForwardType::Class) => " as a class",
                Some(ForwardType::InterfaceClass) => " as an interface class",
            }
        ),
    })
}

/// The type typedef `alias` declares, of a module, class, generate scope or
/// procedural body: its base type bound in `const_env`, named by `name`
/// before dimensions bound in `dimension_env` hold it, or `None` for a
/// forward typedef or one whose type is not known yet (IEEE 1800-2023
/// 6.18). With `declarations`, the types visible in its scope, a name none
/// of them has is an error.
#[allow(clippy::too_many_arguments)]
pub(super) fn bind_alias(
    syntax_tree: &AnalysisContext<'_>,
    session: &mut Session,
    alias: &celox_sv_syntax::TypeAlias,
    const_env: &SharedEnv,
    aliases: &SharedAliases,
    declarations: Option<&Arc<TypeDeclarations>>,
    dimension_env: &SharedEnv,
    defer_class_defaults: bool,
    name: impl FnOnce() -> (String, type_names::TypedefName),
) -> Converted<Option<Type>> {
    let id = match alias.target {
        TypeAliasTarget::DataType(id) => id,
        TypeAliasTarget::Forward(_) => return Ok(None),
        // An interface-based typedef (6.18) is not bound, rather than
        // skipped as if it named nothing.
        TypeAliasTarget::InterfaceMember(_) => {
            return Err(unsupported(format!(
                "interface-based typedef `{}`",
                alias.name.value()
            )));
        }
    };
    let Some(mut base) = type_binding::alias_base_type(
        id,
        syntax_tree.declarations(),
        syntax_tree,
        session,
        const_env,
        aliases,
        declarations,
        defer_class_defaults,
    )?
    else {
        return Ok(None);
    };
    // The base type's bounds are those of `const_env`, and the declared
    // type is named before container dimensions hold it as their element.
    freeze_bounds(&mut base, &syntax_tree.functions, const_env);
    base.name_declaration(
        unit_scope(
            classes::enclosing_unit(syntax_tree.declarations(), alias.tokens),
            session,
        ),
        name,
    );
    let context = PackedDimensions::new(HashMap::default(), dimension_env, aliases);
    let mut ty =
        containers::with_dimensions(base, &alias.dimensions, syntax_tree, session, &context)?;
    semantic_types::bind_declared(
        &mut session.types,
        &syntax_tree.functions,
        &mut ty,
        &context.const_env,
    );
    Ok(Some(ty))
}

/// `ty` with the bounds of its ranges evaluated in `const_env`, the
/// environment of the scope that declares it.
pub(super) fn freeze_bounds(
    ty: &mut Type,
    functions: &const_functions::ConstantFunctions,
    const_env: &ConstEnv,
) {
    // The bounds take their values where the type is declared, so the type
    // keeps its semantic type.
    let shape = ty.shape_keeping_binding();
    for range in &mut shape.packed_ranges {
        if let Some(value) = eval_ast_const_expr(functions, &range.left, const_env) {
            range.left = bound_literal(value);
        }
        if let Some(value) = eval_ast_const_expr(functions, &range.right, const_env) {
            range.right = bound_literal(value);
        }
    }
    for range in &mut shape.unpacked_ranges {
        if let Some(value) = eval_ast_const_expr(functions, &range.left, const_env) {
            range.left = bound_literal(value);
        }
        if let Some(value) = eval_ast_const_expr(functions, &range.right, const_env) {
            range.right = bound_literal(value);
        }
        if let Some(value) = range
            .size
            .as_ref()
            .and_then(|size| eval_ast_const_expr(functions, size, const_env))
        {
            range.size = Some(bound_literal(value));
        }
    }
}

/// The name of typedef `alias`, qualified by the module, interface, program
/// or package that declares it and by its generate scope, and that name as
/// `$typename` writes it.
/// The elaborated scope that declares a type in `unit`: the analysis of
/// the module, interface or program, each instance of which declares types
/// of its own (IEEE 1800-2023 6.22); `None` in a package or a compilation
/// unit, which the design has once.
pub(super) fn unit_scope(
    unit: Option<&celox_sv_syntax::Unit>,
    session: &Session,
) -> Option<Arc<str>> {
    unit.filter(|unit| !matches!(unit.kind, celox_sv_syntax::UnitKind::Package))
        .and(session.scope.clone())
}

pub(super) fn declaration_name(
    tree: &AnalysisContext<'_>,
    alias: &celox_sv_syntax::TypeAlias,
    generate_scope: Option<&str>,
) -> (String, type_names::TypedefName) {
    let name = instances::normalize_identifier(&alias.name.spelling);
    let name = match generate_scope {
        Some(path) => format!("{path}.{name}"),
        None => name,
    };
    let unit = classes::enclosing_unit(tree.declarations(), alias.tokens);
    let typename = type_names::TypedefName::Scope(type_names::scoped_name(unit, &name).into());
    let identity = match unit {
        Some(unit) => format!(
            "{}::{name}",
            instances::normalize_identifier(&unit.name.spelling)
        ),
        // Each source file's compilation unit is a scope of its own, as for
        // its classes (IEEE 1800-2023 3.12.1).
        None => packages::unit_item_identity(tree.compilation_path(), &name),
    };
    (identity, typename)
}

/// Bind typedef `alias` of a procedural block or subroutine body, as
/// procedural lowering and the discovery of the block's declarations both
/// do: its base type in the scope's constants `const_env`, its dimensions
/// also with the members of an enumeration it declares, which are added to
/// `members`, and its bounds frozen in that scope (IEEE 1800-2023 6.18,
/// 6.19). Each such typedef is a type of its own, also where another block
/// declares one of the same name, named by its declaration; it is written
/// with the name of its design element.
pub(super) fn bind_local_alias(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    alias: &celox_sv_syntax::TypeAlias,
    const_env: &SharedEnv,
    aliases: &SharedAliases,
    declarations: Option<&Arc<TypeDeclarations>>,
    members: &mut EnumMemberConstants,
) -> Converted<Option<Type>> {
    let TypeAliasTarget::DataType(target) = alias.target else {
        return Ok(None);
    };
    let mut dimension_env = (**const_env).clone();
    parameters::enum_members_of(tree, session, target, &mut dimension_env, aliases, members)?;
    let dimension_env: SharedEnv = dimension_env.into();
    let Some(mut ty) = bind_alias(
        tree,
        session,
        alias,
        const_env,
        aliases,
        declarations,
        &dimension_env,
        false,
        || {
            let (identity, typename) = declaration_name(tree, alias, None);
            (
                format!("{identity}@native:{}", alias.tokens.start),
                typename,
            )
        },
    )?
    else {
        return Ok(None);
    };
    freeze_bounds(&mut ty, &tree.functions, &dimension_env);
    Ok(Some(ty))
}

fn bound_literal(value: i128) -> ConstExpr {
    if value >= 0 {
        ConstExpr::Literal(value.to_string())
    } else {
        ConstExpr::Literal(format_typed_parameter_literal(value, 128, true))
    }
}

/// Native body ownership supplies the alias; the original declaration is
/// borrowed only for typed operands and a local target/dimension class guard.
fn add_type_parameter(
    parameter: &celox_sv_syntax::Parameter,
    key: celox_sv_syntax::ParameterId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &SharedEnv,
    aliases: &mut SharedAliases,
    declarations: Option<&Arc<TypeDeclarations>>,
) -> Converted<()> {
    let ParameterKind::Type { default } = parameter.kind else {
        return Ok(());
    };
    let name = instances::normalize_identifier(&parameter.name.spelling);
    if let Some(bound) = scope::imported(tree).type_parameter_bindings.get(&key) {
        aliases.insert(name, bound.clone());
    } else if let Some(id) = default {
        let mut context = PackedDimensions::new(HashMap::default(), env, &*aliases);
        context.type_declarations = declarations.cloned();
        let ty = type_binding::bind(id, tree, session, &context)?;
        drop(context);
        if let Some(ty) = ty {
            aliases.insert(name, ty);
        }
    }
    Ok(())
}

pub(super) fn enclose_aliased_packed_type(
    mut r#type: Type,
    direct_ranges: Vec<PackedRange>,
) -> Type {
    // Use-site dimensions enclose the aliased packed type. The packed
    // array they form is unsigned, whatever its elements (IEEE 1800-2023
    // 7.4.1); a type name takes no signing of its own.
    if !direct_ranges.is_empty() {
        r#type.packed_enum |= r#type.is_enum;
        r#type.enum_domain = None;
        r#type.signed_element_depth = if r#type.is_signed {
            Some(direct_ranges.len())
        } else {
            r#type
                .signed_element_depth
                .map(|depth| depth + direct_ranges.len())
        };
        r#type.is_signed = false;
    }
    let mut ranges = direct_ranges;
    ranges.extend(std::mem::take(&mut r#type.packed_ranges));
    r#type.packed_ranges = ranges;
    r#type
}

/// The error of packed dimensions on a chandle, returned directly by the
/// recursive type binder, which keeps its frames small.
#[inline(never)]
#[cold]
pub(super) fn packed_chandles<T>() -> Result<T, AnalyzerError> {
    Err(AnalyzerError::IllegalChandleUse {
        detail: "a packed array of chandles".into(),
    })
}

/// The error of an enumeration whose base type is a chandle, which is not
/// integral (IEEE 1800-2023 6.19). Out of line like [`packed_chandles`].
#[inline(never)]
#[cold]
pub(super) fn chandle_enum<T>() -> Result<T, AnalyzerError> {
    Err(AnalyzerError::IllegalChandleUse {
        detail: "an enumeration of chandle base type".into(),
    })
}

/// `r#type` with the fixed-size unpacked dimensions `ranges` (IEEE 1800-2023
/// 7.4) in front of its own. Every declaration of a fixed-size unpacked
/// array, also of a typedef, a parameter or a container element, takes its
/// type here.
pub(super) fn type_with_unpacked_ranges(
    mut r#type: Type,
    ranges: Vec<UnpackedRange>,
) -> Result<Type, AnalyzerError> {
    // UVM keeps chandles in scalars and dynamic containers only.
    if r#type.is_chandle() && !ranges.is_empty() {
        return Err(AnalyzerError::Unsupported(
            "fixed-size unpacked array of chandle".into(),
        ));
    }
    // An array of queues, dynamic or associative arrays declared through a
    // typedef has no lowering yet; `int a [2][$]` is rejected as it is
    // parsed.
    if r#type.container.is_some() && !ranges.is_empty() {
        return Err(AnalyzerError::Unsupported(
            "fixed-size unpacked array of dynamically sized arrays".into(),
        ));
    }
    let mut unpacked_ranges = ranges;
    unpacked_ranges.extend(std::mem::take(&mut r#type.unpacked_ranges));
    r#type.unpacked_ranges = unpacked_ranges;
    Ok(r#type)
}

pub(super) fn validate_unpacked_dimension_sizes(
    functions: &const_functions::ConstantFunctions,
    ranges: &[UnpackedRange],
    const_env: &ConstEnv,
) -> Result<(), AnalyzerError> {
    if ranges.iter().any(|range| {
        range
            .size()
            .and_then(|size| eval_ast_const_expr(functions, size, const_env))
            .is_some_and(|size| size <= 0)
    }) {
        return Err(AnalyzerError::Unsupported(
            "nonpositive unpacked array dimension".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    fn unit_error(source: &str) -> Option<String> {
        let packages = crate::analyze_packages(&[(source, Path::new("unit.sv"))]).unwrap();
        packages
            .compilation_unit_illegality()
            .map(ToString::to_string)
    }

    fn module_error(source: &str) -> Option<String> {
        crate::analyze_source(source, Path::new("module.sv"))
            .err()
            .map(|error| error.to_string())
    }

    /// A typedef binds its target eagerly, as a use binds a type (IEEE
    /// 1800-2023 6.18): a name nothing visible from it declares is an error,
    /// in the compilation unit even when no module uses the typedef, and so
    /// is a forward typedef whose scope does not define it. Typedefs that do
    /// not bind for other reasons, such as an unsupported union, are not.
    #[test]
    fn typedefs_bind_their_targets() {
        for (typedef, name) in [
            ("typedef some_other_type myalias;", "some_other_type"),
            (
                "typedef missing_forward_typedef;",
                "missing_forward_typedef",
            ),
            ("typedef class C; typedef int C;", "`C`"),
            (
                "typedef enum E; typedef struct packed { logic a; } E;",
                "`E`",
            ),
            (
                "class C; typedef int T; endclass typedef C::Missing Alias;",
                "Missing",
            ),
            ("typedef int T; typedef class C; typedef T C;", "`C`"),
            ("typedef Missing#(1) Alias;", "Missing"),
            // A typedef is visible after its declaration only.
            ("typedef Later Alias; typedef int Later;", "Later"),
            // A typedef Celox does not support does not hide a later one.
            (
                "typedef Missing.word View; typedef Missing Alias;",
                "Missing",
            ),
        ] {
            let source = format!("{typedef}\nmodule test; endmodule\n");
            let error = unit_error(&source).unwrap_or_else(|| panic!("{typedef} is accepted"));
            assert!(
                error.contains(name) && error.contains("not declared"),
                "{typedef}: {error}"
            );
            let error = module_error(&format!("module test; {typedef} endmodule\n"))
                .unwrap_or_else(|| panic!("{typedef} is accepted in a module"));
            assert!(error.contains(name), "{typedef}: {error}");
        }
        for typedef in [
            "typedef t; typedef int t;",
            "typedef class C; class C; endclass",
            "typedef union { int i; bit b; } u_t;",
            "typedef enum E; typedef enum logic { A } E;",
            "typedef struct S; typedef struct packed { logic a; } S;",
            "typedef interface class I; interface class I; endclass",
            "class C; typedef int T; endclass typedef C::T c_t;",
            "class D; endclass typedef class C; typedef D C;",
            "class C #(int N = 1); endclass typedef C#(2) c_t;",
        ] {
            let source = format!("{typedef}\nmodule test; endmodule\n");
            if let Some(error) = unit_error(&source) {
                panic!("{typedef}: {error}");
            }
        }
        // A name declared only in a nested block, or only in another design
        // element, is not visible.
        // Nor is another design element's name, or a label inside a
        // covergroup, which Celox keeps unconverted.
        for source in [
            "module A; int Missing; endmodule\nmodule B; typedef Missing Alias; endmodule\n",
            "module B; initial begin int Missing; end typedef Missing Alias; endmodule\n",
            "module Missing; endmodule\nmodule B; typedef Missing Alias; endmodule\n",
            "module B; bit x; covergroup cg; Missing: coverpoint x; endgroup typedef Missing Alias; endmodule\n",
            "module B; if (1) begin : g typedef int Missing; end typedef Missing Alias; endmodule\n",
            "package P; int Missing; endpackage\nmodule B; import P::*; typedef Missing Alias; endmodule\n",
            "package P; int Missing; endpackage\nmodule B; typedef P::Missing Alias; endmodule\n",
            "package P; int Missing; endpackage\nmodule B; P::Missing value; endmodule\n",
            "package P; int Missing; endpackage\nmodule B(output P::Missing value); endmodule\n",
            // A type parameter's default, also of a parameter no
            // declaration uses.
            "module B #(parameter type T = Missing) (); endmodule\n",
            "module B; parameter type T = Missing; endmodule\n",
            // The typedefs of a class bind in the class scope, also those
            // nothing uses, and those of a class nothing uses.
            "module B; class C; typedef Missing T; endclass C c; endmodule\n",
            "module B; class C; typedef Missing T; endclass endmodule\n",
            // A class property's type names a type visible in the class.
            "module B; class C; Missing x; endclass endmodule\n",
            // A generate block's typedefs bind in the block, with the types
            // of the scopes around it.
            "module B; if (1) begin : g typedef Missing T; end endmodule\n",
            "module B; typedef int t; if (1) begin : g typedef t u; typedef Missing T; end endmodule\n",
            "module B; class C; typedef Missing T; typedef int Missing; endclass C c; endmodule\n",
            "class C; int x; typedef Missing T; endclass\nmodule B; C c; endmodule\n",
            // A package's imports are not its types: only an export makes
            // them visible to its importers.
            "package Q; nettype logic Missing; endpackage\npackage P; import Q::*; endpackage\nmodule B; import P::*; typedef Missing Alias; endmodule\n",
            "package P; nettype logic Missing; endpackage\nmodule B; typedef Missing Alias; endmodule\n",
        ] {
            let error = module_error(source).unwrap_or_else(|| panic!("{source} is accepted"));
            assert!(
                error.contains("Missing") && error.contains("not declared"),
                "{error}"
            );
        }
        // A package-qualified name names a type the package declares: a
        // package value is no type, and an item the package does not have
        // is no item of it.
        let error = unit_error(
            "package P; int Missing; endpackage\ntypedef P::Missing Alias;\nmodule test; endmodule\n",
        )
        .expect("a typedef of a package value is accepted");
        assert!(
            error.contains("P::Missing") && error.contains("not declared"),
            "{error}"
        );
        for source in [
            "package P; typedef int T; endpackage\ntypedef P::Missing Alias;\nmodule test; endmodule\n",
            "package P; typedef int T; endpackage\nmodule B; P::Missing value; endmodule\n",
        ] {
            let error = unit_error(source)
                .or_else(|| module_error(source))
                .unwrap_or_else(|| panic!("{source} is accepted"));
            assert!(error.contains("has no item `Missing`"), "{error}");
        }
        // A class property of `$unit`, and a class that extends a class of
        // another source, name the types visible in the class too.
        let error = unit_error("class C; Missing x; endclass\nmodule test; endmodule\n")
            .expect("an unused $unit class property of an undeclared type is accepted");
        assert!(
            error.contains("Missing") && error.contains("not declared"),
            "{error}"
        );
        let error = crate::analyze_packages(&[
            (
                "package P; class Base; endclass endpackage\n",
                Path::new("p.sv"),
            ),
            (
                "class D extends P::Base; typedef Missing T; endclass\nmodule test; endmodule\n",
                Path::new("unit.sv"),
            ),
        ])
        .unwrap()
        .compilation_unit_illegality()
        .map(ToString::to_string)
        .expect("a typedef of an undeclared name in a class with a foreign base is accepted");
        assert!(
            error.contains("Missing") && error.contains("not declared"),
            "{error}"
        );
        // An analysis of one source rejects an illegal compilation unit
        // too, also one that no module uses.
        let error = module_error("typedef Missing Alias;\nmodule M; endmodule\n")
            .expect("an unused illegal $unit typedef is accepted by analyze_source");
        assert!(
            error.contains("Missing") && error.contains("not declared"),
            "{error}"
        );
        // A value a package exports is no type either, also when the
        // compilation unit imports it.
        let error = crate::analyze_packages(&[
            ("package P; int T; endpackage\n", Path::new("p.sv")),
            (
                "import P::*;\ntypedef T Alias;\nmodule test; endmodule\n",
                Path::new("unit.sv"),
            ),
        ])
        .unwrap()
        .compilation_unit_illegality()
        .expect("a typedef of an imported value is accepted")
        .to_string();
        assert!(error.contains("not declared"), "{error}");
        // A declared type that has no binding is no undeclared name: a
        // typedef whose bounds use a constant not known yet, a typedef of
        // a type Celox does not bind, and a type that a package declares
        // so.
        for source in [
            "module test; parameter W = 4; typedef logic [W-1:0] w_t; typedef w_t v_t; v_t v; endmodule\n",
            "module test; typedef union { int i; bit b; } u_t; typedef u_t v_t; endmodule\n",
            "package P; typedef union { int i; bit b; } u_t; endpackage\nmodule test; import P::*; typedef u_t v_t; endmodule\n",
            "package P; typedef union { int i; bit b; } u_t; nettype logic nt; endpackage\nmodule test; typedef P::u_t v_t; typedef P::nt n_t; endmodule\n",
            "package P; parameter W = 4; typedef logic [W-1:0] w_t; typedef P::w_t v_t; endpackage\nmodule test; P::v_t v; endmodule\n",
        ] {
            if let Some(error) = module_error(source)
                && error.contains("not declared")
            {
                panic!("{source}: {error}");
            }
        }
        // A package the compilation unit imports from another source
        // provides the type.
        let packages = crate::analyze_packages(&[
            ("package P; typedef int T; endpackage\n", Path::new("p.sv")),
            (
                "import P::*;\ntypedef T Alias;\nmodule test; endmodule\n",
                Path::new("unit.sv"),
            ),
        ])
        .unwrap();
        if let Some(error) = packages.compilation_unit_illegality() {
            panic!("{error}");
        }
        // A class typedef names the class's parameters and typedefs, those
        // of the classes it extends, and the types of its scope; a type
        // parameter is visible after its own declaration, also within one
        // parameter declaration.
        for source in [
            "module test; typedef union { int i; bit b; } U; parameter type A = U, B = A; endmodule\n",
            "module test; class C; typedef union { int i; bit b; } U; parameter type A = U, B = A; endclass C c; endmodule\n",
            "module test; typedef int t; if (1) begin : g typedef t u; u x; end endmodule\n",
            "typedef int t; class B; typedef t bt; endclass class C #(type P = int) extends B; typedef P p_t; typedef bt b_t; endclass\nmodule test; C c; endmodule\n",
            "module test; class C #(parameter W = 4); typedef logic [W-1:0] w_t; typedef w_t v_t; endclass C #(8) c; endmodule\n",
        ] {
            if let Some(error) = module_error(source)
                && error.contains("not declared")
            {
                panic!("{source}: {error}");
            }
        }
        // A covergroup or nettype names a type the binder does not model.
        for source in [
            "module test; bit x; covergroup cg @(posedge x); coverpoint x; endgroup cg c; endmodule\n",
            "module test; nettype logic nt; nt w; endmodule\n",
        ] {
            if let Some(error) = module_error(source)
                && error.contains("not declared")
            {
                panic!("{source}: {error}");
            }
        }
        // A type a package declares is visible where it is imported, also
        // one Celox does not bind.
        for source in [
            "package P; nettype logic nt; endpackage\nmodule test; import P::*; typedef nt n_t; endmodule\n",
            "package Q; nettype logic nt; endpackage\npackage P; import Q::nt; export Q::nt; endpackage\nmodule test; import P::*; typedef nt n_t; endmodule\n",
        ] {
            if let Some(error) = module_error(source)
                && error.contains("not declared")
            {
                panic!("{source}: {error}");
            }
        }
        // A typedef of a type reference is not supported, rather than
        // skipped.
        let error = module_error("module test; typedef type(no_such_name) Alias; endmodule\n")
            .expect("a typedef of a type reference is accepted");
        assert!(error.contains("type reference"), "{error}");
        // An interface-based typedef is not supported, rather than skipped.
        let error = module_error("module test; typedef Missing.word View; endmodule\n")
            .expect("an interface-based typedef is accepted");
        assert!(error.contains("interface-based typedef"), "{error}");
    }
}
