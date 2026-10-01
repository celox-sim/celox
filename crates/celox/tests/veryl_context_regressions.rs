#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

// Run known failures explicitly with:
// cargo test -p celox --test veryl_context_regressions -- --include-ignored \
//     --skip runtime_for_with_negative_bound::veryl
all_backends! {
    fn part_select_of_signed_is_unsigned(sim) {
        // SV frontend cannot lower this always_comb assignment expression.
        @ignore_on(sv);
        @case "veryl_context_regressions::part_select_of_signed_is_unsigned";
    }
    fn signed_part_select_sign_extends(sim) {
        // SV frontend cannot lower the loop/break statement.
        @ignore_on(veryl, sv);
        // Veryl 0.21.0 fails this oracle.
        @case "veryl_context_regressions::signed_part_select_sign_extends";
    }
    fn signed_struct_member_sign_extends(sim) {
        // SV frontend cannot lower this member assignment expression.
        @ignore_on(veryl, sv);
        @case "veryl_context_regressions::signed_struct_member_sign_extends";
    }
    fn wide_logical_operand_keeps_result_type(sim) {
        // SV frontend cannot lower the emitted cast.
        @ignore_on(sv);
        @case "veryl_context_regressions::wide_logical_operand_keeps_result_type";
    }
    #[ignore = "Veryl AIR folds a ternary before preserving both arm types; upstream analyzer regression probe"]
    fn constant_ternary_keeps_both_arm_types(sim) {
        @case "veryl_context_regressions::constant_ternary_keeps_both_arm_types";
    }
    #[ignore = "Signed cast of a folded constant is zero-extended; upstream analyzer regression probe"]
    fn signed_cast_of_folded_constant_sign_extends(sim) {
        @case "veryl_context_regressions::signed_cast_of_folded_constant_sign_extends";
    }
    fn case_on_signed_target_matches_negative_labels(sim) {
        // SV frontend cannot lower the emitted cast.
        @ignore_on(veryl, sv);
        // Veryl 0.21.0 fails this oracle.
        @case "veryl_context_regressions::case_on_signed_target_matches_negative_labels";
    }
    #[ignore = "Veryl constant evaluation misses negative case labels; upstream analyzer regression probe"]
    fn constant_case_on_signed_target(sim) {
        @case "veryl_context_regressions::constant_case_on_signed_target";
    }
    fn dynamic_index_store_is_cut_to_element_width(sim) {
        // SV frontend rejects the dynamic array write following partial writes.
        @ignore_on(sv);
        @case "veryl_context_regressions::dynamic_index_store_is_cut_to_element_width";
    }
    fn runtime_for_with_negative_bound(sim) {
        // SV frontend does not support procedural loops in always_comb.
        @ignore_on(veryl, sv);
        // Veryl 0.21.0 fails this oracle and does not terminate in this case.
        @case "veryl_context_regressions::runtime_for_with_negative_bound";
    }
    fn folded_constant_wider_than_its_operand(sim) {
        @case "veryl_context_regressions::folded_constant_wider_than_its_operand";
    }
    #[ignore = "Veryl AIR loses signedness of constant array elements; upstream analyzer regression probe"]
    fn folded_const_select_keeps_its_sign(sim) {
        @case "veryl_context_regressions::folded_const_select_keeps_its_sign";
    }
    #[ignore = "Veryl AIR drops the unsigned type of constant loop bounds; upstream analyzer regression probe"]
    fn runtime_for_bound_keeps_its_type(sim) {
        @case "veryl_context_regressions::runtime_for_bound_keeps_its_type";
    }
    #[ignore = "Veryl AIR folds case operands before their comparison context; upstream analyzer regression probe"]
    fn case_compares_each_label_as_an_if_does(sim) {
        @case "veryl_context_regressions::case_compares_each_label_as_an_if_does";
    }
    fn runtime_case_target_uses_comparison_context(sim) {
        // SV frontend also truncates the case target before comparison (0 instead of 2).
        @ignore_on(veryl, sv);
        @case "veryl_context_regressions::runtime_case_target_uses_comparison_context";
    }
    fn runtime_for_bound_arithmetic_uses_int_context(sim) {
        // SV frontend does not support procedural loops in always_comb.
        @ignore_on(veryl, sv);
        @case "veryl_context_regressions::runtime_for_bound_arithmetic_uses_int_context";
    }
}
