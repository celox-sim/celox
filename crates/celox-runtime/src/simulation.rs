use bit_set::BitSet;
use celox_design::DomainKind;
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
    topo_signals: Vec<(SignalRef, usize, usize)>,
    domain_kinds: Vec<Option<DomainKind>>,
    event_info: Vec<EventInfo<B>>,
    signal_to_id: FxHashMap<SignalRef, usize>,
}

impl<B: SimBackend> SimulationState<B> {
    /// Rebase edge detection after state was advanced outside this scheduler.
    pub fn synchronize_event_values(&mut self, backend: &B) {
        self.last_clock_values.make_empty();
        for (signal, id, _) in &self.topo_signals {
            if *id == usize::MAX {
                continue;
            }
            let value: u8 = backend.get_as(*signal);
            if value != 0 {
                self.last_clock_values.insert(*id);
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
            let value: u8 = backend.get_as(*signal);
            let is_nonzero = value != 0;
            let triggered = match self.domain_kinds[*id] {
                Some(DomainKind::ClockPosedge | DomainKind::ResetAsyncHigh) => {
                    !was_nonzero && is_nonzero
                }
                Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => {
                    was_nonzero && !is_nonzero
                }
                _ => was_nonzero != is_nonzero,
            };
            if triggered {
                backend.mark_triggered_bit(*id);
            }
        }
    }

    pub fn new(
        backend: &B,
        topo_signals: Vec<(SignalRef, usize, usize)>,
        domain_kinds: Vec<Option<DomainKind>>,
        event_info: Vec<EventInfo<B>>,
    ) -> Self {
        let mut last_clock_values = BitSet::with_capacity(backend.num_events());
        let mut signal_to_id = FxHashMap::default();
        for (signal, id, _) in topo_signals.iter().copied() {
            if id == usize::MAX {
                continue;
            }
            signal_to_id.insert(signal, id);
            let value: u8 = backend.get_as(signal);
            if value != 0 {
                last_clock_values.insert(id);
            }
        }

        Self {
            scheduler: Scheduler::new(),
            periodic_events: FxHashMap::default(),
            last_clock_values,
            topo_signals,
            domain_kinds,
            event_info,
            signal_to_id,
        }
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
        let (current_time, events_to_process) = match self.scheduler.pop_all_at_next_time() {
            Some(events) => events,
            None => return Ok(None),
        };
        self.step_events(executor, current_time, events_to_process)
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
        self.step_events(executor, time, Vec::new())
    }

    fn step_events<E>(
        &mut self,
        executor: &mut E,
        current_time: u64,
        events_to_process: Vec<SimEvent<B>>,
    ) -> Result<Option<u64>, SimulatorErrorCode>
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

        for event in &events_to_process {
            if let Some(&id) = self.signal_to_id.get(&event.signal) {
                track_stable_edges = true;
                let was_nonzero = self.last_clock_values.contains(id);
                let is_nonzero = event.next_val != 0;
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
            let value: u8 = executor.backend().get_as(*signal);
            if value != 0 {
                self.last_clock_values.insert(*id);
            } else {
                self.last_clock_values.remove(*id);
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

        executor.finish_timed_step(current_time);
        Ok(Some(current_time))
    }

    pub fn time(&self) -> u64 {
        self.scheduler.time
    }

    pub fn set_time(&mut self, time: u64) {
        self.scheduler.time = time;
    }

    pub fn next_event_time(&self) -> Option<u64> {
        self.scheduler.next_event_time()
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
    }
}

/// Scheduling state captured by [`SimulationState::snapshot`].
pub struct SimulationSnapshot<B: SimBackend> {
    time: u64,
    clocks: Vec<Option<ClockDef>>,
    event_queue: std::collections::BinaryHeap<SimEvent<B>>,
    periodic_events: FxHashMap<PeriodicEventKey, usize>,
    last_clock_values: BitSet,
}

impl<B: SimBackend> Clone for SimulationSnapshot<B> {
    fn clone(&self) -> Self {
        Self {
            time: self.time,
            clocks: self.clocks.clone(),
            event_queue: self.event_queue.clone(),
            periodic_events: self.periodic_events.clone(),
            last_clock_values: self.last_clock_values.clone(),
        }
    }
}

impl<B: SimBackend> SimulationSnapshot<B> {
    /// Simulation time at which the snapshot was taken.
    pub fn time(&self) -> u64 {
        self.time
    }
}
