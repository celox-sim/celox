use super::*;

sv_backends! {
    fn countones_preserves_argument_and_return_types(sim) {
        @case "system_functions::countones_preserves_argument_and_return_types";
    }

    fn countones_ignores_unknown_bits_in_comb_and_ff(sim) {
        @case "system_functions::countones_ignores_unknown_bits_in_comb_and_ff";
    }

    fn countones_in_constant_expressions(sim) {
        @case "system_functions::countones_in_constant_expressions";
    }

    fn bit_vector_predicates_preserve_argument_and_return_types(sim) {
        @case "system_functions::bit_vector_predicates_preserve_argument_and_return_types";
    }

    fn bit_vector_predicates_handle_unknown_bits_in_comb_ff_and_ports(sim) {
        @setup {
            let source = r#"
                module Child(input logic [2:0] value, output logic [2:0] y);
                    assign y = value;
                endmodule
                module Top(input bit clk, input logic [255:0] a,
                           output logic [2:0] comb, registered, via_child,
                           output logic [63:0] unknown_wide,
                           output logic unknown_bit, unknown_partial);
                    always_comb comb = {$onehot(a), $onehot0(a), $isunknown(a)};
                    always_ff @(posedge clk)
                        registered <= {$onehot(a), $onehot0(a), $isunknown(a)};
                    Child child(.value({$onehot(a), $onehot0(a), $isunknown(a)}), .y(via_child));
                    assign unknown_wide = $isunknown(a);
                    assign unknown_bit = $isunknown(a[0]);
                    assign unknown_partial = $isunknown(a[128:0]);
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("bit_vector_predicates_four_state.sv"))], "Top")
            .four_state(true);

        let a = sim.signal("a");
        let clk = sim.event("clk");
        let mut cases = Vec::new();
        // Every combination of 0/1/X/Z in the low four bits.
        for mask in 0u8..16 {
            for value in 0u8..16 {
                cases.push((BigUint::from(value), BigUint::from(mask)));
            }
        }
        // Exercise word boundaries and unknowns in the highest word.
        for bit in [63usize, 64, 127, 128, 200, 255] {
            let high = BigUint::from(1u8) << bit;
            cases.extend([
                (high.clone(), BigUint::from(0u8)),
                (high.clone() | BigUint::from(1u8), BigUint::from(0u8)),
                (BigUint::from(0u8), high.clone()),
                (high.clone(), high.clone()),
                (high.clone() | BigUint::from(1u8), high),
            ]);
        }
        let all = (BigUint::from(1u8) << 256usize) - BigUint::from(1u8);
        cases.extend([
            (all.clone(), BigUint::from(0u8)),
            (all.clone(), all.clone()),
            (BigUint::from(0u8), all),
        ]);
        for (value, mask) in cases {
            let known = &value ^ (&value & &mask);
            let ones = known.iter_u64_digits().map(u64::count_ones).sum::<u32>();
            let unknown = u8::from(mask != BigUint::from(0u8));
            let expected = (u8::from(ones == 1) << 2) | (u8::from(ones <= 1) << 1) | unknown;
            let unknown_bit = u8::from(mask.bit(0));
            let low_mask = (BigUint::from(1u8) << 129usize) - BigUint::from(1u8);
            let unknown_partial = u8::from((&mask & low_mask) != BigUint::from(0u8));
            sim.modify(|io| io.set_four_state(a, value.clone(), mask.clone())).unwrap();
            sim.tick(clk).unwrap();
            for name in ["comb", "registered", "via_child"] {
                assert_eq!(sim.get_four_state(sim.signal(name)), (expected.into(), 0u8.into()), "{name}: value={value:x}, mask={mask:x}");
            }
            for (name, expected) in [("unknown_wide", unknown), ("unknown_bit", unknown_bit), ("unknown_partial", unknown_partial)] {
                assert_eq!(sim.get_four_state(sim.signal(name)), (expected.into(), 0u8.into()), "{name}: value={value:x}, mask={mask:x}");
            }
        }
    }

    fn bit_vector_predicates_in_constant_expressions(sim) {
        @case "system_functions::bit_vector_predicates_in_constant_expressions";
    }
}

#[test]
fn rejects_bit_vector_functions_with_missing_or_extra_arguments() {
    for name in ["$countones", "$onehot", "$onehot0", "$isunknown"] {
        for args in ["", "a, a", ", a", "a,"] {
            let call = format!("{name}({args})");
            let source =
                format!("module Top(input logic a, output int y); assign y = {call}; endmodule");
            let result = Simulator::from_sv_sources(
                vec![(&source, Path::new("invalid_countones.sv"))],
                "Top",
            )
            .build_cranelift();
            assert!(result.is_err(), "invalid call should be rejected: {call}");
        }
    }
}
