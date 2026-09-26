use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

// ============================================================
// Division / Modulo (always_comb)
// ============================================================

// ============================================================
// BitXnor (binary)
// ============================================================

// ============================================================
// Reduction BitNand / BitNor / BitXnor (unary)
// ============================================================

all_backends! {

    fn test_ternary_operator(sim) {
        @case "operators::test_ternary_operator";
    }

    fn test_nested_ternary(sim) {
        @case "operators::test_nested_ternary";
    }

    fn test_bitwise_operations(sim) {
        @case "operators::test_bitwise_operations";
    }

    fn test_shift_logical_vs_arithmetic(sim) {
        @case "operators::test_shift_logical_vs_arithmetic";
    }

    fn test_signed_arithmetic_shift_right(sim) {
        @case "operators::test_signed_arithmetic_shift_right";
    }

    fn test_subtraction_underflow(sim) {
        @case "operators::test_subtraction_underflow";
    }

    fn test_unary_operations(sim) {
        @case "operators::test_unary_operations";
    }

    fn test_unary_plus_operator(sim) {
        @case "operators::test_unary_plus_operator";
    }

    fn test_comparisons(sim) {
        @case "operators::test_comparisons";
    }

    fn test_signed_comparison_and_extension(sim) {
        @case "operators::test_signed_comparison_and_extension";
    }

    fn test_logical_operators_execution(sim) {
        @case "operators::test_logical_operators_execution";
    }

    fn test_reduction_operators_execution(sim) {
        @case "operators::test_reduction_operators_execution";
    }

    fn test_pow_operator_constant_exponent(sim) {
        @ignore_on(sv);
        @case "operators::test_pow_operator_constant_exponent";
    }

    fn test_as_operator_passthrough(sim) {
        @ignore_on(sv);
        @case "operators::test_as_operator_passthrough";
    }

    fn test_pow_operator_constant_exponent_ff(sim) {
        @ignore_on(sv);
        @case "operators::test_pow_operator_constant_exponent_ff";
    }

    fn test_pow_operator_runtime_exponent_comb_and_ff(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @case "operators::test_pow_operator_runtime_exponent_comb_and_ff";
    }

    fn test_pow_operator_runtime_signed_and_unknown_operands(sim) {
        @ignore_on(sv);
        @case "operators::test_pow_operator_runtime_signed_and_unknown_operands";
    }

    fn test_signed_comparison_after_as_cast(sim) {
        @ignore_on(veryl, sv);
        @case "operators::test_signed_comparison_after_as_cast";
    }

    fn test_cast_signed_to_unsigned_affects_comparison(sim) {
        @ignore_on(veryl, sv);
        @case "operators::test_cast_signed_to_unsigned_affects_comparison";
    }

    fn test_symbolic_store_preserves_declared_state_signedness(sim) {
        @ignore_on(sv);
        @case "operators::test_symbolic_store_preserves_declared_state_signedness";
    }

    fn test_unsigned_type_cast_does_not_inherit_source_signedness(sim) {
        @ignore_on(sv);
        @case "operators::test_unsigned_type_cast_does_not_inherit_source_signedness";
    }

    // Basic unsigned division in always_comb.
    fn test_comb_div(sim) {
        @case "operators::test_comb_div";
    }

    // Basic unsigned modulo in always_comb.
    fn test_comb_rem(sim) {
        @case "operators::test_comb_rem";
    }

    fn test_ternary_div_zero_branch_is_lazy(sim) {
        @case "operators::test_ternary_div_zero_branch_is_lazy";
    }

    fn test_ternary_rem_zero_branch_is_lazy(sim) {
        @case "operators::test_ternary_rem_zero_branch_is_lazy";
    }

    // Division in always_ff.
    fn test_ff_div(sim) {
        @case "operators::test_ff_div";
    }

    // Modulo in always_ff.
    fn test_ff_rem(sim) {
        @case "operators::test_ff_rem";
    }

    // XNOR in always_comb: ~(a ^ b)
    fn test_comb_bitxnor(sim) {
        @ignore_on(sv);
        @case "operators::test_comb_bitxnor";
    }

    // XNOR in always_ff.
    fn test_ff_bitxnor(sim) {
        @ignore_on(sv);
        @case "operators::test_ff_bitxnor";
    }

    // Reduction NAND: ~&a  (0 if all bits 1, else 1)
    fn test_comb_reduction_nand(sim) {
        @case "operators::test_comb_reduction_nand";
    }

    // Reduction NOR: ~|a  (1 if all bits 0, else 0)
    fn test_comb_reduction_nor(sim) {
        @case "operators::test_comb_reduction_nor";
    }

    // Reduction XNOR: ~^a  (1 if even number of 1s, i.e. even parity)
    fn test_comb_reduction_xnor(sim) {
        @case "operators::test_comb_reduction_xnor";
    }

    // Reduction NAND in always_ff.
    fn test_ff_reduction_nand(sim) {
        @case "operators::test_ff_reduction_nand";
    }

    fn test_ff_comb_constant_folding_consistency(sim) {
        @case "operators::test_ff_comb_constant_folding_consistency";
    }

    // Reduction NOR in always_ff.
    fn test_ff_reduction_nor(sim) {
        @case "operators::test_ff_reduction_nor";
    }

    fn test_mixed_signed_unsigned_comparison(sim) {
        @ignore_on(veryl, sv);
        @case "operators::test_mixed_signed_unsigned_comparison";
    }
}

#[test]
fn test_nested_ternary_concat_hybrid() {
    let code = r#"
        module Top (sel: input logic, a: input logic<4>, b: input logic<4>, c: input logic<8>, o: output logic<8>) {
            always_comb {
                o = if sel ? {a, b} : (if (a == b) ? c : 8'hEE);
            }
        }
    "#;
    let result = Simulator::builder(code, "Top").build();
    assert!(
        result.is_ok(),
        "Should handle deeply nested expression structures"
    );
}
