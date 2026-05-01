//! Dump SIR, CLIF (pre/post opt), and native code for Linear SEC P=6.
//! Usage: cargo run -p celox --example dump_linear_sec_ir

use celox::Simulator;

#[path = "../tests/fixtures/veryl_std.rs"]
mod veryl_std;

fn linear_sec_source() -> String {
    format!(
        "{}\n{}\n{}",
        veryl_std::source(&["coding", "linear_sec_encoder.veryl"]),
        veryl_std::source(&["coding", "linear_sec_decoder.veryl"]),
        r#"
module Top #(
    param P: u32 = 6,
    const K: u32 = (1 << P) - 1,
    const N: u32 = K - P,
)(
    i_word     : input  logic<N>,
    o_codeword : output logic<K>,
    o_word     : output logic<N>,
    o_corrected: output logic,
) {
    inst u_enc: linear_sec_encoder #(
        P: P,
    ) (
        i_word,
        o_codeword,
    );
    inst u_dec: linear_sec_decoder #(
        P: P,
    ) (
        i_codeword: o_codeword,
        o_word,
        o_corrected,
    );
}
"#
    )
}

fn main() {
    let trace_result = Simulator::builder(&linear_sec_source(), "Top")
        .trace_post_optimized_sir()
        .trace_pre_optimized_clif()
        .trace_post_optimized_clif()
        .trace_native()
        .build_with_trace();

    let trace = &trace_result.trace;

    if let Some(sir) = trace.format_post_optimized_sir() {
        std::fs::write("linear_sec_p6_sir.txt", &sir).unwrap();
        eprintln!("SIR: {} bytes → linear_sec_p6_sir.txt", sir.len());
    }

    if let Some(ref clif) = trace.pre_optimized_clif {
        std::fs::write("linear_sec_p6_clif_pre.txt", clif).unwrap();
        eprintln!(
            "CLIF pre-opt: {} bytes → linear_sec_p6_clif_pre.txt",
            clif.len()
        );
    }

    if let Some(ref clif) = trace.post_optimized_clif {
        std::fs::write("linear_sec_p6_clif_post.txt", clif).unwrap();
        eprintln!(
            "CLIF post-opt: {} bytes → linear_sec_p6_clif_post.txt",
            clif.len()
        );
    }

    if let Some(ref native) = trace.native {
        std::fs::write("linear_sec_p6_native.txt", native).unwrap();
        eprintln!("Native: {} bytes → linear_sec_p6_native.txt", native.len());
    }

    // Sanity check
    let mut sim = trace_result.res.unwrap();
    let i_word = sim.signal("i_word");
    let o_word = sim.signal("o_word");
    sim.modify(|io| io.set(i_word, 42u64)).unwrap();
    let out: u64 = sim.get_as(o_word);
    eprintln!("Sanity check: input=42, output={}", out);
}
