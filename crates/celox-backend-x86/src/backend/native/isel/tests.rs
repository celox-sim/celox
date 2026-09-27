mod bit_count;
mod memory;
mod packed;
mod selection;
mod sparse;
mod wide;

use super::*;
use crate::native::{emit, jit_mem::JitCode, mir_legalize, mir_opt, regalloc};
use crate::{AbsoluteAddr, BasicBlock, BlockId as SirBlockId, InstanceId, SIRValue};
use celox_design::StateObjectId as VarId;
use num_bigint::BigUint;

fn empty_layout() -> MemoryLayout {
    MemoryLayout {
        trace: None,
        four_state: false,
        mode: MemoryLayoutMode::Packed,
        unpacked_arrays: HashMap::default(),
        offsets: HashMap::default(),
        widths: HashMap::default(),
        is_4states: HashMap::default(),
        total_size: 0,
        working_offsets: HashMap::default(),
        working_base_offset: 0,
        sparse_offsets: HashMap::default(),
        sparse_base_offset: 0,
        sparse_layouts: HashMap::default(),
        sparse_active_bits_offset: 0,
        sparse_active_capacity: 0,
        merged_total_size: 0,
        triggered_bits_offset: 0,
        triggered_bits_total_size: 0,
        scratch_base_offset: 0,
        scratch_size: 0,
        runtime_event_capacity: 0,
        runtime_event_slot_size: 0,
        runtime_event_buffer_size: 0,
        runtime_event_site_layouts: vec![],
    }
}

fn get_bits(bytes: &[u8], bit_offset: usize, width: usize) -> u64 {
    let mut value = 0u64;
    for bit in 0..width {
        let source = bit_offset + bit;
        value |= u64::from((bytes[source / 8] >> (source % 8)) & 1) << bit;
    }
    value
}

fn set_bits(bytes: &mut [u8], bit_offset: usize, width: usize, value: u64) {
    for bit in 0..width {
        let destination = bit_offset + bit;
        let mask = 1u8 << (destination % 8);
        if (value >> bit) & 1 != 0 {
            bytes[destination / 8] |= mask;
        } else {
            bytes[destination / 8] &= !mask;
        }
    }
}

fn assert_indexed_state_accesses_have_alias_ranges(function: &MFunction) {
    let (loads, stores) = function.blocks.iter().flat_map(|block| &block.insts).fold(
        (Vec::new(), Vec::new()),
        |mut accesses, inst| {
            match inst {
                MInst::LoadIndexed { alias_range, .. } => accesses.0.push(alias_range),
                MInst::StoreIndexed { alias_range, .. }
                | MInst::OrStoreIndexed { alias_range, .. } => accesses.1.push(alias_range),
                _ => {}
            }
            accesses
        },
    );
    assert!(!loads.is_empty(), "fixture did not lower an indexed load");
    assert!(!stores.is_empty(), "fixture did not lower an indexed store");
    assert!(
        loads.iter().all(|range| range.is_some()),
        "ISel emitted an indexed load without a bounded memory effect"
    );
    assert!(
        stores.iter().all(|range| range.is_some()),
        "ISel emitted an indexed store without a bounded memory effect"
    );
}
