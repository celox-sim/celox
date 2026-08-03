use super::*;

sv_backends! {
    fn simulates_systemverilog_parameterized_port_widths(sim) {
        @setup {
    let sv = r#"
        module Top #(
            parameter WIDTH = (4 * 2)
        ) (
            input logic [WIDTH - 1:0] a,
            output logic [WIDTH - 1:0] y
        );
            assign y = a + 8'd1;
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("parameter_width.sv"))], "Top");

    let a = sim.signal("a");
    let y = sim.signal("y");

    sim.modify(|io| io.set(a, 0x7fu8)).unwrap();

    assert_eq!(sim.get(y), 0x80u8.into());
    }

    fn simulates_systemverilog_bit_ports_as_two_state(sim) {
        @setup {
    let sv = r#"
        module Top(
            output bit [3:0] y
        );
            assign y = 4'b10xz;
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("bit_port.sv"))], "Top")
            .four_state(true);

    let y = sim.signal("y");
    sim.modify(|_| {}).unwrap();

    assert_eq!(sim.get(y), BigUint::from(0b1000u32));
    let (_value, mask) = sim.get_four_state(y);
    assert_eq!(mask, BigUint::from(0u32));
    }

    fn preserves_integer_atom_state_kinds(sim) {
        @setup {
    let sv = r#"
        module Top(input logic a, output int two_state, output integer four_state);
            assign two_state = a;
            assign four_state = a;
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("integer_atoms.sv"))], "Top")
            .four_state(true);

    let a = sim.signal("a");
    sim.modify(|io| {
        io.set_four_state(a, BigUint::from(1u8), BigUint::from(1u8));
    }).unwrap();

    assert_eq!(
        sim.get_four_state(sim.signal("two_state")),
        (BigUint::default(), BigUint::default())
    );
    assert_ne!(
        sim.get_four_state(sim.signal("four_state")).1,
        BigUint::default()
    );
    }
}
