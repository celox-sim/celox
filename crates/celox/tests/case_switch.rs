use celox::{Simulator, SimulatorBuilder};

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    fn test_case_basic_comb(sim) {
        @setup { let code = r#"
module Top (
sel: input logic<2>,
o:   output logic<8>
) {
always_comb {
case sel {
2'd0: o = 8'hAA;
2'd1: o = 8'hBB;
2'd2: o = 8'hCC;
default: o = 8'hDD;
}
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let sel = sim.signal("sel");
    let o = sim.signal("o");

    sim.modify(|io| io.set(sel, 0u8)).unwrap();
    assert_eq!(sim.get(o), 0xAAu8.into());

    sim.modify(|io| io.set(sel, 1u8)).unwrap();
    assert_eq!(sim.get(o), 0xBBu8.into());

    sim.modify(|io| io.set(sel, 2u8)).unwrap();
    assert_eq!(sim.get(o), 0xCCu8.into());

    sim.modify(|io| io.set(sel, 3u8)).unwrap();
    assert_eq!(sim.get(o), 0xDDu8.into());

    }

    fn test_switch_basic_comb(sim) {
        @setup { let code = r#"
module Top (
a: input logic<8>,
o: output logic<8>
) {
always_comb {
switch {
a <: 8'd10: o = 8'h01;
a <: 8'd20: o = 8'h02;
a <: 8'd30: o = 8'h03;
default:    o = 8'hFF;
}
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let o = sim.signal("o");

    sim.modify(|io| io.set(a, 5u8)).unwrap();
    assert_eq!(sim.get(o), 0x01u8.into());

    sim.modify(|io| io.set(a, 15u8)).unwrap();
    assert_eq!(sim.get(o), 0x02u8.into());

    sim.modify(|io| io.set(a, 25u8)).unwrap();
    assert_eq!(sim.get(o), 0x03u8.into());

    sim.modify(|io| io.set(a, 50u8)).unwrap();
    assert_eq!(sim.get(o), 0xFFu8.into());

    }

    fn test_case_multiarm(sim) {
        @setup { let code = r#"
module Top (
sel: input logic<3>,
o:   output logic<8>
) {
always_comb {
case sel {
3'd0, 3'd1: o = 8'hAA;
3'd2, 3'd3: o = 8'hBB;
default:    o = 8'h00;
}
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let sel = sim.signal("sel");
    let o = sim.signal("o");

    sim.modify(|io| io.set(sel, 0u8)).unwrap();
    assert_eq!(sim.get(o), 0xAAu8.into());

    sim.modify(|io| io.set(sel, 1u8)).unwrap();
    assert_eq!(sim.get(o), 0xAAu8.into());

    sim.modify(|io| io.set(sel, 2u8)).unwrap();
    assert_eq!(sim.get(o), 0xBBu8.into());

    sim.modify(|io| io.set(sel, 3u8)).unwrap();
    assert_eq!(sim.get(o), 0xBBu8.into());

    sim.modify(|io| io.set(sel, 4u8)).unwrap();
    assert_eq!(sim.get(o), 0x00u8.into());

    }

    fn test_case_nested_in_if(sim) {
        @setup { let code = r#"
module Top (
en:  input logic,
sel: input logic<2>,
o:   output logic<8>
) {
always_comb {
if en {
case sel {
2'd0: o = 8'h11;
2'd1: o = 8'h22;
default: o = 8'h33;
}
} else {
o = 8'h00;
}
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let en = sim.signal("en");
    let sel = sim.signal("sel");
    let o = sim.signal("o");

    sim.modify(|io| {
        io.set(en, 0u8);
        io.set(sel, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(o), 0x00u8.into());

    sim.modify(|io| io.set(en, 1u8)).unwrap();
    assert_eq!(sim.get(o), 0x22u8.into());

    sim.modify(|io| io.set(sel, 0u8)).unwrap();
    assert_eq!(sim.get(o), 0x11u8.into());

    }

    fn test_case_block_body(sim) {
        @setup { let code = r#"
module Top (
sel: input logic<2>,
o1:  output logic<8>,
o2:  output logic<8>
) {
always_comb {
case sel {
2'd0: {
o1 = 8'hAA;
o2 = 8'h55;
}
2'd1: {
o1 = 8'h55;
o2 = 8'hAA;
}
default: {
o1 = 8'h00;
o2 = 8'h00;
}
}
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let sel = sim.signal("sel");
    let o1 = sim.signal("o1");
    let o2 = sim.signal("o2");

    sim.modify(|io| io.set(sel, 0u8)).unwrap();
    assert_eq!(sim.get(o1), 0xAAu8.into());
    assert_eq!(sim.get(o2), 0x55u8.into());

    sim.modify(|io| io.set(sel, 1u8)).unwrap();
    assert_eq!(sim.get(o1), 0x55u8.into());
    assert_eq!(sim.get(o2), 0xAAu8.into());

    }
}

#[test]
fn test_case_in_always_ff() {

    let code = r#"
        module Top (
            clk: input clock,
            sel: input logic<2>,
            o:   output logic<8>
        ) {
            var r_val: logic<8>;
            always_ff {
                case sel {
                    2'd0: r_val = 8'h10;
                    2'd1: r_val = 8'h20;
                    default: r_val = 8'hFF;
                }
            }
            assign o = r_val;
        }
    "#;
    let result = SimulatorBuilder::new(code, "Top")
        .trace_analyzer_ir()
        .trace_sim_modules()
        .trace_post_optimized_sir()
        .trace_post_optimized_clif()
        .build_with_trace();
    let trace = result.trace;
    let sim = result.res;
    println!("{}", trace.analyzer_ir.clone().unwrap());

    println!("{}", trace.format_slt().unwrap());
    println!("{}", trace.format_post_optimized_sir().unwrap());
    if let Some(clif) = &trace.post_optimized_clif {
        println!("{}", clif);
    }

    let mut sim = sim.unwrap();
    let clk = sim.event("clk");
    let sel = sim.signal("sel");
    let o = sim.signal("o");

    sim.modify(|io| io.set(sel, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o), 0x10u8.into());

    sim.modify(|io| io.set(sel, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o), 0x20u8.into());

    sim.modify(|io| io.set(sel, 3u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o), 0xFFu8.into());

}
