#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn test_interface_connection(sim) {
        @ignore_on(sv);
        @case "interface::test_interface_connection";
    }

    fn test_modport_import_function(sim) {
        // veryl-simulator panics calling a modport-imported function:
        // `Option::unwrap()` on None (veryl-simulator ir/expression.rs:2506).
        @ignore_on(veryl, sv);
        @case "interface::test_modport_import_function";
    }

    fn test_modport_import_function_in_always_ff(sim) {
        // veryl-simulator panics calling from always_ff a modport-imported function:
        // `Option::unwrap()` on None (veryl-simulator ir/expression.rs:2506).
        @ignore_on(veryl, sv);
        @case "interface::test_modport_import_function_in_always_ff";
    }

    fn test_modport_import_function_forwarded_from_instance_array(sim) {
        // veryl-simulator panics calling a modport-imported function:
        // `Option::unwrap()` on None (veryl-simulator ir/expression.rs:2506).
        @ignore_on(veryl, sv);
        @case "interface::test_modport_import_function_forwarded_from_instance_array";
    }
}
