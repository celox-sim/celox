//! The round of a design without processes and counted signals, and the
//! stable-edge helpers it calls, kept in one module.

use bit_set::BitSet;
use celox_design::DomainKind;

use super::SimulationState;
use super::host::SimulationExecutor;
use super::time::PeriodicEventKey;
use crate::{
    SignalRef, SimulatorErrorCode,
    backend::{EventHandle, SimBackend},
    scheduler::SimEvent,
    timing::Halt,
};

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

/// The bit of an edge level that marks an unknown (x or z) value; the value
/// bit then tells x (1) from z (0), as in the state image.
pub(crate) const LEVEL_UNKNOWN: u8 = 2;

/// The level of an edge-sensed signal for [`stable_edge`]: 0, 1, or, in a
/// four-state design, [`LEVEL_UNKNOWN`] with the value bit for x and z.
/// `FOUR_STATE` is the design's state mode; a two-state design reads only
/// the value bits, as [`event_level`] does.
#[inline]
pub(crate) fn edge_level<const FOUR_STATE: bool, B: SimBackend>(
    backend: &B,
    signal: SignalRef,
) -> u8 {
    let value = u8::from(event_level(backend, signal) != 0);
    if FOUR_STATE && signal.is_4state && event_unknown(backend, signal) {
        value | LEVEL_UNKNOWN
    } else {
        value
    }
}

/// Whether a four-state event signal has unknown bits. Its mask plane
/// follows its value bytes in the state image.
#[inline]
fn event_unknown<B: SimBackend>(backend: &B, signal: SignalRef) -> bool {
    let mask = SignalRef {
        offset: signal.offset + celox_state_layout::get_byte_size(signal.width),
        width: signal.width,
        is_4state: false,
        array_layout: None,
    };
    event_level(backend, mask) != 0
}

/// Whether a settled level change from `was` to `is`, levels as
/// [`edge_level`] gives them, is an active edge of a domain of `kind`
/// (IEEE 1800-2023 9.4.2, Table 9-2): a posedge leaves 0 or reaches 1, a
/// negedge leaves 1 or reaches 0, and a change between x and z is neither.
/// A domain of unknown kind reacts to any change. With two-state levels
/// (`FOUR_STATE` false) this is a rise or a fall of the value.
#[inline]
pub(crate) fn stable_edge<const FOUR_STATE: bool>(
    kind: Option<DomainKind>,
    was: u8,
    is: u8,
) -> bool {
    let between_unknowns = FOUR_STATE && was & is & LEVEL_UNKNOWN != 0;
    match kind {
        Some(DomainKind::ClockPosedge | DomainKind::ResetAsyncHigh) => {
            was != 1 && is != 0 && !between_unknowns
        }
        Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => {
            was != 0 && is != 1 && !between_unknowns
        }
        _ => was != is,
    }
}

impl<B: SimBackend> SimulationState<B> {
    #[inline(always)]
    pub(super) fn stable_edge_triggered(&self, backend: &B, id: usize) -> bool {
        if self.four_state {
            self.stable_edge_triggered_four_state(backend, id)
        } else {
            self.stable_edge_triggered_in::<false>(backend, id)
        }
    }

    #[inline(never)]
    fn stable_edge_triggered_four_state(&self, backend: &B, id: usize) -> bool {
        self.stable_edge_triggered_in::<true>(backend, id)
    }

    #[inline(always)]
    fn stable_edge_triggered_in<const FOUR_STATE: bool>(&self, backend: &B, id: usize) -> bool {
        let Some((signal, _)) = self.topo_signals.get(self.signal_index_by_id[id]) else {
            return true;
        };
        let was = self.last_level_in::<FOUR_STATE>(id);
        let is = edge_level::<FOUR_STATE, _>(backend, *signal);
        stable_edge::<FOUR_STATE>(self.domain_kinds[id], was, is)
    }

    pub(super) fn replace_triggers_with_stable_edges(&self, backend: &mut B) {
        if self.four_state {
            self.replace_triggers_in::<true>(backend);
        } else {
            self.replace_triggers_in::<false>(backend);
        }
    }

    fn replace_triggers_in<const FOUR_STATE: bool>(&self, backend: &mut B) {
        backend.clear_triggered_bits();
        for (signal, id) in &self.topo_signals {
            if *id == usize::MAX {
                continue;
            }
            let was = self.last_level_in::<FOUR_STATE>(*id);
            let is = edge_level::<FOUR_STATE, _>(backend, *signal);
            if stable_edge::<FOUR_STATE>(self.domain_kinds[*id], was, is) {
                backend.mark_triggered_bit(*id);
            }
        }
    }

    /// The round of a design without processes and counted signals: the
    /// batch step as it was before process rounds, kept apart from
    /// [`Self::step_round`] so that such designs run its code unchanged.
    pub(super) fn batch_round<E>(
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
                .map(|&id| (id, u8::from(event.next_val != 0)))
        });
        // Scheduled signals that changed level and gate derived clocks.
        let mut gating_sources = Vec::new();
        for (id, is) in scheduled {
            track_stable_edges = true;
            let was = self.last_level(id);
            if was != is
                && self
                    .combinational_derived
                    .get(id)
                    .is_some_and(|derived| !derived.is_empty())
            {
                gating_sources.push(id);
            }
            if self.scheduled_edge(id, was, is) {
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
            for index in 0..self.combinational_derived[source].len() {
                let id = self.combinational_derived[source][index];
                if self.stable_edge_triggered(executor.backend(), id) {
                    executor.backend_mut().mark_triggered_bit(id);
                    edge_approved
                        .get_or_insert_with(|| BitSet::with_capacity(num_events))
                        .insert(id);
                }
                let Some(&(signal, _)) = self.topo_signals.get(self.signal_index_by_id[id]) else {
                    continue;
                };
                let level = self.level(executor.backend(), signal);
                self.record_level(id, level);
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

        self.record_all_levels(executor.backend());

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
}
