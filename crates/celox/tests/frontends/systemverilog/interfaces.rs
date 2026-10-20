use super::*;

sv_backends! {
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
