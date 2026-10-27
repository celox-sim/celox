use super::*;
use std::cell::Cell;

thread_local! {
    /// Calls made on this thread, which is the one running the generated code.
    static CALLS: Cell<u64> = const { Cell::new(0) };
}

fn weighted(args: &[u64]) -> u64 {
    args.iter()
        .enumerate()
        .map(|(index, value)| (index as u64 + 1).wrapping_mul(*value))
        .fold(0, u64::wrapping_add)
}

/// Returns a weighted sum of its arguments after overwriting every register a
/// System V callee may change.
#[allow(clippy::too_many_arguments)]
extern "C" fn trashing_weighted_sum(
    a0: u64,
    a1: u64,
    a2: u64,
    a3: u64,
    a4: u64,
    a5: u64,
    a6: u64,
    a7: u64,
) -> u64 {
    CALLS.set(CALLS.get() + 1);
    let sum = weighted(&[a0, a1, a2, a3, a4, a5, a6, a7]);
    // SAFETY: the block writes only registers declared as clobbered.
    unsafe {
        std::arch::asm!(
            "mov rcx, -1", "mov rdx, -1", "mov rsi, -1", "mov rdi, -1",
            "mov r8, -1", "mov r9, -1", "mov r10, -1", "mov r11, -1",
            "pcmpeqd xmm0, xmm0", "pcmpeqd xmm1, xmm1", "pcmpeqd xmm2, xmm2",
            "pcmpeqd xmm3, xmm3", "pcmpeqd xmm4, xmm4", "pcmpeqd xmm5, xmm5",
            "pcmpeqd xmm6, xmm6", "pcmpeqd xmm7, xmm7", "pcmpeqd xmm8, xmm8",
            "pcmpeqd xmm9, xmm9", "pcmpeqd xmm10, xmm10", "pcmpeqd xmm11, xmm11",
            "pcmpeqd xmm12, xmm12", "pcmpeqd xmm13, xmm13", "pcmpeqd xmm14, xmm14",
            "pcmpeqd xmm15, xmm15",
            out("rcx") _, out("rdx") _, out("rsi") _, out("rdi") _,
            out("r8") _, out("r9") _, out("r10") _, out("r11") _,
            out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
            out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
            out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
            out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
        );
    }
    sum
}

/// Values start after the state header, whose extern table slot the test
/// fills in.
const BASE: usize = 64;
const LIVE: usize = 20;
const ARGS: usize = 8;
const RESULT: usize = BASE + 2 * LIVE * 8;

/// Two calls of eight arguments each (six in registers, two on the stack)
/// with every loaded value live across both.
fn live_across_calls(state_base: StateBaseStrategy) -> MFunction {
    let mut vregs = VRegAllocator::new();
    let live = (0..LIVE).map(|_| vregs.alloc()).collect::<Vec<_>>();
    let result = vregs.alloc();
    let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); LIVE + 1]);
    function.target_features = X86Features::for_test_with_state_base(false, state_base);
    let mut block = MBlock::new(BlockId(0));
    for (index, &value) in live.iter().enumerate() {
        block.push(MInst::Load {
            dst: value,
            base: BaseReg::SimState,
            offset: (BASE + index * 8) as i32,
            size: OpSize::S64,
        });
    }
    for call in 0..2 {
        for index in 0..ARGS {
            block.push(MInst::ExternArg {
                index: index as u8,
                src: live[call * ARGS + index],
            });
        }
        block.push(MInst::CallExtern {
            func: 0,
            arg_count: ARGS as u8,
        });
    }
    block.push(MInst::ExternResult { dst: result });
    for (index, &value) in live.iter().enumerate() {
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: (BASE + (LIVE + index) * 8) as i32,
            src: value,
            size: OpSize::S64,
        });
    }
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: RESULT as i32,
        src: result,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    function.push_block(block);
    function
}

fn run(state_base: StateBaseStrategy, tick_loop: bool) {
    let mut function = live_across_calls(state_base);
    let allocation = regalloc::run_regalloc(&mut function).unwrap();
    let plan = SsaDestructionPlan::build(&function, &allocation.assignment).unwrap();
    let emitted = emit_planned(
        &function,
        &allocation.assignment,
        allocation.spill_frame_size,
        RESULT + 8,
        &plan,
        tick_loop,
        false,
    )
    .unwrap();

    // Only XMMs pinned for the whole function are saved around the calls.
    let mut decoder = Decoder::new(64, &emitted.code[..emitted.text_size], DecoderOptions::NONE);
    while decoder.can_decode() {
        let instruction = decoder.decode();
        for operand in 0..instruction.op_count() {
            let register = instruction.op_register(operand);
            assert!(
                !(Register::XMM0..=Register::XMM8).contains(&register),
                "{state_base:?} saves an unpinned XMM: {instruction}"
            );
        }
    }

    let jit = JitCode::new(&emitted.code).unwrap();
    let mut memory = vec![0u64; emitted.required_state_size as usize / 8 + 1];
    let table = [trashing_weighted_sum as *const () as usize];
    memory[celox_state_layout::STATE_HEADER_EXTERN_FUNCTIONS_ADDR_OFFSET / 8] =
        table.as_ptr() as u64;
    let event_sequence = 0u64;
    memory[STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET / 8] = (&event_sequence as *const u64) as u64;
    memory[STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET / 8] = 1;
    let values = (0..LIVE as u64)
        .map(|index| 0x0101_0101_0000_0000 * (index + 1) + index)
        .collect::<Vec<_>>();
    memory[BASE / 8..BASE / 8 + LIVE].copy_from_slice(&values);

    let calls = CALLS.get();
    assert_eq!(unsafe { (jit.fn_ptr)(memory.as_mut_ptr().cast()) }, 0);
    assert_eq!(CALLS.get(), calls + 2);
    assert_eq!(
        &memory[BASE / 8 + LIVE..BASE / 8 + 2 * LIVE],
        &values[..],
        "{state_base:?} values live across the calls"
    );
    assert_eq!(memory[RESULT / 8], weighted(&values[ARGS..2 * ARGS]));
}

#[test]
fn extern_calls_preserve_live_values_with_an_r15_state_base() {
    run(StateBaseStrategy::R15, false);
    run(StateBaseStrategy::R15, true);
}

#[test]
fn extern_calls_preserve_live_values_with_a_segment_state_base() {
    if X86Features::detect().state_base() != StateBaseStrategy::Gs {
        return;
    }
    run(StateBaseStrategy::Gs, false);
    run(StateBaseStrategy::Gs, true);
}

/// The implicit state argument and signed status must survive ABI register
/// clobbers, spills across the call, and both state-base strategies.
extern "sysv64" fn kernel_with_status(state: *mut u8) -> i64 {
    let _ = trashing_weighted_sum(1, 2, 3, 4, 5, 6, 7, 8);
    // SAFETY: the generated caller passes this test's complete state buffer.
    unsafe { *state.add(RESULT + 16).cast::<i64>() }
}

#[test]
fn native_kernel_calls_preserve_live_values_and_propagate_signed_status() {
    let mut strategies = vec![StateBaseStrategy::R15];
    if X86Features::detect().state_base() == StateBaseStrategy::Gs {
        strategies.push(StateBaseStrategy::Gs);
    }
    for strategy in strategies {
        for tick_loop in [false, true] {
            let mut function = live_across_calls(strategy);
            let instructions = &mut function.blocks[0].insts;
            instructions.retain(|inst| !matches!(inst, MInst::ExternArg { .. }));
            for inst in instructions {
                if let MInst::CallExtern { func, arg_count } = inst {
                    *func = celox_sir::extern_abi::NATIVE_KERNEL_CALL_TAG;
                    *arg_count = 0;
                }
            }
            let allocation = regalloc::run_regalloc(&mut function).unwrap();
            let plan = SsaDestructionPlan::build(&function, &allocation.assignment).unwrap();
            let emitted = emit_planned_with_callees(
                &function,
                &allocation.assignment,
                allocation.spill_frame_size,
                RESULT + 24,
                &plan,
                tick_loop,
                false,
                &[RESULT + 24], // This Rust ABI surrogate uses no private native arena.
            )
            .unwrap();
            let jit = JitCode::new(&emitted.code).unwrap();
            let table = [kernel_with_status as *const () as usize];
            let event_sequence = 0u64;
            for status in [0i64, 7, -0x1234_5678_9abc] {
                let mut memory = vec![0u64; emitted.required_state_size as usize / 8 + 1];
                memory[celox_state_layout::STATE_HEADER_EXTERN_FUNCTIONS_ADDR_OFFSET / 8] =
                    table.as_ptr() as u64;
                memory[STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET / 8] =
                    (&event_sequence as *const u64) as u64;
                memory[STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET / 8] = 1;
                memory[(RESULT + 16) / 8] = status as u64;
                let values = (1..=LIVE as u64).collect::<Vec<_>>();
                memory[BASE / 8..BASE / 8 + LIVE].copy_from_slice(&values);
                let calls = CALLS.get();
                assert_eq!(unsafe { (jit.fn_ptr)(memory.as_mut_ptr().cast()) }, status);
                assert_eq!(CALLS.get() - calls, if status == 0 { 2 } else { 1 });
                if status == 0 {
                    assert_eq!(&memory[BASE / 8 + LIVE..BASE / 8 + 2 * LIVE], &values);
                } else {
                    assert!(
                        memory[BASE / 8 + LIVE..BASE / 8 + 2 * LIVE]
                            .iter()
                            .all(|&v| v == 0)
                    );
                }
            }
        }
    }
}

fn generated_kernel(
    strategy: StateBaseStrategy,
    input_base: usize,
    callee: Option<u32>,
) -> MFunction {
    let mut function = live_across_calls(strategy);
    for inst in &mut function.blocks[0].insts {
        if let MInst::Load { offset, .. } = inst {
            *offset += input_base as i32 - BASE as i32;
        }
    }
    let instructions = &mut function.blocks[0].insts;
    instructions.retain(|inst| !matches!(inst, MInst::ExternArg { .. }));
    match callee {
        Some(callee) => {
            for inst in instructions {
                if let MInst::CallExtern { func, arg_count } = inst {
                    *func = celox_sir::extern_abi::NATIVE_KERNEL_CALL_TAG | callee;
                    *arg_count = 0;
                }
            }
        }
        None => {
            instructions.retain(|inst| !matches!(inst, MInst::CallExtern { .. }));
            for inst in instructions.iter_mut() {
                if let MInst::ExternResult { dst } = inst {
                    *inst = MInst::LoadImm {
                        dst: *dst,
                        value: 0,
                    };
                }
            }
            // Prevent memory-backed reload recipes: each loaded value must
            // survive an overwrite of its source before it is published.
            let position = instructions
                .iter()
                .position(|inst| matches!(inst, MInst::LoadImm { .. }))
                .unwrap();
            let MInst::LoadImm { dst: zero, .. } = instructions[position] else {
                unreachable!()
            };
            instructions.splice(
                position + 1..position + 1,
                (0..LIVE).map(|index| MInst::Store {
                    base: BaseReg::SimState,
                    offset: (input_base + index * 8) as i32,
                    src: zero,
                    size: OpSize::S64,
                }),
            );
        }
    }
    function
}

#[test]
fn generated_callee_spills_do_not_overwrite_live_caller_values() {
    const STATE_SIZE: usize = 4096;
    let mut child = generated_kernel(StateBaseStrategy::R15, 1024, None);
    let allocation = regalloc::run_regalloc(&mut child).unwrap();
    assert!(allocation.spill_frame_size > 0, "callee must really spill");
    let plan = SsaDestructionPlan::build(&child, &allocation.assignment).unwrap();
    let child_code = emit_planned(
        &child,
        &allocation.assignment,
        allocation.spill_frame_size,
        STATE_SIZE,
        &plan,
        false,
        false,
    )
    .unwrap();
    let child_jit = JitCode::new(&child_code.code).unwrap();
    let mut caller = generated_kernel(StateBaseStrategy::R15, BASE, Some(0));
    let allocation = regalloc::run_regalloc(&mut caller).unwrap();
    assert!(
        allocation.spill_frame_size > 0,
        "caller must retain real spilled values"
    );
    assert!(
        matches!(emit(&caller, &allocation.assignment, allocation.spill_frame_size),
        Err(EmitError::Input(ref error)) if error.rule == "EMIT.NATIVE_CALLEE_ARENA")
    );
    let caller_code = emit_with_callees(
        &caller,
        &allocation.assignment,
        allocation.spill_frame_size,
        STATE_SIZE,
        &[child_code.required_state_size as usize],
    )
    .unwrap();
    let caller_jit = JitCode::new(&caller_code.code).unwrap();
    let table = [child_jit.fn_ptr as usize];
    let mut state = vec![
        0u64;
        caller_code
            .required_state_size
            .max(child_code.required_state_size) as usize
            / 8
            + 1
    ];
    state[celox_state_layout::STATE_HEADER_EXTERN_FUNCTIONS_ADDR_OFFSET / 8] =
        table.as_ptr() as u64;
    let original = (1..=LIVE as u64).collect::<Vec<_>>();
    state[BASE / 8..BASE / 8 + LIVE].copy_from_slice(&original);
    for i in 0..LIVE {
        state[1024 / 8 + i] = 0xdead_0000 + i as u64;
    }
    assert_eq!(unsafe { (caller_jit.fn_ptr)(state.as_mut_ptr().cast()) }, 0);
    assert_eq!(&state[BASE / 8 + LIVE..BASE / 8 + 2 * LIVE], &original);
}

fn compile_generated(
    mut function: MFunction,
    state_size: usize,
    callees: &[usize],
    tick_loop: bool,
) -> EmitResult {
    let allocation = regalloc::run_regalloc(&mut function).unwrap();
    assert!(
        allocation.spill_frame_size > 0,
        "every generated level must really spill"
    );
    let plan = SsaDestructionPlan::build(&function, &allocation.assignment).unwrap();
    let options = crate::X86BackendOptions::default().with_native_callees(callees.iter().copied());
    let bounds = options.arena_base.into_iter().collect::<Vec<_>>();
    emit_planned_with_callees(
        &function,
        &allocation.assignment,
        allocation.spill_frame_size,
        state_size,
        &plan,
        tick_loop,
        false,
        &bounds,
    )
    .unwrap()
}

#[test]
fn nested_generated_kernels_have_disjoint_arenas_and_propagate_status() {
    const STATE_SIZE: usize = 4096;
    const COMPLETED: usize = 2048;
    let mut strategies = vec![StateBaseStrategy::R15];
    if X86Features::detect().state_base() == StateBaseStrategy::Gs {
        strategies.push(StateBaseStrategy::Gs);
    }
    for strategy in strategies {
        for tick_loop in [false, true] {
            for status in [0i64, 7, -0x1234_5678_9abc] {
                let mut leaf = generated_kernel(strategy, 1024, None);
                if status != 0 {
                    *leaf.blocks[0].insts.last_mut().unwrap() = MInst::ReturnError { code: status };
                }
                let leaf_code = compile_generated(leaf, STATE_SIZE, &[], false);
                let leaf_jit = JitCode::new(&leaf_code.code).unwrap();
                let middle = generated_kernel(strategy, 512, Some(0));
                let middle_code = compile_generated(
                    middle,
                    STATE_SIZE,
                    &[leaf_code.required_state_size as usize],
                    false,
                );
                let middle_jit = JitCode::new(&middle_code.code).unwrap();
                let mut caller = generated_kernel(strategy, BASE, Some(1));
                let marker = caller.vregs.alloc();
                caller.spill_descs.push(SpillDesc::remat(0x51));
                let instructions = &mut caller.blocks[0].insts;
                let end = instructions.len() - 1;
                instructions.splice(
                    end..end,
                    [
                        MInst::LoadImm {
                            dst: marker,
                            value: 0x51,
                        },
                        MInst::Store {
                            base: BaseReg::SimState,
                            offset: COMPLETED as i32,
                            src: marker,
                            size: OpSize::S64,
                        },
                    ],
                );
                let caller_code = compile_generated(
                    caller,
                    STATE_SIZE,
                    &[middle_code.required_state_size as usize],
                    tick_loop,
                );
                assert!(middle_code.required_state_size > leaf_code.required_state_size);
                assert!(caller_code.required_state_size > middle_code.required_state_size);
                let caller_jit = JitCode::new(&caller_code.code).unwrap();
                let table = [leaf_jit.fn_ptr as usize, middle_jit.fn_ptr as usize];
                let mut state = vec![0u64; caller_code.required_state_size as usize / 8 + 1];
                state[celox_state_layout::STATE_HEADER_EXTERN_FUNCTIONS_ADDR_OFFSET / 8] =
                    table.as_ptr() as u64;
                let sequence = 0u64;
                state[STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET / 8] =
                    (&sequence as *const u64) as u64;
                state[STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET / 8] = 1;
                let original = (1..=LIVE as u64).collect::<Vec<_>>();
                state[BASE / 8..BASE / 8 + LIVE].copy_from_slice(&original);
                for i in 0..LIVE {
                    state[512 / 8 + i] = 1000 + i as u64;
                    state[1024 / 8 + i] = 0xdead_0000 + i as u64;
                }
                assert_eq!(
                    unsafe { (caller_jit.fn_ptr)(state.as_mut_ptr().cast()) },
                    status,
                    "{strategy:?}, tick={tick_loop}"
                );
                assert_eq!(state[COMPLETED / 8], if status == 0 { 0x51 } else { 0 });
                if status == 0 {
                    assert_eq!(&state[BASE / 8 + LIVE..BASE / 8 + 2 * LIVE], &original);
                }
            }
        }
    }
}

#[test]
fn generated_callee_arena_overflow_is_rejected() {
    let mut caller = generated_kernel(StateBaseStrategy::R15, BASE, Some(0));
    let allocation = regalloc::run_regalloc(&mut caller).unwrap();
    let result = emit_with_callees(
        &caller,
        &allocation.assignment,
        allocation.spill_frame_size,
        4096,
        &[usize::MAX],
    );
    assert!(
        matches!(result, Err(EmitError::Input(ref error)) if error.rule=="EMIT.NATIVE_ARENA_RANGE")
    );
}
