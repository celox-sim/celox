use celox::{Simulator, SimulatorErrorKind};

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

// TODO: restore batch_eval test after SimBackend refactor

all_backends! {

    // Test multiple `modify` calls without `tick` accumulate changes.
    fn test_multiple_modify_no_tick(sim) {
        @setup { let code = r#"
module Top (
a: input  logic<8>,
b: input  logic<8>,
s: output logic<8>
) {
assign s = a + b;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let s = sim.signal("s");

    // First modify sets a
    sim.modify(|io| io.set(a, 10u8)).unwrap();
    // Second modify sets b
    sim.modify(|io| io.set(b, 20u8)).unwrap();

    // Both should be reflected
    assert_eq!(sim.get(s), 30u8.into());

    }

    // Test that `modify` triggers combinational re-evaluation immediately.
    fn test_modify_triggers_comb_reevaluation(sim) {
        @setup { let code = r#"
module Top (
sel: input  logic,
a:   input  logic<8>,
b:   input  logic<8>,
y:   output logic<8>
) {
always_comb {
if sel {
y = a;
} else {
y = b;
}
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let sel = sim.signal("sel");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");

    sim.modify(|io| {
        io.set(a, 0xAAu8);
        io.set(b, 0xBBu8);
        io.set(sel, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(y), 0xBBu8.into());

    // Change only sel, check y re-evaluates
    sim.modify(|io| io.set(sel, 1u8)).unwrap();
    assert_eq!(sim.get(y), 0xAAu8.into());

    }

    // Test `get` on initial state (before any modify/tick).
    fn test_initial_state(sim) {
        @setup { let code = r#"
module Top (
a: input  logic<8>,
b: output logic<8>
) {
assign b = a;
}
"#; }
        @build Simulator::builder(code, "Top");
    let b = sim.signal("b");

    // Initial value should be 0
    assert_eq!(sim.get(b), 0u8.into());

    }

    // Test rapid clock toggling: tick many times and verify counter state.
    fn test_rapid_tick_counter(sim) {
        @ignore_on(veryl);
        @setup { let code = r#"
module Top (
clk: input  clock,
rst: input  reset,
cnt: output logic<16>
) {
var counter: logic<16>;
always_ff (clk, rst) {
if_reset {
counter = 16'd0;
} else {
counter = counter + 16'd1;
}
}
assign cnt = counter;
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let cnt = sim.signal("cnt");

    // Reset (AsyncLow: rst=0 means active)
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();

    // Release reset and tick 100 times
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    for _ in 0..100 {
        sim.tick(clk).unwrap();
    }

    assert_eq!(sim.get(cnt), 100u16.into());

    }

    // Test that `modify` inside a closure can set multiple ports at once.
    fn test_modify_multiple_ports(sim) {
        @setup { let code = r#"
module Top (
a: input  logic<8>,
b: input  logic<8>,
c: input  logic<8>,
sum: output logic<8>
) {
assign sum = a + b + c;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let c = sim.signal("c");
    let sum = sim.signal("sum");

    sim.modify(|io| {
        io.set(a, 10u8);
        io.set(b, 20u8);
        io.set(c, 30u8);
    })
    .unwrap();
    assert_eq!(sim.get(sum), 60u8.into());

    }

    fn test_overlapping_partial_write_keeps_untouched_bit_dependency(sim) {
        @ignore_on(veryl);
        @setup { let code = r#"
module Top (
a: input logic,
b: input logic,
c: input logic,
y: output logic,
x0: output logic,
) {
var x: logic<2>;
var t: logic;
always_comb {
x[1] = a;
x[0] = b;
x[0] = c;
t = x[1];
}
assign y = t;
assign x0 = x[0];
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let c = sim.signal("c");
    let y = sim.signal("y");
    let x0 = sim.signal("x0");

    sim.modify(|io| {
        io.set(a, 0u8);
        io.set(b, 1u8);
        io.set(c, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(y), 0u8.into());
    assert_eq!(sim.get(x0), 0u8.into());

    sim.modify(|io| {
        io.set(a, 1u8);
        io.set(b, 0u8);
        io.set(c, 1u8);
    })
    .unwrap();
    // y follows x[1] == a, x0 follows overridden x[0] == c
    assert_eq!(sim.get(y), 1u8.into());
    assert_eq!(sim.get(x0), 1u8.into());

    }

    fn test_concat_with_dynamic_index_runtime(sim) {
        @setup { let code = r#"
module Top (
a: input logic<4>,
i: input logic<2>,
out: output logic<2>,
) {
var x: logic<2>;
assign x = {a[i], a[0]};
assign out = x;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let i = sim.signal("i");
    let out = sim.signal("out");

    // a = 4'b1010 : a[0]=0, a[1]=1, a[2]=0, a[3]=1
    sim.modify(|io| {
        io.set(a, 0b1010u8);
        io.set(i, 1u8);
    })
    .unwrap();
    // out = {a[1], a[0]} = {1,0} = 2
    assert_eq!(sim.get(out), 0b10u8.into());

    sim.modify(|io| io.set(i, 3u8)).unwrap();
    // out = {a[3], a[0]} = {1,0} = 2
    assert_eq!(sim.get(out), 0b10u8.into());

    sim.modify(|io| {
        io.set(a, 0b0101u8);
        io.set(i, 2u8);
    })
    .unwrap();
    // out = {a[2], a[0]} = {1,1} = 3
    assert_eq!(sim.get(out), 0b11u8.into());

    }
}

/// Test that `try_new` returns Ok for valid designs.
#[test]
fn test_try_new_valid() {
    let code = r#"
        module Top (
            a: input  logic<8>,
            b: output logic<8>
        ) {
            assign b = a;
        }
    "#;
    let result = Simulator::builder(code, "Top").build();
    assert!(result.is_ok());
}

/// Test that `try_new` returns Err for combinational loops.
#[test]
fn test_try_new_comb_loop() {
    let code = r#"
        module Top (
            a: input  logic,
            b: output logic
        ) {
            var x: logic;
            assign x = x ^ a;
            assign b = x;
        }
    "#;
    let result = Simulator::builder(code, "Top").build();
    let err = result.expect_err("Expected error for combinational loop");
    assert!(
        matches!(
            err.kind(),
            SimulatorErrorKind::Analyzer(_) | SimulatorErrorKind::SIRParser(_)
        ),
        "Expected Analyzer or SIRParser error, got: {err:?}"
    );
}
