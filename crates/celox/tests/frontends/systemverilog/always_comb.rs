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
