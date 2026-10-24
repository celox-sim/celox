//! Rounds of a time slot with processes: settling,
//! derived-clock edges, domains evaluated before an inactive region and
//! their publication.

use bit_set::BitSet;
use celox_design::edges::settled_edge;

use super::host::SimulationExecutor;
use super::nba::{restore_working_image, working_image};
use super::{EventInfo, SimulationState, Suspended};
use crate::{
    SimulatorErrorCode,
    backend::SimBackend,
    scheduler::SimEvent,
    timing::{Halt, read},
};

/// Bookkeeping of one round of a time slot.
#[derive(Default)]
pub(super) struct Round {
    /// The round's process phase is the reactive region set's: it runs
    /// program processes, and the design activity they create follows it
    /// (IEEE 1800-2023 4.5).
    pub(super) reactive: bool,
    /// The stopped process resumes before any newly woken ready processes.
    pub(super) resume_process: Option<usize>,
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
    /// Event-signal levels (see [`super::edge_level`]) scheduled or written
    /// by processes, not yet marked.
    pub(super) transitions: Vec<(usize, u8)>,
    /// Event-signal levels before and after a process run.
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
    latest: Vec<(u32, u8)>,
    generation: u32,
}

impl Round {
    /// Start a scan of transitions over `events` event ids against a new
    /// baseline.
    fn begin_scan(&mut self, events: usize) {
        if self.latest.len() < events {
            self.latest.resize(events, (0, 0));
        }
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            // Stamps of an earlier wrap must not match.
            self.latest.fill((0, 0));
            self.generation = 1;
        }
    }

    /// The level event `id` had before this transition: its latest level in
    /// the scan, or `baseline`; records `value` as its latest.
    pub(super) fn previous(&mut self, id: usize, value: u8, baseline: impl FnOnce() -> u8) -> u8 {
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
    fn reset(&mut self, track_stable_edges: bool, reactive: bool) {
        self.reactive = reactive;
        self.resume_process = None;
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
        if !self.model.counted.is_empty() {
            self.mark_counted_domains(executor.backend_mut(), round);
        }
        Ok(())
    }

    /// Host-created edges are valid independently of the suspended round's
    /// pending source edges. Keep them through subsequent process settles,
    /// including when an inactive region advances the edge baselines.
    pub(super) fn settle_host_resume<E>(
        &mut self,
        executor: &mut E,
        round: &mut Round,
        before_host: &[u8],
    ) -> Result<(), Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        self.settle_marked(executor, round)?;
        for (index, &was) in before_host.iter().enumerate() {
            let (signal, id) = self.model.topo_signals[index];
            if id == usize::MAX {
                continue;
            }
            let is = self.level(executor.backend(), signal);
            // The global baseline may precede a pre-stop transition, including
            // one deliberately filtered until its scheduled source commits.
            // Only changes from the stopped state belong to this host settle.
            round.previous(id, is, || was);
            if was == is {
                continue;
            }
            let kind = self.model.domain_kinds[id];
            let triggered = if self.four_state {
                settled_edge::<true>(kind, was, is)
            } else {
                settled_edge::<false>(kind, was, is)
            };
            if triggered {
                round.edge_approved.insert(id);
                executor.backend_mut().mark_triggered_bit(id);
            }
            // Opposite-edge aliases also need the intermediate level for a
            // reversal by the continuation. Unchanged pre-stop levels retain
            // their baseline until the pending source domains commit.
            self.record_level(id, is);
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
            for index in 0..self.model.combinational_derived[source].len() {
                let id = self.model.combinational_derived[source][index];
                if self.stable_edge_triggered(backend, id) {
                    backend.mark_triggered_bit(id);
                    round.edge_approved.insert(id);
                }
                let Some(&(signal, _)) = self
                    .model
                    .topo_signals
                    .get(self.model.signal_index_by_id[id])
                else {
                    continue;
                };
                let level = self.level(backend, signal);
                self.record_level(id, level);
            }
        }
    }

    /// A marked trigger evaluates a domain that commits its blocking
    /// results in the active region (the filter of
    /// [`Self::evaluate_before_inactive`]).
    pub(super) fn active_commit_triggered(&self, backend: &B, round: &Round) -> bool {
        if self.model.active_commits.is_empty() {
            return false;
        }
        backend.get_triggered_bits().iter().any(|id| {
            let info = self.model.event_info[id];
            info.canonical_id != usize::MAX
                && info.eval_only_event.is_some()
                && self.model.active_commits.contains(info.canonical_id)
                && (!info.is_cascaded
                    || round.counted_triggered.contains(id)
                    || round.edge_approved.contains(id)
                    || self.stable_edge_triggered(backend, id))
        })
    }

    /// Evaluate the triggered domains before an inactive region runs. Their
    /// nonblocking assignments stay in the working state until the NBA
    /// region. A domain that commits its blocking results does so now, in
    /// the active region: combinational logic follows them and the domains
    /// their edges trigger are marked. Returns whether a domain committed
    /// its blocking results.
    pub(super) fn evaluate_before_inactive<E>(
        &mut self,
        executor: &mut E,
        current_time: u64,
        round: &mut Round,
    ) -> Result<bool, Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        let marked = executor.backend().get_triggered_bits();
        executor.backend_mut().clear_triggered_bits();
        if !self.model.active_commits.is_empty() {
            // The levels before any blocking result is committed.
            self.event_values(executor.backend(), &mut round.before);
        }
        let mut evaluated = BitSet::new();
        let mut committed = false;
        let mut unsplit = Vec::new();
        for id in marked.iter() {
            let info = self.model.event_info[id];
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
            committed |= self.model.active_commits.contains(info.canonical_id);
            self.evaluate_deferred(executor, current_time, round, info)?;
        }
        // Edges the committed blocking results give, directly or through
        // combinational logic, are occurrences of this active region.
        let mut occurred = Vec::new();
        if committed {
            let pending = executor.backend().get_triggered_bits();
            executor.eval_comb()?;
            // The settle's own trigger marks are replaced by the edges judged
            // against the levels before the commits.
            executor.backend_mut().clear_triggered_bits();
            self.record_transitions(executor.backend(), round);
            let transitions = std::mem::take(&mut round.transitions);
            round.begin_scan(self.model.domain_kinds.len());
            self.mark_occurrences(executor.backend_mut(), round, &transitions);
            occurred.extend(executor.backend().get_triggered_bits().iter());
            executor.backend_mut().clear_triggered_bits();
            for id in pending.iter() {
                executor.backend_mut().mark_triggered_bit(id);
            }
            round.transitions = transitions;
            round.transitions.clear();
        }
        // The edges are consumed: later rounds compare against the values
        // they were evaluated at.
        self.synchronize_event_values(executor.backend());
        round.begin_scan(self.model.domain_kinds.len());
        round.scheduled_trigger_ids.make_empty();
        round.counted_triggered.make_empty();
        for id in unsplit.into_iter().chain(occurred) {
            // The edge stays approved: the new baseline no longer shows it.
            executor.backend_mut().mark_triggered_bit(id);
            round.scheduled_trigger_ids.insert(id);
            round.edge_approved.insert(id);
        }
        Ok(committed)
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
                    if let Some(event) = self.model.event_info[*id].apply_event {
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
                if let Some(event) = self.model.event_info[id].apply_event {
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
    /// step. In a `reactive` round the process phase is that of the reactive
    /// region set, and the domains its processes trigger evaluate after it.
    pub(super) fn step_round<E>(
        &mut self,
        executor: &mut E,
        current_time: u64,
        events_to_process: Vec<SimEvent<B>>,
        ready_processes: Vec<usize>,
        entry: bool,
        reactive: bool,
    ) -> Result<(), Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        self.advance_time(executor.backend_mut(), current_time);
        if !self.model.timed {
            return self.batch_round(executor, current_time, events_to_process);
        }
        // Host writes the waits must see one at a time.
        for (signal, value) in executor.take_host_writes() {
            self.observe_host_write(signal, &value);
        }

        for (index, event) in events_to_process.iter().enumerate() {
            let signal = event.signal;
            let value = event.next_val;
            if let Some(&counted) = self.model.counted_by_offset.get(&signal.offset) {
                let backend = executor.backend_mut();
                let old = read(backend, signal);
                backend.set(signal, value);
                let new = read(backend, signal);
                self.model.counted[counted].refs.bump(backend, &old, &new);
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
        if !self.process_state.waiters.is_empty() && !events_to_process.is_empty() {
            // The scheduled values wake their processes before the process
            // phase runs.
            self.observe(executor.backend(), None);
        }

        let mut round = self.scratch.take().unwrap_or_default();
        round.reset(events_to_process.is_empty(), reactive);
        round.begin_scan(self.model.domain_kinds.len());
        self.process_state.ready.extend(ready_processes);
        executor.backend_mut().clear_triggered_bits();
        // An external drive may already have been combinationally evaluated
        // by a testbench read. Compare against our saved baseline even if that
        // evaluation's transient trigger bits have since been cleared.
        round
            .transitions
            .extend(events_to_process.iter().filter_map(|event| {
                self.model
                    .signal_to_id
                    .get(&event.signal)
                    .map(|&id| (id, u8::from(event.next_val != 0)))
            }));

        // The check after the batch already saw every signal.
        let entry = entry && events_to_process.is_empty();
        self.continue_round(executor, current_time, round, events_to_process, entry)
    }

    /// The rest of a round from its process phase. A `$stop` there suspends
    /// the round, and the next step continues it here.
    pub(super) fn continue_round<E>(
        &mut self,
        executor: &mut E,
        current_time: u64,
        mut round: Box<Round>,
        events_to_process: Vec<SimEvent<B>>,
        entry: bool,
    ) -> Result<(), Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        match self.process_phase(executor, current_time, &mut round, entry) {
            Ok(()) => {}
            Err(Halt::Stop) => {
                // Preserve the settle's accepted triggers, not its filtered
                // transient levels, if host writes require another settle.
                round
                    .edge_approved
                    .union_with(&executor.backend().get_triggered_bits());
                let mut levels = Vec::new();
                self.event_values(executor.backend(), &mut levels);
                self.suspended = Some(Suspended {
                    round,
                    events: events_to_process,
                    levels,
                });
                return Err(Halt::Stop);
            }
            Err(halt) => return Err(halt),
        }

        let num_events = executor.backend().num_events();
        let mut triggered_domains = BitSet::with_capacity(num_events);
        let mut discovered_in_this_step = BitSet::with_capacity(num_events);
        let mut comb_already_done = false;
        let mut deferred_pending = !round.deferred.is_empty();
        loop {
            let mut any_new_outer_loop_trigger = false;
            let mut newly_triggered = Vec::new();
            // The host sees the wave's edges before its domains commit, and
            // its writes are in state before they evaluate.
            if executor.before_domains(current_time)? {
                for (signal, value) in executor.take_host_writes() {
                    self.observe_host_write(signal, &value);
                }
                executor.eval_comb()?;
                self.observe_driven(executor.backend());
            }

            loop {
                let mut any_new_sequential_trigger = false;
                let marked_bits = executor.backend().get_triggered_bits();
                executor.backend_mut().clear_triggered_bits();

                let mut can_use_eval_apply =
                    !deferred_pending && triggered_domains.is_empty() && marked_bits.count() == 1;
                if can_use_eval_apply {
                    let single_id = marked_bits.iter().next().expect("one marked trigger");
                    let info = self.model.event_info[single_id];
                    can_use_eval_apply = !info.is_cascaded;
                    if can_use_eval_apply {
                        if let Some(event) = info.eval_ff_event {
                            discovered_in_this_step.insert(single_id);
                            triggered_domains.insert(info.canonical_id);
                            any_new_outer_loop_trigger = true;
                            executor.stage_external_event(event, current_time)?;
                            executor.eval_apply_ff_at(event)?;
                            executor.fire_external_event(event, current_time)?;
                            self.publish_nba(executor, &mut round);
                            executor.eval_comb()?;
                            if round.track_stable_edges {
                                self.replace_triggers_with_stable_edges(executor.backend_mut());
                                // Pulses of the publications end where they
                                // started; the filter cannot see them.
                                for id in round.occurrences.iter() {
                                    executor.backend_mut().mark_triggered_bit(id);
                                }
                            }
                            if !self.model.counted.is_empty() {
                                self.mark_counted_domains(executor.backend_mut(), &mut round);
                            }
                            // Each update is seen by the waits on it
                            // (IEEE 1800-2023 4.5).
                            self.observe_driven(executor.backend());
                            comb_already_done = true;
                            break;
                        }
                    }
                }

                for id in marked_bits.iter() {
                    if discovered_in_this_step.contains(id) {
                        continue;
                    }
                    let info = self.model.event_info[id];
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
                        && let Some(event) = self.model.event_info[id].eval_ff_event
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
                if self.publish_nba(executor, &mut round) || published {
                    self.settle_marked(executor, &mut round)?;
                    continue;
                }
                break;
            }

            for id in &newly_triggered {
                if let Some(event) = self.model.event_info[*id].apply_event {
                    executor.apply_ff_at(event)?;
                }
            }
            if round.publishing {
                self.finish_publications(executor.backend_mut(), &mut round);
                self.observe_driven(executor.backend());
            }
            for id in &newly_triggered {
                if let Some(event) = self.model.event_info[*id].eval_ff_event {
                    executor.fire_external_event(event, current_time)?;
                }
            }
            self.publish_nba(executor, &mut round);

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
                if !self.model.counted.is_empty() {
                    self.mark_counted_domains(executor.backend_mut(), &mut round);
                }
                // A combinational value between two waves of domains is
                // an update the waits see (IEEE 1800-2023 4.5).
                self.observe_driven(executor.backend());
            }
        }

        self.resample_clock_values(executor.backend());

        // Processes the published values wake run in the next round.
        self.observe_driven(executor.backend());

        self.rearm_clocks(current_time, &events_to_process)?;

        self.scratch = Some(round);
        Ok(())
    }
}
