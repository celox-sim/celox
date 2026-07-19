//! Sparse physical-word promotion for direct SimState accesses.
//!
//! Overlapping 8/16/32/64-bit accesses are partitioned into non-overlapping
//! machine cells. Stores define cell SSA versions; loads and partial stores
//! consume the reaching versions. Live-on-entry cells remain symbolic and are
//! loaded only at an actual use or phi edge, never eagerly at function entry.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;

use crate::backend::native::memory_effect::{self, UnknownMemory};
use crate::backend::native::mir::{
    BaseReg, BlockId, MFunction, MInst, OpSize, PackedStateHome, PhiNode, SpillDesc, StateHomeId,
    VReg,
};

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
    rejected: bool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Boundary {
    block: usize,
    cell: CellId,
    reaching: VersionId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PromotionPlan {
    cells: Vec<Cell>,
    versions: Vec<Version>,
    accesses: Vec<Access>,
    boundaries: Vec<Boundary>,
    phis_by_block: Vec<Vec<(CellId, VersionId)>>,
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

fn mark_overlapping_components(components: &mut [Component], start: i64, end: i64) {
    let mut index = components.partition_point(|component| component.end <= start);
    while let Some(component) = components.get_mut(index) {
        if component.start >= end {
            break;
        }
        component.rejected = true;
        index += 1;
    }
}

fn discover_components(
    func: &MFunction,
) -> Result<(Vec<Component>, Vec<RawAccess>), StatePromotionError> {
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
                rejected: false,
            });
        }
    }

    for mir_block in &func.blocks {
        for inst in &mir_block.insts {
            if direct_state_access(inst).is_some() {
                continue;
            }
            for effects in [memory_effect::reads(inst), memory_effect::writes(inst)] {
                if matches!(
                    effects.unknown_memory(),
                    Some(UnknownMemory::Direct(BaseReg::SimState))
                ) {
                    for component in &mut components {
                        component.rejected = true;
                    }
                }
                for range in effects
                    .ranges()
                    .filter(|range| range.base == BaseReg::SimState)
                {
                    let Some(end) = range.end() else {
                        for component in &mut components {
                            component.rejected = true;
                        }
                        continue;
                    };
                    mark_overlapping_components(&mut components, range.offset, end);
                }
            }
        }
    }
    Ok((components, raw))
}

fn build_cells(
    components: &[Component],
) -> Result<(Vec<Cell>, Vec<Vec<CellId>>), StatePromotionError> {
    let mut cells = Vec::new();
    let mut by_component = vec![Vec::new(); components.len()];
    for (component_index, component) in components.iter().enumerate() {
        if !component.has_store || component.rejected {
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
        if components[component].rejected || !components[component].has_store {
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

fn analyze(func: &MFunction, cfg: &NormalizedCfg) -> Result<PromotionPlan, StatePromotionError> {
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
    let (components, raw) = discover_components(func)?;
    let (cells, cells_by_component) = build_cells(&components)?;
    let mut accesses = map_accesses(&components, &raw, &cells, &cells_by_component)?;
    if cells.is_empty() {
        return Ok(PromotionPlan {
            cells,
            versions: Vec::new(),
            accesses,
            boundaries: Vec::new(),
            phis_by_block: vec![Vec::new(); func.blocks.len()],
        });
    }

    let mut accesses_by_block = vec![Vec::<usize>::new(); func.blocks.len()];
    let mut definitions = HashSet::<(CellId, usize)>::new();
    let mut upward_uses = HashSet::<(CellId, usize)>::new();
    for (index, access) in accesses.iter().enumerate() {
        accesses_by_block[access.block].push(index);
    }
    for (block, block_accesses) in accesses_by_block.iter().enumerate() {
        let mut defined = HashSet::<CellId>::new();
        for &access_index in block_accesses {
            let access = &accesses[access_index];
            match access.kind {
                RawAccessKind::Load { .. } => {
                    for part in &access.parts {
                        if !defined.contains(&part.cell) {
                            upward_uses.insert((part.cell, block));
                        }
                    }
                }
                RawAccessKind::Store { .. } => {
                    for part in &access.parts {
                        let cell = cells[part.cell.0];
                        if part.byte_width != cell.size.bytes() as usize
                            && !defined.contains(&part.cell)
                        {
                            upward_uses.insert((part.cell, block));
                        }
                        defined.insert(part.cell);
                        definitions.insert((part.cell, block));
                    }
                }
            }
        }
    }

    let mut dirty = definitions.clone();
    let mut dirty_work = definitions.iter().copied().collect::<VecDeque<_>>();
    while let Some((cell, block)) = dirty_work.pop_front() {
        for &successor in &cfg.successors[block] {
            if dirty.insert((cell, successor)) {
                dirty_work.push_back((cell, successor));
            }
        }
    }
    let mut boundary_pairs = dirty
        .into_iter()
        .filter(|(_, block)| cfg.successors[*block].is_empty())
        .collect::<Vec<_>>();
    boundary_pairs.sort_unstable_by_key(|(cell, block)| (*block, *cell));
    for &(cell, block) in &boundary_pairs {
        if !definitions.contains(&(cell, block)) {
            upward_uses.insert((cell, block));
        }
    }

    let mut live_in = upward_uses.clone();
    let mut live_work = upward_uses.iter().copied().collect::<VecDeque<_>>();
    while let Some((cell, block)) = live_work.pop_front() {
        for &predecessor in &cfg.predecessors[block] {
            let pair = (cell, predecessor);
            if !definitions.contains(&pair) && live_in.insert(pair) {
                live_work.push_back(pair);
            }
        }
    }

    let mut phi_pairs = HashSet::<(CellId, usize)>::new();
    let mut queued = definitions.clone();
    let mut phi_work = definitions.iter().copied().collect::<Vec<_>>();
    while let Some((cell, block)) = phi_work.pop() {
        for &frontier in &cfg.dominance_frontier[block] {
            let pair = (cell, frontier);
            if frontier == 0 || !live_in.contains(&pair) || !phi_pairs.insert(pair) {
                continue;
            }
            if queued.insert(pair) {
                phi_work.push(pair);
            }
        }
    }

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
        if !matches!(access.kind, RawAccessKind::Store { .. }) {
            continue;
        }
        for part in &mut access.parts {
            let id = VersionId(versions.len());
            versions.push(Version {
                id,
                cell: part.cell,
                kind: VersionKind::Store {
                    block: access.block,
                    instruction: access.instruction,
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

    let mut boundaries = boundary_pairs
        .into_iter()
        .map(|(cell, block)| Boundary {
            block,
            cell,
            reaching: VersionId(cell.0),
        })
        .collect::<Vec<_>>();
    // Reuse a second sparse dominator rename only for exact boundary versions.
    // Keep the lookup sparse as well: visiting every promoted cell in every
    // block would turn this pass into O(cells * blocks) on large RTL graphs.
    let mut boundary_indices_by_block = vec![Vec::<usize>::new(); func.blocks.len()];
    for (index, boundary) in boundaries.iter().enumerate() {
        boundary_indices_by_block[boundary.block].push(index);
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
            for part in &accesses[access_index].parts {
                if let Some(definition) = part.definition {
                    changes.push((part.cell, current[part.cell.0]));
                    current[part.cell.0] = definition;
                }
            }
        }
        for &boundary in &boundary_indices_by_block[block] {
            let cell = boundaries[boundary].cell;
            boundaries[boundary].reaching = current[cell.0];
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
    Ok(PromotionPlan {
        cells,
        versions,
        accesses,
        boundaries,
        phis_by_block,
    })
}

impl PromotionPlan {
    fn verify(&self, func: &MFunction, cfg: &NormalizedCfg) -> Result<(), StatePromotionError> {
        let rebuilt = analyze(func, cfg)?;
        if &rebuilt != self {
            return Err(StatePromotionError::new(
                "STATE_PROMOTE.PLAN_MATCH",
                None,
                None,
                "cached physical state SSA differs from an independent MIR rebuild",
            ));
        }
        Ok(())
    }
}

fn allocate_value(
    func: &mut MFunction,
    descriptor: SpillDesc,
) -> Result<VReg, StatePromotionError> {
    let value = func.vregs.alloc();
    if value.0 as usize != func.spill_descs.len() {
        return Err(StatePromotionError::new(
            "STATE_PROMOTE.VALUE_DOMAIN",
            None,
            None,
            "MIR VReg allocator and spill-descriptor table are not dense",
        ));
    }
    func.spill_descs.push(descriptor);
    Ok(value)
}

fn allocate_transient(func: &mut MFunction) -> Result<VReg, StatePromotionError> {
    allocate_value(func, SpillDesc::transient())
}

fn emit_mask(
    func: &mut MFunction,
    output: &mut Vec<MInst>,
    destination: VReg,
    source: VReg,
    mask: u64,
) -> Result<(), StatePromotionError> {
    if mask == u64::MAX {
        output.push(MInst::Mov {
            dst: destination,
            src: source,
        });
    } else if mask <= u64::from(u32::MAX) {
        output.push(MInst::AndImm32 {
            dst: destination,
            src: source,
            imm: mask as u32,
        });
    } else if (mask as i32 as i64 as u64) == mask {
        output.push(MInst::AndImm {
            dst: destination,
            src: source,
            imm: mask,
        });
    } else {
        let constant = allocate_value(func, SpillDesc::remat(mask))?;
        output.push(MInst::LoadImm {
            dst: constant,
            value: mask,
        });
        output.push(MInst::And {
            dst: destination,
            lhs: source,
            rhs: constant,
        });
    }
    Ok(())
}

fn emit_extract(
    func: &mut MFunction,
    output: &mut Vec<MInst>,
    destination: VReg,
    source: VReg,
    source_bit_offset: usize,
    width_bits: usize,
    destination_bit_offset: usize,
) -> Result<(), StatePromotionError> {
    if width_bits == 0
        || width_bits > 64
        || source_bit_offset >= 64
        || destination_bit_offset >= 64
        || destination_bit_offset + width_bits > 64
    {
        return Err(StatePromotionError::new(
            "STATE_PROMOTE.BIT_RANGE",
            None,
            None,
            "machine-cell extraction exceeds one 64-bit register",
        ));
    }
    let mut current = source;
    if source_bit_offset != 0 {
        let shifted = allocate_transient(func)?;
        output.push(MInst::ShrImm {
            dst: shifted,
            src: current,
            imm: source_bit_offset as u8,
        });
        current = shifted;
    }
    if width_bits < 64 {
        let masked = if destination_bit_offset == 0 {
            destination
        } else {
            allocate_transient(func)?
        };
        let mask = u64::MAX >> (64 - width_bits);
        emit_mask(func, output, masked, current, mask)?;
        current = masked;
    }
    if destination_bit_offset != 0 {
        output.push(MInst::ShlImm {
            dst: destination,
            src: current,
            imm: destination_bit_offset as u8,
        });
    } else if current != destination {
        output.push(MInst::Mov {
            dst: destination,
            src: current,
        });
    }
    Ok(())
}

fn version_operand(
    func: &mut MFunction,
    output: &mut Vec<MInst>,
    plan: &PromotionPlan,
    values: &[Option<VReg>],
    version: VersionId,
) -> Result<VReg, StatePromotionError> {
    let row = plan.versions.get(version.0).ok_or_else(|| {
        StatePromotionError::new(
            "STATE_PROMOTE.VERSION_RANGE",
            None,
            None,
            "state use references a missing version",
        )
    })?;
    if let Some(value) = values[version.0] {
        return Ok(value);
    }
    if !matches!(row.kind, VersionKind::LiveOnEntry) {
        return Err(StatePromotionError::new(
            "STATE_PROMOTE.VERSION_VALUE",
            None,
            None,
            "non-entry state version has no machine value",
        ));
    }
    let cell = plan.cells[row.cell.0];
    let value = allocate_transient(func)?;
    output.push(MInst::Load {
        dst: value,
        base: BaseReg::SimState,
        offset: cell.offset,
        size: cell.size,
    });
    Ok(value)
}

fn lower_load(
    func: &mut MFunction,
    output: &mut Vec<MInst>,
    plan: &PromotionPlan,
    values: &[Option<VReg>],
    access: &Access,
    destination: VReg,
) -> Result<(), StatePromotionError> {
    let mut pieces = Vec::with_capacity(access.parts.len());
    for part in &access.parts {
        let source = version_operand(func, output, plan, values, part.reaching)?;
        let piece = allocate_transient(func)?;
        emit_extract(
            func,
            output,
            piece,
            source,
            part.cell_byte_offset * 8,
            part.byte_width * 8,
            part.access_byte_offset * 8,
        )?;
        pieces.push(piece);
    }
    let mut current = pieces[0];
    for (index, piece) in pieces.iter().copied().enumerate().skip(1) {
        let combined = if index + 1 == pieces.len() {
            destination
        } else {
            allocate_transient(func)?
        };
        output.push(MInst::Or {
            dst: combined,
            lhs: current,
            rhs: piece,
        });
        current = combined;
    }
    if pieces.len() == 1 {
        output.push(MInst::Mov {
            dst: destination,
            src: current,
        });
    }
    Ok(())
}

fn lower_store(
    func: &mut MFunction,
    output: &mut Vec<MInst>,
    plan: &PromotionPlan,
    values: &[Option<VReg>],
    access: &Access,
    source: VReg,
) -> Result<(), StatePromotionError> {
    for part in &access.parts {
        let definition = part.definition.ok_or_else(|| {
            StatePromotionError::new(
                "STATE_PROMOTE.STORE_VERSION",
                None,
                None,
                "promoted store fragment has no SSA definition",
            )
        })?;
        let destination = values[definition.0].ok_or_else(|| {
            StatePromotionError::new(
                "STATE_PROMOTE.VERSION_VALUE",
                None,
                None,
                "store state version has no preallocated machine value",
            )
        })?;
        let cell = plan.cells[part.cell.0];
        let cell_bits = cell.size.bytes() as usize * 8;
        if part.cell_byte_offset == 0 && part.byte_width * 8 == cell_bits {
            emit_extract(
                func,
                output,
                destination,
                source,
                part.access_byte_offset * 8,
                cell_bits,
                0,
            )?;
            continue;
        }
        let old = version_operand(func, output, plan, values, part.reaching)?;
        let fragment_bits = part.byte_width * 8;
        let insert_shift = part.cell_byte_offset * 8;
        let fragment_mask = (u64::MAX >> (64 - fragment_bits)) << insert_shift;
        let cell_mask = if cell_bits == 64 {
            u64::MAX
        } else {
            u64::MAX >> (64 - cell_bits)
        };
        let cleared = allocate_transient(func)?;
        emit_mask(func, output, cleared, old, cell_mask & !fragment_mask)?;
        let inserted = allocate_transient(func)?;
        emit_extract(
            func,
            output,
            inserted,
            source,
            part.access_byte_offset * 8,
            fragment_bits,
            insert_shift,
        )?;
        output.push(MInst::Or {
            dst: destination,
            lhs: cleared,
            rhs: inserted,
        });
    }
    Ok(())
}

fn next_home_id(func: &MFunction) -> Result<u32, StatePromotionError> {
    func.spill_descs
        .iter()
        .filter_map(|descriptor| descriptor.deferred_state_home.map(|home| home.id.0))
        .max()
        .map_or(Ok(0), |maximum| {
            maximum.checked_add(1).ok_or_else(|| {
                StatePromotionError::new(
                    "STATE_PROMOTE.HOME_ID_RANGE",
                    None,
                    None,
                    "deferred state-home identity exceeds u32",
                )
            })
        })
}

fn apply(
    func: &mut MFunction,
    cfg: &NormalizedCfg,
    plan: &PromotionPlan,
) -> Result<(), StatePromotionError> {
    if plan.cells.is_empty() {
        return Ok(());
    }
    let mut home_id = next_home_id(func)?;
    let mut values = vec![None::<VReg>; plan.versions.len()];
    let mut phi_locations = HashMap::<VersionId, (usize, usize)>::new();
    for version in &plan.versions {
        if matches!(version.kind, VersionKind::LiveOnEntry) {
            continue;
        }
        let cell = plan.cells[version.cell.0];
        let home = PackedStateHome {
            id: StateHomeId(home_id),
            offset: cell.offset,
            size: cell.size,
            live_on_entry: false,
        };
        home_id = home_id.checked_add(1).ok_or_else(|| {
            StatePromotionError::new(
                "STATE_PROMOTE.HOME_ID_RANGE",
                None,
                None,
                "deferred state-home identity exceeds u32",
            )
        })?;
        let value = allocate_value(func, SpillDesc::transient().with_deferred_state_home(home))?;
        values[version.id.0] = Some(value);
        if let VersionKind::Phi { block, .. } = version.kind {
            let phi = func.blocks[block].phis.len();
            func.blocks[block].phis.push(PhiNode {
                dst: value,
                sources: Vec::with_capacity(cfg.predecessors[block].len()),
            });
            phi_locations.insert(version.id, (block, phi));
        }
    }

    let access_index = plan
        .accesses
        .iter()
        .enumerate()
        .map(|(index, access)| ((access.block, access.instruction), index))
        .collect::<HashMap<_, _>>();
    let mut boundaries_by_block = vec![Vec::<Boundary>::new(); func.blocks.len()];
    for &boundary in &plan.boundaries {
        boundaries_by_block[boundary.block].push(boundary);
    }
    for (block, block_boundaries) in boundaries_by_block.iter().enumerate() {
        let original = std::mem::take(&mut func.blocks[block].insts);
        let mut rewritten = Vec::with_capacity(original.len());
        for (instruction, inst) in original.into_iter().enumerate() {
            if inst.is_terminator() {
                for boundary in block_boundaries {
                    let source = values[boundary.reaching.0].ok_or_else(|| {
                        StatePromotionError::new(
                            "STATE_PROMOTE.BOUNDARY_ENTRY",
                            Some(func.blocks[block].id),
                            Some(instruction),
                            "dirty terminal state unexpectedly resolves to live-on-entry memory",
                        )
                    })?;
                    let cell = plan.cells[boundary.cell.0];
                    rewritten.push(MInst::Store {
                        base: BaseReg::SimState,
                        offset: cell.offset,
                        src: source,
                        size: cell.size,
                    });
                }
            }
            let Some(&access) = access_index.get(&(block, instruction)) else {
                rewritten.push(inst);
                continue;
            };
            let access = &plan.accesses[access];
            match access.kind {
                RawAccessKind::Load { destination } => {
                    lower_load(func, &mut rewritten, plan, &values, access, destination)?;
                }
                RawAccessKind::Store { source } => {
                    lower_store(func, &mut rewritten, plan, &values, access, source)?;
                }
            }
        }
        func.blocks[block].insts = rewritten;
    }

    let mut entry_edge_loads = HashMap::<(usize, CellId), VReg>::new();
    for version in &plan.versions {
        let VersionKind::Phi { block, incoming } = &version.kind else {
            continue;
        };
        let (phi_block, phi_index) = phi_locations[&version.id];
        debug_assert_eq!(phi_block, *block);
        let mut sources = Vec::with_capacity(incoming.len());
        for &(predecessor, source_version) in incoming {
            let source = if let Some(value) = values[source_version.0] {
                value
            } else {
                let cell = plan.versions[source_version.0].cell;
                if let Some(&value) = entry_edge_loads.get(&(predecessor, cell)) {
                    value
                } else {
                    let value = allocate_transient(func)?;
                    let physical = plan.cells[cell.0];
                    let load = MInst::Load {
                        dst: value,
                        base: BaseReg::SimState,
                        offset: physical.offset,
                        size: physical.size,
                    };
                    let instructions = &mut func.blocks[predecessor].insts;
                    let position = instructions
                        .iter()
                        .position(MInst::is_terminator)
                        .unwrap_or(instructions.len());
                    instructions.insert(position, load);
                    entry_edge_loads.insert((predecessor, cell), value);
                    value
                }
            };
            sources.push((func.blocks[predecessor].id, source));
        }
        func.blocks[*block].phis[phi_index].sources = sources;
    }
    Ok(())
}

pub(super) fn promote(
    func: &mut MFunction,
    cfg: &NormalizedCfg,
) -> Result<bool, StatePromotionError> {
    let plan = analyze(func, cfg)?;
    plan.verify(func, cfg)?;
    if plan.cells.is_empty() {
        return Ok(false);
    }
    let mut rewritten = func.clone();
    apply(&mut rewritten, cfg, &plan)?;
    rewritten.verify_result().map_err(|error| {
        StatePromotionError::new(
            "STATE_PROMOTE.MIR_VERIFY",
            None,
            None,
            format!("promoted MIR failed canonical verification: {error}"),
        )
    })?;
    *func = rewritten;
    Ok(true)
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
    fn overlapping_mixed_width_round_trips_become_one_lazy_cell_chain() {
        let mut block = MBlock::new(BlockId(0));
        block.insts = vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::AndImm {
                dst: VReg(1),
                src: VReg(0),
                imm: 0xff,
            },
            MInst::Store {
                base: BaseReg::SimState,
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
            MInst::OrImm {
                dst: VReg(3),
                src: VReg(2),
                imm: 0x40,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 7,
                src: VReg(3),
                size: OpSize::S8,
            },
            MInst::Return,
        ];
        let mut function = function(4, vec![block]);
        let cfg = normalize(&mut function);
        assert!(promote(&mut function, &cfg).unwrap());
        let direct_loads = function.blocks[0]
            .insts
            .iter()
            .filter(|inst| {
                matches!(
                    inst,
                    MInst::Load {
                        base: BaseReg::SimState,
                        ..
                    }
                )
            })
            .count();
        let direct_stores = function.blocks[0]
            .insts
            .iter()
            .filter(|inst| {
                matches!(
                    inst,
                    MInst::Store {
                        base: BaseReg::SimState,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(direct_loads, 1);
        assert_eq!(direct_stores, 1);
        assert!(
            function
                .spill_descs
                .iter()
                .filter_map(|descriptor| descriptor.deferred_state_home)
                .count()
                >= 2
        );
    }

    #[test]
    fn one_arm_store_builds_a_phi_and_loads_entry_state_only_on_the_clean_edge() {
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
            MInst::Store {
                base: BaseReg::SimState,
                offset: 32,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Return,
        ];
        let mut function = function(3, vec![entry, dirty, clean, join]);
        let cfg = normalize(&mut function);
        assert!(promote(&mut function, &cfg).unwrap());
        let join = cfg.block_index[&BlockId(3)];
        let clean = cfg.block_index[&BlockId(2)];
        let entry = cfg.block_index[&BlockId(0)];
        assert_eq!(function.blocks[join].phis.len(), 1);
        assert!(function.blocks[clean].insts.iter().any(|inst| {
            matches!(
                inst,
                MInst::Load {
                    base: BaseReg::SimState,
                    offset: 16,
                    size: OpSize::S64,
                    ..
                }
            )
        }));
        assert!(!function.blocks[entry].insts.iter().any(|inst| {
            matches!(
                inst,
                MInst::Load {
                    base: BaseReg::SimState,
                    offset: 16,
                    ..
                }
            )
        }));
    }

    #[test]
    fn indexed_alias_envelope_rejects_only_the_intersecting_component() {
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
        assert!(plan.cells.iter().all(|cell| cell.offset != 64));
    }
}
