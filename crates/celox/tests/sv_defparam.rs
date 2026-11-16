//! A `defparam` statement assigns a parameter of an instance by its
//! hierarchical name (IEEE 1800-2023 23.10.1). Its value is evaluated in the
//! scope of the statement, it takes precedence over a parameter assignment
//! of the instantiation (23.10), and the last one in the source text gives
//! the value. The instance is one of the module that has the statement.
use celox::Simulation;
use std::path::Path;

/// Runs `source`, whose `initial` blocks print lines, and returns them.
fn display(source: &str, four_state: bool) -> Vec<String> {
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("defparam.sv"))], "Top")
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
    Simulation::from_sv_sources(vec![(source, Path::new("defparam.sv"))], "Top")
        .build()
        .expect_err("the source is rejected")
        .to_string()
        // The report wraps its lines.
        .replace(['\n', '│'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

const CHILD: &str = r#"
    `timescale 1ns/1ns
    module Child #(parameter W = 1, parameter V = 7)(
      output [W-1:0] y, output [31:0] w, output [31:0] v);
      assign y = '1;
      assign w = W;
      assign v = V;
    endmodule
"#;

#[test]
fn assigns_parameters_of_instances() {
    let source = format!(
        r#"{CHILD}
    module Top;
      localparam LP = 3;
      wire [3:0] y_a; wire [31:0] w_a, v_a;
      wire [5:0] y_b; wire [31:0] w_b, v_b;
      wire [1:0] y_c; wire [31:0] w_c, v_c;
      wire [31:0] w_g [2]; wire [31:0] v_g [2];
      wire [31:0] w_d, v_d;

      // Before its instance, with a value of this module's parameters.
      defparam a.W = LP + 1;
      Child a(.y(y_a), .w(w_a), .v(v_a));

      // Over the assignment of the instantiation; `V` keeps its own.
      Child #(.W(2), .V(9)) b(.y(y_b), .w(w_b), .v(v_b));
      defparam b.W = 6;

      // A list, and the last assignment in the source text.
      Child c(.y(y_c), .w(w_c), .v(v_c));
      defparam c.W = 5, c.V = 11;
      defparam c.W = 2;

      // In a generate block, to an instance of the block, with its genvar;
      // and from the module into one block.
      for (genvar i = 0; i < 2; i++) begin : g
        Child u(.y(), .w(w_g[i]), .v(v_g[i]));
        defparam u.V = 20 + i;
      end
      defparam g[1].u.W = 5;

      // A name that starts at the module, and an ordered assignment.
      Child #(3, 4) d(.y(), .w(w_d), .v(v_d));
      defparam Top.d.V = 12;

      initial begin
        #1;
        $display("a: y=%b w=%0d v=%0d", y_a, w_a, v_a);
        $display("b: y=%b w=%0d v=%0d", y_b, w_b, v_b);
        $display("c: y=%b w=%0d v=%0d", y_c, w_c, v_c);
        $display("g0: w=%0d v=%0d", w_g[0], v_g[0]);
        $display("g1: w=%0d v=%0d", w_g[1], v_g[1]);
        $display("d: w=%0d v=%0d", w_d, v_d);
      end
    endmodule
    "#
    );
    for four_state in [false, true] {
        assert_eq!(
            display(&source, four_state),
            [
                "a: y=1111 w=4 v=7",
                "b: y=111111 w=6 v=9",
                "c: y=11 w=2 v=11",
                "g0: w=1 v=20",
                "g1: w=5 v=21",
                "d: w=3 v=12",
            ],
            "four-state {four_state}"
        );
    }
}

/// An escaped generate label may contain `[` (5.6.1); it is not a
/// generate index, so a `defparam` through it still finds its instance.
#[test]
fn assigns_through_escaped_generate_label() {
    let source = format!(
        r#"{CHILD}
    module Top;
      wire [31:0] w_e, v_e;
      wire [31:0] w_h [2];
      if (1) begin : \g[foo
        Child u(.y(), .w(w_e), .v(v_e));
      end
      defparam \g[foo .u.W = 5;
      // The same label in a loop, where a real index follows the name.
      for (genvar i = 0; i < 2; i++) begin : \h[x
        Child u(.y(), .w(w_h[i]), .v());
      end
      defparam \h[x [1].u.W = 6;
      initial begin
        #1;
        $display("e: w=%0d v=%0d", w_e, v_e);
        $display("h: w=%0d,%0d", w_h[0], w_h[1]);
      end
    endmodule
    "#
    );
    assert_eq!(display(&source, false), ["e: w=5 v=7", "h: w=1,6"]);
}

/// The target is a parameter of the instance that can be overridden; a
/// `defparam` in a generate block stays in the block (23.10.1), and does
/// not assign a type (6.20.3).
#[test]
fn rejects_targets_a_defparam_cannot_assign() {
    for (expected, body) in [
        ("X", "Child a(); defparam a.X = 1;"),
        ("names no module instance", "defparam missing.W = 1;"),
        (
            "outside the generate block",
            "Child a(); if (1) begin : g defparam a.W = 2; end",
        ),
        (
            "parameter of its own scope",
            "localparam P = 1; defparam P = 2;",
        ),
        ("`$root`", "Child a(); defparam $root.Top.a.W = 2;"),
        ("instance array", "Child a[2](); defparam a.W = 2;"),
        ("instance array", "Child a[2](); defparam a[0].W = 2;"),
        (
            "names no module instance",
            "Child a(); defparam a[0].W = 2;",
        ),
        ("below a child instance", "Outer o(); defparam o.c.W = 2;"),
        ("data type", "Typed t(); defparam t.T = logic [3:0];"),
    ] {
        let source = format!(
            r#"{CHILD}
            module Outer; Child c(); endmodule
            module Typed #(parameter type T = logic)(); T value; endmodule
            module Top; {body} endmodule
            "#
        );
        let error = rejection(&source);
        assert!(error.contains(expected), "{body}: {error}");
    }
    // A local parameter is not assigned (6.20.4).
    let local = rejection(
        r#"
        module Child; localparam L = 1; endmodule
        module Top; Child a(); defparam a.L = 2; endmodule
        "#,
    );
    assert!(local.contains('L'), "{local}");
}
