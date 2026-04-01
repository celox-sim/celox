use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // Test: Two always_ff blocks in the same module, same clock.
    // Block 1 updates `count`. Block 2 reads `count` to set `r_empty`.
    // NBA semantics: r_empty should see the OLD count value.
    fn test_same_module_count_and_empty_ff(sim) {
        @ignore_on(wasm);
        @setup { let code = r#"
module Top (
clk  : input  clock    ,
rst  : input  reset    ,
push : input  logic    ,
pop  : input  logic    ,
empty: output logic    ,
) {
var count: logic<8>;
var r_empty: logic;
always_ff (clk, rst) {
if_reset {
count = 0;
} else if push && !pop {
count = count + 1;
} else if pop && !push {
if count != 0 {
count = count - 1;
}
}
}
// Separate always_ff block reads count — should read OLD count (NBA)
always_ff (clk, rst) {
if_reset {
r_empty = 1;
} else {
if count == 0 {
r_empty = 1;
} else {
r_empty = 0;
}
}
}
assign empty = r_empty;
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let push = sim.signal("push");
    let pop = sim.signal("pop");
    let empty = sim.signal("empty");

    // Reset
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(push, 0u8);
        io.set(pop, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(empty), 1u32.into(), "empty=1 after reset");

    // Deactivate reset
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // Push: count goes 0→1
    // r_empty reads OLD count (0), so r_empty stays 1 for one more cycle
    sim.modify(|io| io.set(push, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    // NBA: r_empty saw count=0 (old), so empty should be 1
    assert_eq!(
        sim.get(empty),
        1u32.into(),
        "After push: r_empty should see OLD count=0 → empty=1 (1 cycle delay)"
    );

    // Next tick with push deasserted: r_empty now sees count=1 → empty=0
    sim.modify(|io| io.set(push, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get(empty),
        0u32.into(),
        "After settling: r_empty sees count=1 → empty=0"
    );

    // Pop: count goes 1→0
    // r_empty reads OLD count (1), so r_empty stays 0 for one more cycle
    sim.modify(|io| io.set(pop, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    // NBA: r_empty saw count=1 (old), so empty should be 0
    assert_eq!(
        sim.get(empty),
        0u32.into(),
        "After pop: r_empty should see OLD count=1 → empty=0 (1 cycle delay)"
    );

    // Next tick with pop deasserted: r_empty now sees count=0 → empty=1
    sim.modify(|io| io.set(pop, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get(empty),
        1u32.into(),
        "After settling: r_empty sees count=0 → empty=1"
    );

    }

    // Test: count FF in parent module, r_empty FF in child module.
    // The child reads count through a port. NBA: should see OLD count.
    fn test_cross_module_count_and_empty_ff(sim) {
        @setup { let code = r#"
module EmptyDetector (
clk       : input  clock,
rst       : input  reset,
count_in  : input  logic<8>,
r_empty   : output logic,
) {
always_ff (clk, rst) {
if_reset {
r_empty = 1;
} else {
if count_in == 0 {
r_empty = 1;
} else {
r_empty = 0;
}
}
}
}
module Top (
clk  : input  clock,
rst  : input  reset,
push : input  logic,
pop  : input  logic,
empty: output logic,
) {
var count: logic<8>;
always_ff (clk, rst) {
if_reset {
count = 0;
} else if push && !pop {
count = count + 1;
} else if pop && !push {
if count != 0 {
count = count - 1;
}
}
}
inst det: EmptyDetector (
clk,
rst,
count_in: count,
r_empty: empty,
);
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let push = sim.signal("push");
    let pop = sim.signal("pop");
    let empty = sim.signal("empty");

    // Reset
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(push, 0u8);
        io.set(pop, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(empty), 1u32.into(), "empty=1 after reset");

    // Deactivate reset
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // Push: count 0→1
    // EmptyDetector reads OLD count_in (0) → r_empty stays 1
    sim.modify(|io| io.set(push, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get(empty),
        1u32.into(),
        "Cross-module: r_empty should see OLD count=0 → empty=1 (1 cycle delay)"
    );

    // Settle: r_empty sees count=1 → empty=0
    sim.modify(|io| io.set(push, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(empty), 0u32.into(), "empty=0 after settling");

    // Pop: count 1→0
    // EmptyDetector reads OLD count_in (1) → r_empty stays 0
    sim.modify(|io| io.set(pop, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get(empty),
        0u32.into(),
        "Cross-module: r_empty should see OLD count=1 → empty=0 (1 cycle delay)"
    );

    // Settle: r_empty sees count=0 → empty=1
    sim.modify(|io| io.set(pop, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(empty), 1u32.into(), "empty=1 after settling");

    }

    // Stress test: push N items, pop all, verify empty eventually goes high.
    // This specifically catches the "empty=0 forever" bug.
    fn test_empty_never_stuck_at_zero(sim) {
        @ignore_on(wasm);
        @setup { let code = r#"
module Top (
clk  : input  clock,
rst  : input  reset,
push : input  logic,
pop  : input  logic,
empty: output logic,
) {
var count: logic<8>;
var r_empty: logic;
always_ff (clk, rst) {
if_reset {
count = 0;
} else if push && !pop {
count = count + 1;
} else if pop && !push {
if count != 0 {
count = count - 1;
}
}
}
always_ff (clk, rst) {
if_reset {
r_empty = 1;
} else {
if count == 0 {
r_empty = 1;
} else {
r_empty = 0;
}
}
}
assign empty = r_empty;
}
"#; }
        @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let push = sim.signal("push");
    let pop = sim.signal("pop");
    let empty = sim.signal("empty");
    let count = sim.signal("count");

    // Reset
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(push, 0u8);
        io.set(pop, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();

    // Deactivate reset
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // Push 5 items
    for _ in 0..5 {
        sim.modify(|io| io.set(push, 1u8)).unwrap();
        sim.tick(clk).unwrap();
        sim.modify(|io| io.set(push, 0u8)).unwrap();
        sim.tick(clk).unwrap();
    }
    assert_eq!(sim.get(count), 5u32.into(), "count should be 5");
    assert_eq!(sim.get(empty), 0u32.into(), "not empty with 5 items");

    // Pop all 5 items
    for i in 0..5 {
        sim.modify(|io| io.set(pop, 1u8)).unwrap();
        sim.tick(clk).unwrap();
        sim.modify(|io| io.set(pop, 0u8)).unwrap();
        sim.tick(clk).unwrap();
        let expected_count = 4 - i;
        assert_eq!(
            sim.get(count),
            (expected_count as u32).into(),
            "count should be {} after popping {} items",
            expected_count,
            i + 1
        );
    }

    // After all pops + 1 settle cycle, empty must be 1
    // count is 0, but r_empty has 1-cycle delay (NBA)
    // After the last pop tick, r_empty saw OLD count=1 → r_empty=0
    // Need one more tick for r_empty to see count=0
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.get(empty),
        1u32.into(),
        "BUG: empty should be 1 after all items popped — stuck at 0 forever?"
    );

    }
}
