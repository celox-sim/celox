//! Constructs commonly found in synthesizable RTL, checked against a software model.

use super::*;

sv_backends! {
    fn type_parameter_bindings_preserve_alias_dimensions(sim) { @case "synthesizable::type_parameter_bindings_preserve_alias_dimensions"; }
    fn type_parameter_bindings_preserve_packed_members(sim) { @case "synthesizable::type_parameter_bindings_preserve_packed_members"; }
    fn type_parameter_bindings_use_caller_scope(sim) { @case "synthesizable::type_parameter_bindings_use_caller_scope"; }
    fn casex_ignores_unknown_selector_bits(sim) { @case "synthesizable::casex_ignores_unknown_selector_bits"; }
    fn named_calls_preserve_defaults_and_declaration_scope(sim) { @case "synthesizable::named_calls_preserve_defaults_and_declaration_scope"; }
    fn named_calls_in_constant_contexts(sim) { @case "synthesizable::named_calls_in_constant_contexts"; }
    fn named_calls_respect_generate_signatures(sim) { @case "synthesizable::named_calls_respect_generate_signatures"; }
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

sv_backends! {
    fn block_local_names_shadow_other_signals(sim) {
        @case "synthesizable::block_local_names_shadow_other_signals";
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
    // IEEE 1800-2023 9.2.2.2: always_comb may not suspend.
    assert!(
        error.contains("timed task call in an always_comb body"),
        "{error}"
    );
}

#[test]
fn unsupported_constructs_name_their_tracking_issue() {
    {
        let (source, issue) = (
            "module Top(input logic a, b, output logic y); tran g(y, a); endmodule",
            457,
        );
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

sv_backends! {
    fn continuous_assignment_targets_declare_implicit_nets(sim) {
        @case "synthesizable::continuous_assignment_targets_declare_implicit_nets";
    }
}

// `default_nettype none` disables implicit nets (IEEE 1800-2023 22.8), so an
// undeclared continuous assignment target stays an error.
#[test]
fn default_nettype_none_rejects_undeclared_assignment_targets() {
    let source = "`default_nettype none
        module Top(input logic a, output logic y);
            assign x = a;
            assign y = x;
        endmodule
        `default_nettype wire";
    let error = Simulator::from_sv_sources(vec![(source, Path::new("none.sv"))], "Top")
        .build_cranelift()
        .expect_err("an undeclared target under `default_nettype none` is rejected")
        .to_string();
    assert!(error.contains("undeclared `x`"), "{error}");
}

// A constant function selects the bits of its variables by their declared
// indices (IEEE 1800-2023 7.4.1, 11.5.1, 13.4.3), as the same call at run time
// does: of an ascending range, as fpnew's format masks `logic [0:N-1]`, and of
// a range whose right bound is not zero, by bit-selects, part-selects and
// indexed part-selects that read and write.
#[test]
fn constant_functions_select_declared_indices() {
    let source = r#"
        module Top;
            function automatic int unsigned loop_index(logic [0:3] cfg);
                int unsigned r = 0;
                for (int i = 0; i < 4; i++) if (cfg[i]) r = r + (1 << i);
                return r;
            endfunction
            function automatic int unsigned const_index(logic [0:3] cfg);
                return {cfg[0], cfg[1], cfg[2], cfg[3]};
            endfunction
            function automatic int unsigned part(logic [0:3] cfg);
                return cfg[1:2];
            endfunction
            function automatic int unsigned indexed(logic [0:3] cfg, int i);
                return cfg[i +: 2];
            endfunction
            function automatic int unsigned shifted(logic [3:0] x);
                logic [7:4] v;
                v = x;
                return v[7:6];
            endfunction
            function automatic int unsigned write_ascending();
                logic [0:3] v;
                v = '0;
                v[0] = 1'b1;
                v[2:3] = 2'b01;
                return v;
            endfunction
            function automatic int unsigned write_shifted();
                logic [7:4] v;
                v = '0;
                v[4] = 1'b1;
                v[7] = 1'b1;
                return v;
            endfunction
            function automatic int unsigned max_width(logic [0:3] cfg);
                int unsigned r = 0;
                for (int i = 0; i < 4; i++) if (cfg[i] && (8 << i) > r) r = 8 << i;
                return r;
            endfunction
            localparam int unsigned A = loop_index(4'b0011);
            localparam int unsigned B = const_index(4'b0011);
            localparam int unsigned C = part(4'b0011);
            localparam int unsigned D = indexed(4'b0011, 2);
            localparam int unsigned E = shifted(4'b1001);
            localparam int unsigned F = write_ascending();
            localparam int unsigned G = write_shifted();
            localparam int unsigned W = max_width(4'b0011);
            logic [W-1:0] widest;
            initial begin
                $display("%0d %0d %0d %0d %0d %0d %0d %0d", A, B, C, D, E, F, G, $bits(widest));
                $display("%0d %0d %0d %0d %0d %0d %0d %0d", loop_index(4'b0011),
                    const_index(4'b0011), part(4'b0011), indexed(4'b0011, 2), shifted(4'b1001),
                    write_ascending(), write_shifted(), max_width(4'b0011));
            end
        endmodule
    "#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("declared_indices.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    sim.run_until(1).unwrap();
    let displayed: Vec<_> = sim
        .drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => Some(String::from_utf8(message).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(displayed, ["12 3 1 3 2 9 9 64"; 2]);
}
