//! Regression tests: run Veryl designs through both JitBackend and WasmBackend,
//! comparing results to ensure they match.

use celox::{BigUint, SimulatorOptions, WasmBackend};

/// Build a Simulator (JIT) and a WasmBackend from the same Veryl code.
/// Returns (jit_simulator, wasm_backend).
fn build_both(code: &str, top: &str) -> (celox::Simulator, WasmBackend) {
    let sim = celox::Simulator::builder(code, top).build().unwrap();
    let program = sim.program().clone();
    let opts = SimulatorOptions::default();
    let wasm = WasmBackend::new(&program, &opts).expect("WasmBackend::new failed");
    (sim, wasm)
}

// ════════════════════════════════════════════════════════════════
// Combinational tests
// ════════════════════════════════════════════════════════════════

#[test]
fn wasm_reg_simple_assignment() {
    let code = r#"
        module Top (a: input logic<32>, b: output logic<32>) {
            assign b = a;
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let prog = sim.program().clone();
    let wa = wasm.resolve_signal(&prog.get_addr(&[], &["a"]).unwrap());
    let wb = wasm.resolve_signal(&prog.get_addr(&[], &["b"]).unwrap());

    wasm.set(wa, 0xDEADBEEFu32);
    wasm.eval_comb().unwrap();

    sim.modify(|io| io.set(a, 0xDEADBEEFu32)).unwrap();

    assert_eq!(wasm.get(wb), sim.get(b), "simple assignment mismatch");
}

#[test]
fn wasm_reg_bitwise_ops() {
    let code = r#"
        module Top (a: input logic<8>, b: input logic<8>, o_and: output logic<8>, o_or: output logic<8>, o_xor: output logic<8>) {
            always_comb {
                o_and = a & b;
                o_or  = a | b;
                o_xor = a ^ b;
            }
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let ja = sim.signal("a");
    let jb = sim.signal("b");
    let j_and = sim.signal("o_and");
    let j_or = sim.signal("o_or");
    let j_xor = sim.signal("o_xor");
    let prog = sim.program().clone();

    let wa = wasm.resolve_signal(&prog.get_addr(&[], &["a"]).unwrap());
    let wb = wasm.resolve_signal(&prog.get_addr(&[], &["b"]).unwrap());
    let w_and = wasm.resolve_signal(&prog.get_addr(&[], &["o_and"]).unwrap());
    let w_or = wasm.resolve_signal(&prog.get_addr(&[], &["o_or"]).unwrap());
    let w_xor = wasm.resolve_signal(&prog.get_addr(&[], &["o_xor"]).unwrap());

    wasm.set(wa, 0xA5u8);
    wasm.set(wb, 0x5Au8);
    wasm.eval_comb().unwrap();

    sim.modify(|io| {
        io.set(ja, 0xA5u8);
        io.set(jb, 0x5Au8);
    })
    .unwrap();

    assert_eq!(wasm.get(w_and), sim.get(j_and), "AND mismatch");
    assert_eq!(wasm.get(w_or), sim.get(j_or), "OR mismatch");
    assert_eq!(wasm.get(w_xor), sim.get(j_xor), "XOR mismatch");
}

#[test]
fn wasm_reg_arithmetic() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            o_add: output logic<8>,
            o_sub: output logic<8>,
            o_mul: output logic<8>
        ) {
            always_comb {
                o_add = a + b;
                o_sub = a - b;
                o_mul = a * b;
            }
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let ja = sim.signal("a");
    let jb = sim.signal("b");
    let j_add = sim.signal("o_add");
    let j_sub = sim.signal("o_sub");
    let j_mul = sim.signal("o_mul");
    let prog = sim.program().clone();

    let wa = wasm.resolve_signal(&prog.get_addr(&[], &["a"]).unwrap());
    let wb = wasm.resolve_signal(&prog.get_addr(&[], &["b"]).unwrap());
    let w_add = wasm.resolve_signal(&prog.get_addr(&[], &["o_add"]).unwrap());
    let w_sub = wasm.resolve_signal(&prog.get_addr(&[], &["o_sub"]).unwrap());
    let w_mul = wasm.resolve_signal(&prog.get_addr(&[], &["o_mul"]).unwrap());

    for (av, bv) in [(10u8, 20u8), (0xFF, 1), (100, 50), (5, 10)] {
        wasm.set(wa, av);
        wasm.set(wb, bv);
        wasm.eval_comb().unwrap();

        sim.modify(|io| {
            io.set(ja, av);
            io.set(jb, bv);
        })
        .unwrap();

        assert_eq!(
            wasm.get(w_add),
            sim.get(j_add),
            "add mismatch for a={av}, b={bv}"
        );
        assert_eq!(
            wasm.get(w_sub),
            sim.get(j_sub),
            "sub mismatch for a={av}, b={bv}"
        );
        assert_eq!(
            wasm.get(w_mul),
            sim.get(j_mul),
            "mul mismatch for a={av}, b={bv}"
        );
    }
}

#[test]
fn wasm_reg_shifts() {
    let code = r#"
        module Top (a: input logic<8>, o_shr: output logic<8>, o_shl: output logic<8>) {
            always_comb {
                o_shr = a >> 2;
                o_shl = a << 2;
            }
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let ja = sim.signal("a");
    let j_shr = sim.signal("o_shr");
    let j_shl = sim.signal("o_shl");
    let prog = sim.program().clone();

    let wa = wasm.resolve_signal(&prog.get_addr(&[], &["a"]).unwrap());
    let w_shr = wasm.resolve_signal(&prog.get_addr(&[], &["o_shr"]).unwrap());
    let w_shl = wasm.resolve_signal(&prog.get_addr(&[], &["o_shl"]).unwrap());

    wasm.set(wa, 0x80u8);
    wasm.eval_comb().unwrap();

    sim.modify(|io| io.set(ja, 0x80u8)).unwrap();

    assert_eq!(wasm.get(w_shr), sim.get(j_shr), "shr mismatch");
    assert_eq!(wasm.get(w_shl), sim.get(j_shl), "shl mismatch");
}

#[test]
fn wasm_reg_comparisons() {
    let code = r#"
        module Top (a: input logic<8>, b: input logic<8>, o_lt: output logic<1>, o_ge: output logic<1>, o_eq: output logic<1>, o_ne: output logic<1>) {
            always_comb {
                o_lt = a <: b;
                o_ge = a >= b;
                o_eq = a == b;
                o_ne = a != b;
            }
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let ja = sim.signal("a");
    let jb = sim.signal("b");
    let j_lt = sim.signal("o_lt");
    let j_ge = sim.signal("o_ge");
    let j_eq = sim.signal("o_eq");
    let j_ne = sim.signal("o_ne");
    let prog = sim.program().clone();

    let wa = wasm.resolve_signal(&prog.get_addr(&[], &["a"]).unwrap());
    let wb = wasm.resolve_signal(&prog.get_addr(&[], &["b"]).unwrap());
    let w_lt = wasm.resolve_signal(&prog.get_addr(&[], &["o_lt"]).unwrap());
    let w_ge = wasm.resolve_signal(&prog.get_addr(&[], &["o_ge"]).unwrap());
    let w_eq = wasm.resolve_signal(&prog.get_addr(&[], &["o_eq"]).unwrap());
    let w_ne = wasm.resolve_signal(&prog.get_addr(&[], &["o_ne"]).unwrap());

    for (av, bv) in [(10u8, 20u8), (20, 10), (10, 10)] {
        wasm.set(wa, av);
        wasm.set(wb, bv);
        wasm.eval_comb().unwrap();

        sim.modify(|io| {
            io.set(ja, av);
            io.set(jb, bv);
        })
        .unwrap();

        assert_eq!(
            wasm.get(w_lt),
            sim.get(j_lt),
            "lt mismatch for a={av}, b={bv}"
        );
        assert_eq!(
            wasm.get(w_ge),
            sim.get(j_ge),
            "ge mismatch for a={av}, b={bv}"
        );
        assert_eq!(
            wasm.get(w_eq),
            sim.get(j_eq),
            "eq mismatch for a={av}, b={bv}"
        );
        assert_eq!(
            wasm.get(w_ne),
            sim.get(j_ne),
            "ne mismatch for a={av}, b={bv}"
        );
    }
}

#[test]
fn wasm_reg_unary_bitnot() {
    let code = r#"
        module Top (a: input logic<8>, o: output logic<8>) {
            assign o = ~a;
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let ja = sim.signal("a");
    let jo = sim.signal("o");
    let prog = sim.program().clone();

    let wa = wasm.resolve_signal(&prog.get_addr(&[], &["a"]).unwrap());
    let wo = wasm.resolve_signal(&prog.get_addr(&[], &["o"]).unwrap());

    wasm.set(wa, 0x55u8);
    wasm.eval_comb().unwrap();

    sim.modify(|io| io.set(ja, 0x55u8)).unwrap();

    assert_eq!(wasm.get(wo), sim.get(jo), "bitnot mismatch");
}

#[test]
fn wasm_reg_ternary() {
    let code = r#"
        module Top (sel: input logic, a: input logic<8>, b: input logic<8>, o: output logic<8>) {
            assign o = if sel ? a : b ;
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let j_sel = sim.signal("sel");
    let j_a = sim.signal("a");
    let j_b = sim.signal("b");
    let j_o = sim.signal("o");
    let prog = sim.program().clone();

    let w_sel = wasm.resolve_signal(&prog.get_addr(&[], &["sel"]).unwrap());
    let w_a = wasm.resolve_signal(&prog.get_addr(&[], &["a"]).unwrap());
    let w_b = wasm.resolve_signal(&prog.get_addr(&[], &["b"]).unwrap());
    let w_o = wasm.resolve_signal(&prog.get_addr(&[], &["o"]).unwrap());

    wasm.set(w_a, 0xAAu8);
    wasm.set(w_b, 0xBBu8);

    for sel_v in [0u8, 1u8] {
        wasm.set(w_sel, sel_v);
        wasm.eval_comb().unwrap();

        sim.modify(|io| {
            io.set(j_sel, sel_v);
            io.set(j_a, 0xAAu8);
            io.set(j_b, 0xBBu8);
        })
        .unwrap();

        assert_eq!(
            wasm.get(w_o),
            sim.get(j_o),
            "ternary mismatch for sel={sel_v}"
        );
    }
}

#[test]
fn wasm_reg_case_switch() {
    let code = r#"
        module Top (
            sel: input logic<2>,
            o:   output logic<8>
        ) {
            always_comb {
                case sel {
                    2'd0: o = 8'hAA;
                    2'd1: o = 8'hBB;
                    2'd2: o = 8'hCC;
                    default: o = 8'hDD;
                }
            }
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let j_sel = sim.signal("sel");
    let j_o = sim.signal("o");
    let prog = sim.program().clone();

    let w_sel = wasm.resolve_signal(&prog.get_addr(&[], &["sel"]).unwrap());
    let w_o = wasm.resolve_signal(&prog.get_addr(&[], &["o"]).unwrap());

    for sel_v in 0..=3u8 {
        wasm.set(w_sel, sel_v);
        wasm.eval_comb().unwrap();

        sim.modify(|io| io.set(j_sel, sel_v)).unwrap();

        assert_eq!(wasm.get(w_o), sim.get(j_o), "case mismatch for sel={sel_v}");
    }
}

#[test]
fn wasm_reg_always_comb_override() {
    let code = r#"
        module Top (sel: input logic, val: input logic<8>, o: output logic<8>) {
            var tmp: logic<8>;
            always_comb {
                tmp = 8'h11;
                if sel {
                    tmp = val;
                }
                o = tmp;
            }
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let j_sel = sim.signal("sel");
    let j_val = sim.signal("val");
    let j_o = sim.signal("o");
    let prog = sim.program().clone();

    let w_sel = wasm.resolve_signal(&prog.get_addr(&[], &["sel"]).unwrap());
    let w_val = wasm.resolve_signal(&prog.get_addr(&[], &["val"]).unwrap());
    let w_o = wasm.resolve_signal(&prog.get_addr(&[], &["o"]).unwrap());

    // sel=0 -> o=0x11
    wasm.set(w_sel, 0u8);
    wasm.eval_comb().unwrap();
    sim.modify(|io| io.set(j_sel, 0u8)).unwrap();
    assert_eq!(wasm.get(w_o), sim.get(j_o), "override default mismatch");

    // sel=1, val=0xEE -> o=0xEE
    wasm.set(w_sel, 1u8);
    wasm.set(w_val, 0xEEu8);
    wasm.eval_comb().unwrap();
    sim.modify(|io| {
        io.set(j_sel, 1u8);
        io.set(j_val, 0xEEu8);
    })
    .unwrap();
    assert_eq!(wasm.get(w_o), sim.get(j_o), "override val mismatch");
}

#[test]
fn wasm_reg_blocking_assignment_chain() {
    let code = r#"
        module Top (a: input logic<8>, o: output logic<8>) {
            var x: logic<8>;
            always_comb {
                x = a;
                x = x + 8'd1;
                x = x << 1;
                o = x;
            }
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let ja = sim.signal("a");
    let jo = sim.signal("o");
    let prog = sim.program().clone();

    let wa = wasm.resolve_signal(&prog.get_addr(&[], &["a"]).unwrap());
    let wo = wasm.resolve_signal(&prog.get_addr(&[], &["o"]).unwrap());

    wasm.set(wa, 10u8);
    wasm.eval_comb().unwrap();
    sim.modify(|io| io.set(ja, 10u8)).unwrap();

    assert_eq!(wasm.get(wo), sim.get(jo), "blocking chain mismatch");
}

#[test]
fn wasm_reg_concatenation() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            o: output logic<16>
        ) {
            always_comb {
                o = {a, b};
            }
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let ja = sim.signal("a");
    let jb = sim.signal("b");
    let jo = sim.signal("o");
    let prog = sim.program().clone();

    let wa = wasm.resolve_signal(&prog.get_addr(&[], &["a"]).unwrap());
    let wb = wasm.resolve_signal(&prog.get_addr(&[], &["b"]).unwrap());
    let wo = wasm.resolve_signal(&prog.get_addr(&[], &["o"]).unwrap());

    wasm.set(wa, 0x12u8);
    wasm.set(wb, 0x34u8);
    wasm.eval_comb().unwrap();

    sim.modify(|io| {
        io.set(ja, 0x12u8);
        io.set(jb, 0x34u8);
    })
    .unwrap();

    assert_eq!(wasm.get(wo), sim.get(jo), "concat mismatch");
}

#[test]
fn wasm_reg_mixed_concatenation() {
    let code = r#"
        module Top (
            a: input logic<4>,
            b: input logic<4>,
            o: output logic<12>
        ) {
            always_comb {
                o = {a, 4'hF, b};
            }
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let ja = sim.signal("a");
    let jb = sim.signal("b");
    let jo = sim.signal("o");
    let prog = sim.program().clone();

    let wa = wasm.resolve_signal(&prog.get_addr(&[], &["a"]).unwrap());
    let wb = wasm.resolve_signal(&prog.get_addr(&[], &["b"]).unwrap());
    let wo = wasm.resolve_signal(&prog.get_addr(&[], &["o"]).unwrap());

    wasm.set(wa, 0xAu8);
    wasm.set(wb, 0xCu8);
    wasm.eval_comb().unwrap();

    sim.modify(|io| {
        io.set(ja, 0xAu8);
        io.set(jb, 0xCu8);
    })
    .unwrap();

    assert_eq!(wasm.get(wo), sim.get(jo), "mixed concat mismatch");
}

#[test]
fn wasm_reg_reduction_ops() {
    let code = r#"
        module Top (
            a: input logic<4>,
            o_and: output logic,
            o_or:  output logic
        ) {
            assign o_and = &a;
            assign o_or  = |a;
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let ja = sim.signal("a");
    let j_and = sim.signal("o_and");
    let j_or = sim.signal("o_or");
    let prog = sim.program().clone();

    let wa = wasm.resolve_signal(&prog.get_addr(&[], &["a"]).unwrap());
    let w_and = wasm.resolve_signal(&prog.get_addr(&[], &["o_and"]).unwrap());
    let w_or = wasm.resolve_signal(&prog.get_addr(&[], &["o_or"]).unwrap());

    for av in [0u8, 0x0E, 0x0F, 0x05] {
        wasm.set(wa, av);
        wasm.eval_comb().unwrap();
        sim.modify(|io| io.set(ja, av)).unwrap();

        assert_eq!(
            wasm.get(w_and),
            sim.get(j_and),
            "and mismatch for a={av:#x}"
        );
        assert_eq!(wasm.get(w_or), sim.get(j_or), "or mismatch for a={av:#x}");
    }
}

// ════════════════════════════════════════════════════════════════
// Sequential tests (flip-flop)
// ════════════════════════════════════════════════════════════════

#[test]
fn wasm_reg_ff_nonblocking() {
    let code = r#"
        module Top (clk: input clock, a: input logic<32>, q: output logic<32>) {
            var r: logic<32>;
            always_ff (clk) {
                r = a;
                q = r;
            }
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let j_clk = sim.event("clk");
    let ja = sim.signal("a");
    let jq = sim.signal("q");
    let prog = sim.program().clone();

    let clk_addr = prog.get_addr(&[], &["clk"]).unwrap();
    let w_a = wasm.resolve_signal(&prog.get_addr(&[], &["a"]).unwrap());
    let w_q = wasm.resolve_signal(&prog.get_addr(&[], &["q"]).unwrap());
    let w_clk = wasm.resolve_event(&clk_addr);

    // Set a, tick once
    wasm.set(w_a, 0x11111111u32);
    wasm.eval_comb().unwrap();
    wasm.eval_apply_ff_at(&w_clk).unwrap();
    wasm.eval_comb().unwrap();

    sim.modify(|io| io.set(ja, 0x11111111u32)).unwrap();
    sim.tick(j_clk).unwrap();

    assert_eq!(wasm.get(w_q), sim.get(jq), "FF nonblocking tick1 mismatch");

    // Tick again
    wasm.eval_comb().unwrap();
    wasm.eval_apply_ff_at(&w_clk).unwrap();
    wasm.eval_comb().unwrap();

    sim.tick(j_clk).unwrap();

    assert_eq!(wasm.get(w_q), sim.get(jq), "FF nonblocking tick2 mismatch");
}

#[test]
fn wasm_reg_ff_reset() {
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
    let (mut sim, mut wasm) = build_both(code, "Top");
    let j_clk = sim.event("clk");
    let j_rst = sim.signal("rst");
    let j_d = sim.signal("d");
    let j_q = sim.signal("q");
    let prog = sim.program().clone();

    let clk_addr = prog.get_addr(&[], &["clk"]).unwrap();
    let w_clk = wasm.resolve_event(&clk_addr);
    let w_rst = wasm.resolve_signal(&prog.get_addr(&[], &["rst"]).unwrap());
    let w_d = wasm.resolve_signal(&prog.get_addr(&[], &["d"]).unwrap());
    let w_q = wasm.resolve_signal(&prog.get_addr(&[], &["q"]).unwrap());

    // Reset active (AsyncLow: 0 = active)
    wasm.set(w_rst, 0u8);
    wasm.set(w_d, 0xAAu8);
    wasm.eval_comb().unwrap();
    wasm.eval_apply_ff_at(&w_clk).unwrap();
    wasm.eval_comb().unwrap();

    sim.modify(|io| {
        io.set(j_rst, 0u8);
        io.set(j_d, 0xAAu8);
    })
    .unwrap();
    sim.tick(j_clk).unwrap();

    assert_eq!(wasm.get(w_q), sim.get(j_q), "FF reset active mismatch");

    // Deactivate reset
    wasm.set(w_rst, 1u8);
    wasm.eval_comb().unwrap();
    wasm.eval_apply_ff_at(&w_clk).unwrap();
    wasm.eval_comb().unwrap();

    sim.modify(|io| io.set(j_rst, 1u8)).unwrap();
    sim.tick(j_clk).unwrap();

    assert_eq!(wasm.get(w_q), sim.get(j_q), "FF reset deactivated mismatch");
}

#[test]
fn wasm_reg_counter() {
    let code = r#"
        module Top (
            clk: input  '_ clock,
            rst: input  '_ reset_async_low,
            en:  input  logic,
            cnt: output logic<8>
        ) {
            var count: logic<8>;
            assign cnt = count;
            always_ff (clk, rst) {
                if_reset {
                    count = 0;
                } else if en {
                    count = count + 1;
                }
            }
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let j_clk = sim.event("clk");
    let j_rst = sim.signal("rst");
    let j_en = sim.signal("en");
    let j_cnt = sim.signal("cnt");
    let prog = sim.program().clone();

    let clk_addr = prog.get_addr(&[], &["clk"]).unwrap();
    let w_clk = wasm.resolve_event(&clk_addr);
    let w_rst = wasm.resolve_signal(&prog.get_addr(&[], &["rst"]).unwrap());
    let w_en = wasm.resolve_signal(&prog.get_addr(&[], &["en"]).unwrap());
    let w_cnt = wasm.resolve_signal(&prog.get_addr(&[], &["cnt"]).unwrap());

    // Reset
    wasm.set(w_rst, 0u8);
    wasm.set(w_en, 0u8);
    wasm.eval_comb().unwrap();
    wasm.eval_apply_ff_at(&w_clk).unwrap();
    wasm.eval_comb().unwrap();

    sim.modify(|io| {
        io.set(j_rst, 0u8);
        io.set(j_en, 0u8);
    })
    .unwrap();
    sim.tick(j_clk).unwrap();

    assert_eq!(wasm.get(w_cnt), sim.get(j_cnt), "counter reset mismatch");

    // Enable counting
    wasm.set(w_rst, 1u8);
    wasm.set(w_en, 1u8);

    sim.modify(|io| {
        io.set(j_rst, 1u8);
        io.set(j_en, 1u8);
    })
    .unwrap();

    for i in 1..=10u8 {
        wasm.eval_comb().unwrap();
        wasm.eval_apply_ff_at(&w_clk).unwrap();
        wasm.eval_comb().unwrap();

        sim.tick(j_clk).unwrap();

        assert_eq!(
            wasm.get(w_cnt),
            sim.get(j_cnt),
            "counter mismatch at tick {i}"
        );
    }
}

// ════════════════════════════════════════════════════════════════
// Wide value tests
// ════════════════════════════════════════════════════════════════

#[test]
fn wasm_reg_wide_add_128() {
    let code = r#"
        module Top (
            a: input  logic<128>,
            b: input  logic<128>,
            s: output logic<128>
        ) {
            assign s = a + b;
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let ja = sim.signal("a");
    let jb = sim.signal("b");
    let js = sim.signal("s");
    let prog = sim.program().clone();

    let wa = wasm.resolve_signal(&prog.get_addr(&[], &["a"]).unwrap());
    let wb = wasm.resolve_signal(&prog.get_addr(&[], &["b"]).unwrap());
    let ws = wasm.resolve_signal(&prog.get_addr(&[], &["s"]).unwrap());

    let test_cases: Vec<(BigUint, BigUint)> = vec![
        (BigUint::from(100u64), BigUint::from(200u64)),
        (BigUint::from(u64::MAX), BigUint::from(1u64)),
        (BigUint::from(0u64), BigUint::from(0u64)),
    ];

    for (av, bv) in &test_cases {
        wasm.set_wide(wa, av.clone());
        wasm.set_wide(wb, bv.clone());
        wasm.eval_comb().unwrap();

        sim.modify(|io| {
            io.set_wide(ja, av.clone());
            io.set_wide(jb, bv.clone());
        })
        .unwrap();

        assert_eq!(
            wasm.get(ws),
            sim.get(js),
            "wide add mismatch for a={av}, b={bv}"
        );
    }
}

#[test]
fn wasm_reg_wide_bitwise_128() {
    let code = r#"
        module Top (
            a: input logic<128>,
            b: input logic<128>,
            o_and: output logic<128>,
            o_or:  output logic<128>,
            o_xor: output logic<128>
        ) {
            always_comb {
                o_and = a & b;
                o_or  = a | b;
                o_xor = a ^ b;
            }
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let ja = sim.signal("a");
    let jb = sim.signal("b");
    let j_and = sim.signal("o_and");
    let j_or = sim.signal("o_or");
    let j_xor = sim.signal("o_xor");
    let prog = sim.program().clone();

    let wa = wasm.resolve_signal(&prog.get_addr(&[], &["a"]).unwrap());
    let wb = wasm.resolve_signal(&prog.get_addr(&[], &["b"]).unwrap());
    let w_and = wasm.resolve_signal(&prog.get_addr(&[], &["o_and"]).unwrap());
    let w_or = wasm.resolve_signal(&prog.get_addr(&[], &["o_or"]).unwrap());
    let w_xor = wasm.resolve_signal(&prog.get_addr(&[], &["o_xor"]).unwrap());

    let a_val = BigUint::from(0xDEADBEEFCAFEBABEu64);
    let b_val = BigUint::from(0x1234567890ABCDEFu64);

    wasm.set_wide(wa, a_val.clone());
    wasm.set_wide(wb, b_val.clone());
    wasm.eval_comb().unwrap();

    sim.modify(|io| {
        io.set_wide(ja, a_val.clone());
        io.set_wide(jb, b_val.clone());
    })
    .unwrap();

    assert_eq!(wasm.get(w_and), sim.get(j_and), "wide AND mismatch");
    assert_eq!(wasm.get(w_or), sim.get(j_or), "wide OR mismatch");
    assert_eq!(wasm.get(w_xor), sim.get(j_xor), "wide XOR mismatch");
}

// ════════════════════════════════════════════════════════════════
// Submodule hierarchy test
// ════════════════════════════════════════════════════════════════

#[test]
fn wasm_reg_submodule() {
    let code = r#"
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
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let j_in = sim.signal("top_in");
    let j_out = sim.signal("top_out");
    let prog = sim.program().clone();

    let w_in = wasm.resolve_signal(&prog.get_addr(&[], &["top_in"]).unwrap());
    let w_out = wasm.resolve_signal(&prog.get_addr(&[], &["top_out"]).unwrap());

    wasm.set(w_in, 0x55u8);
    wasm.eval_comb().unwrap();

    sim.modify(|io| io.set(j_in, 0x55u8)).unwrap();

    assert_eq!(wasm.get(w_out), sim.get(j_out), "submodule mismatch");
}

// ════════════════════════════════════════════════════════════════
// Bit-select tests
// ════════════════════════════════════════════════════════════════

#[test]
fn wasm_reg_bit_select() {
    let code = r#"
        module Top (a: input logic<5>, b: output logic<8>) {
            assign b[0]      = 1'b1;
            assign b[2:1]    = 2'b10;
            assign b[7:3]    = a;
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let ja = sim.signal("a");
    let jb = sim.signal("b");
    let prog = sim.program().clone();

    let wa = wasm.resolve_signal(&prog.get_addr(&[], &["a"]).unwrap());
    let wb = wasm.resolve_signal(&prog.get_addr(&[], &["b"]).unwrap());

    wasm.set(wa, 0b10101u8);
    wasm.eval_comb().unwrap();

    sim.modify(|io| io.set(ja, 0b10101u8)).unwrap();

    assert_eq!(wasm.get(wb), sim.get(jb), "bit select mismatch");
}

#[test]
fn wasm_reg_overlapping_override() {
    let code = r#"
        module Top (x: input logic<8>, y: input logic<4>, o: output logic<8>) {
            var a: logic<8>;
            always_comb{
                a = x;
                a[3:0] = y;
            }
            assign o = a;
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let jx = sim.signal("x");
    let jy = sim.signal("y");
    let jo = sim.signal("o");
    let prog = sim.program().clone();

    let wx = wasm.resolve_signal(&prog.get_addr(&[], &["x"]).unwrap());
    let wy = wasm.resolve_signal(&prog.get_addr(&[], &["y"]).unwrap());
    let wo = wasm.resolve_signal(&prog.get_addr(&[], &["o"]).unwrap());

    wasm.set(wx, 0xFFu8);
    wasm.set(wy, 0x0u8);
    wasm.eval_comb().unwrap();

    sim.modify(|io| {
        io.set(jx, 0xFFu8);
        io.set(jy, 0x0u8);
    })
    .unwrap();

    assert_eq!(wasm.get(wo), sim.get(jo), "overlapping override mismatch");
}

// ════════════════════════════════════════════════════════════════
// Signed arithmetic
// ════════════════════════════════════════════════════════════════

#[test]
fn wasm_reg_signed_comparison() {
    let code = r#"
        module Top (a: input i8, b: input i8, o_lt: output logic) {
            assign o_lt = a <: b;
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let ja = sim.signal("a");
    let jb = sim.signal("b");
    let jo = sim.signal("o_lt");
    let prog = sim.program().clone();

    let wa = wasm.resolve_signal(&prog.get_addr(&[], &["a"]).unwrap());
    let wb = wasm.resolve_signal(&prog.get_addr(&[], &["b"]).unwrap());
    let wo = wasm.resolve_signal(&prog.get_addr(&[], &["o_lt"]).unwrap());

    // -5 < 2 should be true
    wasm.set(wa, 0xFBu8);
    wasm.set(wb, 0x02u8);
    wasm.eval_comb().unwrap();

    sim.modify(|io| {
        io.set(ja, 0xFBu8);
        io.set(jb, 0x02u8);
    })
    .unwrap();

    assert_eq!(wasm.get(wo), sim.get(jo), "signed comparison mismatch");
}

#[test]
fn wasm_reg_signed_arith_shift() {
    let code = r#"
        module Top (a: input i8, o_sar: output i8) {
            always_comb {
                o_sar = a >>> 2;
            }
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let ja = sim.signal("a");
    let jo = sim.signal("o_sar");
    let prog = sim.program().clone();

    let wa = wasm.resolve_signal(&prog.get_addr(&[], &["a"]).unwrap());
    let wo = wasm.resolve_signal(&prog.get_addr(&[], &["o_sar"]).unwrap());

    wasm.set(wa, 0x80u8);
    wasm.eval_comb().unwrap();

    sim.modify(|io| io.set(ja, 0x80u8)).unwrap();

    assert_eq!(wasm.get(wo), sim.get(jo), "signed arith shift mismatch");
}

// ════════════════════════════════════════════════════════════════
// Logical operators
// ════════════════════════════════════════════════════════════════

#[test]
fn wasm_reg_logical_ops() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            o_and: output logic,
            o_or:  output logic
        ) {
            assign o_and = (|a) && (|b);
            assign o_or  = (|a) || (|b);
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let ja = sim.signal("a");
    let jb = sim.signal("b");
    let j_and = sim.signal("o_and");
    let j_or = sim.signal("o_or");
    let prog = sim.program().clone();

    let wa = wasm.resolve_signal(&prog.get_addr(&[], &["a"]).unwrap());
    let wb = wasm.resolve_signal(&prog.get_addr(&[], &["b"]).unwrap());
    let w_and = wasm.resolve_signal(&prog.get_addr(&[], &["o_and"]).unwrap());
    let w_or = wasm.resolve_signal(&prog.get_addr(&[], &["o_or"]).unwrap());

    for (av, bv) in [(0x55u8, 0x00u8), (0x00, 0x00), (0xFF, 0xFF), (0x00, 0x01)] {
        wasm.set(wa, av);
        wasm.set(wb, bv);
        wasm.eval_comb().unwrap();

        sim.modify(|io| {
            io.set(ja, av);
            io.set(jb, bv);
        })
        .unwrap();

        assert_eq!(
            wasm.get(w_and),
            sim.get(j_and),
            "and mismatch for a={av:#x}, b={bv:#x}"
        );
        assert_eq!(
            wasm.get(w_or),
            sim.get(j_or),
            "or mismatch for a={av:#x}, b={bv:#x}"
        );
    }
}

// ════════════════════════════════════════════════════════════════
// Subtraction underflow
// ════════════════════════════════════════════════════════════════

#[test]
fn wasm_reg_subtraction_underflow() {
    let code = r#"
        module Top (a: input logic<8>, b: input logic<8>, o: output logic<8>) {
            assign o = a - b;
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let ja = sim.signal("a");
    let jb = sim.signal("b");
    let jo = sim.signal("o");
    let prog = sim.program().clone();

    let wa = wasm.resolve_signal(&prog.get_addr(&[], &["a"]).unwrap());
    let wb = wasm.resolve_signal(&prog.get_addr(&[], &["b"]).unwrap());
    let wo = wasm.resolve_signal(&prog.get_addr(&[], &["o"]).unwrap());

    wasm.set(wa, 5u8);
    wasm.set(wb, 10u8);
    wasm.eval_comb().unwrap();

    sim.modify(|io| {
        io.set(ja, 5u8);
        io.set(jb, 10u8);
    })
    .unwrap();

    assert_eq!(wasm.get(wo), sim.get(jo), "underflow mismatch");
}

// ════════════════════════════════════════════════════════════════
// Dependency chain
// ════════════════════════════════════════════════════════════════

#[test]
fn wasm_reg_dependency_chain() {
    let code = r#"
        module Top (a: input logic<32>, b: output logic<32>) {
            var c: logic<32>;
            assign c = b;
            assign b = a;
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let ja = sim.signal("a");
    let jc = sim.signal("c");
    let prog = sim.program().clone();

    let wa = wasm.resolve_signal(&prog.get_addr(&[], &["a"]).unwrap());
    let wc = wasm.resolve_signal(&prog.get_addr(&[], &["c"]).unwrap());

    wasm.set(wa, 0x12345678u32);
    wasm.eval_comb().unwrap();

    sim.modify(|io| io.set(ja, 0x12345678u32)).unwrap();

    assert_eq!(wasm.get(wc), sim.get(jc), "dependency chain mismatch");
}

// ════════════════════════════════════════════════════════════════
// Case in always_ff (sequential case switch)
// ════════════════════════════════════════════════════════════════

#[test]
fn wasm_reg_case_in_always_ff() {
    let code = r#"
        module Top (
            clk: input clock,
            sel: input logic<2>,
            o:   output logic<8>
        ) {
            var r_val: logic<8>;
            always_ff {
                case sel {
                    2'd0: r_val = 8'h10;
                    2'd1: r_val = 8'h20;
                    default: r_val = 8'hFF;
                }
            }
            assign o = r_val;
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let j_clk = sim.event("clk");
    let j_sel = sim.signal("sel");
    let j_o = sim.signal("o");
    let prog = sim.program().clone();

    let clk_addr = prog.get_addr(&[], &["clk"]).unwrap();
    let w_clk = wasm.resolve_event(&clk_addr);
    let w_sel = wasm.resolve_signal(&prog.get_addr(&[], &["sel"]).unwrap());
    let w_o = wasm.resolve_signal(&prog.get_addr(&[], &["o"]).unwrap());

    for sel_v in [0u8, 1u8, 3u8] {
        wasm.set(w_sel, sel_v);
        wasm.eval_comb().unwrap();
        wasm.eval_apply_ff_at(&w_clk).unwrap();
        wasm.eval_comb().unwrap();

        sim.modify(|io| io.set(j_sel, sel_v)).unwrap();
        sim.tick(j_clk).unwrap();

        assert_eq!(
            wasm.get(w_o),
            sim.get(j_o),
            "case_ff mismatch for sel={sel_v}"
        );
    }
}

// ════════════════════════════════════════════════════════════════
// Division / Remainder
// ════════════════════════════════════════════════════════════════

#[test]
fn wasm_reg_div_rem() {
    let code = r#"
        module Top (
            a: input logic<8>,
            b: input logic<8>,
            o_div: output logic<8>,
            o_rem: output logic<8>
        ) {
            always_comb {
                o_div = a / b;
                o_rem = a % b;
            }
        }
    "#;
    let (mut sim, mut wasm) = build_both(code, "Top");
    let ja = sim.signal("a");
    let jb = sim.signal("b");
    let j_div = sim.signal("o_div");
    let j_rem = sim.signal("o_rem");
    let prog = sim.program().clone();

    let wa = wasm.resolve_signal(&prog.get_addr(&[], &["a"]).unwrap());
    let wb = wasm.resolve_signal(&prog.get_addr(&[], &["b"]).unwrap());
    let w_div = wasm.resolve_signal(&prog.get_addr(&[], &["o_div"]).unwrap());
    let w_rem = wasm.resolve_signal(&prog.get_addr(&[], &["o_rem"]).unwrap());

    // 100 / 7 = 14 rem 2
    wasm.set(wa, 100u8);
    wasm.set(wb, 7u8);
    wasm.eval_comb().unwrap();

    sim.modify(|io| {
        io.set(ja, 100u8);
        io.set(jb, 7u8);
    })
    .unwrap();

    assert_eq!(wasm.get(w_div), sim.get(j_div), "div mismatch");
    assert_eq!(wasm.get(w_rem), sim.get(j_rem), "rem mismatch");
}

// ════════════════════════════════════════════════════════════════
// VCD test with WasmBackend
// ════════════════════════════════════════════════════════════════

#[test]
fn wasm_reg_vcd_output() {
    use celox::VcdWriter;

    let code = r#"
        module Top (
            clk: input  '_ clock,
            rst: input  '_ reset_async_low,
            en:  input  logic,
            cnt: output logic<8>
        ) {
            var count: logic<8>;
            assign cnt = count;
            always_ff (clk, rst) {
                if_reset {
                    count = 0;
                } else if en {
                    count = count + 1;
                }
            }
        }
    "#;
    let (sim, mut wasm) = build_both(code, "Top");
    let prog = sim.program().clone();

    let clk_addr = prog.get_addr(&[], &["clk"]).unwrap();
    let w_clk = wasm.resolve_event(&clk_addr);
    let w_rst = wasm.resolve_signal(&prog.get_addr(&[], &["rst"]).unwrap());
    let w_en = wasm.resolve_signal(&prog.get_addr(&[], &["en"]).unwrap());
    let w_cnt = wasm.resolve_signal(&prog.get_addr(&[], &["cnt"]).unwrap());

    // Build VCD descriptors using the JIT simulator's helper
    let descs = sim.build_vcd_descs(false);

    let vcd_path = "/tmp/wasm_vcd_test.vcd";
    let mut vcd_writer = VcdWriter::new(vcd_path, &descs).unwrap();

    // Helper to dump VCD from WASM memory
    let dump_vcd = |wasm: &WasmBackend, writer: &mut VcdWriter, ts: u64| {
        let (ptr, size) = wasm.memory_as_ptr();
        let memory = unsafe { std::slice::from_raw_parts(ptr, size) };
        writer.dump(ts, memory).unwrap();
    };

    // Reset
    wasm.set(w_rst, 0u8);
    wasm.set(w_en, 0u8);
    wasm.eval_comb().unwrap();
    wasm.eval_apply_ff_at(&w_clk).unwrap();
    wasm.eval_comb().unwrap();
    dump_vcd(&wasm, &mut vcd_writer, 0);

    // Enable counting
    wasm.set(w_rst, 1u8);
    wasm.set(w_en, 1u8);
    for i in 1..=5u64 {
        wasm.eval_comb().unwrap();
        wasm.eval_apply_ff_at(&w_clk).unwrap();
        wasm.eval_comb().unwrap();
        dump_vcd(&wasm, &mut vcd_writer, i * 10);
    }

    // Verify VCD file was written
    let content = std::fs::read_to_string(vcd_path).unwrap();
    assert!(
        content.contains("$var wire"),
        "VCD should contain signal declarations"
    );
    assert!(content.contains("#0"), "VCD should contain timestamp 0");
    assert!(content.contains("#50"), "VCD should contain timestamp 50");

    // Counter should have reached 5 -- verify the last value in memory
    assert_eq!(wasm.get(w_cnt), BigUint::from(5u32), "counter should be 5");

    // Clean up
    std::fs::remove_file(vcd_path).unwrap();
}
