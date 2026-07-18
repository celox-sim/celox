//! Exact sparse live intervals for allocation-owned splitting.
//!
//! This model deliberately separates semantic liveness from next-use and
//! spill-cost heuristics.  Every block entry, phi definition, instruction use,
//! instruction definition, phi edge use, and block exit has a stable slot.
//! A value may have one segment per block; mutually exclusive CFG arms do not
//! interfere merely because their blocks are adjacent in layout.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt;

use crate::backend::native::mir::{BlockId, MFunction, VReg};

use super::cfg::NormalizedCfg;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct SlotIndex(u64);

impl SlotIndex {
    fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BlockSlots {
    pub entry: SlotIndex,
    pub phi_def: SlotIndex,
    pub exit: SlotIndex,
    instruction_count: usize,
}

impl BlockSlots {
    pub fn instruction_use(self, instruction: usize) -> Option<SlotIndex> {
        (instruction < self.instruction_count)
            .then(|| {
                u64::try_from(instruction)
                    .ok()?
                    .checked_mul(2)?
                    .checked_add(2)?
                    .checked_add(self.entry.0)
                    .map(SlotIndex)
            })
            .flatten()
    }

    pub fn instruction_def(self, instruction: usize) -> Option<SlotIndex> {
        self.instruction_use(instruction)?.next()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LiveSegment {
    pub block: BlockId,
    pub start: SlotIndex,
    pub end: SlotIndex,
}

impl LiveSegment {
    fn contains(self, slot: SlotIndex) -> bool {
        self.start <= slot && slot < self.end
    }

    fn overlaps(self, other: Self) -> bool {
        self.block == other.block && self.start < other.end && other.start < self.end
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LiveInterval {
    pub value: VReg,
    pub definition: DefinitionSite,
    pub segments: Vec<LiveSegment>,
    pub uses: Vec<UseSite>,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LiveIntervals {
    pub block_slots: Vec<BlockSlots>,
    pub intervals: Vec<Option<LiveInterval>>,
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

pub(super) fn analyze(
    func: &MFunction,
    cfg: &NormalizedCfg,
) -> Result<LiveIntervals, LiveIntervalError> {
    check_model_shape(func, cfg)?;
    let block_slots = assign_slots(func)?;
    let facts = collect_facts(func, cfg, &block_slots)?;
    let (live_in, live_out) = solve_liveness(func, cfg, &facts);
    let intervals = build_intervals(func, cfg, &block_slots, &facts, &live_in, &live_out)?;
    let result = LiveIntervals {
        block_slots,
        intervals,
    };
    result.verify(func, cfg)?;
    Ok(result)
}

fn check_model_shape(func: &MFunction, cfg: &NormalizedCfg) -> Result<(), LiveIntervalError> {
    let blocks = func.blocks.len();
    if blocks == 0
        || cfg.predecessors.len() != blocks
        || cfg.successors.len() != blocks
        || cfg.idom.len() != blocks
        || cfg.block_index.len() != blocks
    {
        return Err(LiveIntervalError::new(
            "LIVE_INTERVAL.MODEL_SHAPE",
            func.blocks.first().map(|block| block.id),
            None,
            Vec::new(),
            "normalized CFG tables do not cover a non-empty MIR function",
        ));
    }
    Ok(())
}

fn assign_slots(func: &MFunction) -> Result<Vec<BlockSlots>, LiveIntervalError> {
    let mut next = 0u64;
    let mut result = Vec::with_capacity(func.blocks.len());
    for block in &func.blocks {
        let instruction_count = u64::try_from(block.insts.len()).map_err(|_| {
            LiveIntervalError::new(
                "LIVE_INTERVAL.SLOT_RANGE",
                Some(block.id),
                None,
                Vec::new(),
                "instruction count exceeds the slot-index domain",
            )
        })?;
        let phi_def = next.checked_add(1).ok_or_else(|| {
            LiveIntervalError::new(
                "LIVE_INTERVAL.SLOT_RANGE",
                Some(block.id),
                None,
                Vec::new(),
                "phi-definition slot overflows u64",
            )
        })?;
        let exit = instruction_count
            .checked_mul(2)
            .and_then(|width| next.checked_add(width))
            .and_then(|slot| slot.checked_add(2))
            .ok_or_else(|| {
                LiveIntervalError::new(
                    "LIVE_INTERVAL.SLOT_RANGE",
                    Some(block.id),
                    None,
                    Vec::new(),
                    "block slot range overflows u64",
                )
            })?;
        result.push(BlockSlots {
            entry: SlotIndex(next),
            phi_def: SlotIndex(phi_def),
            exit: SlotIndex(exit),
            instruction_count: block.insts.len(),
        });
        next = exit.checked_add(2).ok_or_else(|| {
            LiveIntervalError::new(
                "LIVE_INTERVAL.SLOT_RANGE",
                Some(block.id),
                None,
                Vec::new(),
                "function slot range overflows u64",
            )
        })?;
    }
    Ok(result)
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

fn collect_facts(
    func: &MFunction,
    cfg: &NormalizedCfg,
    slots: &[BlockSlots],
) -> Result<ModelFacts, LiveIntervalError> {
    let value_count = func.vregs.count() as usize;
    let mut definitions = vec![None; value_count];
    let mut uses = vec![Vec::new(); value_count];
    let mut blocks = (0..func.blocks.len())
        .map(|_| BlockFacts::default())
        .collect::<Vec<_>>();
    let mut phi_definitions = (0..func.blocks.len())
        .map(|_| HashSet::new())
        .collect::<Vec<_>>();

    for (block_index, block) in func.blocks.iter().enumerate() {
        let block_slots = slots[block_index];
        for (phi_index, phi) in block.phis.iter().enumerate() {
            let site = DefinitionSite::Phi {
                block: block.id,
                phi: phi_index,
                slot: block_slots.phi_def,
            };
            record_definition(&mut definitions, phi.dst, site)?;
            blocks[block_index].definitions.insert(phi.dst);
            phi_definitions[block_index].insert(phi.dst);
        }

        let mut seen_definitions = blocks[block_index].definitions.clone();
        for (instruction, inst) in block.insts.iter().enumerate() {
            let use_slot = block_slots.instruction_use(instruction).ok_or_else(|| {
                LiveIntervalError::new(
                    "LIVE_INTERVAL.SLOT_RANGE",
                    Some(block.id),
                    Some(instruction),
                    Vec::new(),
                    "instruction-use slot is outside the block",
                )
            })?;
            let mut instruction_uses = inst.uses().to_vec();
            instruction_uses.sort_unstable();
            instruction_uses.dedup();
            for value in instruction_uses {
                let site = UseSite::Instruction {
                    block: block.id,
                    instruction,
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
            if let Some(value) = inst.def() {
                let site = DefinitionSite::Instruction {
                    block: block.id,
                    instruction,
                    slot: block_slots.instruction_def(instruction).ok_or_else(|| {
                        LiveIntervalError::new(
                            "LIVE_INTERVAL.SLOT_RANGE",
                            Some(block.id),
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
    for (successor, block) in func.blocks.iter().enumerate() {
        for (phi_index, phi) in block.phis.iter().enumerate() {
            let mut seen_predecessors = BTreeSet::new();
            for &(predecessor_id, value) in &phi.sources {
                let Some(&predecessor) = cfg.block_index.get(&predecessor_id) else {
                    return Err(LiveIntervalError::new(
                        "LIVE_INTERVAL.PHI_PREDECESSOR",
                        Some(block.id),
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
                        Some(block.id),
                        None,
                        vec![value],
                        "phi predecessor is absent from the CFG or appears more than once",
                    ));
                }
                let site = UseSite::PhiEdge {
                    predecessor: predecessor_id,
                    successor: block.id,
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
                    Some(block.id),
                    None,
                    vec![phi.dst],
                    "phi does not provide exactly one source for every predecessor",
                ));
            }
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
    func: &MFunction,
    cfg: &NormalizedCfg,
    facts: &ModelFacts,
) -> (Vec<HashSet<VReg>>, Vec<HashSet<VReg>>) {
    let mut live_in = (0..func.blocks.len())
        .map(|_| HashSet::new())
        .collect::<Vec<_>>();
    let mut live_out = live_in.clone();
    let mut queue = (0..func.blocks.len()).rev().collect::<VecDeque<_>>();
    let mut queued = vec![true; func.blocks.len()];
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

fn build_intervals(
    func: &MFunction,
    cfg: &NormalizedCfg,
    slots: &[BlockSlots],
    facts: &ModelFacts,
    live_in: &[HashSet<VReg>],
    live_out: &[HashSet<VReg>],
) -> Result<Vec<Option<LiveInterval>>, LiveIntervalError> {
    let mut segments = vec![Vec::<LiveSegment>::new(); facts.definitions.len()];
    for (block_index, block) in func.blocks.iter().enumerate() {
        let mut values = HashSet::new();
        values.extend(live_in[block_index].iter().copied());
        values.extend(live_out[block_index].iter().copied());
        values.extend(facts.blocks[block_index].definitions.iter().copied());
        values.extend(facts.blocks[block_index].last_use.keys().copied());
        let block_slots = slots[block_index];
        for value in values {
            let Some(definition) = facts.definitions.get(value.0 as usize).copied().flatten()
            else {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.MISSING_DEFINITION",
                    Some(block.id),
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
                    Some(block.id),
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
                    Some(block.id),
                    None,
                    vec![value],
                    "live segment end overflows or has no local reason to exist",
                )
            })?;
            if start >= end {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.EMPTY_SEGMENT",
                    Some(block.id),
                    None,
                    vec![value],
                    format!("segment {start:?}..{end:?} is empty or reversed"),
                ));
            }
            segments[value.0 as usize].push(LiveSegment {
                block: block.id,
                start,
                end,
            });
        }
    }

    let mut intervals = Vec::with_capacity(facts.definitions.len());
    for (value, definition) in facts.definitions.iter().copied().enumerate() {
        let value = VReg(value as u32);
        match definition {
            Some(definition) => {
                let mut value_segments = std::mem::take(&mut segments[value.0 as usize]);
                value_segments.sort_unstable_by_key(|segment| (segment.block, segment.start));
                intervals.push(Some(LiveInterval {
                    value,
                    definition,
                    segments: value_segments,
                    uses: facts.uses[value.0 as usize].clone(),
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
        check_model_shape(func, cfg)?;
        if self.block_slots.len() != func.blocks.len()
            || self.intervals.len() != func.vregs.count() as usize
        {
            return Err(LiveIntervalError::new(
                "LIVE_INTERVAL.CACHED_SHAPE",
                None,
                None,
                Vec::new(),
                "cached slots or intervals do not cover the MIR function",
            ));
        }
        let expected_slots = assign_slots(func)?;
        if self.block_slots != expected_slots {
            return Err(LiveIntervalError::new(
                "LIVE_INTERVAL.SLOT_IDENTITY",
                None,
                None,
                Vec::new(),
                "cached slot indexes differ from an independent MIR layout",
            ));
        }
        let facts = collect_facts(func, cfg, &expected_slots)?;
        let dominators = DominatorIntervals::build(func, cfg)?;
        let mut cached_in = (0..func.blocks.len())
            .map(|_| HashSet::new())
            .collect::<Vec<_>>();
        let mut cached_out = cached_in.clone();

        for (value_index, interval) in self.intervals.iter().enumerate() {
            let value = VReg(value_index as u32);
            let Some(interval) = interval else {
                if facts.definitions[value_index].is_some() || !facts.uses[value_index].is_empty() {
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
                || interval.uses != facts.uses[value_index]
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
                let slots = expected_slots[block];
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

        for block in 0..func.blocks.len() {
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
                    Some(func.blocks[block].id),
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
                    Some(func.blocks[block].id),
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

struct DominatorIntervals {
    enter: Vec<usize>,
    exit: Vec<usize>,
}

impl DominatorIntervals {
    fn build(func: &MFunction, cfg: &NormalizedCfg) -> Result<Self, LiveIntervalError> {
        let mut children = vec![Vec::new(); func.blocks.len()];
        for block in 1..func.blocks.len() {
            let Some(parent) = cfg.idom[block] else {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.DOMINATOR_TREE",
                    Some(func.blocks[block].id),
                    None,
                    Vec::new(),
                    "reachable non-entry block has no immediate dominator",
                ));
            };
            if parent >= func.blocks.len() {
                return Err(LiveIntervalError::new(
                    "LIVE_INTERVAL.DOMINATOR_TREE",
                    Some(func.blocks[block].id),
                    None,
                    Vec::new(),
                    "immediate dominator is outside the function",
                ));
            }
            children[parent].push(block);
        }
        let mut enter = vec![0; func.blocks.len()];
        let mut exit = vec![0; func.blocks.len()];
        let mut clock = 0usize;
        let mut stack = vec![(0usize, false)];
        while let Some((block, leaving)) = stack.pop() {
            if leaving {
                exit[block] = clock;
                clock = clock.checked_add(1).ok_or_else(|| {
                    LiveIntervalError::new(
                        "LIVE_INTERVAL.DOMINATOR_TREE",
                        Some(func.blocks[block].id),
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
                    Some(func.blocks[block].id),
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
            intervals.block_slots[0]
                .instruction_use(1)
                .unwrap()
                .next()
                .unwrap()
        );
        assert_eq!(
            destination.segments[0].start,
            intervals.block_slots[0].instruction_def(1).unwrap()
        );
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
