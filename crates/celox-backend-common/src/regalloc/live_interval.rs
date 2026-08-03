//! Exact sparse live intervals over opcode-free allocation facts.
//!
//! Backends retain their own machine IR. This module only sees the normalized
//! control-flow, SSA definitions, uses, and phi edges exported at the
//! allocation boundary.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::hash::Hash;

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

/// Sparse per-block live interval for one target-owned virtual register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveInterval<V> {
    pub value: V,
    pub segments: Vec<LiveSegment>,
}

impl<V> LiveInterval<V> {
    pub fn segment_in_block(&self, block: usize) -> Option<LiveSegment> {
        self.segments
            .binary_search_by_key(&block, |segment| segment.block)
            .ok()
            .map(|index| self.segments[index])
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
}

/// Exact SSA liveness reconstructed from allocation facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveIntervals<V> {
    intervals: BTreeMap<V, LiveInterval<V>>,
    live_in: Vec<BTreeSet<V>>,
    live_out: Vec<BTreeSet<V>>,
}

impl<V: Ord> LiveIntervals<V> {
    pub fn get(&self, value: &V) -> Option<&LiveInterval<V>> {
        self.intervals.get(value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&V, &LiveInterval<V>)> {
        self.intervals.iter()
    }

    pub fn live_in(&self, block: usize) -> Option<&BTreeSet<V>> {
        self.live_in.get(block)
    }

    pub fn live_out(&self, block: usize) -> Option<&BTreeSet<V>> {
        self.live_out.get(block)
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

struct BlockFacts<V> {
    definitions: BTreeSet<V>,
    upward_uses: BTreeSet<V>,
    last_use: BTreeMap<V, u64>,
}

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
    blocks: Vec<BlockFacts<V>>,
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
            blocks[block_index].definitions.insert(phi.destination);
        }

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
                if !seen_definitions.contains(&value) {
                    blocks[block_index].upward_uses.insert(value);
                }
                blocks[block_index]
                    .last_use
                    .entry(value)
                    .and_modify(|current| *current = (*current).max(use_slot))
                    .or_insert(use_slot);
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
                blocks[block_index].definitions.insert(value);
                seen_definitions.insert(value);
            }
        }
    }

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
        blocks,
        edge_uses,
    })
}

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
    let (live_in, live_out) = solve_liveness(facts, &predecessors, &model);
    let mut segments = BTreeMap::<V, Vec<LiveSegment>>::new();

    for block in 0..facts.blocks.len() {
        let mut values = BTreeSet::new();
        values.extend(live_in[block].iter().copied());
        values.extend(live_out[block].iter().copied());
        values.extend(model.blocks[block].definitions.iter().copied());
        values.extend(model.blocks[block].last_use.keys().copied());
        for value in values {
            let Some(definition) = model.definitions.get(&value).copied() else {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.MISSING_DEFINITION",
                    Some(block),
                    None,
                    vec![value],
                    "live or used value has no target-MIR definition",
                ));
            };
            if definition.block == block && live_in[block].contains(&value) {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.USE_BEFORE_DEFINITION",
                    Some(block),
                    definition.instruction,
                    vec![value],
                    "value is live at entry of its defining block",
                ));
            }
            let start = if definition.block == block {
                definition.slot
            } else {
                0
            };
            let end = if live_out[block].contains(&value) {
                slots[block].exit.checked_add(1)
            } else if let Some(last_use) = model.blocks[block].last_use.get(&value) {
                last_use.checked_add(1)
            } else if definition.block == block {
                definition.slot.checked_add(1)
            } else {
                None
            }
            .ok_or_else(|| {
                LiveIntervalError::new(
                    "LIVE_INTERVAL.SLOT_RANGE",
                    Some(block),
                    None,
                    vec![value],
                    "live segment end overflows or has no local reason to exist",
                )
            })?;
            if start >= end {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.EMPTY_SEGMENT",
                    Some(block),
                    None,
                    vec![value],
                    format!("segment {start}..{end} is empty or reversed"),
                ));
            }
            segments
                .entry(value)
                .or_default()
                .push(LiveSegment { block, start, end });
        }
    }

    let mut intervals = BTreeMap::new();
    for (&value, &definition) in &model.definitions {
        let mut value_segments = segments.remove(&value).unwrap_or_default();
        value_segments.sort_unstable_by_key(|segment| (segment.block, segment.start));
        let interval = LiveInterval {
            value,
            segments: value_segments,
        };
        if !interval
            .segment_in_block(definition.block)
            .is_some_and(|segment| segment.contains(definition.slot))
        {
            return Err(LiveIntervalError::new(
                "LIVE_INTERVAL.DEFINITION_COVERAGE",
                Some(definition.block),
                definition.instruction,
                vec![value],
                "definition is not covered by its live interval",
            ));
        }
        for site in model.uses.get(&value).into_iter().flatten() {
            let covered = interval
                .segment_in_block(site.block)
                .is_some_and(|segment| segment.contains(site.slot));
            let dominated = if definition.block == site.block {
                definition.slot < site.slot
            } else {
                dominators.dominates(definition.block, site.block)
            };
            if !covered || !dominated {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.DEFINITION_DOMINANCE",
                    Some(site.block),
                    site.instruction,
                    vec![value],
                    "definition does not dominate the target-MIR use",
                ));
            }
        }
        intervals.insert(value, interval);
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
        live_in,
        live_out,
    })
}

#[cfg(test)]
mod tests {
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
