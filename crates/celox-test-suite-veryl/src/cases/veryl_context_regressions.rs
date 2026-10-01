// Regression probes adapted from Veryl simulator expression tests
// at d17ce955ef2990af3201ca9529159eca6248aa76.
// Width and sign oracles follow IEEE 1800-2023 11.8.1 and 11.8.2.
use crate::{BigUint, Design};

fn check(sim: &mut crate::Simulator, inputs: &[(&str, BigUint)], expected: &[(&str, BigUint)]) {
    sim.modify(|io| {
        for (name, value) in inputs {
            let signal = io.signal(name);
            io.set_wide(signal, value.clone());
        }
    })
    .unwrap();
    for (name, value) in expected {
        let signal = sim.signal(name);
        assert_eq!(sim.get(signal), *value, "output {name}");
    }
}

cases! { Regression, "veryl_context_regressions";
    fn part_select_of_signed_is_unsigned(sim) {
        // `x[1:0]` is unsigned whatever `x` is, so the sum is unsigned and
        // `$signed(a[3:0])` zero-extends: 4'he + 1 = 8'h0f, not 8'hff.
        @build Design::new(
            r#"
            module Top (
                a : input  logic<8>,
                y0: output logic<8>,
                y1: output logic<8>,
            ) {
                var x: i32;
                always_comb {
                    x  = 1;
                    y0 = $signed(a[3:0]) + x[1:0];
                }
                always_comb {
                    y1 = 0;
                    for i in 0..2 {
                        if i == 1 {
                            y1 = $signed(a[3:0]) + i[1:0];
                        }
                    }
                }
            }
            "#,
            "Top",
        );
        check(
            &mut sim,
            &[("a", BigUint::from(0x0eu64))],
            &[
                ("y0", BigUint::from(0x0fu64)),
                ("y1", BigUint::from(0x0fu64)),
            ],
        );
    }
    fn signed_part_select_sign_extends(sim) {
        // `$signed` of a part-select is signed, so it sign-extends both in a
        // signed operation and stored bare to a wider variable.
        @build Design::new(
            r#"
            module Top (
                a : input  logic<8>,
                s0: output signed logic<16>,
                s1: output logic<16>,
            ) {
                always_comb {
                    s0 = 0;
                    for i in 0..2 {
                        if a == 99 {
                            break;
                        }
                        if i == 0 {
                            s0 = s0 + $signed(a[3:0]);
                        }
                    }
                    s1 = $signed(a[3:0]);
                }
            }
            "#,
            "Top",
        );
        check(
            &mut sim,
            &[("a", BigUint::from(0x08u64))],
            &[
                ("s0", BigUint::from(0xfff8u64)),
                ("s1", BigUint::from(0xfff8u64)),
            ],
        );
    }
    fn signed_struct_member_sign_extends(sim) {
        // A member read keeps the member's signedness; a select of it does not.
        @build Design::new(
            r#"
            module Top (
                a : input  logic<8>,
                y0: output logic<16>,
                y1: output logic<16>,
                y2: output logic<16>,
                y3: output logic<16>,
            ) {
                struct S {
                    m: signed logic<8>,
                    n: logic<8>,
                }
                var s: S;
                always_comb {
                    s.m = a;
                    s.n = a;
                    y0  = s.m;
                    y1  = s.m + 16'sd0;
                    y2  = $signed(s.m[3:0]) + s.m[7:4];
                    y3  = s.m[7:0] + 16'sd0;
                }
            }
            "#,
            "Top",
        );
        check(
            &mut sim,
            &[("a", BigUint::from(0xfeu64))],
            &[
                ("y0", BigUint::from(0xfffeu64)),
                ("y1", BigUint::from(0xfffeu64)),
                ("y2", BigUint::from(0x001du64)),
                ("y3", BigUint::from(0x00feu64)),
            ],
        );
    }
    fn wide_logical_operand_keeps_result_type(sim) {
        // A condition wider than one bit only warns; the ternary still has its
        // arms' type, here inside a concatenation that sizes by it.
        @build Design::new(
            r#"
            module Top #(
                param R: signed logic<16> = -300,
            ) (
                a: input  logic<8>,
                b: input  logic<4>,
                y: output logic<16>,
                z: output logic<16>,
                t: output logic<4>,
            ) {
                always_comb {
                    y = {(if 1 ? a : b), 4'h5};
                    z = {!a, (a && b), 4'h5};
                    t = (if 16'sh8001 ? $signed((8'hf0 <= -2) as 4) : R);
                }
            }
            "#,
            "Top",
        );
        check(
            &mut sim,
            &[
                ("a", BigUint::from(0xfeu64)),
                ("b", BigUint::from(0x3u64)),
            ],
            &[
                ("y", BigUint::from(0x0fe5u64)),
                ("z", BigUint::from(0x0015u64)),
                ("t", BigUint::from(0x1u64)),
            ],
        );
    }
    fn constant_ternary_keeps_both_arm_types(sim) {
        // A constant condition must not fold the ternary to an arm whose type
        // differs from the other's: `X[0]` is a 1-bit unsigned select, and an
        // unsigned arm makes the whole ternary unsigned.
        @build Design::new(
            r#"
            module Top #(
                param Q: signed logic<4> = -3,
                param X: i32             = 0,
            ) (
                a : input  logic<8>,
                t : output logic<4>,
                y0: output logic<16>,
                y1: output logic<16>,
            ) {
                var sa: signed logic<8>;
                always_comb {
                    sa = a;
                    t  = (if Q ? $signed(a[3:0]) : X[0]) >> 1;
                    y0 = (if 1 ? sa + 8'sd0 : a - 8'd0);
                    y1 = {(if 1 ? a[3:0] : a[7:0]), 4'h5};
                }
            }
            "#,
            "Top",
        );
        check(
            &mut sim,
            &[("a", BigUint::from(0xf8u64))],
            &[
                ("t", BigUint::from(0x4u64)),
                ("y0", BigUint::from(0x00f8u64)),
                ("y1", BigUint::from(0x0085u64)),
            ],
        );
    }
    fn signed_cast_of_folded_constant_sign_extends(sim) {
        // `$signed((P + 6'd8) as 4)` is 4'sb1011, so adding a signed zero
        // sign-extends it whether or not the whole source folds.
        @build Design::new(
            r#"
            module Top #(
                param P: u32 = 3,
            ) (
                a : input  logic<8>,
                y0: output signed logic<8>,
                y1: output signed logic<8>,
            ) {
                always_comb {
                    y0 = ($signed((P + 6'd8) as 4) + 0) | ($signed(a[3:0]) * 0);
                    y1 = $signed((P + 6'd8) as 4) + 0;
                }
            }
            "#,
            "Top",
        );
        check(
            &mut sim,
            &[("a", BigUint::from(0u64))],
            &[
                ("y0", BigUint::from(0xfbu64)),
                ("y1", BigUint::from(0xfbu64)),
            ],
        );
    }
    fn case_on_signed_target_matches_negative_labels(sim) {
        // A label folds to a constant that must keep its signedness, or a signed
        // target is compared zero-extended and misses `-1` and `-8..=-1`.
        @build Design::new(
            r#"
            module Top (
                a : input  logic<8>,
                y0: output logic<8>,
                y1: output logic<8>,
                y2: output logic<8>,
            ) {
                var n: i8;
                always_comb {
                    n = a as i8;
                    case n {
                        -1     : y0 = 1;
                        default: y0 = 2;
                    }
                    case n {
                        -8..=-1: y1 = 1;
                        0..=7  : y1 = 2;
                        default: y1 = 3;
                    }
                    y2 = case n {
                        -2..=-1: 8'd1,
                        default: 8'd2,
                    };
                }
            }
            "#,
            "Top",
        );
        for (a, y0, y1, y2) in [(0xff, 1, 1, 1), (0xf9, 2, 1, 2), (0x03, 2, 2, 2)] {
            check(
                &mut sim,
                &[("a", BigUint::from(a as u64))],
                &[
                    ("y0", BigUint::from(y0 as u64)),
                    ("y1", BigUint::from(y1 as u64)),
                    ("y2", BigUint::from(y2 as u64)),
                ],
            );
        }
    }
    fn constant_case_on_signed_target(sim) {
        // A case evaluated at elaboration compares like the runtime one.
        @build Design::new(
            r#"
            module Top (
                y0: output logic<8>,
                y1: output logic<8>,
            ) {
                function f (
                    n: input i8,
                ) -> logic<8> {
                    var r: logic<8>;
                    case n {
                        -1     : r = 1;
                        -8..=-2: r = 3;
                        default: r = 2;
                    }
                    return r;
                }
                const C0: logic<8> = f(-1);
                const C1: logic<8> = f(-5);
                assign y0 = C0;
                assign y1 = C1;
            }
            "#,
            "Top",
        );
        check(
            &mut sim,
            &[],
            &[
                ("y0", BigUint::from(1u64)),
                ("y1", BigUint::from(3u64)),
            ],
        );
    }
    fn dynamic_index_store_is_cut_to_element_width(sim) {
        // An 8-bit element has 4-byte storage; a wider source must not leave
        // bits above the element there for a reader of the whole word.
        @build Design::new(
            r#"
            module Top (
                a: input  logic<8>,
                y: output logic<64>,
            ) {
                var arr: logic<8> [3, 2];
                always_comb {
                    for k in 0..3 {
                        arr[k][0] = 8'h11;
                        arr[k][1] = 8'h22;
                    }
                    arr[1][a[0]] = a + 32'd2;
                    y = {16'd0, arr[0][0], arr[0][1], arr[1][0], arr[1][1], arr[2][0], arr[2][1]};
                }
            }
            "#,
            "Top",
        );
        check(
            &mut sim,
            &[("a", BigUint::from(0xfeu64))],
            &[("y", BigUint::from(0x0000_1122_0022_1122u64))],
        );
        check(
            &mut sim,
            &[("a", BigUint::from(0xffu64))],
            &[("y", BigUint::from(0x0000_1122_1101_1122u64))],
        );
    }
    fn runtime_for_with_negative_bound(sim) {
        // The emitted SV iterates `int j`: a bound of -2 runs from -2, and one
        // derived from an enclosing loop's iterator may be negative too.
        @build Design::new(
            r#"
            module Top (
                a : input  logic<8>,
                y0: output logic<32>,
                y1: output logic<32>,
            ) {
                var lo: i32;
                var n : logic<8>;
                always_comb {
                    lo = -2;
                    y0 = 0;
                    for j in lo..2 {
                        y0 = y0 + a + 1 + (j - j);
                    }
                }
                always_comb {
                    n  = 0;
                    y1 = 0;
                    for i in 0..4 {
                        if n == 3 {
                            break;
                        }
                        for j in (i - 2)..2 {
                            y1 = y1 + a + 1 + (j - j);
                        }
                        for k in 0..(i - 1) {
                            y1 = y1 + 1000 + (k - k);
                        }
                        n = n + 1;
                    }
                }
            }
            "#,
            "Top",
        );
        check(
            &mut sim,
            &[("a", BigUint::from(1u64))],
            &[
                ("y0", BigUint::from(8u64)),
                ("y1", BigUint::from(0x3fau64)),
            ],
        );
    }
    fn folded_constant_wider_than_its_operand(sim) {
        // `1 << 70` folds to a value computed at the 128-bit context width.
        @build Design::new(
            r#"
            module Top (
                y: output logic<128>,
            ) {
                always_comb {
                    y = 1 << 70;
                }
            }
            "#,
            "Top",
        );
        check(&mut sim, &[], &[("y", BigUint::from(1u32) << 70u32)]);
    }
    fn folded_const_select_keeps_its_sign(sim) {
        // A whole constant element or member keeps its type's sign when folded;
        // a part-select of one is unsigned.
        @build Design::new(
            r#"
            package pkg {
                struct S {
                    m: signed logic<4>,
                    k: logic<4>,
                }
                const CS: S = S'{ m: -3, k: 5 };
                const PC: i32 = -4;
                const PA: i8 [2] = '{-3, -5};
            }
            module Top #(
                param P: i32 = -4,
                param Q: i8 [2] = '{-3, -5},
            ) (
                a : input  logic<8>,
                i : input  logic<1>,
                y0: output logic<16>,
                y1: output logic<16>,
                y2: output logic<16>,
                y3: output logic<16>,
                y4: output logic<16>,
                y5: output logic<16>,
                y6: output logic<16>,
                y7: output logic<16>,
                y8: output logic<16>,
                y9: output logic<16>,
            ) {
                const C : i32    = -4;
                const LS: pkg::S = pkg::S'{ m: -3, k: 5 };
                always_comb {
                    y0 = P[3:0] + 16'sd0;
                    y1 = C[3:0] + 16'sd0;
                    y2 = pkg::PC[3:0] + 16'sd0;
                    y3 = Q[0] + 16'sd0;
                    y4 = pkg::PA[1] + 16'sd0;
                    y5 = Q[i] + 16'sd0;
                    y6 = pkg::PA[i][3:0] + 16'sd0 + a[0];
                    y7 = pkg::CS.m + 16'sd0;
                    y8 = LS.m + 16'sd0;
                    y9 = LS.m[3:0] + 16'sd0 + a;
                }
            }
            "#,
            "Top",
        );
        check(
            &mut sim,
            &[
                ("a", BigUint::from(0u64)),
                ("i", BigUint::from(1u64)),
            ],
            &[
                ("y0", BigUint::from(0x000cu64)),
                ("y1", BigUint::from(0x000cu64)),
                ("y2", BigUint::from(0x000cu64)),
                ("y3", BigUint::from(0xfffdu64)),
                ("y4", BigUint::from(0xfffbu64)),
                ("y5", BigUint::from(0xfffbu64)),
                ("y6", BigUint::from(0x000bu64)),
                ("y7", BigUint::from(0xfffdu64)),
                ("y8", BigUint::from(0xfffdu64)),
                ("y9", BigUint::from(0x000du64)),
            ],
        );
    }
    fn runtime_for_bound_keeps_its_type(sim) {
        // `j < N` compares unsigned when `N` is unsigned, so a negative start
        // runs no iteration; a bound is evaluated at the 32-bit `int` width, so
        // `a + 8'd1` is 256, not 0.
        @build Design::new(
            r#"
            module Top #(
                param N: u32 = 2,
            ) (
                a : input  logic<8>,
                b : input  logic<8>,
                y0: output logic<32>,
                y1: output logic<32>,
                y2: output logic<32>,
                y3: output logic<32>,
                y4: output logic<32>,
            ) {
                var lo: i32;
                var hi: logic<8>;
                always_comb {
                    lo = -2;
                    y0 = 0;
                    for j in lo..N {
                        y0 = y0 + 1 + (j - j) + a;
                    }
                }
                always_comb {
                    hi = a + 8'd2;
                    y1 = 0;
                    for j in rev lo..hi {
                        y1 = y1 + 1 + (j - j);
                    }
                }
                always_comb {
                    y2 = 0;
                    for j in 0..(b + 8'd1) {
                        y2 = y2 + 1 + (j - j);
                    }
                }
                always_comb {
                    y3 = 0;
                    for j in rev 0..(b + 8'd1) {
                        y3 = y3 + 1 + (j - j);
                    }
                }
                always_comb {
                    y4 = 0;
                    for j in 0..=(b + 8'd1) {
                        y4 = y4 + 1 + (j - j);
                    }
                }
            }
            "#,
            "Top",
        );
        check(
            &mut sim,
            &[
                ("a", BigUint::from(0u64)),
                ("b", BigUint::from(0xffu64)),
            ],
            &[
                ("y0", BigUint::from(0u64)),
                ("y1", BigUint::from(4u64)),
                ("y2", BigUint::from(0x100u64)),
                ("y3", BigUint::from(0x100u64)),
                ("y4", BigUint::from(0x101u64)),
            ],
        );
    }
    fn case_compares_each_label_as_an_if_does(sim) {
        // `case t { L: .. }` is `if t ==? L`: the target and a label are sized
        // and signed by their pair, and a constant label folds within it.
        @build Design::new(
            r#"
            module Top (
                a : input  logic<8>,
                t : input  logic<32>,
                y1: output logic<8>,
                y2: output logic<8>,
                y3: output logic<8>,
                y4: output logic<8>,
                y5: output logic<8>,
                y6: output logic<8>,
                y7: output logic<8>,
                y8: output logic<8>,
            ) {
                const K: i16 = -1;
                const J: u8 = 8'hFF;
                function f (
                    n: input logic<8>,
                ) -> logic<8> {
                    var r: logic<8>;
                    case n + 8'h10 {
                        9'h105 : r = 2;
                        default: r = 0;
                    }
                    return r;
                }
                const C: logic<8> = f(8'hF5);
                always_comb {
                    case a + 8'h10 {
                        9'h105 : y1 = 2;
                        default: y1 = 0;
                    }
                    case t {
                        K - 2  : y2 = 1;
                        default: y2 = 0;
                    }
                    case t {
                        K - 3..=K - 1: y3 = 1;
                        default      : y3 = 0;
                    }
                    y4 = case t {
                        K - 3..=K - 1: 8'd1,
                        default      : 8'd0,
                    };
                    y5 = case a + 8'h10 {
                        9'h105 : 8'd2,
                        default: 8'd0,
                    };
                    y6 = C;
                    case J + 8'h01 {
                        16'h0100: y7 = 2;
                        default : y7 = 0;
                    }
                    y8 = case J + 8'h01 {
                        16'h0100: 8'd2,
                        default : 8'd0,
                    };
                }
            }
            "#,
            "Top",
        );
        check(
            &mut sim,
            &[
                ("a", BigUint::from(0xf5u64)),
                ("t", BigUint::from(0xfffdu64)),
            ],
            &[
                ("y1", BigUint::from(2u64)),
                ("y2", BigUint::from(1u64)),
                ("y3", BigUint::from(1u64)),
                ("y4", BigUint::from(1u64)),
                ("y5", BigUint::from(2u64)),
                ("y6", BigUint::from(2u64)),
                ("y7", BigUint::from(2u64)),
                ("y8", BigUint::from(2u64)),
            ],
        );
    }
    fn runtime_case_target_uses_comparison_context(sim) {
        @build Design::new(
            r#"
    module Top (a: input logic<8>, y0: output logic<8>, y1: output logic<8>, y2: output logic<8>, y3: output logic<8>, y4: output logic<8>) {
        var calls: logic<8>;
    function observe(n: input logic<8>, next_value: output logic<8>) -> logic<8> { next_value = n + 1; return next_value; }
    function f(n: input logic<8>) -> logic<8> {
            var r: logic<8>;
            case n + 8'h10 {
                9'h105: r = 2;
                default: r = 0;
            }
            return r;
        }
        always_comb {
            case a + 8'h10 {
                9'h105: y0 = 2;
                default: y0 = 0;
            }
            y1 = case a + 8'h10 { 9'h105: 8'd2, default: 8'd0, };
            y2 = f(a);
        calls = 0;
        // Resizing this comparison must not reevaluate the function call.
        case observe(calls, calls) + 8'd0 { 9'd1: y3 = 1; default: y3 = 0; }
        y4 = calls;
        }
    }
    "#,
            "Top",
        );
        for (a, expected) in [(0xf5u64, 2u64), (0xe5, 0), (0xff, 0)] {
            check(
                &mut sim,
                &[("a", a.into())],
                &[
                    ("y0", expected.into()),
                    ("y1", expected.into()),
                    ("y2", expected.into()),
                    ("y3", 1u8.into()), ("y4", 1u8.into()),
                ],
            );
        }
    }

    fn runtime_for_bound_arithmetic_uses_int_context(sim) {
        @build Design::new(
            r#"
    module Top (b: input logic<8>, y0: output logic<32>, y1: output logic<32>, y2: output logic<32>) {
        always_comb {
            y0 = 0;
            for j in 0..(b + 8'd1) { y0 = y0 + 1 + (j - j); }
        }
        always_comb {
            y1 = 0;
            for j in rev 0..(b + 8'd1) { y1 = y1 + 1 + (j - j); }
        }
        always_comb {
            y2 = 0;
            for j in 0..=(b + 8'd1) { y2 = y2 + 1 + (j - j); }
        }
    }
    "#,
            "Top",
        );
        for b in [0u64, 1, 254, 255] {
            check(
                &mut sim,
                &[("b", b.into())],
                &[
                    ("y0", (b + 1).into()),
                    ("y1", (b + 1).into()),
                    ("y2", (b + 2).into()),
                ],
            );
        }
    }
}
