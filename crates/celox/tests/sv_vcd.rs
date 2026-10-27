#![cfg(feature = "systemverilog")]
//! Source-driven VCD controls exercise host requests and trace observation.
use celox::{SimBackend, Simulation};
use std::path::Path;

fn capture<B: SimBackend>(mut simulation: Simulation<B>, path: &Path) -> String {
    simulation.run_until(4).unwrap();
    simulation.flush_vcd().unwrap();
    std::fs::read_to_string(path).unwrap()
}

macro_rules! backends {
    ($name:ident, $body:expr) => {
        mod $name {
            use super::*;
            #[test]
            fn interpreter() {
                ($body)("interpreter", 0);
            }
            #[test]
            fn cranelift() {
                ($body)("cranelift", 1);
            }
            #[test]
            fn wasm() {
                ($body)("wasm", 2);
            }
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            #[test]
            fn native() {
                ($body)("native", 3);
            }
        }
    };
}

backends!(controls, |backend: &str, number: usize| {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{backend}.vcd"));
    let source = format!(
        r#"
        module Top(output logic [3:0] value);
            timeunit 1ps; timeprecision 1ps;
            initial begin
                $dumpfile("{}");
                $dumpvars(0, Top.value);
                value = 4'b10xz;
                #1 $dumpoff; value = 4'b1111;
                #1 $dumpon;
                #1 $dumpall; $dumpflush;
            end
        endmodule
    "#,
        path.display()
    );
    let build = || {
        Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top").four_state(true)
    };
    let text = match number {
        0 => capture(build().build_interpreter().unwrap(), &path),
        1 => capture(build().build_cranelift().unwrap(), &path),
        2 => capture(build().build_wasm().unwrap(), &path),
        _ => capture(build().build_native().unwrap(), &path),
    };
    assert!(text.contains("$var wire 4"), "{text}");
    assert!(text.contains("b10xz "), "{text}");
    assert!(text.contains("$dumpoff\nbxxxx "), "{text}");
    assert!(text.contains("$dumpon\nb1111 "), "{text}");
    assert!(text.contains("$dumpall\nb1111 "), "{text}");
    assert!(!text.contains("vcd_argument"), "{text}");
});

#[test]
fn depth_and_multiple_selections() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hierarchy.vcd");
    let source = format!(
        r#"
        module Child(output logic child_value);
            timeunit 1ps; timeprecision 1ps;
            initial child_value = 1;
        endmodule
        module Top(output logic own_value, output logic child_wire);
            timeunit 1ps; timeprecision 1ps;
            Child child(.child_value(child_wire));
            initial begin
                $dumpfile("{}");
                $dumpvars(1, Top);
                $dumpvars(0, Top.child.child_value);
                own_value = 1;
            end
        endmodule
    "#,
        path.display()
    );
    let sim = Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
        .build_interpreter()
        .unwrap();
    let text = capture(sim, &path);
    assert!(text.contains("own_value"), "{text}");
    assert!(text.contains("child_value"), "{text}");
}

#[test]
fn later_dumpvars_reports_runtime_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("late.vcd");
    let source = format!(
        r#"module Top(output logic value);
            timeunit 1ps; timeprecision 1ps;
        initial begin $dumpfile("{}"); $dumpvars; #1 $dumpvars; end
        endmodule"#,
        path.display()
    );
    let mut sim = Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
        .build_interpreter()
        .unwrap();
    let error = sim.run_until(2).unwrap_err();
    assert!(format!("{error:?}").contains("same simulation time"));
}

#[test]
fn source_controls_keep_packed_array_storage_native() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("array.vcd");
    let source = format!(
        r#"module Top(output logic done);
            timeunit 1ps; timeprecision 1ps;
        logic [4:0] memory [0:1];
        initial begin
            $dumpfile("{}"); $dumpvars(0, Top.memory);
            memory[0] = 5'h03; memory[1] = 5'h15; done = 1;
        end
        endmodule"#,
        path.display()
    );
    let sim = Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
        .build()
        .unwrap();
    let text = capture(sim, &path);
    assert!(text.contains("$var wire 10"), "{text}");
    assert!(text.contains("memory"), "{text}");
    assert!(!text.contains(" done "), "{text}");
}

#[test]
fn finish_flushes_source_waveform() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("finish.vcd");
    let source = format!(
        r#"module Top(output logic value);
            timeunit 1ps; timeprecision 1ps;
        initial begin $dumpfile("{}"); $dumpvars; value = 1; $finish; end
        endmodule"#,
        path.display()
    );
    let mut sim = Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
        .build_interpreter()
        .unwrap();
    sim.run_until(1).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("value"), "{text}");
    assert!(text.contains("#0\n1"), "{text}");
}

#[test]
fn unwritable_source_path_returns_runtime_error() {
    let dir = tempfile::tempdir().unwrap();
    // Opening a directory as an output file fails regardless of user privileges.
    let source = format!(
        r#"module Top(output logic value);
        initial begin $dumpfile("{}"); $dumpvars; value = 1; end
        endmodule"#,
        dir.path().display()
    );
    let mut sim = Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
        .build_interpreter()
        .unwrap();
    let error = sim.run_until(1).unwrap_err();
    assert!(format!("{error:?}").contains("VCD setup failed"));
}

#[test]
fn embedded_filename_nul_is_rejected() {
    let source = r#"module Top(output logic value);
        initial begin $dumpfile(32'h61006200); $dumpvars; value = 1; end
        endmodule"#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("vcd.sv"))], "Top")
        .build_interpreter()
        .unwrap();
    let error = sim.run_until(1).unwrap_err();
    assert!(format!("{error:?}").contains("NUL byte"));
}

#[test]
fn authored_internal_looking_names_are_selected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("authored.vcd");
    let source = format!(
        r#"module Top(output logic __celox_user);
        initial begin $dumpfile("{}"); $dumpvars(0, Top.__celox_user); __celox_user=1; end
        endmodule"#,
        path.display()
    );
    let simulation = Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
        .build_interpreter()
        .unwrap();
    let text = capture(simulation, &path);
    assert!(text.contains(" __celox_user $end"), "{text}");
    assert!(!text.contains("vcd_argument"), "{text}");
}

#[test]
fn removing_reached_limit_rescans_stable_values() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("resumed.vcd");
    let source = format!(
        r#"module Top(output logic [3:0] value);
        timeunit 1ps; timeprecision 1ps;
        initial begin
            $dumpfile("{}"); $dumpvars; value=0;
            #1 $dumplimit(1); value=3;
            #1 value=5;
            #1 $dumplimit(0);
        end endmodule"#,
        path.display()
    );
    let simulation = Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
        .build()
        .unwrap();
    let text = capture(simulation, &path);
    assert!(text.contains("VCD dump limit reached"), "{text}");
    assert!(text.contains("#3\nb101 "), "{text}");
}

#[test]
fn source_replacement_flushes_old_writer_before_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("same.vcd");
    let source = format!(
        r#"module Top(output logic value);
        timeunit 1ps; timeprecision 1ps;
        initial begin
            value=0;
            #1 $dumpfile("{}"); $dumpvars(0, Top.value); value=1;
        end endmodule"#,
        path.display()
    );
    let mut simulation = Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
        .vcd(&path)
        .build_interpreter()
        .unwrap();
    // Populate the old writer's buffer with more bytes than the replacement.
    for _ in 0..1000 {
        simulation.dump(0);
    }
    let text = {
        simulation.run_until(2).unwrap();
        simulation.flush_vcd().unwrap();
        std::fs::read_to_string(path).unwrap()
    };
    assert_eq!(text.matches("$enddefinitions").count(), 1, "{text}");
    assert!(
        !text.contains("#0"),
        "old buffered timestamps survived replacement: {text}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn ordinary_timed_write_failure_is_returned() {
    let source = r#"module Top(output logic [2097151:0] value);
        initial begin $dumpfile("/dev/full"); $dumpvars; value='1; end
        endmodule"#;
    let mut simulation = Simulation::from_sv_sources(vec![(source, Path::new("vcd.sv"))], "Top")
        .build_interpreter()
        .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| simulation.run_until(1)));
    let error = result
        .expect("a timed waveform failure must not panic")
        .unwrap_err();
    assert!(
        format!("{error:?}").contains("VCD dump failed"),
        "{error:?}"
    );
}

#[test]
fn direct_finish_finalizes_source_waveform() {
    for action in ["get", "get_four_state", "eval_comb", "dump"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("direct.vcd");
        let source = format!(
            r#"module Top(input logic stop_now, output logic value);
            initial begin $dumpfile("{}"); $dumpvars; end
            always_comb begin value=stop_now; if (stop_now) $finish; end
            endmodule"#,
            path.display()
        );
        let mut simulation =
            Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
                .build_interpreter()
                .unwrap();
        simulation.run_until(1).unwrap();
        let stop = simulation.signal("stop_now");
        let value = simulation.signal("value");
        simulation.modify(|io| io.set(stop, 1u8)).unwrap();
        match action {
            "get" => {
                simulation.get(value);
            }
            "get_four_state" => {
                simulation.get_four_state(value);
            }
            "eval_comb" => simulation.eval_comb().unwrap(),
            _ => simulation.try_dump(1).unwrap(),
        }
        // Read with the simulator alive: finish must flush, independently of Drop.
        let text = std::fs::read_to_string(&path).unwrap();
        let last = text.rsplit("#1\n").next().unwrap();
        assert!(
            last.lines().any(|line| line.starts_with('1')),
            "{action}: {text}"
        );
        assert!(simulation.is_finished());
    }
}

#[test]
fn absolute_root_dump_targets_resolve_from_child_scope() {
    for target in ["$root.Top.value", "$root.Top"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("root.vcd");
        let source = format!(
            r#"
            module Recorder;
                initial begin $dumpfile("{}"); $dumpvars(0, {target}); end
            endmodule
            module Top(output logic value);
                Recorder recorder();
                initial value=1;
            endmodule"#,
            path.display()
        );
        let simulation = Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
            .build_interpreter()
            .unwrap();
        let text = capture(simulation, &path);
        assert!(text.contains(" value $end"), "{target}: {text}");
        assert!(
            !text.contains("$root"),
            "the pseudo-root must not become a VCD scope: {text}"
        );
    }
}

#[test]
fn module_recursive_dump_excludes_package_state() {
    for selection in ["$dumpvars(0, Top);", "$dumpvars(0);", "$dumpvars;"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("package.vcd");
        let source = format!(
            r#"
            package p; logic [7:0] package_only = 8'd7; endpackage
            module Child(output logic [7:0] child_value);
                import p::*;
                assign child_value=package_only;
            endmodule
            module Top(output logic [7:0] value, child_value);
                import p::*;
                Child child(child_value);
                assign value=package_only;
                initial begin $dumpfile("{}"); {selection} end
            endmodule"#,
            path.display()
        );
        let simulation = Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
            .build_interpreter()
            .unwrap();
        let text = capture(simulation, &path);
        assert!(text.contains(" value $end"), "{selection}: {text}");
        assert!(text.contains(" child_value $end"), "{selection}: {text}");
        let package_count = text.matches(" package_only $end").count();
        assert_eq!(
            package_count,
            usize::from(selection == "$dumpvars;"),
            "{selection}: {text}"
        );
        assert!(
            !text.contains("Top.p::"),
            "packages must not appear beneath the top module: {text}"
        );
    }
}

#[test]
fn escaped_dump_identifiers_keep_literal_delimiters() {
    for target in [
        r"Top.\a.b ",
        r"Top.\array[0] ",
        r"Top.\u.x .\state.value ",
        r"$root.Top.\u.x .\state.value ",
        r"\$root.Top.value ",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("escaped.vcd");
        let source = format!(
            r#"module Child(output logic \state.value );
                initial \state.value = 1;
            endmodule
            module Top(output logic \a.b , \array[0] , \$root.Top.value , child_value);
                Child \u.x (child_value);
                initial begin \a.b = 1; \array[0] = 0; \$root.Top.value = 1;
                    $dumpfile("{}"); $dumpvars(0, {target}); end
            endmodule"#,
            path.display()
        );
        let simulation = Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
            .build_interpreter()
            .unwrap();
        let text = capture(simulation, &path);
        assert_eq!(text.matches("$var wire ").count(), 1, "{target}: {text}");
    }
}

#[test]
fn constant_indexed_instance_and_generate_selections() {
    for target in [
        "Top.u[INDEX + 0]",
        "Top.g[INDEX + 0]",
        "Top.g[INDEX].nested.value",
        "$root.Top.g[INDEX]",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("indexed.vcd");
        let source = format!(
            r#"module Child(output logic value); initial value=1; endmodule
            module Top #(parameter int INDEX=1) (output logic [1:0] bus);
                Child u[0:1](bus);
                for (genvar i=0; i<2; i++) begin:g
                    logic generated;
                    initial generated=1;
                    Child nested();
                end
                initial begin $dumpfile("{}"); $dumpvars(0, {target}); end
            endmodule"#,
            path.display()
        );
        let simulation = Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
            .build_interpreter()
            .unwrap();
        let text = capture(simulation, &path);
        assert!(text.contains("[1]"), "{target}: {text}");
        assert!(!text.contains("[0]"), "{target}: {text}");
    }
}

#[test]
fn explicit_package_dump_selects_only_requested_variable() {
    for target in ["p::state", r"\p.q ::\state.value "] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("package-select.vcd");
        let source = format!(
            r#"package p; logic [7:0] state=7; logic [7:0] other=9; endpackage
            package \p.q ; logic \state.value = 1; endpackage
            module Recorder;
                initial begin $dumpfile("{}"); $dumpvars(0, {target}); end
            endmodule
            module Top(output logic [7:0] value, output logic escaped_value);
                assign value=p::other;
                assign escaped_value=\p.q ::\state.value ;
                Recorder recorder();
            endmodule"#,
            path.display()
        );
        let simulation = Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
            .build_interpreter()
            .unwrap();
        let text = capture(simulation, &path);
        assert_eq!(text.matches("$var wire ").count(), 1, "{target}: {text}");
        assert!(!text.contains(" other $end"), "{target}: {text}");
        assert!(!text.contains("$scope module Top"), "{target}: {text}");
    }
}

#[test]
fn initial_vcd_tasks_require_timed_simulation_on_every_backend() {
    for task in ["$dumpvars;", "$dumpflush;"] {
        let source = format!("module Top; initial begin {task} end endmodule");
        let build =
            || celox::Simulator::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top");
        let errors = [
            build().build_interpreter().unwrap_err(),
            build().build_cranelift().unwrap_err(),
            build().build_wasm().unwrap_err(),
            build().build().unwrap_err(),
        ];
        for error in errors {
            assert!(
                matches!(
                    error.kind(),
                    celox::SimulatorErrorKind::ProcessRequiresSimulation(_)
                ),
                "{error}"
            );
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn finish_stays_terminal_when_final_waveform_output_fails() {
    for width in [1, 2097152] {
        let source = format!(
            r#"module Top(output logic [{high}:0] value);
            initial begin $dumpfile("/dev/full"); $dumpvars; value='1; $finish; end
            initial begin #1 value='0; end
            endmodule"#,
            high = width - 1
        );
        let mut simulation =
            Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
                .build_interpreter()
                .unwrap();
        assert!(simulation.run_until(2).is_err());
        assert!(simulation.is_finished(), "width={width}");
        let time = simulation.time();
        assert_eq!(simulation.step().unwrap(), None);
        simulation.run_until(10).unwrap();
        assert_eq!(simulation.time(), time);
    }
}

#[test]
fn pending_source_waveform_is_finalized_on_fatal_error() {
    let dir = tempfile::tempdir().unwrap();
    for path in [
        dir.path().join("fatal.vcd"),
        dir.path().join("absent/fatal.vcd"),
    ] {
        let source = format!(
            r#"module Top(output logic value);
            initial begin $dumpfile("{}"); $dumpvars; value=1; $fatal(1,"primary failure"); end
            endmodule"#,
            path.display()
        );
        let mut simulation =
            Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
                .build_interpreter()
                .unwrap();
        let error = simulation.run_until(1).unwrap_err();
        assert!(
            format!("{error:?}").contains("primary failure"),
            "{error:?}"
        );
        if path.parent() == Some(dir.path()) {
            let text = std::fs::read_to_string(path).unwrap();
            assert!(text.contains("$enddefinitions"), "{text}");
            assert!(text.contains("1!"), "{text}");
        }
    }
}

#[test]
fn host_waveform_switch_keeps_source_suspension_and_limit() {
    for control in ["$dumpoff", "$dumplimit(1)"] {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.vcd");
        let second = dir.path().join("second.vcd");
        let source = format!(
            r#"module Top(output logic value);
            timeunit 1ps; timeprecision 1ps;
            initial begin $dumpfile("{}"); $dumpvars; value=0;
                #1 {}; #1 value=1; #1 $dumpon; $dumplimit(0); #1 value=0;
            end endmodule"#,
            first.display(),
            control
        );
        let mut simulation =
            Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
                .build_interpreter()
                .unwrap();
        simulation.run_until(1).unwrap();
        simulation.switch_vcd(&second).unwrap();
        simulation.run_until(2).unwrap();
        simulation.flush_vcd().unwrap();
        let paused = std::fs::read_to_string(&second).unwrap();
        if control == "$dumpoff" {
            assert!(!paused.contains("#2"), "{control}: {paused}");
        } else {
            assert!(paused.contains("VCD dump limit reached"), "{paused}");
            assert!(!paused.contains("1!"), "{paused}");
        }
        simulation.run_until(4).unwrap();
        simulation.flush_vcd().unwrap();
        let resumed = std::fs::read_to_string(second).unwrap();
        assert!(resumed.contains("#3"), "{control}: {resumed}");
        assert!(resumed.contains("#4"), "{control}: {resumed}");
    }
}

#[test]
fn imported_dump_target_follows_package_declaration_and_local_shadowing() {
    for (declaration, package) in [
        ("import p::*;", true),
        ("import p::state;", true),
        ("import p::*; logic state;", false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("import.vcd");
        let source = format!(
            r#"package p; logic state=1; logic other=0; endpackage
            module Top; {} initial begin $dumpfile("{}"); $dumpvars(0,state); end endmodule"#,
            declaration,
            path.display()
        );
        let mut simulation =
            Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
                .build_interpreter()
                .unwrap();
        simulation.run_until(1).unwrap();
        simulation.flush_vcd().unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert_eq!(text.matches("$var ").count(), 1, "{text}");
        assert_eq!(text.contains("$scope module p::"), package, "{text}");
        assert!(!text.contains(" other "), "{text}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn direct_finish_error_stays_terminal() {
    let source = r#"module Top(input logic stop_now, output logic value);
        initial begin $dumpfile("/dev/full"); $dumpvars; end
        always_comb begin value=stop_now; if(stop_now) $finish; end
        endmodule"#;
    let mut simulation = Simulation::from_sv_sources(vec![(source, Path::new("vcd.sv"))], "Top")
        .build_interpreter()
        .unwrap();
    simulation.run_until(0).unwrap();
    let stop = simulation.signal("stop_now");
    simulation.modify(|io| io.set(stop, 1u8)).unwrap();
    assert!(simulation.eval_comb().is_err());
    assert!(simulation.is_finished());
    assert_eq!(simulation.step().unwrap(), None);
}

#[test]
fn imported_escaped_package_delimiters_remain_literal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("escaped-import.vcd");
    let source = format!(
        r#"package \p::q ; logic \state::value =1; endpackage
        module Top; import \p::q ::*;
        initial begin $dumpfile("{}"); $dumpvars(0,\state::value ); end endmodule"#,
        path.display()
    );
    let mut simulation = Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
        .build_interpreter()
        .unwrap();
    simulation.run_until(1).unwrap();
    simulation.flush_vcd().unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    assert_eq!(text.matches("$var ").count(), 1, "{text}");
    assert!(text.contains("\\state::value"), "{text}");
}

#[test]
fn scoped_parameter_dump_does_not_select_imported_state() {
    let source = r#"package p; logic state=1; endpackage
        module Top; import p::*;
        initial begin localparam int state=0; $dumpvars(0,state); end endmodule"#;
    let mut simulation = Simulation::from_sv_sources(vec![(source, Path::new("vcd.sv"))], "Top")
        .build_interpreter()
        .unwrap();
    let error = simulation.run_until(1).unwrap_err();
    assert!(
        format!("{error:?}").contains("selection matches no dumpable variables"),
        "{error:?}"
    );
}

#[test]
fn direct_fatal_errors_publish_waveform_and_terminate() {
    for action in ["eval_comb", "try_dump", "dump"] {
        check_direct_fatal_waveform(action);
    }
}

#[test]
fn final_run_until_fatal_publishes_waveform() {
    check_direct_fatal_waveform("run_until");
}

#[test]
fn final_bounded_run_fatal_publishes_waveform() {
    check_direct_fatal_waveform("run_until_bounded");
}

fn check_direct_fatal_waveform(action: &str) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("direct-fatal.vcd");
    let source = format!(
        r#"module Top(input logic fail_now, output logic value, output int final_calls);
            initial begin final_calls=0; $dumpfile("{}"); $dumpvars(0,Top.value); end
            always_comb begin value=fail_now; if(fail_now) $fatal(1,"direct primary"); end
            final final_calls += 1;
            endmodule"#,
        path.display()
    );
    let mut simulation = Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
        .build_interpreter()
        .unwrap();
    simulation.run_until(0).unwrap();
    let fail = simulation.signal("fail_now");
    simulation.modify(|io| io.set(fail, 1u8)).unwrap();
    let error = match action {
        "eval_comb" => format!("{:?}", simulation.eval_comb().unwrap_err()),
        "try_dump" => format!("{:?}", simulation.try_dump(0).unwrap_err()),
        "dump" => {
            let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                simulation.dump(0);
            }))
            .unwrap_err();
            *panic.downcast::<String>().unwrap()
        }
        "run_until" => format!("{:?}", simulation.run_until(1).unwrap_err()),
        "run_until_bounded" => format!("{:?}", simulation.run_until_bounded(1, 10).unwrap_err()),
        _ => unreachable!(),
    };
    assert!(error.contains("direct primary"), "{action}: {error}");
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains("1!"), "{action}: {text}");
    // $fatal implicitly calls $finish (20.10), including when lazy
    // combinational evaluation was triggered by a waveform API.
    assert!(simulation.is_finished(), "{action}");
    let final_calls = simulation.signal("final_calls");
    assert_eq!(simulation.get(final_calls), 1u8.into(), "{action}");
    simulation.modify(|io| io.set(fail, 0u8)).unwrap();
    simulation.eval_comb().unwrap();
    let value = simulation.signal("value");
    assert_eq!(simulation.get(value), 1u8.into(), "{action}");
    simulation.finish().unwrap();
    assert_eq!(simulation.get(final_calls), 1u8.into(), "{action}");
}

#[test]
fn package_task_dump_target_keeps_lexical_package_owner() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("package-task.vcd");
    let source = format!(
        r#"package p; logic state=1;
        task automatic trace(); $dumpvars(0,state); endtask endpackage
        module Top; logic state=0;
        initial begin $dumpfile("{}"); p::trace(); end endmodule"#,
        path.display()
    );
    let mut simulation = Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
        .build_interpreter()
        .unwrap();
    simulation.run_until(1).unwrap();
    simulation.flush_vcd().unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    assert_eq!(text.matches("$var ").count(), 1, "{text}");
    assert!(text.contains("$scope module p::"), "{text}");
    assert!(text.contains("1!"), "{text}");
}

#[test]
fn lexical_dump_target_shadows_top_module_name_in_child() {
    for declaration in ["logic Top=1;", "Leaf Top();"] {
        for selection in ["Top", "$root.Top.root_value"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("lexical.vcd");
            let source = format!(
                r#"module Leaf; logic value=1; endmodule
                module Child; {}
                initial begin $dumpfile("{}"); $dumpvars(0,{}); end endmodule
                module Top; logic root_value=0; Child child(); endmodule"#,
                declaration,
                path.display(),
                selection
            );
            let mut simulation =
                Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
                    .build_interpreter()
                    .unwrap();
            simulation.run_until(1).unwrap();
            simulation.flush_vcd().unwrap();
            let text = std::fs::read_to_string(path).unwrap();
            assert_eq!(
                text.matches("$var ").count(),
                1,
                "{declaration}, {selection}: {text}"
            );
            if selection == "Top" {
                assert!(!text.contains(" root_value "), "{text}");
                assert!(text.contains("1!"), "{text}");
            } else {
                assert!(text.contains(" root_value "), "{text}");
                assert!(text.contains("0!"), "{text}");
            }
        }
    }
}

#[test]
fn initial_dump_limit_bounds_header_and_resumes_current_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("header-limit.vcd");
    let declarations = (0..100)
        .map(|i| format!("logic signal_{i}=1;\n"))
        .collect::<String>();
    let source = format!(
        r#"module Top; timeunit 1ps; timeprecision 1ps; {}
        initial begin $dumpfile("{}"); $dumplimit(1); $dumpvars;
            #1 $dumpoff; #1 $dumplimit(0); #1 $dumpon; end endmodule"#,
        declarations,
        path.display()
    );
    let mut simulation = Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
        .build_interpreter()
        .unwrap();
    simulation.run_until(0).unwrap();
    simulation.flush_vcd().unwrap();
    let paused = std::fs::read_to_string(&path).unwrap();
    assert!(
        paused.len() < 128,
        "header ignored limit: {} bytes",
        paused.len()
    );
    assert!(paused.contains("VCD dump limit reached"), "{paused}");
    assert!(!paused.contains("$var "), "{paused}");
    simulation.run_until(2).unwrap();
    simulation.flush_vcd().unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), paused);
    simulation.run_until(3).unwrap();
    simulation.flush_vcd().unwrap();
    let resumed = std::fs::read_to_string(path).unwrap();
    assert_eq!(resumed.matches("$date\n").count(), 1, "{resumed}");
    assert_eq!(resumed.matches("$var wire ").count(), 100, "{resumed}");
    assert!(resumed.contains("#3\n$dumpon\n"), "{resumed}");
}

#[test]
fn omitted_dump_depth_preserves_explicit_recursive_selections() {
    for (selection, count) in [("Top.value", 1), ("Top.child", 2), ("p::state", 1)] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("omitted-depth.vcd");
        let source = format!(
            r#"package p; logic state=1; logic other=0; endpackage
            module Grandchild; logic grand_value=1; endmodule
            module Child; logic child_value=1; Grandchild grand(); endmodule
            module Top; logic value=1; logic package_use; Child child();
            assign package_use=p::other;
            initial begin $dumpfile("{}"); $dumpvars(,{}); end endmodule"#,
            path.display(),
            selection
        );
        let mut simulation =
            Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
                .build_interpreter()
                .unwrap();
        simulation.run_until(1).unwrap();
        simulation.flush_vcd().unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert_eq!(
            text.matches("$var wire ").count(),
            count,
            "{selection}: {text}"
        );
        assert!(!text.contains(" other "), "{text}");
        if selection == "Top.child" {
            assert!(text.contains(" grand_value "), "{text}");
        }
        assert_eq!(
            text.contains("$scope module p::"),
            selection == "p::state",
            "{text}"
        );
    }
}

#[test]
fn dumpfile_uses_shared_string_literal_escape_decoding() {
    for kind in ["ordinary", "triple", "lf", "crlf", "triple-crlf"] {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().join("wave");
        let (delimiter, continuation) = match kind {
            "triple" => ("\"\"\"", ""),
            "lf" => ("\"", "\\\n"),
            "crlf" => ("\"", "\\\r\n"),
            "triple-crlf" => ("\"\"\"", "\\\r\n"),
            _ => ("\"", ""),
        };
        let literal = format!(
            "{delimiter}{}{continuation}\\056vcd{delimiter}",
            base.display()
        );
        let source = format!(
            "module Top; logic value=1; initial begin $dumpfile({literal}); $dumpvars(0,Top.value); end endmodule"
        );
        let mut simulation =
            Simulation::from_sv_sources(vec![(&source, Path::new("vcd.sv"))], "Top")
                .build_interpreter()
                .unwrap();
        simulation.run_until(1).unwrap();
        simulation.flush_vcd().unwrap();
        let path = directory.path().join("wave.vcd");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{kind}: {error}, literal={literal:?}"));
        assert!(text.contains("1!"), "{kind}: {text}");
        assert_eq!(
            std::fs::read_dir(directory.path()).unwrap().count(),
            1,
            "{kind}"
        );
    }
}
