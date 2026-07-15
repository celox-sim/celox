use celox::{BigUint, SimulatorBuilder};

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

fn scatter_source() -> String {
    let mut source = String::from(
        r#"
module Top (
    enable  : input  logic,
    selector: input  logic<5>,
    value   : input  logic<6>,
    base    : input  logic<192>,
    lanes   : output logic<192>,
    sentinel: output logic<64>,
) {
    always_comb {
        lanes = base;
        sentinel = 64'ha55a_c33c_f00f_9669;
        if enable {
"#,
    );
    for lane in 0..32 {
        source.push_str(&format!(
            "            if selector == 5'd{lane} {{ lanes[{} +: 6] = value; }}\n",
            lane * 6
        ));
    }
    source.push_str(
        r#"        }
    }
}
"#,
    );
    source
}

fn packed_pattern() -> BigUint {
    (0..24usize).fold(BigUint::from(0u8), |value, byte| {
        value | (BigUint::from(((byte * 37) as u8) ^ 0xa5) << (byte * 8))
    })
}

all_backends! {

fn packed_scatter_last_lane_does_not_touch_adjacent_storage(sim) {
    @omit_veryl;
    @setup { let code = scatter_source(); }
    @build SimulatorBuilder::new(&code, "Top").optimize(true);

    let enable = sim.signal("enable");
    let selector = sim.signal("selector");
    let value = sim.signal("value");
    let base = sim.signal("base");
    let lanes = sim.signal("lanes");
    let sentinel = sim.signal("sentinel");
    let base_value = packed_pattern();
    let full_mask = (BigUint::from(1u8) << 192usize) - BigUint::from(1u8);
    let lane_mask = BigUint::from(0x3fu8);
    let sentinel_value = BigUint::from(0xa55a_c33c_f00f_9669u64);

    for enabled in 0..=1u8 {
        for selected in 0..32u8 {
            let lane_value = (selected.wrapping_mul(11).wrapping_add(7)) & 0x3f;
            sim.modify(|io| {
                io.set(enable, enabled);
                io.set(selector, selected);
                io.set(value, lane_value);
                io.set_wide(base, base_value.clone());
            })
            .unwrap();
            sim.eval_comb().unwrap();

            let expected = if enabled == 0 {
                base_value.clone()
            } else {
                let offset = selected as usize * 6;
                let field = &lane_mask << offset;
                (&base_value & (&full_mask ^ &field)) | (BigUint::from(lane_value) << offset)
            };
            assert_eq!(
                sim.get(lanes),
                expected,
                "packed value for enable={enabled} selector={selected}"
            );
            assert_eq!(
                sim.get(sentinel),
                sentinel_value,
                "adjacent sentinel for enable={enabled} selector={selected}"
            );
            assert_eq!(sim.get(base), base_value, "base input was overwritten");
            assert_eq!(sim.get_as::<u8>(enable), enabled);
            assert_eq!(sim.get_as::<u8>(selector), selected);
            assert_eq!(sim.get_as::<u8>(value), lane_value);
        }
    }
}

}
