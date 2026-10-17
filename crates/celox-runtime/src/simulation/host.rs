//! The contract between the scheduler and the host that executes units.

use fxhash::{FxHashMap, FxHashSet};

use crate::{
    SignalRef, SimulatorErrorCode,
    backend::SimBackend,
    timing::{CountedRefs, Halt, Value, read},
};

/// Backend execution hooks needed by the timed simulation engine.
///
/// The facade implements this contract to retain policy such as runtime-event
/// decoration and waveform capture outside the backend-independent scheduler.
pub trait SimulationExecutor {
    type Backend: SimBackend;

    fn backend(&self) -> &Self::Backend;
    fn backend_mut(&mut self) -> &mut Self::Backend;
    fn eval_comb(&mut self) -> Result<(), Halt>;
    fn eval_apply_ff_at(&mut self, event: <Self::Backend as SimBackend>::Event)
    -> Result<(), Halt>;
    fn eval_only_ff_at(&mut self, event: <Self::Backend as SimBackend>::Event) -> Result<(), Halt>;
    /// Settle combinational logic, then evaluate and commit one domain.
    fn eval_comb_apply_ff_at(
        &mut self,
        event: <Self::Backend as SimBackend>::Event,
    ) -> Result<(), Halt> {
        self.eval_comb()?;
        self.eval_apply_ff_at(event)
    }
    fn apply_ff_at(&mut self, event: <Self::Backend as SimBackend>::Event) -> Result<(), Halt>;
    fn run_process(&mut self, index: usize) -> Result<(), Halt> {
        Ok(self.backend_mut().run_process(index)?)
    }

    /// Values the host wrote to signals that a wait watches since the last
    /// call, in write order (see [`crate::HostWatch`]).
    fn take_host_writes(&mut self) -> Vec<(SignalRef, Value)> {
        Vec::new()
    }

    /// The design finished the simulation: further evaluation must not
    /// change its state.
    fn freeze(&mut self) {}

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

    /// Called after the state for a simulation timestamp has stabilized.
    fn finish_timed_step(&mut self, _timestamp: u64) {}
}

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
