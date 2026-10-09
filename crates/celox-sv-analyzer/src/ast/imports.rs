//! Package imports (IEEE 1800-2023 26.3).
//!
//! An explicit import `import p::x;` makes `x` visible in the importing
//! scope. A wildcard import `import p::*;` makes each name declared in `p`
//! potentially visible: a reference that no declaration of the scope matches
//! binds to it. A name that two wildcard-imported packages declare is an
//! error only when a reference matches it.
//!
//! The names a scope binds this way become aliases of the package items in
//! the symbols the scope starts from. Names declared in nested scopes still
//! shadow them, since nested declarations are renamed apart before names are
//! looked up.

use super::scope::ScopeSymbols;
use super::*;

/// The imports of a scope, and the packages it refers to through a scope.
#[derive(Debug, Default)]
pub(super) struct ScopeImports {
    /// `import p::x;`, as `(p, x)`.
    pub explicit: Vec<(String, String)>,
    /// `import p::*;`
    pub wildcard: Vec<String>,
    /// Every package named by an import or a package scope.
    pub packages: Vec<String>,
}

impl ScopeImports {
    pub fn from_node(node: RefNode<'_>, tree: &SyntaxTree) -> Self {
        let mut imports = Self::default();
        let note = |packages: &mut Vec<String>, name: String| {
            if !packages.contains(&name) {
                packages.push(name);
            }
        };
        for child in node {
            match child {
                RefNode::PackageImportItem(sv_parser::PackageImportItem::Identifier(item)) => {
                    let (Some(package), Some(name)) = (
                        identifier_text(RefNode::PackageIdentifier(&item.nodes.0), tree),
                        identifier_text(RefNode::Identifier(&item.nodes.2), tree),
                    ) else {
                        continue;
                    };
                    note(&mut imports.packages, package.clone());
                    imports.explicit.push((package, name));
                }
                RefNode::PackageImportItem(sv_parser::PackageImportItem::Asterisk(item)) => {
                    let Some(package) =
                        identifier_text(RefNode::PackageIdentifier(&item.nodes.0), tree)
                    else {
                        continue;
                    };
                    note(&mut imports.packages, package.clone());
                    if !imports.wildcard.contains(&package) {
                        imports.wildcard.push(package);
                    }
                }
                RefNode::PackageScopePackage(scope) => {
                    if let Some(package) =
                        identifier_text(RefNode::PackageIdentifier(&scope.nodes.0), tree)
                    {
                        note(&mut imports.packages, package);
                    }
                }
                RefNode::ClassScope(scope) => {
                    if let Some(package) = identifier_text(
                        RefNode::ClassIdentifier(&scope.nodes.0.nodes.0.nodes.1),
                        tree,
                    ) {
                        note(&mut imports.packages, package);
                    }
                }
                // `p::t` as a data type parses as a class type.
                RefNode::ClassType(class_type) if !class_type.nodes.2.is_empty() => {
                    if let Some(package) =
                        identifier_text(RefNode::PsClassIdentifier(&class_type.nodes.0), tree)
                    {
                        note(&mut imports.packages, package);
                    }
                }
                _ => {}
            }
        }
        imports
    }
}

/// The names declared directly in a module or package scope: its ports and
/// parameters, the declarations of its items, its instances, and the
/// functions and tasks it declares anywhere.
pub(super) fn scope_names(node: RefNode<'_>, tree: &SyntaxTree) -> HashSet<String> {
    let mut names = HashSet::default();
    let mut add = |node: RefNode<'_>| {
        if let Some(name) = identifier_text(node, tree) {
            names.insert(name);
        }
    };
    if let Some(list) = module_parameter_port_list(node.clone()) {
        for child in list {
            match child {
                RefNode::ParamAssignment(assignment) => {
                    add(RefNode::ParameterIdentifier(&assignment.nodes.0))
                }
                RefNode::TypeAssignment(assignment) => {
                    add(RefNode::TypeIdentifier(&assignment.nodes.0))
                }
                _ => {}
            }
        }
    }
    if let RefNode::ModuleDeclarationAnsi(module) = &node
        && let Some(ports) = &module.nodes.0.nodes.6
    {
        for child in RefNode::ListOfPortDeclarations(ports) {
            if let RefNode::PortIdentifier(_) = child {
                add(child);
            }
        }
    }
    for child in node.clone() {
        let name = match child {
            RefNode::FunctionBodyDeclaration(_) => unwrap_node!(child, FunctionIdentifier),
            RefNode::TaskBodyDeclaration(_) => unwrap_node!(child, TaskIdentifier),
            RefNode::DpiImportExport(_) => {
                unwrap_node!(child, FunctionIdentifier, TaskIdentifier)
            }
            _ => continue,
        };
        if let Some(name) = name {
            add(name);
        }
    }
    for item in scope_items(node) {
        if let ScopeItem::Module(module_item) = item
            && let Some(RefNode::HierarchicalInstance(_)) = unwrap_node!(
                RefNode::ModuleOrGenerateItem(module_item),
                HierarchicalInstance
            )
        {
            for child in RefNode::ModuleOrGenerateItem(module_item) {
                if let RefNode::InstanceIdentifier(_) = child {
                    add(child);
                }
            }
        }
        let Some(declaration) = item.declaration() else {
            continue;
        };
        for child in RefNode::PackageOrGenerateItemDeclaration(declaration) {
            match child {
                // Declarations nested in a function or task belong to it.
                RefNode::FunctionDeclaration(_) | RefNode::TaskDeclaration(_) => break,
                RefNode::VariableIdentifier(_) | RefNode::NetIdentifier(_) => add(child),
                RefNode::ParamAssignment(assignment) => {
                    add(RefNode::ParameterIdentifier(&assignment.nodes.0))
                }
                RefNode::TypeAssignment(assignment) => {
                    add(RefNode::TypeIdentifier(&assignment.nodes.0))
                }
                RefNode::TypeDeclarationDataType(declaration) => {
                    add(RefNode::TypeIdentifier(&declaration.nodes.2))
                }
                RefNode::EnumNameDeclaration(member) => {
                    add(RefNode::EnumIdentifier(&member.nodes.0))
                }
                RefNode::GenvarIdentifier(_) => add(child),
                _ => {}
            }
        }
    }
    names
}

/// The identifiers a scope uses without a package scope, and declares.
/// Identifiers qualified by a package scope are left out.
pub(super) fn unqualified_names(node: RefNode<'_>, tree: &SyntaxTree) -> HashSet<String> {
    let mut names = HashSet::default();
    // A package scope is followed by its package identifier and then by the
    // identifier it qualifies.
    let mut skip = 0;
    for child in node {
        match child {
            RefNode::PackageScopePackage(_) | RefNode::ClassScope(_) => skip = 2,
            RefNode::PackageImportItem(sv_parser::PackageImportItem::Identifier(_)) => skip = 2,
            RefNode::PackageImportItem(sv_parser::PackageImportItem::Asterisk(_)) => skip = 1,
            RefNode::SimpleIdentifier(_) | RefNode::EscapedIdentifier(_) => {
                if skip > 0 {
                    skip -= 1;
                } else if let Some(name) = identifier_text(child, tree) {
                    names.insert(name);
                }
            }
            _ => {}
        }
    }
    names
}

/// The symbols a scope starts from: those of every package it uses, directly
/// or through another package, and an alias for each name its imports bind.
pub(super) fn imported_symbols(
    node: RefNode<'_>,
    tree: &SyntaxTree,
    packages: &packages::Packages,
) -> Result<ScopeSymbols, AnalyzerError> {
    let imports = ScopeImports::from_node(node.clone(), tree);
    // A package scope may also be a class scope, but an import names a
    // package.
    if let Some(package) = imports
        .explicit
        .iter()
        .map(|(package, _)| package)
        .chain(&imports.wildcard)
        .find(|package| packages.get(package).is_none())
    {
        return Err(AnalyzerError::UnknownPackage {
            name: package.clone(),
        });
    }
    let mut symbols = ScopeSymbols::default();
    let mut used = Vec::new();
    for package in &imports.packages {
        packages.closure(package, &mut used);
    }
    if used.is_empty() {
        return Ok(symbols);
    }
    for package in &used {
        if let Some(package) = packages.get(package) {
            symbols.extend(&package.symbols);
        }
    }
    let local = scope_names(node.clone(), tree);
    let mut bindings: Vec<(String, String)> = Vec::new();
    for (package, name) in &imports.explicit {
        let package_symbols =
            packages
                .get(package)
                .ok_or_else(|| AnalyzerError::UnknownPackage {
                    name: package.clone(),
                })?;
        if !package_symbols.declares(name) {
            return Err(AnalyzerError::UnknownPackageItem {
                package: package.clone(),
                name: name.clone(),
            });
        }
        let target = format!("{package}::{name}");
        // An explicit import of a name the scope declares, or imports from
        // another package, is illegal (IEEE 1800-2023 26.3).
        if local.contains(name) {
            return Err(AnalyzerError::ImportConflict {
                name: name.clone(),
                detail: format!("`import {target};` names an item the scope declares"),
            });
        }
        if let Some((_, other)) = bindings.iter().find(|(bound, _)| bound == name) {
            if *other != target {
                return Err(AnalyzerError::ImportConflict {
                    name: name.clone(),
                    detail: format!("explicitly imported from both `{other}` and `{target}`"),
                });
            }
            continue;
        }
        bindings.push((name.clone(), target));
    }
    if !imports.wildcard.is_empty() {
        for name in unqualified_names(node, tree) {
            if local.contains(&name) || bindings.iter().any(|(bound, _)| *bound == name) {
                continue;
            }
            let mut candidates = Vec::new();
            for package in &imports.wildcard {
                let package_symbols =
                    packages
                        .get(package)
                        .ok_or_else(|| AnalyzerError::UnknownPackage {
                            name: package.clone(),
                        })?;
                if package_symbols.declares(&name) {
                    candidates.push(package);
                }
            }
            match candidates.as_slice() {
                [] => {}
                [package] => bindings.push((name.clone(), format!("{package}::{name}"))),
                [first, second, ..] => {
                    return Err(AnalyzerError::ImportConflict {
                        name: name.clone(),
                        detail: format!(
                            "declared by both wildcard-imported packages `{first}` and `{second}`"
                        ),
                    });
                }
            }
        }
    }
    for (name, target) in &bindings {
        symbols.alias(name, target);
    }
    Ok(symbols)
}
