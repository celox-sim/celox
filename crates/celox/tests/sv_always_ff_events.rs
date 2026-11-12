//! The event control of an `always_ff` need not name an edge (IEEE
//! 1800-2023 9.2.2.4): `always_ff @(clk)` runs at each change of `clk`, as
//! the same `always` does.
use celox::Simulation;
use std::path::Path;

/// Runs `source`, whose `initial` blocks print lines, and returns them.
fn display(source: &str, four_state: bool) -> Vec<String> {
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("always_ff.sv"))], "Top")
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
fn runs_at_each_change_of_an_event_without_an_edge() {
    let source = r#"
    `timescale 1ns/1ns
    interface Clocks;
      logic clk;
    endinterface
    module Top;
      logic clk = 0;
      logic other = 0;
      int changes = 0, either = 0, member = 0, plain = 0, plain_member = 0;
      Clocks clocks();
      assign clocks.clk = clk;
      always_ff @(clk) changes <= changes + 1;
      always_ff @(clk or other) either <= either + 1;
      always_ff @(clocks.clk) member <= member + 1;
      always @(clk) plain <= plain + 1;
      always @(clocks.clk) plain_member <= plain_member + 1;
      initial begin
        repeat (5) #1 clk = ~clk;
        #1 other = 1;
        // Counted against the `always` of the same event control: the
        // member also changes when its continuous assignment first runs.
        #1 $display("changes=%0d either=%0d member=%0d ran=%0d", changes - plain, either - plain,
                    member - plain_member, plain >= 5);
      end
    endmodule
    "#;
    for four_state in [false, true] {
        assert_eq!(
            display(source, four_state),
            ["changes=0 either=1 member=0 ran=1"],
            "four-state {four_state}"
        );
    }
}

/// The event control may name a member through a virtual interface
/// (25.9); its lowered wait is the procedure's event control, not a
/// blocking timing control of its body (9.2.2.4).
#[test]
fn runs_on_a_virtual_interface_member_without_an_edge() {
    let source = r#"
    `timescale 1ns/1ns
    interface bus_if(input logic clk);
    endinterface
    module Top;
      logic tick = 0;
      always #2 tick = ~tick;
      bus_if ticking(tick);
      virtual bus_if vif = ticking;
      int changes = 0;
      always_ff @(vif.clk) changes <= changes + 1;
      initial #9 $display("changes=%0d", changes);
    endmodule
    "#;
    assert_eq!(display(source, false), ["changes=4"]);
}

/// A task that suspends is still rejected behind a virtual-interface
/// event control: only the event control itself is stripped.
#[test]
fn virtual_interface_bodies_keep_the_restrictions_of_always_ff() {
    let source = r#"
    `timescale 1ns/1ns
    interface bus_if(input logic clk);
    endinterface
    module Top;
      logic tick = 0;
      bus_if ticking(tick);
      virtual bus_if vif = ticking;
      logic q = 0;
      task automatic pause();
        #1;
      endtask
      always_ff @(vif.clk) begin
        pause();
        q <= ~q;
      end
    endmodule
    "#;
    let error = Simulation::from_sv_sources(vec![(source, Path::new("always_ff.sv"))], "Top")
        .build()
        .expect_err("the procedure is rejected")
        .to_string()
        .replace(['\n', '│'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(error.contains("timed task call in always_ff"), "{error}");
}

/// The event control may name an event variable (15.5): its lowered
/// wait is the procedure's event control, not a body operation.
#[test]
fn runs_on_a_named_event() {
    let source = r#"
    `timescale 1ns/1ns
    module Top;
      event ev;
      int changes = 0, plain = 0;
      always_ff @(ev) changes <= changes + 1;
      always @(ev) plain <= plain + 1;
      initial begin
        #1 ->> ev;
        #1 ->> ev;
        #1 $display("changes=%0d", changes - plain);
      end
    endmodule
    "#;
    assert_eq!(display(source, false), ["changes=0"]);
}

/// An intra-assignment delay of a nonblocking assignment schedules its
/// update without suspending the procedure (9.4.5, 10.4.2).
#[test]
fn runs_with_a_delayed_nonblocking_assignment() {
    let source = r#"
    `timescale 1ns/1ns
    module Top;
      logic clk = 0;
      int changes = 0, plain = 0;
      always_ff @(clk) changes <= #1 changes + 1;
      always @(clk) plain <= #1 plain + 1;
      initial begin
        repeat (3) #1 clk = ~clk;
        #2 $display("changes=%0d", changes - plain);
      end
    endmodule
    "#;
    assert_eq!(display(source, false), ["changes=0"]);
}

/// A process-form `always_ff` body is an always body: it may call the
/// procedural system tasks an `always_ff` with an edge may call.
#[test]
fn process_form_bodies_call_procedural_system_tasks() {
    let source = r#"
    `timescale 1ns/1ns
    module Top;
      logic clk = 0;
      int changes = 0, plain = 0;
      always_ff @(clk) begin
        $assert(1'b1);
        changes <= changes + 1;
      end
      always @(clk) plain <= plain + 1;
      initial begin
        repeat (3) #1 clk = ~clk;
        #1 $display("changes=%0d", changes - plain);
      end
    endmodule
    "#;
    assert_eq!(display(source, false), ["changes=0"]);
}

/// The procedure keeps the restrictions of an `always_ff`: no blocking
/// timing control, also none of a task it calls, and no other process
/// writes its variables.
#[test]
fn keeps_the_restrictions_of_always_ff() {
    for (expected, body) in [
        (
            "timed task call in always_ff",
            "task automatic pause(); #1; endtask
             always_ff @(clk) begin pause(); q <= d; end",
        ),
        (
            "timed task call in always_ff",
            "task automatic pause(); #1; endtask
             always_ff @(clk) begin pause(); q <= #1 d; end",
        ),
        (
            "always_ff",
            "always_ff @(clk) q <= d;
             always_ff @(clk) q <= ~d;",
        ),
    ] {
        let source = format!(
            "`timescale 1ns/1ns
             module Top(input logic clk, d, output logic q);
               {body}
             endmodule"
        );
        let error =
            Simulation::from_sv_sources(vec![(source.as_str(), Path::new("always_ff.sv"))], "Top")
                .build()
                .expect_err("the procedure is rejected")
                .to_string()
                .replace(['\n', '│'], " ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
        assert!(error.contains(expected), "{body}: {error}");
    }
}
