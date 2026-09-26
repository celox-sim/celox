use crate::Design;

cases! { Regression, "issue3_repro";


fn test_comb_mux_i8_vs_i16_correctness(sim) {
    @setup { let code = r#"
module Top (
    en: input logic,
    a9: input logic<9>,
    out: output logic<9>,
) {
    assign out = if en ? en : a9;
}
"#; }
    @build Design::new(code, "Top");
    let en = sim.signal("en");
    let a9 = sim.signal("a9");
    let out = sim.signal("out");

    // en=0: output is a9
    sim.modify(|io| {
        io.set(en, 0u8);
        io.set(a9, 0x155u16);
    })
    .unwrap();
    assert_eq!(sim.get(out), 0x155u16.into(), "en=0: out should equal a9");

    // en=1: output is en (1-bit=1) zero-extended to 9 bits → 1
    sim.modify(|io| {
        io.set(en, 1u8);
        io.set(a9, 0x155u16);
    })
    .unwrap();
    assert_eq!(
        sim.get(out),
        1u16.into(),
        "en=1: out should be 1 (en zero-extended)"
    );
}



// 4-state mode: exercises the mask-cast path in translate_terminator.
// A ternary with i8->i16 boundary must compile and produce correct results
// with X/Z propagation enabled.
fn test_comb_mux_i8_vs_i16_four_state(sim) {
    @setup { let code = r#"
module Top (
    en: input logic,
    a9: input logic<9>,
    out: output logic<9>,
) {
    assign out = if en ? en : a9;
}
"#; }
    @build Design::new(code, "Top")
        .four_state(true);
    let en = sim.signal("en");
    let a9 = sim.signal("a9");
    let out = sim.signal("out");

    // en=0: output is a9
    sim.modify(|io| {
        io.set(en, 0u8);
        io.set(a9, 0x155u16);
    })
    .unwrap();
    assert_eq!(
        sim.get(out),
        0x155u16.into(),
        "4-state en=0: out should equal a9"
    );

    // en=1: output is 1 (en zero-extended)
    sim.modify(|io| io.set(en, 1u8)).unwrap();
    assert_eq!(sim.get(out), 1u16.into(), "4-state en=1: out should be 1");
}
}
