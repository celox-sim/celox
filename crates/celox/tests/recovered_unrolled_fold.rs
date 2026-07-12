use celox::SimulatorBuilder;

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

const MULTI_STATE_LOOP: &str = r#"
    module Top (
        en       : input  logic,
        bits     : input  logic<4>,
        out_data : output logic<3>,
    ) {
        var valid: logic;
        var age  : logic<3>;
        var data : logic<3>;

        always_comb {
            valid = 1'b0;
            age   = 3'd0;
            data  = 3'd0;
            if en {
                for i in 0..4 {
                    let ai: logic<3> = (i as 3);
                    if bits[i] && (!valid || ai >: age) {
                        valid = 1'b1;
                        age   = ai;
                        data  = ai;
                    }
                }
            }
            out_data  = data;
        }
    }
"#;

all_backends! {

fn recovered_unrolled_multi_state_priority(sim) {
    @omit_veryl;
    @setup { let code = MULTI_STATE_LOOP; }
    @build SimulatorBuilder::new(code, "Top");

    let en = sim.signal("en");
    let bits = sim.signal("bits");
    let out_data = sim.signal("out_data");

    for enabled in [0u8, 1u8] {
        for input in 0u8..16 {
            sim.set(en, enabled);
            sim.set(bits, input);
            sim.eval_comb().unwrap();

            let expected_priority = if enabled == 0 || input == 0 {
                0
            } else {
                (7 - input.leading_zeros()) as u8
            };
            assert_eq!(
                sim.get(out_data),
                expected_priority.into(),
                "data for en={enabled} bits={input:04b}"
            );
        }
    }
}

}
