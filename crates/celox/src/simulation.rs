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
    pub(crate) fn new(mut simulator: Simulator<B>) -> Self {
        let state = simulation_state(&simulator);
        simulator.host_watch = host_watch(&simulator.backend, &simulator.schedule);
        // Only a timed simulation ends at an RTL `$finish`; a bare
        // `Simulator`, ticks included, leaves that to the host.
        simulator.finish_sites = simulator.schedule.finish_sites;
        simulator.skip_runtime_finish();
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
            host_writes: self.simulator.host_watch.log().to_vec(),
        })
    }

    /// Save the value of every state object by path, together with the
    /// simulation time, clocks and pending events by name.
    pub fn save_state(&mut self) -> Result<celox_runtime::StateFile, StateError> {
        let mut file = self.simulator.save_state()?;
        // Settling for the file may have executed a `$finish`.
        self.sync_finish();
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
        self.simulator.load_state(file)?;
        self.state.import_schedule(parts);
        self.simulator.host_watch.set_log(host_writes);
        self.simulator.frozen = self.state.is_finished();
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
        self.simulator
            .host_watch
            .set_log(checkpoint.host_writes.clone());
        Ok(())
    }

    /// A `$finish` that flip-flop or combinational code executed outside a
    /// step, during a direct settle, read or dump, ends the simulation too.
    fn sync_finish(&mut self) {
        if !self.state.is_finished() && self.simulator.take_runtime_finish() {
            self.state.finish(&mut self.simulator);
        }
    }

    /// Captures the current state of all signals and writes them to the VCD file.
    pub fn dump(&mut self, timestamp: u64) {
        self.simulator.dump(timestamp);
        self.sync_finish();
    }

    /// See [`Simulator::try_dump`].
    pub fn try_dump(&mut self, timestamp: u64) -> Result<(), crate::simulator::DumpError> {
        let result = self.simulator.try_dump(timestamp);
        self.sync_finish();
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
        self.sync_finish();
        result?;
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
        self.state.step(&mut self.simulator)
    }

    /// [`Self::step`] that, after settling host writes at the current time,
    /// does not advance past `end_time`.
    fn step_until(&mut self, end_time: u64) -> Result<Option<u64>, RuntimeErrorCode> {
        self.state.step_until(&mut self.simulator, end_time)
    }

    /// Advance time and run until `end_time` (inclusive).
    /// Targets earlier than the current time leave time and state unchanged.
    pub fn run_until(&mut self, end_time: u64) -> Result<(), RuntimeErrorCode> {
        if end_time < self.state.time() {
            // Time only moves forward. Moving the timestamp back while the
            // design keeps the state it reached would leave the two describing
            // different moments, so an earlier target is a no-op.
            return Ok(());
        }
        while let Some(next_time) = self.state.next_event_time() {
            if next_time > end_time {
                break;
            }
            self.step_until(end_time)?;
        }
        if self.state.is_finished() {
            return Ok(());
        }
        self.state.set_time(end_time);
        self.simulator.dump_unless_rewound(end_time);
        Ok(())
    }

    /// Like [`Self::run_until`], but takes at most `max_steps` steps.
    /// Returns `false`, with the simulation at the time of its last step,
    /// when another step would be needed to reach `end_time`.
    pub fn run_until_bounded(
        &mut self,
        end_time: u64,
        max_steps: u64,
    ) -> Result<bool, RuntimeErrorCode> {
        if end_time < self.state.time() {
            return Ok(true);
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
        }
        self.run_until(end_time)?;
        Ok(true)
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
        let result = self.simulator.eval_comb();
        self.sync_finish();
        result
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
    /// that reads the committed state. `~clk` rises when `clk` falls, which
    /// the timed steps drive and a tick, the active edge only, does not. Timed steps and
    /// event-based ticks agree on every backend, in both state modes; the
    /// four-state ticks judge the gate by settling the design.
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
                    let clk_before = sim.get_four_state(sim.signal("clk"));
                    sim.tick(clk).unwrap();
                    let mut observed = observe(&mut sim);
                    assert_eq!(
                        sim.get_four_state(sim.signal("clk")),
                        clk_before,
                        "a tick leaves clk as it was"
                    );
                    // A tick is not a falling edge of `clk`: `~clk` stays put.
                    observed[3] = 0;
                    observed
                })
                .collect()
        }
        let want = expected();
        let want_ticked: Vec<_> = want.iter().map(|&[a, b, c, _]| [a, b, c, 0]).collect();
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
                want_ticked,
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
                want_ticked,
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
                want_ticked,
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
                want_ticked,
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
                    want_ticked,
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
