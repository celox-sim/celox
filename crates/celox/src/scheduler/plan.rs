//! Building the [`Plan`] of a design: which kernels and events the compiled
//! scheduler runs, and how its slots settle and order their domains.
use super::*;

impl Plan {
    pub(super) fn new(
        program: &LaidOutProgram,
        slots: &SchedulerSlots<AbsoluteAddr>,
        wiring_clocks: bool,
    ) -> Result<Self, String> {
        let schema = &program.runtime.runtime_schema;
        let model = ScheduleModel::derive(&program.design, schema);
        let topology = &model.events;
        let native_observers = cfg!(all(target_arch = "x86_64", not(feature = "arm64-codegen")))
            && schema
                .extern_functions
                .iter()
                .any(|function| function.name == crate::simulator::native_io::SYMBOL);
        if !native_observers && !schema.comb_observers.is_empty() {
            return Err("the design observes combinational processes".into());
        }
        if slots.wake.len() != schema.processes.len() {
            return Err("the scheduler slots do not match the kernels".into());
        }
        let sir = &program.sir;
        let comb_fatal = sir
            .eval_comb
            .iter()
            .any(|unit| instructions(unit).any(crate::ir::may_raise_fatal_capture));
        if comb_fatal && !native_observers {
            return Err("combinational logic has fatal assertions".into());
        }
        let rtl_units = || {
            sir.eval_comb
                .iter()
                .chain(sir.eval_apply_ffs.values().flatten())
                .chain(sir.eval_only_ffs.values().flatten())
                .chain(sir.apply_ffs.values().flatten())
        };
        let planned = || {
            rtl_units()
                .chain(sir.eval_comb_apply_ffs.values().flatten())
                .chain(sir.processes.iter().take(schema.processes.len()))
                .flat_map(instructions)
        };
        // A native kernel may read and write any state, which none of the
        // storage sets below can name (see `read_storage`). The raw words of
        // a wide commit are shadowed by typed commits of the same registers.
        if planned().any(|instruction| {
            instruction.accesses_unknown_state()
                && !crate::settle::is_wide_commit_access(instruction)
        }) {
            return Err("a unit may access any state".into());
        }

        let shared = program.shared_storage();
        let storage = |address: AbsoluteAddr| shared.get(&address).copied().unwrap_or(address);
        let addresses = crate::edge_plans::event_addresses(program);
        let event_bits = addresses.len().next_multiple_of(64).max(MIN_EVENT_BITS);
        // The masks of the edges and levels of a slot live in these slots,
        // which the frontend sized for the events it expected.
        for (name, slot) in [
            ("levels", slots.levels),
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
        for &signal in &addresses {
            let (active, clock) = match model.kinds.get(&signal) {
                Some(DomainKind::ClockPosedge) => (1, true),
                Some(DomainKind::ClockNegedge) => (0, true),
                Some(DomainKind::ResetAsyncHigh) => (1, false),
                Some(DomainKind::ResetAsyncLow) => (0, false),
                Some(DomainKind::Other) | None if native_observers => (1, true),
                _ => return Err("an event is not a clock or reset edge".into()),
            };
            let units =
                |map: &HashMap<AbsoluteAddr, Vec<Unit>>| -> Result<Option<Vec<Unit>>, String> {
                    if native_observers {
                        // Match NativeBackend::compile_ff_group exactly: aliases
                        // bind the first kernel of their canonical domain. The
                        // frontend may clone that kernel under several aliases;
                        // concatenating those copies would execute it repeatedly.
                        return Ok(map
                            .iter()
                            .find(|(key, _)| topology.canonical(**key) == signal)
                            .map(|(_, units)| units.clone()));
                    }
                    // An alias's domain must be the event's own; one with
                    // nothing to run is no domain at all.
                    let aliased = map.iter().any(|(key, units)| {
                        *key != signal && topology.canonical(*key) == signal && !units.is_empty()
                    });
                    if aliased {
                        return Err("an event domain has no separate units".into());
                    }
                    Ok(map.get(&signal).cloned())
                };
            let eval_only = units(&sir.eval_only_ffs)?;
            let apply = units(&sir.apply_ffs)?;
            // An event no RTL process waits for, such as a TB clock that
            // only paces initial processes, has an empty domain.
            let eval_apply = match units(&sir.eval_apply_ffs)? {
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
        // Event signals change only where the scheduler sees them change.
        let written = |unit: &Unit| -> Vec<usize> {
            let mut written: Vec<usize> = instructions(unit)
                .flat_map(written_storage)
                .filter_map(|target| event_storage.get(&storage(target.absolute_addr())))
                .flatten()
                .copied()
                .collect();
            written.sort_unstable();
            written.dedup();
            written
        };
        // The events RTL logic drives: clocks derived from the events the
        // scheduler's slots change, and only those (checked below).
        let rtl_written: BTreeSet<usize> = rtl_units().flat_map(written).collect();
        // The storage each unit writes.
        let stores = |unit: &Unit| -> HashSet<AbsoluteAddr> {
            instructions(unit)
                .flat_map(written_storage)
                .map(|target| storage(target.absolute_addr()))
                .collect()
        };
        let rtl_stores: HashSet<AbsoluteAddr> = rtl_units().flat_map(stores).collect();
        let opaque = |unit: &Unit| instructions(unit).any(makes_host_calls);
        let opaque_comb = sir.eval_comb.iter().any(opaque);
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
        // All dependency and settle slices inspect the same immutable units.
        // Index their definitions and control dependencies only once per plan,
        // and only if one of these consumers actually needs a slice.
        let comb_slicer =
            std::cell::LazyCell::new(|| CombSlicer::new(&sir.eval_comb, shared.clone()));
        let wait_slicer = (!wait_reads.is_empty()).then(|| &*comb_slicer);
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
            .eval_comb_stateful
            .as_ref()
            .is_none_or(|targets| !targets.is_empty())
        {
            active_observers.extend(sir.eval_comb.iter().flat_map(loads).map(storage));
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
                unknown_gate
                    || event.eval_only.as_ref().is_none_or(|units| {
                        units.iter().flat_map(instructions).any(|inst| {
                            makes_host_calls(inst)
                                || written_storage(inst).any(|target| {
                                    target.region == STABLE_REGION
                                        && active_observers
                                            .contains(&storage(target.absolute_addr()))
                                })
                        })
                    })
            })
            .map(|(index, _)| index)
            .collect();
        let mut nba_targets: HashMap<AbsoluteAddr, usize> = HashMap::default();
        for process in &model.processes {
            for site in &process.nbas {
                *nba_targets.entry(storage(site.target)).or_default() += 1;
            }
        }
        // Storage no design code writes: only the host changes it, by
        // writes between runs or by a periodic clock at the start of a slot.
        // Clock slots go to the clock events among it: each costs the unit
        // its domains' code even when no clock runs, and the host drives
        // asynchronous resets by writes.
        let undriven = |signal: AbsoluteAddr| {
            !rtl_stores.contains(&signal)
                && !kernel_stores
                    .iter()
                    .zip(&schema.processes)
                    .any(|(stores, process)| {
                        process.kind != celox_design::ProcessKind::Callable
                            && stores.contains(&signal)
                    })
                && !nba_targets.contains_key(&signal)
        };
        // The resets a compiled testbench writes: its unit applies those
        // writes as edges of their slots, with its clocks'. A reset other
        // events derive from combinationally keeps its writes in the host:
        // a slot would make those derived events, which a run otherwise
        // holds stable, cost every slot their waves.
        let testbench_resets: HashSet<AbsoluteAddr> = program
            .runtime
            .testbench
            .iter()
            .flat_map(|tb| tb.statements())
            .flat_map(testbench_resets)
            .filter(|&reset| {
                topology
                    .combinational_derived
                    .get(&reset)
                    .is_none_or(|derived| derived.is_empty())
            })
            .map(storage)
            .collect();
        let clocks: Vec<usize> = events
            .iter()
            .enumerate()
            .filter(|(_, event)| {
                (event.clock || testbench_resets.contains(&storage(event.signal)))
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

        let service_function = schema
            .extern_functions
            .iter()
            .position(|function| function.name == crate::simulator::native_io::SYMBOL)
            .and_then(|index| u32::try_from(index).ok());
        let ordered_available = cfg!(all(target_arch = "x86_64", not(feature = "arm64-codegen")))
            && service_function.is_some();
        let nba_processes: HashSet<usize> = model
            .processes
            .iter()
            .enumerate()
            .filter(|(_, process)| {
                service_function.is_some()
                    && !process.nbas.is_empty()
                    && process.nbas.iter().all(|site| {
                        let target = storage(site.target);
                        site.unit
                            || ordered_available
                            || (site.delay == 0
                                && (!event_storage.contains_key(&target)
                                    && site.position.is_none()
                                    && !program
                                        .layout()
                                        .unpacked_arrays
                                        .contains_key(&site.target)
                                    && !probed.contains(&target)
                                    && !rtl_stores.contains(&target)
                                    && !kernel_stores
                                        .iter()
                                        .any(|stores| stores.contains(&target))))
                    })
            })
            .map(|(index, _)| index)
            .collect();
        let yielding: HashSet<usize> = schema
            .processes
            .iter()
            .zip(&sir.processes)
            .enumerate()
            .filter(|(_, (slots, unit))| returns_status(unit, slots.status, ProcessStatus::Yield))
            .map(|(index, _)| index)
            .collect();
        let service_processes: HashSet<usize> = model
            .processes
            .iter()
            .enumerate()
            .filter(|(index, process)| {
                service_function.is_some()
                    && (!process.host_requests.is_empty()
                        || yielding.contains(index)
                        || nba_processes.contains(index))
                    && process.host_requests.iter().all(|site| {
                        crate::simulator::native_io::supported(site)
                            && site
                                .args
                                .iter()
                                .all(|arg| !program.layout().unpacked_arrays.contains_key(&arg.id))
                    })
            })
            .map(|(index, _)| index)
            .collect();
        let mut kernels: Vec<Option<Kernel>> = Vec::with_capacity(schema.processes.len());
        let mut fallback = Vec::with_capacity(schema.processes.len());
        // Kernels that only delay first: the signals they write are those a
        // waiting kernel's edge may be on.
        for (index, slots) in schema.processes.iter().enumerate() {
            let process = &model.processes[index];
            let unit = &sir.processes[index];
            let reason = if slots.kind == celox_design::ProcessKind::Callable {
                Some("an explicitly invoked kernel")
            } else if slots.kind == celox_design::ProcessKind::Initializer {
                Some("a declaration initializer")
            } else if slots.kind == celox_design::ProcessKind::Final {
                Some("a final procedure")
            } else if process.frame.is_some() {
                Some("the process has dynamic activation frames")
            } else if slots.call_top.is_some() {
                // The runtime keeps the frames of a call it suspended inside.
                Some("the process suspends inside function calls")
            } else if slots.program.is_some() {
                Some("a program process runs in the reactive region set")
            } else if !process.host_requests.is_empty() && !service_processes.contains(&index) {
                Some("the process makes host requests")
            } else if service_processes.contains(&index)
                && !written(unit).is_empty()
                && !ordered_available
            {
                Some("a synchronous host-service process drives an event signal")
            } else if yielding.contains(&index) && !service_processes.contains(&index) {
                Some("a loop checkpoint requires native services")
            } else if !process.waits.is_empty() {
                None
            } else if !slots.const_delays && !ordered_available {
                Some("a delay is not a constant of at least one time unit")
            } else if !process.nbas.is_empty() && !nba_processes.contains(&index) {
                Some("the process has nonblocking assignments")
            } else if !statuses_only(unit, slots.status, &DELAY_STATUSES) {
                Some("the process suspends other than for a delay")
            } else {
                None
            };
            match reason {
                Some(reason) => {
                    kernels.push(None);
                    fallback.push(Some(reason.to_string()));
                }
                None if process.waits.is_empty() => {
                    let writes = written(unit);
                    kernels.push(Some(Kernel {
                        writes,
                        wait: None,
                        delays: true,
                    }));
                    fallback.push(None);
                }
                None => {
                    kernels.push(None);
                    fallback.push(None);
                }
            }
        }
        // How many kernels that only delay write each storage. A waiting
        // kernel's signal has one such writer, which runs once a slot, so
        // the waiting kernel wakes at most once a slot.
        let mut delay_writers: HashMap<AbsoluteAddr, usize> = HashMap::default();
        for (_, stores) in kernels
            .iter()
            .zip(&kernel_stores)
            .filter(|(kernel, _)| kernel.is_some())
        {
            for store in stores {
                *delay_writers.entry(*store).or_default() += 1;
            }
        }
        // Storage the other kernels write, waiting ones included.
        let other_stores: HashSet<AbsoluteAddr> = kernels
            .iter()
            .zip(&kernel_stores)
            .filter(|(kernel, _)| kernel.is_none())
            .flat_map(|(_, stores)| stores.iter().copied())
            .collect();

        let opaque_dependencies = rtl_units()
            .chain(
                sir.processes
                    .iter()
                    .enumerate()
                    .filter_map(|(index, unit)| {
                        (index < fallback.len() && fallback[index].is_none()).then_some(unit)
                    }),
            )
            .any(opaque);
        for index in 0..schema.processes.len() {
            let process = &model.processes[index];
            if process.waits.is_empty() || fallback[index].is_some() {
                continue;
            }
            let baseline = slots.baseline[index];
            let slots = &schema.processes[index];
            let unit = &sir.processes[index];
            let wait = (|| -> Result<(Wait, bool), &'static str> {
                let scalar = process.compact_wait_terms(|signal| {
                    program.layout().widths.get(&signal).copied().unwrap_or(0)
                });
                let raw_site = |listed: &[celox_design::schedule::TermModel<AbsoluteAddr>]| {
                    let dimensions = listed
                        .iter()
                        .map(|term| {
                            let width = *program.layout().widths.get(&term.signal)?;
                            Some((
                                width,
                                program.layout().four_state
                                    && program
                                        .layout()
                                        .is_4states
                                        .get(&term.signal)
                                        .copied()
                                        .unwrap_or(true),
                                model
                                    .counted
                                    .iter()
                                    .any(|counted| counted.slots.signal == term.signal),
                                term.gate.is_some_and(|gate| gate.counters.is_some()),
                            ))
                        })
                        .collect::<Option<Vec<_>>>()
                        .ok_or("the process waits for an unsupported signal representation")?;
                    let raw =
                        celox_design::schedule::CompiledWaitLayout::with_gate_counters(&dimensions)
                            .ok_or("the wait snapshot layout overflows")?;
                    if program.layout().widths[&baseline] < raw.bits {
                        tracing::debug!(target: "celox::scheduler", process = index,
                                available_bits = program.layout().widths[&baseline],
                                required_bits = raw.bits, scalar = raw.scalar_byte,
                                "compiled wait snapshot cannot hold the expression list");
                        return Err("the compiled wait snapshot has insufficient storage");
                    }
                    Ok(Wait {
                        terms: listed.iter().map(|term| (term.signal, term.kind)).collect(),
                        site_terms: Vec::new(),
                        counters: listed
                            .iter()
                            .map(|term| {
                                term.gate
                                    .and_then(|gate| gate.counters)
                                    .map(|counters| counters.all)
                                    .or_else(|| {
                                        model
                                            .counted
                                            .iter()
                                            .find(|counted| counted.slots.signal == term.signal)
                                            .map(|counted| counted.slots.counter(term.kind))
                                    })
                            })
                            .collect(),
                        raw: Some(raw),
                        alternatives: Vec::new(),
                        gates: listed.iter().map(|term| term.gate).collect(),
                        reactive: false,
                        sites: Vec::new(),
                    })
                };
                let (terms, site_terms, raw, alternatives, gates) = match scalar {
                    Some((terms, sites)) => (terms, sites, None, Vec::new(), Vec::new()),
                    None => {
                        if let Some(listed) = process.common_wait_terms() {
                            let wait = raw_site(listed)?;
                            (
                                wait.terms,
                                vec![0; process.waits.len()],
                                wait.raw,
                                Vec::new(),
                                wait.gates,
                            )
                        } else {
                            let mut unique = Vec::new();
                            let mut alternatives = Vec::new();
                            let mut site_terms = Vec::new();
                            for site in &process.waits {
                                let index = match unique.iter().position(|known| *known == site) {
                                    Some(index) => index,
                                    None => {
                                        let index = unique.len();
                                        unique.push(site);
                                        alternatives.push(raw_site(site)?);
                                        index
                                    }
                                };
                                site_terms.push(index);
                            }
                            let mut terms = Vec::new();
                            for term in alternatives.iter().flat_map(|wait| &wait.terms) {
                                if !terms.contains(term) {
                                    terms.push(*term);
                                }
                            }
                            (terms, site_terms, None, alternatives, Vec::new())
                        }
                    }
                };
                // Unknown effects can wake a waiter more than once per slot.
                // Use the generated work list and its budget, not the compact
                // single-writer proof, for those activations.
                let mut reactive = opaque_dependencies;
                for &(signal, _) in &terms {
                    let width = program.layout().widths.get(&signal);
                    let signal = storage(signal);
                    let delayed = delay_writers.get(&signal) == Some(&1)
                        && !rtl_stores.contains(&signal)
                        && !other_stores.contains(&signal);
                    if raw.is_none() && alternatives.is_empty() && width != Some(&1) {
                        return Err("the process waits for a non-scalar compact edge");
                    }
                    reactive |= !(delayed || undriven(signal));
                }
                let assertion =
                    returns_status(unit, slots.status, ProcessStatus::AssertionObserved)
                        || returns_status(unit, slots.status, ProcessStatus::AssertionReactive);
                if assertion
                    && (returns_status(unit, slots.status, ProcessStatus::Delay)
                        || returns_status(unit, slots.status, ProcessStatus::InactiveDelay)
                        || kernel_stores[index]
                            .iter()
                            .any(|store| waited.contains(store)))
                {
                    return Err("an assertion action suspends or drives an event expression");
                }
                let delays = !statuses_only(unit, slots.status, &WAIT_STATUSES);
                if delays
                    && ((!slots.const_delays && !ordered_available)
                        || !statuses_only(unit, slots.status, &MIXED_STATUSES))
                {
                    tracing::debug!(target: "celox::scheduler", process = index,
                        status_definitions = ?instructions(unit).filter_map(|inst| match inst {
                            SIRInstruction::Store(target, _, _, value, ..) if *target == stable(slots.status) =>
                                Some((*value, instructions(unit).find(|definition| definition.defined_register() == Some(*value)))),
                            _ => None,
                        }).collect::<Vec<_>>(), "unsupported native wait statuses");
                    return Err("the process suspends other than for an edge or a delay");
                }
                let writes = &kernel_stores[index];
                if !written(unit).is_empty() && !ordered_available {
                    return Err("the process drives an event signal");
                }
                reactive |= writes.iter().any(|store| waited.contains(store));
                if reactive && !process.nbas.is_empty() && !nba_processes.contains(&index) {
                    return Err("a reactive wait stages nonblocking assignments");
                }
                if reactive && opaque_dependencies && service_function.is_none() {
                    return Err("a reactive wait has opaque dependencies");
                }
                if (raw.is_some() || !alternatives.is_empty())
                    && !process.nbas.is_empty()
                    && !nba_processes.contains(&index)
                {
                    return Err("a raw-expression wait stages nonblocking assignments");
                }
                let sites = if nba_processes.contains(&index) {
                    Vec::new()
                } else {
                    process
                        .nbas
                        .iter()
                        .map(|site| {
                            let target = storage(site.target);
                            let pending =
                                site.pending.ok_or("a nonblocking site queues its values")?;
                            // A position computed at run time, or an element of
                            // an unpacked array, which the runtime merges through
                            // the array's layout, needs the runtime's publication.
                            if site.position.is_some()
                                || program.layout().unpacked_arrays.contains_key(&site.target)
                                || nba_targets.get(&target) != Some(&1)
                                || rtl_stores.contains(&target)
                                || kernel_stores.iter().any(|stores| stores.contains(&target))
                                || waited.contains(&target)
                                || probed.contains(&target)
                                || event_storage.contains_key(&target)
                            {
                                return Err("a nonblocking target has other writers or observers");
                            }
                            Ok(Staged {
                                pending,
                                value: site.value,
                                target: site.target,
                                lsb: site.lsb,
                                width: site.width,
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?
                };
                if sites.len() > 1 << SITE_BITS {
                    return Err("the process has too many nonblocking sites");
                }
                // A kernel that delays, or waits for edges of several
                // signals, may run twice in a slot, staging a site again
                // before the NBA region publishes it.
                let mut signals: Vec<AbsoluteAddr> =
                    terms.iter().map(|&(signal, _)| storage(signal)).collect();
                signals.sort_unstable();
                signals.dedup();
                if !sites.is_empty() && (delays || signals.len() > 1) {
                    return Err(
                        "the process stages nonblocking assignments and may run twice in a slot",
                    );
                }
                Ok((
                    Wait {
                        counters: terms
                            .iter()
                            .enumerate()
                            .map(|(index, &(signal, kind))| {
                                gates
                                    .get(index)
                                    .copied()
                                    .flatten()
                                    .and_then(|gate| gate.counters)
                                    .map(|counters| counters.all)
                                    .or_else(|| {
                                        model
                                            .counted
                                            .iter()
                                            .find(|counted| counted.slots.signal == signal)
                                            .map(|counted| counted.slots.counter(kind))
                                    })
                            })
                            .collect(),
                        terms,
                        site_terms,
                        raw,
                        alternatives,
                        gates,
                        reactive,
                        sites,
                    },
                    delays,
                ))
            })();
            match wait {
                Ok((wait, delays)) => {
                    kernels[index] = Some(Kernel {
                        writes: written(unit),
                        wait: Some(wait),
                        delays,
                    })
                }
                Err(reason) => fallback[index] = Some(reason.to_string()),
            }
        }
        let mut remaining: HashSet<usize> = HashSet::default();
        // The combined observer cone above already includes every wait and
        // its combinational inputs. If no waiting kernel writes that cone,
        // none can wake another (or itself after settling). Assertion-only
        // observers in large RTL designs commonly satisfy this cheap proof.
        let feedback_stores: HashSet<_> = kernels
            .iter()
            .enumerate()
            .filter(|(_, kernel)| kernel.as_ref().is_some_and(|kernel| kernel.wait.is_some()))
            .flat_map(|(index, _)| kernel_stores[index].iter().copied())
            .filter(|address| {
                // The detailed analysis below likewise ignores private process
                // control in combinational cones. Keep direct waits regardless.
                waited.contains(address)
                    || (!control_slots.contains(address) && active_observers.contains(address))
            })
            .collect();
        let event_writing_waits = kernels.iter().enumerate().any(|(index, kernel)| {
            kernel.as_ref().is_some_and(|kernel| {
                !kernel.writes.is_empty()
                    && (kernel.wait.is_some() || service_processes.contains(&index))
            })
        });
        let possible_wait_feedback =
            opaque_dependencies || event_writing_waits || !feedback_stores.is_empty();
        tracing::debug!(target: "celox::scheduler", possible_wait_feedback,
            stores = ?feedback_stores.iter().take(8).map(|address| program.get_path(address)).collect::<Vec<_>>(),
            "candidate wait feedback stores");
        // Native work lists can safely bound possible feedback without proving
        // a cycle. Other backends must identify and exclude cyclic reactive
        // kernels, so retain the precise per-wait analysis for them. Avoiding
        // those repeated cones matters for large procedural RTL schedules.
        if possible_wait_feedback && service_function.is_none() {
            // Find feedback that needs the native work-list bounds. Include
            // combinational cones: writing an input of a waited
            // expression can wake its consumer after the settle, even when no
            // kernel stores the expression itself.
            // Several process kernels commonly wait on the same clock or signal
            // set. Their dependency cones are identical within this immutable
            // plan; analyze each distinct set once without materializing an
            // executable slice just to inspect its loads.
            let mut wait_dependencies = HashMap::default();
            let dependencies: Vec<(HashSet<AbsoluteAddr>, HashSet<AbsoluteAddr>)> = kernels
                .iter()
                .map(|kernel| {
                    let Some(wait) = kernel.as_ref().and_then(|kernel| kernel.wait.as_ref()) else {
                        return (HashSet::default(), HashSet::default());
                    };
                    // Input ports and clock aliases can give hundreds of names
                    // to the same storage. Slicing already uses these homes;
                    // key its cache by them too.
                    let mut signals: Vec<_> = wait
                        .terms
                        .iter()
                        .map(|&(signal, _)| storage(signal))
                        .collect();
                    signals.sort_unstable();
                    signals.dedup();
                    wait_dependencies
                        .entry(signals)
                        .or_insert_with_key(|signals| {
                            let inputs = wait_slicer
                                .as_ref()
                                .map(|slicer| slicer.input_signals(signals.iter().copied()))
                                .unwrap_or_default();
                            let comb: HashSet<_> = inputs
                                .into_iter()
                                .map(storage)
                                .filter(|input| !control_slots.contains(input))
                                .collect();
                            let all: HashSet<_> = signals
                                .iter()
                                .copied()
                                .map(storage)
                                .chain(comb.iter().copied())
                                .collect();
                            (all, comb)
                        })
                        .clone()
                })
                .collect();
            tracing::debug!(target: "celox::scheduler", kernels = kernels.len(), distinct_wait_sets = wait_dependencies.len(), "scheduler wait dependency slices");
            remaining = kernels
                .iter()
                .enumerate()
                .filter_map(|(index, kernel)| kernel.as_ref()?.wait.as_ref().map(|_| index))
                .collect();
            loop {
                let roots: Vec<usize> = remaining
                    .iter()
                    .copied()
                    .filter(|&consumer| {
                        !remaining.iter().any(|&producer| {
                            // A process is not waiting while its own body runs;
                            // registration captures direct signal writes afterward.
                            // A computed expression can still change at the later
                            // settle, after registration, and wake the same process.
                            let watched = if producer == consumer {
                                &dependencies[consumer].1
                            } else {
                                &dependencies[consumer].0
                            };
                            !kernel_stores[producer].is_disjoint(watched)
                        })
                    })
                    .collect();
                if roots.is_empty() {
                    break;
                }
                for root in roots {
                    remaining.remove(&root);
                }
            }
        } else {
            tracing::debug!(target: "celox::scheduler", possible_wait_feedback, nba_processes = nba_processes.len(), "individual wait dependency slices are unnecessary");
        }
        let inactive: Vec<_> = kernels
            .iter()
            .enumerate()
            .filter_map(|(index, kernel)| {
                (ordered_available
                    && kernel.as_ref().is_some_and(|kernel| kernel.delays)
                    && !schema.processes[index].const_delays)
                    .then_some((index, schema.processes[index].status))
            })
            .collect();
        let cyclic = service_function.is_some()
            && (possible_wait_feedback || !nba_processes.is_empty() || !inactive.is_empty());
        for index in remaining {
            if !cyclic
                && kernels[index]
                    .as_ref()
                    .and_then(|kernel| kernel.wait.as_ref())
                    .is_some_and(|wait| wait.reactive)
            {
                kernels[index] = None;
                fallback[index] = Some("reactive waits have cyclic dependencies".into());
            }
        }
        if kernels.iter().flatten().count() > program.layout().widths[&slots.ready] {
            return Err("the compiled ready mask has insufficient storage".into());
        }
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
        let effects = sir.eval_comb.iter().any(|unit| {
            instructions(unit).any(|inst| match inst {
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
            })
        });
        // Dirty tracking compares stable homes by their representative
        // (`shared_storage`), which groups addresses only by an equal start
        // offset: no two homes may overlap from different offsets.
        debug_assert!(
            homes_overlap_only_whole(program.layout()),
            "stable homes overlap at different offsets"
        );
        // A pending settle cannot pass an opaque reader, nor can an opaque
        // writer use a sliced settle selected only from explicit stores.
        // Keep the complete generated settle in that general path.
        let settle_reads: Option<HashSet<AbsoluteAddr>> =
            (!effects && !opaque_dependencies).then(|| {
                sir.eval_comb
                    .iter()
                    .flat_map(instructions)
                    .flat_map(read_storage)
                    .map(|source| storage(source.absolute_addr()))
                    .collect()
            });
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
        let eager = settle_reads
            .as_ref()
            .filter(|_| !update_points && !sir.eval_comb.iter().any(fails))
            .and_then(|_| {
                let stateful = sir.eval_comb_stateful.as_ref();
                let eager = stateful.and_then(|stateful| {
                    if stateful.is_empty() && watched_comb.is_empty() {
                        return Some(Vec::new());
                    }
                    let slicer = &*comb_slicer;
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
            let ends = |unit: &Unit| fails(unit) || emits_finish(unit, &finish_sites);
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
        let incremental = if lazy.is_none()
            && settle_reads.is_some()
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
                        let slicer = &*comb_slicer;
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
        let mut plan = Self {
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
        let derived_from = |event: usize| -> Vec<usize> {
            let signal = plan.events[event].signal;
            topology
                .derived_events
                .get(&signal)
                .into_iter()
                .flatten()
                .copied()
                .chain(
                    topology
                        .combinational_derived
                        .get(&signal)
                        .into_iter()
                        .flatten()
                        .map(|clock| clock.clock),
                )
                .filter_map(|address| index_of.get(&topology.canonical(address)).copied())
                .collect()
        };
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
            for &index in rtl_written.difference(&derived) {
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
        Ok(plan)
    }

    /// The events whose levels the slots record and compare: those that
    /// can have edges in them and the clocks derived from those, in event
    /// order: the bits of [`SchedulerSlots::levels`].
    pub(super) fn tracked(&self) -> Vec<usize> {
        let mut tracked = self.edged();
        tracked.extend(&self.derived);
        tracked.sort_unstable();
        tracked
    }

    /// The events the scheduler's kernels write, in event order: the only
    /// ones that can have edges in its slots.
    pub(super) fn written(&self) -> Vec<usize> {
        let mut written: Vec<usize> = self
            .kernels
            .iter()
            .flatten()
            .flat_map(|kernel| kernel.writes.iter().copied())
            .collect();
        written.sort_unstable();
        written.dedup();
        written
    }

    /// The events that can have edges in the scheduler's slots, in event
    /// order: those its kernels write and those its host clocks may drive.
    pub(super) fn edged(&self) -> Vec<usize> {
        let mut edged = self.written();
        edged.extend(&self.clocks);
        edged.extend(&self.host_edges);
        edged.sort_unstable();
        edged.dedup();
        edged
    }
}
