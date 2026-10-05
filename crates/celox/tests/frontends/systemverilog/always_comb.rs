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
