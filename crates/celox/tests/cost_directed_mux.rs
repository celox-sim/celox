use celox::SimulatorBuilder;

#[test]
fn expensive_always_comb_if_preserves_cfg_and_executes_both_arms() {
    let source = r#"
module Top (
    sel: input logic,
    a: input logic<32>,
    b: input logic<32>,
    o: output logic<32>,
) {
    var shared: logic<32>;
    always_comb {
        shared = a * b;
        if sel {
            o = shared + ((((((((a + 32'd1) + 32'd2) + 32'd3) + 32'd4) + 32'd5) + 32'd6) + 32'd7) + 32'd8);
        } else {
            o = shared ^ ((((((((b + 32'd11) + 32'd12) + 32'd13) + 32'd14) + 32'd15) + 32'd16) + 32'd17) + 32'd18);
        }
    }
}
"#;

    let result = SimulatorBuilder::new(source, "Top")
        .optimize(false)
        .trace_pre_optimized_sir()
        .build_with_trace();
    let sir = result.trace.pre_optimized_sir.as_ref().unwrap();
    let rendered_sir = sir
        .sir
        .updates
        .observe
        .iter()
        .map(|eu| format!("{eu}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered_sir.contains("Branch("), "{rendered_sir}");

    let mut sim = result.res.unwrap();
    let sel = sim.signal("sel");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");
    let a_value = 0x1234_5678u32;
    let b_value = 7u32;
    let shared = a_value.wrapping_mul(b_value);

    sim.modify(|io| {
        io.set(a, a_value);
        io.set(b, b_value);
        io.set(sel, 1u8);
    })
    .unwrap();
    assert_eq!(
        sim.get(o),
        shared.wrapping_add(a_value.wrapping_add(36)).into()
    );

    sim.modify(|io| io.set(sel, 0u8)).unwrap();
    assert_eq!(sim.get(o), (shared ^ b_value.wrapping_add(116)).into());
}

#[test]
fn scalar_case_with_a_data_bit_lowers_to_one_word() {
    use celox::{OptLevel, Simulator};
    use celox_design::BinaryOp;
    use celox_sir::SIRInstruction;
    use std::path::Path;

    // The E906 rounding decision has 16 entries and a zero default. Also
    // exercise the top bit of a full u64 table with a data-dependent default.
    for (width, default_arm) in [(3, "1'b0"), (5, "sign")] {
        let source = format!(
            r#"
            module Top(input bit [{msb}:0] mode, input bit sign, output bit y);
                always @(mode or sign) begin
                    case (mode)
                        {width}'d0: y = 1'b0;
                        {width}'d1: y = 1'b1;
                        {width}'d2: y = !sign;
                        {width}'d3: y = sign;
                        default: y = {default_arm};
                    endcase
                end
            endmodule
        "#,
            msb = width - 1
        );
        let result =
            Simulator::from_sv_sources(vec![(source.as_str(), Path::new("round.sv"))], "Top")
                .four_state(false)
                .opt_level(OptLevel::O2)
                .trace_post_optimized_sir()
                .build_with_trace();
        let mut sim = result.res.unwrap();
        let sir = result.trace.post_optimized_sir.unwrap();
        let blocks: Vec<_> = sir
            .sir
            .updates
            .observe
            .iter()
            .flat_map(|eu| eu.blocks.values())
            .collect();
        assert_eq!(blocks.len(), 1);
        assert!(
            blocks[0]
                .instructions
                .iter()
                .any(|op| matches!(op, SIRInstruction::Binary(_, _, BinaryOp::Shr, _)))
        );

        let mode = sim.signal("mode");
        let sign = sim.signal("sign");
        let y = sim.signal("y");
        for m in 0..(1u8 << width) {
            for s in 0..2u8 {
                sim.modify(|io| {
                    io.set(mode, m);
                    io.set(sign, s);
                })
                .unwrap();
                let expected = match m {
                    0 => 0,
                    1 => 1,
                    2 => s ^ 1,
                    3 => s,
                    _ if width == 5 => s,
                    _ => 0,
                };
                assert_eq!(sim.get(y), expected.into(), "mode={m}, sign={s}");
            }
        }
    }
}

#[test]
fn shared_scalar_bank_uses_consecutive_homes_in_selector_order() {
    check_shared_scalar_bank("logic");
}

#[test]
fn bit_assignments_preserve_shared_scalar_bank_layout() {
    check_shared_scalar_bank("bit");
}

fn check_shared_scalar_bank(data_type: &str) {
    use celox::{OptLevel, Simulator};
    use std::fmt::Write;
    use std::path::Path;

    let mut source = format!("module Top(input bit [5:0] a, b, output {data_type} [7:0] x, y");
    for i in (1..=33).rev() {
        write!(source, ", input {data_type} [7:0] data{i}").unwrap();
    }
    source.push_str(");\n");
    for (key, result) in [("a", "x"), ("b", "y")] {
        writeln!(source, "always_comb case ({key})").unwrap();
        for i in 1..=33 {
            writeln!(source, "6'd{i}: {result} = data{i};").unwrap();
        }
        writeln!(source, "default: {result} = 0; endcase").unwrap();
    }
    source.push_str("endmodule");
    let mut sim = Simulator::from_sv_sources(vec![(&source, Path::new("bank.sv"))], "Top")
        .four_state(false)
        .opt_level(OptLevel::O2)
        .build_native()
        .unwrap();
    let bank: Vec<_> = (1..=33).map(|i| sim.signal(&format!("data{i}"))).collect();
    assert!(
        bank.windows(2).all(|p| p[0].offset + 1 == p[1].offset),
        "{bank:?}"
    );
    let (a, b, x, y) = (
        sim.signal("a"),
        sim.signal("b"),
        sim.signal("x"),
        sim.signal("y"),
    );
    sim.modify(|io| {
        for (i, &input) in bank.iter().enumerate() {
            io.set(input, i as u8 + 1);
        }
    })
    .unwrap();
    for key in 0..64u8 {
        sim.modify(|io| {
            io.set(a, key);
            io.set(b, 63 - key);
        })
        .unwrap();
        let expected = |k| if (1..=33).contains(&k) { k } else { 0u8 };
        assert_eq!(sim.get(x), expected(key).into());
        assert_eq!(sim.get(y), expected(63 - key).into());
    }
}
