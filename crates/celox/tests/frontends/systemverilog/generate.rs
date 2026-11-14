use super::*;

sv_backends! {
    fn generated_constants_select_words_from_128_bit_parameters_at_o2(sim) {
        @setup {
            // Icarus 13 rejects packed array parameters; the same source is
            // independently checked with Verilator.
            let source = r#"
    module Top #(parameter logic [3:0][31:0] WORDS={4{32'd4}})(
      output logic [15:0] widths, output logic [31:0] first,last,edge_first,edge_last);
      for(genvar i=0;i<4;i=i+1) begin:g
        localparam C=$clog2(WORDS[i]+1);
        assign widths[i*4 +: 4]=C;
      end
      localparam FIRST=WORDS[0], LAST=WORDS[3];
      localparam logic [3:0][31:0] EDGE=128'h80000000000000040000000480000000;
      localparam E0=EDGE[0], E3=EDGE[3];
      assign edge_first=E0;
      assign edge_last=E3;
      assign first=FIRST;
      assign last=LAST;
    endmodule
  "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("constant_word.sv"))], "Top")
            .opt_level(celox::OptLevel::O2);
        assert_eq!(sim.get(sim.signal("widths")), BigUint::from(0x3333u32));
        assert_eq!(sim.get(sim.signal("first")), BigUint::from(4u8));
        assert_eq!(sim.get(sim.signal("last")), BigUint::from(4u8));
        assert_eq!(sim.get(sim.signal("edge_first")), BigUint::from(0x80000000u32));
        assert_eq!(sim.get(sim.signal("edge_last")), BigUint::from(0x80000000u32));
    }

}

// A data query of a variable reads the declaration nearest to it (IEEE
// 1800-2023 20.6.2, 23.9), whichever binding evaluates it: a parameter of
// the module, a generate block that declares the name again or not, a
// function with a formal or a local of that name, a method of a class with
// a property of that name, and a block or loop of a procedure.
#[test]
fn data_queries_read_the_nearest_declaration() {
    let source = r#"
        `timescale 1ns/1ns
        package pk;
            class K;
                bit [4:0] s;
                function int size(); return $bits(s); endfunction
                extern function int later();
            endclass
            function int K::later(); return $bits(s); endfunction
        endpackage
        module Top;
            import pk::*;
            logic [15:0] s;
            logic [6:0] v;
            typedef struct packed { logic [2:0] a; logic [8:0] b; } pair_t;
            pair_t q;
            localparam int P = $bits(s);
            function automatic int formal(logic [2:0] s); return $bits(s); endfunction
            function automatic int local_s(); logic [1:0] s; return $bits(s); endfunction
            function automatic int outer(); return $bits(s); endfunction
            if (1) begin : g
                wire [$bits(s)-1:0] a;
                wire [$bits(v)-1:0] b;
                wire [$bits(q)-1:0] c;
            end
            if (1) begin : h
                logic [8:0] s;
                pair_t near;
                wire [$bits(s)-1:0] a;
                wire [$bits(near)-1:0] b;
                if (1) begin : inner
                    wire [$bits(s)-1:0] a;
                    wire [formal(0)+local_s()+outer():0] c;
                end
            end
            logic [7:0] lane_widths;
            for (genvar i = 0; i < 2; ++i) begin : lanes
                logic [i + 2:0] lane;
                wire [$bits(lane)-1:0] a;
                assign lane_widths[4*i +: 4] = $bits(a);
            end
            initial begin
                K k;
                k = new;
                #1;
                $display("%0d %0d %0d %0d %0d", P, formal(0), local_s(), outer(), $bits(s));
                $display("%0d %0d %0d", $bits(g.a), $bits(g.b), $bits(g.c));
                $display("%0d %0d %0d %0d", $bits(h.a), $bits(h.b), $bits(h.inner.a), $bits(h.inner.c));
                $display("%h", lane_widths);
                $display("%0d %0d", k.size(), k.later());
                begin : blk
                    logic [11:0] s;
                    $display("%0d", $bits(s));
                end
                for (int i = 0; i < 1; i++) begin
                    logic [12:0] v;
                    $display("%0d", $bits(v));
                end
                $finish;
            end
        endmodule
    "#;
    assert_eq!(
        hierarchical_references::displayed(source),
        [
            "16 3 2 16 16",
            "16 7 12",
            "9 12 9 22",
            "43",
            "5 5",
            "12",
            "13"
        ]
    );
}

// A data query of a variable of a generate block reads the type the
// variable is declared with: a real, a shortreal or a chandle has no
// dimensions and a queue is sized when the query runs (IEEE 1800-2023 20.7),
// and an enumeration member of a nested block hides the variable (23.9).
#[test]
fn data_queries_read_the_declared_types_of_generate_variables() {
    let source = r#"
        `timescale 1ns/1ns
        module Top;
            if (1) begin : g
                real r;
                shortreal sr;
                chandle h;
                int q[$];
                logic [7:0] A;
                localparam int DR = $dimensions(r);
                localparam int DS = $dimensions(sr);
                localparam int DH = $dimensions(h);
                localparam int BR = $bits(r);
                localparam int BS = $bits(sr);
                localparam int BA = $bits(A);
                wire [$dimensions(r):0] dr;
                wire [$dimensions(h):0] dh;
                if (1) begin : inner
                    typedef enum {A, B} e_t;
                    localparam int W = $bits(A);
                    wire [$bits(A)-1:0] w;
                    initial begin
                        #2;
                        $display("%0d %0d", W, $bits(w));
                    end
                end
                initial begin
                    #1;
                    q.push_back(1);
                    q.push_back(2);
                    q.push_back(3);
                    $display("%0d %0d %0d %0d %0d %0d", DR, DS, DH, BR, BS, BA);
                    $display("%0d %0d %0d", $bits(dr), $bits(dh), $size(q));
                    #2;
                    $finish;
                end
            end
        endmodule
    "#;
    assert_eq!(
        hierarchical_references::displayed(source),
        ["0 0 0 64 32 8", "1 1 3", "32 32"]
    );
}

#[test]
fn generate_function_call_preserves_definition_site_bindings() {
    let source = r#"
        module Top(input logic clk, input logic [1:0] a, output logic [3:0] y);
            function automatic logic f(input logic x); return a[0] ^ x; endfunction
            if (1) begin : g
                logic [1:0] a;
                logic x;
                assign a = 2'b10;
                assign x = 0;
                function automatic logic local_value(); return a[1]; endfunction
                always_comb y[0] = f(x);
                assign y[1] = local_value();
                always_comb y[2] = local_value();
                always_ff @(posedge clk) y[3] <= f(x);
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("generate_function_binding.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let a = sim.signal("a");
    let clk = sim.event("clk");
    for value in 0u8..4 {
        sim.modify(|io| io.set(a, value)).unwrap();
        sim.tick(clk).unwrap();
        let expected = if value & 1 != 0 { 15u8 } else { 6u8 };
        assert_eq!(sim.get(sim.signal("y")), expected.into());
    }
}

// Each iteration of a generate loop declares enumerations of its own (IEEE
// 1800-2023 27.4), whose members are constants of that type there (6.19.3),
// also those of an enumeration declared after another in the block.
#[test]
fn generate_loop_enumerations_type_their_members() {
    let source = r#"
        module Top(output logic [7:0] y);
            for (genvar i = 0; i < 2; i++) begin : g
                typedef enum logic [1:0] {A, B, C} e_t;
                typedef enum logic [1:0] {D, E, F} f_t;
                e_t e;
                f_t f;
                assign e = C;
                assign f = E;
                assign y[i*4 +: 4] = {f, e};
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("generate_enums.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    sim.modify(|_| {}).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0x66u8.into());
}

// Members of an anonymous enum declared in a generate block are constants of
// that block (IEEE 1800-2023 6.19, 27.5) also when the block declares no
// typedef, read by always_ff, always_comb and continuous assignments; a
// sibling block's members of the same names keep their own values.
// BlackParrot's bp_me_stream_pump_control declares its state this way.
#[test]
fn anonymous_enum_members_are_constants_of_their_generate_block() {
    let source = r#"
        module Top #(parameter W = 2)(input logic clk, rst, go,
            output logic ready, output logic [1:0] code, other);
            if (W == 1) begin : z
                assign ready = 1'b1;
                assign code = 2'd0;
                assign other = 2'd0;
            end else begin : nz
                enum logic [1:0] {e_ready, e_primary = 2'd2, e_stream} state_n, state_r;
                assign ready = (state_r == e_ready);
                assign code = state_r;
                always_comb begin
                    state_n = state_r;
                    case (state_r)
                        e_ready: if (go) state_n = e_primary;
                        e_primary: state_n = e_stream;
                        default: state_n = e_ready;
                    endcase
                end
                always_ff @(posedge clk)
                    if (rst) state_r <= e_ready;
                    else state_r <= state_n;
            end
            if (W != 1) begin : sibling
                enum logic [1:0] {e_ready = 2'd3, e_stream = 2'd1} s;
                assign other = e_ready;
            end
        endmodule
    "#;
    let mut sim = Simulator::from_sv_sources(
        vec![(source, Path::new("generate_anonymous_enum.sv"))],
        "Top",
    )
    .build_cranelift()
    .unwrap();
    let clk = sim.event("clk");
    let (rst, go) = (sim.signal("rst"), sim.signal("go"));
    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(go, 1u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    for (ready, code) in [(1u8, 0u8), (0, 2), (0, 3), (1, 0)] {
        assert_eq!(sim.get(sim.signal("ready")), ready.into());
        assert_eq!(sim.get(sim.signal("code")), code.into());
        assert_eq!(sim.get(sim.signal("other")), 3u8.into());
        sim.tick(clk).unwrap();
    }
}

#[test]
fn rejects_unqualified_generate_function_calls_outside_their_scope() {
    for call in [
        "assign y = f(a);",
        "if (1) begin : sibling assign y = f(a); end",
    ] {
        let source = format!(
            r#"
            module Top(input logic a, output logic y);
                if (1) begin : g
                    function automatic logic f(input logic x); return x; endfunction
                end
                {call}
            endmodule
        "#
        );
        assert!(
            Simulator::from_sv_sources(
                vec![(&source, Path::new("generate_function_visibility.sv"))],
                "Top"
            )
            .build_cranelift()
            .is_err(),
            "accepted out-of-scope call: {call}"
        );
    }
}

#[test]
fn keeps_four_state_generate_function_guards_nonexhaustive() {
    let source = r#"
        module Top(input logic a, output logic y);
            function automatic bit f(input logic x); return x; endfunction
            if (1) begin : g
                function automatic logic f(input logic x); return x; endfunction
                always_comb if (f(a) || !f(a)) y = a;
            end
        endmodule
    "#;
    let result = Simulator::from_sv_sources(
        vec![(source, Path::new("generate_four_state_function_guard.sv"))],
        "Top",
    )
    .four_state(true)
    .build_cranelift();
    let error = match result {
        Ok(_) => panic!("accepted an incomplete four-state guard"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("latch inference inside always_comb"),
        "{error}"
    );
}

// Icarus 13 rejects enum labels inside generate scopes and crashes while
// elaborating this source. Keep O2 isolation coverage in the API harness;
// the portable suite separately checks generated typedef layouts.
sv_backends! {
    fn generated_enum_and_shadowed_type_scopes_survive_o2(sim) {
        @setup {
            let source=r#"
        module Top(input logic[7:0] a, output logic[7:0] y,z,widths,
            output logic[3:0] root_value, output logic[7:0] parent_value);
            typedef logic[3:0] T;
            T root;
            assign root=a[3:0];
            assign root_value=root;
            if(1) begin:left
                typedef logic[7:0] T;
                typedef enum logic[1:0] { OFF=0, ON=3 } state_t;
                T temp;
                state_t state;
                assign state=ON;
                assign temp=a;
                assign y=(state==3) ? temp : 0;
                if(1) begin:inner
                    typedef logic[1:0] T;
                    T tiny;
                    assign tiny=a[1:0];
                end
                T after_inner;
                assign after_inner=a;
                assign parent_value=after_inner;
            end
            if(1) begin:right
                typedef logic[3:0] T;
                typedef enum logic[1:0] { OFF=1, ON=2 } state_t;
                T temp;
                state_t state;
                assign state=ON;
                assign temp=a[3:0];
                assign z=(state==2) ? {4'hc,temp} : 0;
            end
            for(genvar i=0;i<2;i=i+1) begin:lanes
                typedef logic[i+2:0] lane_t;
                lane_t temp;
                assign temp=a;
                assign widths[4*i+:4]=$bits(temp);
            end
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(source,Path::new("generated_enum.sv"))],"Top")
            .opt_level(celox::OptLevel::O2);
        let a=sim.signal("a");
        for value in [0xa7u8,0x5c] {
            sim.modify(|io| io.set(a,value)).unwrap();
            assert_eq!(sim.get(sim.signal("y")),BigUint::from(value));
            assert_eq!(sim.get(sim.signal("z")),BigUint::from(0xc0|(value&15)));
            assert_eq!(sim.get(sim.signal("parent_value")),BigUint::from(value));
            assert_eq!(sim.get(sim.signal("root_value")),BigUint::from(value&15));
            assert_eq!(sim.get(sim.signal("widths")),BigUint::from(0x43u8));
        }
    }
}

// A variable of a generate block is named from outside the block by the
// block's name (IEEE 1800-2023 27.6, 23.6): `gen.var`, also of a nested
// block or of a loop iteration, read and written, as Ibex's functional
// coverage reads `gen_regfile_ecc.rf_ecc_err_a_id` from a generate block
// of its own.
#[test]
fn variables_of_generate_blocks_by_their_scope_names() {
    let source = r#"
        `timescale 1ns/1ns
        module Core #(parameter bit Ecc = 1'b1) (input logic [3:0] a, output logic [5:0] y);
            if (1) begin : outer
                if (1) begin : inner
                    logic deep;
                    assign deep = a[3];
                end
                logic mid;
                assign mid = a[1];
            end
            for (genvar i = 0; i < 2; i++) begin : lane
                logic bit_i;
                assign bit_i = a[i];
            end
            if (1) begin : sink
                logic w;
            end
            assign sink.w = ~a[2];
            if (Ecc) begin : gen_ecc
                logic err;
                assign err = ^a;
            end
            logic fcov;
            if (Ecc) begin : g_fcov
                assign fcov = gen_ecc.err;
            end else begin : g_no_fcov
                assign fcov = 1'b0;
            end
            assign y = {fcov, sink.w, outer.inner.deep, outer.mid, lane[1].bit_i, lane[0].bit_i};
        endmodule
        module Top;
            logic [5:0] y1, y0;
            Core u1(.a(4'b1011), .y(y1));
            Core #(.Ecc(1'b0)) u0(.a(4'b1011), .y(y0));
            initial #1 $display("%b %b", y1, y0);
        endmodule
    "#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("scope_names.sv"))], "Top")
                .four_state(four_state)
                .build()
                .unwrap();
        sim.run_until(10).unwrap();
        let lines: Vec<String> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8_lossy(&message).into_owned())
                }
                _ => None,
            })
            .collect();
        assert_eq!(lines, ["111111 011111"], "four-state {four_state}");
    }
}

// A constant function that divides by one of its arguments is a constant
// function (IEEE 1800-2023 13.4.3): its result sizes declarations and bounds
// a loop generate construct, as fpnew's lane count does.
#[test]
fn constant_functions_divide_by_their_arguments() {
    let source = r#"
        `timescale 1ns/1ns
        package fp;
            function automatic int unsigned fp_width(int unsigned fmt);
                return fmt == 2 ? 16 : 32;
            endfunction
            function automatic int unsigned num_lanes(int unsigned width, int unsigned fmt, logic vec);
                return vec ? width / fp_width(fmt) : 1;
            endfunction
        endpackage
        module Slice #(parameter int unsigned Width = 64, parameter int unsigned Fmt = 2) ();
            localparam int unsigned NUM_LANES = fp::num_lanes(Width, Fmt, 1'b1);
            for (genvar lane = 0; lane < int'(NUM_LANES); lane++) begin : gen_lanes
                initial #1 $display("lane %0d of %0d", lane, NUM_LANES);
            end
        endmodule
        module Top;
            function automatic int unsigned quotient(int unsigned w, int unsigned x);
                return w / x;
            endfunction
            function automatic int unsigned remainder(int unsigned w, int unsigned x);
                return w % x;
            endfunction
            localparam int unsigned N = quotient(64, 2);
            localparam int unsigned M = remainder(64, 5);
            logic [N-1:0] wide;
            logic [M-1:0] narrow;
            Slice s();
            initial #2 $display("%0d %0d", $bits(wide), $bits(narrow));
        endmodule
    "#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("divisions.sv"))], "Top")
            .build_cranelift()
            .unwrap();
    sim.run_until(10).unwrap();
    let mut displayed: Vec<_> = sim
        .drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => Some(String::from_utf8(message).unwrap()),
            _ => None,
        })
        .collect();
    displayed.sort();
    assert_eq!(
        displayed,
        [
            "32 4",
            "lane 0 of 4",
            "lane 1 of 4",
            "lane 2 of 4",
            "lane 3 of 4"
        ]
    );
    // A zero divisor gives unknown bits (11.4.2) at the width the context
    // gives the division (11.8.2), as the same call at run time does.
    let zero = r#"
        module Top;
            function automatic logic [31:0] quotient(logic [7:0] w, logic [7:0] x);
                return w / x;
            endfunction
            localparam logic [31:0] N = quotient(8'd4, 8'd0);
            initial $display("%b %b", N, quotient(8'd4, 8'd0));
        endmodule
    "#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(zero, Path::new("zero.sv"))], "Top")
        .four_state(true)
        .build_cranelift()
        .unwrap();
    sim.run_until(1).unwrap();
    let displayed: Vec<_> = sim
        .drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => Some(String::from_utf8(message).unwrap()),
            _ => None,
        })
        .collect();
    let unknown = "x".repeat(32);
    assert_eq!(displayed, [format!("{unknown} {unknown}")]);
}

/// A parameter initializer divides an operand that is folded with its type
/// (a cast, a select, a concatenation) by another parameter (IEEE 1800-2023
/// 11.4.2): the divisor's zero check is part of the constant.
#[test]
fn parameter_initializers_divide_typed_operands_by_parameters() {
    let source = r#"`timescale 1ns/1ns
module top #(parameter a_p = 16777216, parameter b_p = 3, parameter z_p = 0) ();
  localparam longint unsigned cast_div_lp = (64'(a_p)*4)*b_p/b_p/4;
  localparam cast_mod_lp = 64'(a_p) % b_p;
  localparam cast_of_div_lp = 64'(a_p / b_p);
  localparam [63:0] concat_lp = {a_p / b_p, a_p % b_p};
  localparam select_lp = a_p[31:8] / b_p;
  localparam logic [63:0] zero_lp = 64'(a_p) / z_p;
  if (1) begin : g
    localparam longint unsigned scoped_lp = 64'(a_p) / b_p;
    logic [scoped_lp[3:0]-1:0] sized;
    initial #1 $display("%0d %0d", scoped_lp, $bits(sized));
  end
  initial begin
    #2 $display("%0d %0d %0d %h %0d %b", cast_div_lp, cast_mod_lp, cast_of_div_lp, concat_lp,
                select_lp, zero_lp[3:0]);
    $finish;
  end
endmodule"#;
    // Icarus 13 and Verilator 5.054 print the four-state lines.
    for (four_state, by_zero) in [(false, "0000"), (true, "xxxx")] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("divide.sv"))], "top")
                .four_state(four_state)
                .build()
                .unwrap();
        sim.run_until(10).unwrap();
        let lines: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8_lossy(&message).into_owned())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            lines,
            [
                "5592405 5".to_string(),
                format!("16777216 1 5592405 0055555500000001 21845 {by_zero}")
            ],
            "four_state={four_state}"
        );
    }
}

/// The negated reductions `~&`, `~|`, `~^` and `^~` are the complements of
/// the reductions (IEEE 1800-2023 11.4.9) in generate conditions too, as
/// BlackParrot's `if (~|fpu_support_p)` (Icarus: y=1110).
#[test]
fn generate_conditions_take_negated_reductions() {
    let source = r#"
        module Top(output logic [3:0] y);
            localparam logic [3:0] P = 4'b0011;
            localparam logic [3:0] F = 4'b1111;
            if (~|P) begin : g0 assign y[0] = 1'b1; end else begin : h0 assign y[0] = 1'b0; end
            if (~&P) begin : g1 assign y[1] = 1'b1; end else begin : h1 assign y[1] = 1'b0; end
            if (~^P) begin : g2 assign y[2] = 1'b1; end else begin : h2 assign y[2] = 1'b0; end
            if (^~F) begin : g3 assign y[3] = 1'b1; end else begin : h3 assign y[3] = 1'b0; end
        endmodule
    "#;
    for four_state in [false, true] {
        let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("negated.sv"))], "Top")
            .four_state(four_state)
            .build()
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            sim.get(sim.signal("y")),
            BigUint::from(0b1110u8),
            "four-state {four_state}"
        );
    }
}

/// A parameter override may call a constant function of the instantiating
/// module, also one declared in the Verilog-1995 style, whose arguments name
/// its parameters and generate variables (IEEE 1800-2023 13.4.3, 23.10).
/// Verilator 5.054 prints the same lines.
#[test]
fn parameter_overrides_call_constant_functions_of_the_instantiating_module() {
    let source = r#"`timescale 1ns/1ns
module child #(parameter inWidth = 1, parameter topBound = 1, parameter bottomBound = 0) ();
  logic [inWidth-1:0] w;
  initial #1 $display("%m %0d %0d %0d %0d", inWidth, topBound, bottomBound, $bits(w));
endmodule
module top #(parameter sigWidth = 24) ();
  function integer clog2;
    input integer fa;
    begin
      fa = fa - 1;
      for (clog2 = 0; fa > 0; clog2 = clog2 + 1) fa = fa >> 1;
    end
  endfunction
  function automatic integer scaled(input integer a);
    return a * sigWidth;
  endfunction
  localparam sigSumWidth = sigWidth * 3 + 2;
  child #(clog2(sigSumWidth) - 2, scaled(2), 1) positional ();
  child #(.inWidth(clog2(sigWidth + 1) - 2), .topBound(0), .bottomBound(sigWidth / 4)) named ();
  for (genvar i = 1; i < 3; i++) begin : g
    child #(.inWidth(clog2(i * 8)), .topBound(scaled(i))) each ();
  end
  initial #2 $finish;
endmodule"#;
    for four_state in [false, true] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source, Path::new("override_calls.sv"))],
            "top",
        )
        .four_state(four_state)
        .build()
        .unwrap();
        sim.run_until(10).unwrap();
        let mut lines: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8_lossy(&message).into_owned())
                }
                _ => None,
            })
            .collect();
        lines.sort();
        assert_eq!(
            lines,
            [
                "top.g[1].each 3 24 0 3",
                "top.g[2].each 4 48 0 4",
                "top.named 3 0 6 3",
                "top.positional 5 48 1 5",
            ],
            "four_state={four_state}"
        );
    }
}

// A typedef of a generate block may be sized by a parameter of the same
// block (IEEE 1800-2023 27.2, 6.18): variables, functions and their locals
// of the type take its bounds, as OpenTitan's LFSR permutes the columns of
// a matrix sized by the block's `NumSboxes`.
#[test]
fn generate_local_typedefs_take_the_parameters_of_their_block() {
    let source = r#"
        `timescale 1ns/1ns
        module Lfsr #(parameter int LfsrDw = 16) (output logic [15:0] out);
            localparam int LfsrIdxDw = $clog2(LfsrDw);
            if (1) begin : gen_out
                localparam int NumSboxes = LfsrDw / 4;
                logic [3:0][NumSboxes-1:0][LfsrIdxDw-1:0] indices, rotrev;
                for (genvar j = 0; j < LfsrDw; j++) begin : gen_idx
                    assign indices[j / NumSboxes][j % NumSboxes] = LfsrIdxDw'(j);
                end
                typedef logic [NumSboxes-1:0][LfsrIdxDw-1:0] col_t;
                col_t first;
                function automatic col_t lrotcol(col_t col, integer shift);
                    col_t rotated;
                    for (int k = 0; k < NumSboxes; k++) begin
                        rotated[(k + shift) % NumSboxes] = col[k];
                    end
                    return rotated;
                endfunction
                function automatic col_t revcol(col_t col);
                    return {<<LfsrIdxDw{col}};
                endfunction
                always_comb begin
                    rotrev[0] = indices[0];
                    rotrev[1] = lrotcol(indices[1], NumSboxes / 2);
                    rotrev[2] = revcol(indices[2]);
                    rotrev[3] = revcol(lrotcol(indices[3], 1));
                end
                assign first = rotrev[0];
                assign out = {rotrev[3][0], rotrev[2][0], rotrev[1][0], rotrev[0][0]};
            end
        endmodule
        module Top;
            logic [15:0] out;
            Lfsr u(.out(out));
            initial #1 $display("out=%h rows=%h first=%0d", out, u.gen_out.rotrev,
                                $bits(u.gen_out.first));
        endmodule
    "#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("lfsr.sv"))], "Top")
                .four_state(four_state)
                .build()
                .unwrap();
        sim.run_until(10).unwrap();
        let lines: Vec<String> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8_lossy(&message).into_owned())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            lines,
            ["out=eb60 rows=fcde89ab54763210 first=16"],
            "four-state {four_state}"
        );
    }
}

// A typedef of a generate block takes the block's parameter of a name an
// enclosing scope also declares (IEEE 1800-2023 27.2, 23.9): `N` of the
// block, not of the module, also through another typedef and for a
// parameter of the typedef's type.
#[test]
fn generate_local_typedefs_take_the_block_parameter_they_name() {
    let source = r#"
        `timescale 1ns/1ns
        module Top;
            localparam int N = 2;
            if (1) begin : gen_b
                localparam int N = 4;
                typedef logic [N-1:0][1:0] col_t;
                typedef col_t alias_t;
                col_t v;
                alias_t w;
                localparam col_t P = '1;
                function automatic col_t ones(); return '1; endfunction
                assign v = ones();
                assign w = P;
            end
            initial #1 $display("%0d %h %0d %h", $bits(gen_b.v), gen_b.v, $bits(gen_b.w), gen_b.w);
        endmodule
    "#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("shadow.sv"))], "Top")
                .four_state(four_state)
                .build()
                .unwrap();
        sim.run_until(10).unwrap();
        let lines: Vec<String> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8_lossy(&message).into_owned())
                }
                _ => None,
            })
            .collect();
        assert_eq!(lines, ["8 ff 8 ff"], "four-state {four_state}");
    }
}

/// A variable of a generate block whose type is a typedef that a parameter
/// sizes connects to the ports of the instances of the block, whole and by
/// member (IEEE 1800-2023 27.3, 23.3.2).
#[test]
fn generate_local_variables_of_parameter_sized_types_connect_to_instance_ports() {
    let source = r#"`timescale 1ns/1ns
module decoder #(parameter w_p = 5) (input [w_p-1:0] target_i);
  initial #1 $display("%m %0d %h", $bits(target_i), target_i);
endmodule
module router #(parameter cw_p = 5) (input [15:0] data_i);
  typedef struct packed { logic [2:0] len; logic [cw_p-1:0] cord; } hdr_s;
  for (genvar i = 0; i < 2; i++) begin : in_ch
    hdr_s hdr;
    assign hdr = data_i[$bits(hdr_s)-1:0] + 8'(i);
    decoder #(.w_p(cw_p)) member (.target_i(hdr.cord));
    decoder #(.w_p($bits(hdr_s))) whole (.target_i(hdr));
  end
endmodule
module top;
  logic [15:0] data = 16'hbeef;
  router r (.data_i(data));
  initial #2 $finish;
endmodule"#;
    for four_state in [false, true] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source, Path::new("generate_local.sv"))],
            "top",
        )
        .four_state(four_state)
        .build()
        .unwrap();
        sim.run_until(10).unwrap();
        let mut lines: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8_lossy(&message).into_owned())
                }
                _ => None,
            })
            .collect();
        lines.sort();
        assert_eq!(
            lines,
            [
                "top.r.in_ch[0].member 5 0f",
                "top.r.in_ch[0].whole 8 ef",
                "top.r.in_ch[1].member 5 10",
                "top.r.in_ch[1].whole 8 f0",
            ],
            "four_state={four_state}"
        );
    }
}
