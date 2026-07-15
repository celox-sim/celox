use celox::{Simulator, SimulatorBuilder};

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
    @setup { let code = CODE; }
    @build Simulator::builder(code, "Top");
    let bits = sim.signal("bits");
    let gate = sim.signal("gate");
    let fallback = sim.signal("fallback");
    let pop = sim.signal("pop");
    let clz = sim.signal("clz");
    let ctz = sim.signal("ctz");
    let gated_clz = sim.signal("gated_clz");

    for (input, expected_pop, expected_clz, expected_ctz) in [
        (0u64, 0u8, 64u8, 64u8),
        (1u64, 1u8, 63u8, 0u8),
        (1u64 << 63, 1u8, 0u8, 63u8),
        (0x00f0_0000_0000_0008u64, 5u8, 8u8, 3u8),
        (u64::MAX, 64u8, 0u8, 0u8),
    ] {
        sim.set(bits, input);
        sim.set(gate, 1u8);
        sim.set(fallback, 37u8);
        sim.eval_comb().unwrap();
        assert_eq!(sim.get(pop), expected_pop.into(), "popcount({input:#x})");
        assert_eq!(sim.get(clz), expected_clz.into(), "clz({input:#x})");
        assert_eq!(sim.get(ctz), expected_ctz.into(), "ctz({input:#x})");
        assert_eq!(
            sim.get(gated_clz),
            expected_clz.into(),
            "gated clz({input:#x})"
        );

        sim.set(gate, 0u8);
        sim.eval_comb().unwrap();
        assert_eq!(
            sim.get(gated_clz),
            37u8.into(),
            "gated clz fallback({input:#x})"
        );
    }
}

}
