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
