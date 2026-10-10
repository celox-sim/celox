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
    /// Every `p::x` reference, as `(p, x)`. `p` may also name a class.
    pub qualified: Vec<(String, String)>,
    /// The package exports (IEEE 1800-2023 26.6).
    pub exports: Vec<Export>,
}

/// A package export declaration item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Export {
    /// `export *::*;`
    All,
    /// `export p::*;`
    Wildcard(String),
    /// `export p::x;`, as `(p, x)`.
    Item(String, String),
}

impl ScopeImports {
    pub fn from_node(node: RefNode<'_>, tree: &SyntaxTree) -> Self {
        let mut imports = Self::default();
        let note = |packages: &mut Vec<String>, name: String| {
            if !packages.contains(&name) {
                packages.push(name);
            }
        };
        // The identifier a package scope qualifies follows the scope and its
        // package identifier.
        let mut scope: Option<String> = None;
        let mut skip = 0;
        // The items of an export name what it exports, not imports.
        let mut exported = HashSet::default();
        for child in node.clone() {
            let RefNode::PackageExportDeclaration(export) = child else {
                continue;
            };
            match export {
                sv_parser::PackageExportDeclaration::Asterisk(_) => {
                    imports.exports.push(Export::All);
                }
                sv_parser::PackageExportDeclaration::Item(_) => {
                    for item in RefNode::PackageExportDeclaration(export) {
                        let RefNode::PackageImportItem(item) = item else {
                            continue;
                        };
                        exported.insert(item as *const sv_parser::PackageImportItem);
                        let export = match item {
                            sv_parser::PackageImportItem::Identifier(item) => {
                                identifier_text(RefNode::PackageIdentifier(&item.nodes.0), tree)
                                    .zip(identifier_text(RefNode::Identifier(&item.nodes.2), tree))
                                    .map(|(package, name)| Export::Item(package, name))
                            }
                            sv_parser::PackageImportItem::Asterisk(item) => {
                                identifier_text(RefNode::PackageIdentifier(&item.nodes.0), tree)
                                    .map(Export::Wildcard)
                            }
                        };
                        if let Some(export) = export {
                            if let Export::Item(package, _) | Export::Wildcard(package) = &export {
                                note(&mut imports.packages, package.clone());
                            }
                            imports.exports.push(export);
                        }
                    }
                }
            }
        }
        for child in node {
            match child {
                RefNode::PackageImportItem(item)
                    if exported.contains(&(item as *const sv_parser::PackageImportItem)) => {}
                RefNode::SimpleIdentifier(_) | RefNode::EscapedIdentifier(_) => {
                    if skip > 0 {
                        skip -= 1;
                    } else if let Some(package) = scope.take()
                        && let Some(name) = identifier_text(child, tree)
                    {
                        imports.qualified.push((package, name));
                    }
                }
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
                RefNode::PackageScopePackage(package_scope) => {
                    if let Some(package) =
                        identifier_text(RefNode::PackageIdentifier(&package_scope.nodes.0), tree)
                    {
                        note(&mut imports.packages, package.clone());
                        scope = Some(package);
                        skip = 1;
                    }
                }
                RefNode::ClassScope(class_scope) => {
                    if let Some(package) = identifier_text(
                        RefNode::ClassIdentifier(&class_scope.nodes.0.nodes.0.nodes.1),
                        tree,
                    ) {
                        note(&mut imports.packages, package.clone());
                        scope = Some(package);
                        skip = 1;
                    }
                }
                // `p::t` as a data type parses as a class type.
                RefNode::ClassType(class_type) if !class_type.nodes.2.is_empty() => {
                    if let Some(package) =
                        identifier_text(RefNode::PsClassIdentifier(&class_type.nodes.0), tree)
                    {
                        note(&mut imports.packages, package.clone());
                        if let [(_, member, None)] = class_type.nodes.2.as_slice()
                            && class_type.nodes.1.is_none()
                            && let Some(name) =
                                identifier_text(RefNode::ClassIdentifier(member), tree)
                        {
                            imports.qualified.push((package, name));
                        }
                    }
                }
                _ => {}
            }
        }
        imports
    }
}

/// The names declared directly in a module or package scope: its ports and
/// parameters, and the declarations and instances of its items. Names
/// declared in nested scopes, such as structure members, function locals
/// and generate blocks, are left out.
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
        let node = RefNode::PackageOrGenerateItemDeclaration(declaration);
        match declaration {
            sv_parser::PackageOrGenerateItemDeclaration::FunctionDeclaration(_) => {
                if let Some(name) = unwrap_node!(node, FunctionIdentifier) {
                    add(name);
                }
                continue;
            }
            sv_parser::PackageOrGenerateItemDeclaration::TaskDeclaration(_) => {
                if let Some(name) = unwrap_node!(node, TaskIdentifier) {
                    add(name);
                }
                continue;
            }
            sv_parser::PackageOrGenerateItemDeclaration::DpiImportExport(_) => {
                if let Some(name) = unwrap_node!(node, FunctionIdentifier, TaskIdentifier) {
                    add(name);
                }
                continue;
            }
            _ => {}
        }
        // Structure and union members are declared in the member name space
        // of their type.
        let mut members = HashSet::default();
        for child in node.clone() {
            if let RefNode::StructUnionMember(_) = child {
                for member in child {
                    if let RefNode::Locate(locate) = member {
                        members.insert(locate.offset);
                    }
                }
            }
        }
        let mut add_declared = |name: Option<RefNode<'_>>| {
            if let Some(name) = name
                && identifier_locate(name.clone())
                    .is_none_or(|locate| !members.contains(&locate.offset))
            {
                add(name);
            }
        };
        for child in node {
            match child {
                RefNode::VariableDeclAssignment(_) => {
                    add_declared(unwrap_node!(child, VariableIdentifier))
                }
                RefNode::NetDeclAssignment(_) => add_declared(unwrap_node!(child, NetIdentifier)),
                RefNode::ParamAssignment(assignment) => {
                    add_declared(Some(RefNode::ParameterIdentifier(&assignment.nodes.0)))
                }
                RefNode::TypeAssignment(assignment) => {
                    add_declared(Some(RefNode::TypeIdentifier(&assignment.nodes.0)))
                }
                RefNode::TypeDeclarationDataType(declaration) => {
                    add_declared(Some(RefNode::TypeIdentifier(&declaration.nodes.2)))
                }
                RefNode::EnumNameDeclaration(member) => {
                    add_declared(Some(RefNode::EnumIdentifier(&member.nodes.0)))
                }
                RefNode::GenvarIdentifier(_) => add_declared(Some(child)),
                _ => {}
            }
        }
    }
    names
}

/// The names a scope looks up without a package scope: the first identifier
/// of each name reference in an expression, a constant, a data type or a
/// call. Declarations, formal names of connections, members and identifiers
/// qualified by a package scope are not looked up.
pub(super) fn unqualified_names(node: RefNode<'_>, tree: &SyntaxTree) -> HashSet<String> {
    let identifiers = |node: RefNode<'_>| {
        node.into_iter()
            .filter_map(|node| match node {
                RefNode::SimpleIdentifier(_) | RefNode::EscapedIdentifier(_) => {
                    identifier_locate(node)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    // A package scope is followed by its package identifier and then by the
    // identifier it qualifies; `p::t` as a data type parses as the class type
    // `p` with the member `t`.
    let mut qualified = HashSet::default();
    let mut skip = 0;
    for child in node.clone() {
        match child {
            RefNode::PackageScopePackage(_) | RefNode::ClassScope(_) => skip = 2,
            RefNode::ClassType(class_type) if !class_type.nodes.2.is_empty() => {
                qualified.extend(identifiers(child).iter().map(|locate| locate.offset));
            }
            RefNode::SimpleIdentifier(_) | RefNode::EscapedIdentifier(_) if skip > 0 => {
                skip -= 1;
                if let Some(locate) = identifier_locate(child) {
                    qualified.insert(locate.offset);
                }
            }
            _ => {}
        }
    }
    let mut names = HashSet::default();
    for child in node {
        if !matches!(
            child,
            RefNode::HierarchicalIdentifier(_)
                | RefNode::PsParameterIdentifier(_)
                | RefNode::PsTypeIdentifier(_)
                | RefNode::PsOrHierarchicalTfIdentifier(_)
                | RefNode::PsClassIdentifier(_)
                | RefNode::DataTypeType(_)
        ) {
            continue;
        }
        if let Some(first) = identifiers(child).first()
            && !qualified.contains(&first.offset)
            && let Some(name) = tree
                .get_str(first)
                .map(super::instances::normalize_identifier)
        {
            names.insert(name);
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
    Ok(resolve_imports(node, tree, packages)?.symbols)
}

/// The imports of a scope, resolved.
pub(super) struct ResolvedImports {
    /// The symbols the scope starts from (see [`imported_symbols`]).
    pub symbols: ScopeSymbols,
    /// The names a package exports, and the qualified names of the
    /// declarations they denote (IEEE 1800-2023 26.6).
    pub exports: HashMap<String, String>,
}

/// A name an import binds: the declaration it denotes, and the imported
/// packages it is imported through.
struct Binding {
    name: String,
    target: String,
    through: Vec<String>,
}

pub(super) fn resolve_imports(
    node: RefNode<'_>,
    tree: &SyntaxTree,
    packages: &packages::Packages,
) -> Result<ResolvedImports, AnalyzerError> {
    let imports = ScopeImports::from_node(node.clone(), tree);
    let package = |name: &String| {
        packages
            .get(name)
            .ok_or_else(|| AnalyzerError::UnknownPackage { name: name.clone() })
    };
    // A package scope may also be a class scope, but an import or an export
    // names a package.
    for name in imports
        .explicit
        .iter()
        .map(|(package, _)| package)
        .chain(&imports.wildcard)
        .chain(imports.exports.iter().filter_map(|export| match export {
            Export::All => None,
            Export::Wildcard(package) | Export::Item(package, _) => Some(package),
        }))
    {
        package(name)?;
    }
    // A package may name its own items through its scope.
    let own = match &node {
        RefNode::PackageDeclaration(_) => scope_name_from_node(node.clone(), tree).ok(),
        _ => None,
    };
    let mut own_names = None;
    for (package_name, name) in &imports.qualified {
        if own.as_ref() == Some(package_name) {
            // The package names one of its own items.
            let own_names = own_names.get_or_insert_with(|| scope_names(node.clone(), tree));
            if !own_names.contains(name) {
                return Err(AnalyzerError::UnknownPackageItem {
                    package: package_name.clone(),
                    name: name.clone(),
                });
            }
            continue;
        }
        // A qualified name names a declaration of the package; the names it
        // exports are visible only through imports (IEEE 1800-2023 26.6).
        if !package(package_name)?.declares(name) {
            return Err(AnalyzerError::UnknownPackageItem {
                package: package_name.clone(),
                name: name.clone(),
            });
        }
    }
    let mut symbols = ScopeSymbols::default();
    let mut used = Vec::new();
    for package in &imports.packages {
        packages.closure(package, &mut used);
    }
    if used.is_empty() {
        return Ok(ResolvedImports {
            symbols,
            exports: HashMap::default(),
        });
    }
    for package in &used {
        if let Some(package) = packages.get(package) {
            symbols.extend(&package.symbols);
        }
    }
    let local = scope_names(node.clone(), tree);
    // The declaration `package_name::name` denotes, for an import.
    let provided = |package_name: &String, name: &String| -> Result<String, AnalyzerError> {
        package(package_name)?
            .provides(name)
            .ok_or_else(|| AnalyzerError::UnknownPackageItem {
                package: package_name.clone(),
                name: name.clone(),
            })
    };
    let mut bindings: Vec<Binding> = Vec::new();
    // An explicit import of a name the scope declares, or imports from
    // another declaration, is illegal (IEEE 1800-2023 26.3); importing one
    // declaration through several packages is not (26.6).
    let bind_explicitly = |bindings: &mut Vec<Binding>,
                           package_name: &String,
                           name: &String,
                           form: &str|
     -> Result<(), AnalyzerError> {
        let target = provided(package_name, name)?;
        if local.contains(name) {
            return Err(AnalyzerError::ImportConflict {
                name: name.clone(),
                detail: format!(
                    "`{form} {package_name}::{name};` names an item the scope declares"
                ),
            });
        }
        if let Some(binding) = bindings.iter_mut().find(|binding| binding.name == *name) {
            if binding.target != target {
                return Err(AnalyzerError::ImportConflict {
                    name: name.clone(),
                    detail: format!(
                        "explicitly imported as both `{}` and `{target}`",
                        binding.target
                    ),
                });
            }
            if !binding.through.contains(package_name) {
                binding.through.push(package_name.clone());
            }
            return Ok(());
        }
        bindings.push(Binding {
            name: name.clone(),
            target,
            through: vec![package_name.clone()],
        });
        Ok(())
    };
    for (package_name, name) in &imports.explicit {
        bind_explicitly(&mut bindings, package_name, name, "import")?;
    }
    // The packages a wildcard import makes `name` a candidate through, and
    // the one declaration they provide.
    let candidates = |name: &String| -> Result<Option<(String, Vec<String>)>, AnalyzerError> {
        let mut found: Option<(String, Vec<String>)> = None;
        for package_name in &imports.wildcard {
            let Some(target) = package(package_name)?.provides(name) else {
                continue;
            };
            match &mut found {
                None => found = Some((target, vec![package_name.clone()])),
                Some((known, through)) if *known == target => through.push(package_name.clone()),
                Some((_, through)) => {
                    return Err(AnalyzerError::ImportConflict {
                        name: name.clone(),
                        detail: format!(
                            "declared by both wildcard-imported packages `{}` and `{package_name}`",
                            through[0]
                        ),
                    });
                }
            }
        }
        Ok(found)
    };
    if !imports.wildcard.is_empty() {
        for name in unqualified_names(node, tree) {
            if local.contains(&name) {
                continue;
            }
            if let Some(binding) = bindings.iter_mut().find(|binding| binding.name == name) {
                // A wildcard import of the explicitly imported declaration
                // imports it through that package too.
                for package_name in &imports.wildcard {
                    if package(package_name)?.provides(&name).as_ref() == Some(&binding.target)
                        && !binding.through.contains(package_name)
                    {
                        binding.through.push(package_name.clone());
                    }
                }
                continue;
            }
            if let Some((target, through)) = candidates(&name)? {
                bindings.push(Binding {
                    name,
                    target,
                    through,
                });
            }
        }
    }
    // `export p::x;` refers to `x`: it imports a candidate the scope does
    // not otherwise reference, like an explicit import (IEEE 1800-2023 26.6).
    for export in &imports.exports {
        let Export::Item(package_name, name) = export else {
            continue;
        };
        if local.contains(name) {
            return Err(AnalyzerError::ImportConflict {
                name: name.clone(),
                detail: format!(
                    "`export {package_name}::{name};` names an item the package declares"
                ),
            });
        }
        let target = provided(package_name, name)?;
        let imported_through = imports
            .explicit
            .iter()
            .any(|(explicit, item)| explicit == package_name && item == name)
            || imports.wildcard.contains(package_name);
        if !imported_through {
            return Err(AnalyzerError::ImportConflict {
                name: name.clone(),
                detail: format!(
                    "`export {package_name}::{name};` exports a name the package does not \
                     import from `{package_name}`"
                ),
            });
        }
        match bindings.iter_mut().find(|binding| binding.name == *name) {
            Some(binding) if binding.target != target => {
                return Err(AnalyzerError::ImportConflict {
                    name: name.clone(),
                    detail: format!(
                        "`export {package_name}::{name};` exports `{target}`, but the package \
                         imports `{}`",
                        binding.target
                    ),
                });
            }
            Some(binding) => {
                if !binding.through.contains(package_name) {
                    binding.through.push(package_name.clone());
                }
            }
            None => bind_explicitly(&mut bindings, package_name, name, "export")?,
        }
    }
    let mut exports = HashMap::default();
    for export in &imports.exports {
        for binding in &bindings {
            let exported = match export {
                Export::All => true,
                Export::Wildcard(package_name) => binding.through.contains(package_name),
                Export::Item(package_name, name) => {
                    binding.name == *name && binding.through.contains(package_name)
                }
            };
            if exported {
                exports.insert(binding.name.clone(), binding.target.clone());
            }
        }
    }
    for binding in &bindings {
        symbols.alias(&binding.name, &binding.target);
    }
    Ok(ResolvedImports { symbols, exports })
}
