//! Backend-independent executable simulation contracts and utilities.

pub mod backend;
mod error;
mod event_buffer;
pub mod scheduler;
mod simulation;
mod testbench;
mod vcd;

pub use error::SimulatorErrorCode;
pub use event_buffer::RuntimeEventBuffer;
pub use simulation::{EventInfo, SimulationExecutor, SimulationState};
pub use testbench::bind_testbench_program;
pub use vcd::{VcdExternalSignalDesc, VcdSignalDesc, VcdWriter};

pub type AbsoluteAddr = celox_design::StateAddr;
pub type MemoryLayout = celox_state_layout::MemoryLayout<AbsoluteAddr>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SignalArrayLayout {
    pub element_width: usize,
    pub element_count: usize,
    pub element_stride: usize,
    pub plane_size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SignalRef {
    pub offset: usize,
    pub width: usize,
    pub is_4state: bool,
    pub array_layout: Option<SignalArrayLayout>,
}
