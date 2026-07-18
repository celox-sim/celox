//! Sparse physical-register interval unions.
//!
//! The replacement allocator never linearizes the CFG into one artificial
//! instruction interval. Each physical register owns one ordered segment map
//! per MIR block. Interference, insertion, removal, and free-region queries
//! therefore preserve mutual exclusion between CFG arms while remaining
//! logarithmic in the number of segments in the affected blocks.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::ops::Bound::{Excluded, Unbounded};

use crate::backend::native::mir::BlockId;

use super::assignment::PhysReg;
use super::cfg::NormalizedCfg;
use super::live_interval::{LiveSegment, SlotIndex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct AllocationBundleId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UnionEntry {
    end: SlotIndex,
    bundle: AllocationBundleId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct IntervalUnionError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub bundles: Vec<AllocationBundleId>,
    pub message: String,
}

impl IntervalUnionError {
    fn new(
        rule: &'static str,
        block: Option<BlockId>,
        bundles: impl IntoIterator<Item = AllocationBundleId>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            block,
            bundles: bundles.into_iter().collect(),
            message: message.into(),
        }
    }
}

impl fmt::Display for IntervalUnionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.rule)?;
        if let Some(block) = self.block {
            write!(formatter, " at {block}")?;
        }
        if !self.bundles.is_empty() {
            write!(formatter, " bundles={:?}", self.bundles)?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for IntervalUnionError {}

/// All assigned sparse intervals for one physical register.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IntervalUnion {
    block_index: HashMap<BlockId, usize>,
    block_ids: Vec<BlockId>,
    blocks: Vec<BTreeMap<SlotIndex, UnionEntry>>,
    memberships: BTreeMap<AllocationBundleId, Vec<LiveSegment>>,
}

impl IntervalUnion {
    fn new(cfg: &NormalizedCfg) -> Result<Self, IntervalUnionError> {
        let block_count = cfg.successors.len();
        if cfg.block_index.len() != block_count {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.CFG_SHAPE",
                None,
                [],
                "block-index table and CFG row count differ",
            ));
        }
        let mut block_ids = vec![None; block_count];
        for (&block, &index) in &cfg.block_index {
            let Some(slot) = block_ids.get_mut(index) else {
                return Err(IntervalUnionError::new(
                    "INTERVAL_UNION.CFG_SHAPE",
                    Some(block),
                    [],
                    "block index is outside the CFG",
                ));
            };
            if slot.replace(block).is_some() {
                return Err(IntervalUnionError::new(
                    "INTERVAL_UNION.CFG_SHAPE",
                    Some(block),
                    [],
                    "two MIR blocks share one CFG index",
                ));
            }
        }
        let block_ids = block_ids
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                IntervalUnionError::new(
                    "INTERVAL_UNION.CFG_SHAPE",
                    None,
                    [],
                    "CFG index does not name every block row",
                )
            })?;
        Ok(Self {
            block_index: cfg.block_index.clone(),
            block_ids,
            blocks: (0..block_count).map(|_| BTreeMap::new()).collect(),
            memberships: BTreeMap::new(),
        })
    }

    fn validate_segments(&self, segments: &[LiveSegment]) -> Result<(), IntervalUnionError> {
        let mut previous = None::<LiveSegment>;
        for &segment in segments {
            if segment.start >= segment.end {
                return Err(IntervalUnionError::new(
                    "INTERVAL_UNION.SEGMENT_RANGE",
                    Some(segment.block),
                    [],
                    format!(
                        "segment {:?}..{:?} is empty or reversed",
                        segment.start, segment.end
                    ),
                ));
            }
            if !self.block_index.contains_key(&segment.block) {
                return Err(IntervalUnionError::new(
                    "INTERVAL_UNION.SEGMENT_BLOCK",
                    Some(segment.block),
                    [],
                    "segment references a block outside the normalized CFG",
                ));
            }
            if let Some(prior) = previous {
                if (segment.block, segment.start) <= (prior.block, prior.start) {
                    return Err(IntervalUnionError::new(
                        "INTERVAL_UNION.SEGMENT_ORDER",
                        Some(segment.block),
                        [],
                        "bundle segments are not in strict block/slot order",
                    ));
                }
                if prior.overlaps(segment) {
                    return Err(IntervalUnionError::new(
                        "INTERVAL_UNION.SELF_INTERFERENCE",
                        Some(segment.block),
                        [],
                        "one bundle contains overlapping sparse segments",
                    ));
                }
            }
            previous = Some(segment);
        }
        Ok(())
    }

    fn overlapping_entries(
        &self,
        segment: LiveSegment,
    ) -> Result<Vec<(SlotIndex, UnionEntry)>, IntervalUnionError> {
        let Some(&block) = self.block_index.get(&segment.block) else {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.SEGMENT_BLOCK",
                Some(segment.block),
                [],
                "segment references a block outside the normalized CFG",
            ));
        };
        let mut overlaps = Vec::new();
        for (&start, &entry) in self.blocks[block]
            .range((Unbounded, Excluded(segment.end)))
            .rev()
        {
            if entry.end <= segment.start {
                break;
            }
            overlaps.push((start, entry));
        }
        overlaps.reverse();
        Ok(overlaps)
    }

    fn conflicts(
        &self,
        segments: &[LiveSegment],
    ) -> Result<Vec<AllocationBundleId>, IntervalUnionError> {
        self.validate_segments(segments)?;
        let mut conflicts = BTreeSet::new();
        for &segment in segments {
            conflicts.extend(
                self.overlapping_entries(segment)?
                    .into_iter()
                    .map(|(_, entry)| entry.bundle),
            );
        }
        Ok(conflicts.into_iter().collect())
    }

    fn insert(
        &mut self,
        bundle: AllocationBundleId,
        segments: &[LiveSegment],
    ) -> Result<(), IntervalUnionError> {
        self.validate_segments(segments)?;
        if self.memberships.contains_key(&bundle) {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.DUPLICATE_BUNDLE",
                None,
                [bundle],
                "bundle is already present in this physical register",
            ));
        }
        let conflicts = self.conflicts(segments)?;
        if !conflicts.is_empty() {
            let block = segments.iter().find_map(|segment| {
                self.overlapping_entries(*segment)
                    .ok()
                    .and_then(|entries| (!entries.is_empty()).then_some(segment.block))
            });
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.INTERFERENCE",
                block,
                std::iter::once(bundle).chain(conflicts),
                "cannot assign overlapping live bundles to one physical register",
            ));
        }

        for &segment in segments {
            let block = self.block_index[&segment.block];
            let previous = self.blocks[block].insert(
                segment.start,
                UnionEntry {
                    end: segment.end,
                    bundle,
                },
            );
            debug_assert!(previous.is_none());
        }
        self.memberships.insert(bundle, segments.to_vec());
        Ok(())
    }

    fn remove(
        &mut self,
        bundle: AllocationBundleId,
    ) -> Result<Vec<LiveSegment>, IntervalUnionError> {
        let Some(segments) = self.memberships.get(&bundle) else {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.MISSING_BUNDLE",
                None,
                [bundle],
                "cannot remove a bundle which is not assigned to this register",
            ));
        };
        for &segment in segments {
            let block = self.block_index[&segment.block];
            if self.blocks[block].get(&segment.start)
                != Some(&UnionEntry {
                    end: segment.end,
                    bundle,
                })
            {
                return Err(IntervalUnionError::new(
                    "INTERVAL_UNION.MEMBERSHIP",
                    Some(segment.block),
                    [bundle],
                    "bundle membership and ordered segment table disagree",
                ));
            }
        }
        let segments = self
            .memberships
            .remove(&bundle)
            .expect("membership was checked above");
        for &segment in &segments {
            let block = self.block_index[&segment.block];
            self.blocks[block].remove(&segment.start);
        }
        Ok(segments)
    }

    /// Subtract every occupied segment from the input, preserving sparse CFG
    /// block identity. These are the maximal regions available for splitting
    /// a bundle onto this register.
    fn free_segments(
        &self,
        segments: &[LiveSegment],
    ) -> Result<Vec<LiveSegment>, IntervalUnionError> {
        self.validate_segments(segments)?;
        let mut free = Vec::new();
        for &segment in segments {
            let mut cursor = segment.start;
            for (occupied_start, entry) in self.overlapping_entries(segment)? {
                let occupied_start = occupied_start.max(segment.start);
                let occupied_end = entry.end.min(segment.end);
                if cursor < occupied_start {
                    free.push(LiveSegment {
                        block: segment.block,
                        start: cursor,
                        end: occupied_start,
                    });
                }
                cursor = cursor.max(occupied_end);
            }
            if cursor < segment.end {
                free.push(LiveSegment {
                    block: segment.block,
                    start: cursor,
                    end: segment.end,
                });
            }
        }
        Ok(free)
    }

    fn verify(&self) -> Result<(), IntervalUnionError> {
        if self.blocks.len() != self.block_ids.len()
            || self.block_index.len() != self.block_ids.len()
            || self
                .block_ids
                .iter()
                .enumerate()
                .any(|(index, block)| self.block_index.get(block) != Some(&index))
        {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.CFG_SHAPE",
                None,
                [],
                "union block tables are not a bijection",
            ));
        }

        let mut expected = (0..self.blocks.len())
            .map(|_| BTreeMap::<SlotIndex, UnionEntry>::new())
            .collect::<Vec<_>>();
        for (&bundle, segments) in &self.memberships {
            self.validate_segments(segments)?;
            for &segment in segments {
                let block = self.block_index[&segment.block];
                if expected[block]
                    .insert(
                        segment.start,
                        UnionEntry {
                            end: segment.end,
                            bundle,
                        },
                    )
                    .is_some()
                {
                    return Err(IntervalUnionError::new(
                        "INTERVAL_UNION.INTERFERENCE",
                        Some(segment.block),
                        [bundle],
                        "two memberships begin at one register/slot",
                    ));
                }
            }
        }
        for (block, entries) in expected.iter().enumerate() {
            let mut prior = None::<(SlotIndex, UnionEntry)>;
            for (&start, &entry) in entries {
                if let Some((_, previous)) = prior
                    && previous.end > start
                {
                    return Err(IntervalUnionError::new(
                        "INTERVAL_UNION.INTERFERENCE",
                        Some(self.block_ids[block]),
                        [previous.bundle, entry.bundle],
                        "independent union verification found overlapping memberships",
                    ));
                }
                prior = Some((start, entry));
            }
        }
        if self.blocks != expected {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.MEMBERSHIP",
                None,
                [],
                "ordered segment tables differ from independently rebuilt memberships",
            ));
        }
        Ok(())
    }
}

/// Bidirectional physical-register interference matrix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LiveIntervalMatrix {
    unions: BTreeMap<PhysReg, IntervalUnion>,
    assignments: BTreeMap<AllocationBundleId, PhysReg>,
}

impl LiveIntervalMatrix {
    pub(super) fn new(
        cfg: &NormalizedCfg,
        registers: &[PhysReg],
    ) -> Result<Self, IntervalUnionError> {
        let mut unions = BTreeMap::new();
        for &register in registers {
            if unions.insert(register, IntervalUnion::new(cfg)?).is_some() {
                return Err(IntervalUnionError::new(
                    "INTERVAL_UNION.DUPLICATE_REGISTER",
                    None,
                    [],
                    format!("physical register {register} appears more than once"),
                ));
            }
        }
        if unions.is_empty() {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.EMPTY_REGISTER_SET",
                None,
                [],
                "allocator requires at least one physical register",
            ));
        }
        Ok(Self {
            unions,
            assignments: BTreeMap::new(),
        })
    }

    fn union(&self, register: PhysReg) -> Result<&IntervalUnion, IntervalUnionError> {
        self.unions.get(&register).ok_or_else(|| {
            IntervalUnionError::new(
                "INTERVAL_UNION.UNKNOWN_REGISTER",
                None,
                [],
                format!("physical register {register} is outside this matrix"),
            )
        })
    }

    fn union_mut(&mut self, register: PhysReg) -> Result<&mut IntervalUnion, IntervalUnionError> {
        self.unions.get_mut(&register).ok_or_else(|| {
            IntervalUnionError::new(
                "INTERVAL_UNION.UNKNOWN_REGISTER",
                None,
                [],
                format!("physical register {register} is outside this matrix"),
            )
        })
    }

    pub(super) fn registers(&self) -> impl Iterator<Item = PhysReg> + '_ {
        self.unions.keys().copied()
    }

    pub(super) fn register(&self, bundle: AllocationBundleId) -> Option<PhysReg> {
        self.assignments.get(&bundle).copied()
    }

    pub(super) fn conflicts(
        &self,
        register: PhysReg,
        segments: &[LiveSegment],
    ) -> Result<Vec<AllocationBundleId>, IntervalUnionError> {
        self.union(register)?.conflicts(segments)
    }

    pub(super) fn free_segments(
        &self,
        register: PhysReg,
        segments: &[LiveSegment],
    ) -> Result<Vec<LiveSegment>, IntervalUnionError> {
        self.union(register)?.free_segments(segments)
    }

    pub(super) fn assign(
        &mut self,
        bundle: AllocationBundleId,
        register: PhysReg,
        segments: &[LiveSegment],
    ) -> Result<(), IntervalUnionError> {
        if let Some(current) = self.register(bundle) {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.DUPLICATE_ASSIGNMENT",
                None,
                [bundle],
                format!("bundle is already assigned to {current}"),
            ));
        }
        self.union_mut(register)?.insert(bundle, segments)?;
        self.assignments.insert(bundle, register);
        Ok(())
    }

    pub(super) fn unassign(
        &mut self,
        bundle: AllocationBundleId,
    ) -> Result<(PhysReg, Vec<LiveSegment>), IntervalUnionError> {
        let Some(&register) = self.assignments.get(&bundle) else {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.MISSING_ASSIGNMENT",
                None,
                [bundle],
                "bundle has no physical-register assignment",
            ));
        };
        let segments = self.union_mut(register)?.remove(bundle)?;
        self.assignments.remove(&bundle);
        Ok((register, segments))
    }

    pub(super) fn verify(&self) -> Result<(), IntervalUnionError> {
        for union in self.unions.values() {
            union.verify()?;
        }
        let mut rebuilt = BTreeMap::new();
        for (&register, union) in &self.unions {
            for &bundle in union.memberships.keys() {
                if let Some(other) = rebuilt.insert(bundle, register) {
                    return Err(IntervalUnionError::new(
                        "INTERVAL_UNION.DUPLICATE_ASSIGNMENT",
                        None,
                        [bundle],
                        format!("bundle appears in both {other} and {register}"),
                    ));
                }
            }
        }
        if rebuilt != self.assignments {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.ASSIGNMENT_MAP",
                None,
                [],
                "register memberships differ from the bidirectional assignment map",
            ));
        }
        Ok(())
    }
}

pub(super) fn live_length(segments: &[LiveSegment]) -> Option<u64> {
    segments.iter().try_fold(0_u64, |total, segment| {
        total.checked_add(segment.start.distance_to(segment.end)?)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{
        BaseReg, MBlock, MFunction, MInst, OpSize, PhiNode, SpillDesc, VReg, VRegAllocator,
    };

    use super::super::live_interval;

    fn function(value_count: u32, blocks: Vec<MBlock>) -> MFunction {
        let mut values = VRegAllocator::new();
        for _ in 0..value_count {
            values.alloc();
        }
        let mut function =
            MFunction::new(values, vec![SpillDesc::transient(); value_count as usize]);
        function.blocks = blocks;
        function
    }

    fn normalize(function: &mut MFunction) -> NormalizedCfg {
        super::super::cfg::normalize(function).unwrap()
    }

    #[test]
    fn mutually_exclusive_diamond_arms_share_one_register_union() {
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: VReg(0),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::LoadImm {
            dst: VReg(1),
            value: 11,
        });
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::LoadImm {
            dst: VReg(2),
            value: 22,
        });
        right.push(MInst::Jump { target: BlockId(3) });
        let mut merge = MBlock::new(BlockId(3));
        merge.phis.push(PhiNode {
            dst: VReg(3),
            sources: vec![(BlockId(1), VReg(1)), (BlockId(2), VReg(2))],
        });
        merge.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: VReg(3),
            size: OpSize::S64,
        });
        merge.push(MInst::Return);
        let mut function = function(4, vec![entry, left, right, merge]);
        let cfg = normalize(&mut function);
        let intervals = live_interval::analyze(&function, &cfg).unwrap();
        let left = intervals.intervals[1].as_ref().unwrap();
        let right = intervals.intervals[2].as_ref().unwrap();
        let mut matrix = LiveIntervalMatrix::new(&cfg, &[PhysReg::RAX]).unwrap();
        matrix
            .assign(AllocationBundleId(1), PhysReg::RAX, &left.segments)
            .unwrap();
        assert!(
            matrix
                .conflicts(PhysReg::RAX, &right.segments)
                .unwrap()
                .is_empty()
        );
        matrix
            .assign(AllocationBundleId(2), PhysReg::RAX, &right.segments)
            .unwrap();
        matrix.verify().unwrap();
    }

    #[test]
    fn overlapping_assignment_is_rejected_without_mutating_the_union() {
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: VReg(0),
            value: 1,
        });
        block.push(MInst::LoadImm {
            dst: VReg(1),
            value: 2,
        });
        block.push(MInst::Add {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(1),
        });
        block.push(MInst::Return);
        let mut function = function(3, vec![block]);
        let cfg = normalize(&mut function);
        let intervals = live_interval::analyze(&function, &cfg).unwrap();
        let first = intervals.intervals[0].as_ref().unwrap();
        let second = intervals.intervals[1].as_ref().unwrap();
        let mut matrix = LiveIntervalMatrix::new(&cfg, &[PhysReg::RAX]).unwrap();
        matrix
            .assign(AllocationBundleId(0), PhysReg::RAX, &first.segments)
            .unwrap();
        let before = matrix.clone();
        let error = matrix
            .assign(AllocationBundleId(1), PhysReg::RAX, &second.segments)
            .unwrap_err();
        assert_eq!(error.rule, "INTERVAL_UNION.INTERFERENCE");
        assert_eq!(matrix, before);
        matrix.verify().unwrap();
    }

    #[test]
    fn free_regions_are_the_exact_sparse_segment_difference() {
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: VReg(0),
            value: 1,
        });
        block.push(MInst::LoadImm {
            dst: VReg(1),
            value: 2,
        });
        block.push(MInst::Mov {
            dst: VReg(2),
            src: VReg(1),
        });
        block.push(MInst::Mov {
            dst: VReg(3),
            src: VReg(0),
        });
        block.push(MInst::Return);
        let mut function = function(4, vec![block]);
        let cfg = normalize(&mut function);
        let intervals = live_interval::analyze(&function, &cfg).unwrap();
        let outer = intervals.intervals[0].as_ref().unwrap();
        let inner = intervals.intervals[1].as_ref().unwrap();
        let mut matrix = LiveIntervalMatrix::new(&cfg, &[PhysReg::RAX]).unwrap();
        matrix
            .assign(AllocationBundleId(1), PhysReg::RAX, &inner.segments)
            .unwrap();
        let free = matrix.free_segments(PhysReg::RAX, &outer.segments).unwrap();
        assert_eq!(
            free,
            vec![
                LiveSegment {
                    block: BlockId(0),
                    start: outer.segments[0].start,
                    end: inner.segments[0].start,
                },
                LiveSegment {
                    block: BlockId(0),
                    start: inner.segments[0].end,
                    end: outer.segments[0].end,
                },
            ]
        );
    }

    #[test]
    fn unassign_removes_every_sparse_segment_and_preserves_other_bundles() {
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 1,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut exit = MBlock::new(BlockId(1));
        exit.push(MInst::Mov {
            dst: VReg(1),
            src: VReg(0),
        });
        exit.push(MInst::Return);
        let mut function = function(2, vec![entry, exit]);
        let cfg = normalize(&mut function);
        let intervals = live_interval::analyze(&function, &cfg).unwrap();
        let source = intervals.intervals[0].as_ref().unwrap();
        let destination = intervals.intervals[1].as_ref().unwrap();
        let mut matrix = LiveIntervalMatrix::new(&cfg, &[PhysReg::RAX, PhysReg::RDX]).unwrap();
        matrix
            .assign(AllocationBundleId(0), PhysReg::RAX, &source.segments)
            .unwrap();
        matrix
            .assign(AllocationBundleId(1), PhysReg::RDX, &destination.segments)
            .unwrap();
        let (_, removed) = matrix.unassign(AllocationBundleId(0)).unwrap();
        assert_eq!(removed, source.segments);
        assert_eq!(matrix.register(AllocationBundleId(0)), None);
        assert_eq!(matrix.register(AllocationBundleId(1)), Some(PhysReg::RDX));
        matrix.verify().unwrap();
    }
}
