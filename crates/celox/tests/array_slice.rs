use celox::Simulator;
use num_bigint::BigUint;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // A runtime unpacked-array slice reads each element from a runtime
    // coordinate. Elements outside the array read X without shifting their
    // in-range neighbors.
    fn test_runtime_array_slice_reads(sim) {
        // The SystemVerilog frontend does not accept the emitted array slices.
        @ignore_on(sv);
        @setup { let code = r#"
module Top (
sel  : input  logic<2>,
ssel : input  signed logic<3>,
plus : output logic<16>,
minus: output logic<16>,
stepo: output logic<16>,
neg  : output logic<16>,
row  : output logic<16>,
col0 : output logic<16>,
col1 : output logic<16>,
) {
var data: logic<8>[4];
assign data[0] = 8'h01;
assign data[1] = 8'h12;
assign data[2] = 8'h34;
assign data[3] = 8'h80;
var d: logic<8>[2, 4];
always_comb {
for i in 0..2 {
for j in 0..4 {
d[i][j] = (i * 16 + j) as 8;
}
}
}
var p: logic<8>[2];
assign p = data[sel+:2];
assign plus = {p[1], p[0]};
var m: logic<8>[2];
assign m = data[sel-:2];
assign minus = {m[1], m[0]};
var s: logic<8>[2];
assign s = data[sel step 2];
assign stepo = {s[1], s[0]};
var n: logic<8>[2];
assign n = data[ssel+:2];
assign neg = {n[1], n[0]};
var r: logic<8>[2];
assign r = d[sel][1+:2];
assign row = {r[1], r[0]};
var c0: logic<8>[2];
assign c0 = d[0][sel+:2];
assign col0 = {c0[1], c0[0]};
var c1: logic<8>[2];
assign c1 = d[1][sel+:2];
assign col1 = {c1[1], c1[0]};
}
"#; }
        @build Simulator::builder(code, "Top").four_state(true);
    let sel = sim.signal("sel");
    let ssel = sim.signal("ssel");
    // (value, X mask) per 16-bit output; an X byte reads as (0xff, 0xff).
    let expected: [(&str, [(u32, u32); 4]); 6] = [
        ("plus", [(0x1201, 0), (0x3412, 0), (0x8034, 0), (0xff80, 0xff00)]),
        ("minus", [(0x01ff, 0x00ff), (0x1201, 0), (0x3412, 0), (0x8034, 0)]),
        ("stepo", [(0x1201, 0), (0x8034, 0), (0xffff, 0xffff), (0xffff, 0xffff)]),
        ("row", [(0x0201, 0), (0x1211, 0), (0xffff, 0xffff), (0xffff, 0xffff)]),
        ("col0", [(0x0100, 0), (0x0201, 0), (0x0302, 0), (0xff03, 0xff00)]),
        ("col1", [(0x1110, 0), (0x1211, 0), (0x1312, 0), (0xff13, 0xff00)]),
    ];
    for value in 0..4u8 {
        sim.modify(|io| io.set(sel, value)).unwrap();
        for (name, cases) in &expected {
            let (v, m) = cases[value as usize];
            let signal = sim.signal(name);
            assert_eq!(
                sim.get_four_state(signal),
                (v.into(), m.into()),
                "{name} with sel={value}",
            );
        }
    }
    let row = sim.signal("row");
    for payload in [0b01u8, 0b11] {
        sim.modify(|io| io.set_four_state(sel, payload.into(), 0b10u8.into())).unwrap();
        assert_eq!(
            sim.get_four_state(row),
            (0xffffu32.into(), 0xffffu32.into()),
            "row with an X/Z prefix coordinate",
        );
    }

    let neg = sim.signal("neg");
    for (value, expected) in [
        (0b000u8, (0x1201u32, 0u32)),
        (0b010, (0x8034, 0)),
        (0b011, (0xff80, 0xff00)),
        (0b111, (0x01ff, 0x00ff)),
        (0b110, (0xffff, 0xffff)),
    ] {
        sim.modify(|io| io.set(ssel, value)).unwrap();
        assert_eq!(
            sim.get_four_state(neg),
            (expected.0.into(), expected.1.into()),
            "neg with ssel={value:#05b}",
        );
    }
    }

    fn test_runtime_array_slice_evaluates_each_coordinate_once(sim) {
        @ignore_on(sv);
        @setup { let code = r#"
module Top (
    start: input logic<2>,
    chosen: output logic<16>,
    next_idx: output logic<2>,
    folded: output logic<16>,
) {
    function advance (idx: inout logic<2>) -> logic<2> {
        let previous: logic<2> = idx;
        idx = idx + 1;
        return previous;
    }
    var data: logic<8>[2, 4];
    assign data[0] = '{8'h11, 8'h22, 8'h33, 8'h44};
    assign data[1] = '{8'h55, 8'h66, 8'h77, 8'h88};
    always_comb {
        var idx: logic<2>;
        var elements: logic<8>[2];
        idx = start;
        elements = data[advance(idx)][advance(idx)+:2];
        chosen = {elements[1], elements[0]};
        next_idx = idx;
    }
    always_comb {
        var idx: logic<2>;
        var elements: logic<8>[2];
        idx = 1;
        elements = data[0][idx+:2];
        folded = {elements[1], elements[0]};
    }
}
"#; }
        @build Simulator::builder(code, "Top").four_state(true);
        let start = sim.signal("start");
        sim.modify(|io| io.set(start, 0u8)).unwrap();
        for name in ["chosen", "folded"] {
            let signal = sim.signal(name);
            assert_eq!(sim.get_four_state(signal), (0x3322u32.into(), 0u8.into()), "{name}");
        }
        let next_idx = sim.signal("next_idx");
        assert_eq!(sim.get_four_state(next_idx), (2u8.into(), 0u8.into()));
    }

    fn test_runtime_array_slice_wider_than_a_word(sim) {
        // The SystemVerilog frontend does not accept the emitted array slices.
        @ignore_on(sv);
        @setup { let code = r#"
module Top (
sel: input  logic<3>,
o  : output logic<128>,
) {
var data: logic<32>[8];
always_comb {
for i in 0..8 {
data[i] = (32'h1111_1111 * (i + 1)) as 32;
}
}
var p: logic<32>[4];
assign p = data[sel+:4];
assign o = {p[3], p[2], p[1], p[0]};
}
"#; }
        @build Simulator::builder(code, "Top").four_state(true);
    let sel = sim.signal("sel");
    let o = sim.signal("o");
    let word = |i: u32| BigUint::from(0x1111_1111u32 * (i + 1));
    for value in 0..8u32 {
        sim.modify(|io| io.set(sel, value as u8)).unwrap();
        let mut expected_value = BigUint::from(0u8);
        let mut expected_mask = BigUint::from(0u8);
        for position in 0..4u32 {
            let shift = 32 * position as usize;
            if value + position < 8 {
                expected_value |= word(value + position) << shift;
            } else {
                let x = BigUint::from(0xffff_ffffu32) << shift;
                expected_value |= &x;
                expected_mask |= x;
            }
        }
        assert_eq!(
            sim.get_four_state(o),
            (expected_value, expected_mask),
            "sel={value}",
        );
    }
    }

    fn test_runtime_array_slice_two_state(sim) {
        // The SystemVerilog frontend does not accept the emitted array slices.
        @ignore_on(sv);
        @setup { let code = r#"
module Top (
sel: input  bit<2>,
o  : output bit<16>,
) {
var data: bit<8>[4];
assign data[0] = 8'h01;
assign data[1] = 8'h12;
assign data[2] = 8'h34;
assign data[3] = 8'h80;
var p: bit<8>[2];
assign p = data[sel+:2];
assign o = {p[1], p[0]};
}
"#; }
        @build Simulator::builder(code, "Top");
    let sel = sim.signal("sel");
    let o = sim.signal("o");
    for (value, expected) in [(0u8, 0x1201u16), (1, 0x3412), (2, 0x8034), (3, 0x0080)] {
        sim.modify(|io| io.set(sel, value)).unwrap();
        assert_eq!(sim.get(o), expected.into(), "sel={value}");
    }
    }

    fn test_runtime_array_slice_in_ff_and_instance_input(sim) {
        // The SystemVerilog frontend does not accept the emitted array slices.
        @ignore_on(sv);
        @setup { let code = r#"
module Child (
i_data: input  logic<8>[2],
o_data: output logic<16>
) {
assign o_data = {i_data[1], i_data[0]};
}
module Top (
clk : input  clock,
rst : input  reset,
sel : input  logic<2>,
q   : output logic<16>,
inst_o: output logic<16>,
) {
var data: logic<8>[4];
assign data[0] = 8'h01;
assign data[1] = 8'h12;
assign data[2] = 8'h34;
assign data[3] = 8'b10z0_0000;
var r: logic<8>[2];
always_ff {
if_reset {
r[0] = 0;
r[1] = 0;
} else {
r = data[sel-:2];
}
}
assign q = {r[1], r[0]};
inst child: Child (
i_data: data[sel+:2],
o_data: inst_o,
);
}
"#; }
        @build Simulator::builder(code, "Top").four_state(true);
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let sel = sim.signal("sel");
    let q = sim.signal("q");
    let inst_o = sim.signal("inst_o");
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    for (value, ff, comb) in [
        (0u8, (0x01ffu32, 0x00ffu32), (0x1201u32, 0u32)),
        (2, (0x3412, 0), (0x8034, 0x2000)),
        (3, (0x8034, 0x2000), (0xff80, 0xff20)),
    ] {
        sim.modify(|io| io.set(sel, value)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get_four_state(q), (ff.0.into(), ff.1.into()), "q with sel={value}");
        assert_eq!(
            sim.get_four_state(inst_o),
            (comb.0.into(), comb.1.into()),
            "inst_o with sel={value}",
        );
    }
    for payload in [0b01u8, 0b11] {
        sim.modify(|io| io.set_four_state(sel, payload.into(), 0b10u8.into())).unwrap();
        sim.tick(clk).unwrap();
        for signal in [q, inst_o] {
            assert_eq!(sim.get_four_state(signal), (0xffffu32.into(), 0xffffu32.into()));
        }
    }
    }
}

all_backends! {
    fn static_ff_array_slice_reads_multiple_elements(sim) {
        @ignore_on(sv, veryl);
        @setup { let code = r#"
module Top (
    clk: input clock,
    data: input logic<8>[4],
    q: output logic<8>[2],
) {
    always_ff (clk) {
        q = data[1+:2];
    }
}
"#; }
        @build Simulator::builder(code, "Top");
        let data = sim.signal("data");
        let q = sim.signal("q");
        let clk = sim.event("clk");
        sim.modify(|io| io.set(data, 0x44332211u32)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(q), 0x3322u32.into());
    }
}

#[test]
fn initial_array_slices_read_and_write_multiple_elements() {
    let result = Simulator::builder(
        r#"
#[test(Top)] module Top {
    var a: logic<8>[4];
    var b: logic<8>[2];
    var row: logic<8>[2, 4];
    var start: u8;
    initial {
        a[0] = 8'h11;
        a[1] = 8'h22;
        a[2] = 8'h33;
        a[3] = 8'h44;
        start = 1;
        b = a[1+:2];
        $assert(b[0] == 8'h22);
        $assert(b[1] == 8'h33);
        row[1][1+:2] = '{b[0], b[1]};
        b = row[1][start+:2];
        $assert(b[0] == 8'h22);
        $assert(b[1] == 8'h33);
        a[1+:2] = '{b[0], b[1]};
        $assert(a[0] == 8'h11);
        $assert(a[3] == 8'h44);
        $finish();
    }
}
"#,
        "Top",
    )
    .run_test()
    .unwrap();
    assert_eq!(result, celox::TestResult::Pass);
}

#[test]
fn initial_array_slices_keep_the_overlap_in_each_row() {
    let source = r#"
#[test(Top)] module Top {
    var data: logic<8>[2, 5];
    var upper: logic<8>[2];
    var lower: logic<8>[2];
    var stepped: logic<8>[2];
    var missing_all: logic<8>[2];
    var negative: logic<8>[2];
    var from_unknown: logic<8>[2];
    var idx: u8;
    var signed_idx: i8;
    var unknown_idx: logic<3>;
    initial {
        data[0][0] = 8'h11;
        data[0][1] = 8'h22;
        data[0][2] = 8'h33;
        data[0][4] = 8'h55;
        data[1][0] = 8'h77;
        idx = 4;
        upper = data[0][idx+:2];
        idx = 0;
        lower = data[0][idx-:2];
        idx = 2;
        stepped = data[0][idx step 2];
        signed_idx = -3;
        missing_all = data[0][signed_idx+:2];
        signed_idx = -1;
        negative = data[0][signed_idx+:2];
        unknown_idx = 3'bx01;
        from_unknown = data[0][unknown_idx+:2];
        $finish();
    }
}
"#;
    fn check<B: celox::SimBackend>(mut sim: Simulator<B>, four_state: bool) {
        let program = celox::testbench::compile_initial_testbench(&sim).unwrap();
        assert_eq!(
            celox::testbench::run_compiled_testbench(&mut sim, &program),
            celox::TestResult::Pass
        );
        let expected = [
            ("upper", 0x0055u32, 0xff00u32),
            ("lower", 0x1100, 0x00ff),
            ("stepped", 0x0055, 0xff00),
            ("missing_all", 0, 0xffff),
            ("negative", 0x1100, 0x00ff),
            ("from_unknown", if four_state { 0 } else { 0x3322 }, 0xffff),
        ];
        for (name, bits, missing) in expected {
            let mask = if four_state { missing } else { 0 };
            assert_eq!(
                sim.get_four_state(sim.signal(name)),
                ((bits | mask).into(), mask.into()),
                "{name}, four_state={four_state}"
            );
        }
    }
    for four_state in [false, true] {
        let builder = || Simulator::builder(source, "Top").four_state(four_state);
        check(builder().build_interpreter().unwrap(), four_state);
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        check(builder().build_native().unwrap(), four_state);
        #[cfg(feature = "cranelift")]
        check(builder().build_cranelift().unwrap(), four_state);
        #[cfg(feature = "wasm-runtime")]
        check(builder().build_wasm().unwrap(), four_state);
    }
}

#[test]
fn compiled_array_slice_read_settles_pending_input_writes() {
    let source = r#"
#[test(Top)] module Top {
    var input_value: logic<8>;
    var data: logic<8>[4];
    assign data[0] = 8'h11;
    assign data[1] = input_value;
    assign data[2] = 8'h33;
    assign data[3] = 8'h44;
    var readback: logic<8>[2];
    initial {
        input_value = 8'h22;
        readback = data[0+:2];
        $assert(readback[0] == 8'h11);
        $assert(readback[1] == 8'h22);
        input_value = 8'h55;
        readback = data[0+:2];
        $assert(readback[1] == 8'h55);
        $finish();
    }
}
"#;
    fn check<B: celox::SimBackend>(mut sim: Simulator<B>) {
        let program = celox::testbench::compile_initial_testbench(&sim).unwrap();
        assert_eq!(
            celox::testbench::run_compiled_testbench(&mut sim, &program),
            celox::TestResult::Pass
        );
    }
    let builder = || Simulator::builder(source, "Top");
    check(builder().build_interpreter().unwrap());
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    check(builder().build_native().unwrap());
}
