#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // Test: Two always_ff blocks in the same module, same clock.
    // Block 1 updates `count`. Block 2 reads `count` to set `r_empty`.
    // NBA semantics: r_empty should see the OLD count value.
    fn test_same_module_count_and_empty_ff(sim) {
        @ignore_on(sv);
        @case "nba_cross_block_empty::test_same_module_count_and_empty_ff";
    }

    // Test: count FF in parent module, r_empty FF in child module.
    // The child reads count through a port. NBA: should see OLD count.
    fn test_cross_module_count_and_empty_ff(sim) {
        @case "nba_cross_block_empty::test_cross_module_count_and_empty_ff";
    }

    // Stress test: push N items, pop all, verify empty eventually goes high.
    // This specifically catches the "empty=0 forever" bug.
    fn test_empty_never_stuck_at_zero(sim) {
        @case "nba_cross_block_empty::test_empty_never_stuck_at_zero";
    }
}
