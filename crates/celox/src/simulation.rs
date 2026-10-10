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
        !self.components.has_scheduled_components()
    }

    fn host_request(
        &mut self,
        process: usize,
        request: usize,
        time: u64,
    ) -> Result<bool, RuntimeErrorCode> {
        crate::testbench::kernels::serve_host_request(self, process, request, time).map_err(
            |message| RuntimeErrorCode::Runtime {
                message,
                signals: Vec::new(),
            },
        )
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
        // A testbench holds its resets through releases; while one is
        // asserted, its components take reset cycles.
        if !self.program.runtime_schema.processes.is_empty() {
            for reset_event in self.components.reset_event_ids() {
                let address = self.backend.id_to_addr_slice()[reset_event];
                let Some(release) = self
                    .program
                    .runtime_schema
                    .processes
                    .iter()
                    .flat_map(|slots| &slots.releases)
                    .find(|release| release.signal == address)
                else {
                    continue;
                };
                let signal = self.backend.resolve_signal(&address);
                if self.backend.get(signal) == release.value.into() {
                    continue;
                }
                match release
                    .clock
                    .and_then(|clock| self.backend.resolve_event_opt(&clock))
                {
                    Some(clock) => self
                        .components
                        .begin_reset_clock_cycles(clock.id(), Some(reset_event)),
                    None => self.components.begin_reset_cycles(Some(reset_event)),
                }
            }
        }
        let fired = self.components.fire(event.id(), timestamp);
        self.components.end_reset_cycles();
        let writes = fired.map_err(|message| RuntimeErrorCode::Runtime {
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
            release: simulator.backend.resolve_signal(&slots.release),
            releases: slots
                .releases
                .iter()
                .map(|release| celox_runtime::ProcessReleaseRef {
                    signal: simulator.backend.resolve_signal(&release.signal),
                    event: simulator.backend.resolve_event_opt(&release.signal),
                    value: release.value,
                })
                .collect(),
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
    /// since the previous call satisfied first resumes at the current time.
    pub fn step(&mut self) -> Result<Option<u64>, RuntimeErrorCode> {
        self.state.poll_waiting(&mut self.simulator)?;
        self.state.step(&mut self.simulator)
    }

    /// Advance time and run until `end_time` (inclusive). As for
    /// [`Self::step`], processes that a host write woke resume first.
    pub fn run_until(&mut self, end_time: u64) -> Result<(), RuntimeErrorCode> {
        self.state.poll_waiting(&mut self.simulator)?;
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
}
