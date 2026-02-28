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
fn test_dynamic_index_read() {
    let code = r#"
        module Top (i: input logic<2>, o: output logic<8>) {
            var a: logic<8> [4];
            always_comb{
                a[0] = 8'hAA;
                a[1] = 8'hBB;
                a[2] = 8'hCC;
                a[3] = 8'hDD;
            }
            assign o = a[i];
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let i = sim.signal("i");
    let o = sim.signal("o");

    sim.modify(|io| io.set(i, 2u8)).unwrap();
    assert_eq!(sim.get(o), 0xCCu64.into());
}

#[test]
fn test_dynamic_index_write() {
    let code = r#"
        module Top (i: input logic<2>, val: input logic<8>, o: output logic<8>) {
            var a: logic<8> [4];
            always_comb{
                a[0] = 1;
                a[1] = 2;
                a[2] = 3;
                a[3] = 4;
                a[i] = val;
            }
            assign o = a[2];
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let i = sim.signal("i");
    let val = sim.signal("val");
    let o = sim.signal("o");

    sim.modify(|io| {
        io.set(i, 2u8);
        io.set(val, 0x55u8);
    })
    .unwrap();
    assert_eq!(sim.get(o), 0x55u64.into());
}

#[test]
fn test_multidimensional_access() {
    let code = r#"
        module Top (i: input logic<8>, o: output logic<8>) {
            var a: logic<8> [4, 2];
            assign a[1][0] = i;
            assign o = a[1][0];
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let i = sim.signal("i");
    let o = sim.signal("o");

    sim.modify(|io| io.set(i, 0xBEu8)).unwrap();
    assert_eq!(sim.get(o), 0xBEu8.into());
}

#[test]
fn test_minus_colon_and_step_execution() {
    let code = r#"
        module Top (a: input logic<8>, b: output logic<32>) {
            assign b[31-:8] = a;
            assign b[1 step 8] = a;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");

    sim.modify(|io| io.set(a, 0xAAu8)).unwrap();
    assert_eq!(sim.get(b), 0xAA00AA00u64.into());
}

#[test]
fn test_dynamic_slice_bullying() {
    let code = r#"
        module Top (idx: input logic<2>, data: input logic<16>, o: output logic<4>) {
            var mem: logic<16>;
            assign mem = data;
            assign o = mem[idx*4 +: 4];
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let idx = sim.signal("idx");
    let data = sim.signal("data");
    let o = sim.signal("o");

    sim.modify(|io| {
        io.set(data, 0xABCDu16);
        io.set(idx, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(o), 0xCu64.into());
}

#[test]
fn test_partial_write_merging() {
    let code = r#"
        module Top (a: input logic<4>, b: input logic<4>, o: output logic<8>) {
            var tmp: logic<8>;
            assign tmp[3:0] = a;
            always_comb {
                tmp[7:4] = b;
            }
            assign o = tmp;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let o = sim.signal("o");

    sim.modify(|io| {
        io.set(a, 0x5u8);
        io.set(b, 0xAu8);
    })
    .unwrap();
    assert_eq!(sim.get(o), 0xA5u8.into());
}

#[test]
fn test_stride_access() {
    // This test demonstrates a "false loop" where stride access is not correctly identified.
    // v[i*2] and v[i*2+1] should be disjoint, but because they share the same bounding box (0..31),
    // they are currently treated as overlapping, causing a combinational loop.
    let code = r#"
    module Top (
        i: input logic<4>,
        in_data: input logic<32>,
        o: output logic<32>,
    ) {
        var v: logic<32>;
        always_comb{
            v = in_data;
            v[i*2] = v[i*2+1];
        }
        assign o = v;
    }
    "#;
    let mut sim = SimulatorBuilder::new(code, "Top")
        .build()
        .expect("Should build successfully");
    let _builder = SimulatorBuilder::new(code, "Top").false_loop(
        (vec![], vec!["v".to_owned()]),
        (vec![], vec!["v".to_owned()]),
    );
    let i_port = sim.signal("i");
    let in_port = sim.signal("in_data");
    let o_port = sim.signal("o");

    // --- Verification ---
    // Input data: 0b...10101010 (odd bits are 1, even bits are 0)
    let test_data = 0xAAAAAAAAu32;

    sim.modify(|io| {
        io.set(in_port, test_data);
        io.set(i_port, 0u8); // i = 0
    })
    .unwrap();
    // When i=0: v[0] = v[1] is executed.
    // v[1] is 1, so v[0] should also become 1.
    // Result: 0xAAAA_AAAA -> 0xAAAA_AAB
    let result = sim.get(o_port);
    assert_eq!(result, 0xAAAA_AAABu32.into()); //   left: 2863311530
    // right: 2863311531

    sim.modify(|io| {
        io.set(i_port, 1u8); // i = 1
    })
    .unwrap();

    // When i=1: v[2] = v[3] is executed.
    // v[3] is 1, so v[2] should also become 1.
    // Result: 0xAAAA_AAAA -> 0xAAAA_AAAE
    let result = sim.get(o_port);
    assert_eq!(result, 0xAAAA_AAAEu32.into());
}

#[test]
fn test_dynamic_index_write_sir() {
    let code = r#"
    module Top (i: input logic<2>, val: input logic<8>, o: output logic<8>) {
        var a: logic<8> [4];
        always_comb{
            a[0] = 1;
            a[1] = 2;
            a[2] = 3;
            a[3] = 4;
            a[i] = val;
        }
        assign o = a[2];
    }
"#;
    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();
    assert_snapshot!("dynamic_index_write_sir", output);
}

#[test]
fn test_dynamic_offset_sir() {
    let code = r#"
    module Top (
        i: input logic<2>,
        o: output logic<8>
    ) {
        var a: logic<8> [4];
        always_comb {
            a[0] = 8'hAA;
            a[1] = 8'hBB;
            a[2] = 8'hCC;
            a[3] = 8'hDD;
        }
        // Dynamic read: This should generate Load with SIROffset::Dynamic (register)
        assign o = a[i];
    }
"#;
    let trace = setup_and_trace(code, "Top");
    let output = trace.format_program().unwrap();
    assert_snapshot!("dynamic_offset_sir", output);
}



