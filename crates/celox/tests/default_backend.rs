//! The cross-codegen feature must not replace the host-executable default backend.
#![cfg(any(
    all(target_arch = "x86_64", feature = "arm64-codegen"),
    all(target_arch = "aarch64", feature = "x86_64-codegen")
))]

use celox::Simulator;

#[test]
fn cross_codegen_keeps_default_simulator_executable_on_the_host() {
    let code = r#"
        module Top (o: output logic<8>) {
            assign o = 8'h3c;
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let output = sim.signal("o");
    assert_eq!(sim.get(output), 0x3cu64.into());
}
