use celox::SimulatorBuilder;
use num_bigint::BigUint;

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

fn recovered_unrolled_guard_uses_procedural_four_state_truth(sim) {
    @omit_veryl;
    @setup { let code = r#"
        module Top (
            cond : input  logic<8>,
            bits : input  logic<4>,
            value: output logic<4>,
        ) {
            var state: logic<4>;
            always_comb {
                state = 4'd0;
                if cond {
                    for i in 0..4 {
                        state[i] = bits[i];
                    }
                }
                value = state;
            }
        }
    "#; }
    @build SimulatorBuilder::new(code, "Top").four_state(true);

    let cond = sim.signal("cond");
    let bits = sim.signal("bits");
    let value = sim.signal("value");
    let unknown = BigUint::from(1u8) << 2usize;

    sim.modify(|io| {
        io.set(bits, 0b1010u8);
        io.set_four_state(cond, unknown.clone(), unknown.clone());
    })
    .unwrap();
    sim.eval_comb().unwrap();
    assert_eq!(
        sim.get_four_state(value),
        (BigUint::from(0u8), BigUint::from(0u8)),
        "an entirely unknown procedural guard must not enter the recovered loop",
    );

    let known_one = BigUint::from(1u8) << 7usize;
    sim.modify(|io| {
        io.set_four_state(cond, &known_one | &unknown, unknown.clone());
    })
    .unwrap();
    sim.eval_comb().unwrap();
    assert_eq!(
        sim.get_four_state(value),
        (BigUint::from(0b1010u8), BigUint::from(0u8)),
        "a known one bit must make the procedural guard true despite another unknown bit",
    );
}

}
