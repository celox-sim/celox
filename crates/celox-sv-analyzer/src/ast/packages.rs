//! Package elaboration by inlining.
//!
//! A package is a named scope of declarations (parameters, types, functions)
//! that modules reach through `import p::*;` or `p::name`. The declaration
//! collectors work on the items of one module, so a module that uses a package
//! is analyzed with the package's items inlined at the end of its body: its
//! `import` declarations and `p::` qualifiers are removed, and the package
//! `parameter`s become `localparam`s of the module. Identifiers resolve by
//! their plain name, so a clash between a package item and a module item is
//! reported as a duplicate declaration.

use super::*;

/// A source text edit: replace `start..end` with `replacement`. An edit that
/// `requires` a package name applies only when that package is known: a
/// `name::` in an expression parses as a class scope, so only the set of
/// declared packages tells the two apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    start: usize,
    end: usize,
    replacement: String,
    requires: Option<String>,
}

/// The items of one package, ready to be inlined into a module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSource {
    pub name: String,
    /// The package items as written.
    body: String,
    /// Edits that rewrite `body` for inlining, relative to its start.
    edits: Vec<Edit>,
    /// The packages this package may refer to.
    uses: Vec<String>,
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

fn edit(start: usize, end: usize, replacement: &str, requires: Option<String>) -> Edit {
    Edit {
        start,
        end,
        replacement: replacement.to_string(),
        requires,
    }
}

/// The edits that remove package imports and qualifiers (and turn package
/// parameters into localparams) inside `node`, and the package names it may
/// refer to.
fn package_edits(
    node: RefNode<'_>,
    syntax_tree: &SyntaxTree,
    localize_parameters: bool,
) -> (Vec<Edit>, Vec<String>) {
    let mut edits = Vec::new();
    let mut used: Vec<String> = Vec::new();
    let mut note = |name: &Option<String>| {
        if let Some(name) = name
            && !used.contains(name)
        {
            used.push(name.clone());
        }
    };
    for child in node {
        match child {
            RefNode::PackageImportDeclaration(declaration) => {
                for item in RefNode::PackageImportDeclaration(declaration) {
                    if let RefNode::PackageIdentifier(identifier) = item {
                        note(&identifier_text(
                            RefNode::PackageIdentifier(identifier),
                            syntax_tree,
                        ));
                    }
                }
                if let Some((start, end)) =
                    node_span(RefNode::PackageImportDeclaration(declaration), syntax_tree)
                {
                    edits.push(edit(start, end, "", None));
                }
            }
            RefNode::PackageScopePackage(scope) => {
                note(&identifier_text(
                    RefNode::PackageIdentifier(&scope.nodes.0),
                    syntax_tree,
                ));
                if let Some((start, end)) =
                    node_span(RefNode::PackageScopePackage(scope), syntax_tree)
                {
                    edits.push(edit(start, end, "", None));
                }
            }
            RefNode::ClassScope(scope) => {
                // `p::name` in an expression: a package when `p` is declared as one.
                let class_type = &scope.nodes.0;
                if class_type.nodes.1.is_none()
                    && class_type.nodes.2.is_empty()
                    && class_type.nodes.0.nodes.0.is_none()
                {
                    let name = identifier_text(
                        RefNode::ClassIdentifier(&class_type.nodes.0.nodes.1),
                        syntax_tree,
                    );
                    if let Some((start, end)) = node_span(RefNode::ClassScope(scope), syntax_tree) {
                        note(&name);
                        edits.push(edit(start, end, "", name));
                    }
                }
            }
            RefNode::ParameterDeclaration(sv_parser::ParameterDeclaration::Param(parameter))
                if localize_parameters =>
            {
                if let Some((start, end)) =
                    node_span(RefNode::Keyword(&parameter.nodes.0), syntax_tree)
                {
                    edits.push(edit(start, end, "localparam ", None));
                }
            }
            _ => {}
        }
    }
    (edits, used)
}

/// Apply the edits that fall inside `text[range]`, dropping any edit nested in
/// an earlier (outer) one and any conditional edit whose package is unknown.
fn apply_edits(
    text: &str,
    range: (usize, usize),
    mut edits: Vec<Edit>,
    packages: &HashMap<String, PackageSource>,
) -> String {
    edits.retain(|edit| {
        edit.start >= range.0
            && edit.end <= range.1
            && edit
                .requires
                .as_ref()
                .is_none_or(|name| packages.contains_key(name))
    });
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

/// The packages declared in a source, rewritten for inlining.
pub fn source_packages(
    code: &str,
    syntax_tree: &SyntaxTree,
) -> Result<Vec<PackageSource>, AnalyzerError> {
    // Only modules and packages are analyzed, so an import declared at
    // compilation-unit scope would never be found.
    for node in syntax_tree {
        if let RefNode::DescriptionPackageItem(item) = node
            && RefNode::DescriptionPackageItem(item)
                .into_iter()
                .any(|child| matches!(child, RefNode::DpiImportExport(_)))
        {
            return Err(AnalyzerError::Unsupported(
                "DPI-C import or export at compilation-unit scope (declare it in a module \
                 or package)"
                    .to_string(),
            ));
        }
    }
    let mut packages = Vec::new();
    for node in syntax_tree {
        let RefNode::PackageDeclaration(package) = node else {
            continue;
        };
        let name = identifier_text(RefNode::PackageIdentifier(&package.nodes.3), syntax_tree)
            .ok_or_else(|| AnalyzerError::Unsupported("package identifier".to_string()))?;
        reject_package_state(&name, &package.nodes.6, syntax_tree)?;
        let (Some((_, header_end)), Some((footer_start, _))) = (
            node_span(RefNode::Symbol(&package.nodes.4), syntax_tree),
            node_span(RefNode::Keyword(&package.nodes.7), syntax_tree),
        ) else {
            continue;
        };
        let (edits, mut uses) =
            package_edits(RefNode::PackageDeclaration(package), syntax_tree, true);
        uses.retain(|used| *used != name);
        let edits = edits
            .into_iter()
            .filter(|edit| edit.start >= header_end && edit.end <= footer_start)
            .map(|edit| Edit {
                start: edit.start - header_end,
                end: edit.end - header_end,
                ..edit
            })
            .collect();
        packages.push(PackageSource {
            name,
            body: code[header_end..footer_start].to_string(),
            edits,
            uses,
        });
    }
    Ok(packages)
}

/// Reject the variables and nets of package `name`. A package declares one
/// object shared by every module, but inlining would give each module its
/// own copy. Constant variables cannot change, so a copy is equivalent.
fn reject_package_state(
    name: &str,
    items: &[(Vec<sv_parser::AttributeInstance>, sv_parser::PackageItem)],
    syntax_tree: &SyntaxTree,
) -> Result<(), AnalyzerError> {
    use sv_parser::{DataDeclaration, PackageItem, PackageOrGenerateItemDeclaration};
    for (_, item) in items {
        let PackageItem::PackageOrGenerateItemDeclaration(declaration) = item else {
            continue;
        };
        let state = match declaration.as_ref() {
            PackageOrGenerateItemDeclaration::NetDeclaration(net) => {
                Some(RefNode::NetDeclaration(net))
            }
            PackageOrGenerateItemDeclaration::DataDeclaration(data) => match data.as_ref() {
                DataDeclaration::Variable(variable) if variable.nodes.0.is_none() => {
                    Some(RefNode::DataDeclarationVariable(variable))
                }
                _ => None,
            },
            _ => None,
        };
        let Some(state) = state else {
            continue;
        };
        let identifier = state.into_iter().find_map(|node| match node {
            RefNode::VariableIdentifier(_) | RefNode::NetIdentifier(_) => {
                identifier_text(node, syntax_tree)
            }
            _ => None,
        });
        return Err(AnalyzerError::Unsupported(format!(
            "package variable or net `{name}::{}`",
            identifier.as_deref().unwrap_or("?")
        )));
    }
    Ok(())
}

/// Add `name` and, before it, the packages it depends on (each once).
fn visit<'a>(
    name: &'a str,
    packages: &'a HashMap<String, PackageSource>,
    order: &mut Vec<&'a str>,
    active: &mut Vec<&'a str>,
) {
    if order.contains(&name) || active.contains(&name) {
        return;
    }
    let Some((key, package)) = packages.get_key_value(name) else {
        return;
    };
    active.push(key);
    for dependency in &package.uses {
        visit(dependency, packages, order, active);
    }
    active.pop();
    order.push(key);
}

/// `code` with the packages used by `module_name` inlined into it, or `None`
/// when the module uses no package.
pub fn inline_packages(
    code: &str,
    syntax_tree: &SyntaxTree,
    module_name: &str,
    packages: &HashMap<String, PackageSource>,
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
        let (mut edits, used) =
            package_edits(RefNode::ModuleDeclarationAnsi(module), syntax_tree, false);
        if !used.iter().any(|name| packages.contains_key(name)) {
            return Ok(None);
        }
        // Dependencies first, each package once.
        let mut order = Vec::new();
        let mut active = Vec::new();
        for name in &used {
            visit(name, packages, &mut order, &mut active);
        }
        let (Some((module_start, module_end)), Some((footer_start, _))) = (
            node_span(RefNode::ModuleDeclarationAnsi(module), syntax_tree),
            node_span(RefNode::Keyword(&module.nodes.3), syntax_tree),
        ) else {
            return Ok(None);
        };
        let inlined = order
            .iter()
            .map(|name| {
                let package = &packages[*name];
                apply_edits(
                    &package.body,
                    (0, package.body.len()),
                    package.edits.clone(),
                    packages,
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        edits.push(edit(
            footer_start,
            footer_start,
            &format!("\n{inlined}\n"),
            None,
        ));
        let module_text = apply_edits(code, (module_start, module_end), edits, packages);
        return Ok(Some(format!(
            "{}{}{}",
            &code[..module_start],
            module_text,
            &code[module_end..]
        )));
    }
    Ok(None)
}

/// `code` with the `parameter type` defaults of `module_name` replaced by the
/// data types an instantiation binds them to, or `None` when `overrides` names
/// no type parameter of the module.
pub fn apply_type_parameter_overrides(
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
                        edits.push(edit(start, end, bound, None));
                    }
                }
                None => {
                    if let Some((_, end)) =
                        node_span(RefNode::TypeIdentifier(&assignment.nodes.0), syntax_tree)
                    {
                        edits.push(edit(end, end, &format!(" = {bound}"), None));
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
        let module_text = apply_edits(code, (start, end), edits, &HashMap::default());
        return Ok(Some(format!(
            "{}{}{}",
            &code[..start],
            module_text,
            &code[end..]
        )));
    }
    Ok(None)
}
