use super::*;
use std::cell::Cell;

thread_local! {
    /// Calls made on this thread, which is the one running the generated code.
    static CALLS: Cell<u64> = const { Cell::new(0) };
}

#[allow(clippy::too_many_arguments)]
extern "C" fn weighted_sum(
    a0: u64,
    a1: u64,
    a2: u64,
    a3: u64,
    a4: u64,
    a5: u64,
    a6: u64,
    a7: u64,
    a8: u64,
    a9: u64,
) -> u64 {
    CALLS.set(CALLS.get() + 1);
    // Overwrite every register an AAPCS64 callee may change, other than the
    // result register and the platform register x18.
    // SAFETY: the block writes only registers declared as clobbered.
    unsafe {
        std::arch::asm!(
            "mov x1, #-1", "mov x2, #-1", "mov x3, #-1", "mov x4, #-1",
            "mov x5, #-1", "mov x6, #-1", "mov x7, #-1", "mov x8, #-1",
            "mov x9, #-1", "mov x10, #-1", "mov x11, #-1", "mov x12, #-1",
            "mov x13, #-1", "mov x14, #-1", "mov x15, #-1", "mov x16, #-1",
            "mov x17, #-1",
            "movi v0.2d, #0xffffffffffffffff", "movi v1.2d, #0xffffffffffffffff",
            "movi v2.2d, #0xffffffffffffffff", "movi v3.2d, #0xffffffffffffffff",
            "movi v4.2d, #0xffffffffffffffff", "movi v5.2d, #0xffffffffffffffff",
            "movi v6.2d, #0xffffffffffffffff", "movi v7.2d, #0xffffffffffffffff",
            "movi v16.2d, #0xffffffffffffffff", "movi v17.2d, #0xffffffffffffffff",
            "movi v18.2d, #0xffffffffffffffff", "movi v19.2d, #0xffffffffffffffff",
            "movi v20.2d, #0xffffffffffffffff", "movi v21.2d, #0xffffffffffffffff",
            "movi v22.2d, #0xffffffffffffffff", "movi v23.2d, #0xffffffffffffffff",
            "movi v24.2d, #0xffffffffffffffff", "movi v25.2d, #0xffffffffffffffff",
            "movi v26.2d, #0xffffffffffffffff", "movi v27.2d, #0xffffffffffffffff",
            "movi v28.2d, #0xffffffffffffffff", "movi v29.2d, #0xffffffffffffffff",
            "movi v30.2d, #0xffffffffffffffff", "movi v31.2d, #0xffffffffffffffff",
            out("x1") _, out("x2") _, out("x3") _, out("x4") _, out("x5") _,
            out("x6") _, out("x7") _, out("x8") _, out("x9") _, out("x10") _,
            out("x11") _, out("x12") _, out("x13") _, out("x14") _, out("x15") _,
            out("x16") _, out("x17") _,
            out("v0") _, out("v1") _, out("v2") _, out("v3") _, out("v4") _,
            out("v5") _, out("v6") _, out("v7") _, out("v16") _, out("v17") _,
            out("v18") _, out("v19") _, out("v20") _, out("v21") _, out("v22") _,
            out("v23") _, out("v24") _, out("v25") _, out("v26") _, out("v27") _,
            out("v28") _, out("v29") _, out("v30") _, out("v31") _,
        );
    }
    [a0, a1, a2, a3, a4, a5, a6, a7, a8, a9]
        .iter()
        .enumerate()
        .map(|(index, value)| (index as u64 + 1).wrapping_mul(*value))
        .fold(0, u64::wrapping_add)
}

/// Values start after the state header, whose extern table slot the test
/// fills in.
const BASE: usize = 64;
const LIVE: u32 = 24;

#[test]
fn extern_calls_preserve_live_values_and_pass_stack_arguments() {
    extern_calls_preserve_live_values(false);
}

#[test]
fn extern_calls_in_a_tick_loop_preserve_its_pinned_registers() {
    extern_calls_preserve_live_values(true);
}

fn extern_calls_preserve_live_values(tick_loop: bool) {
    let mut block = MBlock::new(BlockId(0));
    for index in 0..LIVE {
        block.push(MInst::Load {
            dst: VReg(index),
            base: BaseReg::SimState,
            offset: (BASE + index as usize * 8) as i32,
            size: OpSize::S64,
        });
    }
    // Ten arguments: eight in registers and two on the stack. Every loaded
    // value stays live across both calls.
    block.push(MInst::CallExtern {
        dst: Some(VReg(100)),
        func: 0,
        args: (0..10).map(VReg).collect(),
    });
    block.push(MInst::CallExtern {
        dst: None,
        func: 0,
        args: (10..20).map(VReg).collect(),
    });
    for index in 0..LIVE {
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: (BASE + (LIVE + index) as usize * 8) as i32,
            src: VReg(index),
            size: OpSize::S64,
        });
    }
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: (BASE + 2 * LIVE as usize * 8) as i32,
        src: VReg(100),
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    let state_size = BASE + (2 * LIVE as usize + 1) * 8;
    let mut function = MFunction::new(vec![block], vec![]);
    crate::mir_opt::optimize(&mut function);
    crate::mir_legalize::legalize_variable_shift_counts(&mut function);
    let allocation = crate::regalloc::allocate_with_spills(function, || false).unwrap();
    let emitted = emit_function(
        &allocation.allocated.function,
        &allocation.allocated.assignment,
        allocation.spill_frame_size,
        state_size,
        &allocation.allocated.edge_copies,
        tick_loop,
        false,
    )
    .unwrap();
    let jit = JitCode::new(&emitted.code).unwrap();
    let mut state = vec![0; emitted.required_state_size as usize];
    let remaining = STATE_HEADER_NATIVE_LOOP_REMAINING_OFFSET;
    state[remaining..remaining + 8].copy_from_slice(&1u64.to_le_bytes());

    let table = [weighted_sum as *const () as usize];
    let table_slot = celox_state_layout::STATE_HEADER_EXTERN_FUNCTIONS_ADDR_OFFSET;
    let values = (0..LIVE as u64)
        .map(|index| 0x0101_0101_0000_0000 * (index + 1) + index)
        .collect::<Vec<_>>();
    for (index, value) in values.iter().enumerate() {
        let offset = BASE + index * 8;
        state[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
    state[table_slot..table_slot + 8].copy_from_slice(&(table.as_ptr() as u64).to_le_bytes());

    let calls = CALLS.get();
    assert_eq!(unsafe { (jit.fn_ptr)(state.as_mut_ptr()) }, 0);
    assert_eq!(CALLS.get(), calls + 2);
    for (index, value) in values.iter().enumerate() {
        let offset = BASE + (LIVE as usize + index) * 8;
        assert_eq!(
            u64::from_le_bytes(state[offset..offset + 8].try_into().unwrap()),
            *value,
            "value {index} live across the calls"
        );
    }
    let offset = BASE + 2 * LIVE as usize * 8;
    assert_eq!(
        u64::from_le_bytes(state[offset..offset + 8].try_into().unwrap()),
        weighted_sum(
            values[0], values[1], values[2], values[3], values[4], values[5], values[6], values[7],
            values[8], values[9],
        )
    );
}
