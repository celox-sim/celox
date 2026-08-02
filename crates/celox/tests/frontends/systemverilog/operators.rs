use super::*;

sv_backends! {
    fn simulates_systemverilog_top_comb_assign(sim) {
        @setup {
    let sv = r#"
        module Top(input logic [7:0] a, input logic [7:0] b, output logic [7:0] y);
            assign y = a ^ b;
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("top.sv"))], "Top");

    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");

    sim.modify(|io| {
        io.set(a, 0x55u8);
        io.set(b, 0x0fu8);
    })
    .unwrap();

    assert_eq!(sim.get(y), 0x5au8.into());
    }

    fn simulates_systemverilog_binary_operators(sim) {
        @setup {
    let sv = r#"
        module Top(
            input logic [7:0] a,
            input logic [7:0] b,
            input logic [2:0] sh,
            output logic [7:0] add,
            output logic [7:0] sub,
            output logic [7:0] mul,
            output logic [7:0] div,
            output logic [7:0] rem,
            output logic [7:0] shl,
            output logic [7:0] shr,
            output logic [7:0] band,
            output logic [7:0] bor,
            output logic [7:0] bxor,
            output logic land,
            output logic lor,
            output logic eq,
            output logic ne
        );
            assign add = a + b;
            assign sub = a - b;
            assign mul = a * b;
            assign div = a / b;
            assign rem = a % b;
            assign shl = a << sh;
            assign shr = a >> sh;
            assign band = a & b;
            assign bor = a | b;
            assign bxor = a ^ b;
            assign land = a && b;
            assign lor = a || b;
            assign eq = a == b;
            assign ne = a != b;
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("operators.sv"))], "Top");

    let a = sim.signal("a");
    let b = sim.signal("b");
    let sh = sim.signal("sh");
    sim.modify(|io| {
        io.set(a, 13u8);
        io.set(b, 5u8);
        io.set(sh, 2u8);
    })
    .unwrap();

    assert_eq!(sim.get(sim.signal("add")), 18u8.into());
    assert_eq!(sim.get(sim.signal("sub")), 8u8.into());
    assert_eq!(sim.get(sim.signal("mul")), 65u8.into());
    assert_eq!(sim.get(sim.signal("div")), 2u8.into());
    assert_eq!(sim.get(sim.signal("rem")), 3u8.into());
    assert_eq!(sim.get(sim.signal("shl")), 52u8.into());
    assert_eq!(sim.get(sim.signal("shr")), 3u8.into());
    assert_eq!(sim.get(sim.signal("band")), 5u8.into());
    assert_eq!(sim.get(sim.signal("bor")), 13u8.into());
    assert_eq!(sim.get(sim.signal("bxor")), 8u8.into());
    assert_eq!(sim.get(sim.signal("land")), 1u8.into());
    assert_eq!(sim.get(sim.signal("lor")), 1u8.into());
    assert_eq!(sim.get(sim.signal("eq")), 0u8.into());
    assert_eq!(sim.get(sim.signal("ne")), 1u8.into());
    }

    fn simulates_systemverilog_unary_operators(sim) {
        @setup {
    let sv = r#"
        module Top(
            input logic [7:0] a,
            output logic [7:0] plus,
            output logic [7:0] neg,
            output logic [7:0] bit_not,
            output logic logic_not
        );
            assign plus = +a;
            assign neg = -a;
            assign bit_not = ~a;
            assign logic_not = !a;
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("unary.sv"))], "Top");

    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 5u8)).unwrap();
    assert_eq!(sim.get(sim.signal("plus")), 5u8.into());
    assert_eq!(sim.get(sim.signal("neg")), 251u8.into());
    assert_eq!(sim.get(sim.signal("bit_not")), 250u8.into());
    assert_eq!(sim.get(sim.signal("logic_not")), 0u8.into());

    sim.modify(|io| io.set(a, 0u8)).unwrap();
    assert_eq!(sim.get(sim.signal("logic_not")), 1u8.into());
    }

    fn simulates_systemverilog_reduction_unary_operators(sim) {
        @setup {
    let sv = r#"
        module Top(
            input logic [7:0] a,
            output logic red_and,
            output logic red_or,
            output logic red_xor
        );
            assign red_and = &a;
            assign red_or  = |a;
            assign red_xor = ^a;
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("reduction_unary.sv"))], "Top");

    let a = sim.signal("a");
    let red_and = sim.signal("red_and");
    let red_or = sim.signal("red_or");
    let red_xor = sim.signal("red_xor");

    sim.modify(|io| io.set(a, 0xffu8)).unwrap();
    assert_eq!(sim.get(red_and), 1u8.into());
    assert_eq!(sim.get(red_or), 1u8.into());
    assert_eq!(sim.get(red_xor), 0u8.into());

    sim.modify(|io| io.set(a, 0x10u8)).unwrap();
    assert_eq!(sim.get(red_and), 0u8.into());
    assert_eq!(sim.get(red_or), 1u8.into());
    assert_eq!(sim.get(red_xor), 1u8.into());
    }

    fn simulates_systemverilog_select_and_concat(sim) {
        @setup {
    let sv = r#"
        module Top(
            input logic [7:0] a,
            input logic [7:0] b,
            output logic [3:0] hi,
            output logic [7:0] cat
        );
            assign hi = a[7:4];
            assign cat = {a[3:0], b[7:4]};
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("select_concat.sv"))], "Top");

    let a = sim.signal("a");
    let b = sim.signal("b");
    let hi = sim.signal("hi");
    let cat = sim.signal("cat");

    sim.modify(|io| {
        io.set(a, 0xabu8);
        io.set(b, 0xcdu8);
    })
    .unwrap();

    assert_eq!(sim.get(hi), 0xau8.into());
    assert_eq!(sim.get(cat), 0xbcu8.into());
    }

    fn simulates_systemverilog_selected_lvalue_assignments(sim) {
        @setup {
    let sv = r#"
        module Top(
            input logic [7:0] a,
            input logic [7:0] b,
            output logic [7:0] y,
            output logic [7:0] z
        );
            always_comb begin
                y[3:0] = a[3:0];
                y[7:4] = b[7:4];
                z[2] = a[7];
                z[0] = b[0];
            end
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("selected_lvalue.sv"))], "Top");

    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");
    let z = sim.signal("z");

    sim.modify(|io| {
        io.set(a, 0x8du8);
        io.set(b, 0x51u8);
    })
    .unwrap();

    assert_eq!(sim.get(y), 0x5du8.into());
    assert_eq!(sim.get(z), 0x05u8.into());
    }

    fn simulates_systemverilog_selected_lvalue_ff_intermediate(sim) {
        @setup {
    let sv = r#"
        module Top(input logic clk, input logic en, input logic [7:0] d, output logic [7:0] q);
            logic [7:0] next;
            always_comb next[7] = q[0];
            always_comb next[0] = d[0];
            always_ff @(posedge clk) begin
                if (en) begin
                    q <= next;
                end
            end
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("selected_lvalue_ff_intermediate.sv"))], "Top");

    let clk = sim.event("clk");
    let en = sim.signal("en");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(en, 1u8);
        io.set(d, 1u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u8.into());

    sim.modify(|io| io.set(d, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x80u8.into());
    }

    fn simulates_systemverilog_hierarchical_selected_lvalue_ff_intermediate(sim) {
        @setup {
    let sv = r#"
        module Child(input logic clk, input logic en, input logic [7:0] d, output logic [7:0] q);
            logic [7:0] next;
            always_comb next[7] = q[0];
            always_comb next[0] = d[0];
            always_ff @(posedge clk) begin
                if (en) begin
                    q <= next;
                end
            end
        endmodule

        module Top(input logic clk, input logic en, input logic [7:0] d, output logic [7:0] q);
            Child u(.clk(clk), .en(en), .d(d), .q(q));
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("hier_selected_lvalue_ff_intermediate.sv"))], "Top");

    let clk = sim.event("clk");
    let en = sim.signal("en");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(en, 1u8);
        io.set(d, 1u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u8.into());

    sim.modify(|io| io.set(d, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x80u8.into());
    }

    fn simulates_systemverilog_hierarchical_parameter_selected_lvalue_ff_intermediate(sim) {
        @setup {
    let sv = r#"
        module Child #(parameter int unsigned W = 8) (
            input logic clk,
            input logic en,
            input logic [W-1:0] d,
            output logic [W-1:0] q
        );
            logic [W-1:0] next;
            always_comb next[W - 1] = q[0];
            always_comb next[0] = d[0];
            always_ff @(posedge clk) begin
                if (en) begin
                    q <= next;
                end
            end
        endmodule

        module Top(input logic clk, input logic en, input logic [31:0] d, output logic [31:0] q);
            Child #(.W(32)) u(.clk(clk), .en(en), .d(d), .q(q));
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("hier_param_selected_lvalue_ff_intermediate.sv"))], "Top");

    let clk = sim.event("clk");
    let en = sim.signal("en");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(en, 1u8);
        io.set(d, 1u32);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u32.into());

    sim.modify(|io| io.set(d, 0u32)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x8000_0000u32.into());
    }

    fn simulates_systemverilog_contiguous_selected_lvalue_ff_intermediate(sim) {
        @setup {
    let sv = r#"
        module Child #(parameter int unsigned W = 32) (
            input logic clk,
            input logic en,
            input logic [W-1:0] d,
            output logic [W-1:0] q
        );
            logic [W-1:0] next;
            always_comb next[W - 1] = q[0];
            for (genvar i = 0; i < W - 1; i++) begin : gen_bits
                always_comb next[i] = d[i];
            end
            always_ff @(posedge clk) begin
                if (en) begin
                    q <= next;
                end
            end
        endmodule

        module Top(input logic clk, input logic en, input logic [31:0] d, output logic [31:0] q);
            Child u(.clk(clk), .en(en), .d(d), .q(q));
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("contiguous_selected_lvalue_ff_intermediate.sv"))], "Top");

    let clk = sim.event("clk");
    let en = sim.signal("en");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(en, 1u8);
        io.set(d, 1u32);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u32.into());

    sim.modify(|io| io.set(d, 0u32)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x8000_0000u32.into());
    }

    fn simulates_systemverilog_genvar_loop_comb_assignments(sim) {
        @setup {
    let sv = r#"
        module Top #(parameter W = 4) (
            input logic [W-1:0] a,
            output logic [W-1:0] y
        );
            for (genvar i = 0; i < W; i++) begin : gen_bits
                always_comb y[i] = ~a[i];
            end
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("genvar_loop.sv"))], "Top");

    let a = sim.signal("a");
    let y = sim.signal("y");

    sim.modify(|io| io.set(a, 0b1100u8)).unwrap();

    assert_eq!(sim.get(y), 0b0011u8.into());
    }

    fn simulates_systemverilog_genvar_loop_with_localparam_and_if(sim) {
        @setup {
    let sv = r#"
        module Top(
            input logic [3:0] a,
            output logic [3:0] y
        );
            for (genvar k = 1; k < 5; k++) begin : gen_bits
                localparam IDX = k - 1;
                if (!$onehot(k)) begin : gen_data
                    localparam DATA_IDX = IDX;
                    always_comb y[IDX] = a[DATA_IDX];
                end else begin : gen_data
                    always_comb y[IDX] = 1'b0;
                end
            end
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("genvar_localparam_if.sv"))], "Top");

    let a = sim.signal("a");
    let y = sim.signal("y");

    sim.modify(|io| io.set(a, 0b1111u8)).unwrap();

    assert_eq!(sim.get(y), 0b0100u8.into());
    }

    fn simulates_systemverilog_packed_multidimensional_selects(sim) {
        @setup {
    let sv = r#"
        module Top(
            input logic a,
            output logic [3:0] y,
            output logic z
        );
            logic [1:0][3:0] m;
            always_comb begin
                m[1][2] = a;
                y = m[1];
                z = m[1][2];
            end
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("packed_multidim.sv"))], "Top");

    let a = sim.signal("a");
    let y = sim.signal("y");
    let z = sim.signal("z");

    sim.modify(|io| io.set(a, 1u8)).unwrap();

    assert_eq!(sim.get(y), 0b0100u8.into());
    assert_eq!(sim.get(z), 1u8.into());
    }

    fn simulates_systemverilog_simple_always_ff(sim) {
        @setup {
    let sv = r#"
        module Top(
            input logic clk,
            input logic rst,
            input logic d,
            output logic q
        );
            always_ff @(posedge clk, negedge rst) begin
                if (!rst) begin
                    q <= 1'b0;
                end else begin
                    q <= d;
                end
            end
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("simple_ff.sv"))], "Top");

    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(d, 1u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into());

    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u8.into());
    }

    fn simulates_systemverilog_always_ff_if_else_chain(sim) {
        @setup {
    let sv = r#"
        module Top(
            input logic clk,
            input logic rst,
            input logic clear,
            input logic [31:0] d,
            output logic [31:0] q
        );
            always_ff @(posedge clk, negedge rst) begin
                if (!rst) begin
                    q <= 32'h0;
                end else if (clear) begin
                    q <= 32'h0;
                end else begin
                    q <= d;
                end
            end
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("ff_if_else_chain.sv"))], "Top");

    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let clear = sim.signal("clear");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(clear, 0u8);
        io.set(d, 0xffu32);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into());

    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0xffu32.into());

    sim.modify(|io| {
        io.set(clear, 1u8);
        io.set(d, 0xa5u32);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into());
    }

    fn simulates_systemverilog_always_ff_case(sim) {
        @setup {
    let sv = r#"
        module Top(
            input logic clk,
            input logic rst,
            input logic [1:0] mode,
            input logic [7:0] d0,
            input logic [7:0] d1,
            input logic [7:0] d2,
            output logic [7:0] q,
            output logic [7:0] q_hold
        );
            always_ff @(posedge clk, negedge rst) begin
                if (!rst) begin
                    q <= 8'h00;
                    q_hold <= 8'h00;
                end else begin
                    case (mode)
                        2'b00: begin
                            q <= d0;
                            q_hold <= d0;
                        end
                        2'b01, 2'b10: begin
                            q <= d1;
                            q_hold <= d1;
                        end
                        default: q <= d2;
                    endcase
                end
            end
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("ff_case.sv"))], "Top")
            .four_state(true);

    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let mode = sim.signal("mode");
    let d0 = sim.signal("d0");
    let d1 = sim.signal("d1");
    let d2 = sim.signal("d2");
    let q = sim.signal("q");
    let q_hold = sim.signal("q_hold");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(mode, 0u8);
        io.set(d0, 0x11u8);
        io.set(d1, 0x22u8);
        io.set(d2, 0x33u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into());
    assert_eq!(sim.get(q_hold), 0u8.into());

    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x11u8.into());
    assert_eq!(sim.get(q_hold), 0x11u8.into());

    sim.modify(|io| io.set(mode, 2u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x22u8.into());
    assert_eq!(sim.get(q_hold), 0x22u8.into());

    sim.modify(|io| io.set(mode, 3u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x33u8.into());
    assert_eq!(sim.get(q_hold), 0x22u8.into());

    sim.modify(|io| {
        io.set_four_state(mode, BigUint::from(0u8), BigUint::from(0b11u8));
        io.set(d2, 0x44u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x44u8.into());
    assert_eq!(sim.get(q_hold), 0x22u8.into());
    }

    fn simulates_systemverilog_repeat_concat(sim) {
        @setup {
    let sv = r#"
        module Top(input logic [1:0] a, output logic [7:0] y);
            assign y = {4{a}};
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("repeat_concat.sv"))], "Top");

    let a = sim.signal("a");
    let y = sim.signal("y");

    sim.modify(|io| io.set(a, 0b10u8)).unwrap();

    assert_eq!(sim.get(y), 0xaau8.into());
    }
}
