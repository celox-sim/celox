//! Veryl adapter and source-owned compiler sidecars.

mod dynamic_for_check;
pub mod hierarchy;
pub mod loop_provenance;
pub mod module;
mod source;
mod testbench;

pub use crate::symbolic::artifact::{
    ExternalHierarchy, ExternalModule, RelocationModule, SimModule, SymbolicRtl,
};
pub use crate::symbolic::assembly::schedule_veryl_symbolic_rtl as schedule_symbolic_rtl;
pub use dynamic_for_check::{check_dynamic_for_bounds, check_elaborated_dynamic_for_bounds};
pub use hierarchy::{parse_ir, parse_ir_with_external_hierarchy, parse_ir_with_loop_provenance};
pub use source::{
    AbsoluteAddr, GlueAddr, GlueBlock, ModuleInitialMemoryValue, RegionedAbsoluteAddr,
    RegionedVarAddr, VerylComponentBinding, VerylComponentConnectionBinding,
    VerylComponentEventBinding, VerylComponentInputBinding, VerylIdMap, VerylScheduledRtlOutput,
    VerylTestbenchSource,
};
pub use testbench::{collect_testbench_observability, compile_semantic_testbench};

/// Veryl-shaped symbolic flattening hooks retained for internal integration
/// tests. New source-independent code should consume [`crate::shared`] output.
pub mod flattening {
    pub use crate::symbolic::flattening::flatten_module;
}

pub(crate) use source::{function_call_arg, function_call_has_arg};
