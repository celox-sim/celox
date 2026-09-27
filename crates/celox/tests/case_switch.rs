#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    fn test_case_basic_comb(sim) {
        @ignore_on(sv);
        @case "case_switch::test_case_basic_comb";
    }

    fn test_switch_basic_comb(sim) {
        @ignore_on(sv);
        @case "case_switch::test_switch_basic_comb";
    }

    fn test_case_multiarm(sim) {
        @ignore_on(sv);
        @case "case_switch::test_case_multiarm";
    }

    fn test_case_nested_in_if(sim) {
        @ignore_on(sv);
        @case "case_switch::test_case_nested_in_if";
    }

    fn test_case_block_body(sim) {
        @ignore_on(sv);
        @case "case_switch::test_case_block_body";
    }

    fn test_case_in_always_ff(sim) {
        @case "case_switch::test_case_in_always_ff";
    }

    fn test_case_in_comb_function_return(sim) {
        @case "case_switch::test_case_in_comb_function_return";
    }

    fn test_case_in_comb_function_output_argument(sim) {
        @ignore_on(sv);
        @case "case_switch::test_case_in_comb_function_output_argument";
    }

    fn test_case_break_inside_comb_function_for(sim) {
        @ignore_on(sv);
        @case "case_switch::test_case_break_inside_comb_function_for";
    }
}
