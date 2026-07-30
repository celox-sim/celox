//! Physical MIR memory effects shared by optimization and allocation.
//!
//! A pseudo instruction is not automatically an unknown SimState clobber.
//! Sparse operations carry the concrete metadata ranges they mutate; keeping
//! those ranges here lets every MemorySSA consumer use the same alias model.

use celox_analysis::memory::{MemoryEffect, MemoryLocation};

use super::mir::{BaseReg, BranchPredicate, MInst};

// Lane-aggregate pseudos name each independently versioned source range. This
// remains an inline, allocation-free value; Heliodor currently needs nine.
const MAX_STATIC_RANGES: usize = 16;
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

/// Memory domain for a write whose concrete byte range is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnknownMemory {
    /// May alias the whole direct-addressed base.
    Direct(BaseReg),
    /// Runtime-owned pointer memory, disjoint from SimState and StackFrame.
    Indirect,
}

/// IR-independent object identity used by shared memory analyses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum MemoryObject {
    SimState,
    StackFrame,
    Indirect,
}

impl MemoryObject {
    pub(crate) fn direct(base: BaseReg) -> Self {
        match base {
            BaseReg::SimState => Self::SimState,
            BaseReg::StackFrame => Self::StackFrame,
        }
    }
}

/// Static effects contain at most the three ranges required by a sparse
/// pseudo and allocate no temporary vector while scanning MIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MemoryEffects {
    ranges: [MemoryRange; MAX_STATIC_RANGES],
    range_count: u8,
    unknown: Option<UnknownMemory>,
}

impl MemoryEffects {
    const NONE: Self = Self {
        ranges: [EMPTY_RANGE; MAX_STATIC_RANGES],
        range_count: 0,
        unknown: None,
    };

    fn unknown(memory: UnknownMemory) -> Self {
        Self {
            unknown: Some(memory),
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

    pub(crate) fn unknown_memory(self) -> Option<UnknownMemory> {
        self.unknown
    }

    pub(crate) fn has_effect(self) -> bool {
        self.unknown.is_some() || self.range_count != 0
    }
}

fn direct_alias_ranges(base: BaseReg, aliases: &[super::mir::MemoryAliasRange]) -> MemoryEffects {
    if aliases.len() > MAX_STATIC_RANGES {
        return MemoryEffects::unknown(UnknownMemory::Direct(base));
    }
    let mut result = MemoryEffects::NONE;
    for (index, alias) in aliases.iter().enumerate() {
        let Some(range) = checked_range(base, alias.offset(), alias.byte_len()) else {
            return MemoryEffects::unknown(UnknownMemory::Direct(base));
        };
        result.ranges[index] = range;
    }
    result.range_count = aliases.len() as u8;
    result
}

/// Translate the compact MIR effect record without coupling celox-analysis to
/// MIR types. The iterator contains at most three exact ranges and one unknown
/// object and performs no allocation.
pub(crate) fn analysis_effects(
    effects: &MemoryEffects,
) -> impl Iterator<Item = MemoryEffect<MemoryObject>> + '_ {
    effects
        .ranges()
        .map(|range| {
            MemoryEffect::Exact(MemoryLocation {
                object: MemoryObject::direct(range.base),
                offset: range.offset,
                byte_len: range.byte_len,
            })
        })
        .chain(effects.unknown_memory().map(|unknown| {
            MemoryEffect::UnknownObject(match unknown {
                UnknownMemory::Direct(base) => MemoryObject::direct(base),
                UnknownMemory::Indirect => MemoryObject::Indirect,
            })
        }))
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

fn sparse_commit_read_ranges(inst: &MInst) -> Option<[MemoryRange; 3]> {
    let MInst::SparseCommit {
        src_offset,
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
            *src_offset,
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

fn sparse_mark_ranges(inst: &MInst) -> Option<[MemoryRange; 1]> {
    let MInst::SparseMarkActive {
        active_index,
        active_bits_offset,
        active_capacity,
        ..
    } = inst
    else {
        return None;
    };
    if *active_capacity == 0 || *active_index as usize >= *active_capacity {
        return None;
    }
    let word = usize::try_from(*active_index).ok()? / 64;
    let byte_offset = word.checked_mul(8)?;
    let offset = i64::from(*active_bits_offset).checked_add(i64::try_from(byte_offset).ok()?)?;
    Some([checked_range(
        BaseReg::SimState,
        i32::try_from(offset).ok()?,
        8,
    )?])
}

pub(crate) fn reads(inst: &MInst) -> MemoryEffects {
    match inst {
        MInst::Load {
            base, offset, size, ..
        }
        | MInst::AndStoreImm {
            base, offset, size, ..
        }
        | MInst::OrStoreImm {
            base, offset, size, ..
        }
        | MInst::BranchPred {
            predicate: BranchPredicate::MemoryNonZero { base, offset, size },
            ..
        } => checked_range(*base, *offset, size.bytes() as usize)
            .map(|range| MemoryEffects::static_ranges(&[range]))
            .unwrap_or_else(|| MemoryEffects::unknown(UnknownMemory::Direct(*base))),
        MInst::LoadIndexed {
            base, alias_range, ..
        } => alias_range
            .and_then(|range| checked_range(*base, range.offset(), range.byte_len()))
            .map(|range| MemoryEffects::static_ranges(&[range]))
            .unwrap_or_else(|| MemoryEffects::unknown(UnknownMemory::Direct(*base))),
        MInst::PackedLaneCompare {
            alias_range, rhs, ..
        } => {
            let lhs = alias_range.and_then(|range| {
                checked_range(BaseReg::SimState, range.offset(), range.byte_len())
            });
            match rhs {
                super::mir::PackedLaneCompareRhs::Scalar(_) => lhs
                    .map(|lhs| MemoryEffects::static_ranges(&[lhs]))
                    .unwrap_or_else(|| {
                        MemoryEffects::unknown(UnknownMemory::Direct(BaseReg::SimState))
                    }),
                super::mir::PackedLaneCompareRhs::Memory { alias_range, .. } => {
                    let rhs = alias_range.and_then(|range| {
                        checked_range(BaseReg::SimState, range.offset(), range.byte_len())
                    });
                    match (lhs, rhs) {
                        (Some(lhs), Some(rhs)) => MemoryEffects::static_ranges(&[lhs, rhs]),
                        _ => MemoryEffects::unknown(UnknownMemory::Direct(BaseReg::SimState)),
                    }
                }
            }
        }
        MInst::LaneAggregate { read_ranges, .. } => {
            direct_alias_ranges(BaseReg::SimState, read_ranges)
        }
        MInst::OrStoreIndexed {
            base, alias_range, ..
        } => alias_range
            .and_then(|range| checked_range(*base, range.offset(), range.byte_len()))
            .map(|range| MemoryEffects::static_ranges(&[range]))
            .unwrap_or_else(|| MemoryEffects::unknown(UnknownMemory::Direct(*base))),
        MInst::LoadPtr { .. } | MInst::LoadPtrIndexed { .. } => {
            MemoryEffects::unknown(UnknownMemory::Indirect)
        }
        MInst::MemCopy {
            src_offset,
            byte_len,
            ..
        } => checked_range(BaseReg::SimState, *src_offset, *byte_len)
            .map(|range| MemoryEffects::static_ranges(&[range]))
            .unwrap_or_else(|| MemoryEffects::unknown(UnknownMemory::Direct(BaseReg::SimState))),
        MInst::MemFill { .. } => MemoryEffects::NONE,
        MInst::SparseCommit { .. } => sparse_commit_read_ranges(inst)
            .map(|ranges| MemoryEffects::static_ranges(&ranges))
            .unwrap_or_else(|| MemoryEffects::unknown(UnknownMemory::Direct(BaseReg::SimState))),
        // Both sparse pseudos perform read-modify-write operations over their
        // metadata. The worklist descriptor does not carry every concrete
        // region, so its SimState read remains conservatively unknown.
        MInst::SparseMarkActive { .. } => sparse_mark_ranges(inst)
            .map(|ranges| MemoryEffects::static_ranges(&ranges))
            .unwrap_or_else(|| MemoryEffects::unknown(UnknownMemory::Direct(BaseReg::SimState))),
        MInst::SparseCommitWorklist { .. } => {
            MemoryEffects::unknown(UnknownMemory::Direct(BaseReg::SimState))
        }
        _ => MemoryEffects::NONE,
    }
}

pub(crate) fn writes(inst: &MInst) -> MemoryEffects {
    match inst {
        MInst::Store {
            base, offset, size, ..
        }
        | MInst::AndStoreImm {
            base, offset, size, ..
        }
        | MInst::OrStoreImm {
            base, offset, size, ..
        } => checked_range(*base, *offset, size.bytes() as usize)
            .map(|range| MemoryEffects::static_ranges(&[range]))
            .unwrap_or_else(|| MemoryEffects::unknown(UnknownMemory::Direct(*base))),
        MInst::MemCopy {
            dst_offset,
            byte_len,
            ..
        } => checked_range(BaseReg::SimState, *dst_offset, *byte_len)
            .map(|range| MemoryEffects::static_ranges(&[range]))
            .unwrap_or_else(|| MemoryEffects::unknown(UnknownMemory::Direct(BaseReg::SimState))),
        MInst::MemFill {
            dst_offset,
            byte_len,
            ..
        } => checked_range(BaseReg::SimState, *dst_offset, *byte_len)
            .map(|range| MemoryEffects::static_ranges(&[range]))
            .unwrap_or_else(|| MemoryEffects::unknown(UnknownMemory::Direct(BaseReg::SimState))),
        MInst::LaneAggregate { write_ranges, .. } => {
            direct_alias_ranges(BaseReg::SimState, write_ranges)
        }
        MInst::SparseCommit { .. } => sparse_commit_ranges(inst)
            .map(|ranges| MemoryEffects::static_ranges(&ranges))
            .unwrap_or_else(|| MemoryEffects::unknown(UnknownMemory::Direct(BaseReg::SimState))),
        MInst::SparseMarkActive { .. } => sparse_mark_ranges(inst)
            .map(|ranges| MemoryEffects::static_ranges(&ranges))
            .unwrap_or_else(|| MemoryEffects::unknown(UnknownMemory::Direct(BaseReg::SimState))),
        // Descriptor rows name several sparse regions which are not carried by
        // this MIR instruction. Keep this one conservative until the table is
        // part of the shared effect model.
        MInst::SparseCommitWorklist { .. } => {
            MemoryEffects::unknown(UnknownMemory::Direct(BaseReg::SimState))
        }
        MInst::StoreIndexed {
            base, alias_range, ..
        }
        | MInst::OrStoreIndexed {
            base, alias_range, ..
        } => alias_range
            .and_then(|range| checked_range(*base, range.offset(), range.byte_len()))
            .map(|range| MemoryEffects::static_ranges(&[range]))
            .unwrap_or_else(|| MemoryEffects::unknown(UnknownMemory::Direct(*base))),
        MInst::StorePtr { .. }
        | MInst::ReleaseStorePtr { .. }
        | MInst::StorePtrIndexed { .. }
        | MInst::ReleaseStorePtrIndexed { .. } => MemoryEffects::unknown(UnknownMemory::Indirect),
        _ => MemoryEffects::NONE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{
        BlockId, BranchPredicate, CmpKind, LaneAggregatePlanId, MemoryAliasRange, OpSize,
        PackedLaneCompareRhs, VReg,
    };

    #[test]
    fn memory_branch_keeps_the_folded_load_effect() {
        let instruction = MInst::BranchPred {
            predicate: BranchPredicate::MemoryNonZero {
                base: BaseReg::SimState,
                offset: 12,
                size: OpSize::S16,
            },
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        };

        assert_eq!(
            reads(&instruction).ranges().collect::<Vec<_>>(),
            vec![MemoryRange {
                base: BaseReg::SimState,
                offset: 12,
                byte_len: 2,
            }]
        );
        assert!(!writes(&instruction).has_effect());
    }

    #[test]
    fn packed_memory_compare_reads_both_ranges_and_rejects_a_missing_bound() {
        let mut inst = MInst::PackedLaneCompare {
            dst: VReg(0),
            rhs: PackedLaneCompareRhs::Memory {
                offset: 200,
                alias_range: MemoryAliasRange::new(200, 16),
            },
            kind: CmpKind::LtU,
            offset: 100,
            lane_count: 16,
            element_stride: 1,
            bit_offset: 0,
            field_width: 8,
            alias_range: MemoryAliasRange::new(100, 16),
        };
        assert_eq!(
            reads(&inst).ranges().collect::<Vec<_>>(),
            vec![
                MemoryRange {
                    base: BaseReg::SimState,
                    offset: 100,
                    byte_len: 16,
                },
                MemoryRange {
                    base: BaseReg::SimState,
                    offset: 200,
                    byte_len: 16,
                },
            ]
        );

        let MInst::PackedLaneCompare {
            rhs: PackedLaneCompareRhs::Memory { alias_range, .. },
            ..
        } = &mut inst
        else {
            unreachable!()
        };
        *alias_range = None;
        assert_eq!(
            reads(&inst).unknown_memory(),
            Some(UnknownMemory::Direct(BaseReg::SimState))
        );
    }

    #[test]
    fn lane_aggregate_preserves_all_exact_read_and_write_ranges() {
        let read_ranges = (0..9)
            .map(|index| MemoryAliasRange::new(index * 16, 8).unwrap())
            .collect::<Vec<_>>();
        let write_ranges = vec![MemoryAliasRange::new(256, 32).unwrap()];
        let inst = MInst::LaneAggregate {
            dst: VReg(0),
            plan: LaneAggregatePlanId(0),
            root: 0,
            source_block: crate::ir::BlockId(0),
            inputs: Vec::new(),
            captured_inputs: 0,
            input_bytes: 0,
            input_base_offset: 0,
            read_ranges: read_ranges.clone(),
            write_ranges,
        };

        assert_eq!(
            reads(&inst)
                .ranges()
                .map(|range| (range.offset, range.byte_len))
                .collect::<Vec<_>>(),
            read_ranges
                .iter()
                .map(|range| (i64::from(range.offset()), range.byte_len()))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            writes(&inst)
                .ranges()
                .map(|range| (range.offset, range.byte_len))
                .collect::<Vec<_>>(),
            vec![(256, 32)]
        );
    }

    #[test]
    fn sparse_mark_active_has_only_metadata_write_ranges() {
        let inst = MInst::SparseMarkActive {
            active_index: 3,
            active_bits_offset: 200,
            active_capacity: 16,
        };
        let effect = writes(&inst);

        assert_eq!(effect.unknown_memory(), None);
        assert_eq!(
            effect.ranges().collect::<Vec<_>>(),
            vec![MemoryRange {
                base: BaseReg::SimState,
                offset: 200,
                byte_len: 8,
            }]
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
        assert_eq!(
            reads(&inst).ranges().collect::<Vec<_>>(),
            vec![
                MemoryRange {
                    base: BaseReg::SimState,
                    offset: 0,
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
    fn sparse_mark_active_reads_the_metadata_it_updates() {
        let inst = MInst::SparseMarkActive {
            active_index: 3,
            active_bits_offset: 200,
            active_capacity: 16,
        };

        assert_eq!(
            reads(&inst).ranges().collect::<Vec<_>>(),
            writes(&inst).ranges().collect::<Vec<_>>()
        );
    }

    #[test]
    fn memcopy_separates_source_reads_from_destination_writes() {
        let inst = MInst::MemCopy {
            src_offset: 100,
            dst_offset: 1000,
            byte_len: 4096,
        };

        assert_eq!(
            reads(&inst).ranges().collect::<Vec<_>>(),
            vec![MemoryRange {
                base: BaseReg::SimState,
                offset: 100,
                byte_len: 4096,
            }]
        );
        assert_eq!(
            writes(&inst).ranges().collect::<Vec<_>>(),
            vec![MemoryRange {
                base: BaseReg::SimState,
                offset: 1000,
                byte_len: 4096,
            }]
        );
    }

    #[test]
    fn memfill_has_one_exact_write_and_no_read() {
        let inst = MInst::MemFill {
            dst_offset: 37,
            byte_len: 23,
            value: 0x5a,
        };

        assert!(!reads(&inst).has_effect());
        assert_eq!(
            writes(&inst).ranges().collect::<Vec<_>>(),
            vec![MemoryRange {
                base: BaseReg::SimState,
                offset: 37,
                byte_len: 23,
            }]
        );
        assert_eq!(writes(&inst).unknown_memory(), None);
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
        assert_eq!(writes(&inst).unknown_memory(), None);
    }

    #[test]
    fn bounded_indexed_load_uses_its_semantic_alias_envelope() {
        let inst = MInst::LoadIndexed {
            dst: VReg(1),
            base: BaseReg::SimState,
            offset: 120,
            index: VReg(0),
            scale: 1,
            size: OpSize::S64,
            alias_range: MemoryAliasRange::new(100, 64),
        };

        assert_eq!(
            reads(&inst).ranges().collect::<Vec<_>>(),
            vec![MemoryRange {
                base: BaseReg::SimState,
                offset: 100,
                byte_len: 64,
            }]
        );
        assert_eq!(reads(&inst).unknown_memory(), None);
    }

    #[test]
    fn bounded_indexed_or_store_is_both_a_read_and_a_write() {
        let inst = MInst::OrStoreIndexed {
            base: BaseReg::SimState,
            offset: 120,
            index: VReg(0),
            src: VReg(1),
            size: OpSize::S64,
            alias_range: MemoryAliasRange::new(100, 64),
        };
        let expected = vec![MemoryRange {
            base: BaseReg::SimState,
            offset: 100,
            byte_len: 64,
        }];

        assert_eq!(reads(&inst).ranges().collect::<Vec<_>>(), expected);
        assert_eq!(writes(&inst).ranges().collect::<Vec<_>>(), expected);
        assert_eq!(reads(&inst).unknown_memory(), None);
        assert_eq!(writes(&inst).unknown_memory(), None);
    }

    #[test]
    fn pointer_store_is_an_indirect_memory_effect() {
        let inst = MInst::StorePtr {
            ptr: VReg(0),
            offset: 0,
            src: VReg(1),
            size: OpSize::S64,
        };

        let effect = writes(&inst);
        assert!(effect.has_effect());
        assert_eq!(effect.unknown_memory(), Some(UnknownMemory::Indirect));
        assert!(effect.ranges().next().is_none());
    }
}
