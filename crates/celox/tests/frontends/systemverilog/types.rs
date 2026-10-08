sv_backends! {
    fn passes_unpacked_arrays_of_equivalent_element_types(sim) {
        @case "types::passes_unpacked_arrays_of_equivalent_element_types";
    }

    fn passes_unpacked_arguments_of_equivalent_element_types(sim) {
        @case "types::passes_unpacked_arguments_of_equivalent_element_types";
    }

    fn rejects_unpacked_argument_of_a_wider_element_type(sim) {
        @case "types::rejects_unpacked_argument_of_a_wider_element_type";
    }

    fn rejects_unpacked_assignment_of_a_different_state_count(sim) {
        @case "types::rejects_unpacked_assignment_of_a_different_state_count";
    }

    fn rejects_unpacked_assignment_of_a_different_element_count(sim) {
        @case "types::rejects_unpacked_assignment_of_a_different_element_count";
    }

    fn rejects_unpacked_port_connection_of_a_narrower_element_type(sim) {
        @case "types::rejects_unpacked_port_connection_of_a_narrower_element_type";
    }

    fn rejects_assignment_pattern_item_of_a_narrower_array_type(sim) {
        @case "types::rejects_assignment_pattern_item_of_a_narrower_array_type";
    }

    fn packed_arrays_of_signed_named_types_have_signed_elements(sim) {
        @case "types::packed_arrays_of_signed_named_types_have_signed_elements";
    }

    fn packed_parameters_of_signed_named_types_have_signed_elements(sim) {
        @case "types::packed_parameters_of_signed_named_types_have_signed_elements";
    }

    fn simulates_systemverilog_parameterized_port_widths(sim) {
        @case "types::simulates_systemverilog_parameterized_port_widths";
    }

    fn simulates_systemverilog_bit_ports_as_two_state(sim) {
        @case "types::simulates_systemverilog_bit_ports_as_two_state";
    }

    fn preserves_integer_atom_state_kinds(sim) {
        @case "types::preserves_integer_atom_state_kinds";
    }
}
