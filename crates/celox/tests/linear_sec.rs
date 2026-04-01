//! Correctness tests for linear SEC (single-error correction) P=6 Hamming code.

use celox::{DeadStorePolicy, Simulator};

const LINEAR_SEC_SRC: &str = concat!(
    include_str!("../../../deps/veryl/crates/std/veryl/src/coding/linear_sec_encoder.veryl"),
    include_str!("../../../deps/veryl/crates/std/veryl/src/coding/linear_sec_decoder.veryl"),
    include_str!("../../../benches/veryl/linear_sec_top.veryl"),
);

fn build_sim() -> Simulator<celox::NativeBackend> {
    Simulator::builder(LINEAR_SEC_SRC, "Top")
        .build()
        .unwrap()
}

fn build_sim_dse() -> Simulator<celox::NativeBackend> {
    Simulator::builder(LINEAR_SEC_SRC, "Top")
        .dead_store_policy(DeadStorePolicy::PreserveTopPorts)
        .build()
        .unwrap()
}

/// Encode→decode roundtrip: output should equal input for all test values.
#[test]
fn roundtrip_no_error() {
    let mut sim = build_sim();
    let i_word = sim.signal("i_word");
    let o_word = sim.signal("o_word");
    let o_corrected = sim.signal("o_corrected");

    for input in [0u64, 1, 42, 0x1FF_FFFF_FFFF_FFFF, 0x155_5555_5555_5555] {
        sim.modify(|io| io.set(i_word, input)).unwrap();
        let out: u64 = sim.get_as(o_word);
        let corrected: u8 = sim.get_as(o_corrected);
        assert_eq!(out, input, "roundtrip failed for input={input:#x}");
        assert_eq!(corrected, 0, "o_corrected should be 0 when no error");
    }
}

/// Same test with DSE enabled.
#[test]
fn roundtrip_no_error_dse() {
    let mut sim = build_sim_dse();
    let i_word = sim.signal("i_word");
    let o_word = sim.signal("o_word");
    let o_corrected = sim.signal("o_corrected");

    for input in [0u64, 1, 42, 0x1FF_FFFF_FFFF_FFFF, 0x155_5555_5555_5555] {
        sim.modify(|io| io.set(i_word, input)).unwrap();
        let out: u64 = sim.get_as(o_word);
        let corrected: u8 = sim.get_as(o_corrected);
        assert_eq!(out, input, "DSE roundtrip failed for input={input:#x}");
        assert_eq!(corrected, 0, "DSE o_corrected should be 0 when no error");
    }
}

/// Exhaustive roundtrip for small input range.
#[test]
fn roundtrip_exhaustive_small() {
    let mut sim = build_sim_dse();
    let i_word = sim.signal("i_word");
    let o_word = sim.signal("o_word");

    for input in 0u64..1024 {
        sim.modify(|io| io.set(i_word, input)).unwrap();
        let out: u64 = sim.get_as(o_word);
        assert_eq!(out, input, "exhaustive roundtrip failed for input={input}");
    }
}
