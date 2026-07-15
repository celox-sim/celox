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
        .eval_comb
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
