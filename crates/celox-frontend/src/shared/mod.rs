//! Source-language-independent frontend contracts.

mod artifact;
mod lookup;

pub use artifact::{FusedSirOptimizationHints, ScheduledRtl, ScheduledRtlOutput};
pub use lookup::{
    FrontendLookup, InstancePath, SourceAddr, SourceVarId, VariableInfo, VariableKind,
};
