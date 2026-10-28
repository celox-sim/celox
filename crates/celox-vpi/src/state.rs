//! The design state behind the VPI entry points: an untimed program
//! instance, whose regions the VPI host schedules itself, or a timed
//! simulation, which runs the design's processes and schedules its regions.

use celox::{
    BigUint, DesignReflection, NativeBackend, NativeProgramImage, NativeProgramInstance,
    NativeSignalIdentity, ReflectionSignalId, RuntimeEvent, RuntimeFormatContext, SignalRef,
    SimBackend, Simulation, SlotIo,
};

// Called only for packed storage of at most128 bits. Keep the common64-bit
// path in one machine word instead of promoting every scalar operation.
fn packed_word(backend: &NativeBackend, signal: SignalRef) -> BigUint {
    if signal.width <= 64 {
        let mask = u64::MAX
            .checked_shr((64 - signal.width) as u32)
            .unwrap_or(0);
        BigUint::from(u64::from_le(backend.get_as::<u64>(signal)) & mask)
    } else {
        let mask = u128::MAX
            .checked_shr((128 - signal.width) as u32)
            .unwrap_or(0);
        BigUint::from(u128::from_le(backend.get_as::<u128>(signal)) & mask)
    }
}

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
        // Packed scalar reads dominate callback polling. Read their stored
        // word directly; arrays keep the backend's gather/plane layout.
        if signal.array_layout.is_none() && signal.width <= 128 {
            let value = packed_word(self.backend(), signal);
            let unknown = if signal.is_4state && self.program_image().four_state() {
                let mask_signal = SignalRef {
                    offset: signal.offset + signal.width.div_ceil(8),
                    ..signal
                };
                packed_word(self.backend(), mask_signal)
            } else {
                BigUint::default()
            };
            (value, unknown)
        } else {
            self.backend().get_four_state(signal)
        }
    }

    fn set_four_state(&mut self, signal: SignalRef, value: BigUint, mask: BigUint) {
        // The typed setter writes packed words and clears a four-state mask
        // without allocating an intermediate byte vector. Unknown four-state
        // values and arrays retain the backend's general plane writer.
        if signal.array_layout.is_none()
            && signal.width <= 128
            && (mask.bits() == 0 || !signal.is_4state || !self.program_image().four_state())
        {
            let mut words = value.iter_u64_digits();
            let low = words.next().unwrap_or(0);
            if signal.width <= 64 {
                self.backend_mut().set(signal, low.to_le());
            } else {
                let word = u128::from(low) | (u128::from(words.next().unwrap_or(0)) << 64);
                self.backend_mut().set(signal, word.to_le());
            }
        } else {
            self.backend_mut().set_four_state(signal, value, mask);
        }
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

#[cfg(all(test, feature = "systemverilog"))]
mod tests {
    use super::*;

    #[test]
    fn packed_reads_match_backend_across_word_boundaries_and_padding() {
        let widths = [1usize, 3, 8, 9, 31, 32, 33, 63, 64, 65, 96, 127, 128, 129];
        let ports = widths
            .iter()
            .flat_map(|width| {
                [
                    format!("input logic [{}:0] p{width}", width - 1),
                    format!("output logic [{}:0] q{width}", width - 1),
                ]
            })
            .collect::<Vec<_>>()
            .join(",");
        let assignments = widths
            .iter()
            .map(|width| format!("assign q{width}=p{width};"))
            .collect::<String>();
        let source = format!("module Top({ports}); {assignments} endmodule");
        for four_state in [false, true] {
            let simulator = celox::Simulator::from_sv_sources(
                vec![(source.as_str(), std::path::Path::new("packed_reads.sv"))],
                "Top",
            )
            .four_state(four_state)
            .opt_level(celox::OptLevel::O1)
            .build()
            .unwrap();
            // Safety: this test executes only an image produced in-process.
            let mut instance = unsafe {
                NativeProgramInstance::from_image(simulator.shared_code().program_image().clone())
            }
            .unwrap();
            for width in widths {
                let signal = instance.signal_ref(&format!("Top.p{width}")).unwrap();
                let bytes = width.div_ceil(8);
                let planes = if four_state { 2 } else { 1 };
                for pattern in [0u8, 0xff, 0x55, 0x80] {
                    let (memory, size) = instance.backend_mut().memory_as_mut_ptr();
                    assert!(signal.offset + bytes * planes <= size);
                    // Safety: write exactly this signal's allocated planes.
                    // Deliberately leave nonzero padding bits in the last byte;
                    // the public reads must remove them in both value and mask.
                    let storage = unsafe {
                        std::slice::from_raw_parts_mut(memory.add(signal.offset), bytes * planes)
                    };
                    for (index, byte) in storage.iter_mut().enumerate() {
                        *byte = pattern.rotate_left(index as u32);
                    }
                    assert_eq!(
                        VpiState::get_four_state(&instance, signal),
                        instance.backend().get_four_state(signal),
                        "width={width}, four_state={four_state}, pattern={pattern:x}"
                    );
                }
                let empty = SignalRef { width: 0, ..signal };
                assert_eq!(
                    VpiState::get_four_state(&instance, empty),
                    instance.backend().get_four_state(empty)
                );
            }
        }
    }
    #[test]
    fn packed_writes_match_backend_and_preserve_neighboring_planes() {
        let widths = [1usize, 3, 8, 9, 31, 32, 33, 63, 64, 65, 96, 127, 128, 129];
        let ports = widths
            .iter()
            .flat_map(|width| {
                [
                    format!("input logic [{}:0] p{width}", width - 1),
                    format!("output logic [{}:0] q{width}", width - 1),
                ]
            })
            .collect::<Vec<_>>()
            .join(",");
        let assignments = widths
            .iter()
            .map(|width| format!("assign q{width}=p{width};"))
            .collect::<String>();
        let source = format!(
            "module Top({ports},input bit [127:0] b,output bit [127:0] bo); {assignments} assign bo=b; endmodule"
        );
        let names = widths
            .iter()
            .flat_map(|width| [format!("Top.p{width}"), format!("Top.q{width}")])
            .chain(["Top.b".into(), "Top.bo".into()])
            .collect::<Vec<_>>();
        let writable = widths
            .iter()
            .map(|width| format!("Top.p{width}"))
            .chain(["Top.b".into()])
            .collect::<Vec<_>>();
        for four_state in [false, true] {
            let simulator = celox::Simulator::from_sv_sources(
                vec![(source.as_str(), std::path::Path::new("packed_writes.sv"))],
                "Top",
            )
            .four_state(four_state)
            .opt_level(celox::OptLevel::O1)
            .build()
            .unwrap();
            let image = simulator.shared_code().program_image().clone();
            // Safety: both instances use an image compiled in this test.
            let (mut fast, mut reference) = unsafe {
                (
                    NativeProgramInstance::from_image(image.clone()).unwrap(),
                    NativeProgramInstance::from_image(image).unwrap(),
                )
            };
            for name in &writable {
                let signal = fast.signal_ref(name).unwrap();
                let reference_signal = reference.signal_ref(name).unwrap();
                let ones = (BigUint::from(1u8) << signal.width) - BigUint::from(1u8);
                let high = BigUint::from(1u8) << (signal.width - 1);
                let patterns = [
                    (
                        (BigUint::from(1u8) << (signal.width + 5)) | &ones,
                        BigUint::default(),
                    ),
                    (ones.clone(), ones.clone()),
                    (BigUint::default(), ones.clone()),
                    (high.clone(), high),
                    (BigUint::from(1u8), BigUint::default()),
                    (BigUint::default(), BigUint::default()),
                ];
                for (value, mask) in patterns {
                    VpiState::set_four_state(&mut fast, signal, value.clone(), mask.clone());
                    reference
                        .backend_mut()
                        .set_four_state(reference_signal, value, mask);
                    // Inspect the actual stored bytes too, including padding
                    // and transitions from unknown masks back to known values.
                    let bytes = signal.width.div_ceil(8)
                        * if signal.is_4state && four_state { 2 } else { 1 };
                    let (fast_ptr, fast_size) = fast.backend().memory_as_ptr();
                    let (reference_ptr, reference_size) = reference.backend().memory_as_ptr();
                    assert!(
                        signal.offset + bytes <= fast_size
                            && reference_signal.offset + bytes <= reference_size
                    );
                    // Safety: these views cover only the selected stored planes.
                    unsafe {
                        assert_eq!(
                            std::slice::from_raw_parts(fast_ptr.add(signal.offset), bytes),
                            std::slice::from_raw_parts(
                                reference_ptr.add(reference_signal.offset),
                                bytes
                            ),
                            "{name}, four_state={four_state}"
                        );
                    }
                    for neighbor in &names {
                        assert_eq!(
                            fast.backend()
                                .get_four_state(fast.signal_ref(neighbor).unwrap()),
                            reference
                                .backend()
                                .get_four_state(reference.signal_ref(neighbor).unwrap()),
                            "neighbor={neighbor}, written={name}, four_state={four_state}"
                        );
                    }
                }
            }
        }
    }
}
