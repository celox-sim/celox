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
            .updates
            .observe
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
            .updates
            .observe
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
            sir.updates
                .observe
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
        let comb_stores: HashSet<AbsoluteAddr> =
            sir.updates.observe.iter().flat_map(stores).collect();
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
        let retained_candidates = sir.updates.retained.as_ref().map_or_else(
            || written_homes.clone(),
            |stateful| {
                stateful
                    .iter()
                    .map(|target| storage(target.absolute_addr()))
                    .filter(|target| written_homes.contains(target))
                    .collect()
            },
        );
        let retained = retained_storage(&sir.updates.observe, &storage, &retained_candidates);
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
                    && !sir.updates.observe.iter().any(fails)
            })
            .and_then(|_| {
                let stateful = sir.updates.retained.as_ref();
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
                    (size(&eager) * 2 < size(&sir.updates.observe)).then_some(eager)
                });
                eager.or_else(|| {
                    (watched_comb.is_empty()
                        && !retains_state(&sir.updates.observe, &storage, &comb_stores))
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
                    let entry = celox_sir::UpdateEntry::FromInputs;
                    let aliased = sir.updates.groups(entry).any(|(key, _)| {
                        *key != event.signal && topology.canonical(*key) == event.signal
                    });
                    match sir.updates.at(&event.signal, entry) {
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
        let lazy = &self.found.lazy;
        let storage = home(shared);
        // A cone leaves out the captures of the processes outside it.
        let incremental = if lazy.is_none()
            && settle_reads.is_some()
            && !self.found.effects
            && !sir.updates.observe.iter().any(fails)
            && sir
                .updates
                .observe
                .iter()
                .flat_map(instructions)
                .flat_map(read_storage)
                .all(|read| read.region == STABLE_REGION)
            && sir.updates.observe.iter().all(|unit| {
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
                    &sir.updates.observe,
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
                    size(&sir.updates.observe)
                );
                (size(&units) * 4 < size(&sir.updates.observe) * 3).then_some(Incremental {
                    source: Some(source),
                    events: None,
                    sources,
                    units,
                    untracked: Some((partitioning.flags, settle.partitions)),
                })
            };
            partition_cone().or_else(|| {
                if schema.settle_partitions.is_some() {
                    return None;
                }
                self.event_schedule(comb_slicer.get())
            })
        } else {
            None
        };
        self.found.incremental = incremental;
    }

    /// Assign each unit the union of its driving register events after SIR
    /// optimization and storage aliasing. Clock inputs and retained state
    /// have their own activation conditions.
    fn event_schedule(&self, slicer: &CombSlicer<'_>) -> Option<Incremental> {
        let program = self.program;
        let sir = self.sir;
        let schema = self.schema;
        let kernels = &self.found.kernels;
        let events = &self.found.events;
        let storage = home(self.shared);
        let retained = &self.found.retained;
        let reads = self.found.settle_reads.as_ref()?;
        let mut sources: HashSet<_> = self
            .found
            .clocks
            .iter()
            .map(|&event| storage(events[event].signal))
            .filter(|addr| reads.contains(addr))
            .collect();
        // A timed or initially ready kernel can update data together with the clock.
        // Its writes belong to the same input activation.
        for (index, kernel) in kernels.iter().enumerate() {
            if kernel.as_ref().is_some_and(|kernel| {
                (kernel.wait.is_none() || kernel.delays) && !kernel.writes.is_empty()
            }) {
                sources.extend(
                    instructions(&sir.processes[index])
                        .flat_map(written_storage)
                        .filter(|write| write.region == STABLE_REGION)
                        .map(|write| storage(write.absolute_addr()))
                        .filter(|addr| reads.contains(addr)),
                );
            }
        }

        let forward = slicer.forward_dependencies();
        let clock = forward.affected_storage(
            sources
                .iter()
                .copied()
                .chain(schema.time_slots.iter().copied().map(&storage)),
        );
        let retained: Vec<_> = match &sir.updates.retained {
            None => retained.iter().copied().collect(),
            Some(targets) => targets
                .iter()
                .map(|target| storage(target.absolute_addr()))
                .filter(|target| retained.contains(target))
                .collect(),
        };
        let stateful = forward.affected_storage(retained.iter().copied());
        let outputs: Vec<HashSet<_>> = sir
            .updates
            .observe
            .iter()
            .map(|unit| {
                instructions(unit)
                    .flat_map(written_storage)
                    .map(|w| storage(w.absolute_addr()))
                    .collect()
            })
            .collect();
        let mut activations: Vec<_> = outputs
            .iter()
            .map(|writes| Activation {
                clock_inputs: !writes.is_disjoint(&clock),
                every_settle: !writes.is_disjoint(&stateful),
                ..Activation::default()
            })
            .collect();
        let mut register_inputs = HashSet::default();
        let mut writers = HashMap::default();
        for (index, event) in events.iter().enumerate() {
            let mut inputs = HashSet::default();
            let precise = event
                .eval_apply
                .iter()
                .chain(event.eval_only.iter().flatten())
                .chain(event.apply.iter().flatten())
                .flat_map(instructions)
                .all(|inst| {
                    if makes_opaque_calls(inst, &schema.extern_functions)
                        || inst.state_writes().objects().is_none()
                    {
                        return false;
                    }
                    inputs.extend(
                        written_storage(inst)
                            .filter(|write| write.region == STABLE_REGION)
                            .map(|write| storage(write.absolute_addr()))
                            .filter(|address| reads.contains(address)),
                    );
                    true
                });
            if !precise || inputs.is_empty() {
                continue;
            }
            writers.insert(event.signal, index);
            register_inputs.extend(inputs.iter().copied());
            let affected = forward.affected_storage(inputs);
            for (writes, activation) in outputs.iter().zip(&mut activations) {
                if !writes.is_disjoint(&affected) {
                    activation.events.set_bit(index as u64, true);
                }
            }
        }
        // A queued NBA publishes at its due time, independently of the clock
        // that issued it. Its destination and occurrence counters are the
        // roots of that publication, including for indexed memory writes.
        let mut targets: Vec<_> = schema
            .nba_sites
            .iter()
            .map(|site| storage(site.target))
            .collect();
        targets.sort_unstable();
        targets.dedup();
        let observations =
            nba::observations::Observations::for_targets(program, self.shared, &targets);
        let mut nba_targets = HashMap::default();
        for (target, observed) in targets.into_iter().zip(observations) {
            let inputs = std::iter::once(target)
                .chain(observed.counter_addresses().map(&storage))
                .filter(|address| reads.contains(address));
            let affected = forward.affected_storage(inputs);
            if affected.is_empty() {
                continue;
            }
            let index = events.len() + nba_targets.len();
            if index >= program.layout().widths[&self.slots.settle_events] {
                return None;
            }
            nba_targets.insert(target, index);
            for (writes, activation) in outputs.iter().zip(&mut activations) {
                if !writes.is_disjoint(&affected) {
                    activation.events.set_bit(index as u64, true);
                }
            }
        }
        if writers.is_empty() && nba_targets.is_empty() {
            // Opaque clocked writers must invalidate the whole design. A
            // change of the clock level alone can still have a small cone.
            let source = sources.iter().min().copied()?;
            let affected = clock.union(&stateful).copied().collect();
            let units = slicer.slice_affected(&affected);
            let size = |units: &[Unit]| units.iter().flat_map(instructions).count();
            return (size(&units) * 4 < size(&sir.updates.observe) * 3).then_some(Incremental {
                source: Some(source),
                sources,
                units,
                events: None,
                untracked: None,
            });
        }
        let inputs: Vec<HashSet<_>> = sir
            .updates
            .observe
            .iter()
            .map(|unit| {
                instructions(unit)
                    .flat_map(read_storage)
                    .map(|r| storage(r.absolute_addr()))
                    .collect()
            })
            .collect();
        let mut groups = event_order::order(activations, &inputs, &outputs);
        let start = (events.len() + nba_targets.len()).next_multiple_of(64);
        let mut publications = HashMap::default();
        let mut event_readers = None;
        let mut changed_outputs: HashMap<_, Vec<_>> = HashMap::default();
        let sizes: Vec<_> = sir
            .updates
            .observe
            .iter()
            .map(|unit| instructions(unit).count())
            .collect();
        if start + 64 <= program.layout().widths[&self.slots.settle_events] {
            let banks = event_order::read_banks(events, &inputs, &register_inputs, storage);
            groups = event_order::register_groups(groups, &banks, &sizes);
        }
        let count = groups.iter().filter(|(a, _)| !a.every_settle).count();
        if count > 0 && start + 64 <= program.layout().widths[&self.slots.settle_events] {
            // Publish a ready-to-use consumer mask. Even a wide clock
            // domain still requires only one word at the settle boundary.
            // Adjacent groups can share a bit if the schedule is larger.
            if count <= 63 {
                event_readers = Some(vec![0; events.len() + nba_targets.len()]);
            }
            let mut sinks: HashMap<_, u64> = HashMap::default();
            for (i, (activation, members)) in groups
                .iter_mut()
                .filter(|(a, _)| !a.every_settle)
                .enumerate()
            {
                let bit = i * count.min(63) / count;
                if let Some(readers) = &mut event_readers {
                    for (word, mut mask) in activation.events.iter_u64_digits().enumerate() {
                        while mask != 0 {
                            readers[word * 64 + mask.trailing_zeros() as usize] |= 1u64 << bit;
                            mask &= mask - 1;
                        }
                    }
                }
                activation.events.set_bit((start + bit) as u64, true);
                for &unit in members.iter() {
                    for &addr in &outputs[unit] {
                        *sinks.entry(addr).or_default() |= 1u64 << bit;
                    }
                }
            }
            register_inputs.extend(
                sir.processes
                    .iter()
                    .take(schema.processes.len())
                    .flat_map(instructions)
                    .flat_map(written_storage)
                    .filter(|w| w.region == STABLE_REGION)
                    .map(|w| storage(w.absolute_addr()))
                    .filter(|a| reads.contains(a)),
            );
            let boundaries = if event_readers.is_some() {
                self.publication_boundaries(&groups, &inputs, &outputs, &sinks, &register_inputs)
            } else {
                HashMap::default()
            };
            let readers = forward.reader_masks(sinks, &boundaries);
            for (addr, producer) in boundaries {
                let later = readers[&addr] & !(producer | (producer - 1));
                if later != 0 {
                    let bit = producer.trailing_zeros() as usize;
                    changed_outputs.entry(bit).or_default().push((addr, later));
                }
            }
            for outputs in changed_outputs.values_mut() {
                outputs.sort_unstable_by_key(|&(addr, readers)| {
                    (readers, program.layout().offsets[&addr], addr)
                });
            }
            publications.extend(register_inputs.into_iter().map(|addr| {
                // A change with only retained readers still needs a settle.
                (addr, readers.get(&addr).copied().unwrap_or(1u64 << 63))
            }));
        }
        if tracing::enabled!(target: "celox::scheduler", tracing::Level::DEBUG) {
            let active = |index: usize| {
                groups
                    .iter()
                    .filter(|(a, _)| a.every_settle || a.events.bit(index as u64))
                    .map(|(_, r)| r.iter().map(|&unit| sizes[unit]).sum::<usize>())
                    .sum::<usize>()
            };
            tracing::debug!(target: "celox::scheduler", groups=groups.len(), units=sizes.len(),
                        full=sizes.iter().sum::<usize>(),
                        events=?events.iter().enumerate().filter(|(_, event)| writers.contains_key(&event.signal))
                            .map(|(i,event)| (program.get_path(&event.signal),active(i))).collect::<Vec<_>>(),
                        "static event schedule");
        }
        Some(Incremental {
            source: sources.iter().copied().min(),
            sources,
            units: Vec::new(),
            events: Some(EventSchedule {
                writers,
                nba_targets,
                publications,
                publication_offset: start,
                event_readers,
                changed_outputs,
                groups,
            }),
            untracked: None,
        })
    }

    /// Stop eager propagation at results shared by many readers. A packed
    /// input can also fan out through several units that each publish some of
    /// its lanes. Include the smaller units fed by that same input.
    fn publication_boundaries(
        &self,
        groups: &[(Activation, Vec<usize>)],
        inputs: &[HashSet<AbsoluteAddr>],
        outputs: &[HashSet<AbsoluteAddr>],
        sinks: &HashMap<AbsoluteAddr, u64>,
        register_inputs: &HashSet<AbsoluteAddr>,
    ) -> HashMap<AbsoluteAddr, u64> {
        const MIN_FANOUT: usize = 8;
        let layout = self.program.layout();
        let home = home(self.shared);
        let mut excluded = self.found.retained.clone();
        excluded.extend(register_inputs);
        excluded.extend(self.schema.nba_sites.iter().map(|site| home(site.target)));
        // A snapshot of the representative must cover every alias, including
        // unknown bits. Unpacked aliases can have a different element stride.
        for (alias, representative) in self.shared {
            if layout.widths.get(alias) != layout.widths.get(representative)
                || layout.is_4states.get(alias) != layout.is_4states.get(representative)
                || layout.unpacked_arrays.contains_key(alias)
            {
                excluded.insert(*representative);
            }
        }

        excluded.extend(
            self.sir
                .updates
                .observe
                .iter()
                .flat_map(instructions)
                .flat_map(written_storage)
                .filter(|w| w.region != STABLE_REGION)
                .map(|w| home(w.absolute_addr())),
        );
        let mut uses = HashMap::default();
        for (group, (a, members)) in groups.iter().enumerate() {
            for &i in members {
                if a.every_settle {
                    excluded.extend(&outputs[i]);
                }
                for &addr in &inputs[i] {
                    let (_, count) = uses.entry(addr).or_insert((group, 0usize));
                    *count += 1;
                }
            }
        }
        let wide_fanout: HashSet<_> = inputs
            .iter()
            .zip(outputs)
            .filter(|(reads, writes)| reads.len() == 1 && writes.len() >= MIN_FANOUT)
            .flat_map(|(reads, _)| reads.iter().copied())
            .collect();
        let mut boundaries = HashMap::default();
        for (group, (_, members)) in groups.iter().enumerate() {
            for &i in members {
                let distributed = inputs[i].len() == 1 && !inputs[i].is_disjoint(&wide_fanout);
                for addr in &outputs[i] {
                    // One producer group must finish every write before the
                    // first reader. Opaque or independently writable storage
                    // cannot use a comparison around this group.
                    let Some(&bit) = sinks.get(addr) else {
                        continue;
                    };
                    if bit.is_power_of_two()
                        && uses.get(addr).is_some_and(|&(first, n)| {
                            first > group && (n >= MIN_FANOUT || distributed)
                        })
                        && !excluded.contains(addr)
                        && layout
                            .widths
                            .get(addr)
                            .is_some_and(|w| (1..=256).contains(w))
                        && !layout.unpacked_arrays.contains_key(addr)
                    {
                        boundaries.insert(*addr, bit);
                    }
                }
            }
        }
        boundaries
    }
}
