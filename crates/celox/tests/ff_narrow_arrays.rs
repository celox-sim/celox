use celox::{OptLevel, SimulatorBuilder};

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {
fn ff_captures_every_one_bit_array_element_without_optimization(sim) {
    @setup {
        let source = r#"
            module Top (clk: input clock, d: input logic<8>, q: output logic<8>) {
                var flags: logic [8];
                always_ff (clk) {
                    for i in 0..8 { flags[i] = d[i]; }
                }
                assign q = {flags[7], flags[6], flags[5], flags[4], flags[3], flags[2], flags[1], flags[0]};
            }
        "#;
    }
    @build SimulatorBuilder::new(source, "Top").opt_level(OptLevel::O0);
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");
    for value in [0u8, 0xff, 0x55, 0xaa, 0x81, 0x7e] {
        sim.modify(|io| io.set(d, value)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(q), value.into(), "input {value:#x}");
    }
}
}

all_backends! {
fn ff_captures_padded_elements_and_unknown_masks(sim) {
    // The SV frontend currently rejects four-state always_ff event signals.
    // This limitation does not apply to the four Celox execution backends.
    @ignore_on(sv);
    @setup {
        let source = r#"
            module Top (clk: input clock, d: input logic<24>, q: output logic<24>) {
                var lanes: logic<3> [8];
                always_ff (clk) {
                    for i in 0..8 { lanes[i] = d[i * 3 +: 3]; }
                }
                assign q = {lanes[7], lanes[6], lanes[5], lanes[4], lanes[3], lanes[2], lanes[1], lanes[0]};
            }
        "#;
    }
    @build SimulatorBuilder::new(source, "Top").opt_level(OptLevel::O0).four_state(true);
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");
    for (value, mask) in [(0xffffffu32, 0u32), (0x91a653, 0x263ac9), (0x123456, 0xe39a58), (0, 0xffffff)] {
        // Set masked bits to X (value=1, mask=1), including an all-X pattern.
        let value = celox::BigUint::from(value | mask);
        let mask = celox::BigUint::from(mask);
        sim.modify(|io| io.set_four_state(d, value.clone(), mask.clone())).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get_four_state(q), (value, mask));
    }
}
}
