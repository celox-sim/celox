//! Constructs commonly found in synthesizable RTL, checked against a software model.

use super::*;

sv_backends! {
    fn enum_members_without_values_follow_their_predecessor(sim) {
        @setup {
            let source = r#"
                module Top(input logic [2:0] a, output logic [3:0] y);
                    typedef enum logic [2:0] { W = 2, X = 5, Y, Z, V = 0, U } e_t;
                    always_comb begin
                        y = 4'hf;
                        if (a == W) y = 4'd0;
                        if (a == X) y = 4'd1;
                        if (a == Y) y = 4'd2;
                        if (a == Z) y = 4'd3;
                        if (a == V) y = 4'd4;
                        if (a == U) y = 4'd5;
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("enum_implicit.sv"))], "Top");
        let a = sim.signal("a");
        // W=2, X=5, Y=6, Z=7, V=0, U=1; values 3 and 4 name no member.
        for (value, expected) in [(2u8, 0u8), (5, 1), (6, 2), (7, 3), (0, 4), (1, 5), (3, 15), (4, 15)] {
            sim.modify(|io| io.set(a, value)).unwrap();
            assert_eq!(sim.get(sim.signal("y")), expected.into(), "a={value}");
        }
    }

    fn always_star_and_edge_sensitive_always_match_the_systemverilog_keywords(sim) {
        @setup {
            let source = r#"
                module Top(input logic clk, input logic [7:0] a, b,
                           output logic [7:0] sum, q);
                    always @(*) sum = a + b;
                    always @(posedge clk) q <= a ^ b;
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("verilog_always.sv"))], "Top");
        let (a, b) = (sim.signal("a"), sim.signal("b"));
        for (x, y) in [(1u8, 2u8), (250, 10), (0, 0)] {
            sim.modify(|io| { io.set(a, x); io.set(b, y); }).unwrap();
            assert_eq!(sim.get(sim.signal("sum")), x.wrapping_add(y).into());
            sim.tick(sim.event("clk")).unwrap();
            assert_eq!(sim.get(sim.signal("q")), (x ^ y).into());
        }
    }

    fn net_declaration_assignment_drives_the_net(sim) {
        @setup {
            let source = r#"
                module Top(input logic [7:0] a, output logic [7:0] y);
                    wire [7:0] w = a + 8'd1;
                    assign y = w;
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("wire_init.sv"))], "Top");
        let a = sim.signal("a");
        for value in [0u8, 1, 254, 255] {
            sim.modify(|io| io.set(a, value)).unwrap();
            assert_eq!(sim.get(sim.signal("y")), value.wrapping_add(1).into());
        }
    }

    fn assigning_the_function_name_returns_its_last_value(sim) {
        @setup {
            let source = r#"
                module Top(input logic [7:0] a, input logic [1:0] sel,
                           output logic [7:0] clamped, doubled, picked);
                    function automatic logic [7:0] clamp(input logic [7:0] x);
                        clamp = x;
                        if (x > 8'd10) clamp = 8'd10;
                    endfunction
                    function automatic logic [7:0] twice_plus_one(input logic [7:0] x);
                        logic [7:0] t;
                        t = x * 2;
                        twice_plus_one = t + 1;
                    endfunction
                    function automatic logic [7:0] pick(input logic [1:0] s);
                        case (s)
                            0: pick = 8'd10;
                            1: pick = 8'd20;
                            default: pick = 8'd30;
                        endcase
                    endfunction
                    assign clamped = clamp(a);
                    always_comb doubled = twice_plus_one(a);
                    assign picked = pick(sel);
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("function_name_return.sv"))], "Top");
        let (a, sel) = (sim.signal("a"), sim.signal("sel"));
        for (value, select) in [(3u8, 0u8), (50, 1), (100, 2), (10, 3)] {
            sim.modify(|io| { io.set(a, value); io.set(sel, select); }).unwrap();
            assert_eq!(sim.get(sim.signal("clamped")), value.min(10).into());
            assert_eq!(sim.get(sim.signal("doubled")), value.wrapping_mul(2).wrapping_add(1).into());
            let picked = match select { 0 => 10u8, 1 => 20, _ => 30 };
            assert_eq!(sim.get(sim.signal("picked")), picked.into());
        }
    }

    fn casez_and_casex_treat_wildcard_bits_as_dont_care(sim) {
        @setup {
            let source = r#"
                module Top(input logic [3:0] a, input logic [7:0] wide,
                           output logic [1:0] z, x, w);
                    always_comb casez (a)
                        4'b1???: z = 3;
                        4'b01??: z = 2;
                        4'b001?: z = 1;
                        default: z = 0;
                    endcase
                    always_comb casex (a)
                        4'b1xxx: x = 3;
                        4'b01xx: x = 2;
                        default: x = 0;
                    endcase
                    // Bits of the selector above the pattern must still match.
                    always_comb casez (wide)
                        4'b1???: w = 3;
                        default: w = 0;
                    endcase
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("casez_casex.sv"))], "Top");
        let (a, wide) = (sim.signal("a"), sim.signal("wide"));
        for value in 0u8..16 {
            sim.modify(|io| { io.set(a, value); io.set(wide, value); }).unwrap();
            let z = if value & 8 != 0 { 3u8 } else if value & 4 != 0 { 2 } else if value & 2 != 0 { 1 } else { 0 };
            let x = if value & 8 != 0 { 3u8 } else if value & 4 != 0 { 2 } else { 0 };
            assert_eq!(sim.get(sim.signal("z")), z.into(), "casez a={value}");
            assert_eq!(sim.get(sim.signal("x")), x.into(), "casex a={value}");
            assert_eq!(sim.get(sim.signal("w")), (u8::from(value & 8 != 0) * 3).into(), "wide={value}");
        }
        sim.modify(|io| io.set(wide, 0x18u8)).unwrap();
        assert_eq!(sim.get(sim.signal("w")), 0u8.into(), "an upper selector bit must not match");
    }

    fn inside_matches_values_ranges_and_wildcards(sim) {
        @setup {
            let source = r#"
                module Top(input logic [3:0] a, output logic y, output logic [1:0] z);
                    assign y = a inside {4'd0, [4'd9:4'd10], 4'b11??};
                    always_comb begin
                        if (a inside {[4'd0:4'd3]}) z = 1;
                        else if (a inside {4'd8, 4'd9}) z = 2;
                        else z = 0;
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("inside.sv"))], "Top");
        let a = sim.signal("a");
        for value in 0u8..16 {
            sim.modify(|io| io.set(a, value)).unwrap();
            let y = value == 0 || (9..=10).contains(&value) || value >> 2 == 0b11;
            assert_eq!(sim.get(sim.signal("y")), u8::from(y).into(), "a={value}");
            let z = if value <= 3 { 1u8 } else if value == 8 || value == 9 { 2 } else { 0 };
            assert_eq!(sim.get(sim.signal("z")), z.into(), "a={value}");
        }
    }

    fn positional_ports_and_parameters_bind_in_declaration_order(sim) {
        @setup {
            let source = r#"
                module Shift #(parameter W = 4, parameter S = 1)(
                    input logic [W-1:0] a, input logic en, output logic [W-1:0] y, output logic z);
                    assign y = en ? a << S : a;
                    assign z = ^a;
                endmodule
                module Top(input logic [7:0] a, input logic en, output logic [7:0] y,
                           output logic z);
                    Shift #(8, 2) u(a, en, y, z);
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("positional.sv"))], "Top");
        let (a, en) = (sim.signal("a"), sim.signal("en"));
        for (value, enable) in [(3u8, 1u8), (64, 1), (0xa5, 0), (0xff, 1)] {
            sim.modify(|io| { io.set(a, value); io.set(en, enable); }).unwrap();
            let expected = if enable != 0 { value << 2 } else { value };
            assert_eq!(sim.get(sim.signal("y")), expected.into(), "a={value} en={enable}");
            assert_eq!(
                sim.get(sim.signal("z")),
                u8::try_from(value.count_ones() & 1).unwrap().into()
            );
        }
    }

    fn exponentiation_works_in_constant_expressions(sim) {
        @setup {
            let source = r#"
                module Pass #(parameter W = 1)(input logic [W-1:0] a, output logic [W-1:0] y);
                    assign y = a;
                endmodule
                module Top #(parameter N = 3)(input logic [2**N-1:0] a, output logic [7:0] y,
                           output logic [3:0] lanes, output logic eq);
                    localparam int D = 2**4 + 3**2;
                    Pass #(.W(2**3)) u(.a(a), .y(y));
                    for (genvar i = 0; i < 2**2; i++) begin : g
                        assign lanes[i] = ~a[i];
                    end
                    if (2**2 == 4) begin : g4
                        assign eq = (a == D);
                    end else begin : other
                        assign eq = 1'b0;
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("pow.sv"))], "Top");
        let a = sim.signal("a");
        for value in [0u8, 5, 25, 200] {
            sim.modify(|io| io.set(a, value)).unwrap();
            assert_eq!(sim.get(sim.signal("y")), value.into());
            assert_eq!(sim.get(sim.signal("lanes")), (!value & 0xf).into());
            assert_eq!(sim.get(sim.signal("eq")), u8::from(value == 25).into());
        }
    }

    fn packages_provide_types_parameters_functions_and_enums(sim) {
        @setup {
            let source = r#"
                package base_pkg;
                    localparam int BASE = 5;
                endpackage
                package math_pkg;
                    import base_pkg::*;
                    parameter int WIDTH = 8;
                    localparam int OFFSET = BASE + 1;
                    typedef logic [WIDTH-1:0] word_t;
                    typedef enum logic [1:0] { IDLE, RUN, DONE } state_t;
                    function automatic word_t bump(input word_t x);
                        return x + OFFSET;
                    endfunction
                endpackage
                module Top import math_pkg::*; (
                    input word_t a, input logic [1:0] code,
                    output word_t bumped, output logic running, output logic done,
                    output logic [math_pkg::WIDTH-1:0] scoped);
                    state_t state;
                    always_comb begin
                        state = state_t'(code);
                        running = (state == RUN);
                        done = (code == math_pkg::DONE);
                    end
                    assign bumped = bump(a);
                    assign scoped = a ^ math_pkg::OFFSET;
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("packages.sv"))], "Top");
        let (a, code) = (sim.signal("a"), sim.signal("code"));
        for (value, state) in [(0u8, 0u8), (10, 1), (250, 2), (255, 3)] {
            sim.modify(|io| { io.set(a, value); io.set(code, state); }).unwrap();
            assert_eq!(sim.get(sim.signal("bumped")), value.wrapping_add(6).into(), "a={value}");
            assert_eq!(sim.get(sim.signal("running")), u8::from(state == 1).into());
            assert_eq!(sim.get(sim.signal("done")), u8::from(state == 2).into());
            assert_eq!(sim.get(sim.signal("scoped")), (value ^ 6).into());
        }
    }

    fn block_locals_and_dependent_assignments_accumulate(sim) {
        @setup {
            let source = r#"
                module Top(input logic [7:0] a, output logic [3:0] ones, output logic [7:0] chain,
                           output logic [7:0] swapped);
                    always_comb begin
                        logic [3:0] count;
                        count = 0;
                        for (int i = 0; i < 8; i++) count = count + a[i];
                        ones = count;
                    end
                    always_comb begin
                        chain = a;
                        chain = chain + 1;
                        chain = chain * 2;
                        chain = chain + 3;
                    end
                    always_comb begin : swap
                        logic [3:0] hi, lo;
                        hi = a[7:4];
                        lo = a[3:0];
                        swapped = {lo, hi};
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("accumulate.sv"))], "Top");
        let a = sim.signal("a");
        for value in [0u8, 1, 7, 0x5a, 0xff] {
            sim.modify(|io| io.set(a, value)).unwrap();
            assert_eq!(sim.get(sim.signal("ones")), u8::try_from(value.count_ones()).unwrap().into());
            let chain = value.wrapping_add(1).wrapping_mul(2).wrapping_add(3);
            assert_eq!(sim.get(sim.signal("chain")), chain.into());
            assert_eq!(sim.get(sim.signal("swapped")), value.rotate_left(4).into());
        }
    }
}

#[test]
fn block_local_names_may_not_shadow_other_signals() {
    for source in [
        "module Top(input logic a, output logic t); \
         always_comb begin logic t; t = a; end endmodule",
        "module Top(input logic a, output logic y, z); \
         always_comb begin logic t; t = a; y = t; end \
         always_comb begin logic t; t = ~a; z = t; end endmodule",
    ] {
        let error = Simulator::from_sv_sources(vec![(source, Path::new("shadow.sv"))], "Top")
            .build_cranelift()
            .expect_err("a shadowing block-local name must be rejected")
            .to_string();
        assert!(
            error.contains("duplicate internal signal")
                || error.contains("duplicate port or signal name"),
            "{error}"
        );
    }
}
