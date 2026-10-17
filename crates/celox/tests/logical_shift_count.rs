#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

// Dynamic logical shift counts of at least the operand width must yield zero
// rather than wrapping modulo the machine word on any backend.
all_backends! {
    fn two_state_dynamic_counts(sim) { @case "logical_shift_count::two_state_dynamic_counts"; }
    fn two_state_single_bit_wrap(sim) { @case "logical_shift_count::two_state_single_bit_wrap"; }
    fn two_state_wide_count(sim) { @case "logical_shift_count::two_state_wide_count"; }
    fn four_state_dynamic_counts(sim) { @case "logical_shift_count::four_state_dynamic_counts"; }
}
