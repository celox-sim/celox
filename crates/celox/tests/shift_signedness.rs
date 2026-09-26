#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    fn test_shift_right_arithmetic_native(sim) {
        @case "shift_signedness::test_shift_right_arithmetic_native";
    }

    fn test_shift_right_logical_signed_native(sim) {
        @case "shift_signedness::test_shift_right_logical_signed_native";
    }

    fn test_shift_right_arithmetic_wide(sim) {
        @case "shift_signedness::test_shift_right_arithmetic_wide";
    }

    fn test_shift_right_logical_wide(sim) {
        @case "shift_signedness::test_shift_right_logical_wide";
    }

    fn test_shift_constant_folding_wide(sim) {
        @case "shift_signedness::test_shift_constant_folding_wide";
    }

    fn test_shift_constant_folding_native(sim) {
        @case "shift_signedness::test_shift_constant_folding_native";
    }
}
