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

#[test]
fn cross_codegen_defaults_keep_unpacked_arrays_strided() {
    let source = r#"
        module Top (clk: input clock, index: input logic<3>, q: output logic<20>) {
            var mem: logic<20> [6];
            always_ff (clk) { mem[index] = 20'd17; }
            assign q = mem[index];
        }
    "#;
    fn run<B: celox::SimBackend>(mut sim: celox::Simulator<B>) {
        assert!(sim.signal("mem").array_layout.is_some());
        let clk = sim.event("clk");
        let index = sim.signal("index");
        sim.modify(|io| io.set(index, 5u8)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(
            sim.get_four_state(sim.signal("q")),
            (17u8.into(), 0u8.into())
        );
    }
    run(Simulator::builder(source, "Top")
        .four_state(true)
        .build()
        .unwrap());
    run(Simulator::builder(source, "Top")
        .four_state(true)
        .build_with_trace()
        .res
        .unwrap());
    run(Simulator::builder(source, "Top")
        .four_state(true)
        .build_tiered()
        .unwrap());
}
