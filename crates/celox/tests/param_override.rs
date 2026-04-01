use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {

// Basic param override: change WIDTH from default 8 to 16 and verify signal width.
fn test_param_override_basic_width(sim) {
    @setup { let code = r#"
        module Top #(
            param WIDTH: u32 = 8,
        )(
            a: input  logic<WIDTH>,
            b: output logic<WIDTH>,
        ) {
            assign b = a;
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");
    sim.modify(|io| io.set(a, 0xABu8)).unwrap();
    assert_eq!(sim.get(b), 0xABu8.into());
}

// Param value reflected in logic (assign b = a + OFFSET).
fn test_param_override_logic_reflection(sim) {
    @setup { let code = r#"
        module Top #(
            param OFFSET: u32 = 10,
        )(
            a: input  logic<32>,
            b: output logic<32>,
        ) {
            assign b = a + OFFSET;
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");
    sim.modify(|io| io.set(a, 5u32)).unwrap();
    assert_eq!(sim.get(b), 15u32.into());
}

// No override -> default value is used.
fn test_param_override_default_value(sim) {
    @setup { let code = r#"
        module Top #(
            param INIT: u32 = 42,
        )(
            o: output logic<32>,
        ) {
            assign o = INIT;
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let o = sim.signal("o");
    assert_eq!(sim.get(o), 42u32.into());
}

// Multiple params overridden simultaneously.
fn test_param_override_multiple(sim) {
    @setup { let code = r#"
        module Top #(
            param A: u32 = 1,
            param B: u32 = 2,
        )(
            o: output logic<32>,
        ) {
            assign o = A + B;
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let o = sim.signal("o");
    assert_eq!(sim.get(o), 3u32.into());
}

// Param expression `N - 1` inside always_ff must evaluate correctly.
fn test_param_in_always_ff(sim) {
    @setup { let code = r#"
        module Top #(
            param N: u32 = 8,
        )(
            clk:   input  '_ clock,
            rst:   input  '_ reset,
            start: input  logic,
            done:  output logic,
        ) {
            var counter: logic<32>;
            var running: logic;

            always_ff (clk, rst) {
                if_reset {
                    counter = 0;
                    running = 0;
                } else {
                    if start && !(|running) {
                        counter = 0;
                        running = 1;
                    } else if (|running) {
                        if counter == N - 1 {
                            running = 0;
                            counter = 0;
                        } else {
                            counter = counter + 1;
                        }
                    }
                }
            }

            assign done = !(|running);
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.signal("clk");
    let rst = sim.signal("rst");
    let start = sim.signal("start");
    let done = sim.signal("done");
    let counter = sim.signal("counter");

    // Reset
    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(clk, 0u8);
        io.set(start, 0u8);
    })
    .unwrap();
    sim.modify(|io| io.set(clk, 1u8)).unwrap();
    sim.modify(|io| io.set(rst, 0u8)).unwrap();

    // Start
    sim.modify(|io| io.set(start, 1u8)).unwrap();
    sim.modify(|io| io.set(clk, 0u8)).unwrap();
    sim.modify(|io| io.set(clk, 1u8)).unwrap();
    sim.modify(|io| io.set(start, 0u8)).unwrap();

    // Clock N-1 = 7 more times — counter should reach 7 and stop
    for i in 0..7 {
        sim.modify(|io| io.set(clk, 0u8)).unwrap();
        sim.modify(|io| io.set(clk, 1u8)).unwrap();
        let c: u64 = sim.get(counter).try_into().unwrap();
        eprintln!("cycle {i}: counter={c}, done={:?}", sim.get(done));
    }

    // After 8 total rising edges (1 start + 7 more), counter should have hit N-1=7
    // and running should be 0 -> done should be 1
    let done_val: u64 = sim.get(done).try_into().unwrap();
    assert_eq!(done_val, 1, "done should be 1 after N=8 cycles");
}

// Param expression in always_ff of a child instance with overridden params.
fn test_param_in_child_always_ff(sim) {
    @setup { let code = r#"
        module Counter #(
            param N: u32 = 1024,
        )(
            clk:   input  '_ clock,
            rst:   input  '_ reset,
            start: input  logic,
            done:  output logic,
        ) {
            var counter: logic<32>;
            var running: logic;

            always_ff (clk, rst) {
                if_reset {
                    counter = 0;
                    running = 0;
                } else {
                    if start && !(|running) {
                        counter = 0;
                        running = 1;
                    } else if (|running) {
                        if counter == N - 1 {
                            running = 0;
                            counter = 0;
                        } else {
                            counter = counter + 1;
                        }
                    }
                }
            }

            assign done = !(|running);
        }

        module Top (
            clk:   input  '_ clock,
            rst:   input  '_ reset,
            start: input  logic,
            done:  output logic,
        ) {
            inst u_counter: Counter #(N: 4) (
                clk:   clk,
                rst:   rst,
                start: start,
                done:  done,
            );
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.signal("clk");
    let rst = sim.signal("rst");
    let start = sim.signal("start");
    let done = sim.signal("done");

    // Reset
    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(clk, 0u8);
        io.set(start, 0u8);
    })
    .unwrap();
    sim.modify(|io| io.set(clk, 1u8)).unwrap();
    sim.modify(|io| io.set(rst, 0u8)).unwrap();

    // Start
    sim.modify(|io| io.set(start, 1u8)).unwrap();
    sim.modify(|io| io.set(clk, 0u8)).unwrap();
    sim.modify(|io| io.set(clk, 1u8)).unwrap();
    sim.modify(|io| io.set(start, 0u8)).unwrap();

    // Clock N-1 = 3 more times
    for _ in 0..3 {
        sim.modify(|io| io.set(clk, 0u8)).unwrap();
        sim.modify(|io| io.set(clk, 1u8)).unwrap();
    }

    let done_val: u64 = sim.get(done).try_into().unwrap();
    assert_eq!(
        done_val, 1,
        "done should be 1 after N=4 cycles (child override)"
    );
}

// Const-folded if condition using param expression in always_ff.
fn test_param_const_fold_if(sim) {
    @setup { let code = r#"
        module Top #(
            param MODE: u32 = 1,
        )(
            clk: input clock,
            rst: input reset,
            a: input logic<8>,
            b: input logic<8>,
            o: output logic<8>,
        ) {
            always_ff (clk, rst) {
                if_reset {
                    o = 0;
                } else {
                    if MODE == 1 {
                        o = a;
                    } else {
                        o = b;
                    }
                }
            }
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(a, 0xAAu8);
        io.set(b, 0xBBu8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o), 0xAAu32.into(), "MODE=1 should select a");
}

// Param propagation to a child module via inst param override.
fn test_param_override_child_propagation(sim) {
    @setup { let code = r#"
        module Child #(
            param WIDTH: u32 = 8,
        )(
            i_data: input  logic<WIDTH>,
            o_data: output logic<WIDTH>,
        ) {
            assign o_data = i_data;
        }

        module Top #(
            param WIDTH: u32 = 8,
        )(
            a: input  logic<WIDTH>,
            b: output logic<WIDTH>,
        ) {
            inst u_child: Child #(WIDTH: WIDTH) (
                i_data: a,
                o_data: b,
            );
        }
    "#; }
    @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");
    sim.modify(|io| io.set(a, 0xABu8)).unwrap();
    assert_eq!(sim.get(b), 0xABu8.into());
}

}

// Tests that use .param() override - these need specific builder config
// and cannot use all_backends! @build directly since param() returns
// different types. The tests above cover the default param behavior.
// The param override tests below need to remain as regular #[test].

#[test]
fn test_param_override_basic_width_override() {
    let code = r#"
        module Top #(
            param WIDTH: u32 = 8,
        )(
            a: input  logic<WIDTH>,
            b: output logic<WIDTH>,
        ) {
            assign b = a;
        }
    "#;

    // Override WIDTH=16 → signals are 16-bit, can hold larger values.
    let mut sim = Simulator::builder(code, "Top")
        .param("WIDTH", 16)
        .build()
        .unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    sim.modify(|io| io.set(a, 0xABCDu16)).unwrap();
    assert_eq!(sim.get(b), 0xABCDu16.into());
}

#[test]
fn test_param_override_logic_reflection_override() {
    let code = r#"
        module Top #(
            param OFFSET: u32 = 10,
        )(
            a: input  logic<32>,
            b: output logic<32>,
        ) {
            assign b = a + OFFSET;
        }
    "#;

    // Override OFFSET=100
    let mut sim = Simulator::builder(code, "Top")
        .param("OFFSET", 100)
        .build()
        .unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    sim.modify(|io| io.set(a, 5u32)).unwrap();
    assert_eq!(sim.get(b), 105u32.into());
}

#[test]
fn test_param_override_multiple_override() {
    let code = r#"
        module Top #(
            param A: u32 = 1,
            param B: u32 = 2,
        )(
            o: output logic<32>,
        ) {
            assign o = A + B;
        }
    "#;

    // Override both: 10 + 20 = 30
    let mut sim = Simulator::builder(code, "Top")
        .param("A", 10)
        .param("B", 20)
        .build()
        .unwrap();
    let o = sim.signal("o");
    assert_eq!(sim.get(o), 30u32.into());
}

#[test]
fn test_param_in_always_ff_override_n4() {
    let code = r#"
        module Top #(
            param N: u32 = 8,
        )(
            clk:   input  '_ clock,
            rst:   input  '_ reset,
            start: input  logic,
            done:  output logic,
        ) {
            var counter: logic<32>;
            var running: logic;

            always_ff (clk, rst) {
                if_reset {
                    counter = 0;
                    running = 0;
                } else {
                    if start && !(|running) {
                        counter = 0;
                        running = 1;
                    } else if (|running) {
                        if counter == N - 1 {
                            running = 0;
                            counter = 0;
                        } else {
                            counter = counter + 1;
                        }
                    }
                }
            }

            assign done = !(|running);
        }
    "#;

    // Now test with N overridden to 4
    let mut sim = Simulator::builder(code, "Top")
        .param("N", 4)
        .build()
        .unwrap();
    let clk = sim.signal("clk");
    let rst = sim.signal("rst");
    let start = sim.signal("start");
    let done = sim.signal("done");
    let counter = sim.signal("counter");

    // Reset
    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(clk, 0u8);
        io.set(start, 0u8);
    })
    .unwrap();
    sim.modify(|io| io.set(clk, 1u8)).unwrap();
    sim.modify(|io| io.set(rst, 0u8)).unwrap();

    // Start
    sim.modify(|io| io.set(start, 1u8)).unwrap();
    sim.modify(|io| io.set(clk, 0u8)).unwrap();
    sim.modify(|io| io.set(clk, 1u8)).unwrap();
    sim.modify(|io| io.set(start, 0u8)).unwrap();

    // Clock N-1 = 3 more times
    for i in 0..3 {
        sim.modify(|io| io.set(clk, 0u8)).unwrap();
        sim.modify(|io| io.set(clk, 1u8)).unwrap();
        let c: u64 = sim.get(counter).try_into().unwrap();
        eprintln!("override cycle {i}: counter={c}, done={:?}", sim.get(done));
    }

    let done_val: u64 = sim.get(done).try_into().unwrap();
    assert_eq!(
        done_val, 1,
        "done should be 1 after N=4 cycles with override"
    );
}

#[test]
fn test_param_const_fold_if_mode2() {
    let code = r#"
        module Top #(
            param MODE: u32 = 1,
        )(
            clk: input clock,
            rst: input reset,
            a: input logic<8>,
            b: input logic<8>,
            o: output logic<8>,
        ) {
            always_ff (clk, rst) {
                if_reset {
                    o = 0;
                } else {
                    if MODE == 1 {
                        o = a;
                    } else {
                        o = b;
                    }
                }
            }
        }
    "#;

    // Override MODE=2 → o = b
    let mut sim = Simulator::builder(code, "Top")
        .param("MODE", 2)
        .build()
        .unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(a, 0xAAu8);
        io.set(b, 0xBBu8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o), 0xBBu32.into(), "MODE=2 should select b");
}

#[test]
fn test_param_override_child_propagation_wide() {
    let code = r#"
        module Child #(
            param WIDTH: u32 = 8,
        )(
            i_data: input  logic<WIDTH>,
            o_data: output logic<WIDTH>,
        ) {
            assign o_data = i_data;
        }

        module Top #(
            param WIDTH: u32 = 8,
        )(
            a: input  logic<WIDTH>,
            b: output logic<WIDTH>,
        ) {
            inst u_child: Child #(WIDTH: WIDTH) (
                i_data: a,
                o_data: b,
            );
        }
    "#;

    // Override WIDTH=16 → child also gets 16-bit ports
    let mut sim = Simulator::builder(code, "Top")
        .param("WIDTH", 16)
        .build()
        .unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    sim.modify(|io| io.set(a, 0xABCDu16)).unwrap();
    assert_eq!(sim.get(b), 0xABCDu16.into());
}
