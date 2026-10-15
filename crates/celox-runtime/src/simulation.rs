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

/// Bound all active and inactive passes within one simulation time slot.
const PROCESS_DELTA_LIMIT: usize = 100_000;

fn count_delta_pass(rounds: &mut usize) -> Result<(), SimulatorErrorCode> {
    *rounds += 1;
    if *rounds > PROCESS_DELTA_LIMIT {
        return Err(SimulatorErrorCode::Runtime {
            message: "process delta limit exceeded without advancing time".to_owned(),
            signals: Vec::new(),
        });
    }
    Ok(())
}

/// Backend execution hooks needed by the timed simulation engine.
///
/// The facade implements this contract to retain policy such as runtime-event
/// decoration and waveform capture outside the backend-independent scheduler.
pub trait SimulationExecutor {
    type Backend: SimBackend;
    fn finish_requested(&mut self, _time: u64) -> bool {
        false
    }

    /// Whether individual scheduled writes have observable combinational
    /// effects even without an event waiter.
    fn scheduled_updates_need_settle(&self) -> bool {
        false
    }

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
        {
            let result = self.eval_apply_ff_at(event);
            (u64::from(result.is_ok()), result)
        }
    }

    /// Whether clock waits may be served by fused ticks, which skip the
    /// clock signal's own edges and the host's per-edge hooks.
    fn fused_ticks_allowed(&self) -> bool {
        true
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
    pub clocks: Vec<ProcessClockRef<B>>,
}

impl<B: SimBackend> Clone for ProcessRefs<B> {
    fn clone(&self) -> Self {
        Self {
            status: self.status,
            delay: self.delay,
            clock: self.clock,
            clocks: self.clocks.clone(),
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
/// with event id `clock`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ClockWait {
    clock: usize,
    remaining: u64,
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
    round_published_domains: BitSet,
    round_triggered_domains: BitSet,
    scheduled_clock_drives: BitSet,
    scheduled_clock_time: u64,
    round_evaluated: bool,
    round_new_rising: BitSet,
    periodic_events: FxHashMap<PeriodicEventKey, usize>,
    last_clock_values: BitSet,
    pending_host_edges: BitSet,
    unknown_clock_values: BitSet,
    topo_signals: Vec<(SignalRef, usize, usize)>,
    domain_kinds: Vec<Option<DomainKind>>,
    event_info: Vec<EventInfo<B>>,
    signal_to_id: FxHashMap<SignalRef, usize>,
    signal_index_by_id: Vec<usize>,
    processes: Vec<ProcessRefs<B>>,
    process_event_counters: Vec<(usize, SignalRef, SignalRef)>,
    active_ff_domains: BitSet,
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
    /// Rising edges of process clocks so far.
    ticks: u64,
    inactive: BTreeSet<usize>,
    /// A process requested the end of the simulation.
    finished: bool,
    finish_round: bool,
}

fn transition_edges(was: (bool, bool), is: (bool, bool)) -> (bool, bool) {
    let (was_nonzero, was_unknown) = was;
    let (is_nonzero, is_unknown) = is;
    match (was_unknown, is_unknown) {
        (true, true) => (false, false),
        (true, false) => (is_nonzero, !is_nonzero),
        (false, true) => (!was_nonzero, was_nonzero),
        (false, false) => (!was_nonzero && is_nonzero, was_nonzero && !is_nonzero),
    }
}

/// The event signal's least significant bit and its X/Z mask.
fn sample_event_signal<B: SimBackend>(backend: &B, signal: SignalRef) -> (bool, bool) {
    if backend.layout().four_state && signal.is_4state {
        let (value, mask) = backend.get_four_state(signal);
        (value.bit(0), mask.bit(0))
    } else {
        let value: u8 = backend.get_as(signal);
        (value & 1 != 0, false)
    }
}

/// The error of a process clock whose next edge lies beyond simulation time.
fn clock_overflow(clock: usize) -> SimulatorErrorCode {
    SimulatorErrorCode::Runtime {
        message: format!("process clock {clock} overflows simulation time"),
        signals: Vec::new(),
    }
}

impl<B: SimBackend> SimulationState<B> {
    /// Rebase edge detection after state was advanced outside this scheduler.
    pub fn synchronize_event_values(&mut self, backend: &B) {
        self.pending_host_edges.make_empty();
        self.sample_event_values(backend);
    }

    fn sample_event_values(&mut self, backend: &B) {
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

    /// Retain FF occurrences observed by a host evaluation until the scheduler runs.
    pub fn capture_host_edges(&mut self, backend: &B) {
        if self.finished {
            return;
        }
        for (_, id, _) in &self.topo_signals {
            if *id == usize::MAX {
                continue;
            }
            let info = &self.event_info[*id];
            if (info.eval_ff_event.is_some()
                || info.eval_only_event.is_some()
                || info.apply_event.is_some())
                && self.stable_edge_triggered(backend, *id)
            {
                self.pending_host_edges.insert(*id);
            }
        }
        self.sample_event_values(backend);
    }

    fn stable_edge_triggered(&self, backend: &B, id: usize) -> bool {
        let Some((signal, _, _)) = self.topo_signals.get(self.signal_index_by_id[id]) else {
            return true;
        };
        let was = (
            self.last_clock_values.contains(id),
            self.unknown_clock_values.contains(id),
        );
        let is = sample_event_signal(backend, *signal);
        match self.domain_kinds[id] {
            Some(
                DomainKind::ClockPosedge
                | DomainKind::ResetAsyncHigh
                | DomainKind::ClockNegedge
                | DomainKind::ResetAsyncLow,
            ) => self.edge_triggers(id, was, is),
            _ => was != is,
        }
    }

    fn replace_triggers_with_stable_edges(&self, backend: &mut B) {
        backend.clear_triggered_bits();
        for (signal, id, _) in &self.topo_signals {
            if *id == usize::MAX {
                continue;
            }
            let was = (
                self.last_clock_values.contains(*id),
                self.unknown_clock_values.contains(*id),
            );
            let is = sample_event_signal(backend, *signal);
            let (posedge, negedge) = transition_edges(was, is);
            let triggered = match self.domain_kinds[*id] {
                Some(DomainKind::ClockPosedge | DomainKind::ResetAsyncHigh) => posedge,
                Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => negedge,
                _ => was != is,
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
        let mut signal_index_by_id = vec![usize::MAX; backend.num_events()];
        for (index, (signal, id, _)) in topo_signals.iter().copied().enumerate() {
            if id == usize::MAX {
                continue;
            }
            signal_to_id.insert(signal, id);
            signal_index_by_id[id] = index;
            let (value, unknown) = sample_event_signal(backend, signal);
            if value {
                last_clock_values.insert(id);
            }
            if unknown {
                unknown_clock_values.insert(id);
            }
        }

        Self {
            scheduler: Scheduler::new(),
            periodic_events: FxHashMap::default(),
            last_clock_values,
            pending_host_edges: BitSet::new(),
            unknown_clock_values,
            topo_signals,
            domain_kinds,
            event_info,
            signal_to_id,
            signal_index_by_id,
            process_wakeups: (0..processes.len())
                .map(|process| Reverse((0, process)))
                .collect(),
            processes,
            process_event_counters: Vec::new(),
            active_ff_domains: BitSet::new(),
            waiting: BTreeSet::new(),
            tick_clocks: BTreeMap::new(),
            clock_waits: BTreeMap::new(),
            done: BTreeSet::new(),
            ticks: 0,
            inactive: BTreeSet::new(),
            finished: false,
            round_published_domains: BitSet::new(),
            round_triggered_domains: BitSet::new(),
            scheduled_clock_drives: BitSet::new(),
            scheduled_clock_time: 0,
            round_evaluated: false,
            round_new_rising: BitSet::new(),
            finish_round: false,
        }
    }

    /// Rising edges of process clocks the simulation has run so far.
    pub fn ticks(&self) -> u64 {
        self.ticks
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
        self.scheduler.clocks[event_id] = Some(ClockDef {
            period: period.max(2),
        });
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

    /// Mark domains whose evaluation publishes blocking stores before NBA application.
    pub fn with_active_ff_events(mut self, events: Vec<B::Event>) -> Self {
        self.active_ff_domains = events
            .into_iter()
            .map(|event| self.event_info[event.id()].canonical_id)
            .collect();
        self
    }

    /// Attach exact kernel edge counters to their runtime event domains.
    pub fn with_process_event_counters(
        mut self,
        counters: Vec<(SignalRef, SignalRef, SignalRef)>,
    ) -> Self {
        self.process_event_counters = counters
            .into_iter()
            .filter_map(|(signal, rises, falls)| {
                self.signal_to_id.get(&signal).map(|id| (*id, rises, falls))
            })
            .collect();
        self
    }

    /// Capture suspended event waits without executing their continuations.
    pub fn sample_waiting_events<E>(&mut self, executor: &mut E) -> Result<(), SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        if self.finished {
            return Ok(());
        }
        for &process in &self.waiting {
            let refs = &self.processes[process];
            if !matches!(
                ProcessStatus::from_code(executor.backend().get_as(refs.status)),
                Some(ProcessStatus::Wait | ProcessStatus::Pending)
            ) {
                continue;
            }
            let delay = executor.backend().get_as::<u64>(refs.delay);
            executor.backend_mut().set(refs.delay, 1u64);
            let result = executor.run_process(process);
            executor.backend_mut().set(refs.delay, delay);
            result?;
        }
        Ok(())
    }

    /// Clock signals changed outside a scheduler round through retained memory.
    pub fn escaped_host_clock_writes(&self, backend: &B) -> Vec<SignalRef> {
        self.processes
            .iter()
            .flat_map(|process| &process.clocks)
            .filter_map(|clock| {
                let id = clock.event.id();
                let baseline = (
                    self.last_clock_values.contains(id),
                    self.unknown_clock_values.contains(id),
                );
                (baseline != sample_event_signal(backend, clock.signal)).then_some(clock.signal)
            })
            .collect()
    }

    /// A generated process clock owns its signal, including before its first wait.
    pub fn validate_host_write(&self, signal: SignalRef) -> Result<(), SimulatorErrorCode> {
        if let Some(clock) = self
            .processes
            .iter()
            .flat_map(|process| &process.clocks)
            .find(|clock| clock.signal == signal)
        {
            return Err(SimulatorErrorCode::Runtime {
                message: format!(
                    "process clock {} is driven by a direct host write",
                    clock.event.id()
                ),
                signals: Vec::new(),
            });
        }
        Ok(())
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
        for (id, clock) in &self.tick_clocks {
            if self.clock_has_waiters(*id) || clock.falling_edge.is_some() {
                self.check_clock_generator(*id)?;
            }
        }
        let Some(current_time) = self.next_event_time() else {
            return Ok(None);
        };
        if current_time > limit {
            return Ok(None);
        }
        let mut events_to_process = if self.scheduler.next_event_time() == Some(current_time) {
            self.scheduler
                .pop_all_at_next_time()
                .map(|(_, events)| events)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if self.scheduled_clock_time != current_time {
            self.scheduled_clock_drives.make_empty();
            self.scheduled_clock_time = current_time;
        }
        for event in &events_to_process {
            self.scheduled_clock_drives.insert(event.event_ref.id());
            if let Some(id) = self.signal_to_id.get(&event.signal) {
                self.scheduled_clock_drives.insert(*id);
            }
        }
        let mut falling = Vec::new();
        for (id, clock) in &self.tick_clocks {
            if clock.falling_edge == Some(current_time) {
                falling.push(*id);
                events_to_process.push(SimEvent {
                    time: current_time,
                    event_ref: clock.event,
                    signal: clock.signal,
                    next_val: 0,
                });
            }
        }
        let rising: Vec<usize> = self
            .tick_clocks
            .iter()
            .filter(|(id, clock)| {
                let high = sample_event_signal(executor.backend(), clock.signal) == (true, false);
                let pending_rise = high
                    && (!self.last_clock_values.contains(**id)
                        || self.unknown_clock_values.contains(**id));
                (!events_to_process.is_empty() || pending_rise)
                    && !falling.contains(*id)
                    && clock.next_edge == current_time
                    && self.clock_has_waiters(**id)
                    && (!high || pending_rise)
            })
            .map(|(id, _)| *id)
            .collect();
        let mut generated_events = Vec::new();
        for id in &rising {
            let clock = &self.tick_clocks[id];
            generated_events.push(SimEvent {
                time: current_time,
                event_ref: clock.event,
                signal: clock.signal,
                next_val: 1,
            });
        }
        let ready = self.take_ready_processes(current_time);
        let result = self.step_round(
            executor,
            current_time,
            events_to_process,
            generated_events,
            ready,
            true,
        );
        self.finish_clock_round(current_time, rising, &falling, result)?;
        self.run_remaining_rounds(executor, current_time, limit)?;
        executor.finish_timed_step(self.scheduler.time);
        // Fused ticks may have moved the time on.
        Ok(Some(self.scheduler.time))
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
            let due: Vec<usize> = self
                .tick_clocks
                .iter()
                .filter(|(id, clock)| {
                    clock.next_edge == current_time && self.clock_has_waiters(**id)
                })
                .map(|(id, _)| *id)
                .collect();
            if let [clock] = due[..]
                && self.fused_ticks(executor, clock, current_time, limit)?
            {
                // Time moved on; the processes that resume run in the next step.
                return Ok(());
            }
            let fired = !due.is_empty();
            if fired {
                self.fire_edges(executor, current_time, &due)?;
            }
            let ready = self.take_ready_processes(current_time);
            if ready.is_empty() && self.waiting.is_empty() {
                if fired {
                    // The edges may have released waits that resume now.
                    continue;
                }
                break;
            }
            let result =
                self.step_round(executor, current_time, Vec::new(), Vec::new(), ready, false);
            let progressed = result.as_ref().is_ok_and(|ran| *ran);
            self.finish_clock_round(current_time, Vec::new(), &[], result)?;
            if !progressed && !fired {
                break;
            }
        }
        Ok(())
    }

    /// Apply the rising edges of the process clocks `due` at `time`, count
    /// them against the processes waiting on them, and schedule the
    /// resumptions and falling edges.
    fn fire_edges<E>(
        &mut self,
        executor: &mut E,
        time: u64,
        due: &[usize],
    ) -> Result<(), SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        for id in due {
            time.checked_add(self.tick_clocks[id].period)
                .ok_or_else(|| clock_overflow(*id))?;
        }
        // A clock that is high when its edge is due (the host or a previous
        // run left it so) falls first, in a round of its own: the rising
        // edge that counts is then a real one.
        let mut events = Vec::with_capacity(due.len());
        let mut rising = Vec::with_capacity(due.len());
        let mut falling = Vec::new();
        for id in due {
            let clock = &self.tick_clocks[id];
            let high = sample_event_signal(executor.backend(), clock.signal) == (true, false);
            events.push(SimEvent {
                time,
                event_ref: clock.event,
                signal: clock.signal,
                next_val: u8::from(!high),
            });
            if !high {
                rising.push(*id);
            } else {
                falling.push(*id);
            }
        }
        for id in &falling {
            self.tick_clocks
                .get_mut(id)
                .expect("a normalizing clock")
                .falling_edge = Some(time);
        }
        let result = self.step_round(executor, time, events, Vec::new(), Vec::new(), true);
        self.finish_clock_round(time, rising, &falling, result)
    }

    fn finish_clock_round(
        &mut self,
        time: u64,
        rising: Vec<usize>,
        falling: &[usize],
        result: Result<bool, SimulatorErrorCode>,
    ) -> Result<(), SimulatorErrorCode> {
        let rising: BTreeSet<_> = rising
            .into_iter()
            .chain(self.round_new_rising.iter())
            .collect();
        let committed = rising
            .into_iter()
            .filter(|id| {
                (result.is_ok() && !self.finished)
                    || self
                        .round_published_domains
                        .contains(self.event_info[*id].canonical_id)
            })
            .collect();
        self.credit_rising_edges(time, committed)?;
        if result.is_ok()
            || (self.round_evaluated
                && self
                    .round_triggered_domains
                    .is_subset(&self.round_published_domains))
        {
            for id in falling {
                self.tick_clocks
                    .get_mut(id)
                    .expect("a pending process-clock fall")
                    .falling_edge = None;
                self.last_clock_values.remove(*id);
            }
        }
        result.map(|_| ())
    }

    fn credit_rising_edges(
        &mut self,
        time: u64,
        rising: Vec<usize>,
    ) -> Result<(), SimulatorErrorCode> {
        for id in rising {
            let clock = self.tick_clocks.get_mut(&id).expect("a due process clock");
            let next_edge = time
                .checked_add(clock.period)
                .ok_or_else(|| clock_overflow(id))?;
            self.ticks += 1;
            clock.next_edge = next_edge;
            clock.falling_edge = Some(next_edge - clock.period / 2);
            let resume_at = clock.next_edge;
            self.last_clock_values.insert(id);
            self.count_edges(id, 1, resume_at);
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
            self.clock_waits.remove(&process);
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
        // The fused kernel runs the waited-on clock's domain alone: a design
        // whose registers drive another clock or reset needs the scheduler's
        // rounds, and a clock that is high now needs its normalizing fall.
        if time > limit
            || !executor.fused_ticks_allowed()
            || self.domain_kinds[clock] != Some(DomainKind::ClockPosedge)
            || !self.inactive.is_empty()
            || !self.waiting.is_empty()
            || !self.process_wakeups.is_empty()
            || self.scheduler.next_event_time().is_some()
            || self
                .tick_clocks
                .values()
                .any(|clock| clock.falling_edge.is_some())
            || self.clock_waits.values().any(|wait| wait.clock != clock)
            || self.clock_waits.len() + self.done.len() != self.processes.len()
            || self.event_info.iter().any(|info| info.is_cascaded)
            || sample_event_signal(executor.backend(), self.tick_clocks[&clock].signal)
                != (false, false)
        {
            return Ok(false);
        }
        let Some(needed) = self.clock_waits.values().map(|wait| wait.remaining).min() else {
            return Ok(false);
        };
        let (event, signal, period) = {
            let clock = &self.tick_clocks[&clock];
            (clock.event, clock.signal, clock.period)
        };
        // Edges at `time`, `time + period`, ...: the last one stays at or
        // before `limit`.
        let count = needed.min(limit.saturating_sub(time) / period + 1);
        if count == 0 {
            return Ok(false);
        }
        // If the batch's resumption cannot be represented, execute normal
        // bounded edges instead. They diagnose the offending edge before it
        // mutates the backend, retaining the valid earlier progress.
        if count
            .checked_mul(period)
            .and_then(|span| time.checked_add(span))
            .is_none()
        {
            return Ok(false);
        }
        // Combinational next-state logic samples the rising level of the clock.
        executor.backend_mut().set(signal, 1u8);
        let (completed, result) = executor.tick_many(event, count);
        if completed == 0 {
            executor.backend_mut().set(signal, 0u8);
        }
        if completed > count || (completed == 0 && result.is_ok()) {
            return Err(SimulatorErrorCode::Runtime {
                message: "the backend made invalid progress on fused clock ticks".into(),
                signals: Vec::new(),
            });
        }
        // The edges the backend completed count, also before an error, so
        // the schedule agrees with the state it left.
        if completed != 0 {
            self.ticks += completed;
            self.scheduler.time = time + (completed - 1) * period;
            let next_edge = completed
                .checked_mul(period)
                .and_then(|span| time.checked_add(span))
                .ok_or_else(|| clock_overflow(clock))?;
            let tick_clock = self
                .tick_clocks
                .get_mut(&clock)
                .expect("a due process clock");
            tick_clock.next_edge = next_edge;
            tick_clock.falling_edge = Some(next_edge - period / 2);
            self.last_clock_values.insert(clock);
            self.count_edges(clock, completed, next_edge);
        }
        result?;
        // The processes that resume read the state settled after the last
        // edge.
        executor.eval_comb()?;
        Ok(true)
    }

    fn check_clock_generator(&self, id: usize) -> Result<(), SimulatorErrorCode> {
        if self.scheduler.clocks.get(id).is_some_and(Option::is_some) {
            return Err(SimulatorErrorCode::Runtime {
                message: format!("process clock {id} is already driven by add_clock"),
                signals: Vec::new(),
            });
        }
        if (self.scheduled_clock_time == self.scheduler.time
            && self.scheduled_clock_drives.contains(id))
            || self
                .scheduler
                .event_queue
                .iter()
                .any(|event| event.event_ref.id() == id)
        {
            return Err(SimulatorErrorCode::Runtime {
                message: format!("process clock {id} is already driven by schedule"),
                signals: Vec::new(),
            });
        }
        Ok(())
    }

    /// Whether a clock/reset differs from the last settled scheduler state.
    /// Host reads may have already cleared backend trigger bits.
    pub fn has_unsettled_edges(&self, backend: &B) -> bool {
        self.topo_signals.iter().any(|(signal, id, _)| {
            *id != usize::MAX
                && (
                    self.last_clock_values.contains(*id),
                    self.unknown_clock_values.contains(*id),
                ) != sample_event_signal(backend, *signal)
        })
    }

    /// Pending external clock/reset changes, excluding drives whose phase
    /// belongs to a process clock. Those must retry with their scheduled batch.
    pub fn has_unsettled_host_edges(&self, backend: &B) -> bool {
        self.topo_signals.iter().any(|(signal, id, _)| {
            *id != usize::MAX
                && !self
                    .processes
                    .iter()
                    .flat_map(|process| &process.clocks)
                    .any(|clock| clock.signal == *signal)
                && (
                    self.last_clock_values.contains(*id),
                    self.unknown_clock_values.contains(*id),
                ) != sample_event_signal(backend, *signal)
        })
    }

    /// Whether a pending transition activates its clock/reset domain.
    pub fn has_unsettled_active_edges(&self, backend: &B) -> bool {
        self.topo_signals
            .iter()
            .any(|(_, id, _)| *id != usize::MAX && self.stable_edge_triggered(backend, *id))
    }

    /// Whether any process is suspended on an event or a condition.
    pub fn has_waiting_processes(&self) -> bool {
        !self.waiting.is_empty()
    }

    /// Resume the processes waiting for an event or a condition at the
    /// current time, for state that was changed from outside the scheduler,
    /// and settle what they drive. Returns whether a process ran; nothing
    /// happens when none wakes.
    pub fn poll_waiting<E>(&mut self, executor: &mut E) -> Result<bool, SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        self.poll_waiting_until(executor, u64::MAX)
    }

    /// [`Self::poll_waiting`], where the fused clock ticks a woken process
    /// may start stop before `limit`, as in [`Self::step_until`].
    pub fn poll_waiting_until<E>(
        &mut self,
        executor: &mut E,
        limit: u64,
    ) -> Result<bool, SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        if self.finished
            || (self.waiting.is_empty() && self.pending_host_edges.is_empty())
            || self.scheduler.time > limit
        {
            return Ok(false);
        }
        let time = self.scheduler.time;
        if self
            .scheduler
            .next_event_time()
            .is_some_and(|next| next <= time)
        {
            return Ok(false);
        }
        self.sample_waiting_events(executor)?;
        let ready = self.take_ready_processes(time);
        let result = self.step_round(executor, time, Vec::new(), Vec::new(), ready, false);
        let progressed = result.as_ref().is_ok_and(|ran| *ran);
        self.finish_clock_round(time, Vec::new(), &[], result)?;
        if !progressed {
            return Ok(false);
        }
        self.run_remaining_rounds(executor, time, limit)?;
        executor.finish_timed_step(self.scheduler.time);
        Ok(true)
    }

    /// Remove the processes waiting for `time`, in declaration order.
    fn take_ready_processes(&mut self, time: u64) -> Vec<usize> {
        let mut ready = std::mem::take(&mut self.inactive)
            .into_iter()
            .collect::<Vec<_>>();
        while let Some(&Reverse((wakeup, process))) = self.process_wakeups.peek() {
            if wakeup != time {
                break;
            }
            self.process_wakeups.pop();
            ready.push(process);
        }
        ready
    }

    /// Run `ready` processes, and the processes waiting for an event or a
    /// condition, in declaration order until each suspends or ends. A
    /// process another one wakes runs in a further pass of the same round,
    /// before any register the round triggers updates. A process that waits
    /// for zero time is queued for a later round; inactive-region delays
    /// drain before pending register updates. Returns
    /// whether any process ran a statement.
    fn run_processes<E>(
        &mut self,
        executor: &mut E,
        current_time: u64,
        ready: Vec<usize>,
        delta_rounds: &mut usize,
    ) -> Result<bool, SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        let mut progressed = false;
        let mut pass = ready;
        loop {
            pass.extend(self.waiting.iter().copied());
            pass.sort_unstable();
            pass.dedup();
            if pass.is_empty() {
                return Ok(progressed);
            }
            count_delta_pass(delta_rounds)?;
            let mut any_ran = false;
            for process in pass.drain(..) {
                loop {
                    let before = self
                        .process_event_counters
                        .iter()
                        .map(|(_, rises, falls)| {
                            (
                                executor.backend().get_as::<u32>(*rises),
                                executor.backend().get_as::<u32>(*falls),
                            )
                        })
                        .collect::<Vec<_>>();
                    executor
                        .backend_mut()
                        .set(self.processes[process].delay, 0u64);
                    executor.run_process(process)?;
                    if executor.finish_requested(current_time) {
                        self.finish_round = true;
                        self.finished = true;
                        return Ok(true);
                    }
                    for ((id, rises, falls), (old_rises, old_falls)) in
                        self.process_event_counters.iter().zip(before)
                    {
                        let changed = match self.domain_kinds[*id] {
                            Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => {
                                executor.backend().get_as::<u32>(*falls) != old_falls
                            }
                            _ => executor.backend().get_as::<u32>(*rises) != old_rises,
                        };
                        if changed {
                            executor.backend_mut().mark_triggered_bit(*id);
                        }
                    }
                    let refs = &self.processes[process];
                    let status: u8 = executor.backend().get_as(refs.status);
                    match ProcessStatus::from_code(status) {
                        Some(ProcessStatus::Delay | ProcessStatus::InactiveDelay) => {
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
                            if delay == 0
                                && ProcessStatus::from_code(status)
                                    == Some(ProcessStatus::InactiveDelay)
                            {
                                self.inactive.insert(process);
                            } else {
                                self.process_wakeups.push(Reverse((time, process)));
                            }
                            any_ran = true;
                        }
                        Some(ProcessStatus::Done) => {
                            self.waiting.remove(&process);
                            self.done.insert(process);
                            any_ran = true;
                        }
                        Some(ProcessStatus::Finish) => {
                            self.finish_round = executor.finish_requested(current_time);
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
                            let Some(clock) = refs.clocks.get(index as usize) else {
                                return Err(SimulatorErrorCode::InternalError);
                            };
                            self.waiting.remove(&process);
                            if count == 0 {
                                // A zero-count wait does not yield to another process,
                                // but repeated immediate continuations must be bounded.
                                count_delta_pass(delta_rounds)?;
                                continue;
                            } else {
                                let id = clock.event.id();
                                self.check_clock_generator(id)?;
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
                                    },
                                );
                            }
                            any_ran = true;
                        }
                        // The kernel found its wait condition unmet and ran
                        // nothing; it stays waiting.
                        Some(ProcessStatus::Pending) => {}
                        Some(ProcessStatus::Ready) | None => {
                            return Err(SimulatorErrorCode::InternalError);
                        }
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

    /// Record a finish emitted by the host while settling current-time writes.
    pub fn finish(&mut self) {
        self.finish_round = true;
        self.finished = true;
    }

    /// Whether a process requested the end of the simulation.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Settle externally driven state against the previous edge baseline even
    /// when no clock/reset signal is explicitly scheduled at this timestamp.
    /// The events and process wakeups due before `time` are processed first,
    /// at their own times, and the processes due at `time` run.
    pub fn settle_at<E>(
        &mut self,
        executor: &mut E,
        time: u64,
    ) -> Result<Option<u64>, SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        let mut settled_target = false;
        while self.next_event_time().is_some_and(|next| next <= time) {
            settled_target |= self.step_until(executor, time)? == Some(time);
        }
        if self.finished {
            return Ok(None);
        }
        if settled_target {
            return Ok(Some(time));
        }
        let ready = self.take_ready_processes(time);
        let result = self.step_round(executor, time, Vec::new(), Vec::new(), ready, true);
        self.finish_clock_round(time, Vec::new(), &[], result)?;
        // A process the settled state woke may wait for zero time: the
        // rounds of this time are drained, as `step` drains them.
        self.run_remaining_rounds(executor, time, time)?;
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
        mut events_to_process: Vec<SimEvent<B>>,
        mut generated_events: Vec<SimEvent<B>>,
        ready_processes: Vec<usize>,
        settle_idle: bool,
    ) -> Result<bool, SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        self.scheduler.time = current_time;
        self.round_published_domains.make_empty();
        self.round_triggered_domains.make_empty();
        self.round_evaluated = false;
        self.round_new_rising.make_empty();
        let mut delta_rounds = 0usize;

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
        let mut scheduled_edges = std::mem::take(&mut self.pending_host_edges);
        let mut early_settle_edges = BitSet::with_capacity(num_events);
        // Pending host writes share this round with current-time scheduled
        // edges. Record their net transitions before scheduled values replace
        // them and before clearing the backend's transient trigger hints.
        for (_, id, _) in &self.topo_signals {
            if *id != usize::MAX && self.stable_edge_triggered(executor.backend(), *id) {
                scheduled_edges.insert(*id);
            }
        }
        // Early settling can expose a derived edge that a later source store
        // or awakened process restores. Capture every update we settle early.
        let capture_batch_edges = events_to_process.len() > 1;
        for event in &events_to_process {
            let settle_update = capture_batch_edges
                || !self.waiting.is_empty()
                || executor.scheduled_updates_need_settle();
            let domain_values: Vec<(bool, bool)> = if settle_update {
                self.topo_signals
                    .iter()
                    .map(|(signal, _, _)| sample_event_signal(executor.backend(), *signal))
                    .collect()
            } else {
                Vec::new()
            };
            let before = sample_event_signal(executor.backend(), event.signal);
            executor.backend_mut().set(event.signal, event.next_val);
            let after = sample_event_signal(executor.backend(), event.signal);
            let mut blocking_source = false;
            if let Some(&id) = self.signal_to_id.get(&event.signal)
                && self.edge_triggers(id, before, after)
            {
                scheduled_edges.insert(id);
                blocking_source = self
                    .active_ff_domains
                    .contains(self.event_info[id].canonical_id);
            }
            if settle_update {
                executor.eval_comb()?;
                if executor.finish_requested(current_time) {
                    self.finished = true;
                    return Ok(true);
                }
            }
            for ((signal, id, _), before) in self.topo_signals.iter().zip(domain_values) {
                if *id != usize::MAX
                    && self.edge_triggers(
                        *id,
                        before,
                        sample_event_signal(executor.backend(), *signal),
                    )
                {
                    if capture_batch_edges || blocking_source {
                        // Blocking source FF stores run in the active region
                        // and can remove an edge already sampled by waiters.
                        scheduled_edges.insert(*id);
                    } else {
                        early_settle_edges.insert(*id);
                    }
                }
            }
            self.sample_waiting_events(executor)?;
        }

        // Processes run after this time's scheduled values are applied and
        // see the state settled at the previous time. The event signals they
        // change are edges of this time, like scheduled events.
        executor.backend_mut().clear_triggered_bits();
        let counter_values = self.event_counter_values(executor.backend());
        let mut process_driven = Vec::new();
        let mut process_settled = false;
        if !ready_processes.is_empty() || !self.waiting.is_empty() {
            let before: Vec<(bool, bool)> = self
                .topo_signals
                .iter()
                .map(|(signal, _, _)| sample_event_signal(executor.backend(), *signal))
                .collect();
            let progressed =
                self.run_processes(executor, current_time, ready_processes, &mut delta_rounds)?;
            if self.finished && self.finish_round {
                return Ok(true);
            }
            if !progressed
                && !settle_idle
                && events_to_process.is_empty()
                && scheduled_edges.is_empty()
            {
                return Ok(false);
            }
            // Discover combinationally derived edges before choosing the
            // generated clocks that must share this publication round.
            if progressed {
                executor.eval_comb()?;
                process_settled = true;
                if executor.finish_requested(current_time) {
                    self.finished = true;
                    return Ok(true);
                }
            }
            for ((signal, id, _), before) in self.topo_signals.iter().zip(before) {
                if *id == usize::MAX {
                    continue;
                }
                let value = sample_event_signal(executor.backend(), *signal);
                if value != before {
                    process_driven.push((*id, value));
                }
            }
        }

        if self.finished {
            // Suppress generated edges queued before a ready process finished.
            self.round_new_rising.make_empty();
            generated_events.clear();
        }

        let process_recorded_edges = self.counter_edges(executor.backend(), &counter_values);

        // Ready processes may activate a clock at this timestamp. Join it
        // to existing source events before any coincident FF domain commits.
        // With no other event, retain the later fused-tick path.
        if !self.finished
            && (!scheduled_edges.is_empty()
                || !events_to_process.is_empty()
                || !process_driven.is_empty()
                || !generated_events.is_empty()
                || !process_recorded_edges.is_empty()
                || !executor.backend().get_triggered_bits().is_empty())
        {
            for (id, clock) in &self.tick_clocks {
                if clock.next_edge == current_time
                    && clock.falling_edge != Some(current_time)
                    && self.clock_has_waiters(*id)
                    && sample_event_signal(executor.backend(), clock.signal) != (true, false)
                    && !events_to_process
                        .iter()
                        .any(|event| event.signal == clock.signal && event.next_val == 0)
                    && !generated_events
                        .iter()
                        .any(|event| event.event_ref.id() == *id)
                {
                    current_time
                        .checked_add(clock.period)
                        .ok_or_else(|| clock_overflow(*id))?;
                    generated_events.push(SimEvent {
                        time: current_time,
                        event_ref: clock.event,
                        signal: clock.signal,
                        next_val: 1,
                    });
                    self.round_new_rising.insert(*id);
                }
            }
        }

        // A completed clock wait resumes before the following generated
        // rising edge. Apply that edge after the ready processes, then join
        // it to scheduled triggers so coincident FF domains share one batch.
        for event in &generated_events {
            let id = event.event_ref.id();
            current_time
                .checked_add(self.tick_clocks[&id].period)
                .ok_or_else(|| clock_overflow(id))?;
        }
        for event in &generated_events {
            process_settled = false;
            executor.backend_mut().set(event.signal, event.next_val);
        }
        events_to_process.extend(generated_events);

        let mut triggered_domains = BitSet::with_capacity(num_events);
        let mut discovered_in_this_step = BitSet::with_capacity(num_events);
        // Counter-recorded kernel edges survive later blocking stores that
        // restored the signal before the kernel returned.
        let mut recorded_edges = process_recorded_edges;
        recorded_edges.union_with(&scheduled_edges);
        let mut scheduled_trigger_ids = recorded_edges.clone();
        // An external drive may already have been combinationally evaluated
        // by a testbench read. Compare against our saved baseline even if that
        // evaluation's transient trigger bits have since been cleared.
        let mut track_stable_edges = events_to_process.is_empty();
        let scheduled = events_to_process
            .iter()
            .filter_map(|event| {
                self.signal_to_id
                    .get(&event.signal)
                    .map(|&id| (id, (event.next_val & 1 != 0, false)))
            })
            .chain(process_driven);
        for (id, is_nonzero) in scheduled {
            track_stable_edges = true;
            let was_nonzero = (
                self.last_clock_values.contains(id),
                self.unknown_clock_values.contains(id),
            );
            if self.edge_triggers(id, was_nonzero, is_nonzero) {
                scheduled_trigger_ids.insert(id);
                executor.backend_mut().mark_triggered_bit(id);
            }
        }

        if !process_settled {
            executor.eval_comb()?;
        }
        self.round_evaluated = true;
        if executor.finish_requested(current_time) {
            self.finished = true;
            return Ok(true);
        }
        // Capture every waiter before any continuation can remove a
        // propagated edge produced by the active process writes.
        self.sample_waiting_events(executor)?;
        // Settle process writes before deciding whether they restored an
        // early edge. Persistent edges follow the ordinary cascade after
        // source FF publication; only vanished edges need explicit replay.
        for id in early_settle_edges.iter() {
            if !self.stable_edge_triggered(executor.backend(), id) {
                recorded_edges.insert(id);
                scheduled_trigger_ids.insert(id);
            }
        }
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

        // Share the time-slot budget with active waiter feedback passes.
        let mut comb_already_done = false;
        loop {
            let mut any_new_outer_loop_trigger = false;
            let mut newly_triggered = Vec::new();

            loop {
                count_delta_pass(&mut delta_rounds)?;
                let marked_bits = executor.backend().get_triggered_bits();
                executor.backend_mut().clear_triggered_bits();

                let mut can_use_eval_apply = self.processes.is_empty()
                    && self.process_event_counters.is_empty()
                    && triggered_domains.is_empty()
                    && marked_bits.count() == 1;
                if can_use_eval_apply {
                    let single_id = marked_bits.iter().next().expect("one marked trigger");
                    let info = self.event_info[single_id];
                    can_use_eval_apply =
                        !info.is_cascaded && !self.active_ff_domains.contains(info.canonical_id);
                    if can_use_eval_apply {
                        if let Some(event) = info.eval_ff_event {
                            discovered_in_this_step.insert(single_id);
                            triggered_domains.insert(info.canonical_id);
                            any_new_outer_loop_trigger = true;
                            self.round_triggered_domains.insert(info.canonical_id);
                            executor.stage_external_event(event, current_time)?;
                            executor.eval_apply_ff_at(event)?;
                            self.record_domain_publication(executor.backend(), info.canonical_id);
                            executor.fire_external_event(event, current_time)?;
                            executor.eval_comb()?;
                            if executor.finish_requested(current_time) {
                                self.finished = true;
                                return Ok(true);
                            }
                            if track_stable_edges {
                                self.replace_triggers_with_stable_edges(executor.backend_mut());
                            }
                            comb_already_done = true;
                            break;
                        }
                    }
                }

                let mut ff_body_edges = BitSet::with_capacity(num_events);
                for id in marked_bits.iter() {
                    let recorded_edge = recorded_edges.contains(id);
                    recorded_edges.remove(id);
                    if discovered_in_this_step.contains(id) {
                        continue;
                    }
                    let info = self.event_info[id];
                    if triggered_domains.contains(info.canonical_id) {
                        continue;
                    }
                    // Native trigger bits can be coarse hints raised by a
                    // working-region write before its commit. A derived clock
                    // must have a stable edge or a counter-recorded transition.
                    if info.is_cascaded
                        && !recorded_edge
                        && !self.stable_edge_triggered(executor.backend(), id)
                    {
                        continue;
                    }
                    discovered_in_this_step.insert(id);
                    triggered_domains.insert(info.canonical_id);
                    newly_triggered.push(info.canonical_id);

                    self.round_triggered_domains.insert(info.canonical_id);
                    let before = self.event_counter_values(executor.backend());
                    if let Some(event) = info.eval_only_event {
                        executor.stage_external_event(
                            info.eval_ff_event.unwrap_or(event),
                            current_time,
                        )?;
                        executor.eval_only_ff_at(event)?;
                    } else if let Some(event) = info.eval_ff_event {
                        executor.stage_external_event(event, current_time)?;
                        executor.eval_apply_ff_at(event)?;
                        self.record_domain_publication(executor.backend(), info.canonical_id);
                    } else {
                        unreachable!(
                            "FF trigger discovered without a corresponding execution unit"
                        );
                    }
                    if executor.finish_requested(current_time) {
                        self.finished = true;
                        return Ok(true);
                    }
                    ff_body_edges.union_with(&self.counter_edges(executor.backend(), &before));
                }

                recorded_edges.union_with(&ff_body_edges);

                // Working-region writes become visible only when this NBA
                // batch is published; discover their cascade edges then.
                executor.backend_mut().clear_triggered_bits();
                for id in ff_body_edges.iter() {
                    executor.backend_mut().mark_triggered_bit(id);
                }
                // Inactive-region (#0) continuations run after the active
                // FF evaluations and before the pending NBA publications.
                // Drain active evaluations before promoting another inactive
                // batch, including evaluations a combinational settle woke.
                let inactive_counter_values = self.event_counter_values(executor.backend());
                let mut inactive_samples = None;
                let mut active_ran =
                    self.run_processes(executor, current_time, Vec::new(), &mut delta_rounds)?;
                if !active_ran {
                    // Drain active combinational effects of blocking FF stores
                    // before either inactive continuations or NBA publication.
                    if !self.inactive.is_empty()
                        || !triggered_domains.is_disjoint(&self.active_ff_domains)
                    {
                        executor.eval_comb()?;
                        if executor.finish_requested(current_time) {
                            self.finished = true;
                            return Ok(true);
                        }
                        self.replace_triggers_with_stable_edges(executor.backend_mut());
                        for id in ff_body_edges.iter() {
                            executor.backend_mut().mark_triggered_bit(id);
                        }
                        if executor
                            .backend()
                            .get_triggered_bits()
                            .iter()
                            .any(|id| !discovered_in_this_step.contains(id))
                        {
                            continue;
                        }
                        executor.backend_mut().clear_triggered_bits();
                    }
                    active_ran =
                        self.run_processes(executor, current_time, Vec::new(), &mut delta_rounds)?;
                }
                if self.finished && self.finish_round {
                    return Ok(true);
                }
                let progressed = if active_ran {
                    true
                } else {
                    let ready = std::mem::take(&mut self.inactive)
                        .into_iter()
                        .collect::<Vec<_>>();
                    if self.finished {
                        break;
                    }
                    if ready.is_empty() {
                        if !ff_body_edges.is_empty() {
                            continue;
                        }
                        break;
                    }
                    // A new inactive batch may create a fresh occurrence of
                    // a domain already evaluated in this time slot.
                    let processed = discovered_in_this_step.clone();
                    discovered_in_this_step.make_empty();
                    triggered_domains.make_empty();
                    let before = self
                        .topo_signals
                        .iter()
                        .map(|(signal, _, _)| sample_event_signal(executor.backend(), *signal))
                        .collect::<Vec<_>>();
                    for ((_, id, _), value) in self.topo_signals.iter().zip(&before) {
                        if *id != usize::MAX && processed.contains(*id) {
                            if value.1 {
                                self.unknown_clock_values.insert(*id);
                            } else {
                                self.unknown_clock_values.remove(*id);
                            }
                            if value.0 {
                                self.last_clock_values.insert(*id);
                            } else {
                                self.last_clock_values.remove(*id);
                            }
                        }
                    }
                    inactive_samples = Some(before);
                    self.run_processes(executor, current_time, ready, &mut delta_rounds)?
                };
                if progressed {
                    let mut driven =
                        self.counter_edges(executor.backend(), &inactive_counter_values);
                    recorded_edges.union_with(&driven);
                    driven.union_with(&ff_body_edges);
                    executor.eval_comb()?;
                    if executor.finish_requested(current_time) {
                        self.finished = true;
                        return Ok(true);
                    }
                    if let Some(before) = inactive_samples {
                        executor.backend_mut().clear_triggered_bits();
                        for ((signal, id, _), before) in self.topo_signals.iter().zip(before) {
                            if *id != usize::MAX
                                && self.edge_triggers(
                                    *id,
                                    before,
                                    sample_event_signal(executor.backend(), *signal),
                                )
                            {
                                executor.backend_mut().mark_triggered_bit(*id);
                            }
                        }
                    } else if track_stable_edges {
                        self.replace_triggers_with_stable_edges(executor.backend_mut());
                    }
                    for id in driven.iter() {
                        executor.backend_mut().mark_triggered_bit(id);
                    }
                } else {
                    break;
                }
            }

            if newly_triggered.is_empty() && !any_new_outer_loop_trigger {
                break;
            }

            // Publish all simultaneous NBA updates before polling waiters.
            // Poll between outer cascade stages (IEEE 1800-2023 4.5), never
            // between the domains evaluated in the same active region.
            let polled = !self.waiting.is_empty() && !self.finished;
            let before: Vec<(bool, bool)> = if polled {
                self.topo_signals
                    .iter()
                    .map(|(signal, _, _)| sample_event_signal(executor.backend(), *signal))
                    .collect()
            } else {
                Vec::new()
            };
            let mut any_process_ran = false;
            for id in &newly_triggered {
                if let Some(event) = self.event_info[*id].apply_event {
                    executor.apply_ff_at(event)?;
                    self.record_domain_publication(executor.backend(), *id);
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
                if executor.finish_requested(current_time) {
                    self.finished = true;
                    return Ok(true);
                }
                if track_stable_edges {
                    self.replace_triggers_with_stable_edges(executor.backend_mut());
                }
            }

            if polled {
                self.sample_waiting_events(executor)?;
                let counter_values = self.event_counter_values(executor.backend());
                any_process_ran |=
                    self.run_processes(executor, current_time, Vec::new(), &mut delta_rounds)?;
                if self.finished && self.finish_round {
                    return Ok(true);
                }
                if any_process_ran {
                    let mut driven = self.counter_edges(executor.backend(), &counter_values);
                    recorded_edges.union_with(&driven);
                    for ((signal, id, _), before) in self.topo_signals.iter().zip(before) {
                        if *id == usize::MAX {
                            continue;
                        }
                        let value = sample_event_signal(executor.backend(), *signal);
                        if value != before && self.edge_triggers(*id, before, value) {
                            driven.insert(*id);
                            executor.backend_mut().mark_triggered_bit(*id);
                        }
                    }
                    executor.eval_comb()?;
                    if executor.finish_requested(current_time) {
                        self.finished = true;
                        return Ok(true);
                    }
                    // An NBA-woken process can create a new occurrence of an
                    // already evaluated domain in the same time slot.
                    recorded_edges.union_with(&driven);
                    for id in driven.iter() {
                        discovered_in_this_step.remove(id);
                        triggered_domains.remove(self.event_info[id].canonical_id);
                    }
                    if track_stable_edges {
                        if driven.is_empty() {
                            self.replace_triggers_with_stable_edges(executor.backend_mut());
                        } else {
                            executor.backend_mut().clear_triggered_bits();
                            for id in driven.iter() {
                                executor.backend_mut().mark_triggered_bit(id);
                            }
                        }
                    }
                }
            }
        }

        for (signal, id, _) in &self.topo_signals {
            if *id == usize::MAX {
                continue;
            }
            let (value, unknown) = sample_event_signal(executor.backend(), *signal);
            if unknown {
                self.unknown_clock_values.insert(*id);
            } else {
                self.unknown_clock_values.remove(*id);
            }
            if value {
                self.last_clock_values.insert(*id);
            } else {
                self.last_clock_values.remove(*id);
            }
        }

        for event in periodic_events_to_process {
            let event_id = event.event_ref.id();
            if let Some(Some(clock)) = self.scheduler.clocks.get(event_id) {
                // Keep the extra odd-period tick in the high phase, matching
                // the process/testbench clock model.
                // A clock needs at least one time unit in each phase.
                let period = clock.period.max(2);
                let half = (period / 2) + ((period & 1) & u64::from(event.next_val != 0));
                let time =
                    current_time
                        .checked_add(half)
                        .ok_or_else(|| SimulatorErrorCode::Runtime {
                            message: format!("event {event_id} overflows simulation time"),
                            signals: Vec::new(),
                        })?;
                self.push_periodic_event(SimEvent {
                    time,
                    event_ref: event.event_ref,
                    signal: event.signal,
                    next_val: 1 - event.next_val,
                });
            }
        }

        Ok(true)
    }

    fn record_domain_publication(&mut self, backend: &B, canonical: usize) {
        self.round_published_domains.insert(canonical);
        for (signal, id, _) in &self.topo_signals {
            if *id != usize::MAX && self.event_info[*id].canonical_id == canonical {
                let (value, unknown) = sample_event_signal(backend, *signal);
                if unknown {
                    self.unknown_clock_values.insert(*id);
                } else {
                    self.unknown_clock_values.remove(*id);
                }
                if value {
                    self.last_clock_values.insert(*id);
                } else {
                    self.last_clock_values.remove(*id);
                }
            }
        }
    }

    fn event_counter_values(&self, backend: &B) -> Vec<(u32, u32)> {
        self.process_event_counters
            .iter()
            .map(|(_, rises, falls)| (backend.get_as(*rises), backend.get_as(*falls)))
            .collect()
    }

    fn counter_edges(&self, backend: &B, before: &[(u32, u32)]) -> BitSet {
        let mut edges = BitSet::with_capacity(backend.num_events());
        for ((id, rises, falls), (old_rises, old_falls)) in
            self.process_event_counters.iter().zip(before)
        {
            let changed = match self.domain_kinds[*id] {
                Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => {
                    backend.get_as::<u32>(*falls) != *old_falls
                }
                _ => backend.get_as::<u32>(*rises) != *old_rises,
            };
            if changed {
                edges.insert(*id);
            }
        }
        edges
    }

    /// Whether a change of event signal `id` from `was_nonzero` to
    /// `is_nonzero` is the edge its domain triggers on.
    fn edge_triggers(&self, id: usize, was: (bool, bool), is: (bool, bool)) -> bool {
        let (posedge, negedge) = transition_edges(was, is);
        match self.domain_kinds[id] {
            Some(DomainKind::ClockPosedge | DomainKind::ResetAsyncHigh) => posedge,
            Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => negedge,
            _ => posedge,
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
        if !self.pending_host_edges.is_empty() {
            return Some(self.scheduler.time);
        }
        let process = if self.inactive.is_empty() {
            self.process_wakeups.peek().map(|&Reverse((time, _))| time)
        } else {
            Some(self.scheduler.time)
        };
        let clocks = self
            .tick_clocks
            .iter()
            .flat_map(|(id, clock)| {
                [
                    self.clock_has_waiters(*id).then_some(clock.next_edge),
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
            .filter_map(|(id, clock)| Some((id, clock.as_ref()?.period.max(2))))
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
            pending_host_edges: self.pending_host_edges.clone(),
            unknown_clock_values: self.unknown_clock_values.clone(),
            process_wakeups: self.process_wakeups.clone(),
            waiting: self.waiting.clone(),
            tick_clocks: self.tick_clocks.clone(),
            clock_waits: self.clock_waits.clone(),
            done: self.done.clone(),
            ticks: self.ticks,
            inactive: self.inactive.clone(),
            scheduled_clock_drives: self.scheduled_clock_drives.clone(),
            scheduled_clock_time: self.scheduled_clock_time,
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
        // Every translation happens before anything changes.
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
        self.scheduler.time = snapshot.time;
        self.scheduler.clocks.clone_from(&snapshot.clocks);
        self.scheduler.event_queue = event_queue;
        self.periodic_events.clone_from(&snapshot.periodic_events);
        self.last_clock_values
            .clone_from(&snapshot.last_clock_values);
        self.pending_host_edges
            .clone_from(&snapshot.pending_host_edges);
        self.unknown_clock_values
            .clone_from(&snapshot.unknown_clock_values);
        self.process_wakeups.clone_from(&snapshot.process_wakeups);
        self.waiting.clone_from(&snapshot.waiting);
        self.tick_clocks = tick_clocks;
        self.clock_waits.clone_from(&snapshot.clock_waits);
        self.done.clone_from(&snapshot.done);
        self.ticks = snapshot.ticks;
        self.inactive.clone_from(&snapshot.inactive);
        self.scheduled_clock_drives
            .clone_from(&snapshot.scheduled_clock_drives);
        self.scheduled_clock_time = snapshot.scheduled_clock_time;
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
    pub captured_edges: Vec<B::Event>,
    pub unknown_events: Vec<B::Event>,
    /// Suspended processes and the time each resumes at.
    pub process_wakeups: Vec<(usize, u64)>,
    /// Processes waiting for an event or a condition, in ascending order.
    pub waiting_processes: Vec<usize>,
    /// Process clocks that have been waited on.
    pub process_clocks: Vec<ProcessClockState<B>>,
    /// Processes waiting for edges of a process clock: the process, the
    /// clock's event, and the edges still to wait for.
    pub clock_waits: Vec<(usize, B::Event, u64)>,
    /// Processes that ran to their end, in ascending order.
    pub done_processes: Vec<usize>,
    /// Rising edges of process clocks so far.
    pub ticks: u64,
    /// Timestamp and one-shot clock drives already consumed at that time.
    pub scheduled_clock_time: u64,
    pub scheduled_clock_drives: Vec<B::Event>,
    pub inactive_processes: Vec<usize>,
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
            captured_edges: self
                .pending_host_edges
                .iter()
                .map(|id| events[id])
                .collect(),
            unknown_events: self
                .unknown_clock_values
                .iter()
                .map(|id| events[id])
                .collect(),
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
                .map(|(process, wait)| (*process, events[wait.clock], wait.remaining))
                .collect(),
            done_processes: self.done.iter().copied().collect(),
            ticks: self.ticks,
            scheduled_clock_time: self.scheduled_clock_time,
            scheduled_clock_drives: self
                .scheduled_clock_drives
                .iter()
                .map(|id| events[id])
                .collect(),
            inactive_processes: self.inactive.iter().copied().collect(),
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
        for event in parts.high_events {
            self.last_clock_values.insert(event.id());
        }
        self.unknown_clock_values.make_empty();
        for event in parts.unknown_events {
            self.unknown_clock_values.insert(event.id());
        }
        self.pending_host_edges = parts
            .captured_edges
            .into_iter()
            .map(|event| event.id())
            .collect();
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
            .map(|(process, event, remaining)| {
                (
                    process,
                    ClockWait {
                        clock: event.id(),
                        remaining,
                    },
                )
            })
            .collect();
        self.done = parts.done_processes.into_iter().collect();
        self.ticks = parts.ticks;
        self.scheduled_clock_time = parts.scheduled_clock_time;
        self.scheduled_clock_drives = parts
            .scheduled_clock_drives
            .into_iter()
            .map(|event| event.id())
            .collect();
        self.inactive = parts.inactive_processes.into_iter().collect();
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
    pending_host_edges: BitSet,
    unknown_clock_values: BitSet,
    process_wakeups: BinaryHeap<ProcessWakeup>,
    waiting: BTreeSet<usize>,
    tick_clocks: BTreeMap<usize, TickClock<B>>,
    clock_waits: BTreeMap<usize, ClockWait>,
    done: BTreeSet<usize>,
    ticks: u64,
    inactive: BTreeSet<usize>,
    scheduled_clock_drives: BitSet,
    scheduled_clock_time: u64,
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
            pending_host_edges: self.pending_host_edges.clone(),
            unknown_clock_values: self.unknown_clock_values.clone(),
            process_wakeups: self.process_wakeups.clone(),
            waiting: self.waiting.clone(),
            tick_clocks: self.tick_clocks.clone(),
            clock_waits: self.clock_waits.clone(),
            done: self.done.clone(),
            ticks: self.ticks,
            inactive: self.inactive.clone(),
            scheduled_clock_drives: self.scheduled_clock_drives.clone(),
            scheduled_clock_time: self.scheduled_clock_time,
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
