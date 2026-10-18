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

pub(crate) use batch::{edge_level, event_level};
pub use host::{HostBoundary, HostWatch, SimulationExecutor, UnitExecutor};
pub use nba::NbaUpdate;
pub use persist::{ScheduleParts, SimulationSnapshot};
pub use wait::{Baseline, Waiter, Waiters};

use self::round::Round;
use crate::{
    SimulatorErrorCode,
    backend::SimBackend,
    schedule::ResolvedSchedule,
    scheduler::{Scheduler, SimEvent},
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

/// A round suspended by `$stop` in its process phase: the round and the
/// scheduled events it processes, whose periodic clocks re-arm at its end.
struct Suspended<B: SimBackend> {
    round: Box<Round>,
    events: Vec<SimEvent<B>>,
}

/// Backend-independent state and execution rules for timed simulation.
pub struct SimulationState<B: SimBackend> {
    scheduler: Scheduler<B>,
    /// Events whose signal was high (value bit set) when edge detection last
    /// sampled it.
    last_clock_values: BitSet,
    /// Events whose signal was x or z when edge detection last sampled it.
    /// Empty unless the design is four-state.
    last_clock_unknown: BitSet,
    /// The design keeps unknown bits, so edges follow the four-state rows
    /// of IEEE 1800-2023 Table 9-2.
    four_state: bool,
    /// The design's scheduling facts, which never change.
    model: ResolvedSchedule<B>,
    process_wakeups: BinaryHeap<ProcessWakeup>,
    process_state: ProcessState,
    /// The host changed state that the next step must settle at the
    /// current time: an event signal, or a signal a wait watches.
    host_pending: bool,
    /// A process requested the end of the simulation.
    finished: bool,
    /// A process suspended the simulation (`$stop`) in the last step.
    stopped: bool,
    /// The round a `$stop` suspended, which the next step continues.
    suspended: Option<Suspended<B>>,
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
        let mut state = Self {
            scheduler: Scheduler::new(),
            last_clock_values: BitSet::with_capacity(backend.num_events()),
            last_clock_unknown: BitSet::new(),
            four_state: backend.layout().four_state,
            process_wakeups: (0..model.processes.len())
                .map(|process| Reverse((0, process)))
                .collect(),
            model,
            process_state,
            host_pending: false,
            finished: false,
            stopped: false,
            suspended: None,
            scratch: None,
            round_budget: SLOT_ROUND_BUDGET,
            return_budget: SLOT_RETURN_BUDGET,
            slot_passes: 0,
        };
        state.synchronize_event_values(backend);
        state
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
        self.stopped = false;
        match self.step_time(executor, limit) {
            Ok(time) => Ok(time),
            Err(Halt::Finish) => {
                // The waveform records the state at the finish.
                executor.finish_timed_step(self.scheduler.time);
                self.finish(executor);
                Ok(Some(self.scheduler.time))
            }
            // The time slot goes on when the simulation continues.
            Err(Halt::Stop) => Ok(Some(self.scheduler.time)),
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

    /// Whether the last step returned because a process suspended the
    /// simulation with `$stop` (IEEE 1800-2023 20.2). The simulation is
    /// suspended in the middle of a time slot, and the next step continues it
    /// after the `$stop`.
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// Whether the simulation is suspended in the middle of a time slot, so
    /// that its scheduling state cannot be captured.
    pub fn is_suspended(&self) -> bool {
        self.suspended.is_some()
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
            Err(Halt::Stop) => Ok(Some(self.scheduler.time)),
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
            if *id != usize::MAX && self.level(backend, *signal) != self.last_level(*id) {
                self.host_pending = true;
                return;
            }
        }
    }

    pub fn time(&self) -> u64 {
        self.scheduler.time
    }

    /// Set the scheduler time and publish it to the design's time slots.
    /// This does not evaluate processes or combinational logic.
    pub fn set_time(&mut self, backend: &mut B, time: u64) {
        self.scheduler.time = time;
        self.publish_time(backend, time);
    }

    /// Move to `time` without running anything there, as the end of a run
    /// to a time without work does. Design code reads the new time.
    pub fn advance_time(&mut self, backend: &mut B, time: u64) {
        self.set_time(backend, time);
    }

    /// Store `time` where design code reads the simulation time (`$time`).
    fn publish_time(&self, backend: &mut B, time: u64) {
        for &slot in &self.model.time_slots {
            backend.set(slot, time);
        }
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
