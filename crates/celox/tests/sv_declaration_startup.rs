#![cfg(feature = "systemverilog")]

use celox::{SimBackend, Simulation};
use std::path::Path;

// IEEE 1800-2023 6.8 initializes static declarations before initial/always
// procedures, including initializers that require runtime evaluation.
// `always @*` waits for a change only in four-state mode; in two-state mode it
// runs once at time zero (#2021), so the tests of its startup guard build
// four-state simulations.
const RANDOM_DECLARATION: &str = r#"
module Top;
  int a = $urandom | 1;
  logic q = 0;
  always @* if (a != 0) q = 1;
  initial begin
    #1;
    if (a == 0 || q !== 0) $fatal(1, "declaration initialization woke wildcard");
    a = 0;
    #1;
    a = 5;
    #1;
    if (q !== 1) $fatal(1, "later assignment did not wake wildcard");
    $finish;
  end
endmodule
"#;

const ORDINARY_OVERWRITE: &str = r#"
module Top;
  int a = $urandom | 1;
  logic [31:0] q = 32'h55;
  always @* q = a;
  initial a = 0;
  initial begin
    #1;
    if (a !== 0 || q !== 0) $fatal(1, "ordinary initial overwrite was lost");
    a = 5;
    #1;
    if (q !== 5) $fatal(1, "later overwrite was lost");
    $finish;
  end
endmodule
"#;

const HIERARCHY_DECLARATIONS: &str = r#"
module Child(input int drive, output int seeded, output logic q);
  int seed = $urandom | 1;
  assign seeded = seed;
  initial q = 0;
  always @* if (drive != 0) q = 1;
endmodule
module Top;
  int drive = 1;
  int seeded;
  logic q;
  Child child(drive, seeded, q);
  initial drive = 0;
  initial begin
    #1;
    if (seeded == 0) $fatal(1, "child declaration was not initialized");
    if (drive !== 0) $fatal(1, "ordinary parent overwrite was lost");
    // Avoid assuming an active-region ordering for the initial port alias.
    drive = 5;
    #1;
    if (q !== 1) $fatal(1, "later parent input event was lost");
    $finish;
  end
endmodule
"#;

const HOST_WRITES: &str = r#"
module Top(input bit unrelated);
  int a = $urandom | 1;
  int q = 0;
  always @* if (a != 0) q = a;
endmodule
"#;

const HOST_DECLARATION: &str = r#"
module Top;
  int a = $test$plusargs("on");
  logic q = 0;
  always @* if (a != 0) q = 1;
  initial begin
    #1;
    if (a !== 1 || q !== 0) $fatal(1, "host declaration startup ordering");
    a = 0;
    #1;
    a = 5;
    #1;
    if (q !== 1) $fatal(1, "host declaration blocked later events");
    $finish;
  end
endmodule
"#;

const TERMINAL_DECLARATION: &str = r#"
module Top;
  int calls = 0;
  int ordinary = 0;
  function int initialize();
    calls = calls + 1;
    $finish;
    return 0;
  endfunction
  int a = initialize();
  initial ordinary = 1;
endmodule
"#;

const STOPPED_DECLARATION: &str = r#"
module Top;
  int a = 0;
  logic q = 0;
  function int initialize();
    a = 1;
    $stop;
    return 0;
  endfunction
  int result = initialize();
  always @* if (a != 0) q = 1;
endmodule
"#;

const DEFAULT_BEFORE_OVERWRITE: &str = r#"
module Top;
  logic a;
  logic observed = 1;
  function int initialize();
    observed = a;
    return $urandom;
  endfunction
  int result = initialize();
  initial a = 1;
  initial begin
    #1;
    if (a !== 1) $fatal(1, "ordinary overwrite was not replayed");
    $finish;
  end
endmodule
"#;

fn stopped_declaration_keeps_comb_inert<B: SimBackend>(mut simulation: Simulation<B>) {
    let a = simulation.signal("a");
    let q = simulation.signal("q");
    assert!(simulation.is_stopped());
    assert_eq!(simulation.get(a), 1u8.into());
    assert_eq!(simulation.get(q), 0u8.into());
    assert_eq!(simulation.get_four_state(q), (0u8.into(), 0u8.into()));
    simulation.eval_comb().unwrap();
    assert_eq!(simulation.get(q), 0u8.into());
    // Event draining selects a separate comb-capture path for later host
    // modifications. It must honor the same suspended declaration phase.
    drop(simulation.drain_runtime_events());
    simulation.modify(|_| {}).unwrap();
    assert_eq!(simulation.get(q), 0u8.into());
    assert!(matches!(
        simulation.checkpoint(),
        Err(celox::CheckpointError::Stopped)
    ));
    assert!(matches!(
        simulation.save_state(),
        Err(celox::StateError::Checkpoint(
            celox::CheckpointError::Stopped
        ))
    ));
    simulation.run_until(0).unwrap();
    assert!(!simulation.is_stopped());
    assert!(!simulation.is_finished());
    simulation.eval_comb().unwrap();
    // The write to a happened inside static initialization. Resuming the
    // initializer must arm the wildcard without treating that write as an event.
    assert_eq!(simulation.get(q), 0u8.into());
    simulation.modify(|io| io.set(a, 2u8)).unwrap();
    simulation.run_until(0).unwrap();
    assert_eq!(simulation.get(q), 1u8.into());
}

fn finished_declaration_stays_terminal<B: SimBackend>(mut simulation: Simulation<B>) {
    let calls = simulation.signal("calls");
    let ordinary = simulation.signal("ordinary");
    assert!(simulation.is_finished());
    for _ in 0..3 {
        assert_eq!(simulation.get(calls), 1u8.into());
        assert_eq!(simulation.get(ordinary), 0u8.into());
        assert_eq!(simulation.step().unwrap(), None);
        simulation.run_until(5).unwrap();
        assert!(simulation.is_finished());
    }
    assert_eq!(simulation.get(calls), 1u8.into());
    assert_eq!(simulation.get(ordinary), 0u8.into());
}

fn finishes<B: SimBackend>(mut simulation: Simulation<B>) {
    // Step through HDL events without assuming the source's time precision.
    for _ in 0..16 {
        if simulation.is_finished() || simulation.step().unwrap().is_none() {
            break;
        }
    }
    assert!(simulation.is_finished(), "the source must reach $finish");
}

fn host_writes_survive_before_first_step<B: SimBackend>(mut simulation: Simulation<B>) {
    let a = simulation.signal("a");
    let q = simulation.signal("q");
    let unrelated = simulation.signal("unrelated");
    // The runtime declaration is odd, so six is always a real change.
    simulation
        .modify(|io| {
            io.set(a, 6u8);
            io.set(unrelated, 1u8);
        })
        .unwrap();
    simulation.run_until(0).unwrap();
    assert_eq!(simulation.get(a), 6u8.into());
    assert_eq!(simulation.get(unrelated), 1u8.into());
    assert_eq!(simulation.get(q), 6u8.into());
    simulation.run_until(0).unwrap();
    assert_eq!(simulation.get(q), 6u8.into());
}

fn restored_guard_remains_inert<B: SimBackend>(mut simulation: Simulation<B>) {
    let a = simulation.signal("a");
    let q = simulation.signal("q");
    let unrelated = simulation.signal("unrelated");
    // A checkpoint may be taken before the first scheduler step because the
    // constructor has already completed static runtime declarations.
    assert_eq!(simulation.get(q), 0u8.into());
    let checkpoint = simulation.checkpoint().unwrap();
    simulation.modify(|io| io.set(a, 6u8)).unwrap();
    simulation.run_until(0).unwrap();
    assert_eq!(simulation.get(q), 6u8.into());
    simulation.restore(&checkpoint).unwrap();
    simulation.run_until(0).unwrap();
    assert_eq!(simulation.get(q), 0u8.into());
    simulation.modify(|io| io.set(unrelated, 1u8)).unwrap();
    simulation.run_until(0).unwrap();
    assert_eq!(simulation.get(q), 0u8.into());
    simulation.modify(|io| io.set(a, 8u8)).unwrap();
    simulation.run_until(0).unwrap();
    assert_eq!(simulation.get(q), 8u8.into());
}

macro_rules! declaration_backend {
    ($module:ident, $build:ident) => {
        mod $module {
            use super::*;

            #[test]
            fn runtime_declaration_does_not_wake_wildcard() {
                finishes(
                    Simulation::from_sv_sources(
                        vec![(RANDOM_DECLARATION, Path::new("runtime_declaration.sv"))],
                        "Top",
                    )
                    .four_state(true)
                    .$build()
                    .unwrap(),
                );
            }

            #[test]
            fn ordinary_initial_overwrites_runtime_declaration_and_wakes_wildcard() {
                for four_state in [false, true] {
                    finishes(
                        Simulation::from_sv_sources(
                            vec![(ORDINARY_OVERWRITE, Path::new("declaration_overwrite.sv"))],
                            "Top",
                        )
                        .four_state(four_state)
                        .$build()
                        .unwrap(),
                    );
                }
            }

            #[test]
            fn child_declarations_precede_ordinary_parent_initials() {
                for four_state in [false, true] {
                    let mut simulation = Simulation::from_sv_sources(
                        vec![(
                            HIERARCHY_DECLARATIONS,
                            Path::new("hierarchy_declarations.sv"),
                        )],
                        "Top",
                    )
                    .four_state(four_state)
                    .live_signal(vec![("child".into(), 0)], vec!["seeded".into()])
                    .$build()
                    .unwrap();
                    // A host read settles the port connection but does not
                    // run ordinary initial kernels. Child declarations must
                    // already have run before the first scheduler step.
                    let seeded = simulation.child_signal(&[("child", 0)], "seeded");
                    assert_ne!(simulation.get(seeded), 0u8.into());
                    finishes(simulation);
                }
            }

            #[test]
            fn first_host_writes_survive_declaration_startup() {
                for four_state in [false, true] {
                    host_writes_survive_before_first_step(
                        Simulation::from_sv_sources(
                            vec![(HOST_WRITES, Path::new("declaration_host_writes.sv"))],
                            "Top",
                        )
                        .four_state(four_state)
                        .live_signal(Vec::new(), vec!["a".into()])
                        .live_signal(Vec::new(), vec!["q".into()])
                        .$build()
                        .unwrap(),
                    );
                }
            }

            #[test]
            fn host_request_declaration_finishes_before_wildcard_registration() {
                finishes(
                    Simulation::from_sv_sources(
                        vec![(HOST_DECLARATION, Path::new("declaration_plusargs.sv"))],
                        "Top",
                    )
                    .four_state(true)
                    .plusargs(["+on"])
                    .$build()
                    .unwrap(),
                );
            }

            #[test]
            fn host_reads_and_eval_do_not_activate_a_stopped_declaration() {
                stopped_declaration_keeps_comb_inert(
                    Simulation::from_sv_sources(
                        vec![(STOPPED_DECLARATION, Path::new("declaration_stop.sv"))],
                        "Top",
                    )
                    .four_state(true)
                    .live_signal(Vec::new(), vec!["a".into()])
                    .live_signal(Vec::new(), vec!["q".into()])
                    .$build()
                    .unwrap(),
                );
            }

            #[test]
            fn runtime_declaration_observes_default_before_folded_overwrite() {
                for four_state in [false, true] {
                    let mut simulation = Simulation::from_sv_sources(
                        vec![(
                            DEFAULT_BEFORE_OVERWRITE,
                            Path::new("declaration_default.sv"),
                        )],
                        "Top",
                    )
                    .four_state(four_state)
                    .live_signal(Vec::new(), vec!["observed".into()])
                    .$build()
                    .unwrap();
                    let observed = simulation.signal("observed");
                    let expected = u8::from(four_state);
                    assert_eq!(
                        simulation.get_four_state(observed),
                        (expected.into(), expected.into())
                    );
                    finishes(simulation);
                }
            }

            #[test]
            fn finished_declaration_does_not_restart_on_later_steps() {
                for four_state in [false, true] {
                    finished_declaration_stays_terminal(
                        Simulation::from_sv_sources(
                            vec![(TERMINAL_DECLARATION, Path::new("declaration_finish.sv"))],
                            "Top",
                        )
                        .four_state(four_state)
                        .live_signal(Vec::new(), vec!["calls".into()])
                        .live_signal(Vec::new(), vec!["ordinary".into()])
                        .$build()
                        .unwrap(),
                    );
                }
            }

            #[test]
            fn checkpoint_restores_the_armed_startup_guard() {
                restored_guard_remains_inert(
                    Simulation::from_sv_sources(
                        vec![(HOST_WRITES, Path::new("declaration_checkpoint.sv"))],
                        "Top",
                    )
                    .four_state(true)
                    .live_signal(Vec::new(), vec!["a".into()])
                    .live_signal(Vec::new(), vec!["q".into()])
                    .$build()
                    .unwrap(),
                );
            }
        }
    };
}

declaration_backend!(interpreter, build_interpreter);
declaration_backend!(cranelift, build_cranelift);
#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
declaration_backend!(native, build_native);
declaration_backend!(wasm, build_wasm);

#[test]
#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
fn native_runtime_declaration_startup_at_o0_and_o2() {
    for level in [celox::OptLevel::O0, celox::OptLevel::O2] {
        finishes(
            Simulation::from_sv_sources(
                vec![(RANDOM_DECLARATION, Path::new("declaration_opt_level.sv"))],
                "Top",
            )
            .four_state(true)
            .opt_level(level)
            .build_native()
            .unwrap(),
        );
    }
}

#[test]
fn plain_simulator_rejects_initial_kernels() {
    let error = celox::Simulator::from_sv_sources(
        vec![(
            HOST_DECLARATION,
            Path::new("declaration_plain_simulator.sv"),
        )],
        "Top",
    )
    .plusargs(["+on"])
    .build_interpreter()
    .map(|_| ())
    .expect_err("Simulator accepted a design whose kernels it does not run");
    // Declaration initializers start before the initial block.
    let celox::SimulatorErrorKind::ProcessRequiresSimulation(origin) = error.kind() else {
        panic!("{error}");
    };
    assert_eq!(origin.construct, "declaration initializer");
    assert_eq!(
        origin.location.as_deref(),
        Some("declaration_plain_simulator.sv:3:3")
    );
}
