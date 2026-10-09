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
                reject_package_state(&name, &package.nodes.6, tree)?;
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

/// Reject the variables and nets of package `name`. A package declares one
/// object shared by every module (IEEE 1800-2023 26.2), but each module that
/// uses the package lowers its own copy of the package items. Constant
/// variables cannot change, so a copy is equivalent.
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
    let _package = scope::enter_package(name);
    let imported = imports::imported_symbols(node.clone(), tree, packages)?;
    let aliases = imported.aliases.clone();
    let module = Module::from_module_node_with_parameter_overrides(
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
    let symbols = module
        .symbols
        .expect("a package analysis keeps the package symbols");
    // The package's own names and locals become qualified; the names its
    // imports bind are replaced by the items they denote.
    let own: HashSet<String> = symbols
        .declared_names()
        .into_iter()
        .filter(|declared| !aliases.contains_key(declared))
        .collect();
    let mut names: HashMap<String, String> = own
        .iter()
        .map(|declared| (declared.clone(), format!("{name}::{declared}")))
        .collect();
    for local in symbols.local_names() {
        if !local.contains("::") {
            names.insert(local.clone(), format!("{name}::{local}"));
        }
    }
    names.extend(aliases);
    let symbols = symbols.renamed(&names);
    Ok(Package {
        own,
        dependencies: Vec::new(),
        symbols,
    })
}
