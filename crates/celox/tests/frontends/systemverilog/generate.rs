use super::*;

sv_backends! {
    fn generated_constants_select_words_from_128_bit_parameters_at_o2(sim) {
        @setup {
            // Icarus 13 rejects packed array parameters; the same source is
            // independently checked with Verilator.
            let source = r#"
    module Top #(parameter logic [3:0][31:0] WORDS={4{32'd4}})(
      output logic [15:0] widths, output logic [31:0] first,last,edge_first,edge_last);
      for(genvar i=0;i<4;i=i+1) begin:g
        localparam C=$clog2(WORDS[i]+1);
        assign widths[i*4 +: 4]=C;
      end
      localparam FIRST=WORDS[0], LAST=WORDS[3];
      localparam logic [3:0][31:0] EDGE=128'h80000000000000040000000480000000;
      localparam E0=EDGE[0], E3=EDGE[3];
      assign edge_first=E0;
      assign edge_last=E3;
      assign first=FIRST;
      assign last=LAST;
    endmodule
  "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("constant_word.sv"))], "Top")
            .opt_level(celox::OptLevel::O2);
        assert_eq!(sim.get(sim.signal("widths")), BigUint::from(0x3333u32));
        assert_eq!(sim.get(sim.signal("first")), BigUint::from(4u8));
        assert_eq!(sim.get(sim.signal("last")), BigUint::from(4u8));
        assert_eq!(sim.get(sim.signal("edge_first")), BigUint::from(0x80000000u32));
        assert_eq!(sim.get(sim.signal("edge_last")), BigUint::from(0x80000000u32));
    }

    fn generate_local_typedefs_keep_lexical_scope(sim) {
        @case "generate::generate_local_typedefs_keep_lexical_scope";
    }

    fn bits_of_generate_local_array_element_localparam(sim) {
        @case "generate::bits_of_generate_local_array_element_localparam";
    }

    fn bits_of_generate_local_array_elements(sim) {
        @case "generate::bits_of_generate_local_array_elements";
    }

    fn genvar_shadows_module_localparam(sim) {
        @case "generate::genvar_shadows_module_localparam";
    }

    fn genvar_shadows_inherited_parameter_values_and_types(sim) {
        @case "generate::genvar_shadows_inherited_parameter_values_and_types";
    }

    fn genvar_shadows_inherited_parameter_shapes(sim) {
        @case "generate::genvar_shadows_inherited_parameter_shapes";
    }

    fn size_queries_use_generate_local_parameter_types(sim) {
        @case "generate::size_queries_use_generate_local_parameter_types";
    }

    fn counts_only_known_ones_in_generate_system_functions(sim) {
        @case "generate::counts_only_known_ones_in_generate_system_functions";
    }

    fn uses_self_determined_unbased_shift_counts_in_generate_cases(sim) {
        @case "generate::uses_self_determined_unbased_shift_counts_in_generate_cases";
    }

    fn freezes_typedef_ranges_before_generate_parameter_shadowing(sim) {
        @case "generate::freezes_typedef_ranges_before_generate_parameter_shadowing";
    }

    fn preserves_unsigned_rhs_in_compound_genvar_updates(sim) {
        @case "generate::preserves_unsigned_rhs_in_compound_genvar_updates";
    }

    fn coerces_genvar_values_to_signed_32_bits(sim) {
        @case "generate::coerces_genvar_values_to_signed_32_bits";
    }

    fn restricts_generate_size_queries_to_visible_functions(sim) {
        @case "generate::restricts_generate_size_queries_to_visible_functions";
    }

    fn predeclares_generate_signals_and_functions(sim) {
        @case "generate::predeclares_generate_signals_and_functions";
    }

    fn binds_generate_parameter_declarations_as_localparams(sim) {
        @case "generate::binds_generate_parameter_declarations_as_localparams";
    }

    fn expands_constant_functions_in_genvar_updates(sim) {
        @case "generate::expands_constant_functions_in_genvar_updates";
    }

    fn ignores_type_aliases_in_inactive_generate_branches(sim) {
        @case "generate::ignores_type_aliases_in_inactive_generate_branches";
    }

    fn distinguishes_escaped_generate_scope_components(sim) {
        @case "generate::distinguishes_escaped_generate_scope_components";
    }

    fn rejects_an_escaped_generate_block_named_like_a_port(sim) {
        @case "generate::rejects_an_escaped_generate_block_named_like_a_port";
    }

    fn prefers_function_local_types_over_generate_signals(sim) {
        @case "generate::prefers_function_local_types_over_generate_signals";
    }

    fn evaluates_module_constant_functions_in_generate_schemes(sim) {
        @case "generate::evaluates_module_constant_functions_in_generate_schemes";
    }

    fn permits_functions_in_escaped_conditional_generate_labels(sim) {
        @case "generate::permits_functions_in_escaped_conditional_generate_labels";
    }

    fn resolves_each_generate_signal_declarator_independently(sim) {
        @case "generate::resolves_each_generate_signal_declarator_independently";
    }

    fn uses_scoped_function_return_metadata_for_comb_completeness(sim) {
        @case "generate::uses_scoped_function_return_metadata_for_comb_completeness";
    }

    fn preserves_generate_localparam_types_in_child_overrides(sim) {
        @case "generate::preserves_generate_localparam_types_in_child_overrides";
    }

    fn treats_unknown_generate_truth_as_false(sim) {
        @case "generate::treats_unknown_generate_truth_as_false";
    }

    fn resolves_generate_signal_size_parameter_dependencies(sim) {
        @case "generate::resolves_generate_signal_size_parameter_dependencies";
    }

    fn resolves_generate_function_names_lexically(sim) {
        @case "generate::resolves_generate_function_names_lexically";
    }

    fn resolves_forward_generate_localparam_dependencies(sim) {
        @case "generate::resolves_forward_generate_localparam_dependencies";
    }

    fn elaborates_generate_lanes_with_local_state_and_instances(sim) {
        @case "generate::elaborates_generate_lanes_with_local_state_and_instances";
    }

    fn elaborates_nested_generate_and_case_branches(sim) {
        @case "generate::elaborates_nested_generate_and_case_branches";
    }

    fn specializes_generate_scopes_for_each_child_parameter(sim) {
        @case "generate::specializes_generate_scopes_for_each_child_parameter";
    }

    fn elaborates_generate_declaration_widths_and_shadowing(sim) {
        @case "generate::elaborates_generate_declaration_widths_and_shadowing";
    }

    fn preserves_generate_constant_scopes_and_masks(sim) {
        @case "generate::preserves_generate_constant_scopes_and_masks";
    }

    fn connects_generated_outputs_to_ascending_packed_slices(sim) {
        @case "generate::connects_generated_outputs_to_ascending_packed_slices";
    }

    fn selects_case_generate_with_four_state_parameter_and_local_labels(sim) {
        @case "generate::selects_case_generate_with_four_state_parameter_and_local_labels";
    }

    fn uses_one_width_and_sign_context_for_generate_case(sim) {
        @case "generate::uses_one_width_and_sign_context_for_generate_case";
    }
}

#[test]
fn generate_function_call_preserves_definition_site_bindings() {
    let source = r#"
        module Top(input logic clk, input logic [1:0] a, output logic [3:0] y);
            function automatic logic f(input logic x); return a[0] ^ x; endfunction
            if (1) begin : g
                logic [1:0] a;
                logic x;
                assign a = 2'b10;
                assign x = 0;
                function automatic logic local_value(); return a[1]; endfunction
                always_comb y[0] = f(x);
                assign y[1] = local_value();
                always_comb y[2] = local_value();
                always_ff @(posedge clk) y[3] <= f(x);
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("generate_function_binding.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let a = sim.signal("a");
    let clk = sim.event("clk");
    for value in 0u8..4 {
        sim.modify(|io| io.set(a, value)).unwrap();
        sim.tick(clk).unwrap();
        let expected = if value & 1 != 0 { 15u8 } else { 6u8 };
        assert_eq!(sim.get(sim.signal("y")), expected.into());
    }
}

#[test]
fn rejects_unqualified_generate_function_calls_outside_their_scope() {
    for call in [
        "assign y = f(a);",
        "if (1) begin : sibling assign y = f(a); end",
    ] {
        let source = format!(
            r#"
            module Top(input logic a, output logic y);
                if (1) begin : g
                    function automatic logic f(input logic x); return x; endfunction
                end
                {call}
            endmodule
        "#
        );
        assert!(
            Simulator::from_sv_sources(
                vec![(&source, Path::new("generate_function_visibility.sv"))],
                "Top"
            )
            .build_cranelift()
            .is_err(),
            "accepted out-of-scope call: {call}"
        );
    }
}

#[test]
fn keeps_four_state_generate_function_guards_nonexhaustive() {
    let source = r#"
        module Top(input logic a, output logic y);
            function automatic bit f(input logic x); return x; endfunction
            if (1) begin : g
                function automatic logic f(input logic x); return x; endfunction
                always_comb if (f(a) || !f(a)) y = a;
            end
        endmodule
    "#;
    let result = Simulator::from_sv_sources(
        vec![(source, Path::new("generate_four_state_function_guard.sv"))],
        "Top",
    )
    .four_state(true)
    .build_cranelift();
    let error = match result {
        Ok(_) => panic!("accepted an incomplete four-state guard"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("latch inference inside always_comb"),
        "{error}"
    );
}

// Icarus 13 rejects enum labels inside generate scopes and crashes while
// elaborating this source. Keep O2 isolation coverage in the API harness;
// the portable suite separately checks generated typedef layouts.
sv_backends! {
    fn generated_enum_and_shadowed_type_scopes_survive_o2(sim) {
        @setup {
            let source=r#"
        module Top(input logic[7:0] a, output logic[7:0] y,z,widths,
            output logic[3:0] root_value, output logic[7:0] parent_value);
            typedef logic[3:0] T;
            T root;
            assign root=a[3:0];
            assign root_value=root;
            if(1) begin:left
                typedef logic[7:0] T;
                typedef enum logic[1:0] { OFF=0, ON=3 } state_t;
                T temp;
                state_t state;
                assign state=ON;
                assign temp=a;
                assign y=(state==3) ? temp : 0;
                if(1) begin:inner
                    typedef logic[1:0] T;
                    T tiny;
                    assign tiny=a[1:0];
                end
                T after_inner;
                assign after_inner=a;
                assign parent_value=after_inner;
            end
            if(1) begin:right
                typedef logic[3:0] T;
                typedef enum logic[1:0] { OFF=1, ON=2 } state_t;
                T temp;
                state_t state;
                assign state=ON;
                assign temp=a[3:0];
                assign z=(state==2) ? {4'hc,temp} : 0;
            end
            for(genvar i=0;i<2;i=i+1) begin:lanes
                typedef logic[i+2:0] lane_t;
                lane_t temp;
                assign temp=a;
                assign widths[4*i+:4]=$bits(temp);
            end
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(source,Path::new("generated_enum.sv"))],"Top")
            .opt_level(celox::OptLevel::O2);
        let a=sim.signal("a");
        for value in [0xa7u8,0x5c] {
            sim.modify(|io| io.set(a,value)).unwrap();
            assert_eq!(sim.get(sim.signal("y")),BigUint::from(value));
            assert_eq!(sim.get(sim.signal("z")),BigUint::from(0xc0|(value&15)));
            assert_eq!(sim.get(sim.signal("parent_value")),BigUint::from(value));
            assert_eq!(sim.get(sim.signal("root_value")),BigUint::from(value&15));
            assert_eq!(sim.get(sim.signal("widths")),BigUint::from(0x43u8));
        }
    }
}
