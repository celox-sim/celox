use super::*;

sv_backends! {

    // Compare cached runtime calls with elaboration-time constants through
    // Celox's four-state API, including the payload and mask of unknown bits.
    fn constant_function_inside_unknowns(sim) {
        @setup { let source = r#"
    module Top(output logic [4:0] q, output logic [4:0] p);
      function automatic logic unknown_match();
        logic x;
        return x inside {1'b1};
      endfunction
      function automatic logic definite_match(); return 1'bx inside {1'b0, 1'b?}; endfunction
      function automatic logic definite_mismatch(); return 2'bx0 inside {2'b11}; endfunction
      function automatic logic unknown_range(); return 1'bx inside {[1'b0:1'b1]}; endfunction
      function automatic logic empty_range(); return 2'b11 inside {[2'bxx:2'b01]}; endfunction
      localparam logic [4:0] P = {empty_range(), unknown_range(), definite_mismatch(), definite_match(), unknown_match()};
      assign p = P;
      assign q = {empty_range(), unknown_range(), definite_mismatch(), definite_match(), unknown_match()};
    endmodule
  "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("constant_inside.sv"))], "Top").four_state(true);
        for name in ["q", "p"] {
            assert_eq!(sim.get_four_state(sim.signal(name)), (BigUint::from(0x0bu8), BigUint::from(0x09u8)), "{name}");
        }
    }

}

fn display_lines(source: &str, four_state: bool) -> Vec<String> {
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("operators.sv"))], "top")
            .four_state(four_state)
            .build()
            .unwrap();
    sim.run_until(10).unwrap();
    sim.drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => {
                Some(String::from_utf8_lossy(&message).into_owned())
            }
            _ => None,
        })
        .collect()
}

/// The X and Z bits of a parameter value are don't-care bits of `inside` and
/// `==?` (IEEE 1800-2023 11.4.6, 11.4.13) also when the comparison reads an
/// element of a packed array parameter, here one an instantiation overrides
/// as the match addresses of a decoder. Values agree with Verilator 5.054.
#[test]
fn wildcard_comparisons_read_unknown_bits_of_parameter_elements() {
    let source = r#"`timescale 1ns/1ns
module dec #(parameter els_p = 2, parameter [els_p-1:0][19:0] base_addr_p = '0)
  (input [19:0] addr, output logic [els_p-1:0] m);
  for (genvar i = 0; i < els_p; i++) begin : g
    assign m[i] = (addr inside {base_addr_p[i]});
  end
endmodule
module top;
  localparam a_gp = 20'h2_????;
  localparam b_gp = 20'h0_1???;
  localparam [19:0] t_gp = 20'h2_????;
  localparam [1:0][19:0] arr = {a_gp, b_gp};
  logic [19:0] addr;
  logic [1:0] m, n;
  dec #(.els_p(2), .base_addr_p({a_gp, b_gp})) d(.addr(addr), .m(m));
  dec #(.els_p(2), .base_addr_p(arr)) e(.addr(addr), .m(n));
  task show;
    $display("%h %b%b%b%b%b %b %b", addr, addr inside {a_gp}, addr inside {t_gp},
             addr inside {arr[1]}, addr ==? arr[1], addr[15:0] ==? arr[0][15:0], m, n);
  endtask
  initial begin
    addr = 20'h20000; #1 show();
    addr = 20'h20004; #1 show();
    addr = 20'h01008; #1 show();
    addr = 20'h30000; #1 show();
  end
endmodule"#;
    for four_state in [false, true] {
        assert_eq!(
            display_lines(source, four_state),
            [
                "20000 11110 10 10",
                "20004 11110 10 10",
                "01008 00001 01 01",
                "30000 00000 00 00",
            ],
            "four_state={four_state}"
        );
    }
}

/// A parameter, and an element of a packed array parameter, keep the Z bits
/// of their value (IEEE 1800-2023 5.7.1) in four-state simulation.
#[test]
fn parameter_values_keep_unknown_bits() {
    let source = r#"`timescale 1ns/1ns
module top;
  localparam a_gp = 20'h2_????;
  localparam [19:0] t_gp = 20'h2_????;
  localparam [1:0][19:0] arr = {20'h2_????, 20'h0_1???};
  initial $display("%h %h %h %h %h", a_gp, t_gp, arr[1], arr[0][15:0], arr);
endmodule"#;
    assert_eq!(
        display_lines(source, true),
        ["2zzzz 2zzzz 2zzzz 1zzz 2zzzz01zzz"]
    );
}

// A replication count is a constant expression (IEEE 1800-2023 11.4.12),
// here a concatenation of parameters, one of which an enumeration member
// gives, as Ariane's `ValidRule = {{NrRegion * NB_PERIPHERALS}{1'b1}}`
// does in a package. The parameters are known when the enumeration members
// are, after the scope's first pass over its parameters.
#[test]
fn replication_counts_of_parameters_given_by_enumeration_members() {
    let source = r#"
        `timescale 1ns/1ns
        package soc;
            typedef enum int unsigned { DRAM = 0, GPIO = 1, Debug = 9 } slaves_t;
            localparam NB = Debug + 1;
            localparam NrRegion = 2;
            localparam logic [NrRegion-1:0][NB-1:0] ValidRule = {{NrRegion * NB}{1'b1}};
        endpackage
        module Top;
            typedef enum logic [1:0] { A, B, C } abc_t;
            localparam W = C + 1;
            localparam logic [2*W-1:0] MASK = {{W}{2'b10}};
            initial begin
                #1 $display("%b %0d", soc::ValidRule, $bits(soc::ValidRule));
                $display("%b", MASK);
            end
        endmodule
    "#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("counts.sv"))], "Top")
                .four_state(four_state)
                .build_cranelift()
                .unwrap();
        sim.run_until(10).unwrap();
        let displayed: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8(message).unwrap())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            displayed,
            ["11111111111111111111 20", "101010"],
            "four_state={four_state}"
        );
    }
}

// A count that stays unknown once every parameter of the scope is collected
// is still rejected, and so is a replication of a value that is no constant,
// whether or not its count is known yet.
#[test]
fn replication_counts_of_unresolved_parameters_are_rejected() {
    for source in [
        "module Top(output logic [7:0] y);
            localparam A = undefined_name + 1;
            localparam logic [7:0] V = {{A}{1'b1}};
            assign y = V;
        endmodule",
        "module Top #(parameter N) (input logic sig, output logic [7:0] y);
            localparam logic [7:0] P = {{N}{sig}};
            assign y = '0;
        endmodule",
    ] {
        assert!(
            Simulator::from_sv_sources(vec![(source, Path::new("unresolved.sv"))], "Top")
                .build_cranelift()
                .is_err(),
            "{source}"
        );
    }
}

/// An unpacked array in the set of `inside` stands for its elements (IEEE
/// 1800-2023 11.4.13), which is not implemented: such a set is rejected
/// rather than compared with the array as one value, for fixed-size arrays,
/// queues, dynamic and associative arrays, and class properties, in `inside`
/// expressions and `case inside` items. The left operand of `inside`, and a
/// `case` item, is singular. A set of elements of arrays is taken.
#[test]
fn unpacked_arrays_in_inside_sets_are_rejected() {
    for (body, want) in [
        (
            "int a[4]; initial x = 3 inside {a};",
            "an unpacked array in the set of inside",
        ),
        (
            "int q[$]; initial x = 3 inside {1, q};",
            "an unpacked array in the set of inside",
        ),
        (
            "int d[]; initial x = 3 inside {d};",
            "an unpacked array in the set of inside",
        ),
        (
            "int m[int]; initial x = 3 inside {m};",
            "an unpacked array in the set of inside",
        ),
        (
            "class C; int q[$]; endclass C c; initial begin c = new; x = 3 inside {c.q}; end",
            "an unpacked array in the set of inside",
        ),
        (
            "int a[2]; initial x = a inside {1};",
            "an unpacked array as the operand of inside",
        ),
        (
            "int a[2]; int v; initial case (v) inside a: x = 1; default: x = 0; endcase",
            "an unpacked array in the set of inside",
        ),
        (
            "int a[2]; int v; initial case (v) a: x = 1; default: x = 0; endcase",
            "an unpacked array as a case item",
        ),
    ] {
        let source = format!("module Top(output bit x); {body} endmodule");
        let Err(error) = Simulator::from_sv_sources(vec![(&source, Path::new("inside.sv"))], "Top")
            .build_cranelift()
        else {
            panic!("{body} is accepted");
        };
        assert!(error.to_string().contains(want), "{body}: {error}");
    }
    let source = "module Top(output bit x, output bit y);
        int a[4] = '{1, 2, 3, 4};
        int q[$] = '{5};
        initial begin x = 3 inside {a[2], q[0]}; y = 4 inside {a[2], q[0]}; end
    endmodule";
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("inside.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("x")), BigUint::from(1u8));
    assert_eq!(sim.get(sim.signal("y")), BigUint::from(0u8));
}

/// A multi-word result whose width is no multiple of 64 keeps the bits above
/// its width clear: the carry out of a sum, the borrow of a difference or a
/// negation, the high bits of a product or a left shift, the sign fill of an
/// arithmetic right shift and an inversion must not reach the reductions,
/// comparisons and selects that read the result (BlackParrot's
/// `bp_be_pipe_int` computes `sum_zero = ~|sum` of a 65-bit sum). Values
/// agree with Verilator 5.054, including results narrowed from wider
/// operands.
const WIDE_PADDING_SOURCE: &str = r#"
module Top(input logic [@W@-1:0] a, b, input logic [6:0] n,
  output logic [5:0] add_o, sub_o, mul_o, neg_o, shl_o, shln_o, sar_o, sarn_o, inv_o, bp_o,
  output logic [5:0] shrw_o, addw_o, output logic bp_c);
  localparam int W = @W@;
  localparam logic [W-1:0] HALF = {1'b1, {(W-1){1'b0}}};
  logic [W-1:0] add, sub, mul, neg, shl, shln, sar, sarn, inv, bp, shrw, addw;
  assign add = a + b;
  assign sub = a - b;
  assign mul = a * b;
  assign neg = -a;
  assign shl = a << 3;
  assign shln = a << n;
  assign sar = $signed(a) >>> 3;
  assign sarn = $signed(a) >>> n;
  assign inv = ~a;
  assign {bp_c, bp} = {a[W-1], a} + {b[W-1], b} + 1'b1;
  assign shrw = {b, a} >> 3;
  assign addw = {b, a} + {a, b};
  `define OBS(r) {~|r, |r, r == '0, ^r, r >= HALF, r[W-1]}
  assign add_o = `OBS(add);
  assign sub_o = `OBS(sub);
  assign mul_o = `OBS(mul);
  assign neg_o = `OBS(neg);
  assign shl_o = `OBS(shl);
  assign shln_o = `OBS(shln);
  assign sar_o = `OBS(sar);
  assign sarn_o = `OBS(sarn);
  assign inv_o = `OBS(inv);
  assign bp_o = `OBS(bp);
  assign shrw_o = `OBS(shrw);
  assign addw_o = `OBS(addw);
endmodule
"#;

/// The six observations of `OBS` in `WIDE_PADDING_SOURCE`, most significant first.
fn wide_padding_observations(r: &BigUint, width: usize) -> u8 {
    let zero = r.bits() == 0;
    let bits = [
        zero,
        !zero,
        zero,
        r.count_ones() % 2 == 1,
        // `r >= HALF` and `r[W-1]`.
        r.bit(width as u64 - 1),
        r.bit(width as u64 - 1),
    ];
    bits.into_iter()
        .fold(0, |acc, bit| (acc << 1) | u8::from(bit))
}

fn check_wide_padding<B: celox::SimBackend>(mut sim: Simulator<B>, width: usize, context: &str) {
    let one = BigUint::from(1u8);
    let modulus = &one << width;
    let all = &modulus - 1u8;
    let half = &one << (width - 1);
    let pattern = BigUint::from_bytes_le(&[0x5a; 16]) & &all;
    let values = [
        BigUint::from(0u8),
        one.clone(),
        all.clone(),
        half.clone(),
        &one << 64usize,
        &half | &one,
        pattern,
    ];
    let shifts = [0u8, 3, 63, 64, width as u8 - 1, width as u8, 127];
    let sign_fill = |shift: usize| {
        if shift >= width {
            all.clone()
        } else {
            &all ^ (&all >> shift)
        }
    };
    let sar = |a: &BigUint, shift: usize| {
        if a.bit(width as u64 - 1) {
            (a >> shift) | sign_fill(shift)
        } else {
            a >> shift
        }
    };
    let sign_extend = |a: &BigUint| {
        if a.bit(width as u64 - 1) {
            a | &modulus
        } else {
            a.clone()
        }
    };
    let [ia, ib, inn] = ["a", "b", "n"].map(|name| sim.signal(name));
    let outputs = [
        "add_o", "sub_o", "mul_o", "neg_o", "shl_o", "shln_o", "sar_o", "sarn_o", "inv_o", "bp_o",
        "shrw_o", "addw_o", "bp_c",
    ]
    .map(|name| (name, sim.signal(name)));
    for (i, a) in values.iter().enumerate() {
        for (j, b) in values.iter().enumerate() {
            let n = shifts[(i * values.len() + j) % shifts.len()];
            let shift = usize::from(n);
            let bp_total = (sign_extend(a) + sign_extend(b) + 1u8) % (&modulus << 1usize);
            let results = [
                (a + b) % &modulus,
                (a + &modulus - b) % &modulus,
                (a * b) % &modulus,
                (&modulus - a) % &modulus,
                (a << 3usize) % &modulus,
                (a << shift) % &modulus,
                sar(a, 3),
                sar(a, shift),
                a ^ &all,
                &bp_total % &modulus,
                (((b << width) | a) >> 3usize) % &modulus,
                (a + b) % &modulus,
            ];
            let mut expected: Vec<BigUint> = results
                .iter()
                .map(|r| BigUint::from(wide_padding_observations(r, width)))
                .collect();
            expected.push(&bp_total >> width);
            sim.modify(|io| {
                io.set_wide(ia, a.clone());
                io.set_wide(ib, b.clone());
                io.set(inn, n);
            })
            .unwrap();
            for ((name, signal), expected) in outputs.iter().zip(&expected) {
                assert_eq!(
                    sim.get_four_state(*signal),
                    (expected.clone(), BigUint::from(0u8)),
                    "{name}: {context} width={width} a={a:#x} b={b:#x} n={n}"
                );
            }
        }
    }
}

macro_rules! wide_padding_backend {
    ($name:ident, $build:ident) => {
        #[test]
        fn $name() {
            for width in [65usize, 100] {
                let source = WIDE_PADDING_SOURCE.replace("@W@", &width.to_string());
                for level in [
                    celox::OptLevel::O0,
                    celox::OptLevel::O1,
                    celox::OptLevel::O2,
                ] {
                    for four_state in [false, true] {
                        let sim =
                            Simulator::from_sv_sources(vec![(&source, Path::new("pad.sv"))], "Top")
                                .opt_level(level)
                                .four_state(four_state)
                                .$build()
                                .unwrap();
                        let context = format!("{level:?} four_state={four_state}");
                        check_wide_padding(sim, width, &context);
                    }
                }
            }
        }
    };
}

mod wide_results_clear_bits_above_their_width {
    use super::*;

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    wide_padding_backend!(native, build_native);
    wide_padding_backend!(cranelift, build_cranelift);
    wide_padding_backend!(wasm, build_wasm);
    wide_padding_backend!(interpreter, build_interpreter);
}

const TRANSPOSED_ROW_REDUCTIONS_SOURCE: &str = r#"`timescale 1ns/1ns
module tp (input [2:0][2:0] i, output [2:0][2:0] o);
  for (genvar x = 0; x < 3; x++) begin : rof
    for (genvar y = 0; y < 3; y++) begin : rof2
      assign o[y][x] = i[x][y];
    end
  end
endmodule
module top;
  logic [1:0][2:0][2:0] s;
  for (genvar e = 0; e < 2; e++) begin : u
    wire [2:0][2:0] yt;
    tp t (.i(s[e]), .o(yt));
    wire [1:0] any, par;
    for (genvar i = 0; i < 2; i++) begin : ch
      wire [2:0] li;
      for (genvar j = 0; j < 3; j++) begin : cc
        assign li[j] = yt[i][j];
      end
      assign any[i] = |li;
      assign par[i] = ^li;
    end
  end
  initial begin
    for (int k = 0; k < 512; k++) begin
      s = {9'(k * 37 + 5), 9'(k)};
      #1 $display("%0d %b %b %b %b", k, u[0].any, u[1].any, u[0].par, u[1].par);
    end
  end
endmodule"#;

/// The reductions `|li` and `^li` of the rows of a transposed array that a
/// bit-by-bit copy takes into three-bit nets, as `bsg_wormhole_router` does
/// after `bsg_transpose` and `bsg_concentrate_static`, for every value of
/// one of two instances. The optimizer (VectorizeConcat) turns the copy of
/// row 0 into a copy of the whole nine-bit array into the three-bit net,
/// which takes its low bits; the native code generators used to keep all
/// nine, and `|li` reduced the whole array (in the RTLMeter design
/// BlackParrot a router dequeued an empty FIFO). Values agree with
/// Verilator 5.054.
fn check_transposed_row_reductions(lines: Vec<String>, context: &str) {
    // Bit `j` of row `i` of the transposed array is bit `3 * j + i` of `s[e]`.
    let rows = |value: u32, reduce: fn(u32, u32) -> u32| {
        let row = |i: u32| {
            (0..3)
                .map(|j| (value >> (3 * j + i)) & 1)
                .reduce(reduce)
                .unwrap()
        };
        format!("{}{}", row(1), row(0))
    };
    let expected: Vec<String> = (0..512u32)
        .map(|k| {
            let s = [k, (k * 37 + 5) & 0x1ff];
            let (or, xor): (fn(u32, u32) -> u32, fn(u32, u32) -> u32) =
                (|a, b| a | b, |a, b| a ^ b);
            format!(
                "{k} {} {} {} {}",
                rows(s[0], or),
                rows(s[1], or),
                rows(s[0], xor),
                rows(s[1], xor)
            )
        })
        .collect();
    assert_eq!(lines, expected, "{context}");
}

macro_rules! transposed_row_reductions_backend {
    ($name:ident, $build:ident) => {
        #[test]
        fn $name() {
            for level in [celox::OptLevel::O1, celox::OptLevel::O2] {
                for four_state in [false, true] {
                    let mut sim = celox::Simulation::from_sv_sources(
                        vec![(TRANSPOSED_ROW_REDUCTIONS_SOURCE, Path::new("transpose.sv"))],
                        "top",
                    )
                    .opt_level(level)
                    .four_state(four_state)
                    .$build()
                    .unwrap();
                    sim.run_until(1000).unwrap();
                    let lines = sim
                        .drain_runtime_events()
                        .into_iter()
                        .filter_map(|event| match event {
                            celox::RuntimeEvent::Display { message } => {
                                Some(String::from_utf8_lossy(&message).into_owned())
                            }
                            _ => None,
                        })
                        .collect();
                    let context = format!("{level:?} four_state={four_state}");
                    check_transposed_row_reductions(lines, &context);
                }
            }
        }
    };
}

mod reductions_of_transposed_rows_read_their_row {
    use super::*;

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    transposed_row_reductions_backend!(native, build_native);
    transposed_row_reductions_backend!(cranelift, build_cranelift);
    transposed_row_reductions_backend!(wasm, build_wasm);
    transposed_row_reductions_backend!(interpreter, build_interpreter);
}

/// An operand of `&&` or `||` whose known value decides the result, and a
/// known condition of `?:`, leave the result independent of the other
/// operand (IEEE 1800-2023 11.4.7, 11.4.11): a left one leaves the right
/// operand unevaluated, and a right one decides the result whatever the left
/// one is, as `prim_fifo_sync` writes `full_q || Pass == 1'b0`. A
/// pass-through FIFO turned off by a parameter, as OpenTitan's
/// `prim_fifo_sync` with `.Pass(0)` is, has no combinational path from its
/// input to its output, and a design that feeds the output back to the input
/// has no loop. Values agree with Verilator 5.054.
#[test]
fn known_operands_of_logical_and_conditional_operators_leave_the_others_unread() {
    for (form, valid, data) in [
        (
            "logical",
            "full_q || (Pass && wvalid_i)",
            "(Pass && !full_q) ? wdata_i : storage",
        ),
        (
            "conditional",
            "full_q || (Pass ? wvalid_i : 1'b0)",
            "Pass ? (full_q ? storage : wdata_i) : storage",
        ),
        (
            "right operands",
            "full_q || (wvalid_i && Pass)",
            "(full_q || Pass == 1'b0) ? storage : wdata_i",
        ),
    ] {
        let source = format!(
            r#"`timescale 1ns/1ns
module fifo1 #(parameter bit Pass = 1'b1) (input logic clk, input logic wvalid_i,
    input logic [7:0] wdata_i, output logic rvalid_o, output logic [7:0] rdata_o);
  logic full_q = 1'b0;
  logic [7:0] storage = 8'h00;
  assign rvalid_o = {valid};
  assign rdata_o = {data};
  always_ff @(posedge clk) begin full_q <= wvalid_i; if (wvalid_i) storage <= wdata_i; end
endmodule
module top;
  logic clk = 0, rv;
  logic [7:0] rd;
  fifo1 #(.Pass(1'b0)) u(.clk, .wvalid_i(!rv), .wdata_i(rd + 8'd1), .rvalid_o(rv), .rdata_o(rd));
  always #1 clk = ~clk;
  initial begin #9 $display("%b %h", rv, rd); $finish; end
endmodule"#
        );
        for four_state in [false, true] {
            assert_eq!(
                display_lines(&source, four_state),
                ["0 02"],
                "{form} four_state={four_state}"
            );
        }
    }
}

/// A condition comparing an enumeration parameter with a member is known as
/// well, also in an `always_comb`: the counter that OpenTitan's
/// `aes_cipher_control_fsm` keeps only for one S-box implementation reads
/// nothing in the others, where its register is a wire back to its input.
/// Values agree with Verilator 5.054.
#[test]
fn known_conditions_comparing_parameters_leave_the_other_arm_unread() {
    let source = r#"`timescale 1ns/1ns
package impl_pkg;
  typedef enum logic [1:0] {ImplLut, ImplDom} impl_e;
endpackage
module ctr import impl_pkg::*; #(parameter impl_e Impl = ImplLut) (input logic clk, output logic [2:0] q_o);
  logic [2:0] cyc_ctr_d, cyc_ctr_q;
  always_comb cyc_ctr_d = (Impl == ImplDom) ? cyc_ctr_q + 3'd1 : 3'd0;
  if (Impl == ImplDom) begin : g_ctr
    initial cyc_ctr_q = '0;
    always_ff @(posedge clk) cyc_ctr_q <= cyc_ctr_d;
  end else begin : g_no_ctr
    assign cyc_ctr_q = cyc_ctr_d;
  end
  assign q_o = cyc_ctr_q;
endmodule
module top;
  import impl_pkg::*;
  logic clk = 0;
  logic [2:0] lut, dom;
  ctr #(.Impl(ImplLut)) u_lut(.clk, .q_o(lut));
  ctr #(.Impl(ImplDom)) u_dom(.clk, .q_o(dom));
  always #1 clk = ~clk;
  initial begin #9 $display("%h %h", lut, dom); $finish; end
endmodule
"#;
    for four_state in [false, true] {
        assert_eq!(
            display_lines(source, four_state),
            ["0 4"],
            "four_state={four_state}"
        );
    }
}
