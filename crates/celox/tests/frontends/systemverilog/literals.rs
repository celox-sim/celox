use super::*;

#[test]
fn simulates_systemverilog_literals_with_widths_and_masks() {
    let sv = r#"
        module Top(
            output logic [7:0] y,
            output logic [3:0] z
        );
            assign y = 4'hff;
            assign z = 4'b10xz;
        endmodule
    "#;

    let mut sim = Simulator::from_sv_sources(vec![(sv, Path::new("literal.sv"))], "Top")
        .four_state(true)
        .build()
        .expect("SV simulation should build");

    let y = sim.signal("y");
    let z = sim.signal("z");
    sim.modify(|_| {}).unwrap();

    assert_eq!(sim.get(y), BigUint::from(0x0fu32));
    let (z_value, z_mask) = sim.get_four_state(z);
    assert_eq!(z_value, BigUint::from(0b1010u32));
    assert_eq!(z_mask, BigUint::from(0b0011u32));
}
