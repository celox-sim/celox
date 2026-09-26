#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {

fn test_context_determined_width_subtraction(sim) {
    @case "context_width::test_context_determined_width_subtraction";
}

fn test_unsized_constant_width_subtraction(sim) {
    @case "context_width::test_unsized_constant_width_subtraction";
}

fn test_runtime_variable_width3_subtraction(sim) {
    @case "context_width::test_runtime_variable_width3_subtraction";
}

fn test_runtime_variable_width2_subtraction(sim) {
    @case "context_width::test_runtime_variable_width2_subtraction";
}

fn test_comparison_different_widths(sim) {
    @case "context_width::test_comparison_different_widths";
}

fn test_addition_different_widths(sim) {
    @case "context_width::test_addition_different_widths";
}

fn test_ff_width_propagation(sim) {
    @case "context_width::test_ff_width_propagation";
}

fn test_zero_extend(sim) {
    @case "context_width::test_zero_extend";
}

fn test_nested_width_propagation(sim) {
    @case "context_width::test_nested_width_propagation";
}

fn test_runtime_shift_width_behavior(sim) {
    @case "context_width::test_runtime_shift_width_behavior";
}

fn test_runtime_arithmetic_shift_behavior(sim) {
    @case "context_width::test_runtime_arithmetic_shift_behavior";
}

}
