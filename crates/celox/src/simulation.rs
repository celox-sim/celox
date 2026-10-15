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
    pending_host_work: bool,
}

impl<B: SimBackend> Clone for SimulationCheckpoint<B> {
    fn clone(&self) -> Self {
        Self {
            simulator: self.simulator.clone(),
            schedule: self.schedule.clone(),
            pending_host_work: self.pending_host_work,
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
    pending_host_work: bool,
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

    fn scheduled_updates_need_settle(&self) -> bool {
        !self.program.runtime_schema.comb_observers.is_empty()
            || self
                .program
                .runtime_schema
                .runtime_event_sites
                .iter()
                .any(|site| matches!(site.kind, celox_design::RuntimeEventKind::Finish))
            || self.components.has_scheduled_components()
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
            let result = self.tick_deferred_comb(event);
            return (u64::from(result.is_ok()), result);
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
    let kinds = simulator.program.design.event_kinds();
    for (_, id, _) in topo_signals.iter().copied() {
        if id != usize::MAX {
            let addr = simulator.backend.id_to_addr_slice()[id];
            domain_kinds[id] = kinds.get(&addr).copied();
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
        Self {
            simulator,
            state,
            pending_host_work: false,
        }
    }

    /// Returns warnings emitted during compilation.
    pub fn warnings(&self) -> &[crate::CompilationWarning] {
        self.simulator.warnings()
    }

    /// Save the design state, simulation time, clocks and pending events.
    pub fn checkpoint(&self) -> Result<SimulationCheckpoint<B>, CheckpointError> {
        if self.simulator.escaped_mutable_memory
            && !self
                .state
                .escaped_host_clock_writes(&self.simulator.backend)
                .is_empty()
        {
            return Err(CheckpointError::UntrackedHostClockWrite);
        }
        Ok(SimulationCheckpoint {
            simulator: self.simulator.checkpoint()?,
            schedule: self.state.snapshot(),
            pending_host_work: self.pending_host_work,
        })
    }

    /// Save the value of every state object by path, together with the
    /// simulation time, clocks and pending events by name.
    pub fn save_state(&mut self) -> Result<celox_runtime::StateFile, StateError> {
        if self.has_processes() {
            return Err(StateError::Processes);
        }
        let was_dirty = self.simulator.dirty;
        let mut file = if self.state.is_finished() {
            self.simulator
                .state_schema()
                .capture(&self.simulator.backend)
        } else {
            self.simulator.save_state()?
        };
        self.synchronize_runtime_finish(was_dirty);
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
        // V1 recorded Boolean (known) baselines. Masks of the restored
        // current values may belong to a still-pending write and cannot be
        // substituted for those historical baselines.
        self.state.import_schedule(parts);
        self.pending_host_work = false;
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
        self.simulator.restore(&checkpoint.simulator)?;
        self.pending_host_work = checkpoint.pending_host_work;
        Ok(())
    }

    /// Captures the current state of all signals and writes them to the VCD file.
    pub fn dump(&mut self, timestamp: u64) {
        if let Err(error) = self.try_dump(timestamp) {
            panic!("VCD dump at {timestamp} failed: {error}");
        }
    }

    /// See [`Simulator::try_dump`].
    pub fn try_dump(&mut self, timestamp: u64) -> Result<(), crate::simulator::DumpError> {
        let was_dirty = self.simulator.dirty;
        let result = if self.state.is_finished() {
            self.simulator.try_dump_without_evaluation(timestamp)
        } else {
            self.simulator.try_dump(timestamp)
        };
        self.synchronize_runtime_finish(was_dirty);
        result
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

    fn record_escaped_host_clock_writes(&mut self) {
        if self.simulator.escaped_mutable_memory {
            for signal in self
                .state
                .escaped_host_clock_writes(&self.simulator.backend)
            {
                if !self.simulator.pending_host_writes.contains(&signal) {
                    self.simulator.pending_host_writes.push(signal);
                }
            }
        }
    }

    fn synchronize_runtime_finish(&mut self, was_dirty: bool) {
        if !self.simulator.dirty {
            self.pending_host_work = false;
        }
        if self.simulator.has_pending_runtime_finish() {
            self.state.finish();
        } else if was_dirty && !self.simulator.dirty && !self.state.is_finished() {
            self.state
                .sample_waiting_events(&mut self.simulator)
                .unwrap();
            self.state.capture_host_edges(&self.simulator.backend);
        }
    }

    // A write itself can settle eagerly while a RuntimeEventDrain is active.
    // In that case publish its edges even if the simulator was clean before
    // the write; otherwise retain the notification until the next settle.
    fn synchronize_host_write(&mut self) {
        self.pending_host_work = true;
        self.synchronize_runtime_finish(true);
    }

    /// Retrieves the current value of a variable using a pre-resolved [`SignalRef`] handle.
    pub fn get(&mut self, signal: SignalRef) -> num_bigint::BigUint {
        self.record_escaped_host_clock_writes();
        let was_dirty = self.simulator.dirty;
        let value = if self.state.is_finished() {
            self.simulator.backend.get(signal)
        } else {
            self.simulator.get(signal)
        };
        self.synchronize_runtime_finish(was_dirty);
        value
    }

    /// Returns the value and unknown-bit mask of a signal.
    pub fn get_four_state(
        &mut self,
        signal: SignalRef,
    ) -> (num_bigint::BigUint, num_bigint::BigUint) {
        self.record_escaped_host_clock_writes();
        let was_dirty = self.simulator.dirty;
        let value = if self.state.is_finished() {
            self.simulator.backend.get_four_state(signal)
        } else {
            self.simulator.get_four_state(signal)
        };
        self.synchronize_runtime_finish(was_dirty);
        value
    }

    /// Sets a signal to a known value. The write takes effect at the current
    /// time: processes waiting for it resume when the simulation next runs.
    pub fn set_wide(&mut self, signal: SignalRef, value: num_bigint::BigUint) {
        self.simulator.set_wide(signal, value);
        self.synchronize_host_write();
    }

    /// Sets a signal's value and unknown-bit mask, like [`Self::set_wide`].
    pub fn set_four_state(
        &mut self,
        signal: SignalRef,
        value: num_bigint::BigUint,
        mask: num_bigint::BigUint,
    ) {
        self.simulator.set_four_state(signal, value, mask);
        self.synchronize_host_write();
    }

    /// Modifies internal state via a callback and re-stabilizes combinational logic.
    pub fn modify<F>(&mut self, f: F) -> Result<(), RuntimeErrorCode>
    where
        F: FnOnce(&mut crate::IOContext<B>),
    {
        if self.state.is_finished() {
            self.simulator.modify_without_evaluation(f);
            return Ok(());
        }
        let result = self.simulator.modify(f);
        self.synchronize_host_write();
        result
    }

    fn validate_event_time(&self, time: u64) -> Result<(), RuntimeErrorCode> {
        if time < self.state.time() {
            return Err(RuntimeErrorCode::Runtime {
                message: format!(
                    "cannot schedule an event in the past: {time} before {}",
                    self.state.time()
                ),
                signals: Vec::new(),
            });
        }
        Ok(())
    }

    /// Register a clock signal and its period, enqueuing the first edge.
    /// Odd periods alternate floor/ceil half periods. Periods below two
    /// time units use a two-unit period so both phases advance time.
    /// `initial_delay` specifies when the first rising edge occurs. Past
    /// timestamps are ignored by this compatibility API; use
    /// [`Self::try_add_clock_by_id`] for an error result.
    pub fn add_clock(&mut self, port: &str, period: u64, initial_delay: u64) {
        if self.validate_event_time(initial_delay).is_err() {
            return;
        }
        let signal = self.simulator.signal(port);
        let addr = self.simulator.program.get_addr(&[], &[port]).unwrap();
        if let Some(ev) = self.simulator.backend.resolve_event_opt(&addr) {
            self.state.add_clock(ev, signal, period, initial_delay);
        }
    }

    /// Schedule a one-shot event at a specific time.
    /// The signal must be registered as an event (clock or async reset) in the backend.
    pub fn schedule(&mut self, port: &str, time: u64, value: u64) -> Result<(), RuntimeErrorCode> {
        self.validate_event_time(time)?;
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
        if self.state.is_finished() {
            return Ok(None);
        }
        self.settle_host_writes()?;
        if self.state.is_finished() {
            return Ok(None);
        }
        if self.state.poll_waiting(&mut self.simulator)? {
            return Ok((!self.state.is_finished()).then_some(self.state.time()));
        }
        if self.state.has_unsettled_host_edges(&self.simulator.backend) {
            let time = self.state.time();
            let active = self
                .state
                .has_unsettled_active_edges(&self.simulator.backend);
            let queued = self.state.next_event_time() == Some(time);
            if queued {
                let result = self.state.step(&mut self.simulator)?;
                return Ok(result.filter(|_| !self.state.is_finished()));
            }
            self.state.settle_at(&mut self.simulator, time)?;
            if self.state.is_finished() {
                return Ok(None);
            }
            if active {
                return Ok(Some(time));
            }
        }
        self.state.step(&mut self.simulator)
    }

    /// Settle the combinational logic after host writes and resume the
    /// processes they woke at the current time, with the rounds that
    /// follow, as [`Self::step`] does before advancing. Time does not
    /// advance.
    pub fn settle(&mut self) -> Result<(), RuntimeErrorCode> {
        // A lazy output read can settle the host write and leave only its
        // finish event pending. Synchronize termination before the clean path.
        if self.simulator.has_pending_runtime_finish() {
            self.state.finish();
        }
        if self.state.is_finished() {
            return Ok(());
        }
        let time = self.state.time();
        if !self.simulator.pending_host_writes.is_empty()
            || self.simulator.dirty
            || !self.simulator.backend.get_triggered_bits().is_empty()
            || self.state.has_unsettled_edges(&self.simulator.backend)
            || self
                .state
                .next_event_time()
                .is_some_and(|next| next <= time)
        {
            self.settle_host_writes()?;
            if self.state.is_finished() {
                return Ok(());
            }
            let mut drained = false;
            while self
                .state
                .next_event_time()
                .is_some_and(|next| next <= time)
            {
                self.state.step_until(&mut self.simulator, time)?;
                drained = true;
            }
            if !drained {
                self.state.settle_at(&mut self.simulator, time)?;
            }
        } else {
            self.state.poll_waiting_until(&mut self.simulator, time)?;
        }
        Ok(())
    }

    /// Settle the combinational logic a host write left dirty, so a process
    /// waiting for a derived value sees it.
    fn settle_host_writes(&mut self) -> Result<(), RuntimeErrorCode> {
        if self.state.is_finished() {
            return Ok(());
        }
        self.record_escaped_host_clock_writes();
        let writes = self.simulator.take_host_writes();
        for &signal in &writes {
            if let Err(error) = self.state.validate_host_write(signal) {
                self.simulator.pending_host_writes = writes;
                return Err(error);
            }
        }
        if self.simulator.dirty {
            self.eval_comb()?;
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

    /// Whether settled host writes require work at the current time.
    /// Wait conditions are probed without executing their continuations.
    pub fn has_pending_host_work(&mut self) -> Result<bool, RuntimeErrorCode> {
        self.settle_host_writes()?;
        self.state.probe_waiting(&mut self.simulator)?;
        Ok(self.state.next_event_time() == Some(self.state.time()))
    }

    /// Run one step at or before `limit`, including processes a host write
    /// woke at the current time. Leave a later event queued.
    pub fn step_until(&mut self, limit: u64) -> Result<Option<u64>, RuntimeErrorCode> {
        if self.state.is_finished() {
            return Ok(None);
        }
        if self.state.time() > limit {
            return Ok(None);
        }
        self.settle_host_writes()?;
        if self.state.is_finished() {
            return Ok(None);
        }
        if self.state.poll_waiting_until(&mut self.simulator, limit)? {
            return Ok((!self.state.is_finished()).then_some(self.state.time()));
        }
        if self.state.has_unsettled_host_edges(&self.simulator.backend) {
            let time = self.state.time();
            let active = self
                .state
                .has_unsettled_active_edges(&self.simulator.backend);
            let queued = self.state.next_event_time() == Some(time);
            if queued {
                let result = self.state.step_until(&mut self.simulator, limit)?;
                return Ok(result.filter(|_| !self.state.is_finished()));
            }
            self.state.settle_at(&mut self.simulator, time)?;
            if self.state.is_finished() {
                return Ok(None);
            }
            if active {
                return Ok(Some(time));
            }
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
        if self.state.is_finished() {
            return Ok(());
        }
        if self.state.time() > end_time {
            return Ok(());
        }
        self.settle_host_writes()?;
        self.state
            .poll_waiting_until(&mut self.simulator, end_time)?;
        if self.state.has_unsettled_host_edges(&self.simulator.backend) {
            let time = self.state.time();
            if self.state.next_event_time() == Some(time) {
                self.state.step_until(&mut self.simulator, end_time)?;
            } else {
                self.state.settle_at(&mut self.simulator, time)?;
            }
        }
        if self.state.is_finished() {
            return Ok(());
        }
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
        self.record_escaped_host_clock_writes();
        let was_dirty = self.simulator.dirty;
        let events = if self.state.is_finished() {
            self.simulator.drain_runtime_events_without_evaluation()
        } else {
            self.simulator.drain_runtime_events()
        };
        self.synchronize_runtime_finish(was_dirty);
        events
    }

    /// Returns the time of the next scheduled event, if any.
    pub fn next_event_time(&self) -> Option<u64> {
        if self.pending_host_work && self.state.has_waiting_processes() && !self.state.is_finished()
        {
            Some(self.state.time())
        } else {
            self.state.next_event_time()
        }
    }

    /// Directly execute combinational logic evaluation.
    pub fn eval_comb(&mut self) -> Result<(), RuntimeErrorCode> {
        self.record_escaped_host_clock_writes();
        for &signal in &self.simulator.pending_host_writes {
            self.state.validate_host_write(signal)?;
        }
        self.simulator.eval_comb()?;
        if self.simulator.has_pending_runtime_finish() {
            self.state.finish();
        }
        self.state.sample_waiting_events(&mut self.simulator)?;
        self.state.capture_host_edges(&self.simulator.backend);
        self.pending_host_work = false;
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
    /// does not resolve to an event (clock or asynchronous reset), or a runtime
    /// error if the initial edge would precede the current time.
    pub fn try_add_clock_by_id(
        &mut self,
        event_id: u32,
        period: u64,
        initial_delay: u64,
    ) -> Result<(), RuntimeErrorCode> {
        self.validate_event_time(initial_delay)?;
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
        self.validate_event_time(time)?;
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
    #[test]
    fn odd_clock_phase_survives_checkpoint_across_backends() {
        fn check<B: crate::SimBackend>(mut backend: crate::Simulator<B>) {
            // Use a known low initial clock in both state representations.
            let clock = backend.signal("clk");
            let count = backend.signal("count");
            backend
                .modify(|io| {
                    io.set(clock, 0u8);
                    io.set(count, 0u8);
                })
                .unwrap();
            let mut sim = Simulation::new(backend);
            sim.add_clock("clk", 3, 5);
            sim.run_until(11).unwrap();
            assert_eq!(sim.get(count), 3u8.into());
            assert_eq!(sim.get(clock), 1u8.into());
            assert_eq!(sim.next_event_time(), Some(13));
            let saved = sim.checkpoint().unwrap();
            sim.run_until(12).unwrap();
            assert_eq!(sim.get(clock), 1u8.into());
            sim.run_until(13).unwrap();
            assert_eq!(sim.get(clock), 0u8.into());
            sim.run_until(14).unwrap();
            assert_eq!(sim.get(count), 4u8.into());
            sim.restore(&saved).unwrap();
            assert_eq!(sim.time(), 11);
            assert_eq!(sim.get(count), 3u8.into());
            assert_eq!(sim.get(clock), 1u8.into());
            assert_eq!(sim.next_event_time(), Some(13));
            sim.run_until(12).unwrap();
            assert_eq!(sim.get(clock), 1u8.into());
            sim.run_until(13).unwrap();
            assert_eq!(sim.get(clock), 0u8.into());
            sim.run_until(14).unwrap();
            assert_eq!(sim.get(count), 4u8.into());
        }
        let code = "module Top (clk: input clock, count: output logic<8>) { always_ff (clk) { count += 1; } }";
        for four_state in [false, true] {
            let build = || {
                crate::Simulator::builder(code, "Top")
                    .four_state(four_state)
                    .emit_triggers()
            };
            check(build().build_interpreter().unwrap());
            check(build().build_cranelift().unwrap());
            check(build().build_wasm().unwrap());
            check(build().build_tiered().unwrap());
            #[cfg(any(
                all(target_arch = "x86_64", not(feature = "arm64-codegen")),
                all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
            ))]
            check(build().build_native().unwrap());
        }
    }
    #[cfg(feature = "systemverilog")]
    const GENERATED_ALIAS_SOURCE: &str = r#"
module Generator(input logic clk, output logic g);
    initial g = 0;
    always_ff @(posedge clk) g <= ~g;
endmodule
module Top(input logic cp, cn, output logic [7:0] p, n);
    logic gp, gn;
    Generator up(.clk(cp), .g(gp));
    Generator un(.clk(cn), .g(gn));
    initial begin p = 0; n = 0; end
    always_ff @(posedge gp) p <= p + 1;
    always_ff @(negedge gn) n <= n + 1;
endmodule
"#;

    #[cfg(feature = "systemverilog")]
    #[test]
    fn generated_alias_edges_keep_their_instances_and_stable_phases() {
        fn check<B: crate::SimBackend>(mut backend: crate::Simulator<B>) {
            for (expected, address) in backend
                .program
                .design
                .events
                .ordered_events
                .iter()
                .enumerate()
            {
                let event = backend.backend.resolve_event(address);
                assert_eq!(
                    celox_runtime::backend::EventHandle::id(&event),
                    expected,
                    "event handles must match IDs embedded in trigger instructions"
                );
            }
            let cp = backend.signal("cp");
            let cn = backend.signal("cn");
            backend
                .modify(|io| {
                    io.set(cp, 0u8);
                    io.set(cn, 0u8);
                })
                .unwrap();
            let mut sim = Simulation::new(backend);
            sim.add_clock("cp", 10, 2);
            sim.add_clock("cn", 10, 7);
            sim.run_until(27).unwrap();
            assert_eq!(sim.get(sim.signal("p")), 2u8.into());
            assert_eq!(sim.get(sim.signal("n")), 1u8.into());
        }
        let source = GENERATED_ALIAS_SOURCE;
        for four_state in [false, true] {
            let build = || {
                crate::Simulator::from_sv_sources(
                    vec![(source, std::path::Path::new("generated_alias.sv"))],
                    "Top",
                )
                .four_state(four_state)
                .emit_triggers()
            };
            check(build().build_interpreter().unwrap());
            check(build().build_cranelift().unwrap());
            check(build().build_wasm().unwrap());
            check(build().build_tiered().unwrap());
            #[cfg(any(
                all(target_arch = "x86_64", not(feature = "arm64-codegen")),
                all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
            ))]
            check(build().build_native().unwrap());
        }
    }
    #[cfg(all(
        feature = "systemverilog",
        any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        )
    ))]
    #[test]
    fn timed_alias_edges_and_checkpoint_survive_tiered_promotion() {
        for four_state in [false, true] {
            let mut backend = crate::Simulator::from_sv_sources(
                vec![(
                    GENERATED_ALIAS_SOURCE,
                    std::path::Path::new("promoted_alias.sv"),
                )],
                "Top",
            )
            .four_state(four_state)
            .emit_triggers()
            .tier_promotion(crate::TierPromotion::AfterSteps(32))
            .build_tiered()
            .unwrap();
            let cp = backend.signal("cp");
            let cn = backend.signal("cn");
            backend
                .modify(|io| {
                    io.set(cp, 0u8);
                    io.set(cn, 0u8);
                })
                .unwrap();
            let mut sim = Simulation::new(backend);
            sim.add_clock("cp", 10, 2);
            sim.add_clock("cn", 10, 7);
            let saved = sim.checkpoint().unwrap();
            assert!(!sim.simulator.is_compiled());
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut until = 27u64;
            loop {
                sim.run_until(until).unwrap();
                assert_eq!(
                    sim.get(sim.signal("p")),
                    (((until - 2) / 20 + 1) & 255).into()
                );
                assert_eq!(
                    sim.get(sim.signal("n")),
                    (((until - 17) / 20 + 1) & 255).into()
                );
                if sim.simulator.is_compiled() {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "timed tiered backend did not promote"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
                until += 10;
            }
            sim.restore(&saved).unwrap();
            assert!(sim.simulator.is_compiled());
            sim.run_until(27).unwrap();
            assert_eq!(sim.get(sim.signal("p")), 2u8.into());
            assert_eq!(sim.get(sim.signal("n")), 1u8.into());
        }
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
        module_design_with_failure(on_rising, true)
    }

    fn module_design_with_failure(
        on_rising: bool,
        after_first_rise: bool,
    ) -> (ModuleBuilder, crate::frontend_sdk::SignalId) {
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
        let active = if after_first_rise {
            m.binary(BinaryOp::And, active, nonzero, bit).unwrap()
        } else {
            active
        };
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

    #[test]
    fn escaped_memory_preserves_unpublished_generated_phase_retries() {
        fn check<B: SimBackend>(mut sim: Simulation<B>, on_rising: bool) {
            let clock = sim.signal("clk");
            let (pointer, length) = sim.memory_as_mut_ptr();
            assert!(clock.offset < length);
            if !on_rising {
                sim.run_until(0).unwrap();
            }
            sim.set_wide(sim.signal("bad"), 1u8.into());
            let time = if on_rising { 0 } else { 2 };
            let error = sim.run_until(time).unwrap_err().to_string();
            assert!(!error.contains("direct host write"), "{error}");
            assert_eq!(sim.ticks(), u64::from(!on_rising));
            // Merely obtaining the pointer does not invalidate a retained
            // scheduler drive, even before that drive is published.
            let checkpoint = sim.checkpoint().unwrap();
            for step in [true, false] {
                sim.restore(&checkpoint).unwrap();
                let parts = sim.state.export_schedule(&sim.simulator.backend);
                sim.state.import_schedule(parts);
                let error = sim.run_until(time).unwrap_err().to_string();
                assert!(!error.contains("direct host write"), "{error}");
                sim.set_wide(sim.signal("bad"), 0u8.into());
                if step {
                    assert_eq!(sim.step_until(time).unwrap(), Some(time));
                } else {
                    sim.run_until(time).unwrap();
                }
                sim.run_until(2).unwrap();
                assert_eq!(
                    sim.get(sim.signal("count")),
                    1u32.into(),
                    "rising={on_rising}, step={step}"
                );
                assert_eq!(sim.get(sim.signal("falls")), 1u32.into());
                assert_eq!(sim.ticks(), 1);
                sim.run_until(4).unwrap();
                assert_eq!(sim.get(sim.signal("count")), 2u32.into());
                assert_eq!(sim.ticks(), 2);
            }
            sim.restore(&checkpoint).unwrap();
            // A real pointer write back to the OLD sampling baseline must
            // still be rejected while the generated drive is pending.
            unsafe {
                pointer.add(clock.offset).write(u8::from(!on_rising));
            }
            assert!(matches!(
                sim.checkpoint(),
                Err(CheckpointError::UntrackedHostClockWrite)
            ));
            let error = sim.step_until(time).unwrap_err().to_string();
            assert!(error.contains("direct host write"), "{error}");
            sim.restore(&checkpoint).unwrap();
            sim.set_wide(sim.signal("bad"), 0u8.into());
            sim.run_until(4).unwrap();
            assert_eq!(sim.ticks(), 2);
        }
        for on_rising in [true, false] {
            let build = || {
                Simulator::from_frontend(module_design_with_failure(on_rising, false).0.finish())
                    .true_loop(
                        (Vec::new(), vec!["v".into()]),
                        (Vec::new(), vec!["v".into()]),
                        8,
                    )
            };
            check(
                Simulation::new(build().build_interpreter().unwrap()),
                on_rising,
            );
            check(
                Simulation::new(build().build_cranelift().unwrap()),
                on_rising,
            );
            check(Simulation::new(build().build_wasm().unwrap()), on_rising);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(Simulation::new(build().build_native().unwrap()), on_rising);
        }
    }

    #[test]
    fn custom_executors_keep_per_edge_hooks_without_fusion_opt_in() {
        struct Host<B: SimBackend> {
            sim: Simulator<B>,
            staged: Vec<u64>,
            fired: Vec<u64>,
        }
        impl<B: SimBackend> SimulationExecutor for Host<B> {
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
                SimulationExecutor::eval_only_ff_at(&mut self.sim, event)
            }
            fn apply_ff_at(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
                SimulationExecutor::apply_ff_at(&mut self.sim, event)
            }
            fn stage_external_event(
                &mut self,
                _event: B::Event,
                time: u64,
            ) -> Result<(), RuntimeErrorCode> {
                self.staged.push(time);
                Ok(())
            }
            fn fire_external_event(
                &mut self,
                _event: B::Event,
                time: u64,
            ) -> Result<(), RuntimeErrorCode> {
                self.fired.push(time);
                Ok(())
            }
            // Intentionally rely on the default capability and tick_many.
        }
        fn check<B: SimBackend>(sim: Simulator<B>) {
            let mut state = simulation_state(&sim);
            let mut host = Host {
                sim,
                staged: Vec::new(),
                fired: Vec::new(),
            };
            while state.next_event_time().is_some_and(|time| time <= 4) {
                state.step_until(&mut host, 4).unwrap();
            }
            assert_eq!(state.ticks(), 3);
            assert_eq!(host.staged, vec![0, 2, 4]);
            assert_eq!(host.fired, vec![0, 2, 4]);
            assert_eq!(host.sim.backend.get_as::<u8>(host.sim.signal("q")), 3);
        }
        let mut m = ModuleBuilder::new("Top").unwrap();
        let clk = m.internal("clk", ValueType::bits(1).unwrap()).unwrap();
        let word = ValueType::bits(8).unwrap();
        let q = m.output("q", word).unwrap();
        let current = m.read(q).unwrap();
        let one = m.constant(Constant::two_state(1u8, 8).unwrap());
        let next = m.binary(BinaryOp::Add, current, one, word).unwrap();
        m.register(m.whole(q).unwrap(), next, clk, Edge::Posedge, None, None)
            .unwrap();
        m.clock_period(clk, 2).unwrap();
        let count = m.constant(Constant::two_state(3u8, 64).unwrap());
        m.process(vec![Statement::ClockCycles { clock: clk, count }])
            .unwrap();
        let design = m.finish();
        let build = || Simulator::from_frontend(design.clone());
        check(build().build_interpreter().unwrap());
        check(build().build_cranelift().unwrap());
        check(build().build_wasm().unwrap());
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(build().build_native().unwrap());
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
        assert!(
            state
                .escaped_host_clock_writes(executor.backend())
                .is_empty()
        );
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
    #[test]
    fn failed_clock_activations_remain_errors_on_retry() {
        fn check<B: SimBackend>(mut sim: Simulation<B>, scheduled: bool) {
            if scheduled {
                sim.schedule("clk", 0, 1).unwrap();
            } else {
                sim.add_clock("clk", 4, 0);
            }
            assert_eq!(sim.step().unwrap(), Some(0));
            let expected = if scheduled {
                "already driven by schedule"
            } else {
                "already driven by add_clock"
            };
            let error = sim.run_until(2).unwrap_err().to_string();
            assert!(error.contains(expected), "{error}");
            let time = sim.time();
            for _ in 0..2 {
                let error = sim.step().unwrap_err().to_string();
                assert!(error.contains(expected), "{error}");
                let error = sim.run_until(20).unwrap_err().to_string();
                assert!(error.contains(expected), "{error}");
                assert_eq!(sim.time(), time);
                assert_eq!(sim.ticks(), 0);
            }
            let saved = sim.checkpoint().unwrap();
            sim.restore(&saved).unwrap();
            let error = sim.step().unwrap_err().to_string();
            assert!(error.contains(expected), "{error}");
        }
        for scheduled in [false, true] {
            check(
                Simulation::from_frontend(design())
                    .build_interpreter()
                    .unwrap(),
                scheduled,
            );
            check(
                Simulation::from_frontend(design())
                    .build_cranelift()
                    .unwrap(),
                scheduled,
            );
            check(
                Simulation::from_frontend(design()).build_wasm().unwrap(),
                scheduled,
            );
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(
                Simulation::from_frontend(design()).build_native().unwrap(),
                scheduled,
            );
        }
    }
    #[test]
    fn earlier_scheduled_drives_conflict_with_delayed_clock_activation() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.schedule("clk", 0, 1).unwrap();
            assert_eq!(sim.step().unwrap(), Some(0));
            let error = sim.run_until(2).unwrap_err().to_string();
            assert!(error.contains("already driven by schedule"), "{error}");
            assert_eq!(sim.ticks(), 0);
        }
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
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.schedule("clk", 0, 1).unwrap();
        assert_eq!(sim.step().unwrap(), Some(0));
        let checkpoint = sim.checkpoint().unwrap();
        let error = sim.run_until(2).unwrap_err().to_string();
        assert!(error.contains("already driven by schedule"), "{error}");
        sim.restore(&checkpoint).unwrap();
        // Model a host waking the suspended process at the restored timestamp.
        let mut parts = sim.state.export_schedule(&sim.simulator.backend);
        parts.process_wakeups = vec![(0, 0)];
        sim.state.import_schedule(parts);
        let error = sim.step().unwrap_err().to_string();
        assert!(error.contains("already driven by schedule"), "{error}");
        assert_eq!(sim.ticks(), 0);
    }
    fn check_transfer<B: SimBackend>(mut from: Simulation<B>, mut to: Simulation<B>) {
        from.schedule("clk", 0, 1).unwrap();
        from.step().unwrap();
        let design = from.simulator.checkpoint().unwrap();
        let mut parts = from.state.export_schedule(&from.simulator.backend);
        parts.process_wakeups = vec![(0, 0)];
        to.simulator.restore(&design).unwrap();
        to.state.import_schedule(parts);
        let error = to.step().unwrap_err().to_string();
        assert!(error.contains("already driven by schedule"), "{error}");
        assert_eq!(to.ticks(), 0);
    }
    #[test]
    fn exported_schedule_retains_consumed_clock_drives() {
        let build = || {
            Simulation::from_frontend(design())
                .build_interpreter()
                .unwrap()
        };
        check_transfer(build(), build());
        let build = || {
            Simulation::from_frontend(design())
                .build_cranelift()
                .unwrap()
        };
        check_transfer(build(), build());
        let build = || Simulation::from_frontend(design()).build_wasm().unwrap();
        check_transfer(build(), build());
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        {
            let build = || Simulation::from_frontend(design()).build_native().unwrap();
            check_transfer(build(), build());
        }
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
            assert!(sim.drain_runtime_events().iter().any(
                |e| matches!(e,crate::RuntimeEvent::Display { message } if message.as_slice()==b"x")
            ));
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

#[cfg(test)]
mod pending_generated_clock_tests {
    use super::*;
    use crate::frontend_sdk::{Constant, Edge, ModuleBuilder, Statement, UnaryOp, ValueType};
    fn design() -> crate::frontend_sdk::FrontendArtifact {
        let mut m = ModuleBuilder::new("Top").unwrap();
        let bit = ValueType::bits(1).unwrap();
        let a = m.internal("a", bit).unwrap();
        let b = m.internal("b", bit).unwrap();
        let qa = m.output("qa", bit).unwrap();
        let qb = m.output("qb", bit).unwrap();
        let old = m.read(qb).unwrap();
        let next = m.unary(UnaryOp::BitNot, old, bit).unwrap();
        let qa_target = m.whole(qa).unwrap();
        let qb_target = m.whole(qb).unwrap();
        m.register(qa_target, old, a, Edge::Posedge, None, None)
            .unwrap();
        m.register(qb_target, next, b, Edge::Posedge, None, None)
            .unwrap();
        m.clock_period(a, 4).unwrap();
        m.clock_period(b, 4).unwrap();
        let two = m.constant(Constant::two_state(2u8, 64).unwrap());
        let one = m.constant(Constant::two_state(1u8, 64).unwrap());
        let four = m.constant(Constant::two_state(4u8, 64).unwrap());
        m.process(vec![Statement::ClockCycles {
            clock: b,
            count: two,
        }])
        .unwrap();
        m.process(vec![
            Statement::Delay { amount: four },
            Statement::ClockCycles {
                clock: a,
                count: one,
            },
        ])
        .unwrap();
        m.finish()
    }
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(3).unwrap();
        assert_eq!(sim.get(sim.signal("qb")), 1u8.into());
        // A failed round can leave this generated drive high without publishing its FF batch.
        sim.simulator.backend.set(sim.signal("b"), 1u8);
        sim.step().unwrap();
        assert_eq!(sim.get(sim.signal("qa")), 1u8.into());
        assert_eq!(sim.get(sim.signal("qb")), 0u8.into());
        // B rose once at time zero; both A and B rise at time four.
        assert_eq!(sim.ticks(), 3);
    }
    #[test]
    fn pending_rises_share_pre_edge_state_with_new_clocks() {
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
mod current_time_settle_tests {
    use super::*;
    #[test]
    fn settle_consumes_current_time_clock_events() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            sim.schedule("clk", 0, 1).unwrap();
            sim.settle().unwrap();
            assert_eq!(sim.get(sim.signal("q")), 1u8.into());
            assert_eq!(sim.time(), 0);
            assert_eq!(sim.next_event_time(), None);
            sim.settle().unwrap();
            assert_eq!(sim.get(sim.signal("q")), 1u8.into());
        }
        let source =
            "module Top(input bit clk,output bit[7:0]q);always @(posedge clk)q<=q+1;endmodule";
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
mod drained_finish_tests {
    use super::*;

    fn check<B: SimBackend>(mut sim: Simulation<B>, mode: u8) {
        sim.schedule("clk", 10, 1).unwrap();
        sim.run_until(0).unwrap();
        let before = sim.checkpoint().unwrap();
        let mut drain = (mode == 1)
            .then(|| sim.simulator.runtime_event_drain())
            .flatten();
        sim.simulator.set(sim.signal("go"), 1u8);
        let events = if let Some(drain) = &mut drain {
            drain.drain()
        } else if mode == 2 {
            sim.simulator.eval_comb().unwrap();
            Vec::new()
        } else {
            sim.drain_runtime_events()
        };
        drop(drain);
        if mode != 2 {
            assert!(events.contains(&crate::RuntimeEvent::Finish));
        }
        sim.run_until(20).unwrap();
        assert!(sim.is_finished());
        assert_eq!(sim.time(), 0);
        assert_eq!(sim.get(sim.signal("q")), 0u8.into());
        let finished = sim.checkpoint().unwrap();
        sim.restore(&before).unwrap();
        sim.run_until(20).unwrap();
        assert!(!sim.is_finished());
        assert_eq!(sim.time(), 20);
        assert_eq!(sim.get(sim.signal("q")), 1u8.into());
        sim.restore(&finished).unwrap();
        sim.run_until(20).unwrap();
        assert!(sim.is_finished());
        assert_eq!(sim.time(), 0);
    }

    #[test]
    fn consuming_finish_events_does_not_clear_the_request() {
        let source = "module Top(input bit clk,go,output bit q);always_comb if(go)$finish;always @(posedge clk)q<=1;endmodule";
        let builder = || {
            Simulator::from_sv_sources(
                vec![(source, std::path::Path::new("finish-drain.sv"))],
                "Top",
            )
        };
        for mode in 0..3 {
            check(
                Simulation::new(builder().build_interpreter().unwrap()),
                mode,
            );
            check(Simulation::new(builder().build_cranelift().unwrap()), mode);
            check(Simulation::new(builder().build_wasm().unwrap()), mode);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(Simulation::new(builder().build_native().unwrap()), mode);
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod scheduled_pulse_tests {
    use super::*;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(0).unwrap();
        for name in ["any_woke", "neg_woke", "pos_woke", "ffneg"] {
            assert_eq!(sim.get(sim.signal(name)), 0u8.into());
        }
        sim.schedule("clk", 1, 0).unwrap();
        sim.schedule("clk", 1, 1).unwrap();
        sim.run_until(1).unwrap();
        for name in ["any_woke", "neg_woke", "pos_woke", "ffneg", "sampled"] {
            assert_eq!(sim.get(sim.signal(name)), 1u8.into(), "{name}");
        }
        assert_eq!(sim.get(sim.signal("level_woke")), 0u8.into());
    }
    #[test]
    fn scheduled_pulses_wake_waiters_after_the_batch() {
        let source = "module Top(output bit any_woke,neg_woke,pos_woke,ffneg,sampled,level_woke);bit clk=1;initial begin wait(clk==0);level_woke=1;end always @(negedge clk)ffneg<=1;initial begin @(clk);any_woke=1;sampled=clk;end initial begin @(negedge clk);neg_woke=1;end initial begin @(posedge clk);pos_woke=1;end endmodule";
        let compound = source
            .replace("@(clk)", "@({clk,1'b0})")
            .replace("@(negedge clk);", "@(negedge (clk | 1'b0));")
            .replace("@(posedge clk);", "@(posedge (clk | 1'b0));");
        for source in [source, compound.as_str()] {
            let build = || {
                Simulator::from_sv_sources(
                    vec![(source, std::path::Path::new("scheduled-pulse.sv"))],
                    "Top",
                )
            };
            check(Simulation::new(build().build_interpreter().unwrap()));
            check(Simulation::new(build().build_cranelift().unwrap()));
            check(Simulation::new(build().build_wasm().unwrap()));
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            {
                check(Simulation::new(build().build_native().unwrap()));
                let image = build().compile_native().unwrap().into_program_image();
                let image = crate::NativeProgramImage::from_container_bytes(
                    &image.to_container_bytes().unwrap(),
                )
                .unwrap();
                check(Simulation::new(
                    Simulator::from_sources(Vec::new(), "Top")
                        .build_native_from_image(image)
                        .unwrap(),
                ));
            }
        }
    }
}

#[cfg(test)]
mod host_clock_review_tests {
    use super::*;
    use crate::frontend_sdk::{BinaryOp, Constant, Edge, ModuleBuilder, Statement, ValueType};
    fn design() -> crate::frontend_sdk::FrontendArtifact {
        let mut module = ModuleBuilder::new("Top").unwrap();
        let bit = ValueType::bits(1).unwrap();
        let word = ValueType::bits(8).unwrap();
        let clk = module.internal("clk", bit).unwrap();
        let q = module.output("q", word).unwrap();
        module
            .set_initial(q, Constant::two_state(0u8, 8).unwrap())
            .unwrap();
        let value = module.read(q).unwrap();
        let one = module.constant(Constant::two_state(1u8, 8).unwrap());
        let next = module.binary(BinaryOp::Add, value, one, word).unwrap();
        let target = module.whole(q).unwrap();
        module
            .register(target, next, clk, Edge::Posedge, None, None)
            .unwrap();
        module.clock_period(clk, 2).unwrap();
        let ten = module.constant(Constant::two_state(10u8, 64).unwrap());
        module
            .process(vec![Statement::ClockCycles {
                clock: clk,
                count: ten,
            }])
            .unwrap();
        module.finish()
    }
    fn settle<B: SimBackend>(mut sim: Simulation<B>) {
        sim.settle().unwrap();
        assert_eq!(sim.time(), 0);
        assert_eq!(sim.ticks(), 1);
        assert_eq!(sim.get(sim.signal("q")), 1u8.into());
        assert_eq!(sim.next_event_time(), Some(1));
    }
    fn host<B: SimBackend>(mut sim: Simulation<B>, mode: u8) {
        let clk = sim.signal("clk");
        let before = sim.checkpoint().unwrap();
        match mode {
            0 => sim.set_wide(clk, 1u8.into()),
            1 => sim.set_four_state(clk, 1u8.into(), 0u8.into()),
            2 => sim
                .modify(|io| {
                    io.set(clk, 1u8);
                    io.set(clk, 0u8);
                })
                .unwrap(),
            _ => {
                sim.set_wide(clk, 1u8.into());
                sim.get(sim.signal("q"));
            }
        }
        let error = sim.run_until(0).unwrap_err().to_string();
        assert!(error.contains("direct host write"), "{error}");
        assert_eq!(sim.get(sim.signal("q")), 0u8.into());
        assert_eq!(sim.ticks(), 0);
        assert!(
            sim.run_until(0)
                .unwrap_err()
                .to_string()
                .contains("direct host write")
        );
        sim.restore(&before).unwrap();
        sim.run_until(0).unwrap();
        assert_eq!(sim.ticks(), 1);
    }
    #[test]
    fn settling_a_process_clock_keeps_current_time() {
        let build = || Simulator::from_frontend(design());
        settle(Simulation::new(build().build_interpreter().unwrap()));
        settle(Simulation::new(build().build_cranelift().unwrap()));
        settle(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        settle(Simulation::new(build().build_native().unwrap()));
    }
    #[test]
    fn checkpoint_rejects_escaped_generated_clock_writes() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            let clk = sim.signal("clk");
            let (ptr, len) = sim.memory_as_mut_ptr();
            assert!(clk.offset < len);
            sim.run_until(1).unwrap();
            sim.checkpoint().unwrap();
            unsafe {
                ptr.add(clk.offset).write(1);
            }
            assert!(sim.checkpoint().is_err());
            unsafe {
                ptr.add(clk.offset).write(0);
            }
            let checkpoint = sim.checkpoint().unwrap();
            sim.restore(&checkpoint).unwrap();
            sim.run_until(2).unwrap();
            assert_eq!(sim.ticks(), 2);
        }
        let build = || Simulator::from_frontend(design());
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
    #[test]
    fn retained_memory_cannot_inject_generated_clock_edges() {
        fn check<B: SimBackend>(mut sim: Simulation<B>, explicit: bool) {
            let clk = sim.signal("clk");
            let (ptr, len) = sim.memory_as_mut_ptr();
            assert!(clk.offset < len);
            sim.run_until(1).unwrap();
            assert_eq!(sim.ticks(), 1);
            unsafe {
                ptr.add(clk.offset).write(1);
            }
            let result = if explicit {
                sim.eval_comb()
            } else {
                sim.run_until(2)
            };
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("direct host write")
            );
            assert_eq!(sim.ticks(), 1);
            assert_eq!(sim.time(), 1);
            assert_eq!(sim.simulator.backend.get(sim.signal("q")), 1u8.into());
        }
        for explicit in [false, true] {
            let build = || Simulator::from_frontend(design());
            check(
                Simulation::new(build().build_interpreter().unwrap()),
                explicit,
            );
            check(
                Simulation::new(build().build_cranelift().unwrap()),
                explicit,
            );
            check(Simulation::new(build().build_wasm().unwrap()), explicit);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(Simulation::new(build().build_native().unwrap()), explicit);
        }
    }
    #[test]
    fn direct_host_clock_writes_are_rejected_before_activation() {
        let build = || Simulator::from_frontend(design());
        for mode in 0..4 {
            host(Simulation::new(build().build_interpreter().unwrap()), mode);
            host(Simulation::new(build().build_cranelift().unwrap()), mode);
            host(Simulation::new(build().build_wasm().unwrap()), mode);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            host(Simulation::new(build().build_native().unwrap()), mode);
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod failed_ff_progress_tests {
    use super::*;
    fn check<B: SimBackend>(mut sim: Simulator<B>) {
        let event = sim.event("clk");
        let (completed, result) = SimulationExecutor::tick_many(&mut sim, event, 10);
        assert_eq!(completed, 0, "{}", std::any::type_name::<B>());
        assert!(result.is_err());
        assert_eq!(
            sim.get(sim.signal("q")),
            0u8.into(),
            "{}",
            std::any::type_name::<B>()
        );
    }
    #[test]
    fn failed_ff_evaluation_does_not_complete_a_tick() {
        let source = "module Top(input bit clk,ok,output bit q);always_ff @(posedge clk)begin assert(ok)else $fatal(1,\"failed FF\");q<=1;end endmodule";
        let build = || {
            Simulator::from_sv_sources(vec![(source, std::path::Path::new("failed-ff.sv"))], "Top")
        };
        check(build().build_interpreter().unwrap());
        check(build().build_cranelift().unwrap());
        check(build().build_wasm().unwrap());
        check(build().build_tiered().unwrap());
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        {
            check(build().build_native().unwrap());
            check(
                build()
                    .threads(4)
                    .parallel_partition(crate::ParallelPartition::Always)
                    .build_native()
                    .unwrap(),
            );
        }
    }
}

#[cfg(test)]
mod generated_phase_review_tests {
    use super::*;
    use crate::frontend_sdk::{Constant, Edge, ModuleBuilder, Statement, ValueType};
    fn design(unknown: Option<u8>, edge: Edge) -> crate::frontend_sdk::FrontendArtifact {
        let mut m = ModuleBuilder::new("ClockPhases").unwrap();
        let bit = ValueType::bits(1).unwrap();
        let clk = m.internal("clk", ValueType::logic(1).unwrap()).unwrap();
        m.set_initial(
            clk,
            match unknown {
                Some(value) => Constant::four_state(value, 1u8, 1).unwrap(),
                None => Constant::two_state(0u8, 1).unwrap(),
            },
        )
        .unwrap();
        let q = m.output("q", bit).unwrap();
        let one = m.constant(Constant::two_state(1u8, 1).unwrap());
        let target = m.whole(q).unwrap();
        m.register(target, one, clk, edge, None, None).unwrap();
        m.clock_period(clk, 4).unwrap();
        let three = m.constant(Constant::two_state(3u8, 64).unwrap());
        m.process(vec![Statement::ClockCycles {
            clock: clk,
            count: three,
        }])
        .unwrap();
        m.finish()
    }
    fn fall<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 0u8.into());
        assert_eq!(sim.state.settle_at(&mut sim.simulator, 2).unwrap(), Some(2));
        assert_eq!(sim.time(), 2);
        assert_eq!(sim.get(sim.signal("clk")), 0u8.into());
        assert_eq!(sim.get(sim.signal("q")), 1u8.into());
        assert_eq!(sim.next_event_time(), Some(4));
    }
    fn unknown<B: SimBackend>(mut sim: Simulation<B>, payload: u8, edge: Edge) {
        assert_eq!(
            sim.get_four_state(sim.signal("clk")),
            (payload.into(), 1u8.into())
        );
        sim.run_until(0).unwrap();
        assert_eq!(
            sim.get(sim.signal("q")),
            u8::from(edge == Edge::Posedge).into()
        );
        assert_eq!(
            sim.get_four_state(sim.signal("clk")),
            (1u8.into(), 0u8.into())
        );
        assert_eq!(sim.ticks(), 1);
        sim.run_until(2).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 1u8.into());
    }
    #[test]
    fn settle_at_drains_a_generated_fall_at_the_target() {
        let build = || Simulator::from_frontend(design(None, Edge::Negedge));
        fall(Simulation::new(build().build_interpreter().unwrap()));
        fall(Simulation::new(build().build_cranelift().unwrap()));
        fall(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        fall(Simulation::new(build().build_native().unwrap()));
    }
    #[test]
    fn unknown_initial_clock_payloads_have_the_same_edge_sequence() {
        for (payload, edge) in [
            (0, Edge::Posedge),
            (1, Edge::Posedge),
            (0, Edge::Negedge),
            (1, Edge::Negedge),
        ] {
            let build = || Simulator::from_frontend(design(Some(payload), edge)).four_state(true);
            unknown(
                Simulation::new(build().build_interpreter().unwrap()),
                payload,
                edge,
            );
            unknown(
                Simulation::new(build().build_cranelift().unwrap()),
                payload,
                edge,
            );
            unknown(
                Simulation::new(build().build_wasm().unwrap()),
                payload,
                edge,
            );
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            unknown(
                Simulation::new(build().build_native().unwrap()),
                payload,
                edge,
            );
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod lazy_host_clock_review_tests {
    use super::*;
    fn check<B: SimBackend>(mut sim: Simulation<B>, mask: u8) {
        let clk = sim.signal("clk");
        let q = sim.signal("q");
        sim.set_four_state(clk, 1u8.into(), 0u8.into());
        sim.settle().unwrap();
        assert_eq!(sim.get(q), 1u8.into());
        sim.set_four_state(clk, 0u8.into(), mask.into());
        let _ = sim.get(q);
        sim.settle().unwrap();
        sim.set_four_state(clk, 1u8.into(), 0u8.into());
        let _ = sim.get(q);
        sim.settle().unwrap();
        assert_eq!(sim.get(q), 2u8.into(), "inactive mask {mask}");
    }
    #[test]
    fn lazy_reads_do_not_hide_inactive_clock_transitions() {
        let source =
            "module Top(input logic clk,output int q);always @(posedge clk)q<=q+1;endmodule";
        let build = || {
            Simulator::from_sv_sources(vec![(source, std::path::Path::new("lazy-clock.sv"))], "Top")
                .four_state(true)
        };
        for mask in [0, 1] {
            check(Simulation::new(build().build_interpreter().unwrap()), mask);
            check(Simulation::new(build().build_cranelift().unwrap()), mask);
            check(Simulation::new(build().build_wasm().unwrap()), mask);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(Simulation::new(build().build_native().unwrap()), mask);
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod legacy_clock_state_review_tests {
    use super::*;
    fn check<B: SimBackend>(mut source: Simulation<B>, mut target: Simulation<B>, known: u8) {
        let clk = source.signal("clk");
        source.set_four_state(clk, 1u8.into(), 0u8.into());
        source.settle().unwrap();
        source.set_four_state(clk, known.into(), 1u8.into());
        source.settle().unwrap();
        let file = source.save_state().unwrap();
        let unknown_bytes = 4 + file
            .schedule
            .as_ref()
            .unwrap()
            .unknown_events
            .iter()
            .flatten()
            .map(|name| 4 + name.len())
            .sum::<usize>();
        let mut bytes = Vec::new();
        file.write_to(&mut bytes).unwrap();
        bytes[8..10].copy_from_slice(&1u16.to_le_bytes());
        bytes.truncate(bytes.len() - unknown_bytes);
        let legacy = celox_runtime::StateFile::read_from(bytes.as_slice()).unwrap();
        target.load_state(&legacy).unwrap();
        let q = target.signal("q");
        let before = target.get(q);
        let clk = target.signal("clk");
        target.set_four_state(clk, known.into(), 0u8.into());
        target.settle().unwrap();
        assert_eq!(target.get(q), before);
    }
    #[test]
    fn legacy_files_keep_known_boolean_clock_baselines() {
        for (edge, known) in [("posedge", 1u8), ("negedge", 0u8)] {
            let source = format!(
                "module Top(input logic clk,output int q);always @({edge} clk)q<=q+1;endmodule"
            );
            let build = || {
                Simulator::from_sv_sources(
                    vec![(source.as_str(), std::path::Path::new("legacy-clock.sv"))],
                    "Top",
                )
                .four_state(true)
            };
            check(
                Simulation::new(build().build_interpreter().unwrap()),
                Simulation::new(build().build_interpreter().unwrap()),
                known,
            );
            check(
                Simulation::new(build().build_cranelift().unwrap()),
                Simulation::new(build().build_cranelift().unwrap()),
                known,
            );
            check(
                Simulation::new(build().build_wasm().unwrap()),
                Simulation::new(build().build_wasm().unwrap()),
                known,
            );
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(
                Simulation::new(build().build_native().unwrap()),
                Simulation::new(build().build_native().unwrap()),
                known,
            );
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod staged_edge_review_tests {
    use super::*;
    fn check<B: SimBackend>(mut sim: Simulation<B>, mut restored: Simulation<B>) {
        let a = sim.signal("a");
        let qa = sim.signal("qa");
        let qb = sim.signal("qb");
        sim.set_wide(a, 1u8.into());
        let mut file = sim.save_state().unwrap();
        file.schedule.as_mut().unwrap().unknown_events = None;
        let mut bytes = Vec::new();
        file.write_to(&mut bytes).unwrap();
        let file = celox_runtime::StateFile::read_from(bytes.as_slice()).unwrap();
        restored.load_state(&file).unwrap();
        restored.settle().unwrap();
        assert_eq!(restored.get(qa), 1u8.into());
        sim.schedule("b", 0, 1).unwrap();
        sim.settle().unwrap();
        assert_eq!(sim.get(qa), 1u8.into());
        assert_eq!(sim.get(qb), 1u8.into());
    }
    #[test]
    fn host_edges_join_current_time_events_and_survive_legacy_state_files() {
        let source = "module Top(input bit a,b,output bit[7:0]qa,qb);always @(posedge a)qa<=qb+1;always @(posedge b)qb<=qa+1;endmodule";
        let build =
            || Simulator::from_sv_sources(vec![(source, std::path::Path::new("staged.sv"))], "Top");
        check(
            Simulation::new(build().build_interpreter().unwrap()),
            Simulation::new(build().build_interpreter().unwrap()),
        );
        check(
            Simulation::new(build().build_cranelift().unwrap()),
            Simulation::new(build().build_cranelift().unwrap()),
        );
        check(
            Simulation::new(build().build_wasm().unwrap()),
            Simulation::new(build().build_wasm().unwrap()),
        );
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(
            Simulation::new(build().build_native().unwrap()),
            Simulation::new(build().build_native().unwrap()),
        );
    }
}

#[cfg(test)]
mod normalizing_fall_review_tests {
    use super::*;
    use crate::frontend_sdk::{
        BinaryOp, Constant, Edge, ModuleBuilder, Statement, UnaryOp, ValueType,
    };
    fn design() -> crate::frontend_sdk::FrontendArtifact {
        let bit = ValueType::bits(1).unwrap();
        let mut m = ModuleBuilder::new("Top").unwrap();
        let clk = m.internal("clk", bit).unwrap();
        m.set_initial(clk, Constant::two_state(1u8, 1).unwrap())
            .unwrap();
        let enable = m.input("enable", bit).unwrap();
        let q = m.output("q", bit).unwrap();
        let v = m.output("v", bit).unwrap();
        m.set_initial(q, Constant::two_state(1u8, 1).unwrap())
            .unwrap();
        let clock_value = m.read(clk).unwrap();
        let zero = m.constant(Constant::two_state(0u8, 1).unwrap());
        m.register(
            m.whole(q).unwrap(),
            clock_value,
            clk,
            Edge::Negedge,
            None,
            None,
        )
        .unwrap();
        m.clock_period(clk, 4).unwrap();

        let low = m.unary(UnaryOp::BitNot, clock_value, bit).unwrap();
        let enabled = m.read(enable).unwrap();
        let active = m.binary(BinaryOp::And, low, enabled, bit).unwrap();
        let previous = m.read(v).unwrap();
        let toggled = m.unary(UnaryOp::BitNot, previous, bit).unwrap();
        let next = m.mux(active, toggled, zero).unwrap();
        m.assign(m.whole(v).unwrap(), next).unwrap();
        let count = m.constant(Constant::two_state(1u8, 64).unwrap());
        m.process(vec![Statement::ClockCycles { clock: clk, count }])
            .unwrap();
        m.finish()
    }
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.memory_as_mut_ptr();
        assert_eq!(sim.get(sim.signal("clk")), 1u8.into());
        sim.set_wide(sim.signal("enable"), 1u8.into());
        assert!(sim.run_until(0).is_err());
        assert_eq!(sim.ticks(), 0);
        sim.checkpoint().unwrap();
        let parts = sim.state.export_schedule(&sim.simulator.backend);
        assert_eq!(parts.process_clocks[0].falling_edge, Some(0));
        assert_eq!(sim.get(sim.signal("q")), 1u8.into());
        sim.set_wide(sim.signal("enable"), 0u8.into());
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 0u8.into());
        assert_eq!(sim.ticks(), 1);
    }
    #[test]
    fn failed_initial_falls_publish_before_the_first_rise_on_retry() {
        let build = || {
            Simulator::from_frontend(design()).true_loop(
                (Vec::new(), vec!["v".into()]),
                (Vec::new(), vec!["v".into()]),
                8,
            )
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod finished_host_write_review_tests {
    use super::*;
    fn check<B: SimBackend>(mut sim: Simulation<B>, operation: &str) {
        sim.run_until(0).unwrap();
        assert!(sim.is_finished());
        let a = sim.signal("a");
        let q = sim.signal("q");
        sim.simulator.set_wide(a, 1u8.into());
        match operation {
            "step" => assert_eq!(sim.step().unwrap(), None),
            "step_until" => assert_eq!(sim.step_until(1).unwrap(), None),
            _ => sim.run_until(1).unwrap(),
        };
        assert_eq!(sim.simulator.backend.get(q), 0u8.into());
        sim.eval_comb().unwrap();
        assert_eq!(sim.simulator.backend.get(q), 1u8.into());
    }
    #[test]
    fn finished_processes_do_not_settle_later_host_writes() {
        let source = "module Top(input bit a,output bit q);assign q=a;initial $finish;endmodule";
        let build = || {
            Simulator::from_sv_sources(vec![(source, std::path::Path::new("finished.sv"))], "Top")
        };
        for operation in ["step", "step_until", "run_until"] {
            check(
                Simulation::new(build().build_interpreter().unwrap()),
                operation,
            );
            check(
                Simulation::new(build().build_cranelift().unwrap()),
                operation,
            );
            check(Simulation::new(build().build_wasm().unwrap()), operation);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(Simulation::new(build().build_native().unwrap()), operation);
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod public_host_edge_review_tests {
    use super::*;
    fn check<B: SimBackend>(mut sim: Simulation<B>, operation: &str) {
        let clk = sim.signal("clk");
        let q = sim.signal("q");
        sim.set_wide(clk, 1u8.into());
        match operation {
            "step" => {
                sim.step().unwrap();
            }
            "step_until" => {
                sim.step_until(0).unwrap();
            }
            _ => sim.run_until(0).unwrap(),
        };
        assert_eq!(sim.get(q), 1u8.into());
        assert_eq!(sim.time(), 0);
        sim.run_until(10).unwrap();
        assert!(sim.schedule("clk", 5, 0).is_err());
        sim.settle().unwrap();
        assert_eq!(sim.time(), 10);
    }
    #[test]
    fn public_runs_settle_host_edges_and_reject_past_schedules() {
        let source = "module Top(input bit clk,output bit q);always @(posedge clk)q<=1;endmodule";
        for operation in ["step", "step_until", "run_until"] {
            let build = || {
                Simulator::from_sv_sources(
                    vec![(source, std::path::Path::new("host-edge.sv"))],
                    "Top",
                )
            };
            check(
                Simulation::new(build().build_interpreter().unwrap()),
                operation,
            );
            check(
                Simulation::new(build().build_cranelift().unwrap()),
                operation,
            );
            check(Simulation::new(build().build_wasm().unwrap()), operation);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(Simulation::new(build().build_native().unwrap()), operation);
        }
    }
    #[test]
    fn legacy_pending_known_to_unknown_edges_keep_their_known_baseline() {
        let source = "module Top(input logic clk,output bit q);always @(posedge clk)q<=1;endmodule";
        let build = || {
            Simulator::from_sv_sources(
                vec![(source, std::path::Path::new("legacy-pending.sv"))],
                "Top",
            )
            .four_state(true)
        };
        fn check<B: SimBackend>(mut source: Simulation<B>, mut target: Simulation<B>) {
            source.set_four_state(source.signal("clk"), 0u8.into(), 1u8.into());
            let mut file = source.save_state().unwrap();
            file.schedule.as_mut().unwrap().unknown_events = None;
            let mut bytes = Vec::new();
            file.write_to(&mut bytes).unwrap();
            let file = celox_runtime::StateFile::read_from(bytes.as_slice()).unwrap();
            target.load_state(&file).unwrap();
            target.settle().unwrap();
            assert_eq!(target.get(target.signal("q")), 1u8.into());
        }
        check(
            Simulation::new(build().build_interpreter().unwrap()),
            Simulation::new(build().build_interpreter().unwrap()),
        );
        check(
            Simulation::new(build().build_cranelift().unwrap()),
            Simulation::new(build().build_cranelift().unwrap()),
        );
        check(
            Simulation::new(build().build_wasm().unwrap()),
            Simulation::new(build().build_wasm().unwrap()),
        );
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(
            Simulation::new(build().build_native().unwrap()),
            Simulation::new(build().build_native().unwrap()),
        );
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod scheduled_derived_concat_review_tests {
    use super::*;
    use std::path::Path;
    #[test]
    fn queued_source_pulses_wake_derived_signal_waiters() {
        let source = "module Top(input bit a,output bit q,seen);bit unused;assign q=a;always @(posedge a)unused<=1;initial begin @(negedge q);seen=1;end endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            let a = sim.signal("a");
            sim.modify(|io| io.set(a, 1u8)).unwrap();
            sim.run_until(0).unwrap();
            sim.schedule("a", 1, 1).unwrap();
            sim.schedule("a", 1, 0).unwrap();
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("q")), 1u8.into());
            assert_eq!(sim.get(sim.signal("seen")), 1u8.into());
        }
        let build =
            || Simulator::from_sv_sources(vec![(source, Path::new("derived-pulse.sv"))], "Top");
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build().unwrap()));
    }
    #[test]
    fn concatenated_nbas_do_not_invent_expression_pulses() {
        fn check<B: SimBackend>(mut sim: Simulation<B>, value: u8, seen: u8) {
            sim.run_until(0).unwrap();
            sim.schedule("clk", 1, 1).unwrap();
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), seen.into(), "{value}");
            assert_eq!(sim.get(sim.signal("q")), value.into());
        }
        for (value, seen) in [(3u8, 0u8), (2, 1)] {
            let source = format!(
                "module Top(input bit clk,output bit seen);bit[1:0]q;always @(posedge clk){{q[1],q[0]}}<=2'd{value};initial begin @(q[1]^q[0]);seen=1;end endmodule"
            );
            let build = || {
                Simulator::from_sv_sources(
                    vec![(source.as_str(), Path::new("concat-nba.sv"))],
                    "Top",
                )
            };
            check(
                Simulation::new(build().build_interpreter().unwrap()),
                value,
                seen,
            );
            check(
                Simulation::new(build().build_cranelift().unwrap()),
                value,
                seen,
            );
            check(Simulation::new(build().build_wasm().unwrap()), value, seen);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(Simulation::new(build().build().unwrap()), value, seen);
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod past_by_id_review_tests {
    use super::*;
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(10).unwrap();
        let clk = sim.signal("clk");
        let id = sim
            .simulator
            .backend
            .id_to_addr_slice()
            .iter()
            .position(|addr| sim.simulator.backend.resolve_signal(addr) == clk)
            .unwrap() as u32;
        assert!(sim.schedule_by_id(id, 5, 1).is_err());
        assert!(sim.try_add_clock_by_id(id, 4, 5).is_err());
        sim.add_clock("clk", 4, 5);
        sim.add_clock_by_id(id, 4, 5);
        sim.settle().unwrap();
        assert_eq!(sim.time(), 10);
        assert_eq!(sim.next_event_time(), None);
        sim.schedule_by_id(id, 10, 1).unwrap();
        sim.settle().unwrap();
        assert_eq!(sim.time(), 10);
        assert_eq!(sim.get(sim.signal("q")), 1u8.into());
    }
    #[test]
    fn by_id_schedules_and_clock_registration_cannot_rewind_time() {
        let source = "module Top(input bit clk,output bit q);always @(posedge clk)q<=1;endmodule";
        let build = || {
            Simulator::from_sv_sources(vec![(source, std::path::Path::new("past-id.sv"))], "Top")
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod finish_during_round_review_tests {
    use super::*;
    use std::path::Path;
    fn queued_finish<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(0).unwrap();
        sim.schedule("go", 1, 0).unwrap();
        sim.schedule("later", 1, 1).unwrap();
        sim.run_until(1).unwrap();
        assert!(sim.is_finished());
        assert_eq!(sim.simulator.backend.get_as::<u8>(sim.signal("later")), 0);
    }
    fn nba_finish<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(0).unwrap();
        sim.schedule("clk", 1, 1).unwrap();
        sim.run_until(1).unwrap();
        assert!(sim.is_finished());
        assert_eq!(sim.simulator.backend.get_as::<u8>(sim.signal("a")), 1);
        assert_eq!(sim.simulator.backend.get_as::<u8>(sim.signal("y")), 0);
    }
    #[test]
    fn scheduled_finishes_stop_remaining_work() {
        for waiter in ["initial wait(0);", ""] {
            let source = format!(
                "module Top(input bit go,later);bit q,r;initial go=1;always @(negedge go)q<=1;always @(posedge later)r<=1;always_comb if(!go)$finish;{waiter} endmodule"
            );
            let build = || {
                Simulator::from_sv_sources(
                    vec![(source.as_str(), Path::new("queued-finish.sv"))],
                    "Top",
                )
            };
            queued_finish(Simulation::new(build().build_interpreter().unwrap()));
            queued_finish(Simulation::new(build().build_cranelift().unwrap()));
            queued_finish(Simulation::new(build().build_wasm().unwrap()));
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            queued_finish(Simulation::new(build().build_native().unwrap()));
        }
    }
    #[test]
    fn nba_woken_finishes_stop_remaining_work() {
        let source = "module Top(input bit clk,output bit a,y);bit q;assign y=a;always @(posedge clk)q<=1;initial begin @(posedge q);a=1;$finish;end endmodule";
        let build =
            || Simulator::from_sv_sources(vec![(source, Path::new("nba-finish.sv"))], "Top");
        nba_finish(Simulation::new(build().build_interpreter().unwrap()));
        nba_finish(Simulation::new(build().build_cranelift().unwrap()));
        nba_finish(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        nba_finish(Simulation::new(build().build_native().unwrap()));
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod inactive_host_transition_review_tests {
    use super::*;
    fn check<B: SimBackend>(mut sim: Simulation<B>, method: &str, future: bool) {
        let clk = sim.signal("clk");
        sim.set_wide(clk, 1u8.into());
        sim.run_until(0).unwrap();
        sim.set_wide(clk, 0u8.into());
        if future {
            sim.schedule("clk", 5, 1).unwrap();
        }
        if method == "step" {
            assert_eq!(sim.step().unwrap(), future.then_some(5));
        } else {
            assert_eq!(sim.step_until(4).unwrap(), None);
            if future {
                assert_eq!(sim.step_until(5).unwrap(), Some(5));
            }
        }
    }
    #[test]
    fn inactive_host_edges_do_not_fabricate_steps() {
        let source = "module Top(input bit clk,output bit q);always @(posedge clk)q<=1;endmodule";
        for method in ["step", "step_until"] {
            for future in [false, true] {
                let build = || {
                    Simulator::from_sv_sources(
                        vec![(source, std::path::Path::new("inactive-host.sv"))],
                        "Top",
                    )
                };
                check(
                    Simulation::new(build().build_interpreter().unwrap()),
                    method,
                    future,
                );
                check(
                    Simulation::new(build().build_cranelift().unwrap()),
                    method,
                    future,
                );
                check(
                    Simulation::new(build().build_wasm().unwrap()),
                    method,
                    future,
                );
                #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
                check(
                    Simulation::new(build().build_native().unwrap()),
                    method,
                    future,
                );
            }
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod queued_derived_ff_review_tests {
    use super::*;
    #[test]
    fn single_scheduled_update_preserves_derived_edges_with_waiters() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            let enable = sim.signal("enable");
            sim.modify(|io| io.set(enable, 1u8)).unwrap();
            sim.run_until(0).unwrap();
            sim.schedule("src", 1, 1).unwrap();
            sim.schedule("src", 2, 0).unwrap();
            sim.schedule("src", 3, 1).unwrap();
            for (time, expected) in [(1, 1u8), (2, 1), (3, 2)] {
                sim.run_until(time).unwrap();
                assert_eq!(sim.get(sim.signal("count")), expected.into(), "time {time}");
            }
        }
        for waiter in ["", "initial wait(stop);"] {
            let source = format!(
                "module Top(input bit src,enable,stop,output bit[7:0]count);bit unused;wire gated=src&enable;always @(posedge src)unused<=1;always @(posedge gated)count<=count+1;{waiter} endmodule"
            );
            let build = || {
                Simulator::from_sv_sources(
                    vec![(
                        source.as_str(),
                        std::path::Path::new("single-derived-edge.sv"),
                    )],
                    "Top",
                )
            };
            check(Simulation::new(build().build_interpreter().unwrap()));
            check(Simulation::new(build().build_cranelift().unwrap()));
            check(Simulation::new(build().build_wasm().unwrap()));
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(Simulation::new(build().build_native().unwrap()));
        }
    }

    #[test]
    fn single_scheduled_update_keeps_edges_before_a_waiter_restores_the_clock() {
        let source = "module Top(input bit src,output bit[7:0]count);bit unused;bit enable=1;wire gated=src&enable;always @(posedge src)unused<=1;always @(posedge gated)count<=count+1;initial begin @(posedge gated);enable=0;end endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            sim.schedule("src", 1, 1).unwrap();
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("gated")), 0u8.into());
            assert_eq!(sim.get(sim.signal("count")), 1u8.into());
        }
        let build = || {
            Simulator::from_sv_sources(
                vec![(
                    source,
                    std::path::Path::new("single-transient-derived-edge.sv"),
                )],
                "Top",
            )
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }

    #[test]
    fn queued_pulse_reaches_derived_ff_without_process_waiters() {
        let source = "module Top(input bit src,enable,output bit q);bit unused;wire gated=src&enable;always @(posedge src)unused<=1;always @(negedge gated)q<=1;endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            let src = sim.signal("src");
            let enable = sim.signal("enable");
            sim.modify(|io| {
                io.set(src, 1u8);
                io.set(enable, 1u8);
            })
            .unwrap();
            sim.run_until(0).unwrap();
            assert_eq!(sim.get(sim.signal("q")), 0u8.into());
            sim.schedule("src", 1, 0).unwrap();
            sim.schedule("src", 1, 1).unwrap();
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("gated")), 1u8.into());
            assert_eq!(sim.get(sim.signal("q")), 1u8.into());
        }
        let build = || {
            Simulator::from_sv_sources(
                vec![(source, std::path::Path::new("queued-derived-ff.sv"))],
                "Top",
            )
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
}

#[cfg(test)]
mod coincident_host_edge_limit_review_tests {
    use super::*;
    use crate::frontend_sdk::{BinaryOp, Constant, Edge, ModuleBuilder, Statement, ValueType};
    fn design() -> crate::frontend_sdk::FrontendArtifact {
        design_with_initial_host(false)
    }
    fn design_with_initial_host(high: bool) -> crate::frontend_sdk::FrontendArtifact {
        let bit = ValueType::bits(1).unwrap();
        let byte = ValueType::bits(8).unwrap();
        let mut m = ModuleBuilder::new("Top").unwrap();
        let host = m.input("host", bit).unwrap();
        if high {
            m.set_initial(host, Constant::two_state(1u8, 1).unwrap())
                .unwrap();
        }
        let clk = m.internal("clk", bit).unwrap();
        let seen = m.output("seen", bit).unwrap();
        let q = m.output("q", byte).unwrap();
        let one = m.constant(Constant::two_state(1u8, 1).unwrap());
        let target = m.whole(seen).unwrap();
        m.register(target, one, host, Edge::Posedge, None, None)
            .unwrap();
        let value = m.read(q).unwrap();
        let one = m.constant(Constant::two_state(1u8, 8).unwrap());
        let next = m.binary(BinaryOp::Add, value, one, byte).unwrap();
        let target = m.whole(q).unwrap();
        m.register(target, next, clk, Edge::Posedge, None, None)
            .unwrap();
        m.clock_period(clk, 4).unwrap();
        let count = m.constant(Constant::two_state(8u8, 64).unwrap());
        m.process(vec![Statement::ClockCycles { clock: clk, count }])
            .unwrap();
        m.finish()
    }
    fn check<B: SimBackend>(mut sim: Simulation<B>, method: &str, limit: u64) {
        sim.set_wide(sim.signal("host"), 1u8.into());
        if method == "step_until" {
            let reached = sim.step_until(limit).unwrap().unwrap();
            assert_eq!(reached, sim.time());
            assert!(reached <= limit);
        } else {
            sim.run_until(limit).unwrap();
        }
        if method == "run_until" {
            assert_eq!(sim.time(), limit);
        }
        let ticks = 1 + sim.time() / 4;
        assert_eq!(sim.ticks(), ticks);
        assert_eq!(sim.get(sim.signal("q")), (ticks as u8).into());
        assert_eq!(sim.get(sim.signal("seen")), 1u8.into());
        sim.run_until(4).unwrap();
        assert_eq!(sim.time(), 4);
        assert_eq!(sim.ticks(), 2);
        assert_eq!(sim.get(sim.signal("q")), 2u8.into());
        sim.run_until(28).unwrap();
        assert_eq!(sim.ticks(), 8);
        assert_eq!(sim.get(sim.signal("q")), 8u8.into());
    }
    #[test]
    fn coincident_host_edges_report_the_time_reached_by_step() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.set_wide(sim.signal("host"), 0u8.into());
            let reached = sim.step().unwrap().unwrap();
            assert_eq!(reached, sim.time());
            assert_eq!(sim.get(sim.signal("q")), (sim.ticks() as u8).into());
            sim.run_until(28).unwrap();
            assert_eq!(sim.ticks(), 8);
            assert_eq!(sim.get(sim.signal("q")), 8u8.into());
        }
        let build = || Simulator::from_frontend(design_with_initial_host(true));
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }

    #[test]
    fn coincident_host_edges_do_not_fuse_past_public_time_limits() {
        for (method, limit) in [
            ("step_until", 0),
            ("step_until", 4),
            ("run_until", 0),
            ("run_until", 4),
        ] {
            let build = || Simulator::from_frontend(design());
            check(
                Simulation::new(build().build_interpreter().unwrap()),
                method,
                limit,
            );
            check(
                Simulation::new(build().build_cranelift().unwrap()),
                method,
                limit,
            );
            check(
                Simulation::new(build().build_wasm().unwrap()),
                method,
                limit,
            );
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(
                Simulation::new(build().build_native().unwrap()),
                method,
                limit,
            );
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod host_finish_edge_review_tests {
    use super::*;
    #[test]
    fn host_finish_and_edge_do_not_report_a_step() {
        let source = "module Top(input bit a,output bit q);always_comb if(a)$finish;always @(posedge a)q<=1;endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>, bounded: bool) {
            sim.run_until(0).unwrap();
            assert!(!sim.is_finished());
            sim.set_wide(sim.signal("a"), 1u8.into());
            let result = if bounded {
                sim.step_until(0)
            } else {
                sim.step()
            };
            assert_eq!(result.unwrap(), None);
            assert!(sim.is_finished());
            assert_eq!(sim.get(sim.signal("q")), 0u8.into());
            assert_eq!(sim.time(), 0);
        }
        for bounded in [false, true] {
            let build = || {
                Simulator::from_sv_sources(
                    vec![(source, std::path::Path::new("host-finish-edge.sv"))],
                    "Top",
                )
            };
            check(
                Simulation::new(build().build_interpreter().unwrap()),
                bounded,
            );
            check(Simulation::new(build().build_cranelift().unwrap()), bounded);
            check(Simulation::new(build().build_wasm().unwrap()), bounded);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(Simulation::new(build().build_native().unwrap()), bounded);
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod cross_mode_clock_baseline_review_tests {
    use super::*;
    #[test]
    fn unknown_clock_baselines_coerce_with_loaded_state() {
        let source =
            "module Top(input logic clk,output bit[7:0]q);always @(negedge clk)q<=q+1;endmodule";
        fn check<B: SimBackend>(mut source: Simulation<B>, mut target: Simulation<B>, payload: u8) {
            let clk = source.signal("clk");
            source
                .modify(|io| io.set_four_state(clk, payload.into(), 1u8.into()))
                .unwrap();
            source.run_until(0).unwrap();
            assert_eq!(source.get(source.signal("q")), 0u8.into());
            let file = source.save_state().unwrap();
            target.load_state(&file).unwrap();
            target.settle().unwrap();
            assert_eq!(target.get(target.signal("q")), 0u8.into());
            let clk = target.signal("clk");
            target.set_wide(clk, 1u8.into());
            target.settle().unwrap();
            target.set_wide(clk, 0u8.into());
            target.settle().unwrap();
            assert_eq!(target.get(target.signal("q")), 1u8.into());
        }
        let bit_source = source.replace("input logic clk", "input bit clk");
        for (payload, target_four_state, target_bit) in [
            (0, false, false),
            (1, false, false),
            (0, true, true),
            (1, true, true),
            (0, true, false),
            (1, true, false),
        ] {
            let build = |four_state, bit| {
                let code = if bit { bit_source.as_str() } else { source };
                Simulator::from_sv_sources(
                    vec![(code, std::path::Path::new("baseline-coercion.sv"))],
                    "Top",
                )
                .four_state(four_state)
            };
            check(
                Simulation::new(build(true, false).build_interpreter().unwrap()),
                Simulation::new(
                    build(target_four_state, target_bit)
                        .build_interpreter()
                        .unwrap(),
                ),
                payload,
            );
            check(
                Simulation::new(build(true, false).build_cranelift().unwrap()),
                Simulation::new(
                    build(target_four_state, target_bit)
                        .build_cranelift()
                        .unwrap(),
                ),
                payload,
            );
            check(
                Simulation::new(build(true, false).build_wasm().unwrap()),
                Simulation::new(build(target_four_state, target_bit).build_wasm().unwrap()),
                payload,
            );
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(
                Simulation::new(build(true, false).build_native().unwrap()),
                Simulation::new(build(target_four_state, target_bit).build_native().unwrap()),
                payload,
            );
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod zero_delay_review_tests {
    use super::*;
    #[test]
    fn zero_delay_repeating_processes_report_non_progress() {
        let source = "module Top(output bit clk);always #0 clk=~clk;endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            let error = sim.run_until(0).unwrap_err().to_string();
            assert!(error.contains("delta"), "{error}");
            assert_eq!(sim.time(), 0);
        }
        let build = || {
            Simulator::from_sv_sources(vec![(source, std::path::Path::new("zero-delay.sv"))], "Top")
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
    #[test]
    fn finite_inactive_delays_and_positive_time_loops_still_run() {
        for source in [
            "module Top(output bit q);initial begin repeat(3) #0 q=~q;end endmodule",
            "module Top(output bit q);always #1 q=~q;endmodule",
        ] {
            let mut sim = Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("delta-control.sv"))],
                "Top",
            )
            .build()
            .unwrap();
            sim.run_until(3).unwrap();
            assert_eq!(sim.time(), 3);
            assert_eq!(sim.get(sim.signal("q")), 1u8.into());
        }
    }

    #[test]
    fn static_logic_locals_start_unknown_and_retain_updates() {
        fn check<B: SimBackend>(mut sim: Simulation<B>, ff: bool) {
            if ff {
                sim.schedule("clk", 1, 1).unwrap();
            }
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("out")), 1u8.into());
            if ff {
                sim.schedule("clk", 2, 0).unwrap();
                sim.schedule("clk", 3, 1).unwrap();
            }
            sim.run_until(if ff { 3 } else { 2 }).unwrap();
            assert_eq!(sim.get(sim.signal("out")), 0u8.into());
        }
        for (source, ff) in [
            (
                "module Top(input bit clk,output bit out);task t;static logic x;out=(x===1'bx);x=1;endtask always @(posedge clk)t();endmodule",
                true,
            ),
            (
                "module Top(output bit out);task t;static logic x;out=(x===1'bx);x=1;endtask initial begin #1;t();#1;t();end endmodule",
                false,
            ),
        ] {
            let build = || {
                Simulator::from_sv_sources(
                    vec![(source, std::path::Path::new("static-x.sv"))],
                    "Top",
                )
                .four_state(true)
            };
            check(Simulation::new(build().build_interpreter().unwrap()), ff);
            check(Simulation::new(build().build_cranelift().unwrap()), ff);
            check(Simulation::new(build().build_wasm().unwrap()), ff);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(Simulation::new(build().build_native().unwrap()), ff);
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod active_feedback_review_tests {
    use super::*;

    #[test]
    fn active_waiter_feedback_reports_non_progress() {
        let source = "module Top(output bit a,b);initial begin #1;a=1;end always @(a)b=~b;always @(b)a=~a;endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            let error = sim.step().unwrap_err().to_string();
            assert!(error.contains("delta limit"), "{error}");
            let error = sim.run_until(1).unwrap_err().to_string();
            assert!(error.contains("delta limit"), "{error}");
            assert_eq!(sim.time(), 1);
        }
        let build = || {
            Simulator::from_sv_sources(vec![(source, std::path::Path::new("feedback.sv"))], "Top")
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }

    #[test]
    fn finite_feedback_settles_and_time_advanced_feedback_runs() {
        for (body, time, expected) in [
            (
                "always @(a) if(a<8'b100) b=a;always @(b) if(b<8'b100) a=b+1;",
                1,
                4u8,
            ),
            (
                "always @(a) begin #1;b=a;end always @(b) begin #1;a=b+1;end",
                5,
                3u8,
            ),
        ] {
            let source =
                format!("module Top(output bit[7:0]a,b);initial begin #1;a=1;end {body} endmodule");
            fn check<B: SimBackend>(mut sim: Simulation<B>, time: u64, expected: u8) {
                sim.run_until(time).unwrap();
                assert_eq!(sim.get(sim.signal("a")), expected.into());
            }
            let build = || {
                Simulator::from_sv_sources(
                    vec![(source.as_str(), std::path::Path::new("finite-feedback.sv"))],
                    "Top",
                )
            };
            check(
                Simulation::new(build().build_interpreter().unwrap()),
                time,
                expected,
            );
            check(
                Simulation::new(build().build_cranelift().unwrap()),
                time,
                expected,
            );
            check(
                Simulation::new(build().build_wasm().unwrap()),
                time,
                expected,
            );
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(
                Simulation::new(build().build_native().unwrap()),
                time,
                expected,
            );
        }
    }

    #[test]
    fn blocking_concat_records_only_the_complete_assignment() {
        for body in [
            "initial begin #1;{q[1],q[0]}=2'b11;#1;q[0]=0;end",
            "always @(posedge clk){q[1],q[0]}=2'b11;",
        ] {
            let source = format!(
                "module Top(input bit clk,output bit[1:0]q,output bit seen);{body} initial begin @(posedge(q[1]^q[0]));seen=1;end endmodule"
            );
            fn check<B: SimBackend>(mut sim: Simulation<B>, timed: bool) {
                sim.run_until(0).unwrap();
                if !timed {
                    sim.schedule("clk", 1, 1).unwrap();
                }
                sim.run_until(1).unwrap();
                assert_eq!(sim.get(sim.signal("q")), 3u8.into());
                assert_eq!(sim.get(sim.signal("seen")), 0u8.into());
                if timed {
                    sim.run_until(2).unwrap();
                    assert_eq!(sim.get(sim.signal("seen")), 1u8.into());
                }
            }
            let build = || {
                Simulator::from_sv_sources(
                    vec![(source.as_str(), std::path::Path::new("concat-event.sv"))],
                    "Top",
                )
            };
            let timed = body.starts_with("initial");
            check(Simulation::new(build().build_interpreter().unwrap()), timed);
            check(Simulation::new(build().build_cranelift().unwrap()), timed);
            check(Simulation::new(build().build_wasm().unwrap()), timed);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(Simulation::new(build().build_native().unwrap()), timed);
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod lazy_finish_settle_review {
    use super::*;
    #[test]
    fn settle_recognizes_finish_after_lazy_reads() {
        let source = "module Top(input bit go,output bit y);always_comb begin y=go;if(go)$finish;end endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            let go = sim.signal("go");
            let y = sim.signal("y");
            sim.modify(|io| io.set(go, 1u8)).unwrap();
            assert_eq!(sim.get(y), 1u8.into());
            sim.settle().unwrap();
            assert!(sim.is_finished());
            assert_eq!(sim.step().unwrap(), None);
        }
        let build = || {
            Simulator::from_sv_sources(
                vec![(source, std::path::Path::new("lazy-finish.sv"))],
                "Top",
            )
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod post_nba_retrigger_review_tests {
    use super::*;
    #[test]
    fn nba_woken_processes_retrigger_the_same_ff_domain() {
        let source = "module Top(output bit[7:0]q);bit clk;always @(posedge clk)q<=q+1;initial begin clk=1;wait(q==1);clk=0;clk=1;end endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            assert_eq!(sim.get(sim.signal("q")), 2u8.into());
        }
        let build = || {
            Simulator::from_sv_sources(
                vec![(source, std::path::Path::new("nba-retrigger.sv"))],
                "Top",
            )
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod other_domain_host_step_review {
    use super::*;
    #[test]
    fn any_change_runtime_domains_report_host_falls_at_current_time() {
        let source = "module Top(input bit clk,output bit[7:0]count);always @(posedge clk)count<=count+1;endmodule";
        fn check<B: SimBackend>(simulator: Simulator<B>, bounded: bool) {
            // The compiled event entry is the FF operation. Its edge policy
            // comes from runtime metadata; exercise an any-change domain here.
            let signal = simulator.signal("clk");
            let address = simulator.program.get_addr(&[], &["clk"]).unwrap();
            let event = simulator.backend.resolve_event(&address);
            let mut kinds = vec![None; simulator.backend.num_events()];
            kinds[event.id()] = Some(celox_design::DomainKind::Other);
            let mut info = vec![
                EventInfo {
                    canonical_id: usize::MAX,
                    is_cascaded: false,
                    eval_ff_event: None,
                    eval_only_event: None,
                    apply_event: None
                };
                simulator.backend.num_events()
            ];
            info[event.id()] = EventInfo {
                canonical_id: event.id(),
                is_cascaded: false,
                eval_ff_event: Some(event),
                eval_only_event: simulator.backend.resolve_eval_only_event(&address),
                apply_event: simulator.backend.resolve_apply_event(&address),
            };
            let state = SimulationState::new(
                &simulator.backend,
                vec![(signal, event.id(), event.id())],
                kinds,
                info,
                Vec::new(),
            );
            let mut sim = Simulation {
                simulator,
                state,
                pending_host_work: false,
            };
            sim.set_wide(signal, 1u8.into());
            assert_eq!(sim.step().unwrap(), Some(0));
            assert_eq!(sim.get(sim.signal("count")), 1u8.into());
            sim.set_wide(signal, 0u8.into());
            let time = if bounded {
                sim.step_until(0).unwrap()
            } else {
                sim.step().unwrap()
            };
            assert_eq!(sim.get(sim.signal("count")), 2u8.into());
            assert_eq!(time, Some(0));
        }
        for bounded in [false, true] {
            let build = || {
                Simulator::from_sv_sources(
                    vec![(source, std::path::Path::new("any-change-operation.sv"))],
                    "Top",
                )
            };
            check(build().build_interpreter().unwrap(), bounded);
            check(build().build_cranelift().unwrap(), bounded);
            check(build().build_wasm().unwrap(), bounded);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(build().build_native().unwrap(), bounded);
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod additional_host_edge_reviews {
    use super::*;
    #[test]
    fn early_derived_edges_survive_blocking_source_ff_writes() {
        let source = "module Top(input bit src,output bit q,seen);bit enable=1;wire gated=src&enable;always @(posedge src)enable=0;always @(posedge gated)q<=1;initial begin @(posedge gated);seen=1;end endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            sim.schedule("src", 1, 1).unwrap();
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), 1u8.into());
            assert_eq!(sim.get(sim.signal("gated")), 0u8.into());
            assert_eq!(sim.get(sim.signal("q")), 1u8.into());
        }
        let build = || {
            Simulator::from_sv_sources(
                vec![(source, std::path::Path::new("source-blocking-edge.sv"))],
                "Top",
            )
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
    #[test]
    fn host_changes_sample_all_event_waiters_before_continuations() {
        let source = "module Top(input bit go,output bit a,b);bit enable=1;initial begin @(posedge(go&enable));enable=0;a=1;end initial begin @(posedge(go&enable));b=1;end endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            let go = sim.signal("go");
            sim.modify(|io| io.set(go, 1u8)).unwrap();
            sim.run_until(0).unwrap();
            assert_eq!(sim.get(sim.signal("a")), 1u8.into());
            assert_eq!(sim.get(sim.signal("b")), 1u8.into());
        }
        let build = || {
            Simulator::from_sv_sources(
                vec![(source, std::path::Path::new("host-waiters.sv"))],
                "Top",
            )
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
    #[test]
    fn lazy_facade_operations_latch_runtime_finish() {
        let source = "module Top(input bit go,output bit y);always_comb begin y=go;if(go)$finish;end endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>, op: &str) {
            let go = sim.signal("go");
            let y = sim.signal("y");
            sim.simulator
                .set_wide(go, if op == "modify" { 0u8 } else { 1u8 }.into());
            assert!(!sim.is_finished());
            match op {
                "get" => {
                    assert_eq!(sim.get(y), 1u8.into());
                }
                "get_four_state" => {
                    assert_eq!(sim.get_four_state(y), (1u8.into(), 0u8.into()));
                }
                "drain" => {
                    assert!(
                        sim.drain_runtime_events()
                            .iter()
                            .any(|e| matches!(e, crate::RuntimeEvent::Finish))
                    );
                }
                "modify" => {
                    sim.modify(|io| io.set(go, 1u8)).unwrap();
                    assert!(!sim.is_finished(), "the host write is still deferred");
                    assert_eq!(sim.get(y), 1u8.into());
                }
                _ => unreachable!(),
            }
            assert!(sim.is_finished(), "{op}");
            assert_eq!(sim.step().unwrap(), None);
            sim.simulator.set_wide(go, 0u8.into());
            assert_eq!(sim.get(y), 1u8.into());
            sim.drain_runtime_events();
            assert_eq!(sim.simulator.backend.get(y), 1u8.into());
        }
        for op in ["get", "get_four_state", "drain", "modify"] {
            let build = || {
                Simulator::from_sv_sources(
                    vec![(source, std::path::Path::new("facade-finish.sv"))],
                    "Top",
                )
            };
            check(Simulation::new(build().build_interpreter().unwrap()), op);
            check(Simulation::new(build().build_cranelift().unwrap()), op);
            check(Simulation::new(build().build_wasm().unwrap()), op);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(Simulation::new(build().build_native().unwrap()), op);
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod host_finish_queued_settle_review {
    use super::*;
    #[test]
    fn host_finish_prevents_a_queued_current_time_edge() {
        let source = "module Top(input bit go,clk,output bit q);always @(posedge clk)q<=1;always_comb if(go)$finish;endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            let go = sim.signal("go");
            let q = sim.signal("q");
            sim.schedule("clk", 0, 1).unwrap();
            sim.set_wide(go, 1u8.into());
            sim.settle().unwrap();
            assert!(sim.is_finished());
            assert_eq!(sim.get(q), 0u8.into());
            assert_eq!(sim.get(sim.signal("clk")), 0u8.into());
        }
        let build = || {
            Simulator::from_sv_sources(
                vec![(source, std::path::Path::new("queued-finish.sv"))],
                "Top",
            )
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod lazy_event_pulse_review {
    use super::*;
    #[test]
    fn lazy_settles_retain_host_event_pulses() {
        let source = "module Top(input bit go,output bit y,woke);assign y=go;initial begin @(posedge go);woke=1;end endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            let go = sim.signal("go");
            let y = sim.signal("y");
            let woke = sim.signal("woke");
            sim.modify(|io| io.set(go, 1u8)).unwrap();
            sim.eval_comb().unwrap();
            assert_eq!(sim.get(y), 1u8.into());
            assert_eq!(sim.get(woke), 0u8.into());
            sim.modify(|io| io.set(go, 0u8)).unwrap();
            sim.eval_comb().unwrap();
            assert_eq!(sim.step().unwrap(), Some(0));
            assert_eq!(sim.get(woke), 1u8.into());
        }
        let build = || {
            Simulator::from_sv_sources(vec![(source, std::path::Path::new("lazy-pulse.sv"))], "Top")
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod lazy_read_and_ff_edges_review {
    use super::*;
    #[test]
    fn lazy_rust_reads_retain_host_event_pulses() {
        let source = "module Top(input bit go,output bit y,woke);assign y=go;initial begin @(posedge go);woke=1;end endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            let go = sim.signal("go");
            let y = sim.signal("y");
            let woke = sim.signal("woke");
            sim.modify(|io| io.set(go, 1u8)).unwrap();
            assert_eq!(sim.get(y), 1u8.into());
            sim.modify(|io| io.set(go, 0u8)).unwrap();
            assert_eq!(sim.get(y), 0u8.into());
            assert_eq!(sim.step().unwrap(), Some(0));
            assert_eq!(sim.get(woke), 1u8.into());
        }
        let build = || {
            Simulator::from_sv_sources(vec![(source, std::path::Path::new("read-pulse.sv"))], "Top")
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
    #[test]
    fn explicit_settles_retain_derived_ff_pulses() {
        let source = "module Top(input bit go,output bit derived,output bit[7:0]q);assign derived=go;always_ff @(posedge derived)q<=q+1;endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            let go = sim.signal("go");
            let q = sim.signal("q");
            sim.modify(|io| io.set(go, 1u8)).unwrap();
            sim.eval_comb().unwrap();
            assert_eq!(sim.get(q), 0u8.into());
            sim.modify(|io| io.set(go, 0u8)).unwrap();
            sim.eval_comb().unwrap();
            let checkpoint = sim.checkpoint().unwrap();
            let file = sim.save_state().unwrap();
            let mut bytes = Vec::new();
            file.write_to(&mut bytes).unwrap();
            let file = celox_runtime::StateFile::read_from(bytes.as_slice()).unwrap();
            assert_eq!(sim.step().unwrap(), Some(0));
            assert_eq!(sim.get(q), 1u8.into());
            assert_eq!(sim.step().unwrap(), None);
            sim.restore(&checkpoint).unwrap();
            assert_eq!(sim.step().unwrap(), Some(0));
            assert_eq!(sim.get(q), 1u8.into());
            sim.load_state(&file).unwrap();
            assert_eq!(sim.step().unwrap(), Some(0));
            assert_eq!(sim.get(q), 1u8.into());
            assert_eq!(sim.step().unwrap(), None);
        }
        let build = || {
            Simulator::from_sv_sources(vec![(source, std::path::Path::new("ff-pulse.sv"))], "Top")
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod loaded_schedule_and_finish_review {
    use super::*;
    #[test]
    fn ff_finish_keeps_later_queued_event_unapplied() {
        let source = "module Top(input bit clk,output bit q);always @(posedge clk)begin q=1;$finish;end endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            sim.schedule("clk", 1, 0).unwrap();
            let clk = sim.signal("clk");
            sim.modify(|io| io.set(clk, 1u8)).unwrap();
            sim.run_until(2).unwrap();
            assert!(sim.is_finished());
            assert_eq!(sim.time(), 0);
            assert_eq!(sim.get(clk), 1u8.into());
            assert_eq!(sim.get(sim.signal("q")), 1u8.into());
        }
        let build = || {
            Simulator::from_sv_sources(
                vec![(source, std::path::Path::new("queued-after-finish.sv"))],
                "Top",
            )
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
    #[test]
    fn state_load_rejects_past_events_atomically() {
        let source = "module Top(input bit clk,output bit q);always @(posedge clk)q<=!q;endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(5).unwrap();
            sim.schedule("clk", 6, 1).unwrap();
            let file = sim.save_state().unwrap();
            for periodic in [false, true] {
                let mut corrupt = file.clone();
                let record = corrupt.schedule.as_mut().unwrap();
                if periodic {
                    let mut e = record.events[0].clone();
                    e.time = 4;
                    record.periodic.push((e, 1));
                } else {
                    record.events[0].time = 4;
                }
                let before = sim.save_state().unwrap();
                assert!(sim.load_state(&corrupt).is_err());
                assert_eq!(sim.save_state().unwrap(), before);
                assert_eq!(sim.time(), 5);
            }
            sim.load_state(&file).unwrap();
            sim.run_until(6).unwrap();
            assert_eq!(sim.get(sim.signal("q")), 1u8.into());
        }
        let build = || {
            Simulator::from_sv_sources(
                vec![(source, std::path::Path::new("past-loaded-event.sv"))],
                "Top",
            )
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
}
#[cfg(all(test, feature = "systemverilog"))]
mod implicit_host_edge_review {
    use super::*;
    #[test]
    fn implicit_settles_capture_ff_edges_without_waiters() {
        let source =
            "module Top(input bit clk,output bit[7:0]q);always_ff @(posedge clk)q<=q+1;endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>, operation: &str) {
            sim.run_until(0).unwrap();
            let clk = sim.signal("clk");
            sim.modify(|io| io.set(clk, 1u8)).unwrap();
            match operation {
                "step" => assert_eq!(sim.step().unwrap(), Some(0)),
                "bounded" => assert_eq!(sim.step_until(0).unwrap(), Some(0)),
                "run" => sim.run_until(0).unwrap(),
                _ => unreachable!(),
            }
            assert_eq!(sim.get(sim.signal("q")), 1u8.into());
            assert_eq!(sim.step().unwrap(), None);
        }
        for operation in ["step", "bounded", "run"] {
            let build = || {
                Simulator::from_sv_sources(
                    vec![(source, std::path::Path::new("implicit-host-clock.sv"))],
                    "Top",
                )
            };
            check(
                Simulation::new(build().build_interpreter().unwrap()),
                operation,
            );
            check(
                Simulation::new(build().build_cranelift().unwrap()),
                operation,
            );
            check(Simulation::new(build().build_wasm().unwrap()), operation);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(Simulation::new(build().build_native().unwrap()), operation);
        }
    }
    #[test]
    fn host_four_state_edges_use_value_and_mask() {
        fn check<B: SimBackend>(mut sim: Simulation<B>, positive: bool) {
            let clk = sim.signal("clk");
            sim.modify(|io| io.set_four_state(clk, 0u8.into(), 0u8.into()))
                .unwrap();
            sim.eval_comb().unwrap();
            sim.run_until(0).unwrap();
            let q = sim.signal("q");
            let mut count = sim.get(q);
            for (value, mask, rise, fall) in [
                (0u8, 1u8, true, false),
                (1, 1, false, false),
                (1, 0, true, false),
                (1, 1, false, true),
                (0, 1, false, false),
                (0, 0, false, true),
            ] {
                sim.modify(|io| io.set_four_state(clk, value.into(), mask.into()))
                    .unwrap();
                sim.eval_comb().unwrap();
                sim.run_until(0).unwrap();
                count += u8::from(if positive { rise } else { fall });
                assert_eq!(
                    sim.get(q),
                    count,
                    "positive={positive} value={value} mask={mask}"
                );
                if mask == 1 {
                    let checkpoint = sim.checkpoint().unwrap();
                    let file = sim.save_state().unwrap();
                    let mut bytes = Vec::new();
                    file.write_to(&mut bytes).unwrap();
                    let file = celox_runtime::StateFile::read_from(bytes.as_slice()).unwrap();
                    sim.restore(&checkpoint).unwrap();
                    sim.load_state(&file).unwrap();
                }
            }
            let checkpoint = sim.checkpoint().unwrap();
            let file = sim.save_state().unwrap();
            sim.restore(&checkpoint).unwrap();
            sim.load_state(&file).unwrap();
        }
        for positive in [true, false] {
            let edge = if positive { "posedge" } else { "negedge" };
            let source = format!(
                "module Top(input logic clk,output bit[7:0]q);always @({edge} clk)q<=q+1;endmodule"
            );
            let build = || {
                Simulator::from_sv_sources(
                    vec![(source.as_str(), std::path::Path::new("host-four-state.sv"))],
                    "Top",
                )
                .four_state(true)
            };
            check(
                Simulation::new(build().build_interpreter().unwrap()),
                positive,
            );
            check(
                Simulation::new(build().build_cranelift().unwrap()),
                positive,
            );
            check(Simulation::new(build().build_wasm().unwrap()), positive);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(Simulation::new(build().build_native().unwrap()), positive);
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod finished_state_file_review {
    use super::*;
    #[test]
    fn finished_state_files_do_not_revive_ff_or_comb_designs() {
        fn check<B: SimBackend>(mut sim: Simulation<B>, mut restored: Simulation<B>, comb: bool) {
            sim.schedule("clk", 1, 1).unwrap();
            if comb {
                let go = sim.signal("go");
                sim.modify(|io| io.set(go, 1u8)).unwrap();
                sim.eval_comb().unwrap();
            } else {
                let clk = sim.signal("clk");
                sim.modify(|io| io.set(clk, 1u8)).unwrap();
                sim.run_until(0).unwrap();
            }
            assert!(sim.is_finished());
            let file = sim.save_state().unwrap();
            let mut bytes = Vec::new();
            file.write_to(&mut bytes).unwrap();
            let file = celox_runtime::StateFile::read_from(bytes.as_slice()).unwrap();
            restored.load_state(&file).unwrap();
            assert!(restored.is_finished());
            assert!(restored.simulator.has_pending_runtime_finish());
            assert_eq!(restored.step().unwrap(), None);
            restored.run_until(2).unwrap();
            assert_eq!(restored.time(), 0);
            assert_eq!(restored.get(restored.signal("q")), 1u8.into());
            restored
                .simulator
                .set_wide(restored.signal("go"), 0u8.into());
            assert_eq!(restored.get(restored.signal("q")), 1u8.into());
        }
        for comb in [true, false] {
            let source = if comb {
                "module Top(input bit clk,go,output bit q,dummy);always_comb begin q=go;if(go)$finish;end always @(posedge clk)dummy<=1;endmodule"
            } else {
                "module Top(input bit clk,go,output bit q);always @(posedge clk)begin q=1;$finish;end endmodule"
            };
            let build = || {
                Simulator::from_sv_sources(
                    vec![(source, std::path::Path::new("finished-state-file.sv"))],
                    "Top",
                )
            };
            check(
                Simulation::new(build().build_interpreter().unwrap()),
                Simulation::new(build().build_interpreter().unwrap()),
                comb,
            );
            check(
                Simulation::new(build().build_cranelift().unwrap()),
                Simulation::new(build().build_cranelift().unwrap()),
                comb,
            );
            check(
                Simulation::new(build().build_wasm().unwrap()),
                Simulation::new(build().build_wasm().unwrap()),
                comb,
            );
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(
                Simulation::new(build().build_native().unwrap()),
                Simulation::new(build().build_native().unwrap()),
                comb,
            );
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod nba_waiter_sampling_review {
    use super::*;
    #[test]
    fn all_nba_woken_waiters_sample_before_any_continuation() {
        let source = "module Child(input bit clk,output bit go);always_ff @(posedge clk)go<=1;endmodule module Top(input bit clk,output bit a,b);bit enable=1;wire go;Child child(.clk(clk),.go(go));initial begin @(posedge(go&enable));a=1;enable=0;end initial begin @(posedge(go&enable));b=1;end endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            sim.schedule("clk", 1, 1).unwrap();
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("a")), 1u8.into());
            assert_eq!(sim.get(sim.signal("b")), 1u8.into());
        }
        let build = || {
            Simulator::from_sv_sources(
                vec![(source, std::path::Path::new("nba-waiters.sv"))],
                "Top",
            )
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
    #[test]
    fn all_blocking_ff_woken_waiters_sample_before_any_continuation() {
        let source = "module Child(input bit clk,output bit go);always_ff @(posedge clk)go=1;endmodule module Top(input bit clk,output bit a,b);bit enable=1;wire go;Child child(.clk(clk),.go(go));initial begin @(posedge(go&enable));a=1;enable=0;end initial begin @(posedge(go&enable));b=1;end endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            sim.schedule("clk", 1, 1).unwrap();
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("a")), 1u8.into());
            assert_eq!(sim.get(sim.signal("b")), 1u8.into());
        }
        let build = || {
            Simulator::from_sv_sources(
                vec![(source, std::path::Path::new("blocking-ff-waiters.sv"))],
                "Top",
            )
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
    #[test]
    fn all_direct_blocking_ff_waiters_sample_before_any_continuation() {
        let source = "module Top(input bit clk,output bit a,b);bit enable=1;bit go;always_ff @(posedge clk)go=1;initial begin @(posedge(go&enable));a=1;enable=0;end initial begin @(posedge(go&enable));b=1;end endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            sim.schedule("clk", 1, 1).unwrap();
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("a")), 1u8.into());
            assert_eq!(sim.get(sim.signal("b")), 1u8.into());
        }
        let build = || {
            Simulator::from_sv_sources(
                vec![(
                    source,
                    std::path::Path::new("direct-blocking-ff-waiters.sv"),
                )],
                "Top",
            )
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
    #[test]
    fn finished_save_preserves_unsettled_output_and_roundtrip() {
        let source = "module Top(input bit clk,go,output bit q,derived);assign derived=q;always_comb begin q=go;if(go)$finish;end always @(posedge clk)begin end endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            let go = sim.signal("go");
            sim.modify(|io| io.set(go, 1u8)).unwrap();
            sim.eval_comb().unwrap();
            assert!(sim.is_finished());
            sim.modify(|io| io.set(go, 0u8)).unwrap();
            let file = sim.save_state().unwrap();
            let q = file.objects.iter().find(|obj| obj.path == "q").unwrap();
            assert_eq!(q.value, [1]);
            sim.load_state(&file).unwrap();
            assert!(sim.is_finished());
            assert_eq!(sim.get(sim.signal("q")), 1u8.into());
            assert_eq!(sim.get(sim.signal("derived")), 1u8.into());
            sim.eval_comb().unwrap();
            assert_eq!(sim.get(sim.signal("q")), 0u8.into());
        }
        let build = || {
            Simulator::from_sv_sources(
                vec![(source, std::path::Path::new("finished-dirty-save.sv"))],
                "Top",
            )
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
}

#[cfg(test)]
mod host_clock_activation_review {
    use super::*;
    use crate::frontend_sdk::{Constant, Edge, ModuleBuilder, Statement, ValueType};
    fn design() -> crate::frontend_sdk::FrontendArtifact {
        let mut m = ModuleBuilder::new("Top").unwrap();
        let bit = ValueType::bits(1).unwrap();
        let host = m.input("host", bit).unwrap();
        let clk = m.internal("clk", bit).unwrap();
        let source = m.output("source", bit).unwrap();
        let sampled = m.output("sampled", bit).unwrap();
        let one = m.constant(Constant::two_state(1u8, 1).unwrap());
        let target = m.whole(source).unwrap();
        m.register(target, one, host, Edge::Posedge, None, None)
            .unwrap();
        let old = m.read(source).unwrap();
        let target = m.whole(sampled).unwrap();
        m.register(target, old, clk, Edge::Posedge, None, None)
            .unwrap();
        m.clock_period(clk, 4).unwrap();
        let one = m.constant(Constant::two_state(1u8, 64).unwrap());
        m.process(vec![Statement::ClockCycles {
            clock: clk,
            count: one,
        }])
        .unwrap();
        m.finish()
    }
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        let host = sim.signal("host");
        sim.modify(|io| io.set(host, 1u8)).unwrap();
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("source")), 1u8.into());
        assert_eq!(sim.get(sim.signal("sampled")), 0u8.into());
        assert_eq!(sim.ticks(), 1);
    }
    #[test]
    fn captured_host_edges_share_pre_edge_state_with_first_clock_wait() {
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
mod mixed_other_domain_host_review {
    use super::*;
    #[test]
    fn any_change_host_falls_join_current_time_batches() {
        let source = "module Top(input bit clk,queued,output bit[7:0]count,queued_count);always @(posedge clk)count<=count+1;always @(posedge queued)queued_count<=queued_count+1;endmodule";
        fn check<B: SimBackend>(simulator: Simulator<B>, mode: u8) {
            // The compiled event entry is the FF operation. Its edge policy
            // comes from runtime metadata; exercise an any-change domain here.
            let signal = simulator.signal("clk");
            let address = simulator.program.get_addr(&[], &["clk"]).unwrap();
            let event = simulator.backend.resolve_event(&address);
            let mut kinds = vec![None; simulator.backend.num_events()];
            kinds[event.id()] = Some(celox_design::DomainKind::Other);
            let queued_address = simulator.program.get_addr(&[], &["queued"]).unwrap();
            let queued_event = simulator.backend.resolve_event(&queued_address);
            let queued_signal = simulator.signal("queued");
            kinds[queued_event.id()] = Some(celox_design::DomainKind::ClockPosedge);
            let mut info = vec![
                EventInfo {
                    canonical_id: usize::MAX,
                    is_cascaded: false,
                    eval_ff_event: None,
                    eval_only_event: None,
                    apply_event: None
                };
                simulator.backend.num_events()
            ];
            info[event.id()] = EventInfo {
                canonical_id: event.id(),
                is_cascaded: false,
                eval_ff_event: Some(event),
                eval_only_event: simulator.backend.resolve_eval_only_event(&address),
                apply_event: simulator.backend.resolve_apply_event(&address),
            };
            info[queued_event.id()] = EventInfo {
                canonical_id: queued_event.id(),
                is_cascaded: false,
                eval_ff_event: Some(queued_event),
                eval_only_event: simulator.backend.resolve_eval_only_event(&queued_address),
                apply_event: simulator.backend.resolve_apply_event(&queued_address),
            };
            let state = SimulationState::new(
                &simulator.backend,
                vec![
                    (signal, event.id(), event.id()),
                    (queued_signal, queued_event.id(), queued_event.id()),
                ],
                kinds,
                info,
                Vec::new(),
            );
            let mut sim = Simulation {
                simulator,
                state,
                pending_host_work: false,
            };
            sim.set_wide(signal, 1u8.into());
            assert_eq!(sim.step().unwrap(), Some(0));
            assert_eq!(sim.get(sim.signal("count")), 1u8.into());
            sim.set_wide(signal, 0u8.into());
            sim.schedule("queued", 0, 1).unwrap();
            let time = match mode {
                0 => sim.step().unwrap(),
                1 => sim.step_until(0).unwrap(),
                _ => sim.state.step(&mut sim.simulator).unwrap(),
            };
            assert_eq!(sim.get(sim.signal("count")), 2u8.into());
            assert_eq!(time, Some(0));
            assert_eq!(sim.get(sim.signal("queued_count")), 1u8.into());
        }
        for mode in [0, 1, 2] {
            let build = || {
                Simulator::from_sv_sources(
                    vec![(source, std::path::Path::new("any-change-operation.sv"))],
                    "Top",
                )
            };
            check(build().build_interpreter().unwrap(), mode);
            check(build().build_cranelift().unwrap(), mode);
            check(build().build_wasm().unwrap(), mode);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(build().build_native().unwrap(), mode);
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod active_settle_review {
    use super::*;
    #[test]
    fn all_propagated_blocking_waiters_sample_before_continuations() {
        let source = "module Child(output bit go);initial begin #1;go=1;end endmodule module Top(output bit a,b);bit enable=1;wire go;Child child(.go(go));initial begin @(posedge(go&enable));a=1;enable=0;end initial begin @(posedge(go&enable));b=1;end endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("a")), 1u8.into());
            assert_eq!(sim.get(sim.signal("b")), 1u8.into());
        }
        let build = || {
            Simulator::from_sv_sources(
                vec![(source, std::path::Path::new("active-waiters.sv"))],
                "Top",
            )
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
    #[test]
    fn finished_dump_preserves_dirty_outputs_and_events() {
        let source = "module Top(input bit go,output bit q);always_comb begin q=go;$display(\"q=%d\",q);if(go)$finish;end endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>, path: &std::path::Path) {
            let go = sim.signal("go");
            sim.modify(|io| io.set(go, 1u8)).unwrap();
            sim.eval_comb().unwrap();
            assert!(sim.is_finished());
            sim.drain_runtime_events();
            sim.modify(|io| io.set(go, 0u8)).unwrap();
            sim.try_dump(0).unwrap();
            sim.dump(1);
            sim.flush_vcd().unwrap();
            assert_eq!(sim.get(sim.signal("q")), 1u8.into());
            assert!(sim.drain_runtime_events().is_empty());
            assert!(
                std::fs::read_to_string(path)
                    .unwrap()
                    .contains("$enddefinitions")
            );
            sim.eval_comb().unwrap();
            assert_eq!(sim.get(sim.signal("q")), 0u8.into());
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("finished.vcd");
        let build = || {
            Simulator::from_sv_sources(
                vec![(source, std::path::Path::new("finished-dump.sv"))],
                "Top",
            )
            .vcd(&path)
        };
        check(Simulation::new(build().build_interpreter().unwrap()), &path);
        check(Simulation::new(build().build_cranelift().unwrap()), &path);
        check(Simulation::new(build().build_wasm().unwrap()), &path);
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()), &path);
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod cross_depth_write_review {
    use super::*;
    use std::path::Path;

    #[test]
    fn coincident_writers_at_different_dependency_depths_are_rejected() {
        let source = "module Direct(output bit q);initial begin #1;q=1;end endmodule module Leaf(output bit q);initial begin #1;q=1;end endmodule module Wrapper(output bit q);Leaf leaf(.q(q));endmodule module Top(output bit[7:0]seen);wire a,b;Direct direct(.q(a));Wrapper nested(.q(b));initial forever begin @(a^b);seen=seen+1;end endmodule";
        let error =
            match Simulator::from_sv_sources(vec![(source, Path::new("depth-writes.sv"))], "Top")
                .build_interpreter()
            {
                Ok(_) => panic!("different-depth coincident writers unexpectedly compiled"),
                Err(error) => error.to_string(),
            };
        assert!(error.contains("multiple blocking writes"), "{error}");
        let distinct = source.replace(
            "module Leaf(output bit q);initial begin #1;",
            "module Leaf(output bit q);initial begin #2;",
        );
        fn check<B: SimBackend>(mut sim: Simulation<B>, first: u8, second: u8) {
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), first.into());
            sim.run_until(2).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), second.into());
        }
        // Repeated reads of one nested writer do not duplicate its writes;
        // the simultaneous direct writer is irrelevant to this expression.
        let single = source.replace("@(a^b)", "@(b|b)");
        for (code, first, second) in [(distinct.as_str(), 1, 2), (single.as_str(), 1, 1)] {
            let build =
                || Simulator::from_sv_sources(vec![(code, Path::new("depth-writes.sv"))], "Top");
            check(
                Simulation::new(build().build_interpreter().unwrap()),
                first,
                second,
            );
            check(
                Simulation::new(build().build_cranelift().unwrap()),
                first,
                second,
            );
            check(
                Simulation::new(build().build_wasm().unwrap()),
                first,
                second,
            );
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(
                Simulation::new(build().build_native().unwrap()),
                first,
                second,
            );
        }
    }
}

#[cfg(test)]
mod transient_clock_activation_review {
    use super::*;
    use crate::frontend_sdk::{Constant, Edge, ModuleBuilder, Statement, ValueType};
    fn design() -> crate::frontend_sdk::FrontendArtifact {
        let mut m = ModuleBuilder::new("Top").unwrap();
        let bit = ValueType::bits(1).unwrap();
        let host = m.internal("host", bit).unwrap();
        let word = ValueType::bits(32).unwrap();
        let rises = m.internal("rises", word).unwrap();
        let falls = m.internal("falls", word).unwrap();
        let clk = m.internal("clk", bit).unwrap();
        let source = m.output("source", bit).unwrap();
        let sampled = m.output("sampled", bit).unwrap();
        let one = m.constant(Constant::two_state(1u8, 1).unwrap());
        let target = m.whole(source).unwrap();
        m.register(target, one, host, Edge::Posedge, None, None)
            .unwrap();
        let old = m.read(source).unwrap();
        let target = m.whole(sampled).unwrap();
        m.register(target, old, clk, Edge::Posedge, None, None)
            .unwrap();
        m.clock_period(clk, 4).unwrap();
        let one = m.constant(Constant::two_state(1u8, 64).unwrap());
        let high = m.constant(Constant::two_state(1u8, 1).unwrap());
        let low = m.constant(Constant::two_state(0u8, 1).unwrap());
        let target = m.whole(host).unwrap();
        let edge = m.constant(Constant::two_state(1u8, 32).unwrap());
        m.process(vec![
            Statement::Assign {
                target,
                value: high,
            },
            Statement::Assign { target, value: low },
            Statement::Assign {
                target: m.whole(rises).unwrap(),
                value: edge,
            },
            Statement::Assign {
                target: m.whole(falls).unwrap(),
                value: edge,
            },
            Statement::ClockCycles {
                clock: clk,
                count: one,
            },
        ])
        .unwrap();
        m.finish()
    }
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        let signal = sim.signal("host");
        let rises = sim.signal("rises");
        let falls = sim.signal("falls");
        sim.state = sim
            .state
            .with_process_event_counters(vec![(signal, rises, falls)]);
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("source")), 1u8.into());
        assert_eq!(sim.get(sim.signal("sampled")), 0u8.into());
        assert_eq!(sim.ticks(), 1);
    }
    #[test]
    fn transient_process_edges_share_pre_edge_state_with_first_clock_wait() {
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
mod wait_and_finish_review {
    use super::*;
    #[test]
    fn one_pass_do_while_preserves_fixed_write_times() {
        let source = "module Child(output bit[1:0]q);initial begin #1;do q[0]=1;while(0);end initial begin #2;q[1]=1;end endmodule module Top(output bit[7:0]seen);wire[1:0]q;Child child(.q(q));initial forever begin @(q[0]^q[1]);seen=seen+1;end endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), 1u8.into());
            sim.run_until(2).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), 2u8.into());
        }
        let build = || {
            Simulator::from_sv_sources(vec![(source, std::path::Path::new("do-while.sv"))], "Top")
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
        let coincident = source.replace("#2;q[1]", "#1;q[1]");
        assert!(
            Simulator::from_sv_sources(
                vec![(&coincident, std::path::Path::new("coincident.sv"))],
                "Top"
            )
            .build_interpreter()
            .is_err()
        );
    }
    #[test]
    fn zero_trip_while_preserves_fixed_write_times() {
        check_zero_trip_loop("while(0)");
    }
    #[test]
    fn zero_trip_repeat_preserves_fixed_write_times() {
        for control in ["repeat(0)", "repeat(-1)"] {
            check_zero_trip_loop(control);
        }
    }
    #[test]
    fn zero_trip_for_preserves_fixed_write_times() {
        for control in [
            "for(int i=0;i<0;i++)",
            "for(int i=2;i<1;i++)",
            "for(int i=0;i>1;i--)",
        ] {
            check_zero_trip_loop(control);
        }
        // The initializer creates a real same-activation pulse even though
        // the loop body is skipped. It must remain in the write summary.
        let source = "module Child(output bit[1:0]q);initial begin #1;for(q=1;q<0;q++)#5;q=0;end initial begin #2;q[1]=1;end endmodule module Top;wire[1:0]q;Child child(.q(q));initial forever @(q[0]^q[1]);endmodule";
        assert!(
            Simulator::from_sv_sources(vec![(source, std::path::Path::new("for-init.sv"))], "Top")
                .build_interpreter()
                .is_err()
        );
    }
    #[test]
    fn wrapping_for_loops_cannot_hide_cross_instance_pulses() {
        for (header, stop) in [
            ("bit[1:0]i=3;i<4;i++", "i==0"),
            ("bit[7:0]i=0;i<2'd3+2'd1;i++", "i==1"),
            ("bit signed[2:0]i=3;i<=3;i++", "i<0"),
            ("bit signed[2:0]i=-4;i>=-4;i--", "i>0"),
            ("bit[1:0]i=4;i<2;i++", "i==1"),
            ("bit signed[1:0]i=-1;i>32'd1;i--", "i==-2"),
        ] {
            let source = format!(
                "module Child(output bit q,output int unsigned iterations);initial begin #1;for({header})begin q=~q;iterations++;if({stop})break;end end endmodule module Top(output bit[7:0]seen,output int unsigned iterations);wire q;Child child(.q(q),.iterations(iterations));initial forever begin @(q);seen++;end endmodule"
            );
            for four_state in [false, true] {
                let result = Simulator::from_sv_sources(
                    vec![(source.as_str(), std::path::Path::new("wrapped-pulse.sv"))],
                    "Top",
                )
                .four_state(four_state)
                .build_interpreter();
                let error = match result {
                    Ok(_) => panic!("accepted hidden pulse: {header}, four_state={four_state}"),
                    Err(error) => error.to_string(),
                };
                assert!(
                    error.contains("multiple blocking writes"),
                    "{header}: {error}"
                );
            }
            // Execute the same loop without the unsupported cross-instance
            // observer, proving two real writes rather than an estimated cap.
            let source = source.replace("initial forever begin @(q);seen++;end", "");
            fn check<B: SimBackend>(mut sim: Simulation<B>) {
                sim.run_until(2).unwrap();
                assert_eq!(sim.get(sim.signal("iterations")), 2u32.into());
                assert_eq!(sim.get(sim.signal("q")), 0u8.into());
            }
            let build = || {
                Simulator::from_sv_sources(
                    vec![(
                        source.as_str(),
                        std::path::Path::new("wrapped-execution.sv"),
                    )],
                    "Top",
                )
            };
            check(Simulation::new(build().build_interpreter().unwrap()));
            check(Simulation::new(build().build_cranelift().unwrap()));
            check(Simulation::new(build().build_wasm().unwrap()));
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(Simulation::new(build().build_native().unwrap()));
        }
    }

    #[test]
    fn typed_for_trip_counts_preserve_zero_and_single_iteration_observers() {
        for four_state in [false, true] {
            for (header, iterations) in [
                ("bit[1:0]i=2;i<3;i++", 1u32),
                ("bit[7:0]i=3;i<2'd3+2'd1;i++", 1),
                ("bit[1:0]i=2'bx0;i<1;i++", 1),
                ("bit[1:0]i=1;i<2;i<<=1", 1),
                ("bit signed[2:0]i=-2;i<-1;i++", 1),
                ("bit signed[2:0]i=2;i>1;i--", 1),
                ("bit signed[1:0]i=-1;i<32'd1;i++", 0),
                ("bit[7:0]i=2'd3+2'd1;i<1;i++", 0),
                (
                    "bit[127:0]i='1;i>=128'hffffffffffffffffffffffffffffffff;i--",
                    1,
                ),
            ] {
                let source = format!(
                    "module Child(output bit q,output int unsigned iterations);initial begin #1;for({header})begin q=~q;iterations++;end end endmodule module Top(output bit[7:0]seen,output int unsigned iterations);wire q;Child child(.q(q),.iterations(iterations));initial forever begin @(q);seen++;end endmodule"
                );
                fn check<B: SimBackend>(mut sim: Simulation<B>, iterations: u32, header: &str) {
                    sim.run_until(2).unwrap();
                    assert_eq!(
                        sim.get(sim.signal("iterations")),
                        iterations.into(),
                        "{header}"
                    );
                    assert_eq!(sim.get(sim.signal("seen")), iterations.into(), "{header}");
                }
                let build = || {
                    Simulator::from_sv_sources(
                        vec![(
                            source.as_str(),
                            std::path::Path::new("typed-trip-control.sv"),
                        )],
                        "Top",
                    )
                    .four_state(four_state)
                };
                check(
                    Simulation::new(build().build_interpreter().unwrap()),
                    iterations,
                    header,
                );
                check(
                    Simulation::new(build().build_cranelift().unwrap()),
                    iterations,
                    header,
                );
                check(
                    Simulation::new(build().build_wasm().unwrap()),
                    iterations,
                    header,
                );
                #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
                check(
                    Simulation::new(build().build_native().unwrap()),
                    iterations,
                    header,
                );
            }
        }
    }

    #[test]
    fn loop_body_mutations_prevent_single_trip_proofs() {
        for mutation in ["i=-1;", "reset_i(i);"] {
            let source = format!(
                "module Child(output bit q,output int unsigned iterations);task reset_i(inout int i);i=-1;endtask initial begin #1;for(int i=0;i<1;i++)begin q=~q;iterations++;{mutation} if(iterations==2)break;end end endmodule module Top(output int unsigned iterations);wire q;Child child(.q(q),.iterations(iterations));initial forever @(q);endmodule"
            );
            let result = Simulator::from_sv_sources(
                vec![(source.as_str(), std::path::Path::new("mutated-trip.sv"))],
                "Top",
            )
            .build_interpreter();
            let error = match result {
                Ok(_) => panic!("accepted repeated loop body: {mutation}"),
                Err(error) => error.to_string(),
            };
            assert!(
                error.contains("multiple blocking writes"),
                "{mutation}: {error}"
            );
            let source = source.replace("initial forever @(q);", "");
            fn check<B: SimBackend>(mut sim: Simulation<B>) {
                sim.run_until(2).unwrap();
                assert_eq!(sim.get(sim.signal("iterations")), 2u32.into());
                assert_eq!(sim.get(sim.signal("q")), 0u8.into());
            }
            let build = || {
                Simulator::from_sv_sources(
                    vec![(
                        source.as_str(),
                        std::path::Path::new("mutated-trip-execution.sv"),
                    )],
                    "Top",
                )
            };
            check(Simulation::new(build().build_interpreter().unwrap()));
            check(Simulation::new(build().build_cranelift().unwrap()));
            check(Simulation::new(build().build_wasm().unwrap()));
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(Simulation::new(build().build_native().unwrap()));
        }
    }

    #[test]
    fn zero_trip_for_proofs_respect_initializer_width_and_comparison_sign() {
        for declaration_and_loop in [
            "bit i;initial begin #1;for(i=2;i<1;i++)#5;q[0]=1;end",
            "integer i;initial begin #1;for(i=-1;i>32'd1;i--)#5;q[0]=1;end",
        ] {
            let source = format!(
                "module Child(output bit[1:0]q);{declaration_and_loop} initial begin #2;q[1]=1;end endmodule module Top;wire[1:0]q;Child child(.q(q));initial forever @(q[0]^q[1]);endmodule"
            );
            let result = Simulator::from_sv_sources(
                vec![(source.as_str(), std::path::Path::new("for-types.sv"))],
                "Top",
            )
            .build_interpreter();
            assert!(
                result.is_err(),
                "must not discard a potentially suspending loop: {declaration_and_loop}"
            );
        }
    }
    #[test]
    fn constant_if_preserves_only_the_selected_branch_times() {
        for four_state in [false, true] {
            for (condition, selected_then) in [
                ("1", true),
                ("0", false),
                ("128'h80000000000000000000000000000000", true),
                ("4'bx001", true),
                ("1'bx", false),
            ] {
                for skipped in ["#5;", "begin #7;q[0]=0;end", "skipped();"] {
                    let selected = "q[0]=1;";
                    let (then_body, else_body) = if selected_then {
                        (selected, skipped)
                    } else {
                        (skipped, selected)
                    };
                    let source = format!(
                        "module Child(output bit[1:0]q);task skipped;#7;q[0]=0;endtask initial begin #1;if({condition}){then_body} else {else_body} end initial begin #2;q[1]=1;end endmodule module Top(output bit[7:0]seen);wire[1:0]q;Child child(.q(q));initial forever begin @(q[0]^q[1]);seen=seen+1;end endmodule"
                    );
                    fn check<B: SimBackend>(mut sim: Simulation<B>) {
                        sim.run_until(1).unwrap();
                        assert_eq!(sim.get(sim.signal("seen")), 1u8.into());
                        sim.run_until(2).unwrap();
                        assert_eq!(sim.get(sim.signal("seen")), 2u8.into());
                    }
                    macro_rules! build {
                        ($method:ident) => { check(Simulation::new(Simulator::from_sv_sources(vec![(source.as_str(), std::path::Path::new("constant-if.sv"))], "Top").four_state(four_state).$method().unwrap_or_else(|error| panic!("condition={condition}, skipped={skipped}, four_state={four_state}: {error:?}")))); };
                    }
                    build!(build_interpreter);
                    build!(build_cranelift);
                    build!(build_wasm);
                    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
                    build!(build_native);
                    let coincident = source.replace("#2;q[1]", "#1;q[1]");
                    assert!(
                        Simulator::from_sv_sources(
                            vec![(
                                coincident.as_str(),
                                std::path::Path::new("coincident-if.sv")
                            )],
                            "Top"
                        )
                        .four_state(four_state)
                        .build_interpreter()
                        .is_err()
                    );
                }
            }
        }
    }

    #[test]
    fn constant_case_preserves_only_the_selected_branch_times() {
        // These exercise Celox's conservative cross-instance write guard:
        // separated writes must compile; coincident writes must still fail.
        for four_state in [false, true] {
            for control in [
                "case(1'b1) 1'b1: SELECTED default: SKIPPED endcase",
                "case(1) 1: SELECTED 1: SKIPPED endcase",
                "case(2) 1: SKIPPED default: SELECTED endcase",
                "case(2) 0,2: SELECTED default: SKIPPED endcase",
                "case(128'h80000000000000000000000000000000) 128'h80000000000000000000000000000000: SELECTED default: SKIPPED endcase",
                "case(2'bx1) 2'bx1: SELECTED default: SKIPPED endcase",
                "casez(2'b10) 2'b1?: SELECTED default: SKIPPED endcase",
                "casex(2'b00) 2'bx0: SELECTED default: SKIPPED endcase",
                "case(3) inside [2:4]: SELECTED default: SKIPPED endcase",
                "case(2) 1: SKIPPED endcase SELECTED",
            ] {
                for skipped in ["#5;", "begin #7;q[0]=0;end", "skipped();"] {
                    let control = control
                        .replace("SELECTED", "q[0]=1;")
                        .replace("SKIPPED", skipped);
                    let source = format!(
                        "module Child(output bit[1:0]q);task skipped;#7;q[0]=0;endtask initial begin #1;{control} end initial begin #2;q[1]=1;end endmodule module Top(output bit[7:0]seen);wire[1:0]q;Child child(.q(q));initial forever begin @(q[0]^q[1]);seen=seen+1;end endmodule"
                    );
                    fn check<B: SimBackend>(
                        mut sim: Simulation<B>,
                        control: &str,
                        four_state: bool,
                    ) {
                        sim.run_until(1).unwrap();
                        assert_eq!(
                            sim.get(sim.signal("seen")),
                            1u8.into(),
                            "{control}, four_state={four_state}"
                        );
                        sim.run_until(2).unwrap();
                        assert_eq!(
                            sim.get(sim.signal("seen")),
                            2u8.into(),
                            "{control}, four_state={four_state}"
                        );
                    }
                    macro_rules! build {
                        ($method:ident) => {
                            check(
                                Simulation::new(
                                    Simulator::from_sv_sources(
                                        vec![(
                                            source.as_str(),
                                            std::path::Path::new("constant-case.sv"),
                                        )],
                                        "Top",
                                    )
                                    .four_state(four_state)
                                    .$method()
                                    .unwrap_or_else(|error| {
                                        panic!(
                                            "control={control}, four_state={four_state}: {error:?}"
                                        )
                                    }),
                                ),
                                &control,
                                four_state,
                            );
                        };
                    }
                    build!(build_interpreter);
                    build!(build_cranelift);
                    build!(build_wasm);
                    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
                    build!(build_native);
                    let coincident = source.replace("#2;q[1]", "#1;q[1]");
                    assert!(
                        Simulator::from_sv_sources(
                            vec![(
                                coincident.as_str(),
                                std::path::Path::new("coincident-case.sv")
                            )],
                            "Top"
                        )
                        .four_state(four_state)
                        .build_interpreter()
                        .is_err()
                    );
                }
            }
        }
    }

    #[test]
    fn selected_constant_case_delays_advance_the_fixed_time() {
        let source = "module Child(output bit[1:0]q);initial begin #1;case(1) 1: begin #2;q[0]=1;end default: #7;endcase end initial begin #4;q[1]=1;end endmodule module Top(output bit[7:0]seen);wire[1:0]q;Child child(.q(q));initial forever begin @(q[0]^q[1]);seen=seen+1;end endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(2).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), 0u8.into());
            sim.run_until(3).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), 1u8.into());
            sim.run_until(4).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), 2u8.into());
        }
        macro_rules! build {
            ($method:ident) => {
                check(Simulation::new(
                    Simulator::from_sv_sources(
                        vec![(source, std::path::Path::new("selected-case-delay.sv"))],
                        "Top",
                    )
                    .$method()
                    .unwrap(),
                ));
            };
        }
        build!(build_interpreter);
        build!(build_cranelift);
        build!(build_wasm);
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        build!(build_native);
        let coincident = source.replace("#4;q[1]", "#3;q[1]");
        assert!(
            Simulator::from_sv_sources(
                vec![(
                    coincident.as_str(),
                    std::path::Path::new("coincident-selected-case.sv")
                )],
                "Top"
            )
            .build_interpreter()
            .is_err()
        );
    }

    #[test]
    fn dynamic_case_choices_keep_conservative_write_times() {
        for control in [
            "case(select) 1: q[0]=1; default: #5; endcase",
            "case(1) select: q[0]=1; default: #5; endcase",
            "case(choose()) 1: q[0]=1; default: #5; endcase",
            "case(1) choose(): q[0]=1; default: #5; endcase",
        ] {
            let source = format!(
                "module Child(input bit select,output bit[1:0]q);function bit choose();q[0]=0;return 1;endfunction initial begin #1;{control} end initial begin #2;q[1]=1;end endmodule module Top(input bit select);wire[1:0]q;Child child(.select(select),.q(q));initial forever @(q[0]^q[1]);endmodule"
            );
            let result = Simulator::from_sv_sources(
                vec![(source.as_str(), std::path::Path::new("dynamic-case.sv"))],
                "Top",
            )
            .build_interpreter();
            let error = match result {
                Ok(_) => panic!("must preserve unknown/effectful case choices: {control}"),
                Err(error) => error.to_string(),
            };
            assert!(
                error.contains("multiple blocking writes"),
                "{control}: {error}"
            );
        }
    }

    #[test]
    fn single_pass_loops_preserve_fixed_write_times_and_parent_continuations() {
        for four_state in [false, true] {
            for control in [
                "forever",
                "while(1)",
                "repeat(5)",
                "do",
                "for(int i=0;1;i++)",
                "for(;1;q[0]=0)",
            ] {
                for (body, first) in [
                    ("q[0]=1;break;#9;q[0]=0;", 1u64),
                    ("if(4'bx001)begin q[0]=1;break;end skipped();", 1),
                    (
                        "case(1)1:begin q[0]=1;break;end default:skipped();endcase skipped();",
                        1,
                    ),
                    ("repeat(0)begin continue;end q[0]=1;break;", 1),
                    ("forever begin break;end q[0]=1;break;", 1),
                    ("#2;q[0]=1;break;skipped();", 3),
                ] {
                    let suffix = if control == "do" { "while(1);" } else { "" };
                    let second = first + 1;
                    let source = format!(
                        "module Child(output bit[1:0]q);task skipped;#7;q[0]=0;endtask initial begin #1;{control} begin {body} end {suffix} #2;q[0]=0;end initial begin #{second};q[1]=1;end endmodule module Top(output bit[7:0]seen);wire[1:0]q;Child child(.q(q));initial forever begin @(q[0]^q[1]);seen=seen+1;end endmodule"
                    );
                    fn check<B: SimBackend>(mut sim: Simulation<B>, first: u64) {
                        sim.run_until(first - 1).unwrap();
                        assert_eq!(sim.get(sim.signal("seen")), 0u8.into());
                        sim.run_until(first).unwrap();
                        assert_eq!(sim.get(sim.signal("seen")), 1u8.into());
                        sim.run_until(first + 1).unwrap();
                        assert_eq!(sim.get(sim.signal("seen")), 2u8.into());
                        sim.run_until(first + 2).unwrap();
                        assert_eq!(sim.get(sim.signal("seen")), 3u8.into());
                    }
                    macro_rules! build {
                        ($method:ident) => {
                            check(
                                Simulation::new(
                                    Simulator::from_sv_sources(
                                        vec![(
                                            source.as_str(),
                                            std::path::Path::new("single-pass.sv"),
                                        )],
                                        "Top",
                                    )
                                    .four_state(four_state)
                                    .$method()
                                    .unwrap_or_else(|error| {
                                        panic!(
                                            "{control}, {body}, four_state={four_state}: {error:?}"
                                        )
                                    }),
                                ),
                                first,
                            );
                        };
                    }
                    build!(build_interpreter);
                    build!(build_cranelift);
                    build!(build_wasm);
                    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
                    build!(build_native);
                    let coincident =
                        source.replace(&format!("#{second};q[1]"), &format!("#{first};q[1]"));
                    assert!(
                        Simulator::from_sv_sources(
                            vec![(
                                coincident.as_str(),
                                std::path::Path::new("coincident-single-pass.sv")
                            )],
                            "Top"
                        )
                        .four_state(four_state)
                        .build_interpreter()
                        .is_err()
                    );
                }
            }
        }
    }

    #[test]
    fn conditional_jumps_and_nested_breaks_keep_loop_timing_conservative() {
        for body in [
            "for(q[0]=1;1;)begin q[0]=0;break;end break;",
            "while(select)begin #5;q[0]=1;break;end q[0]=0;break;",
            "q[0]=~q[0];continue;break;",
            "q[0]=~q[0];if(select)continue;break;",
            "if(select)break;#5;q[0]=1;break;",
            "forever begin q[0]=1;break;end #1;q[0]=0;break;",
        ] {
            let source = format!(
                "module Child(input bit select,output bit[1:0]q);initial begin #1;forever begin {body} end end initial begin #2;q[1]=1;end endmodule module Top(input bit select);wire[1:0]q;Child child(.select(select),.q(q));initial forever @(q[0]^q[1]);endmodule"
            );
            let result = Simulator::from_sv_sources(
                vec![(source.as_str(), std::path::Path::new("live-loop.sv"))],
                "Top",
            )
            .build_interpreter();
            let error = match result {
                Ok(_) => panic!("must preserve loop control flow: {body}"),
                Err(error) => error.to_string(),
            };
            assert!(
                error.contains("multiple blocking writes"),
                "{body}: {error}"
            );
        }
    }

    #[test]
    fn selected_constant_if_delays_advance_the_fixed_time() {
        let source = "module Child(output bit[1:0]q);initial begin #1;if(1)begin #2;q[0]=1;end else #7;end initial begin #4;q[1]=1;end endmodule module Top(output bit[7:0]seen);wire[1:0]q;Child child(.q(q));initial forever begin @(q[0]^q[1]);seen=seen+1;end endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(2).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), 0u8.into());
            sim.run_until(3).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), 1u8.into());
            sim.run_until(4).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), 2u8.into());
        }
        macro_rules! build {
            ($method:ident) => {
                check(Simulation::new(
                    Simulator::from_sv_sources(
                        vec![(source, std::path::Path::new("selected-delay.sv"))],
                        "Top",
                    )
                    .$method()
                    .unwrap(),
                ));
            };
        }
        build!(build_interpreter);
        build!(build_cranelift);
        build!(build_wasm);
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        build!(build_native);
    }

    fn check_zero_trip_loop(control: &str) {
        for body in ["q[0]=1;", "begin #7;q[0]=1;end", "begin skipped();end"] {
            let source = format!(
                "module Child(output bit[1:0]q);task skipped;#7;q[0]=1;endtask initial begin #1;{control}{body} q[0]=1;end initial begin #2;q[1]=1;end endmodule module Top(output bit[7:0]seen);wire[1:0]q;Child child(.q(q));initial forever begin @(q[0]^q[1]);seen=seen+1;end endmodule"
            );
            fn check<B: SimBackend>(mut sim: Simulation<B>) {
                sim.run_until(1).unwrap();
                assert_eq!(sim.get(sim.signal("seen")), 1u8.into());
                sim.run_until(2).unwrap();
                assert_eq!(sim.get(sim.signal("seen")), 2u8.into());
            }
            let build = || {
                Simulator::from_sv_sources(
                    vec![(source.as_str(), std::path::Path::new("while.sv"))],
                    "Top",
                )
            };
            check(Simulation::new(build().build_interpreter().unwrap()));
            check(Simulation::new(build().build_cranelift().unwrap()));
            check(Simulation::new(build().build_wasm().unwrap()));
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check(Simulation::new(build().build_native().unwrap()));
            let coincident = source.replace("#2;q[1]", "#1;q[1]");
            assert!(
                Simulator::from_sv_sources(
                    vec![(
                        coincident.as_str(),
                        std::path::Path::new("coincident-while.sv")
                    )],
                    "Top"
                )
                .build_interpreter()
                .is_err()
            );
        }
    }
    struct DefaultFinish<B: SimBackend> {
        sim: Simulator<B>,
        combs: usize,
    }
    impl<B: SimBackend> SimulationExecutor for DefaultFinish<B> {
        type Backend = B;
        fn backend(&self) -> &B {
            &self.sim.backend
        }
        fn backend_mut(&mut self) -> &mut B {
            &mut self.sim.backend
        }
        fn eval_comb(&mut self) -> Result<(), RuntimeErrorCode> {
            self.combs += 1;
            self.sim.eval_comb_checked()
        }
        fn eval_apply_ff_at(&mut self, e: B::Event) -> Result<(), RuntimeErrorCode> {
            self.sim.eval_comb_apply_ff_at_checked(e)
        }
        fn eval_only_ff_at(&mut self, e: B::Event) -> Result<(), RuntimeErrorCode> {
            self.sim.eval_only_ff_at_checked(e)
        }
        fn apply_ff_at(&mut self, e: B::Event) -> Result<(), RuntimeErrorCode> {
            self.sim.apply_ff_at_checked(e)
        }
    }
    #[test]
    fn finish_status_stops_executors_without_a_finish_event_hook() {
        let source = "module Top(input bit clk,output bit q,dummy);assign q=clk;initial $finish;always @(posedge clk)dummy<=1;endmodule";
        fn check<B: SimBackend>(sim: Simulator<B>) {
            let mut state = simulation_state(&sim);
            state.schedule(sim.event("clk"), sim.signal("clk"), 0, 1);
            let mut executor = DefaultFinish { sim, combs: 0 };
            assert_eq!(state.step(&mut executor).unwrap(), Some(0));
            assert!(state.is_finished());
            assert_eq!(executor.combs, 0);
            assert_eq!(
                executor
                    .sim
                    .backend
                    .get_as::<u8>(executor.sim.signal("dummy")),
                0
            );
        }
        let build = || {
            Simulator::from_sv_sources(
                vec![(source, std::path::Path::new("default-finish.sv"))],
                "Top",
            )
        };
        check(build().build_interpreter().unwrap());
        check(build().build_cranelift().unwrap());
        check(build().build_wasm().unwrap());
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(build().build_native().unwrap());
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod level_wait_scheduling_review {
    use super::*;
    #[test]
    fn level_waiters_run_before_an_inactive_clear() {
        let source = "module Top(output bit a,b);bit go;initial begin wait(go);a=1;end initial begin wait(go);b=1;end initial begin #1;go=1;#0;go=0;end endmodule";
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(2).unwrap();
            assert_eq!(sim.get(sim.signal("a")), 1u8.into());
            assert_eq!(sim.get(sim.signal("b")), 1u8.into());
        }
        let build = || {
            Simulator::from_sv_sources(
                vec![(source, std::path::Path::new("stable-wait.sv"))],
                "Top",
            )
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod sampled_host_review {
    use super::*;
    fn check_count<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(0).unwrap();
        let clk = sim.signal("clk");
        for value in [1u8, 0, 1] {
            sim.modify(|io| io.set(clk, value)).unwrap();
            sim.eval_comb().unwrap();
        }
        let checkpoint = sim.checkpoint().unwrap();
        let saved = sim.save_state().unwrap();
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("count")), 2u8.into());
        sim.load_state(&saved).unwrap();
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("count")), 2u8.into());
        sim.restore(&checkpoint).unwrap();
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("count")), 2u8.into());
    }
    fn check_ready<B: SimBackend>(mut sim: Simulation<B>) {
        sim.run_until(0).unwrap();
        let go = sim.signal("go");
        sim.modify(|io| io.set(go, 1u8)).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 0u8.into());
        assert_eq!(sim.next_event_time(), Some(0));
        let checkpoint = sim.checkpoint().unwrap();
        let parts = sim.state.export_schedule(&sim.simulator.backend);
        sim.state.import_schedule(parts);
        assert_eq!(sim.next_event_time(), Some(0));
        sim.run_until(0).unwrap();
        sim.restore(&checkpoint).unwrap();
        assert_eq!(sim.next_event_time(), Some(0));
        sim.run_until(0).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 1u8.into());
        assert_eq!(sim.next_event_time(), None);
    }
    macro_rules! check_all {
        ($source:expr, $check:ident) => {{
            let build = || {
                Simulator::from_sv_sources(
                    vec![($source, std::path::Path::new("host-capture.sv"))],
                    "Top",
                )
            };
            $check(Simulation::new(build().build_interpreter().unwrap()));
            $check(Simulation::new(build().build_cranelift().unwrap()));
            $check(Simulation::new(build().build_wasm().unwrap()));
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            $check(Simulation::new(build().build_native().unwrap()));
        }};
    }
    #[test]
    fn host_evaluations_preserve_repeated_ff_occurrences_and_checkpoints() {
        check_all!(
            "module Top(input bit clk,output bit[7:0]count);always @(posedge clk)count=count+1;endmodule",
            check_count
        );
    }
    #[test]
    fn signed_negative_repeat_counts_do_not_enter_the_body() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(2).unwrap();
            assert_eq!(sim.get(sim.signal("q")), 0u8.into());
        }
        for declaration in [
            "integer n=-1",
            "integer n=-2",
            "logic signed[127:0]n=128'sh80000000000000000000000000000001",
        ] {
            let source = format!(
                "module Top(output bit q);{declaration};initial begin #1;repeat(n)begin q=1;break;end end endmodule"
            );
            check_all!(source.as_str(), check);
        }
    }
    #[test]
    fn scheduled_batches_preserve_repeated_derived_ff_occurrences() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            // The inverted clock rises once when its initial value settles.
            assert_eq!(sim.get(sim.signal("falls")), 1u8.into());
            assert_eq!(sim.get(sim.signal("rises")), 0u8.into());
            for name in ["a", "b", "c"] {
                sim.schedule(name, 1, 1).unwrap();
            }
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("rises")), 2u8.into());
            assert_eq!(sim.get(sim.signal("falls")), 2u8.into());
            assert_eq!(sim.get(sim.signal("aliased")), 2u8.into());
            for name in ["a", "b", "c"] {
                sim.schedule(name, 2, 0).unwrap();
            }
            sim.run_until(2).unwrap();
            assert_eq!(sim.get(sim.signal("rises")), 3u8.into());
            assert_eq!(sim.get(sim.signal("falls")), 4u8.into());
            let a = sim.signal("a");
            sim.modify(|io| io.set(a, 1u8)).unwrap();
            sim.eval_comb().unwrap();
            for name in ["b", "c"] {
                sim.schedule(name, 2, 1).unwrap();
            }
            sim.run_until(2).unwrap();
            assert_eq!(sim.get(sim.signal("rises")), 5u8.into());
            assert_eq!(sim.get(sim.signal("falls")), 5u8.into());
            assert_eq!(sim.get(sim.signal("aliased")), 5u8.into());
        }
        check_all!(
            "module Top(input bit a,b,c,output bit[7:0]rises,falls,aliased,ac,bc,cc);always @(posedge a)ac=ac+1;always @(posedge b)bc=bc+1;always @(posedge c)cc=cc+1;wire derived=a^b^c;wire mirror=derived;wire falling=~derived;always @(posedge derived)rises=rises+1;always @(posedge falling)falls=falls+1;always @(posedge mirror)aliased=aliased+1;endmodule",
            check
        );
    }
    #[test]
    fn host_readiness_probes_do_not_execute_wait_continuations() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            let q = sim.signal("q");
            let go = sim.signal("go");
            let noise = sim.signal("noise");
            sim.modify(|io| io.set(noise, 1u8)).unwrap();
            assert!(!sim.has_pending_host_work().unwrap());
            assert_eq!(sim.get(q), 0u8.into());
            sim.modify(|io| io.set(go, 1u8)).unwrap();
            for _ in 0..2 {
                assert!(sim.has_pending_host_work().unwrap());
                assert_eq!(sim.get(q), 0u8.into());
                assert!(!sim.is_finished());
                assert_eq!(sim.time(), 0);
            }
            sim.run_until(0).unwrap();
            assert_eq!(sim.get(q), 1u8.into());
            assert!(sim.is_finished());
            assert!(!sim.has_pending_host_work().unwrap());
        }
        for wait in ["@(posedge go)", "wait(go)"] {
            let source = format!(
                "module Top(input bit go,noise,output bit q);initial begin {wait};q=1;$finish;end endmodule"
            );
            check_all!(source.as_str(), check);
        }
    }

    #[test]
    fn readiness_probes_recheck_level_conditions_without_latching_them() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            let go = sim.signal("go");
            let q = sim.signal("q");
            sim.modify(|io| io.set(go, 1u8)).unwrap();
            assert!(sim.has_pending_host_work().unwrap());
            sim.modify(|io| io.set(go, 0u8)).unwrap();
            assert!(!sim.has_pending_host_work().unwrap());
            sim.run_until(0).unwrap();
            assert_eq!(sim.get(q), 0u8.into());
            assert!(!sim.is_finished());
        }
        check_all!(
            "module Top(input bit go,output bit q);initial begin wait(go);q=1;$finish;end endmodule",
            check
        );
    }

    #[test]
    fn process_counter_deltas_preserve_repeated_ff_occurrences() {
        fn check<B: SimBackend>(mut sim: Simulation<B>, queued: bool, value: u64, driver: &str) {
            sim.run_until(0).unwrap();
            if queued {
                sim.schedule("d", 1, value).unwrap();
            }
            sim.schedule("clk", 1, 1).unwrap();
            sim.run_until(1).unwrap();
            let expected = if queued { 3u8 } else { 2u8 };
            assert_eq!(
                sim.get(sim.signal("count")),
                expected.into(),
                "queued={queued}, driver={driver}"
            );
            assert_eq!(sim.get(sim.signal("clock_count")), 1u8.into());
        }
        for (edge, initial, value) in [("posedge", 0, 1), ("negedge", 1, 0)] {
            let writes = format!("d={initial};d={value};d={initial};d={value};");
            for driver in [
                format!("initial begin #1;{writes}end"),
                format!("initial begin #1;#0;{writes}end"),
                format!("always @(posedge clk)begin {writes}end"),
                format!(
                    "bit wake;always @(posedge clk)wake<=1;initial begin wait(wake);{writes}end"
                ),
            ] {
                let source = format!(
                    "module Top(input bit clk,output bit[7:0]count,clock_count);bit d={initial};always @(posedge clk)clock_count=clock_count+1;always @({edge} d)count=count+1;initial @({edge} d);{driver} endmodule"
                );
                for queued in [false, true] {
                    macro_rules! build {
                        ($method:ident) => {
                            check(
                                Simulation::new(
                                    Simulator::from_sv_sources(
                                        vec![(
                                            source.as_str(),
                                            std::path::Path::new("counter-delta.sv"),
                                        )],
                                        "Top",
                                    )
                                    .$method()
                                    .unwrap(),
                                ),
                                queued,
                                value,
                                &driver,
                            );
                        };
                    }
                    build!(build_interpreter);
                    build!(build_cranelift);
                    build!(build_wasm);
                    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
                    build!(build_native);
                }
            }
        }
    }

    #[test]
    fn sampled_host_waiters_appear_in_next_event_time() {
        check_all!(
            "module Top(input bit go,output bit q);initial @(posedge go)q=1;endmodule",
            check_ready
        );
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod host_finish_step_review {
    use super::*;

    #[test]
    fn host_dispatch_reports_finished_without_returning_a_time() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            let initial = sim.checkpoint().unwrap();
            let clk = sim.signal("clk");
            for bounded in [false, true] {
                sim.restore(&initial).unwrap();
                sim.modify(|io| io.set(clk, 1u8)).unwrap();
                let result = if bounded {
                    sim.step_until(10)
                } else {
                    sim.step()
                };
                assert_eq!(result.unwrap(), None);
                assert!(sim.is_finished());
                assert_eq!(sim.time(), 0);
                assert_eq!(sim.step().unwrap(), None);
            }
        }
        for body in [
            "initial @(posedge clk)$finish;",
            "always @(posedge clk)$finish;",
        ] {
            let source = format!("module Top(input bit clk);{body} endmodule");
            macro_rules! build {
                ($method:ident) => {
                    check(Simulation::new(
                        Simulator::from_sv_sources(
                            vec![(source.as_str(), std::path::Path::new("host-finish.sv"))],
                            "Top",
                        )
                        .$method()
                        .unwrap(),
                    ));
                };
            }
            build!(build_interpreter);
            build!(build_cranelift);
            build!(build_wasm);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            build!(build_native);
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod round57_review_tests {
    use super::*;
    fn check_backends(
        source: &str,
        check: impl Fn(&mut dyn FnMut(u64) -> Result<Vec<u64>, String>),
    ) {
        fn run<B: SimBackend>(
            mut sim: Simulation<B>,
            check: &impl Fn(&mut dyn FnMut(u64) -> Result<Vec<u64>, String>),
        ) {
            let q = sim.signal("q");
            let mut advance = |time| {
                sim.run_until(time).map_err(|e| e.to_string())?;
                Ok(vec![
                    sim.get(q).to_u64_digits().first().copied().unwrap_or(0),
                ])
            };
            check(&mut advance);
        }
        let build = || {
            Simulator::from_sv_sources(vec![(source, std::path::Path::new("review57.sv"))], "Top")
        };
        run(
            Simulation::new(build().build_interpreter().unwrap()),
            &check,
        );
        run(Simulation::new(build().build_cranelift().unwrap()), &check);
        run(Simulation::new(build().build_wasm().unwrap()), &check);
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        run(Simulation::new(build().build_native().unwrap()), &check);
    }
    #[test]
    fn round61_constant_exits_and_four_state_truth_preserve_single_child_writes() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("q")), 1u8.into());
        }
        for four_state in [false, true] {
            for action in [
                "repeat(2) begin v=~v;if(1'b1)break;end",
                "repeat(2) begin v=~v;case(1)1:break;default:;endcase end",
                "if(4'bx001)v=~v;else begin v=0;v=1;end",
                "if(4'bz001)v=~v;else begin v=0;v=1;end",
                "if(4'b000x)begin v=0;v=1;end else v=~v;",
            ] {
                let source = format!(
                    "module Child(input bit clk,output bit v);always @(posedge clk) begin {action} end endmodule module Top(output bit[7:0]q);bit clk;wire v;Child c(clk,v);initial forever begin @(v);q++;end initial begin #1;clk=1;end endmodule"
                );
                let build = || {
                    Simulator::from_sv_sources(
                        vec![(&source, std::path::Path::new("truth-exit.sv"))],
                        "Top",
                    )
                    .four_state(four_state)
                };
                check(Simulation::new(build().build_interpreter().unwrap()));
                check(Simulation::new(build().build_cranelift().unwrap()));
                check(Simulation::new(build().build_wasm().unwrap()));
                #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
                check(Simulation::new(build().build_native().unwrap()));
            }
        }
    }
    #[test]
    fn round61_unknown_or_unreachable_loop_exits_keep_pulse_guard() {
        for action in [
            "repeat(2)begin v=~v;if(0)break;end",
            "repeat(2)begin v=~v;if(select)break;end",
            "repeat(2)begin v=~v;case(0)1:break;default:;endcase end",
            "repeat(2)begin v=~v;case(select)1:break;default:;endcase end",
            "repeat(2)begin v=~v;repeat(1)break;end",
        ] {
            let source = format!(
                "module Child(input bit clk,select,output bit v);always @(posedge clk)begin {action} end endmodule module Top(input bit clk,select,output bit[7:0]q);wire v;Child c(clk,select,v);initial forever begin @(v);q++;end endmodule"
            );
            for four_state in [false, true] {
                let error = match Simulator::from_sv_sources(
                    vec![(&source, std::path::Path::new("loop-exit-guard.sv"))],
                    "Top",
                )
                .four_state(four_state)
                .build_interpreter()
                {
                    Ok(_) => panic!("multiple writes must remain rejected"),
                    Err(error) => error.to_string(),
                };
                assert!(
                    error.contains("multiple blocking writes"),
                    "{action}: {error}"
                );
            }
        }
    }
    #[test]
    fn round60_static_if_excludes_unreachable_comb_pulses() {
        let exact = "module Top(output bit q);logic v;always_comb if(1'b1)v=0;else begin v=0;v=1;end initial @(v)q=1;endmodule";
        check_backends(exact, |advance| {
            advance(0).unwrap();
        });
        for condition in ["1'b1", "2'b10", "1'b0"] {
            let (then_body, else_body) = if condition == "1'b0" {
                ("v=0;v=1;", "v=go;")
            } else {
                ("v=go;", "v=0;v=1;")
            };
            let source = format!(
                "module Top(output bit[7:0]q);bit go,v;always_comb if({condition})begin {then_body} end else begin {else_body} end initial forever begin @(v);q++;end initial begin #1;go=1;end endmodule"
            );
            check_backends(&source, |advance| assert_eq!(advance(1).unwrap(), vec![1]));
        }
        for condition in ["go", "1'b1"] {
            let source = format!(
                "module Top(input bit go,output bit q);bit v;always_comb if({condition})begin v=0;v=1;end else v=go;initial @(v)q=1;endmodule"
            );
            assert!(
                Simulator::from_sv_sources(
                    vec![(&source, std::path::Path::new("pulse.sv"))],
                    "Top"
                )
                .build_interpreter()
                .is_err()
            );
        }
    }
    #[test]
    fn round60_static_case_excludes_unreachable_child_ff_pulses() {
        for (kind, selector, label) in [
            ("case", "2'd1", "2'd1"),
            ("casez", "2'bz1", "2'b01"),
            ("casex", "2'bx1", "2'b01"),
        ] {
            let source = format!(
                "module Child(input bit clk,output bit v);always @(posedge clk) {kind}({selector}) {label}:v<=~v;default:begin v<=0;v<=1;end endcase endmodule module Top(output bit[7:0]q);bit clk;wire v;Child c(clk,v);initial forever begin @(v);q++;end initial begin #1;clk=1;end endmodule"
            );
            check_backends(&source, |advance| assert_eq!(advance(1).unwrap(), vec![1]));
        }
        let source = "module Child(input bit clk,sel,output bit v);always @(posedge clk) case(sel) 0:v<=~v;default:begin v<=0;v<=1;end endcase endmodule module Top(input bit clk,sel,output bit q);wire v;Child c(clk,sel,v);initial @(v)q=1;endmodule";
        assert!(
            Simulator::from_sv_sources(vec![(source, std::path::Path::new("pulse.sv"))], "Top")
                .build_interpreter()
                .is_err()
        );
    }
    #[test]
    fn repeated_active_edges_share_one_nba_batch() {
        for (assignment, expected) in [("<=", 1), ("=", 2)] {
            let source = format!(
                "module Top(output bit[7:0]q);bit d;initial begin #1;d=1;d=0;d=1;end always @(posedge d) q {assignment} q+1;endmodule"
            );
            check_backends(&source, |advance| {
                assert_eq!(advance(1).unwrap(), vec![expected]);
            });
        }
    }
    #[test]
    fn false_wait_retains_earlier_write_times() {
        let source = "module Child(output bit[1:0]v);initial begin #1;v[0]=1;wait(0);end initial begin #2;v[1]=1;end endmodule module Top(output bit[7:0]q);wire[1:0]v;Child c(.v(v));initial forever begin @(v[0]^v[1]);q++;end endmodule";
        check_backends(source, |advance| {
            assert_eq!(advance(1).unwrap(), vec![1]);
            assert_eq!(advance(2).unwrap(), vec![2]);
        });
    }
    #[test]
    fn always_with_only_timed_task_call_runs() {
        let source = "module Top(output bit q);task tick;#1;q=~q;endtask always tick();endmodule";
        check_backends(source, |advance| {
            assert_eq!(advance(1).unwrap(), vec![1]);
            assert_eq!(advance(2).unwrap(), vec![0]);
            assert_eq!(advance(3).unwrap(), vec![1]);
        });
    }
}

#[cfg(all(test, feature = "host-runtime"))]
mod round57_sdk_tests {
    use super::*;
    use celox_frontend_sdk::{Constant, ModuleBuilder, Statement};
    #[test]
    fn sdk_zero_delay_forever_exhausts_time_slot_budget() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            let error = sim.run_until(0).unwrap_err().to_string();
            assert!(error.contains("process delta limit exceeded"), "{error}");
            assert_eq!(sim.time(), 0);
        }
        let build = || {
            let mut module = ModuleBuilder::new("Top").unwrap();
            let zero = module.constant(Constant::two_state(0u8, 1).unwrap());
            module
                .process(vec![Statement::Forever {
                    body: vec![Statement::Delay { amount: zero }],
                }])
                .unwrap();
            Simulator::from_frontend(module.finish())
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
}

#[cfg(test)]
mod clean_waiter_settle_review {
    use super::*;
    use crate::frontend_sdk::{BinaryOp, Constant, Edge, ModuleBuilder, Statement, ValueType};
    use celox_frontend_core::process::ProcessKernelBuilder;
    use celox_sir::{SIRInstruction, SIROffset, SIRValue};

    fn program() -> crate::ir::LaidOutProgram {
        program_with_sampling_waiter(false)
    }
    fn program_with_sampling_waiter(extra: bool) -> crate::ir::LaidOutProgram {
        let mut m = ModuleBuilder::new("Top").unwrap();
        let bit = ValueType::bits(1).unwrap();
        let word = ValueType::bits(8).unwrap();
        let go = m.input("go", bit).unwrap();
        let clk = m.internal("clk", bit).unwrap();
        let q = m.output("q", word).unwrap();
        let done = m.output("done", bit).unwrap();
        let old = m.read(q).unwrap();
        let input = m.read(go).unwrap();
        let next = m.binary(BinaryOp::Add, old, input, word).unwrap();
        m.register(m.whole(q).unwrap(), next, clk, Edge::Posedge, None, None)
            .unwrap();
        m.clock_period(clk, 4).unwrap();
        let count = m.constant(Constant::two_state(4u8, 64).unwrap());
        let one = m.constant(Constant::two_state(1u8, 1).unwrap());
        m.process(vec![
            Statement::ClockCycles { clock: clk, count },
            Statement::Assign {
                target: m.whole(done).unwrap(),
                value: one,
            },
        ])
        .unwrap();
        let noise = if extra {
            let noise = m.input("noise", bit).unwrap();
            let echo = m.output("noise_seen", bit).unwrap();
            let value = m.read(noise).unwrap();
            m.assign(m.whole(echo).unwrap(), value).unwrap();
            let falls = m.output("falls", word).unwrap();
            let old = m.read(falls).unwrap();
            let one = m.constant(Constant::two_state(1u8, 8).unwrap());
            let next = m.binary(BinaryOp::Add, old, one, word).unwrap();
            let fallclk = m.internal("fallclk", bit).unwrap();
            let clk_value = m.read(clk).unwrap();
            let go_value = m.read(go).unwrap();
            let low = m
                .unary(crate::frontend_sdk::UnaryOp::BitNot, clk_value, bit)
                .unwrap();
            let active_low = m.binary(BinaryOp::And, low, go_value, bit).unwrap();
            m.assign(m.whole(fallclk).unwrap(), active_low).unwrap();
            m.register(
                m.whole(falls).unwrap(),
                next,
                fallclk,
                Edge::Posedge,
                None,
                None,
            )
            .unwrap();
            m.process(vec![Statement::Finish]).unwrap();
            Some(noise)
        } else {
            None
        };
        let (mut program, _) = crate::compile_frontend_to_sir(
            &m.finish(),
            &[],
            &[],
            false,
            &Default::default(),
            None,
            &Default::default(),
        )
        .unwrap();
        let address = |id| {
            *program
                .design
                .state_objects
                .keys()
                .find(|a| {
                    program
                        .design
                        .variable(a)
                        .is_some_and(|v| v.source_id == crate::ir::SourceVarId(id))
                })
                .unwrap()
        };
        let go = address(noise.unwrap_or(go).index());
        let done = address(done.index());
        let mut addresses = Vec::new();
        let mut intern = |address| {
            let index = match addresses.iter().position(|a| *a == address) {
                Some(index) => index,
                None => {
                    addresses.push(address);
                    addresses.len() - 1
                }
            };
            crate::ir::SourceVarId(index as u32)
        };
        let index = usize::from(extra);
        let slots = program.runtime_schema.processes[index]
            .clone()
            .map(&mut intern);
        let go = intern(go);
        let done = intern(done);
        let stable = |var_id| celox_design::RegionedVarAddrBase {
            region: celox_design::STABLE_REGION,
            var_id,
        };
        // The SDK has clock waits but no level-wait statement. Build the
        // combined kernel through the shared frontend process contract.
        let mut kernel = ProcessKernelBuilder::new(slots);
        kernel.begin_wait().unwrap();
        let condition = kernel.builder().alloc_bit(1, false);
        kernel.builder().emit(SIRInstruction::Load(
            condition,
            stable(go),
            SIROffset::Static(0),
            1,
        ));
        kernel.wake_if(condition);
        if !extra {
            let count = kernel.builder().alloc_bit(64, false);
            kernel
                .builder()
                .emit(SIRInstruction::Imm(count, SIRValue::new(4u8)));
            kernel.wait_clock(0, count).unwrap();
            let one = kernel.builder().alloc_bit(1, false);
            kernel
                .builder()
                .emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
            kernel.builder().emit(SIRInstruction::Store(
                stable(done),
                SIROffset::Static(0),
                1,
                one,
                Vec::new(),
                Vec::new(),
            ));
        }
        kernel.finish();
        program.sir.processes[index] = kernel.build().into_map_addr(&mut |addr| {
            let address = addresses[addr.var_id.0 as usize];
            crate::ir::RegionedAbsoluteAddr {
                region: addr.region,
                instance_id: address.instance_id,
                var_id: address.var_id,
            }
        });
        program.into_laid_out(false)
    }
    struct SamplingFailure<B: SimBackend> {
        sim: Simulator<B>,
        fail: bool,
        low_samples: usize,
    }
    impl<B: SimBackend> SimulationExecutor for SamplingFailure<B> {
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
        fn eval_apply_ff_at(&mut self, e: B::Event) -> Result<(), RuntimeErrorCode> {
            SimulationExecutor::eval_apply_ff_at(&mut self.sim, e)
        }
        fn eval_only_ff_at(&mut self, e: B::Event) -> Result<(), RuntimeErrorCode> {
            SimulationExecutor::eval_only_ff_at(&mut self.sim, e)
        }
        fn apply_ff_at(&mut self, e: B::Event) -> Result<(), RuntimeErrorCode> {
            SimulationExecutor::apply_ff_at(&mut self.sim, e)
        }
        fn run_process(&mut self, index: usize) -> Result<(), RuntimeErrorCode> {
            let slots = &self.sim.program.runtime_schema.processes[index];
            let mode = self
                .sim
                .backend
                .get_as::<u64>(self.sim.backend.resolve_signal(&slots.delay));
            let clk = self.sim.backend.get_as::<u8>(self.sim.signal("clk"));
            if self.fail && index == 1 && mode == 1 && clk == 0 {
                self.low_samples += 1;
                // Skip sampling during scheduled-value application, then fail
                // the post-settle sample before the falling-edge FF is run.
                if self.low_samples == 2 {
                    return Err(RuntimeErrorCode::Runtime {
                        message: "injected wait sampling failure".into(),
                        signals: Vec::new(),
                    });
                }
            }
            SimulationExecutor::run_process(&mut self.sim, index)
        }
    }
    #[test]
    fn round61_failed_wait_sampling_retains_unpublished_generated_fall() {
        fn check<B: SimBackend>(mut sim: Simulator<B>) {
            sim.apply_initial_values();
            let go = sim.signal("go");
            sim.set_wide(go, 1u8.into());
            let mut state = simulation_state(&sim);
            let mut executor = SamplingFailure {
                sim,
                fail: false,
                low_samples: 0,
            };
            assert_eq!(state.step_until(&mut executor, 0).unwrap(), Some(0));
            executor.fail = true;
            let error = state.step_until(&mut executor, 2).unwrap_err().to_string();
            assert!(error.contains("injected wait sampling failure"), "{error}");
            assert_eq!(executor.low_samples, 2);
            assert_eq!(
                executor
                    .sim
                    .backend
                    .get_as::<u8>(executor.sim.signal("falls")),
                0
            );
            let schedule = state.export_schedule(executor.backend());
            assert_eq!(schedule.process_clocks[0].falling_edge, Some(2));
            assert_eq!(state.next_event_time(), Some(2));
            executor.fail = false;
            assert_eq!(state.step_until(&mut executor, 2).unwrap(), Some(2));
            assert_eq!(
                executor
                    .sim
                    .backend
                    .get_as::<u8>(executor.sim.signal("falls")),
                1
            );
        }
        let program = program_with_sampling_waiter(true);
        let options = crate::SimulatorOptions {
            emit_triggers: true,
            ..Default::default()
        };
        macro_rules! run {
            ($backend:expr) => {
                check(Simulator::with_backend_and_program(
                    $backend.unwrap(),
                    program.runtime().clone(),
                    Vec::new(),
                ));
            };
        }
        run!(crate::backend::InterpBackend::new(&program, &options));
        run!(crate::backend::JitBackend::new(&program, &options, None));
        run!(crate::backend::wasm_runtime::WasmBackend::new(
            &program, &options
        ));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        run!(crate::backend::native::NativeBackend::new(
            &program, &options
        ));
    }
    #[test]
    fn round61_zero_count_delta_errors_remain_retryable() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            for _ in 0..2 {
                let error = sim.step().unwrap_err().to_string();
                assert!(error.contains("delta limit"), "{error}");
                assert_eq!(sim.next_event_time(), Some(0));
                let saved = sim.checkpoint().unwrap();
                sim.restore(&saved).unwrap();
                let error = sim.run_until(0).unwrap_err().to_string();
                assert!(error.contains("delta limit"), "{error}");
                assert_eq!(sim.time(), 0);
                assert_eq!(sim.ticks(), 0);
            }
        }
        let build = || {
            let mut m = ModuleBuilder::new("Top").unwrap();
            let clk = m.input("clk", ValueType::bits(1).unwrap()).unwrap();
            let q = m.output("q", ValueType::bits(8).unwrap()).unwrap();
            let value = m.read(q).unwrap();
            m.register(m.whole(q).unwrap(), value, clk, Edge::Posedge, None, None)
                .unwrap();
            m.clock_period(clk, 4).unwrap();
            let zero = m.constant(Constant::two_state(0u8, 64).unwrap());
            m.process(vec![Statement::Forever {
                body: vec![Statement::ClockCycles {
                    clock: clk,
                    count: zero,
                }],
            }])
            .unwrap();
            Simulator::from_frontend(m.finish())
        };
        check(Simulation::new(build().build_interpreter().unwrap()));
        check(Simulation::new(build().build_cranelift().unwrap()));
        check(Simulation::new(build().build_wasm().unwrap()));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(Simulation::new(build().build_native().unwrap()));
    }
    fn check<B: SimBackend>(mut simulator: Simulator<B>) {
        simulator.apply_initial_values();
        let mut sim = Simulation::new(simulator);
        sim.run_until(0).unwrap();
        assert_eq!(sim.next_event_time(), None);
        let go = sim.signal("go");
        let (pointer, length) = sim.memory_as_mut_ptr();
        assert!(go.offset < length);
        unsafe {
            pointer.add(go.offset).write(1);
        }
        assert!(!sim.simulator.dirty);
        assert!(sim.simulator.pending_host_writes.is_empty());
        assert!(sim.simulator.backend.get_triggered_bits().is_empty());
        assert!(!sim.state.has_unsettled_edges(&sim.simulator.backend));
        sim.settle().unwrap();
        assert_eq!(sim.time(), 0);
        assert_eq!(sim.ticks(), 1);
        assert_eq!(sim.get(sim.signal("q")), 1u8.into());
        assert_eq!(sim.get(sim.signal("done")), 0u8.into());
        sim.run_until(16).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 4u8.into());
        assert_eq!(sim.get(sim.signal("done")), 1u8.into());
    }
    #[test]
    fn clean_raw_level_waiter_settle_keeps_current_time() {
        let program = program();
        let options = crate::SimulatorOptions {
            emit_triggers: true,
            ..Default::default()
        };
        macro_rules! run {
            ($backend:expr) => {
                check(Simulator::with_backend_and_program(
                    $backend.unwrap(),
                    program.runtime().clone(),
                    Vec::new(),
                ));
            };
        }
        run!(crate::backend::InterpBackend::new(&program, &options));
        run!(crate::backend::JitBackend::new(&program, &options, None));
        run!(crate::backend::wasm_runtime::WasmBackend::new(
            &program, &options
        ));
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        run!(crate::backend::native::NativeBackend::new(
            &program, &options
        ));
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod round62_review_tests {
    use super::*;
    macro_rules! backends {
        ($source:expr, $check:ident) => {{
            let build = || {
                Simulator::from_sv_sources(
                    vec![($source, std::path::Path::new("review62.sv"))],
                    "Top",
                )
            };
            $check(Simulation::new(build().build_interpreter().unwrap()));
            $check(Simulation::new(build().build_cranelift().unwrap()));
            $check(Simulation::new(build().build_wasm().unwrap()));
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            $check(Simulation::new(build().build_native().unwrap()));
        }};
    }
    #[test]
    fn selected_hierarchy_dependencies_skip_dead_reads() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(2).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), 0u8.into());
        }
        for branch in [
            "if(1)q=0;else q=a;",
            "case(1'b1)1'b1:q=0;default:q=a;endcase",
            "case(1'b1)1'b1:q=0;default:q=read_a();endcase",
        ] {
            let source = format!(
                "module Child(input bit a,output bit q);function bit read_a();return a;endfunction always_comb begin {branch} end endmodule module Top(output bit[7:0]seen);bit a;wire q;Child c(a,q);initial begin #1;a=1;a=0;end initial forever begin @(q);seen++;end endmodule"
            );
            backends!(&source, check);
        }
    }
    #[test]
    fn selected_loop_exit_discards_unreachable_suffix() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), 1u8.into());
        }
        for exit in [
            "if(1)break;",
            "case(1)1:break;default:;endcase",
            "if(1)continue;",
        ] {
            let repeat = if exit.contains("continue") { 1 } else { 2 };
            let source = format!(
                "module Child(input bit clk,output bit q);always @(posedge clk)repeat({repeat})begin q=~q;{exit} q=~q;end endmodule module Top(output bit[7:0]seen);bit clk;wire q;Child c(clk,q);initial begin #1;clk=1;end initial forever begin @(q);seen++;end endmodule"
            );
            backends!(&source, check);
        }
    }
    #[test]
    fn captured_host_edge_and_single_queued_edge_both_publish() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            let clk = sim.signal("clk");
            for value in [1u8, 0u8] {
                sim.modify(|io| io.set(clk, value)).unwrap();
                sim.eval_comb().unwrap();
            }
            sim.schedule("clk", 0, 1).unwrap();
            sim.run_until(0).unwrap();
            assert_eq!(sim.get(sim.signal("q")), 2u8.into());
        }
        backends!(
            "module Top(input bit clk,output bit[7:0]q);always @(posedge clk)q=q+1;endmodule",
            check
        );
    }
    #[test]
    fn next_event_time_exposes_unsampled_rust_host_write() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            assert_eq!(sim.next_event_time(), None);
            let clean = sim.checkpoint().unwrap();
            let go = sim.signal("go");
            sim.modify(|io| io.set(go, 1u8)).unwrap();
            let pending = sim.checkpoint().unwrap();
            sim.restore(&clean).unwrap();
            assert_eq!(sim.next_event_time(), None);
            sim.restore(&pending).unwrap();
            for _ in 0..2 {
                assert_eq!(sim.next_event_time(), Some(0));
                assert_eq!(sim.time(), 0);
            }
            assert_eq!(sim.step().unwrap(), Some(0));
            assert_eq!(sim.get(sim.signal("q")), 1u8.into());
            assert_eq!(sim.next_event_time(), None);
        }
        backends!(
            "module Top(input bit go,output bit q);initial begin @(posedge go);q=1;end endmodule",
            check
        );
    }
    #[test]
    fn budget_checks_resume_same_process_before_other_work() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            assert_eq!(sim.get(sim.signal("q")), 4u8.into());
            assert_eq!(sim.get(sim.signal("seen")), 4u8.into());
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("done")), 1u8.into());
        }
        backends!(
            "module Top(output bit[7:0]q,seen,output bit done);initial begin repeat(4)q++;#1;done=1;end initial seen=q;endmodule",
            check
        );
    }
    #[test]
    fn finish_survives_overwritten_runtime_event_records() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            let go = sim.signal("go");
            sim.modify(|io| io.set(go, 1u8)).unwrap();
            sim.run_until(1).unwrap();
            assert!(sim.is_finished(), "overflow lost the finish request");
            assert_eq!(sim.get(sim.signal("q")), 0u8.into());
            let events = sim.drain_runtime_events();
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, crate::RuntimeEvent::Missed { .. })),
                "must overflow the actual runtime ring"
            );
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, crate::RuntimeEvent::Finish)),
                "finish must have been overwritten to exercise the independent latch"
            );
        }
        backends!(
            "module Top(input bit go,output bit q);bit armed;always_comb begin armed=go;if(go)$finish;end always_comb if(armed)repeat(1030)$display(\"noise\");initial begin #1;q=1;end endmodule",
            check
        );
    }
    #[test]
    fn non_suspending_process_loops_report_delta_limit() {
        // Run the actual kernels in a child so a regression cannot hang the test runner.
        const CHILD: &str = "CELOX_ROUND62_KERNEL_CHILD";
        #[expect(
            clippy::disallowed_methods,
            reason = "isolated kernel hang regression marker"
        )]
        let child = std::env::var_os(CHILD).is_some();
        if !child {
            let mut child=std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact","simulation::round62_review_tests::non_suspending_process_loops_report_delta_limit","--nocapture"])
                .env(CHILD,"1").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).spawn().unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                if child.try_wait().unwrap().is_some() {
                    let output = child.wait_with_output().unwrap();
                    assert!(
                        output.status.success(),
                        "{}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    return;
                }
                if std::time::Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("non-suspending kernel hung instead of returning a bounded error");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            for _ in 0..2 {
                let error = sim.run_until(0).unwrap_err().to_string();
                assert!(error.contains("delta limit"), "{error}");
                assert_eq!(sim.next_event_time(), Some(0));
            }
        }
        for body in [
            "always wait(1'b1);",
            "always begin if(1)wait(1);else #1;end",
            "always wait(go);",
            "initial forever begin if(go)continue;#1;end",
        ] {
            let source = format!("module Top(input bit go);{body} endmodule");
            fn arm<B: SimBackend>(mut sim: Simulation<B>) {
                let go = sim.signal("go");
                sim.modify(|io| io.set(go, 1u8)).unwrap();
                check(sim);
            }
            backends!(&source, arm);
        }
    }
    #[test]
    fn eager_host_writes_publish_ff_edges_and_waiters() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            let clean = sim.checkpoint().unwrap();
            for active_drain in [true, false] {
                for observe in 0..3 {
                    sim.restore(&clean).unwrap();
                    sim.eval_comb().unwrap();
                    let drain = active_drain
                        .then(|| sim.simulator.runtime_event_drain())
                        .flatten();
                    let go = sim.signal("go");
                    sim.modify(|io| io.set(go, 1u8)).unwrap();
                    if drain.is_some() {
                        assert!(!sim.simulator.dirty);
                    }
                    match observe {
                        1 => {
                            sim.get(go);
                        }
                        2 => {
                            sim.eval_comb().unwrap();
                        }
                        _ => {}
                    }
                    assert_eq!(sim.next_event_time(), Some(0));
                    assert_eq!(sim.next_event_time(), Some(0));
                    assert_eq!(sim.step().unwrap(), Some(0));
                    assert_eq!(sim.get(sim.signal("q")), 1u8.into());
                    assert_eq!(sim.get(sim.signal("seen")), 1u8.into());
                    assert_eq!(sim.next_event_time(), None);
                }
            }
        }
        backends!(
            "module Top(input bit go,output bit[7:0]q,seen);always_comb if(go)$display(1);always @(posedge go)q++;initial begin @(posedge go);seen++;end endmodule",
            check
        );
    }
    #[test]
    fn zero_trip_hierarchy_dependencies_ignore_unexecuted_body_and_step() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(2).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), 0u8.into());
        }
        for body in [
            "repeat(0)q=a;",
            "repeat(32'sd0)q=read_a();",
            "while(1'b0)q=read_a();",
            "for(int i=0;i<0;i++)q=a;",
            "for(int i=0;1'b0;i=read_a())q=a;",
        ] {
            let source = format!(
                "module Child(input bit a,output bit q);function bit read_a();return a;endfunction always_comb begin q=0;{body} end endmodule module Top(output bit[7:0]seen);bit a;wire q;Child c(a,q);initial begin #1;a=1;a=0;end initial forever begin @(q);seen++;end endmodule"
            );
            backends!(&source, check);
        }
    }
    #[test]
    fn direct_host_setters_expose_pending_process_work() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            let clean = sim.checkpoint().unwrap();
            let go = sim.signal("go");
            for active_drain in [false, true] {
                for four in [false, true] {
                    for observe in 0..3 {
                        sim.restore(&clean).unwrap();
                        sim.eval_comb().unwrap();
                        let _drain = active_drain
                            .then(|| sim.simulator.runtime_event_drain())
                            .flatten();
                        if four {
                            sim.set_four_state(go, 1u8.into(), 0u8.into());
                        } else {
                            sim.set_wide(go, 1u8.into());
                        }
                        match observe {
                            1 => {
                                sim.get(go);
                            }
                            2 => {
                                sim.eval_comb().unwrap();
                            }
                            _ => {}
                        }
                        assert_eq!(sim.next_event_time(), Some(0));
                        assert_eq!(sim.step().unwrap(), Some(0));
                        assert_eq!(sim.get(sim.signal("q")), 1u8.into());
                        assert_eq!(sim.next_event_time(), None);
                    }
                }
            }
        }
        backends!(
            "module Top(input bit go,output bit q);initial begin @(posedge go);q=1;end endmodule",
            check
        );
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod pr1158_merge_regressions {
    use super::*;
    macro_rules! backends {
        ($source:expr, $check:ident) => {{
            for four_state in [false, true] {
                let build = || {
                    Simulator::from_sv_sources(
                        vec![($source, std::path::Path::new("merge-regression.sv"))],
                        "Top",
                    )
                    .four_state(four_state)
                };
                $check(Simulation::new(build().build_interpreter().unwrap()));
                $check(Simulation::new(build().build_cranelift().unwrap()));
                $check(Simulation::new(build().build_wasm().unwrap()));
                #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
                $check(Simulation::new(build().build_native().unwrap()));
            }
        }};
    }
    #[test]
    fn masked_false_while_preserves_separate_write_times() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), 1u8.into());
            sim.run_until(2).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), 2u8.into());
        }
        for condition in ["4'bx000", "4'bz000", "4'b000x"] {
            let source = format!(
                "module Child(output bit[1:0]q);initial begin #1;while({condition})#5;q[0]=1;end initial begin #2;q[1]=1;end endmodule module Top(output bit[7:0]seen);wire[1:0]q;Child c(q);initial forever begin @(q[0]^q[1]);seen++;end endmodule"
            );
            backends!(&source, check);
        }
    }
    #[test]
    fn selected_break_skips_step_hierarchy_reads() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(2).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), 0u8.into());
        }
        for body in [
            "break;",
            "if(1)break;",
            "case(1)1:break;default:continue;endcase",
        ] {
            let source = format!(
                "module Child(input bit a,output bit q);int i;always_comb begin q=0;for(i=0;i<1;i=a)begin {body} end end endmodule module Top(output bit[7:0]seen);bit a;wire q;Child c(a,q);initial begin #1;a=1;a=0;end initial forever begin @(q);seen++;end endmodule"
            );
            backends!(&source, check);
        }
    }
    #[test]
    fn distinct_captured_ff_sources_each_execute_the_body() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            let clk_addr = sim.simulator.program.get_addr(&[], &["clk"]).unwrap();
            let rst_addr = sim.simulator.program.get_addr(&[], &["rst"]).unwrap();
            assert_ne!(
                sim.simulator.program.design.events.canonical(clk_addr),
                sim.simulator.program.design.events.canonical(rst_addr)
            );
            for name in ["clk", "rst"] {
                let signal = sim.signal(name);
                sim.modify(|io| io.set(signal, 1u8)).unwrap();
                sim.eval_comb().unwrap();
            }
            let saved = sim.save_state().unwrap();
            let checkpoint = sim.checkpoint().unwrap();
            assert_eq!(sim.step().unwrap(), Some(0));
            assert_eq!(sim.get(sim.signal("q")), 2u8.into());
            assert_eq!(sim.next_event_time(), None);
            sim.restore(&checkpoint).unwrap();
            assert_eq!(sim.step().unwrap(), Some(0));
            assert_eq!(sim.get(sim.signal("q")), 2u8.into());
            sim.load_state(&saved).unwrap();
            assert_eq!(sim.step().unwrap(), Some(0));
            assert_eq!(sim.get(sim.signal("q")), 2u8.into());
        }
        backends!(
            "module Top(input bit clk,rst,output bit[7:0]q);always_ff @(posedge clk or posedge rst)if(rst)q=q+1;else q=q+1;endmodule",
            check
        );
    }
    #[test]
    fn finished_modify_keeps_outputs_and_diagnostics_frozen_with_drain() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            assert!(sim.is_finished());
            let mut drain = sim.simulator.runtime_event_drain();
            if let Some(drain) = &mut drain {
                drain.drain();
            } else {
                sim.drain_runtime_events();
            }
            let a = sim.signal("a");
            sim.modify(|io| io.set(a, 1u8)).unwrap();
            assert_eq!(sim.get(sim.signal("q")), 0u8.into());
            assert_eq!(sim.step().unwrap(), None);
            let events = if let Some(drain) = &mut drain {
                drain.drain()
            } else {
                sim.drain_runtime_events()
            };
            assert!(
                events.is_empty(),
                "new diagnostics after finish: {events:?}"
            );
            // Explicit evaluation remains an opt-in low-level operation.
            sim.eval_comb().unwrap();
            assert_eq!(sim.get(sim.signal("q")), 1u8.into());
        }
        backends!(
            r#"module Top(input bit a,output bit q);always_comb begin q=a;if(a)$display("late");end initial $finish;endmodule"#,
            check
        );
    }
    #[test]
    fn physical_clock_aliases_do_not_duplicate_host_occurrences() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(0).unwrap();
            let clk_addr = sim.simulator.program.get_addr(&[], &["clk"]).unwrap();
            let mirror_addr = sim.simulator.program.get_addr(&[], &["mirror"]).unwrap();
            assert_eq!(
                sim.simulator.program.design.events.canonical(clk_addr),
                sim.simulator.program.design.events.canonical(mirror_addr)
            );
            let clk = sim.signal("clk");
            for value in [1u8, 0, 1] {
                sim.modify(|io| io.set(clk, value)).unwrap();
                sim.eval_comb().unwrap();
            }
            sim.run_until(0).unwrap();
            assert_eq!(sim.get(sim.signal("direct")), 2u8.into());
            assert_eq!(sim.get(sim.signal("aliased")), 2u8.into());
        }
        backends!(
            "module Child(input bit clk,output bit[7:0]q);always @(posedge clk)q=q+1;endmodule module Top(input bit clk,output wire[7:0]direct,aliased);wire mirror=clk;Child a(clk,direct);Child b(mirror,aliased);endmodule",
            check
        );
    }
    #[test]
    fn masked_known_one_while_retains_executed_body_delay() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.run_until(1).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), 0u8.into());
            sim.run_until(2).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), 1u8.into());
            sim.run_until(3).unwrap();
            assert_eq!(sim.get(sim.signal("seen")), 2u8.into());
        }
        backends!(
            "module Child(output bit[1:0]q);initial begin #1;while(4'bx001)begin #1;q[0]=1;break;end end initial begin #3;q[1]=1;end endmodule module Top(output bit[7:0]seen);wire[1:0]q;Child c(q);initial forever begin @(q[0]^q[1]);seen++;end endmodule",
            check
        );
    }
}
