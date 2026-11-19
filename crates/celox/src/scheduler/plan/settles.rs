//! How the slots settle combinational logic: every time, deferred until
//! something observes it, or by the cone a clock write invalidates.
use super::*;

impl<'a> PlanBuilder<'a> {
    /// Whether a settle is a function of the state it reads, and that state.
    pub(super) fn settle_inputs(&mut self) {
        let program = self.program;
        let schema = self.schema;
        let model = self.model;
        let sir = self.sir;
        let shared = self.shared;
        let addresses = &self.found.addresses;
        let storage = home(shared);
        let finish_sites = schema
            .runtime_event_sites
            .iter()
            .enumerate()
            .filter(|(_, site)| matches!(site.kind, RuntimeEventKind::Finish))
            .map(|(index, _)| index as u32)
            .collect();
        // A settle is the same function of the state it reads every time,
        // unless combinational logic has effects of its own: records,
        // captures or external calls. Clock/reset trigger bits are internal
        // notifications: this scheduler compares event levels itself, and
        // its caller clears the bits before returning to the reference loop.
        // Keep unknown notifications conservative.
        let internal_trigger = |trigger: &celox_design::TriggerIdWithKind| {
            addresses
                .get(trigger.id)
                .is_some_and(|signal| model.kinds.get(signal) == Some(&trigger.kind))
        };
        let effect = |inst: &SIRInstruction<RegionedAbsoluteAddr>| match inst {
            SIRInstruction::Store(_, _, _, _, triggers, sites) => {
                triggers.iter().any(|trigger| !internal_trigger(trigger)) || !sites.is_empty()
            }
            SIRInstruction::Commit(_, _, _, _, triggers) => {
                triggers.iter().any(|trigger| !internal_trigger(trigger))
            }
            // Records, captures and external calls, a callee's too. A
            // call's effects do not say which triggers it raises, so any
            // is taken for one the scheduler does not compare itself.
            _ => inst.host_effects() != celox_sir::HostEffects::default(),
        };
        let effects = sir
            .eval_comb
            .iter()
            .any(|unit| instructions(unit).any(effect));
        // Some effects are no work of a settle whose inputs did not change.
        // The captures of combinational processes (`$display` in
        // `always_comb`, deferred assertions) record only for sites enabled
        // in the settle: by the generated observers when what a process
        // reads changed since the last settle, or by a store that changed
        // its target (`Generator::emit_settle`). A guarded process, an
        // `always @*`, runs only when what it reads changed (IEEE 1800-2023
        // 9.2.2.2, 9.4.2.2), and with it its records and its calls of
        // isolated functions: noncontext DPI-C imports, which reach nothing
        // of the design but their arguments and results (35.5.3). Symbolic
        // logic, which runs in every settle, records through captures and
        // calls no DPI-C import. These effects still keep the plan from
        // running part of a settle or moving it (`effects`): the part would
        // leave out the records and calls its stores do not need.
        let observed = self.native_observers && !schema.comb_observers.is_empty();
        let unchanged_inputs_do_nothing = |inst: &SIRInstruction<RegionedAbsoluteAddr>| match inst {
            SIRInstruction::CombCaptureEvent { .. }
            | SIRInstruction::CombCaptureEnableIfChanged { .. } => observed,
            SIRInstruction::ExternCall { func, .. } => schema
                .extern_functions
                .get(*func as usize)
                .is_some_and(|function| function.isolated),
            SIRInstruction::RuntimeEvent { site_id, .. } => schema
                .runtime_event_sites
                .get(*site_id as usize)
                .is_some_and(|site| match site.kind {
                    RuntimeEventKind::Display | RuntimeEventKind::Write => true,
                    RuntimeEventKind::Report(severity) => severity != celox_design::Severity::Fatal,
                    RuntimeEventKind::Finish | RuntimeEventKind::Stop => false,
                }),
            _ => false,
        };
        let skippable = sir
            .eval_comb
            .iter()
            .flat_map(instructions)
            .all(|inst| unchanged_inputs_do_nothing(inst) || !effect(inst));
        // Dirty tracking compares stable homes by their representative
        // (`shared_storage`), which groups addresses only by an equal start
        // offset: no two homes may overlap from different offsets.
        debug_assert!(
            homes_overlap_only_whole(program.layout()),
            "stable homes overlap at different offsets"
        );
        // A settle whose inputs did not change would store what its outputs
        // already hold, whatever else reads or writes the state: a unit that
        // makes opaque calls marks a change after it runs (`inline_writer`),
        // and a cone settle runs only after precise writes of its sources.
        // Opaque readers only keep settles from waiting for their readers
        // (`deferred_settles`).
        let settle_reads: Option<HashSet<AbsoluteAddr>> = skippable.then(|| {
            sir.eval_comb
                .iter()
                .flat_map(instructions)
                .flat_map(read_storage)
                .map(|source| storage(source.absolute_addr()))
                .collect()
        });
        self.found.finish_sites = finish_sites;
        self.found.effects = effects;
        self.found.settle_reads = settle_reads;
    }

    /// Whether the settles may wait for their readers, and what observes
    /// them (see [`Lazy`]).
    pub(super) fn deferred_settles<F: FnOnce() -> CombSlicer<'a>>(
        &mut self,
        comb_slicer: &mut Slicer<'a, F>,
    ) {
        let schema = self.schema;
        let model = self.model;
        let topology = self.topology;
        let sir = self.sir;
        let shared = self.shared;
        let events = &self.found.events;
        let kernels = &self.found.kernels;
        let finish_sites = &self.found.finish_sites;
        let settle_reads = &self.found.settle_reads;
        let opaque_dependencies = self.found.opaque_dependencies;
        let storage = home(shared);
        let stores = unit_stores(storage);
        // A settle may wait for its readers only when it is a function of
        // its inputs that cannot fail: when it would end the run, it must do
        // so in its own slot; when it reads what an earlier settle left (a
        // latch, a guarded process's snapshots of its inputs, feedback), the
        // settles in between are activations of their own. The comb
        // scheduler knows this from its bit-level dependencies; its loads
        // alone can only bound it, since a load may cover bits the settle
        // has not stored yet and then discard them.
        let comb_stores: HashSet<AbsoluteAddr> = sir.eval_comb.iter().flat_map(stores).collect();
        // Keep sensitivity signals and their occurrence counters eager. A
        // slice rooted only at the final value can discard the counter writes
        // that record an intermediate pulse (even when the value is unchanged).
        let watched_comb: HashSet<_> = kernels
            .iter()
            .flatten()
            .filter_map(|kernel| kernel.wait.as_ref())
            .flat_map(|wait| &wait.terms)
            .map(|&(signal, _)| storage(signal))
            .chain(schema.wait_sites.iter().flat_map(|site| {
                site.gates
                    .iter()
                    .flatten()
                    .flat_map(celox_design::WaitGate::reads)
                    .map(storage)
            }))
            .chain(model.counted.iter().flat_map(|counted| {
                CounterKind::ALL.map(|kind| storage(counted.slots.counter(kind)))
            }))
            .filter(|signal| comb_stores.contains(signal))
            .collect();
        let written_homes: HashSet<_> = comb_stores.iter().copied().map(storage).collect();
        let retained_candidates = sir.eval_comb_stateful.as_ref().map_or_else(
            || written_homes.clone(),
            |stateful| {
                stateful
                    .iter()
                    .map(|target| storage(target.absolute_addr()))
                    .filter(|target| written_homes.contains(target))
                    .collect()
            },
        );
        let retained = retained_storage(&sir.eval_comb, &storage, &retained_candidates);
        let mut eager_candidate_instructions = None;
        // A host watching every update sees each settle as it happens.
        let update_points = schema
            .scheduler
            .as_ref()
            .is_some_and(|scheduler| scheduler.update_points);
        // A pending settle cannot pass a reader whose loads are unknown, nor
        // move the records of the captures it makes.
        let effects = self.found.effects;
        let eager = settle_reads
            .as_ref()
            .filter(|_| {
                !update_points
                    && !opaque_dependencies
                    && !effects
                    && !sir.eval_comb.iter().any(fails)
            })
            .and_then(|_| {
                let stateful = sir.eval_comb_stateful.as_ref();
                let eager = stateful.and_then(|stateful| {
                    if stateful.is_empty() && watched_comb.is_empty() {
                        return Some(Vec::new());
                    }
                    let slicer = comb_slicer.get();
                    let targets = stateful
                        .iter()
                        .map(|target| target.absolute_addr())
                        .filter(|&target| retained.contains(&storage(target)))
                        .chain(watched_comb.iter().copied());
                    // Stateful metadata predates SIR optimization. A target
                    // with no remaining writer (including its shared-storage
                    // aliases) cannot retain a new comb value between settles.
                    // Keep every surviving writer; obsolete targets need no
                    // eager work and must not disable the whole optimization.
                    let eager = slicer.eager_slice(targets.filter(|&target| slicer.stores(target)));
                    // Large eager cones can repeat more work than they save.
                    // Keep at least half of the comb work deferred.
                    let size = |units: &[Unit]| units.iter().flat_map(instructions).count();
                    eager_candidate_instructions = Some(size(&eager));
                    (size(&eager) * 2 < size(&sir.eval_comb)).then_some(eager)
                });
                eager.or_else(|| {
                    (watched_comb.is_empty()
                        && !retains_state(&sir.eval_comb, &storage, &comb_stores))
                    .then(Vec::new)
                })
            });
        let lazy = eager.map(|eager| {
            // Pending settles may leave unrelated outputs unpublished. Every
            // combinational signal a waiter reads belongs to the eager cone.
            // Kernels that read other pending outputs request a full settle.
            let eager_stores: HashSet<_> = eager.iter().flat_map(stores).map(storage).collect();
            debug_assert!(
                watched_comb
                    .iter()
                    .all(|target| eager_stores.contains(target))
            );
            let reads = |unit: &Unit| {
                loads(unit).any(|source| {
                    let source = storage(source);
                    comb_stores.contains(&source) && !eager_stores.contains(&source)
                })
            };
            let ends = |unit: &Unit| fails(unit) || emits_finish(unit, finish_sites);
            let observes = |unit: &Unit| reads(unit) || ends(unit);
            let mut longest = 0;
            for (index, kernel) in kernels.iter().enumerate() {
                if kernel.as_ref().is_some_and(|kernel| kernel.delays) {
                    let bound = delay_bound(&sir.processes[index], schema.processes[index].delay);
                    longest = longest.max(bound);
                }
            }
            let domains = events
                .iter()
                .map(|event| {
                    if event.eval_apply.iter().any(ends) {
                        return DomainSettle::Settled;
                    }
                    if !event.eval_apply.iter().any(reads) {
                        return DomainSettle::Pending;
                    }
                    // An asynchronous reset has few edges: a fused copy of
                    // the settle for them would only lengthen the build.
                    if !event.clock {
                        return DomainSettle::Settled;
                    }
                    let fused = &sir.eval_comb_apply_ffs;
                    let aliased = fused.keys().any(|key| {
                        *key != event.signal && topology.canonical(*key) == event.signal
                    });
                    match fused.get(&event.signal) {
                        Some(units) if !aliased => DomainSettle::Fused(units.clone()),
                        _ => DomainSettle::Settled,
                    }
                })
                .collect();
            let several = events.iter().any(|event| {
                event
                    .eval_only
                    .iter()
                    .chain(&event.apply)
                    .flatten()
                    .any(observes)
            });
            Lazy {
                // A kernel's `$finish` is its status.
                kernels: kernels
                    .iter()
                    .enumerate()
                    .map(|(index, kernel)| {
                        let unit = &sir.processes[index];
                        kernel.is_some()
                            && (observes(unit)
                                || !statuses_only(
                                    unit,
                                    schema.processes[index].status,
                                    &[
                                        ProcessStatus::Delay,
                                        ProcessStatus::InactiveDelay,
                                        ProcessStatus::Wait,
                                        ProcessStatus::Pending,
                                        ProcessStatus::Nba,
                                        ProcessStatus::Done,
                                    ],
                                ))
                    })
                    .collect(),
                domains,
                several,
                latest: u64::MAX - longest,
                eager,
            }
        });
        self.found.comb_stores = comb_stores;
        self.found.watched_comb = watched_comb;
        self.found.retained = retained;
        self.found.eager_candidate_instructions = eager_candidate_instructions;
        self.found.lazy = lazy;
    }

    /// The cone of the settle a single clock write invalidates, when it is
    /// small enough to run alone (see [`Incremental`]).
    pub(super) fn incremental_settle<F: FnOnce() -> CombSlicer<'a>>(
        &mut self,
        comb_slicer: &mut Slicer<'a, F>,
    ) {
        let program = self.program;
        let schema = self.schema;
        let sir = self.sir;
        let shared = self.shared;
        let events = &self.found.events;
        let kernels = &self.found.kernels;
        let settle_reads = &self.found.settle_reads;
        let retained = &self.found.retained;
        let lazy = &self.found.lazy;
        let storage = home(shared);
        // A cone leaves out the captures of the processes outside it.
        let incremental = if lazy.is_none()
            && settle_reads.is_some()
            && !self.found.effects
            && !sir.eval_comb.iter().any(fails)
            && sir
                .eval_comb
                .iter()
                .flat_map(instructions)
                .flat_map(read_storage)
                .all(|read| read.region == STABLE_REGION)
            && sir.eval_comb.iter().all(|unit| {
                celox_sir::cfg::SirCfg::analyze(unit).is_ok_and(|cfg| cfg.loops.is_empty())
            }) {
            // A partitioned settle has a cone of its own: the partitions the
            // kernels' event writes dirty and those they publish to.
            let partition_cone = || -> Option<Incremental> {
                let partitioning = schema.settle_partitions.as_ref()?;
                let settle = program.layout().settle.as_ref()?;
                let sources: HashSet<AbsoluteAddr> = kernels
                    .iter()
                    .flatten()
                    .flat_map(|kernel| kernel.writes.iter())
                    .map(|&event| storage(events[event].signal))
                    .collect();
                let source = *sources.iter().min()?;
                let offsets = &program.layout().offsets;
                // The readers of every cone source, the kernels' data
                // writes included: their marks say "only cone sources
                // changed" too (settle::cone_source_homes).
                let homes = program
                    .cone_homes
                    .clone()
                    .unwrap_or_else(|| crate::settle::cone_source_homes(program));
                let seeds: Vec<usize> = homes
                    .into_iter()
                    .flat_map(|home| {
                        settle
                            .objects
                            .iter()
                            .filter(move |object| object.start <= home && home < object.end)
                            .flat_map(|object| object.partitions.iter().map(|&p| p as usize))
                    })
                    .collect();
                let flags =
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, partitioning.flags);
                let (units, cone_partitions) = crate::settle::clock_cone(
                    &sir.eval_comb,
                    &flags,
                    settle.partitions,
                    offsets,
                    seeds,
                )?;
                let size = |units: &[Unit]| units.iter().flat_map(instructions).count();
                tracing::debug!(
                    "[settle-cone] {cone_partitions} of {} partitions, {} of {} instructions",
                    settle.partitions,
                    size(&units),
                    size(&sir.eval_comb)
                );
                (size(&units) * 4 < size(&sir.eval_comb) * 3).then_some(Incremental {
                    source,
                    sources,
                    units,
                    untracked: Some((partitioning.flags, settle.partitions)),
                })
            };
            partition_cone().or_else(|| {
                kernels
                    .iter()
                    .enumerate()
                    .filter_map(|(index, kernel)| kernel.as_ref().map(|kernel| (index, kernel)))
                    .find(|(_, kernel)| kernel.delays && kernel.writes.len() == 1)
                    .and_then(|(index, kernel)| {
                        let source = storage(events[kernel.writes[0]].signal);
                        if !settle_reads.as_ref()?.contains(&source) {
                            return None;
                        }
                        let sources: HashSet<_> = instructions(&sir.processes[index])
                            .flat_map(written_storage)
                            .filter(|write| write.region == STABLE_REGION)
                            .map(|write| storage(write.absolute_addr()))
                            .filter(|address| settle_reads.as_ref().unwrap().contains(address))
                            .collect();
                        // Read-before-write activations retain their existing
                        // per-settle semantics, including their downstream effects.
                        let stateful: Vec<_> = sir.eval_comb_stateful.as_ref().map_or_else(
                            || retained.iter().copied().collect(),
                            |targets| {
                                targets
                                    .iter()
                                    .map(|target| storage(target.absolute_addr()))
                                    .filter(|target| retained.contains(target))
                                    .collect()
                            },
                        );
                        let slicer = comb_slicer.get();
                        let inputs = sources
                            .iter()
                            .copied()
                            .chain(schema.time_slots.iter().copied().map(&storage))
                            .chain(stateful);
                        let units = slicer.affected_slice(inputs);
                        let size = |units: &[Unit]| units.iter().flat_map(instructions).count();
                        (size(&units) * 4 < size(&sir.eval_comb) * 3).then_some(Incremental {
                            source,
                            sources,
                            units,
                            untracked: None,
                        })
                    })
            })
        } else {
            None
        };
        self.found.incremental = incremental;
    }
}
