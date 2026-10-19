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

use super::instances::normalize_identifier;
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
    pub(crate) fn from_scope(tree: &SyntaxTree) -> Result<Self, AnalyzerError> {
        use celox_sv_syntax::{PackageBindingItemKind, PackageBindingKind};
        let mut imports = Self::default();
        let blocks = generate_scopes(tree)?;
        let nested = nested_declarations(tree);
        for view in package_owners::scope(tree)? {
            let binding = tree.declarations().package_binding(view.id);
            for native in &binding.items {
                let in_block = blocks
                    .iter()
                    .any(|scope| scope.contains(native.tokens.start));
                match (&binding.kind, &native.kind) {
                    (PackageBindingKind::Export, PackageBindingItemKind::All) => {
                        imports.exports.push(Export::All)
                    }
                    (
                        PackageBindingKind::Export,
                        PackageBindingItemKind::Named { package, name },
                    ) => {
                        let package = normalize_identifier(&package.spelling);
                        let name = normalize_identifier(&name.spelling);
                        imports.packages.push(package.clone());
                        imports
                            .export_offsets
                            .entry((package.clone(), name.clone()))
                            .or_insert(native.tokens.start);
                        imports.exports.push(Export::Item(package, name));
                    }
                    (PackageBindingKind::Export, PackageBindingItemKind::Wildcard(package)) => {
                        let package = normalize_identifier(&package.spelling);
                        imports.packages.push(package.clone());
                        imports.exports.push(Export::Wildcard(package));
                    }
                    (
                        PackageBindingKind::Import,
                        PackageBindingItemKind::Named { package, name },
                    ) => {
                        let package = normalize_identifier(&package.spelling);
                        imports.packages.push(package.clone());
                        if !in_block {
                            imports
                                .explicit
                                .push((package, normalize_identifier(&name.spelling)));
                        }
                    }
                    (PackageBindingKind::Import, PackageBindingItemKind::Wildcard(package)) => {
                        let package = normalize_identifier(&package.spelling);
                        imports.packages.push(package.clone());
                        if !in_block {
                            imports.wildcard.push(package);
                        }
                    }
                    _ => {}
                }
            }
        }
        // Qualified expression and type names carry their package segment natively.
        let native = tree.declarations();
        let ranges = active_ranges(tree)?;
        for expression in &native.expressions {
            if !in_ranges(expression.tokens.start, &ranges) {
                continue;
            }
            if let celox_sv_syntax::ExprKind::Member {
                expression: base,
                separator,
                name,
            } = &expression.kind
                && separator == "::"
                && let celox_sv_syntax::ExprKind::Name(package) = &native.expression(*base).kind
            {
                let package = normalize_identifier(&package.spelling);
                if !nested.iter().any(|(start, end, names)| {
                    (*start..*end).contains(&expression.tokens.start) && names.contains(&package)
                }) {
                    imports
                        .qualified
                        .push((package, normalize_identifier(&name.spelling)));
                }
            }
        }
        for ty in &native.types {
            if in_ranges(ty.tokens.start, &ranges)
                && let celox_sv_syntax::DataTypeKind::Named(path) = &ty.kind
                && let [package, name] = path.as_slice()
            {
                imports.qualified.push((
                    normalize_identifier(&package.name.spelling),
                    normalize_identifier(&name.name.spelling),
                ));
            }
        }
        for (package, _) in &imports.qualified {
            if package != packages::UNIT {
                imports.packages.push(package.clone());
            }
        }
        let mut seen = HashSet::default();
        imports.packages.retain(|name| seen.insert(name.clone()));
        seen.clear();
        imports.wildcard.retain(|name| seen.insert(name.clone()));
        Ok(imports)
    }
}

/// The names declared directly in a module or package scope: its ports and
/// parameters, and the declarations and instances of its items. Names
/// declared in nested scopes, such as structure members, function locals
/// and generate blocks, are left out.
pub(super) fn item_names(id: celox_sv_syntax::BodyItemId, tree: &SyntaxTree) -> HashSet<String> {
    use celox_sv_syntax::{BodyItemKind, DataDeclaration, DataTypeKind};
    let native = tree.declarations();
    let mut names = HashSet::default();
    let mut add = |name: &celox_sv_syntax::Name| {
        names.insert(normalize_identifier(&name.spelling));
    };
    let mut types = Vec::new();
    match &native.body_item(id).kind {
        BodyItemKind::Data(id) => {
            if let DataDeclaration::Variables(decl) | DataDeclaration::Constructed(decl) =
                native.data_declaration(*id)
            {
                for variable in &decl.variables {
                    add(&variable.name);
                }
                types.push(decl.data_type);
            }
        }
        BodyItemKind::Net(id) => {
            let decl = native.net_declaration(*id);
            for variable in &decl.variables {
                add(&variable.name);
            }
            types.push(decl.data_type);
        }
        BodyItemKind::Parameter(id) => {
            for parameter in &native.parameter_declaration(*id).parameters {
                add(&parameter.name);
                if let celox_sv_syntax::ParameterKind::Value { data_type, .. } = &parameter.kind {
                    types.push(*data_type);
                }
            }
        }
        BodyItemKind::TypeAlias(id) => {
            let alias = native.type_alias(*id);
            add(&alias.name);
            if let celox_sv_syntax::TypeAliasTarget::DataType(id) = alias.target {
                types.push(id);
            }
        }
        BodyItemKind::Subroutine(id) => add(&native.subroutine(*id).name),
        BodyItemKind::Dpi(id) => add(&native.dpi_declaration(*id).name),
        BodyItemKind::Instance(id) => {
            for instance in &native.instantiation(*id).instances {
                add(&instance.name);
            }
        }
        _ => {}
    }
    for id in types {
        if let DataTypeKind::Enum { items, .. } = &native.data_type(id).kind {
            for item in items {
                add(&item.name);
            }
        }
    }
    names
}
pub(super) fn scope_names(tree: &SyntaxTree) -> Converted<HashSet<String>> {
    use celox_sv_syntax::{BodyItemKind, PortKind};
    let mut names = HashSet::default();
    if let Ok(unit) = tree.root_unit() {
        for parameter in unit.parameters.iter().flatten() {
            names.insert(normalize_identifier(&parameter.name.spelling));
        }
        for id in tree.header_port_ids()? {
            match &unit.ports[id].kind {
                PortKind::Named(name) | PortKind::Explicit { name, .. } => {
                    names.insert(normalize_identifier(&name.spelling));
                }
                _ => {}
            }
        }
    }
    let mut pending = tree.native_body_membership()?;
    let mut seen = HashSet::default();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            return Err(AnalyzerError::Parse(
                "repeated native scope declaration".into(),
            ));
        }
        if let BodyItemKind::Region(items) = &tree.declarations().body_item(id).kind {
            pending.extend(items);
        } else {
            names.extend(item_names(id, tree));
        }
    }
    Ok(names)
}

fn validate_children(
    item: &celox_sv_syntax::BodyItem,
    native: &celox_sv_syntax::Declarations,
) -> Converted<()> {
    use celox_sv_syntax::BodyItemKind;
    let children = match &item.kind {
        BodyItemKind::Region(items)
        | BodyItemKind::Block { items, .. }
        | BodyItemKind::Unconverted { items, .. } => items.clone(),
        BodyItemKind::Conditional {
            then_body,
            otherwise,
            ..
        } => std::iter::once(*then_body).chain(*otherwise).collect(),
        BodyItemKind::Case { arms, .. } => arms.iter().map(|arm| arm.body).collect(),
        BodyItemKind::Loop { body, .. } => vec![*body],
        _ => Vec::new(),
    };
    if children.iter().any(|id| {
        let range = native.body_item(*id).tokens;
        range.start < item.tokens.start || range.end > item.tokens.end
    }) {
        return Err(AnalyzerError::Parse(
            "native import scope child is outside its body owner".into(),
        ));
    }
    Ok(())
}

fn body_items(tree: &SyntaxTree) -> Converted<Vec<celox_sv_syntax::BodyItemId>> {
    use celox_sv_syntax::BodyItemKind;
    let mut pending = tree.native_body_membership()?;
    let mut result = Vec::new();
    let mut seen = HashSet::default();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            return Err(AnalyzerError::Parse(
                "repeated native import scope item".into(),
            ));
        }
        result.push(id);
        validate_children(tree.declarations().body_item(id), tree.declarations())?;
        match &tree.declarations().body_item(id).kind {
            BodyItemKind::Region(items)
            | BodyItemKind::Block { items, .. }
            | BodyItemKind::Unconverted { items, .. } => pending.extend(items),
            BodyItemKind::Conditional {
                then_body,
                otherwise,
                ..
            } => {
                pending.push(*then_body);
                pending.extend(otherwise);
            }
            BodyItemKind::Case { arms, .. } => pending.extend(arms.iter().map(|arm| arm.body)),
            BodyItemKind::Loop { body, .. } => pending.push(*body),
            _ => {}
        }
    }
    Ok(result)
}
fn active_ranges(tree: &SyntaxTree) -> Converted<Vec<celox_sv_syntax::TokenRange>> {
    let mut ranges: Vec<_> = body_items(tree)?
        .into_iter()
        .map(|id| tree.declarations().body_item(id).tokens)
        .collect();
    if let Ok(unit) = tree.root_unit() {
        let end = unit
            .items
            .iter()
            .map(|id| tree.declarations().body_item(*id).tokens.start)
            .min()
            .unwrap_or(unit.tokens.end);
        ranges.push(celox_sv_syntax::TokenRange {
            start: unit.tokens.start,
            end,
        });
    }
    ranges.sort_unstable_by_key(|range| range.start);
    let mut merged: Vec<celox_sv_syntax::TokenRange> = Vec::new();
    for range in ranges {
        if let Some(previous) = merged.last_mut()
            && range.start <= previous.end
        {
            previous.end = previous.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    Ok(merged)
}
fn in_ranges(offset: usize, ranges: &[celox_sv_syntax::TokenRange]) -> bool {
    let next = ranges.partition_point(|range| range.start <= offset);
    next > 0 && offset < ranges[next - 1].end
}
/// Unqualified expression and type references in the selected native scope.
pub(super) fn unqualified_names(tree: &SyntaxTree) -> HashMap<String, Vec<usize>> {
    use celox_sv_syntax::{DataTypeKind, ExprKind};
    let native = tree.declarations();
    let Ok(ranges) = active_ranges(tree) else {
        return HashMap::default();
    };
    let nested = nested_declarations(tree);
    let qualified: HashSet<_> = native
        .expressions
        .iter()
        .filter_map(|expression| match &expression.kind {
            ExprKind::Member {
                expression,
                separator,
                ..
            } if separator == "::" => Some(*expression),
            _ => None,
        })
        .collect();
    // Bare pattern keys name members; they do not bind wildcard imports.
    let pattern_keys: HashSet<_> = native
        .expressions
        .iter()
        .flat_map(|expression| match &expression.kind {
            ExprKind::Pattern { items, .. } => {
                items.iter().filter_map(|item| item.key).collect::<Vec<_>>()
            }
            _ => Vec::new(),
        })
        .filter_map(|id| {
            matches!(native.expression(id).kind, ExprKind::Name(_))
                .then_some(native.expression(id).tokens.start)
        })
        .collect();
    let mut names: HashMap<String, Vec<usize>> = HashMap::default();
    let mut add = |name: &str, offset| {
        let name = normalize_identifier(name);
        if in_ranges(offset, &ranges)
            && !nested.iter().any(|(start, end, declared)| {
                (*start..*end).contains(&offset) && declared.contains(&name)
            })
        {
            names.entry(name).or_default().push(offset);
        }
    };
    for expression in &native.expressions {
        if let ExprKind::Name(name) = &expression.kind
            && !pattern_keys.contains(&expression.tokens.start)
            && !qualified.iter().any(|base| {
                native.expression(*base).tokens == expression.tokens
                    && matches!(&native.expression(*base).kind, ExprKind::Name(_))
            })
        {
            add(&name.spelling, expression.tokens.start);
        }
    }
    for ty in &native.types {
        if let DataTypeKind::Named(path) = &ty.kind
            && let [name] = path.as_slice()
        {
            add(&name.name.spelling, ty.tokens.start);
        }
    }
    for offsets in names.values_mut() {
        offsets.sort_unstable();
        offsets.dedup();
    }
    names
}

/// Native generate scopes, keyed by their authored body owner.
struct GenerateScope {
    parent: Option<usize>,
    id: celox_sv_syntax::BodyItemId,
    start: usize,
    end: usize,
    explicit: Vec<(String, String)>,
    wildcard: Vec<String>,
    declared: HashSet<String>,
}
impl GenerateScope {
    fn contains(&self, offset: usize) -> bool {
        (self.start..self.end).contains(&offset)
    }
}
fn generate_scopes(tree: &SyntaxTree) -> Converted<Vec<GenerateScope>> {
    use celox_sv_syntax::{BodyItemKind, PackageBindingItemKind, PackageBindingKind};
    let native = tree.declarations();
    let mut scopes: Vec<GenerateScope> = Vec::new();
    let mut seen = HashSet::default();
    let mut loop_variables = HashMap::default();
    let mut pending: Vec<(celox_sv_syntax::BodyItemId, Option<usize>, bool)> = tree
        .native_body_membership()?
        .into_iter()
        .map(|id| (id, None, false))
        .collect();
    while let Some((id, parent, force_scope)) = pending.pop() {
        if !seen.insert(id) {
            return Err(AnalyzerError::Parse(
                "repeated native generate scope".into(),
            ));
        }
        let item = native.body_item(id);
        validate_children(item, native)?;
        if let BodyItemKind::Block {
            label,
            leading_label,
            ..
        } = &item.kind
            && let Some(parent) = parent
            && let Some(label) = label.as_ref().or(leading_label.as_ref())
        {
            // A loop body label is declared beside the loop, not in its
            // implicit genvar scope.
            let parent = if matches!(
                native.body_item(scopes[parent].id).kind,
                BodyItemKind::Loop { .. }
            ) {
                scopes[parent].parent
            } else {
                Some(parent)
            };
            if let Some(parent) = parent {
                scopes[parent]
                    .declared
                    .insert(normalize_identifier(&label.spelling));
            }
        }
        let owner = if force_scope
            || matches!(
                item.kind,
                BodyItemKind::Block { .. } | BodyItemKind::Loop { .. }
            ) {
            scopes.push(GenerateScope {
                parent,
                id,
                start: item.tokens.start,
                end: item.tokens.end,
                explicit: Vec::new(),
                wildcard: Vec::new(),
                declared: HashSet::default(),
            });
            Some(scopes.len() - 1)
        } else {
            parent
        };
        if let Some(owner) = owner {
            scopes[owner].declared.extend(item_names(id, tree));
            if let Some(variable) = loop_variables.remove(&id) {
                scopes[owner].declared.insert(variable);
            }
        }
        match &item.kind {
            BodyItemKind::Region(items)
            | BodyItemKind::Block { items, .. }
            | BodyItemKind::Unconverted { items, .. } => {
                pending.extend(items.iter().map(|id| (*id, owner, false)))
            }
            BodyItemKind::Conditional {
                then_body,
                otherwise,
                ..
            } => pending.extend(std::iter::once(then_body).chain(otherwise).map(|id| {
                (
                    *id,
                    owner,
                    !matches!(native.body_item(*id).kind, BodyItemKind::Conditional { .. }),
                )
            })),
            BodyItemKind::Case { arms, .. } => pending.extend(arms.iter().map(|arm| {
                (
                    arm.body,
                    owner,
                    !matches!(
                        native.body_item(arm.body).kind,
                        BodyItemKind::Conditional { .. }
                    ),
                )
            })),
            BodyItemKind::Loop { variable, body, .. } => {
                scopes[owner.expect("loop owns a scope")]
                    .declared
                    .insert(normalize_identifier(&variable.spelling));
                // Each generated body owns its genvar localparam, ahead of
                // wildcard imports authored in that same body.
                loop_variables.insert(*body, normalize_identifier(&variable.spelling));
                pending.push((*body, owner, true));
            }
            BodyItemKind::PackageBinding(id) => {
                let binding = native.package_binding(*id);
                if binding.kind == PackageBindingKind::Import
                    && let Some(owner) = owner
                {
                    for item in &binding.items {
                        match &item.kind {
                            PackageBindingItemKind::Named { package, name } => {
                                scopes[owner].explicit.push((
                                    normalize_identifier(&package.spelling),
                                    normalize_identifier(&name.spelling),
                                ))
                            }
                            PackageBindingItemKind::Wildcard(package) => scopes[owner]
                                .wildcard
                                .push(normalize_identifier(&package.spelling)),
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(scopes)
}

/// What a reference in a generate block names.
enum ScopeLookup {
    /// A declaration of a block around it.
    Declared,
    /// The declaration an import of a block around it binds: that block's
    /// source offset, and the declaration.
    Imported(celox_sv_syntax::BodyItemId, String),
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
            return Ok(ScopeLookup::Imported(scope.id, target));
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
            return Ok(ScopeLookup::Imported(scope.id, target));
        }
    }
    Ok(ScopeLookup::Outside)
}

fn nested_declarations(tree: &SyntaxTree) -> Vec<(usize, usize, HashSet<String>)> {
    use celox_sv_syntax::{
        BodyItemKind, DataTypeKind, ForInitialization, LocalDeclaration, LoopSyntax, StatementKind,
        TypeAliasTarget,
    };
    let native = tree.declarations();
    let mut result = Vec::new();
    let mut pending = Vec::new();
    let Ok(items) = body_items(tree) else {
        return result;
    };
    for id in items {
        match native.body_item(id).kind {
            BodyItemKind::Subroutine(id) => {
                let sub = native.subroutine(id);
                let mut names = HashSet::default();
                names.insert(normalize_identifier(&sub.name.spelling));
                for port in sub
                    .ports
                    .iter()
                    .flatten()
                    .chain(sub.port_declarations.iter().flat_map(|decl| &decl.ports))
                {
                    if let Some(name) = &port.name {
                        names.insert(normalize_identifier(&name.spelling));
                    }
                }
                result.push((sub.tokens.start, sub.tokens.end, names));
                // Walk lexical roots so a nested block keeps its declarations.
                pending.extend(
                    sub.items
                        .iter()
                        .copied()
                        .map(|id| (id, Some(result.len() - 1))),
                );
            }
            BodyItemKind::Procedure(id) => pending.push((native.procedure(id).body, None)),
            _ => {}
        }
    }
    let mut seen = HashSet::default();
    while let Some((id, mut owner)) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        let statement = native.statement(id);
        if matches!(
            statement.kind,
            StatementKind::Block { .. }
                | StatementKind::Parallel { .. }
                | StatementKind::Loop {
                    kind: LoopSyntax::For(_),
                    ..
                }
        ) {
            result.push((
                statement.tokens.start,
                statement.tokens.end,
                HashSet::default(),
            ));
            owner = Some(result.len() - 1);
        }
        if let Some(owner) = owner {
            let names = &mut result[owner].2;
            let mut types = Vec::new();
            match &statement.kind {
                StatementKind::Declaration(
                    LocalDeclaration::Variables(decl) | LocalDeclaration::Constructed(decl),
                ) => {
                    for var in &decl.variables {
                        names.insert(normalize_identifier(&var.name.spelling));
                    }
                    types.push(decl.data_type);
                }
                StatementKind::Declaration(LocalDeclaration::Parameters(id)) => {
                    for parameter in &native.parameter_declaration(*id).parameters {
                        names.insert(normalize_identifier(&parameter.name.spelling));
                    }
                }
                StatementKind::Declaration(LocalDeclaration::TypeAlias(id)) => {
                    let alias = native.type_alias(*id);
                    names.insert(normalize_identifier(&alias.name.spelling));
                    if let TypeAliasTarget::DataType(id) = alias.target {
                        types.push(id);
                    }
                }
                StatementKind::Loop {
                    kind: LoopSyntax::For(header),
                    ..
                } => {
                    if let Some(ForInitialization::Declarations(decls)) = &header.initialization {
                        for decl in decls {
                            for var in &decl.variables {
                                names.insert(normalize_identifier(&var.name.spelling));
                            }
                        }
                    }
                }
                _ => {}
            }
            for ty in types {
                if let DataTypeKind::Enum { items, .. } = &native.data_type(ty).kind {
                    for item in items {
                        names.insert(normalize_identifier(&item.name.spelling));
                    }
                }
            }
        }
        pending.extend(statement.kind.children().map(|id| (id, owner)));
    }
    result
}

/// The symbols a scope starts from: those of every package it uses, directly
/// or through another package, and an alias for each name its imports bind.
/// `unit` is the compilation unit of the module's source file.
pub(super) fn imported_symbols(
    tree: &SyntaxTree,
    packages: &packages::Packages,
    unit: Option<&packages::Unit>,
) -> Result<ScopeSymbols, AnalyzerError> {
    Ok(resolve_imports(tree, packages, unit)?.symbols)
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
    tree: &SyntaxTree,
    packages: &packages::Packages,
    unit: Option<&packages::Unit>,
) -> Result<ResolvedImports, AnalyzerError> {
    let imports = ScopeImports::from_scope(tree)?;
    // A module sees the compilation-unit items declared before it, which are
    // also those declared before any reference in it.
    let start = tree.native_scope_start()?;
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
    let all_scopes = generate_scopes(tree)?;
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
    let own = tree
        .root_unit()
        .ok()
        .filter(|unit| unit.kind == celox_sv_syntax::UnitKind::Package)
        .map(|unit| normalize_identifier(&unit.name.spelling))
        .or_else(|| {
            (tree.scope_handle() == crate::migration::ScopeHandle::Compilation)
                .then(|| "$unit".into())
        });
    let mut own_names = None;
    let mut exported_references = Vec::new();
    let mut own_exported: Vec<String> = Vec::new();
    // Whether the scope uses an item of its compilation unit.
    let mut uses_unit = false;
    for (package_name, name) in &imports.qualified {
        if own.as_ref() == Some(package_name) {
            // The package names one of its own items.
            if own_names.is_none() {
                own_names = Some(scope_names(tree)?);
            }
            let own_names = own_names.as_ref().expect("populated scope names");
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
    let local = scope_names(tree)?;
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
        unqualified_names(tree)
    };
    // A reference in a generate block is looked up in that block and the
    // blocks around it first (IEEE 1800-2023 26.3): the references the
    // imports of a block bind, by name, with the block and the declaration.
    let mut scoped: HashMap<String, Vec<(celox_sv_syntax::BodyItemId, String)>> =
        HashMap::default();
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
    // another declaration than the one visible around it names it there.
    let mut generate_imports: HashMap<celox_sv_syntax::BodyItemId, HashMap<String, String>> =
        HashMap::default();
    for (name, mut uses) in scoped {
        let outside = bindings
            .iter()
            .find(|binding| binding.name == name)
            .map(|binding| binding.target.clone());
        let first = &uses[0].1;
        // A declaration in another generate block also gives the name a
        // meaning there.
        if outside.is_none()
            && !local.contains(&name)
            && !referenced.contains_key(&name)
            && !all_scopes
                .iter()
                .any(|scope| scope.declared.contains(&name))
            && uses.iter().all(|(_, target)| target == first)
        {
            bindings.push(Binding {
                name,
                target: first.clone(),
                through: Vec::new(),
            });
            continue;
        }
        uses.sort_by_key(|(id, target)| {
            (
                all_scopes
                    .iter()
                    .find(|scope| scope.id == *id)
                    .map(|scope| scope.start),
                target.clone(),
            )
        });
        uses.dedup();
        // A block around `block` that imports another declaration, or
        // declares the name, hides the module's binding there.
        let encloses = |scope: &GenerateScope, block: celox_sv_syntax::BodyItemId| {
            all_scopes
                .iter()
                .find(|inner| inner.id == block)
                .is_some_and(|inner| scope.start < inner.start && scope.contains(inner.start))
        };
        let hidden = |block: celox_sv_syntax::BodyItemId, target: &String| {
            uses.iter().any(|(outer, other)| {
                other != target
                    && all_scopes
                        .iter()
                        .any(|scope| scope.id == *outer && encloses(scope, block))
            }) || all_scopes
                .iter()
                .any(|scope| scope.declared.contains(&name) && encloses(scope, block))
        };
        for (block, target) in &uses {
            if outside.as_ref() == Some(target) && !hidden(*block, target) {
                continue;
            }
            generate_imports
                .entry(*block)
                .or_default()
                .insert(name.clone(), target.clone());
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
