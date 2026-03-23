mod backend;
mod context_width;
mod debug;
mod flatting;
mod ir;
mod logic_tree;
mod optimizer;
mod parser;
pub(crate) mod portable;
mod scheduler;
pub(crate) mod serde_helpers;
mod simulation;
mod simulator;
mod vcd;
pub use vcd::{VcdSignalDesc, VcdWriter};

pub use backend::SimulatorErrorCode as RuntimeErrorCode;

pub struct IOContext<'a> {
    pub(crate) backend: &'a mut backend::JitBackend,
}

impl<'a> IOContext<'a> {
    pub fn set<T: Copy>(&mut self, signal: SignalRef, val: T) {
        self.backend.set(signal, val);
    }
    pub fn set_wide(&mut self, signal: SignalRef, val: BigUint) {
        self.backend.set_wide(signal, val);
    }
    pub fn set_four_state(&mut self, signal: SignalRef, val: BigUint, mask: BigUint) {
        self.backend.set_four_state(signal, val, mask);
    }
}

pub use backend::EventRef;
pub use backend::SharedJitCode;
pub use backend::{JitBackend, MemoryLayout, get_byte_size};
pub use celox_macros::veryl_test;
pub use debug::{CompilationTrace, TraceOptions};
pub(crate) use fxhash::FxHashMap as HashMap;
pub(crate) use fxhash::FxHashSet as HashSet;
pub use ir::{AbsoluteAddr, AddrLookupError, PortTypeKind, SignalRef};
pub use malachite_bigint::BigUint;
pub use optimizer::CraneliftOptLevel;
pub use optimizer::CraneliftOptions;
pub use optimizer::OptimizeOptions;
pub use optimizer::RegallocAlgorithm;
pub use parser::LoweringPhase;
pub use parser::ParserError;
pub use parser::SchedulerError;
pub use simulation::Simulation;
pub use simulator::DeadStorePolicy;
pub use simulator::Simulator;
pub use simulator::SimulatorBuilder;
pub use simulator::SimulatorError;
pub use simulator::SimulatorErrorKind;
pub use simulator::SimulatorOptions;
pub use simulator::render_diagnostic;
pub use simulator::{InstanceHierarchy, NamedEvent, NamedSignal};
pub use veryl_metadata::{ClockType, ResetType};

#[cfg(test)]
mod flatting_tests;
