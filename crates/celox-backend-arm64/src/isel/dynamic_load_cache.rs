//! AArch64-owned planning for block-local dynamic load caches.

use crate::mir::OpSize;
use crate::{
    BasicBlock, HashMap, HashSet, MemoryLayout, RegionedAbsoluteAddr, SIRInstruction, SIROffset,
};

#[derive(Debug, Default)]
pub(super) struct BlockDynamicLoadCachePlans {
    pub(super) addresses: HashSet<RegionedAbsoluteAddr>,
}

pub(super) fn native_plane_access_size(byte_size: usize) -> Option<OpSize> {
    match byte_size {
        1 => Some(OpSize::S8),
        2 => Some(OpSize::S16),
        4 => Some(OpSize::S32),
        8 => Some(OpSize::S64),
        _ => None,
    }
}

pub(super) fn block_dynamic_load_cache_plans(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    layout: &MemoryLayout,
) -> BlockDynamicLoadCachePlans {
    const MIN_LOADS: usize = 4;

    let mut counts = HashMap::<RegionedAbsoluteAddr, usize>::default();
    let mut written_ranges = Vec::<(i32, usize)>::new();
    let physical_range = |address: &RegionedAbsoluteAddr| {
        let base = layout.regioned_static_byte_and_intra(address, 0)?.0;
        Some((base, layout.plane_size(&address.absolute_addr())))
    };

    for instruction in &block.instructions {
        match instruction {
            SIRInstruction::Load(_, address, offset, width)
                if *width <= 64
                    && matches!(offset, SIROffset::Dynamic(_) | SIROffset::Element { .. }) =>
            {
                *counts.entry(*address).or_default() += 1;
            }
            SIRInstruction::Store(address, ..) => {
                if let Some(range) = physical_range(address) {
                    written_ranges.push(range);
                }
            }
            SIRInstruction::Commit(_, destination, ..) => {
                if let Some(range) = physical_range(destination) {
                    written_ranges.push(range);
                }
            }
            _ => {}
        }
    }

    let addresses = counts
        .into_iter()
        .filter_map(|(address, count)| {
            if count < MIN_LOADS {
                return None;
            }
            let absolute = address.absolute_addr();
            let byte_size = layout.plane_size(&absolute);
            native_plane_access_size(byte_size)?;
            if layout.widths.get(&absolute).copied().unwrap_or(usize::MAX) > 64 {
                return None;
            }
            if layout.unpacked_arrays.contains_key(&absolute) {
                return None;
            }
            let (base, size) = physical_range(&address)?;
            let end = i64::from(base).checked_add(i64::try_from(size).ok()?)?;
            let overlaps_write = written_ranges.iter().any(|&(write_base, write_size)| {
                let write_end =
                    i64::from(write_base) + i64::try_from(write_size).unwrap_or(i64::MAX);
                i64::from(base) < write_end && i64::from(write_base) < end
            });
            (!overlaps_write).then_some(address)
        })
        .collect();
    BlockDynamicLoadCachePlans { addresses }
}
