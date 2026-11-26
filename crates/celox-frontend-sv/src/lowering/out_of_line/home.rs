//! The function units of the root module that the state modules of the
//! packages call instead of lowering their own (see [`Home`]).
//!
//! The root module, the design's one instance of its module, lowers the
//! package subroutines its processes call into units, as the state module
//! of a package lowers those its initializers call. A call of a state module
//! names the root's unit instead when the unit does what the module's own
//! would: assembly resolves the call to the root instance's copy (see
//! `SimModule::imported_functions`), so each such subroutine is lowered and
//! compiled once. A call that any check below rejects lowers the module's
//! own unit, as before.
//!
//! The root's unit serves the state module when:
//! - every subroutine the module's lowering of the call reaches, through
//!   any chain of calls, has a counterpart in the root with the same
//!   analyzed body, the names of the package's own items qualified there
//!   (see [`Normalized`]); or both dispatch on the class of an object (a
//!   virtual method's dispatcher, a `$cast`), and each class the module
//!   dispatches selects a body of the root's dispatch that is the
//!   module's (see [`dispatch_covers`]). The root knows the classes of the
//!   design that use the package: an object of a class the module knows
//!   takes the same arm in both;
//! - every runtime event of the root's units the call reaches whose output
//!   names its scope (a report, a print of `%m`) reports in the scope of
//!   the caller: a unit's events have the scope of the instance its copy is
//!   in, so a unit of the root that names its scope receives a token of the
//!   scope that calls it and raises a copy of the site for the package's
//!   instance when a state module calls it (see `OutOfLine::scope_callers`).
//!   The scope of a host request's format is the calling process's;
//! - the call passes every formal and the result through the frame, its
//!   `ref` formals bound to properties or automatics of the caller, and
//!   the caller is a process kernel or a unit;
//! - neither module forces a variable procedurally, the two have the same
//!   time scale, and the unit stores no variable of the package that the
//!   module keeps out of units (see `ProcModule::is_excluded`).
//!
//! A unit suspends at the root's function sites, which the kernels of the
//! module therefore list first (see [`ProcModule::list_home_sites`]): their
//! variables are the root's, which the module names through variables of
//! its own bound to them, and the module's own function sites are numbered
//! after them.

use super::super::procedural::FunctionSites;
use super::*;
use std::fmt::Write as _;
use std::sync::Arc;

/// A function of the root that a state module may call (see the module
/// documentation).
#[derive(Debug, Clone)]
pub(in crate::lowering) struct Exported {
    /// Its index among the functions the root attached.
    index: u32,
    /// Its frame as a call builds it, without the unit.
    function: Function,
    /// The slot of each formal, by position: `None` for a `ref` formal.
    formals: Vec<Option<usize>>,
    /// The slot of the result.
    result: Option<usize>,
    /// Whether every formal but the `ref` ones and the result, if any, has
    /// a slot.
    frame_only: bool,
    /// The package variables it and its callees may store, by package and
    /// name.
    writes: Vec<(String, String)>,
    /// How the runtime events of it and its callees name their scope.
    scope: ScopeUse,
}

/// How the runtime events of a function and its callees name the scope
/// that issues them, worst first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ScopeUse {
    /// Some name the scope of the root: a state module's call would
    /// report in the root's scope.
    Root,
    /// Some name the scope of the caller (see
    /// `OutOfLine::scope_callers`).
    Caller,
    /// None names its scope.
    None,
}

/// The function units of the root module that the state modules of the
/// packages may call (see the module documentation).
pub(crate) struct Home {
    pub(in crate::lowering) module: ModuleId,
    name: String,
    pub(in crate::lowering) time_scale: celox_design::TimeScale,
    functions: HashMap<String, Exported>,
    source: Arc<sv::ir::Module>,
    subroutines: HashMap<String, usize>,
    /// The function sites of the root's units, which a kernel that calls
    /// one lists.
    sites: FunctionSites,
    /// The packages whose state modules the root's units report for (see
    /// `OutOfLine::scope_callers`).
    scope_callers: Vec<String>,
    /// Each variable the sites name: its shape, and the package variable it
    /// denotes, if any.
    site_variables: HashMap<SourceVarId, (SvVariable, Option<(String, String)>)>,
}

/// What a variable of the root's function sites is in a state module.
#[derive(Debug, Clone)]
pub(in crate::lowering) enum Mirror {
    /// The root's variable itself, through a binding to the root instance.
    Home(SourceVarId),
    /// A package variable, through a binding to its package.
    Package(String, String),
}

/// What a state module keeps of the root while lowering: the root, and the
/// answers it found about the root's functions.
#[derive(Debug, Clone)]
pub(in crate::lowering) struct HomeLink {
    pub(in crate::lowering) home: Arc<Home>,
    /// The package whose state module this is.
    package: String,
    /// The module's subroutines that reach, themselves included, a body
    /// that is not the same in the root, once asked.
    differ: Option<HashSet<String>>,
    /// The variable of the module for each variable of the root's sites.
    mirrors: HashMap<SourceVarId, SourceVarId>,
    /// The module's variables that denote package variables, by package
    /// and name.
    pub(in crate::lowering) aliases: HashMap<(String, String), SourceVarId>,
    /// The variables made for the root's sites, with what they denote.
    pub(in crate::lowering) bound: Vec<(SourceVarId, Mirror)>,
    /// Whether the module's function sites start with the root's.
    listed: bool,
}

impl std::fmt::Debug for Home {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Home")
            .field("module", &self.module)
            .field("functions", &self.functions.len())
            .finish_non_exhaustive()
    }
}

impl Home {
    /// The functions of `module`, the root named `name`, that `functions`
    /// lowered and attaching them kept at `kept` (see [`attach_functions`]),
    /// `lowered` and `sim_module` being the root's; `None` when the root
    /// offers none or forces a variable.
    pub(in crate::lowering) fn new(
        module: ModuleId,
        name: &str,
        lowered: &LoweredSvModule,
        sim_module: &SimModule,
        functions: &OutOfLine,
        kept: &[Option<u32>],
    ) -> Option<Self> {
        if lowered
            .variables
            .values()
            .any(|variable| variable.force_control.is_some())
        {
            return None;
        }
        let source = Arc::clone(&lowered.source);
        let subroutines: HashMap<String, usize> = source
            .subroutines()
            .iter()
            .enumerate()
            .map(|(index, subroutine)| (subroutine.name.clone(), index))
            .collect();
        let packages: HashMap<SourceVarId, (String, String)> = lowered
            .package_variables
            .iter()
            .map(|(id, package, name)| (*id, (package.clone(), name.clone())))
            .collect();
        let scope = scope_uses(functions, &sim_module.runtime_event_sites);
        let mut exported = HashMap::default();
        for (position, function) in functions.functions.iter().enumerate() {
            let (Some(index), Some(_)) = (kept.get(position).copied().flatten(), &function.unit)
            else {
                continue;
            };
            if function.import.is_some() {
                continue;
            }
            let name = subroutine_name(&function.key, |name| subroutines.contains_key(name));
            let Some(subroutine) = subroutines.get(name).map(|&at| &source.subroutines()[at])
            else {
                continue;
            };
            let slot = |name: &String| {
                lowered
                    .signal_names
                    .get(name)
                    .and_then(|&id| function.slot(id))
            };
            let formals: Vec<Option<usize>> = subroutine
                .params
                .iter()
                .map(|param| {
                    (!param.direction.is_reference())
                        .then(|| slot(&param.name))
                        .flatten()
                })
                .collect();
            let result = subroutine.return_var.as_ref().and_then(slot);
            let frame_only = subroutine
                .params
                .iter()
                .zip(&formals)
                .all(|(param, slot)| param.direction.is_reference() || slot.is_some())
                && (subroutine.return_var.is_none() || result.is_some());
            let Some((_, writes)) = functions.transitive(position) else {
                continue;
            };
            let mut writes: Vec<(String, String)> = writes
                .iter()
                .filter_map(|id| packages.get(id).cloned())
                .collect();
            writes.sort_unstable();
            writes.dedup();
            exported.insert(
                function.key.clone(),
                Exported {
                    index,
                    function: function.frame_only(),
                    formals,
                    result,
                    frame_only,
                    writes,
                    scope: scope[position],
                },
            );
        }
        if exported.is_empty() {
            return None;
        }
        let sites = lowered.unit_sites.clone();
        let mut site_variables = HashMap::default();
        // A binding asks for the shape the root's variable has in the
        // root's module.
        let mut note = |id: SourceVarId| {
            if let (Some(variable), Some(symbolic)) =
                (lowered.variables.get(&id), sim_module.variables.get(&id))
            {
                site_variables.entry(id).or_insert_with(|| {
                    let metadata = &symbolic.metadata;
                    let variable = SvVariable {
                        width: metadata.width,
                        is_4state: metadata.is_4state,
                        domain_kind: metadata.kind,
                        type_kind: metadata.type_kind,
                        array_dims: metadata.array_dims.clone(),
                        value_kind: metadata.value_kind,
                        ..variable.clone()
                    };
                    (variable, packages.get(&id).cloned())
                });
            }
            id
        };
        // Only the host request sites: a unit that waits or makes a
        // nonblocking assignment stays the importer's own.
        for site in &sites.host_request {
            site.map(&mut note);
        }
        Some(Self {
            module,
            name: name.to_string(),
            time_scale: sim_module.time_scale,
            functions: exported,
            source,
            subroutines,
            sites,
            scope_callers: functions.scope_callers.clone(),
            site_variables,
        })
    }

    fn subroutine(&self, name: &str) -> Option<&sv::ir::Subroutine> {
        self.subroutines
            .get(name)
            .map(|&index| &self.source.subroutines()[index])
    }

    /// Whether the root has a function that the state module of `package`
    /// may call for a subroutine of its own.
    fn offers(&self, package: &str, names: impl Iterator<Item = String>) -> bool {
        names.into_iter().any(|name| {
            self.functions.contains_key(&name)
                || self.functions.contains_key(&format!("{package}::{name}"))
        })
    }
}

impl HomeLink {
    /// The link of the state module of `package` to `home`.
    pub(in crate::lowering) fn new(home: Arc<Home>, package: &str) -> Self {
        Self {
            home,
            package: package.to_string(),
            differ: None,
            mirrors: HashMap::default(),
            aliases: HashMap::default(),
            bound: Vec::new(),
            listed: false,
        }
    }

    /// The root's name for the module's subroutine or key `name`: the
    /// package's own items are qualified in the root.
    fn home_name(&self, name: &str, known: impl Fn(&str) -> bool) -> Option<String> {
        if known(name) {
            return Some(name.to_string());
        }
        let qualified = format!("{}::{name}", self.package);
        known(&qualified).then_some(qualified)
    }

    /// Whether the root's counterpart of `subroutine`, the module's, does
    /// what it does.
    fn covers(&self, subroutine: &sv::ir::Subroutine) -> bool {
        let home = &self.home;
        let Some(counterpart) = self
            .home_name(&subroutine.name, |name| home.subroutines.contains_key(name))
            .and_then(|name| home.subroutine(&name))
        else {
            return false;
        };
        let prefix = format!("{}::", self.package);
        Normalized::of(subroutine, &prefix) == Normalized::of(counterpart, &prefix)
            || dispatch_covers(counterpart, subroutine, &prefix, &home.name, &self.package)
    }

    /// Whether every subroutine that a lowering of `root` reaches, `root`
    /// included, is the same in the root (see [`Self::covers`]), the module's
    /// subroutines being `subroutines`. A callee the module does not have is
    /// a DPI-C import, which a unit calls as the inlined body does, or
    /// nothing the lowering would find either.
    fn reaches_same(
        &mut self,
        root: &str,
        subroutines: &HashMap<String, sv::ir::Subroutine>,
    ) -> bool {
        if self.differ.is_none() {
            // The subroutines that differ, and everything that calls them.
            let mut callers: HashMap<&str, Vec<&str>> = HashMap::default();
            let mut pending = Vec::new();
            for (name, subroutine) in subroutines {
                for callee in callees(subroutine) {
                    if let Some((callee, _)) = subroutines.get_key_value(&callee) {
                        callers
                            .entry(callee.as_str())
                            .or_default()
                            .push(name.as_str());
                    }
                }
                if !self.covers(subroutine) {
                    pending.push(name.as_str());
                }
            }
            let mut differ: HashSet<String> = HashSet::default();
            while let Some(name) = pending.pop() {
                if differ.insert(name.to_string()) {
                    pending.extend(callers.get(name).into_iter().flatten());
                }
            }
            self.differ = Some(differ);
        }
        self.differ
            .as_ref()
            .is_some_and(|differ| !differ.contains(root))
    }
}

/// How the runtime events of each of `functions` and its callees name
/// their scope (see [`ScopeUse`]), the module's sites being `sites`.
///
/// The default of a class dispatch, inlined or not, reports a class without
/// an override in the scope of the module that dispatches, which its
/// message names too (see [`unknown_class_report`]): a state module's call
/// reaches it only for a class that the root knows no more than the module
/// does (see [`dispatch_covers`]), whose report differs between the two
/// modules either way.
fn scope_uses(functions: &OutOfLine, sites: &[RuntimeEventSite]) -> Vec<ScopeUse> {
    let own: Vec<ScopeUse> = functions
        .functions
        .iter()
        .map(|function| {
            let mut used = ScopeUse::None;
            let instructions = function
                .unit
                .iter()
                .flat_map(|unit| unit.blocks.values())
                .flat_map(|block| &block.instructions);
            for instruction in instructions {
                let SIRInstruction::RuntimeEvent { site_id, .. } = instruction else {
                    continue;
                };
                let site = &sites[*site_id as usize];
                if site.home.is_some() || !site_names_scope(site) || unknown_class_report(site) {
                    continue;
                }
                used = used.min(if functions.scoped_sites.contains(site_id) {
                    ScopeUse::Caller
                } else {
                    ScopeUse::Root
                });
            }
            used
        })
        .collect();
    // The worst of each function's callees, through any chain of calls:
    // each use spreads to the callers of the functions that make it.
    let mut callers: Vec<Vec<usize>> = vec![Vec::new(); own.len()];
    for (position, function) in functions.functions.iter().enumerate() {
        for &callee in &function.callees {
            callers[callee].push(position);
        }
    }
    let mut uses = own.clone();
    for worst in [ScopeUse::Root, ScopeUse::Caller] {
        let mut pending: Vec<usize> = (0..own.len()).filter(|&at| own[at] == worst).collect();
        while let Some(function) = pending.pop() {
            for &caller in &callers[function] {
                if uses[caller] > worst {
                    uses[caller] = worst;
                    pending.push(caller);
                }
            }
        }
    }
    uses
}

/// The subroutine a function key names: the key itself, or the key without
/// the places of its `ref` formals (see [`function_key`]).
fn subroutine_name(key: &str, known: impl Fn(&str) -> bool) -> &str {
    if known(key) {
        return key;
    }
    match key.strip_suffix(')').and_then(|rest| rest.rfind('(')) {
        Some(open) => &key[..open],
        None => key,
    }
}

/// The subroutines `subroutine` calls, its argument defaults included.
fn callees(subroutine: &sv::ir::Subroutine) -> Vec<String> {
    let mut calls = Vec::new();
    for param in &subroutine.params {
        if let Some(default) = &param.default {
            collect_calls(default, &mut calls);
        }
    }
    for stmt in &subroutine.body {
        stmt.walk(&mut |stmt| stmt_calls(stmt, &mut calls));
    }
    calls
        .into_iter()
        .filter_map(|(callee, _)| match callee {
            sv::ir::Callee::Subroutine(name) => Some(name),
            _ => None,
        })
        .collect()
}

/// The parts of a subroutine whose body is one dispatch on the class of an
/// object (a virtual method's dispatcher, a `$cast`): the selector, the
/// items and the default.
pub(super) fn class_dispatch(
    subroutine: &sv::ir::Subroutine,
) -> Option<(&sv::ir::Expr, &[sv::ir::CaseItem], Option<&[sv::ir::Stmt]>)> {
    let [
        sv::ir::Stmt::Case {
            kind: sv::ir::CaseKind::Exact,
            selector:
                selector @ sv::ir::Expr::Call {
                    callee: sv::ir::Callee::Object(op),
                    ..
                },
            items,
            default,
        },
    ] = subroutine.body.as_slice()
    else {
        return None;
    };
    matches!(**op, sv::ir::ObjectOp::ClassOf).then_some((selector, items, default.as_deref()))
}

/// Whether `home`, the root's dispatch on the class of an object, selects
/// for each class `module`'s dispatch selects a body the same body
/// (`prefix` normalizing the names, see [`Normalized`]), and both are the
/// same subroutine otherwise. The default of a dispatcher names the
/// module that dispatches, `home_name` and `module_name`.
fn dispatch_covers(
    home: &sv::ir::Subroutine,
    module: &sv::ir::Subroutine,
    prefix: &str,
    home_name: &str,
    module_name: &str,
) -> bool {
    let (Some((home_selector, home_items, home_default)), Some((selector, items, default))) =
        (class_dispatch(home), class_dispatch(module))
    else {
        return false;
    };
    let (home_locals, locals) = (hidden_locals(home, prefix), hidden_locals(module, prefix));
    let frame = |subroutine: &sv::ir::Subroutine, locals: &HiddenLocals<'_>| {
        let frame = (
            &subroutine.name,
            subroutine.is_task,
            subroutine.automatic,
            &subroutine.automatic_locals,
            &subroutine.persistent_locals,
            &subroutine.return_type,
            &subroutine.params,
            &subroutine.return_var,
            &subroutine.sensitivity_body,
            &subroutine.class_member,
        );
        Normalized::text(locals, prefix, &frame)
    };
    if frame(home, &home_locals) != frame(module, &locals)
        || Normalized::text(&home_locals, prefix, home_selector)
            != Normalized::text(&locals, prefix, selector)
    {
        return false;
    }
    // The labels of each body of the root's dispatch.
    let mut labels: HashMap<String, HashSet<String>> = HashMap::default();
    for item in home_items {
        labels
            .entry(Normalized::text(&home_locals, prefix, &item.body))
            .or_default()
            .extend(
                item.labels
                    .iter()
                    .map(|label| Normalized::text(&home_locals, prefix, label)),
            );
    }
    let items_covered = items.iter().all(|item| {
        labels
            .get(&Normalized::text(&locals, prefix, &item.body))
            .is_some_and(|known| {
                item.labels
                    .iter()
                    .all(|label| known.contains(&Normalized::text(&locals, prefix, label)))
            })
    });
    // The default's message names the module that dispatches.
    let default_text = |stmts: Option<&[sv::ir::Stmt]>, name: &str, locals: &HiddenLocals<'_>| {
        Normalized::text(locals, prefix, &stmts)
            .replace(&format!("`{name}` dispatches"), "` dispatches")
    };
    items_covered
        && default_text(home_default, home_name, &home_locals)
            == default_text(default, module_name, &locals)
}

/// The `Debug` text of analyzed IR of a subroutine, up to the names the
/// analyzer makes per module:
/// - a quoted name that starts with the package's qualification `prefix`
///   (`pkg::`) starts without it: the root names the package's own items
///   qualified, the package's state module unqualified;
/// - a hidden local of the subroutine (`update%<n>%...`, whose `<n>` counts
///   the module's hidden locals) is its position among the hidden locals:
///   two subroutines that differ only in those names are the same up to
///   renaming automatic variables.
struct Normalized<'a> {
    text: String,
    prefix: &'a str,
    locals: &'a HiddenLocals<'a>,
    /// The text of the quoted string being read, and whether its last
    /// character escapes the next.
    string: Option<(String, bool)>,
}

/// The position of each hidden automatic local of a subroutine among them.
type HiddenLocals<'a> = HashMap<&'a str, usize>;

/// The hidden automatic locals of `subroutine`, without the package's
/// qualification `prefix`.
fn hidden_locals<'a>(subroutine: &'a sv::ir::Subroutine, prefix: &str) -> HiddenLocals<'a> {
    subroutine
        .automatic_locals
        .iter()
        .filter(|name| name.contains('%'))
        .enumerate()
        .map(|(position, name)| (name.strip_prefix(prefix).unwrap_or(name), position))
        .collect()
}

impl<'a> Normalized<'a> {
    fn of(subroutine: &sv::ir::Subroutine, prefix: &str) -> String {
        Normalized::text(&hidden_locals(subroutine, prefix), prefix, subroutine)
    }

    /// The normalized `Debug` text of `value`, a part of the subroutine
    /// whose hidden locals are `locals`.
    fn text(locals: &HiddenLocals<'_>, prefix: &str, value: &dyn std::fmt::Debug) -> String {
        let mut normalized = Normalized {
            text: String::new(),
            prefix,
            locals,
            string: None,
        };
        let _ = write!(normalized, "{value:?}");
        normalized.text
    }

    fn end_string(&mut self, text: &str) {
        let name = text.strip_prefix(self.prefix).unwrap_or(text);
        // `%` is no character of a quoted name's, and the analyzer's
        // hidden names hold no `#`.
        match self.locals.get(name) {
            Some(&position) => {
                let _ = write!(self.text, "\"%#{position}\"");
            }
            None => {
                self.text.push('"');
                self.text.push_str(name);
                self.text.push('"');
            }
        }
    }
}

impl std::fmt::Write for Normalized<'_> {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let mut rest = text;
        while !rest.is_empty() {
            match &mut self.string {
                None => {
                    let end = rest.find('"').unwrap_or(rest.len());
                    self.text.push_str(&rest[..end]);
                    if end < rest.len() {
                        self.string = Some((String::new(), false));
                        rest = &rest[end + 1..];
                    } else {
                        rest = "";
                    }
                }
                Some((string, escaped)) => {
                    let mut end = None;
                    for (index, ch) in rest.char_indices() {
                        if *escaped {
                            *escaped = false;
                        } else if ch == '\\' {
                            *escaped = true;
                        } else if ch == '"' {
                            end = Some(index);
                            break;
                        }
                    }
                    match end {
                        Some(end) => {
                            string.push_str(&rest[..end]);
                            let (string, _) = self.string.take().unwrap_or_default();
                            self.end_string(&string);
                            rest = &rest[end + 1..];
                        }
                        None => {
                            string.push_str(rest);
                            rest = "";
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl ProcModule<'_> {
    /// The token of the module's scope that its processes pass to a unit
    /// of the root that reports in the caller's scope (see
    /// `OutOfLine::scope_callers`): zero for the root, and the package's
    /// for a state module that calls the root's units.
    pub(in crate::lowering) fn scope_token(&mut self) -> u64 {
        self.home_link()
            .and_then(|link| {
                link.home
                    .scope_callers
                    .iter()
                    .position(|package| *package == link.package)
            })
            .map_or(0, |position| position as u64 + 1)
    }

    /// The link to the root, when the module may call its functions.
    fn home_link(&mut self) -> Option<&mut HomeLink> {
        self.out_of_line()?.home.as_mut()
    }

    /// Start the module's function sites with the root's, when the module
    /// has a subroutine the root offers a function for, naming their
    /// variables by variables of the module (see [`Mirror`]): a kernel that
    /// calls one of the root's units lists them, and the module's own units
    /// number their sites after them.
    pub(in crate::lowering) fn list_home_sites(&mut self) {
        let subroutines = Arc::clone(&self.subroutines);
        let Some(link) = self.home_link() else {
            return;
        };
        // A module with nothing to call keeps its sites as they were.
        if !link.home.offers(&link.package, subroutines.keys().cloned()) {
            self.out_of_line().expect("linked above").home = None;
            return;
        }
        let home = Arc::clone(&link.home);
        let mirror = |pm: &mut Self, id: SourceVarId| -> SourceVarId {
            let link = pm.home_link().expect("linked above");
            if let Some(&own) = link.mirrors.get(&id) {
                return own;
            }
            let (variable, package) = home.site_variables[&id].clone();
            let own_package = link.package.clone();
            // The module's own variable of the package variable, if it has
            // one: the package's variable, or its binding to another package.
            let own = match &package {
                Some((package, name)) if *package == own_package => pm.id(name),
                Some(key) => link.aliases.get(key).copied(),
                None => None,
            };
            let own = own.unwrap_or_else(|| {
                let name = format!("$home@{}", id.0);
                let own = pm.hidden(
                    name.clone(),
                    variable.width,
                    variable.signed,
                    variable.is_4state,
                );
                pm.variables.insert(
                    own,
                    SvVariable {
                        path: vec![name],
                        hidden: true,
                        source: None,
                        force_control: None,
                        sampled_from: None,
                        history_of: None,
                        activation_scratch: false,
                        ..variable
                    },
                );
                let target = match package {
                    Some((package, name)) => Mirror::Package(package, name),
                    None => Mirror::Home(id),
                };
                pm.home_link()
                    .expect("linked above")
                    .bound
                    .push((own, target));
                own
            });
            pm.home_link()
                .expect("linked above")
                .mirrors
                .insert(id, own);
            own
        };
        let mut sites = FunctionSites::default();
        for site in &home.sites.host_request {
            let mut ids = Vec::new();
            site.map(|id| ids.push(id));
            let own: HashMap<SourceVarId, SourceVarId> =
                ids.into_iter().map(|id| (id, mirror(self, id))).collect();
            sites.host_request.push(site.map(|id| own[&id]));
        }
        // The module's own wait and NBA sites number from zero: a unit that
        // waits or makes a nonblocking assignment stays the module's own.
        sites.flush = self.function_sites.flush;
        self.function_sites = sites;
        self.home_link().expect("linked above").listed = true;
    }

    /// The function of the root that a call of `name`, `subroutine`, with
    /// its `ref` formals bound to `places`, enters instead of a unit of the
    /// module's, as a new function of the module (see the module
    /// documentation); `None` when the call lowers its own.
    pub(in crate::lowering) fn import_function(
        &mut self,
        name: &str,
        subroutine: &sv::ir::Subroutine,
        key: &str,
        places: &[Place],
        process: bool,
    ) -> Option<usize> {
        // An always_ff or always_comb checks its consistency against the
        // transitive reads and writes of its calls, which an import does
        // not carry; a place names a variable of the caller.
        if !process
            || !self.forceable.is_empty()
            || !self.procedurally_forced.is_empty()
            || places
                .iter()
                .any(|place| matches!(place, Place::Variable(_) | Place::Element { .. }))
        {
            return None;
        }
        // The module lists the root's host request sites only.
        let state = self.process_state(name);
        if state.waits || state.nbas {
            return None;
        }
        let subroutines = Arc::clone(&self.subroutines);
        let link = self.home_link()?;
        if !link.listed {
            return None;
        }
        let home = Arc::clone(&link.home);
        let home_key = link.home_name(key, |key| home.functions.contains_key(key))?;
        let exported = &home.functions[&home_key];
        // A unit that reports in the caller's scope reports in the module's
        // only when it knows the module.
        let token = home
            .scope_callers
            .iter()
            .position(|package| *package == link.package);
        let scope = match exported.scope {
            ScopeUse::None => true,
            ScopeUse::Caller => token.is_some(),
            ScopeUse::Root => false,
        };
        if !exported.frame_only
            || !scope
            || exported.formals.len() != subroutine.params.len()
            || !link.reaches_same(name, &subroutines)
        {
            return None;
        }
        let package = link.package.clone();
        if exported.writes.iter().any(|(owner, variable)| {
            *owner == package && self.id(variable).is_some_and(|id| self.is_excluded(id))
        }) {
            return None;
        }
        // The slots of the formals and the result are the module's
        // variables of them. A call names the other slots it passes, of the
        // places and the omitted arguments, by ids no variable has.
        let mut function = exported.function.clone();
        let mut own_slots: Vec<Option<SourceVarId>> = vec![None; function.slots.len()];
        for (param, slot) in subroutine.params.iter().zip(&exported.formals) {
            if let Some(slot) = *slot {
                own_slots[slot] = Some(self.id(&param.name)?);
            }
        }
        if let Some(result) = &subroutine.return_var {
            own_slots[exported.result?] = Some(self.id(result)?);
        }
        for (position, (slot, own)) in function.slots.iter_mut().zip(own_slots).enumerate() {
            slot.id = own.unwrap_or(SourceVarId(u32::MAX - position as u32));
        }
        function.key = key.to_string();
        function.import = Some(exported.index);
        let functions = self.out_of_line()?;
        let index = functions.functions.len();
        functions.functions.push(function);
        functions.by_name.insert(key.to_string(), index);
        Some(index)
    }
}
