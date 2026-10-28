//! Native clock-cone specialization, including its optional image exports.
#![cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]

use celox::{NativeBackend, SharedNativeCode};
use celox::{NativeProgramImage, SimBackend, Simulator};
use std::sync::Arc;

const SOURCE: &str = r#"
module Child (clk: input clock, d: input bit<8>, q: output bit<8>) {
    always_ff (clk) { q = d; }
}
module Top (clk: input clock, d: input bit<8>, q: output bit<8>) {
    inst child: Child (clk, d, q);
}
"#;

fn image(source: &str, four_state: bool) -> NativeProgramImage {
    Simulator::builder(source, "Top")
        .four_state(four_state)
        .compile_native()
        .unwrap()
        .into_program_image()
}

fn exports(image: &NativeProgramImage) -> usize {
    image
        .code_entries()
        .iter()
        .filter(|entry| entry.name.starts_with("clock_alias_update["))
        .count()
}

#[test]
fn clock_wiring_projection_matches_full_settle_after_image_roundtrip() {
    let image = image(SOURCE, false);
    assert_eq!(exports(&image), 1);
    let image =
        NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap()).unwrap();
    // SAFETY: this test just compiled and serialized the image.
    let shared = Arc::new(unsafe { SharedNativeCode::from_image(image) }.unwrap());
    let mut fast = NativeBackend::from_shared(shared.clone());
    let mut full = NativeBackend::from_shared(shared);
    let event = fast.id_to_event_slice()[0];
    use celox::EventHandle;
    let clock = fast.resolve_signal(&event.addr());
    fast.eval_comb().unwrap();
    full.eval_comb().unwrap();
    for value in [1u8, 0, 1, 0] {
        fast.clear_triggered_bits();
        full.clear_triggered_bits();
        fast.set(clock, value);
        full.set(clock, value);
        assert!(fast.eval_clock_aliases(event).unwrap());
        full.eval_comb().unwrap();
        assert_eq!(fast.get_triggered_bits(), full.get_triggered_bits());
        for (addr, &offset) in &fast.layout().offsets {
            let width = fast.layout().plane_size(addr);
            let bytes = |backend: &NativeBackend| {
                let (ptr, _) = backend.memory_as_ptr();
                // SAFETY: layout ranges describe live signal storage. Exclude
                // host pointers and scratch memory, which differ per instance.
                unsafe { std::slice::from_raw_parts(ptr.add(offset), width) }.to_vec()
            };
            assert_eq!(bytes(&fast), bytes(&full), "{addr:?}");
        }
    }
}

#[test]
fn falling_edges_publish_aliases_and_dirty_inputs_still_settle() {
    let mut sim = Simulator::builder(SOURCE, "Top")
        .four_state(false)
        .build_native()
        .unwrap();
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");
    let child_clk = sim.child_signal(&[("child", 0)], "clk");
    for value in [17u8, 29, 255] {
        // Leave this data write dirty: the projection must not suppress it.
        sim.set(d, value);
        sim.active_edge(clk).unwrap();
        assert_eq!(sim.get(q), value.into());
        assert_eq!(sim.get(child_clk), 1u8.into());
        sim.inactive_edge(clk).unwrap();
        // Inspect the backend directly so this assertion cannot cause a
        // deferred full settle and hide a missing alias publication.
        assert_eq!(sim.backend_ref().get(child_clk), 0u8.into());
        // Already settled rising edge takes the specialized path.
        sim.active_edge(clk).unwrap();
        assert_eq!(sim.get(q), value.into());
        sim.inactive_edge(clk).unwrap();
    }
}

#[test]
fn clock_data_dependencies_and_four_state_keep_full_settle() {
    assert_eq!(exports(&image(SOURCE, true)), 0);
    let source = SOURCE.replace(
        "inst child: Child (clk, d, q);",
        "var data: bit<8>; assign data = if (clk as u8) != 0 ? d : 0; inst child: Child (clk, d: data, q);"
    );
    assert_eq!(exports(&image(&source, false)), 0);
}

#[test]
fn coincident_clock_moves_publish_both_wiring_cones() {
    let source = r#"
        module Counter (clk: input clock, q: output bit<8>) {
            always_ff (clk) { q += 1; }
        }
        module Top (
            clk_a: input 'fast clock, clk_b: input 'slow clock,
            a: output 'fast bit<8>, b: output 'slow bit<8>,
        ) {
            inst first: Counter (clk: clk_a, q: a);
            inst second: Counter (clk: clk_b, q: b);
        }
    "#;
    let compiled = Simulator::builder(source, "Top")
        .four_state(false)
        .compile_native()
        .unwrap();
    assert_eq!(exports(compiled.program_image()), 2);
    let mut sim = compiled.initialize().unwrap();
    let a = sim.event("clk_a");
    let b = sim.event("clk_b");
    use celox_runtime::Edge::{Falling, Rising};
    for expected in 1..=4u8 {
        sim.edges(&[(a, Rising), (b, Rising)]).unwrap();
        assert_eq!(sim.get(sim.signal("a")), expected.into());
        assert_eq!(sim.get(sim.signal("b")), expected.into());
        sim.edges(&[(b, Falling), (a, Falling)]).unwrap();
        for child in ["first", "second"] {
            let clk = sim.child_signal(&[(child, 0)], "clk");
            assert_eq!(sim.backend_ref().get(clk), 0u8.into());
        }
    }
}

#[test]
fn unrelated_combinational_loop_does_not_prevent_clock_specialization() {
    let source = SOURCE.replace(
        "always_ff (clk) { q = d; }",
        r#"
            var sum: bit<8>;
            always_comb {
                sum = 0;
                for i in 0..1024 {
                    if i <: (d as u32) { sum += 1; }
                }
            }
            always_ff (clk) { q = sum; }
        "#,
    );
    let compiled = Simulator::builder(&source, "Top")
        .four_state(false)
        .compile_native()
        .unwrap();
    assert_eq!(exports(compiled.program_image()), 1);
    let mut sim = compiled.initialize().unwrap();
    let clk = sim.event("clk");
    let d = sim.signal("d");
    for expected in [3u8, 99, 255] {
        sim.modify(|io| io.set(d, expected)).unwrap();
        sim.active_edge(clk).unwrap();
        assert_eq!(sim.get(sim.signal("q")), expected.into());
        sim.inactive_edge(clk).unwrap();
    }
}
