//! The timed scheduler: time slots, rounds, the process phase and the
//! batch step of designs without processes.

mod activation;
pub use activation::Activation;
mod batch;
mod compiled;
mod edges;
mod host;
mod nba;
mod persist;
mod process;
mod round;
mod stats;
mod terminal;
mod time;
mod wait;

use std::{cmp::Reverse, collections::BinaryHeap};

use bit_set::BitSet;
use celox_design::CounterKind;

pub(crate) use batch::{edge_level, event_level};
pub use host::{CompiledSlots, HostBoundary, HostWatch, SimulationExecutor, UnitExecutor};
pub use nba::NbaUpdate;
pub use persist::{ScheduleKey, ScheduleParts, SimulationSnapshot};
pub use stats::{KernelSchedule, SchedulerMode, SchedulerStats};
pub use wait::{Baseline, Waiter, Waiters};

use self::round::Round;
use crate::{
    SignalRef, SimulatorErrorCode,
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
    /// Settled event levels before the host can write the suspended state.
    levels: Vec<u8>,
}

/// Sees the steps of [`SimulationState::run_until_observed`] and
/// [`SimulationState::step_until_observed`].
pub trait StepObserver<E: SimulationExecutor> {
    /// Whether a run of the compiled scheduler takes one slot at a time, so
    /// that every slot is a step the observer sees.
    fn one_slot(&self) -> bool {
        false
    }

    /// Called before a step, with the state it starts from.
    fn before_step(&mut self, state: &SimulationState<E::Backend>, executor: &mut E);

    /// Called after a step to at most `limit` that returned `result`. An
    /// error replaces the step's result.
    fn after_step(
        &mut self,
        state: &SimulationState<E::Backend>,
        executor: &mut E,
        limit: u64,
        result: &Result<Option<u64>, SimulatorErrorCode>,
    ) -> Result<(), SimulatorErrorCode>;
}

/// No observer.
impl<E: SimulationExecutor> StepObserver<E> for () {
    fn before_step(&mut self, _: &SimulationState<E::Backend>, _: &mut E) {}

    fn after_step(
        &mut self,
        _: &SimulationState<E::Backend>,
        _: &mut E,
        _: u64,
        _: &Result<Option<u64>, SimulatorErrorCode>,
    ) -> Result<(), SimulatorErrorCode> {
        Ok(())
    }
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
    /// Scheduling facts, including descriptors for dynamically spawned processes.
    model: ResolvedSchedule<B>,
    kernel_count: usize,
    process_wakeups: BinaryHeap<ProcessWakeup>,
    process_state: ProcessState,
    declaration_processes: Vec<usize>,
    declarations_pending: bool,
    /// The host changed state that the next step must settle at the
    /// current time, including before a suspended process resumes.
    host_pending: bool,
    /// A process requested the end of the simulation.
    finished: bool,
    /// Next final procedure while terminal execution is suspended by `$stop`.
    finalizing: Option<usize>,
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
    /// The processes the current time slot ran, most recent last, up to
    /// [`RESUMED_PROCESSES`]: the ones a slot that does not settle reports.
    slot_resumed: std::collections::VecDeque<usize>,
    /// Names of the processes for diagnostics, by index.
    process_names: Vec<String>,
    /// The loop that runs time slots, and the slots each loop ran.
    stats: SchedulerStats,
    /// The bits of the compiled scheduler's events whose changes the host
    /// observes inside a slot: the kernels and host clocks that write them
    /// run in the reference loop.
    observed_events: u64,
}

/// The most recent processes of a time slot that a slot that does not
/// settle reports.
const RESUMED_PROCESSES: usize = 64;

impl<B: SimBackend> SimulationState<B> {
    /// Bound the work of one time slot: `rounds` rounds and passes of the
    /// process phase, and `returns` kernel returns per round, before a step
    /// reports processes that keep resuming at that time. The defaults are
    /// 2^20 and 2^24.
    pub fn set_slot_budgets(&mut self, rounds: u64, returns: u64) {
        self.round_budget = rounds;
        self.return_budget = returns;
    }

    /// Name the processes, by index, in diagnostics such as a time slot
    /// that does not settle.
    pub fn set_process_names(&mut self, names: Vec<String>) {
        self.process_names = names;
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
        let declaration_processes: Vec<_> = model
            .processes
            .iter()
            .enumerate()
            .filter_map(|(index, process)| {
                (process.kind == celox_design::ProcessKind::Initializer).then_some(index)
            })
            .collect();
        let declarations_pending = !declaration_processes.is_empty();
        let mut state = Self {
            scheduler: Scheduler::new(),
            last_clock_values: BitSet::with_capacity(backend.num_events()),
            last_clock_unknown: BitSet::new(),
            four_state: backend.layout().four_state,
            process_wakeups: (0..model.processes.len())
                .filter(|&index| model.processes[index].kind != celox_design::ProcessKind::Final)
                .map(|process| Reverse((0, process)))
                .collect(),
            kernel_count: model.processes.len(),
            model,
            process_state,
            declaration_processes,
            declarations_pending,
            host_pending: false,
            finished: false,
            finalizing: None,
            stopped: false,
            suspended: None,
            scratch: None,
            round_budget: SLOT_ROUND_BUDGET,
            return_budget: SLOT_RETURN_BUDGET,
            slot_passes: 0,
            slot_resumed: std::collections::VecDeque::new(),
            process_names: Vec::new(),
            stats: SchedulerStats::default(),
            observed_events: 0,
        };
        state.stats.kernels = (0..state.model.processes.len())
            .map(|index| match state.model.kernel_fallback.get(index) {
                Some(None) if state.model.scheduler.is_some() => KernelSchedule::Compiled,
                Some(Some(reason)) => KernelSchedule::Reference(reason.clone()),
                _ => KernelSchedule::Reference("the design has no compiled scheduler".into()),
            })
            .collect();
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
        if self.finalizing.is_some() {
            if self.scheduler.time > limit {
                return Ok(None);
            }
            self.finalize_terminal(executor)?;
            return Ok(Some(self.scheduler.time));
        }
        match self.step_time(executor, limit) {
            Ok(time) => Ok(time),
            Err(Halt::Finish) => {
                self.finalize_terminal(executor)?;
                Ok(Some(self.scheduler.time))
            }
            // The time slot goes on when the simulation continues.
            Err(Halt::Stop) => Ok(Some(self.scheduler.time)),
            Err(Halt::Error(error)) => {
                if matches!(&error, SimulatorErrorCode::Fatal { .. }) {
                    let _ = self.finalize_terminal(executor);
                    return Err(error);
                }
                // Preserve the primary runtime failure even if diagnostic
                // waveform output also fails. This does not resume processes.
                let _ = self.finish_timed_boundary(executor, self.scheduler.time);
                let _ = executor.flush_output();
                Err(error)
            }
        }
    }

    /// Run every time slot with work up to and including `end`, then move
    /// to `end`. A target before the current time changes nothing. Returns
    /// whether the simulation is at `end` afterwards; it is not when a
    /// process stopped it (`$stop`) or finished it on the way.
    pub fn run_until<E>(&mut self, executor: &mut E, end: u64) -> Result<bool, SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        self.run_until_observed(executor, end, &mut ())
    }

    /// [`Self::run_until`], showing every step to `observer`.
    pub fn run_until_observed<E, O>(
        &mut self,
        executor: &mut E,
        end: u64,
        observer: &mut O,
    ) -> Result<bool, SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
        O: StepObserver<E>,
    {
        if end < self.scheduler.time {
            // Time only moves forward. Moving the timestamp back while the
            // design keeps the state it reached would leave the two
            // describing different moments, so an earlier target is a no-op.
            return Ok(false);
        }
        let compiles = self.compiles_slots();
        let waiting_kernels = self
            .model
            .scheduler
            .as_ref()
            .map_or(0, |scheduler| scheduler.wait_kernels);
        while let Some(next_time) = self.next_event_time() {
            if next_time > end {
                break;
            }
            // A process the scheduler does not run waits: the slot stays in
            // the reference loop. Test that first, as designs with such
            // processes fail it at every step.
            let compiled = if self.finalizing.is_none()
                && compiles
                && self.process_state.waiters.len() <= waiting_kernels
            {
                self.compiled_limit(executor, end, observer.one_slot())
            } else {
                None
            };
            observer.before_step(self, executor);
            let result = match compiled {
                Some(limit) => self.run_compiled(executor, limit),
                None => self.step_until(executor, end),
            };
            observer.after_step(self, executor, end, &result)?;
            result?;
            if self.stopped {
                return Ok(false);
            }
        }
        if self.finished {
            return Ok(false);
        }
        self.advance_time(executor.backend_mut(), end);
        Ok(true)
    }

    /// Select the loop that runs time slots.
    pub fn set_scheduler_mode(&mut self, mode: SchedulerMode) {
        self.stats.mode = mode;
    }

    /// The loop that runs time slots, what it ran and how each process
    /// kernel is scheduled.
    pub fn scheduler_stats(&self) -> &SchedulerStats {
        &self.stats
    }

    /// The signals whose changes the host observes inside a time slot (see
    /// [`HostBoundary::before_domains`]). The slots in which a kernel or
    /// host clock of the compiled scheduler may change an observed event
    /// signal run in the reference loop, where the host sees the change
    /// before any domain commits; the other kernels' slots stay compiled.
    pub fn set_observed_signals(&mut self, signals: impl IntoIterator<Item = SignalRef>) {
        let Some(scheduler) = &self.model.scheduler else {
            return;
        };
        self.observed_events = signals
            .into_iter()
            .filter_map(|signal| self.model.signal_to_id.get(&signal))
            .filter_map(|event| scheduler.events.iter().position(|id| id == event))
            .fold(0, |mask, bit| mask | 1 << bit);
    }

    /// [`Self::step_until`], showing the step to `observer`. An error of
    /// the observer replaces the step's result.
    pub fn step_until_observed<E, O>(
        &mut self,
        executor: &mut E,
        limit: u64,
        observer: &mut O,
    ) -> Result<Option<u64>, SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
        O: StepObserver<E>,
    {
        observer.before_step(self, executor);
        let result = self.step_until(executor, limit);
        observer.after_step(self, executor, limit, &result)?;
        result
    }

    /// Whether the host changed state that the next step settles at the
    /// current time first.
    pub fn host_pending(&self) -> bool {
        self.host_pending
    }

    /// Whether runtime-owned activation storage needs a same-design checkpoint.
    pub fn has_fork_activations(&self) -> bool {
        !self.process_state.activations.is_empty()
    }

    /// Set whether the next step settles host changes at the current time
    /// first, as when copying another simulation's state.
    #[doc(hidden)]
    pub fn set_host_pending(&mut self, pending: bool) {
        self.host_pending = pending;
    }

    /// Complete a boundary reached from an entry point that returns runtime
    /// errors rather than scheduler control halts.
    fn finish_timed_boundary<E>(
        &mut self,
        executor: &mut E,
        time: u64,
    ) -> Result<(), SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        self.retain_activation_roots(executor);
        match executor.try_finish_timed_step(time) {
            Ok(()) => Ok(()),
            Err(Halt::Error(error)) => {
                if matches!(&error, SimulatorErrorCode::Fatal { .. }) {
                    let _ = self.finalize_terminal(executor);
                }
                Err(error)
            }
            Err(Halt::Finish) => {
                self.finalize_terminal(executor)?;
                Ok(())
            }
            Err(Halt::Stop) => {
                self.stopped = true;
                Ok(())
            }
        }
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
            || (self.stopped && (self.declarations_pending || self.finalizing.is_some()))
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
        self.slot_resumed.clear();
        match self.step_round(executor, time, Vec::new(), Vec::new(), true, false) {
            Ok(()) => {
                self.finish_timed_boundary(executor, time)?;
                Ok(Some(time))
            }
            Err(Halt::Finish) => {
                self.finalize_terminal(executor)?;
                Ok(Some(self.scheduler.time))
            }
            Err(Halt::Stop) => Ok(Some(self.scheduler.time)),
            Err(Halt::Error(error)) => {
                if matches!(&error, SimulatorErrorCode::Fatal { .. }) {
                    let _ = self.finalize_terminal(executor);
                    return Err(error);
                }
                // Preserve the primary runtime failure even if diagnostic
                // waveform output also fails. This does not resume processes.
                let _ = self.finish_timed_boundary(executor, self.scheduler.time);
                let _ = executor.flush_output();
                Err(error)
            }
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
        // A suspended process must also see ordinary raw input writes settled
        // before its continuation reads combinational results.
        if self.suspended.is_some()
            || logged
            || !self.process_state.waiters.is_empty()
            || !self.model.counted.is_empty()
        {
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
        if self.host_pending || self.finalizing.is_some() {
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
