//! Collecting the heap that handle slots in state memory refer to.

use std::sync::Arc;

use celox_heap::{HANDLE_BYTES, Handle};

use super::host::Simulator;
use crate::backend::SimBackend;

impl<B: SimBackend> Simulator<B> {
    /// Byte offsets of every handle slot in the stable and working regions.
    pub(crate) fn handle_slots(&self) -> Arc<[usize]> {
        self.handle_slots
            .get_or_init(|| {
                let layout = self.backend.layout();
                let mut slots = Vec::new();
                for (address, metadata) in &self.program.design.state_objects {
                    if !metadata.value_kind.is_handle() {
                        continue;
                    }
                    debug_assert!(
                        !layout.sparse_offsets.contains_key(address),
                        "handle slots are never sparse FF targets"
                    );
                    let elements = metadata.width / celox_design::HANDLE_WIDTH;
                    let bases = layout.offsets.get(address).copied().into_iter().chain(
                        layout
                            .working_offsets
                            .get(address)
                            .map(|offset| layout.working_base_offset + offset),
                    );
                    for base in bases {
                        slots.extend((0..elements).map(|index| base + index * HANDLE_BYTES));
                    }
                }
                slots.sort_unstable();
                slots.into()
            })
            .clone()
    }

    /// The handles held by the handle slots of state memory.
    pub(crate) fn handle_roots(&self) -> Vec<Handle> {
        let (memory, len) = self.backend.memory_as_ptr();
        self.handle_slots()
            .iter()
            .filter(|&&offset| offset + HANDLE_BYTES <= len)
            .map(|&offset| {
                let mut bytes = [0; HANDLE_BYTES];
                // SAFETY: the slot lies inside the `len` bytes of the image.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        memory.add(offset),
                        bytes.as_mut_ptr(),
                        HANDLE_BYTES,
                    );
                }
                Handle::from_le_bytes(bytes)
            })
            .collect()
    }

    /// Collect the heap if enough was allocated since the last collection.
    /// State memory and `extra_roots` must hold every live handle: the
    /// caller runs this only where no copy of a handle is anywhere else.
    pub(crate) fn maybe_collect_heap(&mut self, extra_roots: impl IntoIterator<Item = Handle>) {
        if !self.heap.should_collect() {
            return;
        }
        let roots = self.handle_roots();
        self.heap.collect(roots.into_iter().chain(extra_roots));
    }
}
