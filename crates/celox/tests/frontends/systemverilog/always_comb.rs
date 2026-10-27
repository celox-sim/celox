sv_backends! {
    fn simulates_systemverilog_always_comb_dependency(sim) {
        @case "always_comb::simulates_systemverilog_always_comb_dependency";
    }
}

sv_backends! {
    fn preserves_intervening_reads_when_merging_conditional_writes(sim) {
        @case "always_comb::preserves_intervening_reads_when_merging_conditional_writes";
    }
}

sv_backends! {
    fn settles_feedback_through_disjoint_bits_of_one_variable(sim) {
        @case "always_comb::settles_feedback_through_disjoint_bits_of_one_variable";
    }
}

sv_backends! {
    fn orders_dynamic_array_read_after_its_writer(sim) {
        @case "always_comb::orders_dynamic_array_read_after_its_writer";
    }
}

sv_backends! {
    fn explicit_list_of_every_read_settles_like_a_wildcard(sim) {
        @case "always_comb::explicit_list_of_every_read_settles_like_a_wildcard";
    }
}

sv_backends! {
    fn two_state_sensitivity_lists_run_at_time_zero(sim) {
        @case "always_comb::two_state_sensitivity_lists_run_at_time_zero";
    }
}

sv_backends! {
    fn two_state_other_sensitivity_lists_run_at_time_zero(sim) {
        @case "always_comb::two_state_other_sensitivity_lists_run_at_time_zero";
    }
}

sv_backends! {
    fn covering_sensitivity_lists_settle_like_a_wildcard(sim) {
        @case "always_comb::covering_sensitivity_lists_settle_like_a_wildcard";
    }
    fn covering_sensitivity_lists_settle_like_a_wildcard_in_four_state(sim) {
        @case "always_comb::covering_sensitivity_lists_settle_like_a_wildcard_in_four_state";
    }
    fn multi_store_blocks_with_covering_lists_stay_processes(sim) {
        @case "always_comb::multi_store_blocks_with_covering_lists_stay_processes";
    }
}

sv_backends! {
    fn settles_feedback_through_chained_concat(sim) {
        @case "always_comb::settles_feedback_through_chained_concat";
    }
    fn settles_feedback_through_chained_mux(sim) {
        @case "always_comb::settles_feedback_through_chained_mux";
    }
    fn settles_feedback_through_chained_qspi(sim) {
        @case "always_comb::settles_feedback_through_chained_qspi";
    }
}

sv_backends! {
    fn settles_feedback_through_chained_concat_four_state(sim) {
        @case "always_comb::settles_feedback_through_chained_concat_four_state";
    }
    fn settles_feedback_through_chained_mux_four_state(sim) {
        @case "always_comb::settles_feedback_through_chained_mux_four_state";
    }
    fn settles_feedback_through_chained_qspi_four_state(sim) {
        @case "always_comb::settles_feedback_through_chained_qspi_four_state";
    }
}
