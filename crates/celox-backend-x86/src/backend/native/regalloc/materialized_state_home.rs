//! Sparse physical MemorySSA verification for allocation-owned state homes.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::native::memory_effect::{self, UnknownMemory};
use crate::native::mir::{BaseReg, BlockId, MFunction, MInst, PackedStateHome, StateHomeId, VReg};
use crate::{HashMap, HashSet};

use super::cfg::NormalizedCfg;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct SyntheticInstructionId(u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyntheticOperation {
    StateStore { home: PackedStateHome },
    StateReload { home: PackedStateHome },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StateHomeInstructionOrigin {
    Original,
    Synthetic {
        id: SyntheticInstructionId,
        operation: SyntheticOperation,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StateHomeInstruction {
    origin: StateHomeInstructionOrigin,
    original: Option<MInst>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StateHomeBlock {
    id: BlockId,
    instructions: Vec<StateHomeInstruction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StateHomeProgram {
    blocks: Vec<StateHomeBlock>,
}

/// One packed-state store emitted by SSA reconstruction, identified by its
/// final per-block SimState-write ordinal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MaterializedStateStore {
    pub block: BlockId,
    pub write_ordinal: usize,
    pub home: PackedStateHome,
}

/// One packed-state reload emitted by SSA reconstruction. Its SSA destination
/// remains stable when dead instructions are compacted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MaterializedStateReload {
    pub reload: VReg,
    pub home: PackedStateHome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MaterializedStateHomeError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub values: Vec<VReg>,
    pub message: String,
}

impl MaterializedStateHomeError {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct ByteId(usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct MemoryVersionId(usize);

#[derive(Debug, Clone, PartialEq, Eq)]
enum MemoryVersionKind {
    Entry {
        owner: Option<StateHomeId>,
    },
    Write {
        owner: Option<StateHomeId>,
    },
    Phi {
        block: usize,
        incoming: Vec<(usize, MemoryVersionId)>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryVersion {
    id: MemoryVersionId,
    byte: ByteId,
    kind: MemoryVersionKind,
}

#[derive(Debug, Clone)]
struct WriteEvent {
    position: usize,
    bytes: Vec<ByteId>,
    owner: Option<StateHomeId>,
    versions: Vec<MemoryVersionId>,
}

#[derive(Debug, Clone)]
struct ReloadQuery {
    block: BlockId,
    position: usize,
    instruction: SyntheticInstructionId,
    home: PackedStateHome,
    bytes: Vec<ByteId>,
    reaching: Vec<MemoryVersionId>,
}

fn error(
    rule: &'static str,
    block: Option<BlockId>,
    instruction: Option<usize>,
    message: impl Into<String>,
) -> MaterializedStateHomeError {
    MaterializedStateHomeError::new(rule, block, instruction, Vec::new(), message)
}

fn register_home(
    homes: &mut HashMap<StateHomeId, PackedStateHome>,
    home: PackedStateHome,
    block: BlockId,
) -> Result<(), MaterializedStateHomeError> {
    if home.byte_range().is_none() {
        return Err(error(
            "STATE_HOME.HOME_RANGE",
            Some(block),
            None,
            "packed-state home byte range overflows i64",
        ));
    }
    if let Some(previous) = homes.insert(home.id, home)
        && previous != home
    {
        return Err(error(
            "STATE_HOME.HOME_IDENTITY",
            Some(block),
            None,
            format!(
                "state home {:?} has conflicting physical metadata {previous:?} and {home:?}",
                home.id
            ),
        ));
    }
    Ok(())
}

fn validate_cfg(
    program: &StateHomeProgram,
    cfg: &NormalizedCfg,
) -> Result<(), MaterializedStateHomeError> {
    let count = program.blocks.len();
    if cfg.predecessors.len() != count
        || cfg.successors.len() != count
        || cfg.idom.len() != count
        || cfg.dominance_frontier.len() != count
        || cfg.block_index.len() != count
        || (0..count).any(|block| cfg.block_index.get(&program.blocks[block].id) != Some(&block))
        || cfg
            .predecessors
            .iter()
            .chain(&cfg.successors)
            .flatten()
            .any(|&block| block >= count)
        || cfg
            .dominance_frontier
            .iter()
            .flatten()
            .any(|&block| block >= count)
    {
        return Err(error(
            "STATE_HOME.HOME_MODEL",
            program.blocks.first().map(|block| block.id),
            None,
            "normalized CFG does not exactly cover packed-state operations",
        ));
    }
    Ok(())
}

fn affected_bytes(
    instruction: &crate::native::mir::MInst,
    byte_index: &BTreeMap<i64, ByteId>,
    block: BlockId,
    position: usize,
) -> Result<Vec<ByteId>, MaterializedStateHomeError> {
    let writes = memory_effect::writes(instruction);
    if writes.unknown_memory() == Some(UnknownMemory::Direct(BaseReg::SimState)) {
        // Range-StateSSA eligibility rejects this object before allocation.
        // Rejecting the candidate here avoids expanding one unknown write into
        // every tracked state byte.
        return Err(error(
            "STATE_HOME.HOME_UNKNOWN_ALIAS",
            Some(block),
            Some(position),
            "unknown direct SimState write prevents a proved packed-state home",
        ));
    }
    let mut bytes = BTreeSet::new();
    for range in writes
        .ranges()
        .filter(|range| range.base == BaseReg::SimState)
    {
        let end = range.end().ok_or_else(|| {
            error(
                "STATE_HOME.HOME_RANGE",
                Some(block),
                Some(position),
                "original SimState write range overflows i64",
            )
        })?;
        bytes.extend(byte_index.range(range.offset..end).map(|(_, &byte)| byte));
    }
    Ok(bytes.into_iter().collect())
}

/// Verify the allocator-owned packed-state stores and reloads against final
/// MIR. The temporary view records only ownership tags; it does not recreate
/// the retired allocation-editing IR.
pub(super) fn verify_materialized_state_homes(
    func: &MFunction,
    cfg: &NormalizedCfg,
    stores: &[MaterializedStateStore],
    reloads: &[MaterializedStateReload],
) -> Result<(), MaterializedStateHomeError> {
    if stores.is_empty() && reloads.is_empty() {
        return Ok(());
    }

    let mut store_homes = BTreeMap::<(BlockId, usize), PackedStateHome>::new();
    for store in stores {
        if store_homes
            .insert((store.block, store.write_ordinal), store.home)
            .is_some()
        {
            return Err(MaterializedStateHomeError::new(
                "STATE_HOME.MATERIALIZED_STORE_UNIQUE",
                Some(store.block),
                None,
                Vec::new(),
                format!(
                    "more than one allocator-owned state store has write ordinal {}",
                    store.write_ordinal
                ),
            ));
        }
    }
    let mut reload_homes = BTreeMap::<VReg, PackedStateHome>::new();
    for reload in reloads {
        if reload_homes.insert(reload.reload, reload.home).is_some() {
            return Err(MaterializedStateHomeError::new(
                "STATE_HOME.MATERIALIZED_RELOAD_UNIQUE",
                None,
                None,
                vec![reload.reload],
                "more than one allocator-owned state reload has this destination",
            ));
        }
    }

    let mut found_stores = BTreeSet::<(BlockId, usize)>::new();
    let mut found_reloads = BTreeSet::<VReg>::new();
    let mut next_id = 0u32;
    let mut blocks = Vec::with_capacity(func.blocks.len());
    for block in &func.blocks {
        let mut instructions = Vec::with_capacity(block.insts.len());
        let mut write_ordinal = 0usize;
        for (instruction, inst) in block.insts.iter().enumerate() {
            let writes = memory_effect::writes(inst);
            let affects_state = writes.unknown_memory()
                == Some(UnknownMemory::Direct(BaseReg::SimState))
                || writes.ranges().any(|range| range.base == BaseReg::SimState);
            let mut operation = None;
            if affects_state {
                let identity = (block.id, write_ordinal);
                if let Some(&home) = store_homes.get(&identity) {
                    if !matches!(
                        inst,
                        MInst::Store {
                            base: BaseReg::SimState,
                            offset,
                            size,
                            ..
                        } if *offset == home.offset && *size == home.size
                    ) {
                        return Err(MaterializedStateHomeError::new(
                            "STATE_HOME.MATERIALIZED_STORE_SHAPE",
                            Some(block.id),
                            Some(instruction),
                            Vec::new(),
                            format!("recorded state store does not materialize {home:?}"),
                        ));
                    }
                    found_stores.insert(identity);
                    operation = Some(SyntheticOperation::StateStore { home });
                }
                write_ordinal = write_ordinal.checked_add(1).ok_or_else(|| {
                    MaterializedStateHomeError::new(
                        "STATE_HOME.MATERIALIZED_STORE_RANGE",
                        Some(block.id),
                        Some(instruction),
                        Vec::new(),
                        "per-block SimState write ordinal exceeds usize",
                    )
                })?;
            }
            if let Some(definition) = inst.def()
                && let Some(&home) = reload_homes.get(&definition)
            {
                if operation.is_some() || !found_reloads.insert(definition) {
                    return Err(MaterializedStateHomeError::new(
                        "STATE_HOME.MATERIALIZED_RELOAD_UNIQUE",
                        Some(block.id),
                        Some(instruction),
                        vec![definition],
                        "materialized state reload does not have one unique MIR definition",
                    ));
                }
                if !matches!(
                    inst,
                    MInst::Load {
                        dst,
                        base: BaseReg::SimState,
                        offset,
                        size,
                    } if *dst == definition && *offset == home.offset && *size == home.size
                ) {
                    return Err(MaterializedStateHomeError::new(
                        "STATE_HOME.MATERIALIZED_RELOAD_SHAPE",
                        Some(block.id),
                        Some(instruction),
                        vec![definition],
                        format!("recorded state reload does not materialize {home:?}"),
                    ));
                }
                operation = Some(SyntheticOperation::StateReload { home });
            }

            let (origin, original) = if let Some(operation) = operation {
                let id = SyntheticInstructionId(next_id);
                next_id = next_id.checked_add(1).ok_or_else(|| {
                    MaterializedStateHomeError::new(
                        "STATE_HOME.MATERIALIZED_ID_RANGE",
                        Some(block.id),
                        Some(instruction),
                        Vec::new(),
                        "materialized state-operation identity exceeds u32",
                    )
                })?;
                (
                    StateHomeInstructionOrigin::Synthetic { id, operation },
                    None,
                )
            } else {
                (StateHomeInstructionOrigin::Original, Some(inst.clone()))
            };
            instructions.push(StateHomeInstruction { origin, original });
        }
        blocks.push(StateHomeBlock {
            id: block.id,
            instructions,
        });
    }

    if found_stores.len() != store_homes.len() {
        let (&(block, ordinal), _) = store_homes
            .iter()
            .find(|(identity, _)| !found_stores.contains(identity))
            .expect("different set lengths imply a missing store");
        return Err(MaterializedStateHomeError::new(
            "STATE_HOME.MATERIALIZED_STORE_IDENTITY",
            Some(block),
            None,
            Vec::new(),
            format!("final MIR has no SimState write ordinal {ordinal}"),
        ));
    }
    if found_reloads.len() != reload_homes.len() {
        let (&reload, _) = reload_homes
            .iter()
            .find(|(reload, _)| !found_reloads.contains(reload))
            .expect("different set lengths imply a missing reload");
        return Err(MaterializedStateHomeError::new(
            "STATE_HOME.MATERIALIZED_RELOAD_IDENTITY",
            None,
            None,
            vec![reload],
            "materialized state reload destination has no final MIR definition",
        ));
    }

    verify_program(&StateHomeProgram { blocks }, cfg)
}

fn verify_program(
    program: &StateHomeProgram,
    cfg: &NormalizedCfg,
) -> Result<(), MaterializedStateHomeError> {
    validate_cfg(program, cfg)?;

    let mut homes = HashMap::<StateHomeId, PackedStateHome>::default();
    let mut required = BTreeSet::<StateHomeId>::new();
    for block in &program.blocks {
        for instruction in &block.instructions {
            let StateHomeInstructionOrigin::Synthetic { operation, .. } = instruction.origin else {
                continue;
            };
            match operation {
                SyntheticOperation::StateStore { home } => {
                    register_home(&mut homes, home, block.id)?;
                }
                SyntheticOperation::StateReload { home } => {
                    register_home(&mut homes, home, block.id)?;
                    required.insert(home.id);
                }
            }
        }
    }
    if required.is_empty() {
        return Ok(());
    }

    let mut tracked = BTreeSet::<i64>::new();
    let mut entry_owner = HashMap::<i64, StateHomeId>::default();
    for &home_id in &required {
        let home = homes[&home_id];
        let bytes = home
            .byte_range()
            .expect("registered packed-state home has a finite range");
        tracked.extend(bytes.clone());
        if home.live_on_entry {
            for byte in bytes {
                if let Some(previous) = entry_owner.insert(byte, home.id)
                    && previous != home.id
                {
                    return Err(error(
                        "STATE_HOME.HOME_ENTRY_ALIAS",
                        None,
                        None,
                        format!(
                            "entry-valid state homes {previous:?} and {:?} overlap byte {byte}",
                            home.id
                        ),
                    ));
                }
            }
        }
    }
    let byte_index = tracked
        .into_iter()
        .enumerate()
        .map(|(index, byte)| (byte, ByteId(index)))
        .collect::<BTreeMap<_, _>>();
    let byte_count = byte_index.len();
    let mut events = vec![Vec::<WriteEvent>::new(); program.blocks.len()];
    let mut queries = vec![Vec::<ReloadQuery>::new(); program.blocks.len()];

    for (block, row) in program.blocks.iter().enumerate() {
        for (position, instruction) in row.instructions.iter().enumerate() {
            match instruction.origin {
                StateHomeInstructionOrigin::Original => {
                    let original = instruction.original.as_ref().ok_or_else(|| {
                        error(
                            "STATE_HOME.HOME_ORIGINAL",
                            Some(row.id),
                            Some(position),
                            "original state-home instruction has no MIR snapshot",
                        )
                    })?;
                    let bytes = affected_bytes(original, &byte_index, row.id, position)?;
                    if !bytes.is_empty() {
                        events[block].push(WriteEvent {
                            position,
                            bytes,
                            owner: None,
                            versions: Vec::new(),
                        });
                    }
                }
                StateHomeInstructionOrigin::Synthetic {
                    operation: SyntheticOperation::StateStore { home },
                    ..
                } => {
                    let range = home
                        .byte_range()
                        .expect("registered packed-state home has a finite range");
                    let bytes = byte_index
                        .range(range.start..range.end)
                        .map(|(_, &byte)| byte)
                        .collect::<Vec<_>>();
                    if !bytes.is_empty() {
                        events[block].push(WriteEvent {
                            position,
                            bytes,
                            owner: Some(home.id),
                            versions: Vec::new(),
                        });
                    }
                }
                StateHomeInstructionOrigin::Synthetic {
                    id,
                    operation: SyntheticOperation::StateReload { home },
                    ..
                } => {
                    let range = home
                        .byte_range()
                        .expect("registered packed-state home has a finite range");
                    let bytes = range
                        .map(|byte| byte_index.get(&byte).copied())
                        .collect::<Option<Vec<_>>>()
                        .ok_or_else(|| {
                            error(
                                "STATE_HOME.HOME_COVERAGE",
                                Some(row.id),
                                Some(position),
                                "state reload is not fully represented by tracked bytes",
                            )
                        })?;
                    queries[block].push(ReloadQuery {
                        block: row.id,
                        position,
                        instruction: id,
                        home,
                        bytes,
                        reaching: Vec::new(),
                    });
                }
            }
        }
    }

    let mut definitions = HashSet::<(ByteId, usize)>::default();
    let mut upward_uses = HashSet::<(ByteId, usize)>::default();
    for block in 0..program.blocks.len() {
        let mut defined = HashSet::<ByteId>::default();
        let mut event = 0usize;
        let mut query = 0usize;
        for position in 0..program.blocks[block].instructions.len() {
            while queries[block]
                .get(query)
                .is_some_and(|candidate| candidate.position == position)
            {
                for &byte in &queries[block][query].bytes {
                    if !defined.contains(&byte) {
                        upward_uses.insert((byte, block));
                    }
                }
                query += 1;
            }
            while events[block]
                .get(event)
                .is_some_and(|candidate| candidate.position == position)
            {
                for &byte in &events[block][event].bytes {
                    defined.insert(byte);
                    definitions.insert((byte, block));
                }
                event += 1;
            }
        }
    }

    let mut live_in = upward_uses.clone();
    let mut live_work = upward_uses.iter().copied().collect::<VecDeque<_>>();
    while let Some((byte, block)) = live_work.pop_front() {
        for &predecessor in &cfg.predecessors[block] {
            let pair = (byte, predecessor);
            if !definitions.contains(&pair) && live_in.insert(pair) {
                live_work.push_back(pair);
            }
        }
    }

    let mut phi_pairs = HashSet::<(ByteId, usize)>::default();
    let mut queued = definitions.clone();
    for byte in 0..byte_count {
        queued.insert((ByteId(byte), 0));
    }
    let mut phi_work = queued.iter().copied().collect::<Vec<_>>();
    while let Some((byte, block)) = phi_work.pop() {
        for &frontier in &cfg.dominance_frontier[block] {
            let pair = (byte, frontier);
            if !live_in.contains(&pair) || !phi_pairs.insert(pair) {
                continue;
            }
            if queued.insert(pair) {
                phi_work.push(pair);
            }
        }
    }

    let bytes_by_id = byte_index.keys().copied().collect::<Vec<_>>();
    let mut versions = Vec::<MemoryVersion>::new();
    let mut entry_versions = Vec::with_capacity(byte_count);
    for (byte, physical) in bytes_by_id.iter().copied().enumerate() {
        let id = MemoryVersionId(versions.len());
        versions.push(MemoryVersion {
            id,
            byte: ByteId(byte),
            kind: MemoryVersionKind::Entry {
                owner: entry_owner.get(&physical).copied(),
            },
        });
        entry_versions.push(id);
    }

    let mut phis_by_block = vec![Vec::<(ByteId, MemoryVersionId)>::new(); program.blocks.len()];
    let mut ordered_phis = phi_pairs.into_iter().collect::<Vec<_>>();
    ordered_phis.sort_unstable_by_key(|(byte, block)| (*block, *byte));
    for (byte, block) in ordered_phis {
        let id = MemoryVersionId(versions.len());
        versions.push(MemoryVersion {
            id,
            byte,
            kind: MemoryVersionKind::Phi {
                block,
                incoming: Vec::new(),
            },
        });
        phis_by_block[block].push((byte, id));
    }
    for block_events in &mut events {
        for event in block_events {
            for &byte in &event.bytes {
                let id = MemoryVersionId(versions.len());
                versions.push(MemoryVersion {
                    id,
                    byte,
                    kind: MemoryVersionKind::Write { owner: event.owner },
                });
                event.versions.push(id);
            }
        }
    }

    let mut children = vec![Vec::<usize>::new(); program.blocks.len()];
    for block in 1..program.blocks.len() {
        let parent = cfg.idom[block].ok_or_else(|| {
            error(
                "STATE_HOME.HOME_DOMINANCE",
                Some(program.blocks[block].id),
                None,
                "reachable non-entry block has no immediate dominator",
            )
        })?;
        if parent >= program.blocks.len() {
            return Err(error(
                "STATE_HOME.HOME_DOMINANCE",
                Some(program.blocks[block].id),
                None,
                "state-home immediate dominator is outside the CFG",
            ));
        }
        children[parent].push(block);
    }

    enum Visit {
        Enter(usize),
        Exit(Vec<ByteId>),
    }
    let mut stacks = entry_versions
        .iter()
        .copied()
        .map(|version| vec![version])
        .collect::<Vec<_>>();
    let mut visited = 0usize;
    let mut visits = vec![Visit::Enter(0)];
    while let Some(visit) = visits.pop() {
        let block = match visit {
            Visit::Exit(pushed) => {
                for byte in pushed.into_iter().rev() {
                    stacks[byte.0].pop();
                }
                continue;
            }
            Visit::Enter(block) => block,
        };
        visited += 1;
        let mut pushed = Vec::new();
        for &(byte, version) in &phis_by_block[block] {
            stacks[byte.0].push(version);
            pushed.push(byte);
        }
        let mut event = 0usize;
        let mut query = 0usize;
        for position in 0..program.blocks[block].instructions.len() {
            while queries[block]
                .get(query)
                .is_some_and(|candidate| candidate.position == position)
            {
                queries[block][query].reaching = queries[block][query]
                    .bytes
                    .iter()
                    .map(|byte| {
                        stacks[byte.0]
                            .last()
                            .copied()
                            .expect("every tracked byte has an entry version")
                    })
                    .collect();
                query += 1;
            }
            while events[block]
                .get(event)
                .is_some_and(|candidate| candidate.position == position)
            {
                let event_row = &events[block][event];
                for (&byte, &version) in event_row.bytes.iter().zip(&event_row.versions) {
                    stacks[byte.0].push(version);
                    pushed.push(byte);
                }
                event += 1;
            }
        }
        for &successor in &cfg.successors[block] {
            for &(byte, phi) in &phis_by_block[successor] {
                let current = *stacks[byte.0]
                    .last()
                    .expect("every tracked byte has an edge version");
                let MemoryVersionKind::Phi { incoming, .. } = &mut versions[phi.0].kind else {
                    return Err(error(
                        "STATE_HOME.HOME_PHI",
                        Some(program.blocks[successor].id),
                        None,
                        "state-home phi index names a non-phi version",
                    ));
                };
                incoming.push((block, current));
            }
        }
        visits.push(Visit::Exit(pushed));
        visits.extend(
            children[block]
                .iter()
                .rev()
                .map(|&child| Visit::Enter(child)),
        );
    }
    if visited != program.blocks.len() {
        return Err(error(
            "STATE_HOME.HOME_DOMINANCE",
            None,
            None,
            "state-home dominator traversal did not reach every block",
        ));
    }
    for version in &mut versions {
        if let MemoryVersionKind::Phi { block, incoming } = &mut version.kind {
            incoming.sort_unstable_by_key(|(predecessor, _)| *predecessor);
            if incoming.len() != cfg.predecessors[*block].len()
                || incoming
                    .iter()
                    .map(|(predecessor, _)| *predecessor)
                    .ne(cfg.predecessors[*block].iter().copied())
            {
                return Err(error(
                    "STATE_HOME.HOME_PHI",
                    Some(program.blocks[*block].id),
                    None,
                    "state-home phi does not cover every predecessor exactly once",
                ));
            }
        }
    }

    let owners = resolve_owners(&versions)?;
    for query in queries.iter().flatten() {
        if query.reaching.len() != query.bytes.len()
            || query
                .reaching
                .iter()
                .any(|version| owners.get(version.0).copied().flatten() != Some(query.home.id))
        {
            return Err(error(
                "STATE_HOME.RELOAD_ALL_PATH_HOME",
                Some(query.block),
                Some(query.position),
                format!(
                    "state reload {:?} is not reached by home {:?} on every physical byte and CFG path",
                    query.instruction, query.home.id
                ),
            ));
        }
    }
    Ok(())
}

fn resolve_owners(
    versions: &[MemoryVersion],
) -> Result<Vec<Option<StateHomeId>>, MaterializedStateHomeError> {
    let count = versions.len();
    let mut is_phi = vec![false; count];
    let mut forward = vec![Vec::<usize>::new(); count];
    let mut reverse = vec![Vec::<usize>::new(); count];
    for version in versions {
        let MemoryVersionKind::Phi { incoming, .. } = &version.kind else {
            continue;
        };
        is_phi[version.id.0] = true;
        for &(_, input) in incoming {
            if versions[input.0].byte != version.byte {
                return Err(error(
                    "STATE_HOME.HOME_PHI",
                    None,
                    None,
                    "state MemorySSA phi crosses physical bytes",
                ));
            }
            if matches!(versions[input.0].kind, MemoryVersionKind::Phi { .. }) {
                forward[version.id.0].push(input.0);
                reverse[input.0].push(version.id.0);
            }
        }
    }

    let mut seen = vec![false; count];
    let mut postorder = Vec::new();
    for root in 0..count {
        if !is_phi[root] || seen[root] {
            continue;
        }
        seen[root] = true;
        let mut stack = vec![(root, 0usize)];
        while let Some((node, next)) = stack.last_mut() {
            if *next == forward[*node].len() {
                postorder.push(*node);
                stack.pop();
                continue;
            }
            let successor = forward[*node][*next];
            *next += 1;
            if !seen[successor] {
                seen[successor] = true;
                stack.push((successor, 0));
            }
        }
    }
    let mut assigned = vec![false; count];
    let mut components = Vec::<Vec<usize>>::new();
    for &root in postorder.iter().rev() {
        if assigned[root] {
            continue;
        }
        assigned[root] = true;
        let mut component = Vec::new();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            component.push(node);
            for &predecessor in reverse[node].iter().rev() {
                if !assigned[predecessor] {
                    assigned[predecessor] = true;
                    stack.push(predecessor);
                }
            }
        }
        components.push(component);
    }
    let mut component_for = vec![None::<usize>; count];
    for (component, members) in components.iter().enumerate() {
        for &member in members {
            component_for[member] = Some(component);
        }
    }
    let mut dependency_sets = vec![BTreeSet::<usize>::new(); components.len()];
    for (component, members) in components.iter().enumerate() {
        for &member in members {
            let MemoryVersionKind::Phi { incoming, .. } = &versions[member].kind else {
                unreachable!("SCC contains only phi versions")
            };
            for &(_, input) in incoming {
                if let Some(input_component) = component_for[input.0]
                    && input_component != component
                {
                    dependency_sets[component].insert(input_component);
                }
            }
        }
    }
    let dependencies = dependency_sets
        .into_iter()
        .map(|row| row.into_iter().collect::<Vec<_>>())
        .collect::<Vec<_>>();

    let mut state = vec![0u8; components.len()];
    let mut order = Vec::with_capacity(components.len());
    for root in 0..components.len() {
        if state[root] != 0 {
            continue;
        }
        state[root] = 1;
        let mut stack = vec![(root, 0usize)];
        while let Some((component, next)) = stack.last_mut() {
            if *next == dependencies[*component].len() {
                state[*component] = 2;
                order.push(*component);
                stack.pop();
                continue;
            }
            let dependency = dependencies[*component][*next];
            *next += 1;
            match state[dependency] {
                0 => {
                    state[dependency] = 1;
                    stack.push((dependency, 0));
                }
                1 => {
                    return Err(error(
                        "STATE_HOME.HOME_PHI_SCC",
                        None,
                        None,
                        "condensed state MemorySSA phi graph is cyclic",
                    ));
                }
                _ => {}
            }
        }
    }

    let mut owners = versions
        .iter()
        .map(|version| match version.kind {
            MemoryVersionKind::Entry { owner } | MemoryVersionKind::Write { owner } => owner,
            MemoryVersionKind::Phi { .. } => None,
        })
        .collect::<Vec<_>>();
    let mut component_owners = vec![None::<Option<StateHomeId>>; components.len()];
    for component in order {
        let mut external = None::<Option<StateHomeId>>;
        for &member in &components[component] {
            let MemoryVersionKind::Phi { incoming, .. } = &versions[member].kind else {
                unreachable!("SCC contains only phi versions")
            };
            for &(_, input) in incoming {
                if component_for[input.0] == Some(component) {
                    continue;
                }
                let owner = if let Some(input_component) = component_for[input.0] {
                    component_owners[input_component].ok_or_else(|| {
                        error(
                            "STATE_HOME.HOME_PHI_ORDER",
                            None,
                            None,
                            "state MemorySSA dependency was not resolved first",
                        )
                    })?
                } else {
                    owners[input.0]
                };
                external = Some(match external {
                    None => owner,
                    Some(previous) if previous == owner => previous,
                    Some(_) => None,
                });
            }
        }
        let owner = external.ok_or_else(|| {
            error(
                "STATE_HOME.HOME_PHI_SEED",
                None,
                None,
                "state MemorySSA phi SCC has no external version",
            )
        })?;
        component_owners[component] = Some(owner);
        for &member in &components[component] {
            owners[member] = owner;
        }
    }
    Ok(owners)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::mir::{MBlock, OpSize, SpillDesc, VRegAllocator};

    fn home() -> PackedStateHome {
        PackedStateHome {
            id: StateHomeId(0),
            offset: 32,
            size: OpSize::S64,
            live_on_entry: false,
        }
    }

    fn diamond(store_right: bool) -> (MFunction, NormalizedCfg) {
        let mut values = VRegAllocator::new();
        for _ in 0..5 {
            values.alloc();
        }
        let mut function = MFunction::new(values, vec![SpillDesc::transient(); 5]);

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
        left.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 32,
            src: VReg(1),
            size: OpSize::S64,
        });
        left.push(MInst::Jump { target: BlockId(3) });

        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::LoadImm {
            dst: VReg(2),
            value: 13,
        });
        if store_right {
            right.push(MInst::Store {
                base: BaseReg::SimState,
                offset: 32,
                src: VReg(2),
                size: OpSize::S64,
            });
        }
        right.push(MInst::Jump { target: BlockId(3) });

        let mut merge = MBlock::new(BlockId(3));
        merge.push(MInst::Load {
            dst: VReg(3),
            base: BaseReg::SimState,
            offset: 32,
            size: OpSize::S64,
        });
        merge.push(MInst::Mov {
            dst: VReg(4),
            src: VReg(3),
        });
        merge.push(MInst::Return);
        function.blocks = vec![entry, left, right, merge];
        let cfg = super::super::cfg::normalize(&mut function).unwrap();
        (function, cfg)
    }

    #[test]
    fn stores_on_every_arm_reach_the_join_reload() {
        let (function, cfg) = diamond(true);
        verify_materialized_state_homes(
            &function,
            &cfg,
            &[
                MaterializedStateStore {
                    block: BlockId(1),
                    write_ordinal: 0,
                    home: home(),
                },
                MaterializedStateStore {
                    block: BlockId(2),
                    write_ordinal: 0,
                    home: home(),
                },
            ],
            &[MaterializedStateReload {
                reload: VReg(3),
                home: home(),
            }],
        )
        .unwrap();
    }

    #[test]
    fn one_arm_store_cannot_reach_the_join_reload() {
        let (function, cfg) = diamond(false);
        let error = verify_materialized_state_homes(
            &function,
            &cfg,
            &[MaterializedStateStore {
                block: BlockId(1),
                write_ordinal: 0,
                home: home(),
            }],
            &[MaterializedStateReload {
                reload: VReg(3),
                home: home(),
            }],
        )
        .unwrap_err();
        assert_eq!(error.rule, "STATE_HOME.RELOAD_ALL_PATH_HOME");
        assert_eq!(error.block, Some(BlockId(3)));
    }
}
