//! Native images must resolve floating-point helpers in the loading process.
#![cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]

use celox::{NativeProgramImage, Simulator};

#[test]
fn floating_point_image_executes_in_a_fresh_process() {
    const IMAGE_PATH: &str = "CELOX_TEST_FLOAT_NATIVE_IMAGE_PATH";
    // A subprocess recursion marker scoped to this test, not a simulator option.
    #[allow(clippy::disallowed_methods)]
    let image_path = std::env::var_os(IMAGE_PATH);
    if let Some(image_path) = image_path {
        let image =
            NativeProgramImage::from_container_bytes(&std::fs::read(image_path).unwrap()).unwrap();
        // Loading requires neither the original source nor its compiler/JIT state.
        let mut sim = Simulator::from_sources(Vec::new(), "Top")
            .build_native_from_image(image)
            .unwrap();
        let a = sim.signal("a");
        let comb = sim.signal("comb");
        let narrow = sim.signal("narrow");
        let captured = sim.signal("captured");
        let clk = sim.event("clk");
        for (input, expected) in [(7u32, 4u32), (0xfffffff9, 0xfffffffc)] {
            sim.modify(|io| io.set(a, input)).unwrap();
            assert_eq!(sim.get(comb), expected.into());
            assert_eq!(sim.get(narrow), expected.into());
            sim.tick(clk).unwrap();
            assert_eq!(sim.get(captured), expected.into());
            let message = if input == 7 {
                b"wide=3.50 narrow=3.50".as_slice()
            } else {
                b"wide=-3.50 narrow=-3.50".as_slice()
            };
            assert_eq!(
                sim.drain_runtime_events(),
                vec![celox::RuntimeEvent::Display {
                    message: message.to_vec(),
                }],
            );
        }
        return;
    }

    let source = r#"
        module Top(clk: input clock, a: input i32, comb: output i32,
                   narrow: output i32, captured: output i32) {
            assign comb = ((a as f64) / 2.0) as i32;
            assign narrow = ((a as f32) / (2 as f32)) as i32;
            always_ff (clk) {
                captured = ((a as f64) / 2.0) as i32;
                $display("wide=%.2f narrow=%.2f", (a as f64) / 2.0,
                         (a as f32) / (2 as f32));
            }
        }
    "#;
    let directory = tempfile::tempdir().unwrap();
    let image_path = directory.path().join("floating.native-image");
    Simulator::builder(source, "Top")
        .compile_native()
        .unwrap()
        .write_image(&image_path)
        .unwrap();
    // Exec a new copy of the test binary so ASLR and runtime helper addresses
    // are independent of the process that compiled and serialized the code.
    let current = std::thread::current();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            current.name().expect("named test thread"),
            "--test-threads=1",
            "--nocapture",
        ])
        .env(IMAGE_PATH, &image_path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "fresh-process native image execution failed: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
