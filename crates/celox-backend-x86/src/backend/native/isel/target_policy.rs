//! x86-specific instruction-selection policy.

use crate::native::mir::MFunction;

pub(super) struct TargetIselPolicy {
    pub(super) select_packed_bit_stores: bool,
    pub(super) select_packed_field_compares: bool,
}

impl TargetIselPolicy {
    pub(super) fn for_function(function: &MFunction, four_state: bool) -> Self {
        let bmi2 = !four_state && function.target_features.bmi2();
        Self {
            select_packed_bit_stores: bmi2,
            select_packed_field_compares: bmi2,
        }
    }
}
