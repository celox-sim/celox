//! Veryl adapter and source-owned compiler sidecars.

use crate::symbolic::artifact::ExternalModule;

pub(crate) mod artifact;
mod component;
mod dynamic_for_check;
pub mod hierarchy;
pub mod loop_provenance;
pub(crate) mod lowering;
pub mod module;
mod schedule;
mod source;
mod testbench;

pub use artifact::{VerylSimModule as SimModule, VerylSymbolicRtl};
pub use dynamic_for_check::{check_dynamic_for_bounds, check_elaborated_dynamic_for_bounds};
pub use hierarchy::{parse_ir, parse_ir_with_external_hierarchy, parse_ir_with_loop_provenance};
pub use schedule::schedule_symbolic_rtl;
pub use source::{
    AbsoluteAddr, GlueAddr, GlueBlock, ModuleInitialMemoryValue, RegionedAbsoluteAddr,
    RegionedVarAddr, VerylComponentBinding, VerylComponentConnectionBinding,
    VerylComponentEventBinding, VerylComponentInputBinding, VerylIdMap, VerylScheduledRtlOutput,
    VerylTestbenchSource,
};
pub use testbench::{collect_testbench_observability, compile_semantic_testbench};

pub(crate) use source::{function_call_arg, function_call_has_arg};
