use celox::Simulator;

const ONEHOT_SRC: &str =
    include_str!("../../../deps/veryl/crates/std/veryl/src/countones/onehot.veryl");

/// Exhaustive 8-bit onehot detection
#[test]
fn test_onehot_8bit_exhaustive() {
    let top = r#"
module Top (
    i_data  : input  logic<8>,
    o_onehot: output logic,
    o_zero  : output logic,
) {
    inst u: onehot #(W: 8) (
        i_data,
        o_onehot,
        o_zero,
    );
}
"#;
    let code = format!("{ONEHOT_SRC}\n{top}");
    let mut sim = Simulator::builder(&code, "Top").build().unwrap();
    let i_data = sim.signal("i_data");
    let o_onehot = sim.signal("o_onehot");
    let o_zero = sim.signal("o_zero");

    for val in 0u16..256 {
        let val = val as u8;
        sim.modify(|io| io.set(i_data, val)).unwrap();

        let is_onehot = sim.get_as::<u8>(o_onehot);
        let is_zero = sim.get_as::<u8>(o_zero);

        let expected_onehot = if val.count_ones() == 1 { 1u8 } else { 0u8 };
        let expected_zero = if val == 0 { 1u8 } else { 0u8 };

        assert_eq!(
            is_onehot, expected_onehot,
            "onehot({val:#010b}): expected={expected_onehot}, got={is_onehot}"
        );
        assert_eq!(
            is_zero, expected_zero,
            "zero({val:#010b}): expected={expected_zero}, got={is_zero}"
        );
    }
}
