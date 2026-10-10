//! Packages (IEEE 1800-2023 26).
//!
//! A package is analyzed once, after the packages it depends on, with the
//! collectors that analyze a module: its declarations form a scope of their
//! own. Its symbols are exported under their qualified names `p::x`, with the
//! references among them bound to those names (see the `scope` module). A
//! module or package that uses packages starts from their symbols, and from
//! an alias for each name its imports bind (see the `imports` module).

use super::scope::ScopeSymbols;
use super::*;

/// The analyzed packages of a design.
#[derive(Debug, Clone, Default)]
pub struct Packages {
    packages: HashMap<String, Arc<Package>>,
}

#[derive(Debug)]
pub(super) struct Package {
    /// The names the package declares.
    own: HashSet<String>,
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
            .filter_map(|package| package.state_module.as_ref())
    }

    pub(super) fn get(&self, name: &str) -> Option<&Package> {
        self.packages.get(name).map(|package| &**package)
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

    /// Analyze the packages declared in `trees`, each after the packages it
    /// depends on.
    pub fn analyze(trees: &[&SyntaxTree]) -> Result<Self, AnalyzerError> {
        let mut declarations: Vec<(String, RefNode<'_>, &SyntaxTree)> = Vec::new();
        for tree in trees {
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
    let mut imported = imports::imported_symbols(node.clone(), tree, packages)?;
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
        own,
        dependencies: Vec::new(),
        symbols,
        state_module,
    })
}
