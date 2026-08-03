use super::*;

sv_backends! {
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
}
