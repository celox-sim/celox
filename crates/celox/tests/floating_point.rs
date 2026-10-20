#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn integral_constants_from_float(sim) {
        // Upstream Veryl 0.22 folds negative f32 constants incorrectly (#1657).
        // https://github.com/celox-sim/celox/issues/1657
        @ignore_on(veryl, native, native_parallel, cranelift, cranelift_parallel, wasm, interp);
        @case "floating_point::integral_constants_from_float";
    }

    fn formatted_display_and_write(sim) {
        // Veryl 0.22 prints the %0.2f format text literally; its adapter also
        // has no byte-output capture API. External simulators check this case.
        @ignore_on(veryl);
        @case "floating_point::formatted_display_and_write";
    }

    fn arithmetic_and_rounding(sim) {
        // Veryl 0.22 simulator truncates 7.0 / 2.0 to 3 instead of rounding to 4.
        @ignore_on(veryl);
        @case "floating_point::arithmetic_and_rounding"; }
    fn f32_precision_and_conversion(sim) {
        // Veryl 0.22 simulator returns the f32 IEEE payload instead of a numeric integer conversion.
        @ignore_on(veryl);
        @case "floating_point::f32_precision_and_conversion"; }
    fn scalar_port_and_function_conversions(sim) {
        // Veryl 0.22 simulator truncates the implicit 3.5 port conversion to 3 instead of 4.
        @ignore_on(veryl);
        @case "floating_point::scalar_port_and_function_conversions"; }
    fn unknown_integer_conversion(sim) {
        // Veryl 0.22 simulator retains X/Z bits when converting an integer through a float.
        @ignore_on(veryl);
        @case "floating_point::unknown_integer_conversion"; }
    fn sequential_float(sim) {
        // Veryl 0.22 simulator returns zero for the clocked floating-point conversion.
        @ignore_on(veryl);
        @case "floating_point::sequential_float"; }
}
