#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    fn test_arithmetic_in_concat(sim) {
        @case "concat_operators::test_arithmetic_in_concat";
    }

    fn test_comparison_in_concat(sim) {
        @case "concat_operators::test_comparison_in_concat";
    }

    fn test_bitwise_and_logical_in_concat(sim) {
        @case "concat_operators::test_bitwise_and_logical_in_concat";
    }

    fn test_ternary_in_concat(sim) {
        @case "concat_operators::test_ternary_in_concat";
    }

    fn test_as_cast_in_concat(sim) {
        @ignore_on(sv);
        @case "concat_operators::test_as_cast_in_concat";
    }

    fn test_nested_concat_and_repeat(sim) {
        @case "concat_operators::test_nested_concat_and_repeat";
    }

    fn test_shift_in_concat(sim) {
        @case "concat_operators::test_shift_in_concat";
    }
}
