//! Exact sparse live intervals for allocation-owned splitting.
//!
//! This model deliberately separates semantic liveness from next-use and
//! spill-cost heuristics.  Every block entry, phi definition, instruction use,
//! instruction definition, phi edge use, and block exit has a stable slot.
//! A value may have one segment per block; mutually exclusive CFG arms do not
//! interfere merely because their blocks are adjacent in layout.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use fxhash::FxHashSet;

use crate::backend::native::mir::{BlockId, MFunction, Uses, VReg};

use super::cfg::NormalizedCfg;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct SlotIndex(u64);

impl SlotIndex {
    const STABLE_MARKER: u64 = 1_u64 << 63;
    const STABLE_ZONE_SHIFT: u32 = 34;
    const STABLE_SEQUENCE_SHIFT: u32 = 2;
    const STABLE_ZONE_LIMIT: u64 = 1_u64 << 29;

    pub(super) fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }

    pub(super) fn distance_to(self, end: Self) -> Option<u64> {
        if self.0 & Self::STABLE_MARKER == 0 || end.0 & Self::STABLE_MARKER == 0 {
            return end.0.checked_sub(self.0);
        }
        let (start_zone, start_sequence, start_phase) = self.stable_parts()?;
        let (end_zone, end_sequence, end_phase) = end.stable_parts()?;
        if start_zone == end_zone {
            let start = u64::from(start_sequence)
                .checked_mul(4)?
                .checked_add(u64::from(start_phase))?;
            let end = u64::from(end_sequence)
                .checked_mul(4)?
                .checked_add(u64::from(end_phase))?;
            end.checked_sub(start)
        } else {
            // Stable labels deliberately reserve a large numeric namespace for
            // future insertions. Spill priority must measure program order,
            // not those empty label gaps, so cross-anchor distance uses only
            // the immutable anchor zones.
            end_zone.checked_sub(start_zone)?.checked_mul(4)
        }
    }

    pub(super) fn stable(zone: u64, sequence: u32, phase: u8) -> Option<Self> {
        if zone >= Self::STABLE_ZONE_LIMIT || phase >= 4 {
            return None;
        }
        Some(Self(
            Self::STABLE_MARKER
                | (zone << Self::STABLE_ZONE_SHIFT)
                | (u64::from(sequence) << Self::STABLE_SEQUENCE_SHIFT)
                | u64::from(phase),
        ))
    }

    pub(super) fn stable_entry() -> Self {
        Self(Self::STABLE_MARKER)
    }

    pub(super) fn stable_phi_def() -> Self {
        Self(Self::STABLE_MARKER | 1)
    }

    fn stable_parts(self) -> Option<(u64, u32, u8)> {
        (self.0 & Self::STABLE_MARKER != 0).then(|| {
            let payload = self.0 & !Self::STABLE_MARKER;
            (
                payload >> Self::STABLE_ZONE_SHIFT,
                ((payload >> Self::STABLE_SEQUENCE_SHIFT) & u64::from(u32::MAX)) as u32,
                (payload & 3) as u8,
            )
        })
    }

    /// Immutable source-MIR anchor zone for one allocation-owned program
    /// point. Sequence labels may be inserted within a zone, but doing so does
    /// not cross another real machine boundary.
    pub(super) fn stable_zone(self) -> Option<u64> {
        self.stable_parts().map(|(zone, _, _)| zone)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct InstructionSlots {
    use_: SlotIndex,
    clobber: SlotIndex,
    def: SlotIndex,
}

impl InstructionSlots {
    pub(super) fn stable(zone: u64, sequence: u32) -> Option<Self> {
        Some(Self {
            use_: SlotIndex::stable(zone, sequence, 0)?,
            clobber: SlotIndex::stable(zone, sequence, 1)?,
            def: SlotIndex::stable(zone, sequence, 2)?,
        })
    }

    pub(super) fn use_slot(self) -> SlotIndex {
        self.use_
    }

    pub(super) fn definition_slot(self) -> SlotIndex {
        self.def
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BlockSlots {
    pub entry: SlotIndex,
    pub phi_def: SlotIndex,
    pub exit: SlotIndex,
    instructions: Vec<InstructionSlots>,
}

impl BlockSlots {
    pub fn instruction_use(&self, instruction: usize) -> Option<SlotIndex> {
        self.instructions.get(instruction).map(|slots| slots.use_)
    }

    /// Target resources clobbered after operands are consumed and before the
    /// instruction result becomes available.
    pub fn instruction_clobber(&self, instruction: usize) -> Option<SlotIndex> {
        self.instructions
            .get(instruction)
            .map(|slots| slots.clobber)
    }

    pub fn instruction_def(&self, instruction: usize) -> Option<SlotIndex> {
        self.instructions.get(instruction).map(|slots| slots.def)
    }

    fn program_order_rank(&self, slot: SlotIndex) -> Option<u64> {
        if slot == self.entry {
            return Some(0);
        }
        if slot == self.phi_def {
            return Some(1);
        }
        let instruction = self
            .instructions
            .partition_point(|candidate| candidate.use_ <= slot)
            .checked_sub(1);
        if let Some(instruction) = instruction {
            let slots = &self.instructions[instruction];
            let base = u64::try_from(instruction)
                .ok()?
                .checked_mul(3)?
                .checked_add(2)?;
            if slot == slots.use_ {
                return Some(base);
            }
            if slot == slots.clobber {
                return base.checked_add(1);
            }
            if slot == slots.def {
                return base.checked_add(2);
            }
            if slots.def.next() == Some(slot) {
                return base.checked_add(3);
            }
        }
        let exit = u64::try_from(self.instructions.len())
            .ok()?
            .checked_mul(3)?
            .checked_add(2)?;
        if slot == self.exit {
            Some(exit)
        } else if self.exit.next() == Some(slot) {
            exit.checked_add(1)
        } else {
            None
        }
    }

    pub(super) fn program_order_distance(&self, start: SlotIndex, end: SlotIndex) -> Option<u64> {
        let start = self.program_order_rank(start)?;
        self.program_order_rank(end)?.checked_sub(start)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum DefinitionSite {
    Phi {
        block: BlockId,
        phi: usize,
        slot: SlotIndex,
    },
    Instruction {
        block: BlockId,
        instruction: usize,
        slot: SlotIndex,
    },
}

impl DefinitionSite {
    pub(super) fn block(self) -> BlockId {
        match self {
            Self::Phi { block, .. } | Self::Instruction { block, .. } => block,
        }
    }

    pub(super) fn slot(self) -> SlotIndex {
        match self {
            Self::Phi { slot, .. } | Self::Instruction { slot, .. } => slot,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UseSite {
    Instruction {
        block: BlockId,
        instruction: usize,
        slot: SlotIndex,
    },
    PhiEdge {
        predecessor: BlockId,
        successor: BlockId,
        phi: usize,
        slot: SlotIndex,
    },
}

impl Ord for UseSite {
    fn cmp(&self, other: &Self) -> Ordering {
        match (*self, *other) {
            (
                Self::Instruction {
                    block: left_block,
                    instruction: left_instruction,
                    slot: left_slot,
                },
                Self::Instruction {
                    block: right_block,
                    instruction: right_instruction,
                    slot: right_slot,
                },
            ) => (left_block, left_slot, left_instruction).cmp(&(
                right_block,
                right_slot,
                right_instruction,
            )),
            (Self::Instruction { .. }, Self::PhiEdge { .. }) => Ordering::Less,
            (Self::PhiEdge { .. }, Self::Instruction { .. }) => Ordering::Greater,
            (
                Self::PhiEdge {
                    predecessor: left_predecessor,
                    successor: left_successor,
                    phi: left_phi,
                    slot: left_slot,
                },
                Self::PhiEdge {
                    predecessor: right_predecessor,
                    successor: right_successor,
                    phi: right_phi,
                    slot: right_slot,
                },
            ) => (left_predecessor, left_successor, left_phi, left_slot).cmp(&(
                right_predecessor,
                right_successor,
                right_phi,
                right_slot,
            )),
        }
    }
}

impl PartialOrd for UseSite {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl UseSite {
    pub(super) fn block(self) -> BlockId {
        match self {
            Self::Instruction { block, .. } => block,
            Self::PhiEdge { predecessor, .. } => predecessor,
        }
    }

    pub(super) fn slot(self) -> SlotIndex {
        match self {
            Self::Instruction { slot, .. } | Self::PhiEdge { slot, .. } => slot,
        }
    }

    /// Compare immutable allocation coordinates while ignoring the mutable
    /// dense lowering position carried by instruction uses.
    fn coordinate_cmp(self, other: Self) -> Ordering {
        match (self, other) {
            (
                Self::Instruction {
                    block: left_block,
                    slot: left_slot,
                    ..
                },
                Self::Instruction {
                    block: right_block,
                    slot: right_slot,
                    ..
                },
            ) => (left_block, left_slot).cmp(&(right_block, right_slot)),
            (Self::Instruction { .. }, Self::PhiEdge { .. }) => Ordering::Less,
            (Self::PhiEdge { .. }, Self::Instruction { .. }) => Ordering::Greater,
            (
                Self::PhiEdge {
                    predecessor: left_predecessor,
                    successor: left_successor,
                    phi: left_phi,
                    slot: left_slot,
                },
                Self::PhiEdge {
                    predecessor: right_predecessor,
                    successor: right_successor,
                    phi: right_phi,
                    slot: right_slot,
                },
            ) => (left_predecessor, left_successor, left_phi, left_slot).cmp(&(
                right_predecessor,
                right_successor,
                right_phi,
                right_slot,
            )),
        }
    }

    pub(super) fn same_coordinate(self, other: Self) -> bool {
        self.coordinate_cmp(other) == Ordering::Equal
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LiveSegment {
    pub block: BlockId,
    pub start: SlotIndex,
    pub end: SlotIndex,
}

impl LiveSegment {
    pub(super) fn contains(self, slot: SlotIndex) -> bool {
        self.start <= slot && slot < self.end
    }

    pub(super) fn overlaps(self, other: Self) -> bool {
        self.block == other.block && self.start < other.end && other.start < self.end
    }
}

/// Canonical immutable use row shared by the fact index and live interval.
///
/// Split transactions replace a complete ordered row atomically. Sharing that
/// row avoids retaining and copying a second all-use vector in `LiveInterval`
/// while still giving readers a stable contiguous slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SharedUseSites(Arc<[UseSite]>);

impl SharedUseSites {
    pub(super) fn as_slice(&self) -> &[UseSite] {
        &self.0
    }
}

impl Default for SharedUseSites {
    fn default() -> Self {
        Self(Arc::from([]))
    }
}

impl From<Vec<UseSite>> for SharedUseSites {
    fn from(sites: Vec<UseSite>) -> Self {
        Self(Arc::from(sites))
    }
}

impl Deref for SharedUseSites {
    type Target = [UseSite];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<'a> IntoIterator for &'a SharedUseSites {
    type Item = &'a UseSite;
    type IntoIter = std::slice::Iter<'a, UseSite>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LiveInterval {
    pub value: VReg,
    pub definition: DefinitionSite,
    pub segments: Vec<LiveSegment>,
    pub(super) uses: SharedUseSites,
}

impl LiveInterval {
    pub fn covers(&self, block: BlockId, slot: SlotIndex) -> bool {
        self.segments
            .iter()
            .any(|segment| segment.block == block && segment.contains(slot))
    }

    pub fn interferes(&self, other: &Self) -> bool {
        let mut left = 0;
        let mut right = 0;
        while left < self.segments.len() && right < other.segments.len() {
            let a = self.segments[left];
            let b = other.segments[right];
            if a.overlaps(b) {
                return true;
            }
            if (a.block, a.end) <= (b.block, b.end) {
                left += 1;
            } else {
                right += 1;
            }
        }
        false
    }

    pub(super) fn contains_use_coordinate(&self, site: UseSite) -> bool {
        self.uses
            .binary_search_by(|candidate| candidate.coordinate_cmp(site))
            .is_ok()
    }

    /// Whether two interval rows describe the same physical allocation
    /// geometry. Dense instruction indices are lowering metadata; stable
    /// definition/use slots and sparse segments are the allocation identity.
    pub(super) fn same_range_geometry(&self, other: &Self) -> bool {
        self.value == other.value
            && same_definition_coordinate(self.definition, other.definition)
            && self.segments == other.segments
            && self.uses.len() == other.uses.len()
            && self
                .uses
                .iter()
                .copied()
                .zip(other.uses.iter().copied())
                .all(|(left, right)| same_use_coordinate(left, right))
    }

    /// Publish current lowering coordinates while retaining allocation-owned
    /// sparse geometry and its interval-union token.
    pub(super) fn relabel_from(&mut self, other: &Self) -> bool {
        if !self.same_range_geometry(other) {
            return false;
        }
        self.definition = other.definition;
        self.uses.clone_from(&other.uses);
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LiveIntervals {
    pub block_slots: Vec<BlockSlots>,
    pub intervals: Vec<Option<LiveInterval>>,
}

impl LiveIntervals {
    /// Compare exact physical liveness after mapping two legal slot-label
    /// namespaces onto their emitted instruction order. Allocation IR uses
    /// stable labels with insertion gaps; materialized MIR uses compact labels.
    pub(super) fn equivalent_program_order(&self, other: &Self, cfg: &NormalizedCfg) -> bool {
        if self.block_slots.len() != other.block_slots.len()
            || self.block_slots.len() != cfg.block_index.len()
            || self.intervals.len() != other.intervals.len()
        {
            return false;
        }
        self.intervals
            .iter()
            .zip(&other.intervals)
            .all(|(left, right)| match (left, right) {
                (None, None) => true,
                (Some(left), Some(right)) => {
                    left.value == right.value
                        && equivalent_definition(
                            left.definition,
                            right.definition,
                            &self.block_slots,
                            &other.block_slots,
                            cfg,
                        )
                        && left.uses.len() == right.uses.len()
                        && left
                            .uses
                            .iter()
                            .copied()
                            .zip(right.uses.iter().copied())
                            .all(|(left, right)| {
                                equivalent_use(
                                    left,
                                    right,
                                    &self.block_slots,
                                    &other.block_slots,
                                    cfg,
                                )
                            })
                        && left.segments.len() == right.segments.len()
                        && left
                            .segments
                            .iter()
                            .zip(&right.segments)
                            .all(|(left, right)| {
                                if left.block != right.block {
                                    return false;
                                }
                                let Some(&block) = cfg.block_index.get(&left.block) else {
                                    return false;
                                };
                                self.block_slots[block].program_order_rank(left.start)
                                    == other.block_slots[block].program_order_rank(right.start)
                                    && self.block_slots[block].program_order_rank(left.end)
                                        == other.block_slots[block].program_order_rank(right.end)
                            })
                }
                _ => false,
            })
    }
}

fn equivalent_definition(
    left: DefinitionSite,
    right: DefinitionSite,
    left_slots: &[BlockSlots],
    right_slots: &[BlockSlots],
    cfg: &NormalizedCfg,
) -> bool {
    let (left_block, left_identity, left_slot) = match left {
        DefinitionSite::Phi { block, phi, slot } => (block, Some(phi), slot),
        DefinitionSite::Instruction { block, slot, .. } => (block, None, slot),
    };
    let (right_block, right_identity, right_slot) = match right {
        DefinitionSite::Phi { block, phi, slot } => (block, Some(phi), slot),
        DefinitionSite::Instruction { block, slot, .. } => (block, None, slot),
    };
    let Some(&block) = cfg.block_index.get(&left_block) else {
        return false;
    };
    left_block == right_block
        && left_identity == right_identity
        && left_slots[block].program_order_rank(left_slot)
            == right_slots[block].program_order_rank(right_slot)
}

fn equivalent_use(
    left: UseSite,
    right: UseSite,
    left_slots: &[BlockSlots],
    right_slots: &[BlockSlots],
    cfg: &NormalizedCfg,
) -> bool {
    match (left, right) {
        (
            UseSite::Instruction {
                block: left_block,
                slot: left_slot,
                ..
            },
            UseSite::Instruction {
                block: right_block,
                slot: right_slot,
                ..
            },
        ) => {
            let Some(&block) = cfg.block_index.get(&left_block) else {
                return false;
            };
            left_block == right_block
                && left_slots[block].program_order_rank(left_slot)
                    == right_slots[block].program_order_rank(right_slot)
        }
        (
            UseSite::PhiEdge {
                predecessor: left_predecessor,
                successor: left_successor,
                phi: left_phi,
                slot: left_slot,
            },
            UseSite::PhiEdge {
                predecessor: right_predecessor,
                successor: right_successor,
                phi: right_phi,
                slot: right_slot,
            },
        ) => {
            let Some(&block) = cfg.block_index.get(&left_predecessor) else {
                return false;
            };
            (left_predecessor, left_successor, left_phi)
                == (right_predecessor, right_successor, right_phi)
                && left_slots[block].program_order_rank(left_slot)
                    == right_slots[block].program_order_rank(right_slot)
        }
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LiveIntervalError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub values: Vec<VReg>,
    pub message: String,
}

impl LiveIntervalError {
    fn new(
        rule: &'static str,
        block: Option<BlockId>,
        instruction: Option<usize>,
        values: Vec<VReg>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            block,
            instruction,
            values,
            message: message.into(),
        }
    }
}

impl fmt::Display for LiveIntervalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.rule)?;
        if let Some(block) = self.block {
            write!(formatter, " at {block}")?;
        }
        if let Some(instruction) = self.instruction {
            write!(formatter, "/i{instruction}")?;
        }
        if !self.values.is_empty() {
            write!(formatter, " values={:?}", self.values)?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for LiveIntervalError {}

#[derive(Default)]
struct BlockFacts {
    definitions: HashSet<VReg>,
    upward_uses: HashSet<VReg>,
    last_use: HashMap<VReg, SlotIndex>,
}

struct ModelFacts {
    definitions: Vec<Option<DefinitionSite>>,
    uses: Vec<Vec<UseSite>>,
    blocks: Vec<BlockFacts>,
    phi_definitions: Vec<HashSet<VReg>>,
    edge_uses: HashMap<(usize, usize), HashSet<VReg>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct IndexedBlockFacts {
    definitions: Vec<(VReg, DefinitionSite)>,
    uses: Vec<(VReg, UseSite)>,
}

#[derive(Default)]
struct IndexedBlockFactDelta {
    removed_definitions: Vec<(VReg, DefinitionSite)>,
    added_definitions: Vec<(VReg, DefinitionSite)>,
    removed_uses: Vec<(VReg, UseSite)>,
    added_uses: Vec<(VReg, UseSite)>,
}

/// Exact stable def/use transaction emitted by allocation IR mutations.
///
/// The old block-delta API remains as an independent debug oracle. Optimized
/// allocation consumes this journal directly, so rewriting one operand does
/// not rescan and compare every unrelated instruction in the same RTL block.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct LivenessFactDelta {
    pub changed_blocks: BTreeSet<BlockId>,
    /// Blocks whose ordered instruction snapshot must be republished. Stable
    /// slots make individual dense insertion positions irrelevant.
    pub layout_blocks: BTreeSet<BlockId>,
    pub removed_definitions: Vec<(VReg, DefinitionSite)>,
    pub added_definitions: Vec<(VReg, DefinitionSite)>,
    pub removed_uses: Vec<(VReg, UseSite)>,
    pub added_uses: Vec<(VReg, UseSite)>,
}

impl LivenessFactDelta {
    pub(super) fn is_empty(&self) -> bool {
        self.layout_blocks.is_empty()
            && self.removed_definitions.is_empty()
            && self.added_definitions.is_empty()
            && self.removed_uses.is_empty()
            && self.added_uses.is_empty()
    }
}

/// Mutable strict-SSA fact index used by allocation-owned splitting.
///
/// Facts are owned by the block in which the machine value is physically
/// live: instruction rows by their block and phi sources by their predecessor.
/// Rebuilding one changed block therefore updates exact def/use rows without
/// solving a function-wide set equation. Values whose old interval crossed
/// the changed block are recomputed independently by sparse reverse CFG walk.
#[derive(Debug, Clone)]
pub(super) struct IncrementalLiveness {
    block_facts: Vec<IndexedBlockFacts>,
    definitions: Vec<Option<DefinitionSite>>,
    uses: Vec<SharedUseSites>,
    /// Dense-slot programs must revisit values crossing a relabeled block.
    /// Stable allocation IR never renumbers physical coordinates, so it does
    /// not materialize this potentially enormous value-by-block relation.
    block_members: Option<Vec<FxHashSet<VReg>>>,
    dominators: DominatorIntervals,
    /// Exact emitted-order length for every active sparse interval. A changed
    /// block updates only its resident segment contribution; changed geometry
    /// is rebuilt once from the new sparse range.
    program_order_lengths: Vec<Option<u64>>,
    /// Reused epoch-marked CFG workspace for one-value sparse interval
    /// reconstruction. This is capacity, not semantic analysis state.
    interval_scratch: SparseIntervalScratch,
}

impl PartialEq for IncrementalLiveness {
    fn eq(&self, other: &Self) -> bool {
        self.block_facts == other.block_facts
            && self.definitions == other.definitions
            && self.uses == other.uses
            && self.block_members == other.block_members
            && self.dominators == other.dominators
            && self.program_order_lengths == other.program_order_lengths
    }
}

impl Eq for IncrementalLiveness {}

#[derive(Debug, Clone)]
struct SparseIntervalScratch {
    epoch: u32,
    live_in: Vec<u32>,
    live_out: Vec<u32>,
    touched: Vec<u32>,
    last_use_epoch: Vec<u32>,
    last_use: Vec<SlotIndex>,
    queue: Vec<usize>,
    live_blocks: Vec<usize>,
}

impl SparseIntervalScratch {
    fn new(block_count: usize) -> Self {
        Self {
            epoch: 0,
            live_in: vec![0; block_count],
            live_out: vec![0; block_count],
            touched: vec![0; block_count],
            last_use_epoch: vec![0; block_count],
            last_use: vec![SlotIndex::stable_entry(); block_count],
            queue: Vec::new(),
            live_blocks: Vec::new(),
        }
    }

    fn begin(&mut self, block_count: usize) {
        if self.live_in.len() != block_count {
            *self = Self::new(block_count);
        }
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.live_in.fill(0);
            self.live_out.fill(0);
            self.touched.fill(0);
            self.last_use_epoch.fill(0);
            self.epoch = 1;
        }
        self.queue.clear();
        self.live_blocks.clear();
    }

    fn touch(&mut self, block: usize) {
        if self.touched[block] != self.epoch {
            self.touched[block] = self.epoch;
            self.live_blocks.push(block);
        }
    }

    fn mark_live_in(&mut self, block: usize) -> bool {
        self.touch(block);
        if self.live_in[block] == self.epoch {
            false
        } else {
            self.live_in[block] = self.epoch;
            true
        }
    }

    fn mark_live_out(&mut self, block: usize) {
        self.touch(block);
        self.live_out[block] = self.epoch;
    }

    fn record_last_use(&mut self, block: usize, slot: SlotIndex) {
        self.touch(block);
        if self.last_use_epoch[block] != self.epoch {
            self.last_use_epoch[block] = self.epoch;
            self.last_use[block] = slot;
        } else {
            self.last_use[block] = self.last_use[block].max(slot);
        }
    }

    fn is_live_out(&self, block: usize) -> bool {
        self.live_out[block] == self.epoch
    }

    fn last_use(&self, block: usize) -> Option<SlotIndex> {
        (self.last_use_epoch[block] == self.epoch).then_some(self.last_use[block])
    }
}

/// Exact result of one allocation-IR liveness update.
///
/// `changed_values` includes rows whose dense lowering coordinates changed.
/// `range_changed_values` is the strict subset whose physical sparse range
/// changed and therefore must be removed from and reinserted into regalloc.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct IncrementalLivenessUpdate {
    pub changed_values: Vec<VReg>,
    pub range_changed_values: Vec<VReg>,
    /// Current absolute emitted-order length for every exact row change.
    /// `None` represents an interval removed by the update.
    pub live_lengths: Vec<(VReg, Option<u64>)>,
}

impl IncrementalLivenessUpdate {
    pub(super) fn extend(&mut self, other: Self) {
        self.changed_values.extend(other.changed_values);
        self.changed_values.sort_unstable();
        self.changed_values.dedup();
        self.range_changed_values.extend(other.range_changed_values);
        self.range_changed_values.sort_unstable();
        self.range_changed_values.dedup();
        let mut lengths = std::mem::take(&mut self.live_lengths)
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        lengths.extend(other.live_lengths);
        self.live_lengths = lengths.into_iter().collect();
    }
}

/// Minimal strict-SSA program view required by exact live-interval analysis.
/// Production MIR and the off-to-the-side allocation IR share this interface,
/// so synthetic values cannot bypass CFG, phi-edge, or dominance liveness.
pub(super) trait LivenessProgram {
    fn value_count(&self) -> u32;
    fn block_count(&self) -> usize;
    fn block_id(&self, block: usize) -> BlockId;
    fn phi_count(&self, block: usize) -> usize;
    fn phi_definition(&self, block: usize, phi: usize) -> VReg;
    fn phi_definition_in_register(&self, _block: usize, _phi: usize) -> bool {
        true
    }
    fn phi_sources(&self, block: usize, phi: usize) -> &[(BlockId, VReg)];
    /// Whether one semantic phi source must reside in a register at the edge.
    /// Ordinary MIR sources do. Allocation IR may instead resolve a source to
    /// an explicit stack/immediate edge location, in which case it must not
    /// create artificial simultaneous register pressure with sibling rows.
    fn phi_source_in_register(&self, _block: usize, _phi: usize, _source: usize) -> bool {
        true
    }
    /// Additional edge uses which do not define an ordinary MIR phi result.
    /// Allocation-owned location liveness uses this for direct stack sources
    /// consumed by out-of-SSA copies.
    fn extra_phi_edge_use_count(&self, _successor: usize) -> usize {
        0
    }
    fn extra_phi_edge_use(&self, _successor: usize, _edge_use: usize) -> (BlockId, VReg, usize) {
        unreachable!("program reports no additional phi-edge uses")
    }
    fn instruction_count(&self, block: usize) -> usize;
    /// Stable semantic identity stored in def/use metadata. Immutable MIR uses
    /// its dense position; allocation IR overrides this with original or
    /// synthetic instruction identity so insertions do not relabel all facts.
    fn instruction_identity(&self, _block: usize, instruction: usize) -> Option<usize> {
        Some(instruction)
    }
    fn instruction_uses(&self, block: usize, instruction: usize) -> Uses;
    fn instruction_definition(&self, block: usize, instruction: usize) -> Option<VReg>;
    /// Allocation IR overrides these points with stable order-maintenance
    /// labels. Ordinary immutable MIR uses compact block-local coordinates.
    fn block_entry_slot(&self, _block: usize) -> Option<SlotIndex> {
        Some(SlotIndex(0))
    }
    fn phi_definition_slot(&self, _block: usize) -> Option<SlotIndex> {
        Some(SlotIndex(1))
    }
    fn instruction_use_slot(&self, _block: usize, instruction: usize) -> Option<SlotIndex> {
        u64::try_from(instruction)
            .ok()?
            .checked_mul(3)?
            .checked_add(2)
            .map(SlotIndex)
    }
    fn block_exit_slot(&self, block: usize) -> Option<SlotIndex> {
        u64::try_from(self.instruction_count(block))
            .ok()?
            .checked_mul(3)?
            .checked_add(2)
            .map(SlotIndex)
    }
    fn has_stable_instruction_slots(&self) -> bool {
        false
    }
}

impl LivenessProgram for MFunction {
    fn value_count(&self) -> u32 {
        self.vregs.count()
    }

    fn block_count(&self) -> usize {
        self.blocks.len()
    }

    fn block_id(&self, block: usize) -> BlockId {
        self.blocks[block].id
    }

    fn phi_count(&self, block: usize) -> usize {
        self.blocks[block].phis.len()
    }

    fn phi_definition(&self, block: usize, phi: usize) -> VReg {
        self.blocks[block].phis[phi].dst
    }

    fn phi_sources(&self, block: usize, phi: usize) -> &[(BlockId, VReg)] {
        &self.blocks[block].phis[phi].sources
    }

    fn instruction_count(&self, block: usize) -> usize {
        self.blocks[block].insts.len()
    }

    fn instruction_uses(&self, block: usize, instruction: usize) -> Uses {
        self.blocks[block].insts[instruction].uses()
    }

    fn instruction_definition(&self, block: usize, instruction: usize) -> Option<VReg> {
        self.blocks[block].insts[instruction].def()
    }
}

pub(super) fn analyze(
    func: &MFunction,
    cfg: &NormalizedCfg,
) -> Result<LiveIntervals, LiveIntervalError> {
    analyze_program(func, cfg)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct NonRegisterPhiSource {
    pub predecessor: BlockId,
    pub successor: BlockId,
    pub phi: usize,
    pub value: VReg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct NonRegisterPhiDefinition {
    pub block: BlockId,
    pub phi: usize,
    pub value: VReg,
}

/// Independently rebuild physical-register liveness for lowered MIR whose
/// semantic phi rows include explicit stack/immediate edge locations.
pub(super) fn analyze_with_nonregister_phi_sources(
    func: &MFunction,
    cfg: &NormalizedCfg,
    nonregister_sources: &BTreeSet<NonRegisterPhiSource>,
    nonregister_definitions: &BTreeSet<NonRegisterPhiDefinition>,
) -> Result<LiveIntervals, LiveIntervalError> {
    for source in nonregister_sources {
        let successor = cfg
            .block_index
            .get(&source.successor)
            .copied()
            .ok_or_else(|| {
                LiveIntervalError::new(
                    "LIVE_INTERVAL.EDGE_LOCATION_BLOCK",
                    Some(source.successor),
                    None,
                    vec![source.value],
                    "non-register phi source references a successor outside normalized CFG",
                )
            })?;
        let phi = func.blocks[successor].phis.get(source.phi).ok_or_else(|| {
            LiveIntervalError::new(
                "LIVE_INTERVAL.EDGE_LOCATION_PHI",
                Some(source.successor),
                None,
                vec![source.value],
                "non-register edge location references a missing phi row",
            )
        })?;
        if phi
            .sources
            .iter()
            .filter(|(predecessor, value)| {
                *predecessor == source.predecessor && *value == source.value
            })
            .count()
            != 1
        {
            return Err(LiveIntervalError::new(
                "LIVE_INTERVAL.EDGE_LOCATION_SOURCE",
                Some(source.successor),
                None,
                vec![source.value],
                "non-register edge location does not identify one exact semantic phi source",
            ));
        }
    }
    for definition in nonregister_definitions {
        let block = cfg
            .block_index
            .get(&definition.block)
            .copied()
            .ok_or_else(|| {
                LiveIntervalError::new(
                    "LIVE_INTERVAL.PHI_LOCATION_BLOCK",
                    Some(definition.block),
                    None,
                    vec![definition.value],
                    "non-register phi definition references a block outside normalized CFG",
                )
            })?;
        if func.blocks[block]
            .phis
            .get(definition.phi)
            .is_none_or(|phi| phi.dst != definition.value)
        {
            return Err(LiveIntervalError::new(
                "LIVE_INTERVAL.PHI_LOCATION_DEFINITION",
                Some(definition.block),
                None,
                vec![definition.value],
                "non-register phi definition does not identify one exact semantic phi row",
            ));
        }
    }
    analyze_program(
        &FilteredPhiLiveness {
            func,
            nonregister_sources,
            nonregister_definitions,
        },
        cfg,
    )
}

struct FilteredPhiLiveness<'a> {
    func: &'a MFunction,
    nonregister_sources: &'a BTreeSet<NonRegisterPhiSource>,
    nonregister_definitions: &'a BTreeSet<NonRegisterPhiDefinition>,
}

impl LivenessProgram for FilteredPhiLiveness<'_> {
    fn value_count(&self) -> u32 {
        self.func.vregs.count()
    }

    fn block_count(&self) -> usize {
        self.func.blocks.len()
    }

    fn block_id(&self, block: usize) -> BlockId {
        self.func.blocks[block].id
    }

    fn phi_count(&self, block: usize) -> usize {
        self.func.blocks[block].phis.len()
    }

    fn phi_definition(&self, block: usize, phi: usize) -> VReg {
        self.func.blocks[block].phis[phi].dst
    }

    fn phi_definition_in_register(&self, block: usize, phi: usize) -> bool {
        let row = &self.func.blocks[block].phis[phi];
        !self
            .nonregister_definitions
            .contains(&NonRegisterPhiDefinition {
                block: self.func.blocks[block].id,
                phi,
                value: row.dst,
            })
    }

    fn phi_sources(&self, block: usize, phi: usize) -> &[(BlockId, VReg)] {
        &self.func.blocks[block].phis[phi].sources
    }

    fn phi_source_in_register(&self, block: usize, phi: usize, source: usize) -> bool {
        let successor = self.func.blocks[block].id;
        let (predecessor, value) = self.func.blocks[block].phis[phi].sources[source];
        !self.nonregister_sources.contains(&NonRegisterPhiSource {
            predecessor,
            successor,
            phi,
            value,
        })
    }

    fn instruction_count(&self, block: usize) -> usize {
        self.func.blocks[block].insts.len()
    }

    fn instruction_uses(&self, block: usize, instruction: usize) -> Uses {
        self.func.blocks[block].insts[instruction].uses()
    }

    fn instruction_definition(&self, block: usize, instruction: usize) -> Option<VReg> {
        self.func.blocks[block].insts[instruction].def()
    }
}

pub(super) fn analyze_program<P: LivenessProgram + ?Sized>(
    program: &P,
    cfg: &NormalizedCfg,
) -> Result<LiveIntervals, LiveIntervalError> {
    check_model_shape(program, cfg)?;
    let block_slots = assign_slots(program)?;
    let facts = collect_facts(program, cfg, &block_slots)?;
    let (live_in, live_out) = solve_liveness(program.block_count(), cfg, &facts);
    let intervals = build_intervals(program, cfg, &block_slots, &facts, &live_in, &live_out)?;
    let result = LiveIntervals {
        block_slots,
        intervals,
    };
    result.verify_program(program, cfg)?;
    Ok(result)
}

fn checked_program_order_length(
    interval: &LiveInterval,
    intervals: &LiveIntervals,
    cfg: &NormalizedCfg,
) -> Result<u64, LiveIntervalError> {
    let mut total = 0_u64;
    for segment in &interval.segments {
        let block = cfg
            .block_index
            .get(&segment.block)
            .copied()
            .ok_or_else(|| {
                LiveIntervalError::new(
                    "LIVE_INTERVAL.LENGTH_BLOCK",
                    Some(segment.block),
                    None,
                    vec![interval.value],
                    "live segment is outside the normalized CFG",
                )
            })?;
        let length = intervals.block_slots[block]
            .program_order_distance(segment.start, segment.end)
            .ok_or_else(|| {
                LiveIntervalError::new(
                    "LIVE_INTERVAL.LENGTH_SLOT",
                    Some(segment.block),
                    None,
                    vec![interval.value],
                    "live segment endpoints are outside emitted instruction order",
                )
            })?;
        total = total.checked_add(length).ok_or_else(|| {
            LiveIntervalError::new(
                "LIVE_INTERVAL.LENGTH_RANGE",
                Some(segment.block),
                None,
                vec![interval.value],
                "program-order live length exceeds u64",
            )
        })?;
    }
    if total == 0 {
        return Err(LiveIntervalError::new(
            "LIVE_INTERVAL.LENGTH_RANGE",
            Some(interval.definition.block()),
            None,
            vec![interval.value],
            "active interval has zero program-order length",
        ));
    }
    Ok(total)
}

impl IncrementalLiveness {
    pub(super) fn build<P: LivenessProgram + ?Sized>(
        program: &P,
        cfg: &NormalizedCfg,
        intervals: &LiveIntervals,
    ) -> Result<Self, LiveIntervalError> {
        check_model_shape(program, cfg)?;
        intervals.verify_program(program, cfg)?;
        let expected_slots = assign_slots(program)?;
        if intervals.block_slots != expected_slots
            || intervals.intervals.len() != program.value_count() as usize
        {
            return Err(LiveIntervalError::new(
                "LIVE_INTERVAL.INCREMENTAL_SHAPE",
                None,
                None,
                Vec::new(),
                "initial intervals do not cover the indexed liveness program",
            ));
        }

        let value_count = program.value_count() as usize;
        let mut definitions = vec![None; value_count];
        let mut uses = vec![Vec::new(); value_count];
        let mut block_facts = Vec::with_capacity(program.block_count());
        for block in 0..program.block_count() {
            let facts = scan_indexed_block(program, cfg, &expected_slots, block)?;
            add_indexed_facts(&mut definitions, &mut uses, &facts)?;
            block_facts.push(facts);
        }
        for value_uses in &mut uses {
            value_uses.sort_unstable();
            value_uses.dedup();
        }
        let uses = uses
            .into_iter()
            .map(SharedUseSites::from)
            .collect::<Vec<_>>();
        for (value, interval) in intervals.intervals.iter().enumerate() {
            match (definitions[value], interval) {
                (None, None) if uses[value].is_empty() => {}
                (Some(DefinitionSite::Phi { .. }), None) if uses[value].is_empty() => {}
                (Some(definition), Some(interval))
                    if interval.value == VReg(value as u32)
                        && interval.definition == definition
                        && interval.uses == uses[value] => {}
                _ => {
                    return Err(LiveIntervalError::new(
                        "LIVE_INTERVAL.INCREMENTAL_FACT_IDENTITY",
                        interval
                            .as_ref()
                            .map(|interval| interval.definition.block()),
                        None,
                        vec![VReg(value as u32)],
                        "indexed def/use facts differ from independently built intervals",
                    ));
                }
            }
        }

        let block_members = if program.has_stable_instruction_slots() {
            None
        } else {
            let mut block_members = (0..program.block_count())
                .map(|_| FxHashSet::default())
                .collect::<Vec<_>>();
            for interval in intervals.intervals.iter().flatten() {
                for segment in &interval.segments {
                    let block = cfg.block_index[&segment.block];
                    block_members[block].insert(interval.value);
                }
            }
            Some(block_members)
        };
        let program_order_lengths = intervals
            .intervals
            .iter()
            .map(|interval| {
                interval
                    .as_ref()
                    .map(|interval| checked_program_order_length(interval, intervals, cfg))
                    .transpose()
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            block_facts,
            definitions,
            uses,
            block_members,
            dominators: DominatorIntervals::build(program, cfg)?,
            program_order_lengths,
            interval_scratch: SparseIntervalScratch::new(program.block_count()),
        })
    }

    pub(super) fn program_order_length(&self, value: VReg) -> Option<u64> {
        self.program_order_lengths
            .get(value.0 as usize)
            .copied()
            .flatten()
    }

    /// Update exact intervals for blocks whose allocation-IR rows changed.
    /// The caller supplies physical fact-owner blocks: a rewritten phi source
    /// is owned by its predecessor edge, not by the successor containing the
    /// semantic phi row.
    pub(super) fn update<P: LivenessProgram + ?Sized>(
        &mut self,
        program: &P,
        cfg: &NormalizedCfg,
        intervals: &mut LiveIntervals,
        changed_blocks: &BTreeSet<BlockId>,
    ) -> Result<Vec<VReg>, LiveIntervalError> {
        Ok(self
            .update_delta(program, cfg, intervals, changed_blocks)?
            .changed_values)
    }

    /// Update liveness while preserving the distinction between lowering-only
    /// relabels and physical sparse-range changes.
    pub(super) fn update_delta<P: LivenessProgram + ?Sized>(
        &mut self,
        program: &P,
        cfg: &NormalizedCfg,
        intervals: &mut LiveIntervals,
        changed_blocks: &BTreeSet<BlockId>,
    ) -> Result<IncrementalLivenessUpdate, LiveIntervalError> {
        check_model_shape(program, cfg)?;
        if self.block_facts.len() != program.block_count()
            || self
                .block_members
                .as_ref()
                .is_some_and(|members| members.len() != program.block_count())
            || self.block_members.is_none() != program.has_stable_instruction_slots()
            || intervals.block_slots.len() != program.block_count()
            || program.value_count() < self.definitions.len() as u32
        {
            return Err(LiveIntervalError::new(
                "LIVE_INTERVAL.INCREMENTAL_SHAPE",
                None,
                None,
                Vec::new(),
                "incremental liveness shape changed outside the stable session domain",
            ));
        }
        if changed_blocks.is_empty() {
            return Ok(IncrementalLivenessUpdate::default());
        }

        let next_value_count = program.value_count() as usize;
        self.definitions.resize(next_value_count, None);
        self.uses
            .resize_with(next_value_count, SharedUseSites::default);
        self.program_order_lengths.resize(next_value_count, None);
        intervals.intervals.resize(next_value_count, None);

        let mut changed_rows = Vec::with_capacity(changed_blocks.len());
        for block in changed_blocks {
            let row = cfg.block_index.get(block).copied().ok_or_else(|| {
                LiveIntervalError::new(
                    "LIVE_INTERVAL.INCREMENTAL_BLOCK",
                    Some(*block),
                    None,
                    Vec::new(),
                    "changed block is outside the normalized CFG",
                )
            })?;
            changed_rows.push(row);
        }
        changed_rows.sort_unstable();
        changed_rows.dedup();

        let mut affected = Vec::<VReg>::new();
        let mut affected_marks = vec![false; next_value_count];
        let mut replacements = Vec::with_capacity(changed_rows.len());
        let mut previous_slots = Vec::with_capacity(changed_rows.len());
        let mut removed_definitions = Vec::new();
        let mut added_definitions = Vec::new();
        let mut removed_uses = Vec::new();
        let mut added_uses = Vec::new();
        for &block in &changed_rows {
            let block_id = program.block_id(block);
            if let Some(block_members) = &self.block_members {
                for &value in &block_members[block] {
                    mark_affected_value(&mut affected, &mut affected_marks, value, block_id)?;
                }
            }
            let slots = assign_block_slots(program, block)?;
            let old_slots = std::mem::replace(&mut intervals.block_slots[block], slots);
            if self.block_members.is_some() {
                previous_slots.push((block, old_slots));
            }
            let facts = scan_indexed_block(program, cfg, &intervals.block_slots, block)?;
            append_sorted_difference(
                &mut removed_definitions,
                &self.block_facts[block].definitions,
                &facts.definitions,
            );
            append_sorted_difference(
                &mut added_definitions,
                &facts.definitions,
                &self.block_facts[block].definitions,
            );
            append_sorted_difference(
                &mut removed_uses,
                &self.block_facts[block].uses,
                &facts.uses,
            );
            append_sorted_difference(&mut added_uses, &facts.uses, &self.block_facts[block].uses);
            replacements.push((block, facts));
        }

        removed_definitions.sort_unstable();
        added_definitions.sort_unstable();
        removed_uses.sort_unstable();
        added_uses.sort_unstable();

        for &(value, definition) in &removed_definitions {
            mark_affected_value(
                &mut affected,
                &mut affected_marks,
                value,
                definition.block(),
            )?;
            let slot = self
                .definitions
                .get_mut(value.0 as usize)
                .ok_or_else(|| value_range_error(definition.block(), value, "definition"))?;
            if *slot != Some(definition) {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.INCREMENTAL_DEFINITION",
                    Some(definition.block()),
                    None,
                    vec![value],
                    "cached block definition differs from the global fact index",
                ));
            }
            *slot = None;
        }
        for &(value, definition) in &added_definitions {
            mark_affected_value(
                &mut affected,
                &mut affected_marks,
                value,
                definition.block(),
            )?;
            record_definition(&mut self.definitions, value, definition)?;
        }
        apply_sorted_use_fact_delta(
            &mut self.uses,
            &removed_uses,
            &added_uses,
            &mut affected,
            &mut affected_marks,
        )?;

        for (block, facts) in replacements {
            self.block_facts[block] = facts;
        }
        affected.sort_unstable();

        if let Some(block_members) = &self.block_members {
            for (block, old_slots) in &previous_slots {
                let block_id = program.block_id(*block);
                let new_slots = &intervals.block_slots[*block];
                for &value in &block_members[*block] {
                    let Some(interval) = intervals.intervals[value.0 as usize].as_ref() else {
                        continue;
                    };
                    let Ok(segment) = interval
                        .segments
                        .binary_search_by_key(&block_id, |segment| segment.block)
                        .map(|row| interval.segments[row])
                    else {
                        continue;
                    };
                    let Some(old_length) =
                        old_slots.program_order_distance(segment.start, segment.end)
                    else {
                        continue;
                    };
                    let Some(new_length) =
                        new_slots.program_order_distance(segment.start, segment.end)
                    else {
                        continue;
                    };
                    let delta = i128::from(new_length) - i128::from(old_length);
                    if delta != 0 {
                        let current =
                            self.program_order_lengths[value.0 as usize].ok_or_else(|| {
                                LiveIntervalError::new(
                                    "LIVE_INTERVAL.LENGTH_CACHE",
                                    Some(block_id),
                                    None,
                                    vec![value],
                                    "active block member has no cached program-order length",
                                )
                            })?;
                        self.program_order_lengths[value.0 as usize] = Some(
                            i128::from(current)
                                .checked_add(delta)
                                .and_then(|length| u64::try_from(length).ok())
                                .filter(|length| *length != 0)
                                .ok_or_else(|| {
                                    LiveIntervalError::new(
                                        "LIVE_INTERVAL.LENGTH_CACHE",
                                        Some(block_id),
                                        None,
                                        vec![value],
                                        "incremental program-order length is zero or outside u64",
                                    )
                                })?,
                        );
                    }
                }
            }
        }

        self.rebuild_affected_intervals(program, cfg, intervals, affected)
    }

    /// Apply the exact stable fact transaction emitted by allocation IR.
    /// Optimized allocation uses this path; debug verification independently
    /// rescans every touched block through [`Self::update_delta`] and requires
    /// byte-for-byte identical indexes and sparse intervals.
    pub(super) fn update_fact_delta<P: LivenessProgram + ?Sized>(
        &mut self,
        program: &P,
        cfg: &NormalizedCfg,
        intervals: &mut LiveIntervals,
        mut delta: LivenessFactDelta,
    ) -> Result<IncrementalLivenessUpdate, LiveIntervalError> {
        check_model_shape(program, cfg)?;
        if !program.has_stable_instruction_slots()
            || self.block_facts.len() != program.block_count()
            || self.block_members.is_some()
            || intervals.block_slots.len() != program.block_count()
            || program.value_count() < self.definitions.len() as u32
        {
            return Err(LiveIntervalError::new(
                "LIVE_INTERVAL.FACT_DELTA_SHAPE",
                None,
                None,
                Vec::new(),
                "exact fact transactions require a stable-slot allocation program",
            ));
        }
        if delta.is_empty() {
            return Ok(IncrementalLivenessUpdate::default());
        }

        let oracle = if super::exhaustive_verification_enabled() {
            let mut oracle_index = self.clone();
            let mut oracle_intervals = intervals.clone();
            let oracle_update = oracle_index.update_delta(
                program,
                cfg,
                &mut oracle_intervals,
                &delta.changed_blocks,
            )?;
            Some((oracle_index, oracle_intervals, oracle_update))
        } else {
            None
        };

        normalize_fact_changes(
            &mut delta.removed_definitions,
            &mut delta.added_definitions,
            "definition",
        )?;
        normalize_fact_changes(&mut delta.removed_uses, &mut delta.added_uses, "use")?;

        let next_value_count = program.value_count() as usize;
        self.definitions.resize(next_value_count, None);
        self.uses
            .resize_with(next_value_count, SharedUseSites::default);
        self.program_order_lengths.resize(next_value_count, None);
        intervals.intervals.resize(next_value_count, None);

        for block_id in &delta.layout_blocks {
            let block = cfg.block_index.get(block_id).copied().ok_or_else(|| {
                LiveIntervalError::new(
                    "LIVE_INTERVAL.FACT_DELTA_BLOCK",
                    Some(*block_id),
                    None,
                    Vec::new(),
                    "instruction-layout publication is outside the normalized CFG",
                )
            })?;
            let slots = assign_block_slots(program, block)?;
            intervals.block_slots[block] = slots;
        }

        let mut affected = Vec::<VReg>::new();
        let mut affected_marks = vec![false; next_value_count];
        for &(value, definition) in &delta.removed_definitions {
            mark_affected_value(
                &mut affected,
                &mut affected_marks,
                value,
                definition.block(),
            )?;
            let slot = self
                .definitions
                .get_mut(value.0 as usize)
                .ok_or_else(|| value_range_error(definition.block(), value, "definition"))?;
            if *slot != Some(definition) {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.FACT_DELTA_DEFINITION",
                    Some(definition.block()),
                    None,
                    vec![value],
                    "removed definition differs from the global stable fact index",
                ));
            }
            *slot = None;
        }
        for &(value, definition) in &delta.added_definitions {
            mark_affected_value(
                &mut affected,
                &mut affected_marks,
                value,
                definition.block(),
            )?;
            record_definition(&mut self.definitions, value, definition)?;
        }
        apply_sorted_use_fact_delta(
            &mut self.uses,
            &delta.removed_uses,
            &delta.added_uses,
            &mut affected,
            &mut affected_marks,
        )?;
        apply_indexed_block_fact_delta(&mut self.block_facts, cfg, &mut delta)?;
        affected.sort_unstable();

        for block_id in &delta.changed_blocks {
            let block = cfg.block_index.get(block_id).copied().ok_or_else(|| {
                LiveIntervalError::new(
                    "LIVE_INTERVAL.FACT_DELTA_BLOCK",
                    Some(*block_id),
                    None,
                    Vec::new(),
                    "changed fact block is outside the normalized CFG",
                )
            })?;
            if intervals.block_slots[block].instructions.len() != program.instruction_count(block) {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.FACT_DELTA_SLOT_SHAPE",
                    Some(*block_id),
                    None,
                    Vec::new(),
                    "stable slot edits do not reproduce the allocation-IR instruction row",
                ));
            }
        }

        let update = self.rebuild_affected_intervals(program, cfg, intervals, affected)?;
        if let Some((oracle_index, oracle_intervals, oracle_update)) = oracle
            && (*self != oracle_index || *intervals != oracle_intervals || update != oracle_update)
        {
            return Err(LiveIntervalError::new(
                "LIVE_INTERVAL.FACT_DELTA_ORACLE",
                delta.changed_blocks.iter().next().copied(),
                None,
                update.changed_values.clone(),
                "exact allocation-IR fact transaction differs from a complete changed-block rescan",
            ));
        }
        Ok(update)
    }

    fn rebuild_affected_intervals<P: LivenessProgram + ?Sized>(
        &mut self,
        program: &P,
        cfg: &NormalizedCfg,
        intervals: &mut LiveIntervals,
        affected: Vec<VReg>,
    ) -> Result<IncrementalLivenessUpdate, LiveIntervalError> {
        let mut update = IncrementalLivenessUpdate::default();
        for value in affected {
            let definition = self.definitions[value.0 as usize];
            let value_uses = &self.uses[value.0 as usize];
            let row = value.0 as usize;
            let can_relabel = program.has_stable_instruction_slots()
                && intervals.intervals[row].as_ref().is_some_and(|previous| {
                    can_relabel_unchanged_interval(previous, definition, value_uses)
                });
            if can_relabel {
                let previous = intervals.intervals[row]
                    .as_mut()
                    .expect("stable relabel candidate disappeared");
                let definition = definition.expect("stable relabel has no definition");
                if previous.definition != definition || previous.uses != *value_uses {
                    previous.definition = definition;
                    previous.uses.clone_from(value_uses);
                    update.changed_values.push(value);
                }
                continue;
            }

            if let (Some(block_members), Some(previous)) = (
                self.block_members.as_mut(),
                intervals.intervals[row].as_ref(),
            ) {
                for segment in &previous.segments {
                    block_members[cfg.block_index[&segment.block]].remove(&value);
                }
            }
            let next = build_sparse_value_interval(
                program,
                cfg,
                &intervals.block_slots,
                &self.definitions,
                &self.uses,
                &self.dominators,
                &mut self.interval_scratch,
                value,
            )?;
            if intervals.intervals[row] != next {
                update.changed_values.push(value);
                update.range_changed_values.push(value);
            }
            intervals.intervals[row] = next;
            self.program_order_lengths[row] = intervals.intervals[row]
                .as_ref()
                .map(|interval| checked_program_order_length(interval, intervals, cfg))
                .transpose()?;
            if let Some(interval) = intervals.intervals[row].as_ref() {
                if let Some(block_members) = self.block_members.as_mut() {
                    for segment in &interval.segments {
                        block_members[cfg.block_index[&segment.block]].insert(value);
                    }
                }
            }
        }
        update.live_lengths = update
            .changed_values
            .iter()
            .copied()
            .map(|value| (value, self.program_order_length(value)))
            .collect();
        Ok(update)
    }
}

/// Dense allocation-IR positions are diagnostics/lowering coordinates, not
/// live-range geometry. Stable slots let an insertion relabel those positions
/// without solving CFG liveness again for every value crossing the block.
fn can_relabel_unchanged_interval(
    previous: &LiveInterval,
    definition: Option<DefinitionSite>,
    uses: &[UseSite],
) -> bool {
    let Some(definition) = definition else {
        return false;
    };
    if !same_definition_coordinate(previous.definition, definition)
        || previous.uses.len() != uses.len()
        || previous
            .uses
            .iter()
            .copied()
            .zip(uses.iter().copied())
            .any(|(left, right)| !same_use_coordinate(left, right))
    {
        return false;
    }
    true
}

fn same_definition_coordinate(left: DefinitionSite, right: DefinitionSite) -> bool {
    match (left, right) {
        (
            DefinitionSite::Phi {
                block: left_block,
                phi: left_phi,
                slot: left_slot,
            },
            DefinitionSite::Phi {
                block: right_block,
                phi: right_phi,
                slot: right_slot,
            },
        ) => (left_block, left_phi, left_slot) == (right_block, right_phi, right_slot),
        (
            DefinitionSite::Instruction {
                block: left_block,
                slot: left_slot,
                ..
            },
            DefinitionSite::Instruction {
                block: right_block,
                slot: right_slot,
                ..
            },
        ) => (left_block, left_slot) == (right_block, right_slot),
        _ => false,
    }
}

fn same_use_coordinate(left: UseSite, right: UseSite) -> bool {
    left.same_coordinate(right)
}

fn normalize_fact_changes<T: Copy + Ord>(
    removed: &mut Vec<T>,
    added: &mut Vec<T>,
    kind: &str,
) -> Result<(), LiveIntervalError> {
    removed.sort_unstable();
    added.sort_unstable();
    if removed.windows(2).any(|pair| pair[0] == pair[1])
        || added.windows(2).any(|pair| pair[0] == pair[1])
    {
        return Err(LiveIntervalError::new(
            "LIVE_INTERVAL.FACT_DELTA_IDENTITY",
            None,
            None,
            Vec::new(),
            format!("exact {kind} transaction contains a duplicate fact"),
        ));
    }

    let mut removed_only = Vec::with_capacity(removed.len());
    let mut added_only = Vec::with_capacity(added.len());
    let mut removed_index = 0;
    let mut added_index = 0;
    while removed_index < removed.len() && added_index < added.len() {
        match removed[removed_index].cmp(&added[added_index]) {
            Ordering::Less => {
                removed_only.push(removed[removed_index]);
                removed_index += 1;
            }
            Ordering::Greater => {
                added_only.push(added[added_index]);
                added_index += 1;
            }
            Ordering::Equal => {
                removed_index += 1;
                added_index += 1;
            }
        }
    }
    removed_only.extend_from_slice(&removed[removed_index..]);
    added_only.extend_from_slice(&added[added_index..]);
    *removed = removed_only;
    *added = added_only;
    Ok(())
}

fn block_fact_row<'a>(
    facts: &'a mut [IndexedBlockFacts],
    cfg: &NormalizedCfg,
    block: BlockId,
) -> Result<&'a mut IndexedBlockFacts, LiveIntervalError> {
    let row = cfg.block_index.get(&block).copied().ok_or_else(|| {
        LiveIntervalError::new(
            "LIVE_INTERVAL.FACT_DELTA_BLOCK",
            Some(block),
            None,
            Vec::new(),
            "stable fact is outside the normalized CFG",
        )
    })?;
    facts.get_mut(row).ok_or_else(|| {
        LiveIntervalError::new(
            "LIVE_INTERVAL.FACT_DELTA_BLOCK",
            Some(block),
            None,
            Vec::new(),
            "stable fact has no cached block row",
        )
    })
}

fn apply_indexed_block_fact_delta(
    facts: &mut [IndexedBlockFacts],
    cfg: &NormalizedCfg,
    delta: &mut LivenessFactDelta,
) -> Result<(), LiveIntervalError> {
    let mut rows = BTreeMap::<BlockId, IndexedBlockFactDelta>::new();
    for fact in std::mem::take(&mut delta.removed_definitions) {
        rows.entry(fact.1.block())
            .or_default()
            .removed_definitions
            .push(fact);
    }
    for fact in std::mem::take(&mut delta.added_definitions) {
        rows.entry(fact.1.block())
            .or_default()
            .added_definitions
            .push(fact);
    }
    for fact in std::mem::take(&mut delta.removed_uses) {
        rows.entry(fact.1.block())
            .or_default()
            .removed_uses
            .push(fact);
    }
    for fact in std::mem::take(&mut delta.added_uses) {
        rows.entry(fact.1.block())
            .or_default()
            .added_uses
            .push(fact);
    }

    for (block, delta) in rows {
        let row = block_fact_row(facts, cfg, block)?;
        row.definitions = merge_sorted_fact_row(
            &row.definitions,
            &delta.removed_definitions,
            &delta.added_definitions,
            block,
            "definition",
            |fact| fact.0,
        )?;
        row.uses = merge_sorted_fact_row(
            &row.uses,
            &delta.removed_uses,
            &delta.added_uses,
            block,
            "use",
            |fact| fact.0,
        )?;
    }
    Ok(())
}

/// Replace one block-owned sorted fact row atomically. A split may rewrite
/// thousands of operands in one RTL block; repeated `Vec::remove/insert`
/// would shift that same row once per operand.
fn merge_sorted_fact_row<T: Copy + Ord>(
    existing: &[T],
    removed: &[T],
    added: &[T],
    block: BlockId,
    kind: &str,
    value_of: impl Fn(T) -> VReg,
) -> Result<Vec<T>, LiveIntervalError> {
    if let Some(duplicate) = removed.windows(2).find(|pair| pair[0] == pair[1]) {
        return Err(LiveIntervalError::new(
            "LIVE_INTERVAL.FACT_DELTA_ROW",
            Some(block),
            None,
            vec![value_of(duplicate[0])],
            format!("block transaction removes the same {kind} fact twice"),
        ));
    }
    if let Some(duplicate) = added.windows(2).find(|pair| pair[0] == pair[1]) {
        return Err(LiveIntervalError::new(
            "LIVE_INTERVAL.FACT_DELTA_ROW",
            Some(block),
            None,
            vec![value_of(duplicate[0])],
            format!("block transaction adds the same {kind} fact twice"),
        ));
    }

    let mut merged = Vec::with_capacity(
        existing
            .len()
            .saturating_sub(removed.len())
            .saturating_add(added.len()),
    );
    let mut existing_index = 0;
    let mut removed_index = 0;
    let mut added_index = 0;
    loop {
        while let (Some(&existing_fact), Some(&removed_fact)) =
            (existing.get(existing_index), removed.get(removed_index))
        {
            match existing_fact.cmp(&removed_fact) {
                Ordering::Less => break,
                Ordering::Equal => {
                    existing_index += 1;
                    removed_index += 1;
                }
                Ordering::Greater => {
                    return Err(LiveIntervalError::new(
                        "LIVE_INTERVAL.FACT_DELTA_ROW",
                        Some(block),
                        None,
                        vec![value_of(removed_fact)],
                        format!("removed {kind} is absent from the cached block row"),
                    ));
                }
            }
        }
        if existing_index == existing.len() && removed_index < removed.len() {
            return Err(LiveIntervalError::new(
                "LIVE_INTERVAL.FACT_DELTA_ROW",
                Some(block),
                None,
                vec![value_of(removed[removed_index])],
                format!("removed {kind} is absent from the cached block row"),
            ));
        }

        match (
            existing.get(existing_index).copied(),
            added.get(added_index).copied(),
        ) {
            (Some(existing_fact), Some(added_fact)) => match existing_fact.cmp(&added_fact) {
                Ordering::Less => {
                    merged.push(existing_fact);
                    existing_index += 1;
                }
                Ordering::Greater => {
                    merged.push(added_fact);
                    added_index += 1;
                }
                Ordering::Equal => {
                    return Err(LiveIntervalError::new(
                        "LIVE_INTERVAL.FACT_DELTA_ROW",
                        Some(block),
                        None,
                        vec![value_of(added_fact)],
                        format!("added {kind} already exists in the cached block row"),
                    ));
                }
            },
            (Some(existing_fact), None) => {
                merged.push(existing_fact);
                existing_index += 1;
            }
            (None, Some(added_fact)) => {
                merged.push(added_fact);
                added_index += 1;
            }
            (None, None) => break,
        }
    }
    Ok(merged)
}

fn value_range_error(block: BlockId, value: VReg, kind: &str) -> LiveIntervalError {
    LiveIntervalError::new(
        "LIVE_INTERVAL.VALUE_RANGE",
        Some(block),
        None,
        vec![value],
        format!("incremental {kind} is outside the allocation VReg table"),
    )
}

fn mark_affected_value(
    affected: &mut Vec<VReg>,
    marks: &mut [bool],
    value: VReg,
    block: BlockId,
) -> Result<(), LiveIntervalError> {
    let mark = marks
        .get_mut(value.0 as usize)
        .ok_or_else(|| value_range_error(block, value, "changed-block membership"))?;
    if !*mark {
        *mark = true;
        affected.push(value);
    }
    Ok(())
}

fn append_sorted_difference<T: Copy + Ord>(output: &mut Vec<T>, left: &[T], right: &[T]) {
    let mut left_index = 0;
    let mut right_index = 0;
    while left_index < left.len() {
        while right_index < right.len() && right[right_index] < left[left_index] {
            right_index += 1;
        }
        if right_index == right.len() || left[left_index] < right[right_index] {
            output.push(left[left_index]);
        }
        left_index += 1;
    }
}

/// Apply all changed-block use facts once per value. The previous updater ran
/// `retain` over the complete global use row for every removed site, making a
/// split round quadratic in a heavily used RTL value. Both sides are ordered,
/// so removal and insertion are exact linear merges.
fn apply_sorted_use_fact_delta(
    uses: &mut [SharedUseSites],
    removed: &[(VReg, UseSite)],
    added: &[(VReg, UseSite)],
    affected: &mut Vec<VReg>,
    affected_marks: &mut [bool],
) -> Result<(), LiveIntervalError> {
    let mut removed_index = 0;
    let mut added_index = 0;
    while removed_index < removed.len() || added_index < added.len() {
        let value = match (removed.get(removed_index), added.get(added_index)) {
            (Some((removed, _)), Some((added, _))) => (*removed).min(*added),
            (Some((removed, _)), None) => *removed,
            (None, Some((added, _))) => *added,
            (None, None) => break,
        };
        let removed_end = removed_index
            + removed[removed_index..].partition_point(|(candidate, _)| *candidate == value);
        let added_end = added_index
            + added[added_index..].partition_point(|(candidate, _)| *candidate == value);
        let block = removed
            .get(removed_index)
            .filter(|(candidate, _)| *candidate == value)
            .or_else(|| {
                added
                    .get(added_index)
                    .filter(|(candidate, _)| *candidate == value)
            })
            .map(|(_, site)| site.block())
            .expect("use fact delta value has no site");
        mark_affected_value(affected, affected_marks, value, block)?;
        let value_uses = uses
            .get_mut(value.0 as usize)
            .ok_or_else(|| value_range_error(block, value, "use"))?;
        replace_sorted_uses(
            value_uses,
            &removed[removed_index..removed_end],
            &added[added_index..added_end],
            value,
        )?;
        removed_index = removed_end;
        added_index = added_end;
    }
    Ok(())
}

fn replace_sorted_uses(
    existing: &mut SharedUseSites,
    removed: &[(VReg, UseSite)],
    added: &[(VReg, UseSite)],
    value: VReg,
) -> Result<(), LiveIntervalError> {
    if let Some(duplicate) = removed.windows(2).find(|pair| pair[0].1 == pair[1].1) {
        return Err(LiveIntervalError::new(
            "LIVE_INTERVAL.INCREMENTAL_USE",
            Some(duplicate[0].1.block()),
            None,
            vec![value],
            "changed blocks remove the same cached use more than once",
        ));
    }
    if let Some(duplicate) = added.windows(2).find(|pair| pair[0].1 == pair[1].1) {
        return Err(LiveIntervalError::new(
            "LIVE_INTERVAL.INCREMENTAL_USE",
            Some(duplicate[0].1.block()),
            None,
            vec![value],
            "changed blocks add the same use more than once",
        ));
    }

    let mut merged = Vec::with_capacity(
        existing
            .len()
            .saturating_sub(removed.len())
            .saturating_add(added.len()),
    );
    let mut existing_index = 0;
    let mut removed_index = 0;
    let mut added_index = 0;
    loop {
        while let (Some(&existing_site), Some((_, removed_site))) =
            (existing.get(existing_index), removed.get(removed_index))
        {
            match existing_site.cmp(removed_site) {
                Ordering::Less => break,
                Ordering::Equal => {
                    existing_index += 1;
                    removed_index += 1;
                }
                Ordering::Greater => {
                    return Err(LiveIntervalError::new(
                        "LIVE_INTERVAL.INCREMENTAL_USE",
                        Some(removed_site.block()),
                        None,
                        vec![value],
                        "cached block use is absent from the global fact index",
                    ));
                }
            }
        }
        if existing_index == existing.len() && removed_index < removed.len() {
            let removed_site = removed[removed_index].1;
            return Err(LiveIntervalError::new(
                "LIVE_INTERVAL.INCREMENTAL_USE",
                Some(removed_site.block()),
                None,
                vec![value],
                "cached block use is absent from the global fact index",
            ));
        }

        match (
            existing.get(existing_index).copied(),
            added.get(added_index),
        ) {
            (Some(existing_site), Some((_, added_site))) => match existing_site.cmp(added_site) {
                Ordering::Less => {
                    merged.push(existing_site);
                    existing_index += 1;
                }
                Ordering::Greater => {
                    merged.push(*added_site);
                    added_index += 1;
                }
                Ordering::Equal => {
                    return Err(LiveIntervalError::new(
                        "LIVE_INTERVAL.INCREMENTAL_USE",
                        Some(added_site.block()),
                        None,
                        vec![value],
                        "new block use already exists in the global fact index",
                    ));
                }
            },
            (Some(existing_site), None) => {
                merged.push(existing_site);
                existing_index += 1;
            }
            (None, Some((_, added_site))) => {
                merged.push(*added_site);
                added_index += 1;
            }
            (None, None) => break,
        }
    }
    *existing = merged.into();
    Ok(())
}

fn add_indexed_facts(
    definitions: &mut [Option<DefinitionSite>],
    uses: &mut [Vec<UseSite>],
    facts: &IndexedBlockFacts,
) -> Result<(), LiveIntervalError> {
    for &(value, definition) in &facts.definitions {
        record_definition(definitions, value, definition)?;
    }
    for &(value, site) in &facts.uses {
        record_use(uses, value, site)?;
    }
    Ok(())
}

fn scan_indexed_block<P: LivenessProgram + ?Sized>(
    program: &P,
    cfg: &NormalizedCfg,
    slots: &[BlockSlots],
    block: usize,
) -> Result<IndexedBlockFacts, LiveIntervalError> {
    let block_id = program.block_id(block);
    let block_slots = slots.get(block).ok_or_else(|| {
        LiveIntervalError::new(
            "LIVE_INTERVAL.INCREMENTAL_BLOCK",
            Some(block_id),
            None,
            Vec::new(),
            "indexed block has no slot row",
        )
    })?;
    let mut facts = IndexedBlockFacts::default();
    for phi in 0..program.phi_count(block) {
        if program.phi_definition_in_register(block, phi) {
            let value = program.phi_definition(block, phi);
            facts.definitions.push((
                value,
                DefinitionSite::Phi {
                    block: block_id,
                    phi,
                    slot: block_slots.phi_def,
                },
            ));
        }
    }
    for instruction in 0..program.instruction_count(block) {
        let instruction_identity = program
            .instruction_identity(block, instruction)
            .ok_or_else(|| {
                LiveIntervalError::new(
                    "LIVE_INTERVAL.INSTRUCTION_IDENTITY",
                    Some(block_id),
                    Some(instruction),
                    Vec::new(),
                    "instruction has no stable liveness identity",
                )
            })?;
        let use_slot = block_slots.instruction_use(instruction).ok_or_else(|| {
            LiveIntervalError::new(
                "LIVE_INTERVAL.SLOT_RANGE",
                Some(block_id),
                Some(instruction),
                Vec::new(),
                "incremental instruction-use slot is outside the block",
            )
        })?;
        let mut instruction_uses = program.instruction_uses(block, instruction).to_vec();
        instruction_uses.sort_unstable();
        instruction_uses.dedup();
        facts.uses.extend(instruction_uses.into_iter().map(|value| {
            (
                value,
                UseSite::Instruction {
                    block: block_id,
                    instruction: instruction_identity,
                    slot: use_slot,
                },
            )
        }));
        if let Some(value) = program.instruction_definition(block, instruction) {
            facts.definitions.push((
                value,
                DefinitionSite::Instruction {
                    block: block_id,
                    instruction: instruction_identity,
                    slot: block_slots.instruction_def(instruction).unwrap(),
                },
            ));
        }
    }

    for &successor in &cfg.successors[block] {
        let successor_id = program.block_id(successor);
        for phi in 0..program.phi_count(successor) {
            let mut matched = None;
            for (source, &(predecessor, value)) in
                program.phi_sources(successor, phi).iter().enumerate()
            {
                if predecessor != block_id {
                    continue;
                }
                if matched.replace((source, value)).is_some() {
                    return Err(LiveIntervalError::new(
                        "LIVE_INTERVAL.PHI_PREDECESSOR",
                        Some(successor_id),
                        None,
                        vec![value],
                        "phi has duplicate sources for one predecessor",
                    ));
                }
            }
            let Some((source, value)) = matched else {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.PHI_PREDECESSOR",
                    Some(successor_id),
                    None,
                    vec![program.phi_definition(successor, phi)],
                    "phi has no source for a normalized predecessor",
                ));
            };
            if program.phi_source_in_register(successor, phi, source) {
                facts.uses.push((
                    value,
                    UseSite::PhiEdge {
                        predecessor: block_id,
                        successor: successor_id,
                        phi,
                        slot: block_slots.exit,
                    },
                ));
            }
        }
        for edge_use in 0..program.extra_phi_edge_use_count(successor) {
            let (predecessor, value, phi) = program.extra_phi_edge_use(successor, edge_use);
            if predecessor == block_id {
                facts.uses.push((
                    value,
                    UseSite::PhiEdge {
                        predecessor: block_id,
                        successor: successor_id,
                        phi,
                        slot: block_slots.exit,
                    },
                ));
            }
        }
    }
    facts.definitions.sort_unstable();
    facts.uses.sort_unstable();
    facts.uses.dedup();
    Ok(facts)
}

fn build_sparse_value_interval<P: LivenessProgram + ?Sized>(
    program: &P,
    cfg: &NormalizedCfg,
    slots: &[BlockSlots],
    definitions: &[Option<DefinitionSite>],
    uses: &[SharedUseSites],
    dominators: &DominatorIntervals,
    scratch: &mut SparseIntervalScratch,
    value: VReg,
) -> Result<Option<LiveInterval>, LiveIntervalError> {
    let definition = definitions.get(value.0 as usize).copied().flatten();
    let value_uses = uses
        .get(value.0 as usize)
        .ok_or_else(|| value_range_error(program.block_id(0), value, "use list"))?;
    // A phi is not an emitted machine instruction. Once allocation-owned
    // homes have removed every register use of its result, the definition has
    // no physical live range at all. Instruction definitions remain one-slot
    // ranges even without uses because the instruction still needs a concrete
    // destination until dead-code elimination removes it.
    if value_uses.is_empty() && matches!(definition, Some(DefinitionSite::Phi { .. })) {
        return Ok(None);
    }
    let Some(definition) = definition else {
        if value_uses.is_empty() {
            return Ok(None);
        }
        return Err(LiveIntervalError::new(
            "LIVE_INTERVAL.MISSING_DEFINITION",
            value_uses.first().map(|site| site.block()),
            None,
            vec![value],
            "incrementally used value has no machine definition",
        ));
    };
    for &use_site in value_uses {
        verify_definition_dominates_use(cfg, dominators, definition, use_site, value)?;
    }

    let definition_block = cfg.block_index[&definition.block()];
    scratch.begin(program.block_count());
    scratch.touch(definition_block);
    for &site in value_uses {
        let block = cfg.block_index[&site.block()];
        scratch.record_last_use(block, site.slot());
        match site {
            UseSite::Instruction { .. } if block == definition_block => {}
            UseSite::Instruction { .. } => {
                if scratch.mark_live_in(block) {
                    scratch.queue.push(block);
                }
            }
            UseSite::PhiEdge { .. } => {
                scratch.mark_live_out(block);
                if block != definition_block && scratch.mark_live_in(block) {
                    scratch.queue.push(block);
                }
            }
        }
    }
    let mut queue_cursor = 0usize;
    while queue_cursor < scratch.queue.len() {
        let block = scratch.queue[queue_cursor];
        queue_cursor += 1;
        for &predecessor in &cfg.predecessors[block] {
            scratch.mark_live_out(predecessor);
            if predecessor != definition_block && scratch.mark_live_in(predecessor) {
                scratch.queue.push(predecessor);
            }
        }
    }

    scratch
        .live_blocks
        .sort_unstable_by_key(|&block| program.block_id(block));
    let mut segments = Vec::with_capacity(scratch.live_blocks.len());
    for &block in &scratch.live_blocks {
        let block_slots = &slots[block];
        let start = if block == definition_block {
            definition.slot()
        } else {
            block_slots.entry
        };
        let end = if scratch.is_live_out(block) {
            block_slots.exit.next()
        } else if let Some(last_use) = scratch.last_use(block) {
            last_use.next()
        } else if block == definition_block {
            definition.slot().next()
        } else {
            None
        }
        .ok_or_else(|| {
            LiveIntervalError::new(
                "LIVE_INTERVAL.SLOT_RANGE",
                Some(program.block_id(block)),
                None,
                vec![value],
                "incremental live segment has no finite end",
            )
        })?;
        if start >= end {
            return Err(LiveIntervalError::new(
                "LIVE_INTERVAL.EMPTY_SEGMENT",
                Some(program.block_id(block)),
                None,
                vec![value],
                "incremental live segment is empty or reversed",
            ));
        }
        segments.push(LiveSegment {
            block: program.block_id(block),
            start,
            end,
        });
    }
    segments.sort_unstable_by_key(|segment| (segment.block, segment.start));
    Ok(Some(LiveInterval {
        value,
        definition,
        segments,
        uses: value_uses.clone(),
    }))
}

fn check_model_shape<P: LivenessProgram + ?Sized>(
    program: &P,
    cfg: &NormalizedCfg,
) -> Result<(), LiveIntervalError> {
    let blocks = program.block_count();
    if blocks == 0
        || cfg.predecessors.len() != blocks
        || cfg.successors.len() != blocks
        || cfg.idom.len() != blocks
        || cfg.block_index.len() != blocks
        || (0..blocks)
            .any(|block| cfg.block_index.get(&program.block_id(block)).copied() != Some(block))
    {
        return Err(LiveIntervalError::new(
            "LIVE_INTERVAL.MODEL_SHAPE",
            (blocks != 0).then(|| program.block_id(0)),
            None,
            Vec::new(),
            "normalized CFG tables do not exactly cover the liveness program",
        ));
    }
    Ok(())
}

fn assign_slots<P: LivenessProgram + ?Sized>(
    program: &P,
) -> Result<Vec<BlockSlots>, LiveIntervalError> {
    let mut result = Vec::with_capacity(program.block_count());
    for block in 0..program.block_count() {
        result.push(assign_block_slots(program, block)?);
    }
    Ok(result)
}

fn assign_block_slots<P: LivenessProgram + ?Sized>(
    program: &P,
    block: usize,
) -> Result<BlockSlots, LiveIntervalError> {
    let block_id = program.block_id(block);
    let block_instruction_count = program.instruction_count(block);
    let entry = program.block_entry_slot(block).ok_or_else(|| {
        LiveIntervalError::new(
            "LIVE_INTERVAL.SLOT_RANGE",
            Some(block_id),
            None,
            Vec::new(),
            "block entry is outside the slot-index domain",
        )
    })?;
    let phi_def = program.phi_definition_slot(block).ok_or_else(|| {
        LiveIntervalError::new(
            "LIVE_INTERVAL.SLOT_RANGE",
            Some(block_id),
            None,
            Vec::new(),
            "phi definition is outside the slot-index domain",
        )
    })?;
    let mut instructions = Vec::with_capacity(block_instruction_count);
    for instruction in 0..block_instruction_count {
        let use_ = program
            .instruction_use_slot(block, instruction)
            .ok_or_else(|| {
                LiveIntervalError::new(
                    "LIVE_INTERVAL.SLOT_RANGE",
                    Some(block_id),
                    Some(instruction),
                    Vec::new(),
                    "instruction use is outside the slot-index domain",
                )
            })?;
        let clobber = use_.next().ok_or_else(|| {
            LiveIntervalError::new(
                "LIVE_INTERVAL.SLOT_RANGE",
                Some(block_id),
                Some(instruction),
                Vec::new(),
                "instruction clobber is outside the slot-index domain",
            )
        })?;
        let def = clobber.next().ok_or_else(|| {
            LiveIntervalError::new(
                "LIVE_INTERVAL.SLOT_RANGE",
                Some(block_id),
                Some(instruction),
                Vec::new(),
                "instruction definition is outside the slot-index domain",
            )
        })?;
        if instructions
            .last()
            .is_some_and(|previous: &InstructionSlots| previous.def >= use_)
        {
            return Err(LiveIntervalError::new(
                "LIVE_INTERVAL.SLOT_ORDER",
                Some(block_id),
                Some(instruction),
                Vec::new(),
                "instruction program points are duplicated or out of order",
            ));
        }
        instructions.push(InstructionSlots { use_, clobber, def });
    }
    let exit = program.block_exit_slot(block).ok_or_else(|| {
        LiveIntervalError::new(
            "LIVE_INTERVAL.SLOT_RANGE",
            Some(block_id),
            None,
            Vec::new(),
            "block exit is outside the slot-index domain",
        )
    })?;
    if entry >= phi_def
        || instructions
            .first()
            .is_some_and(|instruction| phi_def >= instruction.use_)
        || instructions
            .last()
            .is_some_and(|instruction| instruction.def >= exit)
        || (instructions.is_empty() && phi_def >= exit)
    {
        return Err(LiveIntervalError::new(
            "LIVE_INTERVAL.SLOT_ORDER",
            Some(block_id),
            None,
            Vec::new(),
            "block boundary and instruction program points are out of order",
        ));
    }
    Ok(BlockSlots {
        entry,
        phi_def,
        exit,
        instructions,
    })
}

fn record_definition(
    definitions: &mut [Option<DefinitionSite>],
    value: VReg,
    site: DefinitionSite,
) -> Result<(), LiveIntervalError> {
    let Some(definition) = definitions.get_mut(value.0 as usize) else {
        return Err(LiveIntervalError::new(
            "LIVE_INTERVAL.VALUE_RANGE",
            Some(site.block()),
            match site {
                DefinitionSite::Instruction { instruction, .. } => Some(instruction),
                DefinitionSite::Phi { .. } => None,
            },
            vec![value],
            "definition is outside the MIR VReg table",
        ));
    };
    if let Some(previous) = *definition {
        return Err(LiveIntervalError::new(
            "LIVE_INTERVAL.MULTIPLE_DEFINITIONS",
            Some(site.block()),
            match site {
                DefinitionSite::Instruction { instruction, .. } => Some(instruction),
                DefinitionSite::Phi { .. } => None,
            },
            vec![value],
            format!("value was already defined at {previous:?}"),
        ));
    }
    *definition = Some(site);
    Ok(())
}

fn record_use(
    uses: &mut [Vec<UseSite>],
    value: VReg,
    site: UseSite,
) -> Result<(), LiveIntervalError> {
    let Some(value_uses) = uses.get_mut(value.0 as usize) else {
        return Err(LiveIntervalError::new(
            "LIVE_INTERVAL.VALUE_RANGE",
            Some(site.block()),
            match site {
                UseSite::Instruction { instruction, .. } => Some(instruction),
                UseSite::PhiEdge { .. } => None,
            },
            vec![value],
            "use is outside the MIR VReg table",
        ));
    };
    if value_uses.last().copied() != Some(site) {
        value_uses.push(site);
    }
    Ok(())
}

fn collect_facts<P: LivenessProgram + ?Sized>(
    program: &P,
    cfg: &NormalizedCfg,
    slots: &[BlockSlots],
) -> Result<ModelFacts, LiveIntervalError> {
    let value_count = program.value_count() as usize;
    let mut definitions = vec![None; value_count];
    let mut uses = vec![Vec::new(); value_count];
    let mut blocks = (0..program.block_count())
        .map(|_| BlockFacts::default())
        .collect::<Vec<_>>();
    let mut phi_definitions = (0..program.block_count())
        .map(|_| HashSet::new())
        .collect::<Vec<_>>();

    for block_index in 0..program.block_count() {
        let block_id = program.block_id(block_index);
        let block_slots = &slots[block_index];
        for phi_index in 0..program.phi_count(block_index) {
            let destination = program.phi_definition(block_index, phi_index);
            if !program.phi_definition_in_register(block_index, phi_index) {
                continue;
            }
            let site = DefinitionSite::Phi {
                block: block_id,
                phi: phi_index,
                slot: block_slots.phi_def,
            };
            record_definition(&mut definitions, destination, site)?;
            blocks[block_index].definitions.insert(destination);
            phi_definitions[block_index].insert(destination);
        }

        let mut seen_definitions = blocks[block_index].definitions.clone();
        for instruction in 0..program.instruction_count(block_index) {
            let instruction_identity = program
                .instruction_identity(block_index, instruction)
                .ok_or_else(|| {
                    LiveIntervalError::new(
                        "LIVE_INTERVAL.INSTRUCTION_IDENTITY",
                        Some(block_id),
                        Some(instruction),
                        Vec::new(),
                        "instruction has no stable liveness identity",
                    )
                })?;
            let use_slot = block_slots.instruction_use(instruction).ok_or_else(|| {
                LiveIntervalError::new(
                    "LIVE_INTERVAL.SLOT_RANGE",
                    Some(block_id),
                    Some(instruction),
                    Vec::new(),
                    "instruction-use slot is outside the block",
                )
            })?;
            let mut instruction_uses = program.instruction_uses(block_index, instruction).to_vec();
            instruction_uses.sort_unstable();
            instruction_uses.dedup();
            for value in instruction_uses {
                let site = UseSite::Instruction {
                    block: block_id,
                    instruction: instruction_identity,
                    slot: use_slot,
                };
                record_use(&mut uses, value, site)?;
                if !seen_definitions.contains(&value) {
                    blocks[block_index].upward_uses.insert(value);
                }
                blocks[block_index]
                    .last_use
                    .entry(value)
                    .and_modify(|current| *current = (*current).max(use_slot))
                    .or_insert(use_slot);
            }
            if let Some(value) = program.instruction_definition(block_index, instruction) {
                let site = DefinitionSite::Instruction {
                    block: block_id,
                    instruction: instruction_identity,
                    slot: block_slots.instruction_def(instruction).ok_or_else(|| {
                        LiveIntervalError::new(
                            "LIVE_INTERVAL.SLOT_RANGE",
                            Some(block_id),
                            Some(instruction),
                            vec![value],
                            "instruction-definition slot is outside the block",
                        )
                    })?,
                };
                record_definition(&mut definitions, value, site)?;
                blocks[block_index].definitions.insert(value);
                seen_definitions.insert(value);
            }
        }
    }

    let mut edge_uses = HashMap::<(usize, usize), HashSet<VReg>>::new();
    for successor in 0..program.block_count() {
        let successor_id = program.block_id(successor);
        for phi_index in 0..program.phi_count(successor) {
            let destination = program.phi_definition(successor, phi_index);
            let mut seen_predecessors = BTreeSet::new();
            for (source_index, &(predecessor_id, value)) in
                program.phi_sources(successor, phi_index).iter().enumerate()
            {
                let Some(&predecessor) = cfg.block_index.get(&predecessor_id) else {
                    return Err(LiveIntervalError::new(
                        "LIVE_INTERVAL.PHI_PREDECESSOR",
                        Some(successor_id),
                        None,
                        vec![value],
                        format!("phi references missing predecessor {predecessor_id}"),
                    ));
                };
                if !cfg.predecessors[successor].contains(&predecessor)
                    || !seen_predecessors.insert(predecessor)
                {
                    return Err(LiveIntervalError::new(
                        "LIVE_INTERVAL.PHI_PREDECESSOR",
                        Some(successor_id),
                        None,
                        vec![value],
                        "phi predecessor is absent from the CFG or appears more than once",
                    ));
                }
                if !program.phi_source_in_register(successor, phi_index, source_index) {
                    continue;
                }
                let site = UseSite::PhiEdge {
                    predecessor: predecessor_id,
                    successor: successor_id,
                    phi: phi_index,
                    slot: slots[predecessor].exit,
                };
                record_use(&mut uses, value, site)?;
                edge_uses
                    .entry((predecessor, successor))
                    .or_default()
                    .insert(value);
                blocks[predecessor]
                    .last_use
                    .entry(value)
                    .and_modify(|current| *current = (*current).max(slots[predecessor].exit))
                    .or_insert(slots[predecessor].exit);
            }
            if seen_predecessors.len() != cfg.predecessors[successor].len() {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.PHI_PREDECESSOR",
                    Some(successor_id),
                    None,
                    vec![destination],
                    "phi does not provide exactly one source for every predecessor",
                ));
            }
        }
        for edge_use in 0..program.extra_phi_edge_use_count(successor) {
            let (predecessor_id, value, phi) = program.extra_phi_edge_use(successor, edge_use);
            let Some(&predecessor) = cfg.block_index.get(&predecessor_id) else {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.EXTRA_EDGE_PREDECESSOR",
                    Some(successor_id),
                    None,
                    vec![value],
                    format!("additional edge use references missing predecessor {predecessor_id}"),
                ));
            };
            if !cfg.predecessors[successor].contains(&predecessor) {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.EXTRA_EDGE_PREDECESSOR",
                    Some(successor_id),
                    None,
                    vec![value],
                    "additional edge use is not on a normalized CFG edge",
                ));
            }
            let site = UseSite::PhiEdge {
                predecessor: predecessor_id,
                successor: successor_id,
                phi,
                slot: slots[predecessor].exit,
            };
            record_use(&mut uses, value, site)?;
            edge_uses
                .entry((predecessor, successor))
                .or_default()
                .insert(value);
            blocks[predecessor]
                .last_use
                .entry(value)
                .and_modify(|current| *current = (*current).max(slots[predecessor].exit))
                .or_insert(slots[predecessor].exit);
        }
    }
    for value_uses in &mut uses {
        value_uses.sort_unstable();
        value_uses.dedup();
    }

    Ok(ModelFacts {
        definitions,
        uses,
        blocks,
        phi_definitions,
        edge_uses,
    })
}

fn solve_liveness(
    block_count: usize,
    cfg: &NormalizedCfg,
    facts: &ModelFacts,
) -> (Vec<HashSet<VReg>>, Vec<HashSet<VReg>>) {
    let mut live_in = (0..block_count).map(|_| HashSet::new()).collect::<Vec<_>>();
    let mut live_out = live_in.clone();
    let mut queue = (0..block_count).rev().collect::<VecDeque<_>>();
    let mut queued = vec![true; block_count];
    while let Some(block) = queue.pop_front() {
        queued[block] = false;
        let mut next_out = HashSet::new();
        for &successor in &cfg.successors[block] {
            next_out.extend(live_in[successor].iter().copied());
            if let Some(edge) = facts.edge_uses.get(&(block, successor)) {
                next_out.extend(edge.iter().copied());
            }
        }
        let mut next_in = facts.blocks[block].upward_uses.clone();
        next_in.extend(
            next_out
                .iter()
                .copied()
                .filter(|value| !facts.blocks[block].definitions.contains(value)),
        );
        if next_in != live_in[block] || next_out != live_out[block] {
            live_in[block] = next_in;
            live_out[block] = next_out;
            for &predecessor in &cfg.predecessors[block] {
                if !queued[predecessor] {
                    queued[predecessor] = true;
                    queue.push_back(predecessor);
                }
            }
        }
    }
    (live_in, live_out)
}

fn build_intervals<P: LivenessProgram + ?Sized>(
    program: &P,
    cfg: &NormalizedCfg,
    slots: &[BlockSlots],
    facts: &ModelFacts,
    live_in: &[HashSet<VReg>],
    live_out: &[HashSet<VReg>],
) -> Result<Vec<Option<LiveInterval>>, LiveIntervalError> {
    let mut segments = vec![Vec::<LiveSegment>::new(); facts.definitions.len()];
    for block_index in 0..program.block_count() {
        let block_id = program.block_id(block_index);
        let mut values = HashSet::new();
        values.extend(live_in[block_index].iter().copied());
        values.extend(live_out[block_index].iter().copied());
        values.extend(facts.blocks[block_index].definitions.iter().copied());
        values.extend(facts.blocks[block_index].last_use.keys().copied());
        let block_slots = &slots[block_index];
        for value in values {
            let Some(definition) = facts.definitions.get(value.0 as usize).copied().flatten()
            else {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.MISSING_DEFINITION",
                    Some(block_id),
                    None,
                    vec![value],
                    "live or used value has no MIR definition",
                ));
            };
            let definition_block = cfg.block_index[&definition.block()];
            let starts_live = live_in[block_index].contains(&value);
            if starts_live && definition_block == block_index {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.USE_BEFORE_DEFINITION",
                    Some(block_id),
                    None,
                    vec![value],
                    "value is live at entry of its defining block",
                ));
            }
            let start = if definition_block == block_index {
                definition.slot()
            } else {
                block_slots.entry
            };
            let end = if live_out[block_index].contains(&value) {
                block_slots.exit.next()
            } else if let Some(&last_use) = facts.blocks[block_index].last_use.get(&value) {
                last_use.next()
            } else if definition_block == block_index {
                definition.slot().next()
            } else {
                None
            }
            .ok_or_else(|| {
                LiveIntervalError::new(
                    "LIVE_INTERVAL.SLOT_RANGE",
                    Some(block_id),
                    None,
                    vec![value],
                    "live segment end overflows or has no local reason to exist",
                )
            })?;
            if start >= end {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.EMPTY_SEGMENT",
                    Some(block_id),
                    None,
                    vec![value],
                    format!("segment {start:?}..{end:?} is empty or reversed"),
                ));
            }
            segments[value.0 as usize].push(LiveSegment {
                block: block_id,
                start,
                end,
            });
        }
    }

    let mut intervals = Vec::with_capacity(facts.definitions.len());
    for (value, definition) in facts.definitions.iter().copied().enumerate() {
        let value = VReg(value as u32);
        match definition {
            Some(DefinitionSite::Phi { .. }) if facts.uses[value.0 as usize].is_empty() => {
                intervals.push(None);
            }
            Some(definition) => {
                let mut value_segments = std::mem::take(&mut segments[value.0 as usize]);
                value_segments.sort_unstable_by_key(|segment| (segment.block, segment.start));
                intervals.push(Some(LiveInterval {
                    value,
                    definition,
                    segments: value_segments,
                    uses: facts.uses[value.0 as usize].clone().into(),
                }));
            }
            None if facts.uses[value.0 as usize].is_empty() => intervals.push(None),
            None => {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.MISSING_DEFINITION",
                    facts.uses[value.0 as usize]
                        .first()
                        .map(|site| site.block()),
                    None,
                    vec![value],
                    "used value has no MIR definition",
                ));
            }
        }
    }
    Ok(intervals)
}

impl LiveIntervals {
    /// Verify liveness without reusing the construction's live-in/live-out
    /// sets.  Entry/exit sets are reconstructed from interval coverage and
    /// checked against fresh MIR use/def and phi-edge equations.
    pub(super) fn verify(
        &self,
        func: &MFunction,
        cfg: &NormalizedCfg,
    ) -> Result<(), LiveIntervalError> {
        self.verify_program(func, cfg)
    }

    pub(super) fn verify_program<P: LivenessProgram + ?Sized>(
        &self,
        program: &P,
        cfg: &NormalizedCfg,
    ) -> Result<(), LiveIntervalError> {
        check_model_shape(program, cfg)?;
        if self.block_slots.len() != program.block_count()
            || self.intervals.len() != program.value_count() as usize
        {
            return Err(LiveIntervalError::new(
                "LIVE_INTERVAL.CACHED_SHAPE",
                None,
                None,
                Vec::new(),
                "cached slots or intervals do not cover the MIR function",
            ));
        }
        let expected_slots = assign_slots(program)?;
        if self.block_slots != expected_slots {
            return Err(LiveIntervalError::new(
                "LIVE_INTERVAL.SLOT_IDENTITY",
                None,
                None,
                Vec::new(),
                "cached slot indexes differ from an independent MIR layout",
            ));
        }
        let facts = collect_facts(program, cfg, &expected_slots)?;
        let dominators = DominatorIntervals::build(program, cfg)?;
        let mut cached_in = (0..program.block_count())
            .map(|_| HashSet::new())
            .collect::<Vec<_>>();
        let mut cached_out = cached_in.clone();

        for (value_index, interval) in self.intervals.iter().enumerate() {
            let value = VReg(value_index as u32);
            let Some(interval) = interval else {
                let unused_phi = matches!(
                    facts.definitions[value_index],
                    Some(DefinitionSite::Phi { .. })
                ) && facts.uses[value_index].is_empty();
                if (!unused_phi && facts.definitions[value_index].is_some())
                    || !facts.uses[value_index].is_empty()
                {
                    return Err(LiveIntervalError::new(
                        "LIVE_INTERVAL.MISSING_INTERVAL",
                        None,
                        None,
                        vec![value],
                        "defined or used value has no cached interval",
                    ));
                }
                continue;
            };
            if interval.value != value
                || Some(interval.definition) != facts.definitions[value_index]
                || interval.uses.as_slice() != facts.uses[value_index]
            {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.VALUE_IDENTITY",
                    Some(interval.definition.block()),
                    None,
                    vec![value],
                    "cached definition or use list differs from MIR",
                ));
            }
            let mut previous = None::<LiveSegment>;
            for &segment in &interval.segments {
                let Some(&block) = cfg.block_index.get(&segment.block) else {
                    return Err(LiveIntervalError::new(
                        "LIVE_INTERVAL.SEGMENT_BLOCK",
                        Some(segment.block),
                        None,
                        vec![value],
                        "segment references a missing block",
                    ));
                };
                let slots = &expected_slots[block];
                let limit = slots.exit.next().ok_or_else(|| {
                    LiveIntervalError::new(
                        "LIVE_INTERVAL.SLOT_RANGE",
                        Some(segment.block),
                        None,
                        vec![value],
                        "block exit cannot be represented as a half-open segment",
                    )
                })?;
                if segment.start < slots.entry
                    || segment.start >= segment.end
                    || segment.end > limit
                    || previous
                        .is_some_and(|old| (old.block, old.start) >= (segment.block, segment.start))
                {
                    return Err(LiveIntervalError::new(
                        "LIVE_INTERVAL.SEGMENT_SHAPE",
                        Some(segment.block),
                        None,
                        vec![value],
                        format!("invalid or unsorted segment {segment:?}"),
                    ));
                }
                if segment.contains(slots.entry) {
                    cached_in[block].insert(value);
                }
                if segment.contains(slots.exit) {
                    cached_out[block].insert(value);
                }
                previous = Some(segment);
            }
            if !interval.covers(interval.definition.block(), interval.definition.slot()) {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.DEFINITION_COVERAGE",
                    Some(interval.definition.block()),
                    None,
                    vec![value],
                    "definition is not covered by its live interval",
                ));
            }
            for &site in &interval.uses {
                if !interval.covers(site.block(), site.slot()) {
                    return Err(LiveIntervalError::new(
                        "LIVE_INTERVAL.USE_COVERAGE",
                        Some(site.block()),
                        match site {
                            UseSite::Instruction { instruction, .. } => Some(instruction),
                            UseSite::PhiEdge { .. } => None,
                        },
                        vec![value],
                        "use is not covered by its live interval",
                    ));
                }
                verify_definition_dominates_use(
                    cfg,
                    &dominators,
                    interval.definition,
                    site,
                    value,
                )?;
            }
        }

        for block in 0..program.block_count() {
            let mut expected_out = HashSet::new();
            for &successor in &cfg.successors[block] {
                expected_out.extend(cached_in[successor].iter().copied());
                if let Some(edge) = facts.edge_uses.get(&(block, successor)) {
                    expected_out.extend(edge.iter().copied());
                }
            }
            let mut expected_in = facts.blocks[block].upward_uses.clone();
            expected_in.extend(
                expected_out
                    .iter()
                    .copied()
                    .filter(|value| !facts.blocks[block].definitions.contains(value)),
            );
            if cached_out[block] != expected_out || cached_in[block] != expected_in {
                let mut values = cached_out[block]
                    .symmetric_difference(&expected_out)
                    .chain(cached_in[block].symmetric_difference(&expected_in))
                    .copied()
                    .collect::<Vec<_>>();
                values.sort_unstable();
                values.dedup();
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.DATAFLOW_EQUATION",
                    Some(program.block_id(block)),
                    None,
                    values,
                    "cached entry/exit coverage does not satisfy CFG liveness equations",
                ));
            }
            if cached_in[block]
                .iter()
                .any(|value| facts.phi_definitions[block].contains(value))
            {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.PHI_LIVE_IN",
                    Some(program.block_id(block)),
                    None,
                    Vec::new(),
                    "phi result is live before its simultaneous block-entry definition",
                ));
            }
        }
        Ok(())
    }
}

fn verify_definition_dominates_use(
    cfg: &NormalizedCfg,
    dominators: &DominatorIntervals,
    definition: DefinitionSite,
    use_site: UseSite,
    value: VReg,
) -> Result<(), LiveIntervalError> {
    let definition_block = cfg.block_index[&definition.block()];
    let use_block = cfg.block_index[&use_site.block()];
    let valid = if definition_block == use_block {
        definition.slot() < use_site.slot()
    } else {
        dominators.dominates(definition_block, use_block)
    };
    if valid {
        return Ok(());
    }
    Err(LiveIntervalError::new(
        "LIVE_INTERVAL.DEFINITION_DOMINANCE",
        Some(use_site.block()),
        match use_site {
            UseSite::Instruction { instruction, .. } => Some(instruction),
            UseSite::PhiEdge { .. } => None,
        },
        vec![value],
        format!(
            "definition in {} does not dominate use in {}",
            definition.block(),
            use_site.block()
        ),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DominatorIntervals {
    enter: Vec<usize>,
    exit: Vec<usize>,
}

impl DominatorIntervals {
    fn build<P: LivenessProgram + ?Sized>(
        program: &P,
        cfg: &NormalizedCfg,
    ) -> Result<Self, LiveIntervalError> {
        let mut children = vec![Vec::new(); program.block_count()];
        for block in 1..program.block_count() {
            let Some(parent) = cfg.idom[block] else {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.DOMINATOR_TREE",
                    Some(program.block_id(block)),
                    None,
                    Vec::new(),
                    "reachable non-entry block has no immediate dominator",
                ));
            };
            if parent >= program.block_count() {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.DOMINATOR_TREE",
                    Some(program.block_id(block)),
                    None,
                    Vec::new(),
                    "immediate dominator is outside the function",
                ));
            }
            children[parent].push(block);
        }
        let mut enter = vec![0; program.block_count()];
        let mut exit = vec![0; program.block_count()];
        let mut clock = 0usize;
        let mut stack = vec![(0usize, false)];
        while let Some((block, leaving)) = stack.pop() {
            if leaving {
                exit[block] = clock;
                clock = clock.checked_add(1).ok_or_else(|| {
                    LiveIntervalError::new(
                        "LIVE_INTERVAL.DOMINATOR_TREE",
                        Some(program.block_id(block)),
                        None,
                        Vec::new(),
                        "dominator traversal index overflows usize",
                    )
                })?;
                continue;
            }
            enter[block] = clock;
            clock = clock.checked_add(1).ok_or_else(|| {
                LiveIntervalError::new(
                    "LIVE_INTERVAL.DOMINATOR_TREE",
                    Some(program.block_id(block)),
                    None,
                    Vec::new(),
                    "dominator traversal index overflows usize",
                )
            })?;
            stack.push((block, true));
            stack.extend(children[block].iter().rev().map(|child| (*child, false)));
        }
        Ok(Self { enter, exit })
    }

    fn dominates(&self, dominator: usize, block: usize) -> bool {
        self.enter[dominator] <= self.enter[block] && self.exit[block] <= self.exit[dominator]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{
        BaseReg, MBlock, MFunction, MInst, OpSize, PhiNode, SpillDesc, VRegAllocator,
    };

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
    fn instruction_use_and_definition_slots_allow_last_use_register_reuse() {
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: VReg(0),
            value: 7,
        });
        block.push(MInst::Mov {
            dst: VReg(1),
            src: VReg(0),
        });
        block.push(MInst::Return);
        let mut function = function(2, vec![block]);
        let cfg = normalize(&mut function);
        let intervals = analyze(&function, &cfg).unwrap();
        let source = intervals.intervals[0].as_ref().unwrap();
        let destination = intervals.intervals[1].as_ref().unwrap();
        assert!(!source.interferes(destination));
        assert_eq!(
            source.segments[0].end,
            intervals.block_slots[0].instruction_clobber(1).unwrap()
        );
        assert!(
            intervals.block_slots[0].instruction_clobber(1).unwrap()
                < intervals.block_slots[0].instruction_def(1).unwrap()
        );
        assert_eq!(
            destination.segments[0].start,
            intervals.block_slots[0].instruction_def(1).unwrap()
        );
    }

    #[test]
    fn unused_phi_has_no_machine_range_but_unused_instruction_definition_does() {
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 7,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut exit = MBlock::new(BlockId(1));
        exit.phis.push(PhiNode {
            dst: VReg(1),
            sources: vec![(BlockId(0), VReg(0))],
        });
        exit.push(MInst::LoadImm {
            dst: VReg(2),
            value: 11,
        });
        exit.push(MInst::Return);
        let mut function = function(3, vec![entry, exit]);
        let cfg = normalize(&mut function);

        let intervals = analyze(&function, &cfg).unwrap();
        intervals.verify(&function, &cfg).unwrap();

        assert!(intervals.intervals[0].is_some());
        assert!(intervals.intervals[1].is_none());
        let instruction = intervals.intervals[2].as_ref().unwrap();
        assert!(instruction.uses.is_empty());
        assert!(matches!(
            instruction.definition,
            DefinitionSite::Instruction { .. }
        ));
    }

    #[test]
    fn block_local_slots_do_not_renumber_an_unchanged_successor() {
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 7,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut exit = MBlock::new(BlockId(1));
        exit.push(MInst::Mov {
            dst: VReg(1),
            src: VReg(0),
        });
        exit.push(MInst::Return);
        let mut before = function(2, vec![entry.clone(), exit.clone()]);
        let before_cfg = normalize(&mut before);
        let before_intervals = analyze(&before, &before_cfg).unwrap();

        entry.insts.insert(
            1,
            MInst::Mov {
                dst: VReg(2),
                src: VReg(0),
            },
        );
        let mut after = function(3, vec![entry, exit]);
        let after_cfg = normalize(&mut after);
        let after_intervals = analyze(&after, &after_cfg).unwrap();

        assert_ne!(
            before_intervals.block_slots[0],
            after_intervals.block_slots[0]
        );
        assert_eq!(
            before_intervals.block_slots[1],
            after_intervals.block_slots[1]
        );
    }

    #[test]
    fn incremental_liveness_rebuilds_only_values_crossing_a_changed_block() {
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 7,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut exit = MBlock::new(BlockId(1));
        exit.push(MInst::Mov {
            dst: VReg(1),
            src: VReg(0),
        });
        exit.push(MInst::Return);
        let mut function = function(3, vec![entry, exit]);
        let cfg = normalize(&mut function);
        let mut intervals = analyze(&function, &cfg).unwrap();
        let mut incremental = IncrementalLiveness::build(&function, &cfg, &intervals).unwrap();
        let unchanged_successor_slots = intervals.block_slots[1].clone();

        function.blocks[0].insts.insert(
            1,
            MInst::LoadImm {
                dst: VReg(2),
                value: 11,
            },
        );
        let changed = incremental
            .update(
                &function,
                &cfg,
                &mut intervals,
                &BTreeSet::from([BlockId(0)]),
            )
            .unwrap();
        let rebuilt = analyze(&function, &cfg).unwrap();

        assert_eq!(intervals, rebuilt);
        assert_eq!(intervals.block_slots[1], unchanged_successor_slots);
        assert_eq!(changed, vec![VReg(0), VReg(2)]);
    }

    #[test]
    fn incremental_liveness_rebuilds_a_rewritten_phi_predecessor_row() {
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
            value: 13,
        });
        right.push(MInst::Jump { target: BlockId(3) });
        let mut merge = MBlock::new(BlockId(3));
        merge.phis.push(PhiNode {
            dst: VReg(3),
            sources: vec![(BlockId(1), VReg(1)), (BlockId(2), VReg(2))],
        });
        merge.push(MInst::Return);
        let mut function = function(5, vec![entry, left, right, merge]);
        let cfg = normalize(&mut function);
        let mut intervals = analyze(&function, &cfg).unwrap();
        let mut incremental = IncrementalLiveness::build(&function, &cfg, &intervals).unwrap();

        let left_index = cfg.block_index[&BlockId(1)];
        let merge_index = cfg.block_index[&BlockId(3)];
        function.blocks[left_index].insts.insert(
            1,
            MInst::Mov {
                dst: VReg(4),
                src: VReg(1),
            },
        );
        function.blocks[merge_index].phis[0]
            .sources
            .iter_mut()
            .find(|(predecessor, _)| *predecessor == BlockId(1))
            .unwrap()
            .1 = VReg(4);
        let changed = incremental
            .update(
                &function,
                &cfg,
                &mut intervals,
                &BTreeSet::from([BlockId(1)]),
            )
            .unwrap();

        assert_eq!(intervals, analyze(&function, &cfg).unwrap());
        assert_eq!(changed, vec![VReg(1), VReg(4)]);
        assert!(matches!(
            intervals.intervals[4].as_ref().unwrap().uses.as_slice(),
            [UseSite::PhiEdge {
                predecessor: BlockId(1),
                successor: BlockId(3),
                ..
            }]
        ));
    }

    #[test]
    fn diamond_arm_values_do_not_interfere_but_phi_sources_are_edge_live() {
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
        let intervals = analyze(&function, &cfg).unwrap();
        let left = intervals.intervals[1].as_ref().unwrap();
        let right = intervals.intervals[2].as_ref().unwrap();
        let left_block = cfg.block_index[&BlockId(1)];
        let right_block = cfg.block_index[&BlockId(2)];
        assert!(!left.interferes(right));
        assert!(left.covers(BlockId(1), intervals.block_slots[left_block].exit));
        assert!(right.covers(BlockId(2), intervals.block_slots[right_block].exit));
        assert!(matches!(left.uses.last(), Some(UseSite::PhiEdge { .. })));
        assert!(matches!(right.uses.last(), Some(UseSite::PhiEdge { .. })));
    }

    #[test]
    fn loop_carried_phi_source_is_live_on_the_backedge() {
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 0,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut header = MBlock::new(BlockId(1));
        header.phis.push(PhiNode {
            dst: VReg(1),
            sources: vec![(BlockId(0), VReg(0)), (BlockId(2), VReg(2))],
        });
        header.push(MInst::Branch {
            cond: VReg(1),
            true_bb: BlockId(2),
            false_bb: BlockId(3),
        });
        let mut body = MBlock::new(BlockId(2));
        body.push(MInst::AddImm {
            dst: VReg(2),
            src: VReg(1),
            imm: 1,
        });
        body.push(MInst::Jump { target: BlockId(1) });
        let mut exit = MBlock::new(BlockId(3));
        exit.push(MInst::Return);
        let mut function = function(3, vec![entry, header, body, exit]);
        let cfg = normalize(&mut function);
        let intervals = analyze(&function, &cfg).unwrap();
        let backedge = intervals.intervals[2].as_ref().unwrap();
        let body_index = cfg.block_index[&BlockId(2)];
        assert!(backedge.covers(BlockId(2), intervals.block_slots[body_index].exit));
        intervals.verify(&function, &cfg).unwrap();
    }

    #[test]
    fn independent_verifier_rejects_a_missing_edge_segment() {
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 9,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut exit = MBlock::new(BlockId(1));
        exit.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: VReg(0),
            size: OpSize::S64,
        });
        exit.push(MInst::Return);
        let mut function = function(1, vec![entry, exit]);
        let cfg = normalize(&mut function);
        let mut intervals = analyze(&function, &cfg).unwrap();
        let entry = cfg.block_index[&BlockId(0)];
        let exit_slot = intervals.block_slots[entry].exit;
        let segment = intervals.intervals[0]
            .as_mut()
            .unwrap()
            .segments
            .iter_mut()
            .find(|segment| segment.block == BlockId(0))
            .unwrap();
        segment.end = exit_slot;
        let error = intervals.verify(&function, &cfg).unwrap_err();
        assert_eq!(error.rule, "LIVE_INTERVAL.DATAFLOW_EQUATION");
    }

    #[test]
    fn long_cfg_keeps_one_sparse_segment_per_live_block() {
        const BLOCKS: usize = 4096;
        let mut blocks = Vec::with_capacity(BLOCKS);
        for index in 0..BLOCKS {
            let mut block = MBlock::new(BlockId(index as u32));
            if index == 0 {
                block.push(MInst::LoadImm {
                    dst: VReg(0),
                    value: 1,
                });
            }
            if index + 1 == BLOCKS {
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(0),
                    size: OpSize::S64,
                });
                block.push(MInst::Return);
            } else {
                block.push(MInst::Jump {
                    target: BlockId((index + 1) as u32),
                });
            }
            blocks.push(block);
        }
        let mut function = function(1, blocks);
        let cfg = normalize(&mut function);
        let intervals = analyze(&function, &cfg).unwrap();
        assert_eq!(
            intervals.intervals[0].as_ref().unwrap().segments.len(),
            BLOCKS
        );
    }
}
