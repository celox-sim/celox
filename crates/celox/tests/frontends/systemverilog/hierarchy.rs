use super::*;

sv_backends! {
    fn array_parameter_elements_select_conditional_whole_arrays(sim) {
        @case "hierarchy::array_parameter_elements_select_conditional_whole_arrays";
    }
    fn array_parameter_elements_preserve_wide_specializations(sim) {
        @case "hierarchy::array_parameter_elements_preserve_wide_specializations";
    }
    fn array_parameter_elements_forward_generated_values(sim) {
        @case "hierarchy::array_parameter_elements_forward_generated_values";
    }
    fn array_parameter_elements_keep_signed_element_types(sim) {
        @case "hierarchy::array_parameter_elements_keep_signed_element_types";
    }
    fn array_parameter_elements_select_package_multidimensional_values(sim) {
        @case "hierarchy::array_parameter_elements_select_package_multidimensional_values";
    }
    fn array_parameter_elements_preserve_four_state_and_conversion(sim) {
        @case "hierarchy::array_parameter_elements_preserve_four_state_and_conversion";
    }

    fn port_pattern_constant_unpacked_defaults(sim) {
        @case "hierarchy::port_pattern_constant_unpacked_defaults";
    }

    fn port_pattern_constant_literal_defaults(sim) {
        @case "hierarchy::port_pattern_constant_literal_defaults";
    }

    fn port_pattern_constant_mixed_state_defaults(sim) {
        @case "hierarchy::port_pattern_constant_mixed_state_defaults";
    }

    fn port_pattern_constant_aggregate_defaults(sim) {
        @case "hierarchy::port_pattern_constant_aggregate_defaults";
    }
    fn port_pattern_default_bounds_require_matching_types(sim) {
        @case "hierarchy::port_pattern_default_bounds_require_matching_types";
    }

    fn port_pattern_review_packed_value_types(sim) {
        @case "hierarchy::port_pattern_review_packed_value_types";
    }
    fn port_pattern_review_conditional_structs(sim) {
        @case "hierarchy::port_pattern_review_conditional_structs";
    }
    fn port_pattern_review_subarray_items(sim) {
        @case "hierarchy::port_pattern_review_subarray_items";
    }

    fn port_pattern_review_aggregate_defaults(sim) {
        @case "hierarchy::port_pattern_review_aggregate_defaults";
    }

    fn port_pattern_review_mixed_state_structs(sim) {
        @case "hierarchy::port_pattern_review_mixed_state_structs";
    }
    fn port_pattern_review_member_key_collisions(sim) {
        @case "hierarchy::port_pattern_review_member_key_collisions";
    }
    fn port_pattern_review_packed_struct_arrays(sim) {
        @case "hierarchy::port_pattern_review_packed_struct_arrays";
    }
    fn port_pattern_review_interface_inputs(sim) {
        @case "hierarchy::port_pattern_review_interface_inputs";
    }

    fn port_patterns_preserve_four_state_elements(sim) {
        @case "hierarchy::port_patterns_preserve_four_state_elements";
    }
    fn port_patterns_reject_incorrect_element_counts(sim) {
        @case "hierarchy::port_patterns_reject_incorrect_element_counts";
    }

    fn port_patterns_use_specialized_formal_shapes(sim) {
        @case "hierarchy::port_patterns_use_specialized_formal_shapes";
    }
    fn port_patterns_keep_caller_values_and_generated_names(sim) {
        @case "hierarchy::port_patterns_keep_caller_values_and_generated_names";
    }
    fn port_patterns_bind_nested_structure_elements(sim) {
        @case "hierarchy::port_patterns_bind_nested_structure_elements";
    }

    fn named_array_parameters_forward_whole_values(sim) {
        @case "hierarchy::named_array_parameters_forward_whole_values";
    }

    fn named_array_parameters_forward_package_subarrays(sim) {
        @case "hierarchy::named_array_parameters_forward_package_subarrays";
    }

    fn named_array_parameters_follow_caller_specialization(sim) {
        @case "hierarchy::named_array_parameters_follow_caller_specialization";
    }
    fn named_array_parameters_keep_four_state_values(sim) {
        @case "hierarchy::named_array_parameters_keep_four_state_values";
    }

    fn sized_parameter_actuals_follow_parent_specialization(sim) {
        @case "hierarchy::sized_parameter_actuals_follow_parent_specialization";
    }

    fn generated_parameter_part_select_actuals(sim) {
        @case "hierarchy::generated_parameter_part_select_actuals";
    }

    fn conditional_parameter_actuals_follow_parent_specialization(sim) {
        @case "hierarchy::conditional_parameter_actuals_follow_parent_specialization";
    }

    fn concatenated_parameter_actuals_use_each_parent_specialization(sim) {
        @case "hierarchy::concatenated_parameter_actuals_use_each_parent_specialization";
    }

    fn input_port_defaults_use_child_scope_and_only_omitted_connections(sim) {
        @case "hierarchy::input_port_defaults_use_child_scope_and_only_omitted_connections";
    }

    fn bits_query_parameter_overrides_follow_caller_specialization(sim) {
        @case "hierarchy::bits_query_parameter_overrides_follow_caller_specialization";
    }

    fn bits_query_parameter_overrides(sim) {
        @case "hierarchy::bits_query_parameter_overrides";
    }

    fn parameter_patterns_keep_original_declarations_and_isolated_bindings(sim) { @case "hierarchy::parameter_patterns_keep_original_declarations_and_isolated_bindings"; }
    fn parameter_patterns_preserve_enclosing_expressions(sim) { @case "hierarchy::parameter_patterns_preserve_enclosing_expressions"; }
    fn widens_small_parameters_preserving_four_state_bits(sim) {
        @case "hierarchy::widens_small_parameters_preserving_four_state_bits";
    }
    fn widens_small_parameters_into_child_int_outputs(sim) {
        @case "hierarchy::widens_small_parameters_into_child_int_outputs";
    }
    fn parameter_patterns_preserve_packed_member_states(sim) { @case "hierarchy::parameter_patterns_preserve_packed_member_states"; }
    fn parameter_patterns_transfer_packed_values(sim) { @case "hierarchy::parameter_patterns_transfer_packed_values"; }
    fn parameter_patterns_override_scalar_defaults(sim) { @case "hierarchy::parameter_patterns_override_scalar_defaults"; }
    fn parameter_patterns_evaluate_caller_constant_functions(sim) { @case "hierarchy::parameter_patterns_evaluate_caller_constant_functions"; }
    fn parameter_patterns_override_body_declarations(sim) { @case "hierarchy::parameter_patterns_override_body_declarations"; }
    fn parameter_patterns_keep_fill_and_arithmetic_context(sim) {
        @case "hierarchy::parameter_patterns_keep_fill_and_arithmetic_context";
    }
    fn parameter_patterns_bind_caller_types(sim) {
        @case "hierarchy::parameter_patterns_bind_caller_types";
    }
    fn parameter_patterns_bind_nested_caller_types(sim) {
        @case "hierarchy::parameter_patterns_bind_nested_caller_types";
    }
    fn parameter_patterns_keep_unknown_fill(sim) {
        @case "hierarchy::parameter_patterns_keep_unknown_fill";
    }

    fn drives_omitted_and_open_child_inputs_with_z(sim) {
        @case "hierarchy::drives_omitted_and_open_child_inputs_with_z";
    }

    fn treats_child_outputs_as_explicit_net_drivers(sim) {
        @case "hierarchy::treats_child_outputs_as_explicit_net_drivers";
    }

    fn simulates_systemverilog_named_port_hierarchy(sim) {
        @case "hierarchy::simulates_systemverilog_named_port_hierarchy";
    }

    fn simulates_systemverilog_hierarchy_through_internal_signal(sim) {
        @case "hierarchy::simulates_systemverilog_hierarchy_through_internal_signal";
    }

    fn simulates_veryl_generated_style_gray_encoder_hierarchy(sim) {
        @case "hierarchy::simulates_veryl_generated_style_gray_encoder_hierarchy";
    }

    fn simulates_parameter_specialized_systemverilog_hierarchy(sim) {
        @case "hierarchy::simulates_parameter_specialized_systemverilog_hierarchy";
    }

    fn simulates_systemverilog_hierarchical_always_ff(sim) {
        @case "hierarchy::simulates_systemverilog_hierarchical_always_ff";
    }

    fn simulates_systemverilog_hierarchical_always_ff_with_constant_clear(sim) {
        @case "hierarchy::simulates_systemverilog_hierarchical_always_ff_with_constant_clear";
    }

    fn settles_feedback_through_disjoint_bits_of_child_input(sim) {
        @case "hierarchy::settles_feedback_through_disjoint_bits_of_child_input";
    }

    fn settles_feedback_beside_dynamic_child_reader(sim) {
        @case "hierarchy::settles_feedback_beside_dynamic_child_reader";
    }

    fn simulates_veryl_generated_countones_sv(sim) {
        @setup {
    let sv = include_str!("../../../testdata/verilator/Countones.sv");
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("Countones.sv"))], "Top");

    let i_data = sim.signal("i_data");
    let o_ones = sim.signal("o_ones");

    for value in [0u64, 1, 0xffff, 0xdead_beef, u64::MAX] {
        sim.modify(|io| io.set(i_data, value)).unwrap();
        assert_eq!(sim.get(o_ones), value.count_ones().into());
    }
    }

    fn simulates_veryl_generated_onehot_sv(sim) {
        @setup {
    let sv = include_str!("../../../testdata/verilator/Onehot.sv");
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("Onehot.sv"))], "Top");

    let i_data = sim.signal("i_data");
    let o_onehot = sim.signal("o_onehot");
    let o_zero = sim.signal("o_zero");

    for value in [0u64, 1, 2, 3, 1u64 << 63, (1u64 << 63) | 1] {
        sim.modify(|io| io.set(i_data, value)).unwrap();
        assert_eq!(sim.get(o_onehot), u8::from(value.count_ones() == 1).into());
        assert_eq!(sim.get(o_zero), u8::from(value == 0).into());
    }
    }

    fn simulates_veryl_generated_edge_detector_sv(sim) {
        @setup {
    let sv = include_str!("../../../testdata/verilator/EdgeDetector.sv");
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("EdgeDetector.sv"))], "Top");

    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_data = sim.signal("i_data");
    let o_edge = sim.signal("o_edge");
    let o_posedge = sim.signal("o_posedge");
    let o_negedge = sim.signal("o_negedge");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_data, 0u32);
    }).unwrap();
    sim.tick(clk).unwrap();

    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();

    sim.modify(|io| io.set(i_data, 1u32)).unwrap();
    assert_eq!(sim.get(o_edge), 1u8.into());
    assert_eq!(sim.get(o_posedge), 1u8.into());
    assert_eq!(sim.get(o_negedge), 0u8.into());

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_edge), 0u8.into());

    sim.modify(|io| io.set(i_data, 0u32)).unwrap();
    assert_eq!(sim.get(o_edge), 1u8.into());
    assert_eq!(sim.get(o_posedge), 0u8.into());
    assert_eq!(sim.get(o_negedge), 1u8.into());

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_edge), 0u8.into());
    }

    fn simulates_veryl_generated_std_counter_sv(sim) {
        @setup {
    let sv = include_str!("../../../testdata/verilator/StdCounter.sv");
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("StdCounter.sv"))], "Top");

    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_up = sim.signal("i_up");
    let o_count = sim.signal("o_count");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_up, 0u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_count), 0u32.into());

    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(i_up, 1u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_count), 1u32.into());

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_count), 2u32.into());
    }

    fn simulates_veryl_generated_gray_counter_sv(sim) {
        @setup {
    let sv = include_str!("../../../testdata/verilator/GrayCounter.sv");
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("GrayCounter.sv"))], "Top");

    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_up = sim.signal("i_up");
    let o_count = sim.signal("o_count");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_up, 0u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_count), 0u32.into());

    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(i_up, 1u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_count), 1u32.into());

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_count), 3u32.into());
    }

    fn simulates_veryl_generated_lfsr_sv(sim) {
        @setup {
    let sv = include_str!("../../../testdata/verilator/Lfsr.sv");
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("Lfsr.sv"))], "Top");

    let clk = sim.event("clk");
    let i_en = sim.signal("i_en");
    let i_set = sim.signal("i_set");
    let i_setval = sim.signal("i_setval");
    let o_val = sim.signal("o_val");

    sim.modify(|io| {
        io.set(i_en, 1u8);
        io.set(i_set, 1u8);
        io.set(i_setval, 1u32);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_val), 1u32.into());

    sim.modify(|io| io.set(i_set, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_val), 0x8000_0057u32.into());
    }

}

#[test]
fn simulates_veryl_generated_gray_codec_sv_smoke() {
    let sv = include_str!("../../../testdata/verilator/GrayCodec.sv");
    let mut sim = Simulator::from_sv_sources(vec![(sv, Path::new("GrayCodec.sv"))], "Top")
        .build_native()
        .unwrap();
    let i_bin = sim.signal("i_bin");
    let o_gray = sim.signal("o_gray");
    let o_bin = sim.signal("o_bin");

    sim.modify(|io| io.set(i_bin, 0xdead_beefu32)).unwrap();

    assert_eq!(sim.get(o_gray), 0xb1fb_6198u32.into());
    assert_eq!(sim.get(o_bin), 0xdead_beefu32.into());
}

#[test]
fn builds_veryl_generated_verilator_sv_smoke() {
    for (name, sv) in [
        (
            "Countones.sv",
            include_str!("../../../testdata/verilator/Countones.sv"),
        ),
        (
            "Fifo.sv",
            include_str!("../../../testdata/verilator/Fifo.sv"),
        ),
        (
            "EdgeDetector.sv",
            include_str!("../../../testdata/verilator/EdgeDetector.sv"),
        ),
        (
            "GrayCodec.sv",
            include_str!("../../../testdata/verilator/GrayCodec.sv"),
        ),
        (
            "GrayCounter.sv",
            include_str!("../../../testdata/verilator/GrayCounter.sv"),
        ),
        (
            "Lfsr.sv",
            include_str!("../../../testdata/verilator/Lfsr.sv"),
        ),
        (
            "Onehot.sv",
            include_str!("../../../testdata/verilator/Onehot.sv"),
        ),
        (
            "StdCounter.sv",
            include_str!("../../../testdata/verilator/StdCounter.sv"),
        ),
    ] {
        Simulator::from_sv_sources(vec![(sv, Path::new(name))], "Top")
            .build_native()
            .unwrap_or_else(|err| panic!("failed to build {name}: {err:?}"));
    }
}

fn assert_combinational_loop(source: &str) {
    match Simulator::from_sv_sources(vec![(source, Path::new("loop.sv"))], "Top").build_cranelift()
    {
        Ok(_) => panic!("a genuine combinational loop must be rejected:\n{source}"),
        Err(error) => assert!(
            matches!(
                error.kind(),
                celox::SimulatorErrorKind::SIRParser(
                    celox::ParserError::Scheduler(celox::SchedulerError::CombinationalLoop { .. })
                        | celox::ParserError::SchedulerWithLocation {
                            error: celox::SchedulerError::CombinationalLoop { .. },
                            ..
                        }
                )
            ),
            "expected a combinational loop, got: {error:?}"
        ),
    }
}

// Bit-precise scheduling must still reject feedback through the same bit.
#[test]
fn rejects_feedback_through_the_same_bit_of_one_variable() {
    assert_combinational_loop(
        r#"
        module Top(input logic inp, output logic out);
            logic [1:0] v;
            logic lo;
            assign lo = v[0];
            assign v = {inp, lo};
            assign out = v[0];
        endmodule
    "#,
    );
}

#[test]
fn rejects_feedback_through_the_same_bit_of_child_input() {
    assert_combinational_loop(
        r#"
        module Child(input logic [1:0] a, output logic lo);
            assign lo = a[0];
        endmodule
        module Top(input logic inp, output logic out);
            logic [1:0] v;
            logic lo;
            Child c(.a(v), .lo(lo));
            assign v = {inp, lo};
            assign out = v[1];
        endmodule
    "#,
    );
}

sv_backends! {
    fn literal_parameter_api_preserves_width_sign_and_unknowns(sim) {
        @setup {
            let source = r#"
                module Top #(parameter TEXT="default", parameter NEG=0,
                    parameter logic [57:0] FILL=0, parameter logic [3:0] UNKNOWN=0,
                    parameter logic [64:0] WIDE=0, parameter FINAL=0,
                    parameter logic [57:0] X=0, parameter logic [57:0] Z=0,
                    parameter logic [57:0] COPY=FILL)(output logic matched);
                    assign matched=(TEXT=="XILINX") && ($bits(TEXT)==48)
                        && (NEG==-8) && (NEG<0) && ($bits(NEG)==32)
                        && (FILL==58'h3ffffffffffffff) && (UNKNOWN===4'b10xz)
                        && (WIDE==65'h10000000000000001) && (FINAL==7)
                        && (COPY==58'h3ffffffffffffff)
                        && (X===58'bx) && (Z===58'bz);
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("parameter_api.sv"))], "Top")
            .param_literal("TEXT", "\"XILINX\"")
            .param("NEG", 3).param_literal("NEG", "-8")
            .param_literal("FILL", "'1").param_literal("UNKNOWN", "4'b10xz")
            .param_literal("WIDE", "65'h10000000000000001")
            .param_literal("FINAL", "-1").param("FINAL", 7)
            .param_literal("X", "'x").param_literal("Z", "'z")
            .four_state(true).opt_level(celox::OptLevel::O2);
        assert_eq!(sim.get(sim.signal("matched")), BigUint::from(1u8));
    }
}

#[test]
fn rejects_incompatible_named_array_parameter_overrides() {
    for declaration in [
        "logic [7:0] TABLE[3] = '{1, 2, 3}",
        "logic [3:0] TABLE[2] = '{1, 2}",
        "logic signed [7:0] TABLE[2] = '{1, 2}",
        "bit [7:0] TABLE[2] = '{1, 2}",
        "logic [7:0] TABLE[1][2] = '{'{1, 2}}",
    ] {
        let source = format!(
            r#"
            module Child #(parameter logic [7:0] Q[2] = '{{0, 0}})(output logic [7:0] y);
                assign y = Q[0];
            endmodule
            module Top(output logic [7:0] y);
                localparam {declaration};
                Child #(.Q(TABLE)) child(y);
            endmodule
        "#
        );
        let error = match Simulator::from_sv_sources(
            vec![(&source, Path::new("array_override.sv"))],
            "Top",
        )
        .build_cranelift()
        {
            Err(error) => error,
            Ok(_) => panic!("incompatible array parameter must be rejected"),
        };
        assert!(
            error.to_string().contains("is not assignment compatible"),
            "{declaration}: {error}"
        );
    }
}

#[test]
fn rejects_incompatible_port_pattern_subarray_items() {
    for declaration in [
        "logic [7:0] row",
        "logic [7:0] row[3]",
        "logic [3:0] row[2]",
        "logic signed [7:0] row[2]",
        "bit [7:0] row[2]",
        "logic [7:0] row[1][2]",
    ] {
        let source = format!(
            r#"
            module Child(input logic [7:0] cfg[2][2], output wire [7:0] y);
                assign y=cfg[0][0];
            endmodule
            module Top(output wire [7:0] y);
                {declaration};
                Child c(.cfg('{{row,row}}), .y(y));
            endmodule
            "#
        );
        let error =
            match Simulator::from_sv_sources(vec![(&source, Path::new("subarray_item.sv"))], "Top")
                .build_cranelift()
            {
                Err(error) => error,
                Ok(_) => panic!("incompatible subarray item must be rejected: {declaration}"),
            };
        let error = error.to_string();
        assert!(
            error.contains("is not assignment compatible")
                || (error.contains("scalar")
                    && error.contains("assignment pattern item for unpacked array")),
            "{declaration}: {error}"
        );
    }
}

#[test]
fn rejects_array_values_in_scalar_port_pattern_elements() {
    for pattern in ["'{row,row}", "'{default:row}"] {
        let source = format!(
            r#"
            module Child(input logic [7:0] cfg[2], output wire [7:0] y);
                assign y=cfg[0];
            endmodule
            module Top(output wire [7:0] y);
                logic [7:0] row[2];
                Child c(.cfg({pattern}), .y(y));
            endmodule
            "#
        );
        let error = match Simulator::from_sv_sources(
            vec![(&source, Path::new("array_scalar_item.sv"))],
            "Top",
        )
        .build_cranelift()
        {
            Err(error) => error,
            Ok(_) => panic!("array value must not be truncated to a scalar: {pattern}"),
        };
        let error = error.to_string();
        assert!(
            error.contains("assignment pattern item for scalar"),
            "{pattern}: {error}"
        );
    }
}

#[test]
fn rejects_recursive_port_pattern_defaults_for_packed_unions() {
    for connection in [
        "Child c(.cfg('{default:'x}));",
        "Child c(.cfg(P::items_t'{default:'x}));",
        "Cfg c(.cfg('{default:'x}));",
        "WithParam #(.CFG('{default:'x})) c();",
    ] {
        let source = format!(
            r#"
            package P;
                typedef union packed {{logic a; bit b;}} item_t;
                typedef item_t items_t[2];
            endpackage
            module Child(input P::item_t cfg[2]); endmodule
            interface Cfg(input P::item_t cfg[2]); endinterface
            module WithParam #(parameter P::item_t CFG[2]='{{'0,'0}})(); endmodule
            module Top;
                {connection}
            endmodule
            "#
        );
        let error = match Simulator::from_sv_sources(
            vec![(&source, Path::new("union_pattern_default.sv"))],
            "Top",
        )
        .four_state(true)
        .build_cranelift()
        {
            Err(error) => error,
            Ok(_) => panic!("unsupported union default must be rejected: {connection}"),
        };
        assert!(
            error.to_string().contains("packed union"),
            "{connection}: {error}"
        );
    }
}

#[test]
fn array_parameter_elements_reject_runtime_values() {
    for (declaration, expression) in [
        ("logic [4:0] A[2]='{5'd3,5'd4};", "A[0]"),
        ("localparam logic [4:0] A[2]='{5'd3,5'd4};", "A[index]"),
    ] {
        let source = format!(
            "module Child #(parameter P=0)(output wire[4:0] y);assign y=P;endmodule\n\
             module Top(input int index,output wire[4:0] y);{declaration}\n\
             Child #(.P({expression})) child(.y(y));endmodule"
        );
        let error = match Simulator::from_sv_sources(
            vec![(&source, Path::new("runtime_array_parameter.sv"))],
            "Top",
        )
        .build_cranelift()
        {
            Err(error) => error,
            Ok(_) => panic!("runtime parameter argument {expression} must be rejected"),
        };
        assert!(
            error.to_string().contains("parameter"),
            "{expression}: {error}"
        );
    }
}

#[test]
fn array_parameter_elements_check_unselected_conditional_branch_types() {
    for declaration in [
        "logic[4:0] B[2]='{1,2};",
        "logic[3:0] B[3]='{1,2,3};",
        "logic signed[3:0] B[2]='{1,2};",
        "bit[3:0] B[2]='{1,2};",
    ] {
        let source = format!(
            "module Child #(parameter logic[3:0] Q[2]='{{0,0}})(output wire[3:0] y);\n\
             assign y=Q[0];endmodule\n\
             module Top(output wire[3:0] y);localparam logic[3:0] A[2]='{{3,4}};\n\
             localparam {declaration}\n\
             Child #(.Q(1 ? A : B)) child(.y(y));endmodule"
        );
        assert!(
            Simulator::from_sv_sources(
                vec![(&source, Path::new("conditional_array_type.sv"))],
                "Top"
            )
            .build_cranelift()
            .is_err(),
            "unselected conditional branch with {declaration} must be rejected"
        );
    }
}
