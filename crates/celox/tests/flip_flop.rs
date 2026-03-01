use insta::assert_snapshot;
use celox::{BigUint, Simulation, Simulator, SimulatorBuilder};

fn setup_and_trace(code: &str, top: &str) -> celox::CompilationTrace {
    let result = SimulatorBuilder::new(code, top)
        .optimize(true)
        .trace_sim_modules()
        .trace_post_optimized_sir()
        .build_with_trace();

    result.trace
}

#[test]
fn test_ff_nonblocking() {
    let code = r#"
        module Top (clk: input clock, a: input logic<32>, q: output logic<32>) {
            var r: logic<32>;
            always_ff (clk) {
                r = a;
                q = r;
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let a = sim.signal("a");
    let q = sim.signal("q");

    sim.modify(|io| io.set(a, 0x11111111u32)).unwrap();
    sim.tick(clk).unwrap();
    // After 1st tick: r = 0x11111111, q = 0x0
    assert_eq!(sim.get(q), 0x0u32.into());

    sim.tick(clk).unwrap();
    // After 2nd tick: q = 0x11111111
    assert_eq!(sim.get(q), 0x11111111u32.into());
}

#[test]
fn test_ff_if_reset_basic() {
    let code = r#"
        module Top (clk: input clock, rst: input reset, d: input logic<8>, q: output logic<8>) {
            always_ff (clk, rst) {
                if_reset {
                    q = 0;
                } else {
                    q = d;
                }
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d = sim.signal("d");
    let q = sim.signal("q");

    // Reset (AsyncLow: active when rst=0)
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(d, 0xAAu8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x0u32.into());

    // Normal operation (deactivate reset)
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0xAAu32.into());
}

#[test]
fn test_single_clock_optimization() {
    let code = r#"
        module Top (clk: input clock, d: input logic<8>, q: output logic<8>) {
            always_ff (clk) { q = d; }
        }
    "#;
    let trace = setup_and_trace(code, "Top");
    let program = trace.post_optimized_sir.unwrap();
    assert!(program.eval_only_ffs.is_empty());
    assert!(program.apply_ffs.is_empty());
}

#[test]
fn test_multi_clock_no_optimization() {
    let code = r#"
        module Top (clk1: input clock, clk2: input clock, d1: input logic<8>, q1: output logic<8>) {
            always_ff (clk1) { q1 = d1; }
            always_ff (clk2) { }
        }
    "#;
    let trace = setup_and_trace(code, "Top");
    let program = trace.post_optimized_sir.unwrap();
    assert!(!program.eval_only_ffs.is_empty());
    assert!(!program.apply_ffs.is_empty());
}

#[test]
fn test_async_reset() {
    let code = r#"
        module Top (clk: input clock, rst: input reset_async_high, d: input logic<8>, q: output logic<8>) {
            always_ff (clk, rst) {
                if_reset {
                    q = 8'h55;
                } else {
                    q = d;
                }
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let rst_event = sim.event("rst");
    let rst_port = sim.signal("rst");
    let d = sim.signal("d");
    let q = sim.signal("q");

    // Async reset trigger
    sim.modify(|io| io.set(rst_port, 1u8)).unwrap();
    sim.tick(rst_event).unwrap();
    assert_eq!(sim.get(q), 0x55u32.into());

    // Stay reset even if d changes
    sim.modify(|io| io.set(d, 0xFFu8)).unwrap();
    assert_eq!(sim.get(q), 0x55u32.into());

    // Release reset (should stay 0x55 because no clock or active reset edge)
    sim.modify(|io| io.set(rst_port, 0u8)).unwrap();
    assert_eq!(sim.get(q), 0x55u32.into());
}

#[test]
fn test_ff_swap_correctness() {
    let code = r#"
        module Top (clk: input clock, rst: input reset, a: output logic<8>, b: output logic<8>) {
            var r1: logic<8>;
            var r2: logic<8>;
            always_ff (clk, rst) {
                if_reset {
                    r1 = 8'hAA;
                    r2 = 8'h55;
                } else {
                    r1 = r2;
                    r2 = r1;
                }
            }
            assign a = r1;
            assign b = r2;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let a = sim.signal("a");
    let b = sim.signal("b");

    // Reset to initialize (AsyncLow: active when rst=0)
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(a), 0xAAu32.into());
    assert_eq!(sim.get(b), 0x55u32.into());

    // Tick to swap (deactivate reset)
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();

    assert_eq!(sim.get(a), 0x55u32.into());
    assert_eq!(sim.get(b), 0xAAu32.into());
}

#[test]
fn test_multiple_clocks() {
    let code = r#"
        module Top (clk1: input clock, clk2: input clock, d1: input logic<8>, d2: input logic<8>, q1: output logic<8>, q2: output logic<8>) {
            always_ff (clk1) { q1 = d1; }
            always_ff (clk2) { q2 = d2; }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk1 = sim.event("clk1");
    let clk2 = sim.event("clk2");
    let d1 = sim.signal("d1");
    let d2 = sim.signal("d2");
    let q1 = sim.signal("q1");
    let q2 = sim.signal("q2");

    sim.modify(|io| {
        io.set(d1, 0x11u8);
        io.set(d2, 0x22u8);
    })
    .unwrap();

    sim.tick(clk1).unwrap();
    assert_eq!(sim.get(q1), 0x11u32.into());
    assert_eq!(sim.get(q2), 0x0u32.into());

    sim.tick(clk2).unwrap();
    assert_eq!(sim.get(q2), 0x22u32.into());
}

#[test]
fn test_internal_generated_clock() {
    let code = r#"
        module Top (
            clk: input clock,
            d:   input logic<8>,
            q:   output logic<8>
        ) {
            var clk_div: logic;

            // Clock divider (toggle every clk rising edge, half frequency)
            always_ff (clk) {
                clk_div = ~clk_div;
            }

            // Downstream FF driven by the internally generated clock
            // It should trigger when clk_div transitions from 0 to 1
            always_ff (clk_div) {
                q = d;
            }
        }
    "#;
    let mut simulation = Simulation::builder(code, "Top")
        .build()
        .unwrap();

    let d = simulation.signal("d");
    let q = simulation.signal("q");

    // Set input data
    simulation.modify(|io| io.set(d, 0xAAu8)).unwrap();

    // 10-tick period. Edges at:
    // t=0 (0->1)  => clk_div changes 0->1 (rising edge for downstream FF)
    // t=5 (1->0)  => clk_div stays 1
    // t=10 (0->1) => clk_div changes 1->0
    simulation.add_clock("clk", 10, 0);

    // Run until t=5, which includes the first rising edge of clk at t=0.
    // The clk_div should become 1, triggering the downstream FF to capture 'd' (0xAA).
    simulation.run_until(5).unwrap();

    assert_eq!(
        simulation.get(q),
        0xAAu32.into(),
        "Downstream FF should have captured 0xAA when clk_div rose"
    );
}

#[test]
fn test_hierarchical_clocks() {
    let code = r#"
        module Sub (clk: input clock, d: input logic<8>, q: output logic<8>) {
            always_ff (clk) { q = d; }
        }
        module Top (clk: input clock, d: input logic<8>, q: output logic<8>) {
            inst s: Sub (clk, d, q);
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| io.set(d, 0xFEu8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0xFEu32.into());
}

#[test]
fn test_multiple_async_resets() {
    let code = r#"
        module Top (clk: input clock, rst1: input reset_async_high, rst2: input reset_async_high, d: input logic<8>, q: output logic<8>) {
            var r1: logic<8>;
            var r2: logic<8>;

            always_ff (clk, rst1) {
                if_reset {
                    r1 = 8'h0A;
                } else {
                    r1 = d;
                }
            }
            always_ff (clk, rst2) {
                if_reset {
                    r2 = 8'h0B;
                } else {
                    r2 = d;
                }
            }
            assign q = r1 | r2; // dummy use
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let rst1_event = sim.event("rst1");
    let rst1_port = sim.signal("rst1");
    let rst2_event = sim.event("rst2");
    let rst2_port = sim.signal("rst2");
    let r1 = sim.signal("r1");
    let r2 = sim.signal("r2");

    sim.modify(|io| io.set(rst2_port, 1u8)).unwrap();
    sim.tick(rst2_event).unwrap();
    assert_eq!(sim.get(r2), 0x0Bu32.into());

    sim.modify(|io| io.set(rst1_port, 1u8)).unwrap();
    sim.tick(rst1_event).unwrap();
    assert_eq!(sim.get(r1), 0x0Au32.into());
}

#[test]
fn test_ff_if_reset_multi_cycle() {
    let code = r#"
        module Top (clk: input clock, rst: input reset, q: output logic<8>) {
            always_ff (clk, rst) {
                if_reset {
                    q = 0;
                } else {
                    q = q + 1;
                }
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let q = sim.signal("q");

    // Deactivate reset first (AsyncLow: rst=1 means inactive)
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u32.into());
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 2u32.into());

    // Activate reset (AsyncLow: rst=0 means active)
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u32.into());
}

#[test]
fn test_ff_if_reset_with_nested_if() {
    let code = r#"
        module Top (clk: input clock, rst: input reset, en: input logic, q: output logic<8>) {
            always_ff (clk, rst) {
                if_reset {
                    q = 0;
                } else {
                    if en {
                        q = q + 1;
                    }
                }
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let en = sim.signal("en");
    let q = sim.signal("q");

    // Deactivate reset (AsyncLow: rst=1 means inactive)
    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(en, 1u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u32.into());

    sim.modify(|io| io.set(en, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u32.into());
}

#[test]
fn test_ff_struct_constructor_expression() {
    let code = r#"
        module Top (clk: input clock, in_a: input logic<8>, in_b: input logic<8>, out_a: output logic<8>, out_b: output logic<8>) {
            struct S {
                a: logic<8>,
                b: logic<8>,
            }
            var r: S;
            always_ff (clk) {
                r.a = in_a;
                r.b = in_b;
            }
            assign out_a = r.a;
            assign out_b = r.b;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let in_b = sim.signal("in_b");
    let out_a = sim.signal("out_a");
    let out_b = sim.signal("out_b");

    sim.modify(|io| {
        io.set(in_a, 0x12u8);
        io.set(in_b, 0x34u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_a), 0x12u32.into());
    assert_eq!(sim.get(out_b), 0x34u32.into());
}

#[test]
fn test_ff_array_literal_default_expression() {
    let code = r#"
        module Top (clk: input clock, in_data: input logic<8>, out_data: output logic<8>[4]) {
            var r: logic<8>[4];
            always_ff (clk) {
                r = '{default: in_data};
            }
            assign out_data = r;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let in_data = sim.signal("in_data");
    let out_data = sim.signal("out_data");

    sim.modify(|io| io.set(in_data, 0x55u8)).unwrap();
    sim.tick(clk).unwrap();
    let q_val = sim.get(out_data);
    for i in 0..4 {
        let bit_val = (q_val.clone() >> (i * 8)) & BigUint::from(0xFFu32);
        assert_eq!(bit_val, 0x55u32.into());
    }
}

#[test]
fn test_ff_array_literal_nested_default_multidim_expression() {
    let code = r#"
        module Top (
            clk: input clock, 
            in_data: input logic<8>, 
            o00: output logic<8>,
            o01: output logic<8>,
            o10: output logic<8>,
            o11: output logic<8>
        ) {
            var r: logic<8> [2, 2];
            always_ff (clk) {
                r = '{default: '{default: in_data}};
            }
            assign o00 = r[0][0];
            assign o01 = r[0][1];
            assign o10 = r[1][0];
            assign o11 = r[1][1];
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let in_data = sim.signal("in_data");
    let o00 = sim.signal("o00");

    sim.modify(|io| io.set(in_data, 0xAAu8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o00), 0xAAu32.into());
}

#[test]
fn test_ff_function_call_expression() {
    let code = r#"
        module Top (clk: input clock, in_a: input logic<8>, out_q: output logic<8>) {
            function f (x: input logic<8>) -> logic<8> {
                return x + 1;
            }
            always_ff (clk) {
                out_q = f(in_a);
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(in_a, 10u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 11u32.into());
}

#[test]
fn test_ff_function_call_statement_with_output_argument() {
    let code = r#"
        module Top (clk: input clock, in_a: input logic<8>, out_q: output logic<8>) {
            function f (x: input logic<8>, y: output logic<8>) {
                y = x + 2;
            }
            var tmp: logic<8>;
            always_ff (clk) {
                f(in_a, tmp);
                out_q = tmp;
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(in_a, 10u8)).unwrap();
    sim.tick(clk).unwrap();
    // 1st tick: tmp becomes (10+2)=12, out_q reads OLD tmp (0)
    assert_eq!(sim.get(out_q), 0u32.into());
    sim.tick(clk).unwrap();
    // 2nd tick: out_q reads 12
    assert_eq!(sim.get(out_q), 12u32.into());
}

#[test]
fn test_ff_function_call_statement_with_output_argument_and_return_value() {
    let code = r#"
        module Top (clk: input clock, in_a: input logic<8>, out_q1: output logic<8>, out_q2: output logic<8>) {
            function f (x: input logic<8>, y: output logic<8>) -> logic<8> {
                y = x + 3;
                return x + 4;
            }
            var tmp: logic<8>;
            always_ff (clk) {
                out_q1 = f(in_a, tmp);
                out_q2 = tmp;
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q1 = sim.signal("out_q1");
    let out_q2 = sim.signal("out_q2");

    sim.modify(|io| io.set(in_a, 100u8)).unwrap();
    sim.tick(clk).unwrap();
    // After 1st tick: out_q1=104, tmp=103, out_q2=0 (old tmp)
    assert_eq!(sim.get(out_q1), 104u32.into());
    assert_eq!(sim.get(out_q2), 0u32.into());
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q2), 103u32.into());
}

#[test]
fn test_ff_function_call_expression_with_output_argument_and_return_value() {
    let code = r#"
        module Top (clk: input clock, in_a: input logic<8>, out_q1: output logic<8>, out_q2: output logic<8>) {
            function f (x: input logic<8>, y: output logic<8>) -> logic<8> {
                y = x + 5;
                return x + 6;
            }
            var tmp: logic<8>;
            always_ff (clk) {
                out_q1 = f(in_a, tmp) + 1;
                out_q2 = tmp + 1;
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q1 = sim.signal("out_q1");
    let out_q2 = sim.signal("out_q2");

    sim.modify(|io| io.set(in_a, 50u8)).unwrap();
    sim.tick(clk).unwrap();
    // 1st tick: out_q1 = (50+6)+1 = 57, out_q2 = 0+1 = 1
    assert_eq!(sim.get(out_q1), 57u32.into());
    assert_eq!(sim.get(out_q2), 1u32.into());
    sim.tick(clk).unwrap();
    // 2nd tick: out_q2 = (50+5)+1 = 56
    assert_eq!(sim.get(out_q2), 56u32.into());
}

#[test]
fn test_ff_function_call_expression_with_if() {
    let code = r#"
        module Top (clk: input clock, in_a: input logic<8>, sel: input logic, out_q: output logic<8>) {
            function f (x: input logic<8>) -> logic<8> {
                return x + 1;
            }
            always_ff (clk) {
                if sel {
                    out_q = f(in_a);
                } else {
                    out_q = 0;
                }
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let sel = sim.signal("sel");
    let out_q = sim.signal("out_q");

    sim.modify(|io| {
        io.set(in_a, 20u8);
        io.set(sel, 1u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 21u32.into());

    sim.modify(|io| io.set(sel, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0u32.into());
}

#[test]
fn test_ff_nested_function_call_expression() {
    let code = r#"
        module Top (clk: input clock, in_a: input logic<8>, out_q: output logic<8>) {
            function f (x: input logic<8>) -> logic<8> {
                return x + 1;
            }
            function g (x: input logic<8>) -> logic<8> {
                return f(x) * 2;
            }
            always_ff (clk) {
                out_q = g(in_a);
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(in_a, 5u8)).unwrap();
    sim.tick(clk).unwrap();
    // (5+1)*2 = 12
    assert_eq!(sim.get(out_q), 12u32.into());
}

#[test]
fn test_ff_function_call_multistatement_body() {
    let code = r#"
        module Top (clk: input clock, in_a: input logic<8>, out_q: output logic<8>) {
            function f (x: input logic<8>) -> logic<8> {
                var tmp: logic<8>;
                tmp = x + 1;
                tmp = tmp * 2;
                return tmp;
            }
            always_ff (clk) {
                out_q = f(in_a);
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(in_a, 3u8)).unwrap();
    sim.tick(clk).unwrap();
    // (3+1)*2 = 8
    assert_eq!(sim.get(out_q), 8u32.into());
}

#[test]
fn test_ff_function_call_indexed_argument_access() {
    let code = r#"
        module Top (clk: input clock, in_a: input logic<8>[4], out_q: output logic<8>) {
            function f (x: input logic<8>[4]) -> logic<8> {
                return x[2];
            }
            always_ff (clk) {
                out_q = f(in_a);
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| {
        let mut val = BigUint::from(0u32);
        val |= BigUint::from(0xBEu32) << 16;
        io.set_wide(in_a, val);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0xBEu32.into());
}

#[test]
fn test_ff_function_call_nested_output_statement_in_function_body() {
    let code = r#"
        module Top (clk: input clock, in_a: input logic<8>, out_q: output logic<8>) {
            function f (x: input logic<8>, y: output logic<8>) {
                y = x + 1;
            }
            function g (x: input logic<8>, y: output logic<8>) {
                f(x, y);
            }
            var tmp: logic<8>;
            always_ff (clk) {
                g(in_a, tmp);
                out_q = tmp;
            }
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let in_a = sim.signal("in_a");
    let out_q = sim.signal("out_q");

    sim.modify(|io| io.set(in_a, 7u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 0u32.into());
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(out_q), 8u32.into());
}

#[test]
fn test_store_coalescing_sir() {
    let trace = setup_and_trace(
        r#"
        module ModuleA (clk: input clock,a: input logic<8>,b: input logic<8>,c: input logic<8>,d: input logic<8>){
            var mem: logic<8> [4];

            always_ff {
                mem[0] = a;
                mem[1] = b;
                mem[2] = c;
                mem[3] = d;
            }
        }
"#,
        "ModuleA",
    );
    let output = trace.format_program().unwrap();
    assert_snapshot!("store_coalescing_sir", output);
}

#[test]
fn test_rle_sir() {
    let trace = setup_and_trace(
        r#"
module ModuleA (
    clk: input clock,
    x: input logic<32>
) {
    var a: logic<32>;
    var b: logic<32>;
    var c: logic<32>;
    var d: logic<32>;

    always_ff (clk) {
        // Simple RLE
        a = x;
        b = x; 
        
        // Nonblocking semantics in always_ff:
        // d = c reads OLD stable c (not the just-assigned c = x),
        // so this should remain a load from stable c.
        c = x;
        d = c;
    }
}
"#,
        "ModuleA",
    );
    let output = trace.format_program().unwrap();
    assert_snapshot!("rle_sir", output);
}

#[test]
fn test_ff_dynamic_store_sir() {
    let code = r#"
    module Top (
        clk: input clock,
        i: input logic<2>,
        val: input logic<8>
    ) {
        var a: logic<8> [4];
        always_ff (clk) {
            // Dynamic write in FF should generate Store with SIROffset::Dynamic (offset=rX)
            a[i] = val;
        }
    }
"#;
    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();
    assert_snapshot!("ff_dynamic_store_sir", output);
}

#[test]
fn test_commit_sinking_multi_store_sir() {
    let code = r#"
    module Top (
        clk: input clock,
        rst: input reset,
        a: output logic<8>,
        b: output logic<8>
    ) {
        always_ff (clk, rst) {
            if_reset {
                a = 0;
                b = 0;
            } else {
                a = 1;
                b = 2;
            }
        }
    }
"#;

    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();

    assert_snapshot!("commit_sinking_multi_store_sir", output);
}

#[test]
fn test_ff_common_load_hoisting_sir() {
    let code = r#"
    module Top (
        clk: input clock,
        rst: input reset,
        d: input logic<8>,
        a: output logic<8>,
        b: output logic<8>
    ) {
        always_ff (clk, rst) {
            if_reset {
                a = d;
            } else {
                b = d;
            }
        }
    }
"#;

    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();

    assert_snapshot!("ff_common_load_hoisting_sir", output);
}

#[test]
fn test_ff_function_call_multistatement_hoisting_compile() {
    let code = r#"
    module Top (
        clk: input clock,
        d  : input logic<8>,
        q  : output logic<8>,
    ) {
        function f (
            x: input logic<8>,
        ) -> logic<8> {
            if x == 8'd0 {
                return x + 8'd1;
            }
            return x + 8'd2;
        }

        always_ff {
            q = f(d);
        }
    }
"#;

    let trace = setup_and_trace(code, "Top");

    let output = trace.format_program().unwrap();
    assert_snapshot!("ff_function_call_multistatement_hoisting_sir", output);
}

#[test]
fn test_async_reset_sir_snapshot() {
    let code = r#"
module Top (
    clk: input clock,
    rst: input reset_async_high,
    d: input logic<8>,
    q: output logic<8>,
) {
    always_ff (clk, rst) {
        if_reset {
            q = 0;
        } else {
            q = d;
        }
    }
}
"#;

    let trace = setup_and_trace(code, "Top");
    let sir_output = trace.format_program().unwrap();
    insta::assert_snapshot!("async_reset_sir", sir_output);
}

#[test]
fn test_benchmark_loop_sir() {
    let code = r#"
    module Top #(
        param N: u32 = 10,
    )(
        clk: input clock,
        rst: input reset,
        cnt: output logic<32>[N],
    ) {
        for i in 0..N: g {
            always_ff (clk, rst) {
                if_reset {
                    cnt[i] = 0;
                } else {
                    cnt[i] += 1;
                }
            }
        }
    }
    "#;
    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();
    assert_snapshot!("benchmark_loop_sir", output);
}



