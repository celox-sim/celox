use super::*;

sv_backends! {
    fn preserves_positional_veryl_to_systemverilog_port_order(sim) {
        @setup {
    let veryl = r#"
        module Top (
            a: input logic,
            b: input logic,
            y: output logic<2>,
        ) {
            inst child: $sv::PositionalPorts (b, a, y);
        }
    "#;
    let sv = r#"
        module PositionalPorts(input logic a, input logic b, output logic [1:0] y);
            assign y = {a, b};
        endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("positional_ports.veryl"))],
            vec![(sv, Path::new("positional_ports.sv"))],
            "Top",
        );

    let a = sim.signal("a");
    let b = sim.signal("b");
    sim.modify(|io| {
        io.set(a, 1u8);
        io.set(b, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 1u8.into());
    }

    fn preserves_out_of_order_named_veryl_to_systemverilog_ports(sim) {
        @setup {
    let veryl = r#"
        module Top (
            xa: input logic,
            xb: input logic,
            y: output logic<2>,
        ) {
            inst child: $sv::NamedPorts (
                b: xb,
                a: xa,
                y,
            );
        }
    "#;
    let sv = r#"
        module NamedPorts(input logic a, input logic b, output logic [1:0] y);
            assign y = {a, b};
        endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("named_ports.veryl"))],
            vec![(sv, Path::new("named_ports.sv"))],
            "Top",
        );

    let xa = sim.signal("xa");
    let xb = sim.signal("xb");
    sim.modify(|io| {
        io.set(xa, 1u8);
        io.set(xb, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(sim.signal("y")), 2u8.into());
    }

    fn converts_veryl_logic_to_systemverilog_bit_input(sim) {
        @setup {
    let veryl = r#"
        module Top (
            a: input logic,
            y: output logic,
        ) {
            inst child: $sv::BitInput (a, y);
        }
    "#;
    let sv = r#"
        module BitInput(input bit a, output logic y); assign y = a; endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))],
            vec![(sv, Path::new("bit_input.sv"))],
            "Top",
        ).four_state(true);

    let a = sim.signal("a");
    sim.modify(|io| {
        io.set_four_state(a, BigUint::from(1u8), BigUint::from(1u8));
    }).unwrap();
    assert_eq!(sim.get(sim.signal("y")), 0u8.into());
    }

    fn simulates_veryl_top_with_systemverilog_child(sim) {
        @setup {
    let veryl = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            y: output logic<8>,
        ) {
            inst u_xor: $sv::Xor8 (
                a,
                b,
                y,
            );
        }
    "#;
    let sv = r#"
        module Xor8(input logic [7:0] a, input logic [7:0] b, output logic [7:0] y);
            always_comb begin
                y = a ^ b;
            end
        endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
        vec![(veryl, Path::new("top.veryl"))],
        vec![(sv, Path::new("xor8.sv"))],
        "Top",
    );

    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");

    sim.modify(|io| {
        io.set(a, 0xf0u8);
        io.set(b, 0x33u8);
    })
    .unwrap();

    assert_eq!(sim.get(y), 0xc3u8.into());
    }

    fn simulates_veryl_top_with_nested_systemverilog_hierarchy(sim) {
        @setup {
    let veryl = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            y: output logic<8>,
        ) {
            inst u_wrapper: $sv::Wrapper (
                a,
                b,
                y,
            );
        }
    "#;
    let sv = r#"
        module Xor8(input logic [7:0] a, input logic [7:0] b, output logic [7:0] y);
            assign y = a ^ b;
        endmodule

        module Wrapper(input logic [7:0] a, input logic [7:0] b, output logic [7:0] y);
            Xor8 u_xor(.a(a), .b(b), .y(y));
        endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
        vec![(veryl, Path::new("top.veryl"))],
        vec![(sv, Path::new("nested.sv"))],
        "Top",
    );

    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");

    sim.modify(|io| {
        io.set(a, 0xa5u8);
        io.set(b, 0x3cu8);
    })
    .unwrap();

    assert_eq!(sim.get(y), 0x99u8.into());
    }

    fn zero_extends_signed_veryl_selections_into_systemverilog_inputs(sim) {
        @setup {
    let veryl = r#"
        module Top (
            a: input signed logic<8>,
            part_y: output logic<16>,
            bit_y: output logic<16>,
        ) {
            inst child: $sv::SelectionInputs (
                part_value: a[7:0],
                bit_value: a[7],
                part_y,
                bit_y,
            );
        }
    "#;
    let sv = r#"
        module SelectionInputs(
            input logic [15:0] part_value,
            input logic [15:0] bit_value,
            output logic [15:0] part_y,
            output logic [15:0] bit_y
        );
            assign part_y = part_value;
            assign bit_y = bit_value;
        endmodule
    "#;
        }
        @build Simulator::from_mixed_sources(
            vec![(veryl, Path::new("signed_selection.veryl"))],
            vec![(sv, Path::new("selection_inputs.sv"))],
            "Top",
        );

    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 0x80u8)).unwrap();
    assert_eq!(sim.get(sim.signal("part_y")), 0x0080u16.into());
    assert_eq!(sim.get(sim.signal("bit_y")), 1u16.into());
    }
}
