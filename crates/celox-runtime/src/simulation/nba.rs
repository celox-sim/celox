//! Nonblocking assignments of process kernels and the NBA region.

use num_bigint::BigUint;

use super::SimulationState;
use super::host::SimulationExecutor;
use super::round::Round;
use crate::{
    SignalRef,
    backend::SimBackend,
    timing::{Halt, NbaSiteRefs, Value, read},
};

impl<B: SimBackend> SimulationState<B> {
    /// Move the value staged at nonblocking site `site` of `process` into
    /// the ordered queue.
    pub(super) fn queue_nba(
        &mut self,
        backend: &mut B,
        process: usize,
        site: usize,
    ) -> Result<(), Halt> {
        let refs = self.model.processes[process]
            .nba_sites
            .get(site)
            .ok_or_else(|| Self::site_error(process, site))?;
        if let Some(pending) = refs.pending {
            // The flag is set: the kernel reached the site again before the
            // NBA region and returned before staging over the earlier value.
            backend.set(pending, 0u8);
        }
        let Some((lsb, width)) = refs.range(backend) else {
            return Ok(());
        };
        // A program process's update waits for the Re-NBA region (IEEE
        // 1800-2023 24.3.1).
        let queue = if self.model.processes[process].program.is_some() {
            &mut self.process_state.re_nba_queue
        } else {
            &mut self.process_state.nba_queue
        };
        queue.push(NbaUpdate {
            target: refs.target,
            lsb,
            width,
            value: read(backend, refs.value),
        });
        Ok(())
    }

    /// Publish the queued and flagged nonblocking assignments of process
    /// kernels (the NBA region, IEEE 1800-2023 4.5): the queued ones in
    /// issue order, then the flagged ones.
    pub(super) fn publish_nba<E>(&mut self, executor: &mut E, round: &mut Round) -> bool
    where
        E: SimulationExecutor<Backend = B>,
    {
        if round.nba_published {
            return false;
        }
        round.nba_published = true;
        let flagged = if round.kernels_ran && self.model.has_pending_nba_sites {
            let backend = executor.backend();
            self.model
                .processes
                .iter()
                .filter(|process| process.program.is_none())
                .flat_map(|process| &process.nba_sites)
                .filter_map(|site| site.pending)
                .filter(|&pending| backend.get_as::<u8>(pending) != 0)
                .count()
        } else {
            0
        };
        let mut queued = std::mem::take(&mut self.process_state.nba_queue);
        if queued.is_empty() && flagged == 0 {
            self.process_state.nba_queue = queued;
            return false;
        }
        // Several updates may pulse an event signal: each is an update the
        // event domains see, not only the final value.
        let several = queued.len() + flagged > 1;
        if several {
            self.event_values(executor.backend(), &mut round.before);
            round.transitions.clear();
        }
        self.publish_queued(executor, round, &queued, several);
        // Keep the queue's storage for the next time slot.
        queued.clear();
        self.process_state.nba_queue = queued;
        if flagged > 0 {
            self.publish_flagged(executor, round, false, several);
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

    /// Publish the nonblocking assignments of program processes (the Re-NBA
    /// region, IEEE 1800-2023 4.4.2.8): the queued ones in issue order, then
    /// the flagged ones. Like the writes of a process run, each is recorded
    /// in `round.transitions` for the caller to settle. Returns whether any
    /// was published.
    pub(super) fn publish_re_nba<E>(&mut self, executor: &mut E, round: &mut Round) -> bool
    where
        E: SimulationExecutor<Backend = B>,
    {
        let any_flagged = self.model.has_pending_nba_sites && {
            let backend = executor.backend();
            self.model
                .program_processes
                .iter()
                .flat_map(|&process| &self.model.processes[process].nba_sites)
                .filter_map(|site| site.pending)
                .any(|pending| backend.get_as::<u8>(pending) != 0)
        };
        if self.process_state.re_nba_queue.is_empty() && !any_flagged {
            return false;
        }
        self.event_values(executor.backend(), &mut round.before);
        let mut queued = std::mem::take(&mut self.process_state.re_nba_queue);
        self.publish_queued(executor, round, &queued, true);
        // Keep the queue's storage for the next time slot.
        queued.clear();
        self.process_state.re_nba_queue = queued;
        if any_flagged {
            self.publish_flagged(executor, round, true, true);
        }
        true
    }

    /// Publish `queued` in issue order, recording the transitions of each in
    /// `round.transitions` if `record`.
    fn publish_queued<E>(
        &mut self,
        executor: &mut E,
        round: &mut Round,
        queued: &[NbaUpdate],
        record: bool,
    ) where
        E: SimulationExecutor<Backend = B>,
    {
        for update in queued {
            let new = NbaValue::Queued(&update.value);
            self.publish_update(executor.backend_mut(), update, new);
            if record {
                self.record_transitions(executor.backend(), round);
            }
        }
    }

    /// Publish the flagged nonblocking sites of the program processes if
    /// `program`, otherwise of the design's, in declaration order, recording
    /// the transitions of each in `round.transitions` if `record`.
    fn publish_flagged<E>(
        &mut self,
        executor: &mut E,
        round: &mut Round,
        program: bool,
        record: bool,
    ) where
        E: SimulationExecutor<Backend = B>,
    {
        for process in 0..self.model.processes.len() {
            if self.model.processes[process].program.is_some() != program {
                continue;
            }
            for site in 0..self.model.processes[process].nba_sites.len() {
                let backend = executor.backend_mut();
                let Some(pending) = self.model.processes[process].nba_sites[site].pending else {
                    continue;
                };
                if backend.get_as::<u8>(pending) == 0 {
                    continue;
                }
                backend.set(pending, 0u8);
                let site = &self.model.processes[process].nba_sites[site];
                let Some((lsb, width)) = site.range(backend) else {
                    continue;
                };
                let (target, value) = (site.target, site.value);
                let update = NbaUpdate {
                    target,
                    lsb,
                    width,
                    value: Value::default(),
                };
                self.publish_update(backend, &update, NbaValue::Staged(value));
                if record {
                    self.record_transitions(executor.backend(), round);
                }
            }
        }
    }

    /// Write `update`, whose value is `new`, into its target. An update
    /// inside a target of at most 64 bits with one state plane is merged as
    /// `u64`; any other as [`Value`].
    fn publish_update(&mut self, backend: &mut B, update: &NbaUpdate, new: NbaValue<'_>) {
        let target = update.target;
        let narrow = |signal: SignalRef| {
            signal.width <= 64
                && signal.array_layout.is_none()
                && !(self.four_state && signal.is_4state)
        };
        let inside = update.lsb + update.width <= target.width && narrow(target);
        let new = match new {
            NbaValue::Queued(value) if inside => Ok(value.0.iter_u64_digits().next().unwrap_or(0)),
            NbaValue::Staged(value) if inside && narrow(value) => Ok(backend.get_as::<u64>(value)),
            NbaValue::Queued(value) => Err(value.clone()),
            NbaValue::Staged(value) => Err(read(backend, value)),
        };
        match new {
            Ok(new) => {
                let all = low_bits(target.width);
                let old = backend.get_as::<u64>(target) & all;
                let merged = update.merge_narrow(old, new);
                self.write_signal(backend, target, |backend| backend.set(target, merged));
            }
            Err(value) => {
                let merged = NbaUpdate { value, ..*update }.merge_into(&read(backend, target));
                self.write_signal(backend, target, |backend| {
                    if target.is_4state {
                        backend.set_four_state(target, merged.0, merged.1);
                    } else {
                        backend.set_wide(target, merged.0);
                    }
                });
            }
        }
    }
}

impl NbaSiteRefs {
    /// The bit position and the number of bits the site writes now, or
    /// `None` when it writes nothing.
    fn range<B: SimBackend>(&self, backend: &B) -> Option<(usize, usize)> {
        let Some(position) = self.position else {
            return Some((self.lsb, self.width));
        };
        let width = backend.get_as::<u64>(position.width) as usize;
        let offset = backend.get_as::<u64>(position.offset) as usize;
        (width != 0).then(|| (self.lsb + offset, width.min(self.width)))
    }
}

/// The value of a nonblocking update: queued, or staged in a site's value
/// object.
enum NbaValue<'a> {
    Queued(&'a Value),
    Staged(SignalRef),
}

/// The low `width` bits of a `u64`, for `width` up to 64.
fn low_bits(width: usize) -> u64 {
    u64::MAX >> (64 - width)
}

/// The working and sparse working state: the next-state values of
/// evaluated but unpublished flip-flop domains.
pub(super) fn working_image<B: SimBackend>(backend: &B) -> Vec<u8> {
    let layout = backend.layout();
    let range = layout.working_base_offset..layout.triggered_bits_offset;
    let (ptr, len) = backend.memory_as_ptr();
    assert!(range.end <= len, "working state exceeds the memory image");
    // Safety: the backend exposes `len` readable bytes at `ptr`.
    let memory = unsafe { std::slice::from_raw_parts(ptr, len) };
    memory[range].to_vec()
}

pub(super) fn restore_working_image<B: SimBackend>(backend: &mut B, image: &[u8]) {
    let offset = backend.layout().working_base_offset;
    backend.write_memory(offset, image);
}

/// A nonblocking assignment waiting for the NBA region.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NbaUpdate {
    pub target: SignalRef,
    /// Bit position of the assigned range in the target.
    pub lsb: usize,
    pub width: usize,
    pub value: Value,
}

impl NbaUpdate {
    /// Write the update into the target's value.
    pub fn merge_into(&self, target: &Value) -> Value {
        let width = self.target.width;
        let range = ((BigUint::from(1u8) << self.width) - 1u8) << self.lsb;
        let all = (BigUint::from(1u8) << width) - 1u8;
        let keep = &all ^ (&range & &all);
        let place =
            |old: &BigUint, new: &BigUint| ((old & &keep) | ((new << self.lsb) & &range)) & &all;
        (
            place(&target.0, &self.value.0),
            place(&target.1, &self.value.1),
        )
    }

    /// [`Self::merge_into`] for a target of at most 64 bits with one state
    /// plane: the bits of the target's value `old` with `new` written in.
    fn merge_narrow(&self, old: u64, new: u64) -> u64 {
        let all = low_bits(self.target.width);
        let range = (low_bits(self.width) << self.lsb) & all;
        ((old & !range) | ((new << self.lsb) & range)) & all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_merges_match_wide_merges() {
        let patterns = [0u64, u64::MAX, 0xa5a5_5a5a_0f0f_f0f0, 0x8000_0000_0000_0001];
        for target_width in [1, 7, 8, 9, 16, 33, 63, 64] {
            for lsb in 0..target_width {
                for width in 1..=target_width - lsb {
                    for (old, new) in patterns.iter().zip(patterns.iter().rev()) {
                        let all = low_bits(target_width);
                        let update = NbaUpdate {
                            target: SignalRef {
                                offset: 0,
                                width: target_width,
                                is_4state: false,
                                array_layout: None,
                            },
                            lsb,
                            width,
                            value: (BigUint::from(new & low_bits(width)), BigUint::default()),
                        };
                        let wide =
                            update.merge_into(&(BigUint::from(old & all), BigUint::default()));
                        assert_eq!(
                            BigUint::from(update.merge_narrow(old & all, *new)),
                            wide.0,
                            "target {target_width}, lsb {lsb}, width {width}"
                        );
                    }
                }
            }
        }
    }
}
