use celox::{BigUint, SimulatorOptions, WasmBackend};

fn build_wasm(code: &str, top: &str) -> (WasmBackend, celox::Simulator) {
    let sim = celox::Simulator::builder(code, top).build().unwrap();
    let program = sim.program().clone();
    let opts = SimulatorOptions::default();
    let backend = WasmBackend::new(&program, &opts).expect("WasmBackend::new failed");
    (backend, sim)
}

#[test]
fn test_wasm_adder_combinational() {
    let code = r#"
        module Top (
            a: input  logic<8>,
            b: input  logic<8>,
            s: output logic<8>
        ) {
            assign s = a + b;
        }
    "#;
    let (mut backend, sim) = build_wasm(code, "Top");

    let a_addr = sim.program().get_addr(&[], &["a"]).unwrap();
    let b_addr = sim.program().get_addr(&[], &["b"]).unwrap();
    let s_addr = sim.program().get_addr(&[], &["s"]).unwrap();

    let a_sig = backend.resolve_signal(&a_addr);
    let b_sig = backend.resolve_signal(&b_addr);
    let s_sig = backend.resolve_signal(&s_addr);

    backend.set(a_sig, 10u8);
    backend.set(b_sig, 20u8);
    backend.eval_comb().expect("eval_comb failed");

    let result: u8 = backend.get_as(s_sig);
    assert_eq!(result, 30, "Expected 10 + 20 = 30, got {result}");
}

#[test]
fn test_wasm_mux_branching() {
    let code = r#"
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
    "#;
    let (mut backend, sim) = build_wasm(code, "Top");

    let sel_addr = sim.program().get_addr(&[], &["sel"]).unwrap();
    let a_addr = sim.program().get_addr(&[], &["a"]).unwrap();
    let b_addr = sim.program().get_addr(&[], &["b"]).unwrap();
    let y_addr = sim.program().get_addr(&[], &["y"]).unwrap();

    let sel = backend.resolve_signal(&sel_addr);
    let a = backend.resolve_signal(&a_addr);
    let b = backend.resolve_signal(&b_addr);
    let y = backend.resolve_signal(&y_addr);

    backend.set(sel, 1u8);
    backend.set(a, 42u8);
    backend.set(b, 99u8);
    backend.eval_comb().unwrap();
    assert_eq!(backend.get_as::<u8>(y), 42);

    backend.set(sel, 0u8);
    backend.eval_comb().unwrap();
    assert_eq!(backend.get_as::<u8>(y), 99);
}

#[test]
fn test_wasm_counter_sequential() {
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
    let (mut backend, sim) = build_wasm(code, "Top");

    let clk_addr = sim.program().get_addr(&[], &["clk"]).unwrap();
    let rst_addr = sim.program().get_addr(&[], &["rst"]).unwrap();
    let en_addr = sim.program().get_addr(&[], &["en"]).unwrap();
    let cnt_addr = sim.program().get_addr(&[], &["cnt"]).unwrap();

    let rst_sig = backend.resolve_signal(&rst_addr);
    let en_sig = backend.resolve_signal(&en_addr);
    let cnt_sig = backend.resolve_signal(&cnt_addr);
    let clk_ev = backend.resolve_event(&clk_addr);

    backend.set(rst_sig, 0u8);
    backend.set(en_sig, 0u8);
    backend.eval_comb().unwrap();
    backend.eval_apply_ff_at(&clk_ev).unwrap();
    backend.eval_comb().unwrap();
    assert_eq!(backend.get_as::<u8>(cnt_sig), 0);

    backend.set(rst_sig, 1u8);
    backend.set(en_sig, 1u8);
    for i in 1..=5u8 {
        backend.eval_comb().unwrap();
        backend.eval_apply_ff_at(&clk_ev).unwrap();
        backend.eval_comb().unwrap();
        assert_eq!(backend.get_as::<u8>(cnt_sig), i, "tick {i}");
    }
}

#[test]
fn test_wasm_wide_value_128bit() {
    let code = r#"
        module Top (
            a: input  logic<128>,
            b: input  logic<128>,
            s: output logic<128>
        ) {
            assign s = a + b;
        }
    "#;
    let (mut backend, sim) = build_wasm(code, "Top");

    let a_addr = sim.program().get_addr(&[], &["a"]).unwrap();
    let b_addr = sim.program().get_addr(&[], &["b"]).unwrap();
    let s_addr = sim.program().get_addr(&[], &["s"]).unwrap();

    let a_sig = backend.resolve_signal(&a_addr);
    let b_sig = backend.resolve_signal(&b_addr);
    let s_sig = backend.resolve_signal(&s_addr);

    backend.set_wide(a_sig, BigUint::from(100u64));
    backend.set_wide(b_sig, BigUint::from(200u64));
    backend.eval_comb().unwrap();
    assert_eq!(backend.get(s_sig), BigUint::from(300u64));

    let big_a = BigUint::from(u64::MAX);
    let big_b = BigUint::from(1u64);
    backend.set_wide(a_sig, big_a.clone());
    backend.set_wide(b_sig, big_b.clone());
    backend.eval_comb().unwrap();
    assert_eq!(backend.get(s_sig), &big_a + &big_b);
}
