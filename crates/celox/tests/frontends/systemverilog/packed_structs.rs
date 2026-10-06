use super::*;

#[path = "packed_structs/port_connections.rs"]
mod port_connections;

sv_backends! {
    fn packed_struct_layout_and_nested_fields(sim) {
        @case "packed_structs::packed_struct_layout_and_nested_fields";
    }

    fn packed_struct_field_assignments_and_registers(sim) {
        @case "packed_structs::packed_struct_field_assignments_and_registers";
    }

    fn packed_struct_mixed_state_members(sim) {
        @case "packed_structs::packed_struct_mixed_state_members";
    }

    fn packed_struct_parameterized_ports(sim) {
        @case "packed_structs::packed_struct_parameterized_ports";
    }

    fn packed_struct_bounds_and_type_queries(sim) {
        @case "packed_structs::packed_struct_bounds_and_type_queries";
    }

    fn packed_struct_wide_four_state_layout(sim) {
        @case "packed_structs::packed_struct_wide_four_state_layout";
    }

    fn packed_struct_whole_value_signedness(sim) {
        @case "packed_structs::packed_struct_whole_value_signedness";
    }

    fn packed_struct_typedef_bounds_survive_generate_shadowing(sim) {
        @case "packed_structs::packed_struct_typedef_bounds_survive_generate_shadowing";
    }
}

sv_backends! {
    fn packed_struct_alias_packed_dimensions(sim) {
        @case "packed_structs::packed_struct_alias_packed_dimensions";
    }
}
