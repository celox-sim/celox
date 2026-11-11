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

mod checkpoint_file;
pub use checkpoint_file::CheckpointFileError;
mod differential;
mod vcd_tasks;
mod waveform_state;
pub use waveform_state::SourceWaveformState;

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
/// What a host observes inside the time slots of a [`Simulation`], in
/// addition to the state between steps: changes of its signals before the
/// flip-flop domains commit, and the settled slot.
///
/// A slot observer is how a host such as a VPI driver keeps the regions of
/// IEEE 1800-2023 4.4 inside one time slot. Its writes are blocking writes
/// of the host at the slot's time, like [`Simulation::modify`].
pub trait SlotObserver<B: SimBackend> {
    /// A round of the slot has run its processes and scheduled values and
    /// is about to evaluate its flip-flop domains: the round's edges are in
    /// state and no nonblocking update has been made. Writes are in state
    /// before the domains evaluate.
    fn before_domains(&mut self, io: &mut crate::SlotIo<'_, B>, time: u64);

    /// The slot has settled: every region is empty. Writes are updates of
    /// `time`, which the simulation settles there before it advances.
    ///
    /// This is where an observer finds the changes of signals that are not
    /// among the event signals it watches: after the nonblocking updates of
    /// the slot and the combinational settle that follows them. That is an
    /// implementation choice, not an order the standard fixes: the observer's
    /// callback and the updates the settle makes are active events of the
    /// same region, which a simulator may process in any order (IEEE
    /// 1800-2023 4.7).
    fn end_of_slot(&mut self, io: &mut crate::SlotIo<'_, B>, time: u64);

    /// Whether the simulation shows the observer every update of a slot
    /// through [`Self::after_update`]. Asked when the observer is attached.
    fn observes_updates(&self) -> bool {
        false
    }

    /// An update of the slot may have changed a watched value: a process
    /// ran until it suspended, flip-flop domains committed, nonblocking
    /// assignments were published, scheduled values applied, or
    /// combinational logic settled. A value change callback runs here, after
    /// the change and before the simulation goes on (IEEE 1800-2023 38.36.1,
    /// 4.10). Writes are blocking writes of the host at `time`.
    fn after_update(&mut self, _io: &mut crate::SlotIo<'_, B>, _time: u64) {}
}

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
        self.run_process_checked(index)?;
        self.runtime_finish()
    }

    fn enter_deferred_assertion_process(&mut self, process: usize) {
        self.enter_deferred_process(process);
    }
    fn leave_deferred_assertion_process(&mut self) {
        self.leave_deferred_process();
    }
    fn finish_deferred_assertion_process(&mut self, process: usize) {
        self.finish_deferred_process(process);
    }
    fn cancel_deferred_assertion_process(&mut self, process: usize) {
        self.cancel_deferred_process(process);
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
        // Runtime declarations may also run on the first deferred step. Initialize
        // sampled copies and history defaults then (IEEE 1800-2023 16.5.1).
        // Restoring a completed declaration phase never calls this hook.
        self.initialize_copies();
        Ok(())
    }

    fn set_compiled_host_budgets(&mut self, rounds: u64, returns: u64) {
        self.native.return_budget = returns;
        self.native.round_budget = rounds;
    }

    fn compiled_host_inputs(&self) -> bool {
        self.compiled_nba_queues()
    }

    fn put_compiled_host_inputs(
        &mut self,
        pending: bool,
        writes: Vec<(SignalRef, celox_runtime::timing::Value)>,
    ) -> Result<(), celox_runtime::SimulatorErrorCode> {
        self.put_native_host_inputs(pending, writes);
        Ok(())
    }

    fn compiled_region_queues(&self) -> bool {
        self.compiled_nba_queues()
    }

    fn put_compiled_region_entry(&mut self, pending: bool) {
        self.set_native_region_entry(pending);
    }

    fn compiled_ready_queue(&self) -> bool {
        self.compiled_nba_queues()
    }

    fn compiled_nba_queues(&self) -> bool {
        cfg!(all(target_arch = "x86_64", not(feature = "arm64-codegen")))
            && self.has_native_scheduler_services()
    }

    fn put_compiled_nbas(
        &mut self,
        ready: Vec<celox_runtime::NbaUpdate>,
        delayed: Vec<(u64, celox_runtime::NbaUpdate)>,
    ) -> Result<(), celox_runtime::SimulatorErrorCode> {
        self.native.nba_queue.extend(ready);
        self.native.delayed_nbas.extend(delayed);
        Ok(())
    }

    fn take_compiled_delayed_nbas(&mut self) -> Vec<(u64, celox_runtime::NbaUpdate)> {
        self.native.delayed_nbas.drain(..).collect()
    }

    fn has_compiled_nbas(&self) -> bool {
        !self.native.nba_queue.is_empty() || !self.native.delayed_nbas.is_empty()
    }

    fn take_compiled_nbas(&mut self) -> Vec<celox_runtime::NbaUpdate> {
        self.native.nba_queue.drain(..).collect()
    }

    fn host_request(
        &mut self,
        request: &celox_design::HostRequestSite<SignalRef>,
        timestamp: u64,
    ) -> Result<(), Halt> {
        match request.kind {
            celox_design::HostRequestKind::Process | celox_design::HostRequestKind::Testbench => {
                Err(celox_runtime::SimulatorErrorCode::InternalError.into())
            }
            celox_design::HostRequestKind::Plusargs => {
                self.run_plusargs_request(request, timestamp)
            }
            celox_design::HostRequestKind::Distribution => {
                self.run_distribution_request(request, timestamp)
            }
            celox_design::HostRequestKind::FileIo => self.run_file_io_request(request, timestamp),
            celox_design::HostRequestKind::Vcd => self.run_vcd_request(request, timestamp),
            celox_design::HostRequestKind::Scan => self.run_scan_request(request, timestamp),
            celox_design::HostRequestKind::Format => self.run_format_request(request, timestamp),
            celox_design::HostRequestKind::System => self.run_system_request(request),
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

    fn before_domains(&mut self, timestamp: u64) -> Result<bool, Halt> {
        if self.frozen {
            return Ok(false);
        }
        self.show_slot(timestamp, |observer, io, time| {
            observer.before_domains(io, time)
        })
    }

    fn take_slot_writes(&mut self) -> bool {
        std::mem::take(&mut self.observation.writes)
    }

    #[inline]
    fn observes_updates(&self) -> bool {
        self.observation.observes_updates && !self.frozen
    }

    fn after_update(&mut self, timestamp: u64) -> Result<bool, Halt> {
        // The value change points of the kernels since the last update
        // point: the observer may have written there, or asked to finish.
        let changed = std::mem::take(&mut self.observation.value_changed);
        if changed & 4 != 0 {
            return Err(Halt::Finish);
        }
        let wrote = self.show_slot(timestamp, |observer, io, time| {
            observer.after_update(io, time)
        })?;
        Ok(wrote || changed & 2 != 0)
    }

    fn await_runtime_event_room(&mut self) {
        Simulator::await_runtime_event_room(self);
    }

    fn compiled_slots(&self) -> celox_runtime::CompiledSlots {
        use celox_runtime::CompiledSlots;
        // A scheduler generated without update points cannot show the
        // host each update.
        if self.frozen
            || (self.observation.observes_updates
                && !self
                    .program
                    .runtime_schema
                    .scheduler
                    .as_ref()
                    .is_some_and(|scheduler| scheduler.update_points))
            || !self.components.is_empty()
            || !self.backend.runs_compiled_scheduler()
        {
            return CompiledSlots::None;
        }
        // Waveforms record, the heap collects, deferred assertions mature
        // and a slot observer looks at every slot boundary.
        let vcd = &self.sv_vcd;
        if self.slot_end.postponed_output
            || self.slot_end.deferred_assertions
            || self.observation.observer.is_some()
            || self.vcd_writer.is_some()
            || vcd.path.is_some()
            || vcd.started.is_some()
            || vcd.pending
            || !self.handle_slots().is_empty()
            // A fused flip-flop's delayed updates are collected by the
            // runtime at each slot boundary, at their capture time, when
            // no generated queue publishes them instead.
            || (!self.compiled_nba_queues()
                && self
                    .program
                    .runtime_schema
                    .nba_sites
                    .iter()
                    .any(|site| site.unit))
        {
            return CompiledSlots::One;
        }
        CompiledSlots::Any
    }

    fn retain_process_values(&mut self, values: &[(SignalRef, celox_runtime::timing::Value)]) {
        let slots = self.handle_slots();
        self.heap.process_roots.clear();
        for (signal, (value, _)) in values {
            let end = signal.offset + signal.width.div_ceil(8);
            for &(offset, kind) in slots_within(&slots, signal.offset, end) {
                let bytes = (value >> ((offset - signal.offset) * 8)).to_bytes_le();
                let mut raw = [0; celox_heap::HANDLE_BYTES];
                let count = bytes.len().min(raw.len());
                raw[..count].copy_from_slice(&bytes[..count]);
                self.heap.process_roots.push(celox_heap::Root {
                    kind,
                    handle: celox_heap::Handle::from_le_bytes(raw),
                });
            }
        }
    }

    fn retain_call_stacks(&mut self, stacks: &[(usize, &celox_runtime::SavedCalls)]) {
        for &(kernel, saved) in stacks {
            let handles = self.saved_frame_handles(kernel, saved);
            debug_assert!(handles.is_some(), "the saved frames end with the stack");
            for (offset, kind) in handles.unwrap_or_default() {
                let kind = match kind {
                    celox_design::ValueKind::StrRef => celox_heap::HandleKind::String,
                    celox_design::ValueKind::ObjRef => celox_heap::HandleKind::Object,
                    celox_design::ValueKind::Bits => continue,
                };
                let mut raw = [0; celox_heap::HANDLE_BYTES];
                raw.copy_from_slice(&saved.stack[offset..offset + celox_heap::HANDLE_BYTES]);
                self.heap.process_roots.push(celox_heap::Root {
                    kind,
                    handle: celox_heap::Handle::from_le_bytes(raw),
                });
            }
        }
    }

    fn finish_timed_step(&mut self, timestamp: u64) {
        self.dirty = false;
        self.dump_unless_rewound(timestamp);
    }

    /// Combinational logic still to settle belongs to `timestamp`, so its
    /// reports read that time: settle it before the run moves on.
    fn try_leave_time(&mut self, _timestamp: u64) -> Result<(), Halt> {
        if self.timestamped_reports && self.dirty {
            self.eval_comb_checked().map_err(Halt::Error)?;
            self.dirty = false;
            self.runtime_finish()?;
        }
        Ok(())
    }

    fn try_observed_deferred_assertions(&mut self, _timestamp: u64) -> Result<(), Halt> {
        self.mature_deferred_assertion_region(celox_design::AssertionDeferral::Observed)
            .map_err(Halt::Error)
    }

    /// Final deferred assertions mature in Postponed, after Reactive activity.
    fn try_end_time_slot(&mut self, _timestamp: u64) -> Result<(), Halt> {
        self.mature_deferred_assertion_region(celox_design::AssertionDeferral::Final)
            .map_err(Halt::Error)?;
        self.run_strobes(_timestamp)
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
            .map_err(|error| Halt::Error(vcd_dump_runtime_error(error)))?;
        if !self.frozen {
            self.observation.writes |= self.show_slot(timestamp, |observer, io, time| {
                observer.end_of_slot(io, time)
            })?;
        }
        Ok(())
    }
}

impl<B: SimBackend> Simulator<B> {
    /// Show the slot observer, if any, the state at `time`. Returns whether
    /// it wrote.
    fn show_slot(
        &mut self,
        time: u64,
        show: impl FnOnce(&mut dyn SlotObserver<B>, &mut crate::SlotIo<'_, B>, u64),
    ) -> Result<bool, Halt> {
        let Some(observer) = self.observation.observer.as_deref_mut() else {
            return Ok(false);
        };
        let mut io = crate::SlotIo::new(crate::IOContext {
            backend: &mut self.backend,
            watch: &mut self.host_watch,
        });
        show(observer, &mut io, time);
        let wrote = io.wrote();
        self.dirty |= wrote;
        if io.finish_requested() {
            return Err(Halt::Finish);
        }
        Ok(wrote)
    }
}

pub(crate) fn vcd_dump_runtime_error(error: crate::simulator::DumpError) -> RuntimeErrorCode {
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
    let mut state = SimulationState::new(
        &simulator.backend,
        ResolvedSchedule::resolve(&simulator.backend, &simulator.schedule),
    );
    state.set_process_names(
        simulator
            .program
            .runtime_schema
            .process_origins
            .iter()
            .map(ToString::to_string)
            .collect(),
    );
    state
}

impl<B: SimBackend> Simulation<B> {
    #[cfg(test)]
    pub(crate) fn new(simulator: Simulator<B>) -> Self {
        Self::try_new(simulator).expect("runtime static declaration initialization failed")
    }

    #[cfg(test)]
    pub(crate) fn try_new(simulator: Simulator<B>) -> Result<Self, RuntimeErrorCode> {
        Self::try_new_with_initialization(simulator, true)
    }

    pub(crate) fn try_new_with_initialization(
        mut simulator: Simulator<B>,
        initialize: bool,
    ) -> Result<Self, RuntimeErrorCode> {
        let mut state = simulation_state(&simulator);
        simulator.host_watch = host_watch(&simulator.backend, &simulator.schedule);
        // Only a timed simulation ends at an RTL `$finish`; a bare
        // `Simulator`, ticks included, leaves that to the host.
        simulator.finish_sites = simulator.schedule.finish_sites;
        simulator.timed = true;
        simulator.skip_runtime_finish();
        if initialize {
            state.initialize_declarations(&mut simulator)?;
            // Initialize sampled values and histories before the first slot.
            simulator.initialize_copies();
        }
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
    /// Effect-handler state (including file I/O and VPI) is excluded. The host
    /// system coordinates its own handler snapshots with this core checkpoint.
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

    /// File-I/O effect state owned by the embedding system, outside core checkpoints.
    pub fn file_io(&self) -> &celox_runtime::file_io::FileIo {
        self.simulator.file_io()
    }

    /// Install a host-prepared file-I/O handler at an execution boundary.
    pub fn replace_file_io(&mut self, file_io: celox_runtime::file_io::FileIo) {
        self.simulator.replace_file_io(file_io);
    }

    /// The schema used for diagnostic HDL captures, including observer callbacks.
    pub fn state_schema(&self) -> std::sync::Arc<crate::StateSchema> {
        self.simulator.state_schema()
    }

    /// Capture current HDL values without settling or advancing time. The file
    /// has no schedule and cannot resume a timed simulation or a VPI session.
    pub fn capture_state(&self) -> Result<celox_runtime::StateFile, StateError> {
        self.state_schema().capture(&self.simulator.backend)
    }

    /// Save the value of every state object by path, together with the
    /// simulation time, clocks and pending events by name.
    pub fn save_state(&mut self) -> Result<celox_runtime::StateFile, StateError> {
        if self.state.is_suspended() {
            return Err(CheckpointError::Stopped.into());
        }
        if let Err(error) = self.simulator.prepare_save_state() {
            if let StateError::Runtime(error) = &error {
                self.finalize_fatal(error);
            }
            return Err(error);
        }
        // Settling for the file may have executed a `$finish`.
        self.sync_finish_checked().map_err(StateError::Runtime)?;
        // A final procedure can write state or suspend with `$stop`.
        if self.state.is_suspended() {
            return Err(CheckpointError::Stopped.into());
        }
        let mut file = self
            .simulator
            .state_schema()
            .capture(&self.simulator.backend)?;
        let parts = self.state.export_schedule(&self.simulator.backend);
        let mut record = self.simulator.name_schedule(parts, &mut file.strings)?;
        record.processes.host_writes = self.simulator.name_host_writes();
        self.simulator
            .validate_activation_queue_values(&record.processes)?;
        file.schedule = Some(record);
        Ok(file)
    }

    /// Load a state file saved from a `Simulation`, including its time,
    /// clocks and pending events. See [`Simulator::load_state`] for how
    /// objects are matched.
    pub fn load_state(&mut self, file: &celox_runtime::StateFile) -> Result<(), StateError> {
        let record = file.schedule.as_ref().ok_or(StateError::MissingSchedule)?;
        self.simulator.check_vcd_rewind(record.time)?;
        self.simulator
            .validate_activation_queue_values(&record.processes)?;
        let mut parts = self.simulator.resolve_schedule(record, file)?;
        let host_writes = self
            .simulator
            .resolve_host_writes(&record.processes.host_writes)?;
        self.simulator.load_state_at_time(file, record.time)?;
        self.simulator.bind_activation_strings(
            &mut parts.process_state,
            &record.processes,
            &file.strings,
        );
        self.state.import_schedule(parts);
        self.state.retain_activation_roots(&mut self.simulator);
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
        self.simulator
            .restore_checkpoint_prepared(&checkpoint.simulator)?;
        self.state.retain_activation_roots(&mut self.simulator);
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
            self.finish()?;
        }
        Ok(())
    }

    /// A fatal report implicitly calls `$finish`; retain the original diagnostic
    /// even if a final procedure or output flush fails during termination.
    fn finalize_fatal(&mut self, error: &RuntimeErrorCode) {
        if matches!(error, RuntimeErrorCode::Fatal { .. }) {
            let _ = self.finish();
        }
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
        if let Err(error) = self.try_dump(timestamp) {
            panic!("waveform dump at {timestamp} failed: {error}");
        }
    }

    /// See [`Simulator::try_dump`].
    pub fn try_dump(&mut self, timestamp: u64) -> Result<(), crate::simulator::DumpError> {
        let result = self.simulator.try_dump(timestamp);
        if let Err(crate::simulator::DumpError::Runtime(error)) = &result {
            self.finalize_fatal(error);
        }
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

    /// The stored four-state `(value, mask)` of `signal`, without settling
    /// combinational logic first: what host writes and the last settle left.
    pub fn peek_four_state(&self, signal: SignalRef) -> (num_bigint::BigUint, num_bigint::BigUint) {
        self.simulator.backend.get_four_state(signal)
    }

    /// Read the bytes of a string slot without settling or advancing time.
    pub fn peek_string(&self, signal: SignalRef) -> Vec<u8> {
        let backend = &self.simulator.backend;
        backend
            .heap()
            .string(backend.get_as::<u32>(signal))
            .to_vec()
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
        // The tracked writes counted their occurrences of gated waits.
        self.state
            .note_gated_host_writes(&mut self.simulator.backend, false);
        if let Err(error) = &result {
            self.finalize_fatal(error);
        }
        let finished = self.sync_finish_checked();
        result?;
        finished?;
        let logged = self.simulator.host_watch.has_log();
        self.state.note_host_writes(&self.simulator.backend, logged);
        Ok(())
    }

    /// Show every time slot to `observer`, which watches `signals`.
    ///
    /// The slots in which a compiled process kernel or host clock may change
    /// an event signal among `signals` run in the reference loop, where
    /// [`SlotObserver::before_domains`] sees the change before any domain
    /// commits; the other kernels' slots stay compiled, one slot per run so
    /// that [`SlotObserver::end_of_slot`] sees each of them. Changes of other
    /// signals are the observer's to find, at either point. A differential
    /// simulation stops checking: the shadow does not see the observer's
    /// writes.
    pub fn observe_slots(&mut self, observer: Box<dyn SlotObserver<B>>, signals: &[SignalRef]) {
        self.simulator.observation.observes_updates = observer.observes_updates();
        self.simulator.observation.observer = Some(observer);
        let signals = self.reference_observed(signals);
        self.state.set_observed_signals(signals.iter().copied());
        if self.shadow.take().is_some() {
            self.unchecked = Some("a slot observer writes state the shadow does not see");
        }
    }

    /// Replace the signals the observer of [`Self::observe_slots`] watches.
    pub fn set_observed_signals(&mut self, signals: &[SignalRef]) {
        let signals = self.reference_observed(signals);
        self.state.set_observed_signals(signals.iter().copied());
    }

    /// The observed signals whose changes keep slots in the reference loop:
    /// none when the compiled scheduler shows the observer every update.
    fn reference_observed<'a>(&self, signals: &'a [SignalRef]) -> &'a [SignalRef] {
        let shown = self.simulator.observation.observes_updates
            && self
                .simulator
                .program
                .runtime_schema
                .scheduler
                .as_ref()
                .is_some_and(|scheduler| scheduler.update_points);
        if shown { &[] } else { signals }
    }

    /// Stop showing the time slots to the observer of
    /// [`Self::observe_slots`], and return it.
    pub fn stop_observing_slots(&mut self) -> Option<Box<dyn SlotObserver<B>>> {
        self.state.set_observed_signals([]);
        self.simulator.observation.observes_updates = false;
        self.simulator.observation.observer.take()
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
        // With nothing scheduled, the step ends the slot too.
        let was_stopped = self.is_stopped();
        self.end_idle_slot(self.state.next_event_time().unwrap_or(u64::MAX))?;
        if !was_stopped && self.is_stopped() {
            return Ok(Some(self.time()));
        }
        self.step_until(u64::MAX)
    }

    /// A slot with nothing due is over when a run leaves its time. In a
    /// design with deferred assertions, logic that host writes or the
    /// initial settle left dirty settles there, and the actions waiting from
    /// any settle at that time mature, so that a deferred `$fatal` ends the
    /// run at that time.
    #[inline]
    fn end_idle_slot(&mut self, end_time: u64) -> Result<(), RuntimeErrorCode> {
        if (!self.simulator.slot_end.work && !self.simulator.timestamped_reports)
            || end_time <= self.state.time()
            || self.state.next_event_time() == Some(self.state.time())
        {
            return Ok(());
        }
        self.settle_idle_slot()
    }

    #[cold]
    #[inline(never)]
    fn settle_idle_slot(&mut self) -> Result<(), RuntimeErrorCode> {
        let result = if self.simulator.dirty {
            let result = self.simulator.eval_comb_checked();
            self.simulator.dirty = false;
            // A `$finish` of the settle finishes the simulation here.
            result.and_then(|()| self.sync_finish_checked())
        } else {
            Ok(())
        };
        if let Err(error) = result
            .and_then(|()| self.simulator.mature_deferred_assertions())
            .and_then(|()| {
                self.simulator
                    .run_strobes(self.state.time())
                    .map_err(|halt| match halt {
                        Halt::Error(error) => error,
                        _ => RuntimeErrorCode::InternalError,
                    })
            })
        {
            self.finalize_fatal(&error);
            self.flush_error_output();
            return Err(error);
        }
        Ok(())
    }

    /// [`Self::step`] while [`Self::set_fail_on`] is set: a slot that only
    /// settles host writes is watched before the step goes on to the next
    /// time.
    #[cold]
    #[inline(never)]
    fn watched_step(&mut self) -> Result<Option<u64>, RuntimeErrorCode> {
        let mut settled = None;
        while let Some(next_time) = self.state.next_event_time() {
            if next_time > self.state.time() {
                self.watch_settle()?;
                if self.is_finished() || self.is_stopped() || self.reports.failure.is_some() {
                    return Ok(Some(self.state.time()));
                }
            }
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
    /// step stopped at `$stop` or at a failure, or `max_steps` ran out or
    /// `interrupt` returned `true` before a step; `steps` counts the steps.
    #[cold]
    #[inline(never)]
    fn watched_steps(
        &mut self,
        end_time: u64,
        max_steps: u64,
        steps: &mut u64,
        interrupt: &mut dyn FnMut() -> bool,
    ) -> Result<bool, RuntimeErrorCode> {
        while let Some(next_time) = self.state.next_event_time() {
            if next_time > end_time {
                break;
            }
            if *steps >= max_steps || interrupt() {
                return Ok(false);
            }
            if next_time > self.state.time() {
                self.watch_settle()?;
                if self.is_finished() || self.is_stopped() || self.reports.failure.is_some() {
                    return Ok(false);
                }
            }
            // One slot per step, so that a slot that only settles host
            // writes is watched before the next runs.
            self.step_until(next_time)?;
            self.watch_reports();
            *steps += 1;
            if self.state.is_stopped() || self.reports.failure.is_some() {
                return Ok(false);
            }
        }
        self.watch_settle()?;
        Ok(!self.is_stopped() && self.reports.failure.is_none())
    }

    /// Settle combinational logic that no step settled, as after host writes
    /// that wake no process, at the current time, and watch its reports. A
    /// watched run does this before it moves to a later time.
    fn watch_settle(&mut self) -> Result<(), RuntimeErrorCode> {
        if self.state.is_finished() {
            return Ok(());
        }
        // The run leaves the time or ends at it: the slot is over, and the
        // deferred actions waiting from any settle at that time mature
        // before the reports are watched.
        let mature = self.simulator.slot_end.deferred_assertions
            && self.state.next_event_time() != Some(self.state.time());
        if !self.simulator.dirty && !mature {
            return Ok(());
        }
        if self.simulator.dirty {
            if let Err(error) = self
                .simulator
                .eval_comb_checked()
                .and_then(|()| self.sync_finish_checked())
            {
                self.finalize_fatal(&error);
                self.flush_error_output();
                return Err(error);
            }
            self.simulator.dirty = false;
        }
        let matured = if mature {
            self.simulator.mature_deferred_assertions()
        } else {
            Ok(())
        };
        if let Err(error) = &matured {
            self.finalize_fatal(error);
        }
        self.watch_reports();
        if let Err(error) = matured {
            self.flush_error_output();
            return Err(error);
        }
        Ok(())
    }

    /// Stop [`Self::run_until`] and [`Self::run_until_bounded`] after the
    /// time step of the first report of at least `severity`, which
    /// [`Self::failure`] then returns. While it is set, the events of each
    /// step are taken from the design's event buffer after the step and keep
    /// the time of that step, so none are missed and their reports have an
    /// exact time. They wait for [`Self::drain_runtime_events`]. `None`
    /// stops only at `$fatal`, as before.
    ///
    /// # Panics
    ///
    /// When `severity` is set while a [`crate::RuntimeEventDrain`] is
    /// active: the drain would take the reports to watch.
    pub fn set_fail_on(&mut self, severity: Option<crate::Severity>) {
        assert!(
            severity.is_none() || !self.simulator.runtime_event_drain_active(),
            "cannot watch reports while a RuntimeEventDrain is active",
        );
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
        if self.reports.fail_on.is_some() {
            if !self.watched_steps(end_time, u64::MAX, &mut 0, &mut || false)? {
                return Ok(());
            }
        } else {
            let was_stopped = self.is_stopped();
            self.end_idle_slot(end_time)?;
            if !was_stopped && self.is_stopped() {
                return Ok(());
            }
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
            if let crate::simulator::DumpError::Runtime(runtime_error) = &error {
                self.finalize_fatal(runtime_error);
                self.flush_error_output();
            }
            return Err(vcd_dump_runtime_error(error));
        }
        Ok(())
    }

    /// Run every time slot before `time`, then move to `time` without
    /// running its slot: the next [`Self::step`] or [`Self::run_until`]
    /// runs it, together with the host writes made in between. A target not
    /// after the current time changes nothing. Like [`Self::run_until`], it
    /// returns early, before `time`, at a `$stop` or `$finish`.
    pub fn advance_to(&mut self, time: u64) -> Result<(), RuntimeErrorCode> {
        let Some(last) = time.checked_sub(1).filter(|&last| last >= self.time()) else {
            return Ok(());
        };
        self.run_until(last)?;
        if self.time() != last || self.is_finished() || self.is_stopped() {
            return Ok(());
        }
        // Leaving `last` ends its slot.
        if self.reports.fail_on.is_some() {
            self.watch_settle()?;
            if self.reports.failure.is_some() {
                return Ok(());
            }
        } else {
            self.end_idle_slot(time)?;
        }
        if self.is_finished() || self.is_stopped() {
            return Ok(());
        }
        self.state.advance_time(&mut self.simulator.backend, time);
        if let Some(shadow) = &mut self.shadow {
            shadow
                .state
                .advance_time(&mut shadow.simulator.backend, time);
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
            if !self.watched_steps(end_time, max_steps, &mut 0, &mut || false)? {
                return Ok(false);
            }
            self.run_until(end_time)?;
            return Ok(!self.state.is_stopped());
        }
        let was_stopped = self.is_stopped();
        self.end_idle_slot(end_time)?;
        if !was_stopped && self.is_stopped() {
            return Ok(false);
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

    /// Run time slots, compiled where the scheduler can, until the
    /// simulation finishes or stops, nothing is scheduled, `max_steps` steps
    /// have run, or `interrupt` returns `true` before a step. A run of the
    /// compiled scheduler counts a step for each slot it runs, and runs at
    /// most 65536 of them before `interrupt` is asked again. Unlike [`Self::run_until`], the simulation
    /// stays at the time of its last step. Returns the steps run.
    pub fn run_slots(
        &mut self,
        max_steps: u64,
        mut interrupt: impl FnMut() -> bool,
    ) -> Result<u64, RuntimeErrorCode> {
        self.reports.failure = None;
        let mut steps = 0;
        // Reports to watch take a slot at a time, settling what no step
        // settles before the run ends.
        if self.reports.fail_on.is_some() {
            self.watched_steps(u64::MAX, max_steps, &mut steps, &mut interrupt)?;
            return Ok(steps);
        }
        // So does a shadow to compare.
        if self.shadow.is_some() {
            while steps < max_steps
                && !self.is_finished()
                && self.state.next_event_time().is_some()
                && !interrupt()
            {
                let step = self.step()?;
                steps += 1;
                if step.is_none() || self.is_stopped() || self.reports.failure.is_some() {
                    break;
                }
            }
            return Ok(steps);
        }
        let was_stopped = self.is_stopped();
        self.end_idle_slot(self.state.next_event_time().unwrap_or(u64::MAX))?;
        if !was_stopped && self.is_stopped() {
            return Ok(steps);
        }
        let mut observer = SlotSteps {
            left: max_steps,
            compiled: 0,
            interrupt: &mut interrupt,
        };
        self.state
            .run_slots_observed(&mut self.simulator, &mut observer)?;
        Ok(max_steps - observer.left)
    }

    /// End simulation at its current time, running final procedures once.
    /// Idle steps and bounded runs remain resumable until this is requested.
    /// If a final procedure executes `$stop`, a later step resumes finalization.
    pub fn finish(&mut self) -> Result<(), RuntimeErrorCode> {
        use celox_runtime::StepObserver;
        let Some(mut shadow) = self.shadow.take() else {
            return self.state.finish(&mut self.simulator);
        };
        let mut check = differential::ShadowCheck::new(&mut shadow, self.mode());
        check.before_step(&self.state, &mut self.simulator);
        let result = self.state.finish(&mut self.simulator);
        let comparison = check.after_finish(&self.state, &mut self.simulator, &result);
        self.checked_steps += check.checked;
        self.shadow = Some(shadow);
        comparison?;
        result
    }

    /// Whether simulation has terminated and its final procedures have completed.
    /// Termination can be requested by the design or by [`Self::finish`].
    /// Once complete, [`Self::step`] returns `None` and [`Self::run_until`]
    /// stops at the termination time.
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

    /// A handle that takes the runtime events from another thread while the
    /// simulation runs (see [`Simulator::runtime_event_drain`]), or `None`
    /// when the backend has no shared ring buffer, a handle is active, or
    /// the simulation reads the events itself: to watch reports
    /// ([`Self::set_fail_on`]) or to compare them with a shadow
    /// ([`SchedulerMode::Differential`](crate::SchedulerMode)).
    pub fn runtime_event_drain(&mut self) -> Option<crate::RuntimeEventDrain> {
        if self.reports.fail_on.is_some() || self.shadow.is_some() {
            return None;
        }
        self.simulator.runtime_event_drain()
    }

    /// Takes the runtime events (`$display`, messages, `$finish`, ...) the
    /// design emitted since the last call, in emission order.
    pub fn drain_runtime_events(&mut self) -> Vec<crate::RuntimeEvent> {
        // Keep termination visible to the timed owner before collecting reports.
        // The untimed drain intentionally turns immediate fatals into reports.
        if self.simulator.dirty {
            let settled = self.simulator.eval_comb_checked();
            self.simulator.dirty = false;
            match settled.and_then(|()| self.sync_finish_checked()) {
                Err(error @ RuntimeErrorCode::Fatal { .. }) => self.finalize_fatal(&error),
                result => result.unwrap(),
            }
        }
        // A slot with nothing more due is over, also before the first step:
        // the initial settle of combinational logic belongs to time zero.
        // A fatal action is drained as a fatal report.
        if !self.state.is_finished()
            && self.simulator.slot_end.deferred_assertions
            && self.state.next_event_time() != Some(self.state.time())
        {
            if let Err(error) = self.simulator.mature_deferred_assertions() {
                self.finalize_fatal(&error);
            }
        }
        let mut events = std::mem::take(&mut self.reports.events);
        events.extend(
            self.simulator
                .drain_settled_runtime_events_with_context(crate::RuntimeFormatContext::default()),
        );
        events
    }

    /// Returns the time of the next scheduled event, if any.
    pub fn next_event_time(&self) -> Option<u64> {
        self.state.next_event_time()
    }

    /// Directly execute combinational logic evaluation.
    pub fn eval_comb(&mut self) -> Result<(), RuntimeErrorCode> {
        let result = self.simulator.eval_comb();
        if let Err(error) = &result {
            self.finalize_fatal(error);
        }
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
        self.state
            .note_gated_host_writes(&mut self.simulator.backend, true);
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

    /// The work a partitioned settle counted since the simulation started,
    /// when built with `settle_profile`.
    pub fn settle_profile(&self) -> Option<celox_state_layout::SettleProfile> {
        self.simulator.settle_profile()
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

    /// Like [`Self::child_signal`], returning an error for a path that does
    /// not resolve.
    pub fn try_child_signal(
        &self,
        instance_path: &[(&str, usize)],
        var: &str,
    ) -> Result<SignalRef, crate::ir::AddrLookupError> {
        self.simulator.try_child_signal(instance_path, var)
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

/// The most slots a run of the compiled scheduler takes in
/// [`Simulation::run_slots`] before it asks the caller whether to go on.
const SLOT_BATCH: u64 = 1 << 16;

/// Counts the steps of [`Simulation::run_slots`] and ends its run when they
/// run out or the caller interrupts it.
struct SlotSteps<'a, F> {
    left: u64,
    /// The compiled slots before the current step.
    compiled: u64,
    interrupt: &'a mut F,
}

impl<B: SimBackend, F: FnMut() -> bool> celox_runtime::StepObserver<Simulator<B>>
    for SlotSteps<'_, F>
{
    fn budget(&self, _: &SimulationState<B>) -> u64 {
        self.left.min(SLOT_BATCH)
    }

    fn interrupts(&mut self, _: &SimulationState<B>) -> bool {
        self.left == 0 || (self.interrupt)()
    }

    fn reads_scheduling_state(&self) -> bool {
        false
    }

    fn before_step(&mut self, state: &SimulationState<B>, _: &mut Simulator<B>) {
        self.compiled = state.scheduler_stats().compiled_slots;
    }

    fn after_step(
        &mut self,
        state: &SimulationState<B>,
        _: &mut Simulator<B>,
        _: u64,
        _: &Result<Option<u64>, celox_runtime::SimulatorErrorCode>,
    ) -> Result<(), celox_runtime::SimulatorErrorCode> {
        let compiled = state.scheduler_stats().compiled_slots - self.compiled;
        self.left = self.left.saturating_sub(compiled.max(1));
        Ok(())
    }
}

#[cfg(all(test, feature = "host-runtime"))]
mod process_tests;

#[cfg(all(test, feature = "host-runtime", feature = "systemverilog"))]
mod process_control_tests;
#[cfg(all(test, feature = "host-runtime"))]
mod runtime_core_tests;

/// Edges of clocks and resets the design drives itself follow IEEE 1800-2023
/// 9.4.2, Table 9-2 in a four-state timed simulation, on every backend: a
/// posedge leaves 0 or reaches 1, a negedge leaves 1 or reaches 0, and a
/// change between x and z is neither.
#[cfg(all(test, feature = "host-runtime"))]
mod four_state_edge_tests;

#[cfg(hotspot_jit)]
impl Simulation<crate::backend::TieredBackend> {
    /// Counts for incremental JIT execution, or `None` for whole-image tiering.
    pub fn hotspot_stats(&self) -> Option<crate::backend::HotspotStats> {
        self.simulator.hotspot_stats()
    }
}

/// The handle slots of `slots`, which are sorted by offset, whose offsets lie
/// in `start..end`: a range of them, found by bisection rather than by a scan
/// of every slot for every value.
fn slots_within(
    slots: &[(usize, celox_heap::HandleKind)],
    start: usize,
    end: usize,
) -> &[(usize, celox_heap::HandleKind)] {
    let first = slots.partition_point(|&(offset, _)| offset < start);
    let last = slots.partition_point(|&(offset, _)| offset < end);
    &slots[first..last.max(first)]
}

#[cfg(test)]
mod slots_within_tests {
    use celox_heap::HandleKind;

    #[test]
    fn slots_within_are_those_a_scan_finds() {
        // Sorted, gapped and adjacent offsets of both kinds; ranges inside,
        // across, before, after and between them, and empty ones.
        let slots: Vec<(usize, HandleKind)> = [0, 8, 16, 40, 48, 49, 100, 108]
            .into_iter()
            .enumerate()
            .map(|(index, offset)| {
                let kind = if index % 3 == 0 {
                    HandleKind::String
                } else {
                    HandleKind::Object
                };
                (offset, kind)
            })
            .collect();
        for start in 0..120 {
            for end in 0..120 {
                let scanned: Vec<_> = slots
                    .iter()
                    .filter(|&&(offset, _)| offset >= start && offset < end)
                    .copied()
                    .collect();
                assert_eq!(
                    super::slots_within(&slots, start, end),
                    &scanned[..],
                    "{start}..{end}"
                );
            }
        }
        assert!(super::slots_within(&[], 0, 8).is_empty());
    }
}
