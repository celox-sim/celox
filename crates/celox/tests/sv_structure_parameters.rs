//! A member of a parameter of packed structure type is a constant (IEEE
//! 1800-2023 11.2.1, A.8.4 `constant_select`) of the member's declared
//! width and sign (7.2.1): it sizes ports and variables, bounds and selects
//! generate constructs, initializes other parameters and overrides those of
//! instances. The value of the parameter is a literal, an assignment
//! pattern (10.9.2) or what an instantiation assigns.
use celox::Simulation;
use std::path::Path;

/// Runs `source`, whose `initial` blocks print lines, and returns them.
fn display(source: &str, four_state: bool) -> Vec<String> {
    let mut sim =
        Simulation::from_sv_sources(vec![(source, Path::new("structure_parameters.sv"))], "Top")
            .four_state(four_state)
            .build()
            .unwrap();
    sim.run_until(100).unwrap();
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

/// The error of building `source`, on one line.
fn rejection(source: &str) -> String {
    Simulation::from_sv_sources(vec![(source, Path::new("structure_parameters.sv"))], "Top")
        .build()
        .expect_err("the source is rejected")
        .to_string()
        // The report wraps its lines.
        .replace(['\n', '│'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A structure wider than the numeric constants, with a member across bit
/// 128, as a pattern default, a forwarded value and an overriding literal.
#[test]
fn members_size_declarations_generates_and_overrides() {
    let source = r#"
    `timescale 1ns/1ns
    typedef struct packed {
      bit [4:0]  ATOMIC;
      bit [99:0] PAD0;
      bit [7:0]  THREADS;
      bit [99:0] PAD1;
      bit [16:0] BTB_SIZE;
      bit [8:0]  WIDTH;
    } param_t;
    module Dff #(parameter WIDTH = 1)(input logic [WIDTH-1:0] din, output logic [WIDTH-1:0] dout);
      assign dout = din;
    endmodule
    module Leaf #(
      parameter param_t pt = '{ATOMIC: 5'h01, PAD0: 100'h3, THREADS: 8'h02, PAD1: 100'h5,
                               BTB_SIZE: 17'h00200, WIDTH: 9'h008},
      parameter int ID = 0
    )(
      input  logic [pt.THREADS-1:0]               a,
      output logic [pt.THREADS-1:0][pt.WIDTH-1:0] y,
      output logic [$clog2(pt.BTB_SIZE)-1:0]      idx
    );
      localparam NT     = pt.THREADS;
      localparam NT_MSB = int'(pt.THREADS) - 1;
      localparam LG     = $clog2(pt.BTB_SIZE);
      logic [pt.THREADS-1:0] t;
      logic [pt.WIDTH-1:0]   mem [pt.THREADS];
      logic [NT_MSB:0]       u;
      Dff #(pt.THREADS)         f0(.din(a),  .dout(t));
      Dff #(.WIDTH(pt.THREADS)) f1(.din(~a), .dout(u));
      for (genvar i = 0; i < pt.THREADS; i++) begin : g
        assign mem[i] = {pt.WIDTH{t[i]}};
        assign y[i]   = mem[i];
      end
      if (pt.ATOMIC == 1) begin : atomic
        assign idx = (pt.THREADS)'(u);
      end else begin : plain
        assign idx = '0;
      end
      initial
        $display("leaf%0d NT=%0d NT_MSB=%0d LG=%0d a=%0d y=%0d idx=%0d mem=%0d member=%0d",
                 ID, NT, NT_MSB, LG, $bits(a), $bits(y), $bits(idx), $size(mem), $bits(NT));
    endmodule
    module Wrapper #(
      parameter param_t pt = '{ATOMIC: 5'h01, PAD0: 100'h3, THREADS: 8'h02, PAD1: 100'h5,
                               BTB_SIZE: 17'h00200, WIDTH: 9'h008}
    )(
      input  logic [pt.THREADS-1:0]               a,
      output logic [pt.THREADS-1:0][pt.WIDTH-1:0] y,
      output logic [$clog2(pt.BTB_SIZE)-1:0]      idx
    );
      Leaf #(.pt(pt), .ID(1)) leaf(.*);
    endmodule
    module Top;
      // ATOMIC=0 THREADS=3 BTB_SIZE=16 WIDTH=4
      localparam param_t tp = 239'h1c0c0000000000000000000000014002004;
      logic [1:0]      a1;
      logic [1:0][7:0] y1;
      logic [8:0]      idx1;
      logic [2:0]      a2;
      logic [2:0][3:0] y2;
      logic [3:0]      idx2;
      Wrapper w(.a(a1), .y(y1), .idx(idx1));
      Leaf #(.pt(tp), .ID(2)) l2(.a(a2), .y(y2), .idx(idx2));
      initial begin
        a1 = 2'b01;
        a2 = 3'b101;
        #1;
        $display("y1=%h idx1=%h y2=%h idx2=%h", y1, idx1, y2, idx2);
      end
    endmodule
    "#;
    for four_state in [false, true] {
        let mut lines = display(source, four_state);
        lines.sort();
        assert_eq!(
            lines,
            [
                "leaf1 NT=2 NT_MSB=1 LG=9 a=2 y=16 idx=9 mem=2 member=8",
                "leaf2 NT=3 NT_MSB=2 LG=4 a=3 y=12 idx=4 mem=3 member=8",
                "y1=00ff idx1=002 y2=f0f idx2=0",
            ],
            "four-state {four_state}"
        );
    }
}

/// Members of members, signed members, a parameter of a package named with
/// its scope and through an import, and a parameter of a generate block
/// that hides the module's.
#[test]
fn members_of_nested_package_and_generate_parameters() {
    let source = r#"
    `timescale 1ns/1ns
    package Config;
      typedef struct packed { logic [3:0] rows; logic signed [3:0] bias; } inner_t;
      typedef struct packed { inner_t shape; logic [7:0] depth; } outer_t;
      localparam outer_t CFG = '{shape: '{rows: 4'd3, bias: -4'sd2}, depth: 8'd5};
    endpackage
    module Sized #(parameter int N = 1)(output logic [N-1:0] ones);
      assign ones = '1;
    endmodule
    module Top;
      import Config::*;
      logic [Config::CFG.depth-1:0]  qualified;
      logic [CFG.shape.rows-1:0]     imported;
      logic [CFG.depth-1:0]          through;
      localparam int BIAS = CFG.shape.bias;
      localparam int WRAP = CFG.shape.rows - 4;
      Sized #(.N(CFG.depth)) sized(.ones(through));
      for (genvar i = 0; i < 2; i++) begin : g
        localparam inner_t CFG = '{rows: i + 1, bias: 4'sd1};
        logic [CFG.rows-1:0] local_bits;
        initial #1 $display("g%0d rows=%0d bits=%0d", i, CFG.rows, $bits(local_bits));
      end
      initial begin
        #2;
        $display("qualified=%0d imported=%0d through=%b", $bits(qualified), $bits(imported), through);
        $display("bias=%0d signed=%0d wrap=%h", BIAS, CFG.shape.bias < 0, WRAP);
      end
    endmodule
    "#;
    for four_state in [false, true] {
        assert_eq!(
            display(source, four_state),
            [
                "g0 rows=1 bits=1",
                "g1 rows=2 bits=2",
                "qualified=5 imported=3 through=11111",
                // A member keeps its sign; an unsigned member wraps as its
                // width and that of the other operand say (11.6.1).
                "bias=-2 signed=1 wrap=ffffffff",
            ],
            "four-state {four_state}"
        );
    }
}

/// A value narrower than the structure is extended to it, a negative one
/// by its sign, also past the 128 bits a number is kept in.
#[test]
fn members_of_extended_values() {
    let source = r#"
    `timescale 1ns/1ns
    typedef struct packed {
      bit [4:0]  HIGH;
      bit [99:0] PAD0;
      bit [7:0]  ACROSS;
      bit [99:0] PAD1;
      bit [16:0] MIDDLE;
      bit [8:0]  LOW;
    } param_t;
    module Top;
      localparam param_t ONES  = -1;
      localparam param_t SMALL = 5;
      localparam param_t FILL  = '1;
      logic [ONES.HIGH:0]   a;
      logic [ONES.ACROSS:0] b;
      logic [ONES.LOW:0]    c;
      logic [SMALL.HIGH:0]  d;
      logic [SMALL.LOW:0]   e;
      logic [FILL.HIGH:0]   f;
      initial #1 $display("%0d %0d %0d %0d %0d %0d", $bits(a), $bits(b), $bits(c), $bits(d),
                          $bits(e), $bits(f));
    endmodule
    "#;
    for four_state in [false, true] {
        assert_eq!(
            display(source, four_state),
            ["32 256 512 1 6 32"],
            "four-state {four_state}"
        );
    }
}

/// An instantiation assigns the parameter a pattern, whose values are
/// constants of the instantiating module.
#[test]
fn members_of_pattern_valued_overrides() {
    let source = r#"
    `timescale 1ns/1ns
    typedef struct packed { bit [7:0] n; bit [3:0] w; } param_t;
    module Leaf #(parameter param_t pt = '{n: 8'd1, w: 4'd2})(
      output logic [pt.n-1:0][pt.w-1:0] y);
      assign y = '1;
      for (genvar i = 0; i < pt.n; i++) begin : g
        initial #1 $display("leaf n=%0d w=%0d i=%0d", pt.n, pt.w, i);
      end
    endmodule
    module Top #(parameter int K = 2);
      logic [2:0][4:0] a;
      logic [0:0][1:0] b;
      logic [K-1:0][0:0] c;
      Leaf #(.pt('{n: 8'd3, w: 4'd5})) la(.y(a));
      Leaf lb(.y(b));
      Leaf #(.pt('{n: K, w: 4'd1})) lc(.y(c));
      initial #2 $display("a=%h b=%h c=%h", a, b, c);
    endmodule
    "#;
    for four_state in [false, true] {
        let mut lines = display(source, four_state);
        lines.sort();
        assert_eq!(
            lines,
            [
                "a=7fff b=3 c=3",
                "leaf n=1 w=2 i=0",
                "leaf n=2 w=1 i=0",
                "leaf n=2 w=1 i=1",
                "leaf n=3 w=5 i=0",
                "leaf n=3 w=5 i=1",
                "leaf n=3 w=5 i=2",
            ],
            "four-state {four_state}"
        );
    }
}

/// A packed parameter given by an assignment pattern has a value at
/// elaboration, like one given by a literal: it is no variable.
#[test]
fn pattern_valued_packed_parameters_are_elaboration_constants() {
    let source = r#"
    `timescale 1ns/1ns
    module Top #(parameter int N = 3);
      localparam logic [N-1:0][3:0] TABLE = '{N{4'd5}};
      localparam logic [1:0][3:0]   PAIR  = '{4'd2, 4'd6};
      logic [PAIR[1]:0]  two;
      logic [PAIR[0]-1:0] six;
      logic [$bits(TABLE)-1:0] all;
      for (genvar i = 0; i < PAIR[1]; i++) begin : g
        initial #1 $display("g%0d", i);
      end
      initial begin
        #2;
        $display("two=%0d six=%0d all=%0d table=%h", $bits(two), $bits(six), $bits(all), TABLE);
      end
    endmodule
    "#;
    for four_state in [false, true] {
        assert_eq!(
            display(source, four_state),
            ["g0", "g1", "two=3 six=6 all=12 table=555"],
            "four-state {four_state}"
        );
    }
}

/// What is no constant stays one: a member of a variable in a constant
/// expression (11.2.1), and a dotted name that starts at nothing the scope
/// declares, which is a hierarchical name (23.6).
#[test]
fn rejects_members_that_are_no_constants() {
    let variable = rejection(
        r#"
        typedef struct packed { logic [3:0] n; } s_t;
        module Top(input s_t s, output logic y);
          logic [s.n-1:0] v;
          assign y = v[0];
        endmodule
        "#,
    );
    assert!(!variable.is_empty());
    let unknown = rejection(
        r#"
        module Top(output logic y);
          logic [nothing.n-1:0] v;
          assign y = v[0];
        endmodule
        "#,
    );
    assert!(unknown.contains("nothing.n"), "{unknown}");
}

/// A member of an element of an unpacked array parameter of packed
/// structures is a constant when the index is one (IEEE 1800-2023 11.2.1):
/// in a parameter initializer, a generate condition and the bounds of a
/// part-select, with a genvar index; with a run-time index it is a value.
/// The array may be one of a package, named with its scope or imported, or
/// of the module.
#[test]
fn members_of_array_parameter_elements() {
    let source = r#"
    `timescale 1ns/1ns
    package Parts;
      typedef struct packed { logic secret; logic [10:0] offset; logic [10:0] size; } part_t;
      parameter int Count = 3;
      localparam part_t Info [Count] = '{
        '{secret: 1'b0, offset: 11'd0,  size: 11'd8},
        '{secret: 1'b1, offset: 11'd8,  size: 11'd16},
        '{secret: 1'b0, offset: 11'd24, size: 11'd8}
      };
    endpackage
    module Top import Parts::*; ;
      localparam part_t Local [2] = '{'{1'b1, 11'd4, 11'd2}, '{1'b0, 11'd6, 11'd3}};
      logic [31:0] data;
      logic [Count-1:0] sel;
      logic [10:0] addr;
      logic [1:0] idx;
      logic [10:0] base;
      logic [Parts::Info[1].size-1:0] qualified;
      logic [Local[1].offset:Local[0].offset] local_bits;
      for (genvar k = 0; k < Count; k++) begin : gen_part
        localparam int unsigned PartEnd = 32'(Info[k].offset) + 32'(Info[k].size);
        if (Info[k].offset == 0) begin : gen_zero
          assign sel[k] = addr < PartEnd;
        end else begin : gen_nonzero
          assign sel[k] = (addr >= Info[k].offset) & (addr < PartEnd);
        end
        assign data[Info[k].offset +: Info[k].size] = {4{8'(k + 1)}};
        initial #1 $display("k=%0d end=%0d secret=%b", k, PartEnd, Info[k].secret);
      end
      always_comb base = Info[idx].offset;
      initial begin
        addr = 11'd10;
        idx = 2;
        #2 $display("sel=%b base=%0d data=%h", sel, base, data);
        $display("qualified=%0d local=%0d", $bits(qualified), $bits(local_bits));
      end
    endmodule
    "#;
    for four_state in [false, true] {
        let mut lines = display(source, four_state);
        lines.sort();
        assert_eq!(
            lines,
            [
                "k=0 end=8 secret=0",
                "k=1 end=24 secret=1",
                "k=2 end=32 secret=0",
                "qualified=16 local=3",
                "sel=010 base=24 data=03020201",
            ],
            "four-state {four_state}"
        );
    }
}

/// The elements may be given by enumeration members and casts, and a member
/// of enumerated type compares with the enumeration's members.
#[test]
fn enumerated_members_of_array_parameter_elements() {
    let source = r#"
    `timescale 1ns/1ns
    package Keys;
      typedef enum logic [2:0] { KeyA, KeyB } key_e;
    endpackage
    package Parts;
      import Keys::*;
      typedef enum logic [1:0] { Unbuffered, Buffered, LifeCycle } variant_e;
      typedef struct packed {
        variant_e    variant;
        logic [10:0] offset;
        logic [10:0] size;
        key_e        key;
      } part_t;
      parameter int Count = 2;
      localparam part_t Info [Count] = '{
        '{variant: Unbuffered, offset: 11'd0,  size: 64, key: key_e'('0)},
        '{variant: Buffered,   offset: 11'd64, size: 32, key: KeyB}
      };
    endpackage
    module Top import Parts::*; ;
      for (genvar k = 0; k < Count; k++) begin : gen_part
        localparam int End = 32'(Info[k].offset) + 32'(Info[k].size);
        if (Info[k].variant == Buffered) begin : gen_buffered
          initial #1 $display("k=%0d end=%0d buffered key=%0d", k, End, Info[k].key);
        end else begin : gen_unbuffered
          initial #1 $display("k=%0d end=%0d unbuffered key=%0d", k, End, Info[k].key);
        end
      end
    endmodule
    "#;
    for four_state in [false, true] {
        let mut lines = display(source, four_state);
        lines.sort();
        assert_eq!(
            lines,
            ["k=0 end=64 unbuffered key=0", "k=1 end=96 buffered key=1"],
            "four-state {four_state}"
        );
    }
}
