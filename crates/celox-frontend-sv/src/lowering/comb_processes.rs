//! Lowering of the combinational processes of a module.

use super::*;

pub(super) fn lower_comb_processes(
    module: &sv::ir::Module,
    tables: &procedural::ModuleTables,
    port_assignments: &[(SourceVarId, Vec<sv::ir::Stmt>)],
    variables: &mut HashMap<SourceVarId, SvVariable>,
    name_to_id: &mut HashMap<String, SourceVarId>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    four_state: bool,
    allow_inferred_latches: bool,
    runtime_errors: &mut HashMap<i64, RuntimeErrorInfo<SourceVarId>>,
    extern_functions: &mut Vec<ExternFunction>,
    warnings: &mut Vec<crate::warning::PendingWarning>,
    waits: bool,
    watch_seeds: Option<&HashSet<SourceVarId>>,
    gated_waits: bool,
    calls: Option<&mut recursion::CallGraph>,
) -> Result<
    (
        Vec<LogicPath<SourceVarId>>,
        SLTNodeArena<SourceVarId>,
        Vec<SourceVarId>,
        Vec<CombObserver<SourceVarId>>,
        Vec<RuntimeEventSite>,
        Vec<CombAlternative>,
    ),
    sv::AnalyzerError,
> {
    let mut arena = SLTNodeArena::new();
    let mut comb_blocks = Vec::new();
    let mut alternatives = Vec::new();
    let mut observers = Vec::new();
    let mut sites = Vec::new();
    let mut pm = procedural::ProcModule::new(
        module,
        tables,
        variables,
        name_to_id,
        constants,
        parameter_types,
        four_state,
        warnings,
    );
    pm.calls = calls;
    pm.runtime.errors = std::mem::take(runtime_errors);
    pm.externs.functions = std::mem::take(extern_functions);
    let random_seeds = process_random_seeds(module);
    let redundant = redundant_net_drivers(module);
    let watchable =
        watch_seeds.map(|seeds| watchable_variables(&pm, module, port_assignments, seeds));
    // In two-state mode an `always @*` runs once at time zero like an
    // `always_comb` (#2021), and its sensitivity is its implicit reads. One
    // whose written variables have no other driver therefore defines the same
    // function as an `always_comb`: it gets the symbolic form, and the guarded
    // unit only when the symbolic lowering refuses it.
    let mut drivers = None;
    let mut symbolic_star = |index: usize, process: &sv::ir::CombProcess| {
        if four_state || process.kind() != sv::ir::CombProcessKind::AlwaysStar {
            return false;
        }
        let drivers =
            drivers.get_or_insert_with(|| ModuleDrivers::new(module, constants, parameter_types));
        let mut names = HashSet::default();
        procedural::written_names_with_calls(module, process.body(), &mut names);
        // Another process or a continuous assignment may drive other bits of
        // the same variable, as of any symbolic definition. A variable an
        // `initial` block or declaration initializer writes keeps the guarded
        // unit: its time-zero run changes that value, which observers and
        // waiting processes see as a change.
        names.iter().all(|name| {
            drivers.writes.get(name.as_str()).is_none_or(|writes| {
                if writes.iter().any(|write| write.initial.is_some()) {
                    return false;
                }
                let bits = |write: &DriverWrite| write.range.or(write.element);
                let (own, others): (Vec<_>, Vec<_>) =
                    writes.iter().partition(|write| write.local == Some(index));
                others.iter().all(|other| {
                    (other.local.is_none() && other.continuous.is_none())
                        || own
                            .iter()
                            .all(|mine| !net_driver_ranges_overlap(bits(mine), bits(other)))
                })
            })
        })
    };
    // The nets that several continuous assignments drive on common bits get
    // one definition, the resolution of their drivers.
    let resolved_nets = if continuous_targets_repeat(module) {
        net_resolution::ResolvedNets::of(
            module,
            &ModuleDrivers::new(module, constants, parameter_types),
            four_state,
        )
    } else {
        net_resolution::ResolvedNets::default()
    };
    let resolved_ids: HashMap<SourceVarId, &str> = resolved_nets
        .names()
        .filter_map(|name| Some((pm.id(name)?, name)))
        .collect();
    let mut net_drivers: HashMap<SourceVarId, Vec<LogicPath<SourceVarId>>> = HashMap::default();
    let driver_error = |process: &sv::ir::CombProcess| {
        let mut names = HashSet::default();
        procedural::written_names_with_calls(module, process.body(), &mut names);
        let mut names: Vec<_> = names
            .into_iter()
            .filter(|name| resolved_nets.net(name).is_some())
            .collect();
        names.sort_unstable();
        net_resolution::multiple_drivers(names.first().map_or("", |name| name.as_str()))
    };
    for (index, process) in module.comb_processes().iter().enumerate() {
        if redundant.contains(&index) {
            continue;
        }
        if let Some(condition) = process.condition() {
            let condition =
                sv::typecheck::eval_const_expr_with_types(condition, constants, parameter_types)
                    .ok_or_else(|| {
                        sv::AnalyzerError::Unsupported(
                            "unknown conditional-generate condition".to_string(),
                        )
                    })?;
            if condition == 0 {
                continue;
            }
        }
        let net_driver = resolved_nets.drives(index);
        pm.call_unit();
        // A wildcard control waits for its first source event even when
        // every output is assigned. A symbolic combinational definition
        // would instead execute during the initial fixed-point evaluation.
        // It also follows every variable its statement reads, while an
        // `always @*` is sensitive only to the arguments of the functions it
        // calls, not to what their bodies read (IEEE 1800-2023 9.2.2.2.2):
        // one whose callees read more keeps the guarded unit.
        let symbolic_star =
            symbolic_star(index, process) && !pm.callees_read_beyond_wildcard(process);
        if (process.kind() == sv::ir::CombProcessKind::AlwaysStar && !symbolic_star)
            || pm.reads_simulation_state(process.body())
            // The symbolic form would expand every call.
            || pm.calls_out_of_line(process.body())
            || (process.kind() != sv::ir::CombProcessKind::ContinuousAssign
                && pm.calls_task(process.body()))
        {
            if net_driver {
                return Err(driver_error(process));
            }
            pm.runtime.event_sites = std::mem::take(&mut sites);
            comb_blocks.extend(
                ff::Ff::new(&mut pm, random_seeds[&(2, index)]).lower_comb_process(
                    process.body(),
                    process.sensitivity_body(),
                    process.kind(),
                    allow_inferred_latches,
                    process.site(),
                    &[],
                    &mut arena,
                )?,
            );
            sites = std::mem::take(&mut pm.runtime.event_sites);
            continue;
        }
        // A process that latches falls back to Ff below.
        pm.attempt_calls();
        let mut comb = comb::Comb::new(&mut pm, &mut arena);
        comb.events.site_base = sites.len() as u32;
        comb.continuous = process.kind() == sv::ir::CombProcessKind::ContinuousAssign;
        let lowered = comb.lower_process(process.body());
        // The guarded unit accepts whatever the symbolic form refuses of an
        // `always @*`, as it did before the symbolic attempt, and runs a
        // loop whose condition the symbolic form cannot bound, as in
        // `for (i = W - 2; done == 0; i = i - 1)`: the process executes its
        // statements, so the loop ends as the values make it end.
        let runtime_loop = matches!(
            &lowered,
            Err(sv::AnalyzerError::Unsupported(reason)) if reason == comb::RUNTIME_LOOP_CONDITION
        );
        let fallback = (symbolic_star && lowered.is_err())
            || runtime_loop
            || matches!(
                &lowered,
                Err(sv::AnalyzerError::Unsupported(reason))
                    if reason.starts_with("latch inference inside always_comb")
                        && process.kind() != sv::ir::CombProcessKind::ContinuousAssign
                        && (allow_inferred_latches
                            || matches!(
                                process.kind(),
                                sv::ir::CombProcessKind::AlwaysLatch
                                    | sv::ir::CombProcessKind::AlwaysStar
                            ))
            );
        comb.m.end_attempt_calls(!fallback);
        if net_driver {
            // A driver of a resolved net contributes its value to the net's
            // one definition instead of defining the bits itself.
            let paths = lowered.map_err(|_| driver_error(process))?;
            if !comb.events.observers.is_empty() || !comb.events.sites.is_empty() {
                return Err(driver_error(process));
            }
            drop(comb);
            for path in paths {
                let target = match &path.target {
                    LogicPathTarget::Var(target) => Some(target.id),
                    _ => None,
                };
                let net = target.filter(|id| resolved_ids.contains_key(id));
                match net {
                    Some(id) if net_resolution::joinable(&path) => {
                        net_drivers.entry(id).or_default().push(path)
                    }
                    Some(_) => return Err(driver_error(process)),
                    None => comb_blocks.push(path),
                }
            }
            continue;
        }
        match lowered {
            // In a design with a wait, a variable the process stores twice
            // may be watched, so it also gets its imperative alternative.
            // A process with runtime events keeps only its symbolic form.
            Ok(paths)
                if waits
                    && (process.kind() == sv::ir::CombProcessKind::AlwaysComb || symbolic_star)
                    && comb.events.observers.is_empty()
                    && comb.events.sites.is_empty() =>
            {
                drop(comb);
                let mut targets: Vec<_> = paths
                    .iter()
                    .filter_map(|path| match &path.target {
                        LogicPathTarget::Var(target) => Some(*target),
                        _ => None,
                    })
                    .collect();
                targets.sort_unstable();
                targets.dedup();
                comb_blocks.extend(paths);
                if let Some(watchable) = &watchable
                    && targets
                        .iter()
                        .map(|target| target.id)
                        .chain(pm.written_variables(process.body()))
                        .all(|id| !watchable.contains(&id))
                {
                    continue;
                }
                if let Some((paths, alternative)) = lower_comb_alternative(
                    &mut pm,
                    process,
                    random_seeds[&(2, index)],
                    targets,
                    gated_waits,
                    &mut arena,
                ) {
                    comb_blocks.extend(paths);
                    alternatives.push(alternative);
                }
                continue;
            }
            Ok(paths) => comb_blocks.extend(paths),
            // A continuous assignment is evaluated again when its own
            // target changes (IEEE 1800-2023 10.3.2), which a guarded unit,
            // scheduled after what it reads other than what it writes, does
            // not do. As in the symbolic form, one that reads the bits of its
            // target, also in the functions it calls, is refused; other bits
            // of the target variable, as in `assign y[3:0] = f(y[7:4]);`,
            // are read like any other variable.
            Err(_)
                if runtime_loop
                    && process.kind() == sv::ir::CombProcessKind::ContinuousAssign
                    && {
                        // The reads by longest static prefixes (9.2.2.2.1),
                        // so a constant select reads only its bits.
                        let target = comb.continuous_target_bits(process.body());
                        let written = comb.m.written_variables(process.body());
                        comb.m
                            .implicit_reads(process.body(), true)
                            .iter()
                            .any(|read| match &target {
                                // A variable of the target: the bits it writes.
                                Some(bits) if bits.iter().any(|(id, _)| *id == read.id) => {
                                    bits.iter().any(|(id, access)| {
                                        *id == read.id && read.access.overlaps(access)
                                    })
                                }
                                _ => written.contains(&read.id),
                            })
                    } =>
            {
                return Err(sv::AnalyzerError::Unsupported(
                    "continuous assignment that reads its own target through a loop whose \
                     condition depends on values"
                        .into(),
                ));
            }
            Err(_) if fallback => {
                // A retained output is procedural state. The guarded unit
                // executes once per implicit-sensitivity change, without
                // interpreting held values as a combinational feedback loop.
                drop(comb);
                pm.runtime.event_sites = std::mem::take(&mut sites);
                comb_blocks.extend(
                    ff::Ff::new(&mut pm, random_seeds[&(2, index)]).lower_comb_process(
                        process.body(),
                        process.sensitivity_body(),
                        process.kind(),
                        allow_inferred_latches,
                        process.site(),
                        &[],
                        &mut arena,
                    )?,
                );
                sites = std::mem::take(&mut pm.runtime.event_sites);
                continue;
            }
            Err(error) => return Err(error),
        }
        if comb.events.observers.iter().any(|observer| {
            observer
                .sensitivity
                .iter()
                .any(|atom| !comb.m.var(atom.id).array_dims.is_empty())
        }) {
            // Symbolic array reads may conservatively depend on every element.
            // Process activation follows the syntactic static selection instead.
            let include_functions = process.kind() != sv::ir::CombProcessKind::AlwaysStar;
            let mut reads = comb.m.implicit_reads(process.body(), include_functions);
            reads.extend(
                comb.m
                    .implicit_reads(process.sensitivity_body(), include_functions),
            );
            for observer in &mut comb.events.observers {
                observer.sensitivity = observer
                    .sensitivity
                    .iter()
                    .flat_map(|atom| {
                        if comb.m.var(atom.id).array_dims.is_empty() {
                            return vec![*atom];
                        }
                        let selected: Vec<_> = reads
                            .iter()
                            .filter(|read| read.id == atom.id && read.access.overlaps(&atom.access))
                            .map(|read| {
                                VarAtomBase::new(
                                    atom.id,
                                    read.access.lsb.max(atom.access.lsb),
                                    read.access.msb.min(atom.access.msb),
                                )
                            })
                            .collect();
                        if selected.is_empty() {
                            vec![*atom]
                        } else {
                            selected
                        }
                    })
                    .collect();
            }
        }
        // The sites of one process activate together. Every change of a
        // source schedules the process (IEEE 1800-2023 4.3), also one that a
        // later write of the same time slot undoes, so each sensitivity atom
        // counts its occurrences.
        let base = sites.len() as u32;
        let mut counters: Vec<(VarAtomBase<SourceVarId>, SourceVarId)> = Vec::new();
        for mut observer in std::mem::take(&mut comb.events.observers) {
            observer.activation_group = base;
            for atom in &observer.sensitivity {
                if !counters.iter().any(|(counted, _)| counted == atom) {
                    let (counter, _) = comb.m.temp(
                        "comb_occurrences",
                        celox_design::EVENT_COUNTER_WIDTH,
                        false,
                        false,
                    );
                    counters.push((*atom, counter));
                }
            }
            observer.sensitivity_counters = observer
                .sensitivity
                .iter()
                .map(|atom| {
                    *counters
                        .iter()
                        .find(|(counted, _)| counted == atom)
                        .expect("a counter for every sensitivity atom")
                })
                .collect();
            // A deferred action waits in the queue of its process.
            if let Some(celox_design::DeferredSite::Action { queue, .. }) =
                &mut comb.events.sites[(observer.site_id - base) as usize].deferred
            {
                *queue = base;
            }
            observers.push(observer);
        }
        sites.append(&mut comb.events.sites);
    }
    let mut resolved_ids: Vec<_> = resolved_ids.into_iter().collect();
    resolved_ids.sort_unstable_by_key(|(_, name)| *name);
    for (id, name) in resolved_ids {
        let Some(drivers) = net_drivers.remove(&id) else {
            continue;
        };
        let resolution = resolved_nets.net(name).expect("a resolved net");
        comb_blocks.extend(net_resolution::join(
            &mut arena,
            id,
            resolution,
            drivers,
            four_state,
            !pm.var(id).is_4state,
        )?);
    }
    for (target, body) in port_assignments {
        pm.call_unit();
        pm.runtime.event_sites = std::mem::take(&mut sites);
        comb_blocks.extend(ff::Ff::new(&mut pm, 0).lower_comb_process(
            body,
            &[],
            sv::ir::CombProcessKind::ContinuousAssign,
            false,
            None,
            &[*target],
            &mut arena,
        )?);
        sites = std::mem::take(&mut pm.runtime.event_sites);
    }
    *runtime_errors = std::mem::take(&mut pm.runtime.errors);
    *extern_functions = std::mem::take(&mut pm.externs.functions);
    let created = std::mem::take(&mut pm.created);
    Ok((comb_blocks, arena, created, observers, sites, alternatives))
}

/// `seeds` and every variable a combinational process or a port assignment
/// connects to them, in either direction. Clock-domain unification aliases
/// only whole-variable copies, but any variable a process writes and any it
/// writes or reads join one group, at any width and through any expression.
fn watchable_variables(
    pm: &procedural::ProcModule<'_>,
    module: &sv::ir::Module,
    port_assignments: &[(SourceVarId, Vec<sv::ir::Stmt>)],
    seeds: &HashSet<SourceVarId>,
) -> HashSet<SourceVarId> {
    let mut groups: Vec<Vec<SourceVarId>> = Vec::new();
    let mut group = |writes: HashSet<SourceVarId>, reads: HashSet<VarAtomBase<SourceVarId>>| {
        if !writes.is_empty() {
            groups.push(
                writes
                    .into_iter()
                    .chain(reads.iter().map(|read| read.id))
                    .collect(),
            );
        }
    };
    for process in module.comb_processes() {
        let mut reads = pm.implicit_reads(process.body(), true);
        reads.extend(pm.implicit_reads(process.sensitivity_body(), true));
        group(pm.written_variables(process.body()), reads);
    }
    for (target, body) in port_assignments {
        let mut writes = pm.written_variables(body);
        writes.insert(*target);
        group(writes, pm.implicit_reads(body, true));
    }
    let mut watchable = seeds.clone();
    loop {
        let before = groups.len();
        groups.retain(|group| {
            if !group.iter().any(|id| watchable.contains(id)) {
                return true;
            }
            watchable.extend(group.iter().copied());
            false
        });
        if groups.len() == before {
            return watchable;
        }
    }
}

/// The imperative unit of an `always_comb` process lowered symbolically to
/// `targets`, and its [`CombAlternative`]. Keep even single-store alternatives:
/// assembly resolves cross-module gated wait dependencies only after lowering.
/// It chooses imperative execution only when those global dependencies or
/// occurrence counters require preserving procedural order.
fn lower_comb_alternative(
    pm: &mut procedural::ProcModule<'_>,
    process: &sv::ir::CombProcess,
    random_seed: u32,
    targets: Vec<VarAtomBase<SourceVarId>>,
    gated_waits: bool,
    arena: &mut SLTNodeArena<SourceVarId>,
) -> Option<(Vec<LogicPath<SourceVarId>>, CombAlternative)> {
    let checkpoint = pm.checkpoint();
    pm.attempt_calls();
    let mut scratch = SLTNodeArena::new();
    let lowered = ff::Ff::new(pm, random_seed)
        .without_input_occurrences()
        .lower_comb_process(
            process.body(),
            process.sensitivity_body(),
            process.kind(),
            false,
            process.site(),
            &[],
            &mut scratch,
        );
    let alternative = match lowered {
        Ok(paths) if pm.runtime.event_sites.len() == checkpoint.runtime_event_sites() => paths
            .first()
            .and_then(|path| match &path.target {
                LogicPathTarget::Process { process, unit, .. } => Some((*process, unit)),
                _ => None,
            })
            .map(|(id, unit)| {
                let candidates: Vec<_> =
                    celox_frontend_core::watch::multi_store_candidates(unit, |&address| {
                        (!pm.is_hidden(address) && pm.var(address).array_dims.is_empty())
                            .then_some(address)
                    });
                (id, candidates)
            })
            .filter(|(_, candidates)| gated_waits || !candidates.is_empty())
            .map(|(process, candidates)| (paths, process, candidates)),
        _ => None,
    };
    pm.end_attempt_calls(alternative.is_some());
    let Some((mut paths, process, mut candidates)) = alternative else {
        pm.rollback(checkpoint);
        return None;
    };
    for path in &mut paths {
        path.expr = arena
            .alloc(scratch.get(path.expr).clone())
            .expect("an input node of a fresh arena");
    }
    candidates.sort_unstable();
    candidates.dedup();
    Some((
        paths,
        CombAlternative {
            process,
            targets,
            candidates,
        },
    ))
}

/// The continuous assignments `copy = signal` of the hidden event copies of
/// [`FfEventVars`]: a whole-variable copy, which clock-domain unification
/// keeps in the signal's domain unless the copy is sensed at the other edge.
pub(super) fn push_event_copies(
    copies: &[(SourceVarId, SourceVarId)],
    comb_blocks: &mut Vec<LogicPath<SourceVarId>>,
    arena: &mut SLTNodeArena<SourceVarId>,
) -> Result<(), ParserError> {
    for &(copy, signal) in copies {
        let access = BitAccess::new(0, 0);
        let expr = arena
            .alloc(SLTNode::Input {
                variable: signal,
                signed: false,
                index: Vec::new(),
                access,
            })
            .map_err(|error| {
                ParserError::illegal_context(
                    "systemverilog always_ff event copy",
                    error.to_string(),
                    None,
                )
            })?;
        comb_blocks.push(LogicPath {
            transparent_copy: false,
            target: LogicPathTarget::Var(VarAtomBase::new(copy, 0, 0)),
            sources: std::iter::once(VarAtomBase::new(signal, 0, 0)).collect(),
            previous_sources: HashSet::default(),
            address_sources: HashSet::default(),
            local_inputs: Vec::new(),
            order_before: HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
            expr,
        });
    }
    Ok(())
}

/// The bit boundaries at which combinational processes write or statically
/// read each variable. Flattening splits paths at these boundaries (and at
/// those propagated through ports), so bit-disjoint dependencies on one
/// variable are scheduled independently instead of forming a false loop.
pub(super) fn comb_boundaries(
    paths: &[LogicPath<SourceVarId>],
) -> HashMap<SourceVarId, BTreeSet<usize>> {
    let mut boundaries: HashMap<SourceVarId, BTreeSet<usize>> = HashMap::default();
    for path in paths {
        for atom in path.target.var().into_iter().chain(&path.sources) {
            boundaries
                .entry(atom.id)
                .or_default()
                .extend([atom.access.lsb, atom.access.msb + 1]);
        }
    }
    boundaries
}
