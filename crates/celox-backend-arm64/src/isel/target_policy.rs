//! AArch64-specific instruction-selection policy.

use crate::mir::MFunction;

pub(super) struct TargetIselPolicy {
    pub(super) select_packed_bit_stores: bool,
    pub(super) select_packed_field_compares: bool,
}

impl TargetIselPolicy {
    pub(super) fn for_function(_function: &MFunction, _four_state: bool) -> Self {
        // Packed deposit/extract are currently multi-instruction AArch64
        // recipes. Keep the portable scalar path until target-local cost and
        // NEON profitability models authorize these selections.
        Self {
            select_packed_bit_stores: false,
            select_packed_field_compares: false,
        }
    }
}
