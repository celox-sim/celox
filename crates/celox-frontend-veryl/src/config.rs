use veryl_metadata::{ClockType, ResetType};

#[derive(Debug, Clone, Copy)]
pub struct BuildConfig {
    pub clock_type: ClockType,
    pub reset_type: ResetType,
    /// Simulation lanes requested for partitioned execution. More than one
    /// lane also lowers every FF trigger group as independent parts.
    pub parallel_lanes: u32,
}

impl Default for BuildConfig {
    fn default() -> Self {
        Self {
            clock_type: ClockType::PosEdge,
            reset_type: ResetType::AsyncLow,
            parallel_lanes: 1,
        }
    }
}

impl From<&veryl_metadata::Build> for BuildConfig {
    fn from(build: &veryl_metadata::Build) -> Self {
        Self {
            clock_type: build.clock_type,
            reset_type: build.reset_type,
            parallel_lanes: 1,
        }
    }
}
