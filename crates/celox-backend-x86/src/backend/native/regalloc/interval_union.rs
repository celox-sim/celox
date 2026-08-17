//! Sparse stack-slot interval unions.
//!
//! Stack homes retain exact per-block ranges rather than being projected onto
//! one linear instruction interval. Each dynamically-created slot stores only
//! the occupied CFG rows required for first-fit coloring.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use crate::HashMap;
use crate::native::mir::BlockId;

use super::cfg::NormalizedCfg;
use super::live_interval::{LiveSegment, SlotIndex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct AllocationBundleId(pub u32);

/// Borrowed canonical sparse range proved once at an allocator boundary.
/// All stack-slot unions in one matrix share the same CFG index, so a token
/// can be reused across every slot probe without rechecking order,
/// self-interference, and block membership.
#[derive(Debug, Clone, Copy)]
pub(super) struct ValidatedSegments<'a> {
    index: &'a Arc<IntervalIndex>,
    segments: &'a [LiveSegment],
    block_indices: &'a [usize],
}

impl<'a> ValidatedSegments<'a> {
    fn as_slice(self) -> &'a [LiveSegment] {
        self.segments
    }

    pub(super) fn iter(self) -> impl Iterator<Item = (LiveSegment, usize)> + 'a {
        debug_assert_eq!(self.segments.len(), self.block_indices.len());
        self.segments
            .iter()
            .copied()
            .zip(self.block_indices.iter().copied())
    }
}

/// Allocation-owned sparse range with CFG row identities resolved once.
/// `LiveSegment::block` remains the semantic/diagnostic identity; interval
/// unions use the parallel row index for all hot ordered-map operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SparseRange {
    index: Arc<IntervalIndex>,
    segments: Vec<LiveSegment>,
    block_indices: Vec<usize>,
}

impl SparseRange {
    pub(super) fn validated(&self) -> ValidatedSegments<'_> {
        debug_assert_eq!(self.segments.len(), self.block_indices.len());
        ValidatedSegments {
            index: &self.index,
            segments: &self.segments,
            block_indices: &self.block_indices,
        }
    }
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

#[derive(Debug, PartialEq, Eq)]
struct IntervalIndex {
    block_index: HashMap<BlockId, usize>,
    block_ids: Vec<BlockId>,
}

impl IntervalIndex {
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
        })
    }

    fn make_range(
        self: &Arc<Self>,
        segments: Vec<LiveSegment>,
    ) -> Result<SparseRange, IntervalUnionError> {
        let mut block_indices = Vec::with_capacity(segments.len());
        let mut previous = None::<LiveSegment>;
        for &segment in &segments {
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
            let Some(&block_index) = self.block_index.get(&segment.block) else {
                return Err(IntervalUnionError::new(
                    "INTERVAL_UNION.SEGMENT_BLOCK",
                    Some(segment.block),
                    [],
                    "segment references a block outside the normalized CFG",
                ));
            };
            if self.block_ids.get(block_index) != Some(&segment.block) {
                return Err(IntervalUnionError::new(
                    "INTERVAL_UNION.CFG_SHAPE",
                    Some(segment.block),
                    [],
                    "CFG row does not resolve back to the segment block",
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
            block_indices.push(block_index);
        }
        Ok(SparseRange {
            index: Arc::clone(self),
            segments,
            block_indices,
        })
    }
}

/// Sparse interference matrix with an allocation-owned, dynamically growing
/// color domain. Physical-register allocation has a fixed target register
/// set; stack-slot coloring instead creates a new color only when every
/// existing slot interferes with a home's exact CFG range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DynamicUnionNode {
    start: SlotIndex,
    end: SlotIndex,
    next: u32,
}

/// Ownerless, append-only occupancy for one dynamically created stack slot.
///
/// Stack coloring never evicts an assigned home and only needs to know
/// whether a slot is occupied. Reusing `IntervalUnion` here retained one
/// owner-bearing BTree node for nearly every `(slot, block)` pair. Large RTL
/// functions make that relation close to dense: Heliodor produced tens of
/// millions of one-entry trees. A dense four-byte head per CFG block plus one
/// flat node arena stores the same interval union without per-segment heap
/// allocations. Adjacent ranges are merged because their owners are
/// irrelevant after the assignment map records the selected slot.
#[derive(Debug, Clone)]
struct DynamicSlotUnion {
    heads: Box<[u32]>,
    nodes: Vec<DynamicUnionNode>,
}

impl DynamicSlotUnion {
    const NONE: u32 = u32::MAX;

    fn new(block_count: usize) -> Self {
        Self {
            heads: vec![Self::NONE; block_count].into_boxed_slice(),
            nodes: Vec::new(),
        }
    }

    fn interferes_segment(&self, segment: LiveSegment, block: usize) -> bool {
        let mut current = self.heads[block];
        while current != Self::NONE {
            let node = self.nodes[current as usize];
            if node.end <= segment.start {
                current = node.next;
                continue;
            }
            if node.start >= segment.end {
                break;
            }
            return true;
        }
        false
    }

    fn interferes_indexed(&self, segments: ValidatedSegments<'_>) -> bool {
        segments
            .iter()
            .any(|(segment, block)| self.interferes_segment(segment, block))
    }

    fn interferes_indexed_with_hint(&self, segments: ValidatedSegments<'_>, hint: usize) -> bool {
        if self.interferes_segment(segments.segments[hint], segments.block_indices[hint]) {
            return true;
        }
        segments
            .iter()
            .enumerate()
            .any(|(index, (segment, block))| {
                index != hint && self.interferes_segment(segment, block)
            })
    }

    fn insert_indexed(
        &mut self,
        bundle: AllocationBundleId,
        segments: ValidatedSegments<'_>,
    ) -> Result<(), IntervalUnionError> {
        if segments.as_slice().is_empty() {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.EMPTY_ASSIGNMENT",
                None,
                [bundle],
                "a stack-slot assignment must own at least one live segment",
            ));
        }
        if self.interferes_indexed(segments) {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.INTERFERENCE",
                segments.as_slice().first().map(|segment| segment.block),
                [bundle],
                "cannot overlap occupancy in one dynamic stack slot",
            ));
        }
        if self
            .nodes
            .len()
            .checked_add(segments.as_slice().len())
            .is_none_or(|count| count > u32::MAX as usize)
        {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.DYNAMIC_NODE_RANGE",
                segments.as_slice().first().map(|segment| segment.block),
                [bundle],
                "dynamic stack-slot union exceeds the node identity domain",
            ));
        }
        for (segment, block) in segments.iter() {
            self.insert_segment(segment, block);
        }
        Ok(())
    }

    fn insert_segment(&mut self, segment: LiveSegment, block: usize) {
        let mut previous = Self::NONE;
        let mut current = self.heads[block];
        while current != Self::NONE {
            let node = self.nodes[current as usize];
            if node.end <= segment.start {
                previous = current;
                current = node.next;
            } else {
                break;
            }
        }

        let merge_left =
            previous != Self::NONE && self.nodes[previous as usize].end == segment.start;
        let merge_right =
            current != Self::NONE && self.nodes[current as usize].start == segment.end;
        match (merge_left, merge_right) {
            (true, true) => {
                let right = self.nodes[current as usize];
                let left = &mut self.nodes[previous as usize];
                left.end = right.end;
                left.next = right.next;
            }
            (true, false) => {
                self.nodes[previous as usize].end = segment.end;
            }
            (false, true) => {
                self.nodes[current as usize].start = segment.start;
            }
            (false, false) => {
                let node = self.nodes.len() as u32;
                self.nodes.push(DynamicUnionNode {
                    start: segment.start,
                    end: segment.end,
                    next: current,
                });
                if previous == Self::NONE {
                    self.heads[block] = node;
                } else {
                    self.nodes[previous as usize].next = node;
                }
            }
        }
    }

    fn verify(&self, index: &IntervalIndex) -> Result<(), IntervalUnionError> {
        if self.heads.len() != index.block_ids.len() {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.CFG_SHAPE",
                None,
                [],
                "dynamic stack-slot block table does not cover the CFG",
            ));
        }
        for (block, &head) in self.heads.iter().enumerate() {
            let mut current = head;
            let mut previous_end = None::<SlotIndex>;
            let mut traversed = 0usize;
            while current != Self::NONE {
                let Some(&node) = self.nodes.get(current as usize) else {
                    return Err(IntervalUnionError::new(
                        "INTERVAL_UNION.DYNAMIC_NODE_RANGE",
                        index.block_ids.get(block).copied(),
                        [],
                        "dynamic stack-slot chain references a missing node",
                    ));
                };
                if node.start >= node.end {
                    return Err(IntervalUnionError::new(
                        "INTERVAL_UNION.SEGMENT_RANGE",
                        index.block_ids.get(block).copied(),
                        [],
                        "dynamic stack-slot union contains an empty or reversed segment",
                    ));
                }
                if previous_end.is_some_and(|end| end >= node.start) {
                    return Err(IntervalUnionError::new(
                        "INTERVAL_UNION.INTERFERENCE",
                        index.block_ids.get(block).copied(),
                        [],
                        "dynamic stack-slot union contains overlapping or unmerged segments",
                    ));
                }
                previous_end = Some(node.end);
                current = node.next;
                traversed += 1;
                if traversed > self.nodes.len() {
                    return Err(IntervalUnionError::new(
                        "INTERVAL_UNION.DYNAMIC_NODE_CYCLE",
                        index.block_ids.get(block).copied(),
                        [],
                        "dynamic stack-slot union contains a node cycle",
                    ));
                }
            }
        }
        Ok(())
    }

    fn same_occupancy(&self, other: &Self) -> bool {
        if self.heads.len() != other.heads.len() {
            return false;
        }
        for block in 0..self.heads.len() {
            let mut left = self.heads[block];
            let mut right = other.heads[block];
            loop {
                match (left, right) {
                    (Self::NONE, Self::NONE) => break,
                    (Self::NONE, _) | (_, Self::NONE) => return false,
                    _ => {
                        let left_node = self.nodes[left as usize];
                        let right_node = other.nodes[right as usize];
                        if (left_node.start, left_node.end) != (right_node.start, right_node.end) {
                            return false;
                        }
                        left = left_node.next;
                        right = right_node.next;
                    }
                }
            }
        }
        true
    }

    fn is_empty(&self) -> bool {
        self.heads.iter().all(|&head| head == Self::NONE)
    }
}

#[derive(Debug, Clone)]
pub(super) struct DynamicIntervalMatrix {
    index: Arc<IntervalIndex>,
    unions: Vec<DynamicSlotUnion>,
    assignments: BTreeMap<AllocationBundleId, usize>,
    block_slot_counts: Vec<u32>,
}

impl PartialEq for DynamicIntervalMatrix {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
            && self.assignments == other.assignments
            && self.block_slot_counts == other.block_slot_counts
            && self.unions.len() == other.unions.len()
            && self
                .unions
                .iter()
                .zip(&other.unions)
                .all(|(left, right)| left.same_occupancy(right))
    }
}

impl Eq for DynamicIntervalMatrix {}

impl DynamicIntervalMatrix {
    pub(super) fn new(cfg: &NormalizedCfg) -> Result<Self, IntervalUnionError> {
        Ok(Self {
            index: Arc::new(IntervalIndex::new(cfg)?),
            unions: Vec::new(),
            assignments: BTreeMap::new(),
            block_slot_counts: vec![0; cfg.successors.len()],
        })
    }

    pub(super) fn make_range(
        &self,
        segments: Vec<LiveSegment>,
    ) -> Result<SparseRange, IntervalUnionError> {
        self.index.make_range(segments)
    }

    fn validate_token(&self, segments: ValidatedSegments<'_>) -> Result<(), IntervalUnionError> {
        if !Arc::ptr_eq(&self.index, segments.index) {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.RANGE_CFG",
                None,
                [],
                "sparse range belongs to a different normalized CFG",
            ));
        }
        Ok(())
    }

    pub(super) fn first_available_validated(
        &self,
        segments: ValidatedSegments<'_>,
    ) -> Result<usize, IntervalUnionError> {
        self.validate_token(segments)?;
        let Some(hint) = segments
            .block_indices
            .iter()
            .enumerate()
            .max_by_key(|&(_, &block)| self.block_slot_counts[block])
            .map(|(index, _)| index)
        else {
            return Ok(self.unions.len());
        };
        Ok(self
            .unions
            .iter()
            .position(|union| !union.interferes_indexed_with_hint(segments, hint))
            .unwrap_or(self.unions.len()))
    }

    pub(super) fn assign_validated(
        &mut self,
        bundle: AllocationBundleId,
        slot: usize,
        segments: ValidatedSegments<'_>,
    ) -> Result<(), IntervalUnionError> {
        self.validate_token(segments)?;
        if let Some(current) = self.assignments.get(&bundle) {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.DUPLICATE_ASSIGNMENT",
                None,
                [bundle],
                format!("bundle is already assigned to dynamic slot {current}"),
            ));
        }
        if slot > self.unions.len() {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.DYNAMIC_SLOT_RANGE",
                None,
                [bundle],
                format!("dynamic slot {slot} skips the current color domain"),
            ));
        }
        let mut newly_occupied_blocks = Vec::new();
        let mut previous_block = None;
        for (_, block) in segments.iter() {
            if previous_block == Some(block) {
                continue;
            }
            previous_block = Some(block);
            if slot == self.unions.len() || self.unions[slot].heads[block] == DynamicSlotUnion::NONE
            {
                newly_occupied_blocks.push(block);
            }
        }
        if slot == self.unions.len() {
            let mut union = DynamicSlotUnion::new(self.index.block_ids.len());
            union.insert_indexed(bundle, segments)?;
            self.unions.push(union);
        } else {
            self.unions[slot].insert_indexed(bundle, segments)?;
        }
        for block in newly_occupied_blocks {
            self.block_slot_counts[block] = self.block_slot_counts[block]
                .checked_add(1)
                .ok_or_else(|| {
                    IntervalUnionError::new(
                        "INTERVAL_UNION.BLOCK_SLOT_COUNT",
                        self.index.block_ids.get(block).copied(),
                        [bundle],
                        "dynamic stack-slot occupancy count exceeds u32",
                    )
                })?;
        }
        self.assignments.insert(bundle, slot);
        Ok(())
    }

    pub(super) fn slot_count(&self) -> usize {
        self.unions.len()
    }

    pub(super) fn slot(&self, bundle: AllocationBundleId) -> Option<usize> {
        self.assignments.get(&bundle).copied()
    }

    pub(super) fn verify(&self) -> Result<(), IntervalUnionError> {
        for (slot, union) in self.unions.iter().enumerate() {
            union.verify(&self.index)?;
            if union.is_empty() {
                return Err(IntervalUnionError::new(
                    "INTERVAL_UNION.EMPTY_DYNAMIC_SLOT",
                    None,
                    [],
                    format!("dynamic slot {slot} has no assigned sparse range"),
                ));
            }
        }
        if self
            .assignments
            .values()
            .any(|&slot| slot >= self.unions.len())
        {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.ASSIGNMENT_MAP",
                None,
                [],
                "dynamic stack-slot assignment names a missing slot",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::mir::{MBlock, MFunction, MInst, SpillDesc, VRegAllocator};

    fn one_block_cfg() -> NormalizedCfg {
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Return);
        let mut function = MFunction::new(VRegAllocator::new(), Vec::<SpillDesc>::new());
        function.blocks.push(block);
        super::super::cfg::normalize(&mut function).unwrap()
    }

    #[test]
    fn dynamic_slot_merges_adjacent_ranges_in_any_insertion_order() {
        let cfg = one_block_cfg();
        let slot = |phase: u64| SlotIndex::for_test(phase);
        let segments = [
            LiveSegment {
                block: BlockId(0),
                start: slot(0),
                end: slot(1),
            },
            LiveSegment {
                block: BlockId(0),
                start: slot(1),
                end: slot(2),
            },
            LiveSegment {
                block: BlockId(0),
                start: slot(2),
                end: slot(3),
            },
        ];
        let build = |order: [usize; 3]| {
            let mut matrix = DynamicIntervalMatrix::new(&cfg).unwrap();
            for (bundle, segment) in order.into_iter().map(|index| segments[index]).enumerate() {
                let range = matrix.make_range(vec![segment]).unwrap();
                matrix
                    .assign_validated(AllocationBundleId(bundle as u32), 0, range.validated())
                    .unwrap();
            }
            matrix.verify().unwrap();
            matrix
        };
        assert_eq!(build([0, 2, 1]), build([2, 1, 0]));
    }

    #[test]
    fn validated_range_cannot_cross_cfg_owners() {
        let first = DynamicIntervalMatrix::new(&one_block_cfg()).unwrap();
        let second = DynamicIntervalMatrix::new(&one_block_cfg()).unwrap();
        let range = first
            .make_range(vec![LiveSegment {
                block: BlockId(0),
                start: SlotIndex::for_test(0),
                end: SlotIndex::for_test(1),
            }])
            .unwrap();
        let error = second
            .first_available_validated(range.validated())
            .unwrap_err();
        assert_eq!(error.rule, "INTERVAL_UNION.RANGE_CFG");
    }
}
