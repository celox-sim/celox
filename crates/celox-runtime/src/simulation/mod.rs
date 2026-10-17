//! The timed scheduler: time slots, rounds, the process phase and the
//! batch step of designs without processes.

mod batch;
mod edges;
mod host;
mod nba;
mod persist;
mod process;
mod round;
mod time;
mod wait;

use std::{cmp::Reverse, collections::BinaryHeap};

use bit_set::BitSet;
use celox_design::CounterKind;
use fxhash::FxHashMap;

pub(crate) use batch::event_level;
pub use host::{HostBoundary, HostWatch, SimulationExecutor, UnitExecutor};
pub use nba::NbaUpdate;
pub use persist::{ScheduleParts, SimulationSnapshot};
pub use wait::{Baseline, Waiter, Waiters};

use self::{round::Round, time::PeriodicEventKey};
use crate::{
    SimulatorErrorCode,
    backend::SimBackend,
    schedule::ResolvedSchedule,
    scheduler::Scheduler,
    timing::{Halt, ProcessState},
};

/// Kernel returns a round may spend on `Yield` and nonblocking staging
/// before the runtime reports a process that never suspends.
const SLOT_RETURN_BUDGET: u64 = 1 << 24;

/// Rounds one time slot may run before the runtime reports processes that
/// keep resuming at that time, such as `forever #0`.
const SLOT_ROUND_BUDGET: u64 = 1 << 20;

/// A process waiting for simulation time to reach `time`. Ordered so that a
/// max-heap pops the earliest time first and, within one time, the process
/// that was declared first.
type ProcessWakeup = Reverse<(u64, usize)>;

/// Runtime metadata for one event domain.
pub struct EventInfo<B: SimBackend> {
    pub canonical_id: usize,
    pub is_cascaded: bool,
    pub eval_ff_event: Option<B::Event>,
    pub eval_only_event: Option<B::Event>,
    pub apply_event: Option<B::Event>,
}

impl<B: SimBackend> Clone for EventInfo<B> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<B: SimBackend> Copy for EventInfo<B> {}

impl<B: SimBackend> std::fmt::Debug for EventInfo<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventInfo")
            .field("canonical_id", &self.canonical_id)
            .field("is_cascaded", &self.is_cascaded)
            .field("eval_ff_event", &self.eval_ff_event)
            .field("eval_only_event", &self.eval_only_event)
            .field("apply_event", &self.apply_event)
            .finish()
    }
}

/// Backend-independent state and execution rules for timed simulation.
pub struct SimulationState<B: SimBackend> {
    scheduler: Scheduler<B>,
    periodic_events: FxHashMap<PeriodicEventKey, usize>,
    last_clock_values: BitSet,
    /// The design's scheduling facts, which never change.
    model: ResolvedSchedule<B>,
    process_wakeups: BinaryHeap<ProcessWakeup>,
    process_state: ProcessState,
    /// The host changed state that the next step must settle at the
    /// current time: an event signal, or a signal a wait watches.
    host_pending: bool,
    /// A process requested the end of the simulation.
    finished: bool,
    /// Buffers of the round in progress, kept between rounds.
    scratch: Option<Box<Round>>,
    /// Rounds and process-phase passes one time slot may run, and kernel
    /// returns one round may spend, before the slot is reported as not
    /// settling.
    round_budget: u64,
    return_budget: u64,
    /// Rounds and process-phase passes the current time slot has run.
    slot_passes: u64,
}

impl<B: SimBackend> SimulationState<B> {
    /// Bound the work of one time slot: `rounds` rounds and passes of the
    /// process phase, and `returns` kernel returns per round, before a step
    /// reports processes that keep resuming at that time. The defaults are
    /// 2^20 and 2^24.
    pub fn set_slot_budgets(&mut self, rounds: u64, returns: u64) {
        self.round_budget = rounds;
        self.return_budget = returns;
    }

    /// Every process of `model` starts at time zero, in list order.
    pub fn new(backend: &B, model: ResolvedSchedule<B>) -> Self {
        let mut last_clock_values = BitSet::with_capacity(backend.num_events());
        for (signal, id) in model.topo_signals.iter().copied() {
            if id != usize::MAX && event_level(backend, signal) != 0 {
                last_clock_values.insert(id);
            }
        }
        let process_state = ProcessState {
            domain_counters: model
                .counted
                .iter()
                .map(|counted| {
                    CounterKind::ALL.map(|kind| backend.get_as::<u32>(counted.refs.counter(kind)))
                })
                .collect(),
            ..ProcessState::default()
        };
        Self {
            scheduler: Scheduler::new(),
            periodic_events: FxHashMap::default(),
            last_clock_values,
            process_wakeups: (0..model.processes.len())
                .map(|process| Reverse((0, process)))
                .collect(),
            model,
            process_state,
            host_pending: false,
            finished: false,
            scratch: None,
            round_budget: SLOT_ROUND_BUDGET,
            return_budget: SLOT_RETURN_BUDGET,
            slot_passes: 0,
        }
    }

    /// Settle host writes at the current time, then advance to the next time
    /// with scheduled events or processes and run it. Returns that time, the
    /// current time when only host writes were settled, or `None` when
    /// nothing was to do.
    pub fn step<E>(&mut self, executor: &mut E) -> Result<Option<u64>, SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        self.step_until(executor, u64::MAX)
    }

    /// Like [`Self::step`], but run no time after `limit`: settle host
    /// writes at the current time if it is not after `limit`, and advance
    /// only to a time up to `limit`. Returns `None` when nothing was run.
    pub fn step_until<E>(
        &mut self,
        executor: &mut E,
        limit: u64,
    ) -> Result<Option<u64>, SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        match self.step_time(executor, limit) {
            Ok(time) => Ok(time),
            Err(Halt::Finish) => {
                // The waveform records the state at the finish.
                executor.finish_timed_step(self.scheduler.time);
                self.finish(executor);
                Ok(Some(self.scheduler.time))
            }
            Err(Halt::Error(error)) => Err(error),
        }
    }

    /// End the simulation where it is.
    pub fn finish<E>(&mut self, executor: &mut E)
    where
        E: SimulationExecutor<Backend = B>,
    {
        self.finished = true;
        executor.freeze();
    }

    /// Whether a process requested the end of the simulation.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Settle externally driven state against the previous edge baseline even
    /// when no clock/reset signal is explicitly scheduled at this timestamp.
    pub fn settle_at<E>(
        &mut self,
        executor: &mut E,
        time: u64,
    ) -> Result<Option<u64>, SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        self.slot_passes = 0;
        match self.step_round(executor, time, Vec::new(), Vec::new(), true) {
            Ok(()) => {
                executor.finish_timed_step(time);
                Ok(Some(time))
            }
            Err(Halt::Finish) => {
                executor.finish_timed_step(time);
                self.finish(executor);
                Ok(Some(self.scheduler.time))
            }
            Err(Halt::Error(error)) => Err(error),
        }
    }

    /// The host changed state since the last step: note whether the next
    /// step must settle the current time first because an event signal
    /// changed or a process may be woken.
    pub fn note_host_writes(&mut self, backend: &B, logged: bool) {
        if self.finished {
            return;
        }
        // With waiting processes or counted signals, a write may reach a
        // waited or counted signal through combinational logic that has not
        // settled yet; only settling the current time tells. Processes that
        // finished or wait for a time do not see it.
        if logged || !self.process_state.waiters.is_empty() || !self.model.counted.is_empty() {
            self.host_pending = true;
            return;
        }
        // Otherwise only an event signal's change needs the current time.
        for (signal, id) in &self.model.topo_signals {
            if *id != usize::MAX
                && (event_level(backend, *signal) != 0) != self.last_clock_values.contains(*id)
            {
                self.host_pending = true;
                return;
            }
        }
    }

    pub fn time(&self) -> u64 {
        self.scheduler.time
    }

    pub fn set_time(&mut self, time: u64) {
        self.scheduler.time = time;
    }

    /// Time of the next scheduled event or process wakeup. `None` once a
    /// process has finished the simulation.
    pub fn next_event_time(&self) -> Option<u64> {
        if self.finished {
            return None;
        }
        // Host writes are work of the current time.
        if self.host_pending {
            return Some(self.scheduler.time);
        }
        self.next_work_time()
    }

    /// Periodic clocks as (event id, period).
    pub fn clock_periods(&self) -> Vec<(usize, u64)> {
        self.scheduler
            .clocks
            .iter()
            .enumerate()
            .filter_map(|(id, clock)| Some((id, clock.as_ref()?.period.max(2))))
            .collect()
    }
}
