//! Physical MIR memory-write effects shared by optimization and allocation.
//!
//! A pseudo instruction is not automatically an unknown SimState clobber.
//! Sparse operations carry the concrete metadata ranges they mutate; keeping
//! those ranges here lets every MemorySSA consumer use the same alias model.

use super::mir::{BaseReg, MInst};

const MAX_STATIC_RANGES: usize = 3;
const EMPTY_RANGE: MemoryRange = MemoryRange {
    base: BaseReg::SimState,
    offset: 0,
    byte_len: 0,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MemoryRange {
    pub base: BaseReg,
    pub offset: i64,
    pub byte_len: usize,
}

impl MemoryRange {
    pub(crate) fn end(self) -> Option<i64> {
        self.offset.checked_add(i64::try_from(self.byte_len).ok()?)
    }
}

/// `unknown == Some(None)` aliases every base; `Some(Some(base))` aliases only
/// that base. Static effects contain at most the three ranges required by a
/// sparse pseudo and allocate no temporary vector while scanning MIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MemoryWrites {
    ranges: [MemoryRange; MAX_STATIC_RANGES],
    range_count: u8,
    unknown: Option<Option<BaseReg>>,
}

impl MemoryWrites {
    const NONE: Self = Self {
        ranges: [EMPTY_RANGE; MAX_STATIC_RANGES],
        range_count: 0,
        unknown: None,
    };

    fn unknown(base: Option<BaseReg>) -> Self {
        Self {
            unknown: Some(base),
            ..Self::NONE
        }
    }

    fn static_ranges(ranges: &[MemoryRange]) -> Self {
        debug_assert!(ranges.len() <= MAX_STATIC_RANGES);
        let mut result = Self::NONE;
        for (destination, source) in result.ranges.iter_mut().zip(ranges) {
            *destination = *source;
        }
        result.range_count = ranges.len() as u8;
        result
    }

    pub(crate) fn ranges(&self) -> impl Iterator<Item = MemoryRange> + '_ {
        self.ranges[..usize::from(self.range_count)].iter().copied()
    }

    pub(crate) fn unknown_base(self) -> Option<Option<BaseReg>> {
        self.unknown
    }

    pub(crate) fn has_effect(self) -> bool {
        self.unknown.is_some() || self.range_count != 0
    }
}

fn checked_range(base: BaseReg, offset: i32, byte_len: usize) -> Option<MemoryRange> {
    let range = MemoryRange {
        base,
        offset: i64::from(offset),
        byte_len,
    };
    range.end().map(|_| range)
}

fn sparse_commit_ranges(inst: &MInst) -> Option<[MemoryRange; 3]> {
    let MInst::SparseCommit {
        dst_offset,
        byte_size,
        dirty_words_offset,
        dirty_word_count,
        summary_words_offset,
        summary_word_count,
        four_state,
        ..
    } = inst
    else {
        return None;
    };
    let planes = if *four_state { 2 } else { 1 };
    Some([
        checked_range(
            BaseReg::SimState,
            *dst_offset,
            byte_size.checked_mul(planes)?,
        )?,
        checked_range(
            BaseReg::SimState,
            *dirty_words_offset,
            dirty_word_count.checked_mul(8)?,
        )?,
        checked_range(
            BaseReg::SimState,
            *summary_words_offset,
            summary_word_count.checked_mul(8)?,
        )?,
    ])
}

fn sparse_mark_ranges(inst: &MInst) -> Option<[MemoryRange; 3]> {
    let MInst::SparseMarkActive {
        active_index,
        active_count_offset,
        active_flags_offset,
        active_list_offset,
        active_capacity,
    } = inst
    else {
        return None;
    };
    let flag_offset = i64::from(*active_flags_offset).checked_add(i64::from(*active_index))?;
    let flag_offset = i32::try_from(flag_offset).ok()?;
    Some([
        checked_range(BaseReg::SimState, *active_count_offset, 8)?,
        checked_range(BaseReg::SimState, flag_offset, 1)?,
        checked_range(
            BaseReg::SimState,
            *active_list_offset,
            active_capacity.checked_mul(4)?,
        )?,
    ])
}

pub(crate) fn writes(inst: &MInst) -> MemoryWrites {
    match inst {
        MInst::Store {
            base, offset, size, ..
        } => checked_range(*base, *offset, size.bytes() as usize)
            .map(|range| MemoryWrites::static_ranges(&[range]))
            .unwrap_or_else(|| MemoryWrites::unknown(Some(*base))),
        MInst::MemCopy {
            dst_offset,
            byte_len,
            ..
        } => checked_range(BaseReg::SimState, *dst_offset, *byte_len)
            .map(|range| MemoryWrites::static_ranges(&[range]))
            .unwrap_or_else(|| MemoryWrites::unknown(Some(BaseReg::SimState))),
        MInst::SparseCommit { .. } => sparse_commit_ranges(inst)
            .map(|ranges| MemoryWrites::static_ranges(&ranges))
            .unwrap_or_else(|| MemoryWrites::unknown(Some(BaseReg::SimState))),
        MInst::SparseMarkActive { .. } => sparse_mark_ranges(inst)
            .map(|ranges| MemoryWrites::static_ranges(&ranges))
            .unwrap_or_else(|| MemoryWrites::unknown(Some(BaseReg::SimState))),
        // Descriptor rows name several sparse regions which are not carried by
        // this MIR instruction. Keep this one conservative until the table is
        // part of the shared effect model.
        MInst::SparseCommitWorklist { .. } => MemoryWrites::unknown(Some(BaseReg::SimState)),
        MInst::StoreIndexed {
            base, alias_range, ..
        } => alias_range
            .and_then(|range| checked_range(*base, range.offset(), range.byte_len()))
            .map(|range| MemoryWrites::static_ranges(&[range]))
            .unwrap_or_else(|| MemoryWrites::unknown(Some(*base))),
        MInst::StorePtr { .. }
        | MInst::ReleaseStorePtr { .. }
        | MInst::StorePtrIndexed { .. }
        | MInst::ReleaseStorePtrIndexed { .. } => MemoryWrites::unknown(None),
        _ => MemoryWrites::NONE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{MemoryAliasRange, OpSize, VReg};

    #[test]
    fn sparse_mark_active_has_only_metadata_write_ranges() {
        let inst = MInst::SparseMarkActive {
            active_index: 3,
            active_count_offset: 100,
            active_flags_offset: 200,
            active_list_offset: 300,
            active_capacity: 16,
        };
        let effect = writes(&inst);

        assert_eq!(effect.unknown_base(), None);
        assert_eq!(
            effect.ranges().collect::<Vec<_>>(),
            vec![
                MemoryRange {
                    base: BaseReg::SimState,
                    offset: 100,
                    byte_len: 8,
                },
                MemoryRange {
                    base: BaseReg::SimState,
                    offset: 203,
                    byte_len: 1,
                },
                MemoryRange {
                    base: BaseReg::SimState,
                    offset: 300,
                    byte_len: 64,
                },
            ]
        );
    }

    #[test]
    fn sparse_commit_covers_destination_and_bitmap_clears() {
        let inst = MInst::SparseCommit {
            src_offset: 0,
            dst_offset: 100,
            byte_size: 17,
            dirty_words_offset: 200,
            dirty_word_count: 2,
            summary_words_offset: 300,
            summary_word_count: 1,
            four_state: true,
        };

        assert_eq!(
            writes(&inst).ranges().collect::<Vec<_>>(),
            vec![
                MemoryRange {
                    base: BaseReg::SimState,
                    offset: 100,
                    byte_len: 34,
                },
                MemoryRange {
                    base: BaseReg::SimState,
                    offset: 200,
                    byte_len: 16,
                },
                MemoryRange {
                    base: BaseReg::SimState,
                    offset: 300,
                    byte_len: 8,
                },
            ]
        );
    }

    #[test]
    fn bounded_indexed_store_uses_its_semantic_alias_envelope() {
        let inst = MInst::StoreIndexed {
            base: BaseReg::SimState,
            offset: 120,
            index: VReg(0),
            src: VReg(1),
            size: OpSize::S64,
            alias_range: MemoryAliasRange::new(100, 64),
        };

        assert_eq!(
            writes(&inst).ranges().collect::<Vec<_>>(),
            vec![MemoryRange {
                base: BaseReg::SimState,
                offset: 100,
                byte_len: 64,
            }]
        );
        assert_eq!(writes(&inst).unknown_base(), None);
    }
}
