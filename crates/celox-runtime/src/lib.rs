//! Backend-independent executable simulation contracts and utilities.

pub mod backend;
mod derived_tick;
pub mod distribution;
mod edge;
mod error;
mod event_buffer;
pub mod file_io;
pub mod file_io_backend;
pub mod parallel;
pub mod plusargs;
mod reflection;
pub mod scan;
mod schedule;
pub mod scheduler;
mod simulation;
mod state_file;
mod testbench;
pub mod timing;
mod vcd;

pub use derived_tick::{
    BackendExecutor, DerivedDomain, DerivedTickPlan, DerivedTickPlans, DerivedTickScratch, EdgeRun,
    EventDomain, Gate, GatedDomain, TickExecutor, build_derived_tick_plans,
};
pub use edge::{
    Edge, EdgeOutcome, EventEdge, EventEdges, SignalWrite, at_level, build_event_edges,
    move_signal, run_edge, run_edges, run_tick, set_level,
};
pub use error::{SimulatorErrorCode, comb_fatal_code, comb_fatal_site};
pub use event_buffer::RuntimeEventBuffer;
pub use reflection::{
    DesignReflection, ReflectionScope, ReflectionScopeId, ReflectionSignal, ReflectionSignalId,
    SignalDirection,
};
pub use schedule::{ResolvedSchedule, host_watch};
pub use simulation::{
    Activation, CompiledSlots, EventInfo, HostBoundary, KernelSchedule,
    PersistedSimulationSnapshot, ScheduleKey, ScheduleParts, SchedulerMode, SchedulerStats,
    SimulationExecutor, SimulationSnapshot, SimulationState, StepObserver, UnitExecutor,
};
pub use state_file::{
    ActivationRecord, BaselineRecord, FrameValueKind, FrameValueRecord, NbaRecord, ProcessRecord,
    ProcessRef, ScheduleRecord, ScheduledEvent, StateDifference, StateFile, StateFileError,
    StateObject, StateRole, WaiterRecord, format_state_value,
};
pub use testbench::bind_testbench_program;
pub use timing::{
    CountedDomainRefs, CountedRefs, Halt, HostWatch, NbaSiteRefs, NbaUpdate, ProcessRefs,
    ProcessState, TermRefs, Waiter, Waiters,
};
pub use vcd::{VcdExternalSignalDesc, VcdSignalDesc, VcdStatistics, VcdWriter};

pub type AbsoluteAddr = celox_design::StateAddr;
pub type MemoryLayout = celox_state_layout::MemoryLayout<AbsoluteAddr>;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct SignalArrayLayout {
    pub element_width: usize,
    pub element_count: usize,
    pub element_stride: usize,
    pub plane_size: usize,
}

impl SignalArrayLayout {
    /// The bytes from the first element's slot to the end of the last one.
    /// A layout describing part of an array keeps the whole array's
    /// `plane_size`, which locates its mask plane.
    pub fn element_extent(&self) -> usize {
        self.element_count * self.element_stride
    }

    /// The packed logical value of the plane stored in `plane`, element 0 in
    /// the lowest bits, without per-element padding. Linear in the plane size.
    pub fn gather(&self, plane: &[u8]) -> num_bigint::BigUint {
        let element_bytes = self.element_width.div_ceil(8);
        let mut packed = vec![0u8; (self.element_count * self.element_width).div_ceil(8) + 1];
        for index in 0..self.element_count {
            let first = index * self.element_width;
            let (start, shift) = (first / 8, first % 8);
            let element = &plane[index * self.element_stride..][..element_bytes];
            for (byte, &raw) in element.iter().enumerate() {
                let wide = u16::from(raw & self.element_byte_mask(byte)) << shift;
                packed[start + byte] |= wide as u8;
                if let Some(next) = packed.get_mut(start + byte + 1) {
                    *next |= (wide >> 8) as u8;
                }
            }
        }
        num_bigint::BigUint::from_bytes_le(&packed)
    }

    /// Store the packed logical `value` into `plane`: each element's bits at
    /// its stride slot, padding bits inside a slot cleared. Bytes of a slot
    /// past the element are left alone. Linear in the plane size.
    pub fn scatter(&self, plane: &mut [u8], value: &num_bigint::BigUint) {
        let packed = value.to_bytes_le();
        let byte_at = |first: usize| {
            let (start, shift) = (first / 8, first % 8);
            let low = packed.get(start).copied().unwrap_or(0);
            let high = packed.get(start + 1).copied().unwrap_or(0);
            ((u16::from(low) | (u16::from(high) << 8)) >> shift) as u8
        };
        let element_bytes = self.element_width.div_ceil(8);
        for index in 0..self.element_count {
            let first = index * self.element_width;
            let element = &mut plane[index * self.element_stride..][..element_bytes];
            for (byte, slot) in element.iter_mut().enumerate() {
                *slot = byte_at(first + byte * 8) & self.element_byte_mask(byte);
            }
        }
    }

    /// Copy the elements of `src` into `dst`, planes of this layout.
    pub fn copy(&self, src: &[u8], dst: &mut [u8]) {
        let element_bytes = self.element_width.div_ceil(8);
        for index in 0..self.element_count {
            let at = index * self.element_stride;
            for byte in 0..element_bytes {
                dst[at + byte] = src[at + byte] & self.element_byte_mask(byte);
            }
        }
    }

    /// The bits of byte `byte` of an element slot that belong to the element.
    fn element_byte_mask(&self, byte: usize) -> u8 {
        match self.element_width.saturating_sub(byte * 8) {
            8.. => u8::MAX,
            bits => (1u8 << bits) - 1,
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct SignalRef {
    pub offset: usize,
    pub width: usize,
    pub is_4state: bool,
    pub array_layout: Option<SignalArrayLayout>,
}

/// Copy the value of `from`, with its unknown bits, into `to`, a signal of
/// the same width and state form.
pub fn copy_signal<B: backend::SimBackend>(backend: &mut B, from: SignalRef, to: SignalRef) {
    if from.array_layout.is_some() || to.array_layout.is_some() {
        // Each array has its own layout, packed or with strided elements:
        // the backend gathers and scatters the elements.
        let (value, mask) = backend.get_four_state(from);
        backend.set_four_state(to, value, mask);
        return;
    }
    let planes = if backend.layout().four_state && from.is_4state {
        2
    } else {
        1
    };
    let bytes = celox_state_layout::get_byte_size(from.width) * planes;
    let (memory, size) = backend.memory_as_mut_ptr();
    assert!(from.offset + bytes <= size && to.offset + bytes <= size);
    // SAFETY: both ranges are in the backend's memory. Signal views can
    // alias, so use the copy operation that permits overlapping ranges.
    unsafe { std::ptr::copy(memory.add(from.offset), memory.add(to.offset), bytes) };
}

#[cfg(test)]
mod array_layout_tests {
    use super::SignalArrayLayout;
    use num_bigint::BigUint;

    #[test]
    fn strided_planes_round_trip_like_element_shifts() {
        for element_width in [1usize, 3, 8, 13, 16, 64, 70] {
            for element_count in [1, 2, 5, 33] {
                let element_stride = element_width.div_ceil(8).next_power_of_two();
                let array = SignalArrayLayout {
                    element_width,
                    element_count,
                    element_stride,
                    plane_size: element_stride * element_count,
                };
                let width = element_width * element_count;
                // Bits past the array must not reach any slot.
                let value = (0..width + 9).fold(BigUint::default(), |acc, bit| {
                    if (bit * 7 + bit / 3) % 5 < 2 {
                        acc | (BigUint::from(1u8) << bit)
                    } else {
                        acc
                    }
                });
                let mut plane = vec![0xa5u8; array.plane_size];
                array.scatter(&mut plane, &value);
                let element_mask = (BigUint::from(1u8) << element_width) - 1u8;
                for index in 0..element_count {
                    let slot = &plane[index * element_stride..][..element_width.div_ceil(8)];
                    let want = (&value >> (index * element_width)) & &element_mask;
                    assert_eq!(
                        BigUint::from_bytes_le(slot),
                        want,
                        "{element_width}x{element_count}"
                    );
                }
                let all = (BigUint::from(1u8) << width) - 1u8;
                assert_eq!(array.gather(&plane), &value & &all);
                let mut copied = vec![0x5au8; array.plane_size];
                array.copy(&plane, &mut copied);
                assert_eq!(array.gather(&copied), &value & &all);
            }
        }
    }
}
