//! Procedural continuous assignments (IEEE 1800-2023 10.6.1).
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
    celox::Simulation::from_sv_sources(vec![(source, Path::new("assigns.sv"))], "top")
        .four_state(four_state)
}

/// The assigned variable follows the expression, the assignment overrides
/// procedural assignments to the variable, and a `deassign` leaves the
/// variable its value until the next procedural assignment.
const FOLLOWS: &str = r#"`timescale 1ns/1ns
module top;
  int arg;
  logic en;
  logic clk = 0;
  always @(posedge clk) en <= 1'b0;
  initial begin
    arg = 0;
    assign en = (arg == 1);
    #1 $display("A %b", en);
    arg = 1;
    #1 $display("B %b", en);
    en = 1'b0;
    #1 $display("C %b", en);
    arg = 0;
    #1 $display("D %b", en);
    deassign en;
    arg = 1;
    #1 $display("E %b", en);
    en = 1'b1;
    #1 $display("F %b", en);
    // A nonblocking update scheduled before the assignment executes lands
    // after it, and does not change the variable.
    clk = 1;
    #0 assign en = (arg == 1);
    #1 $display("G %b", en);
    $finish;
  end
endmodule"#;

/// A second `assign` of a variable replaces the first, and the variable of
/// a flip-flop is clocked again after the `deassign`.
const FLIP_FLOP: &str = r#"`timescale 1ns/1ns
module top;
  logic q, d, clock, src;
  always @(posedge clock) q <= d;
  initial begin
    clock = 0; d = 1; src = 1;
    #10 assign q = 0;
    #1 clock = 1; #1 clock = 0;
    #1 $display("A %b", q);
    assign q = src;
    #1 d = 0; clock = 1; #1 clock = 0;
    #1 $display("B %b", q);
    src = 0;
    #1 $display("C %b", q);
    src = 1;
    #1 $display("D %b", q);
    deassign q;
    #1 $display("E %b", q);
    #1 clock = 1; #1 clock = 0;
    #1 $display("F %b", q);
    assign q = src;
    #1 $display("G %b", q);
    $finish;
  end
endmodule"#;

/// The statements in an `always` procedure with an event control, which
/// then runs as a process, in each generate block with its own variable.
const LEVEL_SENSITIVE: &str = r#"`timescale 1ns/1ns
module top;
  logic [1:0] clear, preset;
  logic clock, d;
  int show = 0;
  for (genvar i = 0; i < 2; i++) begin : g
    logic [3:0] q;
    always @(clear[i] or preset[i])
      if (!clear[i]) assign q = 4'd0;
      else if (!preset[i]) assign q = 4'd15;
      else deassign q;
    initial forever @(posedge clock) q = {4{d}} ^ 4'(i);
    always @(show) $display("%0d: %0d %0d", show, i, q);
  end
  initial begin
    clock = 0; d = 1; clear = 2'b11; preset = 2'b11;
    #1 clock = 1; #1 clock = 0;
    #1 show = 1;
    #1 clear = 2'b01;
    #1 clock = 1; #1 clock = 0;
    #1 show = 2;
    #1 clear = 2'b11; preset = 2'b10;
    #1 clock = 1; #1 clock = 0;
    #1 show = 3;
    #1 preset = 2'b11; d = 0;
    #1 show = 4;
    #1 clock = 1; #1 clock = 0;
    #1 show = 5;
    #1 $finish;
  end
endmodule"#;

/// The lines of a run of [`LEVEL_SENSITIVE`] from time 1 on, sorted: the
/// blocks print in either order, and the two-state settle at time zero
/// wakes the printing procedures once more.
fn level_sensitive_lines<B: celox::SimBackend>(sim: &mut celox::Simulation<B>) -> Vec<String> {
    let mut lines = display_lines(sim);
    lines.retain(|line| !line.starts_with("0:"));
    lines.sort();
    lines
}

/// The lines of [`LEVEL_SENSITIVE`].
const LEVEL_SENSITIVE_LINES: [&str; 10] = [
    "1: 0 15", "1: 1 14", "2: 0 15", "2: 1 0", "3: 0 15", "3: 1 14", "4: 0 15", "4: 1 14",
    "5: 0 0", "5: 1 1",
];

sv_backends! {
    fn assigned_variable_follows_its_expression(sim) {
        @setup {}
        @build simulation(FOLLOWS, false);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 0", "B 1", "C 1", "D 0", "E 0", "F 1", "G 1"]);
    }
    fn assigned_four_state_variable_follows_its_expression(sim) {
        @setup {}
        @build simulation(FOLLOWS, true);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 0", "B 1", "C 1", "D 0", "E 0", "F 1", "G 1"]);
    }
    fn assign_overrides_a_flip_flop_until_deassign(sim) {
        @setup {}
        @build simulation(FLIP_FLOP, false);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 0", "B 1", "C 0", "D 1", "E 1", "F 0", "G 1"]);
    }
    fn assign_overrides_a_four_state_flip_flop_until_deassign(sim) {
        @setup {}
        @build simulation(FLIP_FLOP, true);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["A 0", "B 1", "C 0", "D 1", "E 1", "F 0", "G 1"]);
    }
    fn assign_in_level_sensitive_always_of_generate_blocks(sim) {
        @setup {}
        @build simulation(LEVEL_SENSITIVE, false);
        sim.run_until(100).unwrap();
        assert_eq!(level_sensitive_lines(&mut sim), LEVEL_SENSITIVE_LINES);
    }
    fn assign_in_level_sensitive_always_of_four_state_generate_blocks(sim) {
        @setup {}
        @build simulation(LEVEL_SENSITIVE, true);
        sim.run_until(100).unwrap();
        assert_eq!(level_sensitive_lines(&mut sim), LEVEL_SENSITIVE_LINES);
    }
}

/// A `deassign` without an `assign` of its variable ends nothing: it does
/// not make the variable the target of a procedural continuous assignment,
/// so another procedure may write it, and the statement itself is dropped.
#[test]
fn deassign_without_assign_leaves_the_variable_to_its_drivers() {
    let source = r#"`timescale 1ns/1ns
module top;
  logic q, d;
  always_comb q = d;
  initial begin
    deassign q;
    d = 1'b1;
    #1 $display("q=%b", q);
    $finish;
  end
endmodule"#;
    let mut sim = simulation(source, false).build().unwrap();
    sim.run_until(100).unwrap();
    assert_eq!(display_lines(&mut sim), ["q=1"]);
}

#[test]
fn rejected_procedural_continuous_assignments_name_their_reason() {
    for (body, expected) in [
        (
            "logic [3:0] v; initial assign v[0] = 1'b1;",
            "assigns a whole variable",
        ),
        ("wire w; initial assign w = 1'b1;", "net `w`"),
        (
            "logic v, a; always_comb assign v = a;",
            "`always_comb`, `always_latch` or `always_ff` procedure",
        ),
        (
            "logic v; function void f(); assign v = 1'b1; endfunction initial f();",
            "in a subroutine",
        ),
        (
            "logic v, r, d; task automatic t(); fork assign r = d; join_none endtask \
             initial begin assign v = 1'b1; t(); end",
            "in a subroutine",
        ),
        (
            "logic v; initial begin automatic logic a = 1; assign v = a; end",
            "reads a variable of a procedural block",
        ),
        (
            "initial begin static logic v; assign v = 1'b1; end",
            "to a variable of a procedural block",
        ),
        (
            "logic v, a; assign v = a; initial assign v = 1'b1;",
            "which a continuous assignment",
        ),
        (
            "logic v, a; function void set(); v = a; endfunction always_comb set(); \
             initial assign v = 1'b1;",
            "which a continuous assignment",
        ),
        (
            "logic v, a; task pass(output logic o); o = a; endtask \
             always_comb pass(v); initial assign v = 1'b1;",
            "which a continuous assignment",
        ),
        (
            "logic v, a; task bump(inout logic o); o = a; endtask \
             task wrap(output logic o); bump(o); endtask \
             always_comb wrap(v); initial assign v = 1'b1;",
            "which a continuous assignment",
        ),
        (
            "logic v, r, a; task pass(output logic [1:0] o); o = {a, a}; endtask \
             always_comb pass({v, r}); initial assign v = 1'b1;",
            "which a continuous assignment",
        ),
        (
            "logic v; integer n; always_comb n = $sscanf(\"1\", \"%d\", v); \
             initial assign v = 1'b1;",
            "which a continuous assignment",
        ),
        (
            "logic v; integer n; function void scan(); n = $sscanf(\"1\", \"%d\", v); \
             endfunction always_comb scan(); initial assign v = 1'b1;",
            "which a continuous assignment",
        ),
        (
            "logic v, a; function logic twice(logic x); return x; endfunction \
             initial assign v = twice(a);",
            "procedural continuous assignment of an expression",
        ),
        (
            "logic a, b; initial assign {a, b} = 2'b01;",
            "to a concatenation",
        ),
        (
            "real r; initial assign r = 1.0;",
            "packed integral variable",
        ),
        // A `deassign` without an `assign` names a valid target too.
        ("wire w; initial deassign w;", "net `w`"),
        ("real r; initial deassign r;", "packed integral variable"),
        (
            "logic v; initial force v = 1'b1;",
            "`force` or `release` statement",
        ),
        (
            "logic v; initial release v;",
            "`force` or `release` statement",
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

/// A package task that writes its `output` actual from an `always_comb` is
/// that procedure's write of the actual (9.2.2.2.1), also through an import.
#[test]
fn imported_task_outputs_are_writes_of_the_calling_procedure() {
    for call in ["p::pass(v)", "pass(v)"] {
        let source = format!(
            "package p; task automatic pass(output logic o); o = 1'b0; endtask endpackage \
             module top; import p::*; logic v; always_comb {call}; \
             initial assign v = 1'b1; endmodule"
        );
        let error = match simulation(&source, false).build() {
            Ok(_) => panic!("{call}: accepted"),
            Err(error) => error.to_string(),
        };
        let error = error
            .split(|c: char| c.is_whitespace() || c == '│')
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            error.contains("which a continuous assignment"),
            "{call}: {error}"
        );
    }
}

#[test]
fn virtual_interface_handles_are_not_procedural_assign_targets() {
    let source = "interface bus_if; logic data; endinterface \
                  module top; virtual bus_if vif1, vif2; \
                  initial assign vif1 = vif2; endmodule";
    let error = match simulation(source, false).build() {
        Ok(_) => panic!("virtual interface target: accepted"),
        Err(error) => error.to_string(),
    };
    let error = error
        .split(|c: char| c.is_whitespace() || c == '│')
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        error.contains("packed integral variable"),
        "virtual interface target: {error}"
    );
}
