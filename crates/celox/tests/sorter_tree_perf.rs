// Regression tests for sorter tree compilation scaling.
//
// The SorterTreeDistEntry design creates deeply nested mux trees from
// MinReductionTree's binary merger structure.  Before the select-based mux
// lowering fix, branch-based mux lowering created 3 blocks per mux,
// causing exponential block count growth (N=16 took 24 minutes).
//
// These tests verify that compilation time scales roughly linearly with N.
use celox::SimulatorBuilder;

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;
use std::time::Instant;

fn load_sorter_sources() -> String {
    [
        include_str!("fixtures/sorter_tree/sorter_item.veryl"),
        include_str!("fixtures/sorter_tree/dist_entry.veryl"),
        include_str!("fixtures/sorter_tree/min_reduction_tree.veryl"),
        include_str!("fixtures/sorter_tree/linear_sorter_pull.veryl"),
        include_str!("fixtures/sorter_tree/linear_sorter.veryl"),
        include_str!("fixtures/sorter_tree/sorter_tree.veryl"),
    ]
    .join("\n")
}

fn build_sorter(n: u64) -> std::time::Duration {
    let code = load_sorter_sources();
    let start = Instant::now();
    SimulatorBuilder::new(&code, "SorterTreeDistEntry")
        .param("N", n)
        .param("LEAF_DEPTH", 4)
        .param("OUT_DEPTH", 16)
        .build()
        .unwrap();
    start.elapsed()
}

/// Compilation-scaling regression across small, medium, and large designs.
///
/// Keep all measurements in one test so the Rust test harness cannot run
/// heavyweight sorter builds concurrently. Each size is built exactly once.
#[test]
fn sorter_tree_compilation_scales() {
    let t4 = build_sorter(4);
    let t8 = build_sorter(8);
    let t16 = build_sorter(16);
    let t32 = build_sorter(32);
    let t64 = build_sorter(64);
    let t128 = build_sorter(128);

    let ratio_4_8 = t8.as_secs_f64() / t4.as_secs_f64();
    let ratio_16_64 = t64.as_secs_f64() / t16.as_secs_f64();
    let ratio_32_128 = t128.as_secs_f64() / t32.as_secs_f64();
    eprintln!(
        "SorterTreeDistEntry compile times: N=4 {t4:?}, N=8 {t8:?}, N=16 {t16:?}, \
         N=32 {t32:?}, N=64 {t64:?}, N=128 {t128:?}; ratios: \
         N=8/N=4 {ratio_4_8:.2}x, N=64/N=16 {ratio_16_64:.2}x, \
         N=128/N=32 {ratio_32_128:.2}x"
    );

    // Linear scaling gives roughly 2x here; exponential growth exceeds this
    // broad bound by orders of magnitude.
    assert!(
        ratio_4_8 < 4.0,
        "N=8/N=4 ratio is {ratio_4_8:.2}x, expected < 4.0x (linear scaling)"
    );
    assert!(
        ratio_16_64 < 10.0,
        "N=64/N=16 ratio is {ratio_16_64:.2}x, expected < 10.0x"
    );
    assert!(
        ratio_32_128 < 12.0,
        "N=128/N=32 ratio is {ratio_32_128:.2}x, expected < 12.0x"
    );
}
