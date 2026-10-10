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
    /// The source offset of each `export p::x;` item, by `(p, x)`.
    pub export_offsets: HashMap<(String, String), usize>,
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
        // An import in a generate block belongs to that block (see
        // `generate_scopes`); its package is still one the module uses.
        let blocks = generate_blocks(node.clone());
        let in_block = |item: RefNode<'_>| {
            let offset = node_range(item).0;
            blocks
                .iter()
                .any(|(start, end)| (*start..*end).contains(&offset))
        };
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
                            if let Export::Item(package, name) = &export
                                && let Some(offset) = RefNode::PackageImportItem(item)
                                    .into_iter()
                                    .find_map(|node| match node {
                                        RefNode::Locate(locate) => Some(locate.offset),
                                        _ => None,
                                    })
                            {
                                // The first export of a name is the one a
                                // later reference sees.
                                imports
                                    .export_offsets
                                    .entry((package.clone(), name.clone()))
                                    .or_insert(offset);
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
                    if !in_block(RefNode::PackageImportItemIdentifier(item)) {
                        imports.explicit.push((package, name));
                    }
                }
                RefNode::PackageImportItem(sv_parser::PackageImportItem::Asterisk(item)) => {
                    let Some(package) =
                        identifier_text(RefNode::PackageIdentifier(&item.nodes.0), tree)
                    else {
                        continue;
                    };
                    note(&mut imports.packages, package.clone());
                    if !in_block(RefNode::PackageImportItemAsterisk(item))
                        && !imports.wildcard.contains(&package)
                    {
                        imports.wildcard.push(package);
                    }
                }
                // `$unit::x` names an item of the compilation unit.
                RefNode::PackageScope(sv_parser::PackageScope::Unit(_)) => {
                    scope = Some(packages::UNIT.to_string());
                    skip = 0;
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
/// qualified by a package scope are not looked up, nor are names a nested
/// subroutine or block declares, within it. Each name maps to the source
/// offsets of its references, in order.
pub(super) fn unqualified_names(
    node: RefNode<'_>,
    tree: &SyntaxTree,
) -> HashMap<String, Vec<usize>> {
    let nested = nested_declarations(node.clone(), tree);
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
            RefNode::PackageScope(sv_parser::PackageScope::Unit(_)) => skip = 1,
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
    let mut names = HashMap::default();
    for child in node {
        if !matches!(
            child,
            RefNode::HierarchicalIdentifier(_)
                | RefNode::PsParameterIdentifier(_)
                | RefNode::PsTypeIdentifier(_)
                | RefNode::PsOrHierarchicalTfIdentifier(_)
                // The target of a continuous assignment, and the array of a
                // `foreach` loop.
                | RefNode::PsOrHierarchicalNetIdentifier(_)
                | RefNode::PsOrHierarchicalArrayIdentifier(_)
                | RefNode::PsClassIdentifier(_)
                | RefNode::DataTypeType(_)
                // `t v;` with a type `t` declares a net of a user-defined
                // net type as far as the syntax tells.
                | RefNode::NetTypeIdentifier(_)
        ) {
            continue;
        }
        if let Some(first) = identifiers(child).first()
            && !qualified.contains(&first.offset)
            && let Some(name) = tree
                .get_str(first)
                .map(super::instances::normalize_identifier)
            && !nested.iter().any(|(start, end, declared)| {
                (*start..*end).contains(&first.offset) && declared.contains(&name)
            })
        {
            names
                .entry(name)
                .or_insert_with(Vec::new)
                .push(first.offset);
        }
    }
    names
}

/// The source range of `node`: from its first token to the end of its last.
pub(super) fn node_range(node: RefNode<'_>) -> (usize, usize) {
    let mut start = usize::MAX;
    let mut end = 0;
    for child in node {
        if let RefNode::Locate(locate) = child {
            start = start.min(locate.offset);
            end = end.max(locate.offset + locate.len);
        }
    }
    (start, end)
}

/// The source ranges of the generate blocks under a module `node`: each is a
/// scope of its own (IEEE 1800-2023 27.5).
fn generate_blocks(node: RefNode<'_>) -> Vec<(usize, usize)> {
    if !matches!(
        node,
        RefNode::ModuleDeclarationAnsi(_) | RefNode::ModuleDeclarationNonansi(_)
    ) {
        return Vec::new();
    }
    node.into_iter()
        .filter_map(|child| match child {
            RefNode::GenerateBlock(sv_parser::GenerateBlock::Multiple(_)) => {
                Some(node_range(child))
            }
            // A single item is a scope too, except a conditional generate
            // directly in a conditional one, which continues its scope.
            RefNode::GenerateBlock(sv_parser::GenerateBlock::GenerateItem(item))
                if !matches!(
                    &**item,
                    sv_parser::GenerateItem::ModuleOrGenerateItem(common)
                        if matches!(
                            &**common,
                            sv_parser::ModuleOrGenerateItem::ModuleItem(common)
                                if matches!(
                                    common.nodes.1,
                                    sv_parser::ModuleCommonItem::ConditionalGenerateConstruct(_)
                                )
                        )
                ) =>
            {
                Some(node_range(child))
            }
            _ => None,
        })
        .collect()
}

/// A generate block: its imports, and the names it declares itself.
struct GenerateScope {
    start: usize,
    end: usize,
    /// `import p::x;`, as `(p, x)`.
    explicit: Vec<(String, String)>,
    /// `import p::*;`
    wildcard: Vec<String>,
    declared: HashSet<String>,
}

impl GenerateScope {
    fn contains(&self, offset: usize) -> bool {
        (self.start..self.end).contains(&offset)
    }
}

/// The generate blocks of a module `node`, each with the imports and
/// declarations that are its own rather than those of a block, function or
/// task nested in it.
fn generate_scopes(node: RefNode<'_>, tree: &SyntaxTree) -> Vec<GenerateScope> {
    let blocks = generate_blocks(node.clone());
    if blocks.is_empty() {
        return Vec::new();
    }
    let subroutines: Vec<(usize, usize)> = node
        .clone()
        .into_iter()
        .filter(|child| {
            matches!(
                child,
                RefNode::FunctionDeclaration(_)
                    | RefNode::TaskDeclaration(_)
                    | RefNode::SeqBlock(_)
            )
        })
        .map(node_range)
        .collect();
    let mut scopes: Vec<GenerateScope> = blocks
        .iter()
        .map(|&(start, end)| GenerateScope {
            start,
            end,
            explicit: Vec::new(),
            wildcard: Vec::new(),
            declared: HashSet::default(),
        })
        .collect();
    // The innermost block that contains `offset`, outside any subroutine.
    let owner = |offset: usize, nested: &[(usize, usize)]| -> Option<usize> {
        if nested
            .iter()
            .any(|(start, end)| (*start..*end).contains(&offset))
        {
            return None;
        }
        blocks
            .iter()
            .enumerate()
            .filter(|(_, (start, end))| (*start..*end).contains(&offset))
            .min_by_key(|(_, (start, end))| end - start)
            .map(|(index, _)| index)
    };
    for child in node {
        let (offset, nested) = match &child {
            // A subroutine's name is declared in the enclosing scope.
            RefNode::FunctionIdentifier(_) | RefNode::TaskIdentifier(_) => {
                (node_range(child.clone()).0, &[][..])
            }
            _ => (node_range(child.clone()).0, &subroutines[..]),
        };
        let name = match child {
            RefNode::PackageImportItem(sv_parser::PackageImportItem::Identifier(item)) => {
                if let Some(index) = owner(offset, nested)
                    && let (Some(package), Some(name)) = (
                        identifier_text(RefNode::PackageIdentifier(&item.nodes.0), tree),
                        identifier_text(RefNode::Identifier(&item.nodes.2), tree),
                    )
                {
                    scopes[index].explicit.push((package, name));
                }
                continue;
            }
            RefNode::PackageImportItem(sv_parser::PackageImportItem::Asterisk(item)) => {
                if let Some(index) = owner(offset, nested)
                    && let Some(package) =
                        identifier_text(RefNode::PackageIdentifier(&item.nodes.0), tree)
                    && !scopes[index].wildcard.contains(&package)
                {
                    scopes[index].wildcard.push(package);
                }
                continue;
            }
            RefNode::VariableDeclAssignment(_) => unwrap_node!(child, VariableIdentifier),
            RefNode::NetDeclAssignment(_) => unwrap_node!(child, NetIdentifier),
            RefNode::ParamAssignment(assignment) => {
                Some(RefNode::ParameterIdentifier(&assignment.nodes.0))
            }
            // `genvar i;`; a loop's own `genvar i` is the loop's.
            RefNode::GenvarDeclaration(declaration) => {
                if let Some(index) = owner(offset, nested) {
                    for genvar in RefNode::GenvarDeclaration(declaration) {
                        if let RefNode::GenvarIdentifier(_) = genvar
                            && let Some(name) = identifier_text(genvar, tree)
                        {
                            scopes[index].declared.insert(name);
                        }
                    }
                }
                continue;
            }
            RefNode::InstanceIdentifier(_)
            | RefNode::FunctionIdentifier(_)
            | RefNode::TaskIdentifier(_) => Some(child),
            _ => continue,
        };
        if let Some(index) = owner(offset, nested)
            && let Some(name) = name.and_then(|name| identifier_text(name, tree))
        {
            scopes[index].declared.insert(name);
        }
    }
    scopes
}

/// What a reference in a generate block names.
enum ScopeLookup {
    /// A declaration of a block around it.
    Declared,
    /// The declaration an import of a block around it binds: that block's
    /// source offset, and the declaration.
    Imported(usize, String),
    /// Nothing in the blocks around it: the module's scope decides.
    Outside,
}

/// Look `name`, referenced at `offset`, up in the generate blocks around the
/// reference, innermost first: in each, its declarations, then its explicit
/// imports, then its wildcard imports (IEEE 1800-2023 26.3).
fn scope_lookup(
    name: &String,
    offset: usize,
    scopes: &[GenerateScope],
    provided: &dyn Fn(&String, &String) -> Result<String, AnalyzerError>,
    packages: &packages::Packages,
) -> Result<ScopeLookup, AnalyzerError> {
    let mut around: Vec<&GenerateScope> = scopes
        .iter()
        .filter(|scope| scope.contains(offset))
        .collect();
    around.sort_by_key(|scope| scope.end - scope.start);
    for scope in around {
        if scope.declared.contains(name) {
            return Ok(ScopeLookup::Declared);
        }
        let mut explicit: Option<String> = None;
        for (package_name, _) in scope.explicit.iter().filter(|(_, item)| item == name) {
            let target = provided(package_name, name)?;
            match &explicit {
                Some(known) if *known != target => {
                    return Err(AnalyzerError::ImportConflict {
                        name: name.clone(),
                        detail: format!("explicitly imported as both `{known}` and `{target}`"),
                    });
                }
                _ => explicit = Some(target),
            }
        }
        if let Some(target) = explicit {
            return Ok(ScopeLookup::Imported(scope.start, target));
        }
        let mut found: Option<(String, &String)> = None;
        for package_name in &scope.wildcard {
            let Some(target) = packages
                .get(package_name)
                .ok_or_else(|| AnalyzerError::UnknownPackage {
                    name: package_name.clone(),
                })?
                .provides(name)
            else {
                continue;
            };
            match &found {
                Some((known, first)) if *known != target => {
                    return Err(AnalyzerError::ImportConflict {
                        name: name.clone(),
                        detail: format!(
                            "declared by both wildcard-imported packages `{first}` and \
                             `{package_name}`"
                        ),
                    });
                }
                Some(_) => {}
                None => found = Some((target, package_name)),
            }
        }
        if let Some((target, _)) = found {
            return Ok(ScopeLookup::Imported(scope.start, target));
        }
    }
    Ok(ScopeLookup::Outside)
}

/// The functions, tasks and blocks under `node` that declare names of their
/// own: their source ranges, and the names each declares itself, not in a
/// block nested in it.
fn nested_declarations(
    node: RefNode<'_>,
    tree: &SyntaxTree,
) -> Vec<(usize, usize, HashSet<String>)> {
    let range = |node: RefNode<'_>| {
        let mut start = usize::MAX;
        let mut end = 0;
        for child in node {
            if let RefNode::Locate(locate) = child {
                start = start.min(locate.offset);
                end = end.max(locate.offset + locate.len);
            }
        }
        (start, end)
    };
    let scopes: Vec<_> = node
        .into_iter()
        .filter(|child| {
            matches!(
                child,
                RefNode::FunctionDeclaration(_)
                    | RefNode::TaskDeclaration(_)
                    | RefNode::SeqBlock(_)
                    // A `for` loop variable is declared for the loop only.
                    | RefNode::LoopStatementFor(_)
            )
        })
        .map(|child| (range(child.clone()), child))
        .collect();
    let mut declarations = Vec::new();
    for ((start, end), scope) in &scopes {
        let inner: Vec<_> = scopes
            .iter()
            .map(|(range, _)| *range)
            .filter(|range| *range != (*start, *end) && *start <= range.0 && range.1 <= *end)
            .collect();
        let mut declared = HashSet::default();
        for child in scope.clone() {
            let name = match child {
                // A function's name denotes its result inside it.
                RefNode::FunctionIdentifier(_)
                | RefNode::VariableIdentifier(_)
                | RefNode::PortIdentifier(_)
                | RefNode::ParameterIdentifier(_) => child,
                RefNode::TypeDeclarationDataType(declaration) => {
                    RefNode::TypeIdentifier(&declaration.nodes.2)
                }
                RefNode::EnumNameDeclaration(member) => RefNode::EnumIdentifier(&member.nodes.0),
                _ => continue,
            };
            let Some(locate) = identifier_locate(name.clone()) else {
                continue;
            };
            if inner
                .iter()
                .any(|(inner_start, inner_end)| (*inner_start..*inner_end).contains(&locate.offset))
            {
                continue;
            }
            if let Some(name) = identifier_text(name, tree) {
                declared.insert(name);
            }
        }
        if !declared.is_empty() {
            declarations.push((*start, *end, declared));
        }
    }
    declarations
}

/// The symbols a scope starts from: those of every package it uses, directly
/// or through another package, and an alias for each name its imports bind.
/// `unit` is the compilation unit of the module's source file.
pub(super) fn imported_symbols(
    node: RefNode<'_>,
    tree: &SyntaxTree,
    packages: &packages::Packages,
    unit: Option<&packages::Unit>,
) -> Result<ScopeSymbols, AnalyzerError> {
    Ok(resolve_imports(node, tree, packages, unit)?.symbols)
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
    unit: Option<&packages::Unit>,
) -> Result<ResolvedImports, AnalyzerError> {
    let imports = ScopeImports::from_node(node.clone(), tree);
    // A module sees the compilation-unit items declared before it, which are
    // also those declared before any reference in it.
    let start = node
        .clone()
        .into_iter()
        .find_map(|child| match child {
            RefNode::Locate(locate) => Some(locate.offset),
            _ => None,
        })
        .unwrap_or_default();
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
    // So does an import of a generate block, whether or not a reference uses
    // it.
    let all_scopes = generate_scopes(node.clone(), tree);
    for scope in &all_scopes {
        for name in scope
            .explicit
            .iter()
            .map(|(package, _)| package)
            .chain(&scope.wildcard)
        {
            package(name)?;
        }
    }
    // A package may name its own items through its scope.
    let own = match &node {
        RefNode::PackageDeclaration(_) => scope_name_from_node(node.clone(), tree).ok(),
        _ => None,
    };
    let mut own_names = None;
    let mut exported_references = Vec::new();
    let mut own_exported: Vec<String> = Vec::new();
    // Whether the scope uses an item of its compilation unit.
    let mut uses_unit = false;
    for (package_name, name) in &imports.qualified {
        if own.as_ref() == Some(package_name) {
            // The package names one of its own items.
            let own_names = own_names.get_or_insert_with(|| scope_names(node.clone(), tree));
            // A name the package exports is known once its imports are.
            if !own_names.contains(name) && !own_exported.contains(name) {
                own_exported.push(name.clone());
            }
            continue;
        }
        if package_name == packages::UNIT {
            // `$unit` does not reach items declared later (IEEE 1800-2023
            // 3.12.1).
            if !unit.is_some_and(|unit| unit.declares_before(name, start)) {
                return Err(AnalyzerError::UnknownPackageItem {
                    package: package_name.clone(),
                    name: name.clone(),
                });
            }
            uses_unit = true;
            continue;
        }
        // A qualified name names a declaration of the package, or one it
        // exports: `q::x` then denotes the declaration `x` denotes in `q`
        // (IEEE 1800-2023 26.6).
        let package_symbols = package(package_name)?;
        if !package_symbols.declares(name) {
            let target = package_symbols.provides(name).ok_or_else(|| {
                AnalyzerError::UnknownPackageItem {
                    package: package_name.clone(),
                    name: name.clone(),
                }
            })?;
            let reference = (format!("{package_name}::{name}"), target);
            if !exported_references.contains(&reference) {
                exported_references.push(reference);
            }
        }
    }
    let mut symbols = ScopeSymbols::default();
    let mut used = Vec::new();
    for package in imports.packages.iter().chain(
        unit.iter()
            .flat_map(|unit| &unit.visibility.imports)
            .map(|(_, package, _)| package),
    ) {
        packages.closure(package, &mut used);
    }
    if used.is_empty() && unit.is_none() {
        if let (Some(own), Some(name)) = (&own, own_exported.first()) {
            return Err(AnalyzerError::UnknownPackageItem {
                package: own.clone(),
                name: name.clone(),
            });
        }
        return Ok(ResolvedImports {
            symbols,
            exports: HashMap::default(),
        });
    }
    for package in used.iter().filter_map(|package| packages.get(package)) {
        symbols.extend(&package.symbols);
    }
    for (reference, target) in &exported_references {
        symbols.alias(reference, target);
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
    let scoped_imports = all_scopes
        .iter()
        .any(|scope| !scope.explicit.is_empty() || !scope.wildcard.is_empty());
    let mut referenced = if imports.wildcard.is_empty() && unit.is_none() && !scoped_imports {
        HashMap::default()
    } else {
        unqualified_names(node.clone(), tree)
    };
    // A reference in a generate block is looked up in that block and the
    // blocks around it first (IEEE 1800-2023 26.3): the references the
    // imports of a block bind, by name, with the block and the declaration.
    let mut scoped: HashMap<String, Vec<(usize, String)>> = HashMap::default();
    if scoped_imports {
        for scope in &all_scopes {
            let mut explicit: HashMap<&String, String> = HashMap::default();
            for (package_name, name) in &scope.explicit {
                // Every explicit import names an item of its package, and one
                // declaration per name, whether or not a reference uses it.
                let target = provided(package_name, name)?;
                if let Some(known) = explicit.get(name)
                    && *known != target
                {
                    return Err(AnalyzerError::ImportConflict {
                        name: name.clone(),
                        detail: format!("explicitly imported as both `{known}` and `{target}`"),
                    });
                }
                explicit.insert(name, target);
                if scope.declared.contains(name) {
                    return Err(AnalyzerError::ImportConflict {
                        name: name.clone(),
                        detail: format!(
                            "`import {package_name}::{name};` names an item the generate block \
                             declares"
                        ),
                    });
                }
            }
        }
        for (name, offsets) in &mut referenced {
            let mut outer = Vec::new();
            for &offset in offsets.iter() {
                match scope_lookup(name, offset, &all_scopes, &provided, packages)? {
                    ScopeLookup::Declared => {}
                    ScopeLookup::Imported(block, target) => {
                        scoped
                            .entry(name.clone())
                            .or_default()
                            .push((block, target));
                    }
                    ScopeLookup::Outside => outer.push(offset),
                }
            }
            *offsets = outer;
        }
        referenced.retain(|_, offsets| !offsets.is_empty());
    }
    // `export p::x;` refers to `x`: it imports a candidate the scope does
    // not otherwise reference, like an explicit import (IEEE 1800-2023 26.6),
    // before other references bind names through wildcard imports.
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
            None => {
                // A reference before the export resolves without it.
                if let Some(export_offset) = imports
                    .export_offsets
                    .get(&(package_name.clone(), name.clone()))
                    && referenced
                        .get(name)
                        .and_then(|offsets| offsets.first())
                        .is_some_and(|reference| reference < export_offset)
                    && let Some((earlier, _)) = candidates(name)?
                    && earlier != target
                {
                    return Err(AnalyzerError::ImportConflict {
                        name: name.clone(),
                        detail: format!(
                            "referenced as `{earlier}` before `export {package_name}::{name};`"
                        ),
                    });
                }
                bind_explicitly(&mut bindings, package_name, name, "export")?
            }
        }
    }
    if !imports.wildcard.is_empty() {
        for name in referenced.keys().cloned() {
            if local.contains(&name) {
                continue;
            }
            // An explicitly imported name is not a wildcard candidate
            // (IEEE 1800-2023 26.3).
            if bindings.iter().any(|binding| binding.name == name) {
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
    // The compilation unit encloses the module: a name the module neither
    // declares nor imports is looked up among the unit's items declared
    // before it, and then through the unit's imports before it.
    if let Some(unit) = unit {
        // Explicit imports of one name into the unit must import one
        // declaration, whether or not the module uses it (IEEE 1800-2023
        // 26.3).
        let mut explicit: HashMap<&String, String> = HashMap::default();
        for (_, package_name, item) in &unit.visibility.imports {
            let Some(name) = item else {
                continue;
            };
            let target = provided(package_name, name)?;
            if unit.visibility.declared_at.contains_key(name) {
                return Err(AnalyzerError::ImportConflict {
                    name: name.clone(),
                    detail: format!(
                        "`import {target};` names an item the compilation unit declares"
                    ),
                });
            }
            if let Some(known) = explicit.get(name)
                && *known != target
            {
                return Err(AnalyzerError::ImportConflict {
                    name: name.clone(),
                    detail: format!("explicitly imported as both `{known}` and `{target}`"),
                });
            }
            explicit.insert(name, target);
        }
        let unit_imports: Vec<_> = unit
            .visibility
            .imports
            .iter()
            .filter(|(offset, ..)| *offset < start)
            .collect();
        // The declaration the unit's explicit imports of `name` import:
        // several must import one (IEEE 1800-2023 26.3).
        let unit_explicit = |name: &String| -> Result<Option<String>, AnalyzerError> {
            let mut target: Option<String> = None;
            for (_, package_name, _) in unit_imports
                .iter()
                .filter(|(_, _, item)| item.as_ref() == Some(name))
            {
                let provided = provided(package_name, name)?;
                match &target {
                    Some(known) if *known != provided => {
                        return Err(AnalyzerError::ImportConflict {
                            name: name.clone(),
                            detail: format!(
                                "explicitly imported as both `{known}` and `{provided}`"
                            ),
                        });
                    }
                    Some(_) => {}
                    None => target = Some(provided),
                }
            }
            Ok(target)
        };
        for name in referenced.keys() {
            if local.contains(name) || bindings.iter().any(|binding| binding.name == *name) {
                continue;
            }
            let target = if unit.declares_before(name, start) {
                uses_unit = true;
                Some(format!("{}::{name}", packages::UNIT))
            } else if let Some(target) = unit_explicit(name)? {
                Some(target)
            } else {
                let mut found: Option<(String, &String)> = None;
                for (_, package_name, _) in
                    unit_imports.iter().filter(|(_, _, item)| item.is_none())
                {
                    let Some(target) = package(package_name)?.provides(name) else {
                        continue;
                    };
                    match &found {
                        Some((known, first)) if *known != target => {
                            return Err(AnalyzerError::ImportConflict {
                                name: name.clone(),
                                detail: format!(
                                    "declared by both wildcard-imported packages `{first}` and \
                                     `{package_name}`"
                                ),
                            });
                        }
                        Some(_) => {}
                        None => found = Some((target, package_name)),
                    }
                }
                found.map(|(target, _)| target)
            };
            if let Some(target) = target {
                bindings.push(Binding {
                    name: name.clone(),
                    target,
                    through: Vec::new(),
                });
            }
        }
    }
    // A name the generate blocks import is a name of the whole module when
    // nothing else gives it a meaning; otherwise each block that imports
    // another declaration names it there.
    let mut generate_imports: HashMap<usize, HashMap<String, String>> = HashMap::default();
    for (name, uses) in scoped {
        let outside = bindings
            .iter()
            .find(|binding| binding.name == name)
            .map(|binding| binding.target.clone());
        let first = &uses[0].1;
        if outside.is_none()
            && !local.contains(&name)
            && !referenced.contains_key(&name)
            && uses.iter().all(|(_, target)| target == first)
        {
            bindings.push(Binding {
                name,
                target: first.clone(),
                through: Vec::new(),
            });
            continue;
        }
        for (block, target) in uses {
            if outside.as_ref() == Some(&target) {
                continue;
            }
            if symbols.type_aliases.contains_key(&target) {
                return Err(AnalyzerError::Unsupported(format!(
                    "type `{name}` that a generate block imports as another declaration than \
                     its module"
                )));
            }
            generate_imports
                .entry(block)
                .or_default()
                .insert(name.clone(), target);
        }
    }
    symbols.generate_imports = generate_imports;
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
    // `q::x` in `q` names the declaration `q` exports as `x`.
    if let Some(own) = &own {
        for name in &own_exported {
            let target = exports
                .get(name)
                .ok_or_else(|| AnalyzerError::UnknownPackageItem {
                    package: own.clone(),
                    name: name.clone(),
                })?;
            symbols.alias(&format!("{own}::{name}"), target);
        }
    }
    if let Some(unit) = unit
        && uses_unit
    {
        symbols.extend(&unit.scope.as_ref().map_err(Clone::clone)?.symbols);
    }
    for binding in &bindings {
        symbols.alias(&binding.name, &binding.target);
    }
    Ok(ResolvedImports { symbols, exports })
}
