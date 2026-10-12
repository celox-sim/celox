use crate::{
    RuntimeErrorCode, Simulator,
    backend::{EventHandle, MemoryLayout, SimBackend},
    ir::SignalRef,
    simulator::{
        Checkpoint, CheckpointError, InstanceHierarchy, NamedEvent, NamedSignal, StateError,
    },
};
use celox_runtime::{
    EventInfo, ProcessRefs, SimulationExecutor, SimulationSnapshot, SimulationState,
};

/// Saved state of a [`Simulation`], created by [`Simulation::checkpoint`]:
/// the design state together with simulation time, clocks and pending events.
pub struct SimulationCheckpoint<B: SimBackend = crate::DefaultBackend> {
    simulator: Checkpoint,
    schedule: SimulationSnapshot<B>,
}

impl<B: SimBackend> Clone for SimulationCheckpoint<B> {
    fn clone(&self) -> Self {
        Self {
            simulator: self.simulator.clone(),
            schedule: self.schedule.clone(),
        }
    }
}

impl<B: SimBackend> SimulationCheckpoint<B> {
    /// Simulation time at which the checkpoint was taken.
    pub fn time(&self) -> u64 {
        self.schedule.time()
    }

    /// Size of the saved design state in bytes.
    pub fn state_size(&self) -> usize {
        self.simulator.state_size()
    }
}

impl<B: SimBackend> std::fmt::Debug for SimulationCheckpoint<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SimulationCheckpoint")
            .field("time", &self.time())
            .field("state_size", &self.state_size())
            .finish_non_exhaustive()
    }
}

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
    fn finish_requested(&mut self, _time: u64) -> bool {
        self.has_pending_runtime_finish()
    }

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

    fn run_process(&mut self, index: usize) -> Result<(), RuntimeErrorCode> {
        self.backend
            .run_process(index)
            .map_err(|e| self.decorate_runtime_error(e))
    }

    fn tick_many(&mut self, event: B::Event, count: u64) -> (u64, Result<(), RuntimeErrorCode>) {
        if count == 0 {
            return (0, Ok(()));
        }
        if !self.program.runtime_schema.comb_observers.is_empty() {
            if let Err(error) = self.eval_comb_checked() {
                return (0, Err(error));
            }
            return (1, self.tick_deferred_comb(event));
        }
        // Every tick settles the combinational logic first, so a clean
        // state needs no special case and the whole run stays in the
        // backend's loop.
        let (completed, result) = self.backend.eval_comb_apply_ff_many_at(event, count);
        self.dirty = true;
        (
            completed,
            result.map_err(|error| self.decorate_runtime_error(error)),
        )
    }

    fn fused_ticks_allowed(&self) -> bool {
        self.vcd_writer.is_none()
            && !self.components.has_scheduled_components()
            && self.program.runtime_schema.comb_observers.is_empty()
            && !self
                .program
                .runtime_schema
                .runtime_event_sites
                .iter()
                .any(|site| matches!(site.kind, celox_design::RuntimeEventKind::Finish))
            && !self.program.runtime_schema.clock_phase_sensitive_comb
    }

    fn stage_external_event(
        &mut self,
        event: B::Event,
        _timestamp: u64,
    ) -> Result<(), RuntimeErrorCode> {
        self.components.stage_inputs(event.id(), &mut self.backend);
        Ok(())
    }

    fn fire_external_event(
        &mut self,
        event: B::Event,
        timestamp: u64,
    ) -> Result<(), RuntimeErrorCode> {
        let writes = self
            .components
            .fire(event.id(), timestamp)
            .map_err(|message| RuntimeErrorCode::Runtime {
                message,
                signals: Vec::new(),
            })?;
        for write in writes {
            write.apply(&mut self.backend);
            self.dirty = true;
        }
        Ok(())
    }

    fn finish_timed_step(&mut self, timestamp: u64) {
        self.dirty = false;
        self.dump_unless_rewound(timestamp);
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

    /// Build a timed simulation directly from SystemVerilog sources.
    #[cfg(feature = "systemverilog")]
    pub fn from_sv_sources<'a>(
        sources: Vec<(&'a str, &'a std::path::Path)>,
        top: &'a str,
    ) -> crate::SimulatorBuilder<'a, Simulation> {
        crate::SimulatorBuilder::<Simulation>::from_sources(Vec::new(), top)
            .into_sv_sources(sources)
    }

    /// Low-level adapter hook for a timed simulation from an external artifact.
    ///
    /// Frontend crates should wrap this with a constructor named for their own
    /// artifact type.
    pub fn from_frontend(
        artifact: celox_frontend_sdk::FrontendArtifact,
    ) -> crate::SimulatorBuilder<'static, Simulation> {
        crate::SimulatorBuilder::<Simulation>::from_frontend(artifact)
    }
}

// ── Generic methods available for any backend ───────────────────────

pub(crate) fn simulation_state<B: SimBackend>(simulator: &Simulator<B>) -> SimulationState<B> {
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

    let processes = simulator
        .program
        .runtime_schema
        .processes
        .iter()
        .map(|slots| ProcessRefs {
            status: simulator.backend.resolve_signal(&slots.status),
            delay: simulator.backend.resolve_signal(&slots.delay),
            clock: simulator.backend.resolve_signal(&slots.clock),
            clocks: slots
                .clocks
                .iter()
                .map(|clock| celox_runtime::ProcessClockRef {
                    event: simulator
                        .backend
                        .resolve_event_opt(&clock.signal)
                        .expect("a process clock is the clock of a sequential domain"),
                    signal: simulator.backend.resolve_signal(&clock.signal),
                    period: clock.period,
                })
                .collect(),
        })
        .collect();

    SimulationState::new(
        &simulator.backend,
        topo_signals,
        domain_kinds,
        event_info,
        processes,
    )
    .with_active_ff_events(
        simulator
            .program
            .runtime_schema
            .active_ff_events
            .iter()
            .filter_map(|event| simulator.backend.resolve_event_opt(event))
            .collect(),
    )
    .with_process_event_counters(
        simulator
            .program
            .runtime_schema
            .process_event_counters
            .iter()
            .map(|counters| {
                (
                    simulator.backend.resolve_signal(&counters.signal),
                    simulator.backend.resolve_signal(&counters.rises),
                    simulator.backend.resolve_signal(&counters.falls),
                )
            })
            .collect(),
    )
}

impl<B: SimBackend> Simulation<B> {
    pub(crate) fn new(simulator: Simulator<B>) -> Self {
        let state = simulation_state(&simulator);
        Self { simulator, state }
    }

    /// Returns warnings emitted during compilation.
    pub fn warnings(&self) -> &[crate::CompilationWarning] {
        self.simulator.warnings()
    }

    /// Save the design state, simulation time, clocks and pending events.
    pub fn checkpoint(&self) -> Result<SimulationCheckpoint<B>, CheckpointError> {
        Ok(SimulationCheckpoint {
            simulator: self.simulator.checkpoint()?,
            schedule: self.state.snapshot(),
        })
    }

    /// Save the value of every state object by path, together with the
    /// simulation time, clocks and pending events by name.
    pub fn save_state(&mut self) -> Result<celox_runtime::StateFile, StateError> {
        if self.has_processes() {
            return Err(StateError::Processes);
        }
        let mut file = self.simulator.save_state()?;
        let parts = self.state.export_schedule(&self.simulator.backend);
        file.schedule = Some(self.simulator.name_schedule(parts));
        Ok(file)
    }

    /// Load a state file saved from a `Simulation`, including its time,
    /// clocks and pending events. See [`Simulator::load_state`] for how
    /// objects are matched.
    pub fn load_state(&mut self, file: &celox_runtime::StateFile) -> Result<(), StateError> {
        if self.has_processes() {
            return Err(StateError::Processes);
        }
        let record = file.schedule.as_ref().ok_or(StateError::MissingSchedule)?;
        self.simulator.check_vcd_rewind(record.time)?;
        let parts = self.simulator.resolve_schedule(record)?;
        self.simulator.load_state(file)?;
        self.state.import_schedule(parts);
        Ok(())
    }

    /// Return to the state saved in `checkpoint`, including its simulation
    /// time. See [`Simulator::restore`] for what is not rolled back.
    ///
    /// The simulation dumps VCD output at every step, so a restore to a time
    /// the VCD file has already passed is rejected; call [`Self::switch_vcd`]
    /// first.
    pub fn restore(&mut self, checkpoint: &SimulationCheckpoint<B>) -> Result<(), CheckpointError> {
        self.simulator.validate_restore(&checkpoint.simulator)?;
        self.simulator.check_vcd_rewind(checkpoint.time())?;
        let events = self.simulator.backend.id_to_event_slice();
        self.state
            .restore(&checkpoint.schedule, |event| {
                events
                    .get(event.id())
                    .copied()
                    .filter(|local| local.addr() == event.addr())
            })
            .map_err(|_| CheckpointError::DesignMismatch)?;
        self.simulator.restore(&checkpoint.simulator)
    }

    /// Captures the current state of all signals and writes them to the VCD file.
    pub fn dump(&mut self, timestamp: u64) {
        self.simulator.dump(timestamp);
    }

    /// See [`Simulator::try_dump`].
    pub fn try_dump(&mut self, timestamp: u64) -> Result<(), crate::simulator::DumpError> {
        self.simulator.try_dump(timestamp)
    }

    /// See [`Simulator::switch_vcd`].
    pub fn switch_vcd(&mut self, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
        self.simulator.switch_vcd(path)
    }

    pub fn flush_vcd(&mut self) -> std::io::Result<()> {
        self.simulator.flush_vcd()
    }

    /// Resolves a signal path into a performance-optimized [`SignalRef`].
    pub fn signal(&self, path: &str) -> SignalRef {
        self.simulator.signal(path)
    }

    /// Retrieves the current value of a variable using a pre-resolved [`SignalRef`] handle.
    pub fn get(&mut self, signal: SignalRef) -> num_bigint::BigUint {
        self.simulator.get(signal)
    }

    /// Returns the value and unknown-bit mask of a signal.
    pub fn get_four_state(
        &mut self,
        signal: SignalRef,
    ) -> (num_bigint::BigUint, num_bigint::BigUint) {
        self.simulator.get_four_state(signal)
    }

    /// Sets a signal to a known value. The write takes effect at the current
    /// time: processes waiting for it resume when the simulation next runs.
    pub fn set_wide(&mut self, signal: SignalRef, value: num_bigint::BigUint) {
        self.simulator.set_wide(signal, value);
    }

    /// Sets a signal's value and unknown-bit mask, like [`Self::set_wide`].
    pub fn set_four_state(
        &mut self,
        signal: SignalRef,
        value: num_bigint::BigUint,
        mask: num_bigint::BigUint,
    ) {
        self.simulator.set_four_state(signal, value, mask);
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
    ///
    /// A process waiting for an event or a condition that a host write
    /// since the previous call satisfied resumes at the current time
    /// instead: the call then returns the current time without advancing,
    /// so what the process did is observable at its own time.
    pub fn step(&mut self) -> Result<Option<u64>, RuntimeErrorCode> {
        self.settle_host_writes()?;
        if self.state.poll_waiting(&mut self.simulator)? {
            return Ok(Some(self.state.time()));
        }
        self.state.step(&mut self.simulator)
    }

    /// Settle the combinational logic after host writes and resume the
    /// processes they woke at the current time, with the rounds that
    /// follow, as [`Self::step`] does before advancing. Time does not
    /// advance.
    pub fn settle(&mut self) -> Result<(), RuntimeErrorCode> {
        let time = self.state.time();
        if self.simulator.dirty
            || !self.simulator.backend.get_triggered_bits().is_empty()
            || self.state.has_unsettled_edges(&self.simulator.backend)
            || self
                .state
                .next_event_time()
                .is_some_and(|next| next <= time)
        {
            self.settle_host_writes()?;
            self.state.settle_at(&mut self.simulator, time)?;
        } else {
            self.state.poll_waiting(&mut self.simulator)?;
        }
        Ok(())
    }

    /// Settle the combinational logic a host write left dirty, so a process
    /// waiting for a derived value sees it.
    fn settle_host_writes(&mut self) -> Result<(), RuntimeErrorCode> {
        if self.simulator.dirty {
            self.simulator.eval_comb()?;
        }
        if self.simulator.has_pending_runtime_finish() {
            self.state.finish();
        }
        Ok(())
    }

    /// Whether a process is suspended on an event or a condition.
    pub fn has_waiting_processes(&self) -> bool {
        self.state.has_waiting_processes()
    }

    /// Run one step at or before `limit`, including processes a host write
    /// woke at the current time. Leave a later event queued.
    pub fn step_until(&mut self, limit: u64) -> Result<Option<u64>, RuntimeErrorCode> {
        if self.state.time() > limit {
            return Ok(None);
        }
        self.settle_host_writes()?;
        if self.state.poll_waiting_until(&mut self.simulator, limit)? {
            return Ok(Some(self.state.time()));
        }
        if self
            .state
            .next_event_time()
            .is_some_and(|time| time <= limit)
        {
            self.state.step_until(&mut self.simulator, limit)
        } else {
            Ok(None)
        }
    }

    /// Advance time and run until `end_time` (inclusive). As for
    /// [`Self::step`], processes that a host write woke resume first. A time
    /// before the current time leaves the simulation unchanged.
    pub fn run_until(&mut self, end_time: u64) -> Result<(), RuntimeErrorCode> {
        if end_time < self.state.time() {
            return Ok(());
        }
        self.settle_host_writes()?;
        self.state
            .poll_waiting_until(&mut self.simulator, end_time)?;
        while let Some(next_time) = self.state.next_event_time() {
            if next_time > end_time {
                break;
            }
            self.state.step_until(&mut self.simulator, end_time)?;
        }
        if self.state.is_finished() {
            return Ok(());
        }
        self.state.set_time(end_time);
        self.simulator.dump_unless_rewound(end_time);
        Ok(())
    }

    fn has_processes(&self) -> bool {
        !self.simulator.program.runtime_schema.processes.is_empty()
    }

    /// Whether a process of the design requested the end of the simulation.
    /// Once it has, [`Self::step`] returns `None` and [`Self::run_until`]
    /// stops at the time of the request.
    pub fn is_finished(&self) -> bool {
        self.state.is_finished()
    }

    /// Returns the current simulation time.
    pub fn time(&self) -> u64 {
        self.state.time()
    }

    /// Rising edges of the clocks processes wait on that the simulation has
    /// run so far.
    pub fn ticks(&self) -> u64 {
        self.state.ticks()
    }

    /// Periodic clocks registered with [`Self::add_clock`] (or loaded with a
    /// state), as (event id, period).
    pub fn clock_periods(&self) -> Vec<(usize, u64)> {
        self.state.clock_periods()
    }

    /// Takes the runtime events (`$display`, messages, `$finish`, ...) the
    /// design emitted since the last call, in emission order.
    pub fn drain_runtime_events(&mut self) -> Vec<crate::RuntimeEvent> {
        self.simulator.drain_runtime_events()
    }

    /// Returns the time of the next scheduled event, if any.
    pub fn next_event_time(&self) -> Option<u64> {
        self.state.next_event_time()
    }

    /// Directly execute combinational logic evaluation.
    pub fn eval_comb(&mut self) -> Result<(), RuntimeErrorCode> {
        self.simulator.eval_comb()?;
        if self.simulator.has_pending_runtime_finish() {
            self.state.finish();
        }
        Ok(())
    }

    /// Returns a raw pointer to the backend memory and its total size in bytes.
    pub fn memory_as_ptr(&self) -> (*const u8, usize) {
        self.simulator.memory_as_ptr()
    }

    /// Returns a mutable raw pointer to the backend memory and its total size in bytes.
    pub fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        self.simulator.memory_as_mut_ptr()
    }

    /// Returns an opaque owner that keeps the backend memory allocation alive.
    pub fn memory_owner(&self) -> Option<std::sync::Arc<dyn std::any::Any + Send + Sync>> {
        self.simulator.memory_owner()
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

    /// Registers a clock signal by event ID.
    ///
    /// Invalid IDs and IDs that do not resolve to events are ignored for
    /// backward compatibility. Use [`Self::try_add_clock_by_id`] to detect
    /// registration errors.
    pub fn add_clock_by_id(&mut self, event_id: u32, period: u64, initial_delay: u64) {
        let _ = self.try_add_clock_by_id(event_id, period, initial_delay);
    }

    /// Registers a clock signal by event ID, reporting invalid IDs.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeErrorCode::NotAnEvent`] if `event_id` is unknown or
    /// does not resolve to an event (clock or asynchronous reset).
    pub fn try_add_clock_by_id(
        &mut self,
        event_id: u32,
        period: u64,
        initial_delay: u64,
    ) -> Result<(), RuntimeErrorCode> {
        let addr = self
            .simulator
            .backend
            .id_to_addr_slice()
            .get(event_id as usize)
            .copied()
            .ok_or_else(|| RuntimeErrorCode::NotAnEvent(format!("event_id={event_id}")))?;
        let signal = self.simulator.backend.resolve_signal(&addr);
        if let Some(ev) = self.simulator.backend.resolve_event_opt(&addr) {
            self.state.add_clock(ev, signal, period, initial_delay);
            Ok(())
        } else {
            Err(RuntimeErrorCode::NotAnEvent(format!("event_id={event_id}")))
        }
    }

    /// Schedule a one-shot event by event ID.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeErrorCode::NotAnEvent`] if `event_id` is unknown or
    /// does not resolve to a schedulable event.
    pub fn schedule_by_id(
        &mut self,
        event_id: u32,
        time: u64,
        value: u64,
    ) -> Result<(), RuntimeErrorCode> {
        let addr = self
            .simulator
            .backend
            .id_to_addr_slice()
            .get(event_id as usize)
            .copied()
            .ok_or_else(|| RuntimeErrorCode::NotAnEvent(format!("event_id={event_id}")))?;
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

#[cfg(all(test, feature = "host-runtime"))]
mod process_tests {
    use celox_frontend_sdk::{
        BinaryOp, Constant, Edge, ExprId, FrontendArtifact, ModuleBuilder, Statement, UnaryOp,
        ValueType,
    };

    use super::Simulation;
    use crate::{SimBackend, Simulator, SimulatorBuilder};

    fn constant(module: &mut ModuleBuilder, value: u64, width: usize) -> ExprId {
        module.constant(Constant::two_state(value, width).unwrap())
    }

    /// `forever #5 clk = ~clk;` drives a counter, and a second process counts
    /// `steps` one-unit delays into `ticks` before finishing the simulation.
    fn design(four_state: bool, steps: usize) -> FrontendArtifact {
        let bit = ValueType::new(1, false, four_state).unwrap();
        let word = ValueType::new(16, false, four_state).unwrap();
        let mut module = ModuleBuilder::new("Processes").unwrap();
        let clk = module.internal("clk", bit).unwrap();
        let count = module.output("count", word).unwrap();
        let ticks = module.output("ticks", word).unwrap();
        for signal in [clk, count, ticks] {
            let width = if signal == clk { 1 } else { 16 };
            module
                .set_initial(signal, Constant::two_state(0u8, width).unwrap())
                .unwrap();
        }
        let count_expr = module.read(count).unwrap();
        let one = constant(&mut module, 1, 16);
        let next = module.binary(BinaryOp::Add, count_expr, one, word).unwrap();
        let count_target = module.whole(count).unwrap();
        module
            .register(count_target, next, clk, Edge::Posedge, None, None)
            .unwrap();

        let five = constant(&mut module, 5, 8);
        let clk_expr = module.read(clk).unwrap();
        let toggled = module.unary(UnaryOp::BitNot, clk_expr, bit).unwrap();
        let clk_target = module.whole(clk).unwrap();
        module
            .process(vec![Statement::Forever {
                body: vec![
                    Statement::Delay { amount: five },
                    Statement::Assign {
                        target: clk_target,
                        value: toggled,
                    },
                ],
            }])
            .unwrap();

        // Straight-line suspensions, so a long body needs the two-level
        // resume dispatch.
        let unit = constant(&mut module, 1, 8);
        let ticks_expr = module.read(ticks).unwrap();
        let incremented = module.binary(BinaryOp::Add, ticks_expr, one, word).unwrap();
        let ticks_target = module.whole(ticks).unwrap();
        let mut body = Vec::new();
        for _ in 0..steps {
            body.push(Statement::Delay { amount: unit });
            body.push(Statement::Assign {
                target: ticks_target,
                value: incremented,
            });
        }
        body.push(Statement::Finish);
        module.process(body).unwrap();
        module.finish()
    }

    fn check<B: SimBackend>(mut sim: Simulation<B>, steps: u64) {
        let count = sim.signal("count");
        let ticks = sim.signal("ticks");
        // Rising edges at 5, 15 and 25.
        sim.run_until(30).unwrap();
        assert!(!sim.is_finished());
        assert_eq!(sim.get(count), 3u16.into());
        assert_eq!(sim.get(ticks), 30u16.into());
        sim.run_until(u64::MAX - 1).unwrap();
        assert!(sim.is_finished());
        assert_eq!(sim.time(), steps);
        assert_eq!(sim.get(ticks), steps.into());
        assert_eq!(sim.get(count), ((steps + 5) / 10).into());
    }

    fn builder(four_state: bool, steps: usize) -> SimulatorBuilder<'static, Simulator> {
        Simulator::from_frontend(design(four_state, steps))
            .four_state(four_state)
            .emit_triggers()
    }

    fn check_all_backends(four_state: bool, steps: usize) {
        check(
            Simulation::new(builder(four_state, steps).build_interpreter().unwrap()),
            steps as u64,
        );
        check(
            Simulation::new(builder(four_state, steps).build_cranelift().unwrap()),
            steps as u64,
        );
        check(
            Simulation::new(builder(four_state, steps).build_wasm().unwrap()),
            steps as u64,
        );
        check(
            Simulation::new(builder(four_state, steps).build_tiered().unwrap()),
            steps as u64,
        );
        #[cfg(any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        ))]
        {
            check(
                Simulation::new(builder(four_state, steps).build_native().unwrap()),
                steps as u64,
            );
            let image = builder(four_state, steps)
                .compile_native()
                .unwrap()
                .into_program_image();
            let image = crate::NativeProgramImage::from_container_bytes(
                &image.to_container_bytes().unwrap(),
            )
            .unwrap();
            check(
                Simulation::new(
                    Simulator::from_sources(Vec::new(), "Processes")
                        .emit_triggers()
                        .build_native_from_image(image)
                        .unwrap(),
                ),
                steps as u64,
            );
        }
    }

    #[test]
    fn processes_run_on_every_backend() {
        check_all_backends(false, 37);
    }

    #[test]
    fn four_state_processes_run_on_every_backend() {
        check_all_backends(true, 37);
    }

    #[test]
    fn long_processes_dispatch_through_two_switch_levels() {
        check_all_backends(false, 300);
    }
}

/// Event and level waits of SystemVerilog processes on every backend.
#[cfg(all(test, feature = "host-runtime", feature = "systemverilog"))]
mod wait_tests {
    use std::path::Path;

    use super::Simulation;
    use crate::{SimBackend, Simulator, SimulatorBuilder};

    const SOURCE: &str = r#"
        module Top(output logic [7:0] edges, output logic [7:0] levels,
                   output logic [7:0] changes, output logic [7:0] q);
            logic clk = 1'b0;
            logic [7:0] d = 8'd0;
            logic go = 1'b0;
            always #5 clk = ~clk;
            always_ff @(posedge clk) q <= d;
            initial begin
                edges = 8'd0;
                forever begin
                    @(posedge clk);
                    edges = edges + 8'd1;
                end
            end
            initial begin
                levels = 8'd0;
                wait (edges == 8'd3) levels = 8'd1;
                wait (go);
                levels = 8'd2;
            end
            initial begin
                changes = 8'd0;
                forever begin
                    @(d or go);
                    changes = changes + 8'd1;
                end
            end
            initial begin
                #12 d = 8'd1;
                #10 d = 8'd1;
                #10 d = 8'd2;
                @(negedge clk);
                go = 1'b1;
                @(posedge clk);
                $finish;
            end
        endmodule
    "#;

    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        let edges = sim.signal("edges");
        let levels = sim.signal("levels");
        let changes = sim.signal("changes");
        let q = sim.signal("q");
        // Rising edges at 5, 15 and 25; `d` changes at 12 and 32.
        sim.run_until(20).unwrap();
        assert_eq!(sim.get(edges), 2u8.into());
        assert_eq!(sim.get(levels), 0u8.into());
        assert_eq!(sim.get(changes), 1u8.into());
        assert_eq!(sim.get(q), 1u8.into());
        sim.run_until(30).unwrap();
        assert_eq!(sim.get(edges), 3u8.into());
        assert_eq!(sim.get(levels), 1u8.into());
        sim.run_until(u64::MAX - 1).unwrap();
        assert!(sim.is_finished());
        // `go` rises at the negedge at 40; the process finishes at 45.
        assert_eq!(sim.time(), 45);
        assert_eq!(sim.get(edges), 5u8.into());
        assert_eq!(sim.get(levels), 2u8.into());
        assert_eq!(sim.get(changes), 3u8.into());
        assert_eq!(sim.get(q), 2u8.into());
    }

    fn builder(four_state: bool) -> SimulatorBuilder<'static, Simulator> {
        Simulator::from_sv_sources(vec![(SOURCE, Path::new("waits.sv"))], "Top")
            .four_state(four_state)
            .emit_triggers()
    }

    fn check_all_backends(four_state: bool) {
        check(Simulation::new(
            builder(four_state).build_interpreter().unwrap(),
        ));
        check(Simulation::new(
            builder(four_state).build_cranelift().unwrap(),
        ));
        check(Simulation::new(builder(four_state).build_wasm().unwrap()));
        check(Simulation::new(builder(four_state).build_tiered().unwrap()));
        #[cfg(any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        ))]
        check(Simulation::new(builder(four_state).build_native().unwrap()));
    }

    #[test]
    fn waits_run_on_every_backend() {
        check_all_backends(false);
    }

    #[test]
    fn four_state_waits_run_on_every_backend() {
        check_all_backends(true);
    }

    /// A host write that `settle_at` settles wakes a waiting process; the
    /// zero-delay continuations of that process run in the same call.
    #[test]
    fn settle_at_drains_the_rounds_of_its_time() {
        const SOURCE: &str = r#"
            module Top(output logic [7:0] y);
                logic go = 1'b0;
                initial begin
                    y = 8'd0;
                    wait (go);
                    #0 y = 8'd1;
                    #0 y = 8'd2;
                end
            endmodule
        "#;
        let simulator = Simulator::from_sv_sources(vec![(SOURCE, Path::new("settle.sv"))], "Top")
            .build_cranelift()
            .unwrap();
        let mut sim = Simulation::new(simulator);
        assert_eq!(sim.step().unwrap(), Some(0));
        let go = sim.signal("go");
        let y = sim.signal("y");
        assert_eq!(sim.get(y), 0u8.into());
        sim.simulator.set_wide(go, 1u8.into());
        sim.state.settle_at(&mut sim.simulator, 0).unwrap();
        assert_eq!(sim.get(y), 2u8.into());
    }

    /// `settle_at` runs the processes due at its time, and those due
    /// before it first, at their own times.
    #[test]
    fn settle_at_resumes_the_processes_due_at_its_time() {
        const SOURCE: &str = r#"
            module Top(output logic [7:0] y);
                initial begin
                    y = 8'd1;
                    #5 y = 8'd2;
                    #2 y = 8'd3;
                end
            endmodule
        "#;
        let simulator = Simulator::from_sv_sources(vec![(SOURCE, Path::new("due.sv"))], "Top")
            .build_cranelift()
            .unwrap();
        let mut sim = Simulation::new(simulator);
        let y = sim.signal("y");
        assert_eq!(sim.step().unwrap(), Some(0));
        assert_eq!(sim.get(y), 1u8.into());
        assert_eq!(sim.state.settle_at(&mut sim.simulator, 5).unwrap(), Some(5));
        assert_eq!(sim.get(y), 2u8.into());
        assert_eq!(sim.state.settle_at(&mut sim.simulator, 9).unwrap(), Some(9));
        assert_eq!(sim.get(y), 3u8.into());
        assert_eq!(sim.time(), 9);
    }

    /// A process waiting for a value combinational logic derives from a
    /// host write sees the settled value.
    #[test]
    fn host_writes_settle_before_the_waiters_are_polled() {
        const SOURCE: &str = r#"
            module Top(input logic a, input logic b, output logic [7:0] y);
                logic both;
                assign both = a & b;
                initial begin
                    y = 8'd0;
                    wait (both);
                    y = 8'd1;
                    @(negedge both);
                    y = 8'd2;
                end
            endmodule
        "#;
        let simulator = Simulator::from_sv_sources(vec![(SOURCE, Path::new("comb.sv"))], "Top")
            .build_cranelift()
            .unwrap();
        let mut sim = Simulation::new(simulator);
        let (a, b, y) = (sim.signal("a"), sim.signal("b"), sim.signal("y"));
        assert_eq!(sim.step().unwrap(), Some(0));
        sim.modify(|io| io.set(a, 1u8)).unwrap();
        assert_eq!(sim.step().unwrap(), None);
        assert_eq!(sim.get(y), 0u8.into());
        sim.modify(|io| io.set(b, 1u8)).unwrap();
        assert_eq!(sim.step().unwrap(), Some(0));
        assert_eq!(sim.get(y), 1u8.into());
        sim.modify(|io| io.set(a, 0u8)).unwrap();
        sim.run_until(5).unwrap();
        assert_eq!(sim.get(y), 2u8.into());
    }

    /// A process a host write wakes runs in a `step` of its own, at the
    /// current time; the next scheduled time is stepped to afterwards.
    #[test]
    fn a_host_woken_process_runs_in_its_own_step() {
        const SOURCE: &str = r#"
            module Top(output logic [7:0] y);
                logic go = 1'b0;
                initial begin
                    y = 8'd0;
                    wait (go);
                    y = 8'd1;
                    #10 y = 8'd2;
                end
                initial begin
                    #10;
                end
            endmodule
        "#;
        let simulator = Simulator::from_sv_sources(vec![(SOURCE, Path::new("woken.sv"))], "Top")
            .build_cranelift()
            .unwrap();
        let mut sim = Simulation::new(simulator);
        let (go, y) = (sim.signal("go"), sim.signal("y"));
        assert_eq!(sim.step().unwrap(), Some(0));
        sim.simulator.set_wide(go, 1u8.into());
        assert_eq!(sim.step().unwrap(), Some(0));
        assert_eq!(sim.get(y), 1u8.into());
        assert_eq!(sim.step().unwrap(), Some(10));
        assert_eq!(sim.get(y), 2u8.into());
        assert_eq!(sim.step().unwrap(), None);
    }
}

/// Clock waits of processes on every backend: fused where the backend
/// loops in generated code, one tick per call elsewhere.
#[cfg(all(test, feature = "host-runtime"))]
mod clock_wait_tests {
    use celox_frontend_sdk::{
        BinaryOp, Constant, Edge, FrontendArtifact, ModuleBuilder, Statement, ValueType,
    };

    use super::Simulation;
    use crate::{SimBackend, Simulator, SimulatorBuilder};

    /// A counter clocked by a process clock; one process waits 1000 edges
    /// and records the count, another waits 400 and then 5 more.
    fn design(four_state: bool) -> FrontendArtifact {
        let bit = ValueType::new(1, false, four_state).unwrap();
        let word = ValueType::new(16, false, four_state).unwrap();
        let mut module = ModuleBuilder::new("ClockWaits").unwrap();
        let clk = module.internal("clk", bit).unwrap();
        let count = module.output("count", word).unwrap();
        let late = module.output("late", word).unwrap();
        let early = module.output("early", word).unwrap();
        for signal in [count, late, early] {
            module
                .set_initial(signal, Constant::two_state(0u8, 16).unwrap())
                .unwrap();
        }
        let count_expr = module.read(count).unwrap();
        let one = module.constant(Constant::two_state(1u8, 16).unwrap());
        let next = module.binary(BinaryOp::Add, count_expr, one, word).unwrap();
        let count_target = module.whole(count).unwrap();
        module
            .register(count_target, next, clk, Edge::Posedge, None, None)
            .unwrap();
        module.clock_period(clk, 4).unwrap();
        let constant = |module: &mut ModuleBuilder, value: u64| {
            module.constant(Constant::two_state(value, 64).unwrap())
        };
        let thousand = constant(&mut module, 1000);
        let late_target = module.whole(late).unwrap();
        module
            .process(vec![
                Statement::ClockCycles {
                    clock: clk,
                    count: thousand,
                },
                Statement::Assign {
                    target: late_target,
                    value: count_expr,
                },
                Statement::Finish,
            ])
            .unwrap();
        let four_hundred = constant(&mut module, 400);
        let five = constant(&mut module, 5);
        let early_target = module.whole(early).unwrap();
        module
            .process(vec![
                Statement::ClockCycles {
                    clock: clk,
                    count: four_hundred,
                },
                Statement::Assign {
                    target: early_target,
                    value: count_expr,
                },
                Statement::ClockCycles {
                    clock: clk,
                    count: five,
                },
            ])
            .unwrap();
        module.finish()
    }

    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        let count = sim.signal("count");
        let early = sim.signal("early");
        let late = sim.signal("late");
        sim.run_until(1601).unwrap();
        assert_eq!(sim.get(early), 400u16.into());
        assert_eq!(sim.get(count), 401u16.into());
        assert_eq!(sim.ticks(), 401);
        sim.run_until(u64::MAX - 1).unwrap();
        assert!(sim.is_finished());
        assert_eq!(sim.time(), 4000);
        assert_eq!(sim.get(late), 1000u16.into());
        assert_eq!(sim.get(count), 1000u16.into());
        assert_eq!(sim.ticks(), 1000);
    }

    fn builder(four_state: bool) -> SimulatorBuilder<'static, Simulator> {
        Simulator::from_frontend(design(four_state))
            .four_state(four_state)
            .emit_triggers()
    }

    fn check_all_backends(four_state: bool) {
        check(Simulation::new(
            builder(four_state).build_interpreter().unwrap(),
        ));
        check(Simulation::new(
            builder(four_state).build_cranelift().unwrap(),
        ));
        check(Simulation::new(builder(four_state).build_wasm().unwrap()));
        check(Simulation::new(builder(four_state).build_tiered().unwrap()));
        #[cfg(any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        ))]
        check(Simulation::new(builder(four_state).build_native().unwrap()));
    }

    #[test]
    fn clock_waits_run_on_every_backend() {
        check_all_backends(false);
    }

    #[test]
    fn four_state_clock_waits_run_on_every_backend() {
        check_all_backends(true);
    }
    fn edge_design(edge: Edge) -> FrontendArtifact {
        let bit = ValueType::bits(1).unwrap();
        let mut module = ModuleBuilder::new("Edges").unwrap();
        let clk = module.internal("clk", bit).unwrap();
        let q = module.output("q", bit).unwrap();
        let clock_value = module.read(clk).unwrap();
        let one = module.constant(Constant::two_state(1u8, 1).unwrap());
        let value = if edge == Edge::Posedge {
            clock_value
        } else {
            one
        };
        let target = module.whole(q).unwrap();
        module
            .register(target, value, clk, edge, None, None)
            .unwrap();
        module.clock_period(clk, 4).unwrap();
        let three = module.constant(Constant::two_state(3u8, 8).unwrap());
        module
            .process(vec![
                Statement::ClockCycles {
                    clock: clk,
                    count: three,
                },
                Statement::Finish,
            ])
            .unwrap();
        module.finish()
    }

    fn check_edge<B: SimBackend>(mut sim: Simulation<B>, edge: Edge) {
        sim.run_until(0).unwrap();
        assert_eq!(
            sim.get(sim.signal("q")),
            u8::from(edge == Edge::Posedge).into()
        );
        sim.run_until(1).unwrap();
        assert_eq!(
            sim.get(sim.signal("q")),
            u8::from(edge == Edge::Posedge).into()
        );
        sim.run_until(2).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 1u8.into());
        sim.run_until(20).unwrap();
        assert_eq!(sim.time(), 12);
        assert_eq!(sim.ticks(), 3);
        assert!(sim.is_finished());
    }

    #[test]
    fn clock_levels_and_falling_edges_agree_across_backends() {
        for edge in [Edge::Posedge, Edge::Negedge] {
            let builder = || Simulator::from_frontend(edge_design(edge)).emit_triggers();
            check_edge(
                Simulation::new(builder().build_interpreter().unwrap()),
                edge,
            );
            check_edge(Simulation::new(builder().build_cranelift().unwrap()), edge);
            check_edge(Simulation::new(builder().build_wasm().unwrap()), edge);
            check_edge(Simulation::new(builder().build_tiered().unwrap()), edge);
            check_edge(Simulation::new(builder().build_native().unwrap()), edge);
        }
    }

    #[test]
    fn settle_at_does_not_fuse_past_a_delay_boundary() {
        // Use a dedicated SDK design so settle_at itself advances to the due delay.
        let bit = ValueType::bits(1).unwrap();
        let mut module = ModuleBuilder::new("Boundary").unwrap();
        let clk = module.internal("clk", bit).unwrap();
        let q = module.output("q", bit).unwrap();
        let target = module.whole(q).unwrap();
        let one = module.constant(Constant::two_state(1u8, 1).unwrap());
        module
            .register(target, one, clk, Edge::Posedge, None, None)
            .unwrap();
        module.clock_period(clk, 2).unwrap();
        let five = module.constant(Constant::two_state(5u8, 8).unwrap());
        let ten = module.constant(Constant::two_state(10u8, 8).unwrap());
        module
            .process(vec![
                Statement::Delay { amount: five },
                Statement::ClockCycles {
                    clock: clk,
                    count: ten,
                },
            ])
            .unwrap();
        let mut sim = Simulation::from_frontend(module.finish()).build().unwrap();
        assert_eq!(sim.state.settle_at(&mut sim.simulator, 5).unwrap(), Some(5));
        assert_eq!(sim.time(), 5);
        assert_eq!(sim.ticks(), 1);
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod process_review_tests {
    use super::*;

    fn check_failed_batch<B: SimBackend>(mut sim: Simulator<B>) {
        let event = sim.event("clk");
        let (completed, result) = SimulationExecutor::tick_many(&mut sim, event, 10);
        assert_eq!(completed, 0);
        assert!(result.is_err());
        assert_eq!(sim.get(sim.signal("q")), 0u8.into());
    }

    #[test]
    fn failed_pre_edge_comb_does_not_complete_a_clock_tick() {
        let source = "module Top(input bit clk, input bit ok, output bit q); always_comb assert(ok) else $fatal(1, \"failed comb\"); always_ff @(posedge clk) q <= 1; endmodule";
        let builder =
            || Simulator::from_sv_sources(vec![(source, std::path::Path::new("batch.sv"))], "Top");
        check_failed_batch(builder().build_interpreter().unwrap());
        check_failed_batch(builder().build_cranelift().unwrap());
        check_failed_batch(builder().build_wasm().unwrap());
        check_failed_batch(builder().build_tiered().unwrap());
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check_failed_batch(builder().build_native().unwrap());
    }

    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(2).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 1u8.into());
        assert_eq!(sim.get(sim.signal("observed")), 0u8.into());
    }

    #[test]
    fn transient_edges_and_inactive_region_on_all_backends() {
        let source = r#"module Top(output bit q, observed);
            bit clk;
            always_ff @(posedge clk) q <= 1;
            initial begin @(posedge clk); #0; observed = q; end
            initial begin #1; clk = 1; clk = 0; end
        endmodule"#;
        let reverse = source
            .replace("bit clk;", "bit clk = 1;")
            .replace("clk = 1; clk = 0;", "clk = 0; clk = 1;");
        let unknown = source
            .replace("bit clk;", "logic clk = 0;")
            .replace("clk = 1; clk = 0;", "clk = 1'bx; clk = 0;");
        for (source, four_state) in [
            (source, false),
            (reverse.as_str(), false),
            (unknown.as_str(), true),
        ] {
            let builder = || {
                Simulator::from_sv_sources(vec![(source, std::path::Path::new("review.sv"))], "Top")
                    .four_state(four_state)
                    .emit_triggers()
            };
            check(Simulation::new(builder().build_interpreter().unwrap()));
            check(Simulation::new(builder().build_cranelift().unwrap()));
            check(Simulation::new(builder().build_wasm().unwrap()));
            check(Simulation::new(builder().build_tiered().unwrap()));
            #[cfg(any(
                all(target_arch = "x86_64", not(feature = "arm64-codegen")),
                target_arch = "aarch64"
            ))]
            {
                check(Simulation::new(builder().build().unwrap()));
                let image = builder().compile_native().unwrap().into_program_image();
                let image = crate::NativeProgramImage::from_container_bytes(
                    &image.to_container_bytes().unwrap(),
                )
                .unwrap();
                check(Simulation::new(
                    Simulator::from_sources(Vec::new(), "Top")
                        .emit_triggers()
                        .build_native_from_image(image)
                        .unwrap(),
                ));
            }
        }
    }
}

#[cfg(test)]
mod bounded_clock_phase_tests {
    use super::*;
    use crate::frontend_sdk::{
        BinaryOp, Constant, Edge, ModuleBuilder, Statement, UnaryOp, ValueType,
    };

    fn design() -> crate::frontend_sdk::FrontendArtifact {
        let bit = ValueType::bits(1).unwrap();
        let mut module = ModuleBuilder::new("Top").unwrap();
        let clk = module.internal("clk", bit).unwrap();
        let enable = module.output("enable", bit).unwrap();
        let v = module.output("v", bit).unwrap();
        let one = module.constant(Constant::two_state(1u8, 1).unwrap());
        let zero = module.constant(Constant::two_state(0u8, 1).unwrap());
        let target = module.whole(enable).unwrap();
        module
            .register(target, one, clk, Edge::Posedge, None, None)
            .unwrap();
        module.clock_period(clk, 4).unwrap();
        let clock = module.read(clk).unwrap();
        let enabled = module.read(enable).unwrap();
        let previous = module.read(v).unwrap();
        let low = module.unary(UnaryOp::BitNot, clock, bit).unwrap();
        let active = module.binary(BinaryOp::And, low, enabled, bit).unwrap();
        let toggled = module.unary(UnaryOp::BitNot, previous, bit).unwrap();
        let next = module.mux(active, toggled, zero).unwrap();
        let target = module.whole(v).unwrap();
        module.assign(target, next).unwrap();
        let count = module.constant(Constant::two_state(10u8, 64).unwrap());
        module
            .process(vec![Statement::ClockCycles { clock: clk, count }])
            .unwrap();
        module.finish()
    }

    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        assert!(sim.run_until(40).is_err());
        assert_eq!(sim.time(), 2);
        assert_eq!(sim.ticks(), 1);
    }

    #[test]
    fn low_phase_comb_failures_stop_before_later_fused_edges() {
        check(
            Simulation::from_frontend(design())
                .true_loop(
                    (Vec::new(), vec!["v".into()]),
                    (Vec::new(), vec!["v".into()]),
                    8,
                )
                .build_interpreter()
                .unwrap(),
        );
        check(
            Simulation::from_frontend(design())
                .true_loop(
                    (Vec::new(), vec!["v".into()]),
                    (Vec::new(), vec!["v".into()]),
                    8,
                )
                .build_cranelift()
                .unwrap(),
        );
        check(
            Simulation::from_frontend(design())
                .true_loop(
                    (Vec::new(), vec!["v".into()]),
                    (Vec::new(), vec!["v".into()]),
                    8,
                )
                .build_wasm()
                .unwrap(),
        );
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(
            Simulation::from_frontend(design())
                .true_loop(
                    (Vec::new(), vec!["v".into()]),
                    (Vec::new(), vec!["v".into()]),
                    8,
                )
                .build_native()
                .unwrap(),
        );
    }

    #[cfg(feature = "systemverilog")]
    #[test]
    fn step_until_leaves_later_events_queued() {
        let source =
            "module Top(input bit clk, output bit q); always_ff @(posedge clk) q<=1; endmodule";
        let mut sim =
            Simulation::from_sv_sources(vec![(source, std::path::Path::new("limit.sv"))], "Top")
                .build()
                .unwrap();
        sim.schedule("clk", 5, 1).unwrap();
        assert_eq!(sim.state.step_until(&mut sim.simulator, 4).unwrap(), None);
        assert_eq!(sim.time(), 0);
        assert_eq!(sim.next_event_time(), Some(5));
        assert_eq!(sim.get(sim.signal("q")), 0u8.into());
        assert_eq!(
            sim.state.step_until(&mut sim.simulator, 5).unwrap(),
            Some(5)
        );
        assert_eq!(sim.get(sim.signal("q")), 1u8.into());
    }

    #[cfg(feature = "systemverilog")]
    fn check_past_limit<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(100).unwrap();
        let gate = sim.signal("gate");
        let seen = sim.signal("seen");
        sim.set_wide(gate, 1u8.into());
        sim.run_until(60).unwrap();
        assert_eq!(sim.time(), 100);
        assert_eq!(sim.get(seen), 0u8.into());
        assert!(sim.has_waiting_processes());
        sim.run_until(100).unwrap();
        assert_eq!(sim.get(seen), 1u8.into());
    }

    #[cfg(feature = "systemverilog")]
    #[test]
    fn past_run_limit_does_not_poll_waiters_or_rewind_time() {
        let source = "module Top(input bit gate, output bit seen); initial begin wait(gate); seen=1; end endmodule";
        let builder =
            || Simulation::from_sv_sources(vec![(source, std::path::Path::new("past.sv"))], "Top");
        check_past_limit(builder().build_interpreter().unwrap());
        check_past_limit(builder().build_cranelift().unwrap());
        check_past_limit(builder().build_wasm().unwrap());
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check_past_limit(builder().build_native().unwrap());
    }

    fn coincident_design() -> crate::frontend_sdk::FrontendArtifact {
        let bit = ValueType::bits(1).unwrap();
        let byte = ValueType::bits(8).unwrap();
        let mut module = ModuleBuilder::new("Coincident").unwrap();
        let clk = module.internal("clk", bit).unwrap();
        let other = module.internal("other", bit).unwrap();
        let count = module.output("count", byte).unwrap();
        let sampled = module.output("sampled", byte).unwrap();
        let seen = module.output("seen", bit).unwrap();
        let count_expr = module.read(count).unwrap();
        let one = module.constant(Constant::two_state(1u8, 8).unwrap());
        let two = module.constant(Constant::two_state(2u8, 8).unwrap());
        let next = module.binary(BinaryOp::Add, count_expr, one, byte).unwrap();
        let target = module.whole(count).unwrap();
        module
            .register(target, next, clk, Edge::Posedge, None, None)
            .unwrap();
        let target = module.whole(sampled).unwrap();
        module
            .register(target, count_expr, other, Edge::Posedge, None, None)
            .unwrap();
        module.clock_period(clk, 4).unwrap();
        let target = module.whole(seen).unwrap();
        let value = module.read(clk).unwrap();
        module
            .process(vec![
                Statement::ClockCycles {
                    clock: clk,
                    count: one,
                },
                Statement::Assign { target, value },
            ])
            .unwrap();
        module
            .process(vec![Statement::ClockCycles {
                clock: clk,
                count: two,
            }])
            .unwrap();
        module.finish()
    }

    fn check_coincident<B: SimBackend>(mut sim: Simulation<B>) {
        sim.schedule("other", 4, 1).unwrap();
        sim.run_until(4).unwrap();
        assert_eq!(sim.get(sim.signal("seen")), 0u8.into());
        assert_eq!(sim.get(sim.signal("count")), 2u8.into());
        assert_eq!(sim.get(sim.signal("sampled")), 1u8.into());
        assert_eq!(sim.ticks(), 2);
        sim.run_until(2).unwrap();
        assert_eq!(sim.time(), 4);
        assert_eq!(sim.ticks(), 2);
    }

    #[test]
    fn clock_wait_resumes_before_coincident_generated_edge() {
        check_coincident(
            Simulation::from_frontend(coincident_design())
                .build_interpreter()
                .unwrap(),
        );
        check_coincident(
            Simulation::from_frontend(coincident_design())
                .build_cranelift()
                .unwrap(),
        );
        check_coincident(
            Simulation::from_frontend(coincident_design())
                .build_wasm()
                .unwrap(),
        );
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check_coincident(
            Simulation::from_frontend(coincident_design())
                .build_native()
                .unwrap(),
        );
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod ff_counter_regressions {
    use super::*;

    #[test]
    fn blocking_ff_pulses_trigger_other_ff_domains() {
        let source = r#"module Top(input bit clk, output bit q);
        bit c=0;
        always @(posedge clk) begin c=1; c=0; end
        always @(posedge c) q<=1;
    endmodule"#;
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.schedule("clk", 1, 1).unwrap();
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("q")), 1u8.into());
            assert_eq!(sim.get(sim.signal("c")), 0u8.into());
        }
        let builder = || {
            Simulator::from_sv_sources(vec![(source, std::path::Path::new("ff-pulse.sv"))], "Top")
        };
        check(Simulation::new(builder().build_interpreter().unwrap()));
        check(Simulation::new(builder().build_cranelift().unwrap()));
        check(Simulation::new(builder().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(builder().build_native().unwrap()));
    }
}

#[cfg(test)]
mod clock_failure_progress_tests {
    use super::*;
    use crate::frontend_sdk::{
        BinaryOp, Constant, Edge, ModuleBuilder, Statement, UnaryOp, ValueType,
    };

    fn design(on_rising: bool) -> crate::frontend_sdk::FrontendArtifact {
        module_design(on_rising).0.finish()
    }

    fn module_design(on_rising: bool) -> (ModuleBuilder, crate::frontend_sdk::SignalId) {
        let bit = ValueType::bits(1).unwrap();
        let word = ValueType::bits(32).unwrap();
        let mut m = ModuleBuilder::new("Progress").unwrap();
        let other = m.internal("other", bit).unwrap();
        let clk = m.internal("clk", bit).unwrap();
        let bad = m.input("bad", bit).unwrap();
        let count = m.output("count", word).unwrap();
        let falls = m.output("falls", word).unwrap();
        let fallclk = m.internal("fallclk", bit).unwrap();
        let clk_expr = m.read(clk).unwrap();
        let low = m.unary(UnaryOp::BitNot, clk_expr, bit).unwrap();
        let count_expr = m.read(count).unwrap();
        let zero = m.constant(Constant::two_state(0u8, 32).unwrap());
        let running = m.binary(BinaryOp::NotEqual, count_expr, zero, bit).unwrap();
        let fall_phase = m.binary(BinaryOp::And, low, running, bit).unwrap();
        let target = m.whole(fallclk).unwrap();
        m.assign(target, fall_phase).unwrap();
        for (signal, clock) in [(count, clk), (falls, fallclk)] {
            let value = m.read(signal).unwrap();
            let one = m.constant(Constant::two_state(1u8, 32).unwrap());
            let next = m.binary(BinaryOp::Add, value, one, word).unwrap();
            let target = m.whole(signal).unwrap();
            m.register(target, next, clock, Edge::Posedge, None, None)
                .unwrap();
        }
        let v = m.output("v", bit).unwrap();
        let phase = m.read(clk).unwrap();
        let phase = if on_rising {
            phase
        } else {
            m.unary(UnaryOp::BitNot, phase, bit).unwrap()
        };
        let bad = m.read(bad).unwrap();
        let active = m.binary(BinaryOp::And, phase, bad, bit).unwrap();
        let count = m.read(count).unwrap();
        let zero = m.constant(Constant::two_state(0u8, 32).unwrap());
        let nonzero = m.binary(BinaryOp::NotEqual, count, zero, bit).unwrap();
        let active = m.binary(BinaryOp::And, active, nonzero, bit).unwrap();
        let previous = m.read(v).unwrap();
        let toggled = m.unary(UnaryOp::BitNot, previous, bit).unwrap();
        let zero = m.constant(Constant::two_state(0u8, 1).unwrap());
        let next = m.mux(active, toggled, zero).unwrap();
        let target = m.whole(v).unwrap();
        m.assign(target, next).unwrap();
        m.clock_period(clk, 4).unwrap();
        let count = m.constant(Constant::two_state(2u8, 64).unwrap());
        m.process(vec![Statement::ClockCycles { clock: clk, count }])
            .unwrap();
        (m, other)
    }

    fn check<B: SimBackend>(mut sim: Simulation<B>, on_rising: bool) {
        sim.set_wide(sim.signal("bad"), 1u8.into());
        let time = if on_rising { 0 } else { 2 };
        assert!(sim.run_until(time).is_err());
        assert_eq!(sim.time(), time);
        assert_eq!(sim.ticks(), 1);
        let count = sim.signal("count");
        let falls = sim.signal("falls");
        assert_eq!(sim.simulator.backend.get_as::<u32>(count), 1);
        assert_eq!(sim.simulator.backend.get_as::<u32>(falls), 0);
        assert_eq!(sim.next_event_time(), Some(2));
        sim.set_wide(sim.signal("bad"), 0u8.into());
        sim.run_until(2).unwrap();
        assert_eq!(sim.get(falls), 1u32.into());
        assert_eq!(sim.get(count), 1u32.into());
        assert_eq!(sim.ticks(), 1);
        sim.run_until(4).unwrap();
        assert_eq!(sim.get(count), 2u32.into());
        assert_eq!(sim.ticks(), 2);
    }

    #[test]
    fn generated_clock_failures_retain_committed_and_pending_phases() {
        for on_rising in [true, false] {
            let builder = || {
                Simulation::from_frontend(design(on_rising)).true_loop(
                    (Vec::new(), vec!["v".into()]),
                    (Vec::new(), vec!["v".into()]),
                    8,
                )
            };
            check(builder().build_interpreter().unwrap(), on_rising);
            check(builder().build_cranelift().unwrap(), on_rising);
            check(builder().build_wasm().unwrap(), on_rising);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(builder().build_native().unwrap(), on_rising);
        }
    }

    struct FailingExecutor<B: SimBackend> {
        sim: Simulator<B>,
        fail: Option<usize>,
        in_apply: bool,
    }
    impl<B: SimBackend> SimulationExecutor for FailingExecutor<B> {
        type Backend = B;
        fn backend(&self) -> &B {
            &self.sim.backend
        }
        fn backend_mut(&mut self) -> &mut B {
            &mut self.sim.backend
        }
        fn eval_comb(&mut self) -> Result<(), RuntimeErrorCode> {
            SimulationExecutor::eval_comb(&mut self.sim)
        }
        fn eval_apply_ff_at(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
            SimulationExecutor::eval_apply_ff_at(&mut self.sim, event)
        }
        fn eval_only_ff_at(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
            if !self.in_apply && self.fail == Some(event.id()) {
                return Err(RuntimeErrorCode::Runtime {
                    message: "injected FF failure".into(),
                    signals: Vec::new(),
                });
            }
            SimulationExecutor::eval_only_ff_at(&mut self.sim, event)
        }
        fn apply_ff_at(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
            if self.in_apply && self.fail == Some(event.id()) {
                return Err(RuntimeErrorCode::Runtime {
                    message: "injected publication failure".into(),
                    signals: Vec::new(),
                });
            }
            SimulationExecutor::apply_ff_at(&mut self.sim, event)
        }
        fn fused_ticks_allowed(&self) -> bool {
            false
        }
    }

    fn coincident_design() -> crate::frontend_sdk::FrontendArtifact {
        let (mut m, other) = module_design(false);
        let word = ValueType::bits(32).unwrap();
        let count = m.output("other_count", word).unwrap();
        let current = m.read(count).unwrap();
        let one = m.constant(Constant::two_state(1u8, 32).unwrap());
        let next = m.binary(BinaryOp::Add, current, one, word).unwrap();
        let target = m.whole(count).unwrap();
        m.register(target, next, other, Edge::Posedge, None, None)
            .unwrap();
        m.clock_period(other, 2).unwrap();
        let two = m.constant(Constant::two_state(2u8, 64).unwrap());
        m.process(vec![Statement::ClockCycles {
            clock: other,
            count: two,
        }])
        .unwrap();
        m.finish()
    }

    fn check_coincident_failure<B: SimBackend>(sim: Simulator<B>, in_apply: bool) {
        let mut state = simulation_state(&sim);
        let mut executor = FailingExecutor {
            sim,
            fail: None,
            in_apply,
        };
        assert_eq!(state.step_until(&mut executor, 0).unwrap(), Some(0));
        let initial_ticks = state.ticks();
        assert_eq!(state.step_until(&mut executor, 1).unwrap(), Some(1));
        let failed = executor
            .sim
            .event(if in_apply { "fallclk" } else { "other" });
        executor.fail = Some(failed.id());
        assert!(state.step_until(&mut executor, 2).is_err());
        assert_eq!(state.ticks(), initial_ticks + u64::from(in_apply));
        let falls = executor.sim.signal("falls");
        assert_eq!(executor.backend().get_as::<u32>(falls), 0);
        assert_eq!(state.next_event_time(), Some(2));
        executor.fail = None;
        assert_eq!(state.step_until(&mut executor, 2).unwrap(), Some(2));
        assert_eq!(executor.backend().get_as::<u32>(falls), 1);
        assert_eq!(state.ticks(), initial_ticks + 1);
        assert_eq!(
            executor
                .backend()
                .get_as::<u32>(executor.sim.signal("other_count")),
            2
        );
    }

    #[test]
    fn coincident_ff_failures_do_not_discard_unpublished_falling_domains() {
        for in_apply in [false, true] {
            let builder = || {
                Simulator::from_frontend(coincident_design())
                    .emit_triggers()
                    .true_loop(
                        (Vec::new(), vec!["v".into()]),
                        (Vec::new(), vec!["v".into()]),
                        8,
                    )
            };
            check_coincident_failure(builder().build_interpreter().unwrap(), in_apply);
            check_coincident_failure(builder().build_cranelift().unwrap(), in_apply);
            check_coincident_failure(builder().build_wasm().unwrap(), in_apply);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check_coincident_failure(builder().build_native().unwrap(), in_apply);
        }
    }
}

#[cfg(test)]
mod clock_provenance_checkpoint_tests {
    use super::*;
    use crate::frontend_sdk::{Constant, Edge, ModuleBuilder, Statement, ValueType};
    fn design() -> crate::frontend_sdk::FrontendArtifact {
        let mut m = ModuleBuilder::new("Top").unwrap();
        let clk = m.internal("clk", ValueType::bits(1).unwrap()).unwrap();
        let q = m.output("q", ValueType::bits(1).unwrap()).unwrap();
        let one = m.constant(Constant::two_state(1u8, 64).unwrap());
        let value = m.constant(Constant::two_state(1u8, 1).unwrap());
        let target = m.whole(q).unwrap();
        m.register(target, value, clk, Edge::Posedge, None, None)
            .unwrap();
        m.clock_period(clk, 4).unwrap();
        m.process(vec![
            Statement::Delay { amount: one },
            Statement::ClockCycles {
                clock: clk,
                count: one,
            },
        ])
        .unwrap();
        m.finish()
    }
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.schedule("clk", 0, 1).unwrap();
        assert_eq!(sim.step().unwrap(), Some(0));
        let checkpoint = sim.checkpoint().unwrap();
        sim.run_until(2).unwrap();
        sim.restore(&checkpoint).unwrap();
        // Model a host waking the suspended process at the restored timestamp.
        let mut parts = sim.state.export_schedule(&sim.simulator.backend);
        parts.process_wakeups = vec![(0, 0)];
        sim.state.import_schedule(parts);
        let error = sim.step().unwrap_err().to_string();
        assert!(error.contains("already driven by schedule"), "{error}");
        assert_eq!(sim.ticks(), 0);
    }
    #[test]
    fn restored_clock_drive_provenance_rejects_a_same_time_generator() {
        check(
            Simulation::from_frontend(design())
                .build_interpreter()
                .unwrap(),
        );
        check(
            Simulation::from_frontend(design())
                .build_cranelift()
                .unwrap(),
        );
        check(Simulation::from_frontend(design()).build_wasm().unwrap());
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::from_frontend(design()).build_native().unwrap());
    }
}
#[cfg(all(test, feature = "systemverilog"))]
mod active_ff_metadata_tests {
    #[test]
    fn pending_control_flags_do_not_request_active_settling() {
        for (body, expected) in [("q<=d;", false), ("q=d;", true), ("q<=d; q=d;", true)] {
            let code = format!(
                "module Top(input bit clk,d,output bit q); always @(posedge clk) begin {body} end initial #1; endmodule"
            );
            let sim = crate::Simulator::from_sv_sources(
                vec![(&code, std::path::Path::new("active_ff.sv"))],
                "Top",
            )
            .build_interpreter()
            .unwrap();
            assert_eq!(
                !sim.program.runtime_schema.active_ff_events.is_empty(),
                expected,
                "{body}"
            );
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod rtl_finish_backend_tests {
    use super::*;
    #[test]
    fn host_finish_stops_at_current_time_without_waiters() {
        fn check<B: SimBackend>(mut sim: Simulation<B>, future: bool) {
            if future {
                sim.schedule("clk", 10, 1).unwrap();
            }
            sim.simulator.set_wide(sim.signal("go"), 1u8.into());
            assert_eq!(sim.step().unwrap(), None);
            assert!(sim.is_finished());
            assert_eq!(sim.time(), 0);
            sim.run_until(20).unwrap();
            assert_eq!(sim.time(), 0);
            assert_eq!(sim.get(sim.signal("q")), 0u8.into());
        }
        let code = "module Top(input bit go,clk,output bit q);always_comb if(go)$finish;always_ff @(posedge clk)q<=1;endmodule";
        for future in [false, true] {
            let builder = || {
                Simulator::from_sv_sources(
                    vec![(code, std::path::Path::new("host_finish.sv"))],
                    "Top",
                )
            };
            check(
                Simulation::new(builder().build_interpreter().unwrap()),
                future,
            );
            check(
                Simulation::new(builder().build_cranelift().unwrap()),
                future,
            );
            check(Simulation::new(builder().build_wasm().unwrap()), future);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(Simulation::new(builder().build_native().unwrap()), future);
        }
    }

    #[test]
    fn finish_stops_ff_and_comb_publication_on_all_backends() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(2).unwrap();
            assert!(sim.is_finished());
            assert_eq!(sim.get(sim.signal("q")), 0u8.into());
        }
        for body in [
            "always_comb begin q=0;if(go)begin $finish;q=1;end end",
            "always @(posedge go)begin q<=1;$finish;q=1;end",
        ] {
            let code = format!("module Top(output bit q);bit go;initial #1 go=1;{body} endmodule");
            let builder = || {
                Simulator::from_sv_sources(vec![(&code, std::path::Path::new("finish.sv"))], "Top")
            };
            check(Simulation::new(builder().build_interpreter().unwrap()));
            check(Simulation::new(builder().build_cranelift().unwrap()));
            check(Simulation::new(builder().build_wasm().unwrap()));
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(Simulation::new(builder().build_native().unwrap()));
        }
    }
}

#[cfg(test)]
mod finish_clock_batch_tests {
    use super::*;
    use crate::frontend_sdk::{BinaryOp, Constant, Edge, ModuleBuilder, Statement, ValueType};
    #[test]
    fn coincident_finish_suppresses_an_overflowing_next_edge() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(u64::MAX - 1).unwrap();
            assert!(sim.is_finished());
            assert_eq!(sim.ticks(), 1);
            assert_eq!(sim.get(sim.signal("q")), 1u8.into());
        }
        let bit = ValueType::bits(1).unwrap();
        let mut m = ModuleBuilder::new("Top").unwrap();
        let clk = m.internal("clk", bit).unwrap();
        let q = m.output("q", bit).unwrap();
        let one = m.constant(Constant::two_state(1u8, 1).unwrap());
        let dst = m.whole(q).unwrap();
        m.register(dst, one, clk, Edge::Posedge, None, None)
            .unwrap();
        m.clock_period(clk, 2).unwrap();
        let start = m.constant(Constant::two_state(u64::MAX - 3, 64).unwrap());
        let end = m.constant(Constant::two_state(u64::MAX - 1, 64).unwrap());
        let two = m.constant(Constant::two_state(2u8, 64).unwrap());
        m.process(vec![
            Statement::Delay { amount: start },
            Statement::ClockCycles {
                clock: clk,
                count: two,
            },
        ])
        .unwrap();
        m.process(vec![Statement::Delay { amount: end }, Statement::Finish])
            .unwrap();
        let design = m.finish();
        check(Simulation::new(
            Simulator::from_frontend(design.clone())
                .build_interpreter()
                .unwrap(),
        ));
        check(Simulation::new(
            Simulator::from_frontend(design.clone())
                .build_cranelift()
                .unwrap(),
        ));
        check(Simulation::new(
            Simulator::from_frontend(design.clone())
                .build_wasm()
                .unwrap(),
        ));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(
            Simulator::from_frontend(design).build_native().unwrap(),
        ));
    }

    #[test]
    fn finish_suppresses_coincident_generated_edges() {
        fn check<B: SimBackend>(mut sim: Simulation<B>, scheduled: bool) {
            if scheduled {
                sim.schedule("other", 4, 1).unwrap();
            }
            sim.run_until(4).unwrap();
            assert!(sim.is_finished());
            assert_eq!(sim.get(sim.signal("count")), 2u8.into());
            assert_eq!(sim.ticks(), 2);
        }
        for scheduled in [false, true] {
            let bit = ValueType::bits(1).unwrap();
            let word = ValueType::bits(8).unwrap();
            let mut m = ModuleBuilder::new("Top").unwrap();
            let clk = m.internal("clk", bit).unwrap();
            let other = m.internal("other", bit).unwrap();
            let q = m.output("count", word).unwrap();
            let r = m.output("other_q", bit).unwrap();
            let v = m.read(q).unwrap();
            let one = m.constant(Constant::two_state(1u8, 8).unwrap());
            let next = m.binary(BinaryOp::Add, v, one, word).unwrap();
            let dst = m.whole(q).unwrap();
            m.register(dst, next, clk, Edge::Posedge, None, None)
                .unwrap();
            let dst = m.whole(r).unwrap();
            let one = m.constant(Constant::two_state(1u8, 1).unwrap());
            m.register(dst, one, other, Edge::Posedge, None, None)
                .unwrap();
            m.clock_period(clk, 2).unwrap();
            let ten = m.constant(Constant::two_state(10u8, 64).unwrap());
            let four = m.constant(Constant::two_state(4u8, 64).unwrap());
            m.process(vec![Statement::ClockCycles {
                clock: clk,
                count: ten,
            }])
            .unwrap();
            m.process(vec![Statement::Delay { amount: four }, Statement::Finish])
                .unwrap();
            let design = m.finish();
            check(
                Simulation::new(
                    Simulator::from_frontend(design.clone())
                        .build_interpreter()
                        .unwrap(),
                ),
                scheduled,
            );
            check(
                Simulation::new(
                    Simulator::from_frontend(design.clone())
                        .build_cranelift()
                        .unwrap(),
                ),
                scheduled,
            );
            check(
                Simulation::new(
                    Simulator::from_frontend(design.clone())
                        .build_wasm()
                        .unwrap(),
                ),
                scheduled,
            );
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(
                Simulation::new(Simulator::from_frontend(design).build_native().unwrap()),
                scheduled,
            );
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod finish_and_cotrigger_tests {
    use super::*;
    #[test]
    fn process_finish_prevents_a_scheduled_domains_publication() {
        let source = "module Top(input bit clk,output bit q);initial #5 $finish;always @(posedge clk)q<=1;endmodule";
        let mut sim =
            Simulation::from_sv_sources(vec![(source, std::path::Path::new("finish.sv"))], "Top")
                .build()
                .unwrap();
        sim.schedule("clk", 5, 1).unwrap();
        sim.run_until(5).unwrap();
        assert!(sim.is_finished());
        assert_eq!(sim.get(sim.signal("q")), 0u8.into());
    }
    #[test]
    fn cotriggered_syntactic_cascades_record_nba_expression_pulses() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.schedule("c1", 5, 1).unwrap();
            sim.schedule("c2", 5, 1).unwrap();
            sim.run_until(5).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), 1u8.into());
        }
        let source = "module Top(input bit c1,output bit c2,seen);bit a,b;always @(posedge c1)begin c2<=1;a<=1;end always @(posedge c2)b<=1;initial begin @(a^b);seen=1;end endmodule";
        let builder = || {
            Simulator::from_sv_sources(vec![(source, std::path::Path::new("cascade.sv"))], "Top")
        };
        check(Simulation::new(builder().build_interpreter().unwrap()));
        check(Simulation::new(builder().build_cranelift().unwrap()));
        check(Simulation::new(builder().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(builder().build_native().unwrap()));
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod idle_settle_tests {
    use super::*;
    #[test]
    fn settled_reads_do_not_repeat_comb_side_effects() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            let a = sim.signal("a");
            sim.set_wide(a, 1u8.into());
            sim.run_until(0).unwrap();
            assert!(
                sim.drain_runtime_events()
                    .iter()
                    .any(|e| matches!(e,crate::RuntimeEvent::Display { message } if message=="x"))
            );
            sim.settle().unwrap();
            sim.settle().unwrap();
            assert_eq!(sim.get(sim.signal("q")), 1u8.into());
            assert!(sim.drain_runtime_events().is_empty());
        }
        let source = "module Top(input bit a,output bit q);always @* begin q=a;if(a)$display(\"x\");end endmodule";
        let builder =
            || Simulator::from_sv_sources(vec![(source, std::path::Path::new("settle.sv"))], "Top");
        check(Simulation::new(builder().build_interpreter().unwrap()));
        check(Simulation::new(builder().build_cranelift().unwrap()));
        check(Simulation::new(builder().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(builder().build_native().unwrap()));
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod active_comb_nba_tests {
    use super::*;
    #[test]
    fn blocking_comb_waiters_run_before_nbas_on_every_backend() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            sim.schedule("clk", 1, 1).unwrap();
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("q")), 1u8.into());
            assert_eq!(sim.get(sim.signal("observed")), 0u8.into());
        }
        let source = "module Top(input bit clk,output bit q,observed);bit a;wire c=a;always @(posedge clk)begin a=1;q<=1;end initial begin @(posedge c);observed=q;end endmodule";
        let builder =
            || Simulator::from_sv_sources(vec![(source, std::path::Path::new("active.sv"))], "Top");
        check(Simulation::new(builder().build_interpreter().unwrap()));
        check(Simulation::new(builder().build_cranelift().unwrap()));
        check(Simulation::new(builder().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(builder().build_native().unwrap()));
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod static_task_ff_tests {
    use super::*;
    #[test]
    fn static_ff_task_state_survives_checkpoint_and_state_file() {
        fn check<B: SimBackend>(mut sim: Simulation<B>, expected: u8) {
            sim.schedule("clk", 1, 1).unwrap();
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("out")), expected.into());
            let checkpoint = sim.checkpoint().unwrap();
            let file = sim.save_state().unwrap();
            fn next<B: SimBackend>(sim: &mut Simulation<B>, expected: u8) {
                sim.schedule("clk", 2, 0).unwrap();
                sim.schedule("clk", 3, 1).unwrap();
                sim.run_until(3).unwrap();
                assert_eq!(sim.get(sim.signal("out")), expected.into());
            }
            next(&mut sim, expected + 1);
            sim.restore(&checkpoint).unwrap();
            next(&mut sim, expected + 1);
            sim.load_state(&file).unwrap();
            next(&mut sim, expected + 1);
        }
        for (assignment, expected) in [("n=n+1", 11), ("n[3:0]=n[3:0]+1", 11), ("n<=n+1", 10)] {
            let source = format!(
                "module Top(input bit clk,output bit[7:0]out);task tick(output bit[7:0]value);static bit[7:0]n=10;{assignment};value=n;endtask always_ff @(posedge clk)tick(out);endmodule"
            );
            let builder = || {
                Simulator::from_sv_sources(
                    vec![(&source, std::path::Path::new("static.sv"))],
                    "Top",
                )
            };
            check(
                Simulation::new(builder().build_interpreter().unwrap()),
                expected,
            );
            check(
                Simulation::new(builder().build_cranelift().unwrap()),
                expected,
            );
            check(Simulation::new(builder().build_wasm().unwrap()), expected);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(Simulation::new(builder().build_native().unwrap()), expected);
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod direct_finish_tests {
    use super::*;
    #[test]
    fn direct_comb_settle_finishes_without_consuming_a_future_event() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.schedule("clk", 10, 1).unwrap();
            sim.run_until(0).unwrap();
            sim.simulator.set(sim.signal("go"), 1u8);
            sim.eval_comb().unwrap();
            assert!(sim.is_finished());
            assert_eq!(sim.next_event_time(), None);
            assert_eq!(sim.time(), 0);
            assert!(
                sim.drain_runtime_events()
                    .contains(&crate::RuntimeEvent::Finish)
            );
            assert_eq!(sim.step().unwrap(), None);
            assert_eq!(sim.get(sim.signal("q")), 0u8.into());
        }
        let source = "module Top(input bit clk,go,output bit q);always_comb if(go)$finish;always @(posedge clk)q<=1;endmodule";
        let builder =
            || Simulator::from_sv_sources(vec![(source, std::path::Path::new("finish.sv"))], "Top");
        check(Simulation::new(builder().build_interpreter().unwrap()));
        check(Simulation::new(builder().build_cranelift().unwrap()));
        check(Simulation::new(builder().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(builder().build_native().unwrap()));
    }
}
