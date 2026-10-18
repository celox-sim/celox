//! Type parameter bindings and the remaining value-pattern source edits.

use super::*;

/// Bind only declarations in this module's parameter scope. Offsets identify
/// declarations rather than names: a nested local type named T must not inherit
/// the instance's binding for the module parameter T.
pub(super) fn bind(
    node: RefNode<'_>,
    tree: &SyntaxTree,
    overrides: &[(String, Type)],
) -> Result<HashMap<usize, Type>, AnalyzerError> {
    let mut declarations = HashMap::default();
    let mut collect = |node: RefNode<'_>, local: bool| {
        for child in node {
            if let RefNode::TypeAssignment(assignment) = child {
                let identifier = RefNode::TypeIdentifier(&assignment.nodes.0);
                if let Some(name) = identifier_text(identifier.clone(), tree)
                    && let Some(locate) = identifier_locate(identifier)
                {
                    declarations.insert(name, (locate.offset, local));
                }
            }
        }
    };
    let has_port_list = module_parameter_port_list(node.clone()).is_some();
    if let Some(list) = module_parameter_port_list(node.clone()) {
        let mut local = false;
        for child in list {
            if let RefNode::ParameterPortDeclaration(declaration) = child {
                match declaration {
                    sv_parser::ParameterPortDeclaration::LocalParameterDeclaration(_) => {
                        local = true
                    }
                    sv_parser::ParameterPortDeclaration::ParameterDeclaration(_) => local = false,
                    _ => {}
                }
                collect(RefNode::ParameterPortDeclaration(declaration), local);
            }
        }
    }
    for declaration in scope_declarations(node) {
        match declaration {
            sv_parser::PackageOrGenerateItemDeclaration::ParameterDeclaration(declaration) => {
                collect(RefNode::ParameterDeclaration(&declaration.0), has_port_list);
            }
            sv_parser::PackageOrGenerateItemDeclaration::LocalParameterDeclaration(declaration) => {
                collect(RefNode::LocalParameterDeclaration(&declaration.0), true);
            }
            _ => {}
        }
    }
    let mut bindings = HashMap::default();
    for (name, ty) in overrides {
        let &(offset, local) = declarations
            .get(name)
            .ok_or_else(|| AnalyzerError::UnknownParameterOverride { name: name.clone() })?;
        if local {
            return Err(AnalyzerError::Unsupported(format!(
                "localparam override `{name}`"
            )));
        }
        if bindings.insert(offset, ty.clone()).is_some() {
            return Err(AnalyzerError::Unsupported(format!(
                "duplicate parameter override `{name}`"
            )));
        }
    }
    Ok(bindings)
}

/// Freeze every dimension in the caller's environment before crossing the
/// instance boundary. A child's parameters must not reinterpret caller names.
pub(super) fn resolve(mut ty: Type, env: &HashMap<String, i128>) -> Option<Type> {
    fn bound(expr: &ConstExpr, env: &HashMap<String, i128>) -> Option<ConstExpr> {
        let value = eval_ast_const_expr(expr, env)?;
        Some(if value >= 0 {
            ConstExpr::Literal(value.to_string())
        } else {
            ConstExpr::Literal(format_typed_parameter_literal(value, 128, true))
        })
    }
    for range in &mut ty.packed_ranges {
        range.left = bound(&range.left, env)?;
        range.right = bound(&range.right, env)?;
    }
    for range in &mut ty.unpacked_ranges {
        range.left = bound(&range.left, env)?;
        range.right = bound(&range.right, env)?;
        if let Some(size) = &range.size {
            range.size = Some(bound(size, env)?);
        }
    }
    let mut members = Vec::new();
    for member in ty.members {
        let resolved = resolve(member.r#type().clone(), env)?;
        members.push(member.with_type(|_| resolved));
    }
    ty.members = members;
    Some(ty)
}

/// A source text edit: replace `start..end` with `replacement`.
struct Edit {
    start: usize,
    end: usize,
    replacement: String,
}

/// The span of `node` in the original source text. Token offsets refer to
/// the preprocessed text, which can differ from the source (the preprocessor
/// widens the space after `"DPI-C"`), so each token is mapped back and text
/// the preprocessor inserted is skipped.
pub(super) fn node_span(node: RefNode<'_>, syntax_tree: &SyntaxTree) -> Option<(usize, usize)> {
    let origin = |offset| {
        syntax_tree.get_origin(&sv_parser::Locate {
            offset,
            line: 0,
            len: 1,
        })
    };
    let mut range: Option<(usize, usize)> = None;
    for child in node {
        if let RefNode::Locate(locate) = child
            && locate.len > 0
            && let Some((_, start)) = origin(locate.offset)
            && let Some((_, last)) = origin(locate.offset + locate.len - 1)
        {
            let end = last + 1;
            range = Some(match range {
                None => (start, end),
                Some((low, high)) => (low.min(start), high.max(end)),
            });
        }
    }
    range
}

/// Apply the edits that fall inside `text[range]`, dropping any edit nested in
/// an earlier (outer) one.
fn apply_edits(text: &str, range: (usize, usize), mut edits: Vec<Edit>) -> String {
    edits.retain(|edit| edit.start >= range.0 && edit.end <= range.1);
    edits.sort_by_key(|edit| (edit.start, std::cmp::Reverse(edit.end)));
    let mut result = String::new();
    let mut cursor = range.0;
    for edit in edits {
        if edit.start < cursor {
            continue;
        }
        result.push_str(&text[cursor..edit.start]);
        result.push_str(&edit.replacement);
        cursor = edit.end;
    }
    result.push_str(&text[cursor..range.1]);
    result
}

/// Override array/structure value parameters before their target shapes are
/// lowered. Only overridable module parameters may be rebound, never locals.
pub(crate) fn apply_pattern_parameter_overrides(
    code: &str,
    tree: &SyntaxTree,
    module_name: &str,
    overrides: &[(String, String)],
) -> Result<Option<String>, AnalyzerError> {
    if overrides.is_empty() {
        return Ok(None);
    }
    for node in tree {
        if !matches!(
            node,
            RefNode::ModuleDeclarationAnsi(_) | RefNode::ModuleDeclarationNonansi(_)
        ) {
            continue;
        }
        if module_name_from_node(node.clone(), tree)? != module_name {
            continue;
        }
        let allowed = declarations::module_interface_from_node(node.clone(), tree)?.parameters;
        let mut edits = Vec::new();
        let mut found = HashSet::default();
        let mut declarations = module_parameter_port_list(node.clone())
            .into_iter()
            .collect::<Vec<_>>();
        declarations.extend(scope_declarations(node.clone()).into_iter().filter_map(
            |declaration| {
                if let sv_parser::PackageOrGenerateItemDeclaration::ParameterDeclaration(
                    declaration,
                ) = declaration
                {
                    Some(RefNode::ParameterDeclaration(&declaration.0))
                } else {
                    None
                }
            },
        ));
        for child in declarations.into_iter().flat_map(|node| node.into_iter()) {
            let RefNode::ParamAssignment(param) = child else {
                continue;
            };
            let name = parameter_name(RefNode::ParameterIdentifier(&param.nodes.0), tree)?;
            let Some((_, bound)) = overrides.iter().find(|(candidate, _)| *candidate == name)
            else {
                continue;
            };
            if !allowed.contains(&name) {
                return Err(unsupported(format!(
                    "pattern override of non-overridable parameter `{name}`"
                )));
            }
            let Some((_, value)) = &param.nodes.2 else {
                return Err(unsupported(format!(
                    "array parameter `{name}` without a default"
                )));
            };
            let (start, end) = node_span(RefNode::ConstantParamExpression(value), tree)
                .ok_or_else(|| unsupported("array parameter source span"))?;
            edits.push(Edit {
                start,
                end,
                replacement: bound.clone(),
            });
            found.insert(name);
        }
        for (name, _) in overrides {
            if !found.contains(name) {
                return Err(unsupported(format!("unknown array parameter `{name}`")));
            }
        }
        return Ok(Some(apply_edits(code, (0, code.len()), edits)));
    }
    Err(unsupported(format!("module `{module_name}`")))
}
