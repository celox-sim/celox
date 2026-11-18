use super::*;
use crate::{KernelSchedule, SchedulerMode};

/// A DRAM model: flip-flops of the `posedge clk` domain write and read an
/// associative array through the heap helper, calls whose state effects the
/// scheduler cannot name. `comb` defines `sum`, which the domains read.
fn dram(comb: &str) -> String {
    format!(
        r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    always #5 clk = ~clk;
    logic [7:0] mem [int];
    logic [7:0] count = 0, rdata = 0, held = 0;
    logic [7:0] sum;
    {comb}
    always_ff @(posedge clk) begin
        count <= count + 8'd1;
        mem[count % 4] = sum;
    end
    always_ff @(posedge clk) rdata <= mem.exists((count + 1) % 4) ? mem[(count + 1) % 4] : 8'd0;
    always_ff @(negedge clk) held <= sum;
    initial begin #200; $finish; end
endmodule"#
    )
}

fn build(source: &str) -> SimulatorBuilder<'_, crate::Simulation> {
    crate::Simulation::from_sv_sources(vec![(source, std::path::Path::new("settles.sv"))], "Top")
        .four_state(false)
}

/// The plan of the compiled scheduler of `source`, with the native services
/// a native build declares.
fn plan(source: &str) -> crate::scheduler::PlanSummary {
    let mut builder = build(source).emit_triggers();
    builder.native_file_calls = true;
    let (program, _, _, _, _) = builder
        .into_laid_out_program(crate::MemoryLayoutMode::Packed, false, false)
        .unwrap();
    crate::scheduler::plan_for_test(&program).unwrap()
}

/// Run `source` natively against the reference scheduler and check the
/// named signals against the interpreter's at the end.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn agrees(source: &str, until: u64, names: &[&str]) {
    let mut reference = build(source).build_interpreter().unwrap();
    reference.run_until(until * 2).unwrap();
    assert!(reference.is_finished());
    let mut simulation = build(source)
        .scheduler_mode(SchedulerMode::DifferentialRuns)
        .build_native()
        .unwrap();
    for stop in (7..=until + 10).step_by(7) {
        simulation.run_until(stop).unwrap();
    }
    assert!(simulation.is_finished());
    let stats = simulation.scheduler_stats();
    assert!(stats.compiled_slots > 0, "{stats:?}");
    assert!(stats.checked_steps > 0, "{stats:?}");
    assert!(stats.unchecked.is_none(), "{stats:?}");
    assert!(
        stats.kernels.iter().all(|k| *k == KernelSchedule::Compiled),
        "{stats:?}"
    );
    for &name in names {
        let (a, b) = (simulation.signal(name), reference.signal(name));
        assert_eq!(simulation.get(a), reference.get(b), "{name}");
    }
}

/// Opaque calls in a domain keep the compiled scheduler from deferring its
/// settles past their readers, but not from skipping a settle whose inputs
/// did not change, nor from settling only the cone of a clock write: the
/// unit that makes them marks a full change after it runs.
#[test]
fn opaque_writers_keep_unchanged_settles_skipped() {
    for (comb, incremental) in [
        ("assign sum = rdata + count;", true),
        ("assign sum = rdata + count + {7'd0, clk};", false),
    ] {
        let source = dram(comb);
        let plan = plan(&source);
        assert!(plan.settle_reads.is_some(), "{comb}");
        assert!(plan.lazy.is_none(), "{comb}");
        assert_eq!(plan.incremental, incremental, "{comb}");
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        agrees(&source, 200, &["count", "rdata", "held", "sum"]);
    }
}

/// Combinational processes that display and assert, which capture what they
/// report, and data a process rewrites between the clock edges, sometimes
/// with the value it holds.
#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
const OBSERVED: &str = r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    always #5 clk = ~clk;
    logic [7:0] count = 0, other = 0, q = 0;
    logic [7:0] w1, w2;
    assign w1 = count * 3 + other;
    assign w2 = w1 ^ (count >> 2);
    always_comb assert final (w2[2:0] != 3'd5) else $warning("final %0d", w2);
    always_comb assert #0 (w1[1:0] != 2'd3) else $display("observed %0d", w1);
    always_comb if (w2[0] & count[1]) $display("comb %0d %0d", w2, count);
    always_ff @(posedge clk) begin
        count <= count + 8'd1;
        q <= q + w2;
    end
    always #7 other = other + 8'd1;
    always #13 other = other;
    initial begin #200; $finish; end
endmodule"#;

/// A capture records only for a site its observer or a changed store
/// enables, so the compiled scheduler skips the settles whose inputs did
/// not change, and reports what the reference does.
#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
#[test]
fn captures_keep_unchanged_settles_skipped() {
    let plan = plan(OBSERVED);
    assert!(plan.settle_reads.is_some());
    assert!(plan.lazy.is_none());
    assert!(!plan.incremental);
    agrees(OBSERVED, 200, &["count", "other", "q", "w2"]);
}
