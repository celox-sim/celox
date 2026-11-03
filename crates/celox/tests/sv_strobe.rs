#![cfg(feature = "systemverilog")]
use celox::{RuntimeEvent, SimBackend, Simulation, StateFile};
use std::path::Path;

const SOURCE: &str = r#"module Top;
 timeunit 1ps; timeprecision 1ps;
 int value;
 initial begin
   value=1; $strobe("late=%0d",value);
   #1 value=2; $strobe("late=%0d",value);
   #1 value=3;
 end
endmodule"#;

fn output<B: SimBackend>(sim: &mut Simulation<B>, expected: &str) {
    assert_eq!(
        sim.drain_runtime_events(),
        vec![RuntimeEvent::Display {
            message: expected.as_bytes().to_vec()
        }]
    );
}
fn verify<B: SimBackend>(mut sim: Simulation<B>) {
    sim.run_until(0).unwrap();
    output(&mut sim, "late=1");
    let checkpoint = sim.checkpoint().unwrap();
    let file = sim.save_state().unwrap();
    let mut bytes = Vec::new();
    file.write_to(&mut bytes).unwrap();
    let file = StateFile::read_from(bytes.as_slice()).unwrap();
    for _ in 0..3 {
        sim.run_until(1).unwrap();
        output(&mut sim, "late=2");
        sim.restore(&checkpoint).unwrap();
    }
    sim.load_state(&file).unwrap();
    sim.run_until(1).unwrap();
    output(&mut sim, "late=2");
}
macro_rules! backends {
    ($($name:ident:$method:ident),* $(,)?) => {$ (
        #[test]
        fn $name() {
            verify(Simulation::from_sv_sources(vec![(SOURCE,Path::new("strobe_host.sv"))],"Top").$method().unwrap());
        }
    )*};
}
backends!(interpreter:build_interpreter);
#[cfg(feature = "cranelift")]
backends!(cranelift:build_cranelift);
#[cfg(feature = "wasm-runtime")]
backends!(wasm:build_wasm);
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
backends!(native:build_native,tiered:build_tiered);

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn native_image() {
    let image = Simulation::from_sv_sources(vec![(SOURCE, Path::new("strobe_host.sv"))], "Top")
        .compile_native()
        .unwrap()
        .into_program_image();
    let image =
        celox::NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap())
            .unwrap();
    verify(
        Simulation::from_sv_sources(Vec::new(), "Top")
            .build_native_from_image(image)
            .unwrap(),
    );
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn differential_scheduler() {
    verify(
        Simulation::from_sv_sources(vec![(SOURCE, Path::new("strobe_host.sv"))], "Top")
            .scheduler_mode(celox::SchedulerMode::Differential)
            .build_native()
            .unwrap(),
    );
}
