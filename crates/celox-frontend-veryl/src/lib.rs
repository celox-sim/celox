//! Veryl frontend adapter for Celox.
//!
//! Veryl parser identities are projected into [`celox_frontend_core`] contracts
//! before source-neutral scheduling and runtime construction.

mod config;
mod error;
mod veryl;

pub use veryl::*;

pub(crate) mod symbolic {
    pub use celox_frontend_core::symbolic::*;
}

pub(crate) use celox_frontend_core::{
    FrontendLookup, FrontendTrace, FrontendTraceOptions, FusedSirOptimizationHints, InstancePath,
    ScheduledRtl, ScheduledRtlOutput, SourceAddr, SourceVarId, VariableInfo, VariableKind,
};
pub use config::BuildConfig;
pub use error::{FrontendDiagnostic, LoweringPhase, ParserError, SourceLocation};
pub use veryl::lowering::types::{resolve_dims, resolve_total_width};
pub(crate) use veryl::lowering::{
    bitaccess, bitslicer, case, context_width, ff, logic_tree, registry,
};

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
