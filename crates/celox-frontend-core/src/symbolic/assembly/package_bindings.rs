//! Where the variables of each instance live, and the drivers of package variables.

use super::*;

/// Where the variables of each instance live. A module variable bound to a
/// package variable (IEEE 1800-2023 26.2) is that package's one object, in
/// the package's instance; every other variable belongs to its instance.
#[derive(Default)]
pub(crate) struct PackageBindings {
    pub(super) targets: HashMap<AbsoluteAddr, AbsoluteAddr>,
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
        // A hierarchical name may denote a variable that itself denotes a
        // package variable or another instance's variable.
        if paths.is_some() {
            let sources: Vec<_> = targets.keys().copied().collect();
            for source in sources {
                let mut target = targets[&source];
                let mut steps = 0;
                while let Some(&next) = targets.get(&target) {
                    steps += 1;
                    if next == source || steps > targets.len() {
                        return Err(ParserError::illegal_context(
                            "hierarchical reference",
                            "a hierarchical name denotes itself".to_string(),
                            None,
                        ));
                    }
                    target = next;
                }
                targets.insert(source, target);
            }
        }
        Ok(Self { targets })
    }

    /// The object variable `var_id` of instance `instance_id` denotes.
    pub(crate) fn locate(&self, instance_id: InstanceId, var_id: SourceVarId) -> AbsoluteAddr {
        let address = AbsoluteAddr {
            instance_id,
            var_id,
        };
        self.targets.get(&address).copied().unwrap_or(address)
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
        let units = module.eval_apply_ff_blocks.iter().chain(
            module
                .eval_only_ff_blocks
                .iter()
                .filter(|(trigger, _)| !module.eval_apply_ff_blocks.contains_key(*trigger)),
        );
        for (group, (_, unit)) in units.enumerate() {
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
