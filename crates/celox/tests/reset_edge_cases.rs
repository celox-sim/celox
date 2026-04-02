use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // Test `reset_async_high`: the FF resets when the reset signal is HIGH.
    fn test_reset_async_high(sim) {
        @ignore_on(veryl);
        @setup { let code = r#"
module Top (
clk: input  clock,
rst: input  reset_async_high,
d:   input  logic<8>,
q:   output logic<8>
) {
var r: logic<8>;
always_ff (clk, rst) {
if_reset {
r = 8'd0;
} else {
r = d;
}
}
assign q = r;
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d = sim.signal("d");
    let q = sim.signal("q");

    // Assert reset (high-active)
    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(d, 0xAAu8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into()); // held in reset

    // Release reset, capture data
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0xAAu8.into());

    // Change data
    sim.modify(|io| io.set(d, 0x55u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x55u8.into());

    // Re-assert reset mid-operation
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into());

    }

    // Test `reset_sync_high`: synchronous reset, active HIGH.
    fn test_reset_sync_high(sim) {
        @ignore_on(veryl);
        @setup { let code = r#"
module Top (
clk: input  clock,
rst: input  reset_sync_high,
d:   input  logic<8>,
q:   output logic<8>
) {
var r: logic<8>;
always_ff (clk, rst) {
if_reset {
r = 8'd0;
} else {
r = d;
}
}
assign q = r;
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d = sim.signal("d");
    let q = sim.signal("q");

    // Assert sync reset
    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(d, 0xBBu8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into()); // reset takes effect synchronously

    // Release reset, data should be captured on next clock
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0xBBu8.into());

    }

    // Test `reset_sync_low`: synchronous reset, active LOW.
    fn test_reset_sync_low(sim) {
        @ignore_on(veryl);
        @setup { let code = r#"
module Top (
clk: input  clock,
rst: input  reset_sync_low,
d:   input  logic<8>,
q:   output logic<8>
) {
var r: logic<8>;
always_ff (clk, rst) {
if_reset {
r = 8'd0;
} else {
r = d;
}
}
assign q = r;
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d = sim.signal("d");
    let q = sim.signal("q");

    // Assert sync reset (active-low: rst=0 means reset)
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(d, 0xCCu8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into()); // in reset

    // Release reset (rst=1)
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0xCCu8.into());

    }

    // Test multiple FF blocks sharing the same reset signal.
    fn test_shared_reset(sim) {
        @ignore_on(veryl);
        @setup { let code = r#"
module Top (
clk: input  clock,
rst: input  reset,
d1:  input  logic<8>,
d2:  input  logic<8>,
q1:  output logic<8>,
q2:  output logic<8>
) {
var r1: logic<8>;
var r2: logic<8>;
always_ff (clk, rst) {
if_reset {
r1 = 8'h00;
} else {
r1 = d1;
}
}
always_ff (clk, rst) {
if_reset {
r2 = 8'hFF;
} else {
r2 = d2;
}
}
assign q1 = r1;
assign q2 = r2;
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d1 = sim.signal("d1");
    let d2 = sim.signal("d2");
    let q1 = sim.signal("q1");
    let q2 = sim.signal("q2");

    // Reset both FFs (AsyncLow: rst=0 means active)
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(d1, 0xAAu8);
        io.set(d2, 0xBBu8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q1), 0x00u8.into());
    assert_eq!(sim.get(q2), 0xFFu8.into()); // different reset value

    // Release reset
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q1), 0xAAu8.into());
    assert_eq!(sim.get(q2), 0xBBu8.into());

    }

    // Reset value that is non-zero: verifies the reset assignment uses
    // the specified value, not just zero.
    fn test_nonzero_reset_value(sim) {
        @ignore_on(veryl);
        @setup { let code = r#"
module Top (
clk: input  clock,
rst: input  reset,
d:   input  logic<8>,
q:   output logic<8>
) {
var r: logic<8>;
always_ff (clk, rst) {
if_reset {
r = 8'hDE;
} else {
r = d;
}
}
assign q = r;
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(d, 0x00u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0xDEu8.into()); // reset to 0xDE

    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x00u8.into()); // now captures d=0

    }
}
