use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, BinaryHeap},
};

use bit_set::BitSet;
use celox_design::{DomainKind, ProcessStatus};
use fxhash::FxHashMap;

use crate::{
    AbsoluteAddr, SignalRef, SimulatorErrorCode,
    backend::{EventHandle, SimBackend},
    scheduler::{ClockDef, Scheduler, SimEvent},
};

/// Backend execution hooks needed by the timed simulation engine.
///
/// The facade implements this contract to retain policy such as runtime-event
/// decoration and waveform capture outside the backend-independent scheduler.
pub trait SimulationExecutor {
    type Backend: SimBackend;

    fn backend(&self) -> &Self::Backend;
    fn backend_mut(&mut self) -> &mut Self::Backend;
    fn eval_comb(&mut self) -> Result<(), SimulatorErrorCode>;
    fn eval_apply_ff_at(
        &mut self,
        event: <Self::Backend as SimBackend>::Event,
    ) -> Result<(), SimulatorErrorCode>;
    fn eval_only_ff_at(
        &mut self,
        event: <Self::Backend as SimBackend>::Event,
    ) -> Result<(), SimulatorErrorCode>;
    fn apply_ff_at(
        &mut self,
        event: <Self::Backend as SimBackend>::Event,
    ) -> Result<(), SimulatorErrorCode>;
    fn run_process(&mut self, index: usize) -> Result<(), SimulatorErrorCode> {
        self.backend_mut().run_process(index)
    }

    /// Run up to `count` rising edges of `event` as fused ticks: each one
    /// evaluates the combinational logic and then the sequential domain,
    /// without toggling the clock signal. Returns how many completed before
    /// a runtime event or an error forced a return to the host.
    fn tick_many(
        &mut self,
        event: <Self::Backend as SimBackend>::Event,
        count: u64,
    ) -> (u64, Result<(), SimulatorErrorCode>) {
        if count == 0 {
            return (0, Ok(()));
        }
        if let Err(error) = self.eval_comb() {
            return (0, Err(error));
        }
        (1, self.eval_apply_ff_at(event))
    }

    /// Whether clock waits may be served by fused ticks, which skip the
    /// clock signal's own edges and the host's per-edge hooks.
    fn fused_ticks_allowed(&self) -> bool {
        true
    }

    /// Serve host request `request` of process `process` at `time`, reading
    /// and writing the scratch state the request names. Returns whether the
    /// process resumes; `false` ends the simulation, as when a host
    /// component requested the end of the run.
    fn host_request(
        &mut self,
        process: usize,
        request: usize,
        time: u64,
    ) -> Result<bool, SimulatorErrorCode> {
        let _ = time;
        Err(SimulatorErrorCode::Runtime {
            message: format!(
                "process {process} made host request {request}, which this host does not serve"
            ),
            signals: Vec::new(),
        })
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

    /// Called after the state for a simulation timestamp has stabilized.
    fn finish_timed_step(&mut self, _timestamp: u64) {}
}

/// A clock a process may wait on, resolved to the backend.
#[derive(Debug)]
pub struct ProcessClockRef<B: SimBackend> {
    pub event: B::Event,
    pub signal: SignalRef,
    pub period: u64,
}

impl<B: SimBackend> Clone for ProcessClockRef<B> {
    fn clone(&self) -> Self {
        Self {
            event: self.event,
            signal: self.signal,
            period: self.period,
        }
    }
}

/// Control slots of one process kernel, resolved to signals of the backend.
#[derive(Debug)]
pub struct ProcessRefs<B: SimBackend> {
    pub status: SignalRef,
    pub delay: SignalRef,
    pub clock: SignalRef,
    pub release: SignalRef,
    pub clocks: Vec<ProcessClockRef<B>>,
    /// The writes a clock wait may end with.
    pub releases: Vec<ProcessReleaseRef<B>>,
}

/// A write a clock wait ends with: the signal, the event of its domain
/// when it is the clock or reset of one, and the value.
#[derive(Debug)]
pub struct ProcessReleaseRef<B: SimBackend> {
    pub signal: SignalRef,
    pub event: Option<B::Event>,
    pub value: u64,
}

impl<B: SimBackend> Clone for ProcessReleaseRef<B> {
    fn clone(&self) -> Self {
        Self {
            signal: self.signal,
            event: self.event,
            value: self.value,
        }
    }
}

impl<B: SimBackend> Clone for ProcessRefs<B> {
    fn clone(&self) -> Self {
        Self {
            status: self.status,
            delay: self.delay,
            clock: self.clock,
            release: self.release,
            clocks: self.clocks.clone(),
            releases: self.releases.clone(),
        }
    }
}

/// A process clock some process has waited on: the runtime generates its
/// edges from then on. A rising edge is due at `next_edge` while a process
/// waits; the falling edge of the last rising edge that was applied to the
/// signal is pending at `falling_edge`.
#[derive(Debug)]
struct TickClock<B: SimBackend> {
    event: B::Event,
    signal: SignalRef,
    period: u64,
    next_edge: u64,
    falling_edge: Option<u64>,
}

impl<B: SimBackend> Clone for TickClock<B> {
    fn clone(&self) -> Self {
        Self {
            event: self.event,
            signal: self.signal,
            period: self.period,
            next_edge: self.next_edge,
            falling_edge: self.falling_edge,
        }
    }
}

/// A process waiting for `remaining` more rising edges of the process clock
/// with event id `clock`, ending with the process's release `release`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ClockWait {
    clock: usize,
    remaining: u64,
    release: Option<usize>,
}

/// The state of one process clock, for schedules expressed by name.
#[derive(Debug)]
pub struct ProcessClockState<B: SimBackend> {
    pub event: B::Event,
    pub signal: SignalRef,
    pub period: u64,
    pub next_edge: u64,
    pub falling_edge: Option<u64>,
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PeriodicEventKey {
    time: u64,
    event_id: usize,
    event_addr: AbsoluteAddr,
    signal: SignalRef,
    next_val: u8,
}

impl PeriodicEventKey {
    fn from_event<B: SimBackend>(event: &SimEvent<B>) -> Self {
        Self {
            time: event.time,
            event_id: event.event_ref.id(),
            event_addr: event.event_ref.addr(),
            signal: event.signal,
            next_val: event.next_val,
        }
    }
}

/// Backend-independent state and execution rules for timed simulation.
pub struct SimulationState<B: SimBackend> {
    scheduler: Scheduler<B>,
    periodic_events: FxHashMap<PeriodicEventKey, usize>,
    last_clock_values: BitSet,
    /// Events whose signal was x or z when edge detection last sampled it:
    /// a change to a known value is an edge (IEEE 1800-2023 9.4.2).
    unknown_clock_values: BitSet,
    topo_signals: Vec<(SignalRef, usize, usize)>,
    domain_kinds: Vec<Option<DomainKind>>,
    event_info: Vec<EventInfo<B>>,
    signal_to_id: FxHashMap<SignalRef, usize>,
    processes: Vec<ProcessRefs<B>>,
    process_wakeups: BinaryHeap<ProcessWakeup>,
    /// Processes suspended at an event or level wait, which their kernels
    /// re-check whenever the state may have changed.
    waiting: BTreeSet<usize>,
    /// Process clocks that have been waited on, by event id.
    tick_clocks: BTreeMap<usize, TickClock<B>>,
    /// Processes waiting for rising edges of a process clock.
    clock_waits: BTreeMap<usize, ClockWait>,
    /// Processes that ran to their end.
    done: BTreeSet<usize>,
    /// Writes to make when a process resumes from a clock wait.
    pending_releases: BTreeMap<usize, usize>,
    /// Times with a rising edge of a process clock so far.
    ticks: u64,
    /// How many more times with a rising edge of a process clock may run.
    /// At zero, pending falling edges and releases still complete, but no
    /// edge fires and no process resumes from a clock wait.
    tick_budget: Option<u64>,
    /// A process requested the end of the simulation.
    finished: bool,
}

/// Whether an event signal is nonzero, and whether it is x or z.
fn sample_event_signal<B: SimBackend>(backend: &B, signal: SignalRef) -> (bool, bool) {
    if backend.layout().four_state && signal.is_4state {
        let (value, mask) = backend.get_four_state(signal);
        (value.bits() != 0, mask.bits() != 0)
    } else {
        let value: u8 = backend.get_as(signal);
        (value != 0, false)
    }
}

impl<B: SimBackend> SimulationState<B> {
    /// Rebase edge detection after state was advanced outside this scheduler.
    pub fn synchronize_event_values(&mut self, backend: &B) {
        self.last_clock_values.make_empty();
        self.unknown_clock_values.make_empty();
        for (signal, id, _) in &self.topo_signals {
            if *id == usize::MAX {
                continue;
            }
            let (is_nonzero, is_unknown) = sample_event_signal(backend, *signal);
            if is_nonzero {
                self.last_clock_values.insert(*id);
            }
            if is_unknown {
                self.unknown_clock_values.insert(*id);
            }
        }
    }

    fn replace_triggers_with_stable_edges(&self, backend: &mut B) {
        backend.clear_triggered_bits();
        for (signal, id, _) in &self.topo_signals {
            if *id == usize::MAX {
                continue;
            }
            let was_nonzero = self.last_clock_values.contains(*id);
            let was_unknown = self.unknown_clock_values.contains(*id);
            let (is_nonzero, is_unknown) = sample_event_signal(backend, *signal);
            let triggered = match self.domain_kinds[*id] {
                Some(DomainKind::ClockPosedge | DomainKind::ResetAsyncHigh) => {
                    (!was_nonzero || was_unknown) && is_nonzero && !is_unknown
                }
                Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => {
                    (was_nonzero || was_unknown) && !is_nonzero && !is_unknown
                }
                _ => (was_nonzero, was_unknown) != (is_nonzero, is_unknown),
            };
            if triggered {
                backend.mark_triggered_bit(*id);
            }
        }
    }

    /// Every process in `processes` starts at time zero, in list order.
    pub fn new(
        backend: &B,
        topo_signals: Vec<(SignalRef, usize, usize)>,
        domain_kinds: Vec<Option<DomainKind>>,
        event_info: Vec<EventInfo<B>>,
        processes: Vec<ProcessRefs<B>>,
    ) -> Self {
        let mut last_clock_values = BitSet::with_capacity(backend.num_events());
        let mut unknown_clock_values = BitSet::with_capacity(backend.num_events());
        let mut signal_to_id = FxHashMap::default();
        for (signal, id, _) in topo_signals.iter().copied() {
            if id == usize::MAX {
                continue;
            }
            signal_to_id.insert(signal, id);
            let (is_nonzero, is_unknown) = sample_event_signal(backend, signal);
            if is_nonzero {
                last_clock_values.insert(id);
            }
            if is_unknown {
                unknown_clock_values.insert(id);
            }
        }

        Self {
            scheduler: Scheduler::new(),
            periodic_events: FxHashMap::default(),
            last_clock_values,
            unknown_clock_values,
            topo_signals,
            domain_kinds,
            event_info,
            signal_to_id,
            process_wakeups: (0..processes.len())
                .map(|process| Reverse((0, process)))
                .collect(),
            processes,
            waiting: BTreeSet::new(),
            tick_clocks: BTreeMap::new(),
            clock_waits: BTreeMap::new(),
            done: BTreeSet::new(),
            pending_releases: BTreeMap::new(),
            ticks: 0,
            tick_budget: None,
            finished: false,
        }
    }

    /// Times with a rising edge of a process clock the simulation has run
    /// so far.
    pub fn ticks(&self) -> u64 {
        self.ticks
    }

    /// Limit the times with a rising edge of a process clock that may still
    /// run; `None` lifts the limit.
    pub fn set_tick_budget(&mut self, budget: Option<u64>) {
        self.tick_budget = budget;
    }

    /// Whether the tick budget is spent.
    pub fn tick_budget_spent(&self) -> bool {
        self.tick_budget == Some(0)
    }

    /// Whether a process still waits for edges of a clock.
    pub fn has_clock_waits(&self) -> bool {
        !self.clock_waits.is_empty()
    }

    /// Whether every process ran to its end.
    pub fn all_processes_done(&self) -> bool {
        self.done.len() == self.processes.len()
    }

    fn clock_has_waiters(&self, clock: usize) -> bool {
        self.clock_waits.values().any(|wait| wait.clock == clock)
    }

    pub fn add_clock(
        &mut self,
        event: B::Event,
        signal: SignalRef,
        period: u64,
        initial_delay: u64,
    ) {
        let event_id = event.id();
        if event_id >= self.scheduler.clocks.len() {
            self.scheduler.clocks.resize(event_id + 1, None);
        }
        self.scheduler.clocks[event_id] = Some(ClockDef { period });
        self.push_periodic_event(SimEvent {
            time: initial_delay,
            event_ref: event,
            signal,
            next_val: 1,
        });
    }

    pub fn schedule(&mut self, event: B::Event, signal: SignalRef, time: u64, value: u8) {
        self.scheduler.push(SimEvent {
            time,
            event_ref: event,
            signal,
            next_val: value,
        });
    }

    fn push_periodic_event(&mut self, event: SimEvent<B>) {
        *self
            .periodic_events
            .entry(PeriodicEventKey::from_event(&event))
            .or_default() += 1;
        self.scheduler.push(event);
    }

    pub fn step<E>(&mut self, executor: &mut E) -> Result<Option<u64>, SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        self.step_until(executor, u64::MAX)
    }

    /// [`Self::step`], where a run of fused clock ticks stops before
    /// `limit`: the time the step reaches is at most `limit`.
    pub fn step_until<E>(
        &mut self,
        executor: &mut E,
        limit: u64,
    ) -> Result<Option<u64>, SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        let Some(current_time) = self.next_event_time() else {
            return Ok(None);
        };
        let mut events_to_process = if self.scheduler.next_event_time() == Some(current_time) {
            self.scheduler
                .pop_all_at_next_time()
                .map(|(_, events)| events)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let ready = self.take_ready_processes(executor, current_time, &mut events_to_process);
        self.step_round(executor, current_time, events_to_process, ready, true)?;
        self.run_remaining_rounds(executor, current_time, limit)?;
        executor.finish_timed_step(self.scheduler.time);
        Ok(Some(current_time))
    }

    /// Run the further rounds of `current_time`: the rising edges of the
    /// process clocks that are due once the processes have run; a process
    /// that waited for zero time resumes in a later round, after the
    /// previous round's edges have been handled; and the settled state may
    /// wake a process waiting for an event or a condition. A round in which
    /// no process ran changes nothing and ends the time. When every live
    /// process waits on the one clock that is due and nothing else is
    /// pending, its edges run as fused ticks up to `limit` instead.
    fn run_remaining_rounds<E>(
        &mut self,
        executor: &mut E,
        current_time: u64,
        limit: u64,
    ) -> Result<(), SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        while !self.finished {
            let due: Vec<usize> = if self.tick_budget_spent() {
                Vec::new()
            } else {
                self.tick_clocks
                    .iter()
                    .filter(|(id, clock)| {
                        clock.next_edge == current_time && self.clock_has_waiters(**id)
                    })
                    .map(|(id, _)| *id)
                    .collect()
            };
            let falling: Vec<usize> = self
                .tick_clocks
                .iter()
                .filter(|(_, clock)| clock.falling_edge == Some(current_time))
                .map(|(id, _)| *id)
                .collect();
            if falling.is_empty()
                && let [clock] = due[..]
                && self.fused_ticks(executor, clock, current_time, limit)?
            {
                // Time moved on; the processes that resume run in the next step.
                return Ok(());
            }
            let fired = !due.is_empty() || !falling.is_empty();
            if fired {
                self.fire_edges(executor, current_time, &due, &falling)?;
            }
            let mut events = Vec::new();
            let ready = self.take_ready_processes(executor, current_time, &mut events);
            if ready.is_empty() && self.waiting.is_empty() && events.is_empty() {
                if fired {
                    // The edges may have released waits that resume now.
                    continue;
                }
                break;
            }
            if !self.step_round(executor, current_time, events, ready, false)? && !fired {
                break;
            }
        }
        Ok(())
    }

    /// Apply the rising edges of the process clocks `due` and the falling
    /// edges of the clocks `falling` at `time` as one set of simultaneous
    /// events, count the rising edges against the processes waiting on
    /// them, and schedule the resumptions and falling edges.
    fn fire_edges<E>(
        &mut self,
        executor: &mut E,
        time: u64,
        due: &[usize],
        falling: &[usize],
    ) -> Result<(), SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        let events = falling
            .iter()
            .map(|id| (id, 0))
            .chain(due.iter().map(|id| (id, 1)))
            .map(|(id, next_val)| {
                let clock = &self.tick_clocks[id];
                SimEvent {
                    time,
                    event_ref: clock.event,
                    signal: clock.signal,
                    next_val,
                }
            })
            .collect();
        for id in falling {
            self.tick_clocks
                .get_mut(id)
                .expect("a process clock with a falling edge")
                .falling_edge = None;
        }
        self.step_round(executor, time, events, Vec::new(), true)?;
        if due.is_empty() {
            return Ok(());
        }
        self.ticks += 1;
        if let Some(budget) = &mut self.tick_budget {
            *budget -= 1;
        }
        for id in due {
            let clock = self.tick_clocks.get_mut(id).expect("a due process clock");
            clock.next_edge = time + clock.period;
            clock.falling_edge = Some(time + clock.period - clock.period / 2);
            let resume_at = clock.next_edge;
            self.count_edges(*id, 1, resume_at);
        }
        Ok(())
    }

    /// Credit `edges` rising edges of `clock` to the processes waiting on
    /// it; those whose wait is over resume at `resume_at`.
    fn count_edges(&mut self, clock: usize, edges: u64, resume_at: u64) {
        let released: Vec<usize> = self
            .clock_waits
            .iter_mut()
            .filter(|(_, wait)| wait.clock == clock)
            .filter_map(|(process, wait)| {
                wait.remaining = wait.remaining.saturating_sub(edges);
                (wait.remaining == 0).then_some(*process)
            })
            .collect();
        for process in released {
            let wait = self.clock_waits.remove(&process).expect("a released wait");
            if let Some(release) = wait.release {
                self.pending_releases.insert(process, release);
            }
            self.process_wakeups.push(Reverse((resume_at, process)));
        }
    }

    /// When every live process waits on `clock`, no other event or
    /// resumption is pending, and the host allows it, run the edges every
    /// process waits for as fused ticks, as many as the first resumption
    /// needs and `limit` permits. Returns whether that happened.
    fn fused_ticks<E>(
        &mut self,
        executor: &mut E,
        clock: usize,
        time: u64,
        limit: u64,
    ) -> Result<bool, SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        if !executor.fused_ticks_allowed()
            || !self.waiting.is_empty()
            || !self.process_wakeups.is_empty()
            || self.scheduler.next_event_time().is_some()
            || self
                .tick_clocks
                .values()
                .any(|clock| clock.falling_edge.is_some())
            || self.clock_waits.values().any(|wait| wait.clock != clock)
            || self.clock_waits.len() + self.done.len() != self.processes.len()
        {
            return Ok(false);
        }
        let Some(needed) = self.clock_waits.values().map(|wait| wait.remaining).min() else {
            return Ok(false);
        };
        let (event, period) = {
            let clock = &self.tick_clocks[&clock];
            (clock.event, clock.period)
        };
        // Edges at `time`, `time + period`, ...: the last one stays at or
        // before `limit` and within the tick budget.
        let mut count = needed.min(limit.saturating_sub(time) / period + 1);
        if let Some(budget) = self.tick_budget {
            count = count.min(budget);
        }
        if count == 0 {
            return Ok(false);
        }
        let (completed, result) = executor.tick_many(event, count);
        result?;
        if completed == 0 || completed > count {
            return Err(SimulatorErrorCode::Runtime {
                message: "the backend made invalid progress on fused clock ticks".into(),
                signals: Vec::new(),
            });
        }
        self.ticks += completed;
        if let Some(budget) = &mut self.tick_budget {
            *budget -= completed;
        }
        let next_edge = time + completed * period;
        self.tick_clocks
            .get_mut(&clock)
            .expect("a due process clock")
            .next_edge = next_edge;
        self.count_edges(clock, completed, next_edge);
        // The processes that resume read the state settled after the last
        // edge.
        executor.eval_comb()?;
        self.scheduler.time = time + (completed - 1) * period;
        Ok(true)
    }

    /// Resume the processes waiting for an event or a condition at the
    /// current time, for state that was changed from outside the scheduler,
    /// and settle what they drive. Nothing happens when none wakes.
    pub fn poll_waiting<E>(&mut self, executor: &mut E) -> Result<(), SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        if self.finished || self.waiting.is_empty() {
            return Ok(());
        }
        let time = self.scheduler.time;
        if self.step_round(executor, time, Vec::new(), Vec::new(), false)? {
            self.run_remaining_rounds(executor, time, u64::MAX)?;
            executor.finish_timed_step(time);
        }
        Ok(())
    }

    /// Remove the processes waiting for `time`, in declaration order. With
    /// the tick budget spent, a process resuming from a clock wait only gets
    /// its release and stays suspended.
    /// The processes that resume at `time`, after making the releases their
    /// waits ended with: a release of a signal with an event domain is an
    /// event of `time`, added to `events`, so the domain and the host hooks
    /// see its edge; any other is written directly. Once the tick budget
    /// is spent, releases are still made but no process resumes.
    fn take_ready_processes<E>(
        &mut self,
        executor: &mut E,
        time: u64,
        events: &mut Vec<SimEvent<B>>,
    ) -> Vec<usize>
    where
        E: SimulationExecutor<Backend = B>,
    {
        let mut ready = Vec::new();
        while let Some(&Reverse((wakeup, process))) = self.process_wakeups.peek() {
            if wakeup != time {
                break;
            }
            self.process_wakeups.pop();
            if let Some(release) = self.pending_releases.remove(&process) {
                let release = self.processes[process].releases[release].clone();
                match release.event {
                    Some(event) => events.push(SimEvent {
                        time,
                        event_ref: event,
                        signal: release.signal,
                        next_val: release.value as u8,
                    }),
                    None => executor
                        .backend_mut()
                        .set_wide(release.signal, release.value.into()),
                }
            }
            if self.tick_budget_spent() {
                continue;
            }
            ready.push(process);
        }
        ready
    }

    /// Run `ready` processes, and the processes waiting for an event or a
    /// condition, in declaration order until each suspends or ends. A
    /// process another one wakes runs in a further pass of the same round,
    /// before any register the round triggers updates. A process that waits
    /// for zero time is queued for the next round of this time. Returns
    /// whether any process ran a statement.
    fn run_processes<E>(
        &mut self,
        executor: &mut E,
        current_time: u64,
        ready: Vec<usize>,
    ) -> Result<bool, SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        let mut progressed = false;
        let mut pass = ready;
        let mut again = Vec::new();
        loop {
            pass.extend(self.waiting.iter().copied());
            pass.append(&mut again);
            pass.sort_unstable();
            pass.dedup();
            if pass.is_empty() {
                return Ok(progressed);
            }
            let mut any_ran = false;
            for process in pass.drain(..) {
                // A settle resumes the same process at once.
                loop {
                    executor.run_process(process)?;
                    let refs = &self.processes[process];
                    let status: u8 = executor.backend().get_as(refs.status);
                    match ProcessStatus::from_code(status) {
                        Some(ProcessStatus::Settle) => {
                            executor.eval_comb()?;
                            any_ran = true;
                            continue;
                        }
                        Some(ProcessStatus::Delay) => {
                            let delay: u64 = executor.backend().get_as(refs.delay);
                            let time = current_time.checked_add(delay).ok_or_else(|| {
                                SimulatorErrorCode::Runtime {
                                    message: format!(
                                        "process {process} delay overflows simulation time"
                                    ),
                                    signals: Vec::new(),
                                }
                            })?;
                            self.waiting.remove(&process);
                            self.process_wakeups.push(Reverse((time, process)));
                            any_ran = true;
                        }
                        Some(ProcessStatus::Done) => {
                            self.waiting.remove(&process);
                            self.done.insert(process);
                            any_ran = true;
                        }
                        Some(ProcessStatus::Finish) => {
                            self.waiting.remove(&process);
                            self.done.insert(process);
                            self.finished = true;
                            return Ok(true);
                        }
                        Some(ProcessStatus::Wait) => {
                            self.waiting.insert(process);
                            any_ran = true;
                        }
                        Some(ProcessStatus::WaitClock) => {
                            let count: u64 = executor.backend().get_as(refs.delay);
                            let index: u32 = executor.backend().get_as(refs.clock);
                            let release: u32 = executor.backend().get_as(refs.release);
                            let Some(clock) = refs.clocks.get(index as usize) else {
                                return Err(SimulatorErrorCode::InternalError);
                            };
                            let release = match release {
                                0 => None,
                                index if (index as usize) <= refs.releases.len() => {
                                    Some(index as usize - 1)
                                }
                                _ => return Err(SimulatorErrorCode::InternalError),
                            };
                            self.waiting.remove(&process);
                            if count == 0 {
                                // Nothing to wait for: the process continues in
                                // the next pass, after its release.
                                if let Some(release) = release {
                                    let release = refs.releases[release].clone();
                                    executor
                                        .backend_mut()
                                        .set_wide(release.signal, release.value.into());
                                }
                                again.push(process);
                            } else {
                                let id = clock.event.id();
                                let tick_clock =
                                    self.tick_clocks.entry(id).or_insert_with(|| TickClock {
                                        event: clock.event,
                                        signal: clock.signal,
                                        period: clock.period.max(2),
                                        next_edge: current_time,
                                        falling_edge: None,
                                    });
                                tick_clock.next_edge = tick_clock.next_edge.max(current_time);
                                self.clock_waits.insert(
                                    process,
                                    ClockWait {
                                        clock: id,
                                        remaining: count,
                                        release,
                                    },
                                );
                            }
                            any_ran = true;
                        }
                        Some(ProcessStatus::Host) => {
                            let request: u64 = executor.backend().get_as(refs.delay);
                            self.waiting.remove(&process);
                            if !executor.host_request(process, request as usize, current_time)? {
                                self.done.insert(process);
                                self.finished = true;
                                return Ok(true);
                            }
                            again.push(process);
                            any_ran = true;
                        }
                        // The kernel found its wait condition unmet and ran
                        // nothing; it stays waiting.
                        Some(ProcessStatus::Pending) => {}
                        None => return Err(SimulatorErrorCode::InternalError),
                    }
                    break;
                }
            }
            if !any_ran {
                return Ok(progressed);
            }
            progressed = true;
        }
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
        self.step_round(executor, time, Vec::new(), Vec::new(), true)?;
        executor.finish_timed_step(time);
        Ok(Some(time))
    }

    /// Apply `events_to_process`, run `ready_processes` and the waiting
    /// processes, and settle the edges they cause. Without `settle_idle`,
    /// a round with no event in which no process ran is skipped, and
    /// `false` is returned.
    fn step_round<E>(
        &mut self,
        executor: &mut E,
        current_time: u64,
        events_to_process: Vec<SimEvent<B>>,
        ready_processes: Vec<usize>,
        settle_idle: bool,
    ) -> Result<bool, SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        self.scheduler.time = current_time;

        // Keep periodic provenance private so the public SimEvent shape stays
        // stable. Consume matching sidecar counts before fallible work so an
        // execution error cannot leave a stale periodic marker behind.
        let mut periodic_events_to_process: Vec<SimEvent<B>> = Vec::new();
        for event in &events_to_process {
            let key = PeriodicEventKey::from_event(event);
            let mut remove_key = false;
            if let Some(count) = self.periodic_events.get_mut(&key) {
                periodic_events_to_process.push(SimEvent {
                    time: event.time,
                    event_ref: event.event_ref,
                    signal: event.signal,
                    next_val: event.next_val,
                });
                *count -= 1;
                remove_key = *count == 0;
            }
            if remove_key {
                self.periodic_events.remove(&key);
            }
        }
        debug_assert!(
            self.periodic_events
                .keys()
                .all(|event| event.time > current_time),
            "periodic event sidecar fell behind the scheduler"
        );

        let num_events = executor.backend().num_events();
        for event in &events_to_process {
            executor.backend_mut().set(event.signal, event.next_val);
        }

        // Processes run after this time's scheduled values are applied and
        // see the state settled at the previous time. The event signals they
        // change are edges of this time, like scheduled events.
        // (event, whether the signal is nonzero now, whether it was unknown
        // before): a four-state edge from x or z counts as an edge.
        let mut process_driven = Vec::new();
        if !ready_processes.is_empty() || !self.waiting.is_empty() {
            let before: Vec<(bool, bool)> = self
                .topo_signals
                .iter()
                .map(|(signal, _, _)| sample_event_signal(executor.backend(), *signal))
                .collect();
            let progressed = self.run_processes(executor, current_time, ready_processes)?;
            if !progressed && !settle_idle && events_to_process.is_empty() {
                return Ok(false);
            }
            for ((signal, id, _), (was_nonzero, was_unknown)) in
                self.topo_signals.iter().zip(before)
            {
                if *id == usize::MAX {
                    continue;
                }
                let (is_nonzero, is_unknown) = sample_event_signal(executor.backend(), *signal);
                if (is_nonzero, is_unknown) != (was_nonzero, was_unknown) {
                    process_driven.push((*id, is_nonzero, was_unknown));
                }
            }
        }

        let mut triggered_domains = BitSet::with_capacity(num_events);
        let mut discovered_in_this_step = BitSet::with_capacity(num_events);
        let mut scheduled_trigger_ids = BitSet::with_capacity(num_events);
        // An external drive may already have been combinationally evaluated
        // by a testbench read. Compare against our saved baseline even if that
        // evaluation's transient trigger bits have since been cleared.
        let mut track_stable_edges = events_to_process.is_empty();
        executor.backend_mut().clear_triggered_bits();

        let scheduled = events_to_process
            .iter()
            .filter_map(|event| {
                self.signal_to_id
                    .get(&event.signal)
                    .map(|&id| (id, event.next_val != 0, false))
            })
            .chain(process_driven);
        for (id, is_nonzero, from_unknown) in scheduled {
            track_stable_edges = true;
            let was_nonzero = self.last_clock_values.contains(id);
            let from_unknown = from_unknown || self.unknown_clock_values.contains(id);
            let triggered = match self.domain_kinds[id] {
                Some(DomainKind::ClockPosedge | DomainKind::ResetAsyncHigh) => {
                    (!was_nonzero || from_unknown) && is_nonzero
                }
                Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => {
                    (was_nonzero || from_unknown) && !is_nonzero
                }
                _ => (!was_nonzero || from_unknown) && is_nonzero,
            };
            if triggered {
                scheduled_trigger_ids.insert(id);
                executor.backend_mut().mark_triggered_bit(id);
            }
        }

        executor.eval_comb()?;
        if track_stable_edges {
            // Combinational settling before an active scheduled source domain
            // commits may expose transient derived-clock edges. In that case,
            // keep only the source event and rediscover stable edges after the
            // commit. If the source edge is inactive, preserve derived edges
            // while filtering out the scheduled signal's own transition.
            if scheduled_trigger_ids.is_empty() {
                self.replace_triggers_with_stable_edges(executor.backend_mut());
            } else {
                executor.backend_mut().clear_triggered_bits();
                for id in scheduled_trigger_ids.iter() {
                    executor.backend_mut().mark_triggered_bit(id);
                }
            }
        }

        let mut comb_already_done = false;
        loop {
            let mut any_new_outer_loop_trigger = false;
            let mut newly_triggered = Vec::new();

            loop {
                let mut any_new_sequential_trigger = false;
                let marked_bits = executor.backend().get_triggered_bits();
                executor.backend_mut().clear_triggered_bits();

                let mut can_use_eval_apply =
                    triggered_domains.is_empty() && marked_bits.count() == 1;
                if can_use_eval_apply {
                    let single_id = marked_bits.iter().next().expect("one marked trigger");
                    let info = self.event_info[single_id];
                    can_use_eval_apply = !info.is_cascaded;
                    if can_use_eval_apply {
                        if let Some(event) = info.eval_ff_event {
                            discovered_in_this_step.insert(single_id);
                            triggered_domains.insert(info.canonical_id);
                            any_new_outer_loop_trigger = true;
                            executor.stage_external_event(event, current_time)?;
                            executor.eval_apply_ff_at(event)?;
                            executor.fire_external_event(event, current_time)?;
                            executor.eval_comb()?;
                            if track_stable_edges {
                                self.replace_triggers_with_stable_edges(executor.backend_mut());
                            }
                            comb_already_done = true;
                            break;
                        }
                    }
                }

                for id in marked_bits.iter() {
                    if discovered_in_this_step.contains(id) {
                        continue;
                    }
                    discovered_in_this_step.insert(id);

                    let info = self.event_info[id];
                    if triggered_domains.contains(info.canonical_id) {
                        continue;
                    }
                    triggered_domains.insert(info.canonical_id);
                    any_new_sequential_trigger = true;
                    newly_triggered.push(info.canonical_id);

                    if let Some(event) = info.eval_only_event {
                        executor.stage_external_event(
                            info.eval_ff_event.unwrap_or(event),
                            current_time,
                        )?;
                        executor.eval_only_ff_at(event)?;
                    } else if let Some(event) = info.eval_ff_event {
                        executor.stage_external_event(event, current_time)?;
                        executor.eval_apply_ff_at(event)?;
                    } else {
                        unreachable!(
                            "FF trigger discovered without a corresponding execution unit"
                        );
                    }
                }

                if !any_new_sequential_trigger {
                    break;
                }
            }

            if newly_triggered.is_empty() && !any_new_outer_loop_trigger {
                break;
            }

            for id in &newly_triggered {
                if let Some(event) = self.event_info[*id].apply_event {
                    executor.apply_ff_at(event)?;
                }
            }
            for id in &newly_triggered {
                if let Some(event) = self.event_info[*id].eval_ff_event {
                    executor.fire_external_event(event, current_time)?;
                }
            }

            if comb_already_done {
                comb_already_done = false;
            } else {
                executor.eval_comb()?;
                if track_stable_edges {
                    self.replace_triggers_with_stable_edges(executor.backend_mut());
                }
            }
        }

        for (signal, id, _) in &self.topo_signals {
            if *id == usize::MAX {
                continue;
            }
            let (is_nonzero, is_unknown) = sample_event_signal(executor.backend(), *signal);
            if is_nonzero {
                self.last_clock_values.insert(*id);
            } else {
                self.last_clock_values.remove(*id);
            }
            if is_unknown {
                self.unknown_clock_values.insert(*id);
            } else {
                self.unknown_clock_values.remove(*id);
            }
        }

        for event in periodic_events_to_process {
            let event_id = event.event_ref.id();
            if let Some(Some(clock)) = self.scheduler.clocks.get(event_id) {
                self.push_periodic_event(SimEvent {
                    time: current_time + clock.period / 2,
                    event_ref: event.event_ref,
                    signal: event.signal,
                    next_val: 1 - event.next_val,
                });
            }
        }

        Ok(true)
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
        let process = self.process_wakeups.peek().map(|&Reverse((time, _))| time);
        let budget_spent = self.tick_budget_spent();
        let clocks = self
            .tick_clocks
            .iter()
            .flat_map(|(id, clock)| {
                [
                    (!budget_spent && self.clock_has_waiters(*id)).then_some(clock.next_edge),
                    clock.falling_edge,
                ]
            })
            .flatten()
            .min();
        [self.scheduler.next_event_time(), process, clocks]
            .into_iter()
            .flatten()
            .min()
    }

    /// Periodic clocks as (event id, period).
    pub fn clock_periods(&self) -> Vec<(usize, u64)> {
        self.scheduler
            .clocks
            .iter()
            .enumerate()
            .filter_map(|(id, clock)| Some((id, clock.as_ref()?.period)))
            .collect()
    }

    /// Capture the mutable scheduling state: time, pending events, clocks and
    /// the clock values edge detection compares against.
    pub fn snapshot(&self) -> SimulationSnapshot<B> {
        SimulationSnapshot {
            time: self.scheduler.time,
            clocks: self.scheduler.clocks.clone(),
            event_queue: self.scheduler.event_queue.clone(),
            periodic_events: self.periodic_events.clone(),
            last_clock_values: self.last_clock_values.clone(),
            unknown_clock_values: self.unknown_clock_values.clone(),
            process_wakeups: self.process_wakeups.clone(),
            waiting: self.waiting.clone(),
            tick_clocks: self.tick_clocks.clone(),
            clock_waits: self.clock_waits.clone(),
            done: self.done.clone(),
            pending_releases: self.pending_releases.clone(),
            ticks: self.ticks,
            finished: self.finished,
        }
    }

    /// Return to a state captured by [`Self::snapshot`] on a state built for
    /// the same design.
    ///
    /// Event handles may point into the compiled code of the instance that took
    /// the snapshot, so `remap` translates each pending event into a handle of
    /// this instance. If an event cannot be translated, nothing is changed and
    /// that event is returned.
    pub fn restore(
        &mut self,
        snapshot: &SimulationSnapshot<B>,
        mut remap: impl FnMut(B::Event) -> Option<B::Event>,
    ) -> Result<(), B::Event> {
        let event_queue = snapshot
            .event_queue
            .iter()
            .map(|event| {
                Ok(SimEvent {
                    event_ref: remap(event.event_ref).ok_or(event.event_ref)?,
                    ..event.clone()
                })
            })
            .collect::<Result<_, B::Event>>()?;
        self.scheduler.time = snapshot.time;
        self.scheduler.clocks.clone_from(&snapshot.clocks);
        self.scheduler.event_queue = event_queue;
        self.periodic_events.clone_from(&snapshot.periodic_events);
        self.last_clock_values
            .clone_from(&snapshot.last_clock_values);
        self.unknown_clock_values
            .clone_from(&snapshot.unknown_clock_values);
        let tick_clocks = snapshot
            .tick_clocks
            .iter()
            .map(|(id, clock)| {
                Ok((
                    *id,
                    TickClock {
                        event: remap(clock.event).ok_or(clock.event)?,
                        ..clock.clone()
                    },
                ))
            })
            .collect::<Result<_, B::Event>>()?;
        self.process_wakeups.clone_from(&snapshot.process_wakeups);
        self.waiting.clone_from(&snapshot.waiting);
        self.tick_clocks = tick_clocks;
        self.clock_waits.clone_from(&snapshot.clock_waits);
        self.done.clone_from(&snapshot.done);
        self.pending_releases.clone_from(&snapshot.pending_releases);
        self.ticks = snapshot.ticks;
        self.finished = snapshot.finished;
        Ok(())
    }
}

/// Scheduling state expressed with the event handles of one backend, so a
/// caller can translate it to and from names.
pub struct ScheduleParts<B: SimBackend> {
    pub time: u64,
    /// Periodic clocks and their periods.
    pub clocks: Vec<(B::Event, u64)>,
    /// Pending events, including those of periodic clocks.
    pub events: Vec<SimEvent<B>>,
    /// Pending events that a periodic clock re-schedules when they fire, with
    /// the number of identical such events.
    pub periodic: Vec<(SimEvent<B>, u64)>,
    /// Events whose signal was high when edge detection last sampled it.
    pub high_events: Vec<B::Event>,
    /// Suspended processes and the time each resumes at.
    pub process_wakeups: Vec<(usize, u64)>,
    /// Processes waiting for an event or a condition, in ascending order.
    pub waiting_processes: Vec<usize>,
    /// Process clocks that have been waited on.
    pub process_clocks: Vec<ProcessClockState<B>>,
    /// Processes waiting for edges of a process clock: the process, the
    /// clock's event, the edges still to wait for, and the release index.
    pub clock_waits: Vec<(usize, B::Event, u64, Option<usize>)>,
    /// Releases to make when a process resumes: process and release index.
    pub pending_releases: Vec<(usize, usize)>,
    /// Processes that ran to their end, in ascending order.
    pub done_processes: Vec<usize>,
    /// Rising edges of process clocks so far.
    pub ticks: u64,
    /// A process requested the end of the simulation.
    pub finished: bool,
}

impl<B: SimBackend> SimulationState<B> {
    /// Express the scheduling state with `backend`'s event handles.
    pub fn export_schedule(&self, backend: &B) -> ScheduleParts<B> {
        let events = backend.id_to_event_slice();
        ScheduleParts {
            time: self.scheduler.time,
            clocks: self
                .scheduler
                .clocks
                .iter()
                .enumerate()
                .filter_map(|(id, clock)| Some((events[id], clock.as_ref()?.period)))
                .collect(),
            events: self.scheduler.event_queue.iter().cloned().collect(),
            periodic: self
                .periodic_events
                .iter()
                .map(|(key, &count)| {
                    (
                        SimEvent {
                            time: key.time,
                            event_ref: events[key.event_id],
                            signal: key.signal,
                            next_val: key.next_val,
                        },
                        count as u64,
                    )
                })
                .collect(),
            high_events: self.last_clock_values.iter().map(|id| events[id]).collect(),
            process_wakeups: {
                let mut wakeups = self
                    .process_wakeups
                    .iter()
                    .map(|&Reverse((time, process))| (process, time))
                    .collect::<Vec<_>>();
                wakeups.sort_unstable();
                wakeups
            },
            waiting_processes: self.waiting.iter().copied().collect(),
            process_clocks: self
                .tick_clocks
                .values()
                .map(|clock| ProcessClockState {
                    event: clock.event,
                    signal: clock.signal,
                    period: clock.period,
                    next_edge: clock.next_edge,
                    falling_edge: clock.falling_edge,
                })
                .collect(),
            clock_waits: self
                .clock_waits
                .iter()
                .map(|(process, wait)| (*process, events[wait.clock], wait.remaining, wait.release))
                .collect(),
            pending_releases: self
                .pending_releases
                .iter()
                .map(|(process, release)| (*process, *release))
                .collect(),
            done_processes: self.done.iter().copied().collect(),
            ticks: self.ticks,
            finished: self.finished,
        }
    }

    /// Replace the scheduling state with `parts`, whose handles belong to
    /// this state's backend.
    pub fn import_schedule(&mut self, parts: ScheduleParts<B>) {
        self.scheduler.time = parts.time;
        self.scheduler.clocks.clear();
        for (event, period) in parts.clocks {
            let id = event.id();
            if id >= self.scheduler.clocks.len() {
                self.scheduler.clocks.resize(id + 1, None);
            }
            self.scheduler.clocks[id] = Some(ClockDef { period });
        }
        self.scheduler.event_queue = parts.events.into_iter().collect();
        self.periodic_events = parts
            .periodic
            .into_iter()
            .map(|(event, count)| (PeriodicEventKey::from_event(&event), count as usize))
            .collect();
        self.last_clock_values.make_empty();
        // A state file holds known values only.
        self.unknown_clock_values.make_empty();
        for event in parts.high_events {
            self.last_clock_values.insert(event.id());
        }
        self.process_wakeups = parts
            .process_wakeups
            .into_iter()
            .map(|(process, time)| Reverse((time, process)))
            .collect();
        self.waiting = parts.waiting_processes.into_iter().collect();
        self.tick_clocks = parts
            .process_clocks
            .into_iter()
            .map(|clock| {
                (
                    clock.event.id(),
                    TickClock {
                        event: clock.event,
                        signal: clock.signal,
                        period: clock.period,
                        next_edge: clock.next_edge,
                        falling_edge: clock.falling_edge,
                    },
                )
            })
            .collect();
        self.clock_waits = parts
            .clock_waits
            .into_iter()
            .map(|(process, event, remaining, release)| {
                (
                    process,
                    ClockWait {
                        clock: event.id(),
                        remaining,
                        release,
                    },
                )
            })
            .collect();
        self.pending_releases = parts.pending_releases.into_iter().collect();
        self.done = parts.done_processes.into_iter().collect();
        self.ticks = parts.ticks;
        self.finished = parts.finished;
    }
}

/// Scheduling state captured by [`SimulationState::snapshot`].
pub struct SimulationSnapshot<B: SimBackend> {
    time: u64,
    clocks: Vec<Option<ClockDef>>,
    event_queue: std::collections::BinaryHeap<SimEvent<B>>,
    periodic_events: FxHashMap<PeriodicEventKey, usize>,
    last_clock_values: BitSet,
    unknown_clock_values: BitSet,
    process_wakeups: BinaryHeap<ProcessWakeup>,
    waiting: BTreeSet<usize>,
    tick_clocks: BTreeMap<usize, TickClock<B>>,
    clock_waits: BTreeMap<usize, ClockWait>,
    done: BTreeSet<usize>,
    pending_releases: BTreeMap<usize, usize>,
    ticks: u64,
    finished: bool,
}

impl<B: SimBackend> Clone for SimulationSnapshot<B> {
    fn clone(&self) -> Self {
        Self {
            time: self.time,
            clocks: self.clocks.clone(),
            event_queue: self.event_queue.clone(),
            periodic_events: self.periodic_events.clone(),
            last_clock_values: self.last_clock_values.clone(),
            unknown_clock_values: self.unknown_clock_values.clone(),
            process_wakeups: self.process_wakeups.clone(),
            waiting: self.waiting.clone(),
            tick_clocks: self.tick_clocks.clone(),
            clock_waits: self.clock_waits.clone(),
            done: self.done.clone(),
            pending_releases: self.pending_releases.clone(),
            ticks: self.ticks,
            finished: self.finished,
        }
    }
}

impl<B: SimBackend> SimulationSnapshot<B> {
    /// Simulation time at which the snapshot was taken.
    pub fn time(&self) -> u64 {
        self.time
    }
}
