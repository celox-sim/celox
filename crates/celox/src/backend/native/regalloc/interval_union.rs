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
use std::ops::Deref;
use std::sync::Arc;

use crate::backend::native::mir::BlockId;

use super::assignment::PhysReg;
use super::cfg::NormalizedCfg;
use super::live_interval::{LiveSegment, SlotIndex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct AllocationBundleId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct FixedReservationId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum OccupancyOwner {
    Bundle(AllocationBundleId),
    Fixed(FixedReservationId),
}

impl OccupancyOwner {
    fn bundle(self) -> Option<AllocationBundleId> {
        match self {
            Self::Bundle(bundle) => Some(bundle),
            Self::Fixed(_) => None,
        }
    }
}

/// Immutable target occupancy in one physical register. The segment uses the
/// same sparse CFG coordinate space as movable live ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FixedRegisterReservation {
    pub register: PhysReg,
    pub segment: LiveSegment,
}

/// Reusable dense visitation epochs for conflict queries. Allocation bundle
/// IDs are stable dense table indexes, so this replaces a freshly allocated
/// ordered set on every physical-register probe.
#[derive(Debug, Default)]
pub(super) struct ConflictCollector {
    marks: Vec<u32>,
    epoch: u32,
}

/// Borrowed canonical sparse range proved once at an allocator boundary.
/// All physical-register unions in one matrix share the same CFG index, so a
/// token can be reused across every register probe without rechecking order,
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

    fn iter(self) -> impl Iterator<Item = (LiveSegment, usize)> + 'a {
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

    pub(super) fn as_slice(&self) -> &[LiveSegment] {
        &self.segments
    }
}

impl Deref for SparseRange {
    type Target = [LiveSegment];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl ConflictCollector {
    fn begin(&mut self, bundle_count: usize) {
        if self.marks.len() < bundle_count {
            self.marks.resize(bundle_count, 0);
        }
        if self.epoch == u32::MAX {
            self.marks.fill(0);
            self.epoch = 1;
        } else {
            self.epoch += 1;
        }
    }

    fn record(
        &mut self,
        bundle: AllocationBundleId,
        output: &mut Vec<AllocationBundleId>,
    ) -> Result<(), IntervalUnionError> {
        let Some(mark) = self.marks.get_mut(bundle.0 as usize) else {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.BUNDLE_RANGE",
                None,
                [bundle],
                "interval union references a bundle outside the allocation table",
            ));
        };
        if *mark != self.epoch {
            *mark = self.epoch;
            output.push(bundle);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UnionEntry {
    end: SlotIndex,
    owner: OccupancyOwner,
}

/// Exact occupied subrange of one canonical candidate segment.  Conflict
/// collection and split projection share this result, so allocation never
/// searches the same physical interval union a second time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OccupancyCut {
    pub segment: usize,
    pub start: SlotIndex,
    pub end: SlotIndex,
    pub owner: OccupancyOwner,
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
    index: Arc<IntervalIndex>,
    /// Only CFG blocks which currently contain an assigned segment exist.
    /// Empty per-register/per-block trees would multiply large RTL CFGs by
    /// the physical register count.
    blocks: BTreeMap<usize, BTreeMap<SlotIndex, UnionEntry>>,
}

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

impl IntervalUnion {
    fn new(index: Arc<IntervalIndex>) -> Self {
        Self {
            index,
            blocks: BTreeMap::new(),
        }
    }

    fn overlapping_entries_at(
        &self,
        segment: LiveSegment,
        block: usize,
    ) -> Vec<(SlotIndex, UnionEntry)> {
        let mut overlaps = Vec::new();
        let Some(entries) = self.blocks.get(&block) else {
            return overlaps;
        };
        for (&start, &entry) in entries.range((Unbounded, Excluded(segment.end))).rev() {
            if entry.end <= segment.start {
                break;
            }
            overlaps.push((start, entry));
        }
        overlaps.reverse();
        overlaps
    }

    fn interferes_indexed(&self, segments: ValidatedSegments<'_>) -> bool {
        for (segment, block) in segments.iter() {
            let Some(entries) = self.blocks.get(&block) else {
                continue;
            };
            for (&start, entry) in entries.range((Unbounded, Excluded(segment.end))).rev() {
                if entry.end <= segment.start {
                    break;
                }
                if start < segment.end {
                    return true;
                }
            }
        }
        false
    }

    fn interferes_bundle_indexed(&self, segments: ValidatedSegments<'_>) -> bool {
        for (segment, block) in segments.iter() {
            let Some(entries) = self.blocks.get(&block) else {
                continue;
            };
            for (&start, entry) in entries.range((Unbounded, Excluded(segment.end))).rev() {
                if entry.end <= segment.start {
                    break;
                }
                if start < segment.end && matches!(entry.owner, OccupancyOwner::Bundle(_)) {
                    return true;
                }
            }
        }
        false
    }

    fn collect_conflicts_indexed(
        &self,
        segments: ValidatedSegments<'_>,
        bundle_count: usize,
        collector: &mut ConflictCollector,
        output: &mut Vec<AllocationBundleId>,
    ) -> Result<(), IntervalUnionError> {
        collector.begin(bundle_count);
        output.clear();
        for (segment, block) in segments.iter() {
            let Some(entries) = self.blocks.get(&block) else {
                continue;
            };
            for (&start, entry) in entries.range((Unbounded, Excluded(segment.end))).rev() {
                if entry.end <= segment.start {
                    break;
                }
                if start < segment.end {
                    if let Some(bundle) = entry.owner.bundle() {
                        collector.record(bundle, output)?;
                    }
                }
            }
        }
        output.sort_unstable();
        Ok(())
    }

    fn collect_interference_indexed(
        &self,
        segments: ValidatedSegments<'_>,
        bundle_count: usize,
        collector: &mut ConflictCollector,
        conflicts: &mut Vec<AllocationBundleId>,
        cuts: &mut Vec<OccupancyCut>,
    ) -> Result<(), IntervalUnionError> {
        collector.begin(bundle_count);
        conflicts.clear();
        cuts.clear();
        for (segment_index, (segment, block)) in segments.iter().enumerate() {
            let Some(entries) = self.blocks.get(&block) else {
                continue;
            };
            let first_cut = cuts.len();
            for (&start, entry) in entries.range((Unbounded, Excluded(segment.end))).rev() {
                if entry.end <= segment.start {
                    break;
                }
                if start < segment.end {
                    if let Some(bundle) = entry.owner.bundle() {
                        collector.record(bundle, conflicts)?;
                    }
                    cuts.push(OccupancyCut {
                        segment: segment_index,
                        start: start.max(segment.start),
                        end: entry.end.min(segment.end),
                        owner: entry.owner,
                    });
                }
            }
            // The ordered map was traversed backwards to stop at the first
            // non-overlap. Restore canonical segment/start order once here.
            cuts[first_cut..].reverse();
        }
        conflicts.sort_unstable();
        Ok(())
    }

    fn conflicts_indexed(&self, segments: ValidatedSegments<'_>) -> Vec<AllocationBundleId> {
        let mut conflicts = BTreeSet::new();
        for (segment, block) in segments.iter() {
            conflicts.extend(
                self.overlapping_entries_at(segment, block)
                    .into_iter()
                    .filter_map(|(_, entry)| entry.owner.bundle()),
            );
        }
        conflicts.into_iter().collect()
    }

    fn insert_indexed(
        &mut self,
        owner: OccupancyOwner,
        segments: ValidatedSegments<'_>,
    ) -> Result<(), IntervalUnionError> {
        let bundles = owner.bundle();
        if segments.as_slice().is_empty() {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.EMPTY_ASSIGNMENT",
                None,
                bundles,
                "a register assignment must own at least one live segment",
            ));
        }
        for (segment, block) in segments.iter() {
            if self
                .blocks
                .get(&block)
                .is_some_and(|entries| entries.contains_key(&segment.start))
            {
                return Err(IntervalUnionError::new(
                    "INTERVAL_UNION.INTERFERENCE",
                    Some(segment.block),
                    bundles,
                    "validated insertion would replace an existing register segment",
                ));
            }
        }
        let conflict = segments.iter().find_map(|(segment, block)| {
            self.overlapping_entries_at(segment, block)
                .first()
                .copied()
                .map(|(_, entry)| (segment.block, entry.owner))
        });
        if let Some((block, conflict)) = conflict {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.INTERFERENCE",
                Some(block),
                bundles.into_iter().chain(conflict.bundle()),
                "cannot overlap movable or fixed occupancy in one physical register",
            ));
        }
        for (segment, block) in segments.iter() {
            let previous = self.blocks.entry(block).or_default().insert(
                segment.start,
                UnionEntry {
                    end: segment.end,
                    owner,
                },
            );
            debug_assert!(previous.is_none());
        }
        Ok(())
    }

    fn remove_indexed(
        &mut self,
        owner: OccupancyOwner,
        segments: ValidatedSegments<'_>,
    ) -> Result<(), IntervalUnionError> {
        let bundles = owner.bundle();
        if segments.as_slice().is_empty() {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.EMPTY_ASSIGNMENT",
                None,
                bundles,
                "a register assignment must own at least one live segment",
            ));
        }
        for (segment, block) in segments.iter() {
            if self
                .blocks
                .get(&block)
                .and_then(|entries| entries.get(&segment.start))
                != Some(&UnionEntry {
                    end: segment.end,
                    owner,
                })
            {
                return Err(IntervalUnionError::new(
                    "INTERVAL_UNION.MEMBERSHIP",
                    Some(segment.block),
                    bundles,
                    "bundle membership and ordered segment table disagree",
                ));
            }
        }
        for (segment, block) in segments.iter() {
            let remove_block = {
                let Some(entries) = self.blocks.get_mut(&block) else {
                    return Err(IntervalUnionError::new(
                        "INTERVAL_UNION.MEMBERSHIP",
                        Some(segment.block),
                        bundles,
                        "validated bundle block disappeared during removal",
                    ));
                };
                if entries.remove(&segment.start)
                    != Some(UnionEntry {
                        end: segment.end,
                        owner,
                    })
                {
                    return Err(IntervalUnionError::new(
                        "INTERVAL_UNION.MEMBERSHIP",
                        Some(segment.block),
                        bundles,
                        "validated bundle segment disappeared during removal",
                    ));
                }
                entries.is_empty()
            };
            if remove_block {
                self.blocks.remove(&block);
            }
        }
        Ok(())
    }

    /// Subtract every occupied segment from the input, preserving sparse CFG
    /// block identity. These are the maximal regions available for splitting
    /// a bundle onto this register.
    fn free_segments_indexed(&self, segments: ValidatedSegments<'_>) -> Vec<LiveSegment> {
        let mut free = Vec::new();
        for (segment, block) in segments.iter() {
            let mut cursor = segment.start;
            for (occupied_start, entry) in self.overlapping_entries_at(segment, block) {
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
        free
    }

    fn verify(&self) -> Result<(), IntervalUnionError> {
        if self.index.block_index.len() != self.index.block_ids.len()
            || self
                .index
                .block_ids
                .iter()
                .enumerate()
                .any(|(index, block)| self.index.block_index.get(block) != Some(&index))
        {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.CFG_SHAPE",
                None,
                [],
                "union block tables are not a bijection",
            ));
        }

        for (&block, entries) in &self.blocks {
            let Some(&block_id) = self.index.block_ids.get(block) else {
                return Err(IntervalUnionError::new(
                    "INTERVAL_UNION.SEGMENT_BLOCK",
                    None,
                    [],
                    format!("sparse union references out-of-range CFG block {block}"),
                ));
            };
            if entries.is_empty() {
                return Err(IntervalUnionError::new(
                    "INTERVAL_UNION.EMPTY_BLOCK",
                    Some(block_id),
                    [],
                    "sparse union retains an empty block tree",
                ));
            }
            let mut prior = None::<(SlotIndex, UnionEntry)>;
            for (&start, &entry) in entries {
                if start >= entry.end {
                    return Err(IntervalUnionError::new(
                        "INTERVAL_UNION.SEGMENT_RANGE",
                        Some(block_id),
                        entry.owner.bundle(),
                        "union contains an empty or reversed segment",
                    ));
                }
                if let Some((_, previous)) = prior
                    && previous.end > start
                {
                    return Err(IntervalUnionError::new(
                        "INTERVAL_UNION.INTERFERENCE",
                        Some(block_id),
                        previous
                            .owner
                            .bundle()
                            .into_iter()
                            .chain(entry.owner.bundle()),
                        "ordered union contains overlapping assignments",
                    ));
                }
                prior = Some((start, entry));
            }
        }
        Ok(())
    }
}

/// Bidirectional physical-register interference matrix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LiveIntervalMatrix {
    index: Arc<IntervalIndex>,
    register_order: Vec<PhysReg>,
    unions: BTreeMap<PhysReg, IntervalUnion>,
    assignments: BTreeMap<AllocationBundleId, PhysReg>,
    fixed_reservations: Vec<FixedRegisterReservation>,
}

impl LiveIntervalMatrix {
    pub(super) fn new(
        cfg: &NormalizedCfg,
        registers: &[PhysReg],
    ) -> Result<Self, IntervalUnionError> {
        let index = Arc::new(IntervalIndex::new(cfg)?);
        let mut unions = BTreeMap::new();
        for &register in registers {
            if unions
                .insert(register, IntervalUnion::new(Arc::clone(&index)))
                .is_some()
            {
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
            index,
            register_order: registers.to_vec(),
            unions,
            assignments: BTreeMap::new(),
            fixed_reservations: Vec::new(),
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
        self.register_order.iter().copied()
    }

    pub(super) fn register(&self, bundle: AllocationBundleId) -> Option<PhysReg> {
        self.assignments.get(&bundle).copied()
    }

    /// Preflight and replace immutable target occupancy after all movable
    /// ranges changed by the same machine-fact update have been removed.
    /// Stable movable memberships remain in place.
    pub(super) fn replace_fixed_reservations(
        &mut self,
        reservations: &[FixedRegisterReservation],
    ) -> Result<(), IntervalUnionError> {
        let mut canonical = reservations.to_vec();
        canonical.sort_unstable_by_key(|reservation| {
            (
                reservation.register,
                reservation.segment.block,
                reservation.segment.start,
                reservation.segment.end,
            )
        });
        if canonical.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.FIXED_IDENTITY",
                canonical
                    .windows(2)
                    .find(|pair| pair[0] == pair[1])
                    .map(|pair| pair[0].segment.block),
                [],
                "fixed reservation set contains a duplicate interval",
            ));
        }

        let mut fixed_unions = self
            .register_order
            .iter()
            .copied()
            .map(|register| (register, IntervalUnion::new(Arc::clone(&self.index))))
            .collect::<BTreeMap<_, _>>();
        let mut prepared = Vec::with_capacity(canonical.len());
        for (row, &reservation) in canonical.iter().enumerate() {
            let id = FixedReservationId(u32::try_from(row).map_err(|_| {
                IntervalUnionError::new(
                    "INTERVAL_UNION.FIXED_RANGE",
                    Some(reservation.segment.block),
                    [],
                    "fixed reservation count exceeds the identity domain",
                )
            })?);
            let range = self.make_range(vec![reservation.segment])?;
            let fixed = fixed_unions.get_mut(&reservation.register).ok_or_else(|| {
                IntervalUnionError::new(
                    "INTERVAL_UNION.UNKNOWN_REGISTER",
                    Some(reservation.segment.block),
                    [],
                    format!(
                        "fixed reservation names physical register {} outside this matrix",
                        reservation.register
                    ),
                )
            })?;
            fixed.insert_indexed(OccupancyOwner::Fixed(id), range.validated())?;

            let block = range
                .validated()
                .iter()
                .next()
                .map(|(_, block)| block)
                .ok_or_else(|| {
                    IntervalUnionError::new(
                        "INTERVAL_UNION.FIXED_RANGE",
                        Some(reservation.segment.block),
                        [],
                        "validated fixed reservation unexpectedly has no sparse segment",
                    )
                })?;
            if let Some((_, entry)) = self
                .union(reservation.register)?
                .overlapping_entries_at(reservation.segment, block)
                .into_iter()
                .find(|(_, entry)| matches!(entry.owner, OccupancyOwner::Bundle(_)))
            {
                return Err(IntervalUnionError::new(
                    "INTERVAL_UNION.FIXED_INTERFERENCE",
                    Some(reservation.segment.block),
                    entry.owner.bundle(),
                    "replacement fixed reservation overlaps a retained movable bundle",
                ));
            }
            prepared.push((id, reservation, range));
        }

        let previous = self.fixed_reservations.clone();
        for (row, reservation) in previous.into_iter().enumerate() {
            let id = FixedReservationId(u32::try_from(row).map_err(|_| {
                IntervalUnionError::new(
                    "INTERVAL_UNION.FIXED_RANGE",
                    Some(reservation.segment.block),
                    [],
                    "stored fixed reservation identity exceeds u32",
                )
            })?);
            let range = self.make_range(vec![reservation.segment])?;
            self.union_mut(reservation.register)?
                .remove_indexed(OccupancyOwner::Fixed(id), range.validated())?;
        }
        for (id, reservation, range) in prepared {
            self.union_mut(reservation.register)?
                .insert_indexed(OccupancyOwner::Fixed(id), range.validated())?;
        }
        self.fixed_reservations = canonical;
        Ok(())
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

    pub(super) fn conflicts(
        &self,
        register: PhysReg,
        segments: &[LiveSegment],
    ) -> Result<Vec<AllocationBundleId>, IntervalUnionError> {
        let range = self.make_range(segments.to_vec())?;
        self.conflicts_validated(register, range.validated())
    }

    pub(super) fn conflicts_validated(
        &self,
        register: PhysReg,
        segments: ValidatedSegments<'_>,
    ) -> Result<Vec<AllocationBundleId>, IntervalUnionError> {
        self.validate_token(segments)?;
        Ok(self.union(register)?.conflicts_indexed(segments))
    }

    pub(super) fn interferes(
        &self,
        register: PhysReg,
        segments: &[LiveSegment],
    ) -> Result<bool, IntervalUnionError> {
        let range = self.make_range(segments.to_vec())?;
        self.interferes_validated(register, range.validated())
    }

    pub(super) fn interferes_validated(
        &self,
        register: PhysReg,
        segments: ValidatedSegments<'_>,
    ) -> Result<bool, IntervalUnionError> {
        self.validate_token(segments)?;
        Ok(self.union(register)?.interferes_indexed(segments))
    }

    pub(super) fn interferes_bundle_validated(
        &self,
        register: PhysReg,
        segments: ValidatedSegments<'_>,
    ) -> Result<bool, IntervalUnionError> {
        self.validate_token(segments)?;
        Ok(self.union(register)?.interferes_bundle_indexed(segments))
    }

    pub(super) fn collect_conflicts(
        &self,
        register: PhysReg,
        segments: &[LiveSegment],
        bundle_count: usize,
        collector: &mut ConflictCollector,
        output: &mut Vec<AllocationBundleId>,
    ) -> Result<(), IntervalUnionError> {
        let range = self.make_range(segments.to_vec())?;
        self.collect_conflicts_validated(
            register,
            range.validated(),
            bundle_count,
            collector,
            output,
        )
    }

    pub(super) fn collect_conflicts_validated(
        &self,
        register: PhysReg,
        segments: ValidatedSegments<'_>,
        bundle_count: usize,
        collector: &mut ConflictCollector,
        output: &mut Vec<AllocationBundleId>,
    ) -> Result<(), IntervalUnionError> {
        self.validate_token(segments)?;
        self.union(register)?
            .collect_conflicts_indexed(segments, bundle_count, collector, output)
    }

    pub(super) fn collect_interference_validated(
        &self,
        register: PhysReg,
        segments: ValidatedSegments<'_>,
        bundle_count: usize,
        collector: &mut ConflictCollector,
        conflicts: &mut Vec<AllocationBundleId>,
        cuts: &mut Vec<OccupancyCut>,
    ) -> Result<(), IntervalUnionError> {
        self.validate_token(segments)?;
        self.union(register)?.collect_interference_indexed(
            segments,
            bundle_count,
            collector,
            conflicts,
            cuts,
        )
    }

    pub(super) fn free_segments(
        &self,
        register: PhysReg,
        segments: &[LiveSegment],
    ) -> Result<Vec<LiveSegment>, IntervalUnionError> {
        let range = self.make_range(segments.to_vec())?;
        self.free_segments_validated(register, range.validated())
    }

    pub(super) fn free_segments_validated(
        &self,
        register: PhysReg,
        segments: ValidatedSegments<'_>,
    ) -> Result<Vec<LiveSegment>, IntervalUnionError> {
        self.validate_token(segments)?;
        Ok(self.union(register)?.free_segments_indexed(segments))
    }

    pub(super) fn assign(
        &mut self,
        bundle: AllocationBundleId,
        register: PhysReg,
        segments: &[LiveSegment],
    ) -> Result<(), IntervalUnionError> {
        let range = self.make_range(segments.to_vec())?;
        self.assign_validated(bundle, register, range.validated())
    }

    pub(super) fn assign_validated(
        &mut self,
        bundle: AllocationBundleId,
        register: PhysReg,
        segments: ValidatedSegments<'_>,
    ) -> Result<(), IntervalUnionError> {
        self.validate_token(segments)?;
        if let Some(current) = self.register(bundle) {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.DUPLICATE_ASSIGNMENT",
                None,
                [bundle],
                format!("bundle is already assigned to {current}"),
            ));
        }
        self.union_mut(register)?
            .insert_indexed(OccupancyOwner::Bundle(bundle), segments)?;
        self.assignments.insert(bundle, register);
        Ok(())
    }

    pub(super) fn unassign(
        &mut self,
        bundle: AllocationBundleId,
        segments: &[LiveSegment],
    ) -> Result<PhysReg, IntervalUnionError> {
        let range = self.make_range(segments.to_vec())?;
        self.unassign_validated(bundle, range.validated())
    }

    pub(super) fn unassign_validated(
        &mut self,
        bundle: AllocationBundleId,
        segments: ValidatedSegments<'_>,
    ) -> Result<PhysReg, IntervalUnionError> {
        self.validate_token(segments)?;
        let Some(&register) = self.assignments.get(&bundle) else {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.MISSING_ASSIGNMENT",
                None,
                [bundle],
                "bundle has no physical-register assignment",
            ));
        };
        self.union_mut(register)?
            .remove_indexed(OccupancyOwner::Bundle(bundle), segments)?;
        self.assignments.remove(&bundle);
        Ok(register)
    }

    pub(super) fn verify(&self) -> Result<(), IntervalUnionError> {
        if self.register_order.len() != self.unions.len()
            || self.register_order.iter().copied().collect::<BTreeSet<_>>()
                != self.unions.keys().copied().collect()
        {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.REGISTER_ORDER",
                None,
                [],
                "register preference order is not a bijection over interval unions",
            ));
        }
        for union in self.unions.values() {
            union.verify()?;
        }
        let mut rebuilt = BTreeMap::new();
        let mut fixed = BTreeMap::<FixedReservationId, FixedRegisterReservation>::new();
        for (&register, union) in &self.unions {
            for (&block, entries) in &union.blocks {
                let block_id = self.index.block_ids[block];
                for (&start, entry) in entries {
                    match entry.owner {
                        OccupancyOwner::Bundle(bundle) => {
                            if let Some(other) = rebuilt.insert(bundle, register)
                                && other != register
                            {
                                return Err(IntervalUnionError::new(
                                    "INTERVAL_UNION.DUPLICATE_ASSIGNMENT",
                                    None,
                                    [bundle],
                                    format!("bundle appears in both {other} and {register}"),
                                ));
                            }
                        }
                        OccupancyOwner::Fixed(id) => {
                            let reservation = FixedRegisterReservation {
                                register,
                                segment: LiveSegment {
                                    block: block_id,
                                    start,
                                    end: entry.end,
                                },
                            };
                            if fixed.insert(id, reservation).is_some() {
                                return Err(IntervalUnionError::new(
                                    "INTERVAL_UNION.FIXED_IDENTITY",
                                    Some(block_id),
                                    [],
                                    "one fixed reservation identity owns multiple intervals",
                                ));
                            }
                        }
                    }
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
        let expected_fixed = self
            .fixed_reservations
            .iter()
            .copied()
            .enumerate()
            .map(|(row, reservation)| {
                u32::try_from(row)
                    .map(FixedReservationId)
                    .map(|id| (id, reservation))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()
            .map_err(|_| {
                IntervalUnionError::new(
                    "INTERVAL_UNION.FIXED_RANGE",
                    None,
                    [],
                    "stored fixed reservation identity exceeds u32",
                )
            })?;
        if fixed != expected_fixed {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.FIXED_MAP",
                None,
                [],
                "fixed union memberships differ from the immutable reservation table",
            ));
        }
        Ok(())
    }
}

/// Sparse interference matrix with an allocation-owned, dynamically growing
/// color domain. Physical-register allocation has a fixed target register
/// set; stack-slot coloring instead creates a new color only when every
/// existing slot interferes with a home's exact CFG range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DynamicIntervalMatrix {
    index: Arc<IntervalIndex>,
    unions: Vec<IntervalUnion>,
    assignments: BTreeMap<AllocationBundleId, usize>,
}

impl DynamicIntervalMatrix {
    pub(super) fn new(cfg: &NormalizedCfg) -> Result<Self, IntervalUnionError> {
        Ok(Self {
            index: Arc::new(IntervalIndex::new(cfg)?),
            unions: Vec::new(),
            assignments: BTreeMap::new(),
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
        Ok(self
            .unions
            .iter()
            .position(|union| !union.interferes_indexed(segments))
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
        if slot == self.unions.len() {
            let mut union = IntervalUnion::new(Arc::clone(&self.index));
            union.insert_indexed(OccupancyOwner::Bundle(bundle), segments)?;
            self.unions.push(union);
        } else {
            self.unions[slot].insert_indexed(OccupancyOwner::Bundle(bundle), segments)?;
        }
        self.assignments.insert(bundle, slot);
        Ok(())
    }

    pub(super) fn slot(&self, bundle: AllocationBundleId) -> Option<usize> {
        self.assignments.get(&bundle).copied()
    }

    pub(super) fn slot_count(&self) -> usize {
        self.unions.len()
    }

    pub(super) fn verify(&self) -> Result<(), IntervalUnionError> {
        let mut rebuilt = BTreeMap::new();
        for (slot, union) in self.unions.iter().enumerate() {
            union.verify()?;
            if union.blocks.is_empty() {
                return Err(IntervalUnionError::new(
                    "INTERVAL_UNION.EMPTY_DYNAMIC_SLOT",
                    None,
                    [],
                    format!("dynamic slot {slot} has no assigned sparse range"),
                ));
            }
            for entries in union.blocks.values() {
                for entry in entries.values() {
                    let OccupancyOwner::Bundle(bundle) = entry.owner else {
                        return Err(IntervalUnionError::new(
                            "INTERVAL_UNION.DYNAMIC_FIXED",
                            None,
                            [],
                            "dynamic stack-color union contains a fixed register reservation",
                        ));
                    };
                    if let Some(other) = rebuilt.insert(bundle, slot)
                        && other != slot
                    {
                        return Err(IntervalUnionError::new(
                            "INTERVAL_UNION.DUPLICATE_ASSIGNMENT",
                            None,
                            [bundle],
                            format!("bundle appears in both dynamic slots {other} and {slot}"),
                        ));
                    }
                }
            }
        }
        if rebuilt != self.assignments {
            return Err(IntervalUnionError::new(
                "INTERVAL_UNION.ASSIGNMENT_MAP",
                None,
                [],
                "dynamic slot memberships differ from the bidirectional assignment map",
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
        let first_range = matrix.make_range(first.segments.clone()).unwrap();
        let second_range = matrix.make_range(second.segments.clone()).unwrap();
        matrix
            .assign_validated(AllocationBundleId(0), PhysReg::RAX, first_range.validated())
            .unwrap();
        let before = matrix.clone();
        let error = matrix
            .assign_validated(
                AllocationBundleId(1),
                PhysReg::RAX,
                second_range.validated(),
            )
            .unwrap_err();
        assert_eq!(error.rule, "INTERVAL_UNION.INTERFERENCE");
        assert_eq!(matrix, before);
        matrix.verify().unwrap();
    }

    #[test]
    fn empty_register_assignment_is_rejected_without_a_phantom_membership() {
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::Return);
        let mut function = function(0, vec![block]);
        let cfg = normalize(&mut function);
        let mut matrix = LiveIntervalMatrix::new(&cfg, &[PhysReg::RAX]).unwrap();

        let error = matrix
            .assign(AllocationBundleId(0), PhysReg::RAX, &[])
            .unwrap_err();
        assert_eq!(error.rule, "INTERVAL_UNION.EMPTY_ASSIGNMENT");
        assert_eq!(matrix.register(AllocationBundleId(0)), None);
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
        let outer_range = matrix.make_range(outer.segments.clone()).unwrap();
        let free = matrix.free_segments(PhysReg::RAX, &outer.segments).unwrap();
        assert_eq!(
            free,
            matrix
                .free_segments_validated(PhysReg::RAX, outer_range.validated())
                .unwrap()
        );
        assert_eq!(
            matrix.conflicts(PhysReg::RAX, &outer.segments).unwrap(),
            matrix
                .conflicts_validated(PhysReg::RAX, outer_range.validated())
                .unwrap()
        );
        assert_eq!(
            matrix.interferes(PhysReg::RAX, &outer.segments).unwrap(),
            matrix
                .interferes_validated(PhysReg::RAX, outer_range.validated())
                .unwrap()
        );
        let mut raw_collector = ConflictCollector::default();
        let mut raw_conflicts = Vec::new();
        matrix
            .collect_conflicts(
                PhysReg::RAX,
                &outer.segments,
                2,
                &mut raw_collector,
                &mut raw_conflicts,
            )
            .unwrap();
        let mut indexed_collector = ConflictCollector::default();
        let mut indexed_conflicts = Vec::new();
        matrix
            .collect_conflicts_validated(
                PhysReg::RAX,
                outer_range.validated(),
                2,
                &mut indexed_collector,
                &mut indexed_conflicts,
            )
            .unwrap();
        assert_eq!(raw_conflicts, indexed_conflicts);
        let mut projected_conflicts = Vec::new();
        let mut cuts = Vec::new();
        matrix
            .collect_interference_validated(
                PhysReg::RAX,
                outer_range.validated(),
                2,
                &mut indexed_collector,
                &mut projected_conflicts,
                &mut cuts,
            )
            .unwrap();
        assert_eq!(projected_conflicts, indexed_conflicts);
        assert_eq!(
            cuts,
            vec![OccupancyCut {
                segment: 0,
                start: inner.segments[0].start,
                end: inner.segments[0].end,
                owner: OccupancyOwner::Bundle(AllocationBundleId(1)),
            }]
        );
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
    fn fixed_clobber_interval_excludes_only_live_through_ranges() {
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: VReg(0),
            value: 8,
        });
        block.push(MInst::LoadImm {
            dst: VReg(1),
            value: 2,
        });
        block.push(MInst::LoadImm {
            dst: VReg(2),
            value: 3,
        });
        block.push(MInst::UDiv {
            dst: VReg(3),
            lhs: VReg(0),
            rhs: VReg(1),
        });
        block.push(MInst::Add {
            dst: VReg(4),
            lhs: VReg(3),
            rhs: VReg(2),
        });
        block.push(MInst::Return);
        let mut function = function(5, vec![block]);
        let cfg = normalize(&mut function);
        let intervals = live_interval::analyze(&function, &cfg).unwrap();
        let slots = &intervals.block_slots[0];
        let reservation = FixedRegisterReservation {
            register: PhysReg::RAX,
            segment: LiveSegment {
                block: BlockId(0),
                start: slots.instruction_clobber(3).unwrap(),
                end: slots.instruction_def(3).unwrap(),
            },
        };
        let mut matrix = LiveIntervalMatrix::new(&cfg, &[PhysReg::RAX]).unwrap();
        matrix.replace_fixed_reservations(&[reservation]).unwrap();

        let lhs = intervals.intervals[0].as_ref().unwrap();
        let live_through = intervals.intervals[2].as_ref().unwrap();
        let result = intervals.intervals[3].as_ref().unwrap();
        assert!(!matrix.interferes(PhysReg::RAX, &lhs.segments).unwrap());
        assert!(
            matrix
                .interferes(PhysReg::RAX, &live_through.segments)
                .unwrap()
        );
        assert!(!matrix.interferes(PhysReg::RAX, &result.segments).unwrap());

        let range = matrix.make_range(live_through.segments.clone()).unwrap();
        assert!(
            !matrix
                .interferes_bundle_validated(PhysReg::RAX, range.validated())
                .unwrap(),
            "immutable clobber occupancy must not masquerade as a movable resident"
        );
        let mut collector = ConflictCollector::default();
        let mut conflicts = Vec::new();
        let mut cuts = Vec::new();
        matrix
            .collect_interference_validated(
                PhysReg::RAX,
                range.validated(),
                5,
                &mut collector,
                &mut conflicts,
                &mut cuts,
            )
            .unwrap();
        assert!(conflicts.is_empty());
        assert_eq!(
            cuts,
            vec![OccupancyCut {
                segment: 0,
                start: reservation.segment.start,
                end: reservation.segment.end,
                owner: OccupancyOwner::Fixed(FixedReservationId(0)),
            }]
        );
        let lhs_range = matrix.make_range(lhs.segments.clone()).unwrap();
        matrix
            .assign_validated(AllocationBundleId(0), PhysReg::RAX, lhs_range.validated())
            .unwrap();
        assert!(
            matrix
                .interferes_bundle_validated(PhysReg::RAX, lhs_range.validated())
                .unwrap()
        );
        matrix.verify().unwrap();
    }

    #[test]
    fn sparse_range_is_validated_once_and_bound_to_its_cfg_index() {
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: VReg(0),
            value: 1,
        });
        block.push(MInst::Mov {
            dst: VReg(1),
            src: VReg(0),
        });
        block.push(MInst::Return);
        let mut first_function = function(2, vec![block.clone()]);
        let first_cfg = normalize(&mut first_function);
        let intervals = live_interval::analyze(&first_function, &first_cfg).unwrap();
        let source = intervals.intervals[0].as_ref().unwrap();
        let first_matrix = LiveIntervalMatrix::new(&first_cfg, &[PhysReg::RAX]).unwrap();
        let range = first_matrix.make_range(source.segments.clone()).unwrap();

        let mut invalid = source.segments.clone();
        invalid[0].block = BlockId(99);
        let error = first_matrix.make_range(invalid).unwrap_err();
        assert_eq!(error.rule, "INTERVAL_UNION.SEGMENT_BLOCK");

        let mut second_function = function(2, vec![block]);
        let second_cfg = normalize(&mut second_function);
        let second_matrix = LiveIntervalMatrix::new(&second_cfg, &[PhysReg::RAX]).unwrap();
        let error = second_matrix
            .interferes_validated(PhysReg::RAX, range.validated())
            .unwrap_err();
        assert_eq!(error.rule, "INTERVAL_UNION.RANGE_CFG");
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
        let source_range = matrix.make_range(source.segments.clone()).unwrap();
        matrix
            .assign_validated(
                AllocationBundleId(0),
                PhysReg::RAX,
                source_range.validated(),
            )
            .unwrap();
        matrix
            .assign(AllocationBundleId(1), PhysReg::RDX, &destination.segments)
            .unwrap();
        let removed_from = matrix
            .unassign_validated(AllocationBundleId(0), source_range.validated())
            .unwrap();
        assert_eq!(removed_from, PhysReg::RAX);
        assert_eq!(matrix.register(AllocationBundleId(0)), None);
        assert_eq!(matrix.register(AllocationBundleId(1)), Some(PhysReg::RDX));
        matrix.verify().unwrap();
    }
}
