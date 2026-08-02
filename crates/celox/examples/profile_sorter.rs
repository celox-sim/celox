use celox::Simulator;
use std::time::Instant;

fn main() {
    let code = [
        include_str!("../tests/fixtures/sorter_tree/sorter_item.veryl"),
        include_str!("../tests/fixtures/sorter_tree/dist_entry.veryl"),
        include_str!("../tests/fixtures/sorter_tree/min_reduction_tree.veryl"),
        include_str!("../tests/fixtures/sorter_tree/linear_sorter_pull.veryl"),
        include_str!("../tests/fixtures/sorter_tree/linear_sorter.veryl"),
        include_str!("../tests/fixtures/sorter_tree/sorter_tree.veryl"),
    ]
    .join("\n");

    let n: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);

    println!("=== Building SorterTreeDistEntry N={n} ===");
    let build_start = Instant::now();
    let mut sim = Simulator::builder(&code, "SorterTreeDistEntry")
        .param("N", n)
        .param("LEAF_DEPTH", 4)
        .param("OUT_DEPTH", 16)
        .build()
        .unwrap();
    println!("=== Build: {:?} ===", build_start.elapsed());

    let clk = sim.signal("clk");
    let rst = sim.signal("rst");

    // Reset
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.modify(|io| io.set(clk, 1u8)).unwrap();
    sim.modify(|io| io.set(clk, 0u8)).unwrap();
    sim.modify(|io| io.set(rst, 0u8)).unwrap();

    // Runtime benchmark
    let cycles = 10_000u32;
    let run_start = Instant::now();
    for _ in 0..cycles {
        sim.modify(|io| io.set(clk, 1u8)).unwrap();
        sim.modify(|io| io.set(clk, 0u8)).unwrap();
    }
    let elapsed = run_start.elapsed();
    let ns_per_cycle = elapsed.as_nanos() as f64 / cycles as f64;
    let khz = 1e6 / ns_per_cycle;
    println!(
        "=== Runtime: {cycles} cycles in {elapsed:?} ({ns_per_cycle:.0} ns/cycle, {khz:.0} kHz) ==="
    );
}
