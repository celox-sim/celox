//! The assertion control tasks `$asserton` and `$assertoff` (IEEE 1800-2023
//! 20.11).
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

/// `$assertoff` in the top module stops the immediate, deferred and
/// concurrent assertions of another module until `$asserton`.
const CONTROL: &str = r#"`timescale 1ns/1ns
module child(input logic clk, input logic [3:0] v);
  always_ff @(posedge clk) assert (v != 4'd3) else $display("immediate %0d", v);
  always_comb begin
    assert final (v != 4'd5) else $display("final %0d", v);
  end
  assert property (@(posedge clk) v != 4'd7) else $display("property %0d", v);
endmodule
module top;
  logic clk = 0;
  logic [3:0] v = 0;
  child c (.*);
  task automatic tick();
    #1 clk = 1;
    #1 clk = 0;
  endtask
  initial begin
    #1 v = 3;
    tick();
    $assertoff;
    #1 v = 0;
    #1 v = 3;
    tick();
    #1 v = 5;
    #1 v = 7;
    tick();
    $asserton;
    tick();
    #1 v = 5;
    #1 v = 3;
    tick();
    #1 $finish;
  end
endmodule"#;

sv_backends! {
    fn assertoff_stops_every_assertion_until_asserton(sim) {
        @setup {}
        @build celox::Simulation::from_sv_sources(vec![(CONTROL, Path::new("control.sv"))], "top");
        sim.run_until(100).unwrap();
        assert_eq!(
            display_lines(&mut sim),
            ["immediate 3", "property 7", "final 5", "immediate 3"]
        );
    }
    fn assertoff_stops_every_four_state_assertion_until_asserton(sim) {
        @setup {}
        @build celox::Simulation::from_sv_sources(vec![(CONTROL, Path::new("control.sv"))], "top")
            .four_state(true);
        sim.run_until(100).unwrap();
        assert_eq!(
            display_lines(&mut sim),
            ["immediate 3", "property 7", "final 5", "immediate 3"]
        );
    }
}

/// `$asserton` writes no variable an `always_comb` reads, so it does not
/// rerun one (IEEE 1800-2023 9.2.2.2.1, 20.11): an assertion that failed
/// while assertions were off reports only when its process runs again for
/// a change of its inputs. `v` is checked by a symbolic process, `w` by one
/// that calls a task and so runs as a guarded unit.
const RESTART: &str = r#"`timescale 1ns/1ns
module top;
  logic [3:0] v = 0;
  logic [3:0] w = 0;
  task automatic report(input logic [3:0] x);
    $display("w %0d", x);
  endtask
  always_comb assert (v != 4'd5) else $display("v %0d", v);
  always_comb assert (w != 4'd5) else report(w);
  initial begin
    $assertoff;
    #1 v = 5;
    #1 w = 5;
    #1 $asserton;
    #1 v = 6;
    #1 v = 5;
    #1 w = 6;
    #1 w = 5;
    #1 $finish;
  end
endmodule"#;

sv_backends! {
    fn asserton_does_not_rerun_always_comb(sim) {
        @setup {}
        @build celox::Simulation::from_sv_sources(vec![(RESTART, Path::new("restart.sv"))], "top");
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["v 5", "w 5"]);
    }
    fn asserton_does_not_rerun_four_state_always_comb(sim) {
        @setup {}
        @build celox::Simulation::from_sv_sources(vec![(RESTART, Path::new("restart.sv"))], "top")
            .four_state(true);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["v 5", "w 5"]);
    }
}

/// `$assertoff` stops the attempts of concurrent assertions of sequences of
/// one tick, a `##0` chain and one behind an implication, until `$asserton`
/// (20.11).
const SINGLE_TICK: &str = r#"`timescale 1ns/1ns
module top;
  logic clk = 0;
  logic [3:0] v = 0;
  assert property (@(posedge clk) v != 4'd7 ##0 v != 4'd6) else $display("chain %0d", v);
  assert property (@(posedge clk) v == 4'd2 ##0 v[1] |-> v != 4'd2) else $display("implication %0d", v);
  task automatic tick();
    #1 clk = 1;
    #1 clk = 0;
  endtask
  initial begin
    #1 v = 7;
    tick();
    $assertoff;
    #1 v = 6;
    tick();
    #1 v = 2;
    tick();
    $asserton;
    tick();
    #1 v = 6;
    tick();
    #1 $finish;
  end
endmodule"#;

sv_backends! {
    fn assertoff_stops_single_tick_sequences_until_asserton(sim) {
        @setup {}
        @build celox::Simulation::from_sv_sources(vec![(SINGLE_TICK, Path::new("single_tick.sv"))], "top");
        sim.run_until(100).unwrap();
        assert_eq!(
            display_lines(&mut sim),
            ["chain 7", "implication 2", "chain 6"]
        );
    }
    fn assertoff_stops_four_state_single_tick_sequences_until_asserton(sim) {
        @setup {}
        @build celox::Simulation::from_sv_sources(vec![(SINGLE_TICK, Path::new("single_tick.sv"))], "top")
            .four_state(true);
        sim.run_until(100).unwrap();
        assert_eq!(
            display_lines(&mut sim),
            ["chain 7", "implication 2", "chain 6"]
        );
    }
}

/// The control tasks set simulator state, not a variable of the design, so
/// any processes may call them (IEEE 1800-2023 20.11): `always_ff` processes
/// of two instances and an `initial` block, which no driver check rejects.
const PROCESSES: &str = r#"`timescale 1ns/1ns
module ctl(input logic clk, input logic off, input logic on);
  always_ff @(posedge clk) if (off) $assertoff;
  always_ff @(posedge clk) if (on) $asserton;
endmodule
module top;
  logic clk = 0;
  logic off = 0;
  logic on = 0;
  logic [3:0] v = 0;
  int n = 0;
  ctl c0 (.clk, .off, .on(1'b0));
  ctl c1 (.clk, .off(1'b0), .on);
  always @(negedge clk) assert (v != 4'd3) else $display("tick %0d", n);
  task automatic tick();
    n = n + 1;
    #1 clk = 1;
    #1 clk = 0;
    #1;
  endtask
  initial begin
    v = 3;
    tick();
    off = 1;
    tick();
    off = 0;
    tick();
    $asserton;
    tick();
    $assertoff;
    on = 1;
    tick();
    on = 0;
    #1 $finish;
  end
endmodule"#;

sv_backends! {
    fn assertion_control_tasks_of_any_processes(sim) {
        @setup {}
        @build celox::Simulation::from_sv_sources(vec![(PROCESSES, Path::new("processes.sv"))], "top");
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["tick 1", "tick 4", "tick 5"]);
    }
}

#[test]
fn unsupported_assertion_control_names_its_form() {
    for (body, expected) in [
        (
            "initial $assertoff(0);",
            "`$assertoff` with a level or a list of scopes or assertions",
        ),
        (
            "initial $asserton(1, top);",
            "`$asserton` with a level or a list of scopes or assertions",
        ),
        ("initial $assertkill;", "system task `$assertkill`"),
    ] {
        let source = format!("module top; {body} endmodule");
        let error =
            celox::Simulation::from_sv_sources(vec![(&source, Path::new("control.sv"))], "top")
                .build()
                .err()
                .unwrap_or_else(|| panic!("{body}: accepted"))
                .to_string();
        let error = error
            .split(|c: char| c.is_whitespace() || c == '│')
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(error.contains(expected), "{body}: {error}");
    }
}

/// `$assertoff` in a generate block that elaboration does not select
/// (IEEE 1800-2023 27.5) controls nothing: a concurrent assertion of a
/// sequence over cycles checks every attempt.
const INACTIVE_CONTROL: &str = r#"`timescale 1ns/1ns
module top;
  localparam bit CONTROL = 0;
  logic clk = 0;
  logic a = 0, b = 1;
  if (CONTROL) begin : g
    initial $assertoff;
  end
  assert property (@(posedge clk) a |=> b) else $display("fail %0d", $time);
  task automatic tick();
    #1 clk = 1;
    #1 clk = 0;
  endtask
  initial begin
    a = 1;
    tick();
    b = 0;
    a = 0;
    tick();
    tick();
    #1 $finish;
  end
endmodule"#;

/// `$assertoff` starts no attempt of a sequence over cycles until
/// `$asserton`, but an attempt that started before it goes on to its fail
/// action (20.11).
const SEQUENCE_CONTROL: &str = r#"`timescale 1ns/1ns
module top;
  logic clk = 0;
  logic a = 0, b = 1;
  assert property (@(posedge clk) a |=> b) else $display("fail %0d", $time);
  task automatic tick();
    #1 clk = 1;
    #1 clk = 0;
  endtask
  initial begin
    a = 1;
    tick();
    $assertoff;
    b = 0;
    tick();
    tick();
    $asserton;
    tick();
    a = 0;
    tick();
    #1 $finish;
  end
endmodule"#;

/// An action's `$assertoff` runs in the Reactive region, after the attempt
/// of the same tick started in the Observed region (4.4.2.5): that attempt
/// goes on to its fail action (20.11).
const ACTION_CONTROL: &str = r#"`timescale 1ns/1ns
module top;
  logic clk = 0;
  logic a = 1, b = 1;
  assert property (@(posedge clk) a |=> b)
    else begin $display("fail %0d", $time); $assertoff; end
  task automatic tick();
    #1 clk = 1;
    #1 clk = 0;
  endtask
  initial begin
    tick();
    b = 0;
    tick();
    tick();
    tick();
    #1 $finish;
  end
endmodule"#;

sv_backends! {
    fn assertoff_in_an_unselected_generate_block_controls_nothing(sim) {
        @setup {}
        @build celox::Simulation::from_sv_sources(
            vec![(INACTIVE_CONTROL, Path::new("inactive_control.sv"))],
            "top",
        );
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["fail 3"]);
    }
    fn assertoff_starts_no_sequence_attempt_but_lets_started_ones_finish(sim) {
        @setup {}
        @build celox::Simulation::from_sv_sources(
            vec![(SEQUENCE_CONTROL, Path::new("sequence_control.sv"))],
            "top",
        );
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["fail 3", "fail 9"]);
    }
    fn assertoff_starts_no_four_state_sequence_attempt_but_lets_started_ones_finish(sim) {
        @setup {}
        @build celox::Simulation::from_sv_sources(
            vec![(SEQUENCE_CONTROL, Path::new("sequence_control.sv"))],
            "top",
        )
        .four_state(true);
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["fail 3", "fail 9"]);
    }
    fn assertoff_in_an_action_lets_the_attempt_of_its_tick_finish(sim) {
        @setup {}
        @build celox::Simulation::from_sv_sources(
            vec![(ACTION_CONTROL, Path::new("action_control.sv"))],
            "top",
        );
        sim.run_until(100).unwrap();
        assert_eq!(display_lines(&mut sim), ["fail 3", "fail 5"]);
    }
}
