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

/// Process CPU time (user + system) consumed so far.
///
/// Scaling ratios are asserted on CPU time rather than wall-clock time: the
/// suite runs alongside other heavyweight test binaries, and wall-clock
/// measurements inflate when they contend for cores while CPU time stays
/// stable.
fn process_cpu_time() -> std::time::Duration {
    #[cfg(unix)]
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        assert_eq!(libc::getrusage(libc::RUSAGE_SELF, &mut usage), 0);
        std::time::Duration::from_micros(
            (usage.ru_utime.tv_sec as u64) * 1_000_000
                + usage.ru_utime.tv_usec as u64
                + (usage.ru_stime.tv_sec as u64) * 1_000_000
                + usage.ru_stime.tv_usec as u64,
        )
    }
    #[cfg(not(unix))]
    {
        // Wall-clock fallback for platforms without getrusage; the primary
        // CI hosts are Unix.
        use std::sync::OnceLock;
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START.get_or_init(std::time::Instant::now).elapsed()
    }
}

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
    let start = process_cpu_time();
    SimulatorBuilder::new(&code, "SorterTreeDistEntry")
        .param("N", n)
        .param("LEAF_DEPTH", 4)
        .param("OUT_DEPTH", 16)
        .build()
        .unwrap();
    process_cpu_time() - start
}

/// Compilation-scaling regression across small and medium designs.
///
/// Keep all measurements in one test so the Rust test harness cannot run
/// heavyweight sorter builds concurrently. Each size is built exactly once.
/// Sizes stop at N=32: the exponential mux lowering this guards against
/// already took 24 minutes at N=16, while N=64/128 added minutes of CPU time
/// to every test run without catching anything the smaller ratios miss.
#[test]
fn sorter_tree_compilation_scales() {
    let t4 = build_sorter(4);
    let t8 = build_sorter(8);
    let t16 = build_sorter(16);
    let t32 = build_sorter(32);

    let ratio_4_8 = t8.as_secs_f64() / t4.as_secs_f64();
    let ratio_8_32 = t32.as_secs_f64() / t8.as_secs_f64();
    println!(
        "SorterTreeDistEntry compile CPU times: N=4 {t4:?}, N=8 {t8:?}, N=16 {t16:?}, \
         N=32 {t32:?}; ratios: N=8/N=4 {ratio_4_8:.2}x, N=32/N=8 {ratio_8_32:.2}x"
    );

    // Linear scaling gives roughly 2x and 4x here; exponential growth exceeds
    // these broad bounds by orders of magnitude.
    assert!(
        ratio_4_8 < 4.0,
        "N=8/N=4 ratio is {ratio_4_8:.2}x, expected < 4.0x (linear scaling)"
    );
    assert!(
        ratio_8_32 < 10.0,
        "N=32/N=8 ratio is {ratio_8_32:.2}x, expected < 10.0x"
    );
}
