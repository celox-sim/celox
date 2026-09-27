#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    fn test_array_literal_comb_assignment(sim) {
        @ignore_on(sv);
        @case "array_literal::test_array_literal_comb_assignment";
    }

    fn test_array_literal_default_comb_assignment(sim) {
        @ignore_on(sv);
        @case "array_literal::test_array_literal_default_comb_assignment";
    }

    fn test_array_literal_nested_default_multidim_assignment(sim) {
        @ignore_on(sv);
        @case "array_literal::test_array_literal_nested_default_multidim_assignment";
    }

    fn test_array_literal_default_fills_param_sized_array(sim) {
        @ignore_on(sv);
        @case "array_literal::test_array_literal_default_fills_param_sized_array";
    }

    fn test_array_literal_single_element_size_one_array(sim) {
        @ignore_on(sv);
        @case "array_literal::test_array_literal_single_element_size_one_array";
    }

    fn test_array_literal_default_fills_2d_array(sim) {
        @ignore_on(sv);
        @case "array_literal::test_array_literal_default_fills_2d_array";
    }

    // '{default: 0} with no explicit elements: must produce exactly target_len elements.
    // Regression test for off-by-one where remaining was target_len - (x.len()-1).
    fn test_array_literal_default_only(sim) {
        @ignore_on(sv);
        @case "array_literal::test_array_literal_default_only";
    }

    // Two explicit elements + default: remaining slots filled correctly.
    fn test_array_literal_two_explicit_plus_default(sim) {
        @ignore_on(sv);
        @case "array_literal::test_array_literal_two_explicit_plus_default";
    }

    // repeat + default: '{val repeat 2, default: 0} in a size-4 array.
    fn test_array_literal_repeat_plus_default(sim) {
        @ignore_on(sv);
        @case "array_literal::test_array_literal_repeat_plus_default";
    }

    // '{default: 0} in always_ff if_reset: array reset via default fill.
    fn test_array_literal_default_in_ff_reset(sim) {
        @ignore_on(sv);
        @case "array_literal::test_array_literal_default_in_ff_reset";
    }
}
