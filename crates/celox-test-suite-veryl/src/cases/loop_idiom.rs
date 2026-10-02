use crate::Design;

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

cases! { ControlFlow, "loop_idiom";

fn test_guarded_scan_preserves_independent_outputs(sim) {
    @setup { let code = r#"
        module Top (
            entries: input logic<32>,
            gate0: input logic,
            gate1: input logic,
            seed: input logic<8>,
            result0: output logic<8>,
            result1: output logic<8>,
        ) {
            always_comb {
                result0 = seed;
                result1 = seed;
                for i in 0..32 {
                    if gate0 && entries[i] {
                        result0 = seed + (i as 8);
                    }
                    if gate1 && entries[31 - i] {
                        result1 = seed ^ (i as 8);
                    }
                }
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let entries = sim.signal("entries");
    let gate0 = sim.signal("gate0");
    let gate1 = sim.signal("gate1");
    let seed = sim.signal("seed");
    let result0 = sim.signal("result0");
    let result1 = sim.signal("result1");
    for mask in [0u32, 1, 1 << 31, 0x8000_0024, u32::MAX] {
        for initial in [0u8, 37, 250] {
            sim.set(entries, mask);
            sim.set(seed, initial);
            // Change guards while retaining the same data, then return idle.
            for (g0, g1) in [(0u8, 0u8), (1, 0), (0, 1), (1, 1), (0, 0)] {
                sim.set(gate0, g0);
                sim.set(gate1, g1);
                sim.eval_comb().unwrap();
                let expected0 = if g0 != 0 && mask != 0 {
                    initial.wrapping_add((31 - mask.leading_zeros()) as u8)
                } else { initial };
                let expected1 = if g1 != 0 && mask != 0 {
                    initial ^ (31 - mask.trailing_zeros()) as u8
                } else { initial };
                assert_eq!(sim.get(result0), expected0.into(), "mask={mask:#x} gates={g0}/{g1}");
                assert_eq!(sim.get(result1), expected1.into(), "mask={mask:#x} gates={g0}/{g1}");
            }
        }
    }
}


fn test_guarded_packed_scan(sim) {
    @setup { let code = r#"
        module Top (
            entries: input logic<32>,
            gate: input logic,
            seed: input logic<32>,
            result: output logic<32>,
            partial: output logic<32>,
        ) {
            always_comb {
                result = 32'd0;
                partial = seed;
                for i in 0..32 {
                    result[i] = gate && entries[i];
                }
                for i in 0..16 {
                    partial[i] = gate && entries[i];
                }
            }
        }
    "#; }
    @build Design::new(code, "Top");
    let entries = sim.signal("entries");
    let gate = sim.signal("gate");
    let seed = sim.signal("seed");
    let result = sim.signal("result");
    let partial = sim.signal("partial");
    for mask in [0u32, 1, 0x8000_0024, u32::MAX] {
        for initial in [0u32, 0xabcd_ffff, u32::MAX] {
            sim.set(entries, mask);
            sim.set(seed, initial);
            for active in [0u8, 1, 0] {
                sim.set(gate, active);
                sim.eval_comb().unwrap();
                let expected = if active != 0 { mask } else { 0 };
                assert_eq!(sim.get(result), expected.into());
                assert_eq!(sim.get(partial), ((initial & 0xffff_0000) | (expected & 0xffff)).into());
            }
        }
    }
}

fn test_recovered_bit_count_loop_semantics(sim) {

    @setup { let code = CODE; }
    @build Design::new(code, "Top");
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
