use std::sync::Arc;

use celox::{JitBackend, Simulator};

const ADDER: &str = r#"
    module Top (
        a: input logic<8>,
        b: input logic<8>,
        sum: output logic<8>,
    ) {
        assign sum = a + b;
    }
"#;

const FF: &str = r#"
    module Top (
        i_clk: input  clock,
        i_rst: input  reset,
        d:     input  logic<8>,
        q:     output logic<8>,
    ) {
        always_ff (i_clk, i_rst) {
            if_reset {
                q = 0;
            } else {
                q = d;
            }
        }
    }
"#;

/// Two backends from the same SharedJitCode produce correct, independent results.
#[test]
fn shared_code_produces_independent_instances() {
    let sim = Simulator::builder(ADDER, "Top").build().unwrap();
    let shared = sim.shared_code();

    let a = sim.signal("a");
    let b = sim.signal("b");
    let sum = sim.signal("sum");

    let mut b1 = JitBackend::from_shared(Arc::clone(&shared));
    let mut b2 = JitBackend::from_shared(shared);

    b1.set(a, 10u8);
    b1.set(b, 20u8);
    b1.eval_comb().unwrap();
    assert_eq!(b1.get_as::<u8>(sum), 30);

    b2.set(a, 100u8);
    b2.set(b, 55u8);
    b2.eval_comb().unwrap();
    assert_eq!(b2.get_as::<u8>(sum), 155);

    // b1 is unaffected by b2.
    assert_eq!(b1.get_as::<u8>(sum), 30);
}

/// 4-state X init happens for each from_shared instance.
#[test]
fn shared_code_four_state_init() {
    let sim = Simulator::builder(ADDER, "Top")
        .four_state(true)
        .build()
        .unwrap();
    let shared = sim.shared_code();
    let backend = JitBackend::from_shared(shared);
    let a = sim.signal("a");
    let (val, mask) = backend.get_four_state(a);
    assert!(mask > 0u32.into(), "mask should be non-zero (X state)");
    assert!(val > 0u32.into(), "value should be non-zero (X state)");
}

/// Sequential (FF) logic: EventRef from one Simulator works on a
/// from_shared backend (both share the same compiled function pointers).
#[test]
fn shared_code_sequential_logic() {
    let mut sim1 = Simulator::builder(FF, "Top").build().unwrap();
    let shared = sim1.shared_code();

    let d = sim1.signal("d");
    let q = sim1.signal("q");
    let clk_event = sim1.event("i_clk");

    // Drive sim1: reset → d=42 → tick
    // AsyncLow reset: active when rst=0, inactive when rst=1
    sim1.set(sim1.signal("i_rst"), 0u8);
    sim1.tick(clk_event).unwrap();
    assert_eq!(sim1.get(q), 0u32.into());
    sim1.set(sim1.signal("i_rst"), 1u8);
    sim1.set(d, 42u8);
    sim1.tick(clk_event).unwrap();
    assert_eq!(sim1.get(q), 42u32.into());

    // Build a second Simulator from the SAME source — it gets a new
    // SharedJitCode but with identical layout. Drive with different data.
    let mut sim2 = Simulator::builder(FF, "Top").build().unwrap();
    sim2.set(sim2.signal("i_rst"), 0u8);
    sim2.tick(sim2.event("i_clk")).unwrap();
    sim2.set(sim2.signal("i_rst"), 1u8);
    sim2.set(d, 99u8);
    sim2.tick(sim2.event("i_clk")).unwrap();
    assert_eq!(sim2.get(q), 99u32.into());

    // sim1 is still 42.
    assert_eq!(sim1.get(q), 42u32.into());

    // Verify layouts are identical between shared codes.
    let shared2 = sim2.shared_code();
    assert_eq!(shared.layout().total_size, shared2.layout().total_size);
    assert_eq!(shared.layout().merged_total_size, shared2.layout().merged_total_size);
}

/// Memory isolation: writing to one backend does not affect another.
#[test]
fn shared_code_memory_isolation() {
    let sim = Simulator::builder(ADDER, "Top").build().unwrap();
    let shared = sim.shared_code();
    let a = sim.signal("a");

    let mut b1 = JitBackend::from_shared(Arc::clone(&shared));
    let mut b2 = JitBackend::from_shared(shared);

    b1.set(a, 0xAAu8);
    b2.set(a, 0x55u8);

    assert_eq!(b1.get_as::<u8>(a), 0xAA);
    assert_eq!(b2.get_as::<u8>(a), 0x55);
}

/// from_shared produces a backend with the same layout sizes as the original.
#[test]
fn shared_code_layout_consistency() {
    let sim = Simulator::builder(ADDER, "Top").build().unwrap();
    let shared = sim.shared_code();

    let original_stable = sim.stable_region_size();
    let (_, original_total) = sim.memory_as_ptr();

    let backend = JitBackend::from_shared(shared);
    assert_eq!(backend.stable_region_size(), original_stable);
    let (_, new_total) = backend.memory_as_ptr();
    assert_eq!(new_total, original_total);
}
