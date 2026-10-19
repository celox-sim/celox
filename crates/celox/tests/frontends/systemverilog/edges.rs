//! Edges of clocks and resets the design drives itself, in four-state
//! simulation (IEEE 1800-2023 9.4.2, Table 9-2).

sv_backends! {
    fn register_clock_edges_follow_table_9_2(sim) {
        @case "edges::register_clock_edges_follow_table_9_2";
    }

    fn comb_clock_edges_follow_table_9_2(sim) {
        @case "edges::comb_clock_edges_follow_table_9_2";
    }

    fn internal_async_reset_edges_follow_table_9_2(sim) {
        @case "edges::internal_async_reset_edges_follow_table_9_2";
    }

    fn gated_clock_edges_from_unknown_levels(sim) {
        @case "edges::gated_clock_edges_from_unknown_levels";
    }

    fn register_clock_two_state_edges(sim) {
        @case "edges::register_clock_two_state_edges";
    }
}
