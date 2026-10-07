#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn test_interface_connection(sim) {
        @case "interface::test_interface_connection";
    }

    fn test_interface_continuous_assign_logic(sim) {
        // Celox and veryl-simulator both build from the Veryl analyzer IR, whose
        // `ir::Interface` keeps only variables, functions and modports: logic
        // declared inside an interface is dropped without a diagnostic. The
        // emitted SystemVerilog keeps it, so the `sv` arm passes.
        // Observed on every other arm: `total` reads 0x0 instead of 0x12c (the interface `assign` never runs).
        @ignore_on(native, cranelift, wasm, interp, veryl);
        @case "interface::test_interface_continuous_assign_logic";
    }

    fn test_interface_always_comb_packed_array_member(sim) {
        // Celox and veryl-simulator both build from the Veryl analyzer IR, whose
        // `ir::Interface` keeps only variables, functions and modports: logic
        // declared inside an interface is dropped without a diagnostic. The
        // emitted SystemVerilog keeps it, so the `sv` arm passes.
        // Observed on every other arm: `q` reads 0x0 instead of 0x11 (the interface `always_comb` never runs).
        @ignore_on(native, cranelift, wasm, interp, veryl);
        @case "interface::test_interface_always_comb_packed_array_member";
    }

    fn test_interface_always_ff_on_member_clock(sim) {
        // Celox and veryl-simulator both build from the Veryl analyzer IR, whose
        // `ir::Interface` keeps only variables, functions and modports: logic
        // declared inside an interface is dropped without a diagnostic. The
        // emitted SystemVerilog keeps it, so the `sv` arm passes.
        // Observed on every other arm: `count` stays 0x0 instead of 0x3 (the interface `always_ff` never runs).
        @ignore_on(native, cranelift, wasm, interp, veryl);
        @case "interface::test_interface_always_ff_on_member_clock";
    }

    fn test_modport_clock_reset_drive_module_ff(sim) {
        @case "interface::test_modport_clock_reset_drive_module_ff";
    }

    fn test_modport_import_function(sim) {
        // veryl-simulator panics calling a modport-imported function:
        // `Option::unwrap()` on None (veryl-simulator ir/expression.rs:2506).
        @ignore_on(veryl);
        @case "interface::test_modport_import_function";
    }

    fn test_modport_import_function_in_always_ff(sim) {
        // veryl-simulator panics calling a modport-imported function:
        // `Option::unwrap()` on None (veryl-simulator ir/expression.rs:2506).
        @ignore_on(veryl);
        @case "interface::test_modport_import_function_in_always_ff";
    }

    fn test_modport_import_function_forwarded_from_instance_array(sim) {
        // veryl-simulator panics calling a modport-imported function:
        // `Option::unwrap()` on None (veryl-simulator ir/expression.rs:2506).
        @ignore_on(veryl);
        @case "interface::test_modport_import_function_forwarded_from_instance_array";
    }

    fn test_modport_converse_same_and_partial_defaults(sim) {
        @case "interface::test_modport_converse_same_and_partial_defaults";
    }

    fn test_modport_all_input_and_all_output_defaults(sim) {
        @case "interface::test_modport_all_input_and_all_output_defaults";
    }

    fn test_interface_connect_operator(sim) {
        @case "interface::test_interface_connect_operator";
    }

    fn test_interface_connect_modport_to_constant(sim) {
        @case "interface::test_interface_connect_modport_to_constant";
    }

    fn test_interface_instance_array_in_generate(sim) {
        @case "interface::test_interface_instance_array_in_generate";
    }

    fn test_modport_array_port(sim) {
        @case "interface::test_modport_array_port";
    }

    fn test_interface_instance_inside_generate_for(sim) {
        @case "interface::test_interface_instance_inside_generate_for";
    }

    fn test_interface_instance_inside_generate_if(sim) {
        @case "interface::test_interface_instance_inside_generate_if";
    }

    fn test_modport_forwarded_through_hierarchy(sim) {
        @case "interface::test_modport_forwarded_through_hierarchy";
    }

    fn test_generic_interface_port(sim) {
        // Generic `interface` and `interface::client` ports are not lowered from
        // the Veryl IR: Celox reports `Unsupported ... generic interface port
        // [tracking issue #1088]`; veryl-simulator: `build_ir failed:
        // UnsupportedDescription`. The `sv` arm binds them in the emitted SV.
        @ignore_on(native, cranelift, wasm, interp, veryl);
        @case "interface::test_generic_interface_port";
    }

    fn test_interface_struct_and_packed_array_members(sim) {
        @case "interface::test_interface_struct_and_packed_array_members";
    }

    fn test_interface_param_sized_members(sim) {
        @case "interface::test_interface_param_sized_members";
    }

    fn test_interface_param_and_const(sim) {
        // Celox and veryl-simulator both build from the Veryl analyzer IR, whose
        // `ir::Interface` keeps only variables, functions and modports: logic
        // declared inside an interface is dropped without a diagnostic. The
        // emitted SystemVerilog keeps it, so the `sv` arm passes.
        // Observed on every other arm: `f12` reads 0x0 instead of 0x1 (the interface `assign full = data == MAX` never runs).
        @ignore_on(native, cranelift, wasm, interp, veryl);
        @case "interface::test_interface_param_and_const";
    }

    fn test_interface_member_access_in_comb_and_ff(sim) {
        @case "interface::test_interface_member_access_in_comb_and_ff";
    }

    fn test_interface_valid_ready_handshake(sim) {
        @case "interface::test_interface_valid_ready_handshake";
    }

    fn test_interface_four_state_x_propagation(sim) {
        @case "interface::test_interface_four_state_x_propagation";
    }

    fn test_proto_interface_generic_module(sim) {
        @case "interface::test_proto_interface_generic_module";
    }

    fn test_mixin_interface(sim) {
        @case "interface::test_mixin_interface";
    }
}
