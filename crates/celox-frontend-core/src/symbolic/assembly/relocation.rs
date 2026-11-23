//! Relocating the units of every instance into the global design.

mod sampled;

use super::*;
use crate::symbolic::artifact::EdgeEvaluation;
use celox_slt::next_state::{NextStateForm, Publication, lower_transition};

pub(super) type EventEvaluations =
    HashMap<AbsoluteAddr, Vec<EdgeEvaluation<AbsoluteAddr, RegionedAbsoluteAddr>>>;

pub(super) fn elaborated_scope_name(
    root_name: &str,
    path: &InstancePath,
    expanded: &HashMap<InstancePath, InstanceId>,
    indexed_instances: &HashSet<InstanceId>,
) -> String {
    let mut prefix = Vec::with_capacity(path.0.len());
    let segments = path
        .0
        .iter()
        .map(|(name, index)| {
            prefix.push((name.clone(), *index));
            let indexed = expanded
                .get(&InstancePath(prefix.clone()))
                .is_some_and(|id| indexed_instances.contains(id));
            if indexed {
                format!("{name}[{index}]")
            } else {
                name.clone()
            }
        })
        .collect::<Vec<_>>();
    if root_name.is_empty() {
        // Names begin at a top-level module, not at the hidden root that
        // holds several of them (IEEE 1800-2023 23.3.1).
        segments.join(".")
    } else if segments.is_empty() {
        root_name.to_string()
    } else {
        format!("{root_name}.{}", segments.join("."))
    }
}

/// Names the homes of sites ([`celox_design::SiteHome`]) as `%m` prints
/// them: the instance a copied subroutine belongs to, not the one whose
/// code runs it.
pub(super) struct HomeNames<'a> {
    root_name: &'a str,
    expanded: &'a HashMap<InstancePath, InstanceId>,
    indexed_instances: &'a HashSet<InstanceId>,
    homes: &'a celox_design::SiteHomes,
    ports: HashMap<(&'a [(String, usize)], &'a str), &'a (Vec<(String, usize)>, String)>,
}

impl<'a> HomeNames<'a> {
    pub(super) fn new(
        root_name: &'a str,
        expanded: &'a HashMap<InstancePath, InstanceId>,
        indexed_instances: &'a HashSet<InstanceId>,
        homes: &'a celox_design::SiteHomes,
    ) -> Self {
        let ports = homes
            .ports
            .iter()
            .map(|((path, port), actual)| ((path.as_slice(), port.as_str()), actual))
            .collect();
        Self {
            root_name,
            expanded,
            indexed_instances,
            homes,
            ports,
        }
    }

    /// The name of module instance `path` and, within it, of interface
    /// instance `within` (empty for none).
    fn name(&self, path: Vec<(String, usize)>, within: &str) -> String {
        let name = elaborated_scope_name(
            self.root_name,
            &InstancePath(path),
            self.expanded,
            self.indexed_instances,
        );
        match (name.is_empty(), within.is_empty()) {
            (_, true) => name,
            (true, false) => within.to_owned(),
            (false, false) => format!("{name}.{within}"),
        }
    }

    fn instance(&self, id: u32) -> Option<String> {
        let (path, within) = self
            .homes
            .instances
            .get(usize::try_from(id.checked_sub(1)?).ok()?)?;
        Some(self.name(path.clone(), within))
    }

    /// The name of `home` for a site that module instance `holder` holds.
    pub(super) fn resolve(
        &self,
        holder: &InstancePath,
        home: &celox_design::SiteHome,
    ) -> Option<String> {
        match home {
            celox_design::SiteHome::Path { up, down, within } => {
                let kept = match up {
                    Some(up) => holder.0.len().checked_sub(usize::try_from(*up).ok()?)?,
                    None => 0,
                };
                let mut path = holder.0[..kept].to_vec();
                path.extend(down.iter().cloned());
                let within = within.as_deref().unwrap_or("");
                // An interface port names the instance connected to it.
                match self.ports.get(&(path.as_slice(), within)) {
                    Some((actual, handle)) => Some(self.name(actual.clone(), handle)),
                    None => Some(self.name(path, within)),
                }
            }
            celox_design::SiteHome::Instance(id) => self.instance(*id),
        }
    }
}

pub(super) fn flatten_with_trace(
    module: &SimModule,
    path: &InstancePath,
    instance_ids: &HashMap<InstancePath, InstanceId>,
    global_boundaries: &HashMap<AbsoluteAddr, BTreeSet<usize>>,
    unpacked_element_widths: &HashMap<AbsoluteAddr, usize>,
    arena: &mut SLTNodeArena<AbsoluteAddr>,
    bindings: &PackageBindings,
    trace_opts: &FrontendTraceOptions,
    mut trace: Option<&mut FrontendTrace>,
) -> Result<RelocationModule, celox_slt::SLTNodeFactsError> {
    let flattened = flattening::flatten_module(
        module,
        path,
        instance_ids,
        global_boundaries,
        unpacked_element_widths,
        arena,
        &|instance_id, var_id| bindings.locate(instance_id, var_id),
    )?;

    if let Some(trace) = trace.as_deref_mut()
        && trace_opts.pre_atomized_comb_blocks
    {
        match &mut trace.pre_atomized_comb_blocks {
            Some((blocks, trace_arena)) => {
                blocks.extend(flattened.pre_atomized_comb_blocks);
                *trace_arena = arena.clone();
            }
            slot @ None => *slot = Some((flattened.pre_atomized_comb_blocks, arena.clone())),
        }
    }

    if let Some(trace) = trace
        && trace_opts.atomized_comb_blocks
    {
        match &mut trace.atomized_comb_blocks {
            Some((blocks, trace_arena)) => {
                blocks.extend(flattened.relocation.comb_blocks.iter().cloned());
                *trace_arena = arena.clone();
            }
            slot @ None => {
                *slot = Some((flattened.relocation.comb_blocks.clone(), arena.clone()));
            }
        }
    }

    Ok(flattened.relocation)
}

pub(super) fn remap_for_fold_runtime_event_sites<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    start: usize,
    runtime_event_site_map: &HashMap<u32, u32>,
) -> Result<(), ParserError> {
    arena
        .remap_for_fold_effect_sites(start..arena.len(), |site_id, fatal_error_code| {
            Ok(runtime_event_site_map.get(&site_id).map(|&global_site| {
                (
                    global_site,
                    fatal_error_code.map(|_| i64::from(global_site)),
                )
            }))
        })
        .map_err(|error| {
            ParserError::illegal_context(
                "ForFold runtime-event relocation",
                error.to_string(),
                None,
            )
        })
}

pub(super) fn relocate_executation_unit_with_errors<A, B>(
    eu: &ExecutionUnit<A>,
    f: &impl Fn(&A) -> B,
    runtime_error_codes: &HashMap<i64, i64>,
    runtime_event_sites: &HashMap<u32, u32>,
    extern_functions: &HashMap<u32, u32>,
    function_base: u32,
) -> ExecutionUnit<B> {
    ExecutionUnit {
        entry_block_id: eu.entry_block_id,
        blocks: eu
            .blocks
            .iter()
            .map(|(id, block)| {
                (
                    *id,
                    BasicBlock {
                        id: block.id,
                        instructions: block
                            .instructions
                            .iter()
                            .map(|inst| match inst {
                                SIRInstruction::RuntimeEvent { site_id, args } => {
                                    SIRInstruction::RuntimeEvent {
                                        site_id: runtime_event_sites
                                            .get(site_id)
                                            .copied()
                                            .unwrap_or(*site_id),
                                        args: args.clone(),
                                    }
                                }
                                SIRInstruction::CombCaptureEvent {
                                    site_id,
                                    args,
                                    fatal_error_code,
                                    consume_enabled,
                                } => SIRInstruction::CombCaptureEvent {
                                    site_id: runtime_event_sites
                                        .get(site_id)
                                        .copied()
                                        .unwrap_or(*site_id),
                                    args: args.clone(),
                                    fatal_error_code: *fatal_error_code,
                                    consume_enabled: *consume_enabled,
                                },
                                SIRInstruction::ExternCall { dst, func, args } => {
                                    SIRInstruction::ExternCall {
                                        dst: *dst,
                                        // The heap helper is outside every
                                        // module's extern function table.
                                        func: if celox_sir::extern_abi::is_heap_helper(*func) {
                                            *func
                                        } else {
                                            extern_functions[func]
                                        },
                                        args: args.clone(),
                                    }
                                }
                                // A module's units name its functions by their index
                                // in the module; each instance has its own copies,
                                // from `function_base` on.
                                SIRInstruction::Call { .. } => {
                                    let mut call = inst.map_addr(f);
                                    if let SIRInstruction::Call { function, .. } = &mut call {
                                        function.0 += function_base;
                                    }
                                    call
                                }
                                _ => inst.map_addr(f),
                            })
                            .collect(),
                        params: block.params.clone(),
                        terminator: match block.terminator {
                            SIRTerminator::Error(code) => SIRTerminator::Error(
                                runtime_error_codes.get(&code).copied().unwrap_or(code),
                            ),
                            ref terminator => terminator.clone(),
                        },
                    },
                )
            })
            .collect(),
        register_map: eu.register_map.clone(),
    }
}

pub(super) fn relocate_units(
    bindings: &PackageBindings,
    expanded: &HashMap<InstancePath, InstanceId>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    root_name: &str,
    indexed_instances: &HashSet<InstanceId>,
    global_boundaries: &HashMap<AbsoluteAddr, std::collections::BTreeSet<usize>>,
    unpacked_element_widths: &HashMap<AbsoluteAddr, usize>,
    clock_domains: &HashMap<AbsoluteAddr, AbsoluteAddr>,
    parallel_ff_units: Option<
        &mut HashMap<AbsoluteAddr, Vec<(InstanceId, FfPart<RegionedAbsoluteAddr>)>>,
    >,
    split_commits: &mut SplitCommits,
    four_state: bool,
    trace_opts: &crate::FrontendTraceOptions,
    trace: &mut Option<&mut crate::FrontendTrace>,
    startup_ready: Option<AbsoluteAddr>,
    homes: &HomeNames<'_>,
) -> Result<
    (
        SLTNodeArena<AbsoluteAddr>,
        ScheduledUpdates,
        EventEvaluations,
        HashMap<InstanceId, FfRuntimeRelocation>,
        Vec<celox_slt::LogicPath<AbsoluteAddr>>,
        Vec<CombObserver<AbsoluteAddr>>,
        HashMap<i64, RuntimeErrorInfo<AbsoluteAddr>>,
        Vec<RuntimeEventSite>,
        Vec<ExternFunction>,
        i64,
        Vec<RelocatedProcess>,
        (
            Vec<celox_sir::FunctionUnit<RegionedAbsoluteAddr>>,
            Vec<celox_design::FunctionFrame>,
        ),
    ),
    ParserError,
> {
    let mut global_arena = SLTNodeArena::<AbsoluteAddr>::new();
    let mut processes = Vec::new();
    let mut updates = ScheduledUpdates::default();
    let mut evaluations = EventEvaluations::default();
    let mut ff_runtime_relocations = HashMap::default();
    let mut comb_blocks = Vec::new();
    let mut comb_observers = Vec::new();
    let mut runtime_errors = HashMap::default();
    let mut runtime_event_sites = Vec::new();
    let mut extern_functions = Vec::<ExternFunction>::new();
    let mut extern_function_indices = HashMap::<String, usize>::default();
    let mut next_runtime_error_code = 2000;
    let mut functions = Vec::new();
    let mut function_frames = Vec::new();
    let mut parallel_ff_units = parallel_ff_units;
    let mut startup_statuses: Vec<_> = expanded
        .values()
        .flat_map(|id| {
            modules[&instance_modules[id]]
                .processes
                .iter()
                // Only automatically started procedures participate in the
                // time-zero barrier (IEEE 1800-2023 9.2.2.2). Final procedures
                // wait for termination, and callable kernels wait for a caller;
                // neither can release a startup dependency.
                .filter(|process| match process.slots.kind {
                    celox_design::ProcessKind::Initializer | celox_design::ProcessKind::Regular => {
                        true
                    }
                    celox_design::ProcessKind::Final | celox_design::ProcessKind::Callable => false,
                })
                .map(|process| bindings.locate(*id, process.slots.status))
        })
        .collect();
    startup_statuses.sort();
    startup_statuses.dedup();
    let mut gated_startup = false;

    for (path, id) in expanded {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];
        let instance = number_instance(
            path,
            id,
            sim_module,
            bindings,
            clock_domains,
            root_name,
            expanded,
            indexed_instances,
            &mut next_runtime_error_code,
            &mut runtime_errors,
            &mut ff_runtime_relocations,
            &mut extern_function_indices,
            &mut extern_functions,
            &mut runtime_event_sites,
            &functions,
            homes,
        )?;
        let function_base = instance.function_base;
        relocate_functions(&instance, &mut functions);
        // The callees of each copy are the copies in the same instance.
        let rebase = |calls: &[celox_design::CallResume]| {
            calls
                .iter()
                .map(|call| celox_design::CallResume {
                    function: call.function + function_base,
                    ..*call
                })
                .collect::<Vec<_>>()
        };
        function_frames.extend(sim_module.function_frames.iter().map(|frame| {
            celox_design::FunctionFrame {
                calls: rebase(&frame.calls),
                ..frame.clone()
            }
        }));

        // Lane-partitioning units of every trigger group: its independently
        // lowered parts when present, otherwise its whole evaluate/apply pair.
        if let Some(parallel_ff_units) = parallel_ff_units.as_deref_mut() {
            relocate_parallel_ff_parts(
                &instance,
                parallel_ff_units,
                unpacked_element_widths,
                four_state,
            )?;
        }

        relocate_comb_logic(
            &instance,
            path,
            expanded,
            global_boundaries,
            unpacked_element_widths,
            trace_opts,
            trace,
            startup_ready,
            &startup_statuses,
            &mut global_arena,
            &mut comb_blocks,
            &mut comb_observers,
            &mut gated_startup,
        )?;

        // Relocate sequential blocks for this instance
        relocate_event_evaluations(&instance, &mut evaluations, &mut global_arena)?;

        relocate_processes(&instance, &rebase, &mut processes);

        relocate_publications(&instance, split_commits, &mut updates);
    }
    if let Some(ready) = startup_ready.filter(|_| gated_startup) {
        comb_blocks.push(startup_ready_path(
            &mut global_arena,
            &startup_statuses,
            ready,
        )?);
    }
    Ok((
        global_arena,
        updates,
        evaluations,
        ff_runtime_relocations,
        comb_blocks,
        comb_observers,
        runtime_errors,
        runtime_event_sites,
        extern_functions,
        next_runtime_error_code,
        {
            // Instances are numbered in elaboration order, so this is the
            // order processes start in at time zero.
            processes.sort_unstable_by_key(|(key, _)| *key);
            processes.into_iter().map(|(_, process)| process).collect()
        },
        (functions, function_frames),
    ))
}

/// An instance being relocated, with the global numbers of the runtime
/// errors, event sites, extern functions and functions it declares.
struct InstanceRelocation<'a> {
    id: &'a InstanceId,
    sim_module: &'a SimModule,
    bindings: &'a PackageBindings,
    clock_domains: &'a HashMap<AbsoluteAddr, AbsoluteAddr>,
    runtime_error_codes: HashMap<i64, i64>,
    runtime_event_site_map: HashMap<u32, u32>,
    extern_function_map: HashMap<u32, u32>,
    function_base: u32,
    scope: String,
}

/// Place the flip-flop access summaries of instance `id` and give its
/// runtime errors, extern functions, runtime event sites and functions
/// their numbers in the design.
fn number_instance<'a>(
    path: &InstancePath,
    id: &'a InstanceId,
    sim_module: &'a SimModule,
    bindings: &'a PackageBindings,
    clock_domains: &'a HashMap<AbsoluteAddr, AbsoluteAddr>,
    root_name: &str,
    expanded: &HashMap<InstancePath, InstanceId>,
    indexed_instances: &HashSet<InstanceId>,
    next_runtime_error_code: &mut i64,
    runtime_errors: &mut HashMap<i64, RuntimeErrorInfo<AbsoluteAddr>>,
    ff_runtime_relocations: &mut HashMap<InstanceId, FfRuntimeRelocation>,
    extern_function_indices: &mut HashMap<String, usize>,
    extern_functions: &mut Vec<ExternFunction>,
    runtime_event_sites: &mut Vec<RuntimeEventSite>,
    functions: &[celox_sir::FunctionUnit<RegionedAbsoluteAddr>],
    homes: &HomeNames<'_>,
) -> Result<InstanceRelocation<'a>, ParserError> {
    let runtime_event_site_base = u32::try_from(runtime_event_sites.len()).map_err(|_| {
        ParserError::illegal_context(
            "FF runtime-event relocation",
            "runtime event site count exceeds u32",
            None,
        )
    })?;
    let mut runtime_error_codes = HashMap::default();
    for (&local_code, info) in &sim_module.runtime_errors {
        let global_code = *next_runtime_error_code;
        *next_runtime_error_code += 1;
        runtime_error_codes.insert(local_code, global_code);
        runtime_errors.insert(
            global_code,
            RuntimeErrorInfo {
                message: info.message.clone(),
                signals: info
                    .signals
                    .iter()
                    .filter(|var_id| sim_module.variables.contains_key(var_id))
                    .map(|&var_id| bindings.locate(*id, var_id))
                    .collect(),
            },
        );
    }
    ff_runtime_relocations.insert(
        *id,
        FfRuntimeRelocation {
            error_codes: runtime_error_codes.clone(),
            event_site_base: runtime_event_site_base,
        },
    );
    // Extern functions are shared by C symbol name across instances, and
    // every import of one name must declare the same prototype. A name is
    // isolated only if every import of it is.
    let mut extern_function_map = HashMap::default();
    for (local, function) in sim_module.extern_functions.iter().enumerate() {
        let global = match extern_function_indices.get(&function.name).copied() {
            Some(global) if extern_functions[global].signature != function.signature => {
                return Err(ParserError::ExternSignatureMismatch {
                    name: function.name.clone(),
                    first: extern_functions[global].to_string(),
                    second: function.to_string(),
                });
            }
            Some(global) => {
                let shared: &mut celox_design::ExternFunction = &mut extern_functions[global];
                shared.isolated &= function.isolated;
                global
            }
            None => {
                extern_function_indices.insert(function.name.clone(), extern_functions.len());
                extern_functions.push(function.clone());
                extern_functions.len() - 1
            }
        };
        extern_function_map.insert(local as u32, global as u32);
    }
    let mut runtime_event_site_map = HashMap::default();
    let scope = elaborated_scope_name(root_name, path, expanded, indexed_instances);
    let site_base = runtime_event_sites.len() as u32;
    for (local_site, site) in sim_module.runtime_event_sites.iter().enumerate() {
        let global_site = runtime_event_sites.len() as u32;
        runtime_event_site_map.insert(local_site as u32, global_site);
        let mut site = site.clone();
        // A copied subroutine's site names the instance it belongs to.
        site.scope = Some(
            site.home
                .take()
                .and_then(|home| homes.resolve(path, &home))
                .unwrap_or_else(|| scope.clone()),
        );
        // A deferred action names the flush point of its process in the
        // same instance.
        if let Some(celox_design::DeferredSite::Action { queue, .. }) = &mut site.deferred {
            *queue += site_base;
        }
        runtime_event_sites.push(site);
    }
    let function_base = u32::try_from(functions.len()).map_err(|_| {
        ParserError::illegal_context("function relocation", "function count exceeds u32", None)
    })?;
    Ok(InstanceRelocation {
        id,
        sim_module,
        bindings,
        clock_domains,
        runtime_error_codes,
        runtime_event_site_map,
        extern_function_map,
        function_base,
        scope,
    })
}

/// Relocate the functions of the instance; each copy is its own function.
fn relocate_functions(
    instance: &InstanceRelocation<'_>,
    functions: &mut Vec<celox_sir::FunctionUnit<RegionedAbsoluteAddr>>,
) {
    let &InstanceRelocation {
        id,
        sim_module,
        bindings,
        function_base,
        ref runtime_error_codes,
        ref runtime_event_site_map,
        ref extern_function_map,
        ..
    } = instance;
    for function in &sim_module.functions {
        // Frame slots are no state: each copy of a function names its
        // own, unique in the design because the instance is part of the
        // address, and package or hierarchical bindings do not apply.
        let locate = |addr: &RegionedVarAddr| {
            if addr.region == celox_design::FRAME_REGION {
                RegionedAbsoluteAddr {
                    region: addr.region,
                    instance_id: *id,
                    var_id: addr.var_id,
                }
            } else {
                bindings.locate_regioned(addr.region, *id, addr.var_id)
            }
        };
        functions.push(celox_sir::FunctionUnit {
            unit: relocate_executation_unit_with_errors(
                &function.unit,
                &locate,
                runtime_error_codes,
                runtime_event_site_map,
                extern_function_map,
                function_base,
            ),
            frame: celox_sir::FrameLayout {
                size: function.frame.size,
                slots: function
                    .frame
                    .slots
                    .iter()
                    .map(|slot| celox_sir::FrameSlot {
                        addr: locate(&slot.addr),
                        offset: slot.offset,
                        width: slot.width,
                        four_state: slot.four_state,
                        handle: slot.handle,
                        element_width: slot.element_width,
                    })
                    .collect(),
            },
            effects: Default::default(),
        });
    }
}

/// Relocate the lane-partitioning units of every trigger group of the
/// instance into `parallel_ff_units`, by event.
fn relocate_parallel_ff_parts(
    instance: &InstanceRelocation<'_>,
    parallel_ff_units: &mut HashMap<AbsoluteAddr, Vec<(InstanceId, FfPart<RegionedAbsoluteAddr>)>>,
    element_widths: &HashMap<AbsoluteAddr, usize>,
    four_state: bool,
) -> Result<(), ParserError> {
    let &InstanceRelocation {
        id,
        sim_module,
        bindings,
        clock_domains,
        function_base,
        ref runtime_error_codes,
        ref runtime_event_site_map,
        ref extern_function_map,
        ..
    } = instance;
    let relocate = |unit: &ExecutionUnit<RegionedVarAddr>| {
        relocate_executation_unit_with_errors(
            unit,
            &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
            runtime_error_codes,
            runtime_event_site_map,
            extern_function_map,
            function_base,
        )
    };
    let mut trigger_sets = sim_module.event_bodies.keys().collect::<Vec<_>>();
    trigger_sets.sort_unstable();
    for trigger_set in trigger_sets {
        let parts = match sim_module.parallel_ff_parts.get(trigger_set) {
            Some(parts) => parts
                .iter()
                .map(|part| part.map_units(relocate))
                .collect::<Vec<_>>(),
            None => {
                let body = &sim_module.event_bodies[trigger_set];
                let evaluate = if let Some(sample) = &body.sample {
                    relocate(sample)
                } else if let Some(plan) = sim_module.next_state.evaluations.get(trigger_set) {
                    let parts = plan
                        .iter()
                        .map(|batch| match batch {
                            EdgeEvaluation::Procedure(unit) => Ok(relocate(unit)),
                            EdgeEvaluation::Graph(form) => {
                                let mut arena = SLTNodeArena::new();
                                let form = form
                                    .relocate(&sim_module.arena, &mut arena, &|var| {
                                        bindings.locate(*id, *var)
                                    })
                                    .map_err(|e| {
                                        ParserError::illegal_context(
                                            "parallel event",
                                            e.to_string(),
                                            None,
                                        )
                                    })?;
                                lower_transition(
                                    &arena,
                                    &form,
                                    Publication::Deferred,
                                    four_state,
                                    |addr, region| {
                                        RegionedAbsoluteAddr::from_absolute_addr(region, addr)
                                    },
                                    element_widths,
                                )
                                .map_err(|e| {
                                    ParserError::illegal_context("parallel event", e, None)
                                })
                            }
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    if parts.is_empty() {
                        continue;
                    }
                    celox_sir::merge_sir_eus(&parts).0
                } else {
                    continue;
                };
                vec![FfPart {
                    evaluate,
                    apply: relocate(&body.publish),
                }]
            }
        };
        let canonical = |var_id| {
            let address = bindings.locate(*id, var_id);
            clock_domains.get(&address).copied().unwrap_or(address)
        };
        let events = std::iter::once(canonical(trigger_set.clock))
            .chain(trigger_set.resets.iter().map(|&reset| canonical(reset)))
            .collect::<BTreeSet<_>>();
        for event in events {
            parallel_ff_units
                .entry(event)
                .or_default()
                .extend(parts.iter().cloned().map(|part| (*id, part)));
        }
    }
    Ok(())
}

/// Flatten the combinational logic of the instance into the arena of the
/// design and relocate its paths, observers and process units, gating the
/// units that wait for the initial processes.
fn relocate_comb_logic(
    instance: &InstanceRelocation<'_>,
    path: &InstancePath,
    expanded: &HashMap<InstancePath, InstanceId>,
    global_boundaries: &HashMap<AbsoluteAddr, std::collections::BTreeSet<usize>>,
    unpacked_element_widths: &HashMap<AbsoluteAddr, usize>,
    trace_opts: &crate::FrontendTraceOptions,
    trace: &mut Option<&mut crate::FrontendTrace>,
    startup_ready: Option<AbsoluteAddr>,
    startup_statuses: &[AbsoluteAddr],
    global_arena: &mut SLTNodeArena<AbsoluteAddr>,
    comb_blocks: &mut Vec<celox_slt::LogicPath<AbsoluteAddr>>,
    comb_observers: &mut Vec<CombObserver<AbsoluteAddr>>,
    gated_startup: &mut bool,
) -> Result<(), ParserError> {
    let &InstanceRelocation {
        sim_module,
        bindings,
        function_base,
        ref runtime_error_codes,
        ref runtime_event_site_map,
        ref extern_function_map,
        ..
    } = instance;
    let arena_start = global_arena.len();
    let mut relocated_module = flatten_with_trace(
        sim_module,
        path,
        expanded,
        global_boundaries,
        unpacked_element_widths,
        global_arena,
        bindings,
        trace_opts,
        trace.as_deref_mut(),
    )?;
    remap_for_fold_runtime_event_sites(global_arena, arena_start, runtime_event_site_map)?;
    for observer in &mut relocated_module.comb_observers {
        observer.site_id = runtime_event_site_map[&observer.site_id];
        observer.activation_group = runtime_event_site_map[&observer.activation_group];
    }
    for path in &mut relocated_module.comb_blocks {
        if let celox_slt::LogicPathTarget::Process {
            unit,
            after_initials,
            ..
        } = &mut path.target
        {
            if let Some(ready) = startup_ready.filter(|_| *after_initials)
                && !startup_statuses.is_empty()
            {
                path.sources.insert(VarAtomBase::new(ready, 0, 0));
            }
            **unit = relocate_executation_unit_with_errors(
                unit,
                &|addr| *addr,
                runtime_error_codes,
                runtime_event_site_map,
                extern_function_map,
                function_base,
            );
            if let Some(ready) = startup_ready.filter(|_| *after_initials)
                && !startup_statuses.is_empty()
            {
                **unit = after_initial_processes(unit, ready);
                *gated_startup = true;
            }
        }
    }
    merge_same_process_targets(&mut relocated_module.comb_blocks, global_arena)?;
    exclude_written_process_sensitivity(&mut relocated_module.comb_blocks);
    comb_blocks.extend(relocated_module.comb_blocks);
    comb_observers.extend(relocated_module.comb_observers);
    Ok(())
}

/// Merge overlapping or adjacent `LogicPathTarget::Process` entries emitted by
/// the same combinational process that relocate to the same `AbsoluteAddr`, as
/// happens when multiple `ref` ports of one instance alias the same parent
/// variable.
fn merge_same_process_targets(
    comb_blocks: &mut Vec<celox_slt::LogicPath<AbsoluteAddr>>,
    global_arena: &mut SLTNodeArena<AbsoluteAddr>,
) -> Result<(), ParserError> {
    let mut groups: HashMap<(AbsoluteAddr, AbsoluteAddr), Vec<usize>> = HashMap::default();
    for (idx, path) in comb_blocks.iter().enumerate() {
        if let celox_slt::LogicPathTarget::Process {
            process, target, ..
        } = &path.target
        {
            groups.entry((*process, target.id)).or_default().push(idx);
        }
    }
    if !groups.values().any(|indices| indices.len() > 1) {
        return Ok(());
    }
    let mut removed = vec![false; comb_blocks.len()];
    for ((_, target_id), mut indices) in groups {
        if indices.len() <= 1 {
            continue;
        }
        indices.sort_by_key(|&idx| {
            let var = comb_blocks[idx]
                .target
                .var()
                .expect("process target has a var");
            (var.access.lsb, var.access.msb)
        });
        let mut keep_idx: Option<usize> = None;
        for next_idx in indices {
            let next_access = comb_blocks[next_idx]
                .target
                .var()
                .expect("process target has a var")
                .access;
            let Some(k_idx) = keep_idx else {
                keep_idx = Some(next_idx);
                continue;
            };
            let keep_access = comb_blocks[k_idx]
                .target
                .var()
                .expect("process target has a var")
                .access;
            if next_access.lsb.saturating_sub(1) > keep_access.msb {
                keep_idx = Some(next_idx);
                continue;
            }
            let next = comb_blocks[next_idx].clone();
            removed[next_idx] = true;
            let keep = &mut comb_blocks[k_idx];
            keep.sources.extend(next.sources);
            keep.previous_sources.extend(next.previous_sources);
            keep.address_sources.extend(next.address_sources);
            keep.order_before.extend(next.order_before);
            keep.comb_capture_enable_always |= next.comb_capture_enable_always;
            for item in next.local_inputs {
                if !keep.local_inputs.contains(&item) {
                    keep.local_inputs.push(item);
                }
            }
            for site in next.comb_capture_enable_sites {
                if !keep.comb_capture_enable_sites.contains(&site) {
                    keep.comb_capture_enable_sites.push(site);
                }
            }
            for node in next.pre_lower_nodes {
                if !keep.pre_lower_nodes.contains(&node) {
                    keep.pre_lower_nodes.push(node);
                }
            }
            let new_msb = keep_access.msb.max(next_access.msb);
            if new_msb != keep_access.msb
                && let celox_slt::LogicPathTarget::Process { target, .. } = &mut keep.target
            {
                target.access.msb = new_msb;
                let updated_access = target.access;
                keep.expr = global_arena
                    .alloc(celox_slt::SLTNode::Input {
                        variable: target_id,
                        signed: false,
                        index: Vec::new(),
                        access: updated_access,
                    })
                    .map_err(|error| {
                        ParserError::illegal_context(
                            "comb process target merge",
                            error.to_string(),
                            None,
                        )
                    })?;
            }
        }
    }
    let mut idx = 0usize;
    comb_blocks.retain(|_| {
        let keep = !removed[idx];
        idx += 1;
        keep
    });
    Ok(())
}

fn subtract_written_ranges(
    atom: VarAtomBase<AbsoluteAddr>,
    written: &[VarAtomBase<AbsoluteAddr>],
) -> Vec<(usize, usize)> {
    let mut ranges = vec![(atom.access.lsb, atom.access.msb)];
    for target in written.iter().filter(|target| target.id == atom.id) {
        ranges = ranges
            .into_iter()
            .flat_map(|(lsb, msb)| {
                if target.access.msb < lsb || target.access.lsb > msb {
                    return vec![(lsb, msb)];
                }
                let mut kept = Vec::new();
                if lsb < target.access.lsb {
                    kept.push((lsb, target.access.lsb - 1));
                }
                if target.access.msb < msb {
                    kept.push((target.access.msb + 1, msb));
                }
                kept
            })
            .collect();
    }
    ranges
}

/// Exclude storage written by a combinational process from its own relocated
/// sources, sensitivity counters, and activation guard snapshots.
///
/// Frontends subtract written formals before elaboration, which misses sibling
/// `ref` ports (or package/hierarchical aliases) that relocate to the same
/// physical variable in a given instance.
pub(super) fn exclude_written_process_sensitivity(
    comb_blocks: &mut [celox_slt::LogicPath<AbsoluteAddr>],
) {
    let mut groups: HashMap<AbsoluteAddr, Vec<usize>> = HashMap::default();
    for (idx, path) in comb_blocks.iter().enumerate() {
        if let celox_slt::LogicPathTarget::Process { process, .. } = &path.target {
            groups.entry(*process).or_default().push(idx);
        }
    }
    for (process_id, indices) in groups {
        let mut written_targets = Vec::new();
        for &idx in &indices {
            if let celox_slt::LogicPathTarget::Process { target, .. } = &comb_blocks[idx].target
                && target.id != process_id
            {
                written_targets.push(*target);
            }
        }
        if written_targets.is_empty() {
            continue;
        }
        for &idx in &indices {
            let path = &mut comb_blocks[idx];
            if path.sources.iter().any(|source| {
                written_targets
                    .iter()
                    .any(|target| target.id == source.id && target.access.overlaps(&source.access))
            }) {
                let old_sources = std::mem::take(&mut path.sources);
                for source in old_sources {
                    for (lsb, msb) in subtract_written_ranges(source, &written_targets) {
                        path.sources.insert(VarAtomBase::new(source.id, lsb, msb));
                    }
                }
            }
        }
        let guard_updates: HashMap<AbsoluteAddr, (VarAtomBase<AbsoluteAddr>, Vec<(usize, usize)>)> = {
            let celox_slt::LogicPathTarget::Process {
                written_excluded_inputs,
                ..
            } = &comb_blocks[indices[0]].target
            else {
                unreachable!()
            };
            written_excluded_inputs
                .iter()
                .filter(|&&(source, _)| {
                    written_targets.iter().any(|target| {
                        target.id == source.id && target.access.overlaps(&source.access)
                    })
                })
                .map(|&(source, saved)| {
                    (
                        saved,
                        (source, subtract_written_ranges(source, &written_targets)),
                    )
                })
                .collect()
        };
        if guard_updates.is_empty() {
            continue;
        }
        let rewritten_unit = {
            let celox_slt::LogicPathTarget::Process { unit, .. } =
                &mut comb_blocks[indices[0]].target
            else {
                unreachable!()
            };
            narrow_comb_process_guard(unit, &guard_updates);
            unit.clone()
        };
        for &idx in &indices {
            let celox_slt::LogicPathTarget::Process {
                sensitivity_counters,
                written_excluded_inputs,
                unit,
                ..
            } = &mut comb_blocks[idx].target
            else {
                unreachable!()
            };
            if idx != indices[0] {
                *unit = rewritten_unit.clone();
            }
            let old_counters = std::mem::take(sensitivity_counters);
            for (source, counter) in old_counters {
                for (lsb, msb) in subtract_written_ranges(source, &written_targets) {
                    sensitivity_counters.push((VarAtomBase::new(source.id, lsb, msb), counter));
                }
            }
            let old_inputs = std::mem::take(written_excluded_inputs);
            for (source, saved) in old_inputs {
                for (lsb, msb) in subtract_written_ranges(source, &written_targets) {
                    written_excluded_inputs.push((VarAtomBase::new(source.id, lsb, msb), saved));
                }
            }
        }
    }
}

fn narrow_comb_process_guard(
    unit: &mut ExecutionUnit<AbsoluteAddr>,
    guard_updates: &HashMap<AbsoluteAddr, (VarAtomBase<AbsoluteAddr>, Vec<(usize, usize)>)>,
) {
    use celox_sir::{BinaryOp, RegisterId, RegisterType, SIRValue};

    let mut saved_loads: HashMap<RegisterId, (VarAtomBase<AbsoluteAddr>, Vec<(usize, usize)>)> =
        HashMap::default();
    for block in unit.blocks.values() {
        for inst in &block.instructions {
            if let SIRInstruction::Load(dst, addr, ..) = inst
                && let Some(update) = guard_updates.get(addr)
            {
                saved_loads.insert(*dst, update.clone());
            }
        }
    }

    let mut next_reg = unit
        .register_map
        .keys()
        .map(|reg| reg.0 + 1)
        .max()
        .unwrap_or(0);
    let mut alloc_reg = |register_map: &mut HashMap<RegisterId, RegisterType>, ty: RegisterType| {
        let reg = RegisterId(next_reg);
        next_reg += 1;
        register_map.insert(reg, ty);
        reg
    };
    let mut dropped_source_regs = HashSet::default();
    let mut block_ids: Vec<_> = unit.blocks.keys().copied().collect();
    block_ids.sort_unstable();
    for block_id in block_ids {
        let block = unit
            .blocks
            .get_mut(&block_id)
            .expect("block id collected from unit");
        let old_insts = std::mem::take(&mut block.instructions);
        let mut new_insts = Vec::with_capacity(old_insts.len());
        for inst in old_insts {
            match inst {
                SIRInstruction::Load(_, addr, ..)
                    if guard_updates
                        .get(&addr)
                        .is_some_and(|(_, kept)| kept.is_empty()) => {}
                SIRInstruction::Store(addr, _, _, src_reg, ..)
                    if guard_updates
                        .get(&addr)
                        .is_some_and(|(_, kept)| kept.is_empty()) =>
                {
                    dropped_source_regs.insert(src_reg);
                }
                SIRInstruction::Binary(differs, lhs, BinaryOp::NeCase, rhs)
                    if saved_loads.contains_key(&rhs) || saved_loads.contains_key(&lhs) =>
                {
                    let (current, previous, (orig_source, kept)) =
                        if let Some(entry) = saved_loads.get(&rhs) {
                            (lhs, rhs, entry)
                        } else {
                            (rhs, lhs, &saved_loads[&lhs])
                        };
                    if kept.is_empty() {
                        dropped_source_regs.insert(current);
                        new_insts.push(SIRInstruction::Imm(differs, SIRValue::new(0u8)));
                    } else {
                        let cur_is_logic = matches!(
                            unit.register_map.get(&current),
                            Some(RegisterType::Logic { .. })
                        );
                        let prev_is_logic = matches!(
                            unit.register_map.get(&previous),
                            Some(RegisterType::Logic { .. })
                        );
                        let part_ty = |is_logic: bool, sub_w: usize| {
                            if is_logic {
                                RegisterType::Logic { width: sub_w }
                            } else {
                                RegisterType::Bit {
                                    width: sub_w,
                                    signed: false,
                                }
                            }
                        };
                        let mut acc: Option<RegisterId> = None;
                        for (idx, &(k_lsb, k_msb)) in kept.iter().enumerate() {
                            let sub_w = k_msb - k_lsb + 1;
                            let cur_part =
                                alloc_reg(&mut unit.register_map, part_ty(cur_is_logic, sub_w));
                            let prev_part =
                                alloc_reg(&mut unit.register_map, part_ty(prev_is_logic, sub_w));
                            let rel_lsb = k_lsb - orig_source.access.lsb;
                            new_insts
                                .push(SIRInstruction::Slice(cur_part, current, rel_lsb, sub_w));
                            new_insts
                                .push(SIRInstruction::Slice(prev_part, previous, rel_lsb, sub_w));
                            let is_last = idx + 1 == kept.len();
                            match acc {
                                None if is_last => {
                                    new_insts.push(SIRInstruction::Binary(
                                        differs,
                                        cur_part,
                                        BinaryOp::NeCase,
                                        prev_part,
                                    ));
                                }
                                None => {
                                    let part_diff = alloc_reg(
                                        &mut unit.register_map,
                                        RegisterType::Bit {
                                            width: 1,
                                            signed: false,
                                        },
                                    );
                                    new_insts.push(SIRInstruction::Binary(
                                        part_diff,
                                        cur_part,
                                        BinaryOp::NeCase,
                                        prev_part,
                                    ));
                                    acc = Some(part_diff);
                                }
                                Some(prev_acc) => {
                                    let part_diff = alloc_reg(
                                        &mut unit.register_map,
                                        RegisterType::Bit {
                                            width: 1,
                                            signed: false,
                                        },
                                    );
                                    new_insts.push(SIRInstruction::Binary(
                                        part_diff,
                                        cur_part,
                                        BinaryOp::NeCase,
                                        prev_part,
                                    ));
                                    let dst = if is_last {
                                        differs
                                    } else {
                                        alloc_reg(
                                            &mut unit.register_map,
                                            RegisterType::Bit {
                                                width: 1,
                                                signed: false,
                                            },
                                        )
                                    };
                                    new_insts.push(SIRInstruction::Binary(
                                        dst,
                                        prev_acc,
                                        BinaryOp::Or,
                                        part_diff,
                                    ));
                                    acc = Some(dst);
                                }
                            }
                        }
                    }
                }
                other => new_insts.push(other),
            }
        }
        block.instructions = new_insts;
    }

    if !dropped_source_regs.is_empty() {
        let mut used_regs = HashSet::default();
        let mark_offset = |used: &mut HashSet<RegisterId>, offset: &SIROffset| {
            for reg in offset.dynamic_registers().into_iter().flatten() {
                used.insert(reg);
            }
        };
        for block in unit.blocks.values() {
            for inst in &block.instructions {
                match inst {
                    SIRInstruction::Imm(..) => {}
                    SIRInstruction::Binary(_, lhs, _, rhs) => {
                        used_regs.insert(*lhs);
                        used_regs.insert(*rhs);
                    }
                    SIRInstruction::Unary(_, _, src) | SIRInstruction::Slice(_, src, ..) => {
                        used_regs.insert(*src);
                    }
                    SIRInstruction::Load(_, _, offset, _)
                    | SIRInstruction::Commit(_, _, offset, _, _) => {
                        mark_offset(&mut used_regs, offset);
                    }
                    SIRInstruction::Store(_, offset, _, src, _, _) => {
                        mark_offset(&mut used_regs, offset);
                        used_regs.insert(*src);
                    }
                    SIRInstruction::Concat(_, srcs)
                    | SIRInstruction::RuntimeEvent { args: srcs, .. }
                    | SIRInstruction::CombCaptureEvent { args: srcs, .. }
                    | SIRInstruction::ExternCall { args: srcs, .. } => {
                        used_regs.extend(srcs.iter().copied());
                    }
                    SIRInstruction::Mux(_, cond, then_val, else_val) => {
                        used_regs.insert(*cond);
                        used_regs.insert(*then_val);
                        used_regs.insert(*else_val);
                    }
                    SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
                        used_regs.insert(*old);
                        used_regs.insert(*new);
                    }
                    SIRInstruction::Call { frame, args, .. } => {
                        used_regs.insert(*frame);
                        used_regs.extend(args.iter().map(|(_, reg)| *reg));
                    }
                }
            }
            match &block.terminator {
                SIRTerminator::Jump(_, args) => {
                    used_regs.extend(args.iter().copied());
                }
                SIRTerminator::Branch {
                    cond,
                    true_block,
                    false_block,
                } => {
                    used_regs.insert(*cond);
                    used_regs.extend(true_block.1.iter().copied());
                    used_regs.extend(false_block.1.iter().copied());
                }
                SIRTerminator::Switch { selector, .. } => {
                    used_regs.insert(*selector);
                }
                SIRTerminator::Return | SIRTerminator::Error(_) => {}
            }
        }
        for block in unit.blocks.values_mut() {
            block.instructions.retain(|inst| match inst {
                SIRInstruction::Load(dst, ..) => {
                    !(dropped_source_regs.contains(dst) && !used_regs.contains(dst))
                }
                _ => true,
            });
        }
    }
}

/// Relocate the flip-flop units of the instance that evaluate, with or
/// without applying, under the canonical event of every trigger.
fn relocate_event_evaluations(
    instance: &InstanceRelocation<'_>,
    evaluations: &mut EventEvaluations,
    arena: &mut SLTNodeArena<AbsoluteAddr>,
) -> Result<(), ParserError> {
    let &InstanceRelocation {
        id,
        sim_module,
        bindings,
        clock_domains,
        function_base,
        ref runtime_error_codes,
        ref runtime_event_site_map,
        ref extern_function_map,
        ..
    } = instance;
    let relocate = |unit: &ExecutionUnit<RegionedVarAddr>| {
        relocate_executation_unit_with_errors(
            unit,
            &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
            runtime_error_codes,
            runtime_event_site_map,
            extern_function_map,
            function_base,
        )
    };
    let canonical = |signal| {
        let address = bindings.locate(*id, signal);
        clock_domains.get(&address).copied().unwrap_or(address)
    };
    let mut record = |trigger: &celox_design::TriggerSet<SourceVarId>, plan: Vec<_>| {
        for reset in &trigger.resets {
            evaluations
                .entry(canonical(*reset))
                .or_default()
                .extend(plan.iter().cloned());
        }
        evaluations
            .entry(canonical(trigger.clock))
            .or_default()
            .extend(plan);
    };
    for (trigger, batches) in &sim_module.next_state.evaluations {
        let plan = batches
            .iter()
            .map(|batch| match batch {
                EdgeEvaluation::Graph(form) => form
                    .relocate(&sim_module.arena, arena, &|var| bindings.locate(*id, *var))
                    .map(EdgeEvaluation::Graph)
                    .map_err(|e| {
                        ParserError::illegal_context("clock evaluation", e.to_string(), None)
                    }),
                EdgeEvaluation::Procedure(unit) => Ok(EdgeEvaluation::Procedure(relocate(unit))),
            })
            .collect::<Result<Vec<_>, _>>()?;
        record(trigger, plan);
    }
    for (trigger, body) in &sim_module.event_bodies {
        let Some(unit) = &body.sample else { continue };
        if sim_module.next_state.evaluations.contains_key(trigger) {
            continue;
        }
        record(trigger, vec![EdgeEvaluation::Procedure(relocate(unit))]);
    }
    Ok(())
}

/// Graphs can share a sampling pass across a procedure when delaying their
/// staging until after the procedure preserves every read and write.
fn can_delay_samples(
    procedure: &ExecutionUnit<RegionedAbsoluteAddr>,
    reads: &HashSet<RegionedAbsoluteAddr>,
    writes: &HashSet<RegionedAbsoluteAddr>,
) -> bool {
    procedure.blocks.values().all(|block| {
        !matches!(block.terminator, SIRTerminator::Error(_))
            && block.instructions.iter().all(|inst| {
                !inst.may_fail()
                    && !inst.is_host_interaction()
                    && inst.state_writes().objects().is_some_and(|mut addresses| {
                        addresses.all(|addr| !reads.contains(addr) && !writes.contains(addr))
                    })
                    && inst
                        .state_reads()
                        .objects()
                        .is_some_and(|mut addresses| addresses.all(|addr| !writes.contains(addr)))
            })
    })
}

/// Separate the destinations whose operands a procedure changes or whose
/// staging slots it touches. All ports of one array travel together in their
/// original order, since two dynamic indices may denote the same word.
fn take_conflicting_samples(
    form: &mut NextStateForm<AbsoluteAddr>,
    arena: &SLTNodeArena<AbsoluteAddr>,
    procedure: &ExecutionUnit<RegionedAbsoluteAddr>,
    movable: bool,
) -> NextStateForm<AbsoluteAddr> {
    let mut changed = HashSet::default();
    let mut touched = HashSet::default();
    for block in procedure.blocks.values() {
        if !movable || matches!(block.terminator, SIRTerminator::Error(_)) {
            return std::mem::take(form);
        }
        for inst in &block.instructions {
            if inst.may_fail() || inst.is_host_interaction() {
                return std::mem::take(form);
            }
            let (Some(reads), Some(writes)) =
                (inst.state_reads().objects(), inst.state_writes().objects())
            else {
                return std::mem::take(form);
            };
            touched.extend(reads.copied());
            for addr in writes {
                touched.insert(*addr);
                if addr.region == STABLE_REGION {
                    changed.insert(addr.absolute_addr());
                }
            }
        }
    }
    let mut early = HashSet::default();
    let mut check = |target, region, nodes: &[NodeId]| {
        let slot = RegionedAbsoluteAddr::from_absolute_addr(region, target);
        let reads_changed = |node| match arena.get(node) {
            celox_slt::SLTNode::Input { variable, .. } => changed.contains(variable),
            _ => false,
        };
        if touched.contains(&slot)
            || (!changed.is_empty()
                && celox_slt::canon::reachable(arena, nodes.iter().copied())
                    .into_iter()
                    .any(reads_changed))
        {
            early.insert(target);
        }
    };
    for r in &form.roots {
        if let Some(enable) = &r.enable {
            check(
                r.target,
                WORKING_REGION,
                &[r.value, enable.condition, enable.data],
            );
        } else {
            check(r.target, WORKING_REGION, &[r.value]);
        }
    }
    for p in &form.ports {
        check(
            p.array,
            celox_design::SPARSE_WORKING_REGION,
            &[p.guard, p.index, p.value],
        );
    }
    let early_banks: HashSet<_> = form
        .roots
        .iter()
        .filter(|r| early.contains(&r.target))
        .filter_map(|r| r.enable.as_ref().map(|e| e.slot))
        .collect();
    early.extend(
        form.roots
            .iter()
            .filter(|r| {
                r.enable
                    .as_ref()
                    .is_some_and(|e| early_banks.contains(&e.slot))
            })
            .map(|r| r.target),
    );
    let (roots, late) = std::mem::take(&mut form.roots)
        .into_iter()
        .partition(|r| early.contains(&r.target));
    form.roots = late;
    let (ports, late) = std::mem::take(&mut form.ports)
        .into_iter()
        .partition(|p| early.contains(&p.array));
    form.ports = late;
    NextStateForm { roots, ports }
}

/// Coalesce symbolic samples across instances until a procedure needs a staged
/// write or may invalidate a sampled input. Publication keeps its original order.
pub(super) fn lower_event_evaluations(
    events: EventEvaluations,
    arena: &SLTNodeArena<AbsoluteAddr>,
    elements: &HashMap<AbsoluteAddr, usize>,
    updates: &mut ScheduledUpdates,
    direct_events: &HashSet<AbsoluteAddr>,
    four_state: bool,
) -> Result<(), ParserError> {
    let lower = |form: &NextStateForm<_>, publication| {
        lower_transition(
            arena,
            form,
            publication,
            four_state,
            |addr, region| RegionedAbsoluteAddr::from_absolute_addr(region, addr),
            elements,
        )
        .map_err(|e| ParserError::illegal_context("clock evaluation", e, None))
    };
    for (event, evaluations) in events {
        let update = updates.events.entry(event).or_default();
        let mut units = Vec::new();
        let mut pending = NextStateForm::default();
        let mut reads = HashSet::default();
        let mut writes = HashSet::default();
        let mut roots = 0;
        let mut ports = 0;
        let mut procedures = 0;
        let mixed = evaluations
            .iter()
            .any(|e| matches!(e, EdgeEvaluation::Procedure(_)));
        let mut movable = true;
        for evaluation in evaluations {
            match evaluation {
                EdgeEvaluation::Graph(form) => {
                    if mixed {
                        for node in celox_slt::canon::reachable(arena, form.nodes()) {
                            use celox_slt::SLTNode;
                            match arena.get(node) {
                                SLTNode::Input { variable, .. } => {
                                    reads.insert(RegionedAbsoluteAddr::from_absolute_addr(
                                        STABLE_REGION,
                                        *variable,
                                    ));
                                }
                                SLTNode::ForFold { .. }
                                | SLTNode::ForFoldGroup { .. }
                                | SLTNode::Capture { .. } => movable = false,
                                SLTNode::Constant(..)
                                | SLTNode::Binary(..)
                                | SLTNode::Unary(..)
                                | SLTNode::Mux { .. }
                                | SLTNode::Concat(..)
                                | SLTNode::Slice { .. } => {}
                            }
                        }
                        writes.extend(form.roots.iter().map(|root| {
                            RegionedAbsoluteAddr::from_absolute_addr(WORKING_REGION, root.target)
                        }));
                        writes.extend(form.ports.iter().map(|port| {
                            RegionedAbsoluteAddr::from_absolute_addr(
                                celox_design::SPARSE_WORKING_REGION,
                                port.array,
                            )
                        }));
                    }
                    roots += form.roots.len();
                    ports += form.ports.len();
                    pending.roots.extend(form.roots);
                    pending.ports.extend(form.ports);
                }
                EdgeEvaluation::Procedure(unit) => {
                    if !writes.is_empty()
                        && (!movable || !can_delay_samples(&unit, &reads, &writes))
                    {
                        let early = take_conflicting_samples(&mut pending, arena, &unit, movable);
                        if !early.roots.is_empty() || !early.ports.is_empty() {
                            units.push(lower(&early, Publication::Deferred)?);
                        }
                        // The fast disjointness check may retain a superset
                        // until the last pending destination has been emitted.
                        if pending.roots.is_empty() && pending.ports.is_empty() {
                            reads.clear();
                            writes.clear();
                            movable = true;
                        }
                    }
                    procedures += 1;
                    units.push(unit);
                }
            }
        }
        let mut direct_unit = None;
        if !pending.roots.is_empty() || !pending.ports.is_empty() {
            if direct_events.contains(&event) {
                direct_unit = Some(lower(&pending, Publication::Direct)?);
            } else if !four_state
                && let Some(applications) = update.entry(UpdateEntry::Publish)
                && let Some(direct) =
                    sampled::combined(arena, &pending, elements, &units, applications)?
            {
                direct_unit = Some(direct);
            }
            units.push(lower(&pending, Publication::Deferred)?);
        }
        if let Some(direct) = direct_unit {
            update.set(UpdateEntry::Transition, vec![direct]);
        } else {
            let publications = update.entry(UpdateEntry::Publish).ok_or_else(|| {
                ParserError::illegal_context(
                    "event assembly",
                    "event has no publication phase",
                    None,
                )
            })?;
            // An unconditional error or nonreturning procedure ends the event.
            // Only returning samples can reach a later sample or publication.
            let mut phases = Vec::new();
            for unit in units.iter().chain(publications) {
                phases.push(unit);
                if !unit
                    .blocks
                    .values()
                    .any(|b| matches!(b.terminator, celox_sir::SIRTerminator::Return))
                {
                    break;
                }
            }
            update.set(
                UpdateEntry::Transition,
                vec![celox_sir::merge_sir_eu_refs(&phases).0],
            );
        }
        tracing::debug!(target: "celox::edge_graph", ?event, roots, ports, procedures, graphs = units.len()-procedures, "scheduled edge evaluations");
        update.set(UpdateEntry::Sample, units);
    }
    Ok(())
}

/// Relocate the process kernels of the instance, keyed by the instance and
/// their index in its module.
fn relocate_processes(
    instance: &InstanceRelocation<'_>,
    rebase: &impl Fn(&[celox_design::CallResume]) -> Vec<celox_design::CallResume>,
    processes: &mut Vec<((InstanceId, usize), RelocatedProcess)>,
) {
    let &InstanceRelocation {
        id,
        sim_module,
        bindings,
        function_base,
        ref scope,
        ref runtime_error_codes,
        ref runtime_event_site_map,
        ref extern_function_map,
        ..
    } = instance;
    for (index, process) in sim_module.processes.iter().enumerate() {
        processes.push((
            (*id, index),
            RelocatedProcess {
                kernel: relocate_executation_unit_with_errors(
                    &process.kernel,
                    &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                    runtime_error_codes,
                    runtime_event_site_map,
                    extern_function_map,
                    function_base,
                ),
                slots: process.slots.map(|var_id| bindings.locate(*id, var_id)),
                frame: process
                    .frame
                    .clone()
                    .map(|frame| frame.map(|var_id| bindings.locate(*id, var_id))),
                origin: celox_design::ProcessOrigin {
                    scope: Some(scope.clone()),
                    ..process.origin.clone()
                },
                call_resumes: rebase(&process.call_resumes),
                key: (*id, index),
            },
        ));
    }
}

/// Relocate the flip-flop units of the instance that apply, and their
/// split commits, under the canonical event of every trigger.
fn relocate_publications(
    instance: &InstanceRelocation<'_>,
    split_commits: &mut SplitCommits,
    updates: &mut ScheduledUpdates,
) {
    let &InstanceRelocation {
        id,
        sim_module,
        bindings,
        clock_domains,
        function_base,
        ref runtime_error_codes,
        ref runtime_event_site_map,
        ref extern_function_map,
        ..
    } = instance;
    for (trigger_set, body) in &sim_module.event_bodies {
        let eu = &body.publish;
        let clock_addr = bindings.locate(*id, trigger_set.clock);
        let canonical_addr = clock_domains
            .get(&clock_addr)
            .copied()
            .unwrap_or(clock_addr);
        // The split commit of each apply unit, in the same order.
        let split = sim_module
            .split_commit_ff_blocks
            .get(trigger_set)
            .map(|split| SplitCommit {
                active: relocate_executation_unit_with_errors(
                    &split.active,
                    &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                    runtime_error_codes,
                    runtime_event_site_map,
                    extern_function_map,
                    function_base,
                ),
                nba: relocate_executation_unit_with_errors(
                    &split.nba,
                    &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                    runtime_error_codes,
                    runtime_event_site_map,
                    extern_function_map,
                    function_base,
                ),
            });
        // Pushed like the apply units below: the clock, then each reset.
        split_commits
            .entry(canonical_addr)
            .or_default()
            .push(split.clone());
        for &reset in &trigger_set.resets {
            let reset_addr = bindings.locate(*id, reset);
            let canonical_addr = clock_domains
                .get(&reset_addr)
                .copied()
                .unwrap_or(reset_addr);
            split_commits
                .entry(canonical_addr)
                .or_default()
                .push(split.clone());
        }
        updates
            .events
            .entry(canonical_addr)
            .or_default()
            .entry_or_default(UpdateEntry::Publish)
            .push(relocate_executation_unit_with_errors(
                eu,
                &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                runtime_error_codes,
                runtime_event_site_map,
                extern_function_map,
                function_base,
            ));

        for &reset in &trigger_set.resets {
            let reset_addr = bindings.locate(*id, reset);
            let canonical_addr = clock_domains
                .get(&reset_addr)
                .copied()
                .unwrap_or(reset_addr);
            updates
                .events
                .entry(canonical_addr)
                .or_default()
                .entry_or_default(UpdateEntry::Publish)
                .push(relocate_executation_unit_with_errors(
                    eu,
                    &|addr| bindings.locate_regioned(addr.region, *id, addr.var_id),
                    runtime_error_codes,
                    runtime_event_site_map,
                    extern_function_map,
                    function_base,
                ));
        }
    }
}

/// A process kernel placed in one instance.
pub(super) struct RelocatedProcess {
    pub(super) kernel: ExecutionUnit<RegionedAbsoluteAddr>,
    pub(super) slots: ProcessSlots<AbsoluteAddr>,
    pub(super) frame: Option<celox_design::ProcessFrame<AbsoluteAddr>>,
    pub(super) origin: celox_design::ProcessOrigin,
    /// The calls at which the kernel can be suspended inside the callee.
    pub(super) call_resumes: Vec<celox_design::CallResume>,
    /// The instance and the module-local index of the process.
    pub(super) key: (InstanceId, usize),
}
