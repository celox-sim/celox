//! HDL frontend construction for Celox.
//!
//! [`veryl`] and [`systemverilog`] own their source-language adapters,
//! [`symbolic`] is the private compatibility core used while lowering, and
//! [`shared`] contains the source-independent artifacts that may leave this
//! crate. Semantic design and backend phases must not depend on parser-native
//! identities retained during frontend construction.

mod config;
mod error;
pub mod shared;
mod symbolic;
#[cfg(feature = "systemverilog")]
pub mod systemverilog;
mod trace;
pub mod veryl;

pub use config::BuildConfig;
pub use error::{FrontendDiagnostic, LoweringPhase, ParserError, SourceLocation};
pub use shared::{
    FrontendLookup, FusedSirOptimizationHints, InstancePath, ScheduledRtl, ScheduledRtlOutput,
    SourceAddr, SourceVarId, VariableInfo, VariableKind,
};
pub(crate) use symbolic::artifact::{RelocationModule, SimModule};
pub use symbolic::global_ff::{
    FfClockRecipe, FfRuntimeRelocation, SharedClockLowering, build_ff_clock_recipes,
};
pub use symbolic::types::{resolve_dims, resolve_total_width};
pub(crate) use symbolic::{
    bitaccess, bitslicer, case, context_width, ff, flattening, logic_tree, registry,
};
pub use trace::{FrontendTrace, FrontendTraceOptions};
pub(crate) use veryl::{
    AbsoluteAddr, GlueAddr, GlueBlock, ModuleInitialMemoryValue, RegionedAbsoluteAddr,
    RegionedVarAddr, VerylComponentBinding, VerylComponentConnectionBinding,
    VerylComponentEventBinding, VerylComponentInputBinding, VerylIdMap, VerylScheduledRtlOutput,
    VerylTestbenchSource,
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
