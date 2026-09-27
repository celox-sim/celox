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
}

#[test]
fn rejects_countones_with_missing_or_extra_arguments() {
    for call in [
        "$countones()",
        "$countones(a, a)",
        "$countones(, a)",
        "$countones(a,)",
    ] {
        let source =
            format!("module Top(input logic a, output int y); assign y = {call}; endmodule");
        let result =
            Simulator::from_sv_sources(vec![(&source, Path::new("invalid_countones.sv"))], "Top")
                .build_cranelift();
        assert!(result.is_err(), "invalid call should be rejected: {call}");
    }
}
