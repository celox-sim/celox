use celox::{SimBackend, Simulator};

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

const RAM_SRC: &str = include_str!("../../../deps/veryl/crates/std/veryl/src/ram/ram.veryl");
const FIFO_CTRL_SRC: &str =
    include_str!("../../../deps/veryl/crates/std/veryl/src/fifo/fifo_controller.veryl");
const FIFO_SRC: &str = include_str!("../../../deps/veryl/crates/std/veryl/src/fifo/fifo.veryl");

const TOP: &str = r#"
module Top (
    clk        : input  clock,
    rst        : input  reset,
    i_clear    : input  logic,
    i_push     : input  logic,
    i_data     : input  logic<8>,
    i_pop      : input  logic,
    o_data     : output logic<8>,
    o_empty    : output logic,
    o_almost_full: output logic,
    o_full     : output logic,
    o_word_count: output logic<3>,
) {
    inst u_fifo: fifo #(
        WIDTH: 8,
        DEPTH: 4,
    ) (
        i_clk: clk,
        i_rst: rst,
        i_clear,
        o_empty,
        o_almost_full,
        o_full,
        o_word_count,
        i_push,
        i_data,
        i_pop,
        o_data,
    );
}
"#;

fn reset<B: SimBackend>(sim: &mut Simulator<B>) {
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_clear = sim.signal("i_clear");
    let i_push = sim.signal("i_push");
    let i_pop = sim.signal("i_pop");
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_clear, 0u8);
        io.set(i_push, 0u8);
        io.set(i_pop, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    // One more tick to settle flags after reset
    sim.tick(clk).unwrap();
}

all_backends! {

// After reset, FIFO should be empty
fn test_fifo_initial_empty(sim) {
    @ignore_on(wasm);
    @setup { let code = format!("{RAM_SRC}\n{FIFO_CTRL_SRC}\n{FIFO_SRC}\n{TOP}"); }
    @build Simulator::builder(&code, "Top");
    reset(&mut sim);

    let o_empty = sim.signal("o_empty");
    let o_full = sim.signal("o_full");
    let o_word_count = sim.signal("o_word_count");

    assert_eq!(sim.get_as::<u8>(o_empty), 1, "should be empty after reset");
    assert_eq!(
        sim.get_as::<u8>(o_full),
        0,
        "should not be full after reset"
    );
    assert_eq!(sim.get_as::<u8>(o_word_count), 0, "word_count should be 0");
}

// Push one item, verify not empty, pop it back
fn test_fifo_push_pop_single(sim) {
    @ignore_on(wasm);
    @setup { let code = format!("{RAM_SRC}\n{FIFO_CTRL_SRC}\n{FIFO_SRC}\n{TOP}"); }
    @build Simulator::builder(&code, "Top");
    reset(&mut sim);

    let clk = sim.event("clk");
    let i_push = sim.signal("i_push");
    let i_data = sim.signal("i_data");
    let i_pop = sim.signal("i_pop");
    let o_data = sim.signal("o_data");
    let o_empty = sim.signal("o_empty");
    let o_word_count = sim.signal("o_word_count");

    // Push 0xAA
    sim.modify(|io| {
        io.set(i_push, 1u8);
        io.set(i_data, 0xAAu8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(i_push, 0u8)).unwrap();
    sim.tick(clk).unwrap();

    assert_eq!(
        sim.get_as::<u8>(o_empty),
        0,
        "should not be empty after push"
    );
    assert_eq!(sim.get_as::<u8>(o_word_count), 1, "word_count should be 1");

    // DATA_FF_OUT=true: data should already be on output register
    assert_eq!(sim.get_as::<u8>(o_data), 0xAA, "output should be 0xAA");

    // Pop
    sim.modify(|io| io.set(i_pop, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(i_pop, 0u8)).unwrap();
    sim.tick(clk).unwrap();

    assert_eq!(sim.get_as::<u8>(o_empty), 1, "should be empty after pop");
    assert_eq!(sim.get_as::<u8>(o_word_count), 0, "word_count should be 0");
}

// Push until full (DEPTH=4), verify full flag
fn test_fifo_full(sim) {
    @ignore_on(wasm);
    @setup { let code = format!("{RAM_SRC}\n{FIFO_CTRL_SRC}\n{FIFO_SRC}\n{TOP}"); }
    @build Simulator::builder(&code, "Top");
    reset(&mut sim);

    let clk = sim.event("clk");
    let i_push = sim.signal("i_push");
    let i_data = sim.signal("i_data");
    let o_full = sim.signal("o_full");
    let o_word_count = sim.signal("o_word_count");

    // Push 4 items
    for i in 0u8..4 {
        sim.modify(|io| {
            io.set(i_push, 1u8);
            io.set(i_data, i + 1);
        })
        .unwrap();
        sim.tick(clk).unwrap();
    }
    sim.modify(|io| io.set(i_push, 0u8)).unwrap();
    sim.tick(clk).unwrap();

    assert_eq!(sim.get_as::<u8>(o_full), 1, "should be full after 4 pushes");
    assert_eq!(sim.get_as::<u8>(o_word_count), 4);
}

// Push 4 items then pop all, verify FIFO ordering
fn test_fifo_ordering(sim) {
    @ignore_on(wasm);
    @setup { let code = format!("{RAM_SRC}\n{FIFO_CTRL_SRC}\n{FIFO_SRC}\n{TOP}"); }
    @build Simulator::builder(&code, "Top");
    reset(&mut sim);

    let clk = sim.event("clk");
    let i_push = sim.signal("i_push");
    let i_data = sim.signal("i_data");
    let i_pop = sim.signal("i_pop");
    let o_data = sim.signal("o_data");

    // Push 4 items: 0x10, 0x20, 0x30, 0x40
    for val in [0x10u8, 0x20, 0x30, 0x40] {
        sim.modify(|io| {
            io.set(i_push, 1u8);
            io.set(i_data, val);
        })
        .unwrap();
        sim.tick(clk).unwrap();
    }
    sim.modify(|io| io.set(i_push, 0u8)).unwrap();

    // Pop and verify FIFO order
    // First item (0x10) should already be on output (DATA_FF_OUT)
    assert_eq!(sim.get_as::<u8>(o_data), 0x10, "first out should be 0x10");

    sim.modify(|io| io.set(i_pop, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(o_data), 0x20, "second out should be 0x20");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(o_data), 0x30, "third out should be 0x30");

    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(o_data), 0x40, "fourth out should be 0x40");
}

// Clear resets the FIFO to empty
fn test_fifo_clear(sim) {
    @ignore_on(wasm);
    @setup { let code = format!("{RAM_SRC}\n{FIFO_CTRL_SRC}\n{FIFO_SRC}\n{TOP}"); }
    @build Simulator::builder(&code, "Top");
    reset(&mut sim);

    let clk = sim.event("clk");
    let i_push = sim.signal("i_push");
    let i_data = sim.signal("i_data");
    let i_clear = sim.signal("i_clear");
    let o_empty = sim.signal("o_empty");
    let o_word_count = sim.signal("o_word_count");

    // Push 2 items
    for val in [0x11u8, 0x22] {
        sim.modify(|io| {
            io.set(i_push, 1u8);
            io.set(i_data, val);
        })
        .unwrap();
        sim.tick(clk).unwrap();
    }
    sim.modify(|io| io.set(i_push, 0u8)).unwrap();
    sim.tick(clk).unwrap();

    assert_eq!(sim.get_as::<u8>(o_word_count), 2);

    // Clear
    sim.modify(|io| io.set(i_clear, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(i_clear, 0u8)).unwrap();
    sim.tick(clk).unwrap();

    assert_eq!(sim.get_as::<u8>(o_empty), 1, "should be empty after clear");
    assert_eq!(sim.get_as::<u8>(o_word_count), 0);
}

}
