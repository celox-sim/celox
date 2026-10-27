use super::*;

sv_backends! {
    fn forwards_header_ports_through_modports(sim) {
        @case "interfaces::forwards_header_ports_through_modports";
    }
    fn defers_interface_dependent_declarations_before_binding_array_ports(sim) {
        @case "interfaces::defers_interface_dependent_declarations_before_binding_array_ports";
    }
    fn connects_interface_array_slices_in_declaration_order(sim) {
        @case "interfaces::connects_interface_array_slices_in_declaration_order";
    }
    fn drives_interface_array_slices_through_generated_relays(sim) {
        @case "interfaces::drives_interface_array_slices_through_generated_relays";
    }
    fn connects_multidimensional_interface_array_slices(sim) {
        @case "interfaces::connects_multidimensional_interface_array_slices";
    }

    fn distributes_multidimensional_connections_to_interface_arrays(sim) {
        @case "interfaces::distributes_multidimensional_connections_to_interface_arrays";
    }

    fn distributes_array_port_connections_to_interface_arrays(sim) {
        @case "interfaces::distributes_array_port_connections_to_interface_arrays";
    }

    fn distributes_unpacked_connections_to_interface_arrays(sim) {
        @case "interfaces::distributes_unpacked_connections_to_interface_arrays";
    }

    fn mixes_module_contracts_with_local_interface_connections(sim) {
        @case "interfaces::mixes_module_contracts_with_local_interface_connections";
    }
    fn distributes_packed_connections_to_interface_arrays(sim) {
        @case "interfaces::distributes_packed_connections_to_interface_arrays";
    }

    fn binds_implicit_named_interface_ports(sim) {
        @case "interfaces::binds_implicit_named_interface_ports";
    }
    fn binds_implicit_named_interface_arrays_in_generate_scopes(sim) {
        @case "interfaces::binds_implicit_named_interface_arrays_in_generate_scopes";
    }
    fn propagates_interface_dependent_ports(sim) {
        @case "interfaces::propagates_interface_dependent_ports";
    }
    fn keeps_lexical_names_in_interface_scopes(sim) {
        @case "interfaces::keeps_lexical_names_in_interface_scopes";
    }
    fn binds_interfaces_without_declaration_name_dependencies(sim) {
        @case "interfaces::binds_interfaces_without_declaration_name_dependencies";
    }
    fn binds_local_interfaces_before_dependent_declarations(sim) {
        @case "interfaces::binds_local_interfaces_before_dependent_declarations";
    }
    fn queries_interface_packed_struct_member_dimensions(sim) {
        @case "interfaces::queries_interface_packed_struct_member_dimensions";
    }

    fn binds_interface_array_ports_in_declaration_order(sim) {
        @case "interfaces::binds_interface_array_ports_in_declaration_order";
    }
    fn infers_directions_of_ports_without_modport(sim) {
        @case "interfaces::infers_directions_of_ports_without_modport";
    }

    fn binds_positional_connections_and_parameter_values(sim) {
        @case "interfaces::binds_positional_connections_and_parameter_values";
    }

    fn runs_interface_processes_once_per_array_element(sim) {
        @case "interfaces::runs_interface_processes_once_per_array_element";
    }

    fn runs_interface_array_processes_at_integer_boundaries(sim) {
        @case "interfaces::runs_interface_array_processes_at_integer_boundaries";
    }

    fn calls_functions_and_reads_parameters_through_instances_and_ports(sim) {
        @case "interfaces::calls_functions_and_reads_parameters_through_instances_and_ports";
    }

    fn elaborates_net_members_typedefs_and_localparams(sim) {
        @case "interfaces::elaborates_net_members_typedefs_and_localparams";
    }

    fn binds_generic_interface_ports_per_interface(sim) {
        @case "interfaces::binds_generic_interface_ports_per_interface";
    }

    fn clocks_interface_processes_from_member_clocks(sim) {
        @case "interfaces::clocks_interface_processes_from_member_clocks";
    }

    fn reads_undriven_interface_members_as_unknown(sim) {
        @case "interfaces::reads_undriven_interface_members_as_unknown";
    }

    fn overrides_body_parameters_and_drives_members_from_imported_functions(sim) {
        @case "interfaces::overrides_body_parameters_and_drives_members_from_imported_functions";
    }

    fn rewrites_nested_references_and_keeps_declaration_functions(sim) {
        @case "interfaces::rewrites_nested_references_and_keeps_declaration_functions";
    }

    fn drives_members_through_child_outputs_and_array_generate_regions(sim) {
        @case "interfaces::drives_members_through_child_outputs_and_array_generate_regions";
    }

    fn initializes_interface_members_from_their_declarations(sim) {
        @case "interfaces::initializes_interface_members_from_their_declarations";
    }
    fn initializes_interface_array_members_from_their_declarations(sim) {
        @case "interfaces::initializes_interface_array_members_from_their_declarations";
    }
    fn distinguishes_interface_dependency_bindings(sim) {
        @case "interfaces::distinguishes_interface_dependency_bindings";
    }

}

// Verilator 5.052 faults internally on this generic interface relay; keep the
// generic binding regression in the Celox harness and the concrete form shared.
sv_backends! {
    fn binds_implicit_named_generic_interface_ports(sim) {
        @setup {
            let source = r#"
    interface Bus;
      logic [7:0] data;
      modport writer(output data);
      modport reader(input data);
    endinterface
    module Writer(Bus.writer iface, input logic [7:0] x);
      assign iface.data = x;
    endmodule
    module Reader(interface iface, output logic [7:0] y);
      assign y = iface.data;
    endmodule
    module Relay(Bus iface, output logic [7:0] y);
      Reader r(.iface, .y);
    endmodule
    module Top(input logic [7:0] x, output logic [7:0] y);
      Bus iface();
      Writer w(.iface, .x);
      Relay r(.iface, .y);
    endmodule
  "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("implicit_generic_interface.sv"))], "Top");
        let x = sim.signal("x");
        let y = sim.signal("y");
        for value in [0u8, 0x41, 0xff] {
            sim.modify(|io| io.set(x, value)).unwrap();
            assert_eq!(u64::try_from(sim.get(y)).unwrap(), u64::from(value));
        }
    }
}

// Interface objects are not IEEE 20.7 data arrays. This opt-in compatibility
// behavior belongs to Celox's harness rather than the portable language suite.
sv_backends! {
    fn queries_interface_array_sizes_as_an_explicit_extension(sim) {
        @setup {
            let source = r#"
                interface Lane; logic value; endinterface
                module Child #(parameter N=1)(output wire [7:0] y); assign y=N; endmodule
                module Reader(Lane ports[3:2], output wire [7:0] y);
                    assign y=$size(ports);
                endmodule
                module Top(output wire [7:0] local_count, param_count, formal_count,
                    first_count, second_count, selected_count, generated_count, shadow_count, member_shadow_count,
                    output wire [1:0] generated);
                    Lane lanes[3:2]();
                    Lane matrix[1:0][4:2]();
                    localparam N=$size(lanes);
                    assign local_count=N;
                    Child #(.N($size(lanes))) c(.y(param_count));
                    Reader r(.ports(lanes), .y(formal_count));
                    assign first_count=$size(matrix,1);
                    assign second_count=$size(matrix,2);
                    assign selected_count=$size(matrix[0]);
                    for(genvar i=0;i<$size(lanes);i=i+1) begin: bits_out
                        assign generated[i]=1'b1;
                    end
                    if(1) begin: bank
                        Lane local_lanes[5:7]();
                        assign generated_count=$size(local_lanes);
                    end
                    function automatic int shadow();
                        int lanes[5];
                        return $size(lanes);
                    endfunction
                    assign shadow_count=shadow();
                    function automatic int shadow_member();
                        struct packed {logic [4:0] local_lanes;} bank;
                        return $size(bank.local_lanes);
                    endfunction
                    assign member_shadow_count=shadow_member();
                endmodule
            "#;
            let mut policy=celox::DiagnosticPolicy::default();
            policy.set("interface_array_query",celox::DiagnosticLevel::Warn);
        }
        @build Simulator::from_sv_sources(vec![(source,Path::new("interface_array_size.sv"))],"Top")
            .diagnostic_policy(policy);
        for (name,expected) in [("local_count",2u8),("param_count",2),("formal_count",2),
            ("first_count",2),("second_count",3),("selected_count",3),
            ("generated_count",3),("shadow_count",5),("member_shadow_count",5),("generated",3)] {
            assert_eq!(sim.get(sim.signal(name)),BigUint::from(expected),"{name}");
        }
        assert!(!sim.warnings().is_empty());
    }
}

#[test]
fn rejects_invalid_interface_array_selections() {
    for (selection, expected) in [
        ("lanes[4:3]", "outside its declared range"),
        ("lanes[0:1]", "reversed interface array selection"),
        (
            "lanes[1 +: 0]",
            "nonpositive interface array selection width",
        ),
        ("lanes[index +: 2]", "nonconstant interface array selection"),
        ("lanes[0][0]", "too many interface array selections"),
        ("lanes[3:1]", "interface array dimension mismatch"),
        (
            "lanes[128'h7fffffffffffffffffffffffffffffff +: 2]",
            "selection bound overflow",
        ),
    ] {
        let source = format!(
            r#"
            interface Lane;
                logic value;
                modport reader(input value);
            endinterface
            module Reader(Lane.reader ports[2], output wire y);
                assign y=ports[0].value ^ ports[1].value;
            endmodule
            module Top(input int index, output wire y);
                Lane lanes[3:0]();
                for(genvar n=0;n<4;n++) begin: g
                    assign lanes[n].value=1'b0;
                end
                Reader r(.ports({selection}),.y(y));
            endmodule
        "#
        );
        let error = Simulator::from_sv_sources(
            vec![(source.as_str(), Path::new("invalid_interface_slice.sv"))],
            "Top",
        )
        .build_interpreter()
        .expect_err(selection)
        .to_string();
        assert!(error.contains(expected), "{selection}: {error}");
    }
}

#[test]
fn rejects_interface_array_slices_in_multiple_dimensions() {
    let source = r#"
        interface Lane; logic value; endinterface
        module Reader(Lane lanes[1][1], output wire y);
            assign y=lanes[0][0].value;
        endmodule
        module Top(output wire y);
            Lane lanes[2][2]();
            Reader r(.lanes(lanes[0:0][0:0]),.y(y));
        endmodule
    "#;
    let error =
        Simulator::from_sv_sources(vec![(source, Path::new("invalid_matrix_slice.sv"))], "Top")
            .build_interpreter()
            .err()
            .unwrap()
            .to_string();
    assert!(
        error.contains("interface array slices in multiple dimensions"),
        "{error}"
    );
}

// The unindexed value-parameter shorthand is an explicit compatibility mode.
sv_backends! {
    fn unindexed_interface_array_parameters_preserve_bound_values_and_types(sim) {
        @setup {
            let source = "interface Bus #(parameter int WIDTH=8, parameter logic signed [7:0] NEG=-3);
                    logic [WIDTH-1:0] data; modport sink(input data); endinterface
                module Child(Bus.sink bus[5:4], output wire [31:0] q, n);
                    function automatic int width_value(); return bus.WIDTH; endfunction
                    localparam W=width_value();
                    assign q=W;
                    assign n=bus.NEG;
                endmodule
                module Top(output wire [31:0] q,n);
                    Bus #(13,-5) buses[5:4](); Child dut(buses,q,n);
                endmodule";
            let mut policy=celox::DiagnosticPolicy::default();
            policy.set("interface_array_parameter",celox::DiagnosticLevel::Allow);
        }
        @build Simulator::from_sv_sources(vec![(source,Path::new("array_parameter_types.sv"))],"Top").diagnostic_policy(policy);
        assert_eq!(sim.get(sim.signal("q")),BigUint::from(13u8));
        assert_eq!(sim.get(sim.signal("n")),BigUint::from(0xfffffffbu32));
    }
}

#[test]
fn interface_array_parameter_shorthand_does_not_bind_runtime_members() {
    let source = "interface Bus #(parameter WIDTH=8); logic [WIDTH-1:0] data; endinterface
        module Top(output wire [7:0] q); Bus bus[2](); assign q=bus.data; endmodule";
    let mut policy = celox::DiagnosticPolicy::default();
    policy.set("interface_array_parameter", celox::DiagnosticLevel::Allow);
    let error = Simulator::from_sv_sources(vec![(source, Path::new("array_signal.sv"))], "Top")
        .diagnostic_policy(policy)
        .build_cranelift()
        .err()
        .unwrap();
    assert!(error.to_string().contains("bus.data"), "{error}");
}
