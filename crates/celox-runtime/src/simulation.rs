use std::{cmp::Reverse, collections::BinaryHeap};

use bit_set::BitSet;
use celox_design::{CounterKind, DomainKind, ProcessStatus};
use fxhash::FxHashMap;

use crate::{
    AbsoluteAddr, SignalRef, SimulatorErrorCode,
    backend::{EventHandle, SimBackend},
    scheduler::{ClockDef, Scheduler, SimEvent},
    timing::{
        Baseline, CountedDomainRefs, Halt, NbaUpdate, ProcessRefs, ProcessState, StateView, Value,
        copy_planes, encode_planes, kind_index, occurs, planes, planes_equal, planes_occur, read,
    },
};

/// Kernel returns a round may spend on `Yield` and nonblocking staging
/// before the runtime reports a process that never suspends.
const SLOT_RETURN_BUDGET: u64 = 1 << 24;

/// Rounds one time slot may run before the runtime reports processes that
/// keep resuming at that time, such as `forever #0`.
const SLOT_ROUND_BUDGET: u64 = 1 << 20;

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

/// The level of an event signal: its value bits, without the unused bits
/// above its width that share its storage byte. Backends need not keep those
/// bits clear (a four-state X initialization sets them), so a one-bit clock
/// that reads as a whole byte would stay "high" after it falls.
pub(crate) fn event_level<B: SimBackend>(backend: &B, signal: SignalRef) -> u8 {
    let value: u8 = backend.get_as(signal);
    match signal.width {
        width @ 1..8 => value & ((1u8 << width) - 1),
        _ => value,
    }
}

/// Whether a settled level change from `was` to `is` is an active edge of a
/// domain of `kind`. A domain of unknown kind reacts to any change.
pub(crate) fn stable_edge(kind: Option<DomainKind>, was: bool, is: bool) -> bool {
    match kind {
        Some(DomainKind::ClockPosedge | DomainKind::ResetAsyncHigh) => !was && is,
        Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => was && !is,
        _ => was != is,
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

/// Bookkeeping of one round of a time slot.
#[derive(Default)]
struct Round {
    /// Event ids whose trigger survives the settling filter.
    scheduled_trigger_ids: BitSet,
    track_stable_edges: bool,
    /// Event ids a counter triggered; they bypass the stable-edge test of
    /// cascaded domains, since a counted pulse may end at its old value.
    counted_triggered: BitSet,
    /// Domains evaluated before an inactive region, in evaluation order,
    /// with the working image of an evaluation that a later one of the
    /// same domain superseded (IEEE 1800-2023 4.5: their nonblocking
    /// assignments wait for the NBA region).
    deferred: Vec<(usize, Option<Vec<u8>>)>,
    /// Kernels ran in this round, so pending nonblocking flags may be set.
    kernels_ran: bool,
    nba_published: bool,
    /// Event-signal values scheduled or written by processes, not yet
    /// marked.
    transitions: Vec<(usize, bool)>,
    /// Event-signal values before and after a process run.
    before: Vec<u8>,
    after: Vec<u8>,
    /// Event ids a pulse of several publications triggered; like counted
    /// triggers they survive settling and bypass the stable-edge test.
    occurrences: BitSet,
    /// A domain published several evaluations and `transitions` records
    /// the event signals they changed.
    publishing: bool,
    /// Event ids whose level change gates combinationally derived clocks,
    /// not yet judged.
    gating_sources: Vec<usize>,
    /// Derived clocks whose edge was judged at their source's change; they
    /// bypass the stable-edge test of cascaded domains.
    edge_approved: BitSet,
    /// The latest value of each event signal among the transitions marked
    /// since the baseline (`last_clock_values`) was last taken, valid where
    /// the stamp equals `generation`: a value written in one wave of the
    /// active region is the previous value of the next wave's write.
    latest: Vec<(u32, bool)>,
    generation: u32,
}

impl Round {
    /// Start a scan of transitions over `events` event ids against a new
    /// baseline.
    fn begin_scan(&mut self, events: usize) {
        if self.latest.len() < events {
            self.latest.resize(events, (0, false));
        }
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            // Stamps of an earlier wrap must not match.
            self.latest.fill((0, false));
            self.generation = 1;
        }
    }

    /// The value event `id` had before this transition: its latest value in
    /// the scan, or `baseline`; records `value` as its latest.
    fn previous(&mut self, id: usize, value: bool, baseline: impl FnOnce() -> bool) -> bool {
        let (stamp, latest) = std::mem::replace(&mut self.latest[id], (self.generation, value));
        if stamp == self.generation {
            latest
        } else {
            baseline()
        }
    }
}

impl Round {
    /// Start a round, keeping the buffers of earlier ones.
    fn reset(&mut self, track_stable_edges: bool) {
        self.scheduled_trigger_ids.make_empty();
        self.track_stable_edges = track_stable_edges;
        self.counted_triggered.make_empty();
        self.occurrences.make_empty();
        self.publishing = false;
        self.gating_sources.clear();
        self.edge_approved.make_empty();
        self.deferred.clear();
        self.kernels_ran = false;
        self.nba_published = false;
        self.transitions.clear();
    }
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

    /// Rebase edge detection after state was advanced outside this scheduler.
    pub fn synchronize_event_values(&mut self, backend: &B) {
        self.last_clock_values.make_empty();
        for (signal, id, _) in &self.topo_signals {
            if *id == usize::MAX {
                continue;
            }
            if event_level(backend, *signal) != 0 {
                self.last_clock_values.insert(*id);
            }
        }
    }

    fn stable_edge_triggered(&self, backend: &B, id: usize) -> bool {
        let Some((signal, _, _)) = self.topo_signals.get(self.signal_index_by_id[id]) else {
            return true;
        };
        let was_nonzero = self.last_clock_values.contains(id);
        let is_nonzero = event_level(backend, *signal) != 0;
        stable_edge(self.domain_kinds[id], was_nonzero, is_nonzero)
    }

    fn replace_triggers_with_stable_edges(&self, backend: &mut B) {
        backend.clear_triggered_bits();
        for (signal, id, _) in &self.topo_signals {
            if *id == usize::MAX {
                continue;
            }
            let was_nonzero = self.last_clock_values.contains(*id);
            let is_nonzero = event_level(backend, *signal) != 0;
            if stable_edge(self.domain_kinds[*id], was_nonzero, is_nonzero) {
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

    /// Like [`Self::step`], but after settling host writes at the current
    /// time, advance only to a time up to `limit`.
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

    /// Settle host writes at the current time, then run the next time with
    /// work, if it is not after `limit`. Host writes do not hold a step at
    /// the current time: the step goes on to the next event.
    fn step_time<E>(&mut self, executor: &mut E, limit: u64) -> Result<Option<u64>, Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        let mut settled = None;
        loop {
            let Some(current_time) = self.next_event_time() else {
                return Ok(settled);
            };
            if settled.is_some() && current_time > limit {
                return Ok(settled);
            }
            // Host writes alone: settle them, then go on to the next time.
            let host_only = self.host_pending && self.next_work_time() != Some(current_time);
            self.run_time(executor, current_time)?;
            if !host_only {
                return Ok(Some(current_time));
            }
            settled = Some(current_time);
        }
    }

    /// Run the time slot `current_time`: its scheduled events, the
    /// processes due then, and the rounds they cause.
    fn run_time<E>(&mut self, executor: &mut E, current_time: u64) -> Result<(), Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        self.host_pending = false;
        self.slot_passes = 0;
        let events_to_process = if self.scheduler.next_event_time() == Some(current_time) {
            self.scheduler
                .pop_all_at_next_time()
                .map(|(_, events)| events)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let ready = self.take_ready_processes(current_time);
        self.step_round(executor, current_time, events_to_process, ready, true)?;
        // A process that waited for zero time, or that the round woke,
        // resumes in a later round of this time, after the previous round's
        // edges have been handled.
        loop {
            let ready = self.take_ready_processes(current_time);
            if ready.is_empty() && self.process_state.ready.is_empty() {
                break;
            }
            self.charge_pass()?;
            self.step_round(executor, current_time, Vec::new(), ready, false)?;
        }
        executor.finish_timed_step(current_time);
        Ok(())
    }

    /// Count a round or a pass of the process phase against the slot's
    /// budget, so that processes resuming at one time forever end the step
    /// with an error instead of hanging it.
    fn charge_pass(&mut self) -> Result<(), Halt> {
        self.slot_passes += 1;
        self.charge(self.slot_passes, None)
    }

    fn charge(&self, returns: u64, process: Option<usize>) -> Result<(), Halt> {
        let budget = if process.is_some() {
            self.return_budget
        } else {
            self.round_budget
        };
        if returns <= budget {
            return Ok(());
        }
        Err(Halt::Error(SimulatorErrorCode::Runtime {
            message: match process {
                Some(process) => {
                    format!("process {process} does not suspend within one time slot")
                }
                None => format!(
                    "time {} does not settle: processes keep resuming",
                    self.scheduler.time
                ),
            },
            signals: Vec::new(),
        }))
    }

    /// Remove the processes waiting for `time`, in declaration order.
    fn take_ready_processes(&mut self, time: u64) -> Vec<usize> {
        let mut ready = Vec::new();
        while let Some(&Reverse((wakeup, process))) = self.process_wakeups.peek() {
            if wakeup != time {
                break;
            }
            self.process_wakeups.pop();
            ready.push(process);
        }
        ready
    }

    /// Run process `process` once and file it where it resumes. Returns
    /// whether it resumes at once (a queued nonblocking assignment or a
    /// yield), in which case the caller checks the state before running it
    /// again: the return separates the writes before and after it.
    fn run_process<E>(
        &mut self,
        executor: &mut E,
        current_time: u64,
        process: usize,
        returns: &mut u64,
    ) -> Result<bool, Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        {
            executor.run_process(process)?;
            let status: u8 = executor.backend().get_as(self.processes[process].status);
            let delay = || -> u64 { executor.backend().get_as(self.processes[process].delay) };
            match ProcessStatus::from_code(status) {
                Some(ProcessStatus::Delay) => {
                    let delay = delay();
                    self.wake_after(current_time, delay, process)?;
                }
                Some(ProcessStatus::InactiveDelay) => {
                    let delay = delay();
                    if delay == 0 {
                        self.process_state.inactive.insert(process);
                    } else {
                        self.wake_after(current_time, delay, process)?;
                    }
                }
                Some(ProcessStatus::Done) => {}
                Some(ProcessStatus::Finish) => return Err(Halt::Finish),
                Some(ProcessStatus::Wait | ProcessStatus::Pending) => {
                    let site = delay() as usize;
                    self.register_waiter(executor.backend(), process, site)?;
                }
                Some(ProcessStatus::Nba) => {
                    let site = delay() as usize;
                    self.queue_nba(executor.backend_mut(), process, site)?;
                    *returns += 1;
                    self.charge(*returns, Some(process))?;
                    return Ok(true);
                }
                Some(ProcessStatus::Yield) => {
                    *returns += 1;
                    self.charge(*returns, Some(process))?;
                    return Ok(true);
                }
                None => return Err(SimulatorErrorCode::InternalError.into()),
            }
            Ok(false)
        }
    }

    fn wake_after(&mut self, current_time: u64, delay: u64, process: usize) -> Result<(), Halt> {
        let time = current_time.checked_add(delay).ok_or_else(|| {
            Halt::Error(SimulatorErrorCode::Runtime {
                message: format!("process {process} delay overflows simulation time"),
                signals: Vec::new(),
            })
        })?;
        self.process_wakeups.push(Reverse((time, process)));
        Ok(())
    }

    fn site_error(process: usize, site: usize) -> Halt {
        Halt::Error(SimulatorErrorCode::Runtime {
            message: format!("process {process} reported unknown site {site}"),
            signals: Vec::new(),
        })
    }

    /// Process `process` waits at its wait site `site`: remember what each
    /// term compares against from now on.
    fn register_waiter(&mut self, backend: &B, process: usize, site: usize) -> Result<(), Halt> {
        let terms = self.processes[process]
            .wait_sites
            .get(site)
            .ok_or_else(|| Self::site_error(process, site))?;
        let driven = terms.iter().any(|term| term.driven);
        self.process_state
            .waiters
            .register(process, driven, |waiter| {
                // Re-arming at the same site reuses the baselines' storage.
                let reuse = waiter.site == site && waiter.baselines.len() == terms.len();
                waiter.site = site;
                if !reuse {
                    waiter.baselines.clear();
                    waiter
                        .baselines
                        .extend(terms.iter().map(|_| Baseline::Counter(0)));
                }
                for (term, baseline) in terms.iter().zip(&mut waiter.baselines) {
                    match term.counter {
                        Some(counter) => *baseline = Baseline::Counter(backend.get_as(counter)),
                        None => match planes(backend, term.signal) {
                            Some(image) => {
                                if let Baseline::Raw(raw) = baseline {
                                    copy_planes(raw, image, term.signal.width);
                                } else {
                                    let mut raw = Vec::with_capacity(image.len());
                                    copy_planes(&mut raw, image, term.signal.width);
                                    *baseline = Baseline::Raw(raw);
                                }
                            }
                            None => *baseline = Baseline::Value(read(backend, term.signal)),
                        },
                    }
                }
            });
        Ok(())
    }

    /// Move the value staged at nonblocking site `site` of `process` into
    /// the ordered queue.
    fn queue_nba(&mut self, backend: &mut B, process: usize, site: usize) -> Result<(), Halt> {
        let refs = *self.processes[process]
            .nba_sites
            .get(site)
            .ok_or_else(|| Self::site_error(process, site))?;
        if let Some(pending) = refs.pending {
            // The flag is set: the kernel reached the site again before the
            // NBA region and returned before staging over the earlier value.
            backend.set(pending, 0u8);
        }
        let offset = refs
            .offset
            .map_or(0, |offset| backend.get_as::<u64>(offset) as usize);
        self.process_state.nba_queue.push(NbaUpdate {
            target: refs.target,
            lsb: refs.lsb + offset,
            width: refs.width,
            value: read(backend, refs.value),
        });
        Ok(())
    }

    /// Write `signal` on behalf of the runtime, counting a counted signal's
    /// occurrences and showing the value to the waits on it.
    #[inline]
    fn write_signal(&mut self, backend: &mut B, signal: SignalRef, write: impl FnOnce(&mut B)) {
        if self.counted.is_empty() && self.process_state.waiters.is_empty() {
            write(backend);
            return;
        }
        match self.counted_by_offset.get(&signal.offset).copied() {
            Some(counted) => {
                let old = read(backend, signal);
                write(backend);
                let new = read(backend, signal);
                self.counted[counted].refs.bump(backend, &old, &new);
            }
            None => write(backend),
        }
        if !self.process_state.waiters.is_empty() {
            self.observe(backend, Some(signal));
        }
    }

    /// Test the waits against the current state, or only their terms on
    /// signal `only` after a write of it. Woken processes become ready.
    #[inline]
    fn observe(&mut self, backend: &B, only: Option<SignalRef>) {
        self.check_waits(backend, only, false);
    }

    /// Like [`Self::observe`], at a point where only design code wrote
    /// state: terms on signals that only the host and the scheduler write
    /// cannot have changed.
    #[inline]
    fn observe_driven(&mut self, backend: &B) {
        if !self.process_state.waiters.none_driven() {
            self.check_waits(backend, None, true);
        }
    }

    fn check_waits(&mut self, backend: &B, only: Option<SignalRef>, driven_only: bool) {
        let processes = &self.processes;
        let state = StateView::new(backend);
        let ProcessState { waiters, ready, .. } = &mut self.process_state;
        waiters.retain(driven_only, |process, waiter| {
            let terms = &processes[process].wait_sites[waiter.site];
            let mut wake = false;
            for (index, term) in terms.iter().enumerate() {
                if only.is_some_and(|signal| signal != term.signal) || (driven_only && !term.driven)
                {
                    continue;
                }
                let baseline = &mut waiter.baselines[index];
                wake |= match baseline {
                    Baseline::Counter(seen) => term
                        .counter
                        .is_some_and(|counter| backend.get_as::<u32>(counter) != *seen),
                    Baseline::Raw(last) => {
                        let now = state.planes(term.signal).expect("a plain signal");
                        Self::advance(term.kind, last, now, term.signal.width)
                    }
                    Baseline::Value(last) => match state.planes(term.signal) {
                        // A baseline loaded from a state file.
                        Some(now) => {
                            let mut raw = encode_planes(last, term.signal.width, now.len());
                            let hit = Self::advance(term.kind, &mut raw, now, term.signal.width);
                            *baseline = Baseline::Raw(raw);
                            hit
                        }
                        None => {
                            let now = read(backend, term.signal);
                            let hit = occurs(term.kind, last, &now);
                            *last = now;
                            hit
                        }
                    },
                };
            }
            if wake {
                ready.insert(process);
            }
            !wake
        });
    }

    /// Whether the change from the image `last` to `now` is an occurrence of
    /// `kind`; `last` becomes `now`.
    #[inline]
    fn advance(kind: CounterKind, last: &mut Vec<u8>, now: &[u8], width: usize) -> bool {
        if planes_equal(last, now, width) {
            return false;
        }
        let hit = planes_occur(kind, last, now, width);
        copy_planes(last, now, width);
        hit
    }

    /// Show the waits one value the host wrote to `signal`.
    fn observe_host_write(&mut self, signal: SignalRef, value: &Value) {
        let processes = &self.processes;
        let ProcessState { waiters, ready, .. } = &mut self.process_state;
        waiters.retain(false, |process, waiter| {
            let terms = &processes[process].wait_sites[waiter.site];
            let mut wake = false;
            for (term, baseline) in terms.iter().zip(&mut waiter.baselines) {
                if term.signal != signal {
                    continue;
                }
                match baseline {
                    // Counters are bumped by the write itself.
                    Baseline::Counter(_) => {}
                    Baseline::Raw(last) => {
                        let now = encode_planes(value, signal.width, last.len());
                        wake |= Self::advance(term.kind, last, &now, signal.width);
                    }
                    Baseline::Value(last) => {
                        wake |= occurs(term.kind, last, value);
                        *last = value.clone();
                    }
                }
            }
            if wake {
                ready.insert(process);
            }
            !wake
        });
    }

    /// Mark the domains whose counters moved since they last triggered.
    fn mark_counted_domains(&mut self, backend: &mut B, round: &mut Round) {
        for (index, counted) in self.counted.iter().enumerate() {
            for &(id, kind) in &counted.domains {
                let value: u32 = backend.get_as(counted.refs.counter(kind));
                let seen = &mut self.process_state.domain_counters[index][kind_index(kind)];
                if value != *seen {
                    *seen = value;
                    backend.mark_triggered_bit(id);
                    round.scheduled_trigger_ids.insert(id);
                    round.counted_triggered.insert(id);
                    round.track_stable_edges = true;
                }
            }
        }
    }

    /// Mark the domains a scheduled or process-driven value of an event
    /// signal triggers, against the values of the last settled round.
    #[inline]
    fn mark_scheduled_edges(&self, backend: &mut B, round: &mut Round, values: &[(usize, bool)]) {
        for &(id, is_nonzero) in values {
            round.track_stable_edges = true;
            // A signal written again compares with its preceding value.
            let was_nonzero =
                round.previous(id, is_nonzero, || self.last_clock_values.contains(id));
            if was_nonzero != is_nonzero
                && self
                    .combinational_derived
                    .get(id)
                    .is_some_and(|derived| !derived.is_empty())
            {
                round.gating_sources.push(id);
            }
            let triggered = match self.domain_kinds[id] {
                Some(DomainKind::ClockPosedge | DomainKind::ResetAsyncHigh) => {
                    !was_nonzero && is_nonzero
                }
                Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => {
                    was_nonzero && !is_nonzero
                }
                _ => !was_nonzero && is_nonzero,
            };
            if triggered {
                round.scheduled_trigger_ids.insert(id);
                backend.mark_triggered_bit(id);
            }
        }
    }

    /// Settle combinational logic after the round's sources changed, and
    /// keep the triggers the round considers stable.
    #[inline]
    fn settle_marked<E>(&mut self, executor: &mut E, round: &mut Round) -> Result<(), Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        executor.eval_comb()?;
        if round.track_stable_edges {
            // Combinational settling before an active scheduled source domain
            // commits may expose transient derived-clock edges. In that case,
            // keep only the source event and rediscover stable edges after the
            // commit. If the source edge is inactive, preserve derived edges
            // while filtering out the scheduled signal's own transition.
            if round.scheduled_trigger_ids.is_empty() {
                self.replace_triggers_with_stable_edges(executor.backend_mut());
            } else {
                executor.backend_mut().clear_triggered_bits();
                for id in round.scheduled_trigger_ids.iter() {
                    executor.backend_mut().mark_triggered_bit(id);
                }
            }
            // Derived clocks judged at an earlier settle of the round keep
            // their edge until their domain evaluates; their baseline
            // already moved.
            for id in round.edge_approved.iter() {
                executor.backend_mut().mark_triggered_bit(id);
            }
        }
        if !round.gating_sources.is_empty() {
            self.judge_derived_edges(executor.backend_mut(), round);
        }
        if !self.counted.is_empty() {
            self.mark_counted_domains(executor.backend_mut(), round);
        }
        Ok(())
    }

    /// Clocks computed from a changed scheduled or process-driven signal,
    /// such as `clk & enable`, change at the same instant, in the active
    /// region before any nonblocking update (IEEE 1800-2023 4.4.2, 4.5):
    /// their edges are judged now, from the state before any domain commits,
    /// and their domains evaluate together with the source's. Their levels
    /// at this instant become the baseline for the rounds after the commit,
    /// where a gate the commit moves (an enable register rising with the
    /// edge) gives a further edge, as the update events of the NBA region do.
    fn judge_derived_edges(&mut self, backend: &mut B, round: &mut Round) {
        for source in std::mem::take(&mut round.gating_sources) {
            for &id in self.combinational_derived[source].iter() {
                if self.stable_edge_triggered(backend, id) {
                    backend.mark_triggered_bit(id);
                    round.edge_approved.insert(id);
                }
                let Some((signal, _, _)) = self.topo_signals.get(self.signal_index_by_id[id])
                else {
                    continue;
                };
                if event_level(backend, *signal) != 0 {
                    self.last_clock_values.insert(id);
                } else {
                    self.last_clock_values.remove(id);
                }
            }
        }
    }

    fn event_values(&self, backend: &B, values: &mut Vec<u8>) {
        values.clear();
        values.extend(
            self.topo_signals
                .iter()
                .map(|(signal, _, _)| event_level(backend, *signal)),
        );
    }

    /// Run the ready processes in declaration order, including those they
    /// wake, recording the event signals each one changes.
    fn run_ready<E>(
        &mut self,
        executor: &mut E,
        current_time: u64,
        round: &mut Round,
        returns: &mut u64,
    ) -> Result<bool, Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        if self.process_state.ready.is_empty() {
            return Ok(false);
        }
        self.event_values(executor.backend(), &mut round.before);
        while let Some(process) = self.process_state.ready.pop_first() {
            loop {
                let again = self.run_process(executor, current_time, process, returns)?;
                // Each run is a separate update of the event signals, so a
                // pulse across two runs is an edge.
                self.record_transitions(executor.backend(), round);
                // Before the next run, its edges must be seen. After the last
                // one, the check following the settle sees them: a settle
                // does not write what processes write.
                if again || !self.process_state.ready.is_empty() {
                    self.observe_driven(executor.backend());
                }
                if !again {
                    break;
                }
            }
        }
        Ok(true)
    }

    /// Record the event signals that changed since `round.before`.
    fn record_transitions(&self, backend: &B, round: &mut Round) {
        self.event_values(backend, &mut round.after);
        for (((_, id, _), &old), &new) in self
            .topo_signals
            .iter()
            .zip(&round.before)
            .zip(&round.after)
        {
            if *id != usize::MAX && old != new {
                round.transitions.push((*id, new != 0));
            }
        }
        std::mem::swap(&mut round.before, &mut round.after);
    }

    /// Evaluate the triggered domains before an inactive region runs. Their
    /// nonblocking assignments stay in the working state until the NBA
    /// region.
    fn evaluate_before_inactive<E>(
        &mut self,
        executor: &mut E,
        current_time: u64,
        round: &mut Round,
    ) -> Result<(), Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        let marked = executor.backend().get_triggered_bits();
        executor.backend_mut().clear_triggered_bits();
        let mut evaluated = BitSet::new();
        let mut unsplit = Vec::new();
        for id in marked.iter() {
            let info = self.event_info[id];
            if info.canonical_id == usize::MAX || evaluated.contains(info.canonical_id) {
                continue;
            }
            if info.is_cascaded
                && !round.counted_triggered.contains(id)
                && !round.edge_approved.contains(id)
                && !self.stable_edge_triggered(executor.backend(), id)
            {
                continue;
            }
            evaluated.insert(info.canonical_id);
            if info.eval_only_event.is_none() {
                // Without a separate evaluation unit the domain cannot
                // publish later; it evaluates with the remaining domains.
                unsplit.push(id);
                continue;
            }
            round.edge_approved.remove(id);
            self.evaluate_deferred(executor, current_time, round, info)?;
        }
        // The edges are consumed: later rounds compare against the values
        // they were evaluated at.
        self.synchronize_event_values(executor.backend());
        round.begin_scan(self.domain_kinds.len());
        round.scheduled_trigger_ids.make_empty();
        round.counted_triggered.make_empty();
        for id in unsplit {
            // The edge stays approved: the new baseline no longer shows it.
            executor.backend_mut().mark_triggered_bit(id);
            round.scheduled_trigger_ids.insert(id);
            round.edge_approved.insert(id);
        }
        Ok(())
    }

    fn evaluate_deferred<E>(
        &mut self,
        executor: &mut E,
        current_time: u64,
        round: &mut Round,
        info: EventInfo<B>,
    ) -> Result<(), Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        self.supersede(executor.backend(), round, info.canonical_id);
        let Some(event) = info.eval_only_event else {
            return Err(SimulatorErrorCode::InternalError.into());
        };
        executor.stage_external_event(info.eval_ff_event.unwrap_or(event), current_time)?;
        executor.eval_only_ff_at(event)?;
        round.deferred.push((info.canonical_id, None));
        Ok(())
    }

    /// A domain with an unpublished evaluation is evaluated again: keep the
    /// earlier evaluation's working state for the NBA region.
    fn supersede(&self, backend: &B, round: &mut Round, canonical: usize) {
        if let Some((_, image)) = round
            .deferred
            .iter_mut()
            .rev()
            .find(|(id, image)| *id == canonical && image.is_none())
        {
            *image = Some(working_image(backend));
        }
    }

    /// Publish the queued and flagged nonblocking assignments of process
    /// kernels (the NBA region, IEEE 1800-2023 4.5).
    fn publish_nba<E>(&mut self, executor: &mut E, round: &mut Round) -> bool
    where
        E: SimulationExecutor<Backend = B>,
    {
        if round.nba_published {
            return false;
        }
        round.nba_published = true;
        let flagged = round.kernels_ran && self.has_pending_nba_sites;
        if self.process_state.nba_queue.is_empty() && !flagged {
            return false;
        }
        let mut updates = std::mem::take(&mut self.process_state.nba_queue);
        if flagged {
            let backend = executor.backend_mut();
            for process in &self.processes {
                for site in &process.nba_sites {
                    let Some(pending) = site.pending else {
                        continue;
                    };
                    if backend.get_as::<u8>(pending) == 0 {
                        continue;
                    }
                    backend.set(pending, 0u8);
                    let offset = site
                        .offset
                        .map_or(0, |offset| backend.get_as::<u64>(offset) as usize);
                    updates.push(NbaUpdate {
                        target: site.target,
                        lsb: site.lsb + offset,
                        width: site.width,
                        value: read(backend, site.value),
                    });
                }
            }
        }
        if updates.is_empty() {
            return false;
        }
        // Several updates may pulse an event signal: each is an update the
        // event domains see, not only the final value.
        let several = updates.len() > 1;
        if several {
            self.event_values(executor.backend(), &mut round.before);
            round.transitions.clear();
        }
        for update in updates {
            let backend = executor.backend_mut();
            let merged = update.merge_into(&read(backend, update.target));
            let target = update.target;
            self.write_signal(backend, target, |backend| {
                if target.is_4state {
                    backend.set_four_state(target, merged.0, merged.1);
                } else {
                    backend.set_wide(target, merged.0);
                }
            });
            if several {
                self.record_transitions(executor.backend(), round);
            }
        }
        if several {
            let transitions = std::mem::take(&mut round.transitions);
            self.mark_occurrences(executor.backend_mut(), round, &transitions);
            round.transitions = transitions;
            round.transitions.clear();
        }
        round.track_stable_edges = true;
        true
    }

    /// Apply the domains evaluated before an inactive region.
    fn apply_deferred<E>(&mut self, executor: &mut E, round: &mut Round) -> Result<Vec<usize>, Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        let deferred = std::mem::take(&mut round.deferred);
        if deferred.is_empty() {
            return Ok(Vec::new());
        }
        let superseded = deferred.iter().any(|(_, image)| image.is_some());
        if superseded {
            // A domain publishes several evaluations, each an update of its
            // outputs: waits and event domains see each one.
            self.event_values(executor.backend(), &mut round.before);
            round.transitions.clear();
            let live = working_image(executor.backend());
            for (id, image) in &deferred {
                if let Some(image) = image {
                    restore_working_image(executor.backend_mut(), image);
                    if let Some(event) = self.event_info[*id].apply_event {
                        executor.apply_ff_at(event)?;
                    }
                    self.record_transitions(executor.backend(), round);
                    self.observe_driven(executor.backend());
                }
            }
            restore_working_image(executor.backend_mut(), &live);
        }
        let mut applied = Vec::new();
        for (id, image) in deferred {
            if image.is_none() {
                if let Some(event) = self.event_info[id].apply_event {
                    executor.apply_ff_at(event)?;
                }
                applied.push(id);
            }
        }
        // The domains' last evaluations may still publish with the NBA
        // region: their edges are marked once that is done.
        round.publishing = superseded;
        Ok(applied)
    }

    /// After the NBA region of a round in which a domain published several
    /// evaluations: see the last publication and mark the domains the
    /// edges among all of them trigger.
    fn finish_publications(&self, backend: &mut B, round: &mut Round) {
        if !std::mem::take(&mut round.publishing) {
            return;
        }
        self.record_transitions(backend, round);
        let transitions = std::mem::take(&mut round.transitions);
        self.mark_occurrences(backend, round, &transitions);
        round.transitions = transitions;
        round.transitions.clear();
    }

    /// Mark the domains that edges among `transitions` trigger, even if the
    /// signals end at their old values, and keep them marked through the
    /// settles of this round.
    fn mark_occurrences(&self, backend: &mut B, round: &mut Round, transitions: &[(usize, bool)]) {
        for &(id, is_nonzero) in transitions {
            let was_nonzero =
                round.previous(id, is_nonzero, || self.last_clock_values.contains(id));
            let triggered = match self.domain_kinds[id] {
                Some(DomainKind::ClockPosedge | DomainKind::ResetAsyncHigh) => {
                    !was_nonzero && is_nonzero
                }
                Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => {
                    was_nonzero && !is_nonzero
                }
                _ => was_nonzero != is_nonzero,
            };
            if triggered {
                round.occurrences.insert(id);
                round.scheduled_trigger_ids.insert(id);
                round.track_stable_edges = true;
                backend.mark_triggered_bit(id);
            }
        }
    }

    /// The process phase of a round: the active and inactive regions before
    /// any domain publishes (IEEE 1800-2023 4.5). Without ready processes,
    /// waits or counted signals this is one settle, the batch step's.
    fn process_phase<E>(
        &mut self,
        executor: &mut E,
        current_time: u64,
        round: &mut Round,
        mut entry: bool,
    ) -> Result<(), Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        let mut returns = 0;
        let mut first = true;
        loop {
            self.charge_pass()?;
            // Processes run after this time's scheduled values are applied
            // and see the state settled at the previous time. The event
            // signals they change are edges of this time, like scheduled
            // events.
            let ran = self.run_ready(executor, current_time, round, &mut returns)?;
            round.kernels_ran |= ran;
            if first || ran {
                let transitions = std::mem::take(&mut round.transitions);
                self.mark_scheduled_edges(executor.backend_mut(), round, &transitions);
                round.transitions = transitions;
                round.transitions.clear();
                self.settle_marked(executor, round)?;
                first = false;
            }
            if entry {
                // Scheduled and host values, including raw writes of the
                // host's view of the state, and what the settle derived.
                entry = false;
                if !self.process_state.waiters.is_empty() {
                    self.observe(executor.backend(), None);
                }
            } else {
                self.observe_driven(executor.backend());
            }
            if !self.process_state.ready.is_empty() {
                continue;
            }
            if self.process_state.inactive.is_empty() {
                return Ok(());
            }
            // The active region is empty: the domains it triggered evaluate
            // before the inactive region's processes resume.
            self.evaluate_before_inactive(executor, current_time, round)?;
            let inactive = std::mem::take(&mut self.process_state.inactive);
            self.process_state.ready.extend(inactive);
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

    /// Apply `events_to_process`, run the ready processes, and settle the
    /// edges they cause.
    ///
    /// A round is the batch step of a time slot with a process phase in
    /// front of the flip-flop domains (IEEE 1800-2023 4.5): processes run
    /// in the active region, the domains they and the scheduled values
    /// trigger evaluate, zero-delay processes run in the inactive region,
    /// and only then are nonblocking assignments published. A round
    /// without ready processes and counted signals does exactly the batch
    /// step.
    fn step_round<E>(
        &mut self,
        executor: &mut E,
        current_time: u64,
        events_to_process: Vec<SimEvent<B>>,
        ready_processes: Vec<usize>,
        entry: bool,
    ) -> Result<(), Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        if !self.timed {
            return self.batch_round(executor, current_time, events_to_process);
        }
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

        let timed = self.timed;
        if timed {
            // Host writes the waits must see one at a time.
            for (signal, value) in executor.take_host_writes() {
                self.observe_host_write(signal, &value);
            }
        }

        let num_events = executor.backend().num_events();
        for (index, event) in events_to_process.iter().enumerate() {
            let signal = event.signal;
            let value = event.next_val;
            if !timed {
                executor.backend_mut().set(signal, value);
                continue;
            }
            if let Some(&counted) = self.counted_by_offset.get(&signal.offset) {
                let backend = executor.backend_mut();
                let old = read(backend, signal);
                backend.set(signal, value);
                let new = read(backend, signal);
                self.counted[counted].refs.bump(backend, &old, &new);
            } else {
                executor.backend_mut().set(signal, value);
            }
            // A signal the batch writes again pulses: its waits see each
            // value. Otherwise one check after the batch sees the edge.
            if !self.process_state.waiters.is_empty()
                && events_to_process[index + 1..]
                    .iter()
                    .any(|later| later.signal == signal)
            {
                self.observe(executor.backend(), Some(signal));
            }
        }
        if timed && !self.process_state.waiters.is_empty() && !events_to_process.is_empty() {
            // The scheduled values wake their processes before the process
            // phase runs.
            self.observe(executor.backend(), None);
        }

        let mut round = self.scratch.take().unwrap_or_default();
        round.reset(events_to_process.is_empty());
        round.begin_scan(self.domain_kinds.len());
        if timed {
            self.process_state.ready.extend(ready_processes);
        }
        executor.backend_mut().clear_triggered_bits();
        // An external drive may already have been combinationally evaluated
        // by a testbench read. Compare against our saved baseline even if that
        // evaluation's transient trigger bits have since been cleared.
        round
            .transitions
            .extend(events_to_process.iter().filter_map(|event| {
                self.signal_to_id
                    .get(&event.signal)
                    .map(|&id| (id, event.next_val != 0))
            }));

        if timed {
            // The check after the batch already saw every signal.
            let entry = entry && events_to_process.is_empty();
            self.process_phase(executor, current_time, &mut round, entry)?;
        } else {
            let transitions = std::mem::take(&mut round.transitions);
            self.mark_scheduled_edges(executor.backend_mut(), &mut round, &transitions);
            round.transitions = transitions;
            self.settle_marked(executor, &mut round)?;
        }

        let mut triggered_domains = BitSet::with_capacity(num_events);
        let mut discovered_in_this_step = BitSet::with_capacity(num_events);
        let mut comb_already_done = false;
        let mut deferred_pending = !round.deferred.is_empty();
        loop {
            let mut any_new_outer_loop_trigger = false;
            let mut newly_triggered = Vec::new();

            loop {
                let mut any_new_sequential_trigger = false;
                let marked_bits = executor.backend().get_triggered_bits();
                executor.backend_mut().clear_triggered_bits();

                let mut can_use_eval_apply =
                    !deferred_pending && triggered_domains.is_empty() && marked_bits.count() == 1;
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
                            if timed {
                                self.publish_nba(executor, &mut round);
                            }
                            executor.eval_comb()?;
                            if round.track_stable_edges {
                                self.replace_triggers_with_stable_edges(executor.backend_mut());
                                // Pulses of the publications end where they
                                // started; the filter cannot see them.
                                for id in round.occurrences.iter() {
                                    executor.backend_mut().mark_triggered_bit(id);
                                }
                            }
                            if !self.counted.is_empty() {
                                self.mark_counted_domains(executor.backend_mut(), &mut round);
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
                    let info = self.event_info[id];
                    if triggered_domains.contains(info.canonical_id) {
                        continue;
                    }
                    // Native trigger bits can be coarse hints raised by a
                    // working-region write before its commit. A derived clock
                    // must have the requested stable edge before its FFs run.
                    if info.is_cascaded
                        && !round.counted_triggered.contains(id)
                        && !round.occurrences.contains(id)
                        && !round.edge_approved.contains(id)
                        && !self.stable_edge_triggered(executor.backend(), id)
                    {
                        continue;
                    }
                    discovered_in_this_step.insert(id);
                    triggered_domains.insert(info.canonical_id);
                    any_new_sequential_trigger = true;
                    newly_triggered.push(info.canonical_id);

                    if !round.deferred.is_empty() {
                        self.supersede(executor.backend(), &mut round, info.canonical_id);
                    }
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

            let mut published = false;
            if deferred_pending {
                deferred_pending = false;
                // Domains evaluated before an inactive region that did not
                // trigger again publish with this NBA region.
                for id in self.apply_deferred(executor, &mut round)? {
                    if !newly_triggered.contains(&id)
                        && let Some(event) = self.event_info[id].eval_ff_event
                    {
                        executor.fire_external_event(event, current_time)?;
                    }
                }
                published = true;
            }

            if newly_triggered.is_empty() && !any_new_outer_loop_trigger {
                if published {
                    self.finish_publications(executor.backend_mut(), &mut round);
                    self.observe_driven(executor.backend());
                }
                // The NBA region still publishes the processes' assignments,
                // which may trigger further domains.
                if (timed && self.publish_nba(executor, &mut round)) || published {
                    self.settle_marked(executor, &mut round)?;
                    continue;
                }
                break;
            }

            for id in &newly_triggered {
                if let Some(event) = self.event_info[*id].apply_event {
                    executor.apply_ff_at(event)?;
                }
            }
            if round.publishing {
                self.finish_publications(executor.backend_mut(), &mut round);
                self.observe_driven(executor.backend());
            }
            for id in &newly_triggered {
                if let Some(event) = self.event_info[*id].eval_ff_event {
                    executor.fire_external_event(event, current_time)?;
                }
            }
            if timed {
                self.publish_nba(executor, &mut round);
            }

            if comb_already_done {
                comb_already_done = false;
            } else {
                executor.eval_comb()?;
                if round.track_stable_edges {
                    self.replace_triggers_with_stable_edges(executor.backend_mut());
                    for id in round.occurrences.iter() {
                        executor.backend_mut().mark_triggered_bit(id);
                    }
                }
                if !self.counted.is_empty() {
                    self.mark_counted_domains(executor.backend_mut(), &mut round);
                }
            }
        }

        for (signal, id, _) in &self.topo_signals {
            if *id == usize::MAX {
                continue;
            }
            if event_level(executor.backend(), *signal) != 0 {
                self.last_clock_values.insert(*id);
            } else {
                self.last_clock_values.remove(*id);
            }
        }

        // Processes the published values wake run in the next round.
        if timed {
            self.observe_driven(executor.backend());
        }

        for event in periodic_events_to_process {
            let event_id = event.event_ref.id();
            if let Some(Some(clock)) = self.scheduler.clocks.get(event_id) {
                // Keep the extra odd-period tick in the high phase, matching
                // the process/testbench clock model.
                // A clock needs at least one time unit in each phase.
                let period = clock.period.max(2);
                let half = (period / 2) + ((period & 1) & u64::from(event.next_val != 0));
                let time = current_time.checked_add(half).ok_or_else(|| {
                    Halt::Error(SimulatorErrorCode::Runtime {
                        message: format!("event {event_id} overflows simulation time"),
                        signals: Vec::new(),
                    })
                })?;
                self.push_periodic_event(SimEvent {
                    time,
                    event_ref: event.event_ref,
                    signal: event.signal,
                    next_val: 1 - event.next_val,
                });
            }
        }

        self.scratch = Some(round);
        Ok(())
    }

    /// The round of a design without processes and counted signals: the
    /// batch step as it was before process rounds, kept apart from
    /// [`Self::step_round`] so that such designs run its code unchanged.
    fn batch_round<E>(
        &mut self,
        executor: &mut E,
        current_time: u64,
        events_to_process: Vec<SimEvent<B>>,
    ) -> Result<(), Halt>
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

        let mut triggered_domains = BitSet::with_capacity(num_events);
        let mut discovered_in_this_step = BitSet::with_capacity(num_events);
        let mut scheduled_trigger_ids = BitSet::with_capacity(num_events);
        // An external drive may already have been combinationally evaluated
        // by a testbench read. Compare against our saved baseline even if that
        // evaluation's transient trigger bits have since been cleared.
        let mut track_stable_edges = events_to_process.is_empty();
        executor.backend_mut().clear_triggered_bits();

        let scheduled = events_to_process.iter().filter_map(|event| {
            self.signal_to_id
                .get(&event.signal)
                .map(|&id| (id, event.next_val != 0))
        });
        // Scheduled signals that changed level and gate derived clocks.
        let mut gating_sources = Vec::new();
        for (id, is_nonzero) in scheduled {
            track_stable_edges = true;
            let was_nonzero = self.last_clock_values.contains(id);
            if was_nonzero != is_nonzero
                && self
                    .combinational_derived
                    .get(id)
                    .is_some_and(|derived| !derived.is_empty())
            {
                gating_sources.push(id);
            }
            let triggered = match self.domain_kinds[id] {
                Some(DomainKind::ClockPosedge | DomainKind::ResetAsyncHigh) => {
                    !was_nonzero && is_nonzero
                }
                Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => {
                    was_nonzero && !is_nonzero
                }
                _ => !was_nonzero && is_nonzero,
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
        // Clocks computed from a changed scheduled signal, such as
        // `clk & enable`, change at the same instant, in the active region
        // before any nonblocking update (IEEE 1800-2023 4.4.2, 4.5): their
        // edges are judged now, from the state before any domain commits, and
        // their domains evaluate together with the scheduled ones. Their
        // levels at this instant become the baseline for the rounds after the
        // commit, where a gate the commit moves (an enable register rising
        // with the edge) gives a further edge, as the update events of the
        // NBA region do.
        let mut edge_approved: Option<BitSet> = None;
        for &source in &gating_sources {
            for &id in self.combinational_derived[source].iter() {
                if self.stable_edge_triggered(executor.backend(), id) {
                    executor.backend_mut().mark_triggered_bit(id);
                    edge_approved
                        .get_or_insert_with(|| BitSet::with_capacity(num_events))
                        .insert(id);
                }
                let Some((signal, _, _)) = self.topo_signals.get(self.signal_index_by_id[id])
                else {
                    continue;
                };
                if event_level(executor.backend(), *signal) != 0 {
                    self.last_clock_values.insert(id);
                } else {
                    self.last_clock_values.remove(id);
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
                    let info = self.event_info[id];
                    if triggered_domains.contains(info.canonical_id) {
                        continue;
                    }
                    // Native trigger bits can be coarse hints raised by a
                    // working-region write before its commit. A derived clock
                    // must have the requested stable edge before its FFs run.
                    if info.is_cascaded
                        && !edge_approved.as_ref().is_some_and(|ids| ids.contains(id))
                        && !self.stable_edge_triggered(executor.backend(), id)
                    {
                        continue;
                    }
                    discovered_in_this_step.insert(id);
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
            if event_level(executor.backend(), *signal) != 0 {
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
                let time = current_time.checked_add(half).ok_or_else(|| {
                    Halt::Error(SimulatorErrorCode::Runtime {
                        message: format!("event {event_id} overflows simulation time"),
                        signals: Vec::new(),
                    })
                })?;
                self.push_periodic_event(SimEvent {
                    time,
                    event_ref: event.event_ref,
                    signal: event.signal,
                    next_val: 1 - event.next_val,
                });
            }
        }

        Ok(())
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

    /// The next time with scheduled events or processes to run.
    fn next_work_time(&self) -> Option<u64> {
        if self.finished {
            return None;
        }
        if !self.process_state.ready.is_empty() {
            return Some(self.scheduler.time);
        }
        let process = self.process_wakeups.peek().map(|&Reverse((time, _))| time);
        match (self.scheduler.next_event_time(), process) {
            (Some(event), Some(process)) => Some(event.min(process)),
            (event, process) => event.or(process),
        }
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
            process_wakeups: self.process_wakeups.clone(),
            process_state: self.process_state.clone(),
            host_pending: self.host_pending,
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
        self.process_wakeups.clone_from(&snapshot.process_wakeups);
        self.process_state.clone_from(&snapshot.process_state);
        self.host_pending = snapshot.host_pending;
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
    /// Waiting and ready processes, queued nonblocking assignments and the
    /// counter values counted domains last triggered on.
    pub process_state: ProcessState,
    /// The host changed state the next step settles at the current time.
    pub host_pending: bool,
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
            process_state: self.process_state.clone(),
            host_pending: self.host_pending,
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
        self.process_wakeups = parts
            .process_wakeups
            .into_iter()
            .map(|(process, time)| Reverse((time, process)))
            .collect();
        self.process_state = parts.process_state;
        self.host_pending = parts.host_pending;
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
    process_wakeups: BinaryHeap<ProcessWakeup>,
    process_state: ProcessState,
    host_pending: bool,
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
            process_wakeups: self.process_wakeups.clone(),
            process_state: self.process_state.clone(),
            host_pending: self.host_pending,
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

/// The working and sparse working state: the next-state values of
/// evaluated but unpublished flip-flop domains.
fn working_image<B: SimBackend>(backend: &B) -> Vec<u8> {
    let layout = backend.layout();
    let range = layout.working_base_offset..layout.triggered_bits_offset;
    let (ptr, len) = backend.memory_as_ptr();
    assert!(range.end <= len, "working state exceeds the memory image");
    // Safety: the backend exposes `len` readable bytes at `ptr`.
    let memory = unsafe { std::slice::from_raw_parts(ptr, len) };
    memory[range].to_vec()
}

fn restore_working_image<B: SimBackend>(backend: &mut B, image: &[u8]) {
    let offset = backend.layout().working_base_offset;
    backend.write_memory(offset, image);
}
