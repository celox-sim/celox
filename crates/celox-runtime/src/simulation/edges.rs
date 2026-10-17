//! Edges of event signals: marking the domains that scheduled values,
//! process writes, publications and counters trigger.

use celox_design::DomainKind;

use super::round::Round;
use super::{SimulationState, event_level};
use crate::{backend::SimBackend, timing::kind_index};

impl<B: SimBackend> SimulationState<B> {
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
        values: &[(usize, bool)],
    ) {
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

    pub(super) fn event_values(&self, backend: &B, values: &mut Vec<u8>) {
        values.clear();
        values.extend(
            self.topo_signals
                .iter()
                .map(|(signal, _, _)| event_level(backend, *signal)),
        );
    }

    /// Record the event signals that changed since `round.before`.
    pub(super) fn record_transitions(&self, backend: &B, round: &mut Round) {
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

    /// Mark the domains that edges among `transitions` trigger, even if the
    /// signals end at their old values, and keep them marked through the
    /// settles of this round.
    pub(super) fn mark_occurrences(
        &self,
        backend: &mut B,
        round: &mut Round,
        transitions: &[(usize, bool)],
    ) {
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
}
