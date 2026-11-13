//! The actual of an input port may call a function (IEEE 1800-2023
//! 23.3.2.2, 13.4): the port takes the value of the call in the
//! instantiating module, also of a function whose body loops and branches,
//! such as OpenTitan's multi-bit encodings
//! `.mubi_i(mubi8_and_lo(init_locked, access_i.write_lock))`.
use celox::Simulation;
use std::path::Path;

/// Runs `source`, whose `initial` blocks print lines, and returns them.
fn display(source: &str, four_state: bool) -> Vec<String> {
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("call_actuals.sv"))], "Top")
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

#[test]
fn input_actuals_call_functions() {
    let source = r#"
    `timescale 1ns/1ns
    package Mubi;
      typedef enum logic [7:0] { True = 8'h96, False = 8'h69 } mubi8_t;
      function automatic mubi8_t and_act(mubi8_t a, mubi8_t b, mubi8_t act);
        logic [7:0] a_in, b_in, act_in, out;
        a_in = a; b_in = b; act_in = act;
        for (int k = 0; k < 8; k++) begin
          if (act_in[k]) out[k] = a_in[k] && b_in[k];
          else out[k] = a_in[k] || b_in[k];
        end
        return mubi8_t'(out);
      endfunction
      function automatic mubi8_t and_lo(mubi8_t a, mubi8_t b);
        return and_act(a, b, False);
      endfunction
      typedef struct packed { mubi8_t write_lock; mubi8_t read_lock; } access_t;
    endpackage
    module Sender import Mubi::*; (input mubi8_t mubi_i, output mubi8_t mubi_o);
      assign mubi_o = mubi_i;
    endmodule
    module Bytes (input logic [7:0] d_i, output logic [7:0] q_o);
      assign q_o = d_i;
    endmodule
    module Top import Mubi::*; #(parameter bit On = 1'b1) ;
      function automatic logic [7:0] count(logic [7:0] v);
        logic [7:0] n = 0;
        for (int k = 0; k < 8; k++) n += 8'(v[k]);
        return n;
      endfunction
      mubi8_t locked = False;
      access_t access = '{write_lock: True, read_lock: False};
      mubi8_t w, r;
      logic [7:0] c;
      Sender u_w(.mubi_i(and_lo(locked, access.write_lock)), .mubi_o(w));
      Sender u_r(.mubi_i(Mubi::and_lo(locked, access.read_lock)), .mubi_o(r));
      if (On) begin : gen_count
        Bytes u_c(.d_i(count(8'hf1)), .q_o(c));
      end
      initial begin
        #1 $display("w=%h r=%h c=%h", w, r, c);
        access.read_lock = True;
        #1 $display("r=%h", r);
      end
    endmodule
    "#;
    for four_state in [false, true] {
        assert_eq!(
            display(source, four_state),
            ["w=96 r=69 c=05", "r=96"],
            "four-state {four_state}"
        );
    }
}

/// The value of a call is held in a variable of the call's result type,
/// also of a `real` one, under a name the module does not declare, and an
/// instance array's elements share it.
#[test]
fn input_actuals_call_functions_of_real_results_and_instance_arrays() {
    let source = r#"
    `timescale 1ns/1ns
    module Bytes (input logic [7:0] d_i, output logic [7:0] q_o);
      assign q_o = d_i;
    endmodule
    module Reals (input real r_i, output real r_o);
      assign r_o = r_i;
    endmodule
    module Top;
      function automatic logic [7:0] inc(logic [7:0] v);
        logic [7:0] n = v;
        for (int k = 0; k < 1; k++) n++;
        return n;
      endfunction
      function automatic real half(real v);
        real h = v;
        for (int k = 0; k < 1; k++) h = h / 2.0;
        return h;
      endfunction
      logic [7:0] __port_call0_u_b_d_i = 8'h11;
      localparam int __port_call1_u_b_d_i = 7;
      logic [7:0] base = 8'h41, q;
      logic [15:0] qa;
      real x = 3.0, y;
      Bytes u_b(.d_i(inc(base)), .q_o(q));
      Reals u_r(.r_i(half(x)), .r_o(y));
      Bytes u_a[1:0](.d_i(inc(base)), .q_o(qa));
      initial begin
        #1 $display("%h %0d %h %h %f", __port_call0_u_b_d_i, __port_call1_u_b_d_i, q, qa, y);
        base = 8'h50;
        x = 5.0;
        #1 $display("%h %h %f", q, qa, y);
      end
    endmodule
    "#;
    for four_state in [false, true] {
        assert_eq!(
            display(source, four_state),
            ["11 7 42 4242 1.500000", "51 5151 2.500000"],
            "four-state {four_state}"
        );
    }
}

/// A call of a function that returns a `string` is held in a `string`
/// variable; a call of a function with a `ref` formal is not, as it may only
/// be called in a procedural statement (IEEE 1800-2023 13.4.1).
#[test]
fn input_actuals_call_functions_of_string_results_but_not_ref_formals() {
    let source = r#"
    `timescale 1ns/1ns
    module Names (input string n_i, output int len_o);
      assign len_o = n_i.len();
    endmodule
    module Top;
      function automatic string twice(string v);
        string r = v;
        for (int k = 0; k < 1; k++) r = {r, v};
        return r;
      endfunction
      string name = "ab";
      int len;
      Names u_n(.n_i(twice(name)), .len_o(len));
      initial begin
        #1 $display("len=%0d", len);
        name = "abc";
        #1 $display("len=%0d", len);
      end
    endmodule
    "#;
    for four_state in [false, true] {
        assert_eq!(
            display(source, four_state),
            ["len=4", "len=6"],
            "four-state {four_state}"
        );
    }
    let by_reference = r#"
    module Bytes (input logic [7:0] d_i, output logic [7:0] q_o);
      assign q_o = d_i;
    endmodule
    module Top;
      function automatic logic [7:0] peek(ref logic [7:0] v);
        logic [7:0] n = v;
        for (int k = 0; k < 1; k++) n++;
        return n;
      endfunction
      logic [7:0] base = 8'h41, q;
      Bytes u_b(.d_i(peek(base)), .q_o(q));
    endmodule
    "#;
    let error = Simulation::from_sv_sources(vec![(by_reference, Path::new("ref.sv"))], "Top")
        .build()
        .expect_err("a call with a ref formal is not a port actual")
        .to_string()
        .replace(['\n', '│'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        error.contains("can only be called in a procedural statement"),
        "{error}"
    );
}

/// A call inside an operator of an actual is hoisted as one standing alone,
/// and a variable that holds a call's value keeps the declared result type
/// of an unpacked array or a tagged union.
#[test]
fn input_actuals_hoist_nested_calls_and_aggregate_results() {
    let source = r#"
    `timescale 1ns/1ns
    package Types;
      typedef union tagged packed { void Empty; logic [7:0] Full; } item_t;
      typedef logic [7:0] pair_t [2];
    endpackage
    module Bytes (input logic [7:0] d_i, output logic [7:0] q_o);
      assign q_o = d_i;
    endmodule
    module Sum (input Types::pair_t a_i, output logic [7:0] s_o);
      assign s_o = a_i[0] + a_i[1];
    endmodule
    module Items (input Types::item_t item_i, output logic [7:0] v_o);
      assign v_o = item_i.Full;
    endmodule
    module Top import Types::*;;
      function automatic logic [7:0] inc(logic [7:0] v);
        logic [7:0] n = v;
        for (int k = 0; k < 1; k++) n++;
        return n;
      endfunction
      function automatic pair_t pair(logic [7:0] v);
        pair_t p;
        for (int k = 0; k < 2; k++) p[k] = v + 8'(k);
        return p;
      endfunction
      function automatic item_t full(logic [7:0] v);
        item_t item;
        for (int k = 0; k < 1; k++) item = tagged Full v;
        return item;
      endfunction
      logic [7:0] base = 8'h41, q, s, v;
      Bytes u_b(.d_i(inc(base) + 8'h10), .q_o(q));
      Sum u_s(.a_i(pair(base)), .s_o(s));
      Items u_i(.item_i(full(inc(base))), .v_o(v));
      initial begin
        #1 $display("%h %h %h", q, s, v);
        base = 8'h50;
        #1 $display("%h %h %h", q, s, v);
      end
    endmodule
    "#;
    for four_state in [false, true] {
        assert_eq!(
            display(source, four_state),
            ["52 83 42", "61 a1 51"],
            "four-state {four_state}"
        );
    }
}

/// A call the actual evaluates only under a condition, in a branch of a
/// conditional operator or right of `&&` or `||`, is not hoisted: a
/// variable would evaluate it unconditionally (IEEE 1800-2023 11.4.7,
/// 11.4.11). Nor is a call beside an operand that reads a variable, whose
/// changes evaluate the call again (10.3.2). Such an actual stays
/// unsupported.
#[test]
fn input_actuals_keep_calls_evaluated_under_a_condition() {
    for actual in [
        "sel ? inc(base) : 8'h00",
        "8'(sel && inc(base) != 0)",
        "inc(base) + 8'(sel)",
        "inc(base) + inc(8'(sel))",
    ] {
        let source = format!(
            r#"
            module Bytes (input logic [7:0] d_i, output logic [7:0] q_o);
              assign q_o = d_i;
            endmodule
            module Top;
              function automatic logic [7:0] inc(logic [7:0] v);
                logic [7:0] n = v;
                for (int k = 0; k < 1; k++) n++;
                return n;
              endfunction
              logic sel = 1'b0;
              logic [7:0] base = 8'h41, q;
              Bytes u_b(.d_i({actual}), .q_o(q));
            endmodule
            "#
        );
        let error =
            Simulation::from_sv_sources(vec![(source.as_str(), Path::new("guarded.sv"))], "Top")
                .build()
                .expect_err("a guarded call is not hoisted")
                .to_string();
        assert!(error.contains("input port connection"), "{actual}: {error}");
    }
}
