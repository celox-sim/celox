#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

// Run the remaining Celox-only failures explicitly with:
// cargo test -p celox --test veryl_context_regressions -- --include-ignored --skip ::sv
all_backends! {
    fn part_select_of_signed_is_unsigned(sim) {
        // SV frontend cannot lower this always_comb assignment expression.
        @ignore_on(sv);
        @case "veryl_context_regressions::part_select_of_signed_is_unsigned";
    }
    fn signed_part_select_sign_extends(sim) {
        // SV frontend cannot lower the loop/break statement.
        @ignore_on(sv);
        @case "veryl_context_regressions::signed_part_select_sign_extends";
    }
    fn signed_struct_member_sign_extends(sim) {
        // SV frontend cannot lower this member assignment expression.
        @ignore_on(sv);
        @case "veryl_context_regressions::signed_struct_member_sign_extends";
    }
    fn wide_logical_operand_keeps_result_type(sim) {
        // SV frontend cannot lower the emitted cast.
        @ignore_on(sv);
        @case "veryl_context_regressions::wide_logical_operand_keeps_result_type";
    }
    fn constant_ternary_keeps_both_arm_types(sim) {
        @ignore_on(sv);
        @case "veryl_context_regressions::constant_ternary_keeps_both_arm_types";
    }
    fn signed_cast_of_folded_constant_sign_extends(sim) {
        @ignore_on(sv);
        @case "veryl_context_regressions::signed_cast_of_folded_constant_sign_extends";
    }
    fn case_on_signed_target_matches_negative_labels(sim) {
        // SV frontend cannot lower the emitted cast.
        @ignore_on(sv);
        @case "veryl_context_regressions::case_on_signed_target_matches_negative_labels";
    }
    fn constant_case_on_signed_target(sim) {
        @ignore_on(sv);
        @case "veryl_context_regressions::constant_case_on_signed_target";
    }
    fn dynamic_index_store_is_cut_to_element_width(sim) {
        // SV frontend rejects the dynamic array write following partial writes.
        @ignore_on(sv);
        @case "veryl_context_regressions::dynamic_index_store_is_cut_to_element_width";
    }
    fn runtime_for_with_negative_bound(sim) {
        // SV frontend does not support procedural loops in always_comb.
        @ignore_on(sv);
        @case "veryl_context_regressions::runtime_for_with_negative_bound";
    }
    fn folded_constant_wider_than_its_operand(sim) {
        @case "veryl_context_regressions::folded_constant_wider_than_its_operand";
    }
    fn folded_const_select_keeps_its_sign(sim) {
        // Celox still returns 0 rather than 0xfffb for runtime-selected y5.
        @ignore_on(native, cranelift, wasm, interp, sv);
        @case "veryl_context_regressions::folded_const_select_keeps_its_sign";
    }
    fn runtime_for_bound_keeps_its_type(sim) {
        @ignore_on(sv);
        @case "veryl_context_regressions::runtime_for_bound_keeps_its_type";
    }
    fn case_compares_each_label_as_an_if_does(sim) {
        // Celox still returns 0 rather than 2 for the mixed-sign y7 case.
        @ignore_on(native, cranelift, wasm, interp, sv);
        @case "veryl_context_regressions::case_compares_each_label_as_an_if_does";
    }
    fn runtime_case_target_uses_comparison_context(sim) {
        // SV frontend also truncates the case target before comparison (0 instead of 2).
        @ignore_on(sv);
        @case "veryl_context_regressions::runtime_case_target_uses_comparison_context";
    }
    fn runtime_for_bound_arithmetic_uses_int_context(sim) {
        // SV frontend does not support procedural loops in always_comb.
        @ignore_on(sv);
        @case "veryl_context_regressions::runtime_for_bound_arithmetic_uses_int_context";
    }
}

all_backends! {
    fn ff_loop_bound_keeps_its_type(sim) {
        @ignore_on(sv);
        @setup { let code = r#"
            module Top #(
                param UNSIGNED_END: u32 = 2,
                param SIGNED_END: i32 = 2,
            ) (
                clk: input clock,
                lo: input i32,
                hi: input u32,
                unsigned_count: output u32,
                signed_count: output u32,
                dynamic_count: output u32,
                inclusive_count: output u32,
            ) {
                always_ff (clk) {
                    var cu: u32;
                    var cs: u32;
                    var cd: u32;
                    var ci: u32;
                    cu = 0;
                    for j in lo..UNSIGNED_END { cu += 1 + (j - j); }
                    cs = 0;
                    for j in lo..SIGNED_END { cs += 1 + (j - j); }
                    cd = 0;
                    for j in lo..hi { cd += 1 + (j - j); }
                    ci = 0;
                    for j in lo..=UNSIGNED_END { ci += 1 + (j - j); }
                    unsigned_count = cu;
                    signed_count = cs;
                    dynamic_count = cd;
                    inclusive_count = ci;
                }
            }
        "#; }
        @build celox::Simulator::builder(code, "Top");
        let clk = sim.event("clk");
        let lo = sim.signal("lo");
        let hi = sim.signal("hi");
        sim.modify(|io| {
            io.set(lo, -2i32);
            io.set(hi, 2u32);
        }).unwrap();
        sim.tick(clk).unwrap();
        for (name, expected) in [
            ("unsigned_count", 0u32),
            ("signed_count", 4u32),
            ("dynamic_count", 0u32),
            ("inclusive_count", 0u32),
        ] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), expected.into(), "{name}");
        }
    }
}
