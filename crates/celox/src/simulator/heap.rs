//! Collecting the heap that handle slots in state memory refer to.

use std::sync::Arc;

use celox_heap::{HANDLE_BYTES, Handle, HandleKind, Root};

use super::host::Simulator;
use crate::backend::SimBackend;

impl<B: SimBackend> Simulator<B> {
    /// Byte offsets of every handle slot in the stable and working regions,
    /// with what its handles refer to, sorted by offset.
    pub(crate) fn handle_slots(&self) -> Arc<[(usize, HandleKind)]> {
        self.handle_slots
            .get_or_init(|| {
                let layout = self.backend.layout();
                let mut slots = Vec::new();
                for (address, metadata) in &self.program.design.state_objects {
                    let kind = match metadata.value_kind {
                        celox_design::ValueKind::Bits => continue,
                        celox_design::ValueKind::StrRef => HandleKind::String,
                        celox_design::ValueKind::ObjRef => HandleKind::Object,
                    };
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
                        slots
                            .extend((0..elements).map(|index| (base + index * HANDLE_BYTES, kind)));
                    }
                }
                slots.sort_unstable_by_key(|&(offset, _)| offset);
                slots.into()
            })
            .clone()
    }

    /// The handles held by the handle slots of state memory.
    pub(crate) fn handle_roots(&self) -> Vec<Root> {
        let (memory, len) = self.backend.memory_as_ptr();
        self.handle_slots()
            .iter()
            .filter(|&&(offset, _)| offset + HANDLE_BYTES <= len)
            .map(|&(offset, kind)| {
                let mut bytes = [0; HANDLE_BYTES];
                // SAFETY: the slot lies inside the `len` bytes of the image.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        memory.add(offset),
                        bytes.as_mut_ptr(),
                        HANDLE_BYTES,
                    );
                }
                Root {
                    kind,
                    handle: Handle::from_le_bytes(bytes),
                }
            })
            .collect()
    }

    /// The fault a heap operation of compiled code recorded since the last
    /// check, as a runtime error.
    pub(crate) fn take_heap_fault(&mut self) -> Result<(), crate::RuntimeErrorCode> {
        match self.backend.heap_mut().take_fault() {
            None => Ok(()),
            Some(fault) => Err(crate::RuntimeErrorCode::Runtime {
                message: fault.to_string(),
                signals: Vec::new(),
            }),
        }
    }

    /// Collect the heap between time slots, if enough was allocated. Runtime
    /// events still in the backend's buffer are taken first, which reads
    /// their string arguments.
    pub(crate) fn heap_safepoint(&mut self) {
        if self.heap_safepoints_blocked || !self.backend.heap().should_collect() {
            return;
        }
        self.stash_runtime_events();
        self.maybe_collect_heap([]);
    }

    /// Collect the heap if enough was allocated since the last collection.
    /// State memory and `extra_roots` must hold every live handle: the
    /// caller runs this only where no copy of a handle is anywhere else.
    pub(crate) fn maybe_collect_heap(&mut self, extra_roots: impl IntoIterator<Item = Root>) {
        if !self.backend.heap().should_collect() {
            return;
        }
        let roots = self.handle_roots();
        self.backend.heap_mut().collect(
            roots
                .into_iter()
                .chain(self.process_heap_roots.iter().copied())
                .chain(extra_roots),
        );
    }
}
