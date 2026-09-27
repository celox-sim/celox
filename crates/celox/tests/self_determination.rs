#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    fn test_concatenation_self_determination(sim) {
        @case "self_determination::test_concatenation_self_determination";
    }

    fn test_comparison_self_determination(sim) {
        @case "self_determination::test_comparison_self_determination";
    }

    fn test_shift_rhs_self_determination(sim) {
        @case "self_determination::test_shift_rhs_self_determination";
    }

    fn test_shift_rhs_constant_self_determination(sim) {
        @case "self_determination::test_shift_rhs_constant_self_determination";
    }

    // Constant folding should also respect self-determination.
    fn test_concatenation_constant_self_determination(sim) {
        @case "self_determination::test_concatenation_constant_self_determination";
    }

    // Constant folding should also respect self-determination.
    fn test_concatenation_constant_self_determination_runtime(sim) {
        @case "self_determination::test_concatenation_constant_self_determination_runtime";
    }
}
