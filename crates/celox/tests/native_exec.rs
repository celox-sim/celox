//! Integration tests: execute native backend output and verify correctness.
#![cfg(target_arch = "x86_64")]

use celox::{MemoryLayout, MemoryLayoutMode, Program, Simulator, SimulatorBuilder};

#[cfg(target_arch = "x86_64")]
fn run_single_block_mir(insts: Vec<celox::native_backend::mir::MInst>, vreg_count: usize) -> u64 {
    use celox::native_backend::emit;
    use celox::native_backend::jit_mem;
    use celox::native_backend::mir::{
        BlockId as MBlockId, MBlock, MFunction, SpillDesc, VRegAllocator,
    };
    use celox::native_backend::regalloc;

    let mut vregs = VRegAllocator::new();
    for _ in 0..vreg_count {
        vregs.alloc();
    }
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); vreg_count]);
    let mut block = MBlock::new(MBlockId(0));
    for inst in insts {
        block.push(inst);
    }
    block.push(celox::native_backend::mir::MInst::Return);
    func.blocks.push(block);
    func.verify();

    let ra = regalloc::run_regalloc(&mut func).unwrap();
    let emit_result = emit::emit(&func, &ra.assignment, ra.spill_frame_size).expect("emit failed");
    let jit = jit_mem::JitCode::new(&emit_result.code).expect("mmap failed");
    let mut state = vec![0u8; 8];
    let ret = unsafe { jit.call(&mut state) };
    assert_eq!(ret, 0);
    u64::from_le_bytes(state[..8].try_into().unwrap())
}

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
    let layout = MemoryLayout::build(&sir, false, MemoryLayoutMode::ElementStrided);

    use celox::native_backend::{emit, isel, jit_mem, regalloc};

    let eu = &sir.eval_comb[0];
    let mut mfunc = isel::lower_execution_unit(eu, &layout, false);

    if debug {
        eprintln!("=== MIR ===\n{mfunc}");
    }

    let ra = regalloc::run_regalloc(&mut mfunc).unwrap();

    if debug {
        eprintln!("=== Assignment ===\n{:?}", ra.assignment);
    }

    let emit_result = emit::emit(&mfunc, &ra.assignment, ra.spill_frame_size).expect("emit failed");

    if debug {
        eprintln!(
            "=== Disassembly ===\n{}",
            emit::disassemble(&emit_result.code, 0)
        );
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
fn test_native_bsr_or_zero_and_nonzero() {
    use celox::native_backend::mir::{BaseReg, MInst, OpSize, VReg};

    let run = |value| {
        run_single_block_mir(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value,
                },
                MInst::BsrOr {
                    dst: VReg(1),
                    src: VReg(0),
                    zero_value: 63,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(1),
                    size: OpSize::S64,
                },
            ],
            2,
        )
    };

    assert_eq!(run(0), 63);
    assert_eq!(run(1), 0);
    assert_eq!(run(0x8000_0000_0000_0000), 63);
    assert_eq!(run(0x10), 4);
}

#[test]
fn test_native_bsr_nonzero() {
    use celox::native_backend::mir::{BaseReg, MInst, OpSize, VReg};

    let run = |value| {
        run_single_block_mir(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value,
                },
                MInst::Bsr {
                    dst: VReg(1),
                    src: VReg(0),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(1),
                    size: OpSize::S64,
                },
            ],
            2,
        )
    };

    assert_eq!(run(1), 0);
    assert_eq!(run(0x10), 4);
    assert_eq!(run(0x8000_0000_0000_0000), 63);
}

#[test]
fn test_native_div_zero_safe_rhs_select() {
    use celox::native_backend::mir::{BaseReg, CmpKind, MInst, OpSize, VReg};

    let run = |lhs, rhs| {
        run_single_block_mir(
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: lhs,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: rhs,
                },
                MInst::LoadImm {
                    dst: VReg(2),
                    value: 0,
                },
                MInst::LoadImm {
                    dst: VReg(3),
                    value: 1,
                },
                MInst::Cmp {
                    dst: VReg(4),
                    lhs: VReg(1),
                    rhs: VReg(2),
                    kind: CmpKind::Eq,
                },
                MInst::Select {
                    dst: VReg(5),
                    cond: VReg(4),
                    true_val: VReg(3),
                    false_val: VReg(1),
                },
                MInst::UDiv {
                    dst: VReg(6),
                    lhs: VReg(0),
                    rhs: VReg(5),
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(6),
                    size: OpSize::S64,
                },
            ],
            7,
        )
    };

    assert_eq!(run(42, 0), 42);
    assert_eq!(run(42, 7), 6);
}

#[test]
fn test_native_div_preserves_live_divisor_across_div() {
    use celox::native_backend::mir::{BaseReg, MInst, OpSize, VReg};

    let result = run_single_block_mir(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 100,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 2,
            },
            MInst::LoadImm {
                dst: VReg(2),
                value: 50,
            },
            MInst::UDiv {
                dst: VReg(3),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::UDiv {
                dst: VReg(4),
                lhs: VReg(2),
                rhs: VReg(1),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(4),
                size: OpSize::S64,
            },
        ],
        5,
    );

    assert_eq!(result, 25);
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
    assert_eq!(read_u32_at(&state, &sir, &layout, "x"), 0); // (7+3) & 1 = 0
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
                for i in 0..4 {
                    data[i] = 64'd0;
                }
                for g in 0..2 {
                    for s in 0..2 {
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
    let layout = celox::MemoryLayout::build(&sir, false, MemoryLayoutMode::ElementStrided);

    for (eu_idx, eu) in sir.eval_comb.iter().enumerate() {
        let mut mfunc = isel::lower_execution_unit(eu, &layout, false);
        eprintln!("=== EU {eu_idx} MIR ===\n{mfunc}");
        let ra = regalloc::run_regalloc(&mut mfunc).unwrap();
        eprintln!("=== EU {eu_idx} Assignment ===\n{:?}", ra.assignment);
        let emit_result =
            emit::emit(&mfunc, &ra.assignment, ra.spill_frame_size).expect("emit failed");
        eprintln!(
            "=== EU {eu_idx} Disassembly ===\n{}",
            emit::disassemble(&emit_result.code, 0)
        );
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
