//! Integration tests: execute native backend output and verify correctness.

use celox::{MemoryLayout, NativeBackend, Program, Simulator, SimulatorBuilder};
use num_bigint::BigUint;

/// Helper: compile Veryl, run native backend on eval_comb[0], execute, return state.
fn compile_and_run(
    code: &str,
    top: &str,
    setup: impl Fn(&mut [u8], &Program, &MemoryLayout),
) -> (Vec<u8>, Program, MemoryLayout) {
    let trace = SimulatorBuilder::new(code, top)
        .optimize(true)
        .trace_post_optimized_sir()
        .build_with_trace();
    let sir = trace.trace.post_optimized_sir.unwrap();
    let layout = MemoryLayout::build(&sir, false);

    use celox::native_backend::{emit, isel, jit_mem, regalloc};

    let eu = &sir.eval_comb[0];
    let mut mfunc = isel::lower_execution_unit(eu, &layout);
    let assignment = regalloc::run_regalloc(&mut mfunc);
    let emit_result = emit::emit(&mfunc, &assignment, 0).expect("emit failed");
    let jit = jit_mem::JitCode::new(&emit_result.code).expect("mmap failed");

    let mut state = vec![0u8; layout.merged_total_size.max(256)];
    setup(&mut state, &sir, &layout);

    let ret = unsafe { jit.call(&mut state) };
    assert_eq!(ret, 0, "JIT function returned non-zero (error)");

    (state, sir, layout)
}

fn write_u32_at(state: &mut [u8], sir: &Program, layout: &MemoryLayout, name: &str, val: u32) {
    let addr = sir.get_addr(&[], &[name]).unwrap();
    let off = layout.offsets[&addr];
    state[off..off + 4].copy_from_slice(&val.to_le_bytes());
}

fn read_u32_at(state: &[u8], sir: &Program, layout: &MemoryLayout, name: &str) -> u32 {
    let addr = sir.get_addr(&[], &[name]).unwrap();
    let off = layout.offsets[&addr];
    u32::from_le_bytes(state[off..off + 4].try_into().unwrap())
}

#[test]
fn test_native_add() {
    let code = r#"
        module Top (x: input logic<32>, y: input logic<32>, z: output logic<32>) {
            assign z = x + y;
        }
    "#;
    let (state, sir, layout) = compile_and_run(code, "Top", |state, sir, layout| {
        write_u32_at(state, sir, layout, "x", 100);
        write_u32_at(state, sir, layout, "y", 200);
    });
    assert_eq!(read_u32_at(&state, &sir, &layout, "z"), 300);
}

#[test]
fn test_native_bitwise() {
    let code = r#"
        module Top (a: input logic<32>, b: input logic<32>, x: output logic<32>, y: output logic<32>) {
            assign x = a & b;
            assign y = a | b;
        }
    "#;
    let (state, sir, layout) = compile_and_run(code, "Top", |state, sir, layout| {
        write_u32_at(state, sir, layout, "a", 0xFF00FF00);
        write_u32_at(state, sir, layout, "b", 0x0F0F0F0F);
    });
    assert_eq!(read_u32_at(&state, &sir, &layout, "x"), 0x0F000F00);
    assert_eq!(read_u32_at(&state, &sir, &layout, "y"), 0xFF0FFF0F);
}

#[test]
fn test_native_shared_expression() {
    let code = r#"
        module Top (a: input logic<32>, b: input logic<32>, x: output logic<32>, y: output logic<32>) {
            assign x = (a + b) & 32'd1;
            assign y = (a + b) | 32'd2;
        }
    "#;
    let (state, sir, layout) = compile_and_run(code, "Top", |state, sir, layout| {
        write_u32_at(state, sir, layout, "a", 7);
        write_u32_at(state, sir, layout, "b", 3);
    });
    assert_eq!(read_u32_at(&state, &sir, &layout, "x"), 0);  // (7+3) & 1 = 0
    assert_eq!(read_u32_at(&state, &sir, &layout, "y"), 10); // (7+3) | 2 = 10
}

#[test]
fn test_native_sub() {
    let code = r#"
        module Top (a: input logic<32>, b: input logic<32>, z: output logic<32>) {
            assign z = a - b;
        }
    "#;
    let (state, sir, layout) = compile_and_run(code, "Top", |state, sir, layout| {
        write_u32_at(state, sir, layout, "a", 500);
        write_u32_at(state, sir, layout, "b", 200);
    });
    assert_eq!(read_u32_at(&state, &sir, &layout, "z"), 300);
}

#[test]
fn test_native_xor() {
    let code = r#"
        module Top (a: input logic<32>, b: input logic<32>, z: output logic<32>) {
            assign z = a ^ b;
        }
    "#;
    let (state, sir, layout) = compile_and_run(code, "Top", |state, sir, layout| {
        write_u32_at(state, sir, layout, "a", 0xAAAAAAAA);
        write_u32_at(state, sir, layout, "b", 0x55555555);
    });
    assert_eq!(read_u32_at(&state, &sir, &layout, "z"), 0xFFFFFFFF);
}

#[test]
fn test_native_mul() {
    let code = r#"
        module Top (a: input logic<32>, b: input logic<32>, z: output logic<32>) {
            assign z = a * b;
        }
    "#;
    let (state, sir, layout) = compile_and_run(code, "Top", |state, sir, layout| {
        write_u32_at(state, sir, layout, "a", 7);
        write_u32_at(state, sir, layout, "b", 6);
    });
    assert_eq!(read_u32_at(&state, &sir, &layout, "z"), 42);
}

// ────────────────────────────────────────────────────────────────
// Tests using Simulator<NativeBackend> via build_native()
// ────────────────────────────────────────────────────────────────

#[test]
fn test_simulator_native_simple_assignment() {
    let code = r#"
        module Top (a: input logic<32>, b: output logic<32>) {
            assign b = a;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build_native().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    sim.modify(|io| io.set(a, 0xDEADBEEFu32)).unwrap();
    assert_eq!(sim.get(b), 0xDEADBEEFu32.into());
}

#[test]
fn test_simulator_native_add() {
    let code = r#"
        module Top (
            x: input logic<32>,
            y: input logic<32>,
            z: output logic<32>,
        ) {
            assign z = x + y;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build_native().unwrap();
    let x = sim.signal("x");
    let y = sim.signal("y");
    let z = sim.signal("z");
    sim.modify(|io| {
        io.set(x, 100u32);
        io.set(y, 200u32);
    })
    .unwrap();
    assert_eq!(sim.get(z), 300u32.into());
}

#[test]
fn test_simulator_native_dependency_chain() {
    let code = r#"
        module Top (a: input logic<32>, b: output logic<32>) {
            var c: logic<32>;
            assign c = b;
            assign b = a;
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build_native().unwrap();
    let a = sim.signal("a");
    let c = sim.signal("c");
    sim.modify(|io| io.set(a, 0x12345678u32)).unwrap();
    assert_eq!(sim.get(c), 0x12345678u32.into());
}
