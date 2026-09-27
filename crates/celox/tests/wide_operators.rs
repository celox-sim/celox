#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

// ============================================================
// Wide (128-bit) logical operators (&& / ||)
// ============================================================

// ============================================================
// Wide (128-bit) multiplication
// ============================================================

// ============================================================
// Wide (128-bit) division / modulo
// ============================================================

// ============================================================
// Wide (128-bit) XNOR (binary)
// ============================================================

// ============================================================
// Wide (128-bit) arithmetic shift right
// ============================================================

// ============================================================
// Wide (128-bit) signed comparisons
// ============================================================

// ============================================================
// Wide (128-bit) reduction operators
// ============================================================

all_backends! {

    fn test_wide_addition_128bit(sim) {
        @case "wide_operators::test_wide_addition_128bit";
    }

    fn test_wide_addition_carry_propagation(sim) {
        @case "wide_operators::test_wide_addition_carry_propagation";
    }

    fn test_wide_subtraction_128bit(sim) {
        @case "wide_operators::test_wide_subtraction_128bit";
    }

    fn test_wide_subtraction_borrow(sim) {
        @case "wide_operators::test_wide_subtraction_borrow";
    }

    fn test_wide_bitwise_operations(sim) {
        @case "wide_operators::test_wide_bitwise_operations";
    }

    fn test_wide_comparison_eq(sim) {
        @case "wide_operators::test_wide_comparison_eq";
    }

    fn test_wide_shift_left(sim) {
        @case "wide_operators::test_wide_shift_left";
    }

    fn test_wide_ff_accumulator(sim) {
        @case "wide_operators::test_wide_ff_accumulator";
    }

    // 128-bit logical and/or in always_comb.
    //
    // Veryl syntax:
    // - `a && b` => BinaryOp::LogicAnd
    // - `a || b` => BinaryOp::LogicOr
    fn test_wide_comb_logic_and_or(sim) {
        @case "wide_operators::test_wide_comb_logic_and_or";
    }

    // Regression: wide operand (65-bit, 2 chunks) `&&`/`||` a narrow 1-bit operand.
    //
    // Before the fix, `reduce_to_bool` in `emit_wide_logic_andor` passed raw I8
    // chunks to a `bor.i64` accumulator, producing a Cranelift verifier error.
    fn test_wide_logic_or_with_narrow_operand(sim) {
        @case "wide_operators::test_wide_logic_or_with_narrow_operand";
    }

    // 128-bit multiply in always_comb.
    fn test_wide_comb_mul(sim) {
        @case "wide_operators::test_wide_comb_mul";
    }

    // 128-bit multiply in always_ff.
    fn test_wide_ff_mul(sim) {
        @case "wide_operators::test_wide_ff_mul";
    }

    // 128-bit division in always_comb.
    fn test_wide_comb_div(sim) {
        @case "wide_operators::test_wide_comb_div";
    }

    // 128-bit modulo in always_comb.
    fn test_wide_comb_rem(sim) {
        @case "wide_operators::test_wide_comb_rem";
    }

    // 128-bit XNOR in always_comb.
    fn test_wide_comb_bitxnor(sim) {
        @ignore_on(sv);
        @case "wide_operators::test_wide_comb_bitxnor";
    }

    // 128-bit arithmetic shift right in always_comb.
    fn test_wide_comb_sar(sim) {
        @case "wide_operators::test_wide_comb_sar";
    }

    // 128-bit signed less-than.
    fn test_wide_comb_signed_lt(sim) {
        @case "wide_operators::test_wide_comb_signed_lt";
    }

    // 128-bit signed greater-than.
    fn test_wide_comb_signed_gt(sim) {
        @case "wide_operators::test_wide_comb_signed_gt";
    }

    // Signed ordering must use the declared sign bit when the top storage
    // chunk is only partially occupied.
    fn test_wide_signed_compare_non_chunk_aligned(sim) {
        @case "wide_operators::test_wide_signed_compare_non_chunk_aligned";
    }

    // 128-bit reduction NAND.
    fn test_wide_comb_reduction_nand(sim) {
        @case "wide_operators::test_wide_comb_reduction_nand";
    }

    // 128-bit reduction NOR.
    fn test_wide_comb_reduction_nor(sim) {
        @case "wide_operators::test_wide_comb_reduction_nor";
    }

    // 128-bit reduction XNOR.
    fn test_wide_comb_reduction_xnor(sim) {
        @case "wide_operators::test_wide_comb_reduction_xnor";
    }
}
