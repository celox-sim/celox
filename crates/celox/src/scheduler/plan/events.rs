//! The events of a design, what its units write and what observes it, and
//! the events host clocks drive.
use super::*;

impl<'a> PlanBuilder<'a> {
    /// The events of the design with the units of their domains, and the
    /// width of a mask of them.
    pub(super) fn events(&mut self) -> Result<(), String> {
        let program = self.program;
        let slots = self.slots;
        let wiring_clocks = self.wiring_clocks;
        let model = self.model;
        let topology = self.topology;
        let sir = self.sir;
        let native_observers = self.native_observers;
        let shared = self.shared;
        let storage = home(shared);
        let addresses = crate::edge_plans::event_addresses(program);
        let event_bits = addresses.len().next_multiple_of(64).max(MIN_EVENT_BITS);
        // The masks of the edges and levels of a slot live in these slots,
        // which the frontend sized for the events it expected.
        for (name, slot) in [
            ("levels", slots.levels),
            ("settle_events", slots.settle_events),
            ("unknown_levels", slots.unknown_levels),
            ("exit_arg", slots.exit_arg),
        ] {
            if program.layout().widths.get(&slot).copied().unwrap_or(0) < event_bits {
                return Err(format!(
                    "the design has more events than its scheduler's {name} slot holds"
                ));
            }
        }
        let mut events = Vec::with_capacity(addresses.len());
        let mut event_storage: HashMap<AbsoluteAddr, Vec<usize>> = HashMap::default();
        let mut domains = HashMap::<_, Vec<_>>::default();
        for (key, update) in &sir.updates.events {
            domains
                .entry(topology.canonical(*key))
                .or_default()
                .push((key, update));
        }
        for &signal in &addresses {
            let (active, clock) = match model.kinds.get(&signal) {
                Some(DomainKind::ClockPosedge) => (1, true),
                Some(DomainKind::ClockNegedge) => (0, true),
                Some(DomainKind::ResetAsyncHigh) => (1, false),
                Some(DomainKind::ResetAsyncLow) => (0, false),
                Some(DomainKind::Other) | None if native_observers => (1, true),
                _ => return Err("an event is not a clock or reset edge".into()),
            };
            let domain = domains.get(&signal).into_iter().flatten();
            let units = |entry| -> Result<Option<Vec<Unit>>, String> {
                if native_observers {
                    // Match NativeBackend::compile_ff_group exactly: aliases
                    // bind the first kernel of their canonical domain. The
                    // frontend may clone that kernel under several aliases;
                    // concatenating those copies would execute it repeatedly.
                    return Ok(domain
                        .clone()
                        .find_map(|(_, update)| update.entry(entry))
                        .cloned());
                }
                let aliased = domain.clone().any(|(key, update)| {
                    **key != signal && update.entry(entry).is_some_and(|units| !units.is_empty())
                });
                if aliased {
                    return Err("an event domain has no separate units".into());
                }
                Ok(sir.updates.at(&signal, entry).cloned())
            };
            let eval_only = units(celox_sir::UpdateEntry::Sample)?;
            let apply = units(celox_sir::UpdateEntry::Publish)?;
            // An event no RTL process waits for, such as a TB clock that
            // only paces initial processes, has an empty domain.
            let eval_apply = match units(celox_sir::UpdateEntry::Transition)? {
                Some(units) => units,
                None if eval_only.is_none() && apply.is_none() => Vec::new(),
                None => return Err("an event domain has no separate units".into()),
            };
            // Nothing to evaluate is also nothing to apply.
            let (eval_only, apply) = if eval_apply.is_empty() {
                (eval_only.or(Some(Vec::new())), apply.or(Some(Vec::new())))
            } else {
                (eval_only, apply)
            };
            let event = Event {
                signal,
                width: *program
                    .layout()
                    .widths
                    .get(&signal)
                    .ok_or("an event signal has no storage")?,
                active,
                any_settled: matches!(model.kinds.get(&signal), Some(DomainKind::Other) | None),
                clock,
                eval_apply,
                active_commit: model.active_commits.contains(&signal),
                aliases: wiring_clocks
                    .then(|| crate::clock_aliases::extract(program, signal))
                    .flatten(),
                eval_only,
                apply,
            };
            let same_signal = event_storage.entry(storage(signal)).or_default();
            if !native_observers && !same_signal.is_empty() {
                return Err("two events share a signal".into());
            }
            same_signal.push(events.len());
            events.push(event);
        }
        self.found.addresses = addresses;
        self.found.event_bits = event_bits;
        self.found.events = events;
        self.found.event_storage = event_storage;
        Ok(())
    }

    /// What the units write, and what observes a blocking result before the
    /// NBA region: the waits and their combinational inputs, the occurrence
    /// counters, the events and stateful combinational logic. Returns the
    /// slicer of combinational logic when a wait reads anything.
    pub(super) fn observers<'s, F: FnOnce() -> CombSlicer<'a>>(
        &mut self,
        comb_slicer: &'s mut Slicer<'a, F>,
    ) -> Option<&'s CombSlicer<'a>> {
        let schema = self.schema;
        let model = self.model;
        let topology = self.topology;
        let sir = self.sir;
        let shared = self.shared;
        let events = &self.found.events;
        let storage = home(shared);
        let written = written_events(&self.found.event_storage, storage);
        let stores = unit_stores(storage);
        let rtl_units = || rtl_units_of(sir);
        // The events RTL logic drives: clocks derived from the events the
        // scheduler's slots change, and only those (checked below).
        let rtl_written: BTreeSet<usize> = rtl_units().flat_map(written).collect();
        let rtl_stores: HashSet<AbsoluteAddr> = rtl_units().flat_map(stores).collect();
        let opaque_comb = sir
            .updates
            .observe
            .iter()
            .any(|unit| opaque(unit, &schema.extern_functions));
        let kernel_stores: Vec<HashSet<AbsoluteAddr>> = sir
            .processes
            .iter()
            .take(schema.processes.len())
            .map(stores)
            .collect();
        let control_slots: HashSet<_> = schema
            .processes
            .iter()
            .flat_map(|process| process.iter())
            .copied()
            .map(storage)
            .collect();
        let wait_reads: Vec<_> = schema
            .wait_sites
            .iter()
            .flat_map(celox_design::WaitSite::reads)
            .collect();
        let waited: HashSet<AbsoluteAddr> = wait_reads.iter().map(|s| storage(*s)).collect();
        // Storage whose changes bump an occurrence counter: the reference
        // counts the nonblocking updates it publishes into it, which the
        // scheduler's publication does not.
        let probed: HashSet<AbsoluteAddr> = model
            .probes
            .iter()
            .map(|(source, _)| storage(source.id))
            .chain(
                model
                    .counted
                    .iter()
                    .map(|counted| storage(counted.slots.signal)),
            )
            .collect();
        // Blocking results need an intermediate Active-region settle when
        // they can wake a process, move a clock, or feed stateful combinational
        // logic before NBA publication.
        // Otherwise the existing evaluate/apply sequence has the same visible
        // ordering: processes run first, blocking results become stable during
        // evaluation, and nonblocking results publish afterward (LRM 4.5).
        let mut active_observers: HashSet<AbsoluteAddr> = waited
            .iter()
            .chain(&probed)
            .copied()
            .chain(events.iter().map(|event| storage(event.signal)))
            .collect();
        let wait_slicer = if wait_reads.is_empty() {
            None
        } else {
            Some(comb_slicer.get())
        };
        let observed_inputs = wait_slicer
            .as_ref()
            .map(|slicer| slicer.input_signals(wait_reads.iter().copied()))
            .unwrap_or_default();
        active_observers.extend(observed_inputs.into_iter().map(storage));
        // An external call's state dependencies cannot be recovered from SIR
        // loads. Preserve every blocking publication before NBA when the
        // combinational observer cone contains such a call.
        if opaque_comb {
            active_observers.extend(rtl_stores.iter().copied());
        }
        // A stateful combinational consumer can retain an intermediate
        // blocking result even when no waiter or clock reads it. Include all
        // combinational inputs so indirect cones are covered too. Missing
        // statefulness metadata is conservative for the same reason.
        if sir
            .updates
            .retained
            .as_ref()
            .is_none_or(|targets| !targets.is_empty())
        {
            active_observers.extend(sir.updates.observe.iter().flat_map(loads).map(storage));
        }
        let mut unknown_gate = false;
        for clock in topology.combinational_derived.values().flatten() {
            match &clock.gate {
                Some(gate) => active_observers.extend(gate.iter().filter_map(|op| match op {
                    celox_design::GateOp::Load { address, .. } => Some(storage(*address)),
                    _ => None,
                })),
                None => match &clock.inputs {
                    Some(inputs) => active_observers.extend(inputs.iter().copied().map(storage)),
                    None => unknown_gate = true,
                },
            }
        }
        let active_commits: HashSet<usize> = events
            .iter()
            .enumerate()
            .filter(|(_, event)| model.active_commits.contains(&event.signal))
            .filter(|(_, event)| {
                // A container operation of the heap reaches the design only
                // through the handle the unit stores, as the testbench memory
                // `bit [7:0] mem [bit [31:0]]` a negedge process writes does.
                unknown_gate
                    || event.eval_only.as_ref().is_none_or(|units| {
                        units.iter().any(|unit| {
                            let constants = unit_constants(unit);
                            instructions(unit).any(|inst| {
                                (makes_opaque_calls(inst, &schema.extern_functions)
                                    && !container_value_call(inst, &constants))
                                    || written_storage(inst).any(|target| {
                                        target.region == STABLE_REGION
                                            && active_observers
                                                .contains(&storage(target.absolute_addr()))
                                    })
                            })
                        })
                    })
            })
            .map(|(index, _)| index)
            .collect();
        self.found.rtl_written = rtl_written;
        self.found.rtl_stores = rtl_stores;
        self.found.kernel_stores = kernel_stores;
        self.found.control_slots = control_slots;
        self.found.waited = waited;
        self.found.probed = probed;
        self.found.active_observers = active_observers;
        self.found.active_commits = active_commits;
        wait_slicer
    }

    /// The events a host periodic clock may drive in the slots: the clocks,
    /// and the resets of a compiled testbench, that no design code writes.
    /// An input only waiting processes sense is none of them when other
    /// events derive from it.
    pub(super) fn host_clocks(&mut self) -> Result<(), String> {
        let program = self.program;
        let slots = self.slots;
        let schema = self.schema;
        let model = self.model;
        let topology = self.topology;
        let shared = self.shared;
        let events = &self.found.events;
        let rtl_stores = &self.found.rtl_stores;
        let kernel_stores = &self.found.kernel_stores;
        let storage = home(shared);
        let mut nba_targets: HashMap<AbsoluteAddr, usize> = HashMap::default();
        for process in &model.processes {
            for site in &process.nbas {
                *nba_targets.entry(storage(site.target)).or_default() += 1;
            }
        }
        self.found.nba_targets = nba_targets;
        let nba_targets = &self.found.nba_targets;
        // Storage no design code writes: only the host changes it, by
        // writes between runs or by a periodic clock at the start of a slot.
        // Clock slots go to the clock events among it: each costs the unit
        // its domains' code even when no clock runs, and the host drives
        // asynchronous resets by writes.
        let undriven = undriven_storage(schema, rtl_stores, kernel_stores, nba_targets);
        // A slot makes the events that derive from its event combinationally,
        // which a run otherwise holds stable, cost every slot their waves.
        let source_of_events = |signal: &AbsoluteAddr| {
            topology
                .combinational_derived
                .get(signal)
                .is_some_and(|derived| !derived.is_empty())
        };
        // The resets a compiled testbench writes: its unit applies those
        // writes as edges of their slots, with its clocks'. A reset other
        // events derive from combinationally keeps its writes in the host.
        let testbench_resets: HashSet<AbsoluteAddr> = program
            .runtime
            .testbench
            .iter()
            .flat_map(|tb| tb.statements())
            .flat_map(testbench_resets)
            .filter(|reset| !source_of_events(reset))
            .map(storage)
            .collect();
        // An event only waiting processes sense is whatever the design
        // waits for, as often a reset or an enable as a clock, and it has
        // no domain a slot would run. With events deriving from it, it too
        // stays in the host: a clock on it runs from the queue, and the
        // events deriving from it stay as stable as they were before a
        // process waited for it.
        let waited: HashSet<AbsoluteAddr> = host_inputs::signals(schema)
            .into_iter()
            .map(|term| topology.canonical(term))
            .collect();
        let waited_source = |event: &Event| {
            event.eval_apply.is_empty()
                && event.eval_only.as_ref().is_none_or(Vec::is_empty)
                && event.apply.as_ref().is_none_or(Vec::is_empty)
                && waited.contains(&event.signal)
                && source_of_events(&event.signal)
        };
        let clocks: Vec<usize> = events
            .iter()
            .enumerate()
            .filter(|(_, event)| {
                ((event.clock && !waited_source(event))
                    || testbench_resets.contains(&storage(event.signal)))
                    && event.width == 1
                    && undriven(storage(event.signal))
            })
            .map(|(index, _)| index)
            .collect();
        if clocks.len() > program.layout().widths[&slots.clock_armed]
            || clocks.len() > program.layout().widths[&slots.clock_value]
            || clocks.len() > program.layout().widths[&slots.clock_next] / SCHEDULER_TIME_WIDTH
            || clocks.len() > program.layout().widths[&slots.clock_period] / SCHEDULER_TIME_WIDTH
        {
            return Err("the compiled host clock slots have insufficient storage".into());
        }

        self.found.clocks = clocks;
        Ok(())
    }
}
