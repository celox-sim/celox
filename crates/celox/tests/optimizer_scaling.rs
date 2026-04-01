// Scaling regression tests for optimizer passes.
//
// These tests build the same parametric design at two sizes and assert that
// the build-time ratio stays within a bound.  This catches super-polynomial
// scaling regressions regardless of absolute machine speed.
//
// Run with timing details:
//   CELOX_PASS_TIMING=1 cargo test -p celox --test optimizer_scaling -- --nocapture
use celox::SimulatorBuilder;

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;
use std::time::Instant;

// ---------------------------------------------------------------------------
// linear_sec: Hamming SEC encoder+decoder, codeword size = 2^P - 1
// P=5 → 31-bit codeword (26-bit data)
// P=6 → 63-bit codeword (57-bit data)
// Circuit size roughly doubles per P increment.
// ---------------------------------------------------------------------------

fn linear_sec_source(p: u32) -> String {
    let encoder =
        include_str!("../../../deps/veryl/crates/std/veryl/src/coding/linear_sec_encoder.veryl");
    let decoder =
        include_str!("../../../deps/veryl/crates/std/veryl/src/coding/linear_sec_decoder.veryl");
    let top = format!(
        r#"
module Top #(
    param P: u32 = {p},
    const K: u32 = (1 << P) - 1,
    const N: u32 = K - P,
)(
    i_word     : input  logic<N>,
    o_codeword : output logic<K>,
    o_word     : output logic<N>,
    o_corrected: output logic,
) {{
    inst u_enc: linear_sec_encoder #(P: P) (
        i_word,
        o_codeword,
    );
    inst u_dec: linear_sec_decoder #(P: P) (
        i_codeword: o_codeword,
        o_word,
        o_corrected,
    );
}}
"#
    );
    format!("{encoder}\n{decoder}\n{top}")
}

fn build_linear_sec(p: u32) -> std::time::Duration {
    let src = linear_sec_source(p);
    let start = Instant::now();
    let _sim = SimulatorBuilder::new(&src, "Top").build().unwrap();
    start.elapsed()
}

#[test]
fn linear_sec_scaling_p5_to_p6() {
    // Warm up (first build initializes global state in the analyzer)
    let _ = build_linear_sec(5);

    const SAMPLES: usize = 3;

    let mut times_p5 = Vec::with_capacity(SAMPLES);
    let mut times_p6 = Vec::with_capacity(SAMPLES);

    for _ in 0..SAMPLES {
        times_p5.push(build_linear_sec(5));
        times_p6.push(build_linear_sec(6));
    }

    // Use median to reduce noise
    times_p5.sort();
    times_p6.sort();
    let median_p5 = times_p5[SAMPLES / 2].as_secs_f64();
    let median_p6 = times_p6[SAMPLES / 2].as_secs_f64();
    let ratio = median_p6 / median_p5;

    eprintln!("[scaling] linear_sec P=5 median: {median_p5:.3}s");
    eprintln!("[scaling] linear_sec P=6 median: {median_p6:.3}s");
    eprintln!("[scaling] ratio P6/P5: {ratio:.2}x");

    // Circuit size roughly doubles (P=5→P=6).
    // O(n log n) → ~2.2x, O(n²) → ~4x.
    // We allow up to 10x as a generous margin while still catching
    // super-polynomial blowups (which were 50x+ in the original bug).
    assert!(
        ratio < 10.0,
        "optimizer scaling regression: P6/P5 ratio = {ratio:.2}x (limit: 10x)"
    );
}

// ---------------------------------------------------------------------------
// N-counter: N flip-flops, exercises FF optimizer passes.
// N=500 → N=1000: doubles the number of execution units.
// ---------------------------------------------------------------------------

const COUNTER_TEMPLATE: &str = r#"
    module Top #(
        param N: u32 = {N},
    )(
        clk: input clock,
        rst: input reset,
        cnt: output logic<32>[N],
        cnt0: output logic<32>,
    ) {
        assign cnt0 = cnt[0];
        for i in 0..N: g {
            always_ff (clk, rst) {
                if_reset {
                    cnt[i] = 0;
                } else {
                    cnt[i] += 1;
                }
            }
        }
    }
"#;

fn build_counter(n: u32) -> std::time::Duration {
    let src = COUNTER_TEMPLATE.replace("{N}", &n.to_string());
    let start = Instant::now();
    let _sim = SimulatorBuilder::new(&src, "Top").build().unwrap();
    start.elapsed()
}

#[test]
fn counter_scaling_n500_to_n1000() {
    // Warm up
    let _ = build_counter(100);

    const SAMPLES: usize = 3;

    let mut times_small = Vec::with_capacity(SAMPLES);
    let mut times_large = Vec::with_capacity(SAMPLES);

    for _ in 0..SAMPLES {
        times_small.push(build_counter(500));
        times_large.push(build_counter(1000));
    }

    times_small.sort();
    times_large.sort();
    let median_small = times_small[SAMPLES / 2].as_secs_f64();
    let median_large = times_large[SAMPLES / 2].as_secs_f64();
    let ratio = median_large / median_small;

    eprintln!("[scaling] counter N=500  median: {median_small:.3}s");
    eprintln!("[scaling] counter N=1000 median: {median_large:.3}s");
    eprintln!("[scaling] ratio N1000/N500: {ratio:.2}x");

    // N doubles → expect ~2x for linear, ~4x for quadratic.
    // Allow up to 6x as margin (these are independent EUs so should be ~linear).
    assert!(
        ratio < 6.0,
        "optimizer scaling regression: N1000/N500 ratio = {ratio:.2}x (limit: 6x)"
    );
}
