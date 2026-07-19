//! Sparse late forwarding for direct SimState round trips.
//!
//! Overlapping 8/16/32/64-bit accesses are partitioned into non-overlapping
//! machine cells only to prove exact reaching definitions. Executable MIR is
//! not converted to whole-cell SSA. After pressure scheduling, exact
//! same-shaped loads reached by one store become copy uses of one canonical
//! store value. Register allocation can then keep that concrete use cluster
//! resident or split it back to MemorySSA-proved state reloads at the original
//! load points. Narrow-store normalization is paid once per store version,
//! never once per forwarded load.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt;

use crate::backend::native::memory_effect::{self, UnknownMemory};
use crate::backend::native::mir::{BaseReg, BlockId, MFunction, MInst, OpSize, SpillDesc, VReg};

use super::cfg::NormalizedCfg;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StatePromotionError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub message: String,
}

impl StatePromotionError {
    fn new(
        rule: &'static str,
        block: Option<BlockId>,
        instruction: Option<usize>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            block,
            instruction,
            message: message.into(),
        }
    }
}

impl fmt::Display for StatePromotionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.rule)?;
        if let Some(block) = self.block {
            write!(formatter, " at {block}")?;
        }
        if let Some(instruction) = self.instruction {
            write!(formatter, "/i{instruction}")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for StatePromotionError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct CellId(usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct VersionId(usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cell {
    id: CellId,
    offset: i32,
    size: OpSize,
}

impl Cell {
    fn start(self) -> i64 {
        i64::from(self.offset)
    }

    fn end(self) -> i64 {
        self.start() + i64::from(self.size.bytes())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawAccessKind {
    Load { destination: VReg },
    Store { source: VReg },
    Kill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RawAccess {
    block: usize,
    instruction: usize,
    start: i64,
    end: i64,
    kind: RawAccessKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Component {
    start: i64,
    end: i64,
    has_store: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AccessPart {
    cell: CellId,
    cell_byte_offset: usize,
    access_byte_offset: usize,
    byte_width: usize,
    reaching: VersionId,
    definition: Option<VersionId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Access {
    block: usize,
    instruction: usize,
    kind: RawAccessKind,
    parts: Vec<AccessPart>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VersionKind {
    LiveOnEntry,
    Store {
        block: usize,
        instruction: usize,
    },
    Kill {
        block: usize,
        instruction: usize,
    },
    Phi {
        block: usize,
        incoming: Vec<(usize, VersionId)>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Version {
    id: VersionId,
    cell: CellId,
    kind: VersionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PromotionPlan {
    cells: Vec<Cell>,
    versions: Vec<Version>,
    accesses: Vec<Access>,
    phis_by_block: Vec<Vec<(CellId, VersionId)>>,
    barriers: BarrierSsa,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct BarrierVersionId(usize);

#[derive(Debug, Clone, PartialEq, Eq)]
enum BarrierVersionKind {
    LiveOnEntry,
    Kill {
        block: usize,
        instruction: usize,
    },
    Phi {
        block: usize,
        incoming: Vec<(usize, BarrierVersionId)>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BarrierVersion {
    id: BarrierVersionId,
    kind: BarrierVersionKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct BarrierSsa {
    versions: Vec<BarrierVersion>,
    observed: BTreeMap<(usize, usize), BarrierVersionId>,
    phis_by_block: Vec<Vec<BarrierVersionId>>,
}

fn direct_state_access(inst: &MInst) -> Option<(i64, i64, RawAccessKind)> {
    let (offset, size, kind) = match inst {
        MInst::Load {
            dst,
            base: BaseReg::SimState,
            offset,
            size,
        } => (*offset, *size, RawAccessKind::Load { destination: *dst }),
        MInst::Store {
            base: BaseReg::SimState,
            offset,
            src,
            size,
        } => (*offset, *size, RawAccessKind::Store { source: *src }),
        _ => return None,
    };
    let start = i64::from(offset);
    let end = start.checked_add(i64::from(size.bytes()))?;
    Some((start, end, kind))
}

fn op_size(bytes: usize) -> Option<OpSize> {
    match bytes {
        1 => Some(OpSize::S8),
        2 => Some(OpSize::S16),
        4 => Some(OpSize::S32),
        8 => Some(OpSize::S64),
        _ => None,
    }
}

fn component_for_range(components: &[Component], start: i64, end: i64) -> Option<usize> {
    let index = components.partition_point(|component| component.end <= start);
    components
        .get(index)
        .filter(|component| component.start <= start && end <= component.end)
        .map(|_| index)
}

fn discover_components(
    func: &MFunction,
) -> Result<(Vec<Component>, Vec<RawAccess>, Vec<(usize, usize)>), StatePromotionError> {
    let mut raw = Vec::new();
    let mut intervals = Vec::<(i64, i64, bool)>::new();
    for (block, mir_block) in func.blocks.iter().enumerate() {
        for (instruction, inst) in mir_block.insts.iter().enumerate() {
            let Some((start, end, kind)) = direct_state_access(inst) else {
                continue;
            };
            if start < 0 || start >= end {
                return Err(StatePromotionError::new(
                    "STATE_PROMOTE.DIRECT_RANGE",
                    Some(mir_block.id),
                    Some(instruction),
                    "direct SimState access has an invalid physical range",
                ));
            }
            let store = matches!(kind, RawAccessKind::Store { .. });
            intervals.push((start, end, store));
            raw.push(RawAccess {
                block,
                instruction,
                start,
                end,
                kind,
            });
        }
    }
    intervals.sort_unstable_by_key(|&(start, end, store)| (start, end, store));
    let mut components = Vec::<Component>::new();
    for (start, end, store) in intervals {
        if let Some(last) = components.last_mut()
            && start < last.end
        {
            last.end = last.end.max(end);
            last.has_store |= store;
        } else {
            components.push(Component {
                start,
                end,
                has_store: store,
            });
        }
    }

    // Non-direct writes are MemorySSA definitions at their real program
    // points.  They do not blacklist an address range for the whole function:
    // a preceding or non-reaching indexed write cannot invalidate an exact
    // store-to-load chain elsewhere in the CFG.
    let mut unknown_kills = Vec::<(usize, usize)>::new();
    for (block, mir_block) in func.blocks.iter().enumerate() {
        for (instruction, inst) in mir_block.insts.iter().enumerate() {
            if direct_state_access(inst).is_some() {
                continue;
            }
            let effects = memory_effect::writes(inst);
            if matches!(
                effects.unknown_memory(),
                Some(UnknownMemory::Direct(BaseReg::SimState))
            ) {
                unknown_kills.push((block, instruction));
            }
            for range in effects
                .ranges()
                .filter(|range| range.base == BaseReg::SimState)
            {
                let Some(end) = range.end() else {
                    return Err(StatePromotionError::new(
                        "STATE_PROMOTE.KILL_RANGE",
                        Some(mir_block.id),
                        Some(instruction),
                        "known SimState write range exceeds the address domain",
                    ));
                };
                let mut component =
                    components.partition_point(|component| component.end <= range.offset);
                while let Some(overlap) = components.get(component) {
                    if overlap.start >= end {
                        break;
                    }
                    if overlap.has_store {
                        raw.push(RawAccess {
                            block,
                            instruction,
                            start: overlap.start.max(range.offset),
                            end: overlap.end.min(end),
                            kind: RawAccessKind::Kill,
                        });
                    }
                    component += 1;
                }
            }
        }
    }
    Ok((components, raw, unknown_kills))
}

fn build_cells(
    components: &[Component],
) -> Result<(Vec<Cell>, Vec<Vec<CellId>>), StatePromotionError> {
    let mut cells = Vec::new();
    let mut by_component = vec![Vec::new(); components.len()];
    for (component_index, component) in components.iter().enumerate() {
        if !component.has_store {
            continue;
        }
        let mut cursor = component.start;
        while cursor < component.end {
            let remaining = usize::try_from(component.end - cursor).map_err(|_| {
                StatePromotionError::new(
                    "STATE_PROMOTE.COMPONENT_RANGE",
                    None,
                    None,
                    "physical component length does not fit usize",
                )
            })?;
            let bytes = if remaining >= 8 {
                8
            } else if remaining >= 4 {
                4
            } else if remaining >= 2 {
                2
            } else {
                1
            };
            let offset = i32::try_from(cursor).map_err(|_| {
                StatePromotionError::new(
                    "STATE_PROMOTE.CELL_OFFSET",
                    None,
                    None,
                    "promoted physical offset does not fit MIR i32",
                )
            })?;
            let id = CellId(cells.len());
            cells.push(Cell {
                id,
                offset,
                size: op_size(bytes).expect("cell partition uses native powers of two"),
            });
            by_component[component_index].push(id);
            cursor += i64::try_from(bytes).expect("native cell size fits i64");
        }
    }
    Ok((cells, by_component))
}

fn map_accesses(
    components: &[Component],
    raw: &[RawAccess],
    cells: &[Cell],
    cells_by_component: &[Vec<CellId>],
) -> Result<Vec<Access>, StatePromotionError> {
    let mut accesses = Vec::new();
    for access in raw {
        let Some(component) = component_for_range(components, access.start, access.end) else {
            return Err(StatePromotionError::new(
                "STATE_PROMOTE.ACCESS_COMPONENT",
                None,
                None,
                "direct state access is not covered by its overlap component",
            ));
        };
        if !components[component].has_store {
            continue;
        }
        let mut parts = Vec::new();
        let mut cursor = access.start;
        for &cell_id in &cells_by_component[component] {
            let cell = cells[cell_id.0];
            let start = cursor.max(cell.start());
            let end = access.end.min(cell.end());
            if start >= end {
                continue;
            }
            if start != cursor {
                return Err(StatePromotionError::new(
                    "STATE_PROMOTE.ACCESS_COVERAGE",
                    None,
                    None,
                    "promoted cells leave a hole in a direct access",
                ));
            }
            parts.push(AccessPart {
                cell: cell_id,
                cell_byte_offset: usize::try_from(start - cell.start()).map_err(|_| {
                    StatePromotionError::new(
                        "STATE_PROMOTE.ACCESS_RANGE",
                        None,
                        None,
                        "cell-relative byte offset does not fit usize",
                    )
                })?,
                access_byte_offset: usize::try_from(start - access.start).map_err(|_| {
                    StatePromotionError::new(
                        "STATE_PROMOTE.ACCESS_RANGE",
                        None,
                        None,
                        "access-relative byte offset does not fit usize",
                    )
                })?,
                byte_width: usize::try_from(end - start).map_err(|_| {
                    StatePromotionError::new(
                        "STATE_PROMOTE.ACCESS_RANGE",
                        None,
                        None,
                        "access fragment width does not fit usize",
                    )
                })?,
                reaching: VersionId(cell_id.0),
                definition: None,
            });
            cursor = end;
        }
        if cursor != access.end || parts.is_empty() {
            return Err(StatePromotionError::new(
                "STATE_PROMOTE.ACCESS_COVERAGE",
                None,
                None,
                "promoted cells do not exactly cover a direct access",
            ));
        }
        accesses.push(Access {
            block: access.block,
            instruction: access.instruction,
            kind: access.kind,
            parts,
        });
    }
    Ok(accesses)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BarrierEventKind {
    Query,
    Kill { definition: BarrierVersionId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BarrierEvent {
    instruction: usize,
    kind: BarrierEventKind,
}

/// Build one sparse MemorySSA generation for writes whose concrete SimState
/// range is unknown.
///
/// Expanding such a write into one definition per tracked physical cell would
/// cost `unknown_writes * cells`.  A direct store and load can instead compare
/// the single unknown-memory generation observed at their exact points.  If a
/// write reaches only one arm or a later loop iteration, ordinary phi
/// placement gives the load a different generation and forwarding is rejected.
fn analyze_unknown_barriers(
    func: &MFunction,
    cfg: &NormalizedCfg,
    raw: &[RawAccess],
    unknown_kills: &[(usize, usize)],
) -> Result<BarrierSsa, StatePromotionError> {
    let queries = raw
        .iter()
        .filter(|access| {
            matches!(
                access.kind,
                RawAccessKind::Load { .. } | RawAccessKind::Store { .. }
            )
        })
        .map(|access| (access.block, access.instruction))
        .collect::<BTreeSet<_>>();
    if queries.is_empty() {
        return Ok(BarrierSsa {
            phis_by_block: vec![Vec::new(); func.blocks.len()],
            ..BarrierSsa::default()
        });
    }

    let mut events = vec![Vec::<BarrierEvent>::new(); func.blocks.len()];
    for &(block, instruction) in &queries {
        events[block].push(BarrierEvent {
            instruction,
            kind: BarrierEventKind::Query,
        });
    }
    let mut versions = vec![BarrierVersion {
        id: BarrierVersionId(0),
        kind: BarrierVersionKind::LiveOnEntry,
    }];
    let mut definitions = HashSet::<usize>::new();
    for &(block, instruction) in unknown_kills {
        let definition = BarrierVersionId(versions.len());
        versions.push(BarrierVersion {
            id: definition,
            kind: BarrierVersionKind::Kill { block, instruction },
        });
        definitions.insert(block);
        events[block].push(BarrierEvent {
            instruction,
            kind: BarrierEventKind::Kill { definition },
        });
    }
    for block_events in &mut events {
        block_events.sort_unstable_by_key(|event| event.instruction);
    }

    let mut upward_uses = HashSet::<usize>::new();
    for (block, block_events) in events.iter().enumerate() {
        let mut defined = false;
        for event in block_events {
            match event.kind {
                BarrierEventKind::Query if !defined => {
                    upward_uses.insert(block);
                }
                BarrierEventKind::Kill { .. } => defined = true,
                BarrierEventKind::Query => {}
            }
        }
    }
    let mut live_in = upward_uses.clone();
    let mut live_work = upward_uses.into_iter().collect::<VecDeque<_>>();
    while let Some(block) = live_work.pop_front() {
        for &predecessor in &cfg.predecessors[block] {
            if !definitions.contains(&predecessor) && live_in.insert(predecessor) {
                live_work.push_back(predecessor);
            }
        }
    }

    let mut phi_blocks = HashSet::<usize>::new();
    let mut queued = definitions.clone();
    let mut phi_work = definitions.iter().copied().collect::<Vec<_>>();
    while let Some(block) = phi_work.pop() {
        for &frontier in &cfg.dominance_frontier[block] {
            if frontier == 0 || !live_in.contains(&frontier) || !phi_blocks.insert(frontier) {
                continue;
            }
            if queued.insert(frontier) {
                phi_work.push(frontier);
            }
        }
    }
    let mut phis_by_block = vec![Vec::<BarrierVersionId>::new(); func.blocks.len()];
    let mut ordered_phis = phi_blocks.into_iter().collect::<Vec<_>>();
    ordered_phis.sort_unstable();
    for block in ordered_phis {
        let id = BarrierVersionId(versions.len());
        versions.push(BarrierVersion {
            id,
            kind: BarrierVersionKind::Phi {
                block,
                incoming: Vec::with_capacity(cfg.predecessors[block].len()),
            },
        });
        phis_by_block[block].push(id);
    }

    let mut children = vec![Vec::<usize>::new(); func.blocks.len()];
    for block in 1..func.blocks.len() {
        let parent = cfg.idom[block].ok_or_else(|| {
            StatePromotionError::new(
                "STATE_PROMOTE.BARRIER_DOMINATOR_TREE",
                Some(func.blocks[block].id),
                None,
                "reachable unknown-memory block has no immediate dominator",
            )
        })?;
        children[parent].push(block);
    }
    enum Action {
        Enter(usize),
        Exit(BarrierVersionId),
    }
    let mut current = BarrierVersionId(0);
    let mut observed = BTreeMap::<(usize, usize), BarrierVersionId>::new();
    let mut actions = vec![Action::Enter(0)];
    while let Some(action) = actions.pop() {
        let block = match action {
            Action::Exit(previous) => {
                current = previous;
                continue;
            }
            Action::Enter(block) => block,
        };
        let previous = current;
        if let Some(&phi) = phis_by_block[block].first() {
            current = phi;
        }
        for event in &events[block] {
            match event.kind {
                BarrierEventKind::Query => {
                    if observed
                        .insert((block, event.instruction), current)
                        .is_some()
                    {
                        return Err(StatePromotionError::new(
                            "STATE_PROMOTE.BARRIER_QUERY_IDENTITY",
                            Some(func.blocks[block].id),
                            Some(event.instruction),
                            "one direct state instruction has multiple barrier queries",
                        ));
                    }
                }
                BarrierEventKind::Kill { definition } => current = definition,
            }
        }
        for &successor in &cfg.successors[block] {
            for &phi in &phis_by_block[successor] {
                let BarrierVersionKind::Phi { incoming, .. } = &mut versions[phi.0].kind else {
                    unreachable!("barrier phi table references a non-phi version");
                };
                incoming.push((block, current));
            }
        }
        actions.push(Action::Exit(previous));
        actions.extend(children[block].iter().rev().copied().map(Action::Enter));
    }

    for version in &mut versions {
        if let BarrierVersionKind::Phi { block, incoming } = &mut version.kind {
            incoming.sort_unstable_by_key(|(predecessor, _)| *predecessor);
            if incoming.len() != cfg.predecessors[*block].len()
                || incoming
                    .iter()
                    .zip(&cfg.predecessors[*block])
                    .any(|((actual, _), expected)| actual != expected)
            {
                return Err(StatePromotionError::new(
                    "STATE_PROMOTE.BARRIER_PHI_INPUTS",
                    Some(func.blocks[*block].id),
                    None,
                    "unknown-memory phi does not cover every predecessor exactly once",
                ));
            }
        }
    }
    if observed.len() != queries.len() {
        return Err(StatePromotionError::new(
            "STATE_PROMOTE.BARRIER_QUERY_COVERAGE",
            None,
            None,
            "unknown-memory SSA did not visit every direct state access",
        ));
    }
    Ok(BarrierSsa {
        versions,
        observed,
        phis_by_block,
    })
}

fn analyze(func: &MFunction, cfg: &NormalizedCfg) -> Result<PromotionPlan, StatePromotionError> {
    let timing = std::env::var_os("CELOX_REGALLOC_TIMING").is_some()
        || std::env::var_os("CELOX_PHASE_TIMING").is_some();
    if func.blocks.len() != cfg.predecessors.len()
        || func.blocks.len() != cfg.successors.len()
        || func.blocks.len() != cfg.idom.len()
    {
        return Err(StatePromotionError::new(
            "STATE_PROMOTE.CFG_SHAPE",
            None,
            None,
            "normalized CFG does not cover every MIR block",
        ));
    }
    let phase_start = timing.then(std::time::Instant::now);
    let (components, raw, unknown_kills) = discover_components(func)?;
    if let Some(start) = phase_start {
        eprintln!(
            "[state-forward-timing] discover elapsed={:?}",
            start.elapsed()
        );
    }
    let phase_start = timing.then(std::time::Instant::now);
    let barriers = analyze_unknown_barriers(func, cfg, &raw, &unknown_kills)?;
    if let Some(start) = phase_start {
        eprintln!(
            "[state-forward-timing] barrier_ssa elapsed={:?}",
            start.elapsed()
        );
    }
    let phase_start = timing.then(std::time::Instant::now);
    let (cells, cells_by_component) = build_cells(&components)?;
    let mut accesses = map_accesses(&components, &raw, &cells, &cells_by_component)?;
    if let Some(start) = phase_start {
        eprintln!(
            "[state-forward-timing] partition elapsed={:?}",
            start.elapsed()
        );
    }
    if cells.is_empty() {
        return Ok(PromotionPlan {
            cells,
            versions: Vec::new(),
            accesses,
            phis_by_block: vec![Vec::new(); func.blocks.len()],
            barriers,
        });
    }

    let phase_start = timing.then(std::time::Instant::now);
    let mut accesses_by_block = vec![Vec::<usize>::new(); func.blocks.len()];
    let mut definitions = HashSet::<(CellId, usize)>::new();
    for (index, access) in accesses.iter().enumerate() {
        accesses_by_block[access.block].push(index);
    }
    for block_accesses in &mut accesses_by_block {
        block_accesses.sort_unstable_by_key(|&access| accesses[access].instruction);
    }
    for (block, block_accesses) in accesses_by_block.iter().enumerate() {
        for &access_index in block_accesses {
            let access = &accesses[access_index];
            match access.kind {
                RawAccessKind::Store { .. } | RawAccessKind::Kill => {
                    for part in &access.parts {
                        definitions.insert((part.cell, block));
                    }
                }
                RawAccessKind::Load { .. } => {}
            }
        }
    }
    if let Some(start) = phase_start {
        eprintln!(
            "[state-forward-timing] local_def_use elapsed={:?}",
            start.elapsed()
        );
    }

    // Build minimal SSA directly from definition blocks.  Computing pruned
    // SSA here requires one backwards liveness problem per physical state
    // cell; on a large RTL CFG that is O(cells * CFG).  Cytron IDF placement
    // is independent of uses and remains sparse for the common case where a
    // state location has one dominating store.
    let phase_start = timing.then(std::time::Instant::now);
    let mut phi_pairs = HashSet::<(CellId, usize)>::new();
    let mut queued = definitions.clone();
    let mut phi_work = definitions.iter().copied().collect::<Vec<_>>();
    while let Some((cell, block)) = phi_work.pop() {
        for &frontier in &cfg.dominance_frontier[block] {
            let pair = (cell, frontier);
            if frontier == 0 || !phi_pairs.insert(pair) {
                continue;
            }
            if queued.insert(pair) {
                phi_work.push(pair);
            }
        }
    }
    if let Some(start) = phase_start {
        eprintln!(
            "[state-forward-timing] phi_placement elapsed={:?}",
            start.elapsed()
        );
    }

    let phase_start = timing.then(std::time::Instant::now);
    let mut versions = cells
        .iter()
        .map(|cell| Version {
            id: VersionId(cell.id.0),
            cell: cell.id,
            kind: VersionKind::LiveOnEntry,
        })
        .collect::<Vec<_>>();
    let mut phis_by_block = vec![Vec::<(CellId, VersionId)>::new(); func.blocks.len()];
    let mut ordered_phis = phi_pairs.into_iter().collect::<Vec<_>>();
    ordered_phis.sort_unstable_by_key(|(cell, block)| (*block, *cell));
    for (cell, block) in ordered_phis {
        let id = VersionId(versions.len());
        versions.push(Version {
            id,
            cell,
            kind: VersionKind::Phi {
                block,
                incoming: Vec::new(),
            },
        });
        phis_by_block[block].push((cell, id));
    }
    for access in &mut accesses {
        let version_kind = match access.kind {
            RawAccessKind::Store { .. } => Some(false),
            RawAccessKind::Kill => Some(true),
            RawAccessKind::Load { .. } => None,
        };
        let Some(kill) = version_kind else { continue };
        for part in &mut access.parts {
            let id = VersionId(versions.len());
            versions.push(Version {
                id,
                cell: part.cell,
                kind: if kill {
                    VersionKind::Kill {
                        block: access.block,
                        instruction: access.instruction,
                    }
                } else {
                    VersionKind::Store {
                        block: access.block,
                        instruction: access.instruction,
                    }
                },
            });
            part.definition = Some(id);
        }
    }

    let mut children = vec![Vec::<usize>::new(); func.blocks.len()];
    for block in 1..func.blocks.len() {
        let parent = cfg.idom[block].ok_or_else(|| {
            StatePromotionError::new(
                "STATE_PROMOTE.DOMINATOR_TREE",
                Some(func.blocks[block].id),
                None,
                "reachable non-entry block has no immediate dominator",
            )
        })?;
        children[parent].push(block);
    }
    enum Action {
        Enter(usize),
        Exit(Vec<(CellId, VersionId)>),
    }
    let mut current = cells
        .iter()
        .map(|cell| VersionId(cell.id.0))
        .collect::<Vec<_>>();
    let mut actions = vec![Action::Enter(0)];
    while let Some(action) = actions.pop() {
        let block = match action {
            Action::Exit(changes) => {
                for (cell, previous) in changes.into_iter().rev() {
                    current[cell.0] = previous;
                }
                continue;
            }
            Action::Enter(block) => block,
        };
        let mut changes = Vec::new();
        for &(cell, version) in &phis_by_block[block] {
            changes.push((cell, current[cell.0]));
            current[cell.0] = version;
        }
        for &access_index in &accesses_by_block[block] {
            let access = &mut accesses[access_index];
            for part in &mut access.parts {
                part.reaching = current[part.cell.0];
                if let Some(definition) = part.definition {
                    changes.push((part.cell, current[part.cell.0]));
                    current[part.cell.0] = definition;
                }
            }
        }
        for &successor in &cfg.successors[block] {
            for &(_, version) in &phis_by_block[successor] {
                let cell = versions[version.0].cell;
                let incoming = current[cell.0];
                let VersionKind::Phi { incoming: row, .. } = &mut versions[version.0].kind else {
                    unreachable!("phi table references a phi version");
                };
                row.push((block, incoming));
            }
        }
        actions.push(Action::Exit(changes));
        actions.extend(children[block].iter().rev().copied().map(Action::Enter));
    }

    let version_cells = versions
        .iter()
        .map(|version| version.cell)
        .collect::<Vec<_>>();
    for version in &mut versions {
        if let VersionKind::Phi { block, incoming } = &mut version.kind {
            incoming.sort_unstable_by_key(|(predecessor, _)| *predecessor);
            let expected = &cfg.predecessors[*block];
            if incoming.len() != expected.len()
                || incoming
                    .iter()
                    .zip(expected)
                    .any(|((actual, _), expected)| actual != expected)
            {
                return Err(StatePromotionError::new(
                    "STATE_PROMOTE.PHI_INPUTS",
                    Some(func.blocks[*block].id),
                    None,
                    "physical state phi does not cover every predecessor exactly once",
                ));
            }
            if incoming
                .iter()
                .any(|(_, source)| version_cells[source.0] != version.cell)
            {
                return Err(StatePromotionError::new(
                    "STATE_PROMOTE.PHI_CELL",
                    Some(func.blocks[*block].id),
                    None,
                    "physical state phi mixes different machine cells",
                ));
            }
        }
    }
    if let Some(start) = phase_start {
        eprintln!(
            "[state-forward-timing] rename_verify elapsed={:?}",
            start.elapsed()
        );
    }
    Ok(PromotionPlan {
        cells,
        versions,
        accesses,
        phis_by_block,
        barriers,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ForwardCandidate {
    store_block: usize,
    store_instruction: usize,
    block: usize,
    instruction: usize,
    destination: VReg,
    source: VReg,
    size: OpSize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ForwardCluster {
    store_block: usize,
    store_instruction: usize,
    source: VReg,
    size: OpSize,
    loads: Vec<ForwardCandidate>,
}

fn exact_forward_candidates(
    func: &MFunction,
    plan: &PromotionPlan,
) -> Result<Vec<ForwardCandidate>, StatePromotionError> {
    let accesses = plan
        .accesses
        .iter()
        .filter(|access| matches!(access.kind, RawAccessKind::Store { .. }))
        .map(|access| ((access.block, access.instruction), access))
        .collect::<HashMap<_, _>>();
    let mut candidates = Vec::new();
    for load in &plan.accesses {
        let RawAccessKind::Load { destination } = load.kind else {
            continue;
        };
        let mut reaching_store = None::<(usize, usize)>;
        for part in &load.parts {
            let VersionKind::Store { block, instruction } = plan.versions[part.reaching.0].kind
            else {
                reaching_store = None;
                break;
            };
            let location = (block, instruction);
            match reaching_store {
                Some(previous) if previous != location => {
                    reaching_store = None;
                    break;
                }
                Some(_) => {}
                None => reaching_store = Some(location),
            }
        }
        let Some(store_location) = reaching_store else {
            continue;
        };
        let Some(store) = accesses.get(&store_location) else {
            return Err(StatePromotionError::new(
                "STATE_PROMOTE.STORE_ACCESS",
                func.blocks.get(load.block).map(|block| block.id),
                Some(load.instruction),
                "reaching state version has no matching store access",
            ));
        };
        let RawAccessKind::Store { source } = store.kind else {
            return Err(StatePromotionError::new(
                "STATE_PROMOTE.STORE_ACCESS",
                func.blocks.get(load.block).map(|block| block.id),
                Some(load.instruction),
                "reaching store version points at a non-store access",
            ));
        };
        let load_barrier = plan
            .barriers
            .observed
            .get(&(load.block, load.instruction))
            .copied()
            .ok_or_else(|| {
                StatePromotionError::new(
                    "STATE_PROMOTE.BARRIER_LOAD",
                    func.blocks.get(load.block).map(|block| block.id),
                    Some(load.instruction),
                    "direct load has no unknown-memory generation",
                )
            })?;
        let store_barrier = plan
            .barriers
            .observed
            .get(&store_location)
            .copied()
            .ok_or_else(|| {
                StatePromotionError::new(
                    "STATE_PROMOTE.BARRIER_STORE",
                    func.blocks.get(store_location.0).map(|block| block.id),
                    Some(store_location.1),
                    "reaching direct store has no unknown-memory generation",
                )
            })?;
        if load_barrier != store_barrier {
            continue;
        }
        let Some(MInst::Load {
            base: BaseReg::SimState,
            offset: load_offset,
            size: load_size,
            ..
        }) = func
            .blocks
            .get(load.block)
            .and_then(|block| block.insts.get(load.instruction))
        else {
            return Err(StatePromotionError::new(
                "STATE_PROMOTE.LOAD_ACCESS",
                func.blocks.get(load.block).map(|block| block.id),
                Some(load.instruction),
                "analyzed load no longer matches MIR",
            ));
        };
        let Some(MInst::Store {
            base: BaseReg::SimState,
            offset: store_offset,
            size: store_size,
            src: store_source,
        }) = func
            .blocks
            .get(store_location.0)
            .and_then(|block| block.insts.get(store_location.1))
        else {
            return Err(StatePromotionError::new(
                "STATE_PROMOTE.STORE_ACCESS",
                func.blocks.get(store_location.0).map(|block| block.id),
                Some(store_location.1),
                "analyzed store no longer matches MIR",
            ));
        };
        if source != *store_source {
            return Err(StatePromotionError::new(
                "STATE_PROMOTE.STORE_SOURCE",
                func.blocks.get(store_location.0).map(|block| block.id),
                Some(store_location.1),
                "state-version source differs from the reaching MIR store",
            ));
        }
        if load_offset != store_offset || load_size != store_size || source == destination {
            continue;
        }
        candidates.push(ForwardCandidate {
            store_block: store_location.0,
            store_instruction: store_location.1,
            block: load.block,
            instruction: load.instruction,
            destination,
            source,
            size: *load_size,
        });
    }
    candidates.sort_unstable_by_key(|candidate| (candidate.block, candidate.instruction));
    Ok(candidates)
}

fn cluster_candidates(
    candidates: Vec<ForwardCandidate>,
) -> Result<Vec<ForwardCluster>, StatePromotionError> {
    let mut by_store = BTreeMap::<(usize, usize), ForwardCluster>::new();
    for candidate in candidates {
        let key = (candidate.store_block, candidate.store_instruction);
        let cluster = by_store.entry(key).or_insert_with(|| ForwardCluster {
            store_block: candidate.store_block,
            store_instruction: candidate.store_instruction,
            source: candidate.source,
            size: candidate.size,
            loads: Vec::new(),
        });
        if cluster.source != candidate.source || cluster.size != candidate.size {
            return Err(StatePromotionError::new(
                "STATE_PROMOTE.CLUSTER_STORE",
                None,
                None,
                "one physical store location names incompatible forwarding clusters",
            ));
        }
        cluster.loads.push(candidate);
    }
    for cluster in by_store.values_mut() {
        cluster
            .loads
            .sort_unstable_by_key(|candidate| (candidate.block, candidate.instruction));
    }
    Ok(by_store.into_values().collect())
}

fn allocate_cluster_value(func: &mut MFunction) -> Result<VReg, StatePromotionError> {
    let value = func.vregs.alloc();
    if value.0 as usize != func.spill_descs.len() {
        return Err(StatePromotionError::new(
            "STATE_PROMOTE.VALUE_DOMAIN",
            None,
            None,
            "MIR VReg allocator and spill-descriptor table are not dense",
        ));
    }
    func.spill_descs.push(SpillDesc::transient());
    Ok(value)
}

fn canonicalize_store_value(
    func: &mut MFunction,
    source: VReg,
    size: OpSize,
    canonical_bits: &[u8],
) -> Result<(VReg, Option<MInst>), StatePromotionError> {
    let stored_bits = u8::try_from(size.bytes() * 8).expect("native store width fits u8");
    let source_bits = canonical_bits.get(source.0 as usize).ok_or_else(|| {
        StatePromotionError::new(
            "STATE_PROMOTE.CANONICAL_VALUE",
            None,
            None,
            "store source is outside the canonical-value side table",
        )
    })?;
    if *source_bits <= stored_bits {
        return Ok((source, None));
    }
    let canonical = allocate_cluster_value(func)?;
    let normalization = match size {
        OpSize::S8 => MInst::AndImm32 {
            dst: canonical,
            src: source,
            imm: u8::MAX.into(),
        },
        OpSize::S16 => MInst::AndImm32 {
            dst: canonical,
            src: source,
            imm: u16::MAX.into(),
        },
        OpSize::S32 => MInst::Mov32 {
            dst: canonical,
            src: source,
        },
        OpSize::S64 => unreachable!("64-bit stores need no canonicalization"),
    };
    Ok((canonical, Some(normalization)))
}

/// Forward exact same-shaped state round trips after pressure scheduling.
///
/// Each original store remains the packed-state home. Loads reached by that
/// exact store share one canonical store value and become ordinary copy
/// affinities. If the cluster is evicted, point-specific MemorySSA
/// rematerialization recreates a load at the corresponding use. No state cell,
/// terminal writeback, or synthetic phi is added here.
pub(super) fn forward_exact_round_trips(
    func: &mut MFunction,
    cfg: &NormalizedCfg,
) -> Result<usize, StatePromotionError> {
    let plan = analyze(func, cfg)?;
    let candidates = exact_forward_candidates(func, &plan)?;
    if candidates.is_empty() {
        return Ok(0);
    }

    let mut rewritten = func.clone();
    let clusters = cluster_candidates(candidates)?;
    let canonical_bits = super::reload::canonical_value_bits(&rewritten).map_err(|error| {
        StatePromotionError::new(
            error.rule,
            error.block,
            error.instruction,
            format!("canonical store-value analysis failed: {}", error.message),
        )
    })?;
    let mut stores = BTreeMap::<(usize, usize), (VReg, Option<MInst>)>::new();
    let mut loads = BTreeMap::<(usize, usize), (VReg, VReg)>::new();
    let mut forwarded = 0usize;
    for cluster in clusters {
        let (canonical, normalization) = canonicalize_store_value(
            &mut rewritten,
            cluster.source,
            cluster.size,
            &canonical_bits,
        )?;
        stores.insert(
            (cluster.store_block, cluster.store_instruction),
            (canonical, normalization),
        );
        for load in cluster.loads {
            if loads
                .insert(
                    (load.block, load.instruction),
                    (load.destination, canonical),
                )
                .is_some()
            {
                return Err(StatePromotionError::new(
                    "STATE_PROMOTE.CLUSTER_LOAD",
                    rewritten.blocks.get(load.block).map(|block| block.id),
                    Some(load.instruction),
                    "one load location belongs to multiple forwarding clusters",
                ));
            }
            rewritten.spill_descs[load.destination.0 as usize] = SpillDesc::transient();
            forwarded = forwarded.saturating_add(1);
        }
    }

    for block in 0..rewritten.blocks.len() {
        let original = std::mem::take(&mut rewritten.blocks[block].insts);
        let mut instructions = Vec::with_capacity(original.len());
        for (instruction, mut inst) in original.into_iter().enumerate() {
            if let Some(&(canonical, ref normalization)) = stores.get(&(block, instruction)) {
                if let Some(normalization) = normalization {
                    instructions.push(normalization.clone());
                }
                let MInst::Store {
                    base: BaseReg::SimState,
                    src,
                    ..
                } = &mut inst
                else {
                    return Err(StatePromotionError::new(
                        "STATE_PROMOTE.CLUSTER_STORE",
                        Some(rewritten.blocks[block].id),
                        Some(instruction),
                        "forwarding cluster store no longer matches MIR",
                    ));
                };
                *src = canonical;
            }
            if let Some(&(destination, canonical)) = loads.get(&(block, instruction)) {
                instructions.push(MInst::Mov {
                    dst: destination,
                    src: canonical,
                });
            } else {
                instructions.push(inst);
            }
        }
        rewritten.blocks[block].insts = instructions;
    }
    rewritten.verify_result().map_err(|error| {
        StatePromotionError::new(
            "STATE_PROMOTE.MIR_VERIFY",
            None,
            None,
            format!("state-forwarded MIR failed canonical verification: {error}"),
        )
    })?;
    *func = rewritten;
    Ok(forwarded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{MBlock, MemoryAliasRange, VRegAllocator};

    fn function(value_count: u32, blocks: Vec<MBlock>) -> MFunction {
        let mut values = VRegAllocator::new();
        for _ in 0..value_count {
            values.alloc();
        }
        let mut function = MFunction::new(
            values,
            (0..value_count).map(|_| SpillDesc::transient()).collect(),
        );
        function.blocks = blocks;
        function
    }

    fn normalize(function: &mut MFunction) -> NormalizedCfg {
        super::super::cfg::normalize(function).unwrap()
    }

    #[test]
    fn same_store_load_cluster_shares_one_width_normalization() {
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 0x1ff,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 7,
                src: VReg(0),
                size: OpSize::S8,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 7,
                size: OpSize::S8,
            },
            MInst::Store {
                base: BaseReg::StackFrame,
                offset: 0,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 7,
                size: OpSize::S8,
            },
            MInst::Store {
                base: BaseReg::StackFrame,
                offset: 8,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut function = function(3, vec![block]);
        let cfg = normalize(&mut function);

        assert_eq!(forward_exact_round_trips(&mut function, &cfg).unwrap(), 2);
        assert!(matches!(
            function.blocks[0].insts[1],
            MInst::AndImm32 {
                dst: VReg(3),
                src: VReg(0),
                imm: 0xff,
            }
        ));
        assert!(matches!(
            function.blocks[0].insts[2],
            MInst::Store {
                base: BaseReg::SimState,
                offset: 7,
                src: VReg(3),
                size: OpSize::S8,
            }
        ));
        assert!(matches!(
            function.blocks[0].insts[3],
            MInst::Mov {
                dst: VReg(1),
                src: VReg(3),
            }
        ));
        assert!(matches!(
            function.blocks[0].insts[5],
            MInst::Mov {
                dst: VReg(2),
                src: VReg(3),
            }
        ));
        assert_eq!(function.vregs.count(), 4);
    }

    #[test]
    fn already_canonical_narrow_store_reuses_its_source_value() {
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 3,
                size: OpSize::S8,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 7,
                src: VReg(0),
                size: OpSize::S8,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 7,
                size: OpSize::S8,
            },
            MInst::Return,
        ];
        let mut function = function(2, vec![block]);
        let cfg = normalize(&mut function);

        assert_eq!(forward_exact_round_trips(&mut function, &cfg).unwrap(), 1);
        assert_eq!(function.vregs.count(), 2);
        assert!(matches!(
            function.blocks[0].insts[1],
            MInst::Store {
                src: VReg(0),
                size: OpSize::S8,
                ..
            }
        ));
        assert!(matches!(
            function.blocks[0].insts[2],
            MInst::Mov {
                dst: VReg(1),
                src: VReg(0),
            }
        ));
    }

    #[test]
    fn differently_reaching_overlapping_store_keeps_the_original_load() {
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 1,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(0),
                size: OpSize::S64,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 2,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 7,
                src: VReg(1),
                size: OpSize::S8,
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut function = function(3, vec![block]);
        let cfg = normalize(&mut function);

        assert_eq!(forward_exact_round_trips(&mut function, &cfg).unwrap(), 0);
        assert!(matches!(
            function.blocks[0].insts[4],
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            }
        ));
    }

    #[test]
    fn one_arm_store_keeps_the_join_load_as_memory() {
        let mut entry = MBlock::new(BlockId(0));
        entry.insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 1,
            },
            MInst::Branch {
                cond: VReg(0),
                true_bb: BlockId(1),
                false_bb: BlockId(2),
            },
        ];
        let mut dirty = MBlock::new(BlockId(1));
        dirty.insts = vec![
            MInst::LoadImm {
                dst: VReg(1),
                value: 9,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Jump { target: BlockId(3) },
        ];
        let mut clean = MBlock::new(BlockId(2));
        clean.insts = vec![MInst::Jump { target: BlockId(3) }];
        let mut join = MBlock::new(BlockId(3));
        join.insts = vec![
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut function = function(3, vec![entry, dirty, clean, join]);
        let cfg = normalize(&mut function);

        assert_eq!(forward_exact_round_trips(&mut function, &cfg).unwrap(), 0);
        let join = cfg.block_index[&BlockId(3)];
        assert!(function.blocks[join].phis.is_empty());
        assert!(matches!(
            function.blocks[join].insts[0],
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            }
        ));
    }

    #[test]
    fn indexed_reads_do_not_kill_forwardable_state_versions() {
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 0,
            },
            MInst::LoadIndexed {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 0,
                index: VReg(0),
                size: OpSize::S8,
                alias_range: MemoryAliasRange::new(64, 8),
            },
            MInst::LoadImm {
                dst: VReg(2),
                value: 5,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 64,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut function = function(3, vec![block]);
        let cfg = normalize(&mut function);
        let plan = analyze(&function, &cfg).unwrap();
        assert!(plan.cells.iter().any(|cell| cell.offset == 8));
        assert!(plan.cells.iter().any(|cell| cell.offset == 64));
    }

    #[test]
    fn bounded_indexed_store_kills_only_the_reaching_component() {
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 0,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 5,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 64,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::StoreIndexed {
                base: BaseReg::SimState,
                offset: 64,
                index: VReg(0),
                src: VReg(1),
                size: OpSize::S8,
                alias_range: MemoryAliasRange::new(64, 8),
            },
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(3),
                base: BaseReg::SimState,
                offset: 64,
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut function = function(4, vec![block]);
        let cfg = normalize(&mut function);

        assert_eq!(forward_exact_round_trips(&mut function, &cfg).unwrap(), 1);
        assert!(matches!(
            function.blocks[0].insts[5],
            MInst::Mov {
                dst: VReg(2),
                src: VReg(1),
            }
        ));
        assert!(matches!(
            function.blocks[0].insts[6],
            MInst::Load {
                dst: VReg(3),
                base: BaseReg::SimState,
                offset: 64,
                size: OpSize::S64,
            }
        ));
    }

    #[test]
    fn unknown_write_on_one_arm_does_not_blacklist_the_clean_arm() {
        let mut entry = MBlock::new(BlockId(0));
        entry.insts = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 1,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 9,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::Branch {
                cond: VReg(0),
                true_bb: BlockId(1),
                false_bb: BlockId(2),
            },
        ];
        let mut dirty = MBlock::new(BlockId(1));
        dirty.insts = vec![
            MInst::StoreIndexed {
                base: BaseReg::SimState,
                offset: 0,
                index: VReg(0),
                src: VReg(1),
                size: OpSize::S64,
                alias_range: None,
            },
            MInst::Jump { target: BlockId(3) },
        ];
        let mut clean = MBlock::new(BlockId(2));
        clean.insts = vec![
            MInst::Load {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Jump { target: BlockId(3) },
        ];
        let mut join = MBlock::new(BlockId(3));
        join.insts = vec![
            MInst::Load {
                dst: VReg(3),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut function = function(4, vec![entry, dirty, clean, join]);
        let cfg = normalize(&mut function);

        assert_eq!(forward_exact_round_trips(&mut function, &cfg).unwrap(), 1);
        let clean = cfg.block_index[&BlockId(2)];
        let join = cfg.block_index[&BlockId(3)];
        assert!(matches!(
            function.blocks[clean].insts[0],
            MInst::Mov {
                dst: VReg(2),
                src: VReg(1),
            }
        ));
        assert!(matches!(
            function.blocks[join].insts[0],
            MInst::Load {
                dst: VReg(3),
                base: BaseReg::SimState,
                offset: 16,
                size: OpSize::S64,
            }
        ));
    }

    #[test]
    fn unknown_kills_are_one_sparse_generation_not_cells_times_kills() {
        const CELLS: usize = 512;
        const KILLS: usize = 32;
        let mut instructions = vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 0,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 1,
            },
        ];
        for cell in 0..CELLS {
            instructions.push(MInst::Store {
                base: BaseReg::SimState,
                offset: i32::try_from(cell * 16).unwrap(),
                src: VReg(1),
                size: OpSize::S64,
            });
        }
        for _ in 0..KILLS {
            instructions.push(MInst::StoreIndexed {
                base: BaseReg::SimState,
                offset: 0,
                index: VReg(0),
                src: VReg(1),
                size: OpSize::S64,
                alias_range: None,
            });
        }
        for cell in 0..CELLS {
            instructions.push(MInst::Load {
                dst: VReg(u32::try_from(cell + 2).unwrap()),
                base: BaseReg::SimState,
                offset: i32::try_from(cell * 16).unwrap(),
                size: OpSize::S64,
            });
        }
        instructions.push(MInst::Return);
        let mut function = function(
            (CELLS + 2) as u32,
            vec![MBlock {
                id: BlockId(0),
                phis: Vec::new(),
                insts: instructions,
            }],
        );
        let cfg = normalize(&mut function);
        let plan = analyze(&function, &cfg).unwrap();

        assert_eq!(plan.cells.len(), CELLS);
        assert_eq!(plan.versions.len(), CELLS * 2);
        assert_eq!(plan.barriers.versions.len(), KILLS + 1);
        assert_eq!(plan.barriers.observed.len(), CELLS * 2);
    }
}
