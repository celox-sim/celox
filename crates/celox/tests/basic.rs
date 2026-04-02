use celox::{DeadStorePolicy, Simulator, SimulatorBuilder};
use insta::assert_snapshot;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

fn setup_and_trace(code: &str, top: &str) -> celox::CompilationTrace {
    let result = SimulatorBuilder::new(code, top)
        .optimize(true)
        .trace_sim_modules()
        .trace_post_optimized_sir()
        .build_with_trace();

    result.trace
}

// ---------------------------------------------------------------------------
// Dead Store Elimination (DSE) tests
// ---------------------------------------------------------------------------

const DSE_HIERARCHY_SOURCE: &str = r#"
module Sub (
    i_data: input logic<8>,
    o_data: output logic<8>,
) {
    assign o_data = i_data;
}

module Top (
    clk: input clock,
    rst: input reset,
    top_in: input logic<8>,
    top_out: output logic<8>,
) {
    inst u_sub: Sub (
        i_data: top_in,
        o_data: top_out,
    );
}
"#;

all_backends! {

    fn test_simple_assignment(sim) {
        @setup { let code = r#"
module Top (a: input logic<32>, b: output logic<32>) {
assign b = a;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");

    sim.modify(|io| io.set(a, 0xDEADBEEFu32)).unwrap();
    assert_eq!(sim.get(b), 0xDEADBEEFu32.into());

    }

    fn test_dependency_chain(sim) {
        @setup { let code = r#"
module Top (a: input logic<32>, b: output logic<32>) {
var c: logic<32>;
assign c = b;
assign b = a;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let c = sim.signal("c");

    sim.modify(|io| io.set(a, 0x12345678u32)).unwrap();
    assert_eq!(sim.get(c), 0x12345678u32.into());

    }

    fn test_mixed_selects_execution(sim) {
        @setup { let code = r#"
module Top (a: input logic<5>, b: output logic<8>) {
assign b[0]      = 1'b1;
assign b[2:1]    = 2'b10;
assign b[7:3]    = a;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let b = sim.signal("b");

    sim.modify(|io| io.set(a, 0b10101u8)).unwrap();
    assert_eq!(sim.get(b), 0xADu64.into());

    }

    fn test_overlapping_override(sim) {
        @ignore_on(veryl);
        @setup { let code = r#"
module Top (x: input logic<8>, y: input logic<4>, o: output logic<8>) {
var a: logic<8>;
always_comb{
a = x;
a[3:0] = y;
}
assign o = a;
}
"#; }
        @build Simulator::builder(code, "Top");
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

    fn test_comb_override_dependency(sim) {
        @setup { let code = r#"
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
"#; }
        @build Simulator::builder(code, "Top");
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

    fn test_always_comb_blocking_assignment_chain(sim) {
        @ignore_on(veryl);
        @setup { let code = r#"
module Top (a: input logic<8>, o: output logic<8>) {
var x: logic<8>;
always_comb {
x = a;
x = x + 8'd1;
x = x << 1;
o = x;
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let o = sim.signal("o");

    sim.modify(|io| io.set(a, 10u8)).unwrap();
    // (10 + 1) << 1 = 22
    assert_eq!(sim.get(o), 22u8.into());

    }
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

#[test]
fn test_dse_preserve_top_ports() {
    // With PreserveTopPorts, top-level ports (top_in, top_out) survive DSE.
    let mut sim = Simulator::builder(DSE_HIERARCHY_SOURCE, "Top")
        .dead_store_policy(DeadStorePolicy::PreserveTopPorts)
        .build()
        .unwrap();
    let top_in = sim.signal("top_in");
    let top_out = sim.signal("top_out");

    sim.modify(|io| io.set(top_in, 0xABu8)).unwrap();
    assert_eq!(sim.get(top_out), 0xABu64.into());
}

#[test]
fn test_dse_preserve_all_ports() {
    // With PreserveAllPorts, both top-level AND sub-instance ports survive DSE.
    let mut sim = Simulator::builder(DSE_HIERARCHY_SOURCE, "Top")
        .dead_store_policy(DeadStorePolicy::PreserveAllPorts)
        .build()
        .unwrap();
    let top_in = sim.signal("top_in");
    let top_out = sim.signal("top_out");

    sim.modify(|io| io.set(top_in, 0x42u8)).unwrap();
    assert_eq!(sim.get(top_out), 0x42u64.into());

    // Sub-instance ports should also be accessible and correct
    let sub_signals = sim.instance_signals(&[("u_sub", 0)]);
    let sub_o_data = sub_signals.iter().find(|s| s.name == "o_data").unwrap();
    assert_eq!(sim.get(sub_o_data.signal), 0x42u64.into());
}
