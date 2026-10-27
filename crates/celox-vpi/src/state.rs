//! The design state behind the VPI entry points: an untimed program
//! instance, whose regions the VPI host schedules itself, or a timed
//! simulation, which runs the design's processes and schedules its regions.

use celox::{
    BigUint, DesignReflection, NativeBackend, NativeProgramImage, NativeProgramInstance,
    NativeSignalIdentity, ReflectionSignalId, RuntimeEvent, RuntimeFormatContext, SignalRef,
    SimBackend, Simulation, SlotIo,
};

/// What the VPI entry points read and write.
pub(crate) trait VpiState {
    /// The image the state belongs to: its reflection and layout.
    fn image(&self) -> &NativeProgramImage;

    fn reflection(&self) -> &DesignReflection {
        self.image().reflection()
    }

    fn time_precision(&self) -> celox_design::TimePrecision {
        self.image().time_precision()
    }

    fn four_state(&self) -> bool {
        self.image().four_state()
    }

    fn signal_identity(&self, id: ReflectionSignalId) -> Option<NativeSignalIdentity> {
        self.image().signal_identity(id)
    }

    fn save_state(&self) -> Result<celox::StateFile, String>;

    /// The stored value, without settling the design.
    fn get_four_state(&self, signal: SignalRef) -> (BigUint, BigUint);

    fn set_four_state(&mut self, signal: SignalRef, value: BigUint, mask: BigUint);

    /// Override a signal until [`Self::release`]; `false` when it cannot be
    /// forced.
    fn force(&mut self, id: ReflectionSignalId, value: BigUint, mask: BigUint) -> bool;

    fn release(&mut self, id: ReflectionSignalId);

    /// Whether the VPI host schedules the settles and edges of its writes,
    /// rather than a timed simulation doing it.
    fn schedules_writes(&self) -> bool;
}

impl VpiState for NativeProgramInstance {
    fn save_state(&self) -> Result<celox::StateFile, String> {
        NativeProgramInstance::save_state(self).map_err(|error| error.to_string())
    }

    fn image(&self) -> &NativeProgramImage {
        self.program_image()
    }

    fn get_four_state(&self, signal: SignalRef) -> (BigUint, BigUint) {
        self.backend().get_four_state(signal)
    }

    fn set_four_state(&mut self, signal: SignalRef, value: BigUint, mask: BigUint) {
        self.backend_mut().set_four_state(signal, value, mask);
    }

    fn force(&mut self, id: ReflectionSignalId, value: BigUint, mask: BigUint) -> bool {
        self.force_signal(id, value, mask)
    }

    fn release(&mut self, id: ReflectionSignalId) {
        self.release_signal(id);
    }

    fn schedules_writes(&self) -> bool {
        true
    }
}

/// A timed simulation of a native image with processes.
///
/// While a step runs, the simulation is out of `simulation`, and the VPI
/// entry points that its slot observer's callbacks call reach the state
/// through `slot`, which the observer sets for the duration of a dispatch.
pub(crate) struct TimedRuntime {
    schema: std::sync::Arc<celox::StateSchema>,
    image: NativeProgramImage,
    pub(crate) simulation: Option<Simulation<NativeBackend>>,
    slot: Option<*mut SlotIo<'static, NativeBackend>>,
    /// A VPI write since the last [`Self::take_wrote`].
    wrote: bool,
}

impl TimedRuntime {
    pub(crate) fn new(image: NativeProgramImage, simulation: Simulation<NativeBackend>) -> Self {
        Self {
            schema: simulation.state_schema(),
            image,
            simulation: Some(simulation),
            slot: None,
            wrote: false,
        }
    }

    /// Whether a VPI write changed state since the last call.
    pub(crate) fn take_wrote(&mut self) -> bool {
        std::mem::take(&mut self.wrote)
    }

    /// Reach the state through `slot` until [`Self::leave_slot`].
    ///
    /// # Safety
    ///
    /// `slot` must stay live and unaliased until [`Self::leave_slot`].
    pub(crate) unsafe fn enter_slot(&mut self, slot: &mut SlotIo<'_, NativeBackend>) {
        let slot: *mut SlotIo<'_, NativeBackend> = slot;
        self.slot = Some(slot.cast());
    }

    pub(crate) fn leave_slot(&mut self) {
        self.slot = None;
    }

    /// The runtime events of the simulation since the last call; none while
    /// a step runs, which drains them when it returns.
    pub(crate) fn drain_runtime_events(&mut self) -> Vec<RuntimeEvent> {
        self.simulation
            .as_mut()
            .map(Simulation::drain_runtime_events)
            .unwrap_or_default()
    }

    fn slot(&self) -> Option<&SlotIo<'static, NativeBackend>> {
        // Safety: `enter_slot`'s caller keeps the slot live until `leave_slot`.
        self.slot.map(|slot| unsafe { &*slot })
    }

    fn write(&mut self, signal: SignalRef, value: BigUint, mask: BigUint) {
        self.wrote = true;
        if let Some(slot) = self.slot {
            // Safety: `enter_slot`'s caller keeps the slot live and unaliased.
            unsafe { &mut *slot }.set_four_state(signal, value, mask);
        } else if let Some(simulation) = &mut self.simulation {
            // A write outside a step is the host's at the current time; the
            // design reports its failures when the next step settles it.
            let _ = simulation.modify(|io| io.set_four_state(signal, value, mask));
        }
    }
}

impl VpiState for TimedRuntime {
    fn save_state(&self) -> Result<celox::StateFile, String> {
        if let Some(slot) = self.slot() {
            slot.capture_state(&self.schema)
                .map_err(|error| error.to_string())
        } else if let Some(simulation) = &self.simulation {
            simulation
                .capture_state()
                .map_err(|error| error.to_string())
        } else {
            Err("VPI runtime is executing; capture at a callback boundary".into())
        }
    }

    fn image(&self) -> &NativeProgramImage {
        &self.image
    }

    fn get_four_state(&self, signal: SignalRef) -> (BigUint, BigUint) {
        if let Some(slot) = self.slot() {
            slot.get_four_state(signal)
        } else if let Some(simulation) = &self.simulation {
            simulation.peek_four_state(signal)
        } else {
            (BigUint::default(), BigUint::default())
        }
    }

    fn set_four_state(&mut self, signal: SignalRef, value: BigUint, mask: BigUint) {
        self.write(signal, value, mask);
    }

    /// A timed simulation forces through the compiler's override slots only:
    /// it re-applies nothing between its units itself.
    fn force(&mut self, id: ReflectionSignalId, value: BigUint, mask: BigUint) -> bool {
        let slots = self.image.force_slots(id);
        let Some(identity) = (!slots.is_empty())
            .then(|| self.image.signal_identity(id))
            .flatten()
        else {
            return false;
        };
        for (enabled, slot) in slots {
            self.write(slot, value.clone(), mask.clone());
            self.write(enabled, 1u8.into(), BigUint::default());
        }
        for signal in self.image.signal_aliases(identity) {
            self.write(signal, value.clone(), mask.clone());
        }
        true
    }

    fn release(&mut self, id: ReflectionSignalId) {
        for (enabled, _) in self.image.force_slots(id) {
            self.write(enabled, 0u8.into(), BigUint::default());
        }
    }

    fn schedules_writes(&self) -> bool {
        false
    }
}

/// The runtime of a VPI session.
#[allow(
    clippy::large_enum_variant,
    reason = "a session has one runtime, which its heap-allocated host owns"
)]
pub(crate) enum Runtime {
    Untimed(NativeProgramInstance),
    Timed(TimedRuntime),
}

impl Runtime {
    pub(crate) fn save_state(&self) -> Result<celox::StateFile, String> {
        self.state().save_state()
    }

    pub(crate) fn state(&self) -> &dyn VpiState {
        match self {
            Runtime::Untimed(instance) => instance,
            Runtime::Timed(timed) => timed,
        }
    }

    pub(crate) fn state_mut(&mut self) -> &mut dyn VpiState {
        match self {
            Runtime::Untimed(instance) => instance,
            Runtime::Timed(timed) => timed,
        }
    }

    /// The runtime events since the last call, formatted at `time`.
    pub(crate) fn drain_runtime_events(&mut self, time: u64) -> Vec<RuntimeEvent> {
        match self {
            Runtime::Untimed(instance) => {
                instance.drain_runtime_events_with_context(RuntimeFormatContext {
                    tb_time: Some(time),
                    scope: None,
                })
            }
            Runtime::Timed(timed) => timed.drain_runtime_events(),
        }
    }
}
