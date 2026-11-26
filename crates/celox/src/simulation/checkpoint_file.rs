//! Persistence of core execution state. Effect-handler state belongs to the host.
use super::{Simulation, SimulationCheckpoint};
use crate::{CheckpointError, SimBackend, simulator::checkpoint::PersistedCheckpoint};
use celox_runtime::PersistedSimulationSnapshot;
use serde::{Deserialize, Serialize};

const MAGIC: &[u8; 8] = b"CELOXCP1";
// Bump whenever a serialized core-state schema or its interpretation changes.
// Version 7 keeps the private slots of an activation as the bytes of their
// planes. Version 8 keeps the cycle of a randc property of a heap object as
// its entries, those left first, with the position of each and how many are
// left, and the entries its constraints last allowed. Version 9 keys the
// heap's container lookup by the digest of each container's contents.
// Version 10 keeps the covergroup types, bins, options and hits of a heap
// after its other fields. Version 11 keeps the bin arrays, `default`, ignore,
// illegal and wildcard bins of a covergroup with the sets and filters of its
// constructions. Version 12 keeps, with what decides the bins of a
// construction of a covergroup, the strings its `with` filters read, and
// with a filter whether its constructions share their bins. Version 13 keeps
// the crosses of a covergroup: their bins, their options and the hits of the
// automatic cross bins that were sampled.
const VERSION: u32 = 13;

#[derive(Debug, thiserror::Error)]
pub enum CheckpointFileError {
    #[error("invalid or incompatible checkpoint container")]
    InvalidContainer,
    #[error("checkpoint encoding failed: {0}")]
    Encoding(#[from] postcard::Error),
    #[error(transparent)]
    Checkpoint(#[from] CheckpointError),
}

#[derive(Serialize, Deserialize)]
struct CoreState {
    version: u32,
    release: String,
    architecture: String,
    little_endian: bool,
    simulator: PersistedCheckpoint,
    schedule: PersistedSimulationSnapshot,
    host_writes: Vec<(crate::ir::SignalRef, celox_runtime::timing::Value)>,
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

impl<B: SimBackend> SimulationCheckpoint<B> {
    /// Encode core execution state, including live fork activations and heap
    /// objects. No effect-handler state or machine-code addresses are included.
    /// This is a versioned implementation format, not a portable StateFile.
    pub fn to_bytes(&self) -> Result<Vec<u8>, CheckpointFileError> {
        let payload = postcard::to_allocvec(&CoreState {
            version: VERSION,
            release: env!("CARGO_PKG_VERSION").into(),
            architecture: std::env::consts::ARCH.into(),
            little_endian: cfg!(target_endian = "little"),
            simulator: self.simulator.persisted(),
            schedule: self.schedule.persisted(),
            host_writes: self.host_writes.clone(),
        })?;
        let mut bytes = Vec::with_capacity(24 + payload.len());
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&checksum(&payload).to_le_bytes());
        bytes.extend(payload);
        Ok(bytes)
    }
}

impl<B: SimBackend> Simulation<B> {
    /// Decode a checkpoint and bind its event IDs to this compiled instance.
    /// This does not change the simulation or any effect handler.
    ///
    /// # Safety
    /// `bytes` must be an unmodified result of `SimulationCheckpoint::to_bytes`
    /// from a compatible Celox build. The container checks version, layout and
    /// accidental corruption; it does not authenticate arbitrary memory offsets
    /// or process state supplied by an attacker. Never load untrusted checkpoints.
    pub unsafe fn checkpoint_from_bytes(
        &self,
        bytes: &[u8],
    ) -> Result<SimulationCheckpoint<B>, CheckpointFileError> {
        if bytes.len() < 24 || &bytes[..8] != MAGIC {
            return Err(CheckpointFileError::InvalidContainer);
        }
        let size = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let hash = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let payload = &bytes[24..];
        if size != payload.len() as u64 || hash != checksum(payload) {
            return Err(CheckpointFileError::InvalidContainer);
        }
        let (state, remainder): (CoreState, _) = postcard::take_from_bytes(payload)?;
        if !remainder.is_empty()
            || state.version != VERSION
            || state.release != env!("CARGO_PKG_VERSION")
            || state.architecture != std::env::consts::ARCH
            || state.little_endian != cfg!(target_endian = "little")
        {
            return Err(CheckpointFileError::InvalidContainer);
        }
        Ok(SimulationCheckpoint {
            simulator: state.simulator.bind(&self.simulator)?,
            // SAFETY: the caller guarantees a trusted checkpoint and bind above
            // checked that its core layout belongs to this design.
            schedule: unsafe {
                state
                    .schedule
                    .bind::<B>(self.simulator.backend.id_to_event_slice())
            }
            .ok_or(CheckpointError::DesignMismatch)?,
            host_writes: state.host_writes,
        })
    }
}
