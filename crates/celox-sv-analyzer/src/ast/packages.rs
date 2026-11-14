//! Packages (IEEE 1800-2023 26) and compilation units (3.12.1).
//!
//! A package is analyzed after the packages it depends on, with the
//! collectors that analyze a module: its declarations form a scope of their
//! own. Its symbols are exported under their qualified names `p::x`, with the
//! references among them bound to those names (see the `scope` module). A
//! module or package that uses packages starts from their symbols, and from
//! an alias for each name its imports bind (see the `imports` module). Importing
//! scopes can request additional class specializations; their declarations and
//! bodies are bound in the retained package scope (see `classes::foreign`).
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
    /// What analyzing the packages made that modules analyzed with them
    /// continue from.
    design: Design,
    pub(crate) interface_plan: Arc<interfaces::NativeInterfacePlan>,
    /// All interface declaration names, including unsupported non-ANSI bodies.
    /// Namespace validation must not depend on sources selected for binding.
    pub(crate) interface_names: HashSet<String>,
    /// Original syntax shared with interface instances; never generated source.
    pub(crate) interface_sources: HashMap<String, (std::rc::Rc<SyntaxTree>, PathBuf)>,
    pub(super) timescales: Arc<timescales::Timescales>,
    packages: HashMap<String, Arc<Package>>,
    /// The compilation unit of each source file that declares items outside
    /// its modules and packages.
    units: HashMap<PathBuf, Arc<Unit>>,
    /// What analyzing each interface that no source references found wrong
    /// with it, by its name (see [`Self::check_unreferenced_interfaces`]).
    unreferenced_interfaces: Vec<(String, AnalyzerError)>,
    /// What analyzing the packages warned about.
    warnings: Vec<super::warnings::AnalyzerWarning>,
    /// The virtual interfaces of the design, which the modules that use the
    /// packages are analyzed with.
    pub(crate) virtual_interfaces: Option<Arc<virtual_interfaces::VirtualInterfaces>>,
    /// The assertion control state of the design, when a source calls
    /// `$asserton` or `$assertoff` (IEEE 1800-2023 20.11): the state module
    /// of a package no source declares, whose variable every module shares.
    assertion_control: Option<crate::ir::Module>,
}

/// What the analyses of the modules of a design add to the packages they
/// use: the specializations of package class templates the modules request,
/// the semantic types of the design and its call sites. Each analysis takes
/// it as `&mut`, starting from [`Packages::design`].
#[derive(Clone, Debug, Default)]
pub struct Design {
    pub(super) foreign: classes::foreign::Registry,
    /// Source-call identities shared with modules importing these package bodies.
    pub(super) call_sites: usize,
    pub(super) types: semantic_types::TyTable,
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

/// The identity of item `name` of the compilation unit of source `path`:
/// each source file's unit is a scope of its own (IEEE 1800-2023 3.12.1).
pub(super) fn unit_item_identity(path: &std::path::Path, name: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = fxhash::FxHasher::default();
    path.hash(&mut hasher);
    format!("{UNIT}::{name}@{:016x}", hasher.finish())
}

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
    pub(super) state_module: Option<crate::ir::Module>,
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
    /// The authored name of module `name`, which differs for a
    /// specialization of a module with generic interface ports.
    pub fn authored_module_name<'a>(&'a self, name: &'a str) -> &'a str {
        self.interface_plan
            .modules
            .get(name)
            .map_or(name, |plan| plan.original.as_str())
    }

    /// The virtual interfaces the packages were analyzed with.
    pub fn virtual_interfaces(&self) -> Option<&Arc<virtual_interfaces::VirtualInterfaces>> {
        self.virtual_interfaces.as_ref()
    }

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

    /// The first error that analyzing a compilation unit found. A unit is
    /// otherwise analyzed only when a module uses one of its items, and a
    /// design of compilation-unit items alone is checked through this.
    pub fn compilation_unit_error(&self) -> Option<&AnalyzerError> {
        let mut paths = self.units.keys().collect::<Vec<_>>();
        paths.sort();
        paths
            .into_iter()
            .find_map(|path| self.units[path].scope.as_ref().err())
            .or_else(|| self.unreferenced_interfaces.first().map(|(_, error)| error))
    }

    /// The first error analyzing a compilation unit found that makes the
    /// design illegal, not a construct Celox does not support: an illegal
    /// declaration is an error even where no module uses it (IEEE 1800-2023
    /// 3.12.1), while an unsupported one waits for a use, as
    /// [`Self::compilation_unit_error`] says.
    pub fn compilation_unit_illegality(&self) -> Option<&AnalyzerError> {
        let mut paths = self.units.keys().collect::<Vec<_>>();
        paths.sort();
        paths
            .into_iter()
            .find_map(|path| {
                self.units[path]
                    .scope
                    .as_ref()
                    .err()
                    .filter(|error| !error.is_unsupported())
            })
            .or_else(|| {
                self.unreferenced_interfaces
                    .iter()
                    .map(|(_, error)| error)
                    .find(|error| !error.is_unsupported())
            })
    }

    /// Analyze each interface of `trees` that no source instantiates or
    /// names, with the defaults of its parameters, for what is wrong with
    /// it: no hierarchy elaborates one (IEEE 1800-2023 3.12.1), though its
    /// body is compiled, and data are declared before they are used (6.6),
    /// so a value its body reads that is not declared is reported (see
    /// [`AnalysisContext::declared_values`]). One with a parameter without
    /// a default, which only an instance can give, is left alone.
    pub(crate) fn check_unreferenced_interfaces(
        &mut self,
        trees: &[(&SyntaxTree, &Path)],
    ) -> Result<(), AnalyzerError> {
        let referenced = trees
            .iter()
            .flat_map(|(tree, _)| {
                let mut references = interfaces::body_component_references(tree);
                for unit in tree.declarations().all_units() {
                    references.extend(
                        unit.interface_references(tree.declarations())
                            .map(|name| name.spelling.clone()),
                    );
                }
                references
            })
            .map(|name| normalize_identifier(&name))
            .collect::<HashSet<_>>();
        let mut found = Vec::new();
        for (tree, path) in trees {
            for handle in tree.root_scopes()? {
                let scoped = tree.scoped(handle)?;
                let unit = scoped.root_unit()?;
                let defaulted = |parameter: &celox_sv_syntax::Parameter| match &parameter.kind {
                    celox_sv_syntax::ParameterKind::Type { default } => default.is_some(),
                    celox_sv_syntax::ParameterKind::Value { default, .. } => default.is_some(),
                };
                if unit.kind != celox_sv_syntax::UnitKind::Interface
                    || unit.port_style != celox_sv_syntax::PortStyle::Ansi
                    || !unit
                        .parameters
                        .iter()
                        .flatten()
                        .chain(&unit.body_parameters)
                        .all(defaulted)
                {
                    continue;
                }
                let name = normalize_identifier(&scoped.root_name()?);
                if referenced.contains(&name) {
                    continue;
                }
                let overrides = HashMap::default();
                let context = AnalysisContext {
                    declared_values: true,
                    ..AnalysisContext::unspecialized(&scoped)
                };
                let session = &mut Session::with_packages(self, &mut self.design());
                if let Err(error) = interface_ast::analyze(
                    &context.child(&scoped, &overrides),
                    session,
                    path,
                    self,
                    &overrides,
                    &[],
                    &ModuleInterfaces::default(),
                    false,
                ) {
                    found.push((name, error));
                }
            }
        }
        self.unreferenced_interfaces = found;
        Ok(())
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
            .chain(&self.assertion_control)
    }

    /// Whether a source of the design calls an assertion control task, so
    /// that its assertions check the assertion control state.
    pub(crate) fn controls_assertions(&self) -> bool {
        self.assertion_control.is_some()
    }

    /// Package state after specialization requests made by importing scopes.
    /// The state of the design that analyzing the packages left, which the
    /// analyses of its modules continue.
    pub fn design(&self) -> Design {
        self.design.clone()
    }

    pub fn specialized_state_modules(&self, design: &Design) -> Vec<crate::ir::Module> {
        let specialized = design.foreign.states();
        // Different compilation units may each have a state module named
        // `$unit`; preserve every one rather than indexing them by name.
        let mut states = self
            .state_modules()
            .filter(|module| !specialized.contains_key(module.name()))
            .cloned()
            .collect::<Vec<_>>();
        states.extend(specialized.values().cloned());
        states.sort_by(|left, right| {
            left.name().cmp(right.name()).then_with(|| {
                left.signals()
                    .first()
                    .map(|signal| signal.name())
                    .cmp(&right.signals().first().map(|signal| signal.name()))
            })
        });
        states
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
        Self::analyze_with_options(trees, roots, crate::AnalysisOptions::default())
    }

    pub(crate) fn analyze_with_options(
        trees: &[(&SyntaxTree, &Path)],
        roots: Option<&[&str]>,
        options: crate::AnalysisOptions,
    ) -> Result<Self, AnalyzerError> {
        let mut warnings = Vec::new();
        for (tree, _) in trees {
            super::warnings::append(&mut warnings, super::warnings::packages(tree));
        }
        match Self::analyze_with_roots_impl(trees, roots, options) {
            Ok(packages) => Ok(packages.with_warnings(warnings)),
            Err(error) => Err(error.with_warnings(warnings)),
        }
    }

    fn analyze_with_roots_impl(
        trees: &[(&SyntaxTree, &Path)],
        roots: Option<&[&str]>,
        options: crate::AnalysisOptions,
    ) -> Result<Self, AnalyzerError> {
        let mut timescales = timescales::Timescales::collect_with_options(trees, options.clone())?;
        if let Some(roots) = roots {
            timescales.select_roots(roots);
        }
        let mixed_timescales = timescales.has_mixed_timescales();
        let timescales = Arc::new(timescales);
        let session = &mut Session {
            virtual_interfaces: options.virtual_interfaces.clone(),
            ..Session::default()
        };
        let mut packages = Self::analyze_packages(trees, timescales, session)?;
        packages.virtual_interfaces = options.virtual_interfaces;
        if trees
            .iter()
            .any(|(tree, _)| assertion_control::called(tree.declarations()))
        {
            packages.assertion_control = Some(assertion_control::state());
        }
        if mixed_timescales {
            packages
                .warnings
                .push(super::warnings::AnalyzerWarning::MixedTimescale);
        }
        let units: Vec<_> = trees
            .iter()
            .map(|(tree, path)| (tree.compilation_scope(), *path))
            .collect();
        for (tree, path) in &units {
            if tree.body_membership()?.is_empty() {
                continue;
            }
            let visibility = unit_visibility(tree)?;
            // The classes of each compilation unit are its own, though those
            // of two sources may share a name.
            session.classes.clear();
            session.class_specializations.clear();
            session.class_specialization_names.clear();
            session.class_declaration_scopes.clear();
            session.class_declarations_repeated.clear();
            let scope = analyze_package(UNIT, tree, &packages, session);
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
        // Modules analyzed with the packages start from what analyzing
        // them made.
        packages.design = Design {
            call_sites: session.next_call_site,
            foreign: std::mem::take(&mut session.foreign),
            types: std::mem::take(&mut session.types),
        };
        Ok(packages)
    }

    fn analyze_packages(
        trees: &[(&SyntaxTree, &Path)],
        timescales: Arc<timescales::Timescales>,
        session: &mut Session,
    ) -> Result<Self, AnalyzerError> {
        let mut declarations: Vec<(String, SyntaxTree)> = Vec::new();
        for (tree, _) in trees {
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
            let package = analyze_package(name, tree, &packages, session)?;
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
    session: &mut Session,
) -> Result<Package, AnalyzerError> {
    let context = AnalysisContext {
        syntax_tree: tree,
        parameter_overrides: Cow::Owned(HashMap::default()),
        imported: Arc::default(),
        package: None,
        functions: const_functions::ConstantFunctions::default(),
        root_subroutines: false,
        active_interfaces: Arc::default(),
        timescale: Arc::default(),
        interface_instance: None,
        declared_values: true,
    };
    let tree = &context;
    let timed = AnalysisContext {
        timescale: timescales::for_tree(packages.timescales.clone(), tree)?,
        ..tree.clone()
    };
    let tree = &timed;
    let first = {
        // Inside the package, `p::x` names its own item `x`.
        analyze_package_scope(name, &tree.in_package(name), session, packages, None)?
    };
    // No class of another package can be a base in this one, which names
    // none.
    if self_qualified_names_are_plain(name, tree, |package| packages.get(package).is_some())?
        .is_some()
    {
        return Ok(first);
    }
    // A self-qualified reference `p::x` is not hidden by a local `x` of a
    // function (IEEE 1800-2023 26.3). Analyze the package again with its
    // items from the first analysis under their qualified names.
    analyze_package_scope(name, tree, session, packages, Some(&first.symbols))
}

/// Whether each name that package `name` qualifies with its own name,
/// `name::x`, is its item `x` also without the qualifier where it is
/// written: nothing nested in the package around the reference declares
/// `x`, and no class around it inherits a member `x` (IEEE 1800-2023 23.9,
/// 26.3). One analysis of the package, which reads `name::x` as `x`, then
/// binds them all.
///
/// A class whose base the syntax does not tell, such as a type parameter,
/// may inherit a member of any class its package can name: of its own, when
/// it names no other package, as far as the package itself makes its
/// specializations. The classes of that kind around a reference are given
/// back: a specialization that another scope asks of one of them may
/// extend a class of that scope, whose members the package does not know.
/// `is_package` tells the packages among the scopes of qualified names.
pub(super) fn self_qualified_names_are_plain(
    name: &str,
    tree: &AnalysisContext<'_>,
    is_package: impl Fn(&str) -> bool,
) -> Result<Option<HashSet<String>>, AnalyzerError> {
    use celox_sv_syntax::{DataTypeKind, TokenRange};
    let imports = imports::ScopeImports::from_scope(tree)?;
    let references = imports
        .qualified
        .iter()
        .zip(&imports.qualified_tokens)
        .filter(|((package, _), _)| package == name)
        .map(|((_, item), token)| (item.as_str(), *token))
        .collect::<Vec<_>>();
    if references.is_empty() {
        return Ok(Some(HashSet::default()));
    }
    let native = tree.declarations();
    let item = |name: &celox_sv_syntax::Name| instances::normalize_identifier(&name.spelling);
    // The members a class declares, by the name of the class. Classes of
    // one name in several scopes of the source share their members here,
    // which only hides more.
    let mut members = HashMap::<String, HashSet<String>>::default();
    for class in &native.classes {
        let names = members.entry(item(&class.name)).or_default();
        // The members of an enumeration are declared with its type (6.19).
        let enumeration = |names: &mut HashSet<String>, id| {
            if let DataTypeKind::Enum { items, .. } = &native.data_type(id).kind {
                names.extend(items.iter().map(|member| item(&member.name)));
            }
        };
        let parameters = class.parameter_ports.iter().chain(&class.parameters);
        for parameter in parameters.flat_map(|id| &native.parameter_declaration(*id).parameters) {
            names.insert(item(&parameter.name));
            if let celox_sv_syntax::ParameterKind::Value { data_type, .. } = &parameter.kind {
                enumeration(names, *data_type);
            }
        }
        for alias in &class.type_aliases {
            let alias = native.type_alias(*alias);
            names.insert(item(&alias.name));
            if let celox_sv_syntax::TypeAliasTarget::DataType(id) = alias.target {
                enumeration(names, id);
            }
        }
        for property in &class.properties {
            let declaration = &property.declaration;
            names.extend(
                declaration
                    .variables
                    .iter()
                    .map(|variable| item(&variable.name)),
            );
            enumeration(names, declaration.data_type);
        }
        for method in &class.methods {
            names.insert(item(&native.subroutine(method.subroutine).name));
        }
        // The classes it declares (8.23).
        names.extend(
            native
                .classes
                .iter()
                .filter(|nested| {
                    class.tokens.start < nested.tokens.start
                        && nested.tokens.end <= class.tokens.end
                })
                .map(|nested| item(&nested.name)),
        );
    }
    // The scopes of qualified names that are packages; others are classes
    // and types.
    let foreign = imports
        .packages
        .iter()
        .any(|package| package != name && is_package(package));
    // The classes the package itself declares, by their names, which the
    // plain name of one is in every scope of the package that declares no
    // other of the name: a class of another scope of the source may share
    // the name with a class the package imports.
    let package = tree.root_unit().ok().map(|unit| unit.tokens);
    let mut own = HashMap::<String, Option<&celox_sv_syntax::ClassDeclaration>>::default();
    for class in native.classes.iter().filter(|class| {
        classes::enclosing_unit(native, class.tokens).map(|unit| unit.tokens) == package
            && !native.classes.iter().any(|outer| {
                outer.tokens.start < class.tokens.start && class.tokens.end <= outer.tokens.end
            })
    }) {
        own.entry(item(&class.name))
            .and_modify(|known| *known = None)
            .or_insert(Some(class));
    }
    // Whether `class` declares or inherits a member `wanted`, or else the
    // classes it is or extends whose base the syntax does not tell.
    let inherits = |class: &celox_sv_syntax::ClassDeclaration, wanted: &str| {
        let mut seen = HashSet::default();
        let mut pending = vec![class];
        let mut unknown = Vec::new();
        while let Some(class) = pending.pop() {
            if members[&item(&class.name)].contains(wanted) {
                return Err(());
            }
            if !seen.insert(class.tokens.start) {
                continue;
            }
            // The class it extends; an interface class inherits the
            // parameters and types of those it extends (8.26.3).
            let parents = class.base.iter().map(|base| base.data_type).chain(
                class
                    .interface
                    .then_some(&class.interfaces)
                    .into_iter()
                    .flatten()
                    .copied(),
            );
            for parent in parents {
                let known = match &native.data_type(parent).kind {
                    DataTypeKind::Named(path) if path.len() == 1 => {
                        let base = item(&path[0].name);
                        // A parameter or type of the class hides a class of
                        // its name.
                        (!members[&item(&class.name)].contains(&base))
                            .then(|| own.get(&base).copied().flatten())
                            .flatten()
                    }
                    _ => None,
                };
                match known {
                    Some(base) => pending.push(base),
                    None if foreign || members.values().any(|names| names.contains(wanted)) => {
                        return Err(());
                    }
                    None => unknown.push(item(&class.name)),
                }
            }
        }
        Ok(unknown)
    };
    let mut open = HashSet::default();
    // The bodies of the methods declared out of their classes, which are in
    // the scopes of those (8.24), by the names of the classes.
    let methods = native
        .subroutines
        .iter()
        .filter_map(|subroutine| Some((subroutine.tokens, item(&subroutine.scope.last()?.name))))
        .collect::<Vec<_>>();
    // The array methods with a clause that declares their iterators (7.12).
    let scoped = native
        .expressions
        .iter()
        .filter(|expression| {
            matches!(
                expression.kind,
                celox_sv_syntax::ExprKind::ArrayMethodWith { .. }
            )
        })
        .map(|expression| expression.tokens)
        .collect::<Vec<_>>();
    let around = |tokens: &TokenRange, token: usize| tokens.start <= token && token < tokens.end;
    for (wanted, token) in references {
        let target = TokenRange {
            start: token,
            end: token + 1,
        };
        if scoped.iter().any(|tokens| around(tokens, token))
            || dimensions::hides_scope_item(tree, target, wanted)?
        {
            return Ok(None);
        }
        // The classes around the reference, and what they inherit.
        for class in native
            .classes
            .iter()
            .filter(|class| around(&class.tokens, token))
        {
            match inherits(class, wanted) {
                Ok(unknown) => open.extend(unknown),
                Err(()) => return Ok(None),
            }
        }
        for (_, class) in methods.iter().filter(|(tokens, _)| around(tokens, token)) {
            let mut declared = native
                .classes
                .iter()
                .filter(|declared| item(&declared.name) == *class);
            match (declared.next(), declared.next()) {
                (Some(class), None) => match inherits(class, wanted) {
                    Ok(unknown) => open.extend(unknown),
                    Err(()) => return Ok(None),
                },
                _ => return Ok(None),
            }
        }
    }
    Ok(Some(open))
}

#[cfg(test)]
thread_local! {
    /// Analyses of the scopes of packages and compilation units, also for
    /// the specializations other scopes ask of them.
    static SCOPE_ANALYSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn analyze_package_scope(
    name: &str,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    packages: &Packages,
    own_items: Option<&ScopeSymbols>,
) -> Result<Package, AnalyzerError> {
    let imports::ResolvedImports {
        symbols: imported,
        exports,
    } = imports::resolve_imports(tree, packages, None)?;
    if own_items.is_none() && name != UNIT {
        session.foreign.register(
            name,
            tree.syntax_tree,
            &imported,
            packages.timescales.clone(),
        );
    }
    analyze_package_with_symbols(name, tree, session, imported, exports, own_items)
}

pub(super) fn analyze_package_with_symbols(
    name: &str,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    mut imported: ScopeSymbols,
    exports: HashMap<String, String>,
    own_items: Option<&ScopeSymbols>,
) -> Result<Package, AnalyzerError> {
    #[cfg(test)]
    SCOPE_ANALYSES.with(|count| count.set(count.get() + 1));
    if let Some(own_items) = own_items {
        imported.extend(own_items);
    }
    let aliases = imported.aliases.clone();
    let imported_locals = imported.local_names();
    // The names the package declares, from its syntax: an escaped name may
    // contain `::` itself.
    let own = imports::scope_names(tree)?;
    let mut variables = package_variables(name, tree)?;
    let mut module = Module::from_scope_with_parameter_overrides(
        tree,
        session,
        "",
        &HashMap::default(),
        &InterfaceLookup {
            local: &ModuleInterfaces::default(),
            extra: &ModuleInterfaces::default(),
        },
        Arc::new(imported),
    )?;
    for subroutine in &module.subroutines {
        variables.extend(subroutine.persistent_locals.iter().cloned());
    }
    let mut symbols = module
        .symbols
        .take()
        .expect("a package analysis keeps the package symbols");
    // The static properties of its classes are variables of the package,
    // shared by every scope that uses them (IEEE 1800-2023 8.9).
    for class in &symbols.classes {
        variables.extend(
            class
                .statics
                .iter()
                .filter(|property| property.owner == class.name)
                .map(|property| property.variable.clone()),
        );
    }
    // Each scope declares its own signals for virtual interfaces.
    symbols
        .signals
        .retain(|signal| virtual_interfaces::VirtualInterfaceSignal::of(&signal.name).is_none());
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
            diagnostics: Vec::new(),
        })?;
        ir.modules().first().cloned()
    };
    let qualified = |item: &String| format!("{name}::{item}");
    let mut names: HashMap<String, String> = own
        .iter()
        .map(|declared| (declared.clone(), qualified(declared)))
        .collect();
    let mut own_locals = HashSet::default();
    // The members of its classes are qualified by the classes already.
    let prefix = format!("{name}::");
    for local in symbols.local_names() {
        if !imported_locals.contains(&local) && !local.starts_with(&prefix) {
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

    /// How many times the scopes of the packages of `source` are analyzed.
    fn scope_analyses(source: &str) -> usize {
        SCOPE_ANALYSES.with(|count| count.set(0));
        crate::analyze_source(source, std::path::Path::new("self-qualified.sv"))
            .expect("the source analyzes");
        SCOPE_ANALYSES.with(|count| count.get())
    }

    #[test]
    fn plain_self_qualified_names_need_one_analysis() {
        // A package that qualifies its own items where nothing hides them is
        // analyzed once: `p::x` is `x` there (IEEE 1800-2023 26.3).
        let plain = "package p;
                localparam int X = 5;
                typedef enum { NONE, LOW } level_e;
                int counter;
                function automatic int f(int a); return p::X + a + p::LOW; endfunction
                class B; int v; endclass
                class D extends B;
                    function int g(); p::counter = p::f(1); return p::X; endfunction
                endclass
                class G #(type T = B) extends T;
                    function int h(); p::B b; return p::X + p::NONE; endfunction
                endclass
            endpackage
            module Top; endmodule";
        assert_eq!(scope_analyses(plain), 1);
    }

    #[test]
    fn foreign_specializations_lower_only_the_classes_they_change() {
        // A package analyzed again for a specialization that another scope
        // asks for takes over the methods its last analysis lowered, but
        // for the new classes and those they extend, whose dispatchers are
        // over one class more (IEEE 1800-2023 8.20, 8.25).
        use crate::ast::procedural::{CLASS_LOWERINGS, LOWER_EVERY_CLASS};
        let source = |classes: usize| {
            let others = (0..classes)
                .map(|i| {
                    format!(
                        "class K{i}; int v; function int get(); return v + {i}; endfunction endclass\n"
                    )
                })
                .collect::<String>();
            format!(
                "package p;
                    class B; virtual function int f(); return 1; endfunction endclass
                    class C #(int N = 2) extends B;
                        virtual function int f(); return N; endfunction
                    endclass
                    {others}
                endpackage
                module Top(output int o);
                    p::C#(5) c;
                    initial begin c = new; o = c.f(); end
                endmodule"
            )
        };
        let lowerings = |classes: usize, every: bool| {
            LOWER_EVERY_CLASS.with(|flag| flag.set(every));
            CLASS_LOWERINGS.with(|count| count.set(0));
            let ir = crate::analyze_source(&source(classes), std::path::Path::new("lowerings.sv"));
            LOWER_EVERY_CLASS.with(|flag| flag.set(false));
            ir.expect("the source analyzes");
            CLASS_LOWERINGS.with(|count| count.get())
        };
        // Each class of the package once, and the classes the module's
        // specialization changes once more, however many others there are.
        assert_eq!(lowerings(12, false) - lowerings(2, false), 10);
        assert!(lowerings(12, true) - lowerings(2, true) > 10);
    }

    #[test]
    fn foreign_specializations_analyze_a_plain_package_once_each() {
        // A scope that asks for a specialization of a class of the package
        // has the package analyzed for it: once, as the package itself is,
        // unless the specialization extends a class of that scope, which
        // may inherit members the package does not know that hide the names
        // it qualifies (8.25, 26.3).
        let analyses = |user: &str| {
            scope_analyses(&format!(
                "package p;
                    localparam int X = 5;
                    class B; int v; endclass
                    class C #(type T = int);
                        function int f(); return p::X; endfunction
                    endclass
                    class G #(type T = B) extends T;
                        function int g(); return p::X; endfunction
                    endclass
                endpackage
                {user}"
            ))
        };
        // The package and the compilation unit alone.
        let none = analyses("class U; int v; endclass module Top; endmodule");
        // A class of the unit as an argument that no class extends.
        let argument = analyses("class U; int v; endclass module Top; p::C#(U) c; endmodule");
        // A class of the unit that a class of the package extends.
        let base = analyses("class U; int v; endclass module Top; p::G#(U) g; endmodule");
        assert!(argument > none, "{argument}");
        assert!(base > argument, "{argument} {base}");
        // An escaped name may hold the text that starts the arguments of a
        // specialization.
        let escaped = scope_analyses(
            "package p;
                localparam int X = 5;
                class B; int v; endclass
                class \\G#(x #(type T = B) extends T;
                    function int g(); return p::X; endfunction
                endclass
            endpackage
            class U; int v; endclass
            module Top; p::\\G#(x #(U) g; endmodule",
        );
        let unescaped = scope_analyses(
            "package p;
                localparam int X = 5;
                class B; int v; endclass
                class G #(type T = B) extends T;
                    function int g(); return p::X; endfunction
                endclass
            endpackage
            class U; int v; endclass
            module Top; p::G #(U) g; endmodule",
        );
        assert_eq!(escaped, unescaped);
    }

    #[test]
    fn hidden_self_qualified_names_need_a_second_analysis() {
        // A declaration around the reference that gives the name to
        // something else hides the item from the plain name, not from the
        // qualified one: a formal, a variable of a block, a member of the
        // class or of a class it extends, and a member that a base which is
        // a type parameter may bring (IEEE 1800-2023 23.9, 26.3).
        for body in [
            "function automatic int f(int X); return p::X + X; endfunction",
            "function automatic int f(); int X; X = 1; return p::X + X; endfunction",
            "function automatic int f(); begin int X; X = 1; return p::X + X; end endfunction",
            "class C; int X; function int f(); return p::X + X; endfunction endclass",
            "class B; int X; endclass
             class D extends B; function int f(); return p::X + X; endfunction endclass",
            "class B; int X; endclass
             class G #(type T = B) extends T;
                 function int f(); return p::X; endfunction
             endclass",
            "class C; typedef enum { X, Z } e; function int f(); return p::X; endfunction endclass",
            // A method declared out of its class is in the scope of the
            // class (8.24).
            "class C; int X; extern function int f(); endclass
             function int C::f(); return p::X + X; endfunction",
            "class B; int X; endclass
             class D extends B; extern function int f(); endclass
             function int D::f(); return p::X + X; endfunction",
            // An interface class inherits the parameters and types of the
            // interface classes it extends (8.26.3).
            "interface class J; localparam int X = 1; endclass
             interface class I extends J;
                 pure virtual function int f(int a = p::X);
             endclass",
            // The iterator of an array method (7.12).
            "function automatic int f(int q[$]);
                 int found[$];
                 found = q.find(X) with (X == p::X);
                 return found.size();
             endfunction",
        ] {
            let source = format!(
                "package p; localparam int X = 5; {body} endpackage
                module Top; endmodule"
            );
            assert_eq!(scope_analyses(&source), 2, "{body}");
        }
    }

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
            Source::from_syntax_module_with_parameter_overrides(&tree, "Top", &ConstEnv::default())
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
