//! AArch64-owned sparse state planning used during instruction selection.

use celox_sir::analysis::reverse_postorder;

use crate::mir::SparseCommitDescriptor;
use crate::{
    ExecutionUnit, HashSet, MemoryLayout, RegionedAbsoluteAddr, SIRInstruction,
    SPARSE_WORKING_REGION, STABLE_REGION,
};

pub(super) fn find_sparse_worklist_run(
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> Option<(crate::BlockId, usize, usize)> {
    let stored = unit
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            SIRInstruction::Store(address, ..) if address.region == SPARSE_WORKING_REGION => {
                Some(address.absolute_addr())
            }
            _ => None,
        })
        .collect::<HashSet<_>>();
    if stored.is_empty() {
        return None;
    }

    for block_id in reverse_postorder(unit) {
        let block = &unit.blocks[&block_id];
        let mut start = 0usize;
        while start < block.instructions.len() {
            let is_sparse_commit = |instruction: &SIRInstruction<RegionedAbsoluteAddr>| {
                matches!(
                    instruction,
                    SIRInstruction::Commit(source, destination, ..)
                        if source.region == SPARSE_WORKING_REGION
                            && destination.region == STABLE_REGION
                )
            };
            if !is_sparse_commit(&block.instructions[start]) {
                start += 1;
                continue;
            }
            let mut end = start + 1;
            while end < block.instructions.len() && is_sparse_commit(&block.instructions[end]) {
                end += 1;
            }
            let committed = block.instructions[start..end]
                .iter()
                .filter_map(|instruction| match instruction {
                    SIRInstruction::Commit(source, ..) => Some(source.absolute_addr()),
                    _ => None,
                })
                .collect::<HashSet<_>>();
            if stored.is_subset(&committed) {
                return Some((block_id, start, end));
            }
            start = end;
        }
    }
    None
}

pub(super) fn sparse_descriptor_table(layout: &MemoryLayout) -> Vec<u64> {
    let mut rows = layout.sparse_layouts.iter().collect::<Vec<_>>();
    rows.sort_by_key(|(_, sparse)| sparse.active_index);
    let mut table = Vec::with_capacity(rows.len() * SparseCommitDescriptor::WORDS);
    for (address, sparse) in rows {
        let descriptor = SparseCommitDescriptor {
            src_offset: (layout.sparse_base_offset + layout.sparse_offsets[address]) as u64,
            dst_offset: layout.offsets[address] as u64,
            byte_size: layout.plane_size(address) as u64,
            dirty_words_offset: sparse.dirty_words_offset as u64,
            dirty_word_count: sparse.dirty_word_count as u64,
            summary_words_offset: sparse.summary_words_offset as u64,
            summary_word_count: sparse.summary_word_count as u64,
            four_state: u64::from(layout.four_state && layout.is_4states[address]),
        };
        table.extend(descriptor.words());
    }
    table
}
