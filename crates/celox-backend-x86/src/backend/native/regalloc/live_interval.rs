//! Exact sparse live intervals for native MIR and stack-home SSA.
//!
//! This model deliberately separates semantic liveness from next-use and
//! spill-cost heuristics.  Every block entry, phi definition, instruction use,
//! instruction definition, phi edge use, and block exit has a compact slot.
//! A value may have one segment per block; mutually exclusive CFG arms do not
//! interfere merely because their blocks are adjacent in layout.

use std::cmp::Ordering;
use std::collections::{BTreeSet, VecDeque};
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use crate::native::mir::{BlockId, MFunction, Uses, VReg};
use crate::{HashMap, HashSet};

use super::cfg::NormalizedCfg;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct SlotIndex(u64);

impl SlotIndex {
    pub(super) fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }

    pub(super) fn distance_to(self, end: Self) -> Option<u64> {
        end.0.checked_sub(self.0)
    }

    #[cfg(test)]
    pub(super) fn for_test(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct InstructionSlots {
    use_: SlotIndex,
    clobber: SlotIndex,
    def: SlotIndex,
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
    #[cfg(test)]
    pub fn instruction_clobber(&self, instruction: usize) -> Option<SlotIndex> {
        self.instructions
            .get(instruction)
            .map(|slots| slots.clobber)
    }

    pub fn instruction_def(&self, instruction: usize) -> Option<SlotIndex> {
        self.instructions.get(instruction).map(|slots| slots.def)
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

/// Compact immutable use row retained by a live interval.
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
    pub(super) fn segment_in_block(&self, block: BlockId) -> Option<LiveSegment> {
        self.segments
            .binary_search_by_key(&block, |segment| segment.block)
            .ok()
            .map(|index| self.segments[index])
    }

    pub fn covers(&self, block: BlockId, slot: SlotIndex) -> bool {
        self.segment_in_block(block)
            .is_some_and(|segment| segment.contains(slot))
    }

    #[cfg(test)]
    fn interferes(&self, other: &Self) -> bool {
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

/// Minimal strict-SSA program view required by exact live-interval analysis.
/// Native MIR and planned stack-home SSA share this interface.
pub(super) trait LivenessProgram {
    fn value_count(&self) -> u32;
    fn block_count(&self) -> usize;
    fn block_id(&self, block: usize) -> BlockId;
    fn phi_count(&self, block: usize) -> usize;
    fn phi_definition(&self, block: usize, phi: usize) -> VReg;
    fn phi_sources(&self, block: usize, phi: usize) -> &[(BlockId, VReg)];
    fn instruction_count(&self, block: usize) -> usize;
    fn instruction_uses(&self, block: usize, instruction: usize) -> Uses;
    fn instruction_definition(&self, block: usize, instruction: usize) -> Option<VReg>;
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

#[cfg(test)]
pub(super) fn analyze_program<P: LivenessProgram + ?Sized>(
    program: &P,
    cfg: &NormalizedCfg,
) -> Result<LiveIntervals, LiveIntervalError> {
    analyze_program_with_verification(program, cfg, true)
}

pub(super) fn analyze_program_with_verification<P: LivenessProgram + ?Sized>(
    program: &P,
    cfg: &NormalizedCfg,
    verify: bool,
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
    if verify {
        result.verify_program(program, cfg)?;
    }
    Ok(result)
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
    let entry = SlotIndex(0);
    let phi_def = SlotIndex(1);
    let mut instructions = Vec::with_capacity(block_instruction_count);
    for instruction in 0..block_instruction_count {
        let use_ = u64::try_from(instruction)
            .ok()
            .and_then(|instruction| instruction.checked_mul(3))
            .and_then(|slot| slot.checked_add(2))
            .map(SlotIndex)
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
    let exit = u64::try_from(block_instruction_count)
        .ok()
        .and_then(|instruction_count| instruction_count.checked_mul(3))
        .and_then(|slot| slot.checked_add(2))
        .map(SlotIndex)
        .ok_or_else(|| {
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
        .map(|_| HashSet::default())
        .collect::<Vec<_>>();

    for block_index in 0..program.block_count() {
        let block_id = program.block_id(block_index);
        let block_slots = &slots[block_index];
        for phi_index in 0..program.phi_count(block_index) {
            let destination = program.phi_definition(block_index, phi_index);
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
            if let Some(value) = program.instruction_definition(block_index, instruction) {
                let site = DefinitionSite::Instruction {
                    block: block_id,
                    instruction,
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

    let mut edge_uses = HashMap::<(usize, usize), HashSet<VReg>>::default();
    for successor in 0..program.block_count() {
        let successor_id = program.block_id(successor);
        for phi_index in 0..program.phi_count(successor) {
            let destination = program.phi_definition(successor, phi_index);
            let mut seen_predecessors = BTreeSet::new();
            for &(predecessor_id, value) in program.phi_sources(successor, phi_index) {
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
    let mut live_in = (0..block_count)
        .map(|_| HashSet::default())
        .collect::<Vec<_>>();
    let mut queue = (0..block_count).rev().collect::<VecDeque<_>>();
    let mut queued = vec![true; block_count];
    while let Some(block) = queue.pop_front() {
        queued[block] = false;
        let mut next_out = HashSet::default();
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
        // Only live-in is visible to predecessors. Keep the fixed point over
        // that one relation and reconstruct live-out once after convergence;
        // retaining and comparing both tables duplicates every live relation.
        if next_in != live_in[block] {
            live_in[block] = next_in;
            for &predecessor in &cfg.predecessors[block] {
                if !queued[predecessor] {
                    queued[predecessor] = true;
                    queue.push_back(predecessor);
                }
            }
        }
    }
    let live_out = (0..block_count)
        .map(|block| {
            let mut values = HashSet::default();
            for &successor in &cfg.successors[block] {
                values.extend(live_in[successor].iter().copied());
                if let Some(edge) = facts.edge_uses.get(&(block, successor)) {
                    values.extend(edge.iter().copied());
                }
            }
            values
        })
        .collect();
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
        let mut values = HashSet::default();
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
    #[cfg(test)]
    fn verify(&self, func: &MFunction, cfg: &NormalizedCfg) -> Result<(), LiveIntervalError> {
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
            .map(|_| HashSet::default())
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
            let mut expected_out = HashSet::default();
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
    use crate::native::mir::{
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
        let intervals = analyze_program(&function, &cfg).unwrap();
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

        let intervals = analyze_program(&function, &cfg).unwrap();
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
        let before_intervals = analyze_program(&before, &before_cfg).unwrap();

        entry.insts.insert(
            1,
            MInst::Mov {
                dst: VReg(2),
                src: VReg(0),
            },
        );
        let mut after = function(3, vec![entry, exit]);
        let after_cfg = normalize(&mut after);
        let after_intervals = analyze_program(&after, &after_cfg).unwrap();

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
        let intervals = analyze_program(&function, &cfg).unwrap();
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
        let intervals = analyze_program(&function, &cfg).unwrap();
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
        let mut intervals = analyze_program(&function, &cfg).unwrap();
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
        let intervals = analyze_program(&function, &cfg).unwrap();
        assert_eq!(
            intervals.intervals[0].as_ref().unwrap().segments.len(),
            BLOCKS
        );
    }
}
