#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // Test NBA semantics across separate always_ff blocks with the same clock.
    // In RTL, two always_ff blocks on the same clock should both read OLD values
    // (pre-edge) and write NEW values (post-edge), regardless of textual order.
    fn test_nba_separate_blocks_swap(sim) {
        @ignore_on(sv);
        @case "nba_cross_block::test_nba_separate_blocks_swap";
    }

    // Test pipeline pattern across 3 separate always_ff blocks.
    // d → stage1 → stage2 → stage3 should take 3 clock cycles.
    fn test_nba_separate_blocks_pipeline(sim) {
        @ignore_on(sv);
        @case "nba_cross_block::test_nba_separate_blocks_pipeline";
    }

    // Test that the order of always_ff blocks in source code does not matter.
    // Reverse the pipeline order (q first, stage1 last) — same behavior expected.
    fn test_nba_separate_blocks_pipeline_reversed(sim) {
        @case "nba_cross_block::test_nba_separate_blocks_pipeline_reversed";
    }
}
