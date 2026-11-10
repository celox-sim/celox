//! The plan itself, the clocks derived from the events its slots change,
//! and whether their domains need ordered waves.
use super::*;

impl<'a> PlanBuilder<'a> {
    /// The plan of what the phases found so far, without its derived clocks
    /// and ordered waves.
    pub(super) fn plan(&mut self) -> Result<Plan, String> {
        let program = self.program;
        let slots = self.slots;
        let model = self.model;
        let native_observers = self.native_observers;
        let event_bits = self.found.event_bits;
        let events = std::mem::take(&mut self.found.events);
        let rtl_written = &self.found.rtl_written;
        let active_commits = &self.found.active_commits;
        let clocks = std::mem::take(&mut self.found.clocks);
        let service_function = self.found.service_function;
        let ordered_available = self.found.ordered_available;
        let nba_processes = std::mem::take(&mut self.found.nba_processes);
        let service_processes = std::mem::take(&mut self.found.service_processes);
        let kernels = std::mem::take(&mut self.found.kernels);
        let fallback = std::mem::take(&mut self.found.fallback);
        let inactive = std::mem::take(&mut self.found.inactive);
        let cyclic = self.found.cyclic;
        let finish_sites = std::mem::take(&mut self.found.finish_sites);
        let settle_reads = std::mem::take(&mut self.found.settle_reads);
        let lazy = std::mem::take(&mut self.found.lazy);
        let incremental = std::mem::take(&mut self.found.incremental);
        let mut counted_events = Vec::new();
        let counter_base = program.layout().widths[&slots.unknown_levels];
        for (index, counted) in model.counted.iter().enumerate() {
            for (kind_index, kind) in CounterKind::ALL.into_iter().enumerate() {
                let domains: Vec<_> = counted
                    .domains
                    .iter()
                    .filter(|(_, candidate)| *candidate == kind)
                    .filter_map(|(signal, _)| {
                        events.iter().position(|event| event.signal == *signal)
                    })
                    .collect();
                if domains.is_empty() {
                    continue;
                }
                let baseline_offset = counter_base + (index * 3 + kind_index) * 32;
                if baseline_offset + 32 > program.layout().widths[&slots.levels] {
                    return Err(
                        "the compiled domain counter snapshot has insufficient storage".into(),
                    );
                }
                counted_events.push(CountedEvent {
                    counter: counted.slots.counter(kind),
                    baseline_offset,
                    events: domains,
                });
            }
        }
        let host_edges = if ordered_available {
            (0..events.len())
                .filter(|event| !rtl_written.contains(event))
                .collect()
        } else {
            Vec::new()
        };
        let plan = Plan {
            host_edges,
            inactive,
            observers: native_observers
                .then(|| observers::Observers::new(program))
                .filter(|observers| !observers.groups.is_empty()),
            counted: counted_events,
            events,
            kernels,
            clocks,
            fallback,
            finish_sites,
            service_function,
            ordered: None,
            service_processes,
            nba_processes,
            cyclic,
            settle_reads,
            lazy,
            incremental,
            derived: Vec::new(),
            active: active_commits.iter().copied().collect(),
            gating: Vec::new(),
            event_bits,
        };
        Ok(plan)
    }

    /// The clocks derived from the events the slots change, transitively.
    pub(super) fn derived_clocks(&mut self, plan: &Plan) {
        let topology = self.topology;
        let edged = plan.edged();
        // The clocks derived from the events the slots change, transitively:
        // their levels change only through those events' domains and
        // combinational logic.
        let index_of: HashMap<AbsoluteAddr, usize> = plan
            .events
            .iter()
            .enumerate()
            .map(|(index, event)| (event.signal, index))
            .collect();
        self.found.index_of = index_of;
        let index_of = &self.found.index_of;
        let derived_from = derived_events(plan, topology, index_of);
        let mut derived: BTreeSet<_> = plan
            .counted
            .iter()
            .flat_map(|counted| counted.events.iter().copied())
            .filter(|event| !edged.contains(event))
            .collect();
        let mut pending: Vec<_> = edged
            .iter()
            .copied()
            .chain(derived.iter().copied())
            .collect();
        while let Some(event) = pending.pop() {
            for next in derived_from(event) {
                if !edged.contains(&next) && derived.insert(next) {
                    pending.push(next);
                }
            }
        }
        self.found.edged = edged;
        self.found.derived = derived;
    }

    /// Whether Active-region commits can give a clock a second edge in a
    /// slot, which needs ordered waves.
    pub(super) fn active_ordering(&mut self, plan: &Plan) -> Result<(), String> {
        let program = self.program;
        let model = self.model;
        let topology = self.topology;
        let sir = self.sir;
        let shared = self.shared;
        let event_storage = &self.found.event_storage;
        let rtl_written = &self.found.rtl_written;
        let rtl_stores = &self.found.rtl_stores;
        let ordered_available = self.found.ordered_available;
        let event_writing_waits = self.found.event_writing_waits;
        let edged = &self.found.edged;
        let index_of = &self.found.index_of;
        let storage = home(shared);
        let stores = unit_stores(storage);
        // Simple Active waves retain one pending result per domain. Select an
        // ordered queue for cases that can generate a second edge:
        // mixed Active/NBA gate inputs, unknown-to-known edge sequences,
        // direct procedural clock writes, and Active-clock feedback need an
        // ordered queue rather than a domain mask.
        let mut needs_ordered = event_writing_waits || !plan.inactive.is_empty();
        if !plan.active.is_empty() {
            let mut active_stores: HashMap<AbsoluteAddr, Vec<usize>> = HashMap::default();
            for (index, event) in plan.events.iter().enumerate() {
                if !model.active_commits.contains(&event.signal) {
                    continue;
                }
                for unit in event.eval_only.iter().flatten() {
                    for target in instructions(unit).flat_map(written_storage) {
                        if target.region == STABLE_REGION {
                            active_stores
                                .entry(storage(target.absolute_addr()))
                                .or_default()
                                .push(index);
                        }
                    }
                }
            }
            if active_stores
                .keys()
                .any(|address| event_storage.contains_key(address))
            {
                if !ordered_available {
                    return Err("an Active-region update directly drives an event".into());
                }
                needs_ordered = true;
            }
            let nba_stores: HashSet<_> =
                sir.apply_ffs.values().flatten().flat_map(stores).collect();
            let primary = |index: &usize| edged.contains(index) && !rtl_written.contains(index);
            for (source, clocks) in &topology.combinational_derived {
                for clock in clocks {
                    let Some(gate) = &clock.gate else {
                        if clock.inputs.as_ref().is_some_and(|inputs| {
                            inputs
                                .iter()
                                .all(|input| !active_stores.contains_key(&storage(*input)))
                        }) {
                            continue;
                        }
                        tracing::debug!(target: "celox::scheduler",
                            source = ?program.get_path(source),
                            clock = ?program.get_path(&clock.clock),
                            active_stores = ?active_stores.keys().map(|address| program.get_path(address)).collect::<Vec<_>>(),
                            "Active clock cone requires ordered evaluation");
                        if !ordered_available {
                            return Err(
                                "an Active-region clock has an unknown combinational cone".into()
                            );
                        }
                        needs_ordered = true;
                        continue;
                    };
                    let inputs: Vec<_> = gate
                        .iter()
                        .filter_map(|op| match op {
                            celox_design::GateOp::Load { address, .. } => Some(storage(*address)),
                            _ => None,
                        })
                        .collect();
                    if !inputs.iter().any(|input| active_stores.contains_key(input)) {
                        continue;
                    }
                    let source = storage(*source);
                    let source_event = index_of.get(&topology.canonical(source));
                    if !source_event.is_some_and(primary)
                        || inputs.iter().any(|input| {
                            nba_stores.contains(input)
                                || rtl_stores.contains(input) && !active_stores.contains_key(input)
                                || active_stores
                                    .get(input)
                                    .is_some_and(|writers| !writers.iter().all(primary))
                        })
                    {
                        if !ordered_available {
                            return Err("an Active-region clock requires ordered updates".into());
                        }
                        needs_ordered = true;
                    }
                    if program.layout().four_state
                        && std::iter::once(&source).chain(&inputs).any(|input| {
                            program
                                .layout()
                                .is_4states
                                .get(input)
                                .copied()
                                .unwrap_or(true)
                        })
                    {
                        if !ordered_available {
                            return Err(
                                "an Active-region clock may have repeated four-state edges".into(),
                            );
                        }
                        needs_ordered = true;
                    }
                    if plan.kernels.iter().enumerate().any(|(index, kernel)| {
                        kernel.as_ref().is_some_and(|kernel| {
                            source_event.is_some_and(|source| kernel.writes.contains(source))
                        }) && instructions(&sir.processes[index])
                            .map(|inst| {
                                let writes = written_storage(inst)
                                    .any(|target| storage(target.absolute_addr()) == source);
                                // A call may write it any number of times.
                                match writes {
                                    false => 0,
                                    true if inst.call_effects().is_some() => 2,
                                    true => 1,
                                }
                            })
                            .sum::<usize>()
                            > 1
                    }) {
                        if !ordered_available {
                            return Err(
                                "an Active-region clock has several procedural writes".into()
                            );
                        }
                        needs_ordered = true;
                    }
                }
            }
        }
        self.found.needs_ordered = needs_ordered;
        Ok(())
    }

    /// The events RTL logic drives from host-owned inputs alone: they are
    /// stable throughout a compiled run.
    pub(super) fn stable_events<F: FnOnce() -> CombSlicer<'a>>(
        &mut self,
        plan: &Plan,
        comb_slicer: &Slicer<'a, F>,
    ) {
        let program = self.program;
        let schema = self.schema;
        let model = self.model;
        let topology = self.topology;
        let sir = self.sir;
        let shared = self.shared;
        let rtl_written = &self.found.rtl_written;
        let rtl_stores = &self.found.rtl_stores;
        let kernel_stores = &self.found.kernel_stores;
        let nba_targets = &self.found.nba_targets;
        let effects = self.found.effects;
        let edged = &self.found.edged;
        let index_of = &self.found.index_of;
        let derived = &self.found.derived;
        let storage = home(shared);
        let stores = unit_stores(storage);
        // A combinational reset (or clock) driven only by host-owned inputs
        // is stable throughout a compiled run. Host writes are settled and
        // their edges handled before entering the scheduler. Callable kernels,
        // like host writes, can only run outside this unit.
        let scheduled_stores: HashSet<AbsoluteAddr> = sir
            .eval_apply_ffs
            .values()
            .flatten()
            .chain(sir.eval_only_ffs.values().flatten())
            .chain(sir.apply_ffs.values().flatten())
            .flat_map(stores)
            .chain(
                kernel_stores
                    .iter()
                    .zip(&schema.processes)
                    .filter(|(_, process)| process.kind != celox_design::ProcessKind::Callable)
                    .flat_map(|(stores, _)| stores.iter().copied()),
            )
            .chain(nba_targets.keys().copied())
            .collect();
        let clocked_storage: HashSet<_> = edged
            .iter()
            .map(|&event| storage(plan.events[event].signal))
            .collect();
        // Reference processes stop a compiled run before their next wakeup,
        // and ready processes, waiters and queued NBAs prevent entry. Their
        // stores are therefore stable inputs here, just like host writes.
        // Only stores that can execute inside this unit invalidate stability.
        let compiled_stores: HashSet<AbsoluteAddr> = kernel_stores
            .iter()
            .zip(&plan.kernels)
            .filter(|(_, kernel)| kernel.is_some())
            .flat_map(|(stores, _)| stores.iter().copied())
            .chain(
                model
                    .processes
                    .iter()
                    .flat_map(|process| process.nbas.iter().map(|site| storage(site.target))),
            )
            .collect();
        let stable_input = |address| {
            let address = storage(address);
            !rtl_stores.contains(&address)
                && !compiled_stores.contains(&address)
                && !clocked_storage.contains(&address)
                && !schema
                    .time_slots
                    .iter()
                    .any(|slot| storage(*slot) == address)
        };
        let mut stable_host_events: BTreeSet<usize> = topology
            .combinational_derived
            .iter()
            .filter(|(source, _)| stable_input(**source))
            .flat_map(|(_, clocks)| clocks)
            .filter(|clock| !scheduled_stores.contains(&storage(clock.clock)))
            .filter(|clock| {
                clock.gate.as_ref().is_some_and(|gate| {
                    gate.iter().all(|op| match op {
                        celox_design::GateOp::Load { address, .. } => stable_input(*address),
                        _ => true,
                    })
                })
            })
            .filter_map(|clock| index_of.get(&topology.canonical(clock.clock)).copied())
            .collect();
        // Partition flags cache which combinational work is pending; they
        // do not determine its settled values. Following their shared home
        // would pull every partition into each clock/reset's dependency cone.
        let partition_flags = schema.settle_partitions.as_ref().map(|s| storage(s.flags));
        // Constant clocks/resets and pure combinational cones of stable host
        // inputs need no domain waves after the entry settle. They need not
        // appear in the frontend's clock-gate topology (a constant has no
        // source clock at all). Prove stability from the actual SIR cone.
        if !effects {
            for &index in rtl_written.difference(derived) {
                let signal = plan.events[index].signal;
                if stable_host_events.contains(&index)
                    || scheduled_stores.contains(&storage(signal))
                {
                    continue;
                }
                if comb_slicer
                    .stateless_cone(signal, partition_flags)
                    .is_some_and(|(writes, inputs)| {
                        tracing::debug!(target: "celox::scheduler", signal = ?program.get_path(&signal), scheduled = ?writes.intersection(&scheduled_stores).map(|a| program.get_path(a)).take(8).collect::<Vec<_>>(), unstable = ?inputs.iter().filter(|a| Some(**a) != partition_flags && !stable_input(**a)).map(|a| program.get_path(a)).take(8).collect::<Vec<_>>(), "event cone external dependencies");
                        writes.is_disjoint(&scheduled_stores)
                            && inputs
                                .into_iter()
                                .all(|input| Some(input) == partition_flags || stable_input(input))
                    })
                {
                    stable_host_events.insert(index);
                }
            }
        }
        self.found.scheduled_stores = scheduled_stores;
        self.found.stable_host_events = stable_host_events;
    }

    /// The clocks combinational logic computes without an upstream event
    /// domain run as derived clocks.
    pub(super) fn orphan_clocks<F: FnOnce() -> CombSlicer<'a>>(
        &mut self,
        plan: &Plan,
        comb_slicer: &Slicer<'a, F>,
    ) {
        let model = self.model;
        let topology = self.topology;
        let sir = self.sir;
        let shared = self.shared;
        let rtl_written = &self.found.rtl_written;
        let ordered_available = self.found.ordered_available;
        let effects = self.found.effects;
        let comb_stores = &self.found.comb_stores;
        let edged = &self.found.edged;
        let mut derived = std::mem::take(&mut self.found.derived);
        let mut needs_ordered = self.found.needs_ordered;
        let scheduled_stores = &self.found.scheduled_stores;
        let stable_host_events = &self.found.stable_host_events;
        let storage = home(shared);
        let derived_from = derived_events(plan, topology, &self.found.index_of);
        // A clock need not have an upstream event domain: `clk & enable`
        // still clocks RTL when nothing waits on `clk` itself. Such clocks
        // (including latch-based gates) are observed after the same settles
        // as in the reference loop. Topology only supplies their downstream
        // FF domains; it must not be an admission requirement for the root.
        if !effects {
            let candidates: Vec<_> = rtl_written
                .difference(&derived)
                .filter(|index| !edged.contains(index) && !stable_host_events.contains(index))
                .copied()
                .filter(|&index| {
                    let signal = storage(plan.events[index].signal);
                    comb_stores.contains(&signal) && !scheduled_stores.contains(&signal)
                })
                .collect();
            if !candidates.is_empty() {
                // A blocking FF write may move this clock repeatedly before
                // NBA. Use the ordered path when its cone reads such a write.
                let active_stores: HashSet<_> = model
                    .active_commits
                    .iter()
                    .filter_map(|event| sir.eval_only_ffs.get(event))
                    .flatten()
                    .flat_map(instructions)
                    .flat_map(written_storage)
                    .filter(|target| target.region == STABLE_REGION)
                    .map(|target| storage(target.absolute_addr()))
                    .collect();
                let mut pending = Vec::new();
                for index in candidates {
                    let active = !active_stores.is_empty()
                        && comb_slicer
                            .input_signals([plan.events[index].signal])
                            .iter()
                            .any(|input| active_stores.contains(&storage(*input)));
                    if active && !ordered_available {
                        continue;
                    }
                    needs_ordered |= active;
                    pending.push(index);
                }
                while let Some(event) = pending.pop() {
                    if !edged.contains(&event) && derived.insert(event) {
                        pending.extend(derived_from(event));
                    }
                }
            }
        }
        self.found.derived = derived;
        self.found.needs_ordered = needs_ordered;
    }

    /// The events nonblocking assignments and unproven RTL logic drive run as
    /// derived clocks in ordered waves, when the slots need them.
    pub(super) fn ordered_waves(&mut self, plan: &mut Plan) {
        let program = self.program;
        let model = self.model;
        let shared = self.shared;
        let event_storage = &self.found.event_storage;
        let rtl_written = &self.found.rtl_written;
        let ordered_available = self.found.ordered_available;
        let edged = &self.found.edged;
        let mut derived = std::mem::take(&mut self.found.derived);
        let mut needs_ordered = self.found.needs_ordered;
        let stable_host_events = &self.found.stable_host_events;
        let storage = home(shared);
        let nba_written: BTreeSet<_> = plan
            .nba_processes
            .iter()
            .flat_map(|&process| &model.processes[process].nbas)
            .filter_map(|site| event_storage.get(&storage(site.target)))
            .flatten()
            .copied()
            .collect();
        if !nba_written.is_empty() {
            derived.extend(nba_written.iter().filter(|event| !edged.contains(event)));
            needs_ordered = true;
        }
        let unhandled_rtl_events: Vec<_> = rtl_written
            .difference(&derived)
            .filter(|event| ordered_available || !stable_host_events.contains(event))
            .copied()
            .collect();
        if ordered_available && !unhandled_rtl_events.is_empty() {
            // Dependency proofs select the smaller schedule. Their failure
            // selects generated ordered waves over every affected domain.
            // Kernel-written events are already in `edged` and must appear
            // only once in the level snapshot.
            derived.extend(
                unhandled_rtl_events
                    .iter()
                    .filter(|event| !edged.contains(event)),
            );
            needs_ordered = true;
        }
        if needs_ordered {
            // The topology-based observer pruning above predates orphan
            // clocks. Ordered waves must preserve every real Active commit,
            // including writers in a clock cone without a source domain.
            plan.active = plan
                .events
                .iter()
                .enumerate()
                .filter(|(_, event)| event.active_commit)
                .map(|(index, _)| index)
                .collect();
            plan.ordered = Some(ordered::Ordered::new(
                program,
                plan.events.len(),
                plan.service_function.unwrap(),
            ));
        }
        self.found.derived = derived;
        self.found.needs_ordered = needs_ordered;
        self.found.unhandled_rtl_events = unhandled_rtl_events;
    }

    /// The derived clocks and gated sources of the plan, and whether its
    /// events have the units their waves need.
    pub(super) fn finish(&mut self, plan: &mut Plan) -> Result<(), String> {
        let program = self.program;
        let slots = self.slots;
        let schema = self.schema;
        let topology = self.topology;
        let sir = self.sir;
        let shared = self.shared;
        let event_bits = self.found.event_bits;
        let active_commits = &self.found.active_commits;
        let ordered_available = self.found.ordered_available;
        let effects = self.found.effects;
        let watched_comb = &self.found.watched_comb;
        let retained = &self.found.retained;
        let eager_candidate_instructions = self.found.eager_candidate_instructions;
        let edged = &self.found.edged;
        let index_of = &self.found.index_of;
        let derived = std::mem::take(&mut self.found.derived);
        let unhandled_rtl_events = &self.found.unhandled_rtl_events;
        let storage = home(shared);
        let written = written_events(&self.found.event_storage, storage);
        tracing::debug!(target: "celox::scheduler",
                incremental_instructions = plan.incremental.as_ref().map(|cone| cone.units.iter().flat_map(instructions).count()),
                incremental_source = ?plan.incremental.as_ref().map(|cone| program.get_path(&cone.source)),
                deferred_settles = plan.lazy.is_some(),
                comb_effects = effects,
                comb_fallible = sir.eval_comb.iter().any(fails),
                stateful_targets = ?sir.eval_comb_stateful.as_ref().map(Vec::len),
                watched_targets = watched_comb.len(),
                retained_targets = retained.len(),
                ?eager_candidate_instructions,
                comb_instructions = sir.eval_comb.iter().flat_map(instructions).count(),
                eager_instructions = plan.lazy.as_ref().map(|lazy| lazy.eager.iter().flat_map(instructions).count()),
                active_commits = ?active_commits.iter().map(|&event| program.get_path(&plan.events[event].signal)).collect::<Vec<_>>(),
                kernels = ?plan.fallback.iter().enumerate().map(|(index, reason)| (index, schema.process_origins.get(index).map(ToString::to_string), reason, written(&sir.processes[index]).iter().map(|&event| program.get_path(&plan.events[event].signal)).collect::<Vec<_>>())).collect::<Vec<_>>(),
                "compiled scheduler process plans");
        if !ordered_available && !unhandled_rtl_events.is_empty() {
            tracing::debug!(target: "celox::scheduler",
                signals = ?unhandled_rtl_events.iter().map(|&event| program.get_path(&plan.events[event].signal)).collect::<Vec<_>>(),
                edged = ?edged.iter().map(|&event| program.get_path(&plan.events[event].signal)).collect::<Vec<_>>(),
                "compiled scheduler rejected RTL-driven events");

            return Err("RTL logic drives an event signal".into());
        }
        let gating: Vec<(usize, Vec<usize>)> = edged
            .iter()
            .filter_map(|&source| {
                let signal = plan.events[source].signal;
                let gated: Vec<usize> = topology
                    .combinational_derived
                    .get(&signal)
                    .into_iter()
                    .flatten()
                    .filter_map(|clock| index_of.get(&topology.canonical(clock.clock)).copied())
                    .filter(|gated| derived.contains(gated))
                    .collect();
                (!gated.is_empty()).then_some((source, gated))
            })
            .collect();
        plan.derived = derived.into_iter().collect();
        plan.gating = gating;
        let tracked = plan.tracked();
        if (!plan.active.is_empty() || plan.ordered.is_some())
            && (program.layout().widths[&slots.exit_arg] < event_bits * 2
                || tracked.iter().any(|&event| {
                    plan.events[event].eval_only.is_none() || plan.events[event].apply.is_none()
                }))
        {
            return Err(ACTIVE_COMMIT.into());
        }
        if tracked.len() > 1
            && tracked.iter().any(|&event| {
                plan.events[event].eval_only.is_none() || plan.events[event].apply.is_none()
            })
        {
            return Err("an event domain has no separate units".into());
        }
        Ok(())
    }
}
