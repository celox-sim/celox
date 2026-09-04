use super::*;

sv_backends! {
    fn simulates_systemverilog_always_comb_dependency(sim) {
        @setup {
    let sv = r#"
        module Top(
            input logic [7:0] a,
            input logic [7:0] b,
            output logic [7:0] y,
            output logic [7:0] z
        );
            always_comb begin
                y = a ^ b;
                z = y | 8'h80;
            end
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("always_comb.sv"))], "Top");

    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");
    let z = sim.signal("z");

    sim.modify(|io| {
        io.set(a, 0x55u8);
        io.set(b, 0x0fu8);
    })
    .unwrap();

    assert_eq!(sim.get(y), 0x5au8.into());
    assert_eq!(sim.get(z), 0xdau8.into());
    }
}

sv_backends! {
    fn preserves_intervening_reads_when_merging_conditional_writes(sim) {
        @setup {
    let sv = r#"
        module Top(
            input logic c,
            input logic d,
            output logic x,
            output logic y
        );
            always_comb begin
                x = d;
                y = x;
                if (c) x = 1'b1;
            end
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("always_comb_order.sv"))], "Top");

    let c = sim.signal("c");
    let d = sim.signal("d");
    let x = sim.signal("x");
    let y = sim.signal("y");

    sim.modify(|io| {
        io.set(c, 1u8);
        io.set(d, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(x), 1u8.into());
    assert_eq!(sim.get(y), 0u8.into());

    sim.modify(|io| {
        io.set(c, 0u8);
        io.set(d, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(x), 1u8.into());
    assert_eq!(sim.get(y), 1u8.into());
    }
}
