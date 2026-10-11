//! `parameter type` overrides, applied to the source text of a module.

use super::*;

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

/// `code` with the `parameter type` defaults of `module_name` replaced by the
/// data types an instantiation binds them to, or `None` when `overrides` names
/// no type parameter of the module.
pub(crate) fn apply_type_parameter_overrides(
    code: &str,
    syntax_tree: &SyntaxTree,
    module_name: &str,
    overrides: &[(String, String)],
) -> Result<Option<String>, AnalyzerError> {
    for node in syntax_tree {
        let RefNode::ModuleDeclarationAnsi(module) = node else {
            continue;
        };
        if module_name_from_node(RefNode::ModuleDeclarationAnsi(module), syntax_tree)?
            != module_name
        {
            continue;
        }
        let mut edits = Vec::new();
        for child in RefNode::ModuleDeclarationAnsi(module) {
            let RefNode::TypeAssignment(assignment) = child else {
                continue;
            };
            let Some(name) =
                identifier_text(RefNode::TypeIdentifier(&assignment.nodes.0), syntax_tree)
            else {
                continue;
            };
            let Some((_, bound)) = overrides.iter().find(|(parameter, _)| *parameter == name)
            else {
                continue;
            };
            match &assignment.nodes.1 {
                Some((_, default)) => {
                    if let Some((start, end)) = node_span(RefNode::DataType(default), syntax_tree) {
                        edits.push(Edit {
                            start,
                            end,
                            replacement: bound.clone(),
                        });
                    }
                }
                None => {
                    if let Some((_, end)) =
                        node_span(RefNode::TypeIdentifier(&assignment.nodes.0), syntax_tree)
                    {
                        edits.push(Edit {
                            start: end,
                            end,
                            replacement: format!(" = {bound}"),
                        });
                    }
                }
            }
        }
        if edits.is_empty() {
            return Ok(None);
        }
        let Some((start, end)) = node_span(RefNode::ModuleDeclarationAnsi(module), syntax_tree)
        else {
            return Ok(None);
        };
        let module_text = apply_edits(code, (start, end), edits);
        return Ok(Some(format!(
            "{}{}{}",
            &code[..start],
            module_text,
            &code[end..]
        )));
    }
    Ok(None)
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
            if !allowed.contains(&name) || !array_parameters::is_array_parameter(param) {
                return Err(unsupported(format!(
                    "pattern override of non-array parameter `{name}`"
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
