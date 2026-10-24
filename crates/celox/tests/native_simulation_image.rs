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
