use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static CALLS: AtomicU64 = AtomicU64::new(0);

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
    CALLS.fetch_add(1, Ordering::SeqCst);
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
    let (jit, mut state) = compile(MFunction::new(vec![block], vec![]), state_size);

    let table = [weighted_sum as usize];
    let table_slot = celox_state_layout::STATE_HEADER_EXTERN_FUNCTIONS_ADDR_OFFSET;
    let values = (0..LIVE as u64)
        .map(|index| 0x0101_0101_0000_0000 * (index + 1) + index)
        .collect::<Vec<_>>();
    for (index, value) in values.iter().enumerate() {
        let offset = BASE + index * 8;
        state[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
    state[table_slot..table_slot + 8].copy_from_slice(&(table.as_ptr() as u64).to_le_bytes());

    let calls = CALLS.load(Ordering::SeqCst);
    assert_eq!(unsafe { (jit.fn_ptr)(state.as_mut_ptr()) }, 0);
    assert_eq!(CALLS.load(Ordering::SeqCst), calls + 2);
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
