use super::*;

sv_backends! {
    fn countones_preserves_argument_and_return_types(sim) {
        @setup {
            let source = r#"
                module Child(input int value, output int y);
                    assign y = value;
                endmodule
                module Top(input logic signed [7:0] a,
                           output int count, output logic [63:0] wide,
                           output logic [1:0] narrow, output int via_child,
                           output int sum_count, output int selected,
                           output int nested, output int function_count,
                           output logic negative, output logic [63:0] inverted);
                    function automatic logic [7:0] identity(input logic [7:0] v);
                        return v;
                    endfunction
                    assign count = $countones(a);
                    assign wide = $countones(a);
                    assign narrow = $countones(a);
                    assign sum_count = $countones(a + 8'd1);
                    assign selected = $countones(a[5:2]);
                    assign nested = $countones($countones(a));
                    assign function_count = $countones(identity(a));
                    assign negative = ($countones(a) - 32'sd9) < 0;
                    assign inverted = ~$countones(a);
                    Child child(.value($countones(a)), .y(via_child));
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("countones.sv"))], "Top");

        let a = sim.signal("a");
        for value in 0u8..=255 {
            sim.modify(|io| io.set(a, value)).unwrap();
            let ones = value.count_ones();
            for name in ["count", "wide", "via_child", "function_count"] {
                assert_eq!(sim.get(sim.signal(name)), ones.into(), "{name}: {value}");
            }
            assert_eq!(sim.get(sim.signal("narrow")), (ones & 3).into());
            assert_eq!(sim.get(sim.signal("sum_count")), value.wrapping_add(1).count_ones().into());
            assert_eq!(sim.get(sim.signal("selected")), ((value >> 2) & 15).count_ones().into());
            assert_eq!(sim.get(sim.signal("nested")), ones.count_ones().into());
            assert_eq!(sim.get(sim.signal("negative")), 1u8.into());
            assert_eq!(sim.get(sim.signal("inverted")), (!(ones as u64)).into());
        }
    }

    fn countones_ignores_unknown_bits_in_comb_and_ff(sim) {
        @setup {
            let source = r#"
                module Top(input bit clk, input logic [255:0] a,
                           output int count, output int registered,
                           output int packed_count, output int fill_count);
                    always_comb count = $countones(a);
                    always_ff @(posedge clk) registered <= $countones(a);
                    assign packed_count = $countones({a[7:0], 4'b1xz0});
                    assign fill_count = $countones('1);
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("countones_four_state.sv"))], "Top")
            .four_state(true);

        let a = sim.signal("a");
        let clk = sim.event("clk");
        let all = (BigUint::from(1u8) << 256usize) - BigUint::from(1u8);
        for (value, mask, expected, packed) in [
            (all.clone(), BigUint::from(0u8), 256u32, 9u32),
            (all.clone(), all.clone(), 0, 1),
            (BigUint::from(0u8), all.clone(), 0, 1),
            ((BigUint::from(1u8) << 200usize) | BigUint::from(0b1011u8),
             (BigUint::from(1u8) << 200usize) | BigUint::from(0b1100u8), 2, 3),
        ] {
            sim.modify(|io| io.set_four_state(a, value, mask)).unwrap();
            sim.tick(clk).unwrap();
            for name in ["count", "registered"] {
                assert_eq!(sim.get_four_state(sim.signal(name)), (expected.into(), 0u8.into()));
            }
            assert_eq!(sim.get_four_state(sim.signal("packed_count")), (packed.into(), 0u8.into()));
            assert_eq!(sim.get_four_state(sim.signal("fill_count")), (1u8.into(), 0u8.into()));
        }
    }

    fn countones_in_constant_expressions(sim) {
        @setup {
            let source = r#"
                module Top(output logic [31:0] result, output logic [31:0] literals);
                    localparam logic signed [7:0] NEG = -1;
                    localparam C = $countones(NEG);
                    localparam MIXED = $countones(8'b10xz_11xz);
                    if (C == 8 && MIXED == 3 && $bits(C) == 32) begin
                        assign result = $countones(256'hffff_ffff_ffff_ffff_ffff_ffff_ffff_ffff_ffff);
                    end else begin
                        assign result = 0;
                    end
                    assign literals = $countones('1) + $countones(-1) + $countones('x) + $countones('z);
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("countones_constants.sv"))], "Top")
            .four_state(true);

        sim.modify(|_| {}).unwrap();
        assert_eq!(sim.get(sim.signal("result")), 144u32.into());
        assert_eq!(sim.get(sim.signal("literals")), 33u32.into());
    }

    fn bit_vector_predicates_preserve_argument_and_return_types(sim) {
        @setup {
            let source = r#"
                module Child(input logic [63:0] value, output logic [63:0] y);
                    assign y = value;
                endmodule
                module Top(input logic signed [7:0] a,
                           output logic [63:0] hot, hot0, unknown, via_child,
                           output logic [63:0] inverted, via_function,
                           output logic incremented, selected, nested, negative,
                           output logic [7:0] decoded);
                    function automatic logic predicate(input logic [7:0] v);
                        return $onehot(v);
                    endfunction
                    assign hot = $onehot(a);
                    assign hot0 = $onehot0(a);
                    assign unknown = $isunknown(a);
                    assign inverted = ~$onehot(a);
                    assign via_function = predicate(a);
                    assign incremented = $onehot(a + 8'd1);
                    assign selected = $onehot(a[5:2]);
                    assign nested = $onehot($onehot0(a));
                    assign negative = ($onehot(a) - 32'sd2) < 0;
                    Child child(.value($onehot(a)), .y(via_child));
                    always_comb begin
                        case ($onehot(a))
                            1'b0: decoded = 8'h35;
                            1'b1: decoded = 8'ha7;
                        endcase
                    end
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("bit_vector_predicates.sv"))], "Top");

        let a = sim.signal("a");
        for value in 0u8..=255 {
            sim.modify(|io| io.set(a, value)).unwrap();
            let hot = u8::from(value.count_ones() == 1);
            let hot0 = u8::from(value.count_ones() <= 1);
            for name in ["hot", "via_child", "via_function"] {
                assert_eq!(sim.get(sim.signal(name)), hot.into(), "{name}: {value}");
            }
            assert_eq!(sim.get(sim.signal("hot0")), hot0.into());
            assert_eq!(sim.get(sim.signal("unknown")), 0u8.into());
            assert_eq!(sim.get(sim.signal("inverted")), (!(hot as u64)).into());
            assert_eq!(sim.get(sim.signal("incremented")), u8::from(value.wrapping_add(1).count_ones() == 1).into());
            assert_eq!(sim.get(sim.signal("selected")), u8::from(((value >> 2) & 15).count_ones() == 1).into());
            assert_eq!(sim.get(sim.signal("nested")), hot0.into());
            assert_eq!(sim.get(sim.signal("negative")), 0u8.into());
            assert_eq!(sim.get(sim.signal("decoded")), (if hot == 1 { 0xa7u8 } else { 0x35u8 }).into());
        }
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
        @setup {
            let source = r#"
                module Top(output logic [7:0] result, output logic [3:0] literals);
                    localparam H = $onehot(8'b0x0z_0001);
                    localparam Z = $onehot0('z);
                    localparam U = $isunknown(256'hx);
                    if (H && Z && U && $bits(H) == 1 && $size(U) == 1) begin
                        assign result = 8'ha5;
                    end else begin
                        assign result = 0;
                    end
                    assign literals = {$onehot('1), $onehot0('x), $isunknown('z), $isunknown('0)};
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("bit_vector_predicate_constants.sv"))], "Top")
            .four_state(true);

        sim.modify(|_| {}).unwrap();
        assert_eq!(sim.get_four_state(sim.signal("result")), (0xa5u8.into(), 0u8.into()));
        assert_eq!(sim.get_four_state(sim.signal("literals")), (0b1110u8.into(), 0u8.into()));
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
