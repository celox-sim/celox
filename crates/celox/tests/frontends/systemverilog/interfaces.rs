sv_backends! {
    fn infers_directions_of_ports_without_modport(sim) {
        @case "interfaces::infers_directions_of_ports_without_modport";
    }

    fn binds_positional_connections_and_parameter_values(sim) {
        @case "interfaces::binds_positional_connections_and_parameter_values";
    }

    fn runs_interface_processes_once_per_array_element(sim) {
        @case "interfaces::runs_interface_processes_once_per_array_element";
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
}
