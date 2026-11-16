//! The process kernels the scheduler runs, and why the others fall back.
use super::*;

impl PlanBuilder<'_> {
    /// The kernels whose nonblocking assignments, host requests and loop
    /// checkpoints the native services serve inside the unit.
    pub(super) fn services(&mut self) {
        let program = self.program;
        let schema = self.schema;
        let model = self.model;
        let sir = self.sir;
        let shared = self.shared;
        let event_storage = &self.found.event_storage;
        let rtl_stores = &self.found.rtl_stores;
        let kernel_stores = &self.found.kernel_stores;
        let probed = &self.found.probed;
        let storage = home(shared);
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
        self.found.service_function = service_function;
        self.found.ordered_available = ordered_available;
        self.found.nba_processes = nba_processes;
        self.found.yielding = yielding;
        self.found.service_processes = service_processes;
    }

    /// The kernels that only delay, and why each other kernel falls back
    /// unless it waits; then what the kernels that only delay write.
    pub(super) fn delaying_kernels(&mut self) {
        let schema = self.schema;
        let model = self.model;
        let sir = self.sir;
        let shared = self.shared;
        let kernel_stores = &self.found.kernel_stores;
        let ordered_available = self.found.ordered_available;
        let nba_processes = &self.found.nba_processes;
        let yielding = &self.found.yielding;
        let service_processes = &self.found.service_processes;
        let storage = home(shared);
        let written = written_events(&self.found.event_storage, storage);
        let rtl_units = || rtl_units_of(sir);
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
            .zip(kernel_stores)
            .filter(|(kernel, _)| kernel.is_some())
        {
            for store in stores {
                *delay_writers.entry(*store).or_default() += 1;
            }
        }
        // Storage the other kernels write, waiting ones included.
        let other_stores: HashSet<AbsoluteAddr> = kernels
            .iter()
            .zip(kernel_stores)
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
        self.found.kernels = kernels;
        self.found.fallback = fallback;
        self.found.delay_writers = delay_writers;
        self.found.other_stores = other_stores;
        self.found.opaque_dependencies = opaque_dependencies;
    }

    /// The kernels that wait for edges, each with its event controls, or why
    /// it falls back.
    pub(super) fn waiting_kernels(&mut self) {
        let program = self.program;
        let slots = self.slots;
        let schema = self.schema;
        let model = self.model;
        let sir = self.sir;
        let shared = self.shared;
        let event_storage = &self.found.event_storage;
        let rtl_stores = &self.found.rtl_stores;
        let kernel_stores = &self.found.kernel_stores;
        let waited = &self.found.waited;
        let probed = &self.found.probed;
        let nba_targets = &self.found.nba_targets;
        let service_function = self.found.service_function;
        let ordered_available = self.found.ordered_available;
        let nba_processes = &self.found.nba_processes;
        let mut kernels = std::mem::take(&mut self.found.kernels);
        let mut fallback = std::mem::take(&mut self.found.fallback);
        let delay_writers = &self.found.delay_writers;
        let other_stores = &self.found.other_stores;
        let opaque_dependencies = self.found.opaque_dependencies;
        let storage = home(shared);
        let written = written_events(&self.found.event_storage, storage);
        let undriven = undriven_storage(
            schema,
            &self.found.rtl_stores,
            &self.found.kernel_stores,
            &self.found.nba_targets,
        );
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
        self.found.kernels = kernels;
        self.found.fallback = fallback;
    }

    /// Whether a waiting kernel can wake another or itself in a slot: with the
    /// native services the unit bounds that work, without them a reactive
    /// kernel on such a cycle falls back. Also the kernels whose zero delays
    /// need an Inactive pass.
    pub(super) fn wait_feedback(
        &mut self,
        wait_slicer: Option<&CombSlicer<'_>>,
    ) -> Result<(), String> {
        let program = self.program;
        let slots = self.slots;
        let schema = self.schema;
        let shared = self.shared;
        let kernel_stores = &self.found.kernel_stores;
        let control_slots = &self.found.control_slots;
        let waited = &self.found.waited;
        let active_observers = &self.found.active_observers;
        let service_function = self.found.service_function;
        let ordered_available = self.found.ordered_available;
        let nba_processes = &self.found.nba_processes;
        let service_processes = &self.found.service_processes;
        let mut kernels = std::mem::take(&mut self.found.kernels);
        let mut fallback = std::mem::take(&mut self.found.fallback);
        let opaque_dependencies = self.found.opaque_dependencies;
        let storage = home(shared);
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
        self.found.kernels = kernels;
        self.found.fallback = fallback;
        self.found.event_writing_waits = event_writing_waits;
        self.found.inactive = inactive;
        self.found.cyclic = cyclic;
        Ok(())
    }
}
