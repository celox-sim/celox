//! Diagnostics derived from authored function declarations.

use super::*;
use crate::procedural::SourceSpan;

/// A non-fatal source diagnostic, independent of constant evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnalyzerWarning {
    /// An ignored `$exit` in a function outside a program (IEEE 1800-2023 24.7).
    ExitOutsideProgram { site: Option<SourceSpan> },
}

pub(crate) fn append(
    warnings: &mut Vec<AnalyzerWarning>,
    found: impl IntoIterator<Item = AnalyzerWarning>,
) {
    for warning in found {
        if !warnings.contains(&warning) {
            warnings.push(warning);
        }
    }
}

/// Function diagnostics belong to the declaration, including functions whose
/// bodies disappear during constant evaluation. This pass has no side effects.
pub(crate) fn functions<'a>(
    nodes: impl IntoIterator<Item = RefNode<'a>>,
    tree: &SyntaxTree,
) -> Vec<AnalyzerWarning> {
    let mut warnings = Vec::new();
    let mut functions = Vec::new();
    let mut programs = Vec::new();
    for node in nodes {
        if matches!(node, RefNode::ProgramDeclaration(_)) {
            if let Some(span) = super::procedural::source_span(node, tree) {
                programs.push(span);
            }
        } else if matches!(node, RefNode::FunctionDeclaration(_)) {
            functions.push(node);
        }
    }
    for function in functions {
        for node in function {
            if let RefNode::SystemTfCall(call) = node
                && super::system_tf_call_parts(call, tree).is_some_and(|(name, _)| name == "$exit")
            {
                let site = super::procedural::source_span(RefNode::SystemTfCall(call), tree);
                if site.as_ref().is_some_and(|site| {
                    programs.iter().any(|program| {
                        program.path == site.path
                            && program.start <= site.start
                            && site.end <= program.end
                    })
                }) {
                    continue;
                }
                append(
                    &mut warnings,
                    [AnalyzerWarning::ExitOutsideProgram { site }],
                );
            }
        }
    }
    warnings
}

pub(crate) fn components(tree: &SyntaxTree) -> HashMap<String, Vec<AnalyzerWarning>> {
    let mut warnings = HashMap::default();
    for node in tree {
        let identifier = match &node {
            RefNode::ModuleDeclarationAnsi(_) => {
                super::module_name_from_node(node.clone(), tree).ok()
            }
            RefNode::ModuleDeclarationNonansi(_) => {
                super::module_name_from_node(node.clone(), tree).ok()
            }
            RefNode::InterfaceDeclarationAnsi(interface) => {
                super::component_identifier(&interface.nodes.0.nodes.3, tree)
            }
            RefNode::InterfaceDeclarationNonansi(interface) => {
                super::component_identifier(&interface.nodes.0.nodes.3, tree)
            }
            _ => None,
        };
        if let Some(name) = identifier {
            append(warnings.entry(name).or_default(), functions(node, tree));
        }
    }
    warnings
}

pub(crate) fn packages(tree: &SyntaxTree) -> Vec<AnalyzerWarning> {
    let mut warnings = Vec::new();
    for node in tree {
        if matches!(node, RefNode::PackageDeclaration(_)) {
            append(&mut warnings, functions(node, tree));
        }
    }
    if let Some(unit) = super::packages::unit_declaration(tree) {
        append(
            &mut warnings,
            functions(RefNode::PackageDeclaration(&unit), tree),
        );
    }
    warnings
}
