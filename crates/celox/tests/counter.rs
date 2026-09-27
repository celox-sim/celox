#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // Simple counter: increment on each tick, reset to 0
    fn test_counter_n4_basic(sim) {
        @ignore_on(sv);
        @case "counter::test_counter_n4_basic";
    }

    // Large counter array (similar to bench)
    fn test_counter_n100_wrap(sim) {
        @ignore_on(sv);
        @case "counter::test_counter_n100_wrap";
    }

    fn test_phase_state_ssa_preserves_eval_before_apply_and_four_state(sim) {
        @ignore_on(veryl, sv);
        @case "counter::test_phase_state_ssa_preserves_eval_before_apply_and_four_state";
    }
}
