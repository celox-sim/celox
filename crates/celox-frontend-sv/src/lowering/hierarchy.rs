//! Hierarchical names (IEEE 1800-2023 23.6, 23.8) resolved against the
//! elaborated hierarchy.
//!
//! The analyzer works on one module specialization at a time and cannot see
//! the instances below it. A module whose analysis meets a hierarchical name
//! is first elaborated from its declarations only, so that the hierarchy
//! below it can be specialized. Once every specialization is known, each of
//! its dotted names that starts at an instance of the module, at the module's
//! own name or at `$root` is followed through the specialized instances to a
//! signal. The module is then analyzed again with a local signal for each
//! such name, and assembly binds that local signal, per instance, to the
//! object of the instance the name denotes.

use celox_design::ModuleId;
use celox_frontend_core::SourceVarId;
use celox_frontend_core::symbolic::artifact::{HierarchicalBinding, HierarchicalStart};
use celox_sv_analyzer as sv;
use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};

use super::{
    AnalyzedSvModule, LoweredSvInstance, LoweredSvModule, LoweredSvModuleKey, lower_instances,
    module_constants_with_overrides, module_parameter_types, propagate_force_signals,
    unpacked_range_bounds,
};

/// A specialization as the hierarchy below it was elaborated: its
/// instances and the signals names can denote.
pub(super) struct ElaboratedModule {
    name: String,
    /// The interface instances and ports a virtual interface may denote.
    pub(super) interface_handles: Vec<sv::ir::InterfaceHandle>,
    /// Ports and signals, by local name, with their net kind.
    signals: Vec<(String, sv::ir::Type, bool)>,
    interface_scopes: HashSet<String>,
    instances: Vec<LoweredSvInstance>,
    constants: HashMap<String, i128>,
    parameter_types: HashMap<String, (usize, bool)>,
}

impl ElaboratedModule {
    /// Project declarations and instance bindings without lowering executable bodies.
    pub(super) fn from_declarations(
        source: &sv::ir::Module,
        force_signals: &[String],
    ) -> Result<Self, sv::AnalyzerError> {
        let parameter_types = module_parameter_types(source);
        let constants = module_constants_with_overrides(source, &[]);
        let mut instances = lower_instances(source, &constants, &parameter_types)?;
        propagate_force_signals(&mut instances, force_signals);
        Ok(Self {
            name: source.name().to_string(),
            interface_handles: source.interface_handles().to_vec(),
            signals: signals(source),
            interface_scopes: source.interface_scope_names().iter().cloned().collect(),
            instances,
            constants,
            parameter_types,
        })
    }

    pub(super) fn instances(&self) -> &[LoweredSvInstance] {
        &self.instances
    }
}

/// The ports and signals of a module that a hierarchical name can denote:
/// not those that denote another object themselves.
fn signals(module: &sv::ir::Module) -> Vec<(String, sv::ir::Type, bool)> {
    module
        .ports()
        .iter()
        .map(|port| {
            (
                port.name().to_string(),
                port.r#type().clone(),
                port.is_net(),
            )
        })
        .chain(
            module
                .signals()
                .iter()
                .filter(|signal| signal.hierarchical().is_none())
                .map(|signal| {
                    (
                        signal.name().to_string(),
                        signal.r#type().clone(),
                        signal.is_net(),
                    )
                }),
        )
        .collect()
}

/// The signal of another instance that a hierarchical name of a module
/// denotes.
#[derive(Clone)]
pub(super) struct HierarchicalReference {
    pub signal: sv::ast::HierarchicalSignal,
    start: HierarchicalStart,
    path: Vec<(String, usize)>,
    /// The specializations that hold the signal, from the different
    /// instances of the module, and its local name in each.
    pub targets: Vec<(ModuleId, String)>,
}

/// The specialized hierarchy, for following names through it.
pub(super) struct Hierarchy<'a> {
    root: (&'a LoweredSvModuleKey, ModuleId),
    keys: &'a HashMap<ModuleId, &'a LoweredSvModuleKey>,
    module_ids: &'a HashMap<LoweredSvModuleKey, ModuleId>,
    elaborated: &'a HashMap<ModuleId, ElaboratedModule>,
    analyzed: &'a HashMap<String, AnalyzedSvModule>,
    /// The specializations that instantiate each specialization.
    parents: HashMap<ModuleId, Vec<ModuleId>>,
}

/// A name followed to a signal, relative to where the following started.
#[derive(Clone, PartialEq, Eq)]
struct Found {
    /// Whether the following started at the top instance: `path` is then the
    /// path of the signal's instance in the design and `levels` is zero.
    rooted: bool,
    /// How many instances above the name's module the following started.
    levels: usize,
    path: Vec<(String, usize)>,
    /// How many leading components name the signal.
    length: usize,
    target: String,
    r#type: sv::ast::Type,
    is_net: bool,
    /// The specializations that hold the signal.
    modules: Vec<ModuleId>,
}

impl HierarchicalReference {
    /// A reference from the top instance along `path` to `targets`.
    pub(super) fn rooted(
        signal: sv::ast::HierarchicalSignal,
        path: Vec<(String, usize)>,
        targets: Vec<(ModuleId, String)>,
    ) -> Self {
        Self {
            signal,
            start: HierarchicalStart::Root,
            path,
            targets,
        }
    }
}

impl Found {
    /// Whether two followings reach the same relative object, if from
    /// different specializations.
    fn same_object(&self, other: &Found) -> bool {
        (
            self.rooted,
            self.levels,
            &self.path,
            self.length,
            &self.target,
            &self.r#type,
            self.is_net,
        ) == (
            other.rooted,
            other.levels,
            &other.path,
            other.length,
            &other.target,
            &other.r#type,
            other.is_net,
        )
    }
}

impl<'a> Hierarchy<'a> {
    pub(super) fn new(
        root: (&'a LoweredSvModuleKey, ModuleId),
        keys: &'a HashMap<ModuleId, &'a LoweredSvModuleKey>,
        module_ids: &'a HashMap<LoweredSvModuleKey, ModuleId>,
        elaborated: &'a HashMap<ModuleId, ElaboratedModule>,
        analyzed: &'a HashMap<String, AnalyzedSvModule>,
    ) -> Self {
        let mut parents: HashMap<ModuleId, Vec<ModuleId>> = HashMap::default();
        for (&parent, module) in elaborated {
            for instance in module.instances() {
                if let Some(&child) = module_ids.get(&LoweredSvModuleKey::instance_key(instance)) {
                    let parents = parents.entry(child).or_default();
                    if !parents.contains(&parent) {
                        parents.push(parent);
                    }
                }
            }
        }
        Self {
            root,
            keys,
            module_ids,
            elaborated,
            analyzed,
            parents,
        }
    }

    /// The name `module` has in the source, which hierarchical names spell.
    fn authored_name(&self, module: ModuleId) -> &str {
        let name = &self.keys[&module].name;
        self.analyzed.get(name).map_or(name, |analyzed| {
            analyzed.packages.authored_module_name(name)
        })
    }

    /// Follow `components[at..]` down from an instance of `module`: the
    /// longest instance name at each level, which may carry generate scope
    /// prefixes (`g[1].u`) or an array index (`u[3]`), then the longest
    /// signal name of the last instance.
    fn walk(
        &self,
        mut module: ModuleId,
        components: &[String],
        mut at: usize,
    ) -> Result<Option<Found>, sv::AnalyzerError> {
        let mut path = Vec::new();
        'instances: while at < components.len() {
            let elaborated = &self.elaborated[&module];
            for end in (at + 1..components.len()).rev() {
                let name = components[at..end].join(".");
                if let Some((segment, child)) = find_instance(elaborated, &name) {
                    module = *self
                        .module_ids
                        .get(&LoweredSvModuleKey::instance_key(child))
                        .ok_or_else(|| {
                            sv::AnalyzerError::Unsupported(format!(
                                "instance `{name}` of hierarchical reference `{}`",
                                components.join(".")
                            ))
                        })?;
                    path.push(segment);
                    at = end;
                    continue 'instances;
                }
            }
            break;
        }
        let elaborated = &self.elaborated[&module];
        let analyzed = &self.analyzed[&elaborated.name];
        let Some((length, target, r#type, is_net)) =
            (at + 1..=components.len()).rev().find_map(|end| {
                let name = components[at..end].join(".");
                let (target, r#type, is_net) = find_signal(elaborated, analyzed, &name)?;
                Some((end, target, r#type, is_net))
            })
        else {
            return Ok(None);
        };
        let r#type = resolved_type(elaborated, r#type).ok_or_else(|| {
            sv::AnalyzerError::Unsupported(format!(
                "hierarchical reference `{}` to a signal of this type",
                components[..length].join(".")
            ))
        })?;
        Ok(Some(Found {
            rooted: false,
            levels: 0,
            path,
            length,
            target,
            r#type,
            is_net,
            modules: vec![module],
        }))
    }

    /// Follow a name written in `module` down from it: from an instance of
    /// it, a local interface, or from the module's own name (23.8).
    fn downward(
        &self,
        module: ModuleId,
        components: &[String],
    ) -> Result<Option<Found>, sv::AnalyzerError> {
        let elaborated = &self.elaborated[&module];
        if starts_at_instance(elaborated, components) {
            return Ok(self
                .walk(module, components, 0)?
                .filter(|found| !found.path.is_empty()));
        }
        // Interface instances are flattened into the module's signals, rather
        // than retained as child modules. Their scopes still participate in
        // upwards lookup (IEEE 1800-2023 23.8).
        if (1..components.len()).any(|end| {
            elaborated
                .interface_scopes
                .contains(&components[..end].join("."))
        }) {
            return self.walk(module, components, 0);
        }
        if components[0] == self.authored_name(module) {
            return self.walk(module, components, 1);
        }
        Ok(None)
    }

    /// Follow a name upwards from every instance of `module` (IEEE 1800-2023
    /// 23.8): at each enclosing instance, from an instance of its scope or
    /// from its module name. The name must denote the same relative object
    /// from each instance, or the same object of the design (see
    /// [`Self::rooted`]); `None` if it denotes none from some instance.
    fn upward(
        &self,
        module: ModuleId,
        components: &[String],
        memo: &mut HashMap<ModuleId, Option<Found>>,
    ) -> Result<Option<Found>, sv::AnalyzerError> {
        let mut found: Option<Found> = None;
        let parents = self.parents.get(&module).map_or(&[][..], Vec::as_slice);
        if parents.is_empty() {
            return Ok(None);
        }
        for &parent in parents {
            let from_parent = match memo.get(&parent) {
                Some(cached) => cached.clone(),
                None => {
                    let resolved = match self.downward(parent, components)? {
                        Some(found) => Some(found),
                        None => self.upward(parent, components, memo)?,
                    };
                    memo.insert(parent, resolved.clone());
                    resolved
                }
            };
            let Some(mut from_parent) = from_parent else {
                return Ok(None);
            };
            if !from_parent.rooted {
                from_parent.levels += 1;
            }
            match &mut found {
                None => found = Some(from_parent),
                Some(found) if found.same_object(&from_parent) => {
                    for module in from_parent.modules {
                        if !found.modules.contains(&module) {
                            found.modules.push(module);
                        }
                    }
                }
                // The instances are at different depths below the scope the
                // name starts at, or below different scopes of that name.
                Some(_) => {
                    return match self.rooted(module, components, &mut HashMap::default())? {
                        Some(rooted) => Ok(Some(rooted)),
                        None => Err(sv::AnalyzerError::Unsupported(format!(
                            "upward hierarchical reference `{}` that denotes different \
                             objects in instances of module `{}`",
                            components.join("."),
                            self.authored_name(module)
                        ))),
                    };
                }
            }
        }
        Ok(found)
    }

    /// Follow a name upwards from every instance of `module` to one object
    /// of the design: the scope the name starts at is found above each
    /// instance, as [`Self::upward`] finds it, and is a single instance.
    /// `None` if the instances denote different objects, or some none.
    fn rooted(
        &self,
        module: ModuleId,
        components: &[String],
        memo: &mut HashMap<ModuleId, Option<Found>>,
    ) -> Result<Option<Found>, sv::AnalyzerError> {
        let mut found: Option<Found> = None;
        for &parent in self.parents.get(&module).map_or(&[][..], Vec::as_slice) {
            let from_parent = match memo.get(&parent) {
                Some(cached) => cached.clone(),
                None => {
                    let resolved = match self.downward(parent, components)? {
                        Some(mut found) => self.instance_path(parent).map(|mut path| {
                            path.append(&mut found.path);
                            Found {
                                rooted: true,
                                levels: 0,
                                path,
                                ..found
                            }
                        }),
                        None => self.rooted(parent, components, memo)?,
                    };
                    memo.insert(parent, resolved.clone());
                    resolved
                }
            };
            let Some(from_parent) = from_parent else {
                return Ok(None);
            };
            match &found {
                None => found = Some(from_parent),
                Some(found) if found.same_object(&from_parent) => {}
                Some(_) => return Ok(None),
            }
        }
        Ok(found)
    }

    /// The path of the only instance of `module` in the design, from the top
    /// instance; `None` if it has several instances.
    fn instance_path(&self, module: ModuleId) -> Option<Vec<(String, usize)>> {
        if module == self.root.1 {
            return Some(Vec::new());
        }
        let [parent] = self.parents.get(&module)?.as_slice() else {
            return None;
        };
        let mut positions: HashMap<&str, usize> = HashMap::default();
        let mut segments = Vec::new();
        for instance in self.elaborated[parent].instances() {
            // As `find_instance` numbers the elements of an instance array.
            let position = positions.entry(&instance.instance_name).or_default();
            let index = instance.array_index_base + *position;
            *position += 1;
            if self
                .module_ids
                .get(&LoweredSvModuleKey::instance_key(instance))
                == Some(&module)
            {
                segments.push((instance.instance_name.clone(), index));
            }
        }
        let [segment] = segments.try_into().ok()?;
        let mut path = self.instance_path(*parent)?;
        path.push(segment);
        Some(path)
    }
}

/// Resolve the hierarchical names of specialization `module` that its
/// source spells with literal instance indices.
pub(super) fn resolve(hierarchy: &Hierarchy<'_>, module: ModuleId) -> Vec<HierarchicalReference> {
    let analyzed = &hierarchy.analyzed[&hierarchy.keys[&module].name];
    let mut references = Vec::new();
    for components in analyzed
        .source
        .hierarchical_names(&analyzed.name, &analyzed.packages)
    {
        // The scan also finds names in inactive generate branches. A name
        // the analysis uses reaches `resolve_name` again with its error.
        let _ = resolve_name(hierarchy, module, &components, &mut references);
    }
    references
}

/// Add the reference of the hierarchical name `components` of `module` to
/// `references`. Whether the name denotes a signal not referenced yet.
pub(super) fn resolve_name(
    hierarchy: &Hierarchy<'_>,
    module: ModuleId,
    components: &[String],
    references: &mut Vec<HierarchicalReference>,
) -> Result<bool, sv::AnalyzerError> {
    let elaborated = &hierarchy.elaborated[&module];
    let resolved = if components[0] == "$root" {
        // `$root.top. ...` (IEEE 1800-2023 23.3.1).
        if components.get(1) != Some(&hierarchy.root.0.name) {
            return Ok(false);
        }
        hierarchy
            .walk(hierarchy.root.1, components, 2)?
            .map(|found| (HierarchicalStart::Root, found))
    } else if starts_at_instance(elaborated, components)
        || components[0] == hierarchy.authored_name(module)
    {
        hierarchy
            .downward(module, components)?
            .map(|found| (HierarchicalStart::Ancestor(0), found))
    } else if declares(elaborated, &components[0]) {
        // A variable, generate block or interface of the module.
        None
    } else {
        hierarchy
            .upward(module, components, &mut HashMap::default())?
            .map(|found| {
                let start = if found.rooted {
                    HierarchicalStart::Root
                } else {
                    HierarchicalStart::Ancestor(found.levels)
                };
                (start, found)
            })
    };
    let Some((start, found)) = resolved else {
        return Ok(false);
    };
    let written = components[..found.length].join(".");
    if references
        .iter()
        .any(|reference| reference.signal.paths.contains(&written))
    {
        return Ok(false);
    }
    // A name that starts at the module's own name and names no instance
    // denotes the module's own signal (IEEE 1800-2023 23.8).
    if found.levels == 0 && found.path.is_empty() && start == HierarchicalStart::Ancestor(0) {
        references.push(HierarchicalReference {
            signal: sv::ast::HierarchicalSignal {
                paths: vec![written],
                name: found.target,
                r#type: found.r#type,
                is_net: found.is_net,
                local: true,
            },
            start,
            path: Vec::new(),
            targets: Vec::new(),
        });
        return Ok(true);
    }
    references.push(HierarchicalReference {
        signal: sv::ast::HierarchicalSignal {
            paths: vec![written.clone()],
            name: written,
            r#type: found.r#type,
            is_net: found.is_net,
            local: false,
        },
        start,
        path: found.path,
        targets: found
            .modules
            .into_iter()
            .map(|module| (module, found.target.clone()))
            .collect(),
    });
    Ok(true)
}

/// The bindings of the local signals of `references` in `lowered`.
pub(super) fn bindings(
    references: &[HierarchicalReference],
    lowered: &LoweredSvModule,
    lowered_modules: &HashMap<ModuleId, LoweredSvModule>,
) -> Result<HashMap<SourceVarId, HierarchicalBinding>, sv::AnalyzerError> {
    references
        .iter()
        .filter(|reference| !reference.signal.local)
        .map(|reference| {
            let var_id = lowered.signal_names[&reference.signal.name];
            let targets = reference
                .targets
                .iter()
                .map(|(module, target)| {
                    let var_id = lowered_modules[module]
                        .signal_names
                        .get(target)
                        .copied()
                        .ok_or_else(|| {
                            sv::AnalyzerError::Unsupported(format!(
                                "hierarchical reference `{}`",
                                reference.signal.name
                            ))
                        })?;
                    Ok((*module, var_id))
                })
                .collect::<Result<_, sv::AnalyzerError>>()?;
            Ok((
                var_id,
                HierarchicalBinding {
                    start: reference.start,
                    path: reference.path.clone(),
                    targets,
                },
            ))
        })
        .collect()
}

/// How processes of other modules write a signal through hierarchical names.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ForeignWrites {
    /// The number of module specializations whose processes that run
    /// repeatedly (all but `initial` ones) write the signal.
    pub repeating: usize,
    /// Whether an `always_ff` process writes it.
    pub always_ff: bool,
    /// Whether a process that is the only writer of what it writes does: an
    /// `always_ff`, or a combinational process. A plain `always` that writes
    /// through a hierarchical name runs as a process (see
    /// `runs_as_process`), whose writes store into the variable as they
    /// execute.
    pub exclusive: bool,
}

/// Add the signals that the hierarchical names of `module` (`module_id`),
/// which denote `references`, write to `writes`, by specialization and local
/// name.
pub(super) fn collect_foreign_writes(
    module: &sv::ir::Module,
    references: &[HierarchicalReference],
    writes: &mut HashMap<ModuleId, HashMap<String, ForeignWrites>>,
) {
    let mut once = HashSet::default();
    let mut repeatedly = HashSet::default();
    let mut always_ff = HashSet::default();
    let mut exclusive = HashSet::default();
    for process in module.initial_processes() {
        let ordinary = process
            .site()
            .is_none_or(|site| site.construct != "always_ff");
        let written = if !process.repeats() {
            &mut once
        } else if ordinary {
            &mut repeatedly
        } else {
            &mut exclusive
        };
        super::procedural::written_names_with_calls(module, process.body(), written);
    }
    for process in module.comb_processes() {
        super::procedural::written_names_with_calls(module, process.body(), &mut exclusive);
    }
    for process in module.ff_processes() {
        let written = if process.is_always_ff() {
            &mut always_ff
        } else {
            &mut repeatedly
        };
        super::procedural::written_names_with_calls(module, process.body(), written);
    }
    for reference in references {
        let name = &reference.signal.name;
        let by_always_ff = always_ff.contains(name);
        let by_exclusive = by_always_ff || exclusive.contains(name);
        let repeating = by_exclusive || repeatedly.contains(name);
        if !repeating && !once.contains(name) {
            continue;
        }
        for (target_module, target) in &reference.targets {
            let entry = writes
                .entry(*target_module)
                .or_default()
                .entry(target.clone())
                .or_default();
            entry.repeating += usize::from(repeating);
            entry.always_ff |= by_always_ff;
            entry.exclusive |= by_exclusive;
        }
    }
}

/// [`collect_foreign_writes`] for `references` to the members of the
/// interface instances that virtual interfaces of `module` may denote: each
/// instance of a specialization may denote the same instances, so the
/// specialization counts once for each signal it writes.
pub(super) fn collect_virtual_writes(
    module: &sv::ir::Module,
    references: &[HierarchicalReference],
    writes: &mut HashMap<ModuleId, HashMap<String, ForeignWrites>>,
) {
    let mut own = HashMap::default();
    collect_foreign_writes(module, references, &mut own);
    for (target_module, targets) in own {
        for (target, foreign) in targets {
            let entry = writes
                .entry(target_module)
                .or_default()
                .entry(target)
                .or_default();
            entry.repeating += foreign.repeating.min(1);
            entry.always_ff |= foreign.always_ff;
            entry.exclusive |= foreign.exclusive;
        }
    }
}

/// Whether leading components of a name name an instance of the module.
fn starts_at_instance(module: &ElaboratedModule, components: &[String]) -> bool {
    (1..components.len()).any(|end| find_instance(module, &components[..end].join(".")).is_some())
}

/// Whether `name` is declared in the module, other than as an instance, so
/// that a dotted name starting with it is not an upward reference.
fn declares(module: &ElaboratedModule, name: &str) -> bool {
    let scoped = |candidate: &str| {
        candidate == name
            || candidate
                .strip_prefix(name)
                .is_some_and(|rest| rest.starts_with(['.', '[', '$']))
    };
    module.signals.iter().any(|(signal, _, _)| scoped(signal))
        || module.interface_scopes.iter().any(|scope| {
            scoped(scope)
                || name
                    .strip_prefix(scope.as_str())
                    .is_some_and(|rest| rest.starts_with('['))
        })
        || module
            .instances
            .iter()
            .any(|instance| instance.instance_name.starts_with(&format!("{name}.")))
}

/// The instance path segment and instance of the element named `name`.
fn find_instance<'a>(
    module: &'a ElaboratedModule,
    name: &str,
) -> Option<((String, usize), &'a LoweredSvInstance)> {
    let mut positions: HashMap<&str, usize> = HashMap::default();
    for instance in &module.instances {
        // Glue blocks, and so hierarchy indices, follow listing order.
        let position = positions.entry(&instance.instance_name).or_default();
        let index = instance.array_index_base + *position;
        *position += 1;
        let matches = if instance.array_element.is_some() {
            name.strip_prefix(instance.instance_name.as_str())
                .is_some_and(|rest| rest == format!("[{index}]"))
        } else {
            name == instance.instance_name
        };
        if matches {
            return Some(((instance.instance_name.clone(), index), instance));
        }
    }
    None
}

/// The local name, type and net kind of signal `name` of a module.
fn find_signal<'a>(
    module: &'a ElaboratedModule,
    analyzed: &AnalyzedSvModule,
    name: &str,
) -> Option<(String, &'a sv::ir::Type, bool)> {
    let flat = analyzed
        .interface_member_paths
        .iter()
        .find(|(_, path)| path.segments.join(".") == name)
        .map_or(name, |(flat, _)| flat.as_str());
    let signal = |flat: &str| {
        module
            .signals
            .iter()
            .find(|(candidate, _, _)| candidate == flat)
            .map(|(candidate, r#type, is_net)| (candidate.clone(), r#type, *is_net))
    };
    signal(flat).or_else(|| {
        // A member or a port of an interface instance of the module is the
        // signal `instance$member` of the module.
        module.interface_scopes.iter().find_map(|scope| {
            let member = name.strip_prefix(scope.as_str())?.strip_prefix('.')?;
            signal(&format!("{scope}${member}"))
        })
    })
}

/// `r#type` with the bounds it has in its specialization.
fn resolved_type(module: &ElaboratedModule, r#type: &sv::ir::Type) -> Option<sv::ast::Type> {
    // An associative or dynamic array or a queue is denoted by its handle;
    // the name has the container type of the declaration.
    if let Some(container) = r#type.container() {
        return container.with_resolved_container_bounds(&|expr| {
            sv::typecheck::eval_const_expr_with_types(
                expr,
                &module.constants,
                &module.parameter_types,
            )
        });
    }
    let kind = match r#type.kind() {
        sv::ir::TypeKind::Bit => sv::ast::TypeKind::Bit,
        sv::ir::TypeKind::Logic => sv::ast::TypeKind::Logic,
        sv::ir::TypeKind::Reg => sv::ast::TypeKind::Reg,
        sv::ir::TypeKind::Implicit => sv::ast::TypeKind::Implicit,
        sv::ir::TypeKind::String => sv::ast::TypeKind::String,
        _ => return None,
    };
    let eval = |expr| {
        sv::typecheck::eval_const_expr_with_types(expr, &module.constants, &module.parameter_types)
    };
    let packed = r#type
        .packed_ranges()
        .iter()
        .map(|range| Some((eval(range.left())?, eval(range.right())?)))
        .collect::<Option<Vec<_>>>()?;
    let unpacked = r#type
        .unpacked_ranges()
        .iter()
        .map(|range| unpacked_range_bounds(range, &module.constants, &module.parameter_types))
        .collect::<Option<Vec<_>>>()?;
    let resolved = sv::ast::Type::resolved(
        kind,
        r#type.is_signed(),
        r#type.signed_element_depth(),
        &packed,
        &unpacked,
    )
    .with_chandle(r#type.is_chandle());
    // The members of a packed structure, which the name selects.
    Some(match r#type.packed_structure() {
        Some(structure) => resolved.with_members_of(structure),
        None => resolved,
    })
}
