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
