//! Procedural force and release (IEEE 1800-2023 10.6.2). The expected lines
//! are those of Icarus Verilog 13.0 in four-state mode, and of Verilator
//! 5.054 where Icarus evaluates the forced expression only once; an
//! undriven net reads 0 in two-state mode.
use crate::*;

fn display_lines<B: celox::SimBackend>(sim: &mut celox::Simulation<B>) -> Vec<String> {
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

fn simulation(source: &str, four_state: bool) -> celox::SimulatorBuilder<'_, celox::Simulation> {
    celox::Simulation::from_sv_sources(vec![(source, Path::new("forces.sv"))], "top")
        .four_state(four_state)
}

/// The example of IEEE 1800-2023 10.6.2: a force overrides a procedural
/// continuous assignment of a variable and the driver of a net, and the
/// release gives both back to them.
const LRM_EXAMPLE: &str = r#"`timescale 1ns/1ns
module top;
  logic a, b, c, d;
  wire e;
  and and1 (e, a, b, c);
  initial begin
    assign d = a & b & c;
    a = 1; b = 0; c = 1;
    #1 $display("A d=%b e=%b", d, e);
    #9;
    force d = (a | b | c);
    force e = (a | b | c);
    #1 $display("B d=%b e=%b", d, e);
    #9;
    release d;
    release e;
    #1 $display("C d=%b e=%b", d, e);
    $finish;
  end
endmodule"#;

/// A forced flip-flop variable takes the forced value at once, keeps it
/// while forced, also over a nonblocking update of the same time step, and
/// after the release until the flip-flop assigns it again.
const FLIP_FLOPS: &str = r#"`timescale 1ns/1ns
module top;
  logic clk = 0, d = 0, q, qb;
  always @(posedge clk) q <= d;
  always @(posedge clk) qb = d;
  initial begin
    #1 clk = 1; #1 clk = 0;
    #1 $display("A %b %b", q, qb);
    force q = 1'b1; force qb = 1'b1;
    $display("B %b %b", q, qb);
    #1 clk = 1; #1 clk = 0;
    #1 $display("C %b %b", q, qb);
    release q; release qb;
    #1 $display("D %b %b", q, qb);
    #1 clk = 1; #1 clk = 0;
    #1 $display("E %b %b", q, qb);
    // A force in the time step of an edge, after the flip-flop ran.
    d = 1;
    #1 clk = 1; #0 force q = 1'b0; force qb = 1'b0;
    #1 $display("F %b %b", q, qb);
    clk = 0; release q; release qb;
    #1 $display("G %b %b", q, qb);
    $finish;
  end
endmodule"#;

/// A released continuously assigned variable and a released net take the
/// value of their drivers at once; an undriven net is high impedance.
const CONTINUOUS: &str = r#"`timescale 1ns/1ns
module top;
  logic a = 0, b = 0;
  logic v;
  wire w;
  wire u;
  assign v = a;
  assign w = a ^ b;
  initial begin
    #1 $display("A %b %b %b", v, w, u);
    force v = 1'b1; force w = 1'b0; force u = 1'b1;
    a = 1;
    #1 $display("B %b %b %b", v, w, u);
    release v; release w; release u;
    #1 $display("C %b %b %b", v, w, u);
    $finish;
  end
endmodule"#;

/// The forced value follows the expression of the active force, which a
/// later force of the target replaces (4.9.2).
const VARYING: &str = r#"`timescale 1ns/1ns
module top;
  logic [3:0] a = 1, b = 2, c = 7, x;
  initial begin
    force x = a + b;
    a = 4;
    #1 $display("A %0d", x);
    b = 1;
    #1 $display("B %0d", x);
    force x = c;
    a = 0;
    #1 $display("C %0d", x);
    c = 3;
    #1 $display("D %0d", x);
    force x = 4'd9;
    c = 5;
    #1 $display("E %0d", x);
    force x = a + b;
    #1 $display("F %0d", x);
    release x;
    a = 6;
    #1 $display("G %0d", x);
    x = 2;
    #1 $display("H %0d", x);
    $finish;
  end
endmodule"#;

/// A force overrides an active procedural continuous assignment, which
/// drives the variable again at the release.
const OVER_ASSIGN: &str = r#"`timescale 1ns/1ns
module top;
  logic a = 0, v;
  initial begin
    assign v = a;
    #1 $display("A %b", v);
    force v = 1'b1;
    #1 $display("B %b", v);
    a = 1; #1 a = 0;
    #1 $display("C %b", v);
    release v;
    #1 $display("D %b", v);
    a = 1;
    #1 $display("E %b", v);
    $finish;
  end
endmodule"#;

/// Forcing and releasing a clock variable or net gives the edges the
/// procedures waiting on it see.
const CLOCKS: &str = r#"`timescale 1ns/1ns
module top;
  logic clk = 0;
  int n = 0, m = 0;
  wire gclk = clk;
  always @(posedge clk) n++;
  always @(posedge gclk) m++;
  initial begin
    #1 clk = 1; #1 clk = 0;
    #1 $display("A %0d %0d", n, m);
    force clk = 1'b1;
    #1 $display("B %0d %0d", n, m);
    clk = 0; #1 clk = 1;
    #1 $display("C %0d %0d", n, m);
    release clk;
    #1 $display("D %0d %0d %b", n, m, clk);
    clk = 0; #1 clk = 1;
    #1 $display("E %0d %0d", n, m);
    force gclk = 1'b0;
    clk = 0; #1 clk = 1;
    #1 $display("F %0d %0d", n, m);
    release gclk;
    #1 $display("G %0d %0d %b", n, m, gclk);
    $finish;
  end
endmodule"#;

/// A force of a variable or net a child's output drives, and of an input
/// port inside the child, overrides the port connection.
const PORTS: &str = r#"`timescale 1ns/1ns
module child(input logic a, output logic y, output wire z);
  assign y = a;
  assign z = ~a;
  initial begin
    #5 force a = 1'b0;
    #1 $display("child %b %b %b", a, y, z);
    release a;
    #1 $display("child %b %b %b", a, y, z);
  end
endmodule
module top;
  logic a = 1;
  logic y;
  wire z;
  child c(.a(a), .y(y), .z(z));
  initial begin
    #1 $display("A %b %b", y, z);
    force y = 1'b0; force z = 1'b1;
    #1 $display("B %b %b", y, z);
    a = 0;
    #1 $display("C %b %b", y, z);
    release y; release z;
    #1 $display("D %b %b", y, z);
    a = 1;
    #10 $finish;
  end
endmodule"#;

/// Resolved nets, unknown values, forces in an `always` procedure with an
/// event control and in the branches of a fork.
const MIXED: &str = r#"`timescale 1ns/1ns
module top;
  logic a = 1, b = 0, sel = 0;
  wand wa;
  wor wo;
  assign wa = a; assign wa = b;
  assign wo = a; assign wo = b;
  logic [3:0] v;
  bit [3:0] t;
  logic [3:0] p;
  always @(sel) if (sel) force p = 4'd5; else release p;
  initial begin
    v = 4'd1; t = 4'd2; p = 4'd3;
    #1 $display("A %b %b %b %b %0d", wa, wo, v, t, p);
    force wa = 1'b1; force wo = 1'b0; force v = 4'bx1z0; force t = 4'd9;
    sel = 1;
    #1 $display("B %b %b %b %b %0d", wa, wo, v, t, p);
    fork
      begin #1 force v = 4'b0011; end
      begin #2 release v; end
    join
    #1 $display("C %b %b %b %b %0d", wa, wo, v, t, p);
    release wa; release wo; release t; sel = 0;
    #1 $display("D %b %b %b %b %0d", wa, wo, v, t, p);
    p = 4'd7;
    #1 $display("E %0d", p);
    $finish;
  end
endmodule"#;

/// A delayed continuous assignment and a net delay drive the target with
/// the delayed value again after the release.
const DELAYED: &str = r#"`timescale 1ns/1ns
module top;
  logic a = 0, v;
  wire #2 w = a;
  assign #1 v = a;
  initial begin
    #5 $display("A %b %b", v, w);
    force v = 1'b1; force w = 1'b1;
    #1 $display("B %b %b", v, w);
    a = 1;
    #5 $display("C %b %b", v, w);
    release v; release w;
    #0 $display("D %b %b", v, w);
    a = 0;
    #3 $display("F %b %b", v, w);
    $finish;
  end
endmodule"#;

/// Forces of the first time step, before any process waits.
const TIME_ZERO: &str = r#"`timescale 1ns/1ns
module top;
  logic v = 0;
  logic [3:0] a = 4'd3, x;
  wire w;
  logic c;
  assign c = a[0];
  initial force v = 1'b1;
  initial force w = 1'b1;
  initial force x = a;
  initial force c = 1'b0;
  initial begin
    #1 $display("A %b %b %0d %b", v, w, x, c);
    a = 4'd6;
    #1 $display("B %b %b %0d %b", v, w, x, c);
    $finish;
  end
endmodule"#;

sv_backends! {
    fn delayed_drivers_drive_again_after_the_release(sim) {
        @setup {}
        @build simulation(DELAYED, false);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 0 0", "B 1 1", "C 1 1", "D 1 1", "F 0 0"]);
    }
    fn delayed_drivers_drive_again_after_the_release_four_state(sim) {
        @setup {}
        @build simulation(DELAYED, true);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 0 0", "B 1 1", "C 1 1", "D 1 1", "F 0 0"]);
    }
    fn forces_at_time_zero(sim) {
        @setup {}
        @build simulation(TIME_ZERO, false);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 1 1 3 0", "B 1 1 6 0"]);
    }
    fn forces_at_time_zero_four_state(sim) {
        @setup {}
        @build simulation(TIME_ZERO, true);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 1 1 3 0", "B 1 1 6 0"]);
    }
    fn lrm_example_forces_and_releases(sim) {
        @setup {}
        @build simulation(LRM_EXAMPLE, false);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A d=0 e=0", "B d=1 e=1", "C d=0 e=0"]);
    }
    fn lrm_example_forces_and_releases_four_state(sim) {
        @setup {}
        @build simulation(LRM_EXAMPLE, true);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A d=0 e=0", "B d=1 e=1", "C d=0 e=0"]);
    }
    fn forced_flip_flops_hold_until_assigned(sim) {
        @setup {}
        @build simulation(FLIP_FLOPS, false);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 0 0", "B 1 1", "C 1 1", "D 1 1", "E 0 0", "F 0 0", "G 0 0"]);
    }
    fn forced_flip_flops_hold_until_assigned_four_state(sim) {
        @setup {}
        @build simulation(FLIP_FLOPS, true);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 0 0", "B 1 1", "C 1 1", "D 1 1", "E 0 0", "F 0 0", "G 0 0"]);
    }
    fn released_nets_take_their_drivers(sim) {
        @setup {}
        @build simulation(CONTINUOUS, false);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 0 0 0", "B 1 0 1", "C 1 1 0"]);
    }
    fn released_nets_take_their_drivers_four_state(sim) {
        @setup {}
        @build simulation(CONTINUOUS, true);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 0 0 z", "B 1 0 1", "C 1 1 z"]);
    }
    fn forced_value_follows_the_active_expression(sim) {
        @setup {}
        @build simulation(VARYING, false);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 6", "B 5", "C 7", "D 3", "E 9", "F 1", "G 1", "H 2"]);
    }
    fn forced_value_follows_the_active_expression_four_state(sim) {
        @setup {}
        @build simulation(VARYING, true);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 6", "B 5", "C 7", "D 3", "E 9", "F 1", "G 1", "H 2"]);
    }
    fn force_overrides_a_procedural_continuous_assignment(sim) {
        @setup {}
        @build simulation(OVER_ASSIGN, false);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 0", "B 1", "C 1", "D 0", "E 1"]);
    }
    fn force_overrides_a_procedural_continuous_assignment_four_state(sim) {
        @setup {}
        @build simulation(OVER_ASSIGN, true);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 0", "B 1", "C 1", "D 0", "E 1"]);
    }
    fn forced_clocks_give_edges(sim) {
        @setup {}
        @build simulation(CLOCKS, false);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 1 1", "B 2 2", "C 2 2", "D 2 2 1", "E 3 3", "F 4 3", "G 4 4 1"]);
    }
    fn forced_clocks_give_edges_four_state(sim) {
        @setup {}
        @build simulation(CLOCKS, true);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 1 1", "B 2 2", "C 2 2", "D 2 2 1", "E 3 3", "F 4 3", "G 4 4 1"]);
    }
    fn force_overrides_port_connections(sim) {
        @setup {}
        @build simulation(PORTS, false);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 1 0", "B 0 1", "C 0 1", "D 0 1", "child 0 0 1", "child 1 1 0"]);
    }
    fn force_overrides_port_connections_four_state(sim) {
        @setup {}
        @build simulation(PORTS, true);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 1 0", "B 0 1", "C 0 1", "D 0 1", "child 0 0 1", "child 1 1 0"]);
    }
    fn forces_of_resolved_nets_unknowns_and_forks(sim) {
        @setup {}
        @build simulation(MIXED, false);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 0 1 0001 0010 3", "B 1 0 0100 1001 5", "C 1 0 0011 1001 5", "D 0 1 0011 1001 5", "E 7"]);
    }
    fn forces_of_resolved_nets_unknowns_and_forks_four_state(sim) {
        @setup {}
        @build simulation(MIXED, true);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 0 1 0001 0010 3", "B 1 0 x1z0 1001 5", "C 1 0 0011 1001 5", "D 0 1 0011 1001 5", "E 7"]);
    }
}

/// [`CLOCKS`] on the interpreter and on the tiered backend, whose derived
/// clocks follow the forced net as the native image's do (see #3755).
#[test]
fn forced_clocks_give_edges_on_interpreter_and_tiered() {
    let expected = [
        "A 1 1", "B 2 2", "C 2 2", "D 2 2 1", "E 3 3", "F 4 3", "G 4 4 1",
    ];
    for four_state in [false, true] {
        let mut sim = simulation(CLOCKS, four_state).build_interpreter().unwrap();
        sim.run_until(100).unwrap();
        assert_eq!(
            display_lines(&mut sim),
            expected,
            "interpreter four_state={four_state}"
        );
        let mut sim = simulation(CLOCKS, four_state).build_tiered().unwrap();
        sim.run_until(100).unwrap();
        assert_eq!(
            display_lines(&mut sim),
            expected,
            "tiered four_state={four_state}"
        );
    }
}

#[test]
fn rejected_forces_name_their_reason() {
    for (body, expected) in [
        (
            "logic [3:0] v; initial force v[0] = 1'b1;",
            "force or release of a bit-select or part-select",
        ),
        (
            "logic a, b; initial force {a, b} = 2'b01;",
            "force or release of a concatenation",
        ),
        (
            "typedef struct packed { logic a; logic b; } s_t; s_t s; initial force s.a = 1'b1;",
            "of a member or an element",
        ),
        (
            "logic v, a; always_comb v = a; initial force v = 1'b1;",
            "which an `always_comb` procedure assigns",
        ),
        (
            "logic v, a; always @* v = a; initial release v;",
            "which an `always @*` procedure assigns",
        ),
        (
            "logic q, d, c; always @(posedge c) q <= #1 d; initial force q = 1'b1;",
            "intra-assignment delay",
        ),
        (
            "logic v; task t; force v = 1'b1; endtask initial t;",
            "force or release in a subroutine",
        ),
        (
            "logic v, c; always_ff @(posedge c) force v = 1'b1;",
            "force or release in a subroutine",
        ),
        (
            "real r; initial force r = 1.0;",
            "not of a packed integral type",
        ),
        (
            "logic v, a; function logic f(logic x); return x; endfunction \
             initial force v = f(a);",
            "force of an expression that calls",
        ),
        (
            "logic v; initial begin logic l; force v = l; end",
            "reads a variable of a procedural block",
        ),
        (
            "logic v; initial force v = 1'b1, v = 1'b0;",
            "a force assigns one variable or net",
        ),
    ] {
        let source = format!("module top; {body} endmodule");
        let error = match simulation(&source, false).build() {
            Ok(_) => panic!("{body}: accepted"),
            Err(error) => error.to_string(),
        };
        // The report wraps long lines behind a `│` gutter.
        let error = error
            .split(|c: char| c.is_whitespace() || c == '│')
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(error.contains(expected), "{body}: {error}");
    }
}

/// A name of another instance is not a target yet.
#[test]
fn force_of_another_instance_is_rejected() {
    let source = "module child; logic v; endmodule \
                  module top; child c(); initial force c.v = 1'b1; endmodule";
    let error = match simulation(source, false).build() {
        Ok(_) => panic!("accepted"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("another instance"), "{error}");
}
