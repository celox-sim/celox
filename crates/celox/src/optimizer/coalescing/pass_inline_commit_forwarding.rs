use crate::ir::{ExecutionUnit, RegionedAbsoluteAddr};

use super::commit_ops::{DirectStableStoreHazards, inline_commit_forwarding_with_hazards};

/// Run forwarding for one complete event after its cross-EU hazards have been
/// computed.  An individual EU is not a sufficient semantic scope for NBA.
pub(super) fn run_complete_event(
    units: &mut [ExecutionUnit<RegionedAbsoluteAddr>],
    hazards: &DirectStableStoreHazards,
) {
    for unit in units {
        inline_commit_forwarding_with_hazards(unit, hazards);
    }
}
