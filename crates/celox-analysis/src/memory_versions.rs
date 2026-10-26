//! Per-byte-range memory versioning: minimal SSA over elementary byte ranges.
//!
//! [`crate::memory_ssa`] builds one SSA graph for the whole memory state and
//! answers an alias query by walking that graph, folding every `MemoryPhi`
//! whose inputs agree for the query.  Folding is not well defined on cycles:
//! a phi whose inputs are all still on the walk stack has nothing to fold to,
//! so the walker keeps it as a version of its own, and which phis end up in
//! that situation depends on where the walk started.  Two walks over the same
//! memory state then produce different certificates.
//!
//! This module constructs the standard minimal SSA form of every elementary
//! byte range ("atom": a maximal range that no write boundary splits)
//! directly.  A phi for an atom is placed at a block exactly when the block is
//! in the iterated dominance frontier of the atom's writes, with live-on-entry
//! as the implicit definition at the root.  Such a phi always merges two
//! different reaching versions, on any CFG including irreducible ones, so the
//! version of every byte at every program point is a canonical function of the
//! program alone.
//!
//! Construction stores `O((W + F) log A)` tree nodes, where `W` is the number
//! of exact write ranges, `F` the number of placed phi ranges and `A` the
//! number of atoms, plus two version handles per event.  Reading the versions
//! of a byte range costs `O(k log A)` for `k` resulting segments.

use std::collections::BTreeMap;
use std::fmt;

use crate::ssa::SsaCfg;

/// One byte range written by a memory definition.  `Exact` ranges are
/// half-open and non-empty; `All` covers every byte of the object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RangeWrite {
    Exact { start: i64, end: i64 },
    All,
}

/// One ordered program event.  An event with a definition writes the ranges
/// `writes[write_start..write_end]` of the caller's write table; an event
/// without one only records a queryable program point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryVersionEvent<D> {
    pub definition: Option<D>,
    pub write_start: usize,
    pub write_end: usize,
}

/// Handle of one memory state.  Handles are only meaningful within the
/// [`MemoryVersions`] that produced them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VersionId(u32);

/// Reaching version of one byte range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemoryVersion<D> {
    LiveOnEntry,
    Definition(D),
    Phi { block: usize },
}

/// A maximal run of bytes with one reaching version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VersionSegment<D> {
    pub start: i64,
    pub end: i64,
    pub version: MemoryVersion<D>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryVersionsError {
    pub rule: &'static str,
    pub block: Option<usize>,
    pub message: String,
}

impl MemoryVersionsError {
    fn new(rule: &'static str, block: Option<usize>, message: impl Into<String>) -> Self {
        Self {
            rule,
            block,
            message: message.into(),
        }
    }
}

impl fmt::Display for MemoryVersionsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.rule)?;
        if let Some(block) = self.block {
            write!(formatter, " at block {block}")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for MemoryVersionsError {}

const MIXED: u32 = u32::MAX;
const UNVISITED: u32 = u32::MAX;

/// Node of the persistent range tree over atoms.  A node with a concrete
/// `value` is uniform over its whole range and ignores its children; a
/// `MIXED` node splits its range at the midpoint.
#[derive(Debug, Clone, Copy)]
struct Node {
    value: u32,
    left: u32,
    right: u32,
}

/// Per-atom minimal SSA of one function.
#[derive(Debug)]
pub struct MemoryVersions<D> {
    /// Sorted distinct write boundaries.  Atom `i` spans
    /// `[boundaries[i - 1], boundaries[i])`, with the first and last atoms
    /// unbounded.
    boundaries: Vec<i64>,
    nodes: Vec<Node>,
    definitions: Vec<D>,
    predecessors: Vec<Vec<usize>>,
    events: Vec<Vec<(VersionId, VersionId)>>,
    block_entries: Vec<VersionId>,
    block_exits: Vec<VersionId>,
    /// Sorted disjoint atom ranges with a phi, per block.
    phi_ranges: Vec<Vec<(u32, u32)>>,
    phi_range_count: usize,
}

impl<D: Copy> MemoryVersions<D> {
    /// Memory states before and after event `index` of `block`.
    #[must_use]
    pub fn event(&self, block: usize, index: usize) -> Option<(VersionId, VersionId)> {
        self.events.get(block)?.get(index).copied()
    }

    /// Memory state at the entry of `block`, after its phis.
    #[must_use]
    pub fn block_entry(&self, block: usize) -> Option<VersionId> {
        self.block_entries.get(block).copied()
    }

    #[must_use]
    pub fn block_exit(&self, block: usize) -> Option<VersionId> {
        self.block_exits.get(block).copied()
    }

    /// CFG predecessors in the order phi inputs are reported.
    #[must_use]
    pub fn predecessors(&self, block: usize) -> &[usize] {
        self.predecessors.get(block).map_or(&[], Vec::as_slice)
    }

    #[must_use]
    pub fn definition_count(&self) -> usize {
        self.definitions.len()
    }

    /// Number of placed `(block, atom range)` phis.
    #[must_use]
    pub fn phi_range_count(&self) -> usize {
        self.phi_range_count
    }

    /// Byte ranges of `block` whose version at its entry is `Phi { block }`.
    #[must_use]
    pub fn phi_ranges(&self, block: usize) -> Vec<(i64, i64)> {
        self.phi_ranges.get(block).map_or_else(Vec::new, |ranges| {
            ranges
                .iter()
                .map(|&(lo, hi)| (self.atom_start(lo), self.atom_end(hi - 1)))
                .collect()
        })
    }

    /// Append the maximal runs of equal versions of bytes `start..end` in
    /// `version`, in address order, clipped to that range.  Returns `None`
    /// when `version` is not a handle of this analysis.
    pub fn segments(
        &self,
        version: VersionId,
        start: i64,
        end: i64,
        out: &mut Vec<VersionSegment<D>>,
    ) -> Option<()> {
        if version.0 as usize >= self.nodes.len() {
            return None;
        }
        if start >= end {
            return Some(());
        }
        let (lo, hi) = self.atom_range(start, end);
        let mut runs = Vec::new();
        self.read(version.0, 0, self.atom_count(), lo, hi, &mut runs);
        for (atom_lo, atom_hi, value) in runs {
            out.push(VersionSegment {
                start: self.atom_start(atom_lo).max(start),
                end: self.atom_end(atom_hi - 1).min(end),
                version: self.decode(value),
            });
        }
        Some(())
    }

    /// Reaching version of one byte.
    #[must_use]
    pub fn version_at(&self, version: VersionId, byte: i64) -> Option<MemoryVersion<D>> {
        let mut segments = Vec::with_capacity(1);
        self.segments(version, byte, byte.checked_add(1)?, &mut segments)?;
        segments.first().map(|segment| segment.version)
    }

    fn decode(&self, value: u32) -> MemoryVersion<D> {
        if value == 0 {
            MemoryVersion::LiveOnEntry
        } else if let Some(definition) = self.definitions.get(value as usize - 1) {
            MemoryVersion::Definition(*definition)
        } else {
            MemoryVersion::Phi {
                block: value as usize - 1 - self.definitions.len(),
            }
        }
    }

    fn atom_count(&self) -> u32 {
        self.boundaries.len() as u32 + 1
    }

    fn atom_of(&self, byte: i64) -> u32 {
        self.boundaries
            .partition_point(|&boundary| boundary <= byte) as u32
    }

    /// Atoms covered by the non-empty byte range `start..end`.
    fn atom_range(&self, start: i64, end: i64) -> (u32, u32) {
        (self.atom_of(start), self.atom_of(end - 1) + 1)
    }

    fn atom_start(&self, atom: u32) -> i64 {
        if atom == 0 {
            i64::MIN
        } else {
            self.boundaries[atom as usize - 1]
        }
    }

    fn atom_end(&self, atom: u32) -> i64 {
        self.boundaries
            .get(atom as usize)
            .copied()
            .unwrap_or(i64::MAX)
    }

    fn read(
        &self,
        node: u32,
        node_lo: u32,
        node_hi: u32,
        lo: u32,
        hi: u32,
        out: &mut Vec<(u32, u32, u32)>,
    ) {
        let current = self.nodes[node as usize];
        if current.value != MIXED {
            let start = lo.max(node_lo);
            let end = hi.min(node_hi);
            match out.last_mut() {
                Some((_, last_end, value)) if *last_end == start && *value == current.value => {
                    *last_end = end;
                }
                _ => out.push((start, end, current.value)),
            }
            return;
        }
        let mid = node_lo + (node_hi - node_lo) / 2;
        if lo < mid {
            self.read(current.left, node_lo, mid, lo, hi, out);
        }
        if hi > mid {
            self.read(current.right, mid, node_hi, lo, hi, out);
        }
    }
}

struct Builder {
    nodes: Vec<Node>,
    atom_count: u32,
}

impl Builder {
    fn uniform(&mut self, value: u32) -> Result<u32, MemoryVersionsError> {
        let id = u32::try_from(self.nodes.len())
            .ok()
            .filter(|&id| id != MIXED)
            .ok_or_else(|| {
                MemoryVersionsError::new(
                    "MEMORY_VERSIONS.NODE_RANGE",
                    None,
                    "persistent version tree exceeds u32 nodes",
                )
            })?;
        self.nodes.push(Node {
            value,
            left: 0,
            right: 0,
        });
        Ok(id)
    }

    /// Path-copying range assignment of `value` over atoms `lo..hi`.
    fn assign(
        &mut self,
        node: u32,
        node_lo: u32,
        node_hi: u32,
        lo: u32,
        hi: u32,
        value: u32,
    ) -> Result<u32, MemoryVersionsError> {
        let current = self.nodes[node as usize];
        if current.value == value {
            return Ok(node);
        }
        if lo <= node_lo && node_hi <= hi {
            return self.uniform(value);
        }
        // A uniform node stands for its own halves: bounds are implicit, so
        // the same node can describe any sub-range without copying.
        let (left, right) = if current.value == MIXED {
            (current.left, current.right)
        } else {
            (node, node)
        };
        let mid = node_lo + (node_hi - node_lo) / 2;
        let left = if lo < mid {
            self.assign(left, node_lo, mid, lo, hi, value)?
        } else {
            left
        };
        let right = if hi > mid {
            self.assign(right, mid, node_hi, lo, hi, value)?
        } else {
            right
        };
        let left_value = self.nodes[left as usize].value;
        if left_value != MIXED && left_value == self.nodes[right as usize].value {
            return Ok(left);
        }
        let id = self.uniform(MIXED)?;
        self.nodes[id as usize] = Node {
            value: MIXED,
            left,
            right,
        };
        Ok(id)
    }
}

/// Insert `lo..hi` into a set of disjoint sorted ranges, appending the parts
/// that were not yet covered to `added`.
fn insert_range(set: &mut BTreeMap<u32, u32>, lo: u32, hi: u32, added: &mut Vec<(u32, u32)>) {
    let mut overlapping = set
        .range(..=hi)
        .rev()
        .take_while(|&(_, &end)| end >= lo)
        .map(|(&start, &end)| (start, end))
        .collect::<Vec<_>>();
    overlapping.reverse();
    let mut cursor = lo;
    let mut new_lo = lo;
    let mut new_hi = hi;
    for (start, end) in overlapping {
        if start > cursor {
            added.push((cursor, start));
        }
        cursor = cursor.max(end);
        new_lo = new_lo.min(start);
        new_hi = new_hi.max(end);
        set.remove(&start);
    }
    if cursor < hi {
        added.push((cursor, hi));
    }
    set.insert(new_lo, new_hi);
}

/// Build the per-atom minimal SSA of `events` over `cfg`.
pub fn build<D: Copy>(
    cfg: &impl SsaCfg,
    events: &[Vec<MemoryVersionEvent<D>>],
    writes: &[RangeWrite],
) -> Result<MemoryVersions<D>, MemoryVersionsError> {
    let blocks = cfg.successors().len();
    if events.len() != blocks
        || cfg.predecessors().len() != blocks
        || cfg.dominator_children().len() != blocks
        || cfg.dominance_frontier_len() != blocks
    {
        return Err(MemoryVersionsError::new(
            "MEMORY_VERSIONS.MODEL_SHAPE",
            None,
            "event and CFG block tables have different lengths",
        ));
    }
    if cfg.root() >= blocks {
        return Err(MemoryVersionsError::new(
            "MEMORY_VERSIONS.ROOT",
            None,
            "CFG root is outside the block table",
        ));
    }

    let mut boundaries = Vec::new();
    for write in writes {
        if let RangeWrite::Exact { start, end } = *write {
            if start >= end {
                return Err(MemoryVersionsError::new(
                    "MEMORY_VERSIONS.WRITE_RANGE",
                    None,
                    "exact write range is empty or reversed",
                ));
            }
            boundaries.push(start);
            boundaries.push(end);
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let atom_count = u32::try_from(boundaries.len() + 1).map_err(|_| {
        MemoryVersionsError::new(
            "MEMORY_VERSIONS.ATOM_RANGE",
            None,
            "write boundary count exceeds u32 atoms",
        )
    })?;
    let atom_of = |byte: i64| boundaries.partition_point(|&boundary| boundary <= byte) as u32;
    let write_atoms = |write: RangeWrite| match write {
        RangeWrite::Exact { start, end } => (atom_of(start), atom_of(end - 1) + 1),
        RangeWrite::All => (0, atom_count),
    };

    // Definitions in program order; a definition's version id is its index
    // plus one, phi versions follow after every definition.
    let mut definitions = Vec::<D>::new();
    let mut written = vec![Vec::<(u32, u32)>::new(); blocks];
    for (block, block_events) in events.iter().enumerate() {
        for event in block_events {
            if event.write_start > event.write_end || event.write_end > writes.len() {
                return Err(MemoryVersionsError::new(
                    "MEMORY_VERSIONS.WRITE_TABLE",
                    Some(block),
                    "event write slice is outside the write table",
                ));
            }
            let has_writes = event.write_end > event.write_start;
            if event.definition.is_some() != has_writes {
                return Err(MemoryVersionsError::new(
                    "MEMORY_VERSIONS.DEFINITION_WRITES",
                    Some(block),
                    "a memory definition writes at least one range and a plain point none",
                ));
            }
            if let Some(definition) = event.definition {
                definitions.push(definition);
                written[block].extend(
                    writes[event.write_start..event.write_end]
                        .iter()
                        .map(|&write| write_atoms(write)),
                );
            }
        }
    }
    let phi_base = u32::try_from(definitions.len() + 1).map_err(|_| {
        MemoryVersionsError::new(
            "MEMORY_VERSIONS.DEFINITION_RANGE",
            None,
            "definition count exceeds u32 versions",
        )
    })?;
    if (phi_base as usize)
        .checked_add(blocks)
        .is_none_or(|limit| limit >= MIXED as usize)
    {
        return Err(MemoryVersionsError::new(
            "MEMORY_VERSIONS.VERSION_RANGE",
            None,
            "definition and block count exceed u32 versions",
        ));
    }

    // Phi placement: the iterated dominance frontier of every written atom
    // range.  Each (block, atom) pair is inserted at most once.
    let mut phi_sets = vec![BTreeMap::<u32, u32>::new(); blocks];
    let mut worklist = Vec::new();
    for (block, ranges) in written.iter_mut().enumerate() {
        ranges.sort_unstable();
        let mut merged = Vec::<(u32, u32)>::with_capacity(ranges.len());
        for &(lo, hi) in ranges.iter() {
            match merged.last_mut() {
                Some((_, last_hi)) if *last_hi >= lo => *last_hi = (*last_hi).max(hi),
                _ => merged.push((lo, hi)),
            }
        }
        worklist.extend(merged.iter().map(|&(lo, hi)| (block, lo, hi)));
        *ranges = merged;
    }
    let mut added = Vec::new();
    while let Some((block, lo, hi)) = worklist.pop() {
        for frontier in cfg.dominance_frontier(block) {
            if frontier >= blocks {
                return Err(MemoryVersionsError::new(
                    "MEMORY_VERSIONS.FRONTIER_RANGE",
                    Some(block),
                    "dominance frontier names a block outside the function",
                ));
            }
            added.clear();
            insert_range(&mut phi_sets[frontier], lo, hi, &mut added);
            worklist.extend(added.iter().map(|&(lo, hi)| (frontier, lo, hi)));
        }
    }
    if !phi_sets[cfg.root()].is_empty() {
        return Err(MemoryVersionsError::new(
            "MEMORY_VERSIONS.ROOT_PHI",
            Some(cfg.root()),
            "the root block has incoming edges and would need a phi without its live-on-entry input",
        ));
    }
    let phi_ranges = phi_sets
        .into_iter()
        .map(|set| set.into_iter().collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let phi_range_count = phi_ranges.iter().map(Vec::len).sum();

    // Renaming over the dominator tree.  Every block starts from the state at
    // its immediate dominator's exit; path copying makes that sharing free.
    let mut builder = Builder {
        nodes: Vec::new(),
        atom_count,
    };
    let live_on_entry = builder.uniform(0)?;
    let mut block_entries = vec![UNVISITED; blocks];
    let mut block_exits = vec![UNVISITED; blocks];
    let mut event_versions = events
        .iter()
        .map(|block_events| Vec::with_capacity(block_events.len()))
        .collect::<Vec<_>>();
    let mut definition_index = vec![0u32; blocks];
    let mut next_definition = 0u32;
    for (block, block_events) in events.iter().enumerate() {
        definition_index[block] = next_definition;
        next_definition += block_events
            .iter()
            .filter(|event| event.definition.is_some())
            .count() as u32;
    }
    let mut stack = vec![(cfg.root(), live_on_entry)];
    while let Some((block, inherited)) = stack.pop() {
        if block_entries[block] != UNVISITED {
            return Err(MemoryVersionsError::new(
                "MEMORY_VERSIONS.DOMINATOR_TREE",
                Some(block),
                "dominator-tree children contain a cycle",
            ));
        }
        let mut root = inherited;
        for &(lo, hi) in &phi_ranges[block] {
            root = builder.assign(root, 0, builder.atom_count, lo, hi, phi_base + block as u32)?;
        }
        block_entries[block] = root;
        let mut definition = definition_index[block];
        for event in &events[block] {
            let before = root;
            if event.definition.is_some() {
                let value = definition + 1;
                definition += 1;
                for &write in &writes[event.write_start..event.write_end] {
                    let (lo, hi) = write_atoms(write);
                    root = builder.assign(root, 0, builder.atom_count, lo, hi, value)?;
                }
            }
            event_versions[block].push((VersionId(before), VersionId(root)));
        }
        block_exits[block] = root;
        for &child in cfg.dominator_children()[block].iter().rev() {
            if child >= blocks {
                return Err(MemoryVersionsError::new(
                    "MEMORY_VERSIONS.DOMINATOR_CHILD_RANGE",
                    Some(block),
                    "dominator-tree child is outside the function",
                ));
            }
            stack.push((child, root));
        }
    }
    if let Some(block) = block_entries.iter().position(|&entry| entry == UNVISITED) {
        return Err(MemoryVersionsError::new(
            "MEMORY_VERSIONS.DOMINATOR_COVERAGE",
            Some(block),
            "dominator tree does not reach every block",
        ));
    }

    Ok(MemoryVersions {
        boundaries,
        nodes: builder.nodes,
        definitions,
        predecessors: cfg.predecessors().to_vec(),
        events: event_versions,
        block_entries: block_entries.into_iter().map(VersionId).collect(),
        block_exits: block_exits.into_iter().map(VersionId).collect(),
        phi_ranges,
        phi_range_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfg::ControlFlowGraph;
    use crate::memory::{MemoryEffect, MemoryLocation, effects_may_alias};
    use crate::memory_ssa::{self, ClobberWalker, MemoryAccess, MemoryAccessEvent, MemoryClobber};
    use crate::ssa::{self, Event, Version};

    /// `(block, event index)` identity of a definition.
    type Definition = (usize, usize);

    struct Program {
        events: Vec<Vec<MemoryVersionEvent<Definition>>>,
        writes: Vec<RangeWrite>,
    }

    impl Program {
        fn new(blocks: usize) -> Self {
            Self {
                events: vec![Vec::new(); blocks],
                writes: Vec::new(),
            }
        }

        fn point(&mut self, block: usize) -> usize {
            let index = self.events[block].len();
            self.events[block].push(MemoryVersionEvent {
                definition: None,
                write_start: self.writes.len(),
                write_end: self.writes.len(),
            });
            index
        }

        fn write(&mut self, block: usize, ranges: &[RangeWrite]) -> usize {
            let index = self.events[block].len();
            let write_start = self.writes.len();
            self.writes.extend_from_slice(ranges);
            self.events[block].push(MemoryVersionEvent {
                definition: Some((block, index)),
                write_start,
                write_end: self.writes.len(),
            });
            index
        }

        fn build(&self, cfg: &ControlFlowGraph) -> MemoryVersions<Definition> {
            build(cfg, &self.events, &self.writes).unwrap()
        }
    }

    fn exact(start: i64, end: i64) -> RangeWrite {
        RangeWrite::Exact { start, end }
    }

    fn versions_over(
        analysis: &MemoryVersions<Definition>,
        version: VersionId,
        start: i64,
        end: i64,
    ) -> Vec<(i64, i64, MemoryVersion<Definition>)> {
        let mut segments = Vec::new();
        analysis
            .segments(version, start, end, &mut segments)
            .unwrap();
        segments
            .into_iter()
            .map(|segment| (segment.start, segment.end, segment.version))
            .collect()
    }

    #[test]
    fn straight_line_writes_version_exactly_their_bytes() {
        let cfg = ControlFlowGraph::analyze(vec![vec![]], 0).unwrap();
        let mut program = Program::new(1);
        program.write(0, &[exact(8, 16)]);
        program.write(0, &[exact(12, 14), exact(100, 101)]);
        let point = program.point(0);
        let analysis = program.build(&cfg);

        let (before, after) = analysis.event(0, point).unwrap();
        assert_eq!(before, after);
        assert_eq!(
            versions_over(&analysis, after, 0, 20),
            vec![
                (0, 8, MemoryVersion::LiveOnEntry),
                (8, 12, MemoryVersion::Definition((0, 0))),
                (12, 14, MemoryVersion::Definition((0, 1))),
                (14, 16, MemoryVersion::Definition((0, 0))),
                (16, 20, MemoryVersion::LiveOnEntry),
            ]
        );
        assert_eq!(
            analysis.version_at(after, 100),
            Some(MemoryVersion::Definition((0, 1)))
        );
        assert_eq!(analysis.phi_range_count(), 0);
        assert_eq!(analysis.definition_count(), 2);
    }

    #[test]
    fn unknown_write_covers_every_byte() {
        let cfg = ControlFlowGraph::analyze(vec![vec![]], 0).unwrap();
        let mut program = Program::new(1);
        program.write(0, &[exact(8, 16)]);
        program.write(0, &[RangeWrite::All]);
        let analysis = program.build(&cfg);
        assert_eq!(
            versions_over(
                &analysis,
                analysis.block_exit(0).unwrap(),
                i64::MIN,
                i64::MAX
            ),
            vec![(i64::MIN, i64::MAX, MemoryVersion::Definition((0, 1)))]
        );
    }

    #[test]
    fn join_phi_exists_only_where_predecessors_disagree() {
        let cfg = ControlFlowGraph::analyze(vec![vec![1, 2], vec![3], vec![3], vec![]], 0).unwrap();
        let mut program = Program::new(4);
        program.write(0, &[exact(0, 8)]);
        program.write(1, &[exact(4, 12)]);
        let analysis = program.build(&cfg);

        assert_eq!(analysis.phi_ranges(3), vec![(4, 12)]);
        assert_eq!(
            versions_over(&analysis, analysis.block_entry(3).unwrap(), 0, 16),
            vec![
                (0, 4, MemoryVersion::Definition((0, 0))),
                (4, 12, MemoryVersion::Phi { block: 3 }),
                (12, 16, MemoryVersion::LiveOnEntry),
            ]
        );
        assert_eq!(
            versions_over(&analysis, analysis.block_exit(1).unwrap(), 4, 12),
            vec![(4, 12, MemoryVersion::Definition((1, 0)))]
        );
        assert_eq!(
            versions_over(&analysis, analysis.block_exit(2).unwrap(), 4, 12),
            vec![
                (4, 8, MemoryVersion::Definition((0, 0))),
                (8, 12, MemoryVersion::LiveOnEntry),
            ]
        );
    }

    #[test]
    fn loop_header_phi_exists_only_for_ranges_written_in_the_loop() {
        let cfg = ControlFlowGraph::analyze(vec![vec![1], vec![1, 2], vec![]], 0).unwrap();
        let mut program = Program::new(3);
        program.write(0, &[exact(0, 16)]);
        program.write(1, &[exact(8, 12)]);
        let analysis = program.build(&cfg);

        assert_eq!(analysis.phi_ranges(1), vec![(8, 12)]);
        assert_eq!(
            versions_over(&analysis, analysis.block_entry(1).unwrap(), 0, 16),
            vec![
                (0, 8, MemoryVersion::Definition((0, 0))),
                (8, 12, MemoryVersion::Phi { block: 1 }),
                (12, 16, MemoryVersion::Definition((0, 0))),
            ]
        );
        assert_eq!(
            analysis.version_at(analysis.block_entry(2).unwrap(), 8),
            Some(MemoryVersion::Definition((1, 0)))
        );
    }

    /// The shape behind celox-sim/celox#2041: a join inside a loop whose
    /// write does not touch the queried range.  No version of that range is
    /// introduced anywhere in the loop.
    #[test]
    fn disjoint_write_in_a_loop_diamond_introduces_no_version() {
        let cfg = ControlFlowGraph::analyze(
            vec![vec![1], vec![2, 3], vec![4], vec![4], vec![1, 5], vec![]],
            0,
        )
        .unwrap();
        let mut program = Program::new(6);
        program.write(2, &[exact(64, 72)]);
        let analysis = program.build(&cfg);

        assert_eq!(analysis.phi_ranges(1), vec![(64, 72)]);
        assert_eq!(analysis.phi_ranges(4), vec![(64, 72)]);
        for block in 0..6 {
            assert_eq!(
                versions_over(&analysis, analysis.block_entry(block).unwrap(), 0, 8),
                vec![(0, 8, MemoryVersion::LiveOnEntry)],
                "block {block}"
            );
        }
        assert_eq!(
            versions_over(&analysis, analysis.block_entry(4).unwrap(), 64, 72),
            vec![(64, 72, MemoryVersion::Phi { block: 4 })]
        );
        assert_eq!(
            versions_over(&analysis, analysis.block_exit(3).unwrap(), 64, 72),
            vec![(64, 72, MemoryVersion::Phi { block: 1 })]
        );
    }

    #[test]
    fn irreducible_cycle_receives_phis_at_both_entries() {
        let cfg =
            ControlFlowGraph::analyze(vec![vec![1, 2], vec![2], vec![1, 3], vec![]], 0).unwrap();
        let mut program = Program::new(4);
        program.write(1, &[exact(0, 4)]);
        let analysis = program.build(&cfg);

        assert_eq!(analysis.phi_ranges(1), vec![(0, 4)]);
        assert_eq!(analysis.phi_ranges(2), vec![(0, 4)]);
        assert_eq!(
            analysis.version_at(analysis.block_exit(1).unwrap(), 0),
            Some(MemoryVersion::Definition((1, 0)))
        );
        assert_eq!(
            analysis.version_at(analysis.block_entry(3).unwrap(), 0),
            Some(MemoryVersion::Phi { block: 2 })
        );
    }

    #[test]
    fn root_with_incoming_edges_and_writes_is_rejected() {
        let cfg = ControlFlowGraph::analyze(vec![vec![1], vec![0, 2], vec![]], 0).unwrap();
        let mut program = Program::new(3);
        program.write(1, &[exact(0, 4)]);
        let error = build(&cfg, &program.events, &program.writes).unwrap_err();
        assert_eq!(error.rule, "MEMORY_VERSIONS.ROOT_PHI");
    }

    struct Xorshift(u64);

    impl Xorshift {
        fn next(&mut self, bound: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % bound as u64) as usize
        }
    }

    fn random_program(random: &mut Xorshift, blocks: usize, allow_unknown: bool) -> Program {
        let mut program = Program::new(blocks);
        for block in 0..blocks {
            for _ in 0..random.next(4) {
                match random.next(4) {
                    0 => {
                        program.point(block);
                    }
                    1 if allow_unknown && random.next(6) == 0 => {
                        program.write(block, &[RangeWrite::All]);
                    }
                    _ => {
                        let ranges = (0..1 + random.next(2))
                            .map(|_| {
                                let start = 4 * random.next(4) as i64;
                                exact(start, start + 4 * (1 + random.next(2)) as i64)
                            })
                            .collect::<Vec<_>>();
                        program.write(block, &ranges);
                    }
                }
            }
        }
        program
    }

    /// Random reachable CFGs without edges into the root.  Successor choice is
    /// unrestricted, so the graphs contain loops, nested loops and irreducible
    /// cycles.
    fn random_cfg(random: &mut Xorshift, acyclic: bool) -> Option<(usize, ControlFlowGraph)> {
        let blocks = 1 + random.next(7);
        let successors = (0..blocks)
            .map(|block| {
                let mut outgoing = Vec::new();
                if block + 1 < blocks {
                    outgoing.push(block + 1);
                }
                for _ in 0..random.next(3) {
                    let target = if acyclic {
                        if block + 1 >= blocks {
                            continue;
                        }
                        block + 1 + random.next(blocks - block - 1)
                    } else {
                        1 + random.next(blocks.max(2) - 1)
                    };
                    if target < blocks {
                        outgoing.push(target);
                    }
                }
                outgoing
            })
            .collect::<Vec<_>>();
        ControlFlowGraph::analyze(successors, 0)
            .ok()
            .map(|cfg| (blocks, cfg))
    }

    /// Per-atom oracle: one `ssa::build` variable per atom, with uses at every
    /// block boundary and event.
    #[test]
    fn random_cfgs_match_one_sparse_ssa_per_atom() {
        let mut random = Xorshift(0x9e37_79b9_7f4a_7c15);
        let mut cases = 0;
        let mut phi_cases = 0;
        for _ in 0..600 {
            let Some((blocks, cfg)) = random_cfg(&mut random, false) else {
                continue;
            };
            let program = random_program(&mut random, blocks, true);
            let analysis = program.build(&cfg);

            let mut boundaries = program
                .writes
                .iter()
                .filter_map(|write| match *write {
                    RangeWrite::Exact { start, end } => Some([start, end]),
                    RangeWrite::All => None,
                })
                .flatten()
                .collect::<Vec<_>>();
            boundaries.sort_unstable();
            boundaries.dedup();
            let atoms = boundaries.len() + 1;
            let atom_byte = |atom: usize| {
                if atom == 0 {
                    boundaries.first().map_or(0, |first| first - 1)
                } else {
                    boundaries[atom - 1]
                }
            };
            let covers = |write: RangeWrite, byte: i64| match write {
                RangeWrite::Exact { start, end } => start <= byte && byte < end,
                RangeWrite::All => true,
            };

            // One SSA variable per atom; use identities are
            // (block, slot) with slot 0 = entry, 2k+1/2k+2 = before/after
            // event k, last = exit.
            let mut events = vec![Vec::<Event<usize, Definition, (usize, usize)>>::new(); blocks];
            for (block, block_events) in program.events.iter().enumerate() {
                for atom in 0..atoms {
                    events[block].push(Event::Use {
                        variable: atom,
                        usage: (block, 0),
                    });
                }
                for (index, event) in block_events.iter().enumerate() {
                    for atom in 0..atoms {
                        events[block].push(Event::Use {
                            variable: atom,
                            usage: (block, 2 * index + 1),
                        });
                    }
                    if let Some(definition) = event.definition {
                        for atom in 0..atoms {
                            if program.writes[event.write_start..event.write_end]
                                .iter()
                                .any(|&write| covers(write, atom_byte(atom)))
                            {
                                events[block].push(Event::Definition {
                                    variable: atom,
                                    definition,
                                });
                            }
                        }
                    }
                    for atom in 0..atoms {
                        events[block].push(Event::Use {
                            variable: atom,
                            usage: (block, 2 * index + 2),
                        });
                    }
                }
                for atom in 0..atoms {
                    events[block].push(Event::Use {
                        variable: atom,
                        usage: (block, 2 * block_events.len() + 1),
                    });
                }
            }
            // Use identities must be unique per variable, which the single
            // shared usage key is not: tag them with the atom.
            for block_events in &mut events {
                for event in block_events.iter_mut() {
                    if let Event::Use { variable, usage } = event {
                        *usage = (usage.0, usage.1 * atoms + *variable);
                    }
                }
            }
            let oracle = ssa::build(&cfg, &events).unwrap();
            let convert = |version: Version<usize, Definition>| match version {
                Version::Entry(_) => MemoryVersion::LiveOnEntry,
                Version::Definition { definition, .. } => MemoryVersion::Definition(definition),
                Version::Phi { block, .. } => MemoryVersion::Phi { block },
            };

            for block in 0..blocks {
                let slots = program.events[block].len();
                for slot in 0..=2 * slots + 1 {
                    let version = if slot == 0 {
                        analysis.block_entry(block).unwrap()
                    } else if slot == 2 * slots + 1 {
                        analysis.block_exit(block).unwrap()
                    } else {
                        let (before, after) = analysis.event(block, (slot - 1) / 2).unwrap();
                        if slot % 2 == 1 { before } else { after }
                    };
                    for atom in 0..atoms {
                        let expected = convert(oracle.uses[&(block, slot * atoms + atom)]);
                        let actual = analysis.version_at(version, atom_byte(atom)).unwrap();
                        assert_eq!(
                            actual, expected,
                            "block {block} slot {slot} atom {atom} {:?} {:?}",
                            program.events, program.writes
                        );
                    }
                }
            }
            // Phi inputs are the predecessors' exit versions.
            for phi in &oracle.phis {
                phi_cases += 1;
                let byte = atom_byte(phi.variable);
                assert_eq!(
                    analysis.version_at(analysis.block_entry(phi.block).unwrap(), byte),
                    Some(MemoryVersion::Phi { block: phi.block })
                );
                for &(predecessor, version) in &phi.inputs {
                    assert_eq!(
                        analysis.version_at(analysis.block_exit(predecessor).unwrap(), byte),
                        Some(convert(version))
                    );
                }
            }
            // And the analysis has no phi the oracle lacks.
            let mut oracle_phi_bytes = 0usize;
            for phi in &oracle.phis {
                let _ = phi;
                oracle_phi_bytes += 1;
            }
            let mut analysis_phi_atoms = 0usize;
            for block in 0..blocks {
                for (start, end) in analysis.phi_ranges(block) {
                    analysis_phi_atoms += (0..atoms)
                        .filter(|&atom| start <= atom_byte(atom) && atom_byte(atom) < end)
                        .count();
                }
            }
            assert_eq!(analysis_phi_atoms, oracle_phi_bytes);
            cases += 1;
        }
        assert!(cases > 300, "{cases}");
        assert!(phi_cases > 300, "{phi_cases}");
    }

    struct WalkerAnalysis {
        graph: memory_ssa::MemoryAccessGraph<Definition>,
        points: memory_ssa::MemoryPointMap<(usize, usize)>,
        writes: BTreeMap<Definition, Vec<MemoryEffect<u8>>>,
    }

    fn walker_analysis(cfg: &ControlFlowGraph, program: &Program) -> WalkerAnalysis {
        let mut events = vec![Vec::new(); program.events.len()];
        let mut writes = BTreeMap::new();
        for (block, block_events) in program.events.iter().enumerate() {
            for (index, event) in block_events.iter().enumerate() {
                if let Some(definition) = event.definition {
                    writes.insert(
                        definition,
                        program.writes[event.write_start..event.write_end]
                            .iter()
                            .map(|write| match *write {
                                RangeWrite::Exact { start, end } => {
                                    MemoryEffect::Exact(MemoryLocation {
                                        object: 1u8,
                                        offset: start,
                                        byte_len: (end - start) as usize,
                                    })
                                }
                                RangeWrite::All => MemoryEffect::UnknownObject(1u8),
                            })
                            .collect::<Vec<_>>(),
                    );
                }
                events[block].push(MemoryAccessEvent {
                    point: (block, index),
                    definition: event.definition,
                });
            }
        }
        let (graph, points) = memory_ssa::build(cfg, &events).unwrap();
        WalkerAnalysis {
            graph,
            points,
            writes,
        }
    }

    impl WalkerAnalysis {
        fn clobber(
            &self,
            walker: &mut ClobberWalker,
            start: memory_ssa::MemoryAccessId,
            offset: i64,
            byte_len: usize,
        ) -> MemoryClobber {
            let query = MemoryEffect::Exact(MemoryLocation {
                object: 1u8,
                offset,
                byte_len,
            });
            let oracle = |definition: &Definition, query: &MemoryEffect<u8>| {
                self.writes[definition]
                    .iter()
                    .any(|&write| effects_may_alias(write, *query))
            };
            walker.clobber(&self.graph, start, &query, &oracle).unwrap()
        }
    }

    /// On acyclic CFGs the clobber walker is exact, and two points have the
    /// same nearest clobber for a range exactly when every byte of the range
    /// has the same version at both.
    #[test]
    fn acyclic_cfgs_agree_with_the_clobber_walker_on_snapshot_equality() {
        let mut random = Xorshift(0x2545_f491_4f6c_dd1d);
        let mut cases = 0;
        let mut equal_pairs = 0;
        for _ in 0..600 {
            let Some((blocks, cfg)) = random_cfg(&mut random, true) else {
                continue;
            };
            let program = random_program(&mut random, blocks, true);
            let analysis = program.build(&cfg);
            let walker_analysis = walker_analysis(&cfg, &program);
            let mut walker = ClobberWalker::new();

            let mut points = Vec::new();
            for block in 0..blocks {
                points.push((
                    analysis.block_entry(block).unwrap(),
                    walker_analysis.points.block_entry(block).unwrap(),
                ));
                points.push((
                    analysis.block_exit(block).unwrap(),
                    walker_analysis.points.block_exit(block).unwrap(),
                ));
                for index in 0..program.events[block].len() {
                    let (before, after) = analysis.event(block, index).unwrap();
                    let access = walker_analysis.points.event((block, index)).unwrap();
                    points.push((before, access.before));
                    points.push((after, access.after));
                }
            }
            for _ in 0..6 {
                let offset = 4 * random.next(4) as i64;
                let byte_len = 4 * (1 + random.next(2));
                let certificates = points
                    .iter()
                    .map(|&(version, access)| {
                        let mut segments = Vec::new();
                        analysis
                            .segments(version, offset, offset + byte_len as i64, &mut segments)
                            .unwrap();
                        let MemoryClobber::Access(clobber) =
                            walker_analysis.clobber(&mut walker, access, offset, byte_len)
                        else {
                            panic!("acyclic graphs have no indeterminate clobbers")
                        };
                        (segments, clobber)
                    })
                    .collect::<Vec<_>>();
                for (left, (left_segments, left_clobber)) in certificates.iter().enumerate() {
                    for (right_segments, right_clobber) in &certificates[left..] {
                        let walker_equal = left_clobber == right_clobber;
                        let versions_equal = left_segments == right_segments;
                        assert_eq!(
                            walker_equal, versions_equal,
                            "{offset}+{byte_len}: {left_segments:?} / {left_clobber:?} vs {right_segments:?} / {right_clobber:?} in {:?} {:?}",
                            program.events, program.writes
                        );
                        equal_pairs += usize::from(walker_equal);
                    }
                }
                // A write clobber versions exactly the bytes it writes.
                for (segments, clobber) in &certificates {
                    if let Some(MemoryAccess::Definition { definition, .. }) =
                        walker_analysis.graph.access(*clobber)
                    {
                        assert!(segments.iter().any(|segment| {
                            segment.version == MemoryVersion::Definition(*definition)
                        }));
                    }
                }
            }
            cases += 1;
        }
        assert!(cases > 300, "{cases}");
        assert!(equal_pairs > 1000, "{equal_pairs}");
    }

    /// Documented divergence from the clobber walker (celox-sim/celox#2041).
    ///
    /// Loop `1 -> {2, 3} -> 4 -> 1` writes bytes `64..72` in block 2 only.
    /// For a query of bytes `0..8` nothing in the loop aliases, so the
    /// version at every block is live-on-entry.  The walker started at the
    /// header's MemoryPhi finds every input of the inner join's MemoryPhi on
    /// its own stack, keeps that phi as a version of its own, and therefore
    /// also keeps the header phi; started at the inner join it folds both.
    /// The certificate thus depends on the start of the walk, which is what
    /// made planner and rebuild disagree.
    #[test]
    fn clobber_walker_cycle_rule_is_not_canonical_on_loops() {
        let cfg = ControlFlowGraph::analyze(
            vec![vec![1], vec![2, 3], vec![4], vec![4], vec![1, 5], vec![]],
            0,
        )
        .unwrap();
        let mut program = Program::new(6);
        program.write(2, &[exact(64, 72)]);
        let analysis = program.build(&cfg);
        let walker_analysis = walker_analysis(&cfg, &program);

        let header = walker_analysis.points.block_entry(1).unwrap();
        let join = walker_analysis.points.block_entry(4).unwrap();
        let classify = |clobber| match clobber {
            MemoryClobber::Indeterminate => None,
            MemoryClobber::Access(access) => Some(match walker_analysis.graph.access(access) {
                Some(MemoryAccess::LiveOnEntry) => MemoryVersion::LiveOnEntry,
                Some(MemoryAccess::Definition { definition, .. }) => {
                    MemoryVersion::Definition(*definition)
                }
                Some(MemoryAccess::Phi { block, .. }) => MemoryVersion::Phi { block },
                None => unreachable!(),
            }),
        };
        let mut walker = ClobberWalker::new();
        let from_header = classify(walker_analysis.clobber(&mut walker, header, 0, 8));
        let mut walker = ClobberWalker::new();
        let from_join = classify(walker_analysis.clobber(&mut walker, join, 0, 8));
        assert_eq!(from_header, Some(MemoryVersion::Phi { block: 1 }));
        assert_eq!(from_join, Some(MemoryVersion::LiveOnEntry));

        for block in [1, 4] {
            assert_eq!(
                analysis.version_at(analysis.block_entry(block).unwrap(), 0),
                Some(MemoryVersion::LiveOnEntry)
            );
        }
    }
}
