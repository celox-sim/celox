use celox::Simulator;

const NESTED_WRITE: &str = r#"
pub module AdcGroupNestedWriteMre (
    i_primary       : input  logic [8],
    i_overflow      : input  logic [8],
    i_primary_active: input  logic [2],
    o_pop           : output logic [8],
) {
    always_comb {
        for bank in 0..2 {
            for lane in 0..4 {
                let index: u32 = bank * 4 + lane;
                o_pop[index] = 0;
                if i_primary[index] {
                    o_pop[index] = 1;
                }
                if i_overflow[index] && !i_primary_active[bank] {
                    o_pop[index] = 1;
                }
            }
        }
    }
}
"#;

#[test]
fn nested_loop_partial_writes_compile_and_simulate() {
    let mut sim = Simulator::builder(NESTED_WRITE, "AdcGroupNestedWriteMre")
        .build()
        .expect("nested partial writes should compile");
    let i_primary = sim.signal("i_primary");
    let i_overflow = sim.signal("i_overflow");
    let i_primary_active = sim.signal("i_primary_active");
    let o_pop = sim.signal("o_pop");

    sim.modify(|io| {
        io.set(i_primary, 0b0100_0001u8);
        io.set(i_overflow, 0b1010_1010u8);
        io.set(i_primary_active, 0b01u8);
    })
    .unwrap();

    // Bank 0 is active, so only its primary request pops. Bank 1 admits
    // overflow requests in addition to its primary request.
    assert_eq!(sim.get(o_pop), 0b1110_0001u8.into());
}
