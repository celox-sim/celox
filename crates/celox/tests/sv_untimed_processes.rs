//! An untimed `Simulator` runs no process kernel, so building one for a
//! design with a kernel is an error that names the process (#1712).
#![cfg(feature = "systemverilog")]

use std::path::Path;

use celox::{ProcessOrigin, Simulation, Simulator, SimulatorError, SimulatorErrorKind};

/// The build error of every untimed backend.
fn untimed_errors(sources: &[(&str, &str)], top: &str) -> Vec<SimulatorError> {
    let build = || {
        Simulator::from_sv_sources(
            sources
                .iter()
                .map(|(source, path)| (*source, Path::new(path)))
                .collect(),
            top,
        )
    };
    let mut errors = vec![
        build().build_interpreter().map(|_| ()).unwrap_err(),
        build().build_cranelift().map(|_| ()).unwrap_err(),
        build().build_wasm().map(|_| ()).unwrap_err(),
        build().build_tiered().map(|_| ()).unwrap_err(),
    ];
    #[cfg(any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    ))]
    errors.extend([
        build().build_native().map(|_| ()).unwrap_err(),
        build().compile_native().map(|_| ()).unwrap_err(),
    ]);
    errors
}

fn origin(error: &SimulatorError) -> &ProcessOrigin {
    match error.kind() {
        SimulatorErrorKind::ProcessRequiresSimulation(origin) => origin,
        _ => panic!("{error}"),
    }
}

/// Every untimed backend rejects `source` and names the same process.
fn assert_rejected(source: &str, construct: &str, location: &str, scope: &str) {
    for error in untimed_errors(&[(source, "processes.sv")], "Top") {
        let origin = origin(&error);
        assert_eq!(origin.construct, construct, "{error}");
        assert_eq!(origin.location.as_deref(), Some(location), "{error}");
        assert_eq!(origin.scope.as_deref(), Some(scope), "{error}");
        let message = error.to_string();
        assert!(message.contains(location), "{message}");
        assert!(message.contains("Simulation"), "{message}");
    }
}

/// `always @(a)` does not list `b`, so it is not an implicit `always @*`.
const INCOMPLETE_LIST: &str = "module Top(input logic [7:0] a, b, output logic [7:0] y);
  always @(a) y = a + b;
endmodule
";

#[test]
fn incomplete_event_list_is_rejected() {
    assert_rejected(INCOMPLETE_LIST, "always", "processes.sv:2:3", "Top");
}

#[test]
fn incomplete_event_list_runs_in_a_simulation() {
    let mut sim = Simulation::from_sv_sources(vec![(INCOMPLETE_LIST, Path::new("p.sv"))], "Top")
        .build_interpreter()
        .unwrap();
    let (a, b, y) = (sim.signal("a"), sim.signal("b"), sim.signal("y"));
    sim.modify(|io| io.set(b, 5u8)).unwrap();
    sim.run_until(0).unwrap();
    sim.modify(|io| io.set(a, 3u8)).unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(y), 8u8.into());
}

#[test]
fn delayed_initial_is_rejected() {
    assert_rejected(
        "module Top(output logic x);
  initial #5 x = 1;
endmodule
",
        "initial",
        "processes.sv:2:3",
        "Top",
    );
}

#[test]
fn timed_task_in_an_edge_procedure_is_rejected() {
    assert_rejected(
        "module Top(input logic clk, a, output logic q);
  task automatic pause; #1; endtask
  always @(posedge clk) begin pause(); q = a; end
endmodule
",
        "always",
        "processes.sv:3:3",
        "Top",
    );
}

#[test]
fn always_ff_with_file_output_is_rejected() {
    assert_rejected(
        "module Top(input logic clk, d, output logic q);
  always_ff @(posedge clk) begin
    $fwrite(1, \"edge\");
    q <= d;
  end
endmodule
",
        "always_ff",
        "processes.sv:2:3",
        "Top",
    );
}

#[test]
fn the_first_process_is_named() {
    assert_rejected(
        "module Top(output logic x, y);
  initial x = 0;
  initial #2 y = 1;
  always #1 x = ~x;
endmodule
",
        "initial",
        "processes.sv:3:3",
        "Top",
    );
}

#[test]
fn processes_of_child_instances_are_rejected() {
    let sources = [(
        "module Child(input logic [7:0] a, b, output logic [7:0] y);
  always @(a) y = a + b;
endmodule
module Top(input logic [7:0] a, b, output logic [7:0] y);
  Child child(a, b, y);
endmodule
",
        "child.sv",
    )];
    for error in untimed_errors(&sources, "Top") {
        let origin = origin(&error);
        assert_eq!(origin.construct, "always");
        assert_eq!(origin.location.as_deref(), Some("child.sv:2:3"));
        assert_eq!(origin.scope.as_deref(), Some("Top.child"));
    }
}

const VERYL_TOP: &str = "module Top(a: input logic<8>, b: input logic<8>, y: output logic<8>) { inst child: $sv::Child(a, b, y); }";
const SV_CHILD: &str = "module Child(input logic [7:0] a, b, output logic [7:0] y);
  always @(a) y = a + b;
endmodule
";

#[test]
fn processes_below_a_veryl_top_are_rejected() {
    let error = Simulator::from_mixed_sources(
        vec![(VERYL_TOP, Path::new("top.veryl"))],
        vec![(SV_CHILD, Path::new("child.sv"))],
        "Top",
    )
    .build_interpreter()
    .map(|_| ())
    .unwrap_err();
    let origin = origin(&error);
    assert_eq!(origin.construct, "always");
    assert_eq!(origin.location.as_deref(), Some("child.sv:2:3"));
    let mut sim = Simulation::from_sources(vec![(VERYL_TOP, Path::new("top.veryl"))], "Top")
        .with_sv_sources(vec![(SV_CHILD, Path::new("child.sv"))])
        .build_interpreter()
        .unwrap();
    let (a, b, y) = (sim.signal("a"), sim.signal("b"), sim.signal("y"));
    sim.modify(|io| io.set(b, 5u8)).unwrap();
    sim.run_until(0).unwrap();
    sim.modify(|io| io.set(a, 3u8)).unwrap();
    sim.run_until(1).unwrap();
    assert_eq!(sim.get(y), 8u8.into());
}

/// Initial blocks and declaration initializers with constant results are
/// initial values, not processes, so an untimed `Simulator` keeps them.
#[test]
fn constant_initial_state_builds_an_untimed_simulator() {
    let source = "module Top(input logic [7:0] a, output logic [7:0] y, z);
  logic [7:0] base = 8'd3;
  logic [7:0] mem [4];
  logic [7:0] offset;
  initial begin
    offset = 8'd10;
    for (int i = 0; i < 4; i++) mem[i] = 8'(i * 2);
  end
  assign y = a + base + offset;
  assign z = mem[a[1:0]];
endmodule
";
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("initial.sv"))], "Top")
        .build_interpreter()
        .unwrap();
    let (a, y, z) = (sim.signal("a"), sim.signal("y"), sim.signal("z"));
    sim.modify(|io| io.set(a, 3u8)).unwrap();
    assert_eq!(sim.get(y), 16u8.into());
    assert_eq!(sim.get(z), 6u8.into());
}
