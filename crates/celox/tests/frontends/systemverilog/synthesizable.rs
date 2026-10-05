//! Constructs commonly found in synthesizable RTL, checked against a software model.

use super::*;

sv_backends! {
    fn enum_members_without_values_follow_their_predecessor(sim) {
        @case "synthesizable::enum_members_without_values_follow_their_predecessor";
    }

    fn always_star_and_edge_sensitive_always_match_the_systemverilog_keywords(sim) {
        @case "synthesizable::always_star_and_edge_sensitive_always_match_the_systemverilog_keywords";
    }

    fn net_declaration_assignment_drives_the_net(sim) {
        @case "synthesizable::net_declaration_assignment_drives_the_net";
    }

    fn assigning_the_function_name_returns_its_last_value(sim) {
        @case "synthesizable::assigning_the_function_name_returns_its_last_value";
    }

    fn casez_and_casex_treat_wildcard_bits_as_dont_care(sim) {
        @case "synthesizable::casez_and_casex_treat_wildcard_bits_as_dont_care";
    }

    fn inside_matches_values_ranges_and_wildcards(sim) {
        @case "synthesizable::inside_matches_values_ranges_and_wildcards";
    }

    fn positional_ports_and_parameters_bind_in_declaration_order(sim) {
        @case "synthesizable::positional_ports_and_parameters_bind_in_declaration_order";
    }

    fn instance_arrays_broadcast_and_slice_connections(sim) {
        @case "synthesizable::instance_arrays_broadcast_and_slice_connections";
    }

    fn instance_arrays_with_ascending_range(sim) {
        @case "synthesizable::instance_arrays_with_ascending_range";
    }

    fn instance_arrays_with_non_zero_based_range(sim) {
        @case "synthesizable::instance_arrays_with_non_zero_based_range";
    }

    fn instance_array_elements_are_numbered_by_declared_index(sim) {
        @setup {
            let source = r#"
                module Inv(input logic [3:0] a, output logic [3:0] y);
                    assign y = ~a;
                endmodule
                module Top(input logic [7:0] a, output logic [7:0] y);
                    Inv u[3:2](.a(a), .y(y));
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("nz.sv"))], "Top");
        let hierarchy = sim.named_hierarchy();
        let (_, elements) = hierarchy
            .children
            .iter()
            .find(|(name, _)| name == "u")
            .expect("instance array `u`");
        let indices: Vec<_> = elements.iter().map(|element| element.index).collect();
        assert_eq!(indices, [2, 3]);
    }

    fn instance_array_unpacked_connection_descending_to_descending(sim) {
        @case "synthesizable::instance_array_unpacked_connection_descending_to_descending";
    }

    fn instance_array_unpacked_connection_descending_to_ascending(sim) {
        @case "synthesizable::instance_array_unpacked_connection_descending_to_ascending";
    }

    fn instance_array_unpacked_connection_ascending_to_descending(sim) {
        @case "synthesizable::instance_array_unpacked_connection_ascending_to_descending";
    }

    fn instance_array_unpacked_connection_offset_ranges(sim) {
        @case "synthesizable::instance_array_unpacked_connection_offset_ranges";
    }

    fn typedef_unpacked_arrays_are_not_instance_arrays(sim) {
        @case "synthesizable::typedef_unpacked_arrays_are_not_instance_arrays";
    }

    fn instance_arrays_reject_a_fill_literal_tie_off(sim) {
        @case "synthesizable::instance_arrays_reject_a_fill_literal_tie_off";
    }

    fn instance_arrays_reject_an_unsized_constant_tie_off(sim) {
        @case "synthesizable::instance_arrays_reject_an_unsized_constant_tie_off";
    }

    fn single_element_instance_arrays_are_indexed_in_the_hierarchy(sim) {
        @setup {
            let source = r#"
                module Inv(input logic [3:0] a, output logic [3:0] y);
                    assign y = ~a;
                endmodule
                module Top(input logic [3:0] a, output logic [3:0] y);
                    Inv u[0:0](.a(a), .y(y));
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("one.sv"))], "Top");
        let hierarchy = sim.named_hierarchy();
        let (_, elements) = hierarchy
            .children
            .iter()
            .find(|(name, _)| name == "u")
            .expect("instance array `u`");
        assert_eq!(elements.len(), 1);
        assert!(elements[0].indexed);
        assert_eq!(elements[0].index, 0);
    }

    fn exponentiation_works_in_constant_expressions(sim) {
        @case "synthesizable::exponentiation_works_in_constant_expressions";
    }

    fn packages_provide_types_parameters_functions_and_enums(sim) {
        @case "synthesizable::packages_provide_types_parameters_functions_and_enums";
    }

    fn struct_assignment_patterns_follow_the_target_layout(sim) {
        @case "synthesizable::struct_assignment_patterns_follow_the_target_layout";
    }

    fn function_output_and_inout_arguments_are_written_back(sim) {
        @case "synthesizable::function_output_and_inout_arguments_are_written_back";
    }

    fn tasks_without_timing_write_their_outputs(sim) {
        @case "synthesizable::tasks_without_timing_write_their_outputs";
    }

    fn break_and_continue_end_unrolled_loop_iterations(sim) {
        @case "synthesizable::break_and_continue_end_unrolled_loop_iterations";
    }

    fn type_parameters_are_bound_by_instantiations(sim) {
        @case "synthesizable::type_parameters_are_bound_by_instantiations";
    }

    fn block_locals_and_dependent_assignments_accumulate(sim) {
        @case "synthesizable::block_locals_and_dependent_assignments_accumulate";
    }
}

#[test]
fn block_local_names_may_not_shadow_other_signals() {
    for source in [
        "module Top(input logic a, output logic t); \
         always_comb begin logic t; t = a; end endmodule",
        "module Top(input logic a, output logic y, z); \
         always_comb begin logic t; t = a; y = t; end \
         always_comb begin logic t; t = ~a; z = t; end endmodule",
    ] {
        let error = Simulator::from_sv_sources(vec![(source, Path::new("shadow.sv"))], "Top")
            .build_cranelift()
            .expect_err("a shadowing block-local name must be rejected")
            .to_string();
        assert!(
            error.contains("duplicate internal signal")
                || error.contains("duplicate port or signal name"),
            "{error}"
        );
    }
}

#[test]
fn task_timing_control_is_rejected() {
    let source = "module Top(input logic [7:0] a, output logic [7:0] y); \
        task automatic t(input logic [7:0] x, output logic [7:0] z); #5 z = x; endtask \
        always_comb t(a, y); endmodule";
    let error = Simulator::from_sv_sources(vec![(source, Path::new("task_delay.sv"))], "Top")
        .build_cranelift()
        .expect_err("a task with a delay must be rejected")
        .to_string();
    assert!(error.contains("Unsupported"), "{error}");
}

#[test]
fn unsupported_constructs_name_their_tracking_issue() {
    for (source, issue) in [
        (
            "module Top(input logic clk, input logic [7:0] a, output logic [7:0] q); \
             logic [7:0] t; always_ff @(posedge clk) begin t = a; q <= t; end endmodule",
            421,
        ),
        (
            "module Top(input logic a, b, output logic y); and g(y, a, b); endmodule",
            457,
        ),
    ] {
        let error = Simulator::from_sv_sources(vec![(source, Path::new("tracking.sv"))], "Top")
            .build_cranelift()
            .expect_err("the construct must be rejected")
            .to_string();
        assert!(
            error.contains(&format!("[tracking issue #{issue}]")),
            "{error}"
        );
    }
}
