#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    fn test_wide_int_memory_access(sim) {
        @case "wide_data::test_wide_int_memory_access";
    }

    fn test_wide_concatenation(sim) {
        @case "wide_data::test_wide_concatenation";
    }

    fn test_nested_wide_concatenation(sim) {
        @case "wide_data::test_nested_wide_concatenation";
    }

    fn test_wide_partial_write(sim) {
        @case "wide_data::test_wide_partial_write";
    }

    fn test_wide_cross_boundary_unaligned_write(sim) {
        @case "wide_data::test_wide_cross_boundary_unaligned_write";
    }

    fn test_wide_rmw_preserve_neighboring_bits(sim) {
        @case "wide_data::test_wide_rmw_preserve_neighboring_bits";
    }
}
