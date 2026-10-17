//! Nonblocking assignments of process kernels and the NBA region.

use num_bigint::BigUint;

use super::SimulationState;
use super::host::SimulationExecutor;
use super::round::Round;
use crate::{
    SignalRef,
    backend::SimBackend,
    timing::{Halt, Value, read},
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

    /// Publish the queued and flagged nonblocking assignments of process
    /// kernels (the NBA region, IEEE 1800-2023 4.5).
    pub(super) fn publish_nba<E>(&mut self, executor: &mut E, round: &mut Round) -> bool
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
}
