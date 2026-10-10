//! Packages (IEEE 1800-2023 26) and compilation units (3.12.1).
//!
//! A package is analyzed once, after the packages it depends on, with the
//! collectors that analyze a module: its declarations form a scope of their
//! own. Its symbols are exported under their qualified names `p::x`, with the
//! references among them bound to those names (see the `scope` module). A
//! module or package that uses packages starts from their symbols, and from
//! an alias for each name its imports bind (see the `imports` module).
//!
//! Each source file is a compilation unit of its own. Its declarations
//! outside any module or package form the compilation-unit scope `$unit`,
//! which is analyzed like a package: the scope can hold any item a package
//! can (3.12.1). A module of the file sees the items of its unit declared
//! before it, and every subroutine of the unit, after its own scope.

use std::path::PathBuf;

use super::scope::ScopeSymbols;
use super::*;

/// The analyzed packages of a design.
#[derive(Debug, Clone, Default)]
pub struct Packages {
    pub(super) timescales: Arc<timescales::Timescales>,
    packages: HashMap<String, Arc<Package>>,
    /// The compilation unit of each source file that declares items outside
    /// its modules and packages.
    units: HashMap<PathBuf, Arc<Unit>>,
}

/// The compilation-unit scope of a source file.
#[derive(Debug)]
pub(super) struct Unit {
    pub visibility: UnitVisibility,
    /// The scope analyzed as a package. A module needs it only when it uses
    /// an item of the unit, so an item no module uses that cannot be
    /// analyzed is reported only then.
    pub scope: Result<Package, AnalyzerError>,
}

impl Unit {
    /// Whether a reference at source offset `offset` sees the item `name`
    /// of the unit: one declared before it, or a subroutine (IEEE 1800-2023
    /// 3.12.1).
    pub fn declares_before(&self, name: &str, offset: usize) -> bool {
        self.visibility.subroutines.contains(name)
            || self
                .visibility
                .declared_at
                .get(name)
                .is_some_and(|declared| *declared < offset)
    }
}

/// The name of the compilation-unit scope.
pub(super) const UNIT: &str = "$unit";

/// What a module of a compilation unit sees of it (IEEE 1800-2023 3.12.1).
#[derive(Debug, Default)]
pub(super) struct UnitVisibility {
    /// The source offset of the item that declares each name.
    pub declared_at: HashMap<String, usize>,
    /// The subroutines of the unit, which are visible anywhere in it.
    subroutines: HashSet<String>,
    /// The package imports of the unit: their source offsets, packages, and
    /// imported items, with no item for a wildcard import.
    pub imports: Vec<(usize, String, Option<String>)>,
}

#[derive(Debug)]
pub(super) struct Package {
    name: String,
    /// The names the package declares.
    own: HashSet<String>,
    /// The imported names the package exports, and the qualified names of
    /// the declarations they denote (IEEE 1800-2023 26.6).
    exports: HashMap<String, String>,
    /// The packages it imports or refers to.
    dependencies: Vec<String>,
    /// Its symbols and those of its dependencies, by qualified name.
    pub symbols: ScopeSymbols,
    /// The package as a module, when it has variables: the module the
    /// package's one instance holds them in.
    state_module: Option<crate::ir::Module>,
}

impl Package {
    /// Whether the package declares `name`.
    pub fn declares(&self, name: &str) -> bool {
        self.own.contains(name)
    }

    /// The qualified name of the declaration an import of `name` from the
    /// package denotes: one the package declares or exports.
    pub fn provides(&self, name: &str) -> Option<String> {
        if self.declares(name) {
            Some(format!("{}::{name}", self.name))
        } else {
            self.exports.get(name).cloned()
        }
    }
}

impl Packages {
    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.packages.keys().map(String::as_str)
    }

    /// The packages that declare variables, as modules named after them.
    /// The design holds one instance of each, outside the instance
    /// hierarchy; a signal with a [`package
    /// variable`](crate::ir::Signal::package_variable) denotes a signal of
    /// one of them.
    pub fn state_modules(&self) -> impl Iterator<Item = &crate::ir::Module> {
        self.packages
            .values()
            .map(|package| &**package)
            .chain(
                self.units
                    .values()
                    .filter_map(|unit| unit.scope.as_ref().ok()),
            )
            .filter_map(|package| package.state_module.as_ref())
    }

    pub(super) fn get(&self, name: &str) -> Option<&Package> {
        self.packages.get(name).map(|package| &**package)
    }

    /// The compilation unit of the source file `path`, if it declares items.
    pub(super) fn unit(&self, path: &Path) -> Option<&Unit> {
        self.units.get(path).map(|unit| &**unit)
    }

    /// Add `name` and, before it, the packages it depends on, each once.
    /// Names that are not packages, such as classes, are left out.
    pub(super) fn closure(&self, name: &str, order: &mut Vec<String>) {
        if order.iter().any(|known| known == name) {
            return;
        }
        let Some(package) = self.packages.get(name) else {
            return;
        };
        for dependency in &package.dependencies {
            self.closure(dependency, order);
        }
        order.push(name.to_string());
    }

    /// Analyze the packages declared in the source files `trees`, each after
    /// the packages it depends on, and then their compilation units.
    pub fn analyze(trees: &[(&SyntaxTree, &Path)]) -> Result<Self, AnalyzerError> {
        let timescales = Arc::new(timescales::Timescales::collect(trees)?);
        let _timescales = timescales::install(timescales.clone());
        let mut packages = Self::analyze_packages(trees)?;
        packages.timescales = timescales;
        let units: Vec<_> = trees
            .iter()
            .map(|(tree, path)| (unit_declaration(tree), *tree, *path))
            .collect();
        for (unit, tree, path) in &units {
            let Some(unit) = unit else {
                continue;
            };
            let visibility = unit_visibility(unit, tree)?;
            let node = RefNode::PackageDeclaration(unit);
            let scope = analyze_package(UNIT, node, tree, &packages);
            if scope
                .as_ref()
                .is_ok_and(|scope| scope.state_module.is_some())
                && packages.units.values().any(|unit| {
                    unit.scope
                        .as_ref()
                        .is_ok_and(|scope| scope.state_module.is_some())
                })
            {
                return Err(AnalyzerError::Unsupported(
                    "compilation-unit variables in more than one source file".to_string(),
                ));
            }
            // Modules find their unit by the path of their source.
            if trees.iter().filter(|(_, other)| other == path).count() > 1 {
                return Err(AnalyzerError::Unsupported(format!(
                    "compilation-unit declarations in a source whose path `{}` another source \
                     shares",
                    path.display()
                )));
            }
            packages
                .units
                .insert(path.to_path_buf(), Arc::new(Unit { visibility, scope }));
        }
        Ok(packages)
    }

    fn analyze_packages(trees: &[(&SyntaxTree, &Path)]) -> Result<Self, AnalyzerError> {
        let mut declarations: Vec<(String, RefNode<'_>, &SyntaxTree)> = Vec::new();
        for (tree, _) in trees {
            // Only modules and packages are analyzed, so an import declared
            // at compilation-unit scope would never be found.
            for node in *tree {
                if let RefNode::DescriptionPackageItem(item) = node
                    && RefNode::DescriptionPackageItem(item)
                        .into_iter()
                        .any(|child| matches!(child, RefNode::DpiImportExport(_)))
                {
                    return Err(AnalyzerError::Unsupported(
                        "DPI-C import or export at compilation-unit scope (declare it in a \
                         module or package)"
                            .to_string(),
                    ));
                }
            }
            for node in *tree {
                let RefNode::PackageDeclaration(package) = node else {
                    continue;
                };
                let name = identifier_text(RefNode::PackageIdentifier(&package.nodes.3), tree)
                    .ok_or_else(|| AnalyzerError::Unsupported("package identifier".to_string()))?;
                if declarations.iter().any(|(known, ..)| *known == name) {
                    return Err(AnalyzerError::DuplicatePackage { name });
                }
                declarations.push((name, RefNode::PackageDeclaration(package), tree));
            }
        }
        if declarations.is_empty() {
            return Ok(Self::default());
        }
        let names: HashSet<&str> = declarations
            .iter()
            .map(|(name, ..)| name.as_str())
            .collect();
        let dependencies: Vec<Vec<String>> = declarations
            .iter()
            .map(|(name, node, tree)| {
                imports::ScopeImports::from_node(node.clone(), tree)
                    .packages
                    .into_iter()
                    .filter(|package| package != name && names.contains(package.as_str()))
                    .collect()
            })
            .collect();
        let mut order = Vec::new();
        let mut active = Vec::new();
        for index in 0..declarations.len() {
            visit(index, &declarations, &dependencies, &mut order, &mut active)?;
        }
        let mut packages = Self::default();
        for index in order {
            let (name, node, tree) = &declarations[index];
            let package = analyze_package(name, node.clone(), tree, &packages)?;
            packages.packages.insert(
                name.clone(),
                Arc::new(Package {
                    dependencies: dependencies[index].clone(),
                    ..package
                }),
            );
        }
        Ok(packages)
    }
}

/// The variables package `name` declares: each is one object, shared by
/// every module that uses it (IEEE 1800-2023 26.2). Constant variables are
/// left out; they are copied like parameters. Package nets are rejected.
fn package_variables(
    name: &str,
    items: &[(Vec<sv_parser::AttributeInstance>, sv_parser::PackageItem)],
    syntax_tree: &SyntaxTree,
) -> Result<HashSet<String>, AnalyzerError> {
    use sv_parser::{DataDeclaration, PackageItem, PackageOrGenerateItemDeclaration};
    let mut variables = HashSet::default();
    for (_, item) in items {
        let PackageItem::PackageOrGenerateItemDeclaration(declaration) = item else {
            continue;
        };
        match declaration.as_ref() {
            // `word_t v;` with a typedef `word_t` is a variable; user-defined
            // net types are not supported.
            PackageOrGenerateItemDeclaration::NetDeclaration(net)
                if matches!(**net, sv_parser::NetDeclaration::NetTypeIdentifier(_)) =>
            {
                for node in RefNode::NetDeclaration(net) {
                    if let RefNode::NetIdentifier(_) = node
                        && let Some(variable) = identifier_text(node, syntax_tree)
                    {
                        variables.insert(variable);
                    }
                }
            }
            PackageOrGenerateItemDeclaration::NetDeclaration(net) => {
                let identifier = RefNode::NetDeclaration(net).into_iter().find_map(|node| {
                    matches!(node, RefNode::NetIdentifier(_))
                        .then(|| identifier_text(node, syntax_tree))
                        .flatten()
                });
                return Err(AnalyzerError::Unsupported(format!(
                    "package net `{name}::{}`",
                    identifier.as_deref().unwrap_or("?")
                )));
            }
            PackageOrGenerateItemDeclaration::DataDeclaration(data) => {
                if let DataDeclaration::Variable(variable) = data.as_ref()
                    && variable.nodes.0.is_none()
                {
                    for node in RefNode::ListOfVariableDeclAssignments(&variable.nodes.4) {
                        if let RefNode::VariableIdentifier(_) = node
                            && let Some(variable) = identifier_text(node, syntax_tree)
                        {
                            variables.insert(variable);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(variables)
}

fn visit(
    index: usize,
    declarations: &[(String, RefNode<'_>, &SyntaxTree)],
    dependencies: &[Vec<String>],
    order: &mut Vec<usize>,
    active: &mut Vec<usize>,
) -> Result<(), AnalyzerError> {
    if order.contains(&index) {
        return Ok(());
    }
    if active.contains(&index) {
        return Err(AnalyzerError::PackageCycle {
            name: declarations[index].0.clone(),
        });
    }
    active.push(index);
    for dependency in &dependencies[index] {
        if let Some(dependency) = declarations
            .iter()
            .position(|(name, ..)| name == dependency)
        {
            visit(dependency, declarations, dependencies, order, active)?;
        }
    }
    active.pop();
    order.push(index);
    Ok(())
}

fn analyze_package(
    name: &str,
    node: RefNode<'_>,
    tree: &SyntaxTree,
    packages: &Packages,
) -> Result<Package, AnalyzerError> {
    let first = {
        // Inside the package, `p::x` names its own item `x`.
        let _package = scope::enter_package(name);
        analyze_package_scope(name, node.clone(), tree, packages, None)?
    };
    if !imports::ScopeImports::from_node(node.clone(), tree)
        .qualified
        .iter()
        .any(|(package, _)| package == name)
    {
        return Ok(first);
    }
    // A self-qualified reference `p::x` is not hidden by a local `x` of a
    // function (IEEE 1800-2023 26.3). Analyze the package again with its
    // items from the first analysis under their qualified names.
    analyze_package_scope(name, node, tree, packages, Some(&first.symbols))
}

fn analyze_package_scope(
    name: &str,
    node: RefNode<'_>,
    tree: &SyntaxTree,
    packages: &Packages,
    own_items: Option<&ScopeSymbols>,
) -> Result<Package, AnalyzerError> {
    let imports::ResolvedImports {
        symbols: mut imported,
        exports,
    } = imports::resolve_imports(node.clone(), tree, packages, None)?;
    if let Some(own_items) = own_items {
        imported.extend(own_items);
    }
    let aliases = imported.aliases.clone();
    let imported_locals = imported.local_names();
    // The names the package declares, from its syntax: an escaped name may
    // contain `::` itself.
    let own = imports::scope_names(node.clone(), tree);
    let variables = match &node {
        RefNode::PackageDeclaration(package) => package_variables(name, &package.nodes.6, tree)?,
        _ => HashSet::default(),
    };
    let mut module = Module::from_module_node_with_parameter_overrides(
        node,
        tree,
        "",
        &HashMap::default(),
        &InterfaceLookup {
            local: &ModuleInterfaces::default(),
            extra: &ModuleInterfaces::default(),
        },
        Arc::new(imported),
    )?;
    let mut symbols = module
        .symbols
        .take()
        .expect("a package analysis keeps the package symbols");
    // The package's variables, and those it imports, are shared objects;
    // the other signals are constants, copied with their initializers.
    let (state, constants): (Vec<_>, Vec<_>) = std::mem::take(&mut symbols.signals)
        .into_iter()
        .partition(|signal| signal.package_variable.is_some() || variables.contains(signal.name()));
    symbols.initial_processes.retain(|process| {
        !state
            .iter()
            .any(|signal| scope::initializes(process, signal.name()))
    });
    symbols.signals = constants;
    // A declaration of a user-defined net type gives no signal.
    if let Some(net) = variables.iter().find(|variable| {
        !state
            .iter()
            .any(|signal| signal.name() == variable.as_str())
    }) {
        return Err(AnalyzerError::Unsupported(format!(
            "package net `{name}::{net}`"
        )));
    }
    symbols.state_signals = state
        .into_iter()
        .map(|mut signal| {
            if signal.package_variable.is_none() {
                signal.package_variable = Some((name.to_string(), signal.name().to_string()));
            }
            signal
        })
        .collect();
    let state_module = if variables.is_empty() {
        None
    } else {
        let ir = crate::analyze::analyze_source(Source {
            modules: vec![module],
        })?;
        ir.modules().first().cloned()
    };
    let qualified = |item: &String| format!("{name}::{item}");
    let mut names: HashMap<String, String> = own
        .iter()
        .map(|declared| (declared.clone(), qualified(declared)))
        .collect();
    let mut own_locals = HashSet::default();
    for local in symbols.local_names() {
        if !imported_locals.contains(&local) {
            names.insert(local.clone(), qualified(&local));
            own_locals.insert(local);
        }
    }
    // The items of the first analysis give way to those of this one.
    if own_items.is_some() {
        symbols = symbols.without(&own.iter().chain(&own_locals).map(qualified).collect());
    }
    names.extend(aliases);
    let symbols = symbols.renamed(&names);
    Ok(Package {
        name: name.to_string(),
        own,
        exports,
        dependencies: Vec::new(),
        symbols,
        state_module,
    })
}

/// The compilation-unit scope of `tree` as a package declaration: the items
/// of the file outside its design elements, which are package items too.
/// A compilation unit has no keyword of its own, so its keywords are empty.
fn unit_declaration(tree: &SyntaxTree) -> Option<sv_parser::PackageDeclaration> {
    let items: Vec<_> = tree
        .into_iter()
        .filter_map(|node| match node {
            RefNode::DescriptionPackageItem(item) => Some(item.nodes.clone()),
            _ => None,
        })
        .collect();
    if items.is_empty() {
        return None;
    }
    let empty = sv_parser::Locate {
        offset: 0,
        line: 0,
        len: 0,
    };
    let keyword = || sv_parser::Keyword {
        nodes: (empty, Vec::new()),
    };
    Some(sv_parser::PackageDeclaration {
        nodes: (
            Vec::new(),
            keyword(),
            None,
            sv_parser::PackageIdentifier {
                nodes: (sv_parser::Identifier::SimpleIdentifier(Box::new(
                    sv_parser::SimpleIdentifier {
                        nodes: (empty, Vec::new()),
                    },
                )),),
            },
            sv_parser::Symbol {
                nodes: (empty, Vec::new()),
            },
            None,
            items,
            keyword(),
            None,
        ),
    })
}

/// Whether `package` is the compilation-unit scope of a file.
pub(super) fn is_unit(package: &sv_parser::PackageDeclaration) -> bool {
    package.nodes.1.nodes.0.len == 0
}

/// Where the items of the compilation unit `unit` are declared.
fn unit_visibility(
    unit: &sv_parser::PackageDeclaration,
    tree: &SyntaxTree,
) -> Result<UnitVisibility, AnalyzerError> {
    let mut visibility = UnitVisibility::default();
    for (_, item) in &unit.nodes.6 {
        let node = RefNode::PackageItem(item);
        let Some(offset) = node.clone().into_iter().find_map(|child| match child {
            RefNode::Locate(locate) => Some(locate.offset),
            _ => None,
        }) else {
            continue;
        };
        let subroutine = matches!(
            item,
            sv_parser::PackageItem::PackageOrGenerateItemDeclaration(declaration)
                if matches!(
                    **declaration,
                    sv_parser::PackageOrGenerateItemDeclaration::FunctionDeclaration(_)
                        | sv_parser::PackageOrGenerateItemDeclaration::TaskDeclaration(_)
                )
        );
        // An item whose names cannot be read is reported if a module uses
        // the scope.
        for name in interfaces::package_item_names(item, tree).unwrap_or_default() {
            if subroutine {
                visibility.subroutines.insert(name.clone());
            }
            visibility.declared_at.entry(name).or_insert(offset);
        }
        // Only an import declaration of the unit itself; one in a subroutine
        // of the unit is the subroutine's.
        let sv_parser::PackageItem::PackageOrGenerateItemDeclaration(declaration) = item else {
            continue;
        };
        let sv_parser::PackageOrGenerateItemDeclaration::DataDeclaration(data) = &**declaration
        else {
            continue;
        };
        if !matches!(
            **data,
            sv_parser::DataDeclaration::PackageImportDeclaration(_)
        ) {
            continue;
        }
        for child in node {
            match child {
                RefNode::PackageImportItem(sv_parser::PackageImportItem::Identifier(item)) => {
                    if let (Some(package), Some(name)) = (
                        identifier_text(RefNode::PackageIdentifier(&item.nodes.0), tree),
                        identifier_text(RefNode::Identifier(&item.nodes.2), tree),
                    ) {
                        visibility.imports.push((offset, package, Some(name)));
                    }
                }
                RefNode::PackageImportItem(sv_parser::PackageImportItem::Asterisk(item)) => {
                    if let Some(package) =
                        identifier_text(RefNode::PackageIdentifier(&item.nodes.0), tree)
                    {
                        visibility.imports.push((offset, package, None));
                    }
                }
                _ => {}
            }
        }
    }
    Ok(visibility)
}
