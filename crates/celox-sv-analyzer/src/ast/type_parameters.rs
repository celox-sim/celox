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
