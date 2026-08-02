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

    fn simulates_systemverilog_case_equality_operators(sim) {
        @setup {
    let sv = r#"
        module Top(
            input logic [1:0] a,
            input logic [1:0] b,
            input logic [127:0] wide_a,
            input logic [127:0] wide_b,
            output logic eq_case,
            output logic ne_case,
            output logic eq_case_unsized,
            output logic eq_wildcard_unsized,
            output logic eq_case_fill,
            output logic ne_case_fill,
            output logic eq_wildcard_fill,
            output logic ne_wildcard_fill,
            output logic wide_eq_case,
            output logic wide_ne_case
        );
            assign eq_case = a === b;
            assign ne_case = a !== b;
            assign eq_case_unsized = a === 0;
            assign eq_wildcard_unsized = a ==? 0;
            assign eq_case_fill = a === '1;
            assign ne_case_fill = a !== '1;
            assign eq_wildcard_fill = a ==? '1;
            assign ne_wildcard_fill = a !=? '1;
            assign wide_eq_case = wide_a === wide_b;
            assign wide_ne_case = wide_a !== wide_b;
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("case_equality.sv"))], "Top")
            .four_state(true);

    let a = sim.signal("a");
    let b = sim.signal("b");
    let wide_a = sim.signal("wide_a");
    let wide_b = sim.signal("wide_b");
    let unknown = BigUint::from(1u8) << 100usize;
    let known = BigUint::from(5u8);

    sim.modify(|io| {
        io.set_four_state(a, BigUint::from(0b11u8), BigUint::from(0b01u8));
        io.set_four_state(b, BigUint::from(0b11u8), BigUint::from(0b01u8));
        io.set_four_state(wide_a, &known | &unknown, unknown.clone());
        io.set_four_state(wide_b, &known | &unknown, unknown.clone());
    }).unwrap();
    assert_eq!(
        sim.get_four_state(sim.signal("eq_case")),
        (BigUint::from(1u8), BigUint::from(0u8))
    );
    assert_eq!(
        sim.get_four_state(sim.signal("ne_case")),
        (BigUint::from(0u8), BigUint::from(0u8))
    );
    assert_eq!(sim.get(sim.signal("eq_case_unsized")), 0u8.into());
    assert_eq!(sim.get(sim.signal("eq_wildcard_unsized")), 0u8.into());
    assert_eq!(
        sim.get_four_state(sim.signal("wide_eq_case")),
        (BigUint::from(1u8), BigUint::from(0u8))
    );
    assert_eq!(
        sim.get_four_state(sim.signal("wide_ne_case")),
        (BigUint::from(0u8), BigUint::from(0u8))
    );

    sim.modify(|io| {
        io.set_four_state(b, BigUint::from(0b10u8), BigUint::from(0b01u8));
        io.set_four_state(wide_b, known.clone(), unknown.clone());
    }).unwrap();
    assert_eq!(
        sim.get_four_state(sim.signal("eq_case")),
        (BigUint::from(0u8), BigUint::from(0u8))
    );
    assert_eq!(
        sim.get_four_state(sim.signal("ne_case")),
        (BigUint::from(1u8), BigUint::from(0u8))
    );
    assert_eq!(
        sim.get_four_state(sim.signal("wide_eq_case")),
        (BigUint::from(0u8), BigUint::from(0u8))
    );
    assert_eq!(
        sim.get_four_state(sim.signal("wide_ne_case")),
        (BigUint::from(1u8), BigUint::from(0u8))
    );

    sim.modify(|io| io.set(a, 0u8)).unwrap();
    assert_eq!(sim.get(sim.signal("eq_case_unsized")), 1u8.into());
    assert_eq!(sim.get(sim.signal("eq_wildcard_unsized")), 1u8.into());
    assert_eq!(sim.get(sim.signal("eq_case_fill")), 0u8.into());
    assert_eq!(sim.get(sim.signal("ne_case_fill")), 1u8.into());
    assert_eq!(sim.get(sim.signal("eq_wildcard_fill")), 0u8.into());
    assert_eq!(sim.get(sim.signal("ne_wildcard_fill")), 1u8.into());

    sim.modify(|io| io.set(a, 0b11u8)).unwrap();
    assert_eq!(sim.get(sim.signal("eq_case_fill")), 1u8.into());
    assert_eq!(sim.get(sim.signal("ne_case_fill")), 0u8.into());
    assert_eq!(sim.get(sim.signal("eq_wildcard_fill")), 1u8.into());
    assert_eq!(sim.get(sim.signal("ne_wildcard_fill")), 0u8.into());
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
            input logic [1:0] dynamic_label,
            input logic signed [1:0] signed_mode,
            input logic [7:0] d0,
            input logic [7:0] d1,
            input logic [7:0] d2,
            output logic [7:0] q,
            output logic [7:0] q_hold,
            output logic [7:0] q_case_one,
            output logic [7:0] q_before_case,
            output logic [7:0] q_after_case,
            output logic [7:0] q_fill,
            output logic [7:0] q_parameter,
            output logic [7:0] q_rhs_width,
            output logic [7:0] q_overlap,
            output logic [7:0] q_exact,
            output logic [7:0] q_dynamic
        );
            localparam logic [1:0] P = 2'b11;

            always_ff @(posedge clk, negedge rst) begin
                if (!rst) begin
                    q <= 8'h00;
                    q_hold <= 8'h00;
                    q_case_one <= 8'h00;
                    q_before_case <= 8'h00;
                    q_after_case <= 8'h00;
                    q_fill <= 8'h00;
                    q_parameter <= 8'h00;
                    q_rhs_width <= 8'h00;
                    q_overlap <= 8'h00;
                    q_exact <= 8'h00;
                    q_dynamic <= 8'h00;
                end else begin
                    case (mode)
                        0: begin
                            q <= d0;
                            q_hold <= d0;
                        end
                        1, 2: begin
                            q <= d1;
                            q_hold <= d1;
                        end
                        default: q <= d2;
                    endcase

                    case (1)
                        2'b10: q_case_one <= d0;
                        default: q_case_one <= d2;
                    endcase

                    q_before_case <= 8'h55;
                    case (mode)
                        0: q_before_case <= d0;
                        default: ;
                    endcase

                    case (mode)
                        0: q_after_case <= d0;
                        default: q_after_case <= d1;
                    endcase
                    q_after_case <= d2;

                    case (d0)
                        '1: q_fill <= 8'ha5;
                        default: q_fill <= 8'h00;
                    endcase

                    case (signed_mode)
                        P: q_parameter <= 8'h66;
                        default: q_parameter <= 8'h00;
                    endcase

                    case (mode)
                        0: q_rhs_width <= 0;
                        default: q_rhs_width <= 1'b1;
                    endcase

                    case (mode)
                        2'b1x: q_exact <= 8'ha1;
                        2'b1z: q_exact <= 8'hb2;
                        default: q_exact <= 8'hc3;
                    endcase

                    case (mode)
                        dynamic_label: q_dynamic <= 8'hd4;
                        default: q_dynamic <= 8'he5;
                    endcase

                    q_overlap[0] <= 1'b1;
                    case (mode)
                        1: q_overlap <= 0;
                        default: ;
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
    let dynamic_label = sim.signal("dynamic_label");
    let signed_mode = sim.signal("signed_mode");
    let d0 = sim.signal("d0");
    let d1 = sim.signal("d1");
    let d2 = sim.signal("d2");
    let q = sim.signal("q");
    let q_hold = sim.signal("q_hold");
    let q_case_one = sim.signal("q_case_one");
    let q_before_case = sim.signal("q_before_case");
    let q_after_case = sim.signal("q_after_case");
    let q_fill = sim.signal("q_fill");
    let q_parameter = sim.signal("q_parameter");
    let q_rhs_width = sim.signal("q_rhs_width");
    let q_overlap = sim.signal("q_overlap");
    let q_exact = sim.signal("q_exact");
    let q_dynamic = sim.signal("q_dynamic");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(mode, 0u8);
        io.set(dynamic_label, 0u8);
        io.set(signed_mode, 3u8);
        io.set(d0, 0x11u8);
        io.set(d1, 0x22u8);
        io.set(d2, 0x33u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into());
    assert_eq!(sim.get(q_hold), 0u8.into());
    assert_eq!(sim.get(q_case_one), 0u8.into());
    assert_eq!(sim.get(q_before_case), 0u8.into());
    assert_eq!(sim.get(q_after_case), 0u8.into());
    assert_eq!(sim.get(q_fill), 0u8.into());
    assert_eq!(sim.get(q_parameter), 0u8.into());
    assert_eq!(sim.get(q_rhs_width), 0u8.into());
    assert_eq!(sim.get(q_overlap), 0u8.into());
    assert_eq!(sim.get(q_exact), 0u8.into());
    assert_eq!(sim.get(q_dynamic), 0u8.into());

    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x11u8.into());
    assert_eq!(sim.get(q_hold), 0x11u8.into());
    assert_eq!(sim.get(q_case_one), 0x33u8.into());
    assert_eq!(sim.get(q_before_case), 0x11u8.into());
    assert_eq!(sim.get(q_after_case), 0x33u8.into());
    assert_eq!(sim.get(q_fill), 0u8.into());
    assert_eq!(sim.get(q_parameter), 0x66u8.into());
    assert_eq!(sim.get(q_rhs_width), 0u8.into());
    assert_eq!(sim.get(q_overlap), 1u8.into());

    sim.modify(|io| io.set(mode, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x22u8.into());
    assert_eq!(sim.get(q_overlap), 0u8.into());
    assert_eq!(sim.get(q_rhs_width), 1u8.into());

    sim.modify(|io| io.set(mode, 2u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x22u8.into());
    assert_eq!(sim.get(q_hold), 0x22u8.into());
    assert_eq!(sim.get(q_case_one), 0x33u8.into());
    assert_eq!(sim.get(q_before_case), 0x55u8.into());
    assert_eq!(sim.get(q_after_case), 0x33u8.into());
    assert_eq!(sim.get(q_fill), 0u8.into());
    assert_eq!(sim.get(q_parameter), 0x66u8.into());
    assert_eq!(sim.get(q_rhs_width), 1u8.into());
    assert_eq!(sim.get(q_overlap), 1u8.into());

    sim.modify(|io| io.set(mode, 3u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x33u8.into());
    assert_eq!(sim.get(q_hold), 0x22u8.into());
    assert_eq!(sim.get(q_case_one), 0x33u8.into());
    assert_eq!(sim.get(q_before_case), 0x55u8.into());
    assert_eq!(sim.get(q_after_case), 0x33u8.into());
    assert_eq!(sim.get(q_fill), 0u8.into());
    assert_eq!(sim.get(q_parameter), 0x66u8.into());
    assert_eq!(sim.get(q_rhs_width), 1u8.into());
    assert_eq!(sim.get(q_overlap), 1u8.into());

    sim.modify(|io| {
        io.set_four_state(mode, BigUint::from(0u8), BigUint::from(0b11u8));
        io.set_four_state(
            dynamic_label,
            BigUint::from(0u8),
            BigUint::from(0b11u8),
        );
        io.set(d2, 0x44u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x44u8.into());
    assert_eq!(sim.get(q_hold), 0x22u8.into());
    assert_eq!(sim.get(q_case_one), 0x44u8.into());
    assert_eq!(sim.get(q_before_case), 0x55u8.into());
    assert_eq!(sim.get(q_after_case), 0x44u8.into());
    assert_eq!(sim.get(q_fill), 0u8.into());
    assert_eq!(sim.get(q_parameter), 0x66u8.into());
    assert_eq!(sim.get(q_rhs_width), 1u8.into());
    assert_eq!(sim.get(q_overlap), 1u8.into());
    assert_eq!(sim.get(q_exact), 0xc3u8.into());
    assert_eq!(sim.get(q_dynamic), 0xd4u8.into());

    sim.modify(|io| io.set(d0, 0xffu8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q_fill), 0xa5u8.into());

    // 2'b1x has payload 0b11 and mask 0b01; 2'b1z has payload 0b10
    // with the same mask. Exact case equality must distinguish them.
    sim.modify(|io| {
        io.set_four_state(mode, BigUint::from(0b11u8), BigUint::from(0b01u8));
        io.set_four_state(
            dynamic_label,
            BigUint::from(0b11u8),
            BigUint::from(0b01u8),
        );
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q_exact), 0xa1u8.into());
    assert_eq!(sim.get(q_dynamic), 0xd4u8.into());

    sim.modify(|io| {
        io.set_four_state(
            dynamic_label,
            BigUint::from(0b10u8),
            BigUint::from(0b01u8),
        );
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q_exact), 0xa1u8.into());
    assert_eq!(sim.get(q_dynamic), 0xe5u8.into());

    sim.modify(|io| {
        io.set_four_state(mode, BigUint::from(0b10u8), BigUint::from(0b01u8));
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q_exact), 0xb2u8.into());
    assert_eq!(sim.get(q_dynamic), 0xd4u8.into());
    }

    fn simulates_systemverilog_always_ff_case_context_values(sim) {
        @setup {
    let sv = r#"
        module Top(
            input logic clk,
            input logic mode,
            input logic signed [1:0] signed_mode,
            input logic [255:0] wide_mode,
            input logic [255:0] wide_expr_mode,
            output logic [7:0] q_negative,
            output logic [7:0] q_scalar_signed,
            output logic [7:0] q_wide_parameter,
            output logic [7:0] q_wide_fill,
            output logic [7:0] q_wide_expression,
            output logic [63:0] q_fill,
            output logic [63:0] q_shift
        );
            localparam NEGATIVE = -1;
            localparam logic signed SCALAR_SIGNED = 1'b1;
            localparam logic [255:0] WIDE_NEGATIVE = -1;
            localparam logic [255:0] WIDE_FILL = '1;
            localparam logic [255:0] WIDE_SHIFT = 256'b1 << 200;

            always_ff @(posedge clk) begin
                case (signed_mode)
                    NEGATIVE: q_negative <= NEGATIVE;
                    default: q_negative <= 0;
                endcase
                case (signed_mode)
                    SCALAR_SIGNED: q_scalar_signed <= 8'ha5;
                    default: q_scalar_signed <= 0;
                endcase
                case (wide_mode)
                    WIDE_NEGATIVE: q_wide_parameter <= 8'hb6;
                    default: q_wide_parameter <= 0;
                endcase
                case (wide_mode)
                    WIDE_FILL: q_wide_fill <= 8'hc7;
                    default: q_wide_fill <= 0;
                endcase
                case (wide_expr_mode)
                    WIDE_SHIFT: q_wide_expression <= 8'hd8;
                    default: q_wide_expression <= 0;
                endcase
                case (mode)
                    0: q_fill <= '1;
                    default: q_fill <= '0;
                endcase
                case (mode)
                    0: q_shift <= 1 << 40;
                    default: q_shift <= 0;
                endcase
            end
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(
            vec![(sv, Path::new("ff_case_context_values.sv"))],
            "Top",
        );

    let clk = sim.event("clk");
    let mode = sim.signal("mode");
    let signed_mode = sim.signal("signed_mode");
    let wide_mode = sim.signal("wide_mode");
    let wide_expr_mode = sim.signal("wide_expr_mode");
    let wide_negative = (BigUint::from(1u8) << 256) - BigUint::from(1u8);
    let wide_shift = BigUint::from(1u8) << 200;

    sim.modify(|io| {
        io.set(mode, 0u8);
        io.set(signed_mode, 3u8);
        io.set_wide(wide_mode, wide_negative);
        io.set_wide(wide_expr_mode, wide_shift);
    }).unwrap();
    sim.tick(clk).unwrap();

    assert_eq!(sim.get(sim.signal("q_negative")), 0xffu8.into());
    assert_eq!(sim.get(sim.signal("q_scalar_signed")), 0xa5u8.into());
    assert_eq!(sim.get(sim.signal("q_wide_parameter")), 0xb6u8.into());
    assert_eq!(sim.get(sim.signal("q_wide_fill")), 0xc7u8.into());
    assert_eq!(sim.get(sim.signal("q_wide_expression")), 0xd8u8.into());
    assert_eq!(sim.get(sim.signal("q_fill")), u64::MAX.into());
    assert_eq!(sim.get(sim.signal("q_shift")), (1u64 << 40).into());
    }

    fn simulates_systemverilog_always_ff_case_calls_and_xz_parameters(sim) {
        @setup {
    let sv = r#"
        module Top(
            input logic clk,
            input logic [1:0] mode,
            input logic signed [1:0] signed_mode,
            input logic [7:0] d,
            output logic [7:0] q_unrelated,
            output logic [7:0] q_function,
            output logic [7:0] q_parameter_xz,
            output logic [7:0] q_parameter_expr,
            output logic [7:0] q_function_width,
            output logic [7:0] q_function_signed,
            output logic [7:0] q_function_param,
            output logic [7:0] q_parameter_x_fill,
            output logic [7:0] q_parameter_z_fill,
            output logic [7:0] q_function_integer,
            output logic [7:0] q_function_typedef,
            output logic [7:0] q_function_nonansi,
            output logic q_const_case_equality,
            output logic q_const_case_fill,
            output logic q_const_wildcard_equality
        );
            typedef logic [1:0] word_t;

            localparam logic [1:0] X_LABEL = 2'b1x;
            localparam EXPR_LABEL = 2'b11 + 2'b00;
            localparam logic [3:0] X_FILL = 'x;
            localparam logic [3:0] Z_FILL = 'z;
            localparam MATCH_X = 1'bx === 1'bx;
            localparam MATCH_FILL = 8'hff === '1;
            localparam MATCH_WILDCARD = 2'b10 ==? 2'b1x;

            function automatic logic [1:0] decode(input logic [1:0] value);
                return value;
            endfunction

            function automatic logic [7:0] passthrough(input logic [7:0] value);
                return value;
            endfunction

            function automatic logic [1:0] truncate(input logic value);
                return 4;
            endfunction

            function automatic logic signed decode_signed(input logic value);
                return 1'b1;
            endfunction

            function automatic logic is_zero(input logic [1:0] value);
                return value === 0;
            endfunction

            function automatic integer decode_integer(input logic ignored);
                return 2;
            endfunction

            function automatic word_t decode_typedef(input logic ignored);
                return 4;
            endfunction

            function automatic logic decode_nonansi;
                input logic [1:0] value;
                return value === 0;
            endfunction

            always_ff @(posedge clk) begin
                q_unrelated <= d;
                case (decode(mode))
                    2'b01: q_function <= passthrough(d);
                    default: q_function <= 0;
                endcase
                case (mode)
                    X_LABEL: q_parameter_xz <= 8'ha5;
                    default: q_parameter_xz <= 0;
                endcase
                case (signed_mode)
                    EXPR_LABEL: q_parameter_expr <= 8'ha6;
                    default: q_parameter_expr <= 0;
                endcase
                case (truncate(mode[0]))
                    2'b00: q_function_width <= 8'hb6;
                    default: q_function_width <= 0;
                endcase
                case (signed_mode)
                    decode_signed(mode[0]): q_function_signed <= 8'hc7;
                    default: q_function_signed <= 0;
                endcase
                case (is_zero(4'b0100))
                    1'b1: q_function_param <= 8'hd8;
                    default: q_function_param <= 0;
                endcase
                q_parameter_x_fill <= X_FILL;
                q_parameter_z_fill <= Z_FILL;
                case (decode_integer(mode[0]))
                    2: q_function_integer <= 8'he9;
                    default: q_function_integer <= 0;
                endcase
                case (decode_typedef(mode[0]))
                    2'b00: q_function_typedef <= 8'hfa;
                    default: q_function_typedef <= 0;
                endcase
                case (decode_nonansi(4'b0100))
                    1'b1: q_function_nonansi <= 8'hab;
                    default: q_function_nonansi <= 0;
                endcase
                q_const_case_equality <= MATCH_X;
                q_const_case_fill <= MATCH_FILL;
                q_const_wildcard_equality <= MATCH_WILDCARD;
            end
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(
            vec![(sv, Path::new("ff_case_calls_and_xz_parameters.sv"))],
            "Top",
        ).four_state(true);

    let clk = sim.event("clk");
    let mode = sim.signal("mode");
    let signed_mode = sim.signal("signed_mode");
    let d = sim.signal("d");

    sim.modify(|io| {
        io.set(mode, 1u8);
        io.set(signed_mode, 3u8);
        io.set(d, 0x3cu8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(sim.signal("q_unrelated")), 0x3cu8.into());
    assert_eq!(sim.get(sim.signal("q_function")), 0x3cu8.into());
    assert_eq!(sim.get(sim.signal("q_parameter_xz")), 0u8.into());
    assert_eq!(sim.get(sim.signal("q_parameter_expr")), 0xa6u8.into());
    assert_eq!(sim.get(sim.signal("q_function_width")), 0xb6u8.into());
    assert_eq!(sim.get(sim.signal("q_function_signed")), 0xc7u8.into());
    assert_eq!(sim.get(sim.signal("q_function_param")), 0xd8u8.into());
    assert_eq!(
        sim.get_four_state(sim.signal("q_parameter_x_fill")),
        (BigUint::from(0x0fu8), BigUint::from(0x0fu8)),
    );
    assert_eq!(
        sim.get_four_state(sim.signal("q_parameter_z_fill")),
        (BigUint::from(0u8), BigUint::from(0x0fu8)),
    );
    assert_eq!(sim.get(sim.signal("q_function_integer")), 0xe9u8.into());
    assert_eq!(sim.get(sim.signal("q_function_typedef")), 0xfau8.into());
    assert_eq!(sim.get(sim.signal("q_function_nonansi")), 0xabu8.into());
    assert_eq!(sim.get(sim.signal("q_const_case_equality")), 1u8.into());
    assert_eq!(sim.get(sim.signal("q_const_case_fill")), 1u8.into());
    assert_eq!(
        sim.get(sim.signal("q_const_wildcard_equality")),
        1u8.into(),
    );

    sim.modify(|io| {
        io.set_four_state(mode, BigUint::from(0b11u8), BigUint::from(0b01u8));
        io.set(d, 0x5au8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(sim.signal("q_unrelated")), 0x5au8.into());
    assert_eq!(sim.get(sim.signal("q_function")), 0u8.into());
    assert_eq!(sim.get(sim.signal("q_parameter_xz")), 0xa5u8.into());
    assert_eq!(sim.get(sim.signal("q_parameter_expr")), 0xa6u8.into());
    assert_eq!(sim.get(sim.signal("q_function_width")), 0xb6u8.into());
    assert_eq!(sim.get(sim.signal("q_function_signed")), 0xc7u8.into());
    assert_eq!(sim.get(sim.signal("q_function_param")), 0xd8u8.into());
    assert_eq!(
        sim.get_four_state(sim.signal("q_parameter_x_fill")),
        (BigUint::from(0x0fu8), BigUint::from(0x0fu8)),
    );
    assert_eq!(
        sim.get_four_state(sim.signal("q_parameter_z_fill")),
        (BigUint::from(0u8), BigUint::from(0x0fu8)),
    );
    assert_eq!(sim.get(sim.signal("q_function_integer")), 0xe9u8.into());
    assert_eq!(sim.get(sim.signal("q_function_typedef")), 0xfau8.into());
    assert_eq!(sim.get(sim.signal("q_function_nonansi")), 0xabu8.into());
    assert_eq!(sim.get(sim.signal("q_const_case_equality")), 1u8.into());
    assert_eq!(sim.get(sim.signal("q_const_case_fill")), 1u8.into());
    assert_eq!(
        sim.get(sim.signal("q_const_wildcard_equality")),
        1u8.into(),
    );
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
