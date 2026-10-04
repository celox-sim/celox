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

    fn instance_arrays_broadcast_and_slice_connections(sim) {
        @setup {
            let source = r#"
                module Inv(input logic [3:0] a, input logic en, output logic [3:0] y);
                    assign y = en ? ~a : a;
                endmodule
                module Top(input logic [7:0] a, input logic en, output logic [7:0] y);
                    Inv u[1:0](.a(a), .en(en), .y(y));
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("array.sv"))], "Top");
        let (a, en) = (sim.signal("a"), sim.signal("en"));
        for (value, enable) in [(0x3cu8, 1u8), (0xa5, 0), (0xff, 1)] {
            sim.modify(|io| { io.set(a, value); io.set(en, enable); }).unwrap();
            let expected = if enable != 0 { !value } else { value };
            assert_eq!(sim.get(sim.signal("y")), expected.into(), "a={value} en={enable}");
            // `u[1]`, the leftmost element, takes the most significant slice.
            let high = sim.child_signal(&[("u", 1)], "a");
            assert_eq!(sim.get(high), (value >> 4).into(), "a={value}");
        }
    }

    fn instance_arrays_with_ascending_range(sim) {
        @setup {
            let source = r#"
                module Inv(input logic [3:0] a, output logic [3:0] y);
                    assign y = ~a;
                endmodule
                module Top(input logic [7:0] a, output logic [7:0] y);
                    Inv u[0:1](.a(a), .y(y));
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("asc.sv"))], "Top");
        let a = sim.signal("a");
        for value in [0x3cu8, 0xa5, 0xff, 0x01] {
            sim.modify(|io| io.set(a, value)).unwrap();
            assert_eq!(sim.get(sim.signal("y")), (!value).into(), "a={value}");
            // The leftmost element, `u[0]`, takes the most significant slice.
            let high = sim.child_signal(&[("u", 0)], "a");
            let low = sim.child_signal(&[("u", 1)], "a");
            assert_eq!(sim.get(high), (value >> 4).into(), "a={value}");
            assert_eq!(sim.get(low), (value & 0xf).into(), "a={value}");
        }
    }

    fn instance_arrays_with_non_zero_based_range(sim) {
        @setup {
            let source = r#"
                module Inv(input logic [3:0] a, output logic [3:0] y);
                    assign y = ~a;
                endmodule
                module Top(input logic [7:0] a, output logic [7:0] y);
                    Inv u[3:2](.a(a), .y(y));
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("nz.sv"))], "Top");
        let a = sim.signal("a");
        for value in [0x3cu8, 0xa5, 0xff, 0x01] {
            sim.modify(|io| io.set(a, value)).unwrap();
            assert_eq!(sim.get(sim.signal("y")), (!value).into(), "a={value}");
            // The leftmost element, `u[3]`, takes the most significant slice;
            // elements are addressed by their declared index.
            let high = sim.child_signal(&[("u", 3)], "a");
            let low = sim.child_signal(&[("u", 2)], "a");
            assert_eq!(sim.get(high), (value >> 4).into(), "a={value}");
            assert_eq!(sim.get(low), (value & 0xf).into(), "a={value}");
        }
        let hierarchy = sim.named_hierarchy();
        let (_, elements) = hierarchy
            .children
            .iter()
            .find(|(name, _)| name == "u")
            .expect("instance array `u`");
        let indices: Vec<_> = elements.iter().map(|element| element.index).collect();
        assert_eq!(indices, [2, 3]);
    }

    fn instance_array_unpacked_connection_descending_to_descending(sim) {
        @setup {
            // `a` is split from the leftmost instance down; the unpacked array
            // connects its leftmost element to the leftmost instance.
            let source = r#"
                module Inv(input logic [3:0] a, output logic [3:0] y);
                    assign y = ~a;
                endmodule
                module Top(input logic [15:0] a, output logic [15:0] y);
                    logic [3:0] lanes [3:0];
                    Inv u[3:0](.a(a), .y(lanes));
                    assign y = {lanes[3], lanes[2], lanes[1], lanes[0]};
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("unpacked.sv"))], "Top");
        let a = sim.signal("a");
        for value in [0x1234u16, 0xa5c3, 0xffff] {
            sim.modify(|io| io.set(a, value)).unwrap();
            assert_eq!(sim.get(sim.signal("y")), (!value).into(), "a={value:#x}");
        }
    }

    fn instance_array_unpacked_connection_descending_to_ascending(sim) {
        @setup {
            // `a` is split from the leftmost instance down; the unpacked array
            // connects its leftmost element to the leftmost instance.
            let source = r#"
                module Inv(input logic [3:0] a, output logic [3:0] y);
                    assign y = ~a;
                endmodule
                module Top(input logic [15:0] a, output logic [15:0] y);
                    logic [3:0] lanes [0:3];
                    Inv u[3:0](.a(a), .y(lanes));
                    assign y = {lanes[0], lanes[1], lanes[2], lanes[3]};
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("unpacked.sv"))], "Top");
        let a = sim.signal("a");
        for value in [0x1234u16, 0xa5c3, 0xffff] {
            sim.modify(|io| io.set(a, value)).unwrap();
            assert_eq!(sim.get(sim.signal("y")), (!value).into(), "a={value:#x}");
        }
    }

    fn instance_array_unpacked_connection_ascending_to_descending(sim) {
        @setup {
            // `a` is split from the leftmost instance down; the unpacked array
            // connects its leftmost element to the leftmost instance.
            let source = r#"
                module Inv(input logic [3:0] a, output logic [3:0] y);
                    assign y = ~a;
                endmodule
                module Top(input logic [15:0] a, output logic [15:0] y);
                    logic [3:0] lanes [3:0];
                    Inv u[0:3](.a(a), .y(lanes));
                    assign y = {lanes[3], lanes[2], lanes[1], lanes[0]};
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("unpacked.sv"))], "Top");
        let a = sim.signal("a");
        for value in [0x1234u16, 0xa5c3, 0xffff] {
            sim.modify(|io| io.set(a, value)).unwrap();
            assert_eq!(sim.get(sim.signal("y")), (!value).into(), "a={value:#x}");
        }
    }

    fn instance_array_unpacked_connection_offset_ranges(sim) {
        @setup {
            // `a` is split from the leftmost instance down; the unpacked array
            // connects its leftmost element to the leftmost instance.
            let source = r#"
                module Inv(input logic [3:0] a, output logic [3:0] y);
                    assign y = ~a;
                endmodule
                module Top(input logic [15:0] a, output logic [15:0] y);
                    logic [3:0] lanes [1:4];
                    Inv u[4:7](.a(a), .y(lanes));
                    assign y = {lanes[1], lanes[2], lanes[3], lanes[4]};
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("unpacked.sv"))], "Top");
        let a = sim.signal("a");
        for value in [0x1234u16, 0xa5c3, 0xffff] {
            sim.modify(|io| io.set(a, value)).unwrap();
            assert_eq!(sim.get(sim.signal("y")), (!value).into(), "a={value:#x}");
        }
    }

    fn typedef_unpacked_arrays_are_not_instance_arrays(sim) {
        @setup {
            let source = r#"
                module Top(input logic clk, input logic [12:0] addr, input logic [7:0] d,
                           input logic we, output logic [7:0] q, output logic [7:0] m);
                    typedef logic [7:0] byte_t;
                    byte_t mem [0:8191];
                    byte_t grid [2][3];
                    always_ff @(posedge clk) if (we) mem[addr] <= d;
                    assign q = mem[addr];
                    assign grid[1][2] = d;
                    assign m = grid[1][2];
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("typedef_array.sv"))], "Top");
        let (addr, d, we) = (sim.signal("addr"), sim.signal("d"), sim.signal("we"));
        sim.modify(|io| { io.set(addr, 5000u16); io.set(d, 0x5au8); io.set(we, 1u8); }).unwrap();
        let clk = sim.event("clk");
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 0x5au8.into());
        assert_eq!(sim.get(sim.signal("m")), 0x5au8.into());
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

    fn struct_assignment_patterns_follow_the_target_layout(sim) {
        @setup {
            let source = r#"
                module Top(input logic clk, input logic [7:0] a, output logic [11:0] named,
                           output logic [11:0] positional, output logic [11:0] filled,
                           output logic [11:0] registered);
                    typedef struct packed { logic [3:0] p; logic q; logic [6:0] r; } t_t;
                    t_t from_assign, from_comb, from_default, in_ff;
                    assign from_assign = '{p: a[3:0], q: a[7], r: a[6:0]};
                    always_comb from_comb = '{a[3:0], a[7], a[6:0]};
                    always_comb from_default = '{q: 1'b1, default: '0};
                    always_ff @(posedge clk) in_ff <= '{r: a[6:0], q: a[0], p: a[7:4]};
                    assign named = from_assign;
                    assign positional = from_comb;
                    assign filled = from_default;
                    assign registered = in_ff;
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("struct_patterns.sv"))], "Top");
        let a = sim.signal("a");
        for value in [0u8, 0xd5, 0x2a, 0xff] {
            sim.modify(|io| io.set(a, value)).unwrap();
            let low = u16::from(value & 0x7f);
            let expected = u16::from(value & 0xf) << 8 | u16::from(value >> 7) << 7 | low;
            assert_eq!(sim.get(sim.signal("named")), expected.into(), "a={value:#x}");
            assert_eq!(sim.get(sim.signal("positional")), expected.into(), "a={value:#x}");
            assert_eq!(sim.get(sim.signal("filled")), 0x080u16.into());
            sim.tick(sim.event("clk")).unwrap();
            let registered = u16::from(value >> 4) << 8 | u16::from(value & 1) << 7 | low;
            assert_eq!(sim.get(sim.signal("registered")), registered.into(), "a={value:#x}");
        }
    }

    fn function_output_and_inout_arguments_are_written_back(sim) {
        @setup {
            let source = r#"
                module Top(input logic clk, input logic [7:0] a, b,
                           output logic [7:0] sum, diff, lo, hi, twice, part, q);
                    function automatic void sum_diff(input logic [7:0] x, y,
                                                     output logic [7:0] s, d);
                        s = x + y;
                        d = x - y;
                    endfunction
                    function automatic void clamp(input logic [7:0] x, output logic [7:0] l, h);
                        if (x > 8'd10) begin l = 8'd10; h = x; end
                        else begin l = x; h = 8'd10; end
                    endfunction
                    function automatic void bump(inout logic [7:0] v);
                        v = v + 1;
                    endfunction
                    function automatic void invert(input logic [3:0] x, output logic [3:0] z);
                        z = ~x;
                    endfunction
                    function automatic void shift(input logic [7:0] x, output logic [7:0] z);
                        z = x << 1;
                    endfunction
                    always_comb begin
                        sum_diff(a, b, sum, diff);
                        clamp(a, lo, hi);
                        twice = a;
                        bump(twice);
                        bump(twice);
                        part = 8'h00;
                        invert(a[3:0], part[7:4]);
                    end
                    always_ff @(posedge clk) shift(a, q);
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("function_outputs.sv"))], "Top");
        let (a, b) = (sim.signal("a"), sim.signal("b"));
        for (x, y) in [(3u8, 4u8), (50, 7), (255, 255), (0, 1)] {
            sim.modify(|io| { io.set(a, x); io.set(b, y); }).unwrap();
            assert_eq!(sim.get(sim.signal("sum")), x.wrapping_add(y).into());
            assert_eq!(sim.get(sim.signal("diff")), x.wrapping_sub(y).into());
            assert_eq!(sim.get(sim.signal("lo")), x.min(10).into());
            assert_eq!(sim.get(sim.signal("hi")), x.max(10).into());
            assert_eq!(sim.get(sim.signal("twice")), x.wrapping_add(2).into());
            assert_eq!(sim.get(sim.signal("part")), ((!x & 0xf) << 4).into());
            sim.tick(sim.event("clk")).unwrap();
            assert_eq!(sim.get(sim.signal("q")), (x << 1).into());
        }
    }

    fn tasks_without_timing_write_their_outputs(sim) {
        @setup {
            let source = r#"
                module Top(input logic clk, input logic [7:0] a,
                           output logic [7:0] sum, mask, q);
                    task automatic stats(input logic [7:0] x, output logic [7:0] s, m);
                        logic [7:0] t;
                        t = x + 3;
                        s = t * 2;
                        m = t & 8'h0f;
                    endtask
                    task automatic load(input logic [7:0] x, output logic [7:0] z);
                        z = ~x;
                    endtask
                    always_comb stats(a, sum, mask);
                    always_ff @(posedge clk) load(a, q);
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("tasks.sv"))], "Top");
        let a = sim.signal("a");
        for value in [0u8, 1, 100, 255] {
            sim.modify(|io| io.set(a, value)).unwrap();
            let t = value.wrapping_add(3);
            assert_eq!(sim.get(sim.signal("sum")), t.wrapping_mul(2).into());
            assert_eq!(sim.get(sim.signal("mask")), (t & 0xf).into());
            sim.tick(sim.event("clk")).unwrap();
            assert_eq!(sim.get(sim.signal("q")), (!value).into());
        }
    }

    fn break_and_continue_end_unrolled_loop_iterations(sim) {
        @setup {
            let source = r#"
                module Top(input logic clk, input logic [8:0] a, output logic [7:0] first,
                           even, lead, rows, q);
                    always_comb begin
                        first = 8'hff;
                        for (int i = 0; i < 8; i++) begin
                            if (a[i]) begin first = i[7:0]; break; end
                        end
                        even = 0;
                        for (int i = 0; i < 8; i++) begin
                            if (i[0]) continue;
                            even = even + a[i];
                        end
                        lead = 0;
                        for (int i = 0; i < 8; i++) begin
                            if (a[i]) break;
                            lead = lead + 1;
                        end
                        rows = 0;
                        for (int r = 0; r < 3; r++) begin
                            for (int c = 0; c < 3; c++) begin
                                if (!a[r*3+c]) break;
                                rows = rows + 1;
                            end
                        end
                    end
                    always_ff @(posedge clk) begin
                        q <= 8'hff;
                        for (int i = 0; i < 8; i++) begin
                            if (a[i]) begin q <= i[7:0]; break; end
                        end
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("jumps.sv"))], "Top");
        let a = sim.signal("a");
        for value in [0u16, 0x1ff, 0x0a8, 0x005, 0x080, 0x1b7] {
            sim.modify(|io| io.set(a, value)).unwrap();
            let bit = |i: u32| (value >> i) & 1;
            let first = (0..8).find(|i| bit(*i) != 0).map_or(0xff, |i| i as u8);
            let even = (0..8).step_by(2).map(bit).sum::<u16>() as u8;
            let lead = (0..8).take_while(|i| bit(*i) == 0).count() as u8;
            let rows: u8 = (0..3)
                .map(|r| (0..3).take_while(|c| bit(r * 3 + c) != 0).count() as u8)
                .sum();
            assert_eq!(sim.get(sim.signal("first")), first.into(), "a={value:#x}");
            assert_eq!(sim.get(sim.signal("even")), even.into(), "a={value:#x}");
            assert_eq!(sim.get(sim.signal("lead")), lead.into(), "a={value:#x}");
            assert_eq!(sim.get(sim.signal("rows")), rows.into(), "a={value:#x}");
            sim.tick(sim.event("clk")).unwrap();
            assert_eq!(sim.get(sim.signal("q")), first.into(), "a={value:#x}");
        }
    }

    fn type_parameters_are_bound_by_instantiations(sim) {
        @setup {
            let source = r#"
                module Inc #(parameter type T = logic [7:0])(input T a, output T y);
                    assign y = a + 1;
                endmodule
                module Shift #(parameter type T = logic [7:0], parameter int N = 2)(
                    input T a, output T y);
                    assign y = a << N;
                endmodule
                module Mid #(parameter type U = logic [7:0])(input U a, output U y);
                    Inc #(.T(U)) inner(.a(a), .y(y));
                endmodule
                module Neg #(parameter type T = logic [7:0])(input T a, output logic neg);
                    assign neg = (a < 0);
                endmodule
                module Top(input logic [11:0] a, output logic [7:0] y8, output logic [3:0] y4,
                           output logic [3:0] named, output logic [3:0] forwarded,
                           output logic [11:0] shifted, output logic signed_neg, output logic unsigned_neg);
                    typedef logic [3:0] nibble_t;
                    Inc default_width(.a(a[7:0]), .y(y8));
                    Inc #(.T(logic [3:0])) literal(.a(a[3:0]), .y(y4));
                    Inc #(.T(nibble_t)) by_name(.a(a[3:0]), .y(named));
                    Mid #(.U(logic [3:0])) through(.a(a[3:0]), .y(forwarded));
                    Shift #(logic [11:0], 3) positional(a, shifted);
                    Neg #(.T(logic signed [7:0])) signed_neg_u(.a(a[7:0]), .neg(signed_neg));
                    Neg unsigned_neg_u(.a(a[7:0]), .neg(unsigned_neg));
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("type_parameters.sv"))], "Top");
        let a = sim.signal("a");
        for value in [0u16, 5, 15, 200, 255, 1000] {
            sim.modify(|io| io.set(a, value)).unwrap();
            let low = value as u8;
            let bumped = u16::from(low) + 1;
            assert_eq!(sim.get(sim.signal("y8")), low.wrapping_add(1).into(), "a={value}");
            assert_eq!(sim.get(sim.signal("y4")), (bumped & 0xf).into());
            assert_eq!(sim.get(sim.signal("named")), (bumped & 0xf).into());
            assert_eq!(sim.get(sim.signal("forwarded")), (bumped & 0xf).into());
            assert_eq!(sim.get(sim.signal("shifted")), ((value << 3) & 0xfff).into());
            assert_eq!(sim.get(sim.signal("signed_neg")), u8::from(low >= 128).into());
            assert_eq!(sim.get(sim.signal("unsigned_neg")), 0u8.into());
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

#[test]
fn task_timing_control_is_rejected() {
    let source = "module Top(input logic [7:0] a, output logic [7:0] y); \
        task automatic t(input logic [7:0] x, output logic [7:0] z); #5 z = x; endtask \
        always_comb t(a, y); endmodule";
    let error = Simulator::from_sv_sources(vec![(source, Path::new("task_delay.sv"))], "Top")
        .build_cranelift()
        .expect_err("a task with a delay must be rejected")
        .to_string();
    assert!(error.contains("Unsupported"), "{error}");
}
