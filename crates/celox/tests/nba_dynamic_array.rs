#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    fn test_subbyte_arithmetic_padding_does_not_corrupt_concat(sim) {
        @ignore_on(sv);
        @case "nba_dynamic_array::test_subbyte_arithmetic_padding_does_not_corrupt_concat";
    }

    fn test_child_dynamic_ff_read_reaches_parent_after_same_edge_enable(sim) {
        @case "nba_dynamic_array::test_child_dynamic_ff_read_reaches_parent_after_same_edge_enable";
    }

    fn test_static_ff_writes_are_applied_after_all_rhs_evaluation(sim) {
        @ignore_on(sv);
        @case "nba_dynamic_array::test_static_ff_writes_are_applied_after_all_rhs_evaluation";
    }

    // Separate always_ff blocks on the same clock sample the same pre-edge
    // state. A dynamic array write in one block must not become visible to a
    // read in another block until all blocks for the edge have evaluated.
    fn test_dynamic_array_write_is_deferred_across_ff_blocks(sim) {
        @ignore_on(sv);
        @case "nba_dynamic_array::test_dynamic_array_write_is_deferred_across_ff_blocks";
    }

    fn test_partial_sparse_chunks_do_not_overlap_adjacent_variables(sim) {
        @ignore_on(sv);
        @case "nba_dynamic_array::test_partial_sparse_chunks_do_not_overlap_adjacent_variables";
    }

    fn test_always_ff_let_bindings_are_visible_immediately(sim) {
        @ignore_on(sv);
        @case "nba_dynamic_array::test_always_ff_let_bindings_are_visible_immediately";
    }

    fn test_wide_dynamic_ff_checkpoint_round_trip(sim) {
        @case "nba_dynamic_array::test_wide_dynamic_ff_checkpoint_round_trip";
    }

    fn test_unaligned_309_bit_dynamic_ff_round_trip(sim) {
        @ignore_on(wasm);
        @case "nba_dynamic_array::test_unaligned_309_bit_dynamic_ff_round_trip";
    }

    fn test_packed_rat_checkpoint_round_trip(sim) {
        @ignore_on(sv);
        @case "nba_dynamic_array::test_packed_rat_checkpoint_round_trip";
    }

    fn test_dynamic_ff_array_partial_squash_preserves_head_and_branch(sim) {
        @ignore_on(sv);
        @case "nba_dynamic_array::test_dynamic_ff_array_partial_squash_preserves_head_and_branch";
    }

    fn test_line_write_loop_updates_large_sparse_ff_array(sim) {
        @ignore_on(sv);
        @case "nba_dynamic_array::test_line_write_loop_updates_large_sparse_ff_array";
    }

}
