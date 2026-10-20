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
    /// Source-call identities shared with modules importing these package bodies.
    pub(super) call_sites: std::rc::Rc<std::cell::Cell<usize>>,
    pub(crate) interface_plan: Arc<interfaces::NativeInterfacePlan>,
    /// Original syntax shared with interface instances; never generated source.
    pub(crate) interface_sources: HashMap<String, (std::rc::Rc<SyntaxTree>, PathBuf)>,
    pub(super) timescales: Arc<timescales::Timescales>,
    packages: HashMap<String, Arc<Package>>,
    /// The compilation unit of each source file that declares items outside
    /// its modules and packages.
    units: HashMap<PathBuf, Arc<Unit>>,
    /// What analyzing the packages warned about.
    warnings: Vec<super::warnings::AnalyzerWarning>,
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
    /// Whether a native scope position `offset` sees the item `name`
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
    /// The native token position of the item that declares each name.
    pub declared_at: HashMap<String, usize>,
    /// The subroutines of the unit, which are visible anywhere in it.
    subroutines: HashSet<String>,
    /// The package imports of the unit: their native token positions, packages, and
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
    /// Global simulation precision, as a power of ten seconds (IEEE 1800-2023 3.14.3).
    pub fn global_time_precision(&self) -> i32 {
        self.timescales.global_precision()
    }

    /// Resolved local time unit and precision of a module or package.
    pub fn scope_time_scale(&self, name: &str) -> Option<(i32, i32)> {
        self.timescales.scope_scale(name)
    }

    /// Use interface bindings supplied by the hierarchy's selected roots.
    pub fn set_interface_plan(&mut self, plan: interfaces::NativeInterfacePlan) {
        self.interface_plan = Arc::new(plan);
    }

    pub(crate) fn with_warnings(mut self, warnings: Vec<super::warnings::AnalyzerWarning>) -> Self {
        super::warnings::append(&mut self.warnings, warnings);
        self
    }

    /// What analyzing the packages warned about, such as constant functions
    /// that call `$exit`.
    pub fn warnings(&self) -> &[super::warnings::AnalyzerWarning] {
        &self.warnings
    }

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
    pub(crate) fn analyze(trees: &[(&SyntaxTree, &Path)]) -> Result<Self, AnalyzerError> {
        Self::analyze_with_roots(trees, None)
    }

    pub(crate) fn analyze_with_roots(
        trees: &[(&SyntaxTree, &Path)],
        roots: Option<&[&str]>,
    ) -> Result<Self, AnalyzerError> {
        Self::analyze_with_options(trees, roots, false)
    }

    pub(crate) fn analyze_with_options(
        trees: &[(&SyntaxTree, &Path)],
        roots: Option<&[&str]>,
        allow_mixed_timescales: bool,
    ) -> Result<Self, AnalyzerError> {
        let mut warnings = Vec::new();
        for (tree, _) in trees {
            super::warnings::append(&mut warnings, super::warnings::packages(tree));
        }
        match Self::analyze_with_roots_impl(trees, roots, allow_mixed_timescales) {
            Ok(packages) => Ok(packages.with_warnings(warnings)),
            Err(error) => Err(error.with_warnings(warnings)),
        }
    }

    fn analyze_with_roots_impl(
        trees: &[(&SyntaxTree, &Path)],
        roots: Option<&[&str]>,
        allow_mixed_timescales: bool,
    ) -> Result<Self, AnalyzerError> {
        let mut timescales =
            timescales::Timescales::collect_with_options(trees, allow_mixed_timescales)?;
        if let Some(roots) = roots {
            timescales.select_roots(roots);
        }
        let mixed_timescales = timescales.has_mixed_timescales();
        let timescales = Arc::new(timescales);
        let state = std::rc::Rc::new(AnalysisState::default());
        let mut packages = Self::analyze_packages(trees, timescales, &state)?;
        if mixed_timescales {
            packages
                .warnings
                .push(super::warnings::AnalyzerWarning::MixedTimescale);
        }
        packages.call_sites = state.next_call_site.clone();
        let units: Vec<_> = trees
            .iter()
            .map(|(tree, path)| (tree.compilation_scope(), *path))
            .collect();
        for (tree, path) in &units {
            if tree.body_membership()?.is_empty() {
                continue;
            }
            let visibility = unit_visibility(tree)?;
            let scope = analyze_package(UNIT, tree, &packages, &state);
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

    fn analyze_packages(
        trees: &[(&SyntaxTree, &Path)],
        timescales: Arc<timescales::Timescales>,
        state: &std::rc::Rc<AnalysisState>,
    ) -> Result<Self, AnalyzerError> {
        let mut declarations: Vec<(String, SyntaxTree)> = Vec::new();
        for (tree, _) in trees {
            // Only modules and packages are analyzed, so an import declared
            // at compilation-unit scope would never be found.
            for item in &tree.declarations().source_items {
                if let celox_sv_syntax::SourceItemKind::Declaration(id) = item.kind
                    && matches!(
                        tree.declarations().body_item(id).kind,
                        celox_sv_syntax::BodyItemKind::Dpi(_)
                    )
                {
                    return Err(AnalyzerError::Unsupported(
                        "DPI-C import or export at compilation-unit scope (declare it in a \
                         module or package)"
                            .to_string(),
                    ));
                }
            }
            for handle in tree.root_scopes()? {
                if handle
                    .unit(tree.declarations())
                    .is_none_or(|unit| unit.kind != celox_sv_syntax::UnitKind::Package)
                {
                    continue;
                }
                let scoped = tree.scoped(handle)?;
                let name = instances::normalize_identifier(&scoped.root_name()?);
                if declarations.iter().any(|(known, ..)| *known == name) {
                    return Err(AnalyzerError::DuplicatePackage { name });
                }
                declarations.push((name, scoped));
            }
        }
        if declarations.is_empty() {
            return Ok(Self {
                timescales,
                ..Self::default()
            });
        }
        let names: HashSet<&str> = declarations
            .iter()
            .map(|(name, ..)| name.as_str())
            .collect();
        let dependencies: Vec<Vec<String>> = declarations
            .iter()
            .map(|(name, tree)| {
                Ok(imports::ScopeImports::from_scope(tree)?
                    .packages
                    .into_iter()
                    .filter(|package| package != name && names.contains(package.as_str()))
                    .collect())
            })
            .collect::<Result<_, AnalyzerError>>()?;
        let mut order = Vec::new();
        let mut active = Vec::new();
        for index in 0..declarations.len() {
            visit(index, &declarations, &dependencies, &mut order, &mut active)?;
        }
        // Package bodies can query selected hierarchy roots while they are
        // collected, before any module is analyzed.
        let mut packages = Self {
            timescales,
            ..Self::default()
        };
        for index in order {
            let (name, tree) = &declarations[index];
            let package = analyze_package(name, tree, &packages, state)?;
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
pub(super) fn package_variables(
    name: &str,
    tree: &SyntaxTree,
) -> Result<HashSet<String>, AnalyzerError> {
    use celox_sv_syntax::{BodyItemKind, DataDeclaration};
    let native = tree.declarations();
    let mut variables = HashSet::default();
    for id in tree.body_membership()? {
        match native.body_item(id).kind {
            BodyItemKind::Data(id) => {
                if let DataDeclaration::Variables(decl) | DataDeclaration::Constructed(decl) =
                    native.data_declaration(id)
                    && !decl.constant
                {
                    variables.extend(
                        decl.variables.iter().map(|variable| {
                            instances::normalize_identifier(&variable.name.spelling)
                        }),
                    );
                }
            }
            BodyItemKind::Instance(id) => variables.extend(
                native
                    .instantiation(id)
                    .instances
                    .iter()
                    .map(|instance| instances::normalize_identifier(&instance.name.spelling)),
            ),
            BodyItemKind::Net(id) => {
                let decl = native.net_declaration(id);
                return Err(AnalyzerError::Unsupported(format!(
                    "package net `{name}::{}`",
                    decl.variables
                        .first()
                        .map(|var| var.name.spelling.as_str())
                        .unwrap_or("?")
                )));
            }
            _ => {}
        }
    }
    Ok(variables)
}

fn visit(
    index: usize,
    declarations: &[(String, SyntaxTree)],
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
    tree: &SyntaxTree,
    packages: &Packages,
    state: &std::rc::Rc<AnalysisState>,
) -> Result<Package, AnalyzerError> {
    let context = AnalysisContext {
        syntax_tree: tree,
        state: state.clone(),
        parameter_overrides: Cow::Owned(HashMap::default()),
    };
    let tree = &context;
    let _timescales =
        timescales::install_for_tree(&tree.state.timescales, packages.timescales.clone(), tree)?;
    let first = {
        // Inside the package, `p::x` names its own item `x`.
        let _package = scope::enter_package(&tree.state, name);
        analyze_package_scope(name, tree, packages, None)?
    };
    if !imports::ScopeImports::from_scope(tree)?
        .qualified
        .iter()
        .any(|(package, _)| package == name)
    {
        return Ok(first);
    }
    // A self-qualified reference `p::x` is not hidden by a local `x` of a
    // function (IEEE 1800-2023 26.3). Analyze the package again with its
    // items from the first analysis under their qualified names.
    analyze_package_scope(name, tree, packages, Some(&first.symbols))
}

fn analyze_package_scope(
    name: &str,
    tree: &AnalysisContext<'_>,
    packages: &Packages,
    own_items: Option<&ScopeSymbols>,
) -> Result<Package, AnalyzerError> {
    let imports::ResolvedImports {
        symbols: mut imported,
        exports,
    } = imports::resolve_imports(tree, packages, None)?;
    if let Some(own_items) = own_items {
        imported.extend(own_items);
    }
    let aliases = imported.aliases.clone();
    let imported_locals = imported.local_names();
    // The names the package declares, from its syntax: an escaped name may
    // contain `::` itself.
    let own = imports::scope_names(tree)?;
    let variables = package_variables(name, tree)?;
    let mut module = Module::from_scope_with_parameter_overrides(
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

/// Where the items of the compilation unit `unit` are declared.
fn unit_visibility(tree: &SyntaxTree) -> Result<UnitVisibility, AnalyzerError> {
    use celox_sv_syntax::{BodyItemKind, SourceItemKind};
    let mut visibility = UnitVisibility::default();
    for source in &tree.declarations().source_items {
        let SourceItemKind::Declaration(id) = source.kind else {
            continue;
        };
        let item = tree.declarations().body_item(id);
        let offset = source.tokens.start;
        let subroutine = matches!(item.kind, BodyItemKind::Subroutine(_));
        for name in imports::item_names(id, tree) {
            if subroutine {
                visibility.subroutines.insert(name.clone());
            }
            visibility.declared_at.entry(name).or_insert(offset);
        }
        if matches!(item.kind, BodyItemKind::PackageBinding(_)) {
            for view in package_owners::item(id, tree)? {
                for (package, name) in view.names(tree) {
                    visibility.imports.push((offset, package, name));
                }
            }
        }
    }
    Ok(visibility)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_and_module_select_calls_have_distinct_sites() {
        let source = r#"
            package common;
                int count;
                function automatic int draw(); count++; return count; endfunction
            endpackage
            package p;
                function automatic bit pick(input bit [7:0] x);
                    return x[common::draw()];
                endfunction
            endpackage
            module Top(input bit [7:0] x, output bit y);
                import p::*;
                function automatic bit local_pick(input bit [7:0] x);
                    return x[common::draw()];
                endfunction
                assign y = pick(x) ^ local_pick(x);
            endmodule
        "#;
        let path = Path::new("package_module_sites.sv");
        let tree = crate::source_tree::parse_source(source, path).unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        for analyzed in [
            Source::from_syntax(&tree).unwrap(),
            Source::from_syntax_module_with_parameter_overrides(&tree, "Top", &HashMap::default())
                .unwrap(),
        ] {
            let module = &analyzed.modules[0];
            let mut indices = Vec::new();
            for name in ["p::pick", "local_pick"] {
                let function = module.subroutines.iter().find(|f| f.name == name).unwrap();
                let [Stmt::Return(Some(Expr::Select { msb, lsb, .. }))] = function.body.as_slice()
                else {
                    panic!("unexpected function body: {:?}", function.body);
                };
                assert_eq!(msb, lsb);
                indices.push(msb.clone());
            }
            assert_ne!(
                indices[0], indices[1],
                "different source calls must not be deduplicated"
            );
        }
    }

    #[test]
    fn effectful_select_calls_have_distinct_sites_across_packages_and_units() {
        let source = r#"
            package common;
                int count;
                function automatic int draw();
                    count++;
                    return count;
                endfunction
            endpackage
            package p;
                function automatic bit pick(input bit [7:0] x);
                    return x[common::draw()];
                endfunction
            endpackage
            package q;
                function automatic bit pick(input bit [7:0] x);
                    return x[common::draw()];
                endfunction
            endpackage
            function automatic bit pick(input bit [7:0] x);
                return x[common::draw()];
            endfunction
        "#;
        let path = Path::new("package_call_sites.sv");
        let tree = crate::source_tree::parse_source(source, path).unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let packages = Packages::analyze(&[(&tree, path)]).unwrap();
        let scopes = [
            &packages.packages["p"].symbols,
            &packages.packages["q"].symbols,
            &packages.units[path].scope.as_ref().unwrap().symbols,
        ];
        let mut sites = HashSet::default();
        for symbols in scopes {
            let function = symbols
                .subroutines
                .iter()
                .find(|f| f.name.ends_with("::pick"))
                .unwrap();
            let [Stmt::Return(Some(Expr::Select { msb, lsb, .. }))] = function.body.as_slice()
            else {
                panic!("unexpected function body: {:?}", function.body);
            };
            assert_eq!(msb, lsb, "copies of one select index retain their identity");
            let ConstExpr::Function {
                site: Some(site), ..
            } = msb
            else {
                panic!("expected effectful index call: {msb:?}");
            };
            assert!(
                sites.insert(*site),
                "distinct source calls share site {site}"
            );
        }
    }
}
