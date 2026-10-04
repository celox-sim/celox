//! Capturing and restoring the state of a running simulation.
//!
//! Between public calls, the stable region of the memory image holds all
//! design state: flip-flop evaluation re-seeds the working region from the
//! stable region, and the sparse, triggered-bit and scratch regions are
//! cleared before control returns to the host. A checkpoint therefore copies
//! only the stable region, which is laid out identically in every tier of a
//! tiered simulation. The state header in front of it holds host pointers and
//! is never copied.

use celox_state_layout::STATE_HEADER_SIZE;
use num_bigint::BigUint;

use super::host::Simulator;
use crate::backend::SimBackend;

/// The design state of a memory image: the stable region after the state
/// header, tagged with the layout fingerprint it belongs to.
///
/// This is the backend-level building block of [`Checkpoint`]. Hosts that
/// drive a bare backend use it directly.
#[derive(Clone)]
pub struct StateImage {
    fingerprint: u64,
    bytes: Box<[u8]>,
}

impl StateImage {
    /// Copy the design state out of `backend`, whose layout has `fingerprint`
    /// (see [`Simulator::state_fingerprint`]).
    pub fn capture<B: SimBackend>(backend: &B, fingerprint: u64) -> Self {
        let range = state_range(backend);
        let (ptr, len) = backend.memory_as_ptr();
        assert!(range.end <= len, "stable region exceeds the memory image");
        // Safety: the backend exposes `len` readable bytes at `ptr`.
        let memory = unsafe { std::slice::from_raw_parts(ptr, len) };
        Self {
            fingerprint,
            bytes: memory[range].into(),
        }
    }

    /// Whether this image can be written into a backend whose layout has
    /// `fingerprint`.
    pub fn matches(&self, fingerprint: u64) -> bool {
        self.fingerprint == fingerprint
    }

    /// Write the design state into `backend`, whose layout has `fingerprint`.
    /// The caller must re-evaluate combinational logic afterwards, and have a
    /// VCD writer rescan, since the write marks no VCD activity.
    pub fn restore_into<B: SimBackend>(
        &self,
        backend: &mut B,
        fingerprint: u64,
    ) -> Result<(), CheckpointError> {
        if !self.matches(fingerprint) {
            return Err(CheckpointError::DesignMismatch);
        }
        let range = state_range(backend);
        assert_eq!(range.len(), self.bytes.len());
        backend.write_memory(range.start, &self.bytes);
        Ok(())
    }

    /// Size of the saved design state in bytes.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl std::fmt::Debug for StateImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateImage")
            .field("len", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

fn state_range<B: SimBackend>(backend: &B) -> std::ops::Range<usize> {
    STATE_HEADER_SIZE..backend.layout().total_size
}

/// Saved state of a [`Simulator`], created by [`Simulator::checkpoint`].
///
/// A checkpoint can be restored any number of times, into the simulator that
/// created it or into another simulator built from the same design.
#[derive(Clone)]
pub struct Checkpoint {
    image: StateImage,
    comb_observer_snapshots: Vec<Vec<(BigUint, BigUint)>>,
    comb_observer_initial_eval: bool,
}

impl Checkpoint {
    /// Size of the saved design state in bytes.
    pub fn state_size(&self) -> usize {
        self.image.len()
    }
}

impl std::fmt::Debug for Checkpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Checkpoint")
            .field("state_size", &self.state_size())
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
    #[error("the checkpoint was taken from a different design")]
    DesignMismatch,
    /// A time-based simulation dumps VCD output at every step, so it cannot
    /// return to a time its VCD file has already passed.
    #[error(
        "returning to time {time} would rewind the VCD output, which reached time {last_dumped}; \
         switch to a new VCD file first"
    )]
    VcdRewind { time: u64, last_dumped: u64 },
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
        Ok(Checkpoint {
            image: StateImage::capture(&self.backend, self.state_fingerprint()),
            comb_observer_snapshots: self.comb_observer_snapshots.clone(),
            comb_observer_initial_eval: self.comb_observer_initial_eval,
        })
    }

    /// Return to the state saved in `checkpoint`.
    ///
    /// Runtime events (`$display`, assertions) emitted after the checkpoint
    /// are not withdrawn. An attached VCD writer keeps its file and records
    /// the restored values as changes at the next dump; to record a rewound
    /// simulation, continue in a new file with [`Self::switch_vcd`].
    pub fn restore(&mut self, checkpoint: &Checkpoint) -> Result<(), CheckpointError> {
        self.validate_restore(checkpoint)?;
        let fingerprint = self.state_fingerprint();
        checkpoint
            .image
            .restore_into(&mut self.backend, fingerprint)?;
        self.comb_observer_snapshots
            .clone_from(&checkpoint.comb_observer_snapshots);
        self.comb_observer_initial_eval = checkpoint.comb_observer_initial_eval;
        if let Some(writer) = &mut self.vcd_writer {
            writer.rescan();
        }
        self.dirty = true;
        self.settle_dirty_for_runtime_event_drain();
        Ok(())
    }

    /// Check every precondition of [`Self::restore`] without changing state.
    pub(crate) fn validate_restore(&self, checkpoint: &Checkpoint) -> Result<(), CheckpointError> {
        if !self.components.is_empty() {
            return Err(CheckpointError::ExternalComponents);
        }
        if !checkpoint.image.matches(self.state_fingerprint()) {
            return Err(CheckpointError::DesignMismatch);
        }
        Ok(())
    }

    /// Identity of the checkpointable state layout. Simulators with equal
    /// fingerprints can exchange checkpoints.
    pub fn state_fingerprint(&self) -> u64 {
        *self
            .checkpoint_fingerprint
            .get_or_init(|| crate::ir::state_fingerprint(self.backend.layout(), &self.program))
    }
}
