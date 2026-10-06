sv_backends! {
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
