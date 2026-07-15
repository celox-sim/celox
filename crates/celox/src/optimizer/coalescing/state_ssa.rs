//! Phase-aware MemorySSA facts for exact SIR state fragments.
//!
//! SIR registers carry one logical value.  A `Logic` register therefore moves
//! the value and unknown-mask planes atomically; the plane tag below records
//! that storage contract without splitting one SIR operation into two names.
//! Dynamic and partially overlapping writes become explicit unknown memory
//! definitions (`Kill`) instead of invalidating unrelated state fragments.

use std::collections::{BTreeSet, VecDeque};
use std::fmt;

use crate::ir::cfg::SirCfg;
use crate::ir::*;
use crate::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum StatePlane {
    TwoStateValue,
    FourStateValueAndMask,
}

impl StatePlane {
    fn for_type(ty: &RegisterType) -> Self {
        match ty {
            RegisterType::Bit { .. } => Self::TwoStateValue,
            RegisterType::Logic { .. } => Self::FourStateValueAndMask,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct StateLocation {
    addr: RegionedAbsoluteAddr,
    bit_offset: usize,
    width: usize,
}

impl StateLocation {
    fn overlaps(self, other: Self) -> bool {
        self.addr == other.addr
            && self.bit_offset < other.bit_offset.saturating_add(other.width)
            && other.bit_offset < self.bit_offset.saturating_add(self.width)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct StateFragment {
    pub addr: RegionedAbsoluteAddr,
    pub plane: StatePlane,
    pub bit_offset: usize,
    pub width: usize,
}

impl StateFragment {
    pub fn from_access(
        addr: RegionedAbsoluteAddr,
        bit_offset: usize,
        width: usize,
        ty: &RegisterType,
    ) -> Self {
        Self {
            addr,
            plane: StatePlane::for_type(ty),
            bit_offset,
            width,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StatePhase {
    Unclassified,
    Comb,
    FfStableInput,
    FfWorking,
    FfStableApply,
}

#[derive(Debug, Clone, Copy)]
enum AccessRole {
    Read,
    Write,
}

#[derive(Debug, Clone, Default)]
pub(super) struct StatePhaseMap {
    ff_blocks: HashSet<BlockId>,
    classified: bool,
}

impl StatePhaseMap {
    pub fn fused(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        cfg: &SirCfg,
        ff_entry: BlockId,
    ) -> Result<Self, StateSsaError> {
        let Some(ff_entry_index) = cfg.block_index(ff_entry) else {
            return Err(StateSsaError::MissingPhaseEntry(ff_entry));
        };
        let mut ff_indices = vec![false; cfg.block_ids.len()];
        let mut work = vec![ff_entry_index];
        ff_indices[ff_entry_index] = true;
        while let Some(block) = work.pop() {
            for &successor in &cfg.successors[block] {
                if !ff_indices[successor] {
                    ff_indices[successor] = true;
                    work.push(successor);
                }
            }
        }

        // The phase boundary may have several incoming comb returns, but no
        // later FF block may be entered by bypassing that boundary.
        for (block, &is_ff) in ff_indices.iter().enumerate() {
            if !is_ff || block == ff_entry_index {
                continue;
            }
            if cfg.predecessors[block]
                .iter()
                .any(|predecessor| !ff_indices[*predecessor])
            {
                return Err(StateSsaError::PhaseBypass(cfg.block_ids[block]));
            }
        }
        if !eu.blocks.contains_key(&ff_entry) {
            return Err(StateSsaError::MissingPhaseEntry(ff_entry));
        }
        Ok(Self {
            ff_blocks: ff_indices
                .iter()
                .enumerate()
                .filter_map(|(index, is_ff)| is_ff.then_some(cfg.block_ids[index]))
                .collect(),
            classified: true,
        })
    }

    pub fn allows_ff_optimization(&self, block: BlockId) -> bool {
        !self.classified || self.ff_blocks.contains(&block)
    }

    pub fn ff_blocks(&self) -> Option<&HashSet<BlockId>> {
        self.classified.then_some(&self.ff_blocks)
    }

    fn phase(&self, block: BlockId, region: u32, role: AccessRole) -> StatePhase {
        if !self.classified {
            return StatePhase::Unclassified;
        }
        if !self.ff_blocks.contains(&block) {
            return StatePhase::Comb;
        }
        if region == WORKING_REGION || region == SPARSE_WORKING_REGION {
            return StatePhase::FfWorking;
        }
        match role {
            AccessRole::Read => StatePhase::FfStableInput,
            AccessRole::Write => StatePhase::FfStableApply,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct MemoryAccessId(pub usize);

pub(super) type MemoryVersionId = MemoryAccessId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MemoryAccessKind {
    LiveOnEntry,
    Use {
        destination: Option<RegisterId>,
        reaching: MemoryVersionId,
    },
    Def {
        source: RegisterId,
        observable: bool,
    },
    Kill,
    Phi {
        incoming: Vec<(BlockId, MemoryVersionId)>,
    },
}

impl MemoryAccessKind {
    fn defines_version(&self) -> bool {
        !matches!(self, Self::Use { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MemoryAccess {
    pub id: MemoryAccessId,
    pub slot: usize,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub phase: StatePhase,
    pub kind: MemoryAccessKind,
}

#[derive(Debug, Clone)]
pub(super) struct StateSsaSlot {
    pub fragment: StateFragment,
    pub ty: RegisterType,
    pub phi_blocks: Vec<usize>,
    pub live_in_entry: bool,
    pub has_effectful_store: bool,
    pub has_kill: bool,
    pub escapes: bool,
}

#[derive(Debug, Clone, Default)]
struct RawSlot {
    ty: Option<RegisterType>,
    invalid: bool,
    has_load: bool,
    has_store: bool,
    has_effectful_store: bool,
    has_kill: bool,
    escapes: bool,
    def_blocks: HashSet<BlockId>,
    upward_use_blocks: HashSet<BlockId>,
}

impl RawSlot {
    fn record_type(&mut self, ty: &RegisterType, width: usize) {
        if width == 0
            || ty.width() != width
            || self.ty.as_ref().is_some_and(|previous| previous != ty)
        {
            self.invalid = true;
        }
        self.ty.get_or_insert_with(|| ty.clone());
    }
}

#[derive(Debug, Clone, Copy)]
struct UseEffect {
    slot: usize,
    destination: Option<RegisterId>,
    phase: StatePhase,
}

#[derive(Debug, Clone, Copy)]
enum DefEffectKind {
    Exact {
        source: RegisterId,
        observable: bool,
    },
    Kill,
}

#[derive(Debug, Clone, Copy)]
struct DefEffect {
    slot: usize,
    phase: StatePhase,
    kind: DefEffectKind,
}

#[derive(Debug, Clone, Default)]
struct InstructionEffects {
    uses: Vec<UseEffect>,
    defs: Vec<DefEffect>,
}

#[derive(Debug, Clone, Default)]
struct InstructionAccesses {
    uses: Vec<MemoryAccessId>,
    defs: Vec<MemoryAccessId>,
}

#[derive(Debug, Clone)]
pub(super) struct StateSsa {
    pub slots: Vec<StateSsaSlot>,
    pub accesses: Vec<MemoryAccess>,
    effects: HashMap<(BlockId, usize), InstructionEffects>,
    read_versions: HashMap<(BlockId, usize, RegisterId), (usize, MemoryVersionId)>,
    entry_versions: HashMap<BlockId, Vec<MemoryVersionId>>,
    exit_versions: HashMap<BlockId, Vec<MemoryVersionId>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StateSsaError {
    MissingPhaseEntry(BlockId),
    PhaseBypass(BlockId),
    MissingRegister(RegisterId),
    MissingReachingVersion { block: BlockId, slot: usize },
    MissingPhiIncoming { block: BlockId, slot: usize },
    InvalidAccess(&'static str),
}

impl fmt::Display for StateSsaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPhaseEntry(block) => {
                write!(formatter, "state phase entry b{} is absent", block.0)
            }
            Self::PhaseBypass(block) => write!(
                formatter,
                "state phase boundary is bypassed by an edge into b{}",
                block.0
            ),
            Self::MissingRegister(register) => {
                write!(
                    formatter,
                    "state access uses unknown register r{}",
                    register.0
                )
            }
            Self::MissingReachingVersion { block, slot } => write!(
                formatter,
                "state use in b{} has no reaching version for slot {}",
                block.0, slot
            ),
            Self::MissingPhiIncoming { block, slot } => write!(
                formatter,
                "state phi in b{} has incomplete incoming versions for slot {}",
                block.0, slot
            ),
            Self::InvalidAccess(message) => write!(formatter, "invalid StateSSA access: {message}"),
        }
    }
}

impl std::error::Error for StateSsaError {}

fn overlapping_slots(
    by_address: &HashMap<RegionedAbsoluteAddr, Vec<(StateLocation, usize)>>,
    addr: RegionedAbsoluteAddr,
    offset: &SIROffset,
    width: usize,
) -> Vec<usize> {
    if width == 0 {
        return Vec::new();
    }
    let Some(locations) = by_address.get(&addr) else {
        return Vec::new();
    };
    match offset {
        SIROffset::Static(bit_offset) => {
            let access = StateLocation {
                addr,
                bit_offset: *bit_offset,
                width,
            };
            let end = bit_offset.saturating_add(width);
            let upper = locations.partition_point(|(location, _)| location.bit_offset < end);
            locations[..upper]
                .iter()
                .filter_map(|(location, slot)| location.overlaps(access).then_some(*slot))
                .collect()
        }
        SIROffset::Dynamic(_) | SIROffset::Element { .. } => {
            locations.iter().map(|(_, slot)| *slot).collect()
        }
    }
}

fn live_in_blocks(
    cfg: &SirCfg,
    def_blocks: &HashSet<BlockId>,
    upward_use_blocks: &HashSet<BlockId>,
) -> Vec<bool> {
    let mut definitions = vec![false; cfg.block_ids.len()];
    for block in def_blocks {
        definitions[cfg.index[block]] = true;
    }
    let mut live_in = vec![false; cfg.block_ids.len()];
    let mut work = VecDeque::new();
    for block in upward_use_blocks {
        let index = cfg.index[block];
        if !live_in[index] {
            live_in[index] = true;
            work.push_back(index);
        }
    }
    while let Some(block) = work.pop_front() {
        for &predecessor in &cfg.predecessors[block] {
            if !definitions[predecessor] && !live_in[predecessor] {
                live_in[predecessor] = true;
                work.push_back(predecessor);
            }
        }
    }
    live_in
}

fn phi_blocks_for_slot(cfg: &SirCfg, facts: &RawSlot, live_in: &[bool]) -> Vec<usize> {
    let definition_indices = facts
        .def_blocks
        .iter()
        .map(|block| cfg.index[block])
        .collect::<HashSet<_>>();
    let mut phi_blocks = HashSet::default();
    let mut queued = definition_indices.clone();
    let mut work = definition_indices.iter().copied().collect::<Vec<_>>();
    while let Some(definition) = work.pop() {
        for &frontier in &cfg.dominance_frontier[definition] {
            if !live_in[frontier] || !phi_blocks.insert(frontier) {
                continue;
            }
            if queued.insert(frontier) {
                work.push(frontier);
            }
        }
    }
    let mut blocks = phi_blocks.into_iter().collect::<Vec<_>>();
    blocks.sort_unstable();
    blocks
}

impl StateSsa {
    pub fn analyze(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        cfg: &SirCfg,
        region: u32,
        eligible_load_blocks: Option<&HashSet<BlockId>>,
        phases: &StatePhaseMap,
    ) -> Result<Self, StateSsaError> {
        Self::analyze_selected(eu, cfg, region, eligible_load_blocks, phases, false)
    }

    /// Build state versions for every exact load shape, including state which
    /// is never written in this execution unit.  Forwarding deliberately uses
    /// the narrower `analyze` entry point; placement needs a LiveOnEntry token
    /// even for read-only state so two source occurrences are still identified
    /// by the state version they observe.
    pub fn analyze_all_loads(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        cfg: &SirCfg,
        region: u32,
        phases: &StatePhaseMap,
    ) -> Result<Self, StateSsaError> {
        Self::analyze_selected(eu, cfg, region, None, phases, true)
    }

    fn analyze_selected(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        cfg: &SirCfg,
        region: u32,
        eligible_load_blocks: Option<&HashSet<BlockId>>,
        phases: &StatePhaseMap,
        include_read_only: bool,
    ) -> Result<Self, StateSsaError> {
        let mut raw = HashMap::<StateLocation, RawSlot>::default();

        // Discover every exact shape first.  This lets the second pass turn a
        // different shape into a kill for an overlapping candidate.
        for &block_id in &cfg.block_ids {
            let block = &eu.blocks[&block_id];
            for instruction in &block.instructions {
                match instruction {
                    SIRInstruction::Load(
                        destination,
                        addr,
                        SIROffset::Static(bit_offset),
                        width,
                    ) if addr.region == region => {
                        let ty = eu
                            .register_map
                            .get(destination)
                            .ok_or(StateSsaError::MissingRegister(*destination))?;
                        raw.entry(StateLocation {
                            addr: *addr,
                            bit_offset: *bit_offset,
                            width: *width,
                        })
                        .or_default()
                        .record_type(ty, *width);
                    }
                    SIRInstruction::Store(
                        addr,
                        SIROffset::Static(bit_offset),
                        width,
                        source,
                        _,
                        _,
                    ) if addr.region == region => {
                        let ty = eu
                            .register_map
                            .get(source)
                            .ok_or(StateSsaError::MissingRegister(*source))?;
                        raw.entry(StateLocation {
                            addr: *addr,
                            bit_offset: *bit_offset,
                            width: *width,
                        })
                        .or_default()
                        .record_type(ty, *width);
                    }
                    _ => {}
                }
            }
        }

        let mut locations = raw.keys().copied().collect::<Vec<_>>();
        locations.sort_unstable();
        let raw_index = locations
            .iter()
            .copied()
            .enumerate()
            .map(|(index, location)| (location, index))
            .collect::<HashMap<_, _>>();
        let mut locations_by_address =
            HashMap::<RegionedAbsoluteAddr, Vec<(StateLocation, usize)>>::default();
        for (slot, &location) in locations.iter().enumerate() {
            locations_by_address
                .entry(location.addr)
                .or_default()
                .push((location, slot));
        }
        let mut facts = locations
            .iter()
            .map(|location| raw.remove(location).unwrap_or_default())
            .collect::<Vec<_>>();
        let mut effects = HashMap::<(BlockId, usize), InstructionEffects>::default();

        for &block_id in &cfg.block_ids {
            let block = &eu.blocks[&block_id];
            let mut defined = HashSet::<usize>::default();
            for (instruction_index, instruction) in block.instructions.iter().enumerate() {
                let mut instruction_effects = InstructionEffects::default();
                match instruction {
                    SIRInstruction::Load(destination, addr, offset, width)
                        if addr.region == region =>
                    {
                        let exact = match offset {
                            SIROffset::Static(bit_offset) => raw_index
                                .get(&StateLocation {
                                    addr: *addr,
                                    bit_offset: *bit_offset,
                                    width: *width,
                                })
                                .copied(),
                            SIROffset::Dynamic(_) | SIROffset::Element { .. } => None,
                        };
                        if eligible_load_blocks.is_none_or(|blocks| blocks.contains(&block_id))
                            && let Some(slot) = exact
                        {
                            facts[slot].has_load = true;
                            if !defined.contains(&slot) {
                                facts[slot].upward_use_blocks.insert(block_id);
                            }
                            instruction_effects.uses.push(UseEffect {
                                slot,
                                destination: Some(*destination),
                                phase: phases.phase(block_id, region, AccessRole::Read),
                            });
                        }
                        for slot in overlapping_slots(&locations_by_address, *addr, offset, *width)
                        {
                            if Some(slot) != exact {
                                facts[slot].escapes = true;
                            }
                        }
                    }
                    SIRInstruction::Store(addr, offset, width, source, triggers, capture_sites)
                        if addr.region == region =>
                    {
                        let exact = match offset {
                            SIROffset::Static(bit_offset) => raw_index
                                .get(&StateLocation {
                                    addr: *addr,
                                    bit_offset: *bit_offset,
                                    width: *width,
                                })
                                .copied(),
                            SIROffset::Dynamic(_) | SIROffset::Element { .. } => None,
                        };
                        if let Some(slot) = exact {
                            facts[slot].has_store = true;
                            facts[slot].has_effectful_store |=
                                !triggers.is_empty() || !capture_sites.is_empty();
                            facts[slot].def_blocks.insert(block_id);
                            defined.insert(slot);
                            instruction_effects.defs.push(DefEffect {
                                slot,
                                phase: phases.phase(block_id, region, AccessRole::Write),
                                kind: DefEffectKind::Exact {
                                    source: *source,
                                    observable: !triggers.is_empty() || !capture_sites.is_empty(),
                                },
                            });
                        }
                        for slot in overlapping_slots(&locations_by_address, *addr, offset, *width)
                        {
                            if Some(slot) != exact {
                                facts[slot].has_kill = true;
                                facts[slot].def_blocks.insert(block_id);
                                defined.insert(slot);
                                instruction_effects.defs.push(DefEffect {
                                    slot,
                                    phase: phases.phase(block_id, region, AccessRole::Write),
                                    kind: DefEffectKind::Kill,
                                });
                            }
                        }
                    }
                    SIRInstruction::Commit(source, destination, offset, width, _) => {
                        if source.region == region {
                            let exact = match offset {
                                SIROffset::Static(bit_offset) => raw_index
                                    .get(&StateLocation {
                                        addr: *source,
                                        bit_offset: *bit_offset,
                                        width: *width,
                                    })
                                    .copied(),
                                SIROffset::Dynamic(_) | SIROffset::Element { .. } => None,
                            };
                            for slot in
                                overlapping_slots(&locations_by_address, *source, offset, *width)
                            {
                                facts[slot].escapes = true;
                            }
                            if let Some(slot) = exact {
                                instruction_effects.uses.push(UseEffect {
                                    slot,
                                    destination: None,
                                    phase: phases.phase(block_id, region, AccessRole::Read),
                                });
                            }
                        }
                        if destination.region == region {
                            for slot in overlapping_slots(
                                &locations_by_address,
                                *destination,
                                offset,
                                *width,
                            ) {
                                facts[slot].has_kill = true;
                                facts[slot].def_blocks.insert(block_id);
                                defined.insert(slot);
                                instruction_effects.defs.push(DefEffect {
                                    slot,
                                    phase: phases.phase(block_id, region, AccessRole::Write),
                                    kind: DefEffectKind::Kill,
                                });
                            }
                        }
                    }
                    _ => {}
                }
                instruction_effects.uses.sort_by_key(|effect| effect.slot);
                instruction_effects.uses.dedup_by_key(|effect| effect.slot);
                instruction_effects.defs.sort_by_key(|effect| effect.slot);
                instruction_effects.defs.dedup_by_key(|effect| effect.slot);
                if !instruction_effects.uses.is_empty() || !instruction_effects.defs.is_empty() {
                    effects.insert((block_id, instruction_index), instruction_effects);
                }
            }
        }

        // Compact to slots for which forwarding can actually replace a load.
        let selected = facts
            .iter()
            .enumerate()
            .filter_map(|(index, facts)| {
                (!facts.invalid && facts.has_load && (include_read_only || facts.has_store))
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        let mut remap = vec![None; facts.len()];
        for (new, &old) in selected.iter().enumerate() {
            remap[old] = Some(new);
        }
        let mut slots = Vec::with_capacity(selected.len());
        for &old in &selected {
            let ty = facts[old].ty.clone().ok_or(StateSsaError::InvalidAccess(
                "exact slot has no register type",
            ))?;
            let fragment = StateFragment::from_access(
                locations[old].addr,
                locations[old].bit_offset,
                locations[old].width,
                &ty,
            );
            let may_need_phi = facts[old]
                .def_blocks
                .iter()
                .any(|block| !cfg.dominance_frontier[cfg.index[block]].is_empty());
            let needs_liveness = region == WORKING_REGION || may_need_phi;
            let live_in = needs_liveness.then(|| {
                live_in_blocks(cfg, &facts[old].def_blocks, &facts[old].upward_use_blocks)
            });
            let phi_blocks = if may_need_phi {
                phi_blocks_for_slot(
                    cfg,
                    &facts[old],
                    live_in
                        .as_deref()
                        .expect("frontier pruning requested liveness"),
                )
            } else {
                Vec::new()
            };
            slots.push(StateSsaSlot {
                fragment,
                ty,
                phi_blocks,
                live_in_entry: live_in.as_ref().is_some_and(|live_in| live_in[0]),
                has_effectful_store: facts[old].has_effectful_store,
                has_kill: facts[old].has_kill,
                escapes: facts[old].escapes,
            });
        }
        effects.retain(|_, instruction_effects| {
            instruction_effects.uses = instruction_effects
                .uses
                .iter()
                .filter_map(|effect| {
                    Some(UseEffect {
                        slot: remap[effect.slot]?,
                        destination: effect.destination,
                        phase: effect.phase,
                    })
                })
                .collect();
            instruction_effects.defs = instruction_effects
                .defs
                .iter()
                .filter_map(|effect| {
                    Some(DefEffect {
                        slot: remap[effect.slot]?,
                        phase: effect.phase,
                        kind: effect.kind,
                    })
                })
                .collect();
            !instruction_effects.uses.is_empty() || !instruction_effects.defs.is_empty()
        });

        let mut state_ssa = Self {
            slots,
            accesses: Vec::new(),
            effects,
            read_versions: HashMap::default(),
            entry_versions: HashMap::default(),
            exit_versions: HashMap::default(),
        };
        state_ssa.build_access_graph(eu, cfg)?;
        state_ssa.verify(cfg)?;
        Ok(state_ssa)
    }

    fn push_access(
        &mut self,
        slot: usize,
        block: Option<BlockId>,
        instruction: Option<usize>,
        phase: StatePhase,
        kind: MemoryAccessKind,
    ) -> MemoryAccessId {
        let id = MemoryAccessId(self.accesses.len());
        self.accesses.push(MemoryAccess {
            id,
            slot,
            block,
            instruction,
            phase,
            kind,
        });
        id
    }

    fn build_access_graph(
        &mut self,
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        cfg: &SirCfg,
    ) -> Result<(), StateSsaError> {
        let live_versions = (0..self.slots.len())
            .map(|slot| {
                self.push_access(
                    slot,
                    None,
                    None,
                    StatePhase::Unclassified,
                    MemoryAccessKind::LiveOnEntry,
                )
            })
            .collect::<Vec<_>>();
        let mut phi_accesses = vec![Vec::<(usize, MemoryAccessId)>::new(); cfg.block_ids.len()];
        for slot in 0..self.slots.len() {
            for block in self.slots[slot].phi_blocks.clone() {
                let access = self.push_access(
                    slot,
                    Some(cfg.block_ids[block]),
                    None,
                    StatePhase::Unclassified,
                    MemoryAccessKind::Phi {
                        incoming: Vec::new(),
                    },
                );
                phi_accesses[block].push((slot, access));
            }
        }
        for accesses in &mut phi_accesses {
            accesses.sort_by_key(|(slot, _)| *slot);
        }

        let mut instruction_accesses = HashMap::<(BlockId, usize), InstructionAccesses>::default();
        for &block_id in &cfg.block_ids {
            for instruction in 0..eu.blocks[&block_id].instructions.len() {
                let Some(effects) = self.effects.get(&(block_id, instruction)).cloned() else {
                    continue;
                };
                let mut ids = InstructionAccesses::default();
                for effect in effects.uses {
                    let id = self.push_access(
                        effect.slot,
                        Some(block_id),
                        Some(instruction),
                        effect.phase,
                        MemoryAccessKind::Use {
                            destination: effect.destination,
                            reaching: live_versions[effect.slot],
                        },
                    );
                    ids.uses.push(id);
                }
                for effect in effects.defs {
                    let kind = match effect.kind {
                        DefEffectKind::Exact { source, observable } => {
                            MemoryAccessKind::Def { source, observable }
                        }
                        DefEffectKind::Kill => MemoryAccessKind::Kill,
                    };
                    let id = self.push_access(
                        effect.slot,
                        Some(block_id),
                        Some(instruction),
                        effect.phase,
                        kind,
                    );
                    ids.defs.push(id);
                }
                instruction_accesses.insert((block_id, instruction), ids);
            }
        }

        enum Visit {
            Enter(usize),
            Exit(Vec<usize>),
        }
        let mut versions = live_versions
            .iter()
            .copied()
            .map(|version| vec![version])
            .collect::<Vec<_>>();
        let mut visits = vec![Visit::Enter(0)];
        while let Some(visit) = visits.pop() {
            match visit {
                Visit::Exit(pushed) => {
                    for slot in pushed.into_iter().rev() {
                        versions[slot].pop();
                    }
                }
                Visit::Enter(block) => {
                    let block_id = cfg.block_ids[block];
                    let mut pushed = Vec::new();
                    for &(slot, access) in &phi_accesses[block] {
                        versions[slot].push(access);
                        pushed.push(slot);
                    }
                    self.entry_versions.insert(
                        block_id,
                        versions
                            .iter()
                            .enumerate()
                            .map(|(slot, versions)| {
                                versions.last().copied().ok_or(
                                    StateSsaError::MissingReachingVersion {
                                        block: block_id,
                                        slot,
                                    },
                                )
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                    for instruction in 0..eu.blocks[&block_id].instructions.len() {
                        let Some(accesses) =
                            instruction_accesses.get(&(block_id, instruction)).cloned()
                        else {
                            continue;
                        };
                        for access in accesses.uses {
                            let slot = self.accesses[access.0].slot;
                            let reaching = versions[slot].last().copied().ok_or(
                                StateSsaError::MissingReachingVersion {
                                    block: block_id,
                                    slot,
                                },
                            )?;
                            let MemoryAccessKind::Use {
                                reaching: use_reaching,
                                ..
                            } = &mut self.accesses[access.0].kind
                            else {
                                return Err(StateSsaError::InvalidAccess(
                                    "use table names a definition",
                                ));
                            };
                            *use_reaching = reaching;
                        }
                        for access in accesses.defs {
                            let slot = self.accesses[access.0].slot;
                            versions[slot].push(access);
                            pushed.push(slot);
                        }
                    }
                    self.exit_versions.insert(
                        block_id,
                        versions
                            .iter()
                            .enumerate()
                            .map(|(slot, versions)| {
                                versions.last().copied().ok_or(
                                    StateSsaError::MissingReachingVersion {
                                        block: block_id,
                                        slot,
                                    },
                                )
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                    for &successor in &cfg.successors[block] {
                        for &(slot, phi) in &phi_accesses[successor] {
                            let version = versions[slot].last().copied().ok_or(
                                StateSsaError::MissingReachingVersion {
                                    block: block_id,
                                    slot,
                                },
                            )?;
                            let MemoryAccessKind::Phi { incoming } = &mut self.accesses[phi.0].kind
                            else {
                                return Err(StateSsaError::InvalidAccess(
                                    "phi table names a non-phi access",
                                ));
                            };
                            incoming.push((block_id, version));
                        }
                    }
                    visits.push(Visit::Exit(pushed));
                    for &child in cfg.dom_children[block].iter().rev() {
                        visits.push(Visit::Enter(child));
                    }
                }
            }
        }
        for access in &self.accesses {
            let MemoryAccessKind::Use {
                destination: Some(destination),
                reaching,
            } = access.kind
            else {
                continue;
            };
            let (Some(block), Some(instruction)) = (access.block, access.instruction) else {
                return Err(StateSsaError::InvalidAccess(
                    "register-producing state use has no instruction location",
                ));
            };
            if self
                .read_versions
                .insert((block, instruction, destination), (access.slot, reaching))
                .is_some()
            {
                return Err(StateSsaError::InvalidAccess(
                    "state load has more than one exact version",
                ));
            }
        }
        for access in &mut self.accesses {
            if let MemoryAccessKind::Phi { incoming } = &mut access.kind {
                incoming.sort_by_key(|(predecessor, _)| *predecessor);
                incoming.dedup_by_key(|(predecessor, _)| *predecessor);
                let block = access
                    .block
                    .ok_or(StateSsaError::InvalidAccess("phi has no containing block"))?;
                if incoming.len() != cfg.predecessors[cfg.index[&block]].len() {
                    return Err(StateSsaError::MissingPhiIncoming {
                        block,
                        slot: access.slot,
                    });
                }
            }
        }
        Ok(())
    }

    pub fn verify(&self, cfg: &SirCfg) -> Result<(), StateSsaError> {
        for (index, access) in self.accesses.iter().enumerate() {
            if access.id.0 != index || access.slot >= self.slots.len() {
                return Err(StateSsaError::InvalidAccess(
                    "access identity or slot is out of range",
                ));
            }
            match &access.kind {
                MemoryAccessKind::Use { reaching, .. } => {
                    let Some(definition) = self.accesses.get(reaching.0) else {
                        return Err(StateSsaError::InvalidAccess(
                            "use reaches an absent definition",
                        ));
                    };
                    if definition.slot != access.slot || !definition.kind.defines_version() {
                        return Err(StateSsaError::InvalidAccess(
                            "use reaches a different slot or another use",
                        ));
                    }
                    let Some(use_block) = access.block else {
                        return Err(StateSsaError::InvalidAccess("use has no block"));
                    };
                    if let Some(def_block) = definition.block {
                        if def_block == use_block {
                            if let (Some(def_instruction), Some(use_instruction)) =
                                (definition.instruction, access.instruction)
                                && def_instruction >= use_instruction
                            {
                                return Err(StateSsaError::InvalidAccess(
                                    "same-block definition does not precede its use",
                                ));
                            }
                        } else if !cfg.dominates(def_block, use_block) {
                            return Err(StateSsaError::InvalidAccess(
                                "reaching definition does not dominate its use",
                            ));
                        }
                    }
                }
                MemoryAccessKind::Phi { incoming } => {
                    let block = access
                        .block
                        .ok_or(StateSsaError::InvalidAccess("phi has no containing block"))?;
                    let expected = cfg.predecessors[cfg.index[&block]]
                        .iter()
                        .map(|predecessor| cfg.block_ids[*predecessor])
                        .collect::<BTreeSet<_>>();
                    let actual = incoming
                        .iter()
                        .map(|(predecessor, _)| *predecessor)
                        .collect::<BTreeSet<_>>();
                    if expected != actual {
                        return Err(StateSsaError::MissingPhiIncoming {
                            block,
                            slot: access.slot,
                        });
                    }
                    for (_, version) in incoming {
                        if self.accesses.get(version.0).is_none_or(|definition| {
                            definition.slot != access.slot || !definition.kind.defines_version()
                        }) {
                            return Err(StateSsaError::InvalidAccess(
                                "phi incoming version is invalid",
                            ));
                        }
                    }
                }
                MemoryAccessKind::LiveOnEntry
                | MemoryAccessKind::Def { .. }
                | MemoryAccessKind::Kill => {}
            }
        }
        Ok(())
    }

    pub fn killed_slots(
        &self,
        block: BlockId,
        instruction: usize,
    ) -> impl Iterator<Item = usize> + '_ {
        self.effects
            .get(&(block, instruction))
            .into_iter()
            .flat_map(|effects| effects.defs.iter())
            .filter_map(|effect| matches!(effect.kind, DefEffectKind::Kill).then_some(effect.slot))
    }

    pub fn read_version(
        &self,
        block: BlockId,
        instruction: usize,
        destination: RegisterId,
    ) -> Option<(usize, MemoryVersionId)> {
        self.read_versions
            .get(&(block, instruction, destination))
            .copied()
    }

    pub fn entry_version(&self, block: BlockId, slot: usize) -> Option<MemoryVersionId> {
        self.entry_versions.get(&block)?.get(slot).copied()
    }

    pub fn exit_version(&self, block: BlockId, slot: usize) -> Option<MemoryVersionId> {
        self.exit_versions.get(&block)?.get(slot).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::cfg::SirCfg;
    use veryl_analyzer::ir::VarId;

    fn bit(width: usize) -> RegisterType {
        RegisterType::Bit {
            width,
            signed: false,
        }
    }

    fn address(region: u32, variable: u32) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region,
            instance_id: InstanceId(0),
            var_id: VarId::from_raw(variable),
        }
    }

    fn block(
        id: usize,
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        terminator: SIRTerminator,
    ) -> BasicBlock<RegionedAbsoluteAddr> {
        BasicBlock {
            id: BlockId(id),
            params: Vec::new(),
            instructions,
            terminator,
        }
    }

    fn unit(
        blocks: Vec<BasicBlock<RegionedAbsoluteAddr>>,
        registers: impl IntoIterator<Item = (RegisterId, RegisterType)>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: blocks.into_iter().map(|block| (block.id, block)).collect(),
            register_map: registers.into_iter().collect(),
        }
    }

    fn use_access(state: &StateSsa, destination: RegisterId) -> &MemoryAccess {
        state
            .accesses
            .iter()
            .find(|access| {
                matches!(
                    access.kind,
                    MemoryAccessKind::Use {
                        destination: Some(register),
                        ..
                    } if register == destination
                )
            })
            .expect("destination has a StateSSA use")
    }

    #[test]
    fn labels_fused_comb_and_ff_state_accesses() {
        let stable = address(STABLE_REGION, 0);
        let eu = unit(
            vec![
                block(
                    0,
                    vec![SIRInstruction::Store(
                        stable,
                        SIROffset::Static(0),
                        8,
                        RegisterId(0),
                        Vec::new(),
                        Vec::new(),
                    )],
                    SIRTerminator::Jump(BlockId(1), Vec::new()),
                ),
                block(
                    1,
                    vec![SIRInstruction::Load(
                        RegisterId(1),
                        stable,
                        SIROffset::Static(0),
                        8,
                    )],
                    SIRTerminator::Return,
                ),
            ],
            [(RegisterId(0), bit(8)), (RegisterId(1), bit(8))],
        );
        let cfg = SirCfg::analyze(&eu).unwrap();
        let phases = StatePhaseMap::fused(&eu, &cfg, BlockId(1)).unwrap();
        let eligible = HashSet::from_iter([BlockId(1)]);
        let state = StateSsa::analyze(&eu, &cfg, STABLE_REGION, Some(&eligible), &phases).unwrap();

        assert_eq!(state.slots.len(), 1);
        let definition = state
            .accesses
            .iter()
            .find(|access| matches!(access.kind, MemoryAccessKind::Def { .. }))
            .unwrap();
        let use_access = use_access(&state, RegisterId(1));
        assert_eq!(definition.phase, StatePhase::Comb);
        assert_eq!(use_access.phase, StatePhase::FfStableInput);
        assert!(matches!(
            use_access.kind,
            MemoryAccessKind::Use { reaching, .. } if reaching == definition.id
        ));
    }

    #[test]
    fn overlapping_store_is_a_path_local_kill_and_phi_input() {
        let stable = address(STABLE_REGION, 0);
        let eu = unit(
            vec![
                block(
                    0,
                    vec![SIRInstruction::Store(
                        stable,
                        SIROffset::Static(0),
                        8,
                        RegisterId(0),
                        Vec::new(),
                        Vec::new(),
                    )],
                    SIRTerminator::Branch {
                        cond: RegisterId(2),
                        true_block: (BlockId(1), Vec::new()),
                        false_block: (BlockId(2), Vec::new()),
                    },
                ),
                block(
                    1,
                    vec![SIRInstruction::Store(
                        stable,
                        SIROffset::Static(4),
                        4,
                        RegisterId(1),
                        Vec::new(),
                        Vec::new(),
                    )],
                    SIRTerminator::Jump(BlockId(3), Vec::new()),
                ),
                block(2, Vec::new(), SIRTerminator::Jump(BlockId(3), Vec::new())),
                block(
                    3,
                    vec![SIRInstruction::Load(
                        RegisterId(3),
                        stable,
                        SIROffset::Static(0),
                        8,
                    )],
                    SIRTerminator::Return,
                ),
            ],
            [
                (RegisterId(0), bit(8)),
                (RegisterId(1), bit(4)),
                (RegisterId(2), bit(1)),
                (RegisterId(3), bit(8)),
            ],
        );
        let cfg = SirCfg::analyze(&eu).unwrap();
        let state =
            StateSsa::analyze(&eu, &cfg, STABLE_REGION, None, &StatePhaseMap::default()).unwrap();

        assert_eq!(state.slots.len(), 1);
        assert!(state.slots[0].has_kill);
        assert_eq!(state.slots[0].phi_blocks, [cfg.index[&BlockId(3)]]);
        let phi = state
            .accesses
            .iter()
            .find(|access| matches!(access.kind, MemoryAccessKind::Phi { .. }))
            .unwrap();
        let use_access = use_access(&state, RegisterId(3));
        assert!(matches!(
            use_access.kind,
            MemoryAccessKind::Use { reaching, .. } if reaching == phi.id
        ));
        let MemoryAccessKind::Phi { incoming } = &phi.kind else {
            unreachable!()
        };
        assert!(
            incoming.iter().any(|(_, version)| matches!(
                state.accesses[version.0].kind,
                MemoryAccessKind::Kill
            ))
        );
        assert!(incoming.iter().any(|(_, version)| matches!(
            state.accesses[version.0].kind,
            MemoryAccessKind::Def { .. }
        )));
    }

    #[test]
    fn dynamic_store_kills_only_the_aliased_address() {
        let first = address(STABLE_REGION, 0);
        let second = address(STABLE_REGION, 1);
        let eu = unit(
            vec![block(
                0,
                vec![
                    SIRInstruction::Store(
                        first,
                        SIROffset::Static(0),
                        8,
                        RegisterId(0),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Store(
                        second,
                        SIROffset::Static(0),
                        8,
                        RegisterId(1),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Store(
                        first,
                        SIROffset::Dynamic(RegisterId(4)),
                        1,
                        RegisterId(2),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Load(RegisterId(3), first, SIROffset::Static(0), 8),
                    SIRInstruction::Load(RegisterId(5), second, SIROffset::Static(0), 8),
                ],
                SIRTerminator::Return,
            )],
            [
                (RegisterId(0), bit(8)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(1)),
                (RegisterId(3), bit(8)),
                (RegisterId(4), bit(8)),
                (RegisterId(5), bit(8)),
            ],
        );
        let cfg = SirCfg::analyze(&eu).unwrap();
        let state =
            StateSsa::analyze(&eu, &cfg, STABLE_REGION, None, &StatePhaseMap::default()).unwrap();
        let first_slot = state
            .slots
            .iter()
            .position(|slot| slot.fragment.addr == first)
            .unwrap();
        let second_slot = state
            .slots
            .iter()
            .position(|slot| slot.fragment.addr == second)
            .unwrap();

        assert!(state.slots[first_slot].has_kill);
        assert!(!state.slots[second_slot].has_kill);
        assert!(matches!(
            use_access(&state, RegisterId(3)).kind,
            MemoryAccessKind::Use { reaching, .. }
                if matches!(state.accesses[reaching.0].kind, MemoryAccessKind::Kill)
        ));
        assert!(matches!(
            use_access(&state, RegisterId(5)).kind,
            MemoryAccessKind::Use { reaching, .. }
                if matches!(state.accesses[reaching.0].kind, MemoryAccessKind::Def { .. })
        ));
    }

    #[test]
    fn disjoint_fragments_remain_independent() {
        let stable = address(STABLE_REGION, 0);
        let eu = unit(
            vec![block(
                0,
                vec![
                    SIRInstruction::Store(
                        stable,
                        SIROffset::Static(0),
                        8,
                        RegisterId(0),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Store(
                        stable,
                        SIROffset::Static(8),
                        8,
                        RegisterId(1),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Load(RegisterId(2), stable, SIROffset::Static(0), 8),
                    SIRInstruction::Load(RegisterId(3), stable, SIROffset::Static(8), 8),
                ],
                SIRTerminator::Return,
            )],
            (0..4).map(|register| (RegisterId(register), bit(8))),
        );
        let cfg = SirCfg::analyze(&eu).unwrap();
        let state =
            StateSsa::analyze(&eu, &cfg, STABLE_REGION, None, &StatePhaseMap::default()).unwrap();

        assert_eq!(state.slots.len(), 2);
        assert!(state.slots.iter().all(|slot| !slot.has_kill));
        assert_ne!(state.slots[0].fragment, state.slots[1].fragment);
    }

    #[test]
    fn loop_carried_state_gets_a_header_phi() {
        let stable = address(STABLE_REGION, 0);
        let eu = unit(
            vec![
                block(
                    0,
                    vec![SIRInstruction::Store(
                        stable,
                        SIROffset::Static(0),
                        8,
                        RegisterId(0),
                        Vec::new(),
                        Vec::new(),
                    )],
                    SIRTerminator::Jump(BlockId(1), Vec::new()),
                ),
                block(
                    1,
                    vec![SIRInstruction::Load(
                        RegisterId(2),
                        stable,
                        SIROffset::Static(0),
                        8,
                    )],
                    SIRTerminator::Branch {
                        cond: RegisterId(3),
                        true_block: (BlockId(2), Vec::new()),
                        false_block: (BlockId(3), Vec::new()),
                    },
                ),
                block(
                    2,
                    vec![SIRInstruction::Store(
                        stable,
                        SIROffset::Static(0),
                        8,
                        RegisterId(1),
                        Vec::new(),
                        Vec::new(),
                    )],
                    SIRTerminator::Jump(BlockId(1), Vec::new()),
                ),
                block(3, Vec::new(), SIRTerminator::Return),
            ],
            [
                (RegisterId(0), bit(8)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
                (RegisterId(3), bit(1)),
            ],
        );
        let cfg = SirCfg::analyze(&eu).unwrap();
        let state =
            StateSsa::analyze(&eu, &cfg, STABLE_REGION, None, &StatePhaseMap::default()).unwrap();

        assert_eq!(state.slots[0].phi_blocks, [cfg.index[&BlockId(1)]]);
        let phi = state
            .accesses
            .iter()
            .find(|access| {
                access.block == Some(BlockId(1))
                    && matches!(access.kind, MemoryAccessKind::Phi { .. })
            })
            .unwrap();
        assert!(matches!(
            use_access(&state, RegisterId(2)).kind,
            MemoryAccessKind::Use { reaching, .. } if reaching == phi.id
        ));
    }

    #[test]
    fn four_state_fragment_keeps_value_and_mask_atomic() {
        let working = address(WORKING_REGION, 0);
        let stable = address(STABLE_REGION, 0);
        let eu = unit(
            vec![block(
                0,
                vec![
                    SIRInstruction::Store(
                        working,
                        SIROffset::Static(0),
                        8,
                        RegisterId(0),
                        vec![TriggerIdWithKind {
                            kind: DomainKind::Other,
                            id: 0,
                        }],
                        Vec::new(),
                    ),
                    SIRInstruction::Load(RegisterId(1), stable, SIROffset::Static(0), 8),
                    SIRInstruction::Load(RegisterId(2), working, SIROffset::Static(0), 8),
                ],
                SIRTerminator::Return,
            )],
            [
                (RegisterId(0), RegisterType::Logic { width: 8 }),
                (RegisterId(1), RegisterType::Logic { width: 8 }),
                (RegisterId(2), RegisterType::Logic { width: 8 }),
            ],
        );
        let cfg = SirCfg::analyze(&eu).unwrap();
        let state =
            StateSsa::analyze(&eu, &cfg, WORKING_REGION, None, &StatePhaseMap::default()).unwrap();

        assert_eq!(state.slots.len(), 1);
        assert_eq!(
            state.slots[0].fragment.plane,
            StatePlane::FourStateValueAndMask
        );
        assert!(state.slots[0].has_effectful_store);
        assert!(
            !state.slots[0].escapes,
            "a stable-region read is independent"
        );
    }

    #[test]
    fn rejects_a_fused_phase_bypass() {
        let condition = RegisterId(0);
        let eu = unit(
            vec![
                block(
                    0,
                    Vec::new(),
                    SIRTerminator::Branch {
                        cond: condition,
                        true_block: (BlockId(1), Vec::new()),
                        false_block: (BlockId(2), Vec::new()),
                    },
                ),
                block(1, Vec::new(), SIRTerminator::Jump(BlockId(2), Vec::new())),
                block(2, Vec::new(), SIRTerminator::Return),
            ],
            [(condition, bit(1))],
        );
        let cfg = SirCfg::analyze(&eu).unwrap();

        assert_eq!(
            StatePhaseMap::fused(&eu, &cfg, BlockId(1)).unwrap_err(),
            StateSsaError::PhaseBypass(BlockId(2))
        );
    }
}
