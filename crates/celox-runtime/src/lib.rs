//! Backend-independent executable simulation contracts and utilities.

pub mod backend;
mod derived_tick;
pub mod distribution;
mod edge;
mod error;
mod event_buffer;
pub mod file_io;
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
    CompiledSlots, EventInfo, HostBoundary, KernelSchedule, ScheduleKey, ScheduleParts,
    SchedulerMode, SchedulerStats, SimulationExecutor, SimulationSnapshot, SimulationState,
    StepObserver, UnitExecutor,
};
pub use state_file::{
    BaselineRecord, NbaRecord, ProcessRecord, ScheduleRecord, ScheduledEvent, StateDifference,
    StateFile, StateFileError, StateObject, StateRole, WaiterRecord, format_state_value,
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
    // SAFETY: both ranges are in the backend's memory, and they are
    // distinct objects of the state layout.
    unsafe { std::ptr::copy_nonoverlapping(memory.add(from.offset), memory.add(to.offset), bytes) };
}
