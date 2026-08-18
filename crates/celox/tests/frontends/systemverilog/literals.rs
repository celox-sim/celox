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

sv_backends! {
    fn simulates_systemverilog_unbased_unsized_literals(sim) {
        @setup {
    let sv = r#"
        module Top(
            output logic [3:0] y,
            output logic [3:0] z
        );
            assign y = '1;
            assign z = '0;
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("unbased_unsized.sv"))], "Top");

    let y = sim.signal("y");
    let z = sim.signal("z");
    sim.modify(|_| {}).unwrap();

    assert_eq!(sim.get(y), 0x0fu8.into());
    assert_eq!(sim.get(z), 0x00u8.into());
    }

    fn simulates_systemverilog_four_state_input_operator_masks(sim) {
        @setup {
    let sv = r#"
        module Top(
            input logic [3:0] a,
            output logic [3:0] y
        );
            assign y = a ^ 4'b1010;
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("four_state_input_ops.sv"))], "Top")
            .four_state(true);

    let a = sim.signal("a");
    let y = sim.signal("y");
    sim.modify(|io| {
        io.set_four_state(a, BigUint::from(0b1010u32), BigUint::from(0b0011u32));
    })
    .unwrap();

    let (value, mask) = sim.get_four_state(y);
    assert_eq!(value, BigUint::from(0b0011u32));
    assert_eq!(mask, BigUint::from(0b0011u32));
    }

    fn simulates_systemverilog_four_state_literal_operator_masks(sim) {
        @setup {
    let sv = r#"
        module Top(
            output logic [3:0] y
        );
            assign y = 4'b1010 ^ 4'b00xz;
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("four_state_ops.sv"))], "Top")
            .four_state(true);

    let y = sim.signal("y");
    sim.modify(|_| {}).unwrap();

    let (value, mask) = sim.get_four_state(y);
    assert_eq!(value, BigUint::from(0b1011u32));
    assert_eq!(mask, BigUint::from(0b0011u32));
    }
}
