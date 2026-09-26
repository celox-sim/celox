use celox::SimulatorBuilder;

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

const CODE: &str = r#"
    module Top (
        bits: input logic<64>,
        gate: input logic,
        fallback: input logic<7>,
        pop: output logic<7>,
        clz: output logic<7>,
        ctz: output logic<7>,
        gated_clz: output logic<7>,
    ) {
        always_comb {
            pop = 7'd0;
            for i in 0..64 {
                pop = pop + {6'b0, bits[i]};
            }

            clz = 7'd64;
            for i in 0..64 {
                if bits[i] {
                    clz = 7'd63 - (i as 7);
                }
            }

            ctz = 7'd64;
            for i in 0..64 {
                if bits[63 - i] {
                    ctz = 7'd63 - (i as 7);
                }
            }

            gated_clz = if gate ? 7'd64 : fallback;
            for i in 0..64 {
                if bits[63 - i] && gated_clz == 7'd64 {
                    gated_clz = if gate ? (i as 7) : gated_clz;
                }
            }
        }
    }
"#;

#[test]
fn optimized_sir_recovers_expanded_bit_count_loops() {
    let result = SimulatorBuilder::new(CODE, "Top")
        .trace_post_optimized_sir()
        .build_with_trace();
    let sir = result
        .trace
        .format_post_optimized_sir()
        .expect("post-optimized SIR should be captured");

    assert!(sir.contains("PopCount"), "missing popcount idiom:\n{sir}");
    assert!(
        sir.contains("CountLeadingZeros"),
        "missing clz idiom:\n{sir}"
    );
    assert_eq!(
        sir.matches("CountLeadingZeros").count(),
        1,
        "the direct and conditionally seeded clz results should share one count:\n{sir}"
    );
    assert_eq!(
        sir.matches(" = Mux(").count(),
        1,
        "the conditionally seeded clz should reduce to one final selection:\n{sir}"
    );
    assert!(
        sir.contains("CountTrailingZeros"),
        "missing ctz idiom:\n{sir}"
    );
}

all_backends! {

fn test_recovered_bit_count_loop_semantics(sim) {
    @ignore_on(sv);
    @case "loop_idiom::test_recovered_bit_count_loop_semantics";
}

}
