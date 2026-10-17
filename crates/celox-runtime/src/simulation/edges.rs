//! Edges of event signals: marking the domains that scheduled values,
//! process writes, publications and counters trigger.

use celox_design::DomainKind;

use super::round::Round;
use super::{LEVEL_UNKNOWN, SimulationState, edge_level, stable_edge};
use crate::{SignalRef, backend::SimBackend, timing::kind_index};

impl<B: SimBackend> SimulationState<B> {
    /// Rebase edge detection after state was advanced outside this scheduler.
    pub fn synchronize_event_values(&mut self, backend: &B) {
        self.last_clock_values.make_empty();
        self.last_clock_unknown.make_empty();
        self.record_all_levels(backend);
    }

    /// The current level of an event signal, as [`edge_level`] gives it.
    #[inline]
    pub(super) fn level(&self, backend: &B, signal: SignalRef) -> u8 {
        if self.four_state {
            self.level_four_state(backend, signal)
        } else {
            edge_level::<false, _>(backend, signal)
        }
    }

    // The four-state paths stay out of line, so that a two-state design's
    // callers keep the size and inlining they had before.
    #[inline(never)]
    fn level_four_state(&self, backend: &B, signal: SignalRef) -> u8 {
        edge_level::<true, _>(backend, signal)
    }

    /// The level of event `id` when edge detection last sampled it.
    #[inline]
    pub(super) fn last_level_in<const FOUR_STATE: bool>(&self, id: usize) -> u8 {
        let value = u8::from(self.last_clock_values.contains(id));
        if FOUR_STATE && self.last_clock_unknown.contains(id) {
            value | LEVEL_UNKNOWN
        } else {
            value
        }
    }

    #[inline]
    pub(super) fn last_level(&self, id: usize) -> u8 {
        if self.four_state {
            self.last_level_four_state(id)
        } else {
            self.last_level_in::<false>(id)
        }
    }

    #[inline(never)]
    fn last_level_four_state(&self, id: usize) -> u8 {
        self.last_level_in::<true>(id)
    }

    /// Make `level` the baseline of event `id`'s next edge.
    #[inline]
    fn record_level_in<const FOUR_STATE: bool>(&mut self, id: usize, level: u8) {
        if level & 1 != 0 {
            self.last_clock_values.insert(id);
        } else {
            self.last_clock_values.remove(id);
        }
        if FOUR_STATE {
            if level & LEVEL_UNKNOWN != 0 {
                self.last_clock_unknown.insert(id);
            } else {
                self.last_clock_unknown.remove(id);
            }
        }
    }

    #[inline]
    pub(super) fn record_level(&mut self, id: usize, level: u8) {
        if self.four_state {
            self.record_level_four_state(id, level);
        } else {
            self.record_level_in::<false>(id, level);
        }
    }

    #[inline(never)]
    fn record_level_four_state(&mut self, id: usize, level: u8) {
        self.record_level_in::<true>(id, level);
    }

    /// Make every event's current level the baseline of its next edge.
    #[inline(always)]
    pub(super) fn record_all_levels(&mut self, backend: &B) {
        if self.four_state {
            self.record_levels_four_state(backend);
        } else {
            self.record_levels::<false>(backend);
        }
    }

    #[inline(never)]
    fn record_levels_four_state(&mut self, backend: &B) {
        self.record_levels::<true>(backend);
    }

    #[inline(always)]
    fn record_levels<const FOUR_STATE: bool>(&mut self, backend: &B) {
        for index in 0..self.topo_signals.len() {
            let (signal, id) = self.topo_signals[index];
            if id == usize::MAX {
                continue;
            }
            let level = edge_level::<FOUR_STATE, _>(backend, signal);
            self.record_level_in::<FOUR_STATE>(id, level);
        }
    }

    /// Whether a scheduled or process-driven change of event `id` from level
    /// `was` to `is` triggers its domain. A domain of unknown kind reacts
    /// to the rising edge.
    #[inline]
    pub(super) fn scheduled_edge(&self, id: usize, was: u8, is: u8) -> bool {
        let kind = match self.domain_kinds[id] {
            kind @ Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => kind,
            _ => Some(DomainKind::ClockPosedge),
        };
        if self.four_state {
            stable_edge::<true>(kind, was, is)
        } else {
            stable_edge::<false>(kind, was, is)
        }
    }

    /// Mark the domains whose counters moved since they last triggered.
    pub(super) fn mark_counted_domains(&mut self, backend: &mut B, round: &mut Round) {
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
    pub(super) fn mark_scheduled_edges(
        &self,
        backend: &mut B,
        round: &mut Round,
        values: &[(usize, u8)],
    ) {
        for &(id, is) in values {
            round.track_stable_edges = true;
            // A signal written again compares with its preceding level.
            let was = round.previous(id, is, || self.last_level(id));
            if was != is
                && self
                    .combinational_derived
                    .get(id)
                    .is_some_and(|derived| !derived.is_empty())
            {
                round.gating_sources.push(id);
            }
            if self.scheduled_edge(id, was, is) {
                round.scheduled_trigger_ids.insert(id);
                backend.mark_triggered_bit(id);
            }
        }
    }

    /// The level (see [`edge_level`]) of every event signal.
    pub(super) fn event_values(&self, backend: &B, values: &mut Vec<u8>) {
        values.clear();
        values.extend(
            self.topo_signals
                .iter()
                .map(|(signal, _)| self.level(backend, *signal)),
        );
    }

    /// Record the event signals that changed since `round.before`.
    pub(super) fn record_transitions(&self, backend: &B, round: &mut Round) {
        self.event_values(backend, &mut round.after);
        for (((_, id), &old), &new) in self
            .topo_signals
            .iter()
            .zip(&round.before)
            .zip(&round.after)
        {
            if *id != usize::MAX && old != new {
                round.transitions.push((*id, new));
            }
        }
        std::mem::swap(&mut round.before, &mut round.after);
    }

    /// Mark the domains that edges among `transitions` trigger, even if the
    /// signals end at their old values, and keep them marked through the
    /// settles of this round.
    pub(super) fn mark_occurrences(
        &self,
        backend: &mut B,
        round: &mut Round,
        transitions: &[(usize, u8)],
    ) {
        for &(id, is) in transitions {
            let was = round.previous(id, is, || self.last_level(id));
            let kind = self.domain_kinds[id];
            let triggered = if self.four_state {
                stable_edge::<true>(kind, was, is)
            } else {
                stable_edge::<false>(kind, was, is)
            };
            if triggered {
                round.occurrences.insert(id);
                round.scheduled_trigger_ids.insert(id);
                round.track_stable_edges = true;
                backend.mark_triggered_bit(id);
            }
        }
    }
}
