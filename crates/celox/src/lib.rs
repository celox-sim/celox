mod backend;
mod context_width;
mod debug;
mod flatting;
mod ir;
mod logic_tree;
mod optimizer;
mod parser;
mod scheduler;
mod simulation;
mod simulator;
mod vcd;

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
pub use backend::{MemoryLayout, get_byte_size};
pub use debug::{CompilationTrace, TraceOptions};
pub(crate) use fxhash::FxHashMap as HashMap;
pub(crate) use fxhash::FxHashSet as HashSet;
pub use ir::{AbsoluteAddr, PortTypeKind, SignalRef};
pub use malachite_bigint::BigUint;
pub use parser::ParserError;
pub use parser::SchedulerError;
pub use simulation::Simulation;
pub use simulator::Simulator;
pub use simulator::SimulatorBuilder;
pub use simulator::SimulatorError;
pub use simulator::SimulatorOptions;
pub use simulator::{NamedEvent, NamedSignal};
pub use celox_macros::veryl_test;

#[cfg(test)]
mod flatting_tests;
