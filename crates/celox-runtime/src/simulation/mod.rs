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
use celox_design::{CounterKind, DomainKind};
use fxhash::FxHashMap;

pub(crate) use batch::{event_level, stable_edge};
pub use host::{HostBoundary, HostWatch, SimulationExecutor, UnitExecutor};
pub use nba::NbaUpdate;
pub use persist::{ScheduleParts, SimulationSnapshot};
pub use wait::{Baseline, Waiter, Waiters};

use self::{round::Round, time::PeriodicEventKey};
use crate::{
    SignalRef, SimulatorErrorCode,
    backend::SimBackend,
    scheduler::Scheduler,
    timing::{CountedDomainRefs, Halt, ProcessRefs, ProcessState},
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
    topo_signals: Vec<(SignalRef, usize, usize)>,
    domain_kinds: Vec<Option<DomainKind>>,
    event_info: Vec<EventInfo<B>>,
    signal_to_id: FxHashMap<SignalRef, usize>,
    signal_index_by_id: Vec<usize>,
    processes: Vec<ProcessRefs>,
    process_wakeups: BinaryHeap<ProcessWakeup>,
    /// Counted signals and the domains their counters trigger.
    counted: Vec<CountedDomainRefs>,
    counted_by_offset: FxHashMap<usize, usize>,
    /// Whether any process has a nonblocking site with a pending flag.
    has_pending_nba_sites: bool,
    process_state: ProcessState,
    /// The host changed state that the next step must settle at the
    /// current time: an event signal, or a signal a wait watches.
    host_pending: bool,
    /// A process requested the end of the simulation.
    finished: bool,
    /// Per event ID, the event IDs of clocks computed combinationally from
    /// the event's signal. Empty when the design has none.
    combinational_derived: Vec<Box<[usize]>>,
    /// Buffers of the round in progress, kept between rounds.
    scratch: Option<Box<Round>>,
    /// The design has process kernels or counted signals. Without either, a
    /// round is exactly the batch step and skips every process check.
    timed: bool,
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

    /// Declare, per event ID, the clocks computed combinationally from the
    /// event's signal. A change of the signal gives them their edges at the
    /// same instant: they are judged from the state before it and evaluate
    /// with the event's own domain. Their levels at that instant are the
    /// baseline for the rounds after the commit, where a gate the commit
    /// moves gives a further edge.
    pub fn set_combinational_derived(&mut self, derived: Vec<Box<[usize]>>) {
        self.combinational_derived = derived;
    }

    /// Every process in `processes` starts at time zero, in list order.
    pub fn new(
        backend: &B,
        topo_signals: Vec<(SignalRef, usize, usize)>,
        domain_kinds: Vec<Option<DomainKind>>,
        event_info: Vec<EventInfo<B>>,
        processes: Vec<ProcessRefs>,
    ) -> Self {
        let mut last_clock_values = BitSet::with_capacity(backend.num_events());
        let mut signal_to_id = FxHashMap::default();
        let mut signal_index_by_id = vec![usize::MAX; backend.num_events()];
        for (index, (signal, id, _)) in topo_signals.iter().copied().enumerate() {
            if id == usize::MAX {
                continue;
            }
            signal_to_id.insert(signal, id);
            signal_index_by_id[id] = index;
            if event_level(backend, signal) != 0 {
                last_clock_values.insert(id);
            }
        }

        let has_processes = !processes.is_empty();
        let has_pending_nba_sites = processes
            .iter()
            .any(|process| process.nba_sites.iter().any(|site| site.pending.is_some()));
        Self {
            scheduler: Scheduler::new(),
            periodic_events: FxHashMap::default(),
            last_clock_values,
            topo_signals,
            domain_kinds,
            event_info,
            signal_to_id,
            signal_index_by_id,
            process_wakeups: (0..processes.len())
                .map(|process| Reverse((0, process)))
                .collect(),
            processes,
            counted: Vec::new(),
            counted_by_offset: FxHashMap::default(),
            has_pending_nba_sites,
            process_state: ProcessState::default(),
            host_pending: false,
            finished: false,
            combinational_derived: Vec::new(),
            scratch: None,
            timed: has_processes,
            round_budget: SLOT_ROUND_BUDGET,
            return_budget: SLOT_RETURN_BUDGET,
            slot_passes: 0,
        }
    }

    /// Count the occurrences of `counted` signals, whose counters trigger
    /// the listed domains instead of edge detection by value.
    pub fn with_counted(mut self, backend: &B, counted: Vec<CountedDomainRefs>) -> Self {
        self.counted_by_offset = counted
            .iter()
            .enumerate()
            .map(|(index, counted)| (counted.refs.signal.offset, index))
            .collect();
        self.process_state.domain_counters = counted
            .iter()
            .map(|counted| {
                CounterKind::ALL.map(|kind| backend.get_as::<u32>(counted.refs.counter(kind)))
            })
            .collect();
        self.timed |= !counted.is_empty();
        self.counted = counted;
        self
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
        if logged || !self.process_state.waiters.is_empty() || !self.counted.is_empty() {
            self.host_pending = true;
            return;
        }
        // Otherwise only an event signal's change needs the current time.
        for (signal, id, _) in &self.topo_signals {
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
