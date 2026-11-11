//! The backend behind a simulator handle.

use super::*;

/// The backend driving a [`NativeSimulatorHandle`].
///
/// Either the default compiled backend (as before) or the tiered backend,
/// which starts on the interpreter and promotes to generated code in the
/// background. Dispatch goes through the shared [`SimBackend`] surface the
/// handle actually uses.
#[cfg(not(target_arch = "wasm32"))]
pub(super) enum HandleBackend {
    Default(celox::DefaultBackend),
    Tiered(Box<celox::TieredBackend>),
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) type DefaultEvent = <celox::DefaultBackend as celox::SimBackend>::Event;

/// Derived clock domains per event, for the backend variant of a handle.
#[cfg(not(target_arch = "wasm32"))]
pub(super) enum HandleDerivedTicks {
    Default(celox::DerivedTickPlans<DefaultEvent>),
    Tiered(celox::DerivedTickPlans<celox::TieredEventRef>),
}

#[cfg(not(target_arch = "wasm32"))]
impl HandleBackend {
    /// Tick an event as `Simulator::tick` does: a pulse of its signal to
    /// the active level, running every domain sensitive to its edges.
    pub(super) fn tick(
        &mut self,
        event_id: usize,
        derived: &HandleDerivedTicks,
        event_edges: &celox::EventEdges,
        scratch: &mut celox::DerivedTickScratch,
    ) -> std::result::Result<(), celox::RuntimeErrorCode> {
        fn tick_with<B: celox::SimBackend>(
            backend: &mut B,
            event_id: usize,
            plans: &celox::DerivedTickPlans<B::Event>,
            event_edges: &celox::EventEdges,
            scratch: &mut celox::DerivedTickScratch,
        ) -> std::result::Result<(), celox::RuntimeErrorCode> {
            let event = backend.id_to_event_slice()[event_id];
            let plan = plans.get(event_id).and_then(Option::as_ref);
            match celox::run_tick(
                &mut celox::BackendExecutor(backend),
                plan,
                event_edges[event_id],
                scratch,
                event,
                true,
                celox::set_level,
            ) {
                Ok(()) => Ok(()),
                // A bare backend reports no `$finish` or `$stop`; what ran
                // settles here.
                Err(celox::Halt::Finish | celox::Halt::Stop) => backend.eval_comb(),
                Err(celox::Halt::Error(error)) => Err(error),
            }
        }
        match (self, derived) {
            (Self::Default(backend), HandleDerivedTicks::Default(plans)) => {
                tick_with(backend, event_id, plans, event_edges, scratch)
            }
            (Self::Tiered(backend), HandleDerivedTicks::Tiered(plans)) => {
                tick_with(backend.as_mut(), event_id, plans, event_edges, scratch)
            }
            _ => unreachable!("derived tick plans belong to the handle's backend"),
        }
    }

    /// Run coincident edges of event signals, as `Simulator::edges` does.
    pub(super) fn edges(
        &mut self,
        moves: &[(usize, celox::Edge)],
        derived: &HandleDerivedTicks,
        event_edges: &celox::EventEdges,
        scratch: &mut celox::DerivedTickScratch,
        dirty: bool,
    ) -> std::result::Result<bool, celox::RuntimeErrorCode> {
        fn edges_with<B: celox::SimBackend>(
            backend: &mut B,
            moves: &[(usize, celox::Edge)],
            plans: &celox::DerivedTickPlans<B::Event>,
            event_edges: &celox::EventEdges,
            scratch: &mut celox::DerivedTickScratch,
            dirty: bool,
        ) -> std::result::Result<bool, celox::RuntimeErrorCode> {
            let moves: Vec<_> = moves
                .iter()
                .map(|&(id, edge)| (backend.id_to_event_slice()[id], edge))
                .collect();
            celox::run_edges(
                &mut celox::BackendExecutor(backend),
                plans,
                event_edges,
                scratch,
                &moves,
                dirty,
                celox::set_level,
            )
            .map_or_else(
                |halt| match halt {
                    // A bare backend reports no `$finish` or `$stop`; settle
                    // next time.
                    celox::Halt::Finish | celox::Halt::Stop => Ok(false),
                    celox::Halt::Error(error) => Err(error),
                },
                |outcome| {
                    Ok(match outcome {
                        celox::EdgeOutcome::Unchanged | celox::EdgeOutcome::Stored => !dirty,
                        celox::EdgeOutcome::Moved => false,
                        celox::EdgeOutcome::Settled => true,
                    })
                },
            )
        }
        match (self, derived) {
            (Self::Default(backend), HandleDerivedTicks::Default(plans)) => {
                edges_with(backend, moves, plans, event_edges, scratch, dirty)
            }
            (Self::Tiered(backend), HandleDerivedTicks::Tiered(plans)) => {
                edges_with(backend.as_mut(), moves, plans, event_edges, scratch, dirty)
            }
            _ => unreachable!("derived tick plans belong to the handle's backend"),
        }
    }

    pub(super) fn event_count(&self) -> usize {
        match self {
            Self::Default(backend) => backend.id_to_event_slice().len(),
            Self::Tiered(backend) => backend.id_to_event_slice().len(),
        }
    }

    pub(super) fn eval_comb(&mut self) -> std::result::Result<(), celox::RuntimeErrorCode> {
        match self {
            Self::Default(backend) => backend.eval_comb(),
            Self::Tiered(backend) => backend.eval_comb(),
        }
    }

    pub(super) fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        match self {
            Self::Default(backend) => backend.memory_as_mut_ptr(),
            Self::Tiered(backend) => backend.memory_as_mut_ptr(),
        }
    }

    pub(super) fn memory_owner(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        match self {
            Self::Default(backend) => backend.memory_owner(),
            Self::Tiered(backend) => backend.memory_owner(),
        }
    }

    pub(super) fn stable_region_size(&self) -> usize {
        match self {
            Self::Default(backend) => backend.stable_region_size(),
            Self::Tiered(backend) => backend.stable_region_size(),
        }
    }

    pub(super) fn capture_state(&self, fingerprint: u64) -> celox::StateImage {
        match self {
            Self::Default(backend) => celox::StateImage::capture(backend, fingerprint),
            Self::Tiered(backend) => celox::StateImage::capture(backend.as_ref(), fingerprint),
        }
    }

    pub(super) fn save_state_file(
        &self,
        schema: &celox::StateSchema,
    ) -> std::result::Result<celox::StateFile, celox::StateError> {
        match self {
            Self::Default(backend) => schema.capture(backend),
            Self::Tiered(backend) => schema.capture(backend.as_ref()),
        }
    }

    pub(super) fn load_state_file(
        &mut self,
        schema: &celox::StateSchema,
        file: &celox::StateFile,
    ) -> std::result::Result<(), celox::StateMismatch> {
        match self {
            Self::Default(backend) => schema.load(backend, file),
            Self::Tiered(backend) => schema.load(backend.as_mut(), file),
        }
    }

    pub(super) fn restore_state(
        &mut self,
        image: &celox::StateImage,
        fingerprint: u64,
    ) -> std::result::Result<(), celox::CheckpointError> {
        match self {
            Self::Default(backend) => image.restore_into(backend, fingerprint),
            Self::Tiered(backend) => image.restore_into(backend.as_mut(), fingerprint),
        }
    }
}
