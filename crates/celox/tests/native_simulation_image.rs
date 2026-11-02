//! A timed `Simulation` built from a native image: the image carries the
//! process kernels and the compiled scheduler, so a serialized image runs as
//! the in-process build does (#1730).
#![cfg(all(
    feature = "systemverilog",
    any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    )
))]

use std::path::Path;

use celox::{
    NativeProgramImage, NativeProgramInstance, RuntimeEvent, SchedulerMode, Simulation,
    SimulatorErrorKind,
};

const PROCESSES: &str = r#"`timescale 1ns/1ns
module Top(output logic [7:0] count, output logic [7:0] seen);
    logic clk = 0;
    always #5 clk = ~clk;
    always_ff @(posedge clk) count <= count + 1;
    initial begin
        count = 0;
        seen = 0;
        #12 $display("at %0t count=%0d", $time, count);
        @(posedge clk) seen = count;
        #20 $display("at %0t seen=%0d count=%0d", $time, seen, count);
    end
endmodule
"#;

fn builder(source: &str) -> celox::SimulatorBuilder<'_, Simulation> {
    Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
}

/// The image, through its container bytes.
fn reloaded(image: &NativeProgramImage) -> NativeProgramImage {
    NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap()).unwrap()
}

/// What a simulation shows up to `end`: its events and outputs.
fn transcript<B: celox::SimBackend>(
    sim: &mut Simulation<B>,
    end: u64,
) -> (Vec<RuntimeEvent>, u64, u64) {
    sim.run_until(end).unwrap();
    let count = sim.get(sim.signal("count")).try_into().unwrap();
    let seen = sim.get(sim.signal("seen")).try_into().unwrap();
    (sim.drain_runtime_events(), count, seen)
}

#[test]
fn a_serialized_native_image_runs_its_processes() {
    let expected = transcript(&mut builder(PROCESSES).build_native().unwrap(), 60);
    assert_eq!(
        expected.0,
        vec![
            RuntimeEvent::Display {
                message: b"at 12 count=1".to_vec()
            },
            RuntimeEvent::Display {
                message: b"at 35 seen=1 count=3".to_vec()
            },
        ]
    );
    let image = reloaded(
        &builder(PROCESSES)
            .compile_native()
            .unwrap()
            .into_program_image(),
    );
    let mut sim = Simulation::from_sv_sources(Vec::new(), "Top")
        .build_native_from_image(image)
        .unwrap();
    assert_eq!(transcript(&mut sim, 60), expected);
    assert!(sim.scheduler_stats().compiled_slots > 0);
    let mut sim = builder(PROCESSES)
        .compile_native()
        .unwrap()
        .initialize()
        .unwrap();
    assert_eq!(transcript(&mut sim, 60), expected);
}

#[test]
fn a_differential_simulation_loads_its_shadow_from_the_image() {
    let image = reloaded(
        &builder(PROCESSES)
            .compile_native()
            .unwrap()
            .into_program_image(),
    );
    let mut sim = Simulation::from_sv_sources(Vec::new(), "Top")
        .scheduler_mode(SchedulerMode::Differential)
        .build_native_from_image(image)
        .unwrap();
    let (events, count, _) = transcript(&mut sim, 60);
    assert_eq!(events.len(), 2);
    assert_eq!(count, 6);
    assert!(sim.scheduler_stats().checked_steps > 0);
}

/// An image from `Simulator` has no compiled scheduler: every slot runs in
/// the reference loop, to the same state as a fresh `Simulation` build.
#[test]
fn an_untimed_image_runs_as_a_simulation_in_the_reference_loop() {
    let source = "module Top(input logic clk, input logic [7:0] step, output logic [7:0] count,
                             output logic [7:0] sum);
        always_ff @(posedge clk) count <= count + 1;
        always_ff @(negedge clk) sum <= sum + count + step;
    endmodule";
    let run = |sim: &mut Simulation<celox::NativeBackend>| {
        let step = sim.signal("step");
        sim.modify(|io| io.set(step, 3u8)).unwrap();
        sim.add_clock("clk", 10, 5);
        sim.run_until(97).unwrap();
        sim.save_state().unwrap()
    };
    let mut fresh = builder(source).build_native().unwrap();
    let expected = run(&mut fresh);
    let image = celox::Simulator::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
        .compile_native()
        .unwrap()
        .into_program_image();
    let mut sim = Simulation::from_sv_sources(Vec::new(), "Top")
        .build_native_from_image(reloaded(&image))
        .unwrap();
    assert_eq!(run(&mut sim), expected);
    assert_eq!(sim.get(sim.signal("count")), 10u8.into());
    assert_eq!(sim.scheduler_stats().compiled_slots, 0);
}

#[test]
fn process_images_do_not_load_as_untimed_programs() {
    let image = builder(PROCESSES)
        .compile_native()
        .unwrap()
        .into_program_image();
    let Err(error) =
        celox::Simulator::from_sv_sources(Vec::new(), "Top").build_native_from_image(image.clone())
    else {
        panic!("a Simulator runs no process");
    };
    assert!(matches!(
        error.kind(),
        SimulatorErrorKind::ProcessRequiresSimulation(_)
    ));
    // SAFETY: the image was compiled from the trusted source above.
    let Err(error) = (unsafe { NativeProgramInstance::from_image(image) }) else {
        panic!("a native program instance runs no process");
    };
    assert!(error.to_string().contains("is a process"), "{error}");
}

#[test]
fn advance_to_stops_before_the_slot_and_its_host_writes_join_it() {
    let source = r#"`timescale 1ns/1ns
module Top(input logic [7:0] value, output logic [7:0] seen);
    initial begin
        seen = 0;
        #5 seen = value;
        $display("at %0t value=%0d", $time, value);
    end
endmodule
"#;
    let image = reloaded(
        &builder(source)
            .compile_native()
            .unwrap()
            .into_program_image(),
    );
    let mut sim = Simulation::from_sv_sources(Vec::new(), "Top")
        .build_native_from_image(image)
        .unwrap();
    sim.advance_to(5).unwrap();
    assert_eq!(sim.time(), 5);
    assert_eq!(sim.get(sim.signal("seen")), 0u8.into());
    assert_eq!(sim.drain_runtime_events(), Vec::new());
    // The write is the host's at time 5, before the process resumes there.
    let value = sim.signal("value");
    sim.modify(|io| io.set(value, 7u8)).unwrap();
    sim.advance_to(5).unwrap();
    assert_eq!(sim.get(sim.signal("seen")), 0u8.into());
    sim.run_until(5).unwrap();
    assert_eq!(sim.get(sim.signal("seen")), 7u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![RuntimeEvent::Display {
            message: b"at 5 value=7".to_vec()
        }]
    );
    sim.advance_to(3).unwrap();
    assert_eq!(sim.time(), 5);
}

/// Element-strided unpacked arrays cannot be dumped: an image compiled
/// without VCD is rejected for a VCD run, as `Simulator` rejects it, and one
/// compiled with VCD records the elements.
#[test]
fn a_vcd_run_needs_an_image_compiled_with_vcd() {
    let source = r#"`timescale 1ns/1ns
module Top;
    logic [2:0] values [4];
    initial begin
        values[2] = 3'd0;
        #1 values[2] = 3'd5;
    end
endmodule
"#;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("top.vcd");
    let strided = builder(source)
        .compile_native()
        .unwrap()
        .into_program_image();
    let Err(error) = Simulation::from_sv_sources(Vec::new(), "Top")
        .vcd(&path)
        .build_native_from_image(reloaded(&strided))
    else {
        panic!("a strided image cannot record VCD");
    };
    assert!(
        error
            .to_string()
            .contains("compile this native image with VCD enabled"),
        "{error}"
    );
    let packed = builder(source)
        .vcd(&path)
        .compile_native()
        .unwrap()
        .into_program_image();
    let mut sim = Simulation::from_sv_sources(Vec::new(), "Top")
        .vcd(&path)
        .build_native_from_image(reloaded(&packed))
        .unwrap();
    sim.run_until(2).unwrap();
    drop(sim);
    let vcd = std::fs::read_to_string(&path).unwrap();
    assert!(vcd.contains("b101"), "{vcd}");
}

/// Deferred assertions keep their queue through the image: a glitch is
/// flushed, a lasting failure reports at the end of its slot.
#[test]
fn deferred_assertions_run_from_the_image() {
    const DEFERRED: &str = r#"`timescale 1ns/1ns
module Top;
    bit a, b;
    initial forever begin
        @(a or b);
        d: assert #0 (a == b) else $error("d");
    end
    initial begin
        #1 a = 1; #0 b = 1;
        #1 a = 0;
        #1 $display("done");
    end
endmodule
"#;
    let run = |sim: &mut Simulation<_>| {
        sim.run_until(5).unwrap();
        sim.drain_runtime_events()
            .into_iter()
            .map(|event| match event {
                RuntimeEvent::Report(report) => report.message,
                RuntimeEvent::Display { message } => message,
                event => panic!("unexpected event {event:?}"),
            })
            .collect::<Vec<_>>()
    };
    let expected = run(&mut builder(DEFERRED).build_native().unwrap());
    assert_eq!(expected, vec![b"d".to_vec(), b"done".to_vec()]);
    let image = reloaded(
        &builder(DEFERRED)
            .compile_native()
            .unwrap()
            .into_program_image(),
    );
    let mut sim = Simulation::from_sv_sources(Vec::new(), "Top")
        .build_native_from_image(image)
        .unwrap();
    assert_eq!(run(&mut sim), expected);
}

/// A native program instance ends no time slots, so an image with sampled
/// values or histories fails to attach.
#[test]
fn a_native_program_instance_rejects_sampled_values() {
    let source = "module Top(input bit clk, input bit a, output bit r);\n  \
                  always_ff @(posedge clk) r <= $past(a);\nendmodule";
    let image = celox::Simulator::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
        .compile_native()
        .unwrap()
        .into_program_image();
    // SAFETY: the image was compiled from the trusted source above.
    let Err(error) = (unsafe { NativeProgramInstance::from_image(image) }) else {
        panic!("a native program instance ends no slots");
    };
    assert!(
        error.to_string().contains("does not end time slots"),
        "{error}"
    );
}

#[test]
fn raw_wait_snapshots_survive_native_image_reload() {
    let source = r#"`timescale 1ns/1ns
module Top;
    bit [1:0] phase = 0;
    logic [69:0] data = 0;
    bit other = 0;
    int count = 0;
    always #1 begin
        phase = phase + 1;
        data = phase == 1 ? 'x : (phase == 2 ? 'z : 70'd7);
    end
    initial forever begin
        @(data or other); count = count + 1;
        #1;
        @(data or other); count = count + 1;
    end
    initial forever begin #2; other = !other; end
    initial #12 $finish;
endmodule"#;
    for level in [celox::OptLevel::O0, celox::OptLevel::O2] {
        for four_state in [false, true] {
            let build = || {
                builder(source)
                    .four_state(four_state)
                    .opt_level(level)
                    .live_signal(Vec::new(), vec!["count".into()])
            };
            let image = reloaded(&build().compile_native().unwrap().into_program_image());
            let mut expected = build()
                .scheduler_mode(SchedulerMode::Reference)
                .build_native()
                .unwrap();
            let mut actual = Simulation::from_sv_sources(Vec::new(), "Top")
                .four_state(four_state)
                .scheduler_mode(SchedulerMode::DifferentialRuns)
                .build_native_from_image(image)
                .unwrap();
            for end in [2, 3, 9, 12] {
                expected.run_until(end).unwrap();
                actual.run_until(end).unwrap();
                assert_eq!(actual.time(), expected.time());
                assert_eq!(
                    actual.get(actual.signal("count")),
                    expected.get(expected.signal("count"))
                );
            }
            assert!(actual.is_finished());
            let stats = actual.scheduler_stats();
            assert!(stats.compiled_slots > 0, "{stats:?}");
            assert!(stats.checked_steps > 0, "{stats:?}");
        }
    }
}

#[test]
fn reactive_waits_survive_gated_clock_image_reload() {
    let source = r#"`timescale 1ns/1ns
module Top(input bit enabled);
    bit clk = 0;
    wire gated = clk & enabled;
    bit [7:0] primary = 0;
    bit [69:0] data = 0;
    wire [69:0] expression = data + data;
    bit [69:0] middle = 0;
    int count = 0;
    always #1 clk = ~clk;
    always @(posedge clk) primary <= primary + 1;
    always @(posedge gated) data <= data + primary + 1;
    initial forever begin @(expression); middle = expression; end
    initial forever begin @(middle); count = count + 1; end
    initial #13 $finish;
endmodule"#;
    for level in [celox::OptLevel::O0, celox::OptLevel::O2] {
        for four_state in [false, true] {
            let build = || builder(source).four_state(four_state).opt_level(level);
            let image = reloaded(&build().compile_native().unwrap().into_program_image());
            let mut expected = build()
                .scheduler_mode(SchedulerMode::Reference)
                .build_native()
                .unwrap();
            let mut actual = Simulation::from_sv_sources(Vec::new(), "Top")
                .four_state(four_state)
                .scheduler_mode(SchedulerMode::DifferentialRuns)
                .build_native_from_image(image)
                .unwrap();
            for simulation in [&mut expected, &mut actual] {
                let enabled = simulation.signal("enabled");
                simulation.modify(|io| io.set(enabled, 1u8)).unwrap();
            }
            for end in [2, 3, 7, 12] {
                expected.run_until(end).unwrap();
                actual.run_until(end).unwrap();
                assert_eq!(actual.time(), expected.time());
                for name in ["data", "middle", "count"] {
                    assert_eq!(
                        actual.get(actual.signal(name)),
                        expected.get(expected.signal(name)),
                        "{name}"
                    );
                }
            }
            assert_eq!(
                actual.get(actual.signal("count")),
                num_bigint::BigUint::from(6u8)
            );
            let stats = actual.scheduler_stats();
            assert!(
                stats.compiled_slots > 0 && stats.checked_steps > 0,
                "{stats:?}"
            );
            assert!(
                stats
                    .kernels
                    .iter()
                    .all(|kernel| matches!(kernel, celox::KernelSchedule::Compiled)),
                "{stats:?}"
            );
        }
    }
}

#[test]
fn native_file_calls_keep_output_order_without_reference_slots() {
    let source = r#"`timescale 1ns/1ns
module Top(input bit enabled);
    bit clk = 0;
    int count = 0;
    logic [69:0] unknown_data = 'z;
    real fraction = 1.5;
    shortint negative = -3;
    always #1 clk = ~clk;
    always @(posedge clk) count <= count + 1;
    always @(posedge clk) if (enabled) begin
        $display("before:%0d", count);
        $fwrite(1, "file:%0d:%h:%0.2f:%0d:%0t", count, unknown_data, fraction, negative, $time);
        $display("after:%0d", count);
    end
    initial #13 $finish;
endmodule"#;
    for level in [celox::OptLevel::O0, celox::OptLevel::O2] {
        for four_state in [false, true] {
            let build = || builder(source).four_state(four_state).opt_level(level);
            let image = reloaded(&build().compile_native().unwrap().into_program_image());
            let mut expected = build()
                .scheduler_mode(SchedulerMode::Reference)
                .build_native()
                .unwrap();
            let mut actual = Simulation::from_sv_sources(Vec::new(), "Top")
                .four_state(four_state)
                .scheduler_mode(SchedulerMode::Compiled)
                .build_native_from_image(image)
                .unwrap();
            for end in [4, 6, 8, 12] {
                if end == 6 {
                    for simulation in [&mut expected, &mut actual] {
                        let enabled = simulation.signal("enabled");
                        simulation.modify(|io| io.set(enabled, 1u8)).unwrap();
                    }
                }
                expected.run_until(end).unwrap();
                actual.run_until(end).unwrap();
                assert_eq!(
                    actual.drain_runtime_events(),
                    expected.drain_runtime_events()
                );
                assert_eq!(
                    actual.get(actual.signal("count")),
                    expected.get(expected.signal("count"))
                );
            }
            let stats = actual.scheduler_stats();
            assert!(stats.compiled_slots > 0, "{stats:?}");
            assert!(
                stats
                    .kernels
                    .iter()
                    .all(|kernel| matches!(kernel, celox::KernelSchedule::Compiled)),
                "{stats:?}"
            );
        }
    }
}

#[test]
fn native_file_calls_use_each_instances_file_table() {
    let source = r#"`timescale 1ns/1ns
module Top(input bit [1023:0] filename, input bit enabled);
    bit clk = 0;
    int count = 0;
    int fd;
    always #1 clk = ~clk;
    always @(posedge clk) count <= count + 1;
    initial fd = $fopen(filename, "w");
    always @(posedge clk) if (enabled) $fdisplay(fd, "%0d", count);
    initial begin #12; $fclose(fd); end
endmodule"#;
    let directory = tempfile::tempdir().unwrap();
    for level in [celox::OptLevel::O0, celox::OptLevel::O2] {
        for four_state in [false, true] {
            let image = reloaded(
                &builder(source)
                    .four_state(four_state)
                    .opt_level(level)
                    .compile_native()
                    .unwrap()
                    .into_program_image(),
            );
            let mut simulations = [image.clone(), image].map(|image| {
                Simulation::from_sv_sources(Vec::new(), "Top")
                    .four_state(four_state)
                    .build_native_from_image(image)
                    .unwrap()
            });
            let paths = [
                directory.path().join("first"),
                directory.path().join("second"),
            ];
            for (index, (simulation, path)) in simulations.iter_mut().zip(&paths).enumerate() {
                let filename = simulation.signal("filename");
                let enabled = simulation.signal("enabled");
                simulation
                    .modify(|io| {
                        io.set_wide(
                            filename,
                            num_bigint::BigUint::from_bytes_be(path.to_str().unwrap().as_bytes()),
                        );
                        io.set(enabled, u8::from(index == 0));
                    })
                    .unwrap();
                simulation.run_until(6).unwrap();
            }
            let enabled = simulations[1].signal("enabled");
            simulations[1].modify(|io| io.set(enabled, 1u8)).unwrap();
            for simulation in &mut simulations {
                simulation.run_until(12).unwrap();
                let stats = simulation.scheduler_stats();
                assert!(stats.compiled_slots > 0, "{stats:?}");
                assert!(
                    stats
                        .kernels
                        .iter()
                        .filter(|kernel| matches!(kernel, celox::KernelSchedule::Compiled))
                        .count()
                        >= 2,
                    "{stats:?}"
                );
            }
            assert_eq!(std::fs::read(&paths[0]).unwrap(), b"0\n1\n2\n3\n4\n5\n");
            assert_eq!(std::fs::read(&paths[1]).unwrap(), b"3\n4\n5\n");
        }
    }
}

#[test]
fn native_file_call_errors_stop_before_nba_publication() {
    let source = r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    int count = 0;
    always #1 clk = ~clk;
    always @(posedge clk) count <= count + 1;
    always @(posedge clk) if (count == 1) $fwrite(32'h8000ffff, "invalid");
endmodule"#;
    for level in [celox::OptLevel::O0, celox::OptLevel::O2] {
        for four_state in [false, true] {
            let mut results = Vec::new();
            for mode in [SchedulerMode::Reference, SchedulerMode::Compiled] {
                let mut simulation = builder(source)
                    .four_state(four_state)
                    .opt_level(level)
                    .scheduler_mode(mode)
                    .build_native()
                    .unwrap();
                let error = simulation.run_until(8).unwrap_err();
                results.push((
                    error.to_string(),
                    simulation.time(),
                    simulation.get(simulation.signal("count")),
                ));
                if mode == SchedulerMode::Compiled {
                    assert!(
                        simulation.scheduler_stats().compiled_slots > 0,
                        "{:?}",
                        simulation.scheduler_stats()
                    );
                }
            }
            assert_eq!(results[0], results[1]);
            assert!(results[1].0.contains("file output"), "{:?}", results[1]);
            assert_eq!(results[1].1, 3);
            assert_eq!(results[1].2, num_bigint::BigUint::from(1u8));
        }
    }
}

#[test]
fn a_level_sensitive_kernels_startup_fence_does_not_disable_native_slots() {
    let source = r#"`timescale 1ns/1ns
module Top;
    logic [7:0] count = 0, seen = 0;
    logic clk = 0;
    always #1 clk = ~clk;
    always_ff @(posedge clk) count <= count + 1;
    always @(count[7:0]) seen = count ^ 8'h5a;
endmodule
"#;
    for four_state in [false, true] {
        for optimize in [false, true] {
            let make = || builder(source).four_state(four_state).optimize(optimize);
            let mut reference = make()
                .scheduler_mode(SchedulerMode::Reference)
                .build_native()
                .unwrap();
            let image = reloaded(&make().compile_native().unwrap().into_program_image());
            let mut native = Simulation::from_sv_sources(Vec::new(), "Top")
                .build_native_from_image(image)
                .unwrap();
            for end in [0, 1, 2, 5, 20] {
                native.run_until(end).unwrap();
                reference.run_until(end).unwrap();
                for name in ["count", "seen"] {
                    assert_eq!(
                        native.get(native.signal(name)),
                        reference.get(reference.signal(name))
                    );
                }
            }
            assert!(
                native
                    .scheduler_stats()
                    .kernels
                    .iter()
                    .all(|kernel| matches!(kernel, celox::KernelSchedule::Compiled)),
                "{:?}",
                native.scheduler_stats()
            );
            assert!(native.scheduler_stats().compiled_slots > 0);
        }
    }
}

#[test]
fn source_delays_inside_level_sensitive_bodies_keep_their_scheduler_boundary() {
    let source = r#"`timescale 1ns/1ns
module Top;
    logic [7:0] count = 0, seen = 0;
    logic clk = 0;
    always #1 clk = ~clk;
    always_ff @(posedge clk) count <= count + 1;
    always @(count) begin #0; seen = count; end
endmodule
"#;
    let mut sim = builder(source)
        .four_state(false)
        .scheduler_mode(SchedulerMode::DifferentialRuns)
        .build_native()
        .unwrap();
    sim.run_until(20).unwrap();
    assert!(
        sim.scheduler_stats()
            .kernels
            .iter()
            .any(|kernel| matches!(kernel, celox::KernelSchedule::Reference(_)))
    );
    assert_eq!(sim.get(sim.signal("seen")), 10u8.into());
}

#[test]
fn native_queued_nbas_preserve_issue_order_and_waiter_occurrences() {
    let source = r#"`timescale 1ns/1ns
module Top;
    logic clk = 0, reset = 1;
    logic [7:0] q = 0, sampled = 0, seen = 0;
    int changes = 0;
    always #1 clk = ~clk;
    always_ff @(posedge clk) sampled <= q;
    initial forever begin
        @(posedge clk or negedge reset);
        q <= q + 1;
        q <= q + 2;
    end
    initial forever begin @(q); seen = q; changes = changes + 1; end
endmodule
"#;
    for four_state in [false, true] {
        for optimize in [false, true] {
            let make = || builder(source).four_state(four_state).optimize(optimize);
            let mut reference = make()
                .scheduler_mode(SchedulerMode::Reference)
                .build_native()
                .unwrap();
            let image = reloaded(&make().compile_native().unwrap().into_program_image());
            let mut native = Simulation::from_sv_sources(Vec::new(), "Top")
                .build_native_from_image(image)
                .unwrap();
            for end in [0, 1, 2, 5, 10] {
                native.run_until(end).unwrap();
                reference.run_until(end).unwrap();
                for name in ["q", "sampled", "seen", "changes"] {
                    assert_eq!(
                        native.get(native.signal(name)),
                        reference.get(reference.signal(name)),
                        "{name} at {end}"
                    );
                }
            }
            assert_eq!(native.get(native.signal("q")), 10u8.into());
            assert_eq!(native.get(native.signal("sampled")), 8u8.into());
            assert!(native.scheduler_stats().compiled_slots > 0);
            assert!(
                native
                    .scheduler_stats()
                    .kernels
                    .iter()
                    .all(|kernel| matches!(kernel, celox::KernelSchedule::Compiled)),
                "{:?}",
                native.scheduler_stats()
            );
        }
    }
}

#[test]
fn native_queued_nbas_survive_an_early_host_error_in_the_saved_state() {
    let source = r#"`timescale 1ns/1ns
module Top;
    logic clk = 0;
    logic [7:0] q = 0;
    always #1 clk = ~clk;
    initial forever begin
        @(posedge clk);
        q <= q + 1;
        q <= q + 2;
        $fwrite(123, "invalid descriptor");
    end
endmodule
"#;
    for four_state in [false, true] {
        for optimize in [false, true] {
            let make = || builder(source).four_state(four_state).optimize(optimize);
            let mut reference = make()
                .scheduler_mode(SchedulerMode::Reference)
                .build_native()
                .unwrap();
            let mut native = make().build_native().unwrap();
            assert!(reference.run_until(2).is_err());
            assert!(native.run_until(2).is_err());
            assert_eq!(native.time(), 1);
            assert_eq!(native.get(native.signal("q")), 0u8.into());
            assert_eq!(
                native.save_state().unwrap(),
                reference.save_state().unwrap()
            );
            assert!(native.scheduler_stats().compiled_slots > 0);
            assert!(
                native
                    .scheduler_stats()
                    .kernels
                    .iter()
                    .all(|kernel| matches!(kernel, celox::KernelSchedule::Compiled))
            );
        }
    }
}
