#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {


fn test_four_state_and_or(sim) {
    @case "four_state::test_four_state_and_or";
}

fn test_four_state_initial_and_set(sim) {
    @ignore_on(veryl);
    @case "four_state::test_four_state_initial_and_set";
}

fn test_ff_struct_logic_to_bit_coercion_clears_mask(sim) {
    @ignore_on(veryl, sv);
    @case "four_state::test_ff_struct_logic_to_bit_coercion_clears_mask";
}

fn test_four_state_mixing(sim) {
    @ignore_on(veryl, sv);
    @case "four_state::test_four_state_mixing";
}

fn test_four_state_mixing_propagation(sim) {
    @ignore_on(veryl, sv);
    @case "four_state::test_four_state_mixing_propagation";
}

fn test_read_a(sim) {
    @case "four_state::test_read_a";
}

fn test_four_state_arithmetic_ops(sim) {
    @case "four_state::test_four_state_arithmetic_ops";
}

fn test_four_state_unary_ops(sim) {
    @case "four_state::test_four_state_unary_ops";
}

// ==========================================================================
// Bitwise XOR with partial X
// ==========================================================================
fn test_four_state_xor_partial_x(sim) {
    @case "four_state::test_four_state_xor_partial_x";
}

// ==========================================================================
// Concatenation with X
// ==========================================================================
fn test_four_state_concat(sim) {
    @case "four_state::test_four_state_concat";
}

// ==========================================================================
// Shift with constant amount (mask should shift too)
// ==========================================================================
fn test_four_state_shift_by_constant(sim) {
    @case "four_state::test_four_state_shift_by_constant";
}

// ==========================================================================
// Shift by X amount → full X output
// ==========================================================================
fn test_four_state_shift_by_x_amount(sim) {
    @case "four_state::test_four_state_shift_by_x_amount";
}

// ==========================================================================
// Comparison with X → result is X
// ==========================================================================
fn test_four_state_comparison_with_x(sim) {
    @case "four_state::test_four_state_comparison_with_x";
}

// ==========================================================================
// Ternary / Mux with X condition
// ==========================================================================
fn test_four_state_mux_x_condition(sim) {
    @ignore_on(veryl);
    @case "four_state::test_four_state_mux_x_condition";
}

// ==========================================================================
// Mux with defined condition, X in selected branch
// ==========================================================================
fn test_four_state_mux_x_in_branch(sim) {
    @case "four_state::test_four_state_mux_x_in_branch";
}

// ==========================================================================
// Multi-word (128-bit) with X mask
// ==========================================================================
fn test_four_state_wide_128bit(sim) {
    @case "four_state::test_four_state_wide_128bit";
}

// ==========================================================================
// always_comb chain with X propagation
// ==========================================================================
fn test_four_state_always_comb_chain(sim) {
    @case "four_state::test_four_state_always_comb_chain";
}

// ==========================================================================
// always_ff: X captured in FF, reset clears X
// ==========================================================================
fn test_four_state_ff_capture_and_reset(sim) {
    @ignore_on(sv);
    @case "four_state::test_four_state_ff_capture_and_reset";
}

// ==========================================================================
// Defined inputs in 4-state mode → same as 2-state behavior
// ==========================================================================
fn test_four_state_all_defined(sim) {
    @case "four_state::test_four_state_all_defined";
}

fn test_four_state_wide_128bit_simple(sim) {
    @case "four_state::test_four_state_wide_128bit_simple";
}
// ==========================================================================
// Multi-word (128-bit) Shifts with X
// ==========================================================================
fn test_four_state_wide_shifts(sim) {
    @case "four_state::test_four_state_wide_shifts";
}

// ==========================================================================
// Multi-word (128-bit) Arithmetic with X (Conservative all-X)
// ==========================================================================
fn test_four_state_shifts_preserve_xz(sim) {
    @case "four_state::test_four_state_shifts_preserve_xz";
}

fn test_four_state_wide_arith(sim) {
    @case "four_state::test_four_state_wide_arith";
}

// ==========================================================================
// Multi-word (128-bit) Signed Ops with X
// ==========================================================================
fn test_four_state_wide_signed(sim) {
    @case "four_state::test_four_state_wide_signed";
}

// ==========================================================================
// Multi-word (128-bit) Concatenation with Mixed 2-state/4-state
// ==========================================================================
fn test_four_state_wide_concat_mixed(sim) {
    @case "four_state::test_four_state_wide_concat_mixed";
}

// ==========================================================================
// P0: MUL / DIV / MOD + X (conservative all-X)
// ==========================================================================
fn test_four_state_mul_with_x(sim) {
    @case "four_state::test_four_state_mul_with_x";
}

fn test_four_state_div_with_x(sim) {
    @case "four_state::test_four_state_div_with_x";
}

fn test_four_state_mod_with_x(sim) {
    @case "four_state::test_four_state_mod_with_x";
}

// ==========================================================================
// P0: Comparison operators with X (NE, GT, GE, LE + signed variants)
// ==========================================================================
// Known unequal bits decide equality even when other bits are X or Z.
fn test_four_state_equality_known_mismatch_with_unknown_bits(sim) {
    @case "four_state::test_four_state_equality_known_mismatch_with_unknown_bits";
}

fn test_four_state_ne_with_x(sim) {
    @case "four_state::test_four_state_ne_with_x";
}

fn test_four_state_gt_with_x(sim) {
    @case "four_state::test_four_state_gt_with_x";
}

fn test_four_state_ge_le_with_x(sim) {
    @case "four_state::test_four_state_ge_le_with_x";
}

fn test_four_state_signed_comparison_with_x(sim) {
    @case "four_state::test_four_state_signed_comparison_with_x";
}

// ==========================================================================
// P0: Reduction XOR + X
// ==========================================================================
fn test_four_state_reduction_xor_with_x(sim) {
    @case "four_state::test_four_state_reduction_xor_with_x";
}

// ==========================================================================
// P0: 65-bit width (1→2 chunk boundary)
// ==========================================================================
fn test_four_state_65bit_boundary(sim) {
    @case "four_state::test_four_state_65bit_boundary";
}

// ==========================================================================
// P1: Negation (-) + X
// ==========================================================================
fn test_four_state_negation_with_x(sim) {
    @case "four_state::test_four_state_negation_with_x";
}

// ==========================================================================
// P1: Logical NOT (!) + X
// ==========================================================================
fn test_four_state_logical_not_with_x(sim) {
    @case "four_state::test_four_state_logical_not_with_x";
}

// ==========================================================================
// P1: SAR + X shift amount
// ==========================================================================
fn test_four_state_sar_x_shift_amount(sim) {
    @case "four_state::test_four_state_sar_x_shift_amount";
}

// ==========================================================================
// P1: 3+ element concatenation with X
// ==========================================================================
fn test_four_state_concat_three_elements(sim) {
    @case "four_state::test_four_state_concat_three_elements";
}

// ==========================================================================
// P1: Wide comparison + X
// ==========================================================================
fn test_four_state_wide_comparison_with_x(sim) {
    @ignore_on(veryl);
    @case "four_state::test_four_state_wide_comparison_with_x";
}

// ==========================================================================
// P2: Multi-bit selector (case) with X
// ==========================================================================
fn test_four_state_multibit_mux_with_x(sim) {
    @ignore_on(sv);
    @case "four_state::test_four_state_multibit_mux_with_x";
}

fn test_four_state_procedural_case_x_uses_default(sim) {
    @ignore_on(sv);
    @case "four_state::test_four_state_procedural_case_x_uses_default";
}

fn test_four_state_procedural_if_known_nonzero_with_x_is_true(sim) {
    @ignore_on(sv);
    @case "four_state::test_four_state_procedural_if_known_nonzero_with_x_is_true";
}

// ==========================================================================
// P2: Width narrowing (wide → narrow) with X
// ==========================================================================
fn test_four_state_width_narrowing_with_x(sim) {
    @case "four_state::test_four_state_width_narrowing_with_x";
}

// ==========================================================================
// P2: Width widening (narrow → wide) with X
// ==========================================================================
fn test_four_state_width_widening_with_x(sim) {
    @case "four_state::test_four_state_width_widening_with_x";
}

// ==========================================================================
// P2: FF with conditional assignment + X
// ==========================================================================
fn test_four_state_ff_conditional_with_x(sim) {
    @ignore_on(sv);
    @case "four_state::test_four_state_ff_conditional_with_x";
}

// ==========================================================================
// P2: Odd-width concatenation (3bit + 5bit) with X
// ==========================================================================
fn test_four_state_concat_odd_width(sim) {
    @case "four_state::test_four_state_concat_odd_width";
}

// ==========================================================================
// P2: 127-bit width test
// ==========================================================================
fn test_four_state_127bit(sim) {
    @case "four_state::test_four_state_127bit";
}

// ==========================================================================
// Wide (128-bit) Unary NOT + X
// ==========================================================================
fn test_four_state_wide_unary_not_with_x(sim) {
    @case "four_state::test_four_state_wide_unary_not_with_x";
}

// ==========================================================================
// Wide (128-bit) Negation + X
// ==========================================================================
fn test_four_state_wide_negation_with_x(sim) {
    @case "four_state::test_four_state_wide_negation_with_x";
}

// ==========================================================================
// Wide (128-bit) Reduction AND/OR/XOR + X
// ==========================================================================
fn test_four_state_wide_reduction_with_x(sim) {
    @case "four_state::test_four_state_wide_reduction_with_x";
}

// ==========================================================================
// Mux: both branches X
// ==========================================================================
fn test_four_state_mux_both_branches_x(sim) {
    @case "four_state::test_four_state_mux_both_branches_x";
}

// ==========================================================================
// Cascaded Mux with X
// ==========================================================================
fn test_four_state_cascaded_mux_with_x(sim) {
    @ignore_on(veryl);
    @case "four_state::test_four_state_cascaded_mux_with_x";
}

// ==========================================================================
// Shift: both data and amount have X
// ==========================================================================
fn test_four_state_shift_both_x(sim) {
    @case "four_state::test_four_state_shift_both_x";
}

// ==========================================================================
// Case statement with 4-state (EqWildcard)
// ==========================================================================
fn test_four_state_case_defined_selector(sim) {
    @case "four_state::test_four_state_case_defined_selector";
}

fn test_four_state_case_x_in_selector(sim) {
    @ignore_on(veryl);
    @case "four_state::test_four_state_case_x_in_selector";
}

// ==========================================================================
// Reduction OR/AND dominant-value semantics
// ==========================================================================
fn test_four_state_reduction_or_dominant_one(sim) {
    @case "four_state::test_four_state_reduction_or_dominant_one";
}

fn test_four_state_reduction_and_dominant_zero(sim) {
    @case "four_state::test_four_state_reduction_and_dominant_zero";
}

fn test_four_state_wide_reduction_or_dominant(sim) {
    @case "four_state::test_four_state_wide_reduction_or_dominant";
}

fn test_four_state_wide_reduction_and_dominant(sim) {
    @case "four_state::test_four_state_wide_reduction_and_dominant";
}

// ==========================================================================
// IEEE 1800 LogicAnd (&&) dominant-value: 0 && x = 0
// ==========================================================================
fn test_four_state_logic_and_dominant_zero(sim) {
    @ignore_on(veryl);
    @case "four_state::test_four_state_logic_and_dominant_zero";
}

// ==========================================================================
// IEEE 1800 LogicOr (||) dominant-value: 1 || x = 1
// ==========================================================================
fn test_four_state_logic_or_dominant_one(sim) {
    @case "four_state::test_four_state_logic_or_dominant_one";
}

// ==========================================================================
// IEEE 1800 EqWildcard (==?) with LHS value at wildcard positions
// ==========================================================================
fn test_four_state_eq_wildcard_value_at_wildcard_pos(sim) {
    @case "four_state::test_four_state_eq_wildcard_value_at_wildcard_pos";
}

fn test_four_state_ne_wildcard_value_at_wildcard_pos(sim) {
    @case "four_state::test_four_state_ne_wildcard_value_at_wildcard_pos";
}

fn test_four_state_wide_wildcard_equality(sim) {
    @case "four_state::test_four_state_wide_wildcard_equality";
}

// ==========================================================================
// Wide MUL + X (128-bit)
// ==========================================================================
fn test_four_state_wide_mul_with_x(sim) {
    @case "four_state::test_four_state_wide_mul_with_x";
}

// ==========================================================================
// Wide DIV + X (128-bit)
// ==========================================================================
fn test_four_state_wide_div_with_x(sim) {
    @case "four_state::test_four_state_wide_div_with_x";
}

// ==========================================================================
// Wide MOD + X (128-bit)
// ==========================================================================
fn test_four_state_wide_mod_with_x(sim) {
    @case "four_state::test_four_state_wide_mod_with_x";
}

// ==========================================================================
// SAR with both data and shift amount having X
// ==========================================================================
fn test_four_state_sar_both_x(sim) {
    @case "four_state::test_four_state_sar_both_x";
}

// ==========================================================================
// Wide NE + X (128-bit)
// ==========================================================================
fn test_four_state_wide_ne_with_x(sim) {
    @ignore_on(veryl);
    @case "four_state::test_four_state_wide_ne_with_x";
}

// ==========================================================================
// Wide GT + X (128-bit unsigned)
// ==========================================================================
fn test_four_state_wide_gt_with_x(sim) {
    @case "four_state::test_four_state_wide_gt_with_x";
}

// ==========================================================================
// Wide GE/LE + X (128-bit unsigned)
// ==========================================================================
fn test_four_state_wide_ge_le_with_x(sim) {
    @case "four_state::test_four_state_wide_ge_le_with_x";
}

// ==========================================================================
// Wide signed comparison + X (128-bit)
// ==========================================================================
fn test_four_state_wide_signed_comparison_with_x(sim) {
    @case "four_state::test_four_state_wide_signed_comparison_with_x";
}

// ==========================================================================
// Wide logical NOT + X (128-bit)
// ==========================================================================
fn test_four_state_wide_logical_not_with_x(sim) {
    @case "four_state::test_four_state_wide_logical_not_with_x";
}

// ==========================================================================
// Concat: X crossing chunk boundary (64-bit)
// ==========================================================================
fn test_four_state_concat_chunk_boundary_x(sim) {
    @case "four_state::test_four_state_concat_chunk_boundary_x";
}

// ==========================================================================
// FF: synchronous reset + X
// ==========================================================================
fn test_four_state_ff_sync_reset_with_x(sim) {
    @ignore_on(sv);
    @case "four_state::test_four_state_ff_sync_reset_with_x";
}

// ==========================================================================
// Explicit cast + X: signed↔unsigned conversion preserves X
// ==========================================================================
fn test_four_state_explicit_cast_with_x(sim) {
    @case "four_state::test_four_state_explicit_cast_with_x";
}

// ==========================================================================
// Z Literal Tests
// ==========================================================================

fn test_z_literal_passthrough(sim) {
    @case "four_state::test_z_literal_passthrough";
}

fn test_z_mux_tristate_pattern(sim) {
    @case "four_state::test_z_mux_tristate_pattern";
}

fn test_x_literal_encoding(sim) {
    @case "four_state::test_x_literal_encoding";
}

}
