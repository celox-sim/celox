//! Checks of the drivers of nets and variables, in a module and across its instances.

use super::*;

/// Record the `inout` ports whose net has its driver in their module or
/// below it. Both sides of an inout port connection form one net (IEEE
/// 1800-2023 23.3.3), so the connection carries the value away from the side
/// that drives it. A net driven on both sides is then reported by
/// [`validate_specialized_instance_net_drivers`].
pub(super) fn resolve_inout_drivers(
    module_ids: &HashMap<LoweredSvModuleKey, ModuleId>,
    modules: &mut HashMap<ModuleId, LoweredSvModule>,
) {
    loop {
        let mut driven = Vec::new();
        for (&module_id, module) in modules.iter() {
            let mut drivers = None;
            for port in module.source.ports() {
                if port.direction() != sv::ir::PortDirection::Inout
                    || module.inout_driven_inside.contains(port.name())
                {
                    continue;
                }
                let (local, children) = drivers.get_or_insert_with(|| {
                    (
                        ModuleDrivers::new(
                            &module.source,
                            &module.constants,
                            &module.parameter_types,
                        ),
                        child_output_drivers(module, module_ids, modules),
                    )
                });
                if !local.local(port.name()).is_empty()
                    || !child_output_driver_ranges(module, children, port.name()).is_empty()
                {
                    driven.push((module_id, port.name().to_string()));
                }
            }
        }
        if driven.is_empty() {
            return;
        }
        for (module_id, port) in driven {
            if let Some(module) = modules.get_mut(&module_id) {
                module.inout_driven_inside.insert(port);
            }
        }
    }
}

pub(super) fn validate_specialized_instance_net_drivers(
    module_ids: &HashMap<LoweredSvModuleKey, ModuleId>,
    modules: &HashMap<ModuleId, LoweredSvModule>,
    four_state: bool,
) -> Result<(), sv::AnalyzerError> {
    for module in modules.values() {
        let drivers =
            ModuleDrivers::new(&module.source, &module.constants, &module.parameter_types);
        let children = child_output_drivers(module, module_ids, modules);
        // Table 10-1 permits procedural writes only to variables. Include
        // suspended processes and writes made by their called subroutines;
        // declaration initializers of nets remain continuous drivers.
        let mut procedural_writes = HashSet::default();
        for body in module
            .source
            .comb_processes()
            .iter()
            .filter(|process| process.kind() != sv::ir::CombProcessKind::ContinuousAssign)
            .map(sv::ir::CombProcess::body)
            .chain(
                module
                    .source
                    .ff_processes()
                    .iter()
                    .map(sv::ir::FfProcess::body),
            )
            .chain(
                module
                    .source
                    .initial_processes()
                    .iter()
                    .map(|process| process.body()),
            )
        {
            procedural::written_names_with_calls(&module.source, body, &mut procedural_writes);
        }
        // Hierarchical aliases keep the specific driver diagnostics below.
        for name in module
            .source
            .signals()
            .iter()
            .filter(|signal| signal.is_net() && signal.hierarchical().is_none())
            .map(|signal| signal.name())
            .chain(
                module
                    .source
                    .ports()
                    .iter()
                    .filter(|port| port.is_net())
                    .map(|port| port.name()),
            )
        {
            if procedural_writes.contains(name) {
                return Err(sv::AnalyzerError::ProceduralNetAssignment {
                    name: name.to_string(),
                });
            }
        }
        for port in module
            .source
            .ports()
            .iter()
            .filter(|port| port.direction() == sv::ir::PortDirection::Input)
        {
            if !child_output_driver_ranges(module, &children, port.name()).is_empty() {
                return Err(sv::AnalyzerError::Unsupported(format!(
                    "write to input port `{}`",
                    port.name()
                )));
            }
        }

        // A hierarchical name denotes another instance's object, which its
        // own instance checks; only procedural writes of variables may
        // reach it through the name.
        let mut written = None;
        for signal in module.source.signals() {
            let Some(name) = signal.hierarchical() else {
                continue;
            };
            let driven = if signal.is_net() {
                written
                    .get_or_insert_with(|| {
                        let source = &module.source;
                        let mut written = HashSet::default();
                        for body in source
                            .comb_processes()
                            .iter()
                            .map(sv::ir::CombProcess::body)
                            .chain(source.ff_processes().iter().map(sv::ir::FfProcess::body))
                            .chain(
                                source
                                    .initial_processes()
                                    .iter()
                                    .map(|process| process.body()),
                            )
                        {
                            procedural::written_names_with_calls(source, body, &mut written);
                        }
                        written
                    })
                    .contains(signal.name())
            } else {
                !drivers.continuous(signal.name()).is_empty()
            };
            if driven || !child_output_driver_ranges(module, &children, signal.name()).is_empty() {
                return Err(sv::AnalyzerError::Unsupported(format!(
                    "{} through hierarchical name `{name}`",
                    if signal.is_net() {
                        "driver of a net"
                    } else {
                        "continuous assignment to a variable"
                    }
                )));
            }
        }

        let net_names = module
            .source
            .signals()
            .iter()
            .filter(|signal| signal.is_net() && signal.hierarchical().is_none())
            .map(|signal| signal.name())
            .chain(
                module
                    .source
                    .ports()
                    .iter()
                    .filter(|port| port.is_net())
                    .map(|port| port.name()),
            );
        let resolved_nets = if continuous_targets_repeat(&module.source) {
            net_resolution::ResolvedNets::of(&module.source, &drivers, four_state)
        } else {
            net_resolution::ResolvedNets::default()
        };
        for signal_name in net_names {
            let child_driver_ranges = child_output_driver_ranges(module, &children, signal_name);
            validate_net_driver_ranges(
                &drivers,
                signal_name,
                &child_driver_ranges,
                resolved_nets.net(signal_name).is_some(),
            )?;
        }

        let variable_names = module
            .source
            .signals()
            .iter()
            .filter(|signal| !signal.is_net() && signal.hierarchical().is_none())
            .map(|signal| signal.name())
            .chain(
                module
                    .source
                    .ports()
                    .iter()
                    .filter(|port| !port.is_net())
                    .map(|port| port.name()),
            );
        for signal_name in variable_names {
            let child_driver_ranges = child_output_driver_ranges(module, &children, signal_name);
            let local_drivers = drivers.local(signal_name);
            let child_overlaps = driver_ranges_overlap(&child_driver_ranges);
            let child_local_overlap = child_driver_ranges.iter().any(|(_, child_range)| {
                local_drivers
                    .iter()
                    .any(|(_, local_range)| net_driver_ranges_overlap(*child_range, *local_range))
            });
            if child_overlaps || child_local_overlap {
                return Err(sv::AnalyzerError::Unsupported(format!(
                    "multiple variable drivers for `{signal_name}`"
                )));
            }
            // A variable written by a continuous assignment or an output
            // port may not also be written procedurally, including by its
            // initializer or an `initial` block (IEEE 1800-2023 6.5).
            let mut continuous = child_driver_ranges
                .into_iter()
                .chain(drivers.continuous(signal_name));
            let initial = drivers.initial(signal_name);
            if continuous.any(|(_, continuous)| {
                initial
                    .iter()
                    .any(|(_, initial)| net_driver_ranges_overlap(continuous, *initial))
            }) {
                return Err(sv::AnalyzerError::Unsupported(format!(
                    "procedural initialization of `{signal_name}`, which a continuous assignment or output port also drives"
                )));
            }
        }
    }
    Ok(())
}

/// The ranges of the signals of `module` that output and driven `inout`
/// ports of its child instances drive, by signal, numbered per signal.
fn child_output_drivers(
    module: &LoweredSvModule,
    module_ids: &HashMap<LoweredSvModuleKey, ModuleId>,
    modules: &HashMap<ModuleId, LoweredSvModule>,
) -> ChildOutputDrivers {
    let mut drivers: ChildOutputDrivers = HashMap::default();
    let mut push = |signal_id, range| {
        let signal_drivers: &mut Vec<_> = drivers.entry(signal_id).or_default();
        signal_drivers.push((signal_drivers.len(), range));
    };
    for instance in &module.instances {
        let key = LoweredSvModuleKey::instance_key(instance);
        let Some(child_id) = module_ids.get(&key).copied() else {
            continue;
        };
        let Some(child) = modules.get(&child_id) else {
            continue;
        };
        let Ok(authored_connections) =
            expand_structure_connections(&instance.port_connections, &module.source, &child.source)
        else {
            // Attaching the instance reports an invalid connection.
            continue;
        };
        let element_connections = match instance.array_element {
            Some((position, count)) => match array_element_connections(
                &authored_connections,
                child,
                position,
                count,
                &module.variables,
                &module.signal_names,
                &module.constants,
                &module.parameter_types,
            ) {
                Ok(connections) => Some(connections),
                // Building the instance reports the invalid connection.
                Err(_) => continue,
            },
            None => None,
        };
        let connections = element_connections
            .as_deref()
            .unwrap_or(&authored_connections);
        for connection in connections {
            if !child.source.ports().iter().any(|port| {
                port.name() == connection.formal
                    && match port.direction() {
                        sv::ir::PortDirection::Output => true,
                        sv::ir::PortDirection::Inout => {
                            child.inout_driven_inside.contains(port.name())
                        }
                        _ => false,
                    }
            }) {
                continue;
            }
            let Some(actual_expr) = connection.actual_expr.as_ref() else {
                continue;
            };
            let Some(accesses) = output_lvalue_accesses(
                actual_expr,
                &module.variables,
                &module.signal_names,
                &module.constants,
                &module.parameter_types,
            ) else {
                let mut targets = Vec::new();
                output_connection_targets(actual_expr, &mut targets);
                let mut seen = HashSet::default();
                for target in targets {
                    if let Some(&signal_id) = module.signal_names.get(target)
                        && seen.insert(signal_id)
                    {
                        push(signal_id, None);
                    }
                }
                continue;
            };
            for OutputLvalueAccess { signal, access, .. } in accesses {
                push(signal, Some((access.lsb as i128, access.msb as i128)));
            }
        }
    }
    drivers
}

/// Driver ranges of the signals of a module, by signal.
type ChildOutputDrivers = HashMap<SourceVarId, Vec<(usize, Option<(i128, i128)>)>>;

/// The drivers of `signal_name` in `drivers` (see [`child_output_drivers`]).
fn child_output_driver_ranges(
    module: &LoweredSvModule,
    drivers: &ChildOutputDrivers,
    signal_name: &str,
) -> Vec<(usize, Option<(i128, i128)>)> {
    module
        .signal_names
        .get(signal_name)
        .and_then(|signal_id| drivers.get(signal_id))
        .cloned()
        .unwrap_or_default()
}

/// The names an output connection that is not an assignment target drives.
fn output_connection_targets<'a>(expr: &'a sv::ir::Expr, targets: &mut Vec<&'a str>) {
    match expr {
        sv::ir::Expr::Ident(name) => targets.push(name),
        sv::ir::Expr::Select { expr, .. }
        | sv::ir::Expr::Resize { expr, .. }
        | sv::ir::Expr::Unary {
            op: sv::ir::UnaryOp::ToTwoState,
            expr,
        } => output_connection_targets(expr, targets),
        sv::ir::Expr::Concat(parts) => parts
            .iter()
            .for_each(|part| output_connection_targets(part, targets)),
        _ => {}
    }
}

fn validate_net_driver_ranges(
    drivers: &ModuleDrivers,
    signal_name: &str,
    child_driver_ranges: &[(usize, Option<(i128, i128)>)],
    resolved: bool,
) -> Result<(), sv::AnalyzerError> {
    let local_drivers = drivers.local(signal_name);
    let overlapping_local_drivers = local_drivers.iter().enumerate().any(|(index, left)| {
        local_drivers[index + 1..]
            .iter()
            .any(|right| left.0 != right.0 && net_driver_ranges_overlap(left.1, right.1))
    });
    let child_local_overlap = child_driver_ranges.iter().any(|(_, child_range)| {
        local_drivers
            .iter()
            .any(|(_, local_range)| net_driver_ranges_overlap(*child_range, *local_range))
    });
    // Each concatenation operand is a continuous driver, even when two
    // operands occur in the same authored assignment.
    let overlapping_continuous = driver_ranges_overlap(&drivers.continuous(signal_name));
    // The continuous assignments of a resolved net are joined into one
    // definition; a driver below an instance still needs bits of its own.
    if driver_ranges_overlap(child_driver_ranges)
        || child_local_overlap
        || (!resolved && (overlapping_local_drivers || overlapping_continuous))
    {
        return Err(net_resolution::multiple_drivers(signal_name));
    }
    Ok(())
}

/// Reject a package variable that several drivers write, counting every
/// instance of a module that writes it (IEEE 1800-2023 6.5). The checks of
/// one module and its children cannot see writes from other subtrees.
pub(super) fn validate_package_variable_drivers<'a>(
    roots: impl IntoIterator<Item = &'a LoweredSvModuleKey>,
    module_ids: &HashMap<LoweredSvModuleKey, ModuleId>,
    modules: &HashMap<ModuleId, LoweredSvModule>,
) -> Result<(), sv::AnalyzerError> {
    // How many instances of each module the design holds.
    let mut instances: HashMap<ModuleId, usize> = HashMap::default();
    fn count(
        key: &LoweredSvModuleKey,
        multiplicity: usize,
        module_ids: &HashMap<LoweredSvModuleKey, ModuleId>,
        modules: &HashMap<ModuleId, LoweredSvModule>,
        instances: &mut HashMap<ModuleId, usize>,
    ) {
        // A child of another frontend has no package variables here.
        let Some(&module_id) = module_ids.get(key) else {
            return;
        };
        *instances.entry(module_id).or_default() += multiplicity;
        for instance in &modules[&module_id].instances {
            count(
                &LoweredSvModuleKey::instance_key(instance),
                multiplicity,
                module_ids,
                modules,
                instances,
            );
        }
    }
    for root in roots {
        count(root, 1, module_ids, modules, &mut instances);
    }
    // A driver is one process, child output or initial block of one
    // instance; a process may assign the variable several times.
    #[derive(Clone, Copy, PartialEq)]
    enum Source {
        Process,
        Child,
        Initial,
    }
    type Driver = (ModuleId, usize, Source, usize);
    struct Writer {
        driver: Driver,
        range: Option<(i128, i128)>,
        /// Treated as the only writer of what it writes.
        exclusive: bool,
        /// The only writer the standard allows, which an initial block
        /// must not write either.
        restricted: bool,
    }
    let mut drivers: HashMap<(String, String), Vec<Writer>> = HashMap::default();
    for (&module_id, module) in modules {
        let multiplicity = instances.get(&module_id).copied().unwrap_or(0);
        if module.package_variables.is_empty() {
            continue;
        }
        let module_drivers =
            ModuleDrivers::new(&module.source, &module.constants, &module.parameter_types);
        let child_drivers = child_output_drivers(module, module_ids, modules);
        for (var_id, package, variable) in &module.package_variables {
            let name = &module.variables[var_id].path[0];
            let local = module_drivers.local(name);
            let children = child_output_driver_ranges(module, &child_drivers, name);
            // One-shot initial blocks; repeating ones are among `local`.
            let initial = module_drivers
                .initial(name)
                .into_iter()
                .filter(|(driver, _)| !module.source.initial_processes()[*driver].repeats())
                .collect::<Vec<_>>();
            let entry = drivers
                .entry((package.clone(), variable.clone()))
                .or_default();
            for copy in 0..multiplicity {
                for &(driver, range) in &local {
                    entry.push(Writer {
                        driver: (module_id, copy, Source::Process, driver),
                        range,
                        exclusive: exclusive_driver(&module.source, driver),
                        restricted: restricted_driver(&module.source, driver),
                    });
                }
                for &(driver, range) in &children {
                    entry.push(Writer {
                        driver: (module_id, copy, Source::Child, driver),
                        range,
                        exclusive: true,
                        restricted: true,
                    });
                }
                for &(driver, range) in &initial {
                    entry.push(Writer {
                        driver: (module_id, copy, Source::Initial, driver),
                        range,
                        exclusive: false,
                        restricted: false,
                    });
                }
            }
        }
    }
    // Processes of several instances may write a package variable, as any
    // processes may write a variable, unless one of them must be its only
    // writer (IEEE 1800-2023 6.5, 9.2.2); an initial block conflicts only
    // with a writer the standard restricts.
    for ((package, variable), writers) in drivers {
        let conflict = |left: &Writer, right: &Writer| {
            let initial = left.driver.2 == Source::Initial || right.driver.2 == Source::Initial;
            left.driver != right.driver
                && if initial {
                    left.restricted || right.restricted
                } else {
                    left.exclusive || right.exclusive
                }
                && net_driver_ranges_overlap(left.range, right.range)
        };
        let overlap = writers.iter().enumerate().any(|(index, left)| {
            writers[index + 1..]
                .iter()
                .any(|right| conflict(left, right))
        });
        if overlap {
            return Err(sv::AnalyzerError::Unsupported(format!(
                "multiple drivers of package variable `{package}::{variable}`"
            )));
        }
    }
    Ok(())
}

fn driver_ranges_overlap(drivers: &[(usize, Option<(i128, i128)>)]) -> bool {
    drivers.iter().enumerate().any(|(index, left)| {
        drivers[index + 1..]
            .iter()
            .any(|right| net_driver_ranges_overlap(left.1, right.1))
    })
}

/// Check the drivers of the variables that processes of other modules write
/// through hierarchical names as if those processes were the module's own: a
/// continuous or `always_comb` driver excludes every other writer (IEEE
/// 1800-2023 6.5, 9.2.2.2), an `always_ff` one every other process
/// (9.2.2.4), and two repeating processes may only write a shared memory,
/// whose edge writes update selected elements, unless all of them are
/// ordinary `always` processes run as processes, whose writes store into
/// the variable as they execute.
pub(super) fn validate_foreign_drivers(
    module: &sv::ir::Module,
    drivers: &ModuleDrivers,
    shared_memories: &HashSet<String>,
    foreign_writes: &HashMap<String, hierarchy::ForeignWrites>,
) -> Result<(), sv::AnalyzerError> {
    if foreign_writes.is_empty() {
        return Ok(());
    }
    let first_ff = module.comb_processes().len();
    let mut written_once = None;
    let mut always_ff = None;
    for (name, writes) in foreign_writes {
        // Drivers of nets through hierarchical names are rejected with the
        // instance's net drivers.
        if module
            .signals()
            .iter()
            .any(|signal| signal.name() == name && signal.is_net())
        {
            continue;
        }
        let drivers = drivers.local(name);
        let continuous = drivers.iter().any(|(id, _)| *id < first_ff);
        let repeating = writes.repeating + usize::from(!drivers.is_empty());
        let local_always_ff = always_ff
            .get_or_insert_with(|| {
                let mut written = HashSet::default();
                for process in module.ff_processes().iter().filter(|p| p.is_always_ff()) {
                    procedural::written_names_with_calls(module, process.body(), &mut written);
                }
                written
            })
            .contains(name);
        let local_once = written_once
            .get_or_insert_with(|| {
                let mut written = HashSet::default();
                for process in module.initial_processes().iter().filter(|p| !p.repeats()) {
                    procedural::written_names_with_calls(module, process.body(), &mut written);
                }
                written
            })
            .contains(name);
        let reason = if continuous {
            Some("its module drives it continuously or from always_comb")
        } else if writes.always_ff && (!drivers.is_empty() || local_once || writes.repeating > 1) {
            Some("an always_ff process writes it")
        } else if local_always_ff {
            Some("an always_ff process of its module writes it")
        } else if repeating > 1
            && !shared_memories.contains(name)
            && (writes.exclusive
                || drivers
                    .iter()
                    .any(|(driver, _)| !shares_variables(module, *driver)))
        {
            Some("another process writes it too")
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(sv::AnalyzerError::Unsupported(format!(
                "multiple variable drivers for `{name}`, which another module writes through a \
                 hierarchical name: {reason}"
            )));
        }
    }
    Ok(())
}

pub(super) fn validate_variable_driver_ranges(
    module: &sv::ir::Module,
    drivers: &ModuleDrivers,
    shared: &HashSet<String>,
) -> Result<(), sv::AnalyzerError> {
    for port in module
        .ports()
        .iter()
        .filter(|port| port.direction() == sv::ir::PortDirection::Input)
    {
        if !drivers.local(port.name()).is_empty() {
            return Err(sv::AnalyzerError::Unsupported(format!(
                "write to input port `{}`",
                port.name()
            )));
        }
    }

    let variable_names = module
        .signals()
        .iter()
        .filter(|signal| !signal.is_net())
        .map(|signal| signal.name())
        .chain(
            module
                .ports()
                .iter()
                .filter(|port| !port.is_net())
                .map(|port| port.name()),
        );
    for signal_name in variable_names {
        let local = drivers.local(signal_name);
        // Several ordinary `always` processes run as processes may write
        // one variable: each write stores into the variable when it
        // executes, and the last one determines the value (IEEE 1800-2023
        // 6.5). Any other procedure is the only writer of what it writes.
        let exclusive = local
            .iter()
            .map(|(driver, _)| !shares_variables(module, *driver))
            .collect::<Vec<_>>();
        let has_overlap = local.iter().enumerate().any(|(index, left)| {
            local[index + 1..].iter().zip(&exclusive[index + 1..]).any(
                |(right, right_exclusive)| {
                    left.0 != right.0
                        && (exclusive[index] || *right_exclusive)
                        && net_driver_ranges_overlap(left.1, right.1)
                },
            )
        });
        let continuous = drivers.continuous(signal_name);
        let function_storage = module.subroutines().iter().any(|subroutine| {
            subroutine
                .persistent_locals
                .iter()
                .any(|name| name == signal_name)
        });
        // Multiple statements in a function are one procedural driver for
        // its static storage, even when the caller is a continuous assignment.
        let overlapping_continuous = continuous.iter().enumerate().any(|(index, left)| {
            continuous[index + 1..].iter().any(|right| {
                (!function_storage || left.0 != right.0)
                    && net_driver_ranges_overlap(left.1, right.1)
            })
        });
        if overlapping_continuous || (has_overlap && !shared.contains(signal_name)) {
            let mut sites: Vec<String> = local
                .iter()
                .filter_map(|(driver, _)| driver_site(module, *driver))
                .collect();
            sites.sort_unstable();
            sites.dedup();
            let at = if sites.is_empty() {
                String::new()
            } else {
                format!(", written at {}", sites.join(", "))
            };
            return Err(sv::AnalyzerError::Unsupported(format!(
                "multiple variable drivers for `{signal_name}`{at}"
            )));
        }
    }
    Ok(())
}
