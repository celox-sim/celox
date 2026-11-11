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
mod tests;
