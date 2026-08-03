use crate::{
    RuntimeErrorCode, Simulator,
    backend::{EventHandle, MemoryLayout, SimBackend},
    ir::SignalRef,
    simulator::{InstanceHierarchy, NamedEvent, NamedSignal},
};
use celox_runtime::{EventInfo, SimulationExecutor, SimulationState};

/// A timed simulation wrapper around the core logic engine.
///
/// Manages simulation time, periodic clocks, and an event queue.
///
/// The default type parameter uses the host's [`crate::DefaultBackend`].
pub struct Simulation<B: SimBackend = crate::DefaultBackend> {
    pub(crate) simulator: Simulator<B>,
    pub(crate) state: SimulationState<B>,
}

impl<B: SimBackend> std::fmt::Debug for Simulation<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Simulation")
            .field("time", &self.state.time())
            .finish()
    }
}

impl<B: SimBackend> SimulationExecutor for Simulator<B> {
    type Backend = B;

    fn backend(&self) -> &B {
        &self.backend
    }

    fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    fn eval_comb(&mut self) -> Result<(), RuntimeErrorCode> {
        self.eval_comb_checked()
    }

    fn eval_apply_ff_at(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
        self.eval_apply_ff_at_checked(event)
    }

    fn eval_only_ff_at(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
        self.eval_only_ff_at_checked(event)
    }

    fn apply_ff_at(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
        self.apply_ff_at_checked(event)
    }

    fn finish_timed_step(&mut self, timestamp: u64) {
        self.dirty = false;
        self.dump(timestamp);
    }
}

// ── Backend-specific constructors ───────────────────────────────────

impl Simulation {
    pub fn builder<'a>(code: &'a str, top: &'a str) -> crate::SimulatorBuilder<'a, Simulation> {
        crate::SimulatorBuilder::<Simulation>::new(code, top)
    }

    pub fn from_sources<'a>(
        sources: Vec<(&'a str, &'a std::path::Path)>,
        top: &'a str,
    ) -> crate::SimulatorBuilder<'a, Simulation> {
        crate::SimulatorBuilder::<Simulation>::from_sources(sources, top)
    }
}

// ── Generic methods available for any backend ───────────────────────

impl<B: SimBackend> Simulation<B> {
    pub(crate) fn new(simulator: Simulator<B>) -> Self {
        let num_events = simulator.backend.num_events();
        let topo_signals: Vec<(SignalRef, usize, usize)> = simulator
            .program
            .design
            .events
            .ordered_events
            .iter()
            .map(|addr| {
                let signal = simulator.backend.resolve_signal(addr);
                let id = simulator
                    .backend
                    .resolve_event_opt(addr)
                    .map(|ev| ev.id())
                    .unwrap_or(usize::MAX);
                let canonical = simulator.program.design.events.canonical(*addr);
                let canonical_id = simulator
                    .backend
                    .resolve_event_opt(&canonical)
                    .map(|ev| ev.id())
                    .unwrap_or(usize::MAX);
                (signal, id, canonical_id)
            })
            .collect();

        let mut domain_kinds = vec![None; num_events];
        for (_, id, _) in topo_signals.iter().copied() {
            if id != usize::MAX {
                let addr = simulator.backend.id_to_addr_slice()[id];
                if let Some(info) = simulator.program.get_variable_info(&addr) {
                    domain_kinds[id] = Some(info.kind);
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
        for (id, info) in event_info.iter_mut().enumerate() {
            let addr = simulator.backend.id_to_addr_slice()[id];
            let canonical = simulator.program.design.events.canonical(addr);

            let is_cascaded = simulator
                .program
                .design
                .events
                .cascaded_events
                .contains(&canonical);

            let eval_ff_event = simulator.backend.resolve_event_opt(&canonical);
            let eval_only_event = simulator.backend.resolve_eval_only_event(&canonical);
            let apply_event = simulator.backend.resolve_apply_event(&canonical);

            if let Some(canonical_ev) = eval_ff_event {
                *info = EventInfo {
                    canonical_id: canonical_ev.id(),
                    is_cascaded,
                    eval_ff_event,
                    eval_only_event,
                    apply_event,
                };
            }
        }

        let state =
            SimulationState::new(&simulator.backend, topo_signals, domain_kinds, event_info);

        Self { simulator, state }
    }

    /// Returns warnings emitted during compilation.
    pub fn warnings(&self) -> &[crate::CompilationWarning] {
        self.simulator.warnings()
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
    pub fn get(&mut self, signal: SignalRef) -> num_bigint::BigUint {
        self.simulator.get(signal)
    }

    /// Modifies internal state via a callback and re-stabilizes combinational logic.
    pub fn modify<F>(&mut self, f: F) -> Result<(), RuntimeErrorCode>
    where
        F: FnOnce(&mut crate::IOContext<B>),
    {
        self.simulator.modify(f)
    }

    /// Register a clock signal and its period, enqueuing the first edge.
    /// `initial_delay` specifies when the first rising edge occurs.
    pub fn add_clock(&mut self, port: &str, period: u64, initial_delay: u64) {
        let signal = self.simulator.signal(port);
        let addr = self.simulator.program.get_addr(&[], &[port]).unwrap();
        if let Some(ev) = self.simulator.backend.resolve_event_opt(&addr) {
            self.state.add_clock(ev, signal, period, initial_delay);
        }
    }

    /// Schedule a one-shot event at a specific time.
    /// The signal must be registered as an event (clock or async reset) in the backend.
    pub fn schedule(&mut self, port: &str, time: u64, value: u64) -> Result<(), RuntimeErrorCode> {
        let signal = self.simulator.signal(port);
        let addr = self.simulator.program.get_addr(&[], &[port]).unwrap();
        let ev_opt = self.simulator.backend.resolve_event_opt(&addr);
        if let Some(ev) = ev_opt {
            self.state.schedule(ev, signal, time, value as u8);
        } else {
            return Err(RuntimeErrorCode::NotAnEvent(port.to_string()));
        }

        Ok(())
    }

    /// Advance time to the next scheduled event and process all events at that time.
    /// Returns the new simulation time, or None if no events are scheduled.
    pub fn step(&mut self) -> Result<Option<u64>, RuntimeErrorCode> {
        self.state.step(&mut self.simulator)
    }

    /// Advance time and run until `end_time` (inclusive).
    pub fn run_until(&mut self, end_time: u64) -> Result<(), RuntimeErrorCode> {
        while let Some(next_time) = self.state.next_event_time() {
            if next_time > end_time {
                break;
            }
            self.step()?;
        }
        self.state.set_time(end_time);
        self.dump(end_time);
        Ok(())
    }

    /// Returns the current simulation time.
    pub fn time(&self) -> u64 {
        self.state.time()
    }

    /// Returns the time of the next scheduled event, if any.
    pub fn next_event_time(&self) -> Option<u64> {
        self.state.next_event_time()
    }

    /// Directly execute combinational logic evaluation.
    pub fn eval_comb(&mut self) -> Result<(), RuntimeErrorCode> {
        self.simulator.eval_comb()
    }

    /// Returns a raw pointer to the backend memory and its total size in bytes.
    pub fn memory_as_ptr(&self) -> (*const u8, usize) {
        self.simulator.memory_as_ptr()
    }

    /// Returns a mutable raw pointer to the backend memory and its total size in bytes.
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
    pub fn named_events(&self) -> Vec<NamedEvent<B>> {
        self.simulator.named_events()
    }

    /// Returns the full instance hierarchy starting from the top module.
    pub fn named_hierarchy(&self) -> InstanceHierarchy {
        self.simulator.named_hierarchy()
    }

    /// Returns all signals for the instance at the given hierarchical path.
    pub fn instance_signals(&self, instance_path: &[(&str, usize)]) -> Vec<NamedSignal> {
        self.simulator.instance_signals(instance_path)
    }

    /// Resolves a signal inside a child instance.
    pub fn child_signal(&self, instance_path: &[(&str, usize)], var: &str) -> SignalRef {
        self.simulator.child_signal(instance_path, var)
    }

    /// Register a clock signal by event ID.
    pub fn add_clock_by_id(&mut self, event_id: u32, period: u64, initial_delay: u64) {
        let addr = self.simulator.backend.id_to_addr_slice()[event_id as usize];
        let signal = self.simulator.backend.resolve_signal(&addr);
        if let Some(ev) = self.simulator.backend.resolve_event_opt(&addr) {
            self.state.add_clock(ev, signal, period, initial_delay);
        }
    }

    /// Schedule a one-shot event by event ID.
    pub fn schedule_by_id(
        &mut self,
        event_id: u32,
        time: u64,
        value: u64,
    ) -> Result<(), RuntimeErrorCode> {
        let addr = self.simulator.backend.id_to_addr_slice()[event_id as usize];
        let signal = self.simulator.backend.resolve_signal(&addr);
        let ev_opt = self.simulator.backend.resolve_event_opt(&addr);
        if let Some(ev) = ev_opt {
            self.state.schedule(ev, signal, time, value as u8);
            Ok(())
        } else {
            Err(RuntimeErrorCode::NotAnEvent(format!(
                "event_id={}",
                event_id
            )))
        }
    }
}
