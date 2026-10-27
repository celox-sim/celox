use veryl_metadata::{ClockType, ResetType};

/// Resolve a reset literal as one unsigned bit, using the configured polarity
/// only when its counterpart has the abstract `reset` type.
pub(crate) fn reset_value(
    kind: veryl_analyzer::ir::ResetValueKind,
    polarity: veryl_analyzer::ir::ResetPolarity,
    reset_type: ResetType,
) -> bool {
    use veryl_analyzer::ir::{ResetPolarity, ResetValueKind};
    let active_high = match polarity {
        ResetPolarity::High => true,
        ResetPolarity::Low => false,
        ResetPolarity::Default => matches!(reset_type, ResetType::AsyncHigh | ResetType::SyncHigh),
    };
    active_high == matches!(kind, ResetValueKind::Active)
}

#[derive(Debug, Clone)]
pub struct BuildConfig {
    pub file_dependencies: crate::FileDependencyCollector,
    pub clock_type: ClockType,
    pub reset_type: ResetType,
    /// Simulation lanes requested for partitioned execution. More than one
    /// lane also lowers every FF trigger group as independent parts.
    pub parallel_lanes: u32,
    /// Permit inferred latches in `always_comb`. Disabled by default.
    pub allow_inferred_latches: bool,
}

impl Default for BuildConfig {
    fn default() -> Self {
        Self {
            clock_type: ClockType::PosEdge,
            reset_type: ResetType::AsyncLow,
            parallel_lanes: 1,
            allow_inferred_latches: false,
            file_dependencies: Default::default(),
        }
    }
}

impl From<&veryl_metadata::Build> for BuildConfig {
    fn from(build: &veryl_metadata::Build) -> Self {
        Self {
            clock_type: build.clock_type,
            reset_type: build.reset_type,
            parallel_lanes: 1,
            allow_inferred_latches: false,
            file_dependencies: Default::default(),
        }
    }
}
