//! Integration tests: execute native backend output and verify correctness.

use celox::{MemoryLayout, Program, Simulator, SimulatorBuilder};

/// Helper: compile Veryl, run native backend on eval_comb[0], execute, return state.
fn compile_and_run(
    code: &str,
    top: &str,
    setup: impl Fn(&mut [u8], &Program, &MemoryLayout),
) -> (Vec<u8>, Program, MemoryLayout) {
    compile_and_run_inner(code, top, setup, false)
}

fn compile_and_run_inner(
    code: &str,
    top: &str,
    setup: impl Fn(&mut [u8], &Program, &MemoryLayout),
    debug: bool,
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

    if debug {
        eprintln!("=== MIR ===\n{mfunc}");
    }

    let ra = regalloc::run_regalloc(&mut mfunc);

    if debug {
        eprintln!("=== Assignment ===\n{:?}", ra.assignment);
    }

    let emit_result = emit::emit(&mfunc, &ra.assignment, ra.spill_frame_size).expect("emit failed");

    if debug {
        eprintln!("=== Disassembly ===\n{}", emit::disassemble(&emit_result.code, 0));
    }

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

// Debug test: register-based shift (used by dynamic index write pattern)
#[test]
fn test_native_shl_register() {
    let code = r#"
        module Top (
            val: input logic<32>,
            shift_amt: input logic<32>,
            z: output logic<32>,
        ) {
            assign z = val << shift_amt;
        }
    "#;
    let (state, sir, layout) = compile_and_run(code, "Top", |state, sir, layout| {
        write_u32_at(state, sir, layout, "val", 0xFF);
        write_u32_at(state, sir, layout, "shift_amt", 16);
    });
    assert_eq!(read_u32_at(&state, &sir, &layout, "z"), 0x00FF0000);
}

// Debug: dump SIR + MIR + disassembly for failing test
#[test]
fn test_debug_let_bitslice_write() {
    let code = r#"
        module Top (
            o_lo: output logic<32>,
            o_hi: output logic<32>
        ) {
            var data: logic<64> [4];
            always_comb {
                for i: u32 in 0..4 {
                    data[i] = 64'd0;
                }
                for g: u32 in 0..2 {
                    for s: u32 in 0..2 {
                        let idx: u32 = g * 2 + s;
                        data[idx][63:32] = (g * 2 + s) as u32;
                        data[idx][31:0]  = (g * 2 + s + 100) as u32;
                    }
                }
                o_hi = data[2][63:32];
                o_lo = data[2][31:0];
            }
        }
    "#;
    let trace = SimulatorBuilder::new(code, "Top")
        .optimize(true)
        .trace_post_optimized_sir()
        .build_with_trace();
    let sir_text = trace.trace.format_program().unwrap();
    eprintln!("{sir_text}");
    let sir = trace.trace.post_optimized_sir.unwrap();

    use celox::native_backend::{emit, isel, regalloc};
    let layout = celox::MemoryLayout::build(&sir, false);

    for (eu_idx, eu) in sir.eval_comb.iter().enumerate() {
        let mut mfunc = isel::lower_execution_unit(eu, &layout);
        eprintln!("=== EU {eu_idx} MIR ===\n{mfunc}");
        let ra = regalloc::run_regalloc(&mut mfunc);
        eprintln!("=== EU {eu_idx} Assignment ===\n{:?}", ra.assignment);
        let emit_result = emit::emit(&mfunc, &ra.assignment, ra.spill_frame_size).expect("emit failed");
        eprintln!("=== EU {eu_idx} Disassembly ===\n{}", emit::disassemble(&emit_result.code, 0));
    }

    // Also run and check the result
    let mut sim = SimulatorBuilder::new(code, "Top").build_native().unwrap();
    let o_hi = sim.signal("o_hi");
    let o_lo = sim.signal("o_lo");
    eprintln!("Native: o_hi={:?}, o_lo={:?}", sim.get(o_hi), sim.get(o_lo));
    assert_eq!(sim.get(o_hi), 2u64.into());
    assert_eq!(sim.get(o_lo), 102u64.into());
}

// Regression: dynamic index write pattern (shl + bitnot + and + or with multiple shift amounts)
#[test]
fn test_native_dynamic_index_pattern() {
    let code = r#"
        module Top (
            packed: input logic<32>,
            idx: input logic<2>,
            val: input logic<8>,
            z: output logic<32>,
        ) {
            var mask: logic<32>;
            var shift: logic<32>;
            assign shift = idx as u32 * 8;
            assign mask = 32'hFF << shift;
            assign z = (packed & ~mask) | ((val as u32) << shift);
        }
    "#;
    let (state, sir, layout) = compile_and_run(code, "Top", |state, sir, layout| {
        write_u32_at(state, sir, layout, "packed", 0x04030201);
        let idx_addr = sir.get_addr(&[], &["idx"]).unwrap();
        let idx_off = layout.offsets[&idx_addr];
        state[idx_off] = 2;
        let val_addr = sir.get_addr(&[], &["val"]).unwrap();
        let val_off = layout.offsets[&val_addr];
        state[val_off] = 0x55;
    });
    assert_eq!(read_u32_at(&state, &sir, &layout, "z"), 0x04550201);
}

