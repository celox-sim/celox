use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    fn test_array_literal_comb_assignment(sim) {
        @setup { let code = r#"
module Top (o0: output logic<8>, o1: output logic<8>) {
var a: logic<8> [2];
always_comb {
a = '{8'h12, 8'h34};
}
assign o0 = a[0];
assign o1 = a[1];
}
"#; }
        @build Simulator::builder(code, "Top");
    let o0 = sim.signal("o0");
    let o1 = sim.signal("o1");

    // Trigger combinational evaluation once.
    sim.modify(|_| {}).unwrap();

    assert_eq!(sim.get(o0), 0x12u8.into());
    assert_eq!(sim.get(o1), 0x34u8.into());

    }

    fn test_array_literal_default_comb_assignment(sim) {
        @setup { let code = r#"
module Top (
o0: output logic<8>,
o1: output logic<8>,
o2: output logic<8>,
o3: output logic<8>
) {
var a: logic<8> [4];
always_comb {
a = '{8'h12, default: 8'hAA};
}
assign o0 = a[0];
assign o1 = a[1];
assign o2 = a[2];
assign o3 = a[3];
}
"#; }
        @build Simulator::builder(code, "Top");
    let o0 = sim.signal("o0");
    let o1 = sim.signal("o1");
    let o2 = sim.signal("o2");
    let o3 = sim.signal("o3");

    sim.modify(|_| {}).unwrap();

    assert_eq!(sim.get(o0), 0x12u8.into());
    assert_eq!(sim.get(o1), 0xAAu8.into());
    assert_eq!(sim.get(o2), 0xAAu8.into());
    assert_eq!(sim.get(o3), 0xAAu8.into());

    }

    fn test_array_literal_nested_default_multidim_assignment(sim) {
        @setup { let code = r#"
module Top (
o00: output logic<8>,
o01: output logic<8>,
o10: output logic<8>,
o11: output logic<8>
) {
var a: logic<8> [2, 2];
always_comb {
a = '{
'{8'h11, default: 8'h22},
default: '{default: 8'hAA}
};
}
assign o00 = a[0][0];
assign o01 = a[0][1];
assign o10 = a[1][0];
assign o11 = a[1][1];
}
"#; }
        @build Simulator::builder(code, "Top");
    let o00 = sim.signal("o00");
    let o01 = sim.signal("o01");
    let o10 = sim.signal("o10");
    let o11 = sim.signal("o11");

    sim.modify(|_| {}).unwrap();

    assert_eq!(sim.get(o00), 0x11u8.into());
    assert_eq!(sim.get(o01), 0x22u8.into());
    assert_eq!(sim.get(o10), 0xAAu8.into());
    assert_eq!(sim.get(o11), 0xAAu8.into());

    }

    #[ignore = "blocked by upstream Veryl IR: UnsupportedByIr at conv/utils.rs:231"]
    fn test_array_literal_single_element_fills_param_sized_array(sim) {
        @setup { // '{val} with a single element (no `default:` keyword) should fill all positions
// when applied to a param-sized array, matching SV assignment-pattern semantics.
let code = r#"
module Top #(param N: u32 = 3) (
i_clk: input clock,
i_rst: input reset,
o0: output logic<8>,
o1: output logic<8>,
o2: output logic<8>,
) {
var arr: logic<8> [N];
assign o0 = arr[0];
assign o1 = arr[1];
assign o2 = arr[2];
always_ff (i_clk, i_rst) {
if_reset {
arr = '{0};
} else {
arr[0] = 8'hAB;
}
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("i_clk");
    let i_rst = sim.signal("i_rst");
    let o0 = sim.signal("o0");
    let o1 = sim.signal("o1");
    let o2 = sim.signal("o2");

    // Reset: all elements should be 0
    sim.modify(|io| io.set(i_rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o0), 0u8.into());
    assert_eq!(sim.get(o1), 0u8.into());
    assert_eq!(sim.get(o2), 0u8.into());

    }

    fn test_array_literal_single_element_size_one_array(sim) {
        @ignore_on(veryl);
        @setup { // '{val} on a 1-element array should still assign that one element correctly.
let code = r#"
module Top (
i_clk: input clock,
i_rst: input reset,
o0: output logic<8>,
) {
var arr: logic<8> [1];
assign o0 = arr[0];
always_ff (i_clk, i_rst) {
if_reset {
arr = '{0};
} else {
arr[0] = 8'hAB;
}
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("i_clk");
    let i_rst = sim.signal("i_rst");
    let o0 = sim.signal("o0");

    sim.modify(|io| io.set(i_rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o0), 0u8.into());

    sim.modify(|io| io.set(i_rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o0), 0xABu8.into());

    }

    #[ignore = "blocked by upstream Veryl IR: UnsupportedByIr at conv/utils.rs:231"]
    fn test_array_literal_single_element_fills_2d_array(sim) {
        @setup { // '{0} on a 2D param-sized array should also fill all elements.
let code = r#"
module Top #(param N: u32 = 2, param M: u32 = 3) (
i_clk: input clock,
i_rst: input reset,
o: output logic<8>,
) {
var arr: logic<8> [N, M];
assign o = arr[1][2];
always_ff (i_clk, i_rst) {
if_reset {
arr = '{0};
} else {
arr[0][0] = 8'hAB;
}
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("i_clk");
    let i_rst = sim.signal("i_rst");
    let o = sim.signal("o");

    sim.modify(|io| io.set(i_rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o), 0u8.into());

    }

    // '{default: 0} with no explicit elements: must produce exactly target_len elements.
    // Regression test for off-by-one where remaining was target_len - (x.len()-1).
    fn test_array_literal_default_only(sim) {
        @setup { let code = r#"
module Top (
o0: output logic<8>,
o1: output logic<8>,
o2: output logic<8>,
o3: output logic<8>,
) {
var a: logic<8> [4];
always_comb {
a = '{default: 8'hBB};
}
assign o0 = a[0];
assign o1 = a[1];
assign o2 = a[2];
assign o3 = a[3];
}
"#; }
        @build Simulator::builder(code, "Top");
    let o0 = sim.signal("o0");
    let o1 = sim.signal("o1");
    let o2 = sim.signal("o2");
    let o3 = sim.signal("o3");

    sim.modify(|_| {}).unwrap();

    assert_eq!(sim.get(o0), 0xBBu8.into());
    assert_eq!(sim.get(o1), 0xBBu8.into());
    assert_eq!(sim.get(o2), 0xBBu8.into());
    assert_eq!(sim.get(o3), 0xBBu8.into());

    }

    // Two explicit elements + default: remaining slots filled correctly.
    fn test_array_literal_two_explicit_plus_default(sim) {
        @setup { let code = r#"
module Top (
o0: output logic<8>,
o1: output logic<8>,
o2: output logic<8>,
o3: output logic<8>,
) {
var a: logic<8> [4];
always_comb {
a = '{8'h11, 8'h22, default: 8'hFF};
}
assign o0 = a[0];
assign o1 = a[1];
assign o2 = a[2];
assign o3 = a[3];
}
"#; }
        @build Simulator::builder(code, "Top");
    let o0 = sim.signal("o0");
    let o1 = sim.signal("o1");
    let o2 = sim.signal("o2");
    let o3 = sim.signal("o3");

    sim.modify(|_| {}).unwrap();

    assert_eq!(sim.get(o0), 0x11u8.into());
    assert_eq!(sim.get(o1), 0x22u8.into());
    assert_eq!(sim.get(o2), 0xFFu8.into());
    assert_eq!(sim.get(o3), 0xFFu8.into());

    }

    // repeat + default: '{val repeat 2, default: 0} in a size-4 array.
    fn test_array_literal_repeat_plus_default(sim) {
        @setup { let code = r#"
module Top (
o0: output logic<8>,
o1: output logic<8>,
o2: output logic<8>,
o3: output logic<8>,
) {
var a: logic<8> [4];
always_comb {
a = '{8'hCC repeat 2, default: 8'hDD};
}
assign o0 = a[0];
assign o1 = a[1];
assign o2 = a[2];
assign o3 = a[3];
}
"#; }
        @build Simulator::builder(code, "Top");
    let o0 = sim.signal("o0");
    let o1 = sim.signal("o1");
    let o2 = sim.signal("o2");
    let o3 = sim.signal("o3");

    sim.modify(|_| {}).unwrap();

    assert_eq!(sim.get(o0), 0xCCu8.into());
    assert_eq!(sim.get(o1), 0xCCu8.into());
    assert_eq!(sim.get(o2), 0xDDu8.into());
    assert_eq!(sim.get(o3), 0xDDu8.into());

    }

    // '{default: 0} in always_ff if_reset: array reset via default fill.
    fn test_array_literal_default_in_ff_reset(sim) {
        @ignore_on(veryl);
        @setup { let code = r#"
module Top (
clk: input clock,
rst: input reset,
o0: output logic<8>,
o1: output logic<8>,
o2: output logic<8>,
) {
var arr: logic<8> [3];
assign o0 = arr[0];
assign o1 = arr[1];
assign o2 = arr[2];
always_ff (clk, rst) {
if_reset {
arr = '{default: 0};
} else {
arr[0] += 1;
arr[1] += 2;
arr[2] += 3;
}
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let o0 = sim.signal("o0");
    let o1 = sim.signal("o1");
    let o2 = sim.signal("o2");

    // Reset
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o0), 0u8.into(), "arr[0] should be 0 after reset");
    assert_eq!(sim.get(o1), 0u8.into(), "arr[1] should be 0 after reset");
    assert_eq!(sim.get(o2), 0u8.into(), "arr[2] should be 0 after reset");

    // One tick
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o0), 1u8.into());
    assert_eq!(sim.get(o1), 2u8.into());
    assert_eq!(sim.get(o2), 3u8.into());

    }
}
