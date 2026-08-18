//! HDL frontend construction for Celox.
//!
//! [`veryl`] and [`systemverilog`] own their source-language adapters,
//! [`celox_frontend_core`] contains source-neutral symbolic assembly and final
//! contracts. Semantic design and backend phases must not depend on
//! parser-native identities retained during frontend construction.

mod config;
mod error;
#[cfg(feature = "systemverilog")]
pub mod systemverilog;
pub mod veryl;

pub mod shared {
    pub use celox_frontend_core::shared::*;
}

pub(crate) mod symbolic {
    pub use celox_frontend_core::symbolic::*;
}

pub use celox_frontend_core::{
    FrontendLookup, FrontendTrace, FrontendTraceOptions, FusedSirOptimizationHints, InstancePath,
    ScheduledRtl, ScheduledRtlOutput, SourceAddr, SourceVarId, TraceSimModule, VariableInfo,
    VariableKind,
};
pub use config::BuildConfig;
pub use error::{FrontendDiagnostic, LoweringPhase, ParserError, SourceLocation};
pub(crate) use veryl::artifact::VerylSimModule as SimModule;
pub use veryl::lowering::types::{resolve_dims, resolve_total_width};
pub(crate) use veryl::lowering::{
    bitaccess, bitslicer, case, context_width, ff, logic_tree, registry,
};
pub(crate) use veryl::{
    GlueAddr, GlueBlock, ModuleInitialMemoryValue, RegionedVarAddr, VerylComponentEventBinding,
    VerylComponentInputBinding, VerylIdMap, VerylTestbenchSource,
};
pub(crate) use veryl::{function_call_arg, function_call_has_arg};

use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_lookup_has_no_source_identities() {
        let lookup = FrontendLookup::default();
        assert!(lookup.instance_ids.is_empty());
        assert!(lookup.instance_module.is_empty());
        assert!(lookup.module_variables.is_empty());
        assert!(lookup.module_var_path_index.is_empty());
        assert!(lookup.module_names.is_empty());
        assert!(lookup.source_to_state.is_empty());
        assert!(lookup.state_to_source.is_empty());
    }

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
