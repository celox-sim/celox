#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {
    fn part_select_of_signed_is_unsigned(sim) {
        @case "veryl_context_regressions::part_select_of_signed_is_unsigned";
    }
    fn signed_part_select_sign_extends(sim) {
        @case "veryl_context_regressions::signed_part_select_sign_extends";
    }
    fn signed_struct_member_sign_extends(sim) {
        @case "veryl_context_regressions::signed_struct_member_sign_extends";
    }
    fn wide_logical_operand_keeps_result_type(sim) {
        @case "veryl_context_regressions::wide_logical_operand_keeps_result_type";
    }
    fn constant_ternary_keeps_both_arm_types(sim) {
        @ignore_on(sv);
        @case "veryl_context_regressions::constant_ternary_keeps_both_arm_types";
    }
    fn signed_cast_of_folded_constant_sign_extends(sim) {
        @case "veryl_context_regressions::signed_cast_of_folded_constant_sign_extends";
    }
    fn case_on_signed_target_matches_negative_labels(sim) {
        @case "veryl_context_regressions::case_on_signed_target_matches_negative_labels";
    }
    fn constant_case_on_signed_target(sim) {
        @case "veryl_context_regressions::constant_case_on_signed_target";
    }
    fn dynamic_index_store_is_cut_to_element_width(sim) {
        @case "veryl_context_regressions::dynamic_index_store_is_cut_to_element_width";
    }
    fn runtime_for_with_negative_bound(sim) {
        @case "veryl_context_regressions::runtime_for_with_negative_bound";
    }
    fn folded_constant_wider_than_its_operand(sim) {
        @case "veryl_context_regressions::folded_constant_wider_than_its_operand";
    }
    fn folded_const_select_keeps_its_sign(sim) {
        @case "veryl_context_regressions::folded_const_select_keeps_its_sign";
    }
    fn runtime_for_bound_keeps_its_type(sim) {
        @ignore_on(sv);
        @case "veryl_context_regressions::runtime_for_bound_keeps_its_type";
    }
    fn case_compares_each_label_as_an_if_does(sim) {
        @case "veryl_context_regressions::case_compares_each_label_as_an_if_does";
    }
    fn runtime_case_target_uses_comparison_context(sim) {
        @case "veryl_context_regressions::runtime_case_target_uses_comparison_context";
    }
    fn runtime_for_bound_arithmetic_uses_int_context(sim) {
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

all_backends! {
    fn dynamic_param_array_read_uses_its_elements(sim) {
        @ignore_on(sv);
        @setup { let code = r#"
            module Child #(
                param Q: i8 [2] = '{1, 2},
            ) (
                clk: input  clock,
                i  : input  logic<1>,
                c  : output logic<16>,
                f  : output logic<16>,
            ) {
                assign c = Q[i];
                always_ff (clk) {
                    f = Q[i];
                }
            }
            module Top (
                clk: input  clock,
                i  : input  logic<1>,
                c0 : output logic<16>,
                f0 : output logic<16>,
                c1 : output logic<16>,
                f1 : output logic<16>,
            ) {
                inst u0: Child (clk, i, c: c0, f: f0);
                inst u1: Child #(Q: '{-3, -5}) (clk, i, c: c1, f: f1);
            }
        "#; }
        @build celox::Simulator::builder(code, "Top");
        let clk = sim.event("clk");
        let i = sim.signal("i");
        for (index, expected) in [(0u8, [1u16, 1, 0xfffd, 0xfffd]), (1, [2, 2, 0xfffb, 0xfffb])] {
            sim.modify(|io| io.set(i, index)).unwrap();
            sim.tick(clk).unwrap();
            for (name, expected) in ["c0", "f0", "c1", "f1"].into_iter().zip(expected) {
                let signal = sim.signal(name);
                assert_eq!(sim.get(signal), expected.into(), "{name} at i={index}");
            }
        }
    }
}

all_backends! {
    fn constant_case_target_uses_comparison_context(sim) {
        @ignore_on(sv);
        @setup { let code = r#"
            module Top (
                clk: input  clock,
                y0 : output logic<8>,
                y1 : output logic<8>,
                y2 : output logic<8>,
                y3 : output logic<8>,
                y4 : output logic<8>,
                y5 : output logic<8>,
                f0 : output logic<8>,
                f3 : output logic<8>,
                f1 : output logic<8>,
                f2 : output logic<8>,
            ) {
                const J: u8 = 8'hFF;
                function g (
                    n: input logic<8>,
                ) -> logic<8> {
                    var r: logic<8>;
                    case J + n {
                        16'h0100: r = 2;
                        default : r = 0;
                    }
                    return r;
                }
                always_comb {
                    y5 = g(8'h01);
                    case J + 8'h01 {
                        16'h0100: y0 = 2;
                        default : y0 = 0;
                    }
                    case (J + 8'h01) >> 1 {
                        16'h0080: y1 = 2;
                        default : y1 = 0;
                    }
                    case J + 8'h01 {
                        16'h0000: y2 = 1;
                        default : y2 = 3;
                    }
                    case J + 8'h01 {
                        16'h00FF..=16'h0100: y3 = 2;
                        default            : y3 = 0;
                    }
                    case J + 8'h01 {
                        8'h00  : y4 = 2;
                        default: y4 = 0;
                    }
                }
                always_ff (clk) {
                    f3 = g(8'h01);
                    case J + 8'h01 {
                        16'h0100: f0 = 2;
                        default : f0 = 0;
                    }
                    case (J + 8'h01) >> 1 {
                        16'h0080: f1 = 2;
                        default : f1 = 0;
                    }
                    case J + 8'h01 {
                        16'h0000: f2 = 1;
                        default : f2 = 3;
                    }
                }
            }
        "#; }
        @build celox::Simulator::builder(code, "Top");
        let clk = sim.event("clk");
        sim.tick(clk).unwrap();
        for (name, expected) in [
            ("y0", 2u8),
            ("y1", 2),
            ("y2", 3),
            ("y3", 2),
            ("y4", 2),
            ("y5", 2),
            ("f0", 2),
            ("f3", 2),
            ("f1", 2),
            ("f2", 3),
        ] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), expected.into(), "{name}");
        }
    }
}

all_backends! {
    fn unfolded_constant_struct_member_reads_its_value(sim) {
        @ignore_on(sv);
        @setup { let code = r#"
            package pkg {
                struct S {
                    m: signed logic<4>,
                    k: logic<4>,
                }
            }
            module Top (
                a : input  logic<8>,
                y0: output logic<16>,
                y1: output logic<16>,
                y2: output logic<16>,
                y3: output logic<16>,
            ) {
                const LS: pkg::S = pkg::S'{ m: -3, k: 5 };
                always_comb {
                    y0 = LS.m + a;
                    y1 = LS.m[3:0] + a;
                    y2 = LS.k[3:0] + a;
                    y3 = LS.m[1:0] + a;
                }
            }
        "#; }
        @build celox::Simulator::builder(code, "Top");
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0u8)).unwrap();
        for (name, expected) in [("y0", 0xdu16), ("y1", 0xd), ("y2", 5), ("y3", 1)] {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), expected.into(), "{name}");
        }
    }
}
