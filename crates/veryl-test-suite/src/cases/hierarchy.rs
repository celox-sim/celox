use crate::Design;

cases! { Hierarchy, "hierarchy";


    fn test_flattened_instance_port_connection(sim) {
        @setup { let code = r#"
module Sub (
i_data: input  logic<8>,
o_data: output logic<8>
) {
assign o_data = i_data;
}
module Top (
top_in:  input  logic<8>,
top_out: output logic<8>
) {
inst u_sub: Sub (
i_data: top_in,
o_data: top_out
);
}
"#; }
        @build Design::new(code, "Top");
    let top_in = sim.signal("top_in");
    let top_out = sim.signal("top_out");

    sim.modify(|io| io.set(top_in, 0x55u8)).unwrap();
    assert_eq!(sim.get(top_out), 0x55u8.into());

    }



    fn test_instance_unpacked_array_slice_input(sim) {

        @setup { let code = r#"
module Child (
i_data: input  logic<8>[2],
o_data: output logic<16>
) {
assign o_data = {i_data[1], i_data[0]};
}
module Top (
o_data: output logic<16>
) {
var data: logic<8>[4];
assign data[0] = 8'h01;
assign data[1] = 8'h12;
assign data[2] = 8'h34;
assign data[3] = 8'h80;
inst child: Child (
i_data: data[1+:2],
o_data,
);
}
"#; }
        @build Design::new(code, "Top");
    let o_data = sim.signal("o_data");

    assert_eq!(sim.get(o_data), 0x3412u16.into());

    }



    fn test_instance_unpacked_array_slice_output(sim) {

        @setup { let code = r#"
module Child (
o_data: output logic<8>[2]
) {
assign o_data[0] = 8'h12;
assign o_data[1] = 8'h34;
}
module Top (
o_data: output logic<16>
) {
var data: logic<8>[4];
inst child: Child (
o_data: data[1+:2],
);
assign o_data = {data[2], data[1]};
}
"#; }
        @build Design::new(code, "Top");
    let o_data = sim.signal("o_data");

    assert_eq!(sim.get(o_data), 0x3412u16.into());

    }



    fn test_instance_input_function_output_writeback(sim) {

        // veryl-simulator currently evaluates the connection value but does
        // not write the function output actual back to the parent variable.
        @setup { let code = r#"
module Child (
i: input logic,
o: output logic
) {
assign o = i;
}
module Top (
a: input logic,
child_o: output logic,
seen_o: output logic
) {
function write_seen (
x: input logic,
seen: output logic
) -> logic {
seen = x;
return x;
}
var seen: logic;
inst child: Child (
i: write_seen(a, seen),
o: child_o
);
assign seen_o = seen;
}
"#; }
        @build Design::new(code, "Top");
    let a = sim.signal("a");
    let child_o = sim.signal("child_o");
    let seen_o = sim.signal("seen_o");

    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get(child_o), 1u8.into());
    assert_eq!(sim.get(seen_o), 1u8.into());

    }



    fn test_instance_input_function_output_concat_dynamic_writeback(sim) {
        // veryl-simulator currently evaluates the connection value but does
        // not write the function output actual back to the parent variables.

        @setup { let code = r#"
module Child (
i: input logic,
o: output logic
) {
assign o = i;
}
module Top (
value: input logic<2>,
index: input logic,
child_o: output logic,
mem_o: output logic<2>,
tmp_o: output logic
) {
function write_pair (
x: input logic<2>,
dst: output logic<2>
) -> logic {
dst = x;
return x[0];
}
var mem: logic<2>;
var tmp: logic;
inst child: Child (
i: write_pair(value, {mem[index], tmp}),
o: child_o
);
assign mem_o = mem;
assign tmp_o = tmp;
}
"#; }
        @build Design::new(code, "Top");
    let value = sim.signal("value");
    let index = sim.signal("index");
    let child_o = sim.signal("child_o");
    let mem_o = sim.signal("mem_o");
    let tmp_o = sim.signal("tmp_o");

    sim.modify(|io| {
        io.set(value, 3u8);
        io.set(index, 1u8);
    }).unwrap();
    assert_eq!(sim.get(child_o), 1u8.into());
    assert_eq!(sim.get(mem_o), 2u8.into());
    assert_eq!(sim.get(tmp_o), 1u8.into());

    }



fn test_inactive_instance_input_output_call_adds_no_parent_driver(sim) {

        @setup { let code = r#"
module Child (
i: input logic,
o: output logic
) {
assign o = i;
}
module Top (
a: input logic,
child_o: output logic,
seen_o: output logic
) {
function write_seen (
x: input logic,
seen: output logic
) -> logic {
seen = !x;
return x;
}
var seen: logic;
inst child: Child (
i: if 1'b0 ? write_seen(a, seen) : a,
o: child_o
);
assign seen = a;
assign seen_o = seen;
}
"#; }
        @build Design::new(code, "Top");
    let a = sim.signal("a");
    let child_o = sim.signal("child_o");
    let seen_o = sim.signal("seen_o");

    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get(child_o), 1u8.into());
    assert_eq!(sim.get(seen_o), 1u8.into());

    }



    fn test_instance_output_dynamic_index_function_output_writeback(sim) {

        @setup { let code = r#"
module Child (
i: input logic,
o: output logic
) {
assign o = i;
}
module Top (
sel: input logic,
mem_o: output logic<2>,
tmp_o: output logic
) {
function choose_index (
x: input logic,
tmp: output logic
) -> logic {
tmp = x;
return x;
}
var mem: logic<2>;
var tmp: logic;
inst child: Child (
i: 1'b1,
o: mem[choose_index(sel, tmp)]
);
assign mem_o = mem;
assign tmp_o = tmp;
}
"#; }
        // Invalid implicit continuous-assignment destination (IEEE 1800-2023 10.2).
        @expect reject;
        @build Design::new(code, "Top");
    }


fn test_instance_output_dynamic_index_composes_aliasing_writeback(sim) {

        @setup { let code = r#"
module Child (i: input logic, o: output logic) {
assign o = i;
}
module Top (
sel: input logic,
mem_o: output logic<2>
) {
function choose_index (
x: input logic,
tmp: output logic
) -> logic {
tmp = x;
return x;
}
var mem: logic<2>;
inst child: Child (
i: 1'b1,
o: mem[choose_index(sel, mem[0])]
);
assign mem_o = mem;
}
"#; }
        // Invalid implicit continuous-assignment destination (IEEE 1800-2023 10.2).
        @expect reject;
        @build Design::new(code, "Top");
    }


fn test_instance_output_concat_advances_each_destination(sim) {

        @setup { let code = r#"
module Child (i: input logic<2>, o: output logic<2>) {
assign o = i;
}
module Top (
value: input logic<2>,
mem_o: output logic<2>,
tmp_o: output logic
) {
var mem: logic<2>;
var tmp: logic;
inst child: Child (
i: value,
o: {mem[tmp], tmp}
);
assign mem_o = mem;
assign tmp_o = tmp;
}
"#; }
        // Invalid implicit continuous-assignment destination (IEEE 1800-2023 10.2).
        @expect reject;
        @build Design::new(code, "Top");
    }


fn test_unconnected_child_output_needs_no_parent_glue(sim) {
        @setup { let code = r#"
module Child (
i: input logic,
unused: output logic
) {
assign unused = i;
}
module Top (
i: input logic,
o: output logic
) {
inst child: Child (
i: i
);
assign o = i;
}
"#; }
        @build Design::new(code, "Top");
    let i = sim.signal("i");
    let o = sim.signal("o");

    sim.modify(|io| io.set(i, 1u8)).unwrap();
    assert_eq!(sim.get(o), 1u8.into());

    }



    fn test_instance_input_port_assignment_width_context(sim) {
        @setup { let code = r#"
module Child (
widen_u: input  logic<16>,
widen_s: input  signed logic<16>,
sum9:    input  logic<9>,
trunc8:  input  logic<8>,
fill8:   input  logic<8>,
o_u16:   output logic<16>,
o_s16:   output logic<16>,
o_sum9:  output logic<9>,
o_trunc: output logic<8>,
o_fill:  output logic<8>
) {
assign o_u16 = widen_u;
assign o_s16 = widen_s;
assign o_sum9 = sum9;
assign o_trunc = trunc8;
assign o_fill = fill8;
}
module Top (
narrow_u: input  logic<8>,
s8:      input  signed logic<8>,
a:       input  logic<8>,
b:       input  logic<8>,
wide16:  input  logic<16>,
o_u16:   output logic<16>,
o_s16:   output logic<16>,
o_sum9:  output logic<9>,
o_trunc: output logic<8>,
o_fill:  output logic<8>
) {
inst child: Child (
widen_u: narrow_u,
widen_s: s8,
sum9: a + b,
trunc8: wide16,
fill8: '1,
o_u16,
o_s16,
o_sum9,
o_trunc,
o_fill
);
}
"#; }
        @build Design::new(code, "Top");
    let u8_in = sim.signal("narrow_u");
    let s8 = sim.signal("s8");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let wide16 = sim.signal("wide16");
    let o_u16 = sim.signal("o_u16");
    let o_s16 = sim.signal("o_s16");
    let o_sum9 = sim.signal("o_sum9");
    let o_trunc = sim.signal("o_trunc");
    let o_fill = sim.signal("o_fill");

    sim.modify(|io| {
        io.set(u8_in, 0xffu8);
        io.set(s8, 0x80u8);
        io.set(a, 0xffu8);
        io.set(b, 1u8);
        io.set(wide16, 0xab34u16);
    })
    .unwrap();

    assert_eq!(sim.get(o_u16), 0x00ffu16.into());
    assert_eq!(sim.get(o_s16), 0xff80u16.into());
    assert_eq!(sim.get(o_sum9), 0x0100u16.into());
    assert_eq!(sim.get(o_trunc), 0x34u8.into());
    assert_eq!(sim.get(o_fill), 0xffu8.into());

    }



fn test_dynamic_output_port_rmw_preserves_unselected_bits(sim) {

        @setup { let code = r#"
module Child (
a: input logic<2>,
y: output logic<2>
) {
assign y = a;
}
module Top (
idx: input logic<3>,
a: input logic<2>,
out: output logic<8>
) {
var mem: logic<8>;
inst child: Child (
a,
y: mem[idx +: 2]
);
assign out = mem;
}
"#; }
        // Invalid implicit continuous-assignment destination (IEEE 1800-2023 10.2).
        @expect reject;
        @build Design::new(code, "Top");
    }


fn test_dynamic_output_port_converts_four_state_child_to_two_state_parent(sim) {

        @setup { let code = r#"
module Child (y: output logic) {
assign y = 1'bx;
}
module Top (idx: input logic, out: output bit<2>) {
var mem: bit<2>;
inst child: Child (y: mem[idx]);
assign out = mem;
}
"#; }
        // Invalid implicit continuous-assignment destination (IEEE 1800-2023 10.2).
        @expect reject;
        @build Design::new(code, "Top");
    }


fn test_dynamic_minus_colon_output_port_rmw(sim) {

        @setup { let code = r#"
module Child (a: input logic<2>, y: output logic<2>) {
assign y = a;
}
module Top (idx: input logic<3>, a: input logic<2>, out: output logic<8>) {
var mem: logic<8>;
inst child: Child (a, y: mem[idx -: 2]);
assign out = mem;
}
"#; }
        // Invalid implicit continuous-assignment destination (IEEE 1800-2023 10.2).
        @expect reject;
        @build Design::new(code, "Top");
    }


fn test_dynamic_step_output_port_rmw(sim) {

        @setup { let code = r#"
module Child (a: input logic<2>, y: output logic<2>) {
assign y = a;
}
module Top (idx: input logic<2>, a: input logic<2>, out: output logic<8>) {
var mem: logic<8>;
inst child: Child (a, y: mem[idx step 2]);
assign out = mem;
}
"#; }
        // Invalid implicit continuous-assignment destination (IEEE 1800-2023 10.2).
        @expect reject;
        @build Design::new(code, "Top");
    }


fn test_dynamic_prefix_colon_output_port_allows_zero_lsb(sim) {

        @setup { let code = r#"
module Child (a: input logic<8>, y: output logic<8>) {
assign y = a;
}
module Top (idx: input logic, a: input logic<8>, out: output logic<16>) {
var mem: logic<8> [2];
inst child: Child (a, y: mem[idx][7:0]);
assign out = mem;
}
"#; }
        // Invalid implicit continuous-assignment destination (IEEE 1800-2023 10.2).
        @expect reject;
        @build Design::new(code, "Top");
    }


fn test_multiple_instances_isolation(sim) {
        @setup { let code = r#"
module Worker (
clk: input clock,
i_val: input logic<8>,
o_val: output logic<8>
) {
var internal_reg: logic<8>;
always_ff {
internal_reg = i_val + 1;
}
assign o_val = internal_reg;
}
module Top (
clk: input clock,
in0: input logic<8>,
in1: input logic<8>,
out0: output logic<8>,
out1: output logic<8>
) {
inst u0: Worker ( clk: clk, i_val: in0, o_val: out0 );
inst u1: Worker ( clk: clk, i_val: in1, o_val: out1 );
}
"#; }
        @build Design::new(code, "Top");
    let clk = sim.event("clk");
    let in0 = sim.signal("in0");
    let in1 = sim.signal("in1");
    let out0 = sim.signal("out0");
    let out1 = sim.signal("out1");

    sim.modify(|io| {
        io.set(in0, 10u8);
        io.set(in1, 20u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();

    assert_eq!(sim.get(out0), 11u8.into());
    assert_eq!(sim.get(out1), 21u8.into());

    }



    fn test_deep_hierarchical_path_resolution(sim) {
        @setup { let code = r#"
module Leaf ( i: input logic, o: output logic ) {
assign o = ~i;
}
module Mid ( i: input logic, o: output logic ) {
inst u_leaf: Leaf ( i: i, o: o );
}
module Top ( top_i: input logic, top_o: output logic ) {
inst u_mid: Mid ( i: top_i, o: top_o );
}
"#; }
        @build Design::new(code, "Top");
    let top_i = sim.signal("top_i");
    let top_o = sim.signal("top_o");

    sim.modify(|io| io.set(top_i, 1u8)).unwrap();
    assert_eq!(sim.get(top_o), 0u8.into());

    }



    fn test_constant_propagation_across_hierarchy(sim) {
        @setup { let code = r#"
module Sub ( i: input logic<8>, o: output logic<8> ) {
assign o = i + 8'h01;
}
module Top ( o: output logic<8> ) {
inst u_sub: Sub ( i: 8'h0F, o: o );
}
"#; }
        @build Design::new(code, "Top");
    let o = sim.signal("o");

    sim.modify(|_| {}).unwrap();
    assert_eq!(sim.get(o), 0x10u8.into());

    }



    fn test_hierarchical_concat_feedback_runtime(sim) {

        @setup { let code = r#"
module Child (
a: input logic<2>,
lo: output logic,
) {
assign lo = a[1];
}
module Top (
inp: input logic,
out: output logic,
) {
var v: logic<2>;
var lo: logic;
inst c: Child (
a: v,
lo: lo,
);
assign v = {inp, lo};
assign out = v[0];
}
"#; }
        @build Design::new(code, "Top");
    let inp = sim.signal("inp");
    let out = sim.signal("out");

    sim.modify(|io| io.set(inp, 0u8)).unwrap();
    assert_eq!(sim.get(out), 0u8.into());

    sim.modify(|io| io.set(inp, 1u8)).unwrap();
    assert_eq!(sim.get(out), 1u8.into());

    sim.modify(|io| io.set(inp, 0u8)).unwrap();
    assert_eq!(sim.get(out), 0u8.into());

    }



    fn test_hierarchical_concat_feedback_runtime_multi_observe(sim) {

        @setup { let code = r#"
module Child (
a: input logic<2>,
lo: output logic,
) {
assign lo = a[1];
}
module Top (
inp: input logic,
out0: output logic,
out1: output logic,
) {
var v: logic<2>;
var lo: logic;
inst c: Child (
a: v,
lo: lo,
);
assign v = {inp, lo};
assign out0 = v[0];
assign out1 = v[1];
}
"#; }
        @build Design::new(code, "Top");
    let inp = sim.signal("inp");
    let out0 = sim.signal("out0");
    let out1 = sim.signal("out1");

    for bit in [0u8, 1u8, 0u8, 1u8] {
        sim.modify(|io| io.set(inp, bit)).unwrap();
        assert_eq!(sim.get(out0), bit.into());
        assert_eq!(sim.get(out1), bit.into());
    }

    }



    fn test_hierarchical_concat_feedback_with_constant_middle_bit(sim) {

        @setup { let code = r#"
module Child (
a: input logic<3>,
lo: output logic,
) {
assign lo = a[2];
}
module Top (
inp: input logic,
out: output logic,
mid: output logic,
) {
var v: logic<3>;
var lo: logic;
inst c: Child (
a: v,
lo: lo,
);
assign v = {inp, 1'b0, lo};
assign out = v[0];
assign mid = v[1];
}
"#; }
        @build Design::new(code, "Top");
    let inp = sim.signal("inp");
    let out = sim.signal("out");
    let mid = sim.signal("mid");

    sim.modify(|io| io.set(inp, 0u8)).unwrap();
    assert_eq!(sim.get(out), 0u8.into());
    assert_eq!(sim.get(mid), 0u8.into());

    sim.modify(|io| io.set(inp, 1u8)).unwrap();
    assert_eq!(sim.get(out), 1u8.into());
    assert_eq!(sim.get(mid), 0u8.into());

    }



    fn test_hierarchical_dynamic_index_feedback_runtime(sim) {

        @setup { let code = r#"
module ChildFb (
a: input logic<3>,
lo: output logic,
) {
assign lo = a[2];
}
module ChildDyn (
a: input logic<3>,
idx: input logic,
o: output logic,
) {
assign o = a[idx];
}
module Top (
i1: input logic,
i2: input logic,
sel: input logic,
out_fb: output logic,
out_dyn: output logic,
) {
var v: logic<3>;
var lo: logic;
var d: logic;
inst fb: ChildFb (
a: v,
lo: lo,
);
inst dyn: ChildDyn (
a: v,
idx: sel,
o: d,
);
// Instance-crossing feedback (fb) and dynamic index access (dyn).
// lo = v[2], while v is built by split assignments so bit-dependencies are precise.
assign v[2:1] = {i2, i1};
assign v[0] = lo;
assign out_fb = lo;
assign out_dyn = d;
}
"#; }
        @build Design::new(code, "Top");
    let i1 = sim.signal("i1");
    let i2 = sim.signal("i2");
    let sel = sim.signal("sel");
    let out_fb = sim.signal("out_fb");
    let out_dyn = sim.signal("out_dyn");

    sim.modify(|io| {
        io.set(i1, 1u8);
        io.set(i2, 0u8);
        io.set(sel, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(out_fb), 0u8.into());
    assert_eq!(sim.get(out_dyn), 0u8.into());

    sim.modify(|io| io.set(sel, 1u8)).unwrap();
    assert_eq!(sim.get(out_fb), 0u8.into());
    assert_eq!(sim.get(out_dyn), 1u8.into());

    sim.modify(|io| {
        io.set(i1, 0u8);
        io.set(i2, 1u8);
        io.set(sel, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(out_fb), 1u8.into());
    assert_eq!(sim.get(out_dyn), 1u8.into());

    }



    fn test_hierarchical_dual_dynamic_readers_feedback_runtime(sim) {

        @setup { let code = r#"
module ChildFb (
a: input logic<3>,
lo: output logic,
) {
assign lo = a[2];
}
module ChildDyn (
a: input logic<3>,
idx: input logic,
o: output logic,
) {
assign o = a[idx];
}
module Top (
i1: input logic,
i2: input logic,
sel0: input logic,
sel1: input logic,
out_fb: output logic,
out0: output logic,
out1: output logic,
) {
var v: logic<3>;
var lo: logic;
var d0: logic;
var d1: logic;
inst fb: ChildFb (
a: v,
lo: lo,
);
inst dyn0: ChildDyn (
a: v,
idx: sel0,
o: d0,
);
inst dyn1: ChildDyn (
a: v,
idx: sel1,
o: d1,
);
assign v[2:1] = {i2, i1};
assign v[0] = lo;
assign out_fb = lo;
assign out0 = d0;
assign out1 = d1;
}
"#; }
        @build Design::new(code, "Top");
    let i1 = sim.signal("i1");
    let i2 = sim.signal("i2");
    let sel0 = sim.signal("sel0");
    let sel1 = sim.signal("sel1");
    let out_fb = sim.signal("out_fb");
    let out0 = sim.signal("out0");
    let out1 = sim.signal("out1");

    sim.modify(|io| {
        io.set(i1, 1u8);
        io.set(i2, 0u8);
        io.set(sel0, 0u8);
        io.set(sel1, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(out_fb), 0u8.into());
    assert_eq!(sim.get(out0), 0u8.into());
    assert_eq!(sim.get(out1), 1u8.into());

    sim.modify(|io| io.set(sel0, 1u8)).unwrap();
    assert_eq!(sim.get(out_fb), 0u8.into());
    assert_eq!(sim.get(out0), 1u8.into());
    assert_eq!(sim.get(out1), 1u8.into());

    sim.modify(|io| {
        io.set(i2, 1u8);
        io.set(sel1, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(out_fb), 1u8.into());
    assert_eq!(sim.get(out0), 1u8.into());
    assert_eq!(sim.get(out1), 1u8.into());

    sim.modify(|io| {
        io.set(i1, 0u8);
        io.set(sel0, 1u8);
        io.set(sel1, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(out_fb), 1u8.into());
    assert_eq!(sim.get(out0), 0u8.into());
    assert_eq!(sim.get(out1), 0u8.into());

    }



    fn test_hierarchical_overlapping_partial_write_dynamic_index_runtime(sim) {

        @setup { let code = r#"
module ChildFb (
a: input logic<3>,
lo: output logic,
) {
assign lo = a[2];
}
module ChildDyn (
a: input logic<3>,
idx: input logic,
o: output logic,
) {
assign o = a[idx];
}
module Top (
i0: input logic,
i1: input logic,
i2: input logic,
sel: input logic,
out_fb: output logic,
out_dyn: output logic,
out_v0: output logic,
out_v1: output logic,
) {
var v: logic<3>;
var lo: logic;
var d: logic;
inst fb: ChildFb (
a: v,
lo: lo,
);
inst dyn: ChildDyn (
a: v,
idx: sel,
o: d,
);
// Non-overlapping source for feedback input.
assign v[2] = i2;
// Overlapping writes to the same bit: final v[1] must be lo (not i1).
always_comb {
v[1] = i1;
v[1] = lo;
v[0] = i0;
}
assign out_fb = lo;
assign out_dyn = d;
assign out_v0 = v[0];
assign out_v1 = v[1];
}
"#; }
        @build Design::new(code, "Top");
    let i0 = sim.signal("i0");
    let i1 = sim.signal("i1");
    let i2 = sim.signal("i2");
    let sel = sim.signal("sel");
    let out_fb = sim.signal("out_fb");
    let out_dyn = sim.signal("out_dyn");
    let out_v0 = sim.signal("out_v0");
    let out_v1 = sim.signal("out_v1");

    // lo = v[2] = i2 = 0. v[1] must be overridden to lo (0), v[0] = i0 (1).
    sim.modify(|io| {
        io.set(i0, 1u8);
        io.set(i1, 1u8);
        io.set(i2, 0u8);
        io.set(sel, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(out_fb), 0u8.into());
    assert_eq!(sim.get(out_v0), 1u8.into());
    assert_eq!(sim.get(out_v1), 0u8.into());
    assert_eq!(sim.get(out_dyn), 0u8.into());

    // sel=0 reads untouched bit v[0] path.
    sim.modify(|io| io.set(sel, 0u8)).unwrap();
    assert_eq!(sim.get(out_dyn), 1u8.into());

    // Change i2 only: should update lo/v[1], while v[0] keeps i0.
    sim.modify(|io| io.set(i2, 1u8)).unwrap();
    assert_eq!(sim.get(out_fb), 1u8.into());
    assert_eq!(sim.get(out_v1), 1u8.into());
    assert_eq!(sim.get(out_v0), 1u8.into());

    sim.modify(|io| io.set(sel, 1u8)).unwrap();
    assert_eq!(sim.get(out_dyn), 1u8.into());

    // Change i1 only: v[1] must still follow lo (i2), not i1.
    sim.modify(|io| io.set(i1, 0u8)).unwrap();
    assert_eq!(sim.get(out_v1), 1u8.into());
    assert_eq!(sim.get(out_dyn), 1u8.into());

    // Change i0 only: dynamic read on sel=0 should follow v[0] immediately.
    sim.modify(|io| {
        io.set(i0, 0u8);
        io.set(sel, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(out_v0), 0u8.into());
    assert_eq!(sim.get(out_dyn), 0u8.into());

    }



    fn test_hierarchical_concat_then_overlap_dynamic_index_runtime(sim) {

        @setup { let code = r#"
module ChildFb (
a: input logic<3>,
lo: output logic,
) {
assign lo = a[2];
}
module ChildDyn (
a: input logic<3>,
idx: input logic,
o: output logic,
) {
assign o = a[idx];
}
module Top (
i0: input logic,
i1: input logic,
i2: input logic,
sel: input logic,
out_fb: output logic,
out_dyn: output logic,
out_v1: output logic,
) {
var v: logic<3>;
var lo: logic;
var d: logic;
inst fb: ChildFb (
a: v,
lo: lo,
);
inst dyn: ChildDyn (
a: v,
idx: sel,
o: d,
);
// Full concat assignment then overlapping bit override.
// Final v[1] should be lo (== v[2] == i2), not i1.
always_comb {
v = {i2, i1, i0};
v[1] = lo;
}
assign out_fb = lo;
assign out_dyn = d;
assign out_v1 = v[1];
}
"#; }
        @build Design::new(code, "Top");

    let i0 = sim.signal("i0");
    let i1 = sim.signal("i1");
    let i2 = sim.signal("i2");
    let sel = sim.signal("sel");
    let out_fb = sim.signal("out_fb");
    let out_dyn = sim.signal("out_dyn");
    let out_v1 = sim.signal("out_v1");

    sim.modify(|io| {
        io.set(i0, 0u8);
        io.set(i1, 0u8);
        io.set(i2, 1u8);
        io.set(sel, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(out_fb), 1u8.into());
    assert_eq!(sim.get(out_v1), 1u8.into());
    assert_eq!(sim.get(out_dyn), 1u8.into());

    // i1 toggles but must not affect v[1] because v[1] is overridden by lo.
    sim.modify(|io| io.set(i1, 1u8)).unwrap();
    assert_eq!(sim.get(out_v1), 1u8.into());
    assert_eq!(sim.get(out_dyn), 1u8.into());

    // i2 toggles and must propagate to lo/v[1]/dynamic sel=1.
    sim.modify(|io| io.set(i2, 0u8)).unwrap();
    assert_eq!(sim.get(out_fb), 0u8.into());
    assert_eq!(sim.get(out_v1), 0u8.into());
    assert_eq!(sim.get(out_dyn), 0u8.into());

    }



    fn test_child_signal_access(sim) {
        @setup { let code = r#"
module Sub (
i_data: input  logic<8>,
o_data: output logic<8>
) {
assign o_data = i_data + 8'h01;
}
module Top (
top_in:  input  logic<8>,
top_out: output logic<8>
) {
inst u_sub: Sub (
i_data: top_in,
o_data: top_out
);
}
"#; }
        @build Design::new(code, "Top");
    let top_in = sim.signal("top_in");

    // Access child instance signal via child_signal()
    let child_i_data = sim.child_signal(&[("u_sub", 0)], "i_data");
    let child_o_data = sim.child_signal(&[("u_sub", 0)], "o_data");

    sim.modify(|io| io.set(top_in, 0x10u8)).unwrap();
    assert_eq!(sim.get(child_i_data), 0x10u8.into());
    assert_eq!(sim.get(child_o_data), 0x11u8.into());

    }
}
