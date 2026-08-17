//! Runtime-only loading and instantiation of compiler-produced native images.

use std::sync::Arc;

use celox_design::{DomainKind, StateAddr};
use celox_runtime::backend::EventHandle;
use celox_runtime::{DesignReflection, ReflectionSignal, SignalRef};
use num_bigint::BigUint;

use crate::{
    NativeBackend, RuntimeEvent, RuntimeFormatContext, SharedNativeCode, SimBackend, SimulatorError,
};

use super::backend::NativeRuntimeSchema;
use super::{NativeImageContainerError, NativeProgramImage};

/// Failure while discovering or attaching a compiler-produced native image.
#[derive(Debug, thiserror::Error)]
pub enum NativeProgramLoadError {
    #[error(transparent)]
    Container(#[from] NativeImageContainerError),
    #[error("no native program image is attached to the runtime")]
    MissingImage,
    #[error("failed to attach native machine code: {0}")]
    Attach(#[source] SimulatorError),
    #[error("failed to initialize native program state: {0}")]
    Initialize(#[source] celox_runtime::SimulatorErrorCode),
}

/// Source-independent identity shared by reflected handles for one signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NativeSignalIdentity {
    /// Ordinary state aliases share their final native-memory location.
    State(SignalRef),
    /// Clock and reset aliases share their canonical event-domain address.
    Event(StateAddr),
}

/// One independently mutable instance of a precompiled native program.
///
/// Construction needs only a serialized [`NativeProgramImage`]. Source text,
/// frontend lookup tables, SIR, and compiler layout artifacts are not retained.
pub struct NativeProgramInstance {
    shared: Arc<SharedNativeCode>,
    backend: NativeBackend,
    runtime_event_read_seq: u64,
    comb_observer_snapshots: Vec<Vec<(BigUint, BigUint)>>,
    comb_observer_initial_eval: bool,
    forced_values: crate::HashMap<NativeSignalIdentity, (BigUint, BigUint)>,
}

impl NativeProgramInstance {
    /// Attach an already-decoded program image and allocate fresh state.
    pub fn from_image(image: NativeProgramImage) -> Result<Self, NativeProgramLoadError> {
        let shared =
            Arc::new(SharedNativeCode::from_image(image).map_err(NativeProgramLoadError::Attach)?);
        let backend = NativeBackend::from_shared(Arc::clone(&shared));
        let mut instance = Self {
            shared,
            backend,
            runtime_event_read_seq: 0,
            comb_observer_snapshots: Vec::new(),
            comb_observer_initial_eval: true,
            forced_values: crate::HashMap::default(),
        };
        instance.comb_observer_snapshots = instance.snapshot_all_comb_observers();
        instance
            .eval_comb_checked()
            .map_err(NativeProgramLoadError::Initialize)?;
        Ok(instance)
    }

    /// Discover a program image appended to arbitrary runtime bytes.
    pub fn from_attached_bytes(bytes: &[u8]) -> Result<Self, NativeProgramLoadError> {
        let appended = NativeProgramImage::discover_appended(bytes)?
            .ok_or(NativeProgramLoadError::MissingImage)?;
        Self::from_image(appended.image)
    }

    /// Discover the program image appended to the running executable.
    pub fn from_current_executable() -> Result<Self, NativeProgramLoadError> {
        let appended = NativeProgramImage::discover_in_current_executable()?
            .ok_or(NativeProgramLoadError::MissingImage)?;
        Self::from_image(appended.image)
    }

    /// Elaborated hierarchy and signal metadata embedded by the compiler.
    pub fn reflection(&self) -> &DesignReflection {
        self.shared.program_image().reflection()
    }

    /// Resolve a fully qualified signal name such as `Top.clock`.
    pub fn signal(&self, full_name: &str) -> Option<&ReflectionSignal> {
        self.reflection()
            .signal_by_name(full_name)
            .map(|(_, signal)| signal)
    }

    /// Resolve only the compact state handle for a fully qualified signal.
    pub fn signal_ref(&self, full_name: &str) -> Option<SignalRef> {
        self.signal(full_name).map(|signal| signal.signal)
    }

    /// Resolve the canonical state/event identity behind a reflected handle.
    pub fn signal_identity(
        &self,
        id: celox_runtime::ReflectionSignalId,
    ) -> Option<NativeSignalIdentity> {
        let signal = self.reflection().signal(id)?;
        Some(if signal.domain_kind == DomainKind::Other {
            NativeSignalIdentity::State(signal.signal)
        } else {
            NativeSignalIdentity::Event(
                self.shared
                    .program_image()
                    .event_topology()
                    .canonical(signal.state_address),
            )
        })
    }

    /// Execute combinational logic after one or more foreign writes.
    pub fn eval_comb(&mut self) -> Result<(), celox_runtime::SimulatorErrorCode> {
        self.eval_comb_checked()
    }

    /// Settle foreign writes and commit all active source domains together.
    ///
    /// Callers apply the raw signal values first and pass the addresses whose
    /// configured edge became active. Multiple domains use split eval/apply
    /// entries so no domain can observe another domain's same-time commit.
    pub fn settle_active_edges(
        &mut self,
        active_edges: &[StateAddr],
    ) -> Result<(), celox_runtime::SimulatorErrorCode> {
        let start_seq = (!self.runtime_schema().runtime_event_sites.is_empty())
            .then(|| crate::simulator::runtime_event_write_seq_for_backend(&self.backend));
        self.eval_comb_checked()?;
        let mut seen = crate::HashSet::default();
        let mut events = Vec::new();
        for address in active_edges {
            let canonical = self
                .shared
                .program_image()
                .event_topology()
                .canonical(*address);
            let Some(event) = self.backend.resolve_event_opt(&canonical) else {
                continue;
            };
            if seen.insert(event.id()) {
                events.push(event);
            }
        }

        if events.len() == 1 {
            self.backend
                .eval_apply_ff_at(events[0])
                .map_err(|error| self.decorate_runtime_error(error))?;
        } else if !events.is_empty() {
            let split = events
                .iter()
                .map(|event| {
                    Some((
                        self.backend.resolve_eval_only_event(&event.addr())?,
                        self.backend.resolve_apply_event(&event.addr())?,
                    ))
                })
                .collect::<Option<Vec<_>>>();
            if let Some(split) = split {
                for (evaluate, _) in &split {
                    self.backend
                        .eval_only_ff_at(*evaluate)
                        .map_err(|error| self.decorate_runtime_error(error))?;
                }
                for (_, apply) in &split {
                    self.backend
                        .apply_ff_at(*apply)
                        .map_err(|error| self.decorate_runtime_error(error))?;
                }
            } else {
                for event in events {
                    self.backend
                        .eval_apply_ff_at(event)
                        .map_err(|error| self.decorate_runtime_error(error))?;
                }
            }
        }
        self.eval_comb_checked()?;
        if let Some(start_seq) = start_seq {
            self.check_fatal_events_since(start_seq)?;
        }
        Ok(())
    }

    /// Drain source-independent `$display` and assertion records emitted by
    /// generated code since the preceding call.
    pub fn drain_runtime_events(&mut self) -> Vec<RuntimeEvent> {
        crate::simulator::collect_runtime_events_for_backend(
            &self.backend,
            &self
                .shared
                .program_image()
                .runtime_schema()
                .runtime_event_sites,
            &mut self.runtime_event_read_seq,
            RuntimeFormatContext::default(),
        )
    }

    /// Direct access used by foreign-interface adapters for value and event
    /// operations. The compiler is not involved in these calls.
    pub fn backend(&self) -> &NativeBackend {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut NativeBackend {
        &mut self.backend
    }

    /// Override one reflected signal at each combinational execution-unit
    /// boundary until [`Self::release_signal`] is called.
    pub fn force_signal(
        &mut self,
        id: celox_runtime::ReflectionSignalId,
        value: BigUint,
        mask: BigUint,
    ) -> bool {
        let Some(identity) = self.signal_identity(id) else {
            return false;
        };
        let aliases = self.signal_refs_for_identity(identity);
        for signal in aliases {
            self.backend
                .set_four_state(signal, value.clone(), mask.clone());
        }
        self.forced_values.insert(identity, (value, mask));
        true
    }

    /// Restore normal design-driver control for a reflected signal.
    pub fn release_signal(&mut self, id: celox_runtime::ReflectionSignalId) {
        if let Some(identity) = self.signal_identity(id) {
            self.forced_values.remove(&identity);
        }
    }

    fn signal_refs_for_identity(&self, identity: NativeSignalIdentity) -> Vec<SignalRef> {
        let mut signals = self
            .reflection()
            .signals()
            .iter()
            .enumerate()
            .filter_map(|(index, signal)| {
                (self.signal_identity(celox_runtime::ReflectionSignalId(index as u32))
                    == Some(identity))
                .then_some(signal.signal)
            })
            .collect::<Vec<_>>();
        signals.sort_unstable();
        signals.dedup();
        signals
    }

    fn runtime_schema(&self) -> &NativeRuntimeSchema {
        self.shared.program_image().runtime_schema()
    }

    fn decorate_runtime_error(
        &self,
        error: celox_runtime::SimulatorErrorCode,
    ) -> celox_runtime::SimulatorErrorCode {
        let celox_runtime::SimulatorErrorCode::DetectedTrueLoopCode(code) = error else {
            return error;
        };
        let Some(info) = self.runtime_schema().runtime_errors.get(&code) else {
            return celox_runtime::SimulatorErrorCode::DetectedTrueLoop;
        };
        let signals = info
            .signals
            .iter()
            .filter_map(|address| {
                self.reflection()
                    .signals()
                    .iter()
                    .find(|signal| signal.state_address == *address)
                    .map(|signal| signal.full_name.clone())
            })
            .collect::<Vec<_>>();
        if info.message == "Detected True Loop" {
            celox_runtime::SimulatorErrorCode::DetectedTrueLoopAt { signals }
        } else {
            celox_runtime::SimulatorErrorCode::Runtime {
                message: info.message.clone(),
                signals,
            }
        }
    }

    fn eval_comb_checked(&mut self) -> Result<(), celox_runtime::SimulatorErrorCode> {
        if self.runtime_schema().runtime_event_sites.is_empty() {
            return self.eval_comb_backend();
        }

        let start_seq = crate::simulator::runtime_event_write_seq_for_backend(&self.backend);
        if self.runtime_schema().comb_observers.is_empty() {
            let result = self.eval_comb_backend();
            self.check_fatal_events_since(start_seq)?;
            return result;
        }

        let before = self.snapshot_all_comb_observers();
        let active_before = before
            .iter()
            .zip(&self.comb_observer_snapshots)
            .map(|(current, previous)| current != previous)
            .collect::<Vec<_>>();
        let mut active_sites = vec![false; self.runtime_schema().runtime_event_sites.len()];
        for (observer, active) in self
            .runtime_schema()
            .comb_observers
            .iter()
            .zip(active_before)
        {
            if active || self.comb_observer_initial_eval {
                for group_observer in &self.runtime_schema().comb_observers {
                    if group_observer.activation_group == observer.activation_group {
                        active_sites[group_observer.site_id as usize] = true;
                    }
                }
            }
        }
        self.backend.set_comb_capture_event_enabled(&active_sites);
        let result = self.eval_comb_backend();
        let after = self.snapshot_all_comb_observers();
        self.backend.set_comb_capture_event_enabled(&vec![
            false;
            self.runtime_schema()
                .runtime_event_sites
                .len()
        ]);
        self.comb_observer_snapshots = after;
        self.comb_observer_initial_eval = false;
        self.check_fatal_events_since(start_seq)?;
        result
    }

    fn eval_comb_backend(&mut self) -> Result<(), celox_runtime::SimulatorErrorCode> {
        if self.forced_values.is_empty() {
            return self
                .backend
                .eval_comb()
                .map_err(|error| self.decorate_runtime_error(error));
        }
        let overrides = self
            .forced_values
            .iter()
            .flat_map(|(identity, (value, mask))| {
                self.signal_refs_for_identity(*identity)
                    .into_iter()
                    .map(|signal| (signal, value.clone(), mask.clone()))
            })
            .collect::<Vec<_>>();
        for (signal, value, mask) in &overrides {
            self.backend
                .set_four_state(*signal, value.clone(), mask.clone());
        }
        self.backend
            .eval_comb_units_with(|backend| {
                for (signal, value, mask) in &overrides {
                    backend.set_four_state(*signal, value.clone(), mask.clone());
                }
            })
            .map_err(|error| self.decorate_runtime_error(error))
    }

    fn check_fatal_events_since(
        &self,
        start_seq: u64,
    ) -> Result<(), celox_runtime::SimulatorErrorCode> {
        let mut read_seq = start_seq;
        let events = crate::simulator::collect_runtime_events_for_backend(
            &self.backend,
            &self.runtime_schema().runtime_event_sites,
            &mut read_seq,
            RuntimeFormatContext::default(),
        );
        if let Some(message) = events.into_iter().find_map(|event| match event {
            RuntimeEvent::AssertFatal { message } => Some(message),
            RuntimeEvent::Display { .. }
            | RuntimeEvent::AssertContinue { .. }
            | RuntimeEvent::Missed { .. } => None,
        }) {
            return Err(celox_runtime::SimulatorErrorCode::Runtime {
                message,
                signals: Vec::new(),
            });
        }
        Ok(())
    }

    fn snapshot_all_comb_observers(&self) -> Vec<Vec<(BigUint, BigUint)>> {
        self.runtime_schema()
            .comb_observers
            .iter()
            .map(|observer| {
                observer
                    .sensitivity
                    .iter()
                    .map(|atom| {
                        let signal = self.backend.resolve_signal(&atom.id);
                        let (value, mask) = if signal.is_4state {
                            self.backend.get_four_state(signal)
                        } else {
                            (self.backend.get(signal), BigUint::default())
                        };
                        (
                            slice_biguint(&value, atom.access.lsb, atom.access.msb),
                            slice_biguint(&mask, atom.access.lsb, atom.access.msb),
                        )
                    })
                    .collect()
            })
            .collect()
    }
}

fn slice_biguint(value: &BigUint, least: usize, most: usize) -> BigUint {
    if most < least {
        return BigUint::default();
    }
    let width = most - least + 1;
    (value >> least) & ((BigUint::from(1u8) << width) - BigUint::from(1u8))
}
