//! Capturing and restoring the state of a running simulation.
//!
//! Between public calls, the stable region of the memory image holds all
//! design state: flip-flop evaluation re-seeds the working region from the
//! stable region, and the sparse, triggered-bit and scratch regions are
//! cleared before control returns to the host. A checkpoint therefore copies
//! only the stable region, which is laid out identically in every tier of a
//! tiered simulation. The state header in front of it holds host pointers and
//! is never copied.

use std::hash::{Hash, Hasher};

use celox_state_layout::STATE_HEADER_SIZE;
use num_bigint::BigUint;

use super::host::Simulator;
use crate::backend::SimBackend;

/// Saved state of a [`Simulator`], created by [`Simulator::checkpoint`].
///
/// A checkpoint can be restored any number of times, into the simulator that
/// created it or into another simulator built from the same design.
#[derive(Clone)]
pub struct Checkpoint {
    fingerprint: u64,
    state: Box<[u8]>,
    comb_observer_snapshots: Vec<Vec<(BigUint, BigUint)>>,
    comb_observer_initial_eval: bool,
}

impl Checkpoint {
    /// Size of the saved design state in bytes.
    pub fn state_size(&self) -> usize {
        self.state.len()
    }
}

impl std::fmt::Debug for Checkpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Checkpoint")
            .field("state_size", &self.state.len())
            .finish_non_exhaustive()
    }
}

/// Reasons a checkpoint cannot be taken or restored.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckpointError {
    /// External component instances hold opaque state. They currently exist
    /// only while a testbench runs, when no checkpoint can be taken.
    #[error(
        "checkpoints are not supported for designs with external components, whose state Celox cannot capture"
    )]
    ExternalComponents,
    #[error("cannot restore a checkpoint while a VCD writer is attached")]
    VcdAttached,
    #[error("the checkpoint was taken from a different design")]
    DesignMismatch,
}

impl<B: SimBackend> Simulator<B> {
    /// Save the current design state.
    ///
    /// Inputs written since the last evaluation are part of the saved state;
    /// combinational logic is settled again after a restore.
    pub fn checkpoint(&self) -> Result<Checkpoint, CheckpointError> {
        if !self.components.is_empty() {
            return Err(CheckpointError::ExternalComponents);
        }
        let range = self.checkpoint_range();
        let (ptr, len) = self.backend.memory_as_ptr();
        assert!(range.end <= len, "stable region exceeds the memory image");
        // Safety: the backend exposes `len` readable bytes at `ptr`.
        let memory = unsafe { std::slice::from_raw_parts(ptr, len) };
        Ok(Checkpoint {
            fingerprint: self.checkpoint_fingerprint(),
            state: memory[range].into(),
            comb_observer_snapshots: self.comb_observer_snapshots.clone(),
            comb_observer_initial_eval: self.comb_observer_initial_eval,
        })
    }

    /// Return to the state saved in `checkpoint`.
    ///
    /// Runtime events (`$display`, assertions) emitted after the checkpoint
    /// are not withdrawn. Restoring is rejected while a VCD writer is
    /// attached, because the waveform cannot go back in time.
    pub fn restore(&mut self, checkpoint: &Checkpoint) -> Result<(), CheckpointError> {
        self.validate_restore(checkpoint)?;
        let range = self.checkpoint_range();
        assert_eq!(range.len(), checkpoint.state.len());
        let (ptr, len) = self.backend.memory_as_mut_ptr();
        assert!(range.end <= len, "stable region exceeds the memory image");
        // Safety: the backend exposes `len` writable bytes at `ptr`, and the
        // checkpoint buffer is a separate allocation.
        unsafe {
            std::ptr::copy_nonoverlapping(
                checkpoint.state.as_ptr(),
                ptr.add(range.start),
                range.len(),
            );
        }
        self.comb_observer_snapshots
            .clone_from(&checkpoint.comb_observer_snapshots);
        self.comb_observer_initial_eval = checkpoint.comb_observer_initial_eval;
        self.dirty = true;
        self.settle_dirty_for_runtime_event_drain();
        Ok(())
    }

    /// Check every precondition of [`Self::restore`] without changing state.
    pub(crate) fn validate_restore(&self, checkpoint: &Checkpoint) -> Result<(), CheckpointError> {
        if !self.components.is_empty() {
            return Err(CheckpointError::ExternalComponents);
        }
        if self.vcd_writer.is_some() {
            return Err(CheckpointError::VcdAttached);
        }
        if checkpoint.fingerprint != self.checkpoint_fingerprint() {
            return Err(CheckpointError::DesignMismatch);
        }
        Ok(())
    }

    fn checkpoint_range(&self) -> std::ops::Range<usize> {
        STATE_HEADER_SIZE..self.backend.layout().total_size
    }

    /// Identity of the stable-region layout: every state object's path,
    /// offset, width and state kind.
    fn checkpoint_fingerprint(&self) -> u64 {
        *self.checkpoint_fingerprint.get_or_init(|| {
            let layout = self.backend.layout();
            let mut objects: Vec<_> = layout
                .offsets
                .iter()
                .map(|(address, &offset)| {
                    (
                        offset,
                        self.program.get_path(address),
                        layout.widths.get(address).copied(),
                        layout.is_4states.get(address).copied(),
                    )
                })
                .collect();
            objects.sort_unstable();
            let mut hasher = std::hash::DefaultHasher::new();
            layout.total_size.hash(&mut hasher);
            layout.four_state.hash(&mut hasher);
            objects.hash(&mut hasher);
            hasher.finish()
        })
    }
}
