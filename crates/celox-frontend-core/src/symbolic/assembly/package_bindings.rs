//! Where the variables of each instance live, and the drivers of package variables.

use super::*;

/// Where the variables of each instance live. A module variable bound to a
/// package variable (IEEE 1800-2023 26.2) is that package's one object, in
/// the package's instance; every other variable belongs to its instance.
#[derive(Default)]
pub(crate) struct PackageBindings {
    /// Declared aliases have entries in the state lookup. Graph operands do not.
    pub(super) object_aliases: Vec<(AbsoluteAddr, AbsoluteAddr)>,
    targets: HashMap<AbsoluteAddr, AbsoluteAddr>,
    /// Per-instance aliases of staged nonblocking shadow pairs (`value`, `mask`).
    /// Kept separate from `targets`/`is_bound` so per-module `module_variables`
    /// remains in 1-to-1 bijection with `state_objects`.
    staged_nba_aliases: HashMap<AbsoluteAddr, AbsoluteAddr>,
    /// Maps a process-private `alias_work` shadow address back to its
    /// canonical target address for sensitivity probes and occurrence counters.
    alias_working_targets: HashMap<AbsoluteAddr, AbsoluteAddr>,
}

impl PackageBindings {
    pub(super) fn new(
        expanded: &HashMap<InstancePath, InstanceId>,
        instance_modules: &HashMap<InstanceId, ModuleId>,
        modules: &HashMap<ModuleId, SimModule>,
        package_instances: &HashMap<ModuleId, InstanceId>,
    ) -> Result<Self, ParserError> {
        let mut targets = HashMap::default();
        for (&instance_id, module_id) in instance_modules {
            let module = &modules[module_id];
            for (&var_id, binding) in &module.package_bindings {
                let package_instance =
                    package_instances.get(&binding.package).ok_or_else(|| {
                        ParserError::illegal_context(
                            "package variable binding",
                            format!(
                                "module `{}` binds a variable to a module that is not a package",
                                module.name
                            ),
                            None,
                        )
                    })?;
                let package = &modules[&binding.package];
                let (Some(variable), Some(target)) = (
                    module.variables.get(&var_id),
                    package.variables.get(&binding.var_id),
                ) else {
                    return Err(ParserError::illegal_context(
                        "package variable binding",
                        format!(
                            "module `{}` binds an unknown variable of package `{}`",
                            module.name, package.name
                        ),
                        None,
                    ));
                };
                if variable.metadata != target.metadata {
                    return Err(ParserError::illegal_context(
                        "package variable binding",
                        format!(
                            "module `{}` binds a variable to `{}` of package `{}` with another \
                             type",
                            module.name,
                            target.path.join("."),
                            package.name
                        ),
                        None,
                    ));
                }
                targets.insert(
                    AbsoluteAddr {
                        instance_id,
                        var_id,
                    },
                    AbsoluteAddr {
                        instance_id: *package_instance,
                        var_id: binding.var_id,
                    },
                );
            }
        }
        let mut paths: Option<HashMap<InstanceId, &InstancePath>> = None;
        for (&instance_id, module_id) in instance_modules {
            let module = &modules[module_id];
            for (&var_id, binding) in &module.hierarchical_bindings {
                let paths = paths
                    .get_or_insert_with(|| expanded.iter().map(|(path, &id)| (id, path)).collect());
                let mut path = match binding.start {
                    HierarchicalStart::Root => Vec::new(),
                    HierarchicalStart::Ancestor(levels) => {
                        let own = &paths[&instance_id].0;
                        own[..own.len().saturating_sub(levels)].to_vec()
                    }
                };
                path.extend(binding.path.iter().cloned());
                let error = |reason: &str| {
                    ParserError::illegal_context(
                        "hierarchical reference",
                        format!(
                            "module `{}` binds `{}` {reason}",
                            module.name,
                            module
                                .variables
                                .get(&var_id)
                                .map_or_else(String::new, |variable| variable.path.join(".")),
                        ),
                        None,
                    )
                };
                let target_instance = *expanded
                    .get(&InstancePath(path))
                    .ok_or_else(|| error("to an instance that does not exist"))?;
                let target_module_id = instance_modules[&target_instance];
                let target_module = &modules[&target_module_id];
                let target_var = binding
                    .targets
                    .iter()
                    .find(|(module, _)| *module == target_module_id)
                    .map(|&(_, var)| var)
                    .ok_or_else(|| error("to an instance of another module"))?;
                let (Some(variable), Some(target)) = (
                    module.variables.get(&var_id),
                    target_module.variables.get(&target_var),
                ) else {
                    return Err(error("to an unknown variable"));
                };
                let (variable, target) = (&variable.metadata, &target.metadata);
                if (
                    variable.width,
                    variable.is_4state,
                    &variable.array_dims,
                    variable.value_kind,
                ) != (
                    target.width,
                    target.is_4state,
                    &target.array_dims,
                    target.value_kind,
                ) {
                    return Err(error("to a variable of another type"));
                }
                targets.insert(
                    AbsoluteAddr {
                        instance_id,
                        var_id,
                    },
                    AbsoluteAddr {
                        instance_id: target_instance,
                        var_id: target_var,
                    },
                );
            }
        }
        // A child instance's `ref` port denotes the variable of its parent
        // module (IEEE 1800-2023 23.3.3.2), which may itself be bound.
        for (path, &parent_instance_id) in expanded {
            let parent_module = &modules[&instance_modules[&parent_instance_id]];
            let mut children = HashMap::default();
            for binding in &parent_module.ref_port_bindings {
                let child = match children.entry((&binding.instance, binding.index)) {
                    std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        let mut child_path = path.0.clone();
                        child_path.push((binding.instance.clone(), binding.index));
                        let child = expanded.get(&InstancePath(child_path)).ok_or_else(|| {
                            ParserError::illegal_context(
                                "ref port",
                                format!(
                                    "unknown child {}[{}] of {}",
                                    binding.instance, binding.index, parent_module.name
                                ),
                                None,
                            )
                        })?;
                        *entry.insert(*child)
                    }
                };
                let child_module = &modules[&instance_modules[&child]];
                let (Some(port), Some(target)) = (
                    child_module.variables.get(&binding.child_port),
                    parent_module.variables.get(&binding.parent_var),
                ) else {
                    return Err(ParserError::illegal_context(
                        "ref port",
                        format!(
                            "unknown variable in ref port binding between `{}` and `{}`",
                            parent_module.name, child_module.name
                        ),
                        None,
                    ));
                };
                if port.kind != crate::VariableKind::Ref {
                    return Err(ParserError::illegal_context(
                        "ref port",
                        format!(
                            "`{}` of `{}` is not a ref port",
                            port.path.join("."),
                            child_module.name
                        ),
                        None,
                    ));
                }
                if (
                    port.metadata.width,
                    port.metadata.is_4state,
                    &port.metadata.array_dims,
                    port.metadata.value_kind,
                ) != (
                    target.metadata.width,
                    target.metadata.is_4state,
                    &target.metadata.array_dims,
                    target.metadata.value_kind,
                ) {
                    return Err(ParserError::illegal_context(
                        "ref port",
                        format!(
                            "ref port `{}` of `{}` binds a variable of another type",
                            port.path.join("."),
                            child_module.name
                        ),
                        None,
                    ));
                }
                if targets
                    .insert(
                        AbsoluteAddr {
                            instance_id: child,
                            var_id: binding.child_port,
                        },
                        AbsoluteAddr {
                            instance_id: parent_instance_id,
                            var_id: binding.parent_var,
                        },
                    )
                    .is_some()
                {
                    return Err(ParserError::illegal_context(
                        "ref port",
                        format!(
                            "ref port `{}` of `{}` is connected more than once",
                            port.path.join("."),
                            child_module.name
                        ),
                        None,
                    ));
                }
            }
        }
        // A hierarchical name or `ref` port may denote a variable that itself
        // denotes a package variable or another instance's variable.
        if !targets.is_empty() {
            let sources: Vec<_> = targets.keys().copied().collect();
            for source in sources {
                let mut target = targets[&source];
                let mut steps = 0;
                while let Some(&next) = targets.get(&target) {
                    steps += 1;
                    if next == source || steps > targets.len() {
                        return Err(ParserError::illegal_context(
                            if paths.is_some() {
                                "hierarchical reference"
                            } else {
                                "ref port"
                            },
                            "a hierarchical name or ref port denotes itself".to_string(),
                            None,
                        ));
                    }
                    target = next;
                }
                targets.insert(source, target);
            }
        }
        for (&instance_id, module_id) in instance_modules {
            let module = &modules[module_id];
            for (&var_id, variable) in &module.variables {
                if variable.kind == crate::VariableKind::Ref
                    && !targets.contains_key(&AbsoluteAddr {
                        instance_id,
                        var_id,
                    })
                {
                    return Err(ParserError::illegal_context(
                        "ref port",
                        format!(
                            "unconnected ref port `{}` of `{}`",
                            variable.path.join("."),
                            module.name
                        ),
                        None,
                    ));
                }
            }
        }
        let mut staged_nba_aliases = HashMap::default();
        let mut alias_working_targets = HashMap::default();
        let mut sorted_instances: Vec<_> = instance_modules
            .iter()
            .map(|(&instance_id, &module_id)| (instance_id, module_id))
            .collect();
        sorted_instances.sort_unstable_by_key(|(instance_id, _)| instance_id.0);
        for (instance_id, module_id) in sorted_instances {
            let module = &modules[&module_id];
            if !module.staged_nba_bindings.is_empty() {
                let mut canonical: HashMap<
                    (usize, u64, AbsoluteAddr),
                    (AbsoluteAddr, AbsoluteAddr),
                > = HashMap::default();
                for binding in &module.staged_nba_bindings {
                    let (Some(value_var), Some(mask_var)) = (
                        module.variables.get(&binding.value),
                        module.variables.get(&binding.mask),
                    ) else {
                        return Err(ParserError::illegal_context(
                            "staged nba binding",
                            format!(
                                "module `{}` references unknown staged nonblocking shadow variable",
                                module.name
                            ),
                            None,
                        ));
                    };
                    let raw_target = AbsoluteAddr {
                        instance_id,
                        var_id: binding.target,
                    };
                    let target_addr = targets.get(&raw_target).copied().unwrap_or(raw_target);
                    let value = AbsoluteAddr {
                        instance_id,
                        var_id: binding.value,
                    };
                    let mask = AbsoluteAddr {
                        instance_id,
                        var_id: binding.mask,
                    };
                    match canonical.entry((binding.process, binding.delay, target_addr)) {
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            entry.insert((value, mask));
                        }
                        std::collections::hash_map::Entry::Occupied(entry) => {
                            let &(canon_value, canon_mask) = entry.get();
                            let canon_value_meta = &module.variables[&canon_value.var_id].metadata;
                            let canon_mask_meta = &module.variables[&canon_mask.var_id].metadata;
                            let value_matches = (
                                value_var.metadata.width,
                                value_var.metadata.is_4state,
                                &value_var.metadata.array_dims,
                                value_var.metadata.value_kind,
                            ) == (
                                canon_value_meta.width,
                                canon_value_meta.is_4state,
                                &canon_value_meta.array_dims,
                                canon_value_meta.value_kind,
                            );
                            let mask_matches = (
                                mask_var.metadata.width,
                                mask_var.metadata.is_4state,
                                &mask_var.metadata.array_dims,
                                mask_var.metadata.value_kind,
                            ) == (
                                canon_mask_meta.width,
                                canon_mask_meta.is_4state,
                                &canon_mask_meta.array_dims,
                                canon_mask_meta.value_kind,
                            );
                            if !value_matches || !mask_matches {
                                return Err(ParserError::illegal_context(
                                    "staged nba binding",
                                    format!(
                                        "staged nonblocking bindings in `{}` alias variables with \
                                         mismatched storage metadata",
                                        module.name
                                    ),
                                    None,
                                ));
                            }
                            if value != canon_value
                                && !targets.contains_key(&value)
                                && !staged_nba_aliases.contains_key(&value)
                            {
                                staged_nba_aliases.insert(value, canon_value);
                            }
                            if mask != canon_mask
                                && !targets.contains_key(&mask)
                                && !staged_nba_aliases.contains_key(&mask)
                            {
                                staged_nba_aliases.insert(mask, canon_mask);
                            }
                        }
                    }
                }
            }
            if !module.alias_working_bindings.is_empty() {
                let mut canonical_work: HashMap<(usize, AbsoluteAddr), AbsoluteAddr> =
                    HashMap::default();
                for binding in &module.alias_working_bindings {
                    let Some(shadow_var) = module.variables.get(&binding.shadow) else {
                        return Err(ParserError::illegal_context(
                            "alias working binding",
                            format!(
                                "module `{}` references unknown alias working shadow variable",
                                module.name
                            ),
                            None,
                        ));
                    };
                    let raw_target = AbsoluteAddr {
                        instance_id,
                        var_id: binding.target,
                    };
                    let target_addr = targets.get(&raw_target).copied().unwrap_or(raw_target);
                    let shadow = AbsoluteAddr {
                        instance_id,
                        var_id: binding.shadow,
                    };
                    match canonical_work.entry((binding.process, target_addr)) {
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            entry.insert(shadow);
                            alias_working_targets.insert(shadow, target_addr);
                        }
                        std::collections::hash_map::Entry::Occupied(entry) => {
                            let &canon_shadow = entry.get();
                            let canon_shadow_meta =
                                &module.variables[&canon_shadow.var_id].metadata;
                            let shadow_matches = (
                                shadow_var.metadata.width,
                                shadow_var.metadata.is_4state,
                                &shadow_var.metadata.array_dims,
                                shadow_var.metadata.value_kind,
                            ) == (
                                canon_shadow_meta.width,
                                canon_shadow_meta.is_4state,
                                &canon_shadow_meta.array_dims,
                                canon_shadow_meta.value_kind,
                            );
                            if !shadow_matches {
                                return Err(ParserError::illegal_context(
                                    "alias working binding",
                                    format!(
                                        "alias working bindings in `{}` alias variables with \
                                         mismatched storage metadata",
                                        module.name
                                    ),
                                    None,
                                ));
                            }
                            if shadow != canon_shadow
                                && !targets.contains_key(&shadow)
                                && !staged_nba_aliases.contains_key(&shadow)
                            {
                                staged_nba_aliases.insert(shadow, canon_shadow);
                            }
                        }
                    }
                }
            }
        }
        let object_aliases = targets.iter().map(|(&a, &b)| (a, b)).collect();
        for (path, &instance_id) in expanded {
            let module = &modules[&instance_modules[&instance_id]];
            let mut children = HashMap::default();
            for (var_id, port) in module.instance_ports.iter() {
                if module.variables.contains_key(&var_id) {
                    return Err(ParserError::illegal_context(
                        "instance port",
                        "module variable and port reference overlap".to_owned(),
                        None,
                    ));
                }
                let child = match children.entry((&port.name, port.index)) {
                    std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        let mut child_path = path.0.clone();
                        child_path.push((port.name.clone(), port.index));
                        let child = expanded.get(&InstancePath(child_path)).ok_or_else(|| {
                            ParserError::illegal_context(
                                "instance port",
                                format!(
                                    "unknown child {}[{}] of {}",
                                    port.name, port.index, module.name
                                ),
                                None,
                            )
                        })?;
                        *entry.insert(*child)
                    }
                };
                let child_module = &modules[&instance_modules[&child]];
                if !child_module.variables.contains_key(&port.variable) {
                    return Err(ParserError::illegal_context(
                        "instance port",
                        format!(
                            "unknown variable {} of {}",
                            port.variable, child_module.name
                        ),
                        None,
                    ));
                }
                let target = AbsoluteAddr {
                    instance_id: child,
                    var_id: port.variable,
                };
                targets.insert(
                    AbsoluteAddr {
                        instance_id,
                        var_id,
                    },
                    targets.get(&target).copied().unwrap_or(target),
                );
            }
        }
        Ok(Self {
            targets,
            object_aliases,
            staged_nba_aliases,
            alias_working_targets,
        })
    }

    /// Returns the canonical target address that an `alias_work` shadow serves,
    /// if `address` is one.
    pub(crate) fn alias_working_target(&self, address: AbsoluteAddr) -> Option<AbsoluteAddr> {
        self.alias_working_targets.get(&address).copied()
    }

    /// The object variable `var_id` of instance `instance_id` denotes.
    pub(crate) fn locate(&self, instance_id: InstanceId, var_id: SourceVarId) -> AbsoluteAddr {
        let address = AbsoluteAddr {
            instance_id,
            var_id,
        };
        self.staged_nba_aliases
            .get(&address)
            .or_else(|| self.targets.get(&address))
            .copied()
            .unwrap_or(address)
    }

    pub(crate) fn locate_regioned(
        &self,
        region: u32,
        instance_id: InstanceId,
        var_id: SourceVarId,
    ) -> RegionedAbsoluteAddr {
        let address = self.locate(instance_id, var_id);
        RegionedAbsoluteAddr {
            region,
            instance_id: address.instance_id,
            var_id: address.var_id,
        }
    }

    /// Whether the variable is bound to a package variable, and so has no
    /// state object of its own.
    pub(super) fn is_bound(&self, address: &AbsoluteAddr) -> bool {
        self.targets.contains_key(address)
    }
}

/// Reject a package variable that two flip-flop processes write, that a
/// continuous driver and any other process write, or that an `always_ff`,
/// `always_comb` or `always_latch` run as a process and any other process
/// write. Instances of one module are separate drivers.
/// Overlapping continuous drivers are rejected by the scheduler.
pub(super) fn check_package_variable_drivers(
    bindings: &PackageBindings,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    package_instances: &HashMap<ModuleId, InstanceId>,
    comb_blocks: &[celox_slt::LogicPath<AbsoluteAddr>],
) -> Result<(), ParserError> {
    let package_instance_ids: HashSet<InstanceId> = package_instances.values().copied().collect();
    // Simulator state bound as a package variable has no driver
    // restrictions; see [`PackageBinding::simulator_state`].
    let simulator_state: HashSet<AbsoluteAddr> = instance_modules
        .iter()
        .flat_map(|(&instance, module_id)| {
            modules[module_id]
                .package_bindings
                .iter()
                .filter(|(_, binding)| binding.simulator_state)
                .map(move |(&var_id, _)| bindings.locate(instance, var_id))
        })
        .collect();
    let packages: HashSet<AbsoluteAddr> = bindings
        .targets
        .values()
        .copied()
        .filter(|target| {
            package_instance_ids.contains(&target.instance_id) && !simulator_state.contains(target)
        })
        .collect();
    if packages.is_empty() {
        return Ok(());
    }
    let width = |address: &AbsoluteAddr| {
        modules[&instance_modules[&address.instance_id]].variables[&address.var_id]
            .metadata
            .width
    };
    let error = |address: &AbsoluteAddr| {
        let package = &modules[&instance_modules[&address.instance_id]];
        ParserError::unsupported(
            1146,
            crate::LoweringPhase::SimulatorParser,
            "package variable drivers",
            format!(
                "multiple drivers of package variable `{}::{}`",
                package.name,
                package.variables[&address.var_id].path.join(".")
            ),
            None,
        )
    };
    // Units of one clock are merged across instances when relocated, so
    // writers are told apart before: one per trigger group of an instance.
    let mut ff_writes: Vec<((InstanceId, usize), AbsoluteAddr, BitAccess)> = Vec::new();
    let mut instances = instance_modules.iter().collect::<Vec<_>>();
    instances.sort_unstable_by_key(|(instance, _)| instance.0);
    for (&instance, module_id) in instances {
        let module = &modules[module_id];
        // Publication units also describe the destinations of symbolic edge
        // plans, which only become executable after hierarchy expansion.
        let triggers: BTreeSet<_> = module.event_bodies.keys().collect();
        for (group, trigger) in triggers.into_iter().enumerate() {
            let body = &module.event_bodies[trigger];
            let procedures = module
                .next_state
                .evaluations
                .get(trigger)
                .into_iter()
                .flatten()
                .filter_map(|batch| match batch {
                    crate::symbolic::artifact::EdgeEvaluation::Procedure(unit) => Some(unit),
                    _ => None,
                });
            for unit in body.sample.iter().chain([&body.publish]).chain(procedures) {
                for block in unit.blocks.values() {
                    for instruction in &block.instructions {
                        for (address, access) in written_bits(instruction) {
                            let address = bindings.locate(instance, address.var_id);
                            if !packages.contains(&address) {
                                continue;
                            }
                            let access = access.unwrap_or_else(|| {
                                BitAccess::new(0, width(&address).saturating_sub(1))
                            });
                            ff_writes.push(((instance, group), address, access));
                        }
                    }
                }
            }
        }
    }
    // Initial writes of module instances: constant ones folded into initial
    // values, and initial processes. The package's own declaration
    // initializers are not drivers.
    let mut initial_writes: Vec<((InstanceId, usize), AbsoluteAddr, BitAccess)> = Vec::new();
    // The writes of processes that must be the only writers of what they
    // write (IEEE 1800-2023 9.2.2.2 to 9.2.2.4).
    let mut restricted_writes: Vec<((InstanceId, usize), AbsoluteAddr, BitAccess)> = Vec::new();
    for (&instance, module_id) in instance_modules {
        let module = &modules[module_id];
        if package_instances.contains_key(module_id) {
            continue;
        }
        for initial in &module.initial_memory_values {
            let address = bindings.locate(instance, initial.address);
            if !packages.contains(&address) {
                continue;
            }
            // The bits the initial value writes.
            let accesses = match &initial.data {
                celox_design::InitialStateData::Packed { written_mask, .. } => {
                    let bits = written_mask.bits() as usize;
                    written_mask
                        .trailing_zeros()
                        .map(|lsb| vec![BitAccess::new(lsb as usize, bits.saturating_sub(1))])
                        .unwrap_or_default()
                }
                celox_design::InitialStateData::Writes(runs) => runs
                    .iter()
                    .filter(|run| run.bit_width > 0)
                    .map(|run| BitAccess::new(run.bit_offset, run.bit_offset + run.bit_width - 1))
                    .collect(),
                celox_design::InitialStateData::MemoryFile(load) => load
                    .written_bits()
                    .map(|(lsb, msb)| vec![BitAccess::new(lsb, msb)])
                    .unwrap_or_default(),
            };
            for access in accesses {
                initial_writes.push(((instance, usize::MAX), address, access));
            }
        }
        for (index, process) in module.processes.iter().enumerate() {
            let restricted = matches!(
                process.origin.construct.as_str(),
                "always_ff" | "always_comb" | "always_latch"
            );
            for block in process.kernel.blocks.values() {
                for instruction in &block.instructions {
                    for (address, access) in written_bits(instruction) {
                        let address = bindings.locate(instance, address.var_id);
                        if packages.contains(&address) {
                            // The bits the store writes, as for flip-flop
                            // writes, so disjoint writers are not rejected.
                            let access = access.unwrap_or_else(|| {
                                BitAccess::new(0, width(&address).saturating_sub(1))
                            });
                            initial_writes.push(((instance, index), address, access));
                            if restricted {
                                restricted_writes.push(((instance, index), address, access));
                            }
                        }
                    }
                }
            }
        }
    }
    let conflict =
        |writes: &[((InstanceId, usize), AbsoluteAddr, BitAccess)],
         distinct: &dyn Fn(&(InstanceId, usize), &(InstanceId, usize)) -> bool| {
            writes
                .iter()
                .enumerate()
                .find_map(|(index, (writer, address, access))| {
                    writes[index + 1..]
                        .iter()
                        .any(|(other, other_address, other_access)| {
                            distinct(writer, other)
                                && other_address == address
                                && other_access.overlaps(access)
                        })
                        .then_some(*address)
                })
        };
    // Two flip-flop processes race. Initial writes and processes of several
    // instances may write one package variable, as any processes may write
    // a variable (IEEE 1800-2023 6.5).
    if let Some(address) = conflict(&ff_writes, &|left, right| left != right) {
        return Err(error(&address));
    }
    // A restricted process overlaps every write of another process.
    for (writer, address, access) in &restricted_writes {
        let overlaps = |(other, other_address, other_access): &(
            (InstanceId, usize),
            AbsoluteAddr,
            BitAccess,
        )| {
            other_address == address && other_access.overlaps(access) && other != writer
        };
        if initial_writes.iter().any(overlaps)
            || ff_writes.iter().any(|(_, other_address, other_access)| {
                other_address == address && other_access.overlaps(access)
            })
        {
            return Err(error(address));
        }
    }
    // A continuous driver overlaps every other write.
    for path in comb_blocks {
        let Some(target) = path.target.var() else {
            continue;
        };
        if ff_writes
            .iter()
            .chain(&initial_writes)
            .any(|(_, address, access)| *address == target.id && access.overlaps(&target.access))
        {
            return Err(error(&target.id));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VariableKind;
    use crate::symbolic::artifact::{PackageBinding, RefPortBinding, SymbolicVariable};

    fn dummy_module(name: &str) -> SimModule {
        SimModule {
            testbench_variables: Default::default(),
            time_precision: None,
            time_scale: celox_design::TimeScale::default(),
            name: name.to_owned(),
            variables: HashMap::default(),
            ff_access_summaries: HashMap::default(),
            event_bodies: HashMap::default(),
            split_commit_ff_blocks: HashMap::default(),
            parallel_ff_parts: HashMap::default(),
            instances: HashMap::default(),
            instance_ports: Default::default(),
            indexed_instance_names: HashSet::default(),
            instance_index_bases: HashMap::default(),
            comb_blocks: Vec::new(),
            comb_observers: Vec::new(),
            runtime_errors: HashMap::default(),
            runtime_event_sites: Vec::new(),
            memory_file_bases: Vec::new(),
            extern_functions: Vec::new(),
            initial_memory_values: Vec::new(),
            declaration_initial_memory_values: Vec::new(),
            folded_initial_memory_values: Vec::new(),
            comb_boundaries: HashMap::default(),
            arena: SLTNodeArena::new(),
            reset_clock_map: HashMap::default(),
            processes: Vec::new(),
            functions: Vec::new(),
            function_frames: Vec::new(),
            package_bindings: HashMap::default(),
            hierarchical_bindings: HashMap::default(),
            ref_port_bindings: Vec::new(),
            staged_nba_bindings: Vec::new(),
            alias_working_bindings: Vec::new(),
            counter_slots: Vec::new(),
            pending_slots: Vec::new(),
            comb_alternatives: Vec::new(),
            wait_sites: Vec::new(),
            nba_sites: Vec::new(),
            nba_switches: Vec::new(),
            host_request_sites: Vec::new(),
            time_slot: None,
            sample_shadows: Vec::new(),
            initial_copies: Vec::new(),
            next_state: Default::default(),
        }
    }

    fn var(name: &str, kind: VariableKind, width: usize, is_4state: bool) -> SymbolicVariable {
        SymbolicVariable {
            path: vec![name.to_owned()],
            kind,
            signed: false,
            metadata: VariableMetadata {
                width,
                is_4state,
                kind: DomainKind::Other,
                type_kind: celox_design::PortTypeKind::Logic,
                array_dims: Vec::new(),
                value_kind: celox_design::ValueKind::Bits,
            },
            packed_dims: vec![width],
            unpacked_ranges: Vec::new(),
            source: None,
            module_affiliated: true,
        }
    }

    #[test]
    fn ref_port_bindings_resolve_chained_and_package_aliases() {
        let pkg_id = ModuleId(0);
        let top_id = ModuleId(1);
        let mid_id = ModuleId(2);
        let leaf_id = ModuleId(3);

        let mut pkg = dummy_module("$pkg");
        pkg.variables
            .insert(SourceVarId(0), var("pv", VariableKind::Variable, 32, false));

        let mut top = dummy_module("Top");
        top.variables.insert(
            SourceVarId(0),
            var("pv_alias", VariableKind::Variable, 32, false),
        );
        top.package_bindings.insert(
            SourceVarId(0),
            PackageBinding {
                package: pkg_id,
                var_id: SourceVarId(0),
                simulator_state: false,
            },
        );
        top.ref_port_bindings.push(RefPortBinding {
            instance: "u_mid".into(),
            index: 0,
            child_port: SourceVarId(0),
            parent_var: SourceVarId(0),
        });

        let mut mid = dummy_module("Mid");
        mid.variables
            .insert(SourceVarId(0), var("m", VariableKind::Ref, 32, false));
        mid.ref_port_bindings.push(RefPortBinding {
            instance: "u_leaf".into(),
            index: 0,
            child_port: SourceVarId(0),
            parent_var: SourceVarId(0),
        });

        let mut leaf = dummy_module("Leaf");
        leaf.variables
            .insert(SourceVarId(0), var("l", VariableKind::Ref, 32, false));

        let mut modules = HashMap::default();
        modules.insert(pkg_id, pkg);
        modules.insert(top_id, top);
        modules.insert(mid_id, mid);
        modules.insert(leaf_id, leaf);

        let pkg_inst = InstanceId(0);
        let top_inst = InstanceId(1);
        let mid_inst = InstanceId(2);
        let leaf_inst = InstanceId(3);

        let mut expanded = HashMap::default();
        expanded.insert(InstancePath(vec![]), top_inst);
        expanded.insert(InstancePath(vec![("u_mid".into(), 0)]), mid_inst);
        expanded.insert(
            InstancePath(vec![("u_mid".into(), 0), ("u_leaf".into(), 0)]),
            leaf_inst,
        );

        let mut instance_modules = HashMap::default();
        instance_modules.insert(pkg_inst, pkg_id);
        instance_modules.insert(top_inst, top_id);
        instance_modules.insert(mid_inst, mid_id);
        instance_modules.insert(leaf_inst, leaf_id);

        let mut package_instances = HashMap::default();
        package_instances.insert(pkg_id, pkg_inst);

        let bindings =
            PackageBindings::new(&expanded, &instance_modules, &modules, &package_instances)
                .unwrap();
        let expected = AbsoluteAddr {
            instance_id: pkg_inst,
            var_id: SourceVarId(0),
        };
        assert_eq!(bindings.locate(top_inst, SourceVarId(0)), expected);
        assert_eq!(bindings.locate(mid_inst, SourceVarId(0)), expected);
        assert_eq!(bindings.locate(leaf_inst, SourceVarId(0)), expected);

        let vars = hierarchy::module_variables(&modules);
        assert!(!vars.0[&mid_id].contains_key(&SourceVarId(0)));
        assert!(!vars.0[&leaf_id].contains_key(&SourceVarId(0)));
    }

    #[test]
    fn ref_port_bindings_reject_unconnected_or_mismatched() {
        let top_id = ModuleId(0);
        let child_id = ModuleId(1);

        let mut top = dummy_module("Top");
        top.variables
            .insert(SourceVarId(0), var("v", VariableKind::Variable, 16, false));

        let mut child = dummy_module("Child");
        child
            .variables
            .insert(SourceVarId(0), var("r", VariableKind::Ref, 32, false));

        let mut modules = HashMap::default();
        modules.insert(top_id, top);
        modules.insert(child_id, child);

        let mut expanded = HashMap::default();
        expanded.insert(InstancePath(vec![]), InstanceId(0));
        expanded.insert(InstancePath(vec![("u".into(), 0)]), InstanceId(1));

        let mut instance_modules = HashMap::default();
        instance_modules.insert(InstanceId(0), top_id);
        instance_modules.insert(InstanceId(1), child_id);

        let err = PackageBindings::new(&expanded, &instance_modules, &modules, &HashMap::default())
            .err()
            .unwrap();
        assert!(err.to_string().contains("unconnected ref port"), "{err}");

        modules
            .get_mut(&top_id)
            .unwrap()
            .ref_port_bindings
            .push(RefPortBinding {
                instance: "u".into(),
                index: 0,
                child_port: SourceVarId(0),
                parent_var: SourceVarId(0),
            });
        let err = PackageBindings::new(&expanded, &instance_modules, &modules, &HashMap::default())
            .err()
            .unwrap();
        assert!(err.to_string().contains("another type"), "{err}");
    }

    #[test]
    fn staged_nba_bindings_alias_only_when_targets_collide_per_instance() {
        use crate::symbolic::artifact::StagedNbaBinding;

        let top_id = ModuleId(0);
        let child_id = ModuleId(1);

        let mut top = dummy_module("Top");
        top.variables.insert(
            SourceVarId(0),
            var("shared", VariableKind::Variable, 32, false),
        );
        top.variables.insert(
            SourceVarId(1),
            var("sep_a", VariableKind::Variable, 32, false),
        );
        top.variables.insert(
            SourceVarId(2),
            var("sep_b", VariableKind::Variable, 32, false),
        );
        for (instance, a_var, b_var) in [
            ("u_alias", SourceVarId(0), SourceVarId(0)),
            ("u_sep", SourceVarId(1), SourceVarId(2)),
        ] {
            top.ref_port_bindings.push(RefPortBinding {
                instance: instance.into(),
                index: 0,
                child_port: SourceVarId(0),
                parent_var: a_var,
            });
            top.ref_port_bindings.push(RefPortBinding {
                instance: instance.into(),
                index: 0,
                child_port: SourceVarId(1),
                parent_var: b_var,
            });
        }

        let mut child = dummy_module("Child");
        child
            .variables
            .insert(SourceVarId(0), var("a", VariableKind::Ref, 32, false));
        child
            .variables
            .insert(SourceVarId(1), var("b", VariableKind::Ref, 32, false));
        child.variables.insert(
            SourceVarId(2),
            var("nba_val_a", VariableKind::Variable, 32, false),
        );
        child.variables.insert(
            SourceVarId(3),
            var("nba_mask_a", VariableKind::Variable, 32, false),
        );
        child.variables.insert(
            SourceVarId(4),
            var("nba_val_b", VariableKind::Variable, 32, false),
        );
        child.variables.insert(
            SourceVarId(5),
            var("nba_mask_b", VariableKind::Variable, 32, false),
        );
        child.variables.insert(
            SourceVarId(6),
            var("nba_val_b_d1", VariableKind::Variable, 32, false),
        );
        child.variables.insert(
            SourceVarId(7),
            var("nba_mask_b_d1", VariableKind::Variable, 32, false),
        );
        child.variables.insert(
            SourceVarId(8),
            var("nba_val_b_p1", VariableKind::Variable, 32, false),
        );
        child.variables.insert(
            SourceVarId(9),
            var("nba_mask_b_p1", VariableKind::Variable, 32, false),
        );
        child.staged_nba_bindings = vec![
            StagedNbaBinding {
                process: 0,
                delay: 0,
                target: SourceVarId(0),
                value: SourceVarId(2),
                mask: SourceVarId(3),
            },
            StagedNbaBinding {
                process: 0,
                delay: 0,
                target: SourceVarId(1),
                value: SourceVarId(4),
                mask: SourceVarId(5),
            },
            StagedNbaBinding {
                process: 0,
                delay: 5,
                target: SourceVarId(1),
                value: SourceVarId(6),
                mask: SourceVarId(7),
            },
            StagedNbaBinding {
                process: 1,
                delay: 0,
                target: SourceVarId(1),
                value: SourceVarId(8),
                mask: SourceVarId(9),
            },
        ];

        let mut modules = HashMap::default();
        modules.insert(top_id, top);
        modules.insert(child_id, child);

        let top_inst = InstanceId(0);
        let alias_inst = InstanceId(1);
        let sep_inst = InstanceId(2);

        let mut expanded = HashMap::default();
        expanded.insert(InstancePath(vec![]), top_inst);
        expanded.insert(InstancePath(vec![("u_alias".into(), 0)]), alias_inst);
        expanded.insert(InstancePath(vec![("u_sep".into(), 0)]), sep_inst);

        let mut instance_modules = HashMap::default();
        instance_modules.insert(top_inst, top_id);
        instance_modules.insert(alias_inst, child_id);
        instance_modules.insert(sep_inst, child_id);

        let bindings =
            PackageBindings::new(&expanded, &instance_modules, &modules, &HashMap::default())
                .unwrap();

        // u_alias shares (process=0, delay=0) value & mask stages for aliased a/b
        assert_eq!(
            bindings.locate(alias_inst, SourceVarId(4)),
            AbsoluteAddr {
                instance_id: alias_inst,
                var_id: SourceVarId(2),
            }
        );
        assert_eq!(
            bindings.locate(alias_inst, SourceVarId(5)),
            AbsoluteAddr {
                instance_id: alias_inst,
                var_id: SourceVarId(3),
            }
        );
        assert!(!bindings.is_bound(&AbsoluteAddr {
            instance_id: alias_inst,
            var_id: SourceVarId(4),
        }));
        assert!(!bindings.is_bound(&AbsoluteAddr {
            instance_id: alias_inst,
            var_id: SourceVarId(5),
        }));
        // Distinct delay and distinct process keep independent stages even in u_alias
        assert_eq!(
            bindings.locate(alias_inst, SourceVarId(6)),
            AbsoluteAddr {
                instance_id: alias_inst,
                var_id: SourceVarId(6),
            }
        );
        assert_eq!(
            bindings.locate(alias_inst, SourceVarId(8)),
            AbsoluteAddr {
                instance_id: alias_inst,
                var_id: SourceVarId(8),
            }
        );
        // u_sep keeps independent stages for unaliased a/b
        assert_eq!(
            bindings.locate(sep_inst, SourceVarId(4)),
            AbsoluteAddr {
                instance_id: sep_inst,
                var_id: SourceVarId(4),
            }
        );
        assert_eq!(
            bindings.locate(sep_inst, SourceVarId(5)),
            AbsoluteAddr {
                instance_id: sep_inst,
                var_id: SourceVarId(5),
            }
        );
    }

    #[test]
    fn alias_working_bindings_alias_only_within_same_instance_and_process() {
        use crate::symbolic::artifact::AliasWorkingBinding;

        let top_id = ModuleId(0);
        let child_id = ModuleId(1);

        let mut top = dummy_module("Top");
        top.variables.insert(
            SourceVarId(0),
            var("shared", VariableKind::Variable, 8, false),
        );
        top.variables.insert(
            SourceVarId(1),
            var("sep_a", VariableKind::Variable, 8, false),
        );
        top.variables.insert(
            SourceVarId(2),
            var("sep_b", VariableKind::Variable, 8, false),
        );
        for (instance, a_var, b_var) in [
            ("u_alias", SourceVarId(0), SourceVarId(0)),
            ("u_sep", SourceVarId(1), SourceVarId(2)),
        ] {
            top.ref_port_bindings.push(RefPortBinding {
                instance: instance.into(),
                index: 0,
                child_port: SourceVarId(0),
                parent_var: a_var,
            });
            top.ref_port_bindings.push(RefPortBinding {
                instance: instance.into(),
                index: 0,
                child_port: SourceVarId(1),
                parent_var: b_var,
            });
        }

        let mut child = dummy_module("Child");
        child
            .variables
            .insert(SourceVarId(0), var("a", VariableKind::Ref, 8, false));
        child
            .variables
            .insert(SourceVarId(1), var("b", VariableKind::Ref, 8, false));
        child.variables.insert(
            SourceVarId(2),
            var("alias_work_p0_a", VariableKind::Variable, 8, false),
        );
        child.variables.insert(
            SourceVarId(3),
            var("alias_work_p0_b", VariableKind::Variable, 8, false),
        );
        child.variables.insert(
            SourceVarId(4),
            var("alias_work_p1_b", VariableKind::Variable, 8, false),
        );
        child.alias_working_bindings = vec![
            AliasWorkingBinding {
                process: 0,
                target: SourceVarId(0),
                shadow: SourceVarId(2),
            },
            AliasWorkingBinding {
                process: 0,
                target: SourceVarId(1),
                shadow: SourceVarId(3),
            },
            AliasWorkingBinding {
                process: 1,
                target: SourceVarId(1),
                shadow: SourceVarId(4),
            },
        ];

        let mut modules = HashMap::default();
        modules.insert(top_id, top);
        modules.insert(child_id, child);

        let top_inst = InstanceId(0);
        let alias_inst = InstanceId(1);
        let sep_inst = InstanceId(2);

        let mut expanded = HashMap::default();
        expanded.insert(InstancePath(vec![]), top_inst);
        expanded.insert(InstancePath(vec![("u_alias".into(), 0)]), alias_inst);
        expanded.insert(InstancePath(vec![("u_sep".into(), 0)]), sep_inst);

        let mut instance_modules = HashMap::default();
        instance_modules.insert(top_inst, top_id);
        instance_modules.insert(alias_inst, child_id);
        instance_modules.insert(sep_inst, child_id);

        let bindings =
            PackageBindings::new(&expanded, &instance_modules, &modules, &HashMap::default())
                .unwrap();

        // u_alias shares process 0 shadows between a and b, pointing back to Top::shared
        let p0_canon = AbsoluteAddr {
            instance_id: alias_inst,
            var_id: SourceVarId(2),
        };
        let top_shared = AbsoluteAddr {
            instance_id: top_inst,
            var_id: SourceVarId(0),
        };
        assert_eq!(bindings.locate(alias_inst, SourceVarId(3)), p0_canon);
        assert_eq!(bindings.alias_working_target(p0_canon), Some(top_shared));

        // Process 1 keeps its own shadow even for the same target
        let p1_shadow = AbsoluteAddr {
            instance_id: alias_inst,
            var_id: SourceVarId(4),
        };
        assert_eq!(bindings.locate(alias_inst, SourceVarId(4)), p1_shadow);
        assert_eq!(bindings.alias_working_target(p1_shadow), Some(top_shared));

        // u_sep keeps independent shadows for unaliased a and b
        let sep_p0_b = AbsoluteAddr {
            instance_id: sep_inst,
            var_id: SourceVarId(3),
        };
        assert_eq!(bindings.locate(sep_inst, SourceVarId(3)), sep_p0_b);
        assert_eq!(
            bindings.alias_working_target(sep_p0_b),
            Some(AbsoluteAddr {
                instance_id: top_inst,
                var_id: SourceVarId(2),
            })
        );
    }
}
