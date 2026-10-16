#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {
    fn tb_string_assignment(sim) {
        @omit_veryl;
        // Veryl does not emit #[test] modules into SystemVerilog.
        // Celox currently truncates string assignments to the nominal one-bit width.
        // All six Celox configurations fail these cases; the support PR enables them.
        @ignore_on(native, cranelift, wasm, interp, sv);
        @case "tb_string::assignment";
    }
    fn tb_string_comparison(sim) {
        @omit_veryl;
        // Veryl does not emit #[test] modules into SystemVerilog.
        // Celox currently truncates string assignments to the nominal one-bit width.
        // All six Celox configurations fail these cases; the support PR enables them.
        @ignore_on(native, cranelift, wasm, interp, sv);
        @case "tb_string::comparison";
    }
    fn tb_string_control_flow(sim) {
        @omit_veryl;
        // Veryl does not emit #[test] modules into SystemVerilog.
        // Celox currently truncates string assignments to the nominal one-bit width.
        // All six Celox configurations fail these cases; the support PR enables them.
        @ignore_on(native, cranelift, wasm, interp, sv);
        @case "tb_string::control_flow";
    }
}
