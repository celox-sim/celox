//! The contract between the scheduler and the host that executes units.

use fxhash::{FxHashMap, FxHashSet};

use crate::{
    SignalRef, SimulatorErrorCode,
    backend::SimBackend,
    timing::{CountedRefs, Halt, Value, read},
};

/// The units a timed step executes: combinational settling, flip-flop
/// domains and process kernels.
pub trait UnitExecutor {
    type Backend: SimBackend;

    fn backend(&self) -> &Self::Backend;
    fn backend_mut(&mut self) -> &mut Self::Backend;
    fn eval_comb(&mut self) -> Result<(), Halt>;
    fn eval_apply_ff_at(&mut self, event: <Self::Backend as SimBackend>::Event)
    -> Result<(), Halt>;
    fn eval_only_ff_at(&mut self, event: <Self::Backend as SimBackend>::Event) -> Result<(), Halt>;
    fn apply_ff_at(&mut self, event: <Self::Backend as SimBackend>::Event) -> Result<(), Halt>;
    fn run_process(&mut self, index: usize) -> Result<(), Halt> {
        Ok(self.backend_mut().run_process(index)?)
    }

    /// Attribute deferred assertion records to this dynamic process execution.
    fn enter_deferred_assertion_process(&mut self, _process: usize) {}
    /// Consume this invocation's records before another execution context runs.
    fn leave_deferred_assertion_process(&mut self) {}
    /// A naturally completed process retains its pending assertion reports.
    fn finish_deferred_assertion_process(&mut self, _process: usize) {}
    /// Disabling a live process flushes its deferred assertion reports (16.4.4).
    fn cancel_deferred_assertion_process(&mut self, _process: usize) {}

    /// Prepare the declaration-only state before runtime static initializers.
    fn reset_declaration_startup(&mut self) -> Result<(), Halt> {
        Ok(())
    }
    /// Arm wildcard sensitivity against the completed declaration state.
    fn prime_declaration_startup(&mut self) -> Result<(), Halt> {
        Ok(())
    }
    /// Publish folded ordinary initial writes after static initialization.
    fn apply_declaration_startup(&mut self) -> Result<(), Halt> {
        Ok(())
    }

    /// Perform a synchronous request after a kernel has stored its
    /// arguments and before that kernel resumes. The timestamp is the
    /// current scheduler time in the design's precision units.
    fn host_request(
        &mut self,
        request: &celox_design::HostRequestSite<SignalRef>,
        _timestamp: u64,
    ) -> Result<(), Halt> {
        Err(SimulatorErrorCode::Runtime {
            message: format!(
                "host does not support {:?} request {}",
                request.kind, request.operation
            ),
            signals: Vec::new(),
        }
        .into())
    }
}

/// What a timed step tells the host around the units it executes. Every
/// hook defaults to doing nothing.
pub trait HostBoundary: UnitExecutor {
    /// Values the host wrote to signals that a wait watches since the last
    /// call, in write order (see [`crate::HostWatch`]).
    fn take_host_writes(&mut self) -> Vec<(SignalRef, Value)> {
        Vec::new()
    }

    /// The design finished the simulation: further evaluation must not
    /// change its state.
    fn freeze(&mut self) {}

    /// Publish buffered diagnostic output without ending or freezing execution.
    fn flush_output(&mut self) -> Result<(), SimulatorErrorCode> {
        Ok(())
    }

    /// Freeze after a design-requested finish, reporting output failures.
    fn try_freeze(&mut self) -> Result<(), SimulatorErrorCode> {
        let output = self.flush_output();
        self.freeze();
        output
    }

    /// Called in every round when its processes and scheduled values have
    /// run and its flip-flop domains are about to evaluate: the round's
    /// edges are in state, and no domain has committed. Returns whether the
    /// host wrote state; the round then settles it before the domains read
    /// it. Writes to signals a wait watches are logged as in
    /// [`Self::take_host_writes`].
    fn before_domains(&mut self, _timestamp: u64) -> Result<bool, Halt> {
        Ok(false)
    }

    /// Whether the host wrote state in [`Self::try_finish_timed_step`]
    /// since the last call. The writes are updates of that slot's time: the
    /// next step settles them there.
    fn take_slot_writes(&mut self) -> bool {
        false
    }

    /// Snapshot external-component inputs immediately before an event domain
    /// evaluates its sequential logic.
    fn stage_external_event(
        &mut self,
        _event: <Self::Backend as SimBackend>::Event,
        _timestamp: u64,
    ) -> Result<(), SimulatorErrorCode> {
        Ok(())
    }

    /// Fire external-component hooks after the event domain commits and
    /// before the following combinational settle.
    fn fire_external_event(
        &mut self,
        _event: <Self::Backend as SimBackend>::Event,
        _timestamp: u64,
    ) -> Result<(), SimulatorErrorCode> {
        Ok(())
    }

    /// How many time slots a compiled scheduler may run between calls of
    /// [`Self::try_finish_timed_step`]. The default keeps every slot in the
    /// runtime's loop.
    fn compiled_slots(&self) -> CompiledSlots {
        CompiledSlots::None
    }

    /// Live values kept in dynamic process frames, outside backend state memory.
    fn retain_process_values(&mut self, _values: &[(crate::SignalRef, crate::timing::Value)]) {}

    /// Called after the state for a simulation timestamp has stabilized.
    fn finish_timed_step(&mut self, _timestamp: u64) {}

    /// Fallible timestamp boundary. The default preserves the original
    /// infallible hook for existing executor implementations.
    fn try_finish_timed_step(&mut self, timestamp: u64) -> Result<(), Halt> {
        self.finish_timed_step(timestamp);
        Ok(())
    }

    /// Active-region work has drained. Mature Observed deferred assertions
    /// before entering Reactive, so a later wakeup cannot flush them (16.4.1).
    fn try_observed_deferred_assertions(&mut self, _timestamp: u64) -> Result<(), Halt> {
        Ok(())
    }

    /// Called after [`Self::try_finish_timed_step`] when the time slot
    /// `timestamp` is over: no more work is due at that time. A step ends
    /// before its slot does when a process waits for zero time. The
    /// runtime's loop calls it; a host that has work here returns
    /// [`CompiledSlots::None`] from [`Self::compiled_slots`].
    fn try_end_time_slot(&mut self, _timestamp: u64) -> Result<(), Halt> {
        Ok(())
    }

    /// Called before a run or a step moves from `timestamp` to a later time:
    /// before the step of a later slot, and at the end of a run to a time
    /// without work. An error ends the run there, as an error of a step
    /// does, and so does a `$finish` the host ran there.
    fn try_leave_time(&mut self, _timestamp: u64) -> Result<(), Halt> {
        Ok(())
    }
}

/// How many time slots a compiled scheduler may run at once (see
/// [`HostBoundary::compiled_slots`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompiledSlots {
    /// None: the runtime's loop runs every slot.
    None,
    /// One per run: the host acts at every slot boundary.
    One,
    /// Any number: the host's boundary call after the last slot of a run
    /// stands for the calls after the others.
    Any,
}

/// Backend execution hooks needed by the timed simulation engine: its
/// units and its host boundary.
///
/// The facade implements this contract to retain policy such as runtime-event
/// decoration and waveform capture outside the backend-independent scheduler.
pub trait SimulationExecutor: UnitExecutor + HostBoundary {}

impl<T: UnitExecutor + HostBoundary + ?Sized> SimulationExecutor for T {}

/// Host writes the runtime must observe one at a time: writes of counted
/// signals bump their counters, and writes of signals a wait watches are
/// logged for the waits to examine. Empty unless a timed simulation with
/// such signals is attached, so ordinary writes pay one emptiness check.
#[derive(Clone, Debug, Default)]
pub struct HostWatch {
    counted: FxHashMap<usize, CountedRefs>,
    waited: FxHashSet<usize>,
    log: Vec<(SignalRef, Value)>,
}

impl HostWatch {
    pub fn new(counted: impl IntoIterator<Item = CountedRefs>, waited: FxHashSet<usize>) -> Self {
        Self {
            counted: counted
                .into_iter()
                .map(|refs| (refs.signal.offset, refs))
                .collect(),
            waited,
            log: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.counted.is_empty() && self.waited.is_empty()
    }

    /// Run `write`, which writes `signal` in `backend`, and observe it.
    pub fn write<B: SimBackend>(
        &mut self,
        backend: &mut B,
        signal: SignalRef,
        write: impl FnOnce(&mut B),
    ) {
        let counted = self.counted.get(&signal.offset).copied();
        let waited = self.waited.contains(&signal.offset);
        if counted.is_none() && !waited {
            write(backend);
            return;
        }
        let old = read(backend, signal);
        write(backend);
        let new = read(backend, signal);
        if let Some(counted) = counted {
            counted.bump(backend, &old, &new);
        }
        if waited && old != new {
            self.log.push((signal, new));
        }
    }

    /// The logged values of waited signals, in write order.
    pub fn log(&self) -> &[(SignalRef, Value)] {
        &self.log
    }

    /// Replace the log, as when returning to a saved state.
    pub fn set_log(&mut self, log: Vec<(SignalRef, Value)>) {
        self.log = log;
    }

    /// Take the logged values of waited signals, in write order.
    pub fn take_log(&mut self) -> Vec<(SignalRef, Value)> {
        std::mem::take(&mut self.log)
    }

    pub fn has_log(&self) -> bool {
        !self.log.is_empty()
    }
}
