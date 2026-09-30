//! Reconstructed Veryl upstream cases, executed through the shared corpus.
#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn inside_outside_range_endpoints(sim) {
        // SV frontend issue #64: unsupported set-membership assignment expressions.
        @ignore_on(sv);
        @case "veryl_language::inside_outside_range_endpoints";
    }
    fn parameter_expression_type_cast_widths(sim) {
        // SV frontend issue #64: unsupported cast expressions.
        @ignore_on(sv);
        @case "veryl_language::parameter_expression_type_cast_widths";
    }
    fn packed_union_members_alias(sim) {
        // SV frontend issue #64: unsupported packed struct/union types.
        @ignore_on(sv);
        @case "veryl_language::packed_union_members_alias";
    }
    fn wide_shift_amount_out_of_range(sim) {
        // Celox truncates the 70-bit shift count to 64 bits: 2^64+1 yields 0x52 instead of zero.
        // Evidence and unchanged expectations: UPSTREAM_CASES.md.
        @ignore_on(native, cranelift, wasm, interp, sv);
        @case "veryl_regressions::wide_shift_amount_out_of_range";
    }
    fn unary_minus_as_shift_amount(sim) {
        @case "veryl_regressions::unary_minus_as_shift_amount";
    }
    fn struct_bit_field_rhs_no_spill(sim) {
        // SV frontend issue #64: unsupported packed struct/union types.
        @ignore_on(sv);
        @case "veryl_regressions::struct_bit_field_rhs_no_spill";
    }
    fn wide_struct_bit_field_rhs_no_spill(sim) {
        // Wasm clears neighboring bits 100..103 in the 200-bit packed structure.
        // SV frontend issue #64 also rejects packed struct/union types.
        // Evidence and unchanged expectations: UPSTREAM_CASES.md.
        @ignore_on(wasm, sv);
        @case "veryl_regressions::wide_struct_bit_field_rhs_no_spill";
    }
    fn wide_ternary_narrow_branch_no_spill(sim) {
        @case "veryl_regressions::wide_ternary_narrow_branch_no_spill";
    }
    fn nested_array_index_const_array(sim) {
        // Celox reads zero from the constant array at idx=1; expected inner=3 and nested=33.
        // SV frontend issue #64 rejects the constant-array assignment expression.
        // Evidence and unchanged expectations: UPSTREAM_CASES.md.
        @ignore_on(native, cranelift, wasm, interp, sv);
        @case "veryl_regressions::nested_array_index_const_array";
    }
    fn inst_port_default_value_connected_not_folded(sim) {
        @case "veryl_regressions::inst_port_default_value_connected_not_folded";
    }
    fn inlined_function_per_callsite_scratch_in_continuous_assign(sim) {
        // SV frontend issue #64: unsupported cast expressions.
        @ignore_on(sv);
        @case "veryl_regressions::inlined_function_per_callsite_scratch_in_continuous_assign";
    }
}
