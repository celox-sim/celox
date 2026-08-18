use veryl_metadata::{ClockType, ResetType};

#[derive(Debug, Clone, Copy)]
pub struct BuildConfig {
    pub clock_type: ClockType,
    pub reset_type: ResetType,
}

impl Default for BuildConfig {
    fn default() -> Self {
        Self {
            clock_type: ClockType::PosEdge,
            reset_type: ResetType::AsyncLow,
        }
    }
}

impl From<&veryl_metadata::Build> for BuildConfig {
    fn from(build: &veryl_metadata::Build) -> Self {
        Self {
            clock_type: build.clock_type,
            reset_type: build.reset_type,
        }
    }
}
