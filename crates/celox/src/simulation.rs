use crate::{
    RuntimeErrorCode, Simulator,
    backend::MemoryLayout,
    ir::{DomainKind, SignalRef},
    scheduler::{Scheduler, SimEvent},
    simulator::{NamedEvent, NamedSignal},
};

/// A timed simulation wrapper around the core logic engine.
///
/// Manages simulation time, periodic clocks, and an event queue.
pub struct Simulation {
    pub(crate) simulator: Simulator,
    pub(crate) scheduler: Scheduler,
    pub(crate) last_clock_values: bit_set::BitSet,
    pub(crate) topo_signals: Vec<(SignalRef, usize, usize)>, // (signal, id, canonical_id)
    pub(crate) domain_kinds: Vec<Option<DomainKind>>,
    pub(crate) event_info: Vec<EventInfo>,
    pub(crate) signal_to_id: crate::HashMap<SignalRef, usize>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct EventInfo {
    pub(crate) canonical_id: usize,
    pub(crate) is_cascaded: bool,
    pub(crate) eval_ff_event: Option<crate::backend::EventRef>,
    pub(crate) eval_only_event: Option<crate::backend::EventRef>,
    pub(crate) apply_event: Option<crate::backend::EventRef>,
}

impl std::fmt::Debug for Simulation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Simulation")
            .field("time", &self.scheduler.time)
            .finish()
    }
}

impl Simulation {
    pub fn builder<'a>(code: &'a str, top: &'a str) -> crate::SimulatorBuilder<'a, Simulation> {
        crate::SimulatorBuilder::<Simulation>::new(code, top)
    }

    pub(crate) fn new(simulator: Simulator) -> Self {
        let num_events = simulator.backend.num_events();
        let topo_signals: Vec<(SignalRef, usize, usize)> = simulator
            .program
            .topological_clocks
            .iter()
            .map(|addr| {
                let signal = simulator.backend.resolve_signal(addr);
                let id = simulator
                    .backend
                    .resolve_event_opt(addr)
                    .map(|ev| ev.id)
                    .unwrap_or(usize::MAX);
                let canonical = simulator
                    .program
                    .clock_domains
                    .get(addr)
                    .copied()
                    .unwrap_or(*addr);
                let canonical_id = simulator
                    .backend
                    .resolve_event_opt(&canonical)
                    .map(|ev| ev.id)
                    .unwrap_or(usize::MAX);
                (signal, id, canonical_id)
            })
            .collect();

        let mut domain_kinds = vec![None; num_events];
        for (_, id, _) in topo_signals.iter().copied() {
            if id != usize::MAX {
                let addr = simulator.backend.id_to_addr[id];
                if let Some(info) = simulator.program.get_variable_info(&addr) {
                    domain_kinds[id] = Some(info.kind);
                }
            }
        }

        let mut last_clock_values = bit_set::BitSet::with_capacity(num_events);
        for (signal, id, _) in topo_signals.iter().copied() {
            if id != usize::MAX {
                let val: u8 = simulator.backend.get_as(signal);
                if val != 0 {
                    last_clock_values.insert(id);
                }
            }
        }

        let mut event_info = vec![
            EventInfo {
                canonical_id: usize::MAX,
                is_cascaded: false,
                eval_ff_event: None,
                eval_only_event: None,
                apply_event: None,
            };
            num_events
        ];
        for id in 0..num_events {
            let addr = simulator.backend.id_to_addr[id];
            let canonical = simulator
                .program
                .clock_domains
                .get(&addr)
                .copied()
                .unwrap_or(addr);

            let is_cascaded = simulator.program.cascaded_clocks.contains(&canonical);

            let eval_ff_event = simulator.backend.resolve_event_opt(&canonical);
            let eval_only_event = simulator.backend.resolve_eval_only_event(&canonical);
            let apply_event = simulator.backend.resolve_apply_event(&canonical);

            if let Some(canonical_ev) = eval_ff_event {
                event_info[id] = EventInfo {
                    canonical_id: canonical_ev.id,
                    is_cascaded,
                    eval_ff_event,
                    eval_only_event,
                    apply_event,
                };
            }
        }

        let mut signal_to_id = crate::HashMap::default();
        for (signal, id, _) in &topo_signals {
            if *id != usize::MAX {
                signal_to_id.insert(*signal, *id);
            }
        }

        Self {
            simulator,
            scheduler: Scheduler::new(),
            last_clock_values,
            topo_signals,
            domain_kinds,
            event_info,
            signal_to_id,
        }
    }

    /// Captures the current state of all signals and writes them to the VCD file.
    pub fn dump(&mut self, timestamp: u64) {
        self.simulator.dump(timestamp);
    }

    /// Resolves a signal path into a performance-optimized [`SignalRef`].
    pub fn signal(&self, path: &str) -> SignalRef {
        self.simulator.signal(path)
    }

    /// Retrieves the current value of a variable using a pre-resolved [`SignalRef`] handle.
    pub fn get(&mut self, signal: SignalRef) -> malachite_bigint::BigUint {
        self.simulator.get(signal)
    }

    /// Modifies internal state via a callback and re-stabilizes combinational logic.
    pub fn modify<F>(&mut self, f: F) -> Result<(), RuntimeErrorCode>
    where
        F: FnOnce(&mut crate::IOContext),
    {
        self.simulator.modify(f)
    }

    /// Register a clock signal and its period, enqueuing the first edge.
    /// `initial_delay` specifies when the first rising edge occurs.
    pub fn add_clock(&mut self, port: &str, period: u64, initial_delay: u64) {
        let signal = self.simulator.signal(port);
        let addr = self.simulator.program.get_addr(&[], &[port]);
        if let Some(ev) = self.simulator.backend.resolve_event_opt(&addr) {
            if ev.id >= self.scheduler.clocks.len() {
                self.scheduler.clocks.resize(ev.id + 1, None);
            }
            self.scheduler.clocks[ev.id] = Some(crate::scheduler::ClockDef { period });
            // Start all clocks with rising edge at t = initial_delay
            self.scheduler.push(SimEvent {
                time: initial_delay,
                event_ref: ev,
                signal,
                next_val: 1,
            });
        }
    }

    /// Schedule a one-shot event at a specific time.
    /// The signal must be registered as an event (clock or async reset) in the backend.
    pub fn schedule(&mut self, port: &str, time: u64, value: u64) -> Result<(), RuntimeErrorCode> {
        let signal = self.simulator.signal(port);
        let addr = self.simulator.program.get_addr(&[], &[port]);
        let ev_opt = self.simulator.backend.resolve_event_opt(&addr);
        if let Some(ev) = ev_opt {
            self.scheduler.push(SimEvent {
                time,
                event_ref: ev,
                signal,
                next_val: value as u8,
            });
        } else {
            return Err(RuntimeErrorCode::NotAnEvent(port.to_string()));
        }

        Ok(())
    }

    /// Advance time to the next scheduled event and process all events at that time.
    /// Returns the new simulation time, or None if no events are scheduled.
    pub fn step(&mut self) -> Result<Option<u64>, RuntimeErrorCode> {
        let (current_time, events_to_process) = match self.scheduler.pop_all_at_next_time() {
            Some(res) => res,
            None => return Ok(None),
        };

        self.scheduler.time = current_time;

        // Apply external events to the Stable region
        let num_events = self.simulator.backend.num_events();
        for ev in &events_to_process {
            self.simulator.backend.set(ev.signal, ev.next_val);
        }

        // Phase 1: Trigger Discovery Loop (Multi-phase)
        let mut triggered_domains = bit_set::BitSet::with_capacity(num_events);
        let mut discovered_in_this_step = bit_set::BitSet::with_capacity(num_events);

        // Initial stabilization for external triggers
        self.simulator.backend.clear_triggered_bits();

        // Mark external triggers
        for ev in &events_to_process {
            if let Some(&id) = self.signal_to_id.get(&ev.signal) {
                let last_val_is_nonzero = self.last_clock_values.contains(id);
                let current_val = ev.next_val;
                let triggered = match self.domain_kinds[id] {
                    Some(DomainKind::ClockPosedge) | Some(DomainKind::ResetAsyncHigh) => {
                        !last_val_is_nonzero && current_val != 0
                    }
                    Some(DomainKind::ClockNegedge) | Some(DomainKind::ResetAsyncLow) => {
                        last_val_is_nonzero && current_val == 0
                    }
                    _ => !last_val_is_nonzero && current_val != 0,
                };
                if triggered {
                    self.simulator.backend.mark_triggered_bit(id);
                }
            }
        }

        self.simulator.backend.eval_comb()?;

        loop {
            let mut any_new_outer_loop_trigger = false;
            let mut newly_triggered = Vec::new();

            // Inner loop: Evaluate FFs. Sequential cascades (FF -> FF) trigger within this loop.
            // All FFs evaluated here read from the STABLE region simultaneously.
            loop {
                let mut any_new_seq = false;

                // Read detected triggers from JIT memory
                let marked_bits = self.simulator.backend.get_triggered_bits();
                self.simulator.backend.clear_triggered_bits();

                // Optimization: If this is the *first* iteration of the outer loop,
                // and exactly ONE trigger fired (from external events), we check
                // if it's statically known to cascade. If it NEVER triggers another
                // clock, we can safely evaluate and commit this FF domain in one shot.
                let mut can_use_eval_apply = triggered_domains.is_empty() && marked_bits.len() == 1;

                if can_use_eval_apply {
                    // Peek at the single triggered ID
                    let mut single_id = 0;
                    for id in marked_bits.iter() {
                        single_id = id;
                        break;
                    }
                    let info = self.event_info[single_id];
                    // Check if this clock domain is an internal cascading target
                    if info.is_cascaded {
                        can_use_eval_apply = false;
                    }

                    if can_use_eval_apply {
                        if let Some(ev) = info.eval_ff_event {
                            discovered_in_this_step.insert(single_id);
                            triggered_domains.insert(info.canonical_id);
                            any_new_outer_loop_trigger = true;

                            // Directly execute eval + apply
                            self.simulator.backend.eval_apply_ff_at(ev)?;

                            // The FF has been applied. We do NOT add it to `newly_triggered`
                            // because it's already committed. We just break the inner loop
                            // and let Phase 3 eval_comb catch any combinational changes.
                            break;
                        }
                    }
                }

                for id in marked_bits.iter() {
                    if discovered_in_this_step.contains(id) {
                        continue;
                    }
                    discovered_in_this_step.insert(id);

                    let info = self.event_info[id];
                    if !triggered_domains.contains(info.canonical_id) {
                        triggered_domains.insert(info.canonical_id);
                        any_new_seq = true;
                        newly_triggered.push(info.canonical_id);

                        if let Some(ev) = info.eval_only_event {
                            self.simulator.backend.eval_only_ff_at(ev)?;
                        } else if let Some(ev) = info.eval_ff_event {
                            // If this domain wasn't split into eval/apply, we can safely use the
                            // unified eval_apply_ff_at since no cascade optimizations applied to it.
                            self.simulator.backend.eval_apply_ff_at(ev)?;
                        } else {
                            unreachable!(
                                "FF trigger discovered but no corresponding execution unit found for domain"
                            );
                        }
                    }
                }

                if !any_new_seq {
                    break;
                }
            }

            if newly_triggered.is_empty() && !any_new_outer_loop_trigger {
                break;
            }

            // Phase 2: Commit (Apply) newly triggered FFs immediately to stable region
            for id in &newly_triggered {
                let info = self.event_info[*id];
                if let Some(ev) = info.apply_event {
                    self.simulator.backend.apply_ff_at(ev)?;
                }
            }

            // Phase 3: Evaluate combinational logic on stable region to propagate FF outputs.
            // If this triggers combinational clocks, the next outer delta cycle catches them.
            self.simulator.backend.eval_comb()?;

            if !any_new_outer_loop_trigger && newly_triggered.is_empty() {
                break;
            }
        }

        // Update last values for the next step based on FINAL values
        for (signal, id, _) in &self.topo_signals {
            if *id != usize::MAX {
                let val: u8 = self.simulator.backend.get_as(*signal);
                if val != 0 {
                    self.last_clock_values.insert(*id);
                } else {
                    self.last_clock_values.remove(*id);
                }
            }
        }

        // Reschedule clocks
        for ev in &events_to_process {
            let ev_ref = ev.event_ref;
            if let Some(Some(def)) = self.scheduler.clocks.get(ev_ref.id) {
                let half_period = def.period / 2;
                self.scheduler.push(SimEvent {
                    time: current_time + half_period,
                    event_ref: ev.event_ref,
                    signal: ev.signal,
                    next_val: 1 - ev.next_val,
                });
            }
        }

        self.simulator.dirty = false;
        self.dump(current_time);

        Ok(Some(current_time))
    }

    /// Advance time and run until `end_time` (inclusive).
    pub fn run_until(&mut self, end_time: u64) -> Result<(), RuntimeErrorCode> {
        while let Some(next_time) = self.scheduler.next_event_time() {
            if next_time > end_time {
                break;
            }
            self.step()?;
        }
        self.scheduler.time = end_time;
        self.dump(end_time);
        Ok(())
    }

    /// Returns the current simulation time.
    pub fn time(&self) -> u64 {
        self.scheduler.time
    }

    /// Returns the time of the next scheduled event, if any.
    pub fn next_event_time(&self) -> Option<u64> {
        self.scheduler.next_event_time()
    }

    /// Directly execute combinational logic evaluation.
    pub fn eval_comb(&mut self) -> Result<(), RuntimeErrorCode> {
        self.simulator.eval_comb()
    }

    /// Returns a raw pointer to the JIT memory and its total size in bytes.
    pub fn memory_as_ptr(&self) -> (*const u8, usize) {
        self.simulator.memory_as_ptr()
    }

    /// Returns a mutable raw pointer to the JIT memory and its total size in bytes.
    pub fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        self.simulator.memory_as_mut_ptr()
    }

    /// Returns the stable region size in bytes.
    pub fn stable_region_size(&self) -> usize {
        self.simulator.stable_region_size()
    }

    /// Returns a reference to the memory layout.
    pub fn layout(&self) -> &MemoryLayout {
        self.simulator.layout()
    }

    /// Returns all ports of the top-level module.
    pub fn named_signals(&self) -> Vec<NamedSignal> {
        self.simulator.named_signals()
    }

    /// Returns all events with their IDs and event references.
    pub fn named_events(&self) -> Vec<NamedEvent> {
        self.simulator.named_events()
    }

    /// Register a clock signal by event ID.
    pub fn add_clock_by_id(&mut self, event_id: u32, period: u64, initial_delay: u64) {
        let addr = self.simulator.backend.id_to_addr[event_id as usize];
        let signal = self.simulator.backend.resolve_signal(&addr);
        if let Some(ev) = self.simulator.backend.resolve_event_opt(&addr) {
            if ev.id >= self.scheduler.clocks.len() {
                self.scheduler.clocks.resize(ev.id + 1, None);
            }
            self.scheduler.clocks[ev.id] = Some(crate::scheduler::ClockDef { period });
            self.scheduler.push(SimEvent {
                time: initial_delay,
                event_ref: ev,
                signal,
                next_val: 1,
            });
        }
    }

    /// Schedule a one-shot event by event ID.
    pub fn schedule_by_id(
        &mut self,
        event_id: u32,
        time: u64,
        value: u64,
    ) -> Result<(), RuntimeErrorCode> {
        let addr = self.simulator.backend.id_to_addr[event_id as usize];
        let signal = self.simulator.backend.resolve_signal(&addr);
        let ev_opt = self.simulator.backend.resolve_event_opt(&addr);
        if let Some(ev) = ev_opt {
            self.scheduler.push(SimEvent {
                time,
                event_ref: ev,
                signal,
                next_val: value as u8,
            });
            Ok(())
        } else {
            Err(RuntimeErrorCode::NotAnEvent(format!(
                "event_id={}",
                event_id
            )))
        }
    }
}
