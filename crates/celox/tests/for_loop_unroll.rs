// A `for` loop with compile-time-constant bounds inside `always_ff` is
// unrolled by the Veryl analyzer before Celox processes the IR.
// These tests verify that the unrolled shift-register pattern produces
// correct non-blocking FF semantics.

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {

fn test_for_loop_unroll_shift_register(sim) {
    @ignore_on(sv);
    @case "for_loop_unroll::test_for_loop_unroll_shift_register";
}

fn test_for_loop_unroll_break_in_always_ff(sim) {
    @ignore_on(sv);
    @case "for_loop_unroll::test_for_loop_unroll_break_in_always_ff";
}

// Default array reset combined with a shift-register `for` loop.
fn test_for_loop_unroll_with_default_zero_reset(sim) {
    @ignore_on(sv);
    @case "for_loop_unroll::test_for_loop_unroll_with_default_zero_reset";
}

}
