use insta::assert_snapshot;
use celox::{Simulator, SimulatorBuilder};

fn setup_and_trace(code: &str, top: &str) -> celox::CompilationTrace {
    let result = SimulatorBuilder::new(code, top)
        .optimize(true)
        .trace_sim_modules()
        .trace_post_optimized_sir()
        .build_with_trace();

    result.trace
}

#[test]
fn test_simple_assignment() {
    let code = r#"
        module Top (a: input logic<32>, b: output logic<32>) {
            assign b = a;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");

    sim.modify(|io| io.set(a, 0xDEADBEEFu32)).unwrap();
    assert_eq!(sim.get(b), 0xDEADBEEFu32.into());
}

#[test]
fn test_dependency_chain() {
    let code = r#"
        module Top (a: input logic<32>, b: output logic<32>) {
            var c: logic<32>;
            assign c = b;
            assign b = a;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let c = sim.signal("c");

    sim.modify(|io| io.set(a, 0x12345678u32)).unwrap();
    assert_eq!(sim.get(c), 0x12345678u32.into());
}

#[test]
fn test_mixed_selects_execution() {
    let code = r#"
        module Top (a: input logic<5>, b: output logic<8>) {
            assign b[0]      = 1'b1;
            assign b[2:1]    = 2'b10;
            assign b[7:3]    = a;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");

    sim.modify(|io| io.set(a, 0b10101u8)).unwrap();
    assert_eq!(sim.get(b), 0xADu64.into());
}

#[test]
fn test_overlapping_override() {
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
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let x = sim.signal("x");
    let y = sim.signal("y");
    let o = sim.signal("o");

    sim.modify(|io| {
        io.set(x, 0xFFu8);
        io.set(y, 0x0u8);
    })
    .unwrap();
    assert_eq!(sim.get(o), 0xF0u64.into());
}

#[test]
fn test_comb_override_dependency() {
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
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let sel = sim.signal("sel");
    let val = sim.signal("val");
    let o = sim.signal("o");

    // sel = 0 -> o = 0x11
    sim.modify(|io| io.set(sel, 0u8)).unwrap();
    assert_eq!(sim.get(o), 0x11u64.into());

    // sel = 1, val = 0xEE -> o = 0xEE
    sim.modify(|io| {
        io.set(sel, 1u8);
        io.set(val, 0xEEu8);
    })
    .unwrap();
    assert_eq!(sim.get(o), 0xEEu64.into());
}

#[test]
fn test_always_comb_blocking_assignment_chain() {
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
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let o = sim.signal("o");

    sim.modify(|io| io.set(a, 10u8)).unwrap();
    // (10 + 1) << 1 = 22
    assert_eq!(sim.get(o), 22u8.into());
}

#[test]
fn test_shared_expression_hoisting() {
    let code = r#"
    module Top (
        a: input logic<32>,
        b: input logic<32>,
        x: output logic<32>,
        y: output logic<32>,
    ) {
        // (a + b) is shared
        assign x = (a + b) & 32'h1;
        assign y = (a + b) | 32'h2;
    }
    "#;

    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();
    assert_snapshot!("shared_expression_sir", output);
}

#[test]
fn test_mux_safe_hoisting() {
    let code = r#"
    module Top (
        a: input logic<32>,
        b: input logic<32>,
        c: input logic,
        x: output logic<32>,
        y: output logic<32>,
    ) {
        var m: logic<32>;
        always_comb {
            if c {
                m = a;
            } else {
                m = b;
            }
        }
        
        // (m + 1) is shared but depends on Mux result (m)
        // It should NOT be hoisted to entry block.
        assign x = (m + 1) & 32'h1;
        assign y = (m + 1) | 32'h2;
    }
    "#;

    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();
    assert_snapshot!("mux_safe_hoisting_sir", output);
}

#[test]
fn test_hash_consing_deduplication() {
    let code = r#"
    module Top (
        a: input logic<32>,
        b: input logic<32>,
        x: output logic<32>,
    ) {
        // Multiple identical additions
        assign x = (a + b) + (a + b);
    }
    "#;

    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();
    assert_snapshot!("hash_consing_sir", output);
}

#[test]
fn test_rle_comb() {
    let trace = setup_and_trace(
        r#"
module ModuleA (
    x: input logic<32>,
    y: input logic<32>,
    z: output logic<32>
) {
    var temp: logic<32>;

    always_comb {
        temp = x + y;
        z = temp;
    }
}
"#,
        "ModuleA",
    );
    let output = trace.format_program().unwrap();
    assert_snapshot!("rle_comb", output);
}



