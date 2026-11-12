use celox::{RuntimeEvent, SimBackend, Simulation};
use std::path::Path;

const SOURCE: &str = r#"module Top;
 timeunit 1ps; timeprecision 1ps;
 int value; string text;
 initial begin
   value=1; text="first"; $monitor("%s value=%0d",text,value);
   #1 $monitoroff; value=2; text="second";
   #1 $monitoron;
   #1 value=3; text="third";
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
    output(&mut sim, "first value=1");
    sim.run_until(1).unwrap();
    assert!(sim.drain_runtime_events().is_empty());
    let file = sim.save_state().unwrap();
    assert!(sim.load_state(&file).is_err());
    let checkpoint = sim.checkpoint().unwrap();
    let bytes = checkpoint.to_bytes().unwrap();
    // SAFETY: bytes were produced by this unchanged simulation.
    let checkpoint = unsafe { sim.checkpoint_from_bytes(&bytes) }.unwrap();
    for _ in 0..3 {
        sim.run_until(2).unwrap();
        output(&mut sim, "second value=2");
        sim.run_until(3).unwrap();
        output(&mut sim, "third value=3");
        sim.restore(&checkpoint).unwrap();
    }
}
macro_rules! backends {
    ($($name:ident:$method:ident),* $(,)?) => {$ (
        #[test]
        fn $name() {
            verify(Simulation::from_sv_sources(vec![(SOURCE,Path::new("monitor_host.sv"))],"Top").$method().unwrap());
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
    let image = Simulation::from_sv_sources(vec![(SOURCE, Path::new("monitor_host.sv"))], "Top")
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
        Simulation::from_sv_sources(vec![(SOURCE, Path::new("monitor_host.sv"))], "Top")
            .scheduler_mode(celox::SchedulerMode::Differential)
            .build_native()
            .unwrap(),
    );
}
