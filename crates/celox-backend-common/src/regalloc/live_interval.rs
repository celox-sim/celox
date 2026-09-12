//! Exact sparse live intervals over opcode-free allocation facts.
//!
//! Backends retain their own machine IR. This module only sees the normalized
//! control-flow, SSA definitions, uses, and phi edges exported at the
//! allocation boundary.

#[cfg(test)]
use std::collections::VecDeque;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::hash::Hash;
use std::sync::OnceLock;

use super::FunctionAllocationFacts;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct DefinitionSite {
    block: usize,
    instruction: Option<usize>,
    slot: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct UseSite {
    block: usize,
    instruction: Option<usize>,
    slot: u64,
}

/// One half-open live segment within a normalized basic block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveSegment {
    pub block: usize,
    pub start: u64,
    pub end: u64,
}

impl LiveSegment {
    pub fn contains(self, point: u64) -> bool {
        self.start <= point && point < self.end
    }

    pub fn overlaps(self, other: Self) -> bool {
        self.block == other.block && self.start < other.end && other.start < self.end
    }
}

/// Storage for exact block-sparse segments, accessed without allocating.
pub trait LiveSegmentStorage {
    fn segment_len(&self) -> usize;
    fn segment_get(&self, index: usize) -> Option<LiveSegment>;
    fn segment_iter(&self) -> impl ExactSizeIterator<Item = LiveSegment>;
    fn segment_is_empty(&self) -> bool {
        self.segment_len() == 0
    }
    fn segment_first(&self) -> Option<LiveSegment> {
        self.segment_get(0)
    }
    fn segment_last(&self) -> Option<LiveSegment> {
        self.segment_len()
            .checked_sub(1)
            .and_then(|i| self.segment_get(i))
    }
}

impl LiveSegmentStorage for Vec<LiveSegment> {
    fn segment_len(&self) -> usize {
        Vec::len(self)
    }
    fn segment_get(&self, index: usize) -> Option<LiveSegment> {
        self.as_slice().get(index).copied()
    }
    fn segment_iter(&self) -> impl ExactSizeIterator<Item = LiveSegment> {
        self.as_slice().iter().copied()
    }
}

/// Twelve-byte ordinary segments, with lossless fallback for wide coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactSegments(CompactSegmentStorage);

#[derive(Debug, Clone, PartialEq, Eq)]
enum CompactSegmentStorage {
    Packed(Vec<[u32; 3]>),
    Wide(Vec<LiveSegment>),
}

impl CompactSegments {
    pub fn len(&self) -> usize {
        self.segment_len()
    }
    pub fn is_empty(&self) -> bool {
        self.segment_is_empty()
    }
    pub fn get(&self, index: usize) -> Option<LiveSegment> {
        self.segment_get(index)
    }
    pub fn first(&self) -> Option<LiveSegment> {
        self.segment_first()
    }
    pub fn last(&self) -> Option<LiveSegment> {
        self.segment_last()
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = LiveSegment> {
        self.segment_iter()
    }

    /// Sort and union overlapping or adjacent segments in each block.
    pub fn coalesce(&mut self) {
        match &mut self.0 {
            CompactSegmentStorage::Packed(segments) => {
                segments.sort_unstable();
                segments.dedup_by(|segment, previous| {
                    if previous[0] == segment[0] && segment[1] <= previous[2] {
                        previous[2] = previous[2].max(segment[2]);
                        true
                    } else {
                        false
                    }
                });
                segments.shrink_to_fit();
            }
            CompactSegmentStorage::Wide(segments) => {
                segments.sort_unstable_by_key(|s| (s.block, s.start, s.end));
                segments.dedup_by(|segment, previous| {
                    if previous.block == segment.block && segment.start <= previous.end {
                        previous.end = previous.end.max(segment.end);
                        true
                    } else {
                        false
                    }
                });
                segments.shrink_to_fit();
            }
        }
    }

    /// Expand one row for consumers that require a contiguous legacy slice.
    pub fn into_vec(self) -> Vec<LiveSegment> {
        match self.0 {
            CompactSegmentStorage::Packed(segments) => {
                segments.into_iter().map(Self::unpack).collect()
            }
            CompactSegmentStorage::Wide(segments) => segments,
        }
    }

    fn pack(segment: LiveSegment) -> Option<[u32; 3]> {
        Some([
            segment.block.try_into().ok()?,
            segment.start.try_into().ok()?,
            segment.end.try_into().ok()?,
        ])
    }
    fn unpack([block, start, end]: [u32; 3]) -> LiveSegment {
        LiveSegment {
            block: block as usize,
            start: u64::from(start),
            end: u64::from(end),
        }
    }
}

impl Default for CompactSegments {
    fn default() -> Self {
        Self(CompactSegmentStorage::Packed(Vec::new()))
    }
}

impl FromIterator<LiveSegment> for CompactSegments {
    fn from_iter<T: IntoIterator<Item = LiveSegment>>(segments: T) -> Self {
        let mut result = Self::default();
        result.extend(segments);
        result
    }
}

impl Extend<LiveSegment> for CompactSegments {
    fn extend<T: IntoIterator<Item = LiveSegment>>(&mut self, segments: T) {
        let segments = segments.into_iter();
        let additional = segments.size_hint().0;
        match &mut self.0 {
            CompactSegmentStorage::Packed(v) => v.reserve(additional),
            CompactSegmentStorage::Wide(v) => v.reserve(additional),
        }
        for segment in segments {
            match &mut self.0 {
                CompactSegmentStorage::Packed(packed) => {
                    if let Some(segment) = Self::pack(segment) {
                        packed.push(segment);
                    } else {
                        let mut wide = packed.iter().copied().map(Self::unpack).collect::<Vec<_>>();
                        wide.push(segment);
                        self.0 = CompactSegmentStorage::Wide(wide);
                    }
                }
                CompactSegmentStorage::Wide(wide) => wide.push(segment),
            }
        }
    }
}

impl LiveSegmentStorage for CompactSegments {
    fn segment_len(&self) -> usize {
        match &self.0 {
            CompactSegmentStorage::Packed(v) => v.len(),
            CompactSegmentStorage::Wide(v) => v.len(),
        }
    }
    fn segment_get(&self, index: usize) -> Option<LiveSegment> {
        match &self.0 {
            CompactSegmentStorage::Packed(v) => v.get(index).copied().map(Self::unpack),
            CompactSegmentStorage::Wide(v) => v.as_slice().get(index).copied(),
        }
    }
    fn segment_iter(&self) -> impl ExactSizeIterator<Item = LiveSegment> {
        (0..self.segment_len())
            .map(|index| self.segment_get(index).expect("segment index is in range"))
    }
}

/// Compact form of exact SSA liveness; the legacy Vec-backed API is unchanged.
pub type CompactLiveIntervals<V> = LiveIntervals<V, CompactSegments>;

/// Sparse per-block live interval for one target-owned virtual register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveInterval<V, S = Vec<LiveSegment>> {
    pub value: V,
    pub segments: S,
}

impl<V, S: LiveSegmentStorage> LiveInterval<V, S> {
    pub fn segment_in_block(&self, block: usize) -> Option<LiveSegment> {
        let mut left = 0;
        let mut right = self.segments.segment_len();
        while left < right {
            let mid = left + (right - left) / 2;
            let segment = self.segments.segment_get(mid)?;
            match segment.block.cmp(&block) {
                std::cmp::Ordering::Less => left = mid + 1,
                std::cmp::Ordering::Greater => right = mid,
                std::cmp::Ordering::Equal => return Some(segment),
            }
        }
        None
    }

    pub fn interferes(&self, other: &Self) -> bool {
        let mut left = 0;
        let mut right = 0;
        while left < self.segments.segment_len() && right < other.segments.segment_len() {
            let a = self
                .segments
                .segment_get(left)
                .expect("segment is in range");
            let b = other
                .segments
                .segment_get(right)
                .expect("segment is in range");
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
}

/// Exact SSA liveness reconstructed from allocation facts.
#[derive(Debug, Clone)]
pub struct LiveIntervals<V, S = Vec<LiveSegment>> {
    intervals: BTreeMap<V, LiveInterval<V, S>>,
    // Most allocators only query intervals. Materialize the legacy block-set
    // views on demand, without making every analysis retain them twice.
    live_in: Vec<OnceLock<BTreeSet<V>>>,
    live_out: Vec<OnceLock<BTreeSet<V>>>,
    block_exits: Vec<u64>,
}

impl<V: PartialEq, S: PartialEq> PartialEq for LiveIntervals<V, S> {
    fn eq(&self, other: &Self) -> bool {
        self.intervals == other.intervals && self.block_exits == other.block_exits
    }
}
impl<V: Eq, S: Eq> Eq for LiveIntervals<V, S> {}

impl<V: Ord, S> LiveIntervals<V, S> {
    pub fn get(&self, value: &V) -> Option<&LiveInterval<V, S>> {
        self.intervals.get(value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&V, &LiveInterval<V, S>)> {
        self.intervals.iter()
    }
}

impl<V: Copy + Ord, S: LiveSegmentStorage> LiveIntervals<V, S> {
    pub fn live_in(&self, block: usize) -> Option<&BTreeSet<V>> {
        self.live_in.get(block).map(|cache| {
            cache.get_or_init(|| {
                self.intervals
                    .iter()
                    .filter_map(|(&value, interval)| {
                        interval
                            .segment_in_block(block)
                            .filter(|segment| segment.start == 0)
                            .map(|_| value)
                    })
                    .collect()
            })
        })
    }

    pub fn live_out(&self, block: usize) -> Option<&BTreeSet<V>> {
        self.live_out.get(block).map(|cache| {
            cache.get_or_init(|| {
                self.intervals
                    .iter()
                    .filter_map(|(&value, interval)| {
                        interval
                            .segment_in_block(block)
                            .filter(|segment| segment.end == self.block_exits[block])
                            .map(|_| value)
                    })
                    .collect()
            })
        })
    }
}

/// Failure while constructing strict-SSA live intervals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveIntervalError<V> {
    pub rule: &'static str,
    pub block: Option<usize>,
    pub instruction: Option<usize>,
    pub values: Vec<V>,
    pub message: String,
}

impl<V> LiveIntervalError<V> {
    fn new(
        rule: &'static str,
        block: Option<usize>,
        instruction: Option<usize>,
        values: Vec<V>,
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

impl<V: fmt::Debug> fmt::Display for LiveIntervalError<V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.rule)?;
        if let Some(block) = self.block {
            write!(formatter, " at block {block}")?;
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

impl<V: fmt::Debug> std::error::Error for LiveIntervalError<V> {}

#[cfg(test)]
struct BlockFacts<V> {
    definitions: BTreeSet<V>,
    upward_uses: BTreeSet<V>,
    last_use: BTreeMap<V, u64>,
}

#[cfg(test)]
impl<V> Default for BlockFacts<V> {
    fn default() -> Self {
        Self {
            definitions: BTreeSet::new(),
            upward_uses: BTreeSet::new(),
            last_use: BTreeMap::new(),
        }
    }
}

struct ModelFacts<V> {
    definitions: BTreeMap<V, DefinitionSite>,
    uses: BTreeMap<V, Vec<UseSite>>,
    #[cfg(test)]
    blocks: Vec<BlockFacts<V>>,
    #[cfg(test)]
    edge_uses: BTreeMap<(usize, usize), BTreeSet<V>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockSlots {
    phi_def: u64,
    exit: u64,
}

fn instruction_use_slot(instruction: usize) -> Option<u64> {
    u64::try_from(instruction)
        .ok()?
        .checked_mul(3)?
        .checked_add(2)
}

fn instruction_def_slot(instruction: usize) -> Option<u64> {
    instruction_use_slot(instruction)?.checked_add(2)
}

fn block_slots<V, R>(
    facts: &FunctionAllocationFacts<V, R>,
) -> Result<Vec<BlockSlots>, LiveIntervalError<V>> {
    facts
        .blocks
        .iter()
        .enumerate()
        .map(|(block, facts)| {
            let exit = u64::try_from(facts.instructions.len())
                .ok()
                .and_then(|count| count.checked_mul(3))
                .and_then(|slot| slot.checked_add(2))
                .ok_or_else(|| {
                    LiveIntervalError::new(
                        "LIVE_INTERVAL.SLOT_RANGE",
                        Some(block),
                        None,
                        Vec::new(),
                        "block exit is outside the program-point domain",
                    )
                })?;
            Ok(BlockSlots { phi_def: 1, exit })
        })
        .collect()
}

fn record_definition<V: Copy + Ord>(
    definitions: &mut BTreeMap<V, DefinitionSite>,
    value: V,
    site: DefinitionSite,
) -> Result<(), LiveIntervalError<V>> {
    if let Some(previous) = definitions.insert(value, site) {
        return Err(LiveIntervalError::new(
            "LIVE_INTERVAL.MULTIPLE_DEFINITIONS",
            Some(site.block),
            site.instruction,
            vec![value],
            format!("value was already defined at {previous:?}"),
        ));
    }
    Ok(())
}

fn collect_model<V, R>(
    facts: &FunctionAllocationFacts<V, R>,
    predecessors: &[Vec<usize>],
    slots: &[BlockSlots],
) -> Result<ModelFacts<V>, LiveIntervalError<V>>
where
    V: Copy + Ord,
{
    let mut definitions = BTreeMap::new();
    let mut uses = BTreeMap::<V, Vec<UseSite>>::new();
    #[cfg(test)]
    let mut blocks = (0..facts.blocks.len())
        .map(|_| BlockFacts::default())
        .collect::<Vec<_>>();

    for (block_index, block) in facts.blocks.iter().enumerate() {
        for phi in &block.phis {
            let site = DefinitionSite {
                block: block_index,
                instruction: None,
                slot: slots[block_index].phi_def,
            };
            record_definition(&mut definitions, phi.destination, site)?;
            #[cfg(test)]
            blocks[block_index].definitions.insert(phi.destination);
        }

        #[cfg(test)]
        let mut seen_definitions = blocks[block_index].definitions.clone();
        for (instruction_index, instruction) in block.instructions.iter().enumerate() {
            let use_slot = instruction_use_slot(instruction_index).ok_or_else(|| {
                LiveIntervalError::new(
                    "LIVE_INTERVAL.SLOT_RANGE",
                    Some(block_index),
                    Some(instruction_index),
                    Vec::new(),
                    "instruction use is outside the program-point domain",
                )
            })?;
            let mut instruction_uses = instruction.uses.clone();
            instruction_uses.sort_unstable();
            instruction_uses.dedup();
            for value in instruction_uses {
                let site = UseSite {
                    block: block_index,
                    instruction: Some(instruction_index),
                    slot: use_slot,
                };
                uses.entry(value).or_default().push(site);
                #[cfg(test)]
                {
                    if !seen_definitions.contains(&value) {
                        blocks[block_index].upward_uses.insert(value);
                    }
                    blocks[block_index]
                        .last_use
                        .entry(value)
                        .and_modify(|current| *current = (*current).max(use_slot))
                        .or_insert(use_slot);
                }
            }
            let def_slot = instruction_def_slot(instruction_index).ok_or_else(|| {
                LiveIntervalError::new(
                    "LIVE_INTERVAL.SLOT_RANGE",
                    Some(block_index),
                    Some(instruction_index),
                    Vec::new(),
                    "instruction definition is outside the program-point domain",
                )
            })?;
            for &value in &instruction.defs {
                let site = DefinitionSite {
                    block: block_index,
                    instruction: Some(instruction_index),
                    slot: def_slot,
                };
                record_definition(&mut definitions, value, site)?;
                #[cfg(test)]
                {
                    blocks[block_index].definitions.insert(value);
                    seen_definitions.insert(value);
                }
            }
        }
    }

    #[cfg(test)]
    let mut edge_uses = BTreeMap::<(usize, usize), BTreeSet<V>>::new();
    for (successor, block) in facts.blocks.iter().enumerate() {
        for phi in &block.phis {
            let mut seen_predecessors = BTreeSet::new();
            for source in &phi.sources {
                if !seen_predecessors.insert(source.predecessor) {
                    return Err(LiveIntervalError::new(
                        "LIVE_INTERVAL.PHI_PREDECESSOR",
                        Some(successor),
                        None,
                        vec![source.value],
                        "phi predecessor appears more than once",
                    ));
                }
                let site = UseSite {
                    block: source.predecessor,
                    instruction: None,
                    slot: slots[source.predecessor].exit,
                };
                uses.entry(source.value).or_default().push(site);
                #[cfg(test)]
                {
                    edge_uses
                        .entry((source.predecessor, successor))
                        .or_default()
                        .insert(source.value);
                    blocks[source.predecessor]
                        .last_use
                        .entry(source.value)
                        .and_modify(|current| *current = (*current).max(site.slot))
                        .or_insert(site.slot);
                }
            }
            if seen_predecessors.len() != predecessors[successor].len() {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.PHI_PREDECESSOR",
                    Some(successor),
                    None,
                    vec![phi.destination],
                    "phi does not provide exactly one source for every predecessor",
                ));
            }
        }
    }
    for sites in uses.values_mut() {
        sites.sort_unstable();
        sites.dedup();
    }

    Ok(ModelFacts {
        definitions,
        uses,
        #[cfg(test)]
        blocks,
        #[cfg(test)]
        edge_uses,
    })
}

#[cfg(test)]
fn solve_liveness<V: Copy + Ord, R>(
    facts: &FunctionAllocationFacts<V, R>,
    predecessors: &[Vec<usize>],
    model: &ModelFacts<V>,
) -> (Vec<BTreeSet<V>>, Vec<BTreeSet<V>>) {
    let mut live_in = vec![BTreeSet::new(); facts.blocks.len()];
    let mut live_out = live_in.clone();
    let mut queue = (0..facts.blocks.len()).rev().collect::<VecDeque<_>>();
    let mut queued = vec![true; facts.blocks.len()];
    while let Some(block) = queue.pop_front() {
        queued[block] = false;
        let mut next_out = BTreeSet::new();
        for &successor in &facts.blocks[block].successors {
            next_out.extend(live_in[successor].iter().copied());
            if let Some(edge) = model.edge_uses.get(&(block, successor)) {
                next_out.extend(edge.iter().copied());
            }
        }
        let mut next_in = model.blocks[block].upward_uses.clone();
        next_in.extend(
            next_out
                .iter()
                .copied()
                .filter(|value| !model.blocks[block].definitions.contains(value)),
        );
        if next_in != live_in[block] || next_out != live_out[block] {
            live_in[block] = next_in;
            live_out[block] = next_out;
            for &predecessor in &predecessors[block] {
                if !queued[predecessor] {
                    queued[predecessor] = true;
                    queue.push_back(predecessor);
                }
            }
        }
    }
    (live_in, live_out)
}

struct DominatorTree {
    enter: Vec<usize>,
    exit: Vec<usize>,
}

impl DominatorTree {
    fn dominates(&self, dominator: usize, block: usize) -> bool {
        self.enter[dominator] <= self.enter[block] && self.exit[block] <= self.exit[dominator]
    }
}

fn compute_dominators<V>(
    entry: usize,
    successors: &[Vec<usize>],
    predecessors: &[Vec<usize>],
) -> Result<DominatorTree, LiveIntervalError<V>> {
    let mut reachable = vec![false; successors.len()];
    let mut postorder = Vec::with_capacity(successors.len());
    let mut stack = vec![(entry, 0usize)];
    reachable[entry] = true;
    while let Some((block, next_successor)) = stack.last_mut() {
        if *next_successor == successors[*block].len() {
            postorder.push(*block);
            stack.pop();
        } else {
            let successor = successors[*block][*next_successor];
            *next_successor += 1;
            if !reachable[successor] {
                reachable[successor] = true;
                stack.push((successor, 0));
            }
        }
    }
    if let Some(block) = reachable.iter().position(|reachable| !reachable) {
        return Err(LiveIntervalError::new(
            "LIVE_INTERVAL.UNREACHABLE_BLOCK",
            Some(block),
            None,
            Vec::new(),
            "allocation facts contain a block unreachable from the entry",
        ));
    }

    postorder.reverse();
    let mut rpo_position = vec![0; successors.len()];
    for (position, &block) in postorder.iter().enumerate() {
        rpo_position[block] = position;
    }
    let mut idom = vec![None; successors.len()];
    idom[entry] = Some(entry);
    let mut changed = true;
    while changed {
        changed = false;
        for &block in postorder.iter().skip(1) {
            let mut processed = predecessors[block]
                .iter()
                .copied()
                .filter(|predecessor| idom[*predecessor].is_some());
            let mut next = processed.next().ok_or_else(|| {
                LiveIntervalError::new(
                    "LIVE_INTERVAL.DOMINATOR_TREE",
                    Some(block),
                    None,
                    Vec::new(),
                    "reachable block has no processed predecessor",
                )
            })?;
            for predecessor in processed {
                next = intersect_dominators(next, predecessor, &idom, &rpo_position);
            }
            if idom[block] != Some(next) {
                idom[block] = Some(next);
                changed = true;
            }
        }
    }
    idom[entry] = None;

    let mut children = vec![Vec::new(); successors.len()];
    for (block, parent) in idom.iter().copied().enumerate() {
        if let Some(parent) = parent {
            children[parent].push(block);
        }
    }
    let mut enter = vec![0; successors.len()];
    let mut exit = vec![0; successors.len()];
    let mut clock = 0usize;
    let mut stack = vec![(entry, false)];
    while let Some((block, leaving)) = stack.pop() {
        if leaving {
            exit[block] = clock;
            clock += 1;
        } else {
            enter[block] = clock;
            clock += 1;
            stack.push((block, true));
            stack.extend(children[block].iter().rev().map(|&child| (child, false)));
        }
    }
    Ok(DominatorTree { enter, exit })
}

fn intersect_dominators(
    mut left: usize,
    mut right: usize,
    idom: &[Option<usize>],
    rpo_position: &[usize],
) -> usize {
    while left != right {
        while rpo_position[left] > rpo_position[right] {
            left = idom[left].expect("processed dominator must have a parent");
        }
        while rpo_position[right] > rpo_position[left] {
            right = idom[right].expect("processed dominator must have a parent");
        }
    }
    left
}

/// Build exact block-sparse live intervals for a strict-SSA target program.
pub fn analyze_live_intervals<V, R>(
    facts: &FunctionAllocationFacts<V, R>,
) -> Result<LiveIntervals<V>, LiveIntervalError<V>>
where
    V: Copy + Eq + Hash + Ord + fmt::Debug,
{
    analyze_intervals_with_storage(facts)
}

/// Build the same exact intervals using compact coordinate storage.
pub fn analyze_compact_live_intervals<V, R>(
    facts: &FunctionAllocationFacts<V, R>,
) -> Result<CompactLiveIntervals<V>, LiveIntervalError<V>>
where
    V: Copy + Eq + Hash + Ord + fmt::Debug,
{
    let compact: CompactLiveIntervals<V> = analyze_intervals_with_storage(facts)?;
    #[cfg(test)]
    {
        let reference: LiveIntervals<V> = analyze_intervals_with_storage(facts)?;
        assert_eq!(compact.intervals.len(), reference.intervals.len());
        for ((value, interval), (expected_value, expected)) in compact.iter().zip(reference.iter())
        {
            assert_eq!(value, expected_value);
            assert_eq!(
                interval.segments.iter().collect::<Vec<_>>(),
                expected.segments
            );
        }
    }
    Ok(compact)
}

fn analyze_intervals_with_storage<V, R, S>(
    facts: &FunctionAllocationFacts<V, R>,
) -> Result<LiveIntervals<V, S>, LiveIntervalError<V>>
where
    V: Copy + Eq + Hash + Ord + fmt::Debug,
    S: LiveSegmentStorage + FromIterator<LiveSegment>,
{
    facts.verify().map_err(|error| {
        LiveIntervalError::new(
            "LIVE_INTERVAL.ALLOCATION_FACTS",
            None,
            None,
            Vec::new(),
            error.to_string(),
        )
    })?;
    let mut predecessors = vec![Vec::new(); facts.blocks.len()];
    for (block, facts) in facts.blocks.iter().enumerate() {
        for &successor in &facts.successors {
            predecessors[successor].push(block);
        }
    }
    let successors = facts
        .blocks
        .iter()
        .map(|block| block.successors.clone())
        .collect::<Vec<_>>();
    let dominators = compute_dominators(facts.entry, &successors, &predecessors)?;
    let slots = block_slots(facts)?;
    let model = collect_model(facts, &predecessors, &slots)?;
    let block_exits = slots
        .iter()
        .map(|slot| {
            slot.exit.checked_add(1).ok_or_else(|| {
                LiveIntervalError::new(
                    "LIVE_INTERVAL.SLOT_RANGE",
                    None,
                    None,
                    Vec::new(),
                    "block exit overflows",
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut intervals = BTreeMap::new();
    // Walk backwards from each SSA use, stopping at its definition. Keep only
    // the final sparse segments, rather than two complete block/value sets in
    // addition to those segments. Scratch space is reused for every value.
    let mut ends = vec![0_u64; facts.blocks.len()];
    let mut touched = Vec::new();
    let mut pending = Vec::new();
    for (&value, &definition) in &model.definitions {
        ends[definition.block] = definition.slot + 1;
        touched.push(definition.block);
        for site in model.uses.get(&value).into_iter().flatten() {
            if definition.block == site.block && definition.slot >= site.slot {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.USE_BEFORE_DEFINITION",
                    Some(site.block),
                    site.instruction,
                    vec![value],
                    "value is used before its definition",
                ));
            }
            if definition.block != site.block && !dominators.dominates(definition.block, site.block)
            {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.DEFINITION_DOMINANCE",
                    Some(site.block),
                    site.instruction,
                    vec![value],
                    "definition does not dominate the target-MIR use",
                ));
            }
            pending.push((site.block, site.slot + 1));
        }
        while let Some((block, end)) = pending.pop() {
            let first_visit = ends[block] == 0;
            if first_visit {
                touched.push(block);
            }
            ends[block] = ends[block].max(end);
            if first_visit && block != definition.block {
                pending.extend(
                    predecessors[block]
                        .iter()
                        .map(|&predecessor| (predecessor, block_exits[predecessor])),
                );
            }
        }
        touched.sort_unstable();
        let segments = touched
            .drain(..)
            .map(|block| LiveSegment {
                block,
                start: if block == definition.block {
                    definition.slot
                } else {
                    0
                },
                end: std::mem::take(&mut ends[block]),
            })
            .collect();
        intervals.insert(value, LiveInterval { value, segments });
    }
    if let Some((&value, sites)) = model
        .uses
        .iter()
        .find(|(value, _)| !model.definitions.contains_key(value))
    {
        return Err(LiveIntervalError::new(
            "LIVE_INTERVAL.MISSING_DEFINITION",
            sites.first().map(|site| site.block),
            sites.first().and_then(|site| site.instruction),
            vec![value],
            "used value has no target-MIR definition",
        ));
    }
    Ok(LiveIntervals {
        intervals,
        live_in: (0..facts.blocks.len()).map(|_| OnceLock::new()).collect(),
        live_out: (0..facts.blocks.len()).map(|_| OnceLock::new()).collect(),
        block_exits,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn compact_segments_preserve_wide_coordinates_and_extension() {
        let narrow = LiveSegment {
            block: 3,
            start: 0,
            end: u32::MAX as u64,
        };
        let wide = LiveSegment {
            block: usize::MAX,
            start: u64::MAX - 1,
            end: u64::MAX,
        };
        let mut segments: CompactSegments = [narrow].into_iter().collect();
        assert!(matches!(segments.0, CompactSegmentStorage::Packed(_)));
        assert_eq!(segments.first(), Some(narrow));
        segments.extend([wide]);
        assert!(matches!(segments.0, CompactSegmentStorage::Wide(_)));
        assert_eq!(segments.iter().collect::<Vec<_>>(), vec![narrow, wide]);
        assert_eq!(segments.last(), Some(wide));
        assert_eq!(segments.get(2), None);
        assert_eq!(segments.into_vec(), vec![narrow, wide]);
    }

    #[test]
    fn compact_union_matches_a_wide_reference_across_batches() {
        for offset in [0, u32::MAX as u64] {
            let source = (0..128_u64)
                .map(|index| LiveSegment {
                    block: (index % 5) as usize,
                    start: offset + (index * 17) % 53,
                    end: offset + (index * 17) % 53 + 1 + index % 7,
                })
                .collect::<Vec<_>>();
            let mut sorted = source.clone();
            sorted.sort_unstable_by_key(|s| (s.block, s.start, s.end));
            let mut reference = Vec::<LiveSegment>::new();
            for segment in sorted {
                if let Some(previous) = reference.last_mut()
                    && previous.block == segment.block
                    && segment.start <= previous.end
                {
                    previous.end = previous.end.max(segment.end);
                } else {
                    reference.push(segment);
                }
            }
            for batch in [1, 7, 64, 128] {
                let mut compact = CompactSegments::default();
                for chunk in source.chunks(batch) {
                    compact.extend(chunk.iter().copied());
                    compact.coalesce();
                }
                assert_eq!(compact.into_vec(), reference);
            }
        }
    }

    use super::*;
    use crate::regalloc::{
        BlockAllocationFacts, InstructionAllocationFacts, InstructionConstraints,
        PhiAllocationFacts, PhiSource,
    };

    fn instruction(uses: Vec<u32>, defs: Vec<u32>) -> InstructionAllocationFacts<u32, ()> {
        InstructionAllocationFacts {
            uses,
            defs,
            constraints: InstructionConstraints::default(),
            is_copy: false,
        }
    }

    #[test]
    fn diamond_arms_remain_non_interfering() {
        let facts = FunctionAllocationFacts {
            entry: 0,
            blocks: vec![
                BlockAllocationFacts {
                    successors: vec![1, 2],
                    phis: Vec::new(),
                    instructions: vec![instruction(Vec::new(), vec![0])],
                },
                BlockAllocationFacts {
                    successors: vec![3],
                    phis: Vec::new(),
                    instructions: vec![instruction(vec![0], vec![1])],
                },
                BlockAllocationFacts {
                    successors: vec![3],
                    phis: Vec::new(),
                    instructions: vec![instruction(vec![0], vec![2])],
                },
                BlockAllocationFacts {
                    successors: Vec::new(),
                    phis: vec![PhiAllocationFacts {
                        destination: 3,
                        sources: vec![
                            PhiSource {
                                predecessor: 1,
                                value: 1,
                            },
                            PhiSource {
                                predecessor: 2,
                                value: 2,
                            },
                        ],
                    }],
                    instructions: vec![instruction(vec![3], Vec::new())],
                },
            ],
        };

        assert_matches_dataflow(&facts);
        let intervals = analyze_live_intervals(&facts).unwrap();
        assert!(
            !intervals
                .get(&1)
                .unwrap()
                .interferes(intervals.get(&2).unwrap())
        );
        assert!(intervals.live_out(1).unwrap().contains(&1));
        assert!(intervals.live_out(2).unwrap().contains(&2));
        assert!(!intervals.live_in(3).unwrap().contains(&1));
    }

    fn assert_matches_dataflow(facts: &FunctionAllocationFacts<u32, ()>) {
        let actual = analyze_live_intervals(facts).unwrap();
        let unqueried = actual.clone();
        assert!(actual.live_in.iter().all(|cache| cache.get().is_none()));
        assert!(actual.live_out.iter().all(|cache| cache.get().is_none()));
        let mut predecessors = vec![Vec::new(); facts.blocks.len()];
        for (block, facts) in facts.blocks.iter().enumerate() {
            for &successor in &facts.successors {
                predecessors[successor].push(block);
            }
        }
        let slots = block_slots(facts).unwrap();
        let model = collect_model(facts, &predecessors, &slots).unwrap();
        let compact = analyze_compact_live_intervals(facts).unwrap();
        let (live_in, live_out) = solve_liveness(facts, &predecessors, &model);
        for block in 0..facts.blocks.len() {
            assert_eq!(actual.live_in(block).unwrap(), &live_in[block]);
            assert_eq!(actual.live_out(block).unwrap(), &live_out[block]);
            assert_eq!(compact.live_in(block).unwrap(), &live_in[block]);
            assert_eq!(compact.live_out(block).unwrap(), &live_out[block]);
            for (&value, definition) in &model.definitions {
                let expected = if live_in[block].contains(&value)
                    || live_out[block].contains(&value)
                    || model.blocks[block].definitions.contains(&value)
                    || model.blocks[block].last_use.contains_key(&value)
                {
                    Some(LiveSegment {
                        block,
                        start: if definition.block == block {
                            definition.slot
                        } else {
                            0
                        },
                        end: if live_out[block].contains(&value) {
                            slots[block].exit + 1
                        } else {
                            model.blocks[block]
                                .last_use
                                .get(&value)
                                .copied()
                                .unwrap_or(definition.slot)
                                + 1
                        },
                    })
                } else {
                    None
                };
                assert_eq!(
                    actual.get(&value).unwrap().segment_in_block(block),
                    expected
                );
            }
        }
        assert_eq!(
            actual, unqueried,
            "queries must not change interval equality"
        );
    }

    #[test]
    fn backward_segments_match_dataflow_across_loops_and_branches() {
        let mut random = 123_u64;
        for _ in 0..32 {
            let mut blocks = Vec::new();
            for block in 0..16 {
                random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                let mut successors = if block < 15 {
                    vec![block + 1]
                } else {
                    Vec::new()
                };
                let target = 1 + ((random >> 32) as usize % 15);
                if target != block && !successors.contains(&target) {
                    successors.push(target);
                }
                let uses = if block == 0 {
                    Vec::new()
                } else {
                    (0..8).filter(|bit| (random >> bit) & 1 != 0).collect()
                };
                blocks.push(BlockAllocationFacts {
                    successors,
                    phis: Vec::new(),
                    instructions: vec![instruction(
                        uses,
                        if block == 0 {
                            (0..8).collect()
                        } else {
                            vec![8 + block as u32]
                        },
                    )],
                });
            }
            assert_matches_dataflow(&FunctionAllocationFacts { entry: 0, blocks });
        }
    }

    #[test]
    fn backward_segments_match_loop_carried_phi_lifetimes() {
        assert_matches_dataflow(&FunctionAllocationFacts {
            entry: 0,
            blocks: vec![
                BlockAllocationFacts {
                    successors: vec![1],
                    phis: Vec::new(),
                    instructions: vec![instruction(Vec::new(), vec![0])],
                },
                BlockAllocationFacts {
                    successors: vec![2, 3],
                    phis: vec![PhiAllocationFacts {
                        destination: 1,
                        sources: vec![
                            PhiSource {
                                predecessor: 0,
                                value: 0,
                            },
                            PhiSource {
                                predecessor: 2,
                                value: 2,
                            },
                        ],
                    }],
                    instructions: Vec::new(),
                },
                BlockAllocationFacts {
                    successors: vec![1],
                    phis: Vec::new(),
                    instructions: vec![instruction(vec![1], vec![2])],
                },
                BlockAllocationFacts {
                    successors: Vec::new(),
                    phis: Vec::new(),
                    instructions: vec![instruction(vec![1], Vec::new())],
                },
            ],
        });
    }

    #[test]
    fn long_live_ranges_do_not_materialize_block_sets() {
        let blocks = (0..512)
            .map(|block| BlockAllocationFacts {
                successors: if block < 511 {
                    vec![block + 1]
                } else {
                    Vec::new()
                },
                phis: Vec::new(),
                instructions: if block == 0 {
                    vec![instruction(Vec::new(), (0..256).collect())]
                } else if block == 511 {
                    vec![instruction((0..256).collect(), Vec::new())]
                } else {
                    Vec::new()
                },
            })
            .collect();
        let intervals =
            analyze_live_intervals(&FunctionAllocationFacts { entry: 0, blocks }).unwrap();
        assert_eq!(
            intervals
                .iter()
                .map(|(_, interval)| interval.segments.len())
                .sum::<usize>(),
            512 * 256
        );
        assert!(intervals.live_in.iter().all(|cache| cache.get().is_none()));
        assert!(intervals.live_out.iter().all(|cache| cache.get().is_none()));
        assert_eq!(intervals.live_in(256).unwrap().len(), 256);
    }

    #[test]
    fn rejects_missing_phi_source() {
        let facts = FunctionAllocationFacts::<u32, ()> {
            entry: 0,
            blocks: vec![
                BlockAllocationFacts {
                    successors: vec![1, 2],
                    phis: Vec::new(),
                    instructions: vec![instruction(Vec::new(), vec![0])],
                },
                BlockAllocationFacts {
                    successors: vec![2],
                    phis: Vec::new(),
                    instructions: vec![instruction(Vec::new(), vec![1])],
                },
                BlockAllocationFacts {
                    successors: Vec::new(),
                    phis: vec![PhiAllocationFacts {
                        destination: 2,
                        sources: vec![PhiSource {
                            predecessor: 0,
                            value: 0,
                        }],
                    }],
                    instructions: Vec::new(),
                },
            ],
        };

        assert_eq!(
            analyze_live_intervals(&facts).unwrap_err().rule,
            "LIVE_INTERVAL.PHI_PREDECESSOR"
        );
    }

    #[test]
    fn rejects_use_before_definition() {
        let facts = FunctionAllocationFacts::<u32, ()> {
            entry: 0,
            blocks: vec![BlockAllocationFacts {
                successors: Vec::new(),
                phis: Vec::new(),
                instructions: vec![
                    instruction(vec![0], Vec::new()),
                    instruction(Vec::new(), vec![0]),
                ],
            }],
        };

        assert_eq!(
            analyze_live_intervals(&facts).unwrap_err().rule,
            "LIVE_INTERVAL.USE_BEFORE_DEFINITION"
        );
    }
}
