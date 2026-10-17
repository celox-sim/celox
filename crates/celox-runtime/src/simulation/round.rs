//! Rounds of a time slot with processes or counted signals: settling,
//! derived-clock edges, domains evaluated before an inactive region and
//! their publication.

use bit_set::BitSet;

use super::host::SimulationExecutor;
use super::nba::{restore_working_image, working_image};
use super::time::PeriodicEventKey;
use super::{EventInfo, SimulationState, event_level};
use crate::{
    SimulatorErrorCode,
    backend::{EventHandle, SimBackend},
    scheduler::SimEvent,
    timing::{Halt, read},
};

/// Bookkeeping of one round of a time slot.
#[derive(Default)]
pub(super) struct Round {
    /// Event ids whose trigger survives the settling filter.
    pub(super) scheduled_trigger_ids: BitSet,
    pub(super) track_stable_edges: bool,
    /// Event ids a counter triggered; they bypass the stable-edge test of
    /// cascaded domains, since a counted pulse may end at its old value.
    pub(super) counted_triggered: BitSet,
    /// Domains evaluated before an inactive region, in evaluation order,
    /// with the working image of an evaluation that a later one of the
    /// same domain superseded (IEEE 1800-2023 4.5: their nonblocking
    /// assignments wait for the NBA region).
    deferred: Vec<(usize, Option<Vec<u8>>)>,
    /// Kernels ran in this round, so pending nonblocking flags may be set.
    pub(super) kernels_ran: bool,
    pub(super) nba_published: bool,
    /// Event-signal values scheduled or written by processes, not yet
    /// marked.
    pub(super) transitions: Vec<(usize, bool)>,
    /// Event-signal values before and after a process run.
    pub(super) before: Vec<u8>,
    pub(super) after: Vec<u8>,
    /// Event ids a pulse of several publications triggered; like counted
    /// triggers they survive settling and bypass the stable-edge test.
    pub(super) occurrences: BitSet,
    /// A domain published several evaluations and `transitions` records
    /// the event signals they changed.
    publishing: bool,
    /// Event ids whose level change gates combinationally derived clocks,
    /// not yet judged.
    pub(super) gating_sources: Vec<usize>,
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
    pub(super) fn previous(
        &mut self,
        id: usize,
        value: bool,
        baseline: impl FnOnce() -> bool,
    ) -> bool {
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
    /// Settle combinational logic after the round's sources changed, and
    /// keep the triggers the round considers stable.
    #[inline]
    pub(super) fn settle_marked<E>(
        &mut self,
        executor: &mut E,
        round: &mut Round,
    ) -> Result<(), Halt>
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

    /// Evaluate the triggered domains before an inactive region runs. Their
    /// nonblocking assignments stay in the working state until the NBA
    /// region.
    pub(super) fn evaluate_before_inactive<E>(
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
    pub(super) fn step_round<E>(
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
                            if timed {
                                // Each update is seen by the waits on it
                                // (IEEE 1800-2023 4.5).
                                self.observe_driven(executor.backend());
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
                if timed {
                    // A combinational value between two waves of domains is
                    // an update the waits see (IEEE 1800-2023 4.5).
                    self.observe_driven(executor.backend());
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
}
