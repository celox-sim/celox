//! Sparse range StateSSA for overlapping static SIR accesses.
//!
//! Exact-slot StateSSA deliberately treats a differently shaped overlapping
//! access as a kill.  Aggregate promotion needs a different identity: all
//! static access endpoints of one object partition it into non-overlapping
//! atoms, and every load/store uses or defines each atom it covers.  This file
//! builds that representation without changing SIR.  A later lazy-writeback
//! pass can therefore make one atomic rewrite only after allocation homes are
//! available.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::state_ssa::StatePlane;
use crate::ir::cfg::SirCfg;
use crate::ir::*;
use crate::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct RangeAtomId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct RangeVersionId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct StateRangeAtom {
    pub addr: RegionedAbsoluteAddr,
    pub plane: StatePlane,
    pub bit_offset: usize,
    pub width: usize,
}

impl StateRangeAtom {
    fn end(self) -> Option<usize> {
        self.bit_offset.checked_add(self.width)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RangeVersionKind {
    LiveOnEntry,
    Store {
        block: BlockId,
        instruction: usize,
        source: RegisterId,
        source_bit_offset: usize,
    },
    Phi {
        block: BlockId,
        incoming: Vec<(BlockId, RangeVersionId)>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RangeVersion {
    pub id: RangeVersionId,
    pub atom: RangeAtomId,
    pub kind: RangeVersionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RangeUsePart {
    pub atom: RangeAtomId,
    pub reaching: RangeVersionId,
    /// First bit in the load destination supplied by this atom.
    pub destination_bit_offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RangeLoad {
    pub block: BlockId,
    pub instruction: usize,
    pub destination: RegisterId,
    pub addr: RegionedAbsoluteAddr,
    pub bit_offset: usize,
    pub width: usize,
    pub parts: Vec<RangeUsePart>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RangeDefPart {
    pub atom: RangeAtomId,
    pub version: RangeVersionId,
    /// First source bit stored in this atom.
    pub source_bit_offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RangeStore {
    pub block: BlockId,
    pub instruction: usize,
    pub source: RegisterId,
    pub addr: RegionedAbsoluteAddr,
    pub bit_offset: usize,
    pub width: usize,
    pub parts: Vec<RangeDefPart>,
}

/// State range versions for one SIR region.  The representation is sparse in
/// both address and CFG space: there is no byte table and no block-by-atom
/// matrix.
#[derive(Debug, Clone)]
pub(super) struct RangeStateSsa {
    pub atoms: Vec<StateRangeAtom>,
    pub versions: Vec<RangeVersion>,
    pub loads: Vec<RangeLoad>,
    pub stores: Vec<RangeStore>,
}

#[derive(Debug, Clone, Copy)]
enum RawAccessKind {
    Load(RegisterId),
    Store(RegisterId),
}

#[derive(Debug, Clone, Copy)]
struct RawAccess {
    block: BlockId,
    block_index: usize,
    instruction: usize,
    addr: RegionedAbsoluteAddr,
    bit_offset: usize,
    width: usize,
    plane: StatePlane,
    kind: RawAccessKind,
}

impl RawAccess {
    fn end(self) -> Option<usize> {
        self.bit_offset.checked_add(self.width)
    }
}

#[derive(Debug, Clone)]
struct MappedAccess {
    raw: RawAccess,
    atoms: Vec<RangeAtomId>,
    store_versions: Vec<RangeVersionId>,
}

fn static_access(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: BlockId,
    block_index: usize,
    instruction: usize,
    addr: RegionedAbsoluteAddr,
    offset: &SIROffset,
    width: usize,
    register: RegisterId,
    kind: RawAccessKind,
) -> Option<RawAccess> {
    let SIROffset::Static(bit_offset) = offset else {
        return None;
    };
    let ty = eu.register_map.get(&register)?;
    if width == 0 || ty.width() != width || bit_offset.checked_add(width).is_none() {
        return None;
    }
    Some(RawAccess {
        block,
        block_index,
        instruction,
        addr,
        bit_offset: *bit_offset,
        width,
        plane: StatePlane::for_type(ty),
        kind,
    })
}

fn discover_accesses(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    region: u32,
    externally_rejected: &HashSet<RegionedAbsoluteAddr>,
) -> Vec<RawAccess> {
    let mut accesses = Vec::new();
    let mut rejected = externally_rejected.clone();
    for (block_index, &block) in cfg.block_ids.iter().enumerate() {
        for (instruction, inst) in eu.blocks[&block].instructions.iter().enumerate() {
            match inst {
                SIRInstruction::Load(destination, addr, offset, width) if addr.region == region => {
                    let Some(access) = static_access(
                        eu,
                        block,
                        block_index,
                        instruction,
                        *addr,
                        offset,
                        *width,
                        *destination,
                        RawAccessKind::Load(*destination),
                    ) else {
                        rejected.insert(*addr);
                        continue;
                    };
                    if access.plane != StatePlane::TwoStateValue {
                        rejected.insert(*addr);
                    }
                    accesses.push(access);
                }
                SIRInstruction::Store(addr, offset, width, source, triggers, capture_sites)
                    if addr.region == region =>
                {
                    let Some(access) = static_access(
                        eu,
                        block,
                        block_index,
                        instruction,
                        *addr,
                        offset,
                        *width,
                        *source,
                        RawAccessKind::Store(*source),
                    ) else {
                        rejected.insert(*addr);
                        continue;
                    };
                    if access.plane != StatePlane::TwoStateValue
                        || !triggers.is_empty()
                        || !capture_sites.is_empty()
                    {
                        rejected.insert(*addr);
                    }
                    accesses.push(access);
                }
                SIRInstruction::Commit(source, destination, _, _, _) => {
                    if source.region == region {
                        rejected.insert(*source);
                    }
                    if destination.region == region {
                        rejected.insert(*destination);
                    }
                }
                _ => {}
            }
        }
    }

    // Mixing value/mask storage and two-state storage under one address is not
    // an atomization boundary. Reject the complete object instead.
    let mut planes = HashMap::<RegionedAbsoluteAddr, StatePlane>::default();
    for access in &accesses {
        if planes
            .insert(access.addr, access.plane)
            .is_some_and(|plane| plane != access.plane)
        {
            rejected.insert(access.addr);
        }
    }
    accesses.retain(|access| !rejected.contains(&access.addr));
    accesses
}

fn partition_atoms(
    accesses: &[RawAccess],
) -> Option<(
    Vec<StateRangeAtom>,
    HashMap<RegionedAbsoluteAddr, Vec<RangeAtomId>>,
)> {
    let mut events = HashMap::<RegionedAbsoluteAddr, BTreeMap<usize, i64>>::default();
    let mut planes = HashMap::<RegionedAbsoluteAddr, StatePlane>::default();
    for access in accesses {
        let end = access.end()?;
        *events
            .entry(access.addr)
            .or_default()
            .entry(access.bit_offset)
            .or_default() += 1;
        *events
            .entry(access.addr)
            .or_default()
            .entry(end)
            .or_default() -= 1;
        if planes
            .insert(access.addr, access.plane)
            .is_some_and(|plane| plane != access.plane)
        {
            return None;
        }
    }

    let mut addresses = events.keys().copied().collect::<Vec<_>>();
    addresses.sort_unstable();
    let mut atoms = Vec::new();
    let mut by_address = HashMap::<RegionedAbsoluteAddr, Vec<RangeAtomId>>::default();
    for addr in addresses {
        let points = &events[&addr];
        let mut active = 0i64;
        let mut previous = None;
        for (&point, &delta) in points {
            if let Some(start) = previous
                && active > 0
                && start < point
            {
                let id = RangeAtomId(atoms.len());
                atoms.push(StateRangeAtom {
                    addr,
                    plane: planes[&addr],
                    bit_offset: start,
                    width: point.checked_sub(start)?,
                });
                by_address.entry(addr).or_default().push(id);
            }
            active = active.checked_add(delta)?;
            if active < 0 {
                return None;
            }
            previous = Some(point);
        }
        if active != 0 {
            return None;
        }
    }
    Some((atoms, by_address))
}

fn covered_atoms(
    access: RawAccess,
    atoms: &[StateRangeAtom],
    by_address: &HashMap<RegionedAbsoluteAddr, Vec<RangeAtomId>>,
) -> Option<Vec<RangeAtomId>> {
    let end = access.end()?;
    let candidates = by_address.get(&access.addr)?;
    let first = candidates.partition_point(|id| atoms[id.0].end() <= Some(access.bit_offset));
    let mut covered = Vec::new();
    let mut cursor = access.bit_offset;
    for &id in &candidates[first..] {
        let atom = atoms[id.0];
        if atom.bit_offset >= end {
            break;
        }
        if atom.bit_offset != cursor || atom.plane != access.plane {
            return None;
        }
        cursor = atom.end()?;
        covered.push(id);
    }
    (cursor == end && !covered.is_empty()).then_some(covered)
}

impl RangeStateSsa {
    /// Build range StateSSA for eligible static two-state objects.
    ///
    /// For A static accesses, P endpoint atoms, and L live `(atom, block)`
    /// pairs, atomization costs O(A log A + P), while sparse liveness, phi
    /// placement, and renaming cost O(A + L + incident CFG/DF edges). Storage
    /// is O(A + P + L). No term is proportional to object bit width or to
    /// `atoms * blocks`.
    pub fn analyze(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        cfg: &SirCfg,
        region: u32,
        externally_rejected: &HashSet<RegionedAbsoluteAddr>,
    ) -> Result<Self, &'static str> {
        let raw = discover_accesses(eu, cfg, region, externally_rejected);
        let (atoms, by_address) = partition_atoms(&raw).ok_or("invalid range partition")?;
        let mut mapped = raw
            .into_iter()
            .map(|raw| {
                Ok(MappedAccess {
                    atoms: covered_atoms(raw, &atoms, &by_address)
                        .ok_or("static access is not exactly covered by range atoms")?,
                    raw,
                    store_versions: Vec::new(),
                })
            })
            .collect::<Result<Vec<_>, &'static str>>()?;
        if atoms.is_empty() {
            return Ok(Self {
                atoms,
                versions: Vec::new(),
                loads: Vec::new(),
                stores: Vec::new(),
            });
        }

        let mut accesses_by_block = vec![Vec::<usize>::new(); cfg.block_ids.len()];
        let mut definitions = HashSet::<(RangeAtomId, usize)>::default();
        let mut upward_uses = HashSet::<(RangeAtomId, usize)>::default();
        for (access_index, access) in mapped.iter().enumerate() {
            accesses_by_block[access.raw.block_index].push(access_index);
        }
        for (block, block_accesses) in accesses_by_block.iter().enumerate() {
            let mut defined = HashSet::<RangeAtomId>::default();
            for &access in block_accesses {
                match mapped[access].raw.kind {
                    RawAccessKind::Load(_) => {
                        for &atom in &mapped[access].atoms {
                            if !defined.contains(&atom) {
                                upward_uses.insert((atom, block));
                            }
                        }
                    }
                    RawAccessKind::Store(_) => {
                        for &atom in &mapped[access].atoms {
                            defined.insert(atom);
                            definitions.insert((atom, block));
                        }
                    }
                }
            }
        }

        // One shared sparse liveness worklist. Each `(atom, block)` is inserted
        // once; there is no full reverse CFG walk per atom.
        let mut live_in = upward_uses.clone();
        let mut live_work = upward_uses.iter().copied().collect::<VecDeque<_>>();
        while let Some((atom, block)) = live_work.pop_front() {
            for &predecessor in &cfg.predecessors[block] {
                let pair = (atom, predecessor);
                if !definitions.contains(&pair) && live_in.insert(pair) {
                    live_work.push_back(pair);
                }
            }
        }

        let mut phi_pairs = HashSet::<(RangeAtomId, usize)>::default();
        let mut queued = definitions.clone();
        let mut phi_work = definitions.iter().copied().collect::<Vec<_>>();
        while let Some((atom, block)) = phi_work.pop() {
            for &frontier in &cfg.dominance_frontier[block] {
                let pair = (atom, frontier);
                if !live_in.contains(&pair) || !phi_pairs.insert(pair) {
                    continue;
                }
                if queued.insert(pair) {
                    phi_work.push(pair);
                }
            }
        }

        let mut versions = Vec::<RangeVersion>::new();
        let mut entry_versions = Vec::with_capacity(atoms.len());
        for atom in 0..atoms.len() {
            let id = RangeVersionId(versions.len());
            versions.push(RangeVersion {
                id,
                atom: RangeAtomId(atom),
                kind: RangeVersionKind::LiveOnEntry,
            });
            entry_versions.push(id);
        }

        let mut phis_by_block =
            vec![Vec::<(RangeAtomId, RangeVersionId)>::new(); cfg.block_ids.len()];
        let mut ordered_phis = phi_pairs.into_iter().collect::<Vec<_>>();
        ordered_phis.sort_unstable_by_key(|(atom, block)| (*block, *atom));
        for (atom, block) in ordered_phis {
            let id = RangeVersionId(versions.len());
            versions.push(RangeVersion {
                id,
                atom,
                kind: RangeVersionKind::Phi {
                    block: cfg.block_ids[block],
                    incoming: Vec::new(),
                },
            });
            phis_by_block[block].push((atom, id));
        }

        for access in &mut mapped {
            let RawAccessKind::Store(source) = access.raw.kind else {
                continue;
            };
            for &atom in &access.atoms {
                let source_bit_offset = atoms[atom.0]
                    .bit_offset
                    .checked_sub(access.raw.bit_offset)
                    .ok_or("atom precedes its defining store")?;
                let id = RangeVersionId(versions.len());
                versions.push(RangeVersion {
                    id,
                    atom,
                    kind: RangeVersionKind::Store {
                        block: access.raw.block,
                        instruction: access.raw.instruction,
                        source,
                        source_bit_offset,
                    },
                });
                access.store_versions.push(id);
            }
        }

        enum Visit {
            Enter(usize),
            Exit(Vec<RangeAtomId>),
        }
        let mut stacks = entry_versions
            .iter()
            .copied()
            .map(|version| vec![version])
            .collect::<Vec<_>>();
        let mut loads = Vec::new();
        let mut stores = Vec::new();
        let mut visits = vec![Visit::Enter(0)];
        while let Some(visit) = visits.pop() {
            match visit {
                Visit::Exit(pushed) => {
                    for atom in pushed.into_iter().rev() {
                        stacks[atom.0].pop();
                    }
                }
                Visit::Enter(block) => {
                    let mut pushed = Vec::new();
                    for &(atom, version) in &phis_by_block[block] {
                        stacks[atom.0].push(version);
                        pushed.push(atom);
                    }
                    for &access_index in &accesses_by_block[block] {
                        let access = &mapped[access_index];
                        match access.raw.kind {
                            RawAccessKind::Load(destination) => {
                                let parts = access
                                    .atoms
                                    .iter()
                                    .map(|&atom| {
                                        Ok(RangeUsePart {
                                            atom,
                                            reaching: *stacks[atom.0]
                                                .last()
                                                .ok_or("range atom has no reaching version")?,
                                            destination_bit_offset: atoms[atom.0]
                                                .bit_offset
                                                .checked_sub(access.raw.bit_offset)
                                                .ok_or("atom precedes its load")?,
                                        })
                                    })
                                    .collect::<Result<Vec<_>, &'static str>>()?;
                                loads.push(RangeLoad {
                                    block: access.raw.block,
                                    instruction: access.raw.instruction,
                                    destination,
                                    addr: access.raw.addr,
                                    bit_offset: access.raw.bit_offset,
                                    width: access.raw.width,
                                    parts,
                                });
                            }
                            RawAccessKind::Store(source) => {
                                let mut parts = Vec::with_capacity(access.atoms.len());
                                for (&atom, &version) in
                                    access.atoms.iter().zip(&access.store_versions)
                                {
                                    let source_bit_offset = atoms[atom.0]
                                        .bit_offset
                                        .checked_sub(access.raw.bit_offset)
                                        .ok_or("atom precedes its defining store")?;
                                    parts.push(RangeDefPart {
                                        atom,
                                        version,
                                        source_bit_offset,
                                    });
                                    stacks[atom.0].push(version);
                                    pushed.push(atom);
                                }
                                stores.push(RangeStore {
                                    block: access.raw.block,
                                    instruction: access.raw.instruction,
                                    source,
                                    addr: access.raw.addr,
                                    bit_offset: access.raw.bit_offset,
                                    width: access.raw.width,
                                    parts,
                                });
                            }
                        }
                    }
                    for &successor in &cfg.successors[block] {
                        for &(atom, phi) in &phis_by_block[successor] {
                            let reaching = *stacks[atom.0]
                                .last()
                                .ok_or("range phi has no predecessor version")?;
                            let RangeVersionKind::Phi { incoming, .. } = &mut versions[phi.0].kind
                            else {
                                return Err("range phi table references a non-phi version");
                            };
                            incoming.push((cfg.block_ids[block], reaching));
                        }
                    }
                    visits.push(Visit::Exit(pushed));
                    for &child in cfg.dom_children[block].iter().rev() {
                        visits.push(Visit::Enter(child));
                    }
                }
            }
        }
        for version in &mut versions {
            if let RangeVersionKind::Phi { incoming, .. } = &mut version.kind {
                incoming.sort_unstable_by_key(|(block, _)| *block);
            }
        }

        let result = Self {
            atoms,
            versions,
            loads,
            stores,
        };
        result.verify(cfg)?;
        Ok(result)
    }

    pub fn verify(&self, cfg: &SirCfg) -> Result<(), &'static str> {
        for (index, atom) in self.atoms.iter().enumerate() {
            if atom.width == 0 || atom.end().is_none() || atom.plane != StatePlane::TwoStateValue {
                return Err("range atom has invalid width or plane");
            }
            if index > 0 {
                let previous = self.atoms[index - 1];
                if previous.addr > atom.addr
                    || (previous.addr == atom.addr && previous.end() > Some(atom.bit_offset))
                {
                    return Err("range atoms are not ordered and disjoint");
                }
            }
        }
        for (index, version) in self.versions.iter().enumerate() {
            if version.id != RangeVersionId(index) || version.atom.0 >= self.atoms.len() {
                return Err("range version identity is invalid");
            }
        }

        let mut entry = vec![None::<RangeVersionId>; self.atoms.len()];
        let mut phis_by_block =
            vec![Vec::<(RangeAtomId, RangeVersionId)>::new(); cfg.block_ids.len()];
        let mut stores_by_block = vec![Vec::<&RangeStore>::new(); cfg.block_ids.len()];
        let mut loads_by_block = vec![Vec::<&RangeLoad>::new(); cfg.block_ids.len()];
        for version in &self.versions {
            match &version.kind {
                RangeVersionKind::LiveOnEntry => {
                    if entry[version.atom.0].replace(version.id).is_some() {
                        return Err("range atom has duplicate live-on-entry versions");
                    }
                }
                RangeVersionKind::Phi { block, incoming } => {
                    let Some(block_index) = cfg.block_index(*block) else {
                        return Err("range phi block is outside the CFG");
                    };
                    if phis_by_block[block_index]
                        .iter()
                        .any(|(atom, _)| *atom == version.atom)
                    {
                        return Err("range block has duplicate phis for one atom");
                    }
                    let expected = cfg.predecessors[block_index]
                        .iter()
                        .map(|predecessor| cfg.block_ids[*predecessor])
                        .collect::<BTreeSet<_>>();
                    let actual = incoming
                        .iter()
                        .map(|(predecessor, _)| *predecessor)
                        .collect::<BTreeSet<_>>();
                    if expected != actual || incoming.len() != expected.len() {
                        return Err("range phi does not cover every predecessor exactly once");
                    }
                    phis_by_block[block_index].push((version.atom, version.id));
                }
                RangeVersionKind::Store { .. } => {}
            }
        }
        if entry.iter().any(Option::is_none) {
            return Err("range atom has no live-on-entry version");
        }
        for entries in &mut phis_by_block {
            entries.sort_unstable_by_key(|(atom, _)| *atom);
        }

        for store in &self.stores {
            let Some(block) = cfg.block_index(store.block) else {
                return Err("range store block is outside the CFG");
            };
            verify_store_parts(store, &self.atoms, &self.versions)?;
            stores_by_block[block].push(store);
        }
        for load in &self.loads {
            let Some(block) = cfg.block_index(load.block) else {
                return Err("range load block is outside the CFG");
            };
            verify_load_parts(load, &self.atoms, &self.versions)?;
            loads_by_block[block].push(load);
        }
        for stores in &mut stores_by_block {
            stores.sort_unstable_by_key(|store| store.instruction);
        }
        for loads in &mut loads_by_block {
            loads.sort_unstable_by_key(|load| load.instruction);
        }

        // Rebuild the reaching versions independently from the public
        // versions and access rows.
        enum VerifyVisit {
            Enter(usize),
            Exit(Vec<RangeAtomId>),
        }
        let mut stacks = entry
            .into_iter()
            .map(|version| vec![version.expect("checked above")])
            .collect::<Vec<_>>();
        let mut visits = vec![VerifyVisit::Enter(0)];
        while let Some(visit) = visits.pop() {
            match visit {
                VerifyVisit::Exit(pushed) => {
                    for atom in pushed.into_iter().rev() {
                        stacks[atom.0].pop();
                    }
                }
                VerifyVisit::Enter(block) => {
                    let mut pushed = Vec::new();
                    for &(atom, version) in &phis_by_block[block] {
                        stacks[atom.0].push(version);
                        pushed.push(atom);
                    }
                    let mut store = 0usize;
                    let mut load = 0usize;
                    loop {
                        let next_store = stores_by_block[block]
                            .get(store)
                            .map(|store| store.instruction);
                        let next_load =
                            loads_by_block[block].get(load).map(|load| load.instruction);
                        if next_store.is_none() && next_load.is_none() {
                            break;
                        }
                        if next_store.is_some() && next_store == next_load {
                            return Err("one SIR instruction is both a range load and store");
                        }
                        if next_store.is_some()
                            && next_load.is_none_or(|load_instruction| {
                                next_store.is_some_and(|store_instruction| {
                                    store_instruction < load_instruction
                                })
                            })
                        {
                            for part in &stores_by_block[block][store].parts {
                                stacks[part.atom.0].push(part.version);
                                pushed.push(part.atom);
                            }
                            store += 1;
                        } else {
                            for part in &loads_by_block[block][load].parts {
                                if stacks[part.atom.0].last().copied() != Some(part.reaching) {
                                    return Err(
                                        "range load does not name the latest reaching version",
                                    );
                                }
                            }
                            load += 1;
                        }
                    }
                    for &successor in &cfg.successors[block] {
                        for &(atom, phi) in &phis_by_block[successor] {
                            let RangeVersionKind::Phi { incoming, .. } = &self.versions[phi.0].kind
                            else {
                                return Err("range phi index names a non-phi version");
                            };
                            let expected = stacks[atom.0]
                                .last()
                                .copied()
                                .ok_or("range verifier has no edge version")?;
                            if incoming
                                .iter()
                                .find(|(predecessor, _)| *predecessor == cfg.block_ids[block])
                                .map(|(_, version)| *version)
                                != Some(expected)
                            {
                                return Err(
                                    "range phi incoming is not the predecessor exit version",
                                );
                            }
                        }
                    }
                    visits.push(VerifyVisit::Exit(pushed));
                    for &child in cfg.dom_children[block].iter().rev() {
                        visits.push(VerifyVisit::Enter(child));
                    }
                }
            }
        }
        Ok(())
    }
}

fn verify_load_parts(
    load: &RangeLoad,
    atoms: &[StateRangeAtom],
    versions: &[RangeVersion],
) -> Result<(), &'static str> {
    let end = load
        .bit_offset
        .checked_add(load.width)
        .ok_or("range load overflows")?;
    let mut cursor = load.bit_offset;
    for part in &load.parts {
        let atom = *atoms.get(part.atom.0).ok_or("range load atom is absent")?;
        let version = versions
            .get(part.reaching.0)
            .ok_or("range load version is absent")?;
        if atom.addr != load.addr
            || atom.bit_offset != cursor
            || version.atom != part.atom
            || part.destination_bit_offset != atom.bit_offset - load.bit_offset
        {
            return Err("range load part has inconsistent coverage or version");
        }
        cursor = atom.end().ok_or("range load atom overflows")?;
    }
    if load.parts.is_empty() || cursor != end {
        return Err("range load parts do not exactly cover the access");
    }
    Ok(())
}

fn verify_store_parts(
    store: &RangeStore,
    atoms: &[StateRangeAtom],
    versions: &[RangeVersion],
) -> Result<(), &'static str> {
    let end = store
        .bit_offset
        .checked_add(store.width)
        .ok_or("range store overflows")?;
    let mut cursor = store.bit_offset;
    for part in &store.parts {
        let atom = *atoms.get(part.atom.0).ok_or("range store atom is absent")?;
        let version = versions
            .get(part.version.0)
            .ok_or("range store version is absent")?;
        let RangeVersionKind::Store {
            block,
            instruction,
            source,
            source_bit_offset,
        } = version.kind
        else {
            return Err("range store part names a non-store version");
        };
        if atom.addr != store.addr
            || atom.bit_offset != cursor
            || version.atom != part.atom
            || block != store.block
            || instruction != store.instruction
            || source != store.source
            || source_bit_offset != part.source_bit_offset
            || part.source_bit_offset != atom.bit_offset - store.bit_offset
        {
            return Err("range store part has inconsistent coverage or definition");
        }
        cursor = atom.end().ok_or("range store atom overflows")?;
    }
    if store.parts.is_empty() || cursor != end {
        return Err("range store parts do not exactly cover the access");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use veryl_analyzer::ir::VarId;

    fn bit(width: usize) -> RegisterType {
        RegisterType::Bit {
            width,
            signed: false,
        }
    }

    fn logic(width: usize) -> RegisterType {
        RegisterType::Logic { width }
    }

    fn address(variable: u32) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: STABLE_REGION,
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

    fn analyze(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> RangeStateSsa {
        let cfg = SirCfg::analyze(eu).unwrap();
        RangeStateSsa::analyze(eu, &cfg, STABLE_REGION, &HashSet::default()).unwrap()
    }

    #[test]
    fn mixed_width_accesses_share_endpoint_atoms() {
        let addr = address(0);
        let eu = unit(
            vec![block(
                0,
                vec![
                    SIRInstruction::Store(
                        addr,
                        SIROffset::Static(0),
                        64,
                        RegisterId(0),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 32),
                    SIRInstruction::Load(RegisterId(2), addr, SIROffset::Static(32), 32),
                ],
                SIRTerminator::Return,
            )],
            [
                (RegisterId(0), bit(64)),
                (RegisterId(1), bit(32)),
                (RegisterId(2), bit(32)),
            ],
        );

        let state = analyze(&eu);

        assert_eq!(
            state
                .atoms
                .iter()
                .map(|atom| (atom.bit_offset, atom.width))
                .collect::<Vec<_>>(),
            vec![(0, 32), (32, 32)]
        );
        assert_eq!(state.stores[0].parts[0].source_bit_offset, 0);
        assert_eq!(state.stores[0].parts[1].source_bit_offset, 32);
        assert_eq!(
            state.loads[0].parts[0].reaching,
            state.stores[0].parts[0].version
        );
        assert_eq!(
            state.loads[1].parts[0].reaching,
            state.stores[0].parts[1].version
        );
    }

    #[test]
    fn diamond_places_independent_phis_for_partially_defined_atoms() {
        let addr = address(0);
        let eu = unit(
            vec![
                block(
                    0,
                    Vec::new(),
                    SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), Vec::new()),
                        false_block: (BlockId(2), Vec::new()),
                    },
                ),
                block(
                    1,
                    vec![SIRInstruction::Store(
                        addr,
                        SIROffset::Static(0),
                        32,
                        RegisterId(1),
                        Vec::new(),
                        Vec::new(),
                    )],
                    SIRTerminator::Jump(BlockId(3), Vec::new()),
                ),
                block(
                    2,
                    vec![SIRInstruction::Store(
                        addr,
                        SIROffset::Static(32),
                        32,
                        RegisterId(2),
                        Vec::new(),
                        Vec::new(),
                    )],
                    SIRTerminator::Jump(BlockId(3), Vec::new()),
                ),
                block(
                    3,
                    vec![SIRInstruction::Load(
                        RegisterId(3),
                        addr,
                        SIROffset::Static(0),
                        64,
                    )],
                    SIRTerminator::Return,
                ),
            ],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(32)),
                (RegisterId(2), bit(32)),
                (RegisterId(3), bit(64)),
            ],
        );

        let state = analyze(&eu);
        let phis = state
            .versions
            .iter()
            .filter(|version| {
                matches!(
                    version.kind,
                    RangeVersionKind::Phi {
                        block: BlockId(3),
                        ..
                    }
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(phis.len(), 2);
        assert_eq!(state.loads[0].parts.len(), 2);
        assert_eq!(state.loads[0].parts[0].reaching, phis[0].id);
        assert_eq!(state.loads[0].parts[1].reaching, phis[1].id);
    }

    #[test]
    fn loop_phi_is_sparse_to_the_atom_written_on_the_backedge() {
        let addr = address(0);
        let eu = unit(
            vec![
                block(0, Vec::new(), SIRTerminator::Jump(BlockId(1), Vec::new())),
                block(
                    1,
                    vec![SIRInstruction::Load(
                        RegisterId(1),
                        addr,
                        SIROffset::Static(0),
                        64,
                    )],
                    SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(2), Vec::new()),
                        false_block: (BlockId(3), Vec::new()),
                    },
                ),
                block(
                    2,
                    vec![SIRInstruction::Store(
                        addr,
                        SIROffset::Static(0),
                        32,
                        RegisterId(2),
                        Vec::new(),
                        Vec::new(),
                    )],
                    SIRTerminator::Jump(BlockId(1), Vec::new()),
                ),
                block(3, Vec::new(), SIRTerminator::Return),
            ],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(64)),
                (RegisterId(2), bit(32)),
            ],
        );

        let state = analyze(&eu);
        let header_phis = state
            .versions
            .iter()
            .filter(|version| {
                matches!(
                    version.kind,
                    RangeVersionKind::Phi {
                        block: BlockId(1),
                        ..
                    }
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(header_phis.len(), 1);
        assert_eq!(state.atoms[header_phis[0].atom.0].bit_offset, 0);
        assert_eq!(state.loads[0].parts[0].reaching, header_phis[0].id);
        assert!(matches!(
            state.versions[state.loads[0].parts[1].reaching.0].kind,
            RangeVersionKind::LiveOnEntry
        ));
    }

    #[test]
    fn dynamic_access_rejects_only_its_object() {
        let rejected_addr = address(0);
        let retained_addr = address(1);
        let eu = unit(
            vec![block(
                0,
                vec![
                    SIRInstruction::Load(
                        RegisterId(1),
                        rejected_addr,
                        SIROffset::Dynamic(RegisterId(0)),
                        8,
                    ),
                    SIRInstruction::Load(RegisterId(2), retained_addr, SIROffset::Static(0), 8),
                ],
                SIRTerminator::Return,
            )],
            [
                (RegisterId(0), bit(64)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
            ],
        );

        let state = analyze(&eu);

        assert_eq!(state.atoms.len(), 1);
        assert_eq!(state.atoms[0].addr, retained_addr);
        assert_eq!(state.loads.len(), 1);
        assert_eq!(state.loads[0].destination, RegisterId(2));
    }

    #[test]
    fn eventful_store_and_four_state_access_reject_complete_objects() {
        let eventful = address(0);
        let four_state = address(1);
        let eu = unit(
            vec![block(
                0,
                vec![
                    SIRInstruction::Store(
                        eventful,
                        SIROffset::Static(0),
                        8,
                        RegisterId(0),
                        vec![TriggerIdWithKind {
                            kind: DomainKind::Other,
                            id: 0,
                        }],
                        Vec::new(),
                    ),
                    SIRInstruction::Load(RegisterId(1), eventful, SIROffset::Static(0), 8),
                    SIRInstruction::Load(RegisterId(2), four_state, SIROffset::Static(0), 8),
                ],
                SIRTerminator::Return,
            )],
            [
                (RegisterId(0), bit(8)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), logic(8)),
            ],
        );

        let state = analyze(&eu);

        assert!(state.atoms.is_empty());
        assert!(state.loads.is_empty());
        assert!(state.stores.is_empty());
    }

    #[test]
    fn atomization_scales_with_endpoints_not_object_width() {
        const ACCESSES: usize = 4096;
        let addr = address(0);
        let instructions = (0..ACCESSES)
            .map(|index| {
                SIRInstruction::Load(
                    RegisterId(index),
                    addr,
                    SIROffset::Static(index * 1_000_000),
                    1,
                )
            })
            .collect::<Vec<_>>();
        let registers = (0..ACCESSES).map(|index| (RegisterId(index), bit(1)));
        let eu = unit(
            vec![block(0, instructions, SIRTerminator::Return)],
            registers,
        );

        let state = analyze(&eu);

        assert_eq!(state.atoms.len(), ACCESSES);
        assert_eq!(state.loads.len(), ACCESSES);
        assert_eq!(state.versions.len(), ACCESSES);
    }
}
