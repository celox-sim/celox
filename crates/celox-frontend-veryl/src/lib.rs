//! Veryl frontend adapter for Celox.
//!
//! Veryl parser identities are projected into [`celox_frontend_core`] contracts
//! before source-neutral scheduling and runtime construction.

pub(crate) mod artifact;
mod component;
mod config;
mod dynamic_for_check;
mod error;
pub mod hierarchy;
pub mod loop_provenance;
pub(crate) mod lowering;
pub mod module;
mod schedule;
mod source;
mod testbench;

pub(crate) mod symbolic {
    pub use celox_frontend_core::symbolic::*;
}

use crate::symbolic::artifact::ExternalModule;
pub use artifact::{VerylSimModule as SimModule, VerylSymbolicRtl};
pub(crate) use celox_frontend_core::{
    FrontendLookup, FrontendTrace, FrontendTraceOptions, FusedSirOptimizationHints, InstancePath,
    ScheduledRtl, ScheduledRtlOutput, SourceAddr, SourceVarId, VariableInfo, VariableKind,
};
pub use config::BuildConfig;
pub use dynamic_for_check::{check_dynamic_for_bounds, check_elaborated_dynamic_for_bounds};
pub use error::{FrontendDiagnostic, LoweringPhase, ParserError, SourceLocation};
pub use hierarchy::{parse_ir, parse_ir_with_external_hierarchy, parse_ir_with_loop_provenance};
pub use lowering::types::{resolve_dims, resolve_total_width};
pub use schedule::schedule_symbolic_rtl;
pub use source::{
    AbsoluteAddr, GlueAddr, GlueBlock, ModuleInitialMemoryValue, RegionedAbsoluteAddr,
    RegionedVarAddr, VerylComponentBinding, VerylComponentConnectionBinding,
    VerylComponentEventBinding, VerylComponentInputBinding, VerylIdMap, VerylScheduledRtlOutput,
    VerylTestbenchSource,
};
pub use testbench::{collect_testbench_observability, compile_semantic_testbench};

pub(crate) use lowering::{bitaccess, bitslicer, case, context_width, ff, logic_tree, registry};
pub(crate) use source::{function_call_arg, function_call_has_arg};

use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_testbench_source_is_empty() {
        let source = VerylTestbenchSource::default();
        assert!(source.initial_statements.is_none());
        assert!(source.functions.is_empty());
        assert!(source.components.is_empty());
        assert!(source.component_libraries.is_empty());
        assert!(source.component_file_base.is_none());
    }
}
