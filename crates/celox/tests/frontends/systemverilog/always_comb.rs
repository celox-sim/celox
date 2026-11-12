use super::*;

sv_backends! {
    fn complete_case_after_local_mux(sim) { @case "always_comb::complete_case_after_local_mux"; }

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
    fn wildcard_ignores_variables_read_inside_called_functions(sim) {
        @case "always_comb::wildcard_ignores_variables_read_inside_called_functions";
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

sv_backends! {
    fn large_sensitivity_list_preserves_initial_wait_and_four_state_changes(sim) {
        @case "always_comb::large_sensitivity_list_preserves_initial_wait_and_four_state_changes";
    }
}

sv_backends! {
    // A loop whose condition depends on the values of the process, such as
    // one that stops at the first sign change, runs in a continuous
    // assignment's function and in an `always_comb` as the values make it
    // stop (IEEE 1800-2023 12.7).
    fn loops_with_value_dependent_conditions(sim) {
        @setup {
            let source = r#"
            module Top(input logic [15:0] a, output logic [3:0] lead, output logic [4:0] ones);
              function [3:0] lead_of(input [15:0] v);
                reg [3:0] i;
                reg done;
                begin
                  done = 0;
                  lead_of = 15;
                  for (i = 14; done == 0; i = i - 1) begin
                    if (v[i + 1] != v[i]) begin
                      lead_of = 14 - i;
                      done = 1;
                    end else if (i == 0) done = 1;
                  end
                end
              endfunction
              assign lead = lead_of(a);
              always_comb begin
                int k;
                ones = 0;
                k = 0;
                while (k < 16 && a[k]) begin
                  ones = ones + 1;
                  k = k + 1;
                end
              end
            endmodule"#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("value_loops.sv"))], "Top");
        let a = sim.signal("a");
        for (value, lead, ones) in [
            (0x0f00u16, 3u8, 0u8),
            (0xfff7, 11, 3),
            (0x0000, 15, 0),
            (0x7fff, 0, 15),
            (0xffff, 15, 16),
        ] {
            sim.modify(|io| io.set(a, value)).unwrap();
            assert_eq!(sim.get(sim.signal("lead")), BigUint::from(lead), "{value:#x}");
            assert_eq!(sim.get(sim.signal("ones")), BigUint::from(ones), "{value:#x}");
        }
    }
}
