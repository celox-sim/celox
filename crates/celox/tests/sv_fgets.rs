#![cfg(feature = "systemverilog")]

use celox::{SimBackend, Simulation};
use std::path::Path;

#[path = "test_utils/file_system.rs"]
mod file_system;

const SOURCE: &str = r#"
module Top(output bit ok);
  timeunit 1ps; timeprecision 1ps;
  int fd,n;
  string line;
  initial begin
    fd=$fopen("lines","w");
    $fwrite(fd,"first\nsecond\n");
    $fclose(fd);
    fd=$fopen("lines","r");
    n=$fgets(line,fd);
    ok=n==6 && line=="first\n";
    #1 n=$fgets(line,fd);
    ok &= n==7 && line=="second\n";
    n=$fgets(line,fd);
    ok &= n==0 && line=="second\n";
    $fclose(fd);
    if (!ok) $fatal(1,"fgets failed");
  end
endmodule
"#;

fn verify<B: SimBackend>(mut sim: Simulation<B>) {
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("ok")), 1u8.into());
    let core = sim.checkpoint().unwrap();
    let effects = sim.file_io().snapshot();
    for _ in 0..3 {
        sim.run_until(1).unwrap();
        assert_eq!(sim.get(sim.signal("ok")), 1u8.into());
        let restored = sim.file_io().prepare_restore(&effects).unwrap();
        sim.replace_file_io(restored);
        sim.restore(&core).unwrap();
    }
}

macro_rules! backends {
    ($($name:ident:$method:ident),* $(,)?) => {$ (
        #[test]
        fn $name() {
            verify(Simulation::from_sv_sources(vec![(SOURCE,Path::new("fgets_host.sv"))],"Top")
                .file_system(|| Box::<file_system::TestIo>::default()).$method().unwrap());
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
    let image = Simulation::from_sv_sources(vec![(SOURCE, Path::new("fgets_image.sv"))], "Top")
        .compile_native()
        .unwrap()
        .into_program_image();
    let image =
        celox::NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap())
            .unwrap();
    verify(
        Simulation::from_sv_sources(Vec::new(), "Top")
            .file_system(|| Box::<file_system::TestIo>::default())
            .build_native_from_image(image)
            .unwrap(),
    );
}
