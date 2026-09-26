#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    fn test_sorter_push_pop_empty(sim) {
        @ignore_on(sv);
        @case "linear_sorter_pull::test_sorter_push_pop_empty";
    }

    // Focused test: push one element, then pop it.
    // The empty flag should go 1 → 0 → 1 across the three phases.
    fn test_sorter_empty_flag_single_push_pop(sim) {
        @ignore_on(sv);
        @case "linear_sorter_pull::test_sorter_empty_flag_single_push_pop";
    }

    // Test: push+pop simultaneously — count should not change.
    fn test_sorter_simultaneous_push_pop(sim) {
        @ignore_on(sv);
        @case "linear_sorter_pull::test_sorter_simultaneous_push_pop";
    }
}
