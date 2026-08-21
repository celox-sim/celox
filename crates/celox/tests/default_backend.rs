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

#[test]
fn cross_codegen_can_capture_native_ir_without_loading_target_code() {
    let code = r#"
        module Top (o: output logic<8>) {
            assign o = 8'h3c;
        }
    "#;

    let (_compiled, trace) = Simulator::builder(code, "Top")
        .trace_pre_optimized_sir()
        .trace_post_optimized_sir()
        .trace_mir()
        .compile_native_with_trace()
        .unwrap();

    assert!(trace.pre_optimized_sir.is_some());
    assert!(trace.post_optimized_sir.is_some());
    assert!(trace.native_optimized_sir.is_some());
    assert!(trace.mir.is_some());
    #[cfg(all(target_arch = "x86_64", feature = "arm64-codegen"))]
    assert!(
        trace
            .mir
            .as_deref()
            .unwrap()
            .contains("AArch64 disassembly of emitted function:")
    );
    #[cfg(all(target_arch = "aarch64", feature = "x86_64-codegen"))]
    assert!(
        trace
            .mir
            .as_deref()
            .unwrap()
            .contains("x86-64 disassembly of emitted function:")
    );
    assert!(trace.reactive_event_graph.is_some());
    assert!(trace.native_state_layout.is_some());
}
