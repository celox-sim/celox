use crate::{
    RuntimeErrorCode, Simulator,
    backend::{EventHandle, MemoryLayout, SimBackend},
    ir::SignalRef,
    simulator::{
        Checkpoint, CheckpointError, InstanceHierarchy, NamedEvent, NamedSignal, StateError,
    },
};
use celox_runtime::{
    Halt, HostBoundary, ResolvedSchedule, SimulationSnapshot, SimulationState, TickExecutor,
    UnitExecutor, host_watch,
};

mod differential;
mod vcd_tasks;

/// Saved state of a [`Simulation`], created by [`Simulation::checkpoint`]:
/// the design state together with simulation time, clocks and pending events.
pub struct SimulationCheckpoint<B: SimBackend = crate::DefaultBackend> {
    simulator: Checkpoint,
    schedule: SimulationSnapshot<B>,
    /// Host writes of waited signals the next step has yet to show the
    /// waits.
    host_writes: Vec<(SignalRef, celox_runtime::timing::Value)>,
}

impl<B: SimBackend> Clone for SimulationCheckpoint<B> {
    fn clone(&self) -> Self {
        Self {
            simulator: self.simulator.clone(),
            schedule: self.schedule.clone(),
            host_writes: self.host_writes.clone(),
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
/// The builder also offers `build_native`, `build_cranelift`, `build_wasm`,
/// `build_interpreter`, and `build_tiered`; thread and partition options apply to timed builds.
pub struct Simulation<B: SimBackend = crate::DefaultBackend> {
    pub(crate) simulator: Simulator<B>,
    pub(crate) state: SimulationState<B>,
    /// The reference simulation every step is checked against in
    /// [`crate::SchedulerMode::Differential`].
    shadow: Option<Box<Simulation<B>>>,
    /// Steps checked against the shadow.
    checked_steps: u64,
    /// Why [`crate::SchedulerMode::Differential`] has no shadow.
    unchecked: Option<&'static str>,
    /// Boxed: only runs with `fail_on` use it.
    reports: Box<ReportWatch>,
}

/// Reports a simulation stops at, set by [`Simulation::set_fail_on`].
#[derive(Debug, Default)]
struct ReportWatch {
    fail_on: Option<crate::Severity>,
    /// Events taken after each step while `fail_on` is set, with the time of
    /// that step, for the next [`Simulation::drain_runtime_events`].
    events: Vec<crate::RuntimeEvent>,
    /// The first report of the last run at or above `fail_on`.
    failure: Option<crate::Report>,
}

impl<B: SimBackend> std::fmt::Debug for Simulation<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Simulation")
            .field("time", &self.state.time())
            .finish()
    }
}

impl<B: SimBackend> UnitExecutor for Simulator<B> {
    type Backend = B;

    fn backend(&self) -> &B {
        &self.backend
    }

    fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    #[inline]
    fn eval_comb(&mut self) -> Result<(), Halt> {
        self.eval_comb_checked()?;
        self.runtime_finish()
    }

    #[inline]
    fn eval_apply_ff_at(&mut self, event: B::Event) -> Result<(), Halt> {
        self.eval_apply_ff_at_checked(event)?;
        self.runtime_finish()
    }

    #[inline]
    fn eval_only_ff_at(&mut self, event: B::Event) -> Result<(), Halt> {
        self.eval_only_ff_at_checked(event)?;
        self.runtime_finish()
    }

    #[inline]
    fn apply_ff_at(&mut self, event: B::Event) -> Result<(), Halt> {
        self.apply_ff_at_checked(event)?;
        self.runtime_finish()
    }

    fn run_process(&mut self, index: usize) -> Result<(), Halt> {
        self.backend
            .run_process(index)
            .map_err(|e| self.decorate_runtime_error(e))?;
        self.runtime_finish()
    }

    fn reset_declaration_startup(&mut self) -> Result<(), Halt> {
        self.reset_declaration_state();
        Ok(())
    }
    fn prime_declaration_startup(&mut self) -> Result<(), Halt> {
        self.prime_declaration_state();
        Ok(())
    }
    fn apply_declaration_startup(&mut self) -> Result<(), Halt> {
        self.apply_ordinary_initial_state();
        Ok(())
    }

    fn host_request(
        &mut self,
        request: &celox_design::HostRequestSite<SignalRef>,
        timestamp: u64,
    ) -> Result<(), Halt> {
        match request.kind {
            celox_design::HostRequestKind::Plusargs => {
                self.run_plusargs_request(request, timestamp)
            }
            celox_design::HostRequestKind::Distribution => {
                self.run_distribution_request(request, timestamp)
            }
            celox_design::HostRequestKind::FileIo => self.run_file_io_request(request, timestamp),
            celox_design::HostRequestKind::Vcd => self.run_vcd_request(request, timestamp),
            celox_design::HostRequestKind::Scan => self.run_scan_request(request, timestamp),
        }
    }
}

impl<B: SimBackend> TickExecutor for Simulator<B> {
    #[inline]
    fn eval_comb_apply_ff_at(&mut self, event: B::Event) -> Result<(), Halt> {
        self.eval_comb_apply_ff_at_checked(event)?;
        self.runtime_finish()
    }
}

impl<B: SimBackend> HostBoundary for Simulator<B> {
    fn take_host_writes(&mut self) -> Vec<(SignalRef, celox_runtime::timing::Value)> {
        self.host_watch.take_log()
    }

    fn freeze(&mut self) {
        self.frozen = true;
    }

    fn flush_output(&mut self) -> Result<(), RuntimeErrorCode> {
        if self.sv_vcd.started.is_some() {
            self.flush_vcd()
                .map_err(|error| RuntimeErrorCode::Runtime {
                    message: format!("VCD flush at finish failed: {error}"),
                    signals: Vec::new(),
                })?;
        }
        Ok(())
    }

    fn try_freeze(&mut self) -> Result<(), RuntimeErrorCode> {
        let output = self.flush_output();
        self.freeze();
        output
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

    fn compiled_slots(&self) -> celox_runtime::CompiledSlots {
        use celox_runtime::CompiledSlots;
        // Deferred assertions mature when a slot is over, which the
        // runtime's loop tells apart from the end of a step.
        if self.frozen
            || !self.components.is_empty()
            || self.deferred_assertions
            || !self.backend.runs_compiled_scheduler()
        {
            return CompiledSlots::None;
        }
        // Waveforms record, and the heap collects, at every slot boundary.
        let vcd = &self.sv_vcd;
        if self.vcd_writer.is_some()
            || vcd.path.is_some()
            || vcd.started.is_some()
            || vcd.pending
            || !self.handle_slots().is_empty()
        {
            return CompiledSlots::One;
        }
        CompiledSlots::Any
    }

    fn finish_timed_step(&mut self, timestamp: u64) {
        self.dirty = false;
        self.dump_unless_rewound(timestamp);
    }

    /// Deferred assertions mature in the Observed and Postponed regions of
    /// the slot (IEEE 1800-2023 16.4).
    fn try_end_time_slot(&mut self, _timestamp: u64) -> Result<(), Halt> {
        self.mature_deferred_assertions().map_err(Halt::Error)
    }

    fn try_finish_timed_step(&mut self, timestamp: u64) -> Result<(), Halt> {
        // The time slot is over: no kernel runs, and its updates are in
        // state memory.
        self.take_heap_fault().map_err(Halt::Error)?;
        self.heap_safepoint();
        self.start_sv_vcd(timestamp).map_err(|error| {
            Halt::Error(RuntimeErrorCode::Runtime {
                message: format!("VCD setup failed: {error}"),
                signals: Vec::new(),
            })
        })?;
        self.dirty = false;
        self.try_dump_unless_rewound(timestamp)
            .map_err(|error| Halt::Error(vcd_dump_runtime_error(error)))
    }
}

fn vcd_dump_runtime_error(error: crate::simulator::DumpError) -> RuntimeErrorCode {
    match error {
        crate::simulator::DumpError::Runtime(error) => error,
        crate::simulator::DumpError::Io(error) => RuntimeErrorCode::Runtime {
            message: format!("VCD dump failed: {error}"),
            signals: Vec::new(),
        },
    }
}

impl<B: SimBackend> Simulator<B> {
    /// A `$finish` the design's flip-flop or combinational code executed
    /// ends a timed simulation (IEEE 1800-2023 20.2).
    #[inline]
    fn runtime_finish(&mut self) -> Result<(), Halt> {
        if !self.finish_sites {
            return Ok(());
        }
        if self.take_runtime_finish() {
            Err(Halt::Finish)
        } else {
            Ok(())
        }
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
    SimulationState::new(
        &simulator.backend,
        ResolvedSchedule::resolve(&simulator.backend, &simulator.schedule),
    )
}

impl<B: SimBackend> Simulation<B> {
    #[cfg(test)]
    pub(crate) fn new(simulator: Simulator<B>) -> Self {
        Self::try_new(simulator).expect("runtime static declaration initialization failed")
    }

    pub(crate) fn try_new(mut simulator: Simulator<B>) -> Result<Self, RuntimeErrorCode> {
        let mut state = simulation_state(&simulator);
        simulator.host_watch = host_watch(&simulator.backend, &simulator.schedule);
        // Only a timed simulation ends at an RTL `$finish`; a bare
        // `Simulator`, ticks included, leaves that to the host.
        simulator.finish_sites = simulator.schedule.finish_sites;
        simulator.timed = true;
        simulator.skip_runtime_finish();
        state.initialize_declarations(&mut simulator)?;
        Ok(Self {
            simulator,
            state,
            shadow: None,
            checked_steps: 0,
            unchecked: None,
            reports: Box::default(),
        })
    }

    /// Returns warnings emitted during compilation.
    pub fn warnings(&self) -> &[crate::CompilationWarning] {
        self.simulator.warnings()
    }

    /// Returns a reference to the compiled SIR program.
    pub fn program(&self) -> &crate::ir::RuntimeProgram {
        self.simulator.program()
    }

    /// Save the design state, simulation time, clocks and pending events.
    pub fn checkpoint(&self) -> Result<SimulationCheckpoint<B>, CheckpointError> {
        if self.state.is_suspended() {
            return Err(CheckpointError::Stopped);
        }
        Ok(SimulationCheckpoint {
            simulator: self.simulator.checkpoint()?,
            schedule: self.state.snapshot(),
            host_writes: self.simulator.host_watch.log().to_vec(),
        })
    }

    /// Save the value of every state object by path, together with the
    /// simulation time, clocks and pending events by name.
    pub fn save_state(&mut self) -> Result<celox_runtime::StateFile, StateError> {
        if self.state.is_suspended() {
            return Err(CheckpointError::Stopped.into());
        }
        let mut file = self.simulator.save_state()?;
        // Settling for the file may have executed a `$finish`.
        self.sync_finish_checked().map_err(StateError::Runtime)?;
        let parts = self.state.export_schedule(&self.simulator.backend);
        let mut record = self.simulator.name_schedule(parts);
        record.processes.host_writes = self.simulator.name_host_writes();
        file.schedule = Some(record);
        Ok(file)
    }

    /// Load a state file saved from a `Simulation`, including its time,
    /// clocks and pending events. See [`Simulator::load_state`] for how
    /// objects are matched.
    pub fn load_state(&mut self, file: &celox_runtime::StateFile) -> Result<(), StateError> {
        let record = file.schedule.as_ref().ok_or(StateError::MissingSchedule)?;
        self.simulator.check_vcd_rewind(record.time)?;
        let parts = self.simulator.resolve_schedule(record)?;
        let host_writes = self
            .simulator
            .resolve_host_writes(&record.processes.host_writes)?;
        self.simulator.load_state_at_time(file, record.time)?;
        self.state.import_schedule(parts);
        // The file keeps the time in its schedule, not in the time slots.
        let time = self.state.time();
        self.state.advance_time(&mut self.simulator.backend, time);
        self.simulator.host_watch.set_log(host_writes);
        self.simulator.frozen = self.state.is_finished();
        // Capturing a state during a stopped initializer is forbidden. A
        // successfully loaded file therefore replaces that transient phase.
        self.simulator.declaration_phase = false;
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
        self.simulator.declaration_phase = false;
        self.simulator
            .host_watch
            .set_log(checkpoint.host_writes.clone());
        Ok(())
    }

    /// A `$finish` that flip-flop or combinational code executed outside a
    /// step, during a direct settle, read or dump, ends the simulation too.
    fn sync_finish_checked(&mut self) -> Result<(), RuntimeErrorCode> {
        if !self.state.is_finished() && self.simulator.take_runtime_finish() {
            self.state.finalize_terminal(&mut self.simulator)?;
        }
        Ok(())
    }

    /// Publish diagnostic state without replacing a primary evaluation error
    /// or freezing a simulation whose caller may recover.
    fn flush_error_output(&mut self) {
        let _ = self.simulator.try_finish_timed_step(self.state.time());
        let _ = self.simulator.flush_output();
    }

    fn sync_finish(&mut self) {
        self.sync_finish_checked()
            .unwrap_or_else(|error| panic!("simulation finish failed: {error}"));
    }

    /// Captures the current state of all signals and writes them to the VCD file.
    pub fn dump(&mut self, timestamp: u64) {
        self.simulator.dump(timestamp);
        self.sync_finish();
    }

    /// See [`Simulator::try_dump`].
    pub fn try_dump(&mut self, timestamp: u64) -> Result<(), crate::simulator::DumpError> {
        let result = self.simulator.try_dump(timestamp);
        let finished = self.sync_finish_checked();
        if matches!(&result, Err(crate::simulator::DumpError::Runtime(_))) {
            self.flush_error_output();
        }
        result?;
        finished.map_err(crate::simulator::DumpError::Runtime)
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
        let value = self.simulator.get(signal);
        self.sync_finish();
        value
    }

    /// Retrieves the current four-state `(value, mask)` using a resolved signal.
    /// Lazily evaluates dirty combinational logic, like [`Simulator::get_four_state`],
    /// and records a finish request raised by that evaluation.
    pub fn get_four_state(
        &mut self,
        signal: SignalRef,
    ) -> (num_bigint::BigUint, num_bigint::BigUint) {
        let value = self.simulator.get_four_state(signal);
        self.sync_finish();
        value
    }

    /// Modifies internal state via a callback and re-stabilizes combinational logic.
    ///
    /// A write that changes an event signal, and any write while a process
    /// waits or a signal is counted, is an update of the current time: the
    /// next [`Self::step`] or [`Self::run_until`] settles it at that time,
    /// then advances.
    pub fn modify<F>(&mut self, f: F) -> Result<(), RuntimeErrorCode>
    where
        F: FnOnce(&mut crate::IOContext<B>),
    {
        let result = self.simulator.modify(f);
        let finished = self.sync_finish_checked();
        result?;
        finished?;
        let logged = self.simulator.host_watch.has_log();
        self.state.note_host_writes(&self.simulator.backend, logged);
        Ok(())
    }

    /// Register a clock signal and its period, enqueuing the first edge.
    /// Odd periods alternate floor/ceil half periods. Periods below two
    /// time units use a two-unit period so both phases advance time.
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
    /// Host writes since the last step are settled at the current time first.
    /// Returns the new simulation time, the current time when only host
    /// writes were settled, or None if nothing was to do.
    pub fn step(&mut self) -> Result<Option<u64>, RuntimeErrorCode> {
        self.reports.failure = None;
        if self.reports.fail_on.is_some() {
            return self.watched_step();
        }
        self.step_until(u64::MAX)
    }

    /// [`Self::step`] while [`Self::set_fail_on`] is set: a slot that only
    /// settles host writes is watched before the step goes on to the next
    /// time.
    #[cold]
    #[inline(never)]
    fn watched_step(&mut self) -> Result<Option<u64>, RuntimeErrorCode> {
        let mut settled = None;
        while let Some(next_time) = self.state.next_event_time() {
            let host_only =
                self.state.host_pending() && self.state.next_work_time() != Some(next_time);
            let time = self.step_until(next_time)?;
            self.watch_reports();
            if !host_only || self.state.is_stopped() || self.reports.failure.is_some() {
                return Ok(time);
            }
            settled = time;
        }
        self.watch_settle()?;
        Ok(settled)
    }

    /// [`Self::step`] that, after settling host writes at the current time,
    /// does not advance past `end_time`.
    fn step_until(&mut self, end_time: u64) -> Result<Option<u64>, RuntimeErrorCode> {
        let Some(mut shadow) = self.shadow.take() else {
            return self.state.step_until(&mut self.simulator, end_time);
        };
        let mut check = differential::ShadowCheck::new(&mut shadow, self.mode());
        let result = self
            .state
            .step_until_observed(&mut self.simulator, end_time, &mut check);
        self.checked_steps += check.checked;
        self.shadow = Some(shadow);
        result
    }

    /// The steps of a run while [`Self::set_fail_on`] is set, apart from
    /// the run loop that has no reports to watch. Returns `false` when a
    /// step stopped at `$stop` or at a failure, or `max_steps` ran out.
    #[cold]
    #[inline(never)]
    fn watched_steps(&mut self, end_time: u64, max_steps: u64) -> Result<bool, RuntimeErrorCode> {
        let mut steps = 0;
        while let Some(next_time) = self.state.next_event_time() {
            if next_time > end_time {
                break;
            }
            if steps >= max_steps {
                return Ok(false);
            }
            // One slot per step, so that a slot that only settles host
            // writes is watched before the next runs.
            self.step_until(next_time)?;
            self.watch_reports();
            steps += 1;
            if self.state.is_stopped() || self.reports.failure.is_some() {
                return Ok(false);
            }
        }
        self.watch_settle()?;
        Ok(self.reports.failure.is_none())
    }

    /// Settle combinational logic that no step settled, as after host writes
    /// that wake no process, at the current time, and watch its reports.
    fn watch_settle(&mut self) -> Result<(), RuntimeErrorCode> {
        if !self.simulator.dirty {
            return Ok(());
        }
        if let Err(error) = self.simulator.eval_comb_checked() {
            self.flush_error_output();
            return Err(error);
        }
        self.simulator.dirty = false;
        self.watch_reports();
        Ok(())
    }

    /// Stop [`Self::run_until`] and [`Self::run_until_bounded`] after the
    /// time step of the first report of at least `severity`, which
    /// [`Self::failure`] then returns. While it is set, the events of each
    /// step are taken from the design's event buffer after the step and keep
    /// the time of that step, so none are missed and their reports have an
    /// exact time. They wait for [`Self::drain_runtime_events`]. `None`
    /// stops only at `$fatal`, as before.
    pub fn set_fail_on(&mut self, severity: Option<crate::Severity>) {
        self.reports.fail_on = severity;
    }

    /// The report the last run stopped at (see [`Self::set_fail_on`]).
    /// The next run clears it.
    pub fn failure(&self) -> Option<&crate::Report> {
        self.reports.failure.as_ref()
    }

    fn watch_reports(&mut self) {
        let Some(fail_on) = self.reports.fail_on else {
            return;
        };
        // A step leaves combinational logic settled.
        let events =
            self.simulator
                .drain_settled_runtime_events_with_context(crate::RuntimeFormatContext {
                    tb_time: Some(self.state.time()),
                    scope: None,
                });
        if self.reports.failure.is_none() {
            self.reports.failure = events.iter().find_map(|event| match event {
                crate::RuntimeEvent::Report(report) if report.severity >= fail_on => {
                    Some(report.clone())
                }
                // A lost event may have been a failure.
                crate::RuntimeEvent::Missed { count } => Some(crate::Report::new(
                    fail_on,
                    format!("missed {count} runtime events").into_bytes(),
                )),
                _ => None,
            });
        }
        self.reports.events.extend(events);
    }

    /// Advance time and run until `end_time` (inclusive).
    /// Targets earlier than the current time leave time and state unchanged.
    /// A process that executes `$stop` returns control early, after the time
    /// slot of the stop, with [`Self::is_stopped`] set; calling this again
    /// continues. A report of the severity of [`Self::set_fail_on`] returns
    /// early in the same way, with [`Self::failure`] set.
    pub fn run_until(&mut self, end_time: u64) -> Result<(), RuntimeErrorCode> {
        self.reports.failure = None;
        // With reports to watch, step one slot at a time; the run below
        // then only advances the time.
        if self.reports.fail_on.is_some() && !self.watched_steps(end_time, u64::MAX)? {
            return Ok(());
        }
        let reached = match self.shadow.take() {
            None => self.state.run_until(&mut self.simulator, end_time)?,
            Some(mut shadow) => {
                let mut check = differential::ShadowCheck::new(&mut shadow, self.mode());
                let result =
                    self.state
                        .run_until_observed(&mut self.simulator, end_time, &mut check);
                self.checked_steps += check.checked;
                self.shadow = Some(shadow);
                result?
            }
        };
        if !reached {
            return Ok(());
        }
        if let Err(error) = self.simulator.try_dump_unless_rewound(end_time) {
            if matches!(&error, crate::simulator::DumpError::Runtime(_)) {
                self.flush_error_output();
            }
            return Err(vcd_dump_runtime_error(error));
        }
        Ok(())
    }

    /// Like [`Self::run_until`], but takes at most `max_steps` steps.
    /// Returns `false`, with the simulation at the time of its last step,
    /// when another step would be needed to reach `end_time`, including when
    /// a step stopped at `$stop` or at a [`Self::failure`].
    pub fn run_until_bounded(
        &mut self,
        end_time: u64,
        max_steps: u64,
    ) -> Result<bool, RuntimeErrorCode> {
        if end_time < self.state.time() {
            return Ok(true);
        }
        self.reports.failure = None;
        if self.reports.fail_on.is_some() {
            if !self.watched_steps(end_time, max_steps)? {
                return Ok(false);
            }
            self.run_until(end_time)?;
            return Ok(!self.state.is_stopped());
        }
        let mut steps = 0;
        while let Some(next_time) = self.state.next_event_time() {
            if next_time > end_time {
                break;
            }
            if steps >= max_steps {
                return Ok(false);
            }
            self.step_until(end_time)?;
            steps += 1;
            if self.state.is_stopped() {
                return Ok(false);
            }
        }
        self.run_until(end_time)?;
        Ok(!self.state.is_stopped())
    }

    /// Whether a process of the design requested the end of the simulation.
    /// Once it has, [`Self::step`] returns `None` and [`Self::run_until`]
    /// stops at the time of the request.
    pub fn is_finished(&self) -> bool {
        self.state.is_finished()
    }

    /// Whether the last [`Self::step`] or [`Self::run_until`] returned early
    /// because a process of the design executed `$stop` (IEEE 1800-2023
    /// 20.2). The simulation is suspended in the middle of that time; the
    /// next step continues it after the `$stop`. Checkpoints and state files
    /// cannot be taken while it is suspended.
    pub fn is_stopped(&self) -> bool {
        self.state.is_stopped()
    }

    /// Returns the current simulation time.
    pub fn time(&self) -> u64 {
        self.state.time()
    }

    /// Periodic clocks registered with [`Self::add_clock`] (or loaded with a
    /// state), as (event id, period).
    pub fn clock_periods(&self) -> Vec<(usize, u64)> {
        self.state.clock_periods()
    }

    /// Takes the runtime events (`$display`, messages, `$finish`, ...) the
    /// design emitted since the last call, in emission order.
    pub fn drain_runtime_events(&mut self) -> Vec<crate::RuntimeEvent> {
        // A slot with nothing more due is over, also before the first step:
        // the initial settle of combinational logic belongs to time zero.
        // A fatal action is drained as a fatal report.
        if self.simulator.deferred_assertions
            && self.state.next_event_time() != Some(self.state.time())
        {
            // Lazily settled logic runs in the slot too.
            self.simulator.settle_for_runtime_event_drain();
            let _ = self.simulator.mature_deferred_assertions();
        }
        let mut events = std::mem::take(&mut self.reports.events);
        events.extend(self.simulator.drain_runtime_events());
        events
    }

    /// Returns the time of the next scheduled event, if any.
    pub fn next_event_time(&self) -> Option<u64> {
        self.state.next_event_time()
    }

    /// Directly execute combinational logic evaluation.
    pub fn eval_comb(&mut self) -> Result<(), RuntimeErrorCode> {
        let result = self.simulator.eval_comb();
        let finished = self.sync_finish_checked();
        if result.is_err() {
            self.flush_error_output();
        }
        result?;
        finished
    }

    /// Returns a raw pointer to the backend memory and its total size in bytes.
    pub fn memory_as_ptr(&self) -> (*const u8, usize) {
        self.simulator.memory_as_ptr()
    }

    /// Returns a mutable raw pointer to the backend memory and its total size in bytes.
    pub fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        self.simulator.memory_as_mut_ptr()
    }

    /// The host wrote the state through [`Self::memory_as_mut_ptr`]. Like
    /// [`Self::modify`], the next step then settles the current time when the
    /// writes may wake a process or changed an event signal. Waits and
    /// counters see the values the writes left, not each write.
    pub fn note_raw_writes(&mut self) {
        self.state.note_host_writes(&self.simulator.backend, false);
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
        assert_eq!(
            sim.get_four_state(count),
            (((steps + 5) / 10).into(), 0u8.into())
        );
    }

    fn builder(four_state: bool, steps: usize) -> SimulatorBuilder<'static, Simulator> {
        Simulator::from_frontend(design(four_state, steps))
            .four_state(four_state)
            .emit_triggers()
    }

    fn check_all_backends(four_state: bool, steps: usize) {
        let timed = || Simulation::from_frontend(design(four_state, steps)).four_state(four_state);
        check(timed().build_interpreter().unwrap(), steps as u64);
        check(timed().build_cranelift().unwrap(), steps as u64);
        check(timed().build_wasm().unwrap(), steps as u64);
        check(
            timed()
                .threads(4)
                .parallel_partition(crate::ParallelPartition::Always)
                .build_cranelift()
                .unwrap(),
            steps as u64,
        );
        check(timed().build_tiered().unwrap(), steps as u64);
        #[cfg(any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        ))]
        {
            check(timed().build_native().unwrap(), steps as u64);
            check(
                timed()
                    .threads(4)
                    .parallel_partition(crate::ParallelPartition::Always)
                    .build_native()
                    .unwrap(),
                steps as u64,
            );
            // A native image starts an untimed Simulator, which runs no
            // process.
            let error = builder(four_state, steps)
                .compile_native()
                .map(|_| ())
                .unwrap_err();
            assert!(matches!(
                error.kind(),
                crate::SimulatorErrorKind::ProcessRequiresSimulation(_)
            ));
        }
    }

    #[cfg(feature = "systemverilog")]
    #[test]
    fn timed_tiered_promotion_preserves_suspended_processes_and_nba() {
        let source = r#"
            `timescale 1fs/1fs
            module Top(output logic [7:0] count);
                logic clk = 0;
                initial count = 0;
                always #5 clk = ~clk;
                always @(posedge clk) count <= count + 1;
                initial begin #36; $finish; end
            endmodule
        "#;
        for four_state in [false, true] {
            for threads in [1, 4] {
                let mut sim = Simulation::from_sv_sources(
                    vec![(source, std::path::Path::new("tiered.sv"))],
                    "Top",
                )
                .four_state(four_state)
                .threads(threads)
                .parallel_partition(crate::ParallelPartition::Always)
                .tier_promotion(crate::TierPromotion::AfterSteps(1000))
                .build_tiered()
                .unwrap();
                let count = sim.signal("count");
                sim.run_until(10).unwrap();
                assert!(!sim.simulator.is_compiled());
                assert_eq!(sim.get(count), 1u8.into());
                // Poll at the same simulation time while future delayed processes
                // remain suspended. The tier transition must retain their resume
                // positions and subsequent Active/NBA ordering (IEEE 1800-2023 4.5).
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                while !sim.simulator.is_compiled() {
                    sim.eval_comb().unwrap();
                    assert!(
                        sim.simulator.promotion_error().is_none(),
                        "{:?}",
                        sim.simulator.promotion_error()
                    );
                    assert!(
                        std::time::Instant::now() < deadline,
                        "tiered compilation timed out"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                assert_eq!(sim.time(), 10);
                assert_eq!(sim.get_four_state(count), (1u8.into(), 0u8.into()));
                sim.run_until(40).unwrap();
                assert!(sim.is_finished());
                assert_eq!(sim.get_four_state(count), (4u8.into(), 0u8.into()));
                assert!(sim.simulator.tiered_execution_stats().compiled_evaluations > 0);
            }
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
    /// A clock taken from one bit of a register is edge-sensed on that bit on
    /// every backend, not on the register as a whole. In four-state mode the
    /// WebAssembly backend leaves the X-initialized bits above the one-bit
    /// clock set, which the scheduler must not read as a high level.
    #[test]
    fn register_bit_clocks_follow_the_selected_bit_on_every_backend() {
        fn check<B: crate::SimBackend>(backend: crate::Simulator<B>, step: u64, bit: u64) {
            let mut sim = Simulation::new(backend);
            let a = sim.signal("cnt1");
            let b = sim.signal("cnt2");
            sim.schedule("rst", 0, 0).unwrap();
            sim.schedule("clk", 0, 0).unwrap();
            sim.step().unwrap();
            sim.schedule("rst", 5, 1).unwrap();
            sim.step().unwrap();
            let mut count = 0u64;
            for edge in 1..=12u64 {
                sim.schedule("clk", edge * 20 - 10, 1).unwrap();
                sim.step().unwrap();
                let (before, after) = (((edge - 1) * step) >> bit, (edge * step) >> bit);
                count += u64::from(before & 1 == 0 && after & 1 == 1);
                assert_eq!(sim.get(a), ((edge * step) & 0xff).into());
                assert_eq!(sim.get(b), count.into(), "a[{bit}] after edge {edge}");
                sim.schedule("clk", edge * 20, 0).unwrap();
                sim.step().unwrap();
            }
        }
        for (step, bit) in [(1, 0), (2, 0), (1, 2)] {
            let code = format!(
                "module Top (clk: input '_ clock, rst: input '_ reset_async_low,
                             cnt1: output logic<8>, cnt2: output logic<8>) {{
                    var a: logic<8>;
                    var b: logic<8>;
                    always_ff (clk, rst) {{ if_reset {{ a = 0; }} else {{ a = a + 8'd{step}; }} }}
                    let divided: '_ clock = a[{bit}];
                    always_ff (divided, rst) {{ if_reset {{ b = 0; }} else {{ b = b + 8'd1; }} }}
                    assign cnt1 = a;
                    assign cnt2 = b;
                }}"
            );
            for four_state in [false, true] {
                let build = || {
                    crate::Simulator::builder(&code, "Top")
                        .four_state(four_state)
                        .emit_triggers()
                };
                check(build().build_interpreter().unwrap(), step, bit);
                check(build().build_cranelift().unwrap(), step, bit);
                check(build().build_wasm().unwrap(), step, bit);
                check(build().build_tiered().unwrap(), step, bit);
                #[cfg(any(
                    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
                    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
                ))]
                check(build().build_native().unwrap(), step, bit);
            }
        }
    }

    /// Clocks computed from `clk` share its edge: `clk & en` is gated by the
    /// enable from before the edge (here a register of `clk`'s own domain)
    /// and its domain reads the state from before `clk`'s domain commits; an
    /// edge raising `en` raises the gate after the commit, a further edge
    /// that reads the committed state. `~clk` rises when `clk` falls. Timed
    /// steps and event-based ticks, one clock period each, agree on every
    /// backend, in both state modes; the four-state ticks judge the gate by
    /// settling the design.
    #[test]
    fn gated_clocks_share_their_source_edge_on_every_backend() {
        const CODE: &str = "module Top (clk: input '_ clock, rst: input '_ reset_async_low,
                en_in: input logic, cnt1: output logic<8>, sampled: output logic<8>,
                fired: output logic<8>, inverted_sampled: output logic<8>) {
            var a: logic<8>;
            var en: logic;
            var b: logic<8>;
            var c: logic<8>;
            var d: logic<8>;
            always_ff (clk, rst) {
                if_reset { a = 0; en = 1; } else { a = a + 8'd1; en = en_in; }
            }
            let gated: '_ clock = clk & en;
            always_ff (gated, rst) {
                if_reset { b = 0; c = 0; } else { b = a; c = c + 8'd1; }
            }
            let inverted: '_ clock = ~clk;
            always_ff (inverted, rst) { if_reset { d = 0; } else { d = a; } }
            assign cnt1 = a;
            assign sampled = b;
            assign fired = c;
            assign inverted_sampled = d;
        }";
        const ENABLES: [u8; 8] = [1, 1, 0, 0, 1, 1, 0, 1];
        fn expected() -> Vec<[u64; 4]> {
            let (mut gate, mut sampled, mut fired) = (1, 0, 0);
            ENABLES
                .iter()
                .zip(1u64..)
                .map(|(&enable, edge)| {
                    if gate == 1 {
                        // The edge passes the open gate before any commit.
                        sampled = edge - 1;
                        fired += 1;
                    } else if enable == 1 {
                        // The commit raises `en` while `clk` is high.
                        sampled = edge;
                        fired += 1;
                    }
                    gate = enable;
                    [edge, sampled, fired, edge]
                })
                .collect()
        }
        fn observe<B: crate::SimBackend>(sim: &mut crate::Simulator<B>) -> [u64; 4] {
            ["cnt1", "sampled", "fired", "inverted_sampled"].map(|name| {
                let signal = sim.signal(name);
                sim.get(signal).try_into().unwrap()
            })
        }
        fn timed<B: crate::SimBackend>(backend: crate::Simulator<B>) -> Vec<[u64; 4]> {
            let mut sim = Simulation::new(backend);
            let en_in = sim.signal("en_in");
            // Assert the reset with an edge, then release it.
            for (port, time, value) in [("rst", 0, 1), ("clk", 0, 0), ("rst", 2, 0), ("rst", 5, 1)]
            {
                sim.schedule(port, time, value).unwrap();
                sim.step().unwrap();
            }
            ENABLES
                .iter()
                .zip(1u64..)
                .map(|(&enable, edge)| {
                    sim.modify(|io| io.set(en_in, enable)).unwrap();
                    sim.schedule("clk", edge * 20 - 10, 1).unwrap();
                    sim.step().unwrap();
                    sim.schedule("clk", edge * 20, 0).unwrap();
                    sim.step().unwrap();
                    observe(&mut sim.simulator)
                })
                .collect()
        }
        fn ticked<B: crate::SimBackend>(mut sim: crate::Simulator<B>) -> Vec<[u64; 4]> {
            let clk = sim.event("clk");
            let (rst, en_in) = (sim.signal("rst"), sim.signal("en_in"));
            sim.modify(|io| io.set(rst, 0u8)).unwrap();
            sim.tick(clk).unwrap();
            sim.modify(|io| io.set(rst, 1u8)).unwrap();
            ENABLES
                .iter()
                .map(|&enable| {
                    sim.modify(|io| io.set(en_in, enable)).unwrap();
                    sim.tick(clk).unwrap();
                    observe(&mut sim)
                })
                .collect()
        }
        let want = expected();
        for four_state in [false, true] {
            let build = || {
                crate::Simulator::builder(CODE, "Top")
                    .four_state(four_state)
                    .emit_triggers()
            };
            let context = |mode: &str| format!("{mode}, four_state={four_state}");
            assert_eq!(
                timed(build().build_interpreter().unwrap()),
                want,
                "{}",
                context("interp timed")
            );
            assert_eq!(
                ticked(build().build_interpreter().unwrap()),
                want,
                "{}",
                context("interp tick")
            );
            assert_eq!(
                timed(build().build_cranelift().unwrap()),
                want,
                "{}",
                context("cranelift timed")
            );
            assert_eq!(
                ticked(build().build_cranelift().unwrap()),
                want,
                "{}",
                context("cranelift tick")
            );
            assert_eq!(
                timed(build().build_wasm().unwrap()),
                want,
                "{}",
                context("wasm timed")
            );
            assert_eq!(
                ticked(build().build_wasm().unwrap()),
                want,
                "{}",
                context("wasm tick")
            );
            assert_eq!(
                timed(build().build_tiered().unwrap()),
                want,
                "{}",
                context("tiered timed")
            );
            assert_eq!(
                ticked(build().build_tiered().unwrap()),
                want,
                "{}",
                context("tiered tick")
            );
            #[cfg(any(
                all(target_arch = "x86_64", not(feature = "arm64-codegen")),
                all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
            ))]
            {
                assert_eq!(
                    timed(build().build_native().unwrap()),
                    want,
                    "{}",
                    context("native timed")
                );
                assert_eq!(
                    ticked(build().build_native().unwrap()),
                    want,
                    "{}",
                    context("native tick")
                );
            }
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

#[cfg(all(test, feature = "host-runtime"))]
mod runtime_core_tests;

/// Edges of clocks and resets the design drives itself follow IEEE 1800-2023
/// 9.4.2, Table 9-2 in a four-state timed simulation, on every backend: a
/// posedge leaves 0 or reaches 1, a negedge leaves 1 or reaches 0, and a
/// change between x and z is neither.
#[cfg(all(test, feature = "host-runtime"))]
mod four_state_edge_tests {
    use celox_frontend_sdk::{
        BinaryOp, Constant, Edge, EventEdge, EventTerm, ExprId, FrontendArtifact, ModuleBuilder,
        Statement, ValueType,
    };
    use num_bigint::BigUint;

    use super::Simulation;
    use crate::SimBackend;

    /// The levels of a clock walking all twelve changes between 0, 1, x and
    /// z (an Euler circuit from 0), as (value, mask), with the posedges and
    /// negedges counted after each change.
    const WALK: [((u8, u8), u8, u8); 12] = [
        ((1, 0), 1, 0),
        ((0, 0), 1, 1),
        ((1, 1), 2, 1),
        ((0, 0), 2, 2),
        ((0, 1), 3, 2),
        ((1, 0), 4, 2),
        ((1, 1), 4, 3),
        ((1, 0), 5, 3),
        ((0, 1), 5, 4),
        ((1, 1), 5, 4),
        ((0, 1), 5, 4),
        ((0, 0), 5, 5),
    ];

    /// Build `$check` against a timed simulation of each backend.
    macro_rules! on_every_backend {
        ($builder:expr, $check:ident) => {{
            let builder = || $builder.four_state(true).emit_triggers();
            $check(builder().build_interpreter().unwrap());
            $check(builder().build_cranelift().unwrap());
            $check(builder().build_wasm().unwrap());
            $check(builder().build_tiered().unwrap());
            #[cfg(any(
                all(target_arch = "x86_64", not(feature = "arm64-codegen")),
                all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
            ))]
            $check(builder().build_native().unwrap());
        }};
    }

    fn set<B: SimBackend>(sim: &mut Simulation<B>, name: &str, (value, mask): (u8, u8)) {
        let signal = sim.signal(name);
        sim.modify(|io| io.set_four_state(signal, value.into(), mask.into()))
            .unwrap();
    }

    fn get<B: SimBackend>(sim: &mut Simulation<B>, name: &str) -> BigUint {
        let signal = sim.signal(name);
        sim.get(signal)
    }

    /// Two processes drive `cp` and `cn` through [`WALK`], one change per
    /// time unit; registers count the posedges of `cp` and the negedges of
    /// `cn`.
    fn process_design() -> FrontendArtifact {
        let bit = ValueType::new(1, false, true).unwrap();
        let word = ValueType::new(8, false, true).unwrap();
        let mut module = ModuleBuilder::new("Top").unwrap();
        let cp = module.internal("cp", bit).unwrap();
        let cn = module.internal("cn", bit).unwrap();
        let pos = module.output("pos", word).unwrap();
        let neg = module.output("neg", word).unwrap();
        for (signal, width) in [(cp, 1), (cn, 1), (pos, 8), (neg, 8)] {
            module
                .set_initial(signal, Constant::two_state(0u8, width).unwrap())
                .unwrap();
        }
        let one = module.constant(Constant::two_state(1u8, 8).unwrap());
        for (counter, clock, edge) in [(pos, cp, Edge::Posedge), (neg, cn, Edge::Negedge)] {
            let read = module.read(counter).unwrap();
            let next = module.binary(BinaryOp::Add, read, one, word).unwrap();
            let target = module.whole(counter).unwrap();
            module
                .register(target, next, clock, edge, None, None)
                .unwrap();
        }
        let delay = module.constant(Constant::two_state(1u8, 8).unwrap());
        for clock in [cp, cn] {
            let target = module.whole(clock).unwrap();
            let mut body = Vec::new();
            for ((value, mask), _, _) in WALK {
                let level: ExprId = module.constant(Constant::four_state(value, mask, 1).unwrap());
                body.push(Statement::Delay { amount: delay });
                body.push(Statement::Assign {
                    target,
                    value: level,
                });
            }
            module.process(body).unwrap();
        }
        module.finish()
    }

    /// A process drives `d` through [`WALK`] and pulses `clk` after each
    /// change; registers `c` and `c2` follow `d` on the rising edges of
    /// `clk`. Event-control waits `@(posedge c)` and `@(negedge c)` and
    /// registers on the posedges of `c` and the negedges of `c2` count the
    /// edges of the register clock.
    fn wait_design() -> FrontendArtifact {
        let bit = ValueType::new(1, false, true).unwrap();
        let word = ValueType::new(8, false, true).unwrap();
        let mut module = ModuleBuilder::new("Top").unwrap();
        let clk = module.internal("clk", bit).unwrap();
        let d = module.internal("d", bit).unwrap();
        let c = module.internal("c", bit).unwrap();
        let c2 = module.internal("c2", bit).unwrap();
        let counters = ["wait_pos", "wait_neg", "ff_pos", "ff_neg"]
            .map(|name| module.output(name, word).unwrap());
        for signal in [clk, d, c, c2] {
            module
                .set_initial(signal, Constant::two_state(0u8, 1).unwrap())
                .unwrap();
        }
        for counter in counters {
            module
                .set_initial(counter, Constant::two_state(0u8, 8).unwrap())
                .unwrap();
        }
        let d_expr = module.read(d).unwrap();
        for register in [c, c2] {
            let target = module.whole(register).unwrap();
            module
                .register(target, d_expr, clk, Edge::Posedge, None, None)
                .unwrap();
        }
        let one = module.constant(Constant::two_state(1u8, 8).unwrap());
        let increment = |module: &mut ModuleBuilder, counter| {
            let read = module.read(counter).unwrap();
            let next = module.binary(BinaryOp::Add, read, one, word).unwrap();
            (module.whole(counter).unwrap(), next)
        };
        let [wait_pos, wait_neg, ff_pos, ff_neg] = counters;
        for (counter, clock, edge) in [(ff_pos, c, Edge::Posedge), (ff_neg, c2, Edge::Negedge)] {
            let (target, next) = increment(&mut module, counter);
            module
                .register(target, next, clock, edge, None, None)
                .unwrap();
        }
        for (counter, edge) in [
            (wait_pos, EventEdge::Posedge),
            (wait_neg, EventEdge::Negedge),
        ] {
            let (target, value) = increment(&mut module, counter);
            module
                .process(vec![Statement::Forever {
                    body: vec![
                        Statement::WaitEvent {
                            terms: vec![EventTerm { signal: c, edge }],
                        },
                        Statement::Assign { target, value },
                    ],
                }])
                .unwrap();
        }
        let delay = module.constant(Constant::two_state(1u8, 8).unwrap());
        let high = module.constant(Constant::two_state(1u8, 1).unwrap());
        let low = module.constant(Constant::two_state(0u8, 1).unwrap());
        let d_target = module.whole(d).unwrap();
        let clk_target = module.whole(clk).unwrap();
        let mut body = Vec::new();
        for ((value, mask), _, _) in WALK {
            let level = module.constant(Constant::four_state(value, mask, 1).unwrap());
            body.push(Statement::Assign {
                target: d_target,
                value: level,
            });
            body.push(Statement::Delay { amount: delay });
            body.push(Statement::Assign {
                target: clk_target,
                value: high,
            });
            body.push(Statement::Delay { amount: delay });
            body.push(Statement::Assign {
                target: clk_target,
                value: low,
            });
        }
        module.process(body).unwrap();
        module.finish()
    }

    /// Event-control waits and flip-flop domains see the same Table 9-2
    /// edges of a register clock.
    #[test]
    fn waits_on_register_clocks_follow_table_9_2() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            for (step, (_, pos, neg)) in (0u64..).zip(WALK) {
                let time = 2 * step + 1;
                sim.run_until(time).unwrap();
                let counts =
                    ["wait_pos", "wait_neg", "ff_pos", "ff_neg"].map(|name| get(&mut sim, name));
                assert_eq!(
                    counts,
                    [pos, neg, pos, neg].map(BigUint::from),
                    "wait_pos, wait_neg, ff_pos, ff_neg at {time}"
                );
            }
        }
        on_every_backend!(Simulation::from_frontend(wait_design()), check);
    }

    #[test]
    fn process_driven_clocks_follow_table_9_2() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            for (time, (_, pos, neg)) in (1..).zip(WALK) {
                sim.run_until(time).unwrap();
                assert_eq!(get(&mut sim, "pos"), pos.into(), "posedges at {time}");
                assert_eq!(get(&mut sim, "neg"), neg.into(), "negedges at {time}");
            }
        }
        on_every_backend!(Simulation::from_frontend(process_design()), check);
    }

    const REGISTER_CLOCK: &str = r#"
module Top (
    clk: input '_ clock,
    rst: input '_ reset_async_low,
    d  : input logic,
    pos: output logic<8>,
    neg: output logic<8>,
) {
    var c: logic;
    always_ff (clk) {
        c = d;
    }
    let cp: '_ clock         = c;
    let cn: '_ clock_negedge = c;
    var p : logic<8>;
    var n : logic<8>;
    always_ff (cp, rst) {
        if_reset {
            p = 0;
        } else {
            p = p + 8'd1;
        }
    }
    always_ff (cn, rst) {
        if_reset {
            n = 0;
        } else {
            n = n + 8'd1;
        }
    }
    assign pos = p;
    assign neg = n;
}
"#;

    /// A clock that is a register follows `d` through [`WALK`], one change
    /// per rising edge of `clk`. `clk` starts at x, so its first rise, x to
    /// 1, is a posedge too.
    #[test]
    fn register_clocks_follow_table_9_2() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            // x -> 0 asserts the reset.
            sim.schedule("rst", 0, 0).unwrap();
            sim.schedule("rst", 1, 1).unwrap();
            set(&mut sim, "d", (0, 0));
            sim.add_clock("clk", 10, 5);
            // The register leaves x for 0: a negedge.
            sim.run_until(5).unwrap();
            assert_eq!(get(&mut sim, "pos"), 0u8.into());
            assert_eq!(get(&mut sim, "neg"), 1u8.into());
            for (time, (level, pos, neg)) in (15..).step_by(10).zip(WALK) {
                set(&mut sim, "d", level);
                sim.run_until(time).unwrap();
                assert_eq!(get(&mut sim, "pos"), pos.into(), "posedges at {time}");
                assert_eq!(get(&mut sim, "neg"), (neg + 1).into(), "negedges at {time}");
            }
        }
        on_every_backend!(Simulation::builder(REGISTER_CLOCK, "Top"), check);
    }

    /// A clock computed combinationally from a register, `e ? a & b : 'z`,
    /// with the register written whole so the clock changes once per edge.
    #[test]
    fn combinational_clocks_follow_table_9_2() {
        const SOURCE: &str = r#"
module Top (
    clk: input '_ clock,
    rst: input '_ reset_async_low,
    d  : input logic<3>,
    pos: output logic<8>,
    neg: output logic<8>,
) {
    var r: logic<3>;
    always_ff (clk) {
        r = d;
    }
    let cp: '_ clock         = if r[2] ? r[1] & r[0] : 1'bz;
    let cn: '_ clock_negedge = if r[2] ? r[1] & r[0] : 1'bz;
    var p : logic<8>;
    var n : logic<8>;
    always_ff (cp, rst) {
        if_reset {
            p = 0;
        } else {
            p = p + 8'd1;
        }
    }
    always_ff (cn, rst) {
        if_reset {
            n = 0;
        } else {
            n = n + 8'd1;
        }
    }
    assign pos = p;
    assign neg = n;
}
"#;
        /// `{e, a, b}` for each level of [`WALK`], as (value, mask).
        const INPUTS: [(u8, u8); 12] = [
            (0b111, 0),
            (0b100, 0),
            (0b110, 0b001), // 1 & z
            (0b110, 0),
            (0b011, 0), // e = 0
            (0b111, 0),
            (0b111, 0b010), // 1 & x
            (0b111, 0),
            (0b000, 0),     // e = 0
            (0b111, 0b100), // e = x
            (0b001, 0b010), // e = 0
            (0b101, 0),
        ];
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.schedule("rst", 0, 0).unwrap();
            sim.schedule("rst", 1, 1).unwrap();
            set(&mut sim, "d", (0b101, 0));
            sim.add_clock("clk", 10, 5);
            sim.run_until(5).unwrap();
            assert_eq!(get(&mut sim, "pos"), 0u8.into());
            assert_eq!(get(&mut sim, "neg"), 1u8.into());
            for (time, (input, (_, pos, neg))) in
                (15..).step_by(10).zip(INPUTS.into_iter().zip(WALK))
            {
                set(&mut sim, "d", input);
                sim.run_until(time).unwrap();
                assert_eq!(get(&mut sim, "pos"), pos.into(), "posedges at {time}");
                assert_eq!(get(&mut sim, "neg"), (neg + 1).into(), "negedges at {time}");
            }
        }
        on_every_backend!(Simulation::builder(SOURCE, "Top"), check);
    }

    /// Asynchronous resets that are registers. An edge to x or z runs the
    /// register's clocked branch (+1), an edge to the active level clears
    /// it, and every clear follows an increment.
    #[test]
    fn register_async_resets_follow_table_9_2() {
        const SOURCE: &str = r#"
module Top (
    clk   : input '_ clock,
    k     : input '_ clock,
    clr   : input logic,
    dh    : input logic,
    dl    : input logic,
    hits_h: output logic<8>,
    hits_l: output logic<8>,
) {
    var qh: logic;
    var ql: logic;
    always_ff (clk) {
        qh = dh;
        ql = dl;
    }
    let rh: '_ reset_async_high = qh;
    let rl: '_ reset_async_low  = ql;
    var h : logic<8>;
    var l : logic<8>;
    always_ff (k, rh) {
        if_reset {
            h = 0;
        } else if clr {
            h = 8'h80;
        } else {
            h = h + 8'h01;
        }
    }
    always_ff (k, rl) {
        if_reset {
            l = 0;
        } else if clr {
            l = 8'h80;
        } else {
            l = l + 8'h01;
        }
    }
    assign hits_h = h;
    assign hits_l = l;
}
"#;
        /// `rh` levels 1, 0, x, 1, x, 0, z, 1, z, x, z, 0 (`rl` has 0 and 1
        /// swapped), and the counter after each change.
        const STEPS: [((u8, u8), u8); 12] = [
            ((1, 0), 0),
            ((0, 0), 0),
            ((1, 1), 1),
            ((1, 0), 0),
            ((1, 1), 0),
            ((0, 0), 0),
            ((0, 1), 1),
            ((1, 0), 0),
            ((0, 1), 0),
            ((1, 1), 0),
            ((0, 1), 0),
            ((0, 0), 0),
        ];
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            set(&mut sim, "dh", (0, 0));
            set(&mut sim, "dl", (1, 0));
            set(&mut sim, "clr", (1, 0));
            sim.schedule("k", 0, 0).unwrap();
            sim.add_clock("clk", 10, 5);
            // x -> 0 is no posedge of `rh`, x -> 1 no negedge of `rl`.
            sim.schedule("k", 7, 1).unwrap();
            sim.run_until(7).unwrap();
            set(&mut sim, "clr", (0, 0));
            assert_eq!(get(&mut sim, "hits_h"), 0x80u8.into());
            assert_eq!(get(&mut sim, "hits_l"), 0x80u8.into());
            for (time, ((value, mask), hits)) in (15..).step_by(10).zip(STEPS) {
                let low = if mask == 0 { value ^ 1 } else { value };
                set(&mut sim, "dh", (value, mask));
                set(&mut sim, "dl", (low, mask));
                sim.run_until(time).unwrap();
                assert_eq!(get(&mut sim, "hits_h"), hits.into(), "rh at {time}");
                assert_eq!(get(&mut sim, "hits_l"), hits.into(), "rl at {time}");
            }
        }
        on_every_backend!(Simulation::builder(SOURCE, "Top"), check);
    }

    /// The edge baseline of a clock left at x survives a checkpoint: the
    /// following rise, x to 1, is a posedge.
    #[test]
    fn unknown_edge_baselines_survive_checkpoints() {
        fn check<B: SimBackend>(mut sim: Simulation<B>) {
            sim.schedule("rst", 0, 0).unwrap();
            sim.schedule("rst", 1, 1).unwrap();
            set(&mut sim, "d", (1, 1));
            sim.add_clock("clk", 10, 5);
            // The register leaves x for x: no edge.
            sim.run_until(5).unwrap();
            assert_eq!(get(&mut sim, "pos"), 0u8.into());
            let saved = sim.checkpoint().unwrap();
            let state = sim.save_state().unwrap();
            set(&mut sim, "d", (1, 0));
            sim.run_until(15).unwrap();
            assert_eq!(get(&mut sim, "pos"), 1u8.into());
            sim.restore(&saved).unwrap();
            set(&mut sim, "d", (1, 0));
            sim.run_until(15).unwrap();
            assert_eq!(get(&mut sim, "pos"), 1u8.into(), "after a restore");
            sim.load_state(&state).unwrap();
            set(&mut sim, "d", (1, 0));
            sim.run_until(15).unwrap();
            assert_eq!(get(&mut sim, "pos"), 1u8.into(), "after a state load");
        }
        on_every_backend!(Simulation::builder(REGISTER_CLOCK, "Top"), check);
    }
}
