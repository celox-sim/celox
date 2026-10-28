//! Native type declarations and syntax-independent type/range operations.
use super::*;
use celox_sv_syntax::{BodyItemKind, ParameterKind, TypeAliasTarget};

pub(super) fn type_aliases_from_scope(
    tree: &AnalysisContext<'_>,
) -> Converted<HashMap<String, Type>> {
    // Constants of this scope are not available until parameter collection.
    // The subsequent pass with an environment requires complete defaults.
    type_aliases_in_scope(
        tree,
        &HashMap::default(),
        &HashMap::default(),
        true,
        None,
        true,
        None,
    )
}
pub(super) fn type_aliases_from_scope_with_env(
    syntax_tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
) -> Converted<HashMap<String, Type>> {
    type_aliases_in_scope(
        syntax_tree,
        const_env,
        &HashMap::default(),
        true,
        None,
        false,
        None,
    )
}

/// Resolve a selected body while retaining its enclosing lexical type bindings.
pub(super) fn type_aliases_from_body_with_env(
    syntax_tree: &AnalysisContext<'_>,
    items: &[celox_sv_syntax::BodyItemId],
    const_env: &HashMap<String, i128>,
    inherited: &HashMap<String, Type>,
) -> Converted<HashMap<String, Type>> {
    type_aliases_in_scope(
        syntax_tree,
        const_env,
        inherited,
        false,
        Some(items),
        false,
        None,
    )
}

/// The typedefs of elaborated generate scope `path`, such as `g[1]`, whose
/// enumeration, struct and union types are their own in each scope, also
/// in each iteration of a loop (IEEE 1800-2023 27.4).
pub(super) fn type_aliases_of_generate_scope(
    syntax_tree: &AnalysisContext<'_>,
    items: &[celox_sv_syntax::BodyItemId],
    const_env: &HashMap<String, i128>,
    inherited: &HashMap<String, Type>,
    path: &str,
) -> Converted<HashMap<String, Type>> {
    type_aliases_in_scope(
        syntax_tree,
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
    const_env: &HashMap<String, i128>,
    inherited: &HashMap<String, Type>,
    include_header: bool,
    items: Option<&[celox_sv_syntax::BodyItemId]>,
    defer_class_defaults: bool,
    generate_scope: Option<&str>,
) -> Converted<HashMap<String, Type>> {
    let mut aliases = scope::imported(&syntax_tree.state).type_aliases.clone();
    aliases.extend(
        inherited
            .iter()
            .map(|(name, ty)| (name.clone(), ty.clone())),
    );
    if include_header && syntax_tree.scope_handle() != crate::source_tree::ScopeHandle::Compilation
    {
        for parameter in syntax_tree.root_unit()?.parameters.iter().flatten() {
            add_type_parameter(
                parameter,
                parameter.id,
                syntax_tree,
                const_env,
                &mut aliases,
            )?;
        }
    }
    let members = match items {
        Some(items) => items.to_vec(),
        None => syntax_tree.body_membership()?,
    };
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
                        const_env,
                        &mut aliases,
                    )?;
                }
            }
            BodyItemKind::TypeAlias(id) => {
                let alias = syntax_tree.declarations().type_alias(*id);
                let TypeAliasTarget::DataType(id) = alias.target else {
                    continue;
                };
                let Some(base) = type_binding::alias_base_type(
                    id,
                    syntax_tree.declarations(),
                    syntax_tree,
                    const_env,
                    &aliases,
                    defer_class_defaults,
                )?
                else {
                    continue;
                };
                let context = PackedDimensions::new(HashMap::default(), const_env, &aliases);
                let mut ty =
                    containers::with_dimensions(base, &alias.dimensions, syntax_tree, &context)?;
                ty.name_declaration(|| declaration_name(syntax_tree, alias, generate_scope));
                aliases.insert(instances::normalize_identifier(&alias.name.spelling), ty);
            }
            _ => {}
        }
    }
    // Alias dimensions belong to their definition scope, not the scope that
    // later declares a signal of that type. Rebuilt for each specialization.
    // A non-negative bound is written as a plain decimal so that it does not
    // widen the index arithmetic of a run-time select to 128 bits.
    for ty in aliases.values_mut() {
        for range in &mut ty.packed_ranges {
            if let Some(value) = eval_ast_const_expr(&syntax_tree.state, &range.left, const_env) {
                range.left = bound_literal(value);
            }
            if let Some(value) = eval_ast_const_expr(&syntax_tree.state, &range.right, const_env) {
                range.right = bound_literal(value);
            }
        }
        for range in &mut ty.unpacked_ranges {
            if let Some(value) = eval_ast_const_expr(&syntax_tree.state, &range.left, const_env) {
                range.left = bound_literal(value);
            }
            if let Some(value) = eval_ast_const_expr(&syntax_tree.state, &range.right, const_env) {
                range.right = bound_literal(value);
            }
            if let Some(value) = range
                .size
                .as_ref()
                .and_then(|size| eval_ast_const_expr(&syntax_tree.state, size, const_env))
            {
                range.size = Some(bound_literal(value));
            }
        }
    }
    Ok(aliases)
}

/// The name of typedef `alias`, qualified by the module, interface, program
/// or package that declares it and by its generate scope.
fn declaration_name(
    tree: &AnalysisContext<'_>,
    alias: &celox_sv_syntax::TypeAlias,
    generate_scope: Option<&str>,
) -> String {
    let name = instances::normalize_identifier(&alias.name.spelling);
    let name = match generate_scope {
        Some(path) => format!("{path}.{name}"),
        None => name,
    };
    match classes::enclosing_unit(tree.declarations(), alias.tokens) {
        Some(unit) => format!(
            "{}::{name}",
            instances::normalize_identifier(&unit.name.spelling)
        ),
        None => format!("{}::{name}", packages::UNIT),
    }
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
    env: &HashMap<String, i128>,
    aliases: &mut HashMap<String, Type>,
) -> Converted<()> {
    let ParameterKind::Type { default } = parameter.kind else {
        return Ok(());
    };
    let name = instances::normalize_identifier(&parameter.name.spelling);
    if let Some(bound) = scope::imported(&tree.state)
        .type_parameter_bindings
        .get(&key)
    {
        aliases.insert(name, bound.clone());
    } else if let Some(id) = default {
        let context = PackedDimensions::new(HashMap::default(), env, aliases);
        if let Some(ty) = type_binding::bind(id, tree, &context)? {
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
    ranges.extend(r#type.packed_ranges);
    r#type.packed_ranges = ranges;
    r#type
}

pub(super) fn type_with_unpacked_ranges(mut r#type: Type, ranges: Vec<UnpackedRange>) -> Type {
    let mut unpacked_ranges = ranges;
    unpacked_ranges.extend(r#type.unpacked_ranges);
    r#type.unpacked_ranges = unpacked_ranges;
    r#type
}

pub(super) fn validate_unpacked_dimension_sizes(
    state: &AnalysisState,
    ranges: &[UnpackedRange],
    const_env: &HashMap<String, i128>,
) -> Result<(), AnalyzerError> {
    if ranges.iter().any(|range| {
        range
            .size()
            .and_then(|size| eval_ast_const_expr(state, size, const_env))
            .is_some_and(|size| size <= 0)
    }) {
        return Err(AnalyzerError::Unsupported(
            "nonpositive unpacked array dimension".to_string(),
        ));
    }
    Ok(())
}
