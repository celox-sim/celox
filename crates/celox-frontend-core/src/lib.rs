//! Source-language-independent symbolic lowering and scheduling.

mod error;
pub mod shared;
pub mod symbolic;
mod trace;

pub use error::{LoweringPhase, ParserError, SourceLocation};
pub use shared::{
    FrontendLookup, FusedSirOptimizationHints, InstancePath, ScheduledRtl, ScheduledRtlOutput,
    SourceAddr, SourceVarId, VariableInfo, VariableKind,
};
pub use trace::{FrontendTrace, FrontendTraceOptions, TraceSimModule};

pub use symbolic::flattening;

pub(crate) type HashMap<K, V> = fxhash::FxHashMap<K, V>;
pub(crate) type HashSet<T> = fxhash::FxHashSet<T>;
