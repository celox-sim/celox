use celox::{BigUint, Simulator, SimulatorBuilder};
use insta::assert_snapshot;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

fn setup_and_trace(code: &str, top: &str) -> celox::CompilationTrace {
    let result = SimulatorBuilder::new(code, top)
        .optimize(true)
        .trace_sim_modules()
        .trace_post_optimized_sir()
        .build_with_trace();

    result.trace
}

all_backends! {

    fn test_dynamic_index_read(sim) {
        @setup { let code = r#"
module Top (i: input logic<2>, o: output logic<8>) {
var a: logic<8> [4];
always_comb{
a[0] = 8'hAA;
a[1] = 8'hBB;
a[2] = 8'hCC;
a[3] = 8'hDD;
}
assign o = a[i];
}
"#; }
        @build Simulator::builder(code, "Top");
    let i = sim.signal("i");
    let o = sim.signal("o");

    sim.modify(|io| io.set(i, 2u8)).unwrap();
    assert_eq!(sim.get(o), 0xCCu64.into());

    }

    fn test_dynamic_index_write(sim) {
        @setup { let code = r#"
module Top (i: input logic<2>, val: input logic<8>, o: output logic<8>) {
var a: logic<8> [4];
always_comb{
a[0] = 1;
a[1] = 2;
a[2] = 3;
a[3] = 4;
a[i] = val;
}
assign o = a[2];
}
"#; }
        @build Simulator::builder(code, "Top");
    let i = sim.signal("i");
    let val = sim.signal("val");
    let o = sim.signal("o");

    sim.modify(|io| {
        io.set(i, 2u8);
        io.set(val, 0x55u8);
    })
    .unwrap();
    assert_eq!(sim.get(o), 0x55u64.into());

    }

    fn test_multidimensional_access(sim) {
        @ignore_on(veryl);
        @setup { let code = r#"
module Top (i: input logic<8>, o: output logic<8>) {
var a: logic<8> [4, 2];
always_comb {
for i_idx: u32 in 0..4 {
for j_idx: u32 in 0..2 {
a[i_idx][j_idx] = 8'b0;
}
}
a[1][0] = i;
}
assign o = a[1][0];
}
"#; }
        @build Simulator::builder(code, "Top");
    let i = sim.signal("i");
    let o = sim.signal("o");

    sim.modify(|io| io.set(i, 0xBEu8)).unwrap();
    assert_eq!(sim.get(o), 0xBEu8.into());

    }

    fn test_minus_colon_and_step_execution(sim) {
        @ignore_on(veryl);
        @setup { let code = r#"
module Top (a: input logic<8>, b: output logic<32>) {
always_comb {
b = 32'b0;
b[31-:8] = a;
b[1 step 8] = a;
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");

    sim.modify(|io| io.set(a, 0xAAu8)).unwrap();
    assert_eq!(sim.get(b), 0xAA00AA00u64.into());

    }

    fn test_dynamic_slice_bullying(sim) {
        @ignore_on(veryl);
        @setup { let code = r#"
module Top (idx: input logic<2>, data: input logic<16>, o: output logic<4>) {
var mem: logic<16>;
assign mem = data;
assign o = mem[idx*4 +: 4];
}
"#; }
        @build Simulator::builder(code, "Top");
    let idx = sim.signal("idx");
    let data = sim.signal("data");
    let o = sim.signal("o");

    sim.modify(|io| {
        io.set(data, 0xABCDu16);
        io.set(idx, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(o), 0xCu64.into());

    }

    fn test_partial_write_merging(sim) {
        @setup { let code = r#"
module Top (a: input logic<4>, b: input logic<4>, o: output logic<8>) {
var tmp: logic<8>;
assign tmp[3:0] = a;
always_comb {
tmp[7:4] = b;
}
assign o = tmp;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    sim.modify(|io| {
        io.set(a, 0x5u8);
        io.set(b, 0xAu8);
    })
    .unwrap();
    assert_eq!(sim.get(o), 0xA5u8.into());

    }

    fn test_genvar_const_in_generate(sim) {
        @ignore_on(veryl);
        @setup { // Genvar used as a simple expression inside a generate block
// should produce per-instance constant values.
let code = r#"
module Top (
o: output logic<8> [4]
) {
for j in 0..4 :g_assign {
assign o[j] = j as u8 + 10;
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let o = sim.signal("o");

    // o is a flat 32-bit value: o[0] in bits [7:0], o[1] in [15:8], etc.
    let val = sim.get(o);
    assert_eq!(val.clone() & BigUint::from(0xFFu32), 10u32.into()); // o[0] = 10
    assert_eq!((val.clone() >> 8u32) & BigUint::from(0xFFu32), 11u32.into()); // o[1] = 11
    assert_eq!(
        (val.clone() >> 16u32) & BigUint::from(0xFFu32),
        12u32.into()
    ); // o[2] = 12
    assert_eq!(
        (val.clone() >> 24u32) & BigUint::from(0xFFu32),
        13u32.into()
    ); // o[3] = 13

    }

    fn test_genvar_dynamic_index_issue21(sim) {
        @ignore_on(veryl);
        @setup { // Issue #21: dynamic indexing of unpacked array using genvar-based
// expression should produce different values per generate instance.
// Each generate instance uses genvar `j` to compute a dynamic index
// into local_data. Without the fix, all instances would read local_data[0].
let code = r#"
module Top (
sel: input logic<2>,
o: output logic<16> [4]
) {
var local_data: logic<16> [4];
always_comb {
local_data[0] = 16'hAAAA;
local_data[1] = 16'hBBBB;
local_data[2] = 16'hCCCC;
local_data[3] = 16'hDDDD;
}
for j in 0..4 :g_idx {
// Dynamic index: genvar j plus runtime sel, truncated to 2 bits
var idx: logic<2>;
assign idx = j as u32 + sel;
assign o[j] = local_data[idx];
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let sel = sim.signal("sel");
    let o = sim.signal("o");

    let mask16 = BigUint::from(0xFFFFu32);

    // sel=0 → src[j]=j → o[j]=local_data[j]
    sim.modify(|io| io.set(sel, 0u8)).unwrap();
    let val = sim.get(o);
    assert_eq!(val.clone() & mask16.clone(), 0xAAAAu32.into()); // o[0]
    assert_eq!((val.clone() >> 16u32) & mask16.clone(), 0xBBBBu32.into()); // o[1]
    assert_eq!((val.clone() >> 32u32) & mask16.clone(), 0xCCCCu32.into()); // o[2]
    assert_eq!((val.clone() >> 48u32) & mask16.clone(), 0xDDDDu32.into()); // o[3]

    // sel=1 → src[j]=(j+1)%4 → rotate left by 1
    sim.modify(|io| io.set(sel, 1u8)).unwrap();
    let val = sim.get(o);
    assert_eq!(val.clone() & mask16.clone(), 0xBBBBu32.into()); // o[0] = local_data[1]
    assert_eq!((val.clone() >> 16u32) & mask16.clone(), 0xCCCCu32.into()); // o[1] = local_data[2]
    assert_eq!((val.clone() >> 32u32) & mask16.clone(), 0xDDDDu32.into()); // o[2] = local_data[3]
    assert_eq!((val.clone() >> 48u32) & mask16.clone(), 0xAAAAu32.into()); // o[3] = local_data[0]

    }

    fn test_dynamic_index_with_bitslice(sim) {
        @ignore_on(veryl);
        @setup { // Regression: arr[dynamic_idx][hi:lo] produced wrong values because
// the bit-select anchor was incorrectly added to the dynamic offset.
let code = r#"
module Top (
idx: input logic<4>,
o_lo: output logic<32>,
o_hi: output logic<32>
) {
var regs: logic<64> [16];
always_comb {
for i: u32 in 0..16 {
regs[i] = 64'b0;
}
// regs[0] = 0x00000001_00000000
regs[0] = 64'h00000001_00000000;
// regs[1] = 0x0000000B_00000001
regs[1] = 64'h0000000B_00000001;
}
assign o_lo = regs[idx][31:0];
assign o_hi = regs[idx][63:32];
}
"#; }
        @build Simulator::builder(code, "Top");
    let idx = sim.signal("idx");
    let o_lo = sim.signal("o_lo");
    let o_hi = sim.signal("o_hi");

    // idx=0: regs[0] = 0x00000001_00000000
    sim.modify(|io| io.set(idx, 0u8)).unwrap();
    assert_eq!(sim.get(o_lo), 0x00000000u64.into()); // lo 32 bits
    assert_eq!(sim.get(o_hi), 0x00000001u64.into()); // hi 32 bits

    // idx=1: regs[1] = 0x0000000B_00000001
    sim.modify(|io| io.set(idx, 1u8)).unwrap();
    assert_eq!(sim.get(o_lo), 0x00000001u64.into()); // lo 32 bits
    assert_eq!(sim.get(o_hi), 0x0000000Bu64.into()); // hi 32 bits

    }

    fn test_let_index_with_bitslice_write(sim) {
        @ignore_on(veryl);
        @setup { // Regression: using a `let`-bound variable as an array index combined
// with a bitslice write (e.g. data[idx][63:32]) produced wrong values
// because eval_dynamic_assign treated the Colon MSB anchor as a
// dimension index.
let code = r#"
module Top (
o_lo: output logic<32>,
o_hi: output logic<32>
) {
var data: logic<64> [4];
always_comb {
for i: u32 in 0..4 {
data[i] = 64'd0;
}
for g: u32 in 0..2 {
for s: u32 in 0..2 {
let idx: u32 = g * 2 + s;
data[idx][63:32] = (g * 2 + s) as u32;
data[idx][31:0]  = (g * 2 + s + 100) as u32;
}
}
o_hi = data[2][63:32];
o_lo = data[2][31:0];
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let o_hi = sim.signal("o_hi");
    let o_lo = sim.signal("o_lo");

    assert_eq!(sim.get(o_hi), 2u64.into());
    assert_eq!(sim.get(o_lo), 102u64.into());

    }

    fn test_let_index_with_bitslice_write_single(sim) {
        @setup { // Minimal: single let index + bitslice write
let code = r#"
module Top (
o_lo: output logic<16>,
o_hi: output logic<16>
) {
var data: logic<32> [2];
always_comb {
data[0] = 32'd0;
data[1] = 32'd0;
let idx: u32 = 1;
data[idx][31:16] = 16'hBEEF;
data[idx][15:0]  = 16'hCAFE;
o_hi = data[1][31:16];
o_lo = data[1][15:0];
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let o_hi = sim.signal("o_hi");
    let o_lo = sim.signal("o_lo");

    assert_eq!(sim.get(o_hi), 0xBEEFu64.into());
    assert_eq!(sim.get(o_lo), 0xCAFEu64.into());

    }

    fn test_ff_bit_select_in_generate_loop(sim) {
        @setup { // Regression: bit-select inside always_ff in a generate loop (e.g. din[i][15])
// produced wrong values because emit_offset_calc computed strides only from
// array dimensions, causing bit indices to be multiplied by the array stride
// instead of 1.
let code = r#"
module Top (
clk: input clock,
rst: input reset,
d0: input logic<16>,
d1: input logic<16>,
en: input logic,
out_a0: output logic<16>,
out_a1: output logic<16>,
out_b0: output logic<16>,
out_b1: output logic<16>,
) {
var din: logic<16> [2];
always_comb {
din[0] = d0;
din[1] = d1;
}
// Path A: comb abs → ff register (workaround)
var abs_c: logic<16> [2];
for i in 0..2 :g_abs_c {
always_comb {
if din[i][15] {
abs_c[i] = ~din[i] + 1;
} else {
abs_c[i] = din[i];
}
}
}
always_ff (clk, rst) {
if_reset {
out_a0 = 0;
out_a1 = 0;
} else if en {
out_a0 = abs_c[0];
out_a1 = abs_c[1];
}
}
// Path B: inline bit-select in always_ff
var out_b: logic<16> [2];
for i in 0..2 :g_reg_b {
always_ff (clk, rst) {
if_reset {
out_b[i] = 0;
} else if en {
if din[i][15] {
out_b[i] = ~din[i] + 1;
} else {
out_b[i] = din[i];
}
}
}
}
always_comb {
out_b0 = out_b[0];
out_b1 = out_b[1];
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d0 = sim.signal("d0");
    let d1 = sim.signal("d1");
    let en = sim.signal("en");
    let out_a0 = sim.signal("out_a0");
    let out_a1 = sim.signal("out_a1");
    let out_b0 = sim.signal("out_b0");
    let out_b1 = sim.signal("out_b1");

    // Reset (AsyncLow: active when rst=0)
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(en, 0u8);
        io.set(d0, 0u16);
        io.set(d1, 0u16);
    })
    .unwrap();
    sim.tick(clk).unwrap();

    // Deactivate reset
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // Set d0 = 5 (positive), d1 = 0xFFFB (-5 as i16)
    sim.modify(|io| {
        io.set(en, 1u8);
        io.set(d0, 5u16);
        io.set(d1, 0xFFFBu16); // -5 in two's complement
    })
    .unwrap();
    sim.tick(clk).unwrap();

    // Path A and Path B should produce identical results
    assert_eq!(
        sim.get(out_a0),
        sim.get(out_b0),
        "d0=5: Path A vs B mismatch"
    );
    assert_eq!(
        sim.get(out_a1),
        sim.get(out_b1),
        "d1=-5: Path A vs B mismatch"
    );
    assert_eq!(sim.get(out_a0), 5u64.into(), "abs(5) should be 5");
    assert_eq!(sim.get(out_a1), 5u64.into(), "abs(-5) should be 5");
    assert_eq!(sim.get(out_b0), 5u64.into(), "abs(5) should be 5 (ff path)");
    assert_eq!(
        sim.get(out_b1),
        5u64.into(),
        "abs(-5) should be 5 (ff path)"
    );

    }
}

#[test]
fn test_stride_access() {
    // This test demonstrates a "false loop" where stride access is not correctly identified.
    // v[i*2] and v[i*2+1] should be disjoint, but because they share the same bounding box (0..31),
    // they are currently treated as overlapping, causing a combinational loop.
    let code = r#"
    module Top (
        i: input logic<4>,
        in_data: input logic<32>,
        o: output logic<32>,
    ) {
        var v: logic<32>;
        always_comb{
            v = in_data;
            v[i*2] = v[i*2+1];
        }
        assign o = v;
    }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .build()
        .expect("Should build successfully");
    let _builder = SimulatorBuilder::new(code, "Top").false_loop(
        (vec![], vec!["v".to_owned()]),
        (vec![], vec!["v".to_owned()]),
    );
    let i_port = sim.signal("i");
    let in_port = sim.signal("in_data");
    let o_port = sim.signal("o");

    // --- Verification ---
    // Input data: 0b...10101010 (odd bits are 1, even bits are 0)
    let test_data = 0xAAAAAAAAu32;

    sim.modify(|io| {
        io.set(in_port, test_data);
        io.set(i_port, 0u8); // i = 0
    })
    .unwrap();
    // When i=0: v[0] = v[1] is executed.
    // v[1] is 1, so v[0] should also become 1.
    // Result: 0xAAAA_AAAA -> 0xAAAA_AAB
    let result = sim.get(o_port);
    assert_eq!(result, 0xAAAA_AAABu32.into()); //   left: 2863311530
    // right: 2863311531

    sim.modify(|io| {
        io.set(i_port, 1u8); // i = 1
    })
    .unwrap();

    // When i=1: v[2] = v[3] is executed.
    // v[3] is 1, so v[2] should also become 1.
    // Result: 0xAAAA_AAAA -> 0xAAAA_AAAE
    let result = sim.get(o_port);
    assert_eq!(result, 0xAAAA_AAAEu32.into());
}

#[test]
fn test_dynamic_index_write_sir() {
    let code = r#"
    module Top (i: input logic<2>, val: input logic<8>, o: output logic<8>) {
        var a: logic<8> [4];
        always_comb{
            a[0] = 1;
            a[1] = 2;
            a[2] = 3;
            a[3] = 4;
            a[i] = val;
        }
        assign o = a[2];
    }
"#;
    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();
    assert_snapshot!("dynamic_index_write_sir", output);
}

#[test]
fn test_dynamic_offset_sir() {
    let code = r#"
    module Top (
        i: input logic<2>,
        o: output logic<8>
    ) {
        var a: logic<8> [4];
        always_comb {
            a[0] = 8'hAA;
            a[1] = 8'hBB;
            a[2] = 8'hCC;
            a[3] = 8'hDD;
        }
        // Dynamic read: This should generate Load with SIROffset::Dynamic (register)
        assign o = a[i];
    }
"#;
    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();
    assert_snapshot!("dynamic_offset_sir", output);
}
