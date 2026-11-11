//! Braun--Hack sections 4.2 and 4.3: W/S states and coupling plan.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

use crate::HashMap;
use crate::native::mir::{BlockId, MFunction, MInst, PackedStateHome, VReg};

use super::cfg::NormalizedCfg;
use super::next_use::{DistanceMap, NextUseAnalysis, NextUseDistance};
use super::reload::{EdgeUse, PlanningRecipes, PointUse, ReloadRecipeAnalysis, ResolvedRecipe};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct LogicalValue(pub u32);

/// Mutable W/S frontiers stay sorted; completed large S rows use indexed
/// 8/16-bit offsets or a dense bitmap, whichever is smaller. These preserve exact
/// ascending iteration and membership without a full value per CFG row.
#[derive(Debug, Clone)]
pub(super) enum LogicalSet {
    Mutable(Vec<LogicalValue>),
    Frozen(std::sync::Arc<FrozenLogicalSet>),
}

#[derive(Debug)]
pub(super) struct FrozenLogicalSet {
    values: std::sync::Arc<[LogicalValue]>,
    storage: FrozenLogicalStorage,
}

#[derive(Debug, PartialEq, Eq)]
enum FrozenLogicalStorage {
    Sparse8 {
        // Each chunk contains at most 64 values within a u8 span.
        chunks: Box<[(u32, u32)]>,
        offsets: Box<[u8]>,
    },
    Sparse {
        // Each chunk contains at most 64 values within a u16 span.
        chunks: Box<[(u32, u32)]>, // (base value, first offset index)
        offsets: Box<[u16]>,
    },
    Dense {
        words: Box<[u64]>,
        len: usize,
    },
}

impl Default for LogicalSet {
    fn default() -> Self {
        Self::Mutable(Vec::new())
    }
}

impl PartialEq for LogicalSet {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Mutable(left), Self::Mutable(right)) => left == right,
            (Self::Frozen(left), Self::Frozen(right)) if left.storage == right.storage => true,
            _ => self.len() == other.len() && self.iter().eq(other.iter()),
        }
    }
}
impl Eq for LogicalSet {}

impl LogicalSet {
    fn new() -> Self {
        Self::default()
    }

    fn freeze(&mut self, pool: &std::sync::Arc<[LogicalValue]>) {
        let Self::Mutable(values) = self else {
            return;
        };
        if values.len() < 64
            || values
                .last()
                .is_some_and(|value| value.0 as usize >= pool.len())
        {
            return;
        }
        let mut chunks = Vec::new();
        let mut offsets = Vec::with_capacity(values.len());
        let mut base = 0;
        let mut chunk_start = 0;
        let mut byte_chunks = Vec::new();
        let mut byte_base = 0;
        let mut byte_chunk_start = 0;
        for (index, value) in values.iter().enumerate() {
            if index == 0 || index - chunk_start == 64 || value.0 - base > u32::from(u16::MAX) {
                let Ok(offset) = u32::try_from(index) else {
                    return;
                };
                base = value.0;
                chunk_start = index;
                chunks.push((base, offset));
            }
            offsets.push((value.0 - base) as u16);
            if index == 0
                || index - byte_chunk_start == 64
                || value.0 - byte_base > u32::from(u8::MAX)
            {
                let Ok(offset) = u32::try_from(index) else {
                    return;
                };
                byte_base = value.0;
                byte_chunk_start = index;
                byte_chunks.push((byte_base, offset));
            }
        }
        let sparse_bytes = offsets.len() * 2 + chunks.len() * 8;
        let byte_sparse_bytes = values.len() + byte_chunks.len() * 8;
        let word_count = values.last().unwrap().0 as usize / 64 + 1;
        let dense_bytes = word_count * std::mem::size_of::<u64>();
        let storage = if dense_bytes < sparse_bytes.min(byte_sparse_bytes)
            && dense_bytes < values.len() * 4
        {
            let mut words = vec![0u64; word_count];
            for value in values.iter() {
                words[value.0 as usize / 64] |= 1u64 << (value.0 % 64);
            }
            FrozenLogicalStorage::Dense {
                words: words.into_boxed_slice(),
                len: values.len(),
            }
        } else if byte_sparse_bytes < sparse_bytes && byte_sparse_bytes < values.len() * 4 {
            let mut chunk = 0;
            let offsets = values
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    if byte_chunks
                        .get(chunk + 1)
                        .is_some_and(|&(_, start)| index == start as usize)
                    {
                        chunk += 1;
                    }
                    (value.0 - byte_chunks[chunk].0) as u8
                })
                .collect();
            FrozenLogicalStorage::Sparse8 {
                chunks: byte_chunks.into_boxed_slice(),
                offsets,
            }
        } else if sparse_bytes < values.len() * 4 {
            FrozenLogicalStorage::Sparse {
                chunks: chunks.into_boxed_slice(),
                offsets: offsets.into_boxed_slice(),
            }
        } else {
            return;
        };
        *self = Self::Frozen(std::sync::Arc::new(FrozenLogicalSet {
            values: std::sync::Arc::clone(pool),
            storage,
        }));
    }

    fn mutable(&mut self) -> &mut Vec<LogicalValue> {
        if matches!(self, Self::Frozen(_)) {
            *self = Self::Mutable(self.iter().copied().collect());
        }
        let Self::Mutable(values) = self else {
            unreachable!()
        };
        values
    }

    #[cfg(test)]
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(super) fn contains(&self, value: &LogicalValue) -> bool {
        match self {
            Self::Mutable(values) => values.binary_search(value).is_ok(),
            Self::Frozen(row) => match &row.storage {
                FrozenLogicalStorage::Sparse8 { chunks, offsets } => {
                    sparse_contains(chunks, offsets, value.0)
                }
                FrozenLogicalStorage::Dense { words, .. } => words
                    .get(value.0 as usize / 64)
                    .is_some_and(|word| word & (1u64 << (value.0 % 64)) != 0),
                FrozenLogicalStorage::Sparse { chunks, offsets } => {
                    sparse_contains(chunks, offsets, value.0)
                }
            },
        }
    }

    pub(super) fn insert(&mut self, value: LogicalValue) -> bool {
        let values = self.mutable();
        let Err(index) = values.binary_search(&value) else {
            return false;
        };
        values.insert(index, value);
        true
    }

    fn remove(&mut self, value: &LogicalValue) -> bool {
        let values = self.mutable();
        let Ok(index) = values.binary_search(value) else {
            return false;
        };
        values.remove(index);
        true
    }

    fn len(&self) -> usize {
        match self {
            Self::Mutable(values) => values.len(),
            Self::Frozen(row) => match &row.storage {
                FrozenLogicalStorage::Sparse8 { offsets, .. } => offsets.len(),
                FrozenLogicalStorage::Sparse { offsets, .. } => offsets.len(),
                FrozenLogicalStorage::Dense { len, .. } => *len,
            },
        }
    }
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn iter(&self) -> LogicalIter<'_> {
        LogicalIter {
            set: self,
            position: 0,
            chunk: 0,
            remaining_word: 0,
        }
    }
    fn retain(&mut self, keep: impl FnMut(&LogicalValue) -> bool) {
        self.mutable().retain(keep);
    }
    fn difference<'a>(&'a self, other: &'a Self) -> impl Iterator<Item = &'a LogicalValue> + 'a {
        let mut membership = OrderedMembership::new(other, self.len());
        self.iter().filter(move |value| !membership.contains(value))
    }
}

/// Membership queries in ascending value order can share one merge cursor.
/// Keep direct lookup for bitmaps and for a few queries into a much larger set.
enum OrderedMembership<'a> {
    Lookup(&'a LogicalSet),
    Merge(std::iter::Peekable<LogicalIter<'a>>),
}

impl<'a> OrderedMembership<'a> {
    fn new(set: &'a LogicalSet, queries: usize) -> Self {
        if set.len() > queries.saturating_mul(8)
            || matches!(set, LogicalSet::Frozen(row) if matches!(&row.storage, FrozenLogicalStorage::Dense { .. }))
        {
            Self::Lookup(set)
        } else {
            Self::Merge(set.iter().peekable())
        }
    }

    fn contains(&mut self, value: &LogicalValue) -> bool {
        match self {
            Self::Lookup(set) => set.contains(value),
            Self::Merge(values) => {
                while values.peek().is_some_and(|&&next| next < *value) {
                    values.next();
                }
                values.peek().is_some_and(|&&next| next == *value)
            }
        }
    }
}

fn sparse_contains<T: Ord + TryFrom<u32>>(
    chunks: &[(u32, u32)],
    offsets: &[T],
    value: u32,
) -> bool {
    let Some(chunk) = chunks
        .partition_point(|&(base, _)| base <= value)
        .checked_sub(1)
    else {
        return false;
    };
    let (base, start) = chunks[chunk];
    let Ok(offset) = T::try_from(value - base) else {
        return false;
    };
    let end = chunks
        .get(chunk + 1)
        .map_or(offsets.len(), |&(_, end)| end as usize);
    offsets[start as usize..end].binary_search(&offset).is_ok()
}

pub(super) struct LogicalIter<'a> {
    set: &'a LogicalSet,
    position: usize,
    chunk: usize,
    remaining_word: u64,
}

impl<'a> Iterator for LogicalIter<'a> {
    type Item = &'a LogicalValue;
    fn next(&mut self) -> Option<Self::Item> {
        let value = match self.set {
            LogicalSet::Mutable(values) => values.get(self.position)?,
            LogicalSet::Frozen(row) => match &row.storage {
                FrozenLogicalStorage::Sparse8 { chunks, offsets } => {
                    let offset = *offsets.get(self.position)?;
                    if chunks
                        .get(self.chunk + 1)
                        .is_some_and(|&(_, start)| self.position == start as usize)
                    {
                        self.chunk += 1;
                    }
                    &row.values[(chunks[self.chunk].0 + u32::from(offset)) as usize]
                }
                FrozenLogicalStorage::Sparse { chunks, offsets } => {
                    let offset = *offsets.get(self.position)?;
                    if chunks
                        .get(self.chunk + 1)
                        .is_some_and(|&(_, start)| self.position == start as usize)
                    {
                        self.chunk += 1;
                    }
                    &row.values[(chunks[self.chunk].0 + u32::from(offset)) as usize]
                }
                FrozenLogicalStorage::Dense { words, .. } => {
                    while self.remaining_word == 0 {
                        self.remaining_word = *words.get(self.chunk)?;
                        self.chunk += 1;
                    }
                    let bit = self.remaining_word.trailing_zeros() as usize;
                    self.remaining_word &= self.remaining_word - 1;
                    &row.values[(self.chunk - 1) * 64 + bit]
                }
            },
        };
        self.position += 1;
        Some(value)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.set.len() - self.position;
        (remaining, Some(remaining))
    }
}
impl ExactSizeIterator for LogicalIter<'_> {}

impl FromIterator<LogicalValue> for LogicalSet {
    fn from_iter<T: IntoIterator<Item = LogicalValue>>(iter: T) -> Self {
        let mut values = iter.into_iter().collect::<Vec<_>>();
        values.sort_unstable();
        values.dedup();
        Self::Mutable(values)
    }
}
impl Extend<LogicalValue> for LogicalSet {
    fn extend<T: IntoIterator<Item = LogicalValue>>(&mut self, iter: T) {
        for value in iter {
            self.insert(value);
        }
    }
}
impl IntoIterator for LogicalSet {
    type Item = LogicalValue;
    type IntoIter = std::vec::IntoIter<LogicalValue>;
    fn into_iter(self) -> Self::IntoIter {
        match self {
            Self::Mutable(values) => values.into_iter(),
            Self::Frozen(_) => self.iter().copied().collect::<Vec<_>>().into_iter(),
        }
    }
}
impl<'a> IntoIterator for &'a LogicalSet {
    type Item = &'a LogicalValue;
    type IntoIter = LogicalIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct SpillHome(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PointSide {
    Before,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ProgramPoint {
    pub block: BlockId,
    pub instruction: usize,
    pub side: PointSide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlannedOp {
    Spill {
        value: LogicalValue,
        home: SpillHome,
    },
    Reload {
        value: LogicalValue,
        home: SpillHome,
    },
    SpillPhi {
        value: LogicalValue,
        home: SpillHome,
    },
}

/// Materialization on one CFG edge.
///
/// A point operation reads or writes the home of one logical SSA value.  A
/// phi edge is different: it transfers a predecessor value into the
/// successor's logical identity.  Keeping both identities explicit prevents
/// a reload from accidentally reading the successor home when only the
/// predecessor home is valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlannedEdgeOp {
    Spill {
        source: LogicalValue,
        destination: LogicalValue,
        destination_home: SpillHome,
    },
    Reload {
        source: LogicalValue,
        source_home: SpillHome,
        destination: LogicalValue,
    },
}

pub(super) fn edge_reload_uses_transferred_home(
    operations: &[PlannedEdgeOp],
    reload_index: usize,
) -> bool {
    let Some(PlannedEdgeOp::Reload {
        source,
        source_home,
        destination,
    }) = operations.get(reload_index).copied()
    else {
        return false;
    };
    operations[..reload_index].iter().any(|operation| {
        matches!(
            operation,
            PlannedEdgeOp::Spill {
                source: spill_source,
                destination: spill_destination,
                destination_home,
            } if *spill_source == source
                && *spill_destination == destination
                && *destination_home == source_home
        )
    })
}

#[derive(Debug)]
pub(super) struct SpillPlan {
    pub logical: LogicalValues,
    pub homes: SpillHomes,
    pub point_ops: Vec<(ProgramPoint, PlannedOp)>,
    pub edge_ops: BTreeMap<(usize, usize), Vec<PlannedEdgeOp>>,
    /// Point reloads whose value is supplied by an independently verified
    /// path-specific MemorySSA recipe rather than a persistent spill home.
    pub recipe_reloads: BTreeSet<(BlockId, usize, LogicalValue)>,
    /// Phi-congruence homes whose complete selected reload set is supplied by
    /// exact rematerialization recipes instead of a stack slot.
    pub recipe_homes: BTreeSet<SpillHome>,
    /// Phi-congruence homes assigned to allocator-managed packed SimState
    /// words.  These homes remain ordinary W/S homes: unlike recipe-only
    /// homes, every path must execute the planned spill before a reload.
    pub state_homes: BTreeMap<SpillHome, PackedStateHome>,
    /// Exact MemorySSA recipe for every reload assigned to `state_homes`.
    /// Keys retain the pre-reconstruction insertion point; reconstruction
    /// independently proves the emitted load against final MIR.
    pub state_reload_recipes: BTreeMap<(BlockId, usize, LogicalValue), ResolvedRecipe>,
    pub w_entry: Vec<LogicalSet>,
    pub w_exit: Vec<LogicalSet>,
    pub s_entry: Vec<LogicalSet>,
    pub s_exit: Vec<LogicalSet>,
}

#[derive(Debug)]
struct BlockTransition {
    point_ops: Vec<(ProgramPoint, PlannedOp)>,
    recipe_reloads: BTreeSet<(BlockId, usize, LogicalValue)>,
    w_exit: LogicalSet,
    s_exit: LogicalSet,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SpillPlanError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub values: Vec<VReg>,
    pub message: String,
}

impl SpillPlanError {
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

#[derive(Debug)]
pub(super) struct LogicalValues {
    count: u32,
}

impl LogicalValues {
    fn build(func: &MFunction) -> Self {
        Self {
            count: func.vregs.count(),
        }
    }

    pub(super) fn of(&self, value: VReg) -> LogicalValue {
        LogicalValue(value.0)
    }

    fn checked_of(
        &self,
        value: VReg,
        block: Option<BlockId>,
        instruction: Option<usize>,
    ) -> Result<LogicalValue, SpillPlanError> {
        if value.0 >= self.count {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.VALUE_RANGE",
                block,
                instruction,
                vec![value],
                format!(
                    "v{} is outside the spill plan's {} logical values",
                    value.0, self.count
                ),
            ));
        }
        Ok(LogicalValue(value.0))
    }
}

#[derive(Debug)]
pub(super) struct SpillHomes {
    count: u32,
}

impl SpillHomes {
    fn build(func: &MFunction) -> Result<Self, SpillPlanError> {
        let count = func.vregs.count() as usize;
        for block in &func.blocks {
            for phi in &block.phis {
                if phi.dst.0 as usize >= count {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.VALUE_RANGE",
                        Some(block.id),
                        None,
                        vec![phi.dst],
                        format!(
                            "phi destination v{} is outside the function's {count} virtual registers",
                            phi.dst.0
                        ),
                    ));
                }
                for &(_, source) in &phi.sources {
                    if source.0 as usize >= count {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.VALUE_RANGE",
                            Some(block.id),
                            None,
                            vec![source],
                            format!(
                                "phi source v{} is outside the function's {count} virtual registers",
                                source.0
                            ),
                        ));
                    }
                }
            }
        }
        Ok(Self {
            count: count as u32,
        })
    }

    pub(super) fn of_vreg(&self, value: VReg) -> SpillHome {
        debug_assert!(value.0 < self.count);
        SpillHome(value.0)
    }

    pub(super) fn of_logical(&self, value: LogicalValue) -> SpillHome {
        debug_assert!(value.0 < self.count);
        SpillHome(value.0)
    }

    pub(super) fn members(&self, home: SpillHome) -> impl Iterator<Item = VReg> + '_ {
        (home.0 < self.count).then_some(VReg(home.0)).into_iter()
    }
}

#[derive(Debug, Default)]
struct EdgeTranslation {
    to_successor: HashMap<LogicalValue, Vec<LogicalValue>>,
    to_predecessor: HashMap<LogicalValue, LogicalValue>,
}

/// Indexed logical-value translation across normalized phi edges.
///
/// Building the two directions in one pass over phi operands avoids rescanning
/// every phi (and its predecessor list) for every member of W/S. A destination
/// has exactly one source on an edge. One source may feed several phi
/// destinations, so the forward relation is one-to-many.
#[derive(Debug)]
struct EdgeTranslations {
    by_edge: HashMap<(usize, usize), EdgeTranslation>,
}

impl EdgeTranslations {
    fn build(
        func: &MFunction,
        cfg: &NormalizedCfg,
        logical: &LogicalValues,
    ) -> Result<Self, SpillPlanError> {
        let mut by_edge = HashMap::<(usize, usize), EdgeTranslation>::default();
        for (successor, block) in func.blocks.iter().enumerate() {
            for phi in &block.phis {
                let destination = logical.checked_of(phi.dst, Some(block.id), None)?;
                for &(predecessor_id, source) in &phi.sources {
                    let Some(&predecessor) = cfg.block_index.get(&predecessor_id) else {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.PHI_PREDECESSOR",
                            Some(block.id),
                            None,
                            vec![source, phi.dst],
                            format!(
                                "phi source predecessor {predecessor_id} is absent from the normalized CFG"
                            ),
                        ));
                    };
                    if !cfg
                        .successors
                        .get(predecessor)
                        .is_some_and(|successors| successors.contains(&successor))
                    {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.EDGE_EXISTS",
                            Some(predecessor_id),
                            None,
                            vec![source, phi.dst],
                            format!(
                                "phi edge {predecessor_id} -> {} is absent from the normalized CFG",
                                block.id
                            ),
                        ));
                    }
                    let source = logical.checked_of(source, Some(predecessor_id), None)?;
                    let translation = by_edge.entry((predecessor, successor)).or_default();
                    translation
                        .to_successor
                        .entry(source)
                        .or_default()
                        .push(destination);
                    if translation
                        .to_predecessor
                        .insert(destination, source)
                        .is_some()
                    {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.PHI_DESTINATION_UNIQUE",
                            Some(predecessor_id),
                            None,
                            vec![VReg(source.0), VReg(destination.0)],
                            format!(
                                "phi destination v{} has duplicate source for {predecessor_id}",
                                destination.0
                            ),
                        ));
                    }
                }
            }
        }
        Ok(Self { by_edge })
    }

    fn to_successors(
        &self,
        predecessor: usize,
        successor: usize,
        value: LogicalValue,
    ) -> impl Iterator<Item = LogicalValue> + '_ {
        let destinations = self
            .by_edge
            .get(&(predecessor, successor))
            .and_then(|translation| translation.to_successor.get(&value))
            .map(Vec::as_slice)
            .unwrap_or_default();
        destinations
            .iter()
            .copied()
            .chain(destinations.is_empty().then_some(value))
    }

    fn to_predecessor(
        &self,
        predecessor: usize,
        successor: usize,
        value: LogicalValue,
    ) -> LogicalValue {
        self.by_edge
            .get(&(predecessor, successor))
            .and_then(|translation| translation.to_predecessor.get(&value))
            .copied()
            .unwrap_or(value)
    }
}

#[cfg(test)]
pub(super) fn plan(
    func: &MFunction,
    cfg: &NormalizedCfg,
    next_use: &NextUseAnalysis,
    registers: usize,
) -> Result<SpillPlan, SpillPlanError> {
    let planning_recipes = PlanningRecipes::stack_only(func.vregs.count());
    plan_with_recipe_costs(func, cfg, next_use, &planning_recipes, registers)
}

#[cfg(test)]
pub(super) fn plan_with_recipe_costs(
    func: &MFunction,
    cfg: &NormalizedCfg,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    registers: usize,
) -> Result<SpillPlan, SpillPlanError> {
    let mut working = func.clone();
    plan_internal(
        &mut working,
        cfg,
        next_use,
        planning_recipes,
        registers,
        None,
    )
}

pub(super) fn plan_with_integrated_schedule(
    func: &mut MFunction,
    cfg: &NormalizedCfg,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    registers: usize,
    constraints: &super::constraints::ConstraintModel,
) -> Result<SpillPlan, SpillPlanError> {
    plan_internal(
        func,
        cfg,
        next_use,
        planning_recipes,
        registers,
        Some(constraints),
    )
}

fn plan_internal(
    func: &mut MFunction,
    cfg: &NormalizedCfg,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    registers: usize,
    constraints: Option<&super::constraints::ConstraintModel>,
) -> Result<SpillPlan, SpillPlanError> {
    let logical = LogicalValues::build(func);
    let homes = SpillHomes::build(func)?;
    let edge_translations = EdgeTranslations::build(func, cfg, &logical)?;
    let mut result = SpillPlan {
        logical,
        homes,
        point_ops: Vec::new(),
        edge_ops: BTreeMap::new(),
        recipe_reloads: BTreeSet::new(),
        recipe_homes: BTreeSet::new(),
        state_homes: BTreeMap::new(),
        state_reload_recipes: BTreeMap::new(),
        w_entry: vec![LogicalSet::new(); func.blocks.len()],
        w_exit: vec![LogicalSet::new(); func.blocks.len()],
        s_entry: vec![LogicalSet::new(); func.blocks.len()],
        s_exit: vec![LogicalSet::new(); func.blocks.len()],
    };
    let logical_keys = std::cell::OnceCell::<std::sync::Arc<[LogicalValue]>>::new();
    for block in 0..func.blocks.len() {
        let entry = if let Some(region) = next_use.region_at_entry(block) {
            init_loop_region(func, next_use, &result, block, region, registers)?
        } else {
            init_usual(
                func,
                cfg,
                next_use,
                planning_recipes,
                &result,
                &edge_translations,
                block,
                registers,
            )
        };
        let mut entry = entry;
        let live_entry = next_use.entry[block]
            .keys()
            .copied()
            .map(|value| {
                result
                    .logical
                    .checked_of(value, Some(func.blocks[block].id), Some(0))
            })
            .collect::<Result<LogicalSet, _>>()?;
        let mut exit_reload_costs = LazyExitReloadCosts {
            func,
            cfg,
            next_use,
            recipes: planning_recipes,
            translations: &edge_translations,
            block,
            cache: HashMap::default(),
        };
        let (spilled, transition, order) = loop {
            // S means that a valid home exists on every path.  Every live
            // value omitted from W_entry therefore requires a home; edge
            // coupling below materializes any missing predecessor store.  A
            // resident value keeps an existing home only when every
            // predecessor already has one.
            let spilled =
                spilled_at_entry(cfg, &result, &edge_translations, block, &live_entry, &entry);
            let (transition, order) = if let Some(constraints) = constraints {
                let instruction_constraints =
                    constraints.instructions.get(block).ok_or_else(|| {
                        SpillPlanError::new(
                            "SPILL_PLAN.SCHEDULE_ORDER",
                            Some(func.blocks[block].id),
                            None,
                            Vec::new(),
                            "allocation constraints do not cover this block",
                        )
                    })?;
                let (transition, order) = plan_scheduled_block_transition(
                    func,
                    next_use,
                    planning_recipes,
                    &result.logical,
                    &result.homes,
                    block,
                    registers,
                    &entry,
                    spilled.clone(),
                    instruction_constraints,
                    &mut exit_reload_costs,
                )?;
                (transition, Some(order))
            } else {
                (
                    plan_block_transition(
                        func,
                        next_use,
                        planning_recipes,
                        &result.logical,
                        &result.homes,
                        block,
                        registers,
                        &entry,
                        spilled.clone(),
                    )?,
                    None,
                )
            };
            let rejected = entry_residents_evicted_before_first_use(
                func,
                next_use,
                block,
                &entry,
                &transition,
                order.as_deref(),
            );
            if rejected.is_empty() {
                break (spilled, transition, order);
            }
            entry.retain(|value| !rejected.contains(value));
        };
        if let Some(order) = order
            && !order.iter().copied().eq(0..func.blocks[block].insts.len())
        {
            let original = func.blocks[block].insts.clone();
            func.blocks[block].insts = order
                .into_iter()
                .map(|source| original[source].clone())
                .collect();
        }
        result.w_entry[block] = entry;
        result.s_entry[block] = spilled;
        result.point_ops.extend(transition.point_ops);
        result.recipe_reloads.extend(transition.recipe_reloads);
        result.w_exit[block] = transition.w_exit;
        result.s_exit[block] = transition.s_exit;
        if result.s_entry[block].len() >= 64 || result.s_exit[block].len() >= 64 {
            let logical_keys =
                logical_keys.get_or_init(|| (0..result.logical.count).map(LogicalValue).collect());
            let same_spilled = result.s_entry[block] == result.s_exit[block];
            result.s_entry[block].freeze(logical_keys);
            if same_spilled && matches!(result.s_entry[block], LogicalSet::Frozen(_)) {
                result.s_exit[block] = result.s_entry[block].clone();
            } else {
                result.s_exit[block].freeze(logical_keys);
            }
        }
    }

    // Section 4.3.  Delaying this until every W/S exit is known is equivalent
    // to the paper's deferred handling of not-yet-processed backedges.
    let spilled_phis = result
        .point_ops
        .iter()
        .filter_map(|(_, operation)| match operation {
            PlannedOp::SpillPhi { value, .. } => Some(*value),
            _ => None,
        })
        .collect::<LogicalSet>();
    for successor in 0..func.blocks.len() {
        for &predecessor in &cfg.predecessors[successor] {
            let mut resident_spills = Vec::new();
            let mut home_transfers = Vec::new();
            let mut scratch_reloads = Vec::new();
            let mut resident_reloads = Vec::new();
            let predecessor_w = result.w_exit[predecessor].clone();
            let predecessor_s = result.s_exit[predecessor].clone();
            let translation = edge_translations.by_edge.get(&(predecessor, successor));
            for &successor_value in &result.w_entry[successor] {
                let value =
                    edge_translations.to_predecessor(predecessor, successor, successor_value);
                if !predecessor_w.contains(&value) {
                    resident_reloads.push(PlannedEdgeOp::Reload {
                        source: value,
                        source_home: result.homes.of_logical(value),
                        destination: successor_value,
                    });
                }
            }
            let renamed = translation.is_some_and(|edge| {
                edge.to_predecessor
                    .iter()
                    .any(|(destination, source)| destination != source)
            });
            if renamed || result.s_entry[successor] != predecessor_s {
                let mut predecessor_membership =
                    OrderedMembership::new(&predecessor_s, result.s_entry[successor].len());
                for &successor_value in &result.s_entry[successor] {
                    let value = translation
                        .and_then(|edge| edge.to_predecessor.get(&successor_value))
                        .copied()
                        .unwrap_or(successor_value);
                    let source_home = result.homes.of_logical(value);
                    let destination_home = result.homes.of_logical(successor_value);
                    // Only unchanged logical values query the merge cursor. Phi
                    // sources can arrive in any order and retain direct lookup.
                    if source_home == destination_home
                        && predecessor_membership.contains(&successor_value)
                    {
                        continue;
                    }
                    if predecessor_w.contains(&value) {
                        resident_spills.push(PlannedEdgeOp::Spill {
                            source: value,
                            destination: successor_value,
                            destination_home,
                        });
                    } else if predecessor_s.contains(&value) {
                        // A phi transfer between independent homes is a short
                        // edge-local reload/store pair.  Keeping the predecessor
                        // SSA value live merely to copy its successor home would
                        // recreate the phi-web live range this representation is
                        // intended to remove.
                        home_transfers.push(PlannedEdgeOp::Reload {
                            source: value,
                            source_home,
                            destination: successor_value,
                        });
                        home_transfers.push(PlannedEdgeOp::Spill {
                            source: successor_value,
                            destination: successor_value,
                            destination_home,
                        });
                    }
                }
            }
            for phi in &func.blocks[successor].phis {
                let destination = result.logical.of(phi.dst);
                if !spilled_phis.contains(&destination) {
                    continue;
                }
                let source = edge_translations.to_predecessor(predecessor, successor, destination);
                let source_home = result.homes.of_logical(source);
                let destination_home = result.homes.of_logical(destination);
                if source_home == destination_home && predecessor_s.contains(&source) {
                    continue;
                }
                if predecessor_w.contains(&source) {
                    resident_spills.push(PlannedEdgeOp::Spill {
                        source,
                        destination,
                        destination_home,
                    });
                } else if predecessor_s.contains(&source) {
                    home_transfers.push(PlannedEdgeOp::Reload {
                        source,
                        source_home,
                        destination,
                    });
                    home_transfers.push(PlannedEdgeOp::Spill {
                        source: destination,
                        destination,
                        destination_home,
                    });
                }
            }
            // A reload/store home transfer needs one transient register.
            // When every successor-resident value already survives in
            // predecessor W, no ordinary edge spill or reload creates that
            // slot. Explicitly park one such value across all home transfers.
            if !home_transfers.is_empty() {
                let surviving_residents = result.w_entry[successor]
                    .iter()
                    .copied()
                    .filter_map(|destination| {
                        let source =
                            edge_translations.to_predecessor(predecessor, successor, destination);
                        predecessor_w
                            .contains(&source)
                            .then_some((source, destination))
                    })
                    .collect::<Vec<_>>();
                if surviving_residents.len() == registers {
                    let (source, destination) = surviving_residents[0];
                    let destination_home = result.homes.of_logical(destination);
                    if !resident_spills.iter().any(|operation| {
                        matches!(
                            operation,
                            PlannedEdgeOp::Spill {
                                source: spill_source,
                                destination: spill_destination,
                                destination_home: spill_home,
                            } if *spill_source == source
                                && *spill_destination == destination
                                && *spill_home == destination_home
                        )
                    }) {
                        resident_spills.push(PlannedEdgeOp::Spill {
                            source,
                            destination,
                            destination_home,
                        });
                    }
                    scratch_reloads.push(PlannedEdgeOp::Reload {
                        // The reload is physically read from the successor
                        // home, but it must split the predecessor logical live
                        // range across every home transfer.
                        source,
                        source_home: destination_home,
                        destination,
                    });
                }
            }
            // Consume edge-resident values before introducing reload
            // temporaries or successor live-ins.  Otherwise one transient
            // transfer can raise an already-full W_exit above capacity.
            let mut operations = resident_spills;
            operations.extend(home_transfers);
            operations.extend(scratch_reloads);
            operations.extend(resident_reloads);
            if !operations.is_empty() {
                result.edge_ops.insert((predecessor, successor), operations);
            }
        }
    }
    Ok(result)
}

/// Scheduling only prices values considered for eviction. Large live-through
/// sets must not cause every block to price every successor's entire live set.
trait ExitReloadCosts {
    fn cost(&mut self, value: LogicalValue) -> u32;
}

#[cfg(test)]
impl ExitReloadCosts for HashMap<LogicalValue, u32> {
    fn cost(&mut self, value: LogicalValue) -> u32 {
        self.get(&value).copied().unwrap_or(0)
    }
}

struct LazyExitReloadCosts<'a> {
    func: &'a MFunction,
    cfg: &'a NormalizedCfg,
    next_use: &'a NextUseAnalysis,
    recipes: &'a PlanningRecipes,
    translations: &'a EdgeTranslations,
    block: usize,
    // Released after this block, with one entry per queried eviction candidate.
    cache: HashMap<LogicalValue, u32>,
}

impl ExitReloadCosts for LazyExitReloadCosts<'_> {
    fn cost(&mut self, value: LogicalValue) -> u32 {
        if let Some(&cost) = self.cache.get(&value) {
            return cost;
        }
        let mut cost = 0u32;
        for &successor in &self.cfg.successors[self.block] {
            let translation = self.translations.by_edge.get(&(self.block, successor));
            // A phi source is demanded once even if it feeds several phis or
            // is also live through. Phi destinations belong to the successor;
            // they must not be charged under their untranslated identity.
            let phi_source = translation.is_some_and(|edge| edge.to_successor.contains_key(&value));
            let untranslated =
                translation.is_none_or(|edge| !edge.to_predecessor.contains_key(&value));
            if phi_source
                || (untranslated && self.next_use.entry[successor].contains_key(&VReg(value.0)))
            {
                cost = cost.saturating_add(u32::from(reload_cost_on_edge(
                    self.func,
                    self.recipes,
                    self.block,
                    successor,
                    value,
                )));
            }
        }
        self.cache.insert(value, cost);
        cost
    }
}

// Independent eager reference: enumerate and translate all successor demands.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn exit_reload_costs(
    func: &MFunction,
    cfg: &NormalizedCfg,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    logical: &LogicalValues,
    edge_translations: &EdgeTranslations,
    block: usize,
) -> Result<HashMap<LogicalValue, u32>, SpillPlanError> {
    let mut costs = HashMap::<LogicalValue, u32>::default();
    for &successor in &cfg.successors[block] {
        let mut demanded = LogicalSet::new();
        for &value in next_use.entry[successor].keys() {
            let destination =
                logical.checked_of(value, Some(func.blocks[successor].id), Some(0))?;
            demanded.insert(edge_translations.to_predecessor(block, successor, destination));
        }
        for phi in &func.blocks[successor].phis {
            if let Some((_, source)) = phi
                .sources
                .iter()
                .find(|(predecessor, _)| *predecessor == func.blocks[block].id)
            {
                demanded.insert(logical.checked_of(
                    *source,
                    Some(func.blocks[block].id),
                    Some(func.blocks[block].insts.len()),
                )?);
            }
        }
        for value in demanded {
            let reload = u32::from(reload_cost_on_edge(
                func,
                planning_recipes,
                block,
                successor,
                value,
            ));
            costs
                .entry(value)
                .and_modify(|cost| *cost = cost.saturating_add(reload))
                .or_insert(reload);
        }
    }
    Ok(costs)
}

fn spilled_at_entry(
    cfg: &NormalizedCfg,
    plan: &SpillPlan,
    edge_translations: &EdgeTranslations,
    block: usize,
    live_entry: &LogicalSet,
    resident: &LogicalSet,
) -> LogicalSet {
    let mut spilled = live_entry
        .difference(resident)
        .copied()
        .collect::<LogicalSet>();
    if !cfg.predecessors[block].is_empty() {
        spilled.extend(resident.iter().copied().filter(|value| {
            cfg.predecessors[block].iter().all(|predecessor| {
                let predecessor_value =
                    edge_translations.to_predecessor(*predecessor, block, *value);
                plan.s_exit[*predecessor].contains(&predecessor_value)
            })
        }));
    }
    spilled
}

/// Remove optimistic entry residents that do not survive to their first local
/// use.  Keeping such a value in W moves its inevitable store from the incoming
/// edge into the block and occupies a register before providing any use.  S is
/// therefore no more expensive on the executed path and can reuse an already
/// valid predecessor home.
///
/// Replanning is bounded by the initial W size, which is at most the target's
/// fixed register count.  Each iteration removes at least one value, so this
/// remains linear in MIR size for a fixed ISA and needs no CFG-sized copy.
fn entry_residents_evicted_before_first_use(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    block: usize,
    resident: &LogicalSet,
    transition: &BlockTransition,
    order: Option<&[usize]>,
) -> LogicalSet {
    let first_use = |value: LogicalValue| {
        order.map_or_else(
            || next_use.next_local_use(block, 0, VReg(value.0)),
            |order| {
                order.iter().position(|&source| {
                    func.blocks[block].insts[source]
                        .uses()
                        .contains(&VReg(value.0))
                })
            },
        )
    };
    transition
        .point_ops
        .iter()
        .filter_map(|(point, operation)| match *operation {
            PlannedOp::Spill { value, .. }
                if resident.contains(&value)
                    && first_use(value).is_none_or(|first_use| point.instruction < first_use) =>
            {
                Some(value)
            }
            PlannedOp::Reload { value, .. }
                if resident.contains(&value)
                    && !transition.recipe_reloads.contains(&(
                        func.blocks[block].id,
                        point.instruction,
                        value,
                    ))
                    && first_use(value) == Some(point.instruction) =>
            {
                Some(value)
            }
            _ => None,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn plan_block_transition(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    logical: &LogicalValues,
    homes: &SpillHomes,
    block: usize,
    registers: usize,
    w_entry: &LogicalSet,
    spilled: LogicalSet,
) -> Result<BlockTransition, SpillPlanError> {
    let mut planner = BlockTransitionPlanner::new(
        func,
        next_use,
        planning_recipes,
        logical,
        homes,
        block,
        registers,
        w_entry,
        spilled,
    )?;
    for (instruction, inst) in func.blocks[block].insts.iter().enumerate() {
        planner.step(
            TransitionPoint {
                output: instruction,
                source: instruction,
            },
            inst,
        )?;
    }
    planner.finish()
}

#[derive(Debug, Clone, Copy)]
struct TransitionPoint {
    /// Instruction position in the final block order.
    output: usize,
    /// Stable position in the order consumed by next-use and planning-recipe
    /// analysis.
    source: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct AllocationCandidateScore {
    blocked_by_deferred_reload: bool,
    pressure_delta: isize,
    continuation_tie: std::cmp::Reverse<bool>,
    resident_operand_tie: std::cmp::Reverse<usize>,
    operand_tie: std::cmp::Reverse<usize>,
    materialization_cost: u32,
    preferred_rank: usize,
    source: usize,
}

struct AllocationReadyQueue {
    ordered: BTreeSet<(AllocationCandidateScore, usize)>,
    by_source: BTreeSet<(usize, usize)>,
    scores: Vec<Option<AllocationCandidateScore>>,
}

impl AllocationReadyQueue {
    fn new(instructions: usize) -> Self {
        Self {
            ordered: BTreeSet::new(),
            by_source: BTreeSet::new(),
            scores: vec![None; instructions],
        }
    }

    fn insert(&mut self, instruction: usize, score: AllocationCandidateScore) {
        debug_assert!(self.scores[instruction].is_none());
        self.ordered.insert((score, instruction));
        self.by_source.insert((score.source, instruction));
        self.scores[instruction] = Some(score);
    }

    fn refresh(&mut self, instruction: usize, score: AllocationCandidateScore) {
        let Some(previous) = self.scores[instruction].replace(score) else {
            return;
        };
        if previous != score {
            self.ordered.remove(&(previous, instruction));
            self.ordered.insert((score, instruction));
        }
    }

    fn pop(
        &mut self,
        current_resident: usize,
        register_capacity: usize,
        demanded: Option<usize>,
    ) -> Option<(AllocationCandidateScore, usize)> {
        let &(source, source_instruction) = self.by_source.first()?;
        let source_score = self.scores[source_instruction]?;
        debug_assert_eq!(source, source_score.source);
        let source_projected = if source_score.pressure_delta < 0 {
            current_resident.saturating_sub(source_score.pressure_delta.unsigned_abs())
        } else {
            current_resident.saturating_add(source_score.pressure_delta as usize)
        };
        let &(best_score, best_instruction) = self.ordered.first()?;
        let demanded = demanded.and_then(|instruction| {
            let score = self.scores[instruction]?;
            let projected = if score.pressure_delta < 0 {
                current_resident.saturating_sub(score.pressure_delta.unsigned_abs())
            } else {
                current_resident.saturating_add(score.pressure_delta as usize)
            };
            (!score.blocked_by_deferred_reload && projected <= register_capacity)
                .then_some((score, instruction))
        });
        let best_projected = if best_score.pressure_delta < 0 {
            current_resident.saturating_sub(best_score.pressure_delta.unsigned_abs())
        } else {
            current_resident.saturating_add(best_score.pressure_delta as usize)
        };
        let best_is_legal =
            !best_score.blocked_by_deferred_reload && best_projected <= register_capacity;
        let (score, instruction) = if let Some(demanded) = demanded {
            demanded
        } else if best_is_legal
            && (best_score.continuation_tie.0 || current_resident >= register_capacity)
        {
            (best_score, best_instruction)
        } else if !source_score.blocked_by_deferred_reload && source_projected <= register_capacity
        {
            (source_score, source_instruction)
        } else {
            (best_score, best_instruction)
        };
        self.ordered.remove(&(score, instruction));
        self.by_source.remove(&(score.source, instruction));
        self.scores[instruction] = None;
        Some((score, instruction))
    }

    fn contains(&self, instruction: usize) -> bool {
        self.scores.get(instruction).is_some_and(Option::is_some)
    }
}

struct DependencyDemandFrame {
    instruction: usize,
    next_dependency: usize,
}

struct DependencyDemand {
    target: usize,
    stack: Vec<DependencyDemandFrame>,
}

impl DependencyDemand {
    fn new(target: usize) -> Self {
        Self {
            target,
            stack: vec![DependencyDemandFrame {
                instruction: target,
                next_dependency: 0,
            }],
        }
    }

    /// Follow one unfinished sink packet backwards until its next ready
    /// prerequisite. Each dependency edge in this demand is scanned once.
    fn next_ready(&mut self, region: &super::schedule::ForwardReadyRegion) -> Option<usize> {
        loop {
            let frame_index = self.stack.len().checked_sub(1)?;
            let instruction = self.stack[frame_index].instruction;
            if region.is_emitted(instruction) {
                self.stack.pop();
                continue;
            }
            let dependency = {
                let dependencies = region.dependencies(instruction);
                let frame = &mut self.stack[frame_index];
                let mut dependency = None;
                while frame.next_dependency != dependencies.len() {
                    let candidate = dependencies[frame.next_dependency];
                    frame.next_dependency += 1;
                    if !region.is_emitted(candidate) {
                        dependency = Some(candidate);
                        break;
                    }
                }
                dependency
            };
            if let Some(dependency) = dependency {
                self.stack.push(DependencyDemandFrame {
                    instruction: dependency,
                    next_dependency: 0,
                });
                continue;
            }
            return region.is_ready(instruction).then_some(instruction);
        }
    }
}

struct ScheduledStepDelta {
    resident: Vec<LogicalValue>,
    deferred: Vec<LogicalValue>,
    remaining_uses: Vec<LogicalValue>,
    continuation: Vec<LogicalValue>,
}

#[allow(clippy::too_many_arguments)]
fn plan_scheduled_block_transition(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    logical: &LogicalValues,
    homes: &SpillHomes,
    block: usize,
    registers: usize,
    w_entry: &LogicalSet,
    spilled: LogicalSet,
    constraints: &[super::constraints::InstructionConstraints],
    exit_reload_costs: &mut dyn ExitReloadCosts,
) -> Result<(BlockTransition, Vec<usize>), SpillPlanError> {
    let instructions = &func.blocks[block].insts;
    if instructions.len() != constraints.len() {
        return Err(SpillPlanError::new(
            "SPILL_PLAN.SCHEDULE_ORDER",
            Some(func.blocks[block].id),
            None,
            Vec::new(),
            "allocation constraints do not cover every block instruction",
        ));
    }
    let original_spilled = spilled.clone();
    let mut remaining =
        RemainingBlockUses::build(func, next_use, logical, block, exit_reload_costs, None)?;
    let mut planner = BlockTransitionPlanner::new(
        func,
        next_use,
        planning_recipes,
        logical,
        homes,
        block,
        registers,
        w_entry,
        spilled,
    )?;
    let mut order = Vec::with_capacity(instructions.len());
    let mut cursor = 0usize;
    while cursor != instructions.len() {
        if !super::schedule::is_allocation_schedulable_at(instructions, constraints, cursor) {
            let output = order.len();
            planner.step_scheduled(
                TransitionPoint {
                    output,
                    source: cursor,
                },
                &instructions[cursor],
                &mut remaining,
            )?;
            order.push(cursor);
            cursor += 1;
            continue;
        }

        let start = cursor;
        while cursor != instructions.len()
            && super::schedule::is_allocation_schedulable_at(instructions, constraints, cursor)
        {
            cursor += 1;
        }
        let end = cursor;
        let region = &instructions[start..end];
        let mut ready = super::schedule::ForwardReadyRegion::build(region).ok_or_else(|| {
            SpillPlanError::new(
                "SPILL_PLAN.SCHEDULE_DEPENDENCY",
                Some(func.blocks[block].id),
                Some(start),
                Vec::new(),
                "movable region does not have a forward dependency order",
            )
        })?;
        let mut queue = AllocationReadyQueue::new(region.len());
        let sinks = ready.sinks().to_vec();
        let mut next_sink = 0usize;
        let mut demand = sinks.first().copied().map(DependencyDemand::new);
        for &local in ready.ready() {
            let source = start + local;
            queue.insert(
                local,
                planner.candidate_score(source, &instructions[source], &remaining)?,
            );
        }
        while !ready.is_complete() {
            let demanded = demand.as_mut().and_then(|demand| demand.next_ready(&ready));
            let Some((score, local)) =
                queue.pop(planner.resident.len(), planner.registers, demanded)
            else {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.SCHEDULE_DEPENDENCY",
                    Some(func.blocks[block].id),
                    Some(start),
                    Vec::new(),
                    "movable region has no dependency-ready allocation candidate",
                ));
            };
            let source = start + local;
            if score.blocked_by_deferred_reload {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_RELOAD_ORDER",
                    Some(func.blocks[block].id),
                    Some(source),
                    instructions[source].uses().to_vec(),
                    "every dependency-ready instruction precedes an allocator-selected recipe reload point",
                ));
            }
            let output = order.len();
            let delta = planner.step_scheduled(
                TransitionPoint { output, source },
                &instructions[source],
                &mut remaining,
            )?;
            let newly_ready = ready.emit(local).ok_or_else(|| {
                SpillPlanError::new(
                    "SPILL_PLAN.SCHEDULE_DEPENDENCY",
                    Some(func.blocks[block].id),
                    Some(source),
                    Vec::new(),
                    "selected instruction was not dependency-ready",
                )
            })?;
            order.push(source);
            if demand.as_ref().is_some_and(|demand| demand.target == local) {
                next_sink += 1;
                while sinks
                    .get(next_sink)
                    .is_some_and(|&sink| ready.is_emitted(sink))
                {
                    next_sink += 1;
                }
                demand = sinks.get(next_sink).copied().map(DependencyDemand::new);
            }

            let mut refresh = BTreeSet::<usize>::new();
            for local in newly_ready {
                refresh.insert(local);
            }
            for value in delta
                .resident
                .into_iter()
                .chain(delta.deferred)
                .chain(delta.remaining_uses)
                .chain(delta.continuation)
            {
                for &candidate in ready.use_candidates(VReg(value.0)) {
                    if ready.is_ready(candidate) {
                        refresh.insert(candidate);
                    }
                }
            }
            for local in refresh {
                let source = start + local;
                let candidate =
                    planner.candidate_score(source, &instructions[source], &remaining)?;
                if queue.contains(local) {
                    queue.refresh(local, candidate);
                } else {
                    queue.insert(local, candidate);
                }
            }
        }
    }
    let transition = planner.finish()?;
    if order.iter().copied().eq(0..instructions.len()) {
        // The original order has exactly the original pressure. In the many
        // short blocks that do not move an instruction, avoid materializing
        // the potentially large live-out set and measuring that same order
        // twice merely to establish equality.
        return Ok((transition, order));
    }
    let identity = (0..instructions.len()).collect::<Vec<_>>();
    let exit = &next_use.exit[block];
    let pressure = |order: &[usize]| {
        super::schedule::pressure_cost_with_membership(
            instructions,
            order,
            exit.len(),
            |value| exit.contains_key(value),
            registers,
        )
    };
    if pressure(&order) <= pressure(&identity) {
        return Ok((transition, order));
    }

    // The allocator's ready walk is intentionally local, so it can avoid one
    // immediate eviction by creating a much wider live wavefront later in the
    // block. Give a bottom-up pressure scheduler one chance to recover such a
    // block before preserving source order. The pressure scheduler itself
    // falls back to identity whenever its complete order is worse.
    let fallback = super::schedule::pressure_preferred_block_order(
        instructions,
        constraints,
        exit.keys().copied(),
        registers,
    )
    .unwrap_or(identity);
    let transition = plan_explicit_block_order(
        func,
        next_use,
        planning_recipes,
        logical,
        homes,
        block,
        registers,
        w_entry,
        original_spilled,
        exit_reload_costs,
        &fallback,
    )?;
    Ok((transition, fallback))
}

#[allow(clippy::too_many_arguments)]
fn plan_explicit_block_order(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    logical: &LogicalValues,
    homes: &SpillHomes,
    block: usize,
    registers: usize,
    w_entry: &LogicalSet,
    spilled: LogicalSet,
    exit_reload_costs: &mut dyn ExitReloadCosts,
    order: &[usize],
) -> Result<BlockTransition, SpillPlanError> {
    let instructions = &func.blocks[block].insts;
    let mut remaining = RemainingBlockUses::build(
        func,
        next_use,
        logical,
        block,
        exit_reload_costs,
        Some(order),
    )?;
    let mut planner = BlockTransitionPlanner::new(
        func,
        next_use,
        planning_recipes,
        logical,
        homes,
        block,
        registers,
        w_entry,
        spilled,
    )?;
    for (output, &source) in order.iter().enumerate() {
        let inst = instructions.get(source).ok_or_else(|| {
            SpillPlanError::new(
                "SPILL_PLAN.SCHEDULE_ORDER",
                Some(func.blocks[block].id),
                Some(source),
                Vec::new(),
                "explicit schedule references an instruction outside the block",
            )
        })?;
        planner.step_scheduled(TransitionPoint { output, source }, inst, &mut remaining)?;
    }
    planner.finish()
}

struct BlockTransitionPlanner<'a> {
    func: &'a MFunction,
    next_use: &'a NextUseAnalysis,
    planning_recipes: &'a PlanningRecipes,
    logical: &'a LogicalValues,
    homes: &'a SpillHomes,
    block: usize,
    registers: usize,
    transition: BlockTransition,
    resident: LogicalSet,
    spilled: LogicalSet,
    deferred_recipe_reloads: BTreeMap<LogicalValue, PointUse>,
    last_definition: Option<LogicalValue>,
}

impl<'a> BlockTransitionPlanner<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        func: &'a MFunction,
        next_use: &'a NextUseAnalysis,
        planning_recipes: &'a PlanningRecipes,
        logical: &'a LogicalValues,
        homes: &'a SpillHomes,
        block: usize,
        registers: usize,
        w_entry: &LogicalSet,
        mut spilled: LogicalSet,
    ) -> Result<Self, SpillPlanError> {
        let mut transition = BlockTransition {
            point_ops: Vec::new(),
            recipe_reloads: BTreeSet::new(),
            w_exit: LogicalSet::new(),
            s_exit: LogicalSet::new(),
        };
        let resident = w_entry.clone();
        for phi in &func.blocks[block].phis {
            let value = logical.checked_of(phi.dst, Some(func.blocks[block].id), Some(0))?;
            if !resident.contains(&value) {
                transition.point_ops.push((
                    ProgramPoint {
                        block: func.blocks[block].id,
                        instruction: 0,
                        side: PointSide::Before,
                    },
                    PlannedOp::SpillPhi {
                        value,
                        home: homes.of_logical(value),
                    },
                ));
                spilled.insert(value);
            }
        }
        Ok(Self {
            func,
            next_use,
            planning_recipes,
            logical,
            homes,
            block,
            registers,
            transition,
            resident,
            spilled,
            deferred_recipe_reloads: BTreeMap::new(),
            last_definition: None,
        })
    }

    fn step(&mut self, point: TransitionPoint, inst: &MInst) -> Result<(), SpillPlanError> {
        let mut before = LinearFutureUses {
            func: self.func,
            next_use: self.next_use,
            block: self.block,
            instruction: point.source,
        };
        let uses = self.begin_step(point, inst, &mut before)?;
        let mut after = LinearFutureUses {
            func: self.func,
            next_use: self.next_use,
            block: self.block,
            instruction: point.source + 1,
        };
        self.finish_step(point, inst, &uses, &mut after)
    }

    fn step_scheduled(
        &mut self,
        point: TransitionPoint,
        inst: &MInst,
        remaining: &mut RemainingBlockUses<'_>,
    ) -> Result<ScheduledStepDelta, SpillPlanError> {
        let resident_before = self.resident.iter().copied().collect::<Vec<_>>();
        let deferred_before = self
            .deferred_recipe_reloads
            .iter()
            .map(|(&value, &point)| (value, point))
            .collect::<Vec<_>>();
        let last_definition_before = self.last_definition;
        let uses = {
            let mut before = DynamicFutureUses(remaining);
            self.begin_step(point, inst, &mut before)?
        };
        let remaining_uses = remaining.emit(point.source, inst, self.logical)?;
        {
            let mut after = DynamicFutureUses(remaining);
            self.finish_step(point, inst, &uses, &mut after)?;
        }
        let mut resident = resident_before
            .iter()
            .copied()
            .filter(|value| !self.resident.contains(value))
            .chain(
                self.resident
                    .iter()
                    .copied()
                    .filter(|value| resident_before.binary_search(value).is_err()),
            )
            .collect::<Vec<_>>();
        resident.sort_unstable();

        let mut deferred = Vec::new();
        let (mut before_index, mut after) =
            (0usize, self.deferred_recipe_reloads.iter().peekable());
        while before_index < deferred_before.len() || after.peek().is_some() {
            match (deferred_before.get(before_index), after.peek().copied()) {
                (Some(&(before_value, before_point)), Some((&after_value, &after_point))) => {
                    match before_value.cmp(&after_value) {
                        Ordering::Less => {
                            deferred.push(before_value);
                            before_index += 1;
                        }
                        Ordering::Equal => {
                            if before_point != after_point {
                                deferred.push(before_value);
                            }
                            before_index += 1;
                            after.next();
                        }
                        Ordering::Greater => {
                            deferred.push(after_value);
                            after.next();
                        }
                    }
                }
                (Some(&(before_value, _)), None) => {
                    deferred.push(before_value);
                    before_index += 1;
                }
                (None, Some((&after_value, _))) => {
                    deferred.push(after_value);
                    after.next();
                }
                (None, None) => break,
            }
        }

        let mut continuation = [last_definition_before, self.last_definition]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        continuation.sort_unstable();
        continuation.dedup();
        Ok(ScheduledStepDelta {
            resident,
            deferred,
            remaining_uses,
            continuation,
        })
    }

    fn candidate_score(
        &self,
        source: usize,
        inst: &MInst,
        remaining: &RemainingBlockUses<'_>,
    ) -> Result<AllocationCandidateScore, SpillPlanError> {
        let block_id = self.func.blocks[self.block].id;
        let mut uses = inst
            .uses()
            .iter()
            .copied()
            .map(|value| self.logical.checked_of(value, Some(block_id), Some(source)))
            .collect::<Result<Vec<_>, _>>()?;
        uses.sort_unstable();
        uses.dedup();
        let mut blocked_by_deferred_reload = false;
        let mut missing = 0usize;
        let mut resident_operands = 0usize;
        let mut materialization_cost = 0u32;
        let costs = super::cost::MachineSpillCosts::with_recipes(self.func, self.planning_recipes);
        for &value in &uses {
            if self.resident.contains(&value) {
                resident_operands += 1;
                continue;
            }
            missing += 1;
            let point = PointUse {
                block: block_id,
                instruction: source,
                value: VReg(value.0),
            };
            if let Some(&expected) = self.deferred_recipe_reloads.get(&value) {
                if expected != point {
                    blocked_by_deferred_reload = true;
                    continue;
                }
                materialization_cost = materialization_cost.saturating_add(u32::from(
                    self.planning_recipes
                        .point_specific_materialization_cost(point)
                        .unwrap_or_else(|| costs.persistent_reload(VReg(value.0))),
                ));
            } else {
                materialization_cost = materialization_cost
                    .saturating_add(u32::from(costs.persistent_reload(VReg(value.0))));
            }
        }
        let dying = uses
            .iter()
            .filter(|&&value| remaining.remaining_uses(value) == 1 && !remaining.is_live_out(value))
            .count();
        let definition_live = inst
            .def()
            .map(|value| self.logical.checked_of(value, Some(block_id), Some(source)))
            .transpose()?
            .is_some_and(|value| {
                remaining.remaining_uses(value) != 0 || remaining.is_live_out(value)
            });
        let pressure_delta = isize::try_from(missing + usize::from(definition_live))
            .unwrap_or(isize::MAX)
            .saturating_sub(isize::try_from(dying).unwrap_or(isize::MAX));
        Ok(AllocationCandidateScore {
            blocked_by_deferred_reload,
            pressure_delta,
            continuation_tie: std::cmp::Reverse(
                self.last_definition
                    .is_some_and(|value| uses.contains(&value)),
            ),
            // Reusing an operand already in W closes the residency cluster
            // which made that value worth keeping.  Rank this before a new
            // root with no operands; otherwise a stream of cheap Loads fills
            // W, spills those very results, and only then visits their uses.
            resident_operand_tie: std::cmp::Reverse(resident_operands),
            // Once W is full, an operand-bearing instruction closes existing
            // work (resident, spilled, or rematerializable).  A zero-input
            // root only creates another value which must displace such work.
            operand_tie: std::cmp::Reverse(uses.len()),
            materialization_cost,
            // Preserve the incoming ISel order as the deterministic final
            // tie-breaker after dependency locality and the current W/S
            // transition have made no distinction.
            preferred_rank: source,
            source,
        })
    }

    fn begin_step(
        &mut self,
        point: TransitionPoint,
        inst: &MInst,
        future_uses: &mut impl FutureUses,
    ) -> Result<Vec<LogicalValue>, SpillPlanError> {
        let block_id = self.func.blocks[self.block].id;
        let mut uses = inst
            .uses()
            .into_iter()
            .map(|value| {
                self.logical
                    .checked_of(value, Some(block_id), Some(point.output))
            })
            .collect::<Result<Vec<_>, _>>()?;
        uses.sort_unstable();
        uses.dedup();
        for &value in &uses {
            if self.resident.insert(value) {
                if let Some(expected) = self.deferred_recipe_reloads.remove(&value) {
                    let actual = PointUse {
                        block: block_id,
                        instruction: point.source,
                        value: VReg(value.0),
                    };
                    if expected != actual {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.RECIPE_RELOAD_POINT",
                            Some(block_id),
                            Some(point.output),
                            vec![VReg(value.0)],
                            format!(
                                "deferred recipe reload expected {expected:?} but reached {actual:?}"
                            ),
                        ));
                    }
                    self.transition
                        .recipe_reloads
                        .insert((block_id, point.output, value));
                }
                self.transition.point_ops.push((
                    ProgramPoint {
                        block: block_id,
                        instruction: point.output,
                        side: PointSide::Before,
                    },
                    PlannedOp::Reload {
                        value,
                        home: self.homes.of_logical(value),
                    },
                ));
            }
        }
        limit(
            self.func,
            self.planning_recipes,
            self.homes,
            &mut self.transition.point_ops,
            self.block,
            point.output,
            self.registers,
            &uses,
            &mut self.resident,
            &mut self.spilled,
            &mut self.deferred_recipe_reloads,
            future_uses,
        )?;
        Ok(uses)
    }

    fn finish_step(
        &mut self,
        point: TransitionPoint,
        inst: &MInst,
        uses: &[LogicalValue],
        future_uses: &mut impl FutureUses,
    ) -> Result<(), SpillPlanError> {
        let block_id = self.func.blocks[self.block].id;
        let clobbered = super::assignment::allocatable_clobber_count(inst, self.func);
        if clobbered > self.registers {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.CLOBBER_CAPACITY",
                Some(block_id),
                Some(point.output),
                inst.uses().to_vec(),
                format!(
                    "instruction clobbers {clobbered} registers but the allocator has only {}",
                    self.registers
                ),
            ));
        }
        if clobbered != 0 {
            limit_live_through_clobber(
                self.func,
                self.planning_recipes,
                self.homes,
                &mut self.transition.point_ops,
                self.block,
                point.output,
                self.registers.saturating_sub(clobbered),
                &mut self.resident,
                &mut self.spilled,
                &mut self.deferred_recipe_reloads,
                future_uses,
            )?;
        }
        if let Some(definition) = inst.def() {
            let definition =
                self.logical
                    .checked_of(definition, Some(block_id), Some(point.output))?;
            if !self.resident.contains(&definition) && self.resident.len() == self.registers {
                let Some(maximum) = self.registers.checked_sub(1) else {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.OPERAND_PRESSURE",
                        Some(block_id),
                        Some(point.output),
                        vec![VReg(definition.0)],
                        "an instruction result requires a register but no registers are available",
                    ));
                };
                limit(
                    self.func,
                    self.planning_recipes,
                    self.homes,
                    &mut self.transition.point_ops,
                    self.block,
                    point.output,
                    maximum,
                    uses,
                    &mut self.resident,
                    &mut self.spilled,
                    &mut self.deferred_recipe_reloads,
                    future_uses,
                )?;
            }
            self.resident.insert(definition);
            self.last_definition = Some(definition);
        } else {
            self.last_definition = None;
        }
        self.resident
            .retain(|value| !future_uses.distance(*value).is_dead());
        Ok(())
    }

    fn finish(mut self) -> Result<BlockTransition, SpillPlanError> {
        if !self.deferred_recipe_reloads.is_empty() {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.RECIPE_RELOAD_POINT",
                Some(self.func.blocks[self.block].id),
                None,
                self.deferred_recipe_reloads
                    .keys()
                    .map(|value| VReg(value.0))
                    .collect(),
                "deferred recipe reload did not reach its final local use",
            ));
        }
        self.transition.w_exit = self.resident;
        self.transition.s_exit = self.spilled;
        Ok(self.transition)
    }
}

#[allow(clippy::too_many_arguments)]
fn limit_live_through_clobber(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    homes: &SpillHomes,
    point_ops: &mut Vec<(ProgramPoint, PlannedOp)>,
    block: usize,
    point_instruction: usize,
    capacity: usize,
    resident: &mut LogicalSet,
    spilled: &mut LogicalSet,
    deferred_recipe_reloads: &mut BTreeMap<LogicalValue, PointUse>,
    future_uses: &mut impl FutureUses,
) -> Result<(), SpillPlanError> {
    let mut live_through = resident
        .iter()
        .copied()
        .filter(|value| !future_uses.distance(*value).is_dead())
        .collect::<BTreeSet<_>>();
    while live_through.len() > capacity {
        let Some(victim) = live_through.iter().copied().max_by(|left, right| {
            compare_eviction_candidates(
                func,
                planning_recipes,
                spilled,
                future_uses,
                (*left, future_uses.distance(*left)),
                (*right, future_uses.distance(*right)),
            )
        }) else {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.MIN_VICTIM",
                Some(func.blocks[block].id),
                Some(point_instruction),
                Vec::new(),
                "clobber pressure exceeded capacity but MIN had no live-through victim",
            ));
        };
        evict_value(
            func,
            planning_recipes,
            homes,
            point_ops,
            block,
            point_instruction,
            victim,
            spilled,
            deferred_recipe_reloads,
            future_uses,
        )?;
        live_through.remove(&victim);
        resident.remove(&victim);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn init_usual(
    func: &MFunction,
    cfg: &NormalizedCfg,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    plan: &SpillPlan,
    edge_translations: &EdgeTranslations,
    block: usize,
    registers: usize,
) -> LogicalSet {
    let processed = cfg.predecessors[block]
        .iter()
        .copied()
        .filter(|predecessor| *predecessor < block)
        .collect::<Vec<_>>();
    if processed.is_empty() {
        return LogicalSet::new();
    }
    if processed.len() == 1 && cfg.predecessors[block].len() == 1 {
        let predecessor = processed[0];
        return plan.w_exit[predecessor]
            .iter()
            .copied()
            .flat_map(|value| edge_translations.to_successors(predecessor, block, value))
            .collect();
    }
    let mut frequency = HashMap::<LogicalValue, usize>::default();
    for predecessor in &processed {
        for &value in &plan.w_exit[*predecessor] {
            for value in edge_translations.to_successors(*predecessor, block, value) {
                *frequency.entry(value).or_default() += 1;
            }
        }
    }
    let mut candidates = frequency
        .keys()
        .copied()
        .filter_map(|value| {
            let mut keep_cost = 0u128;
            let mut drop_cost = 0u128;
            for &predecessor in &processed {
                let predecessor_value = edge_translations.to_predecessor(predecessor, block, value);
                if plan.w_exit[predecessor].contains(&predecessor_value) {
                    if !plan.s_exit[predecessor].contains(&predecessor_value) {
                        drop_cost = drop_cost
                            .saturating_add(u128::from(spill_cost(func, predecessor_value)));
                    }
                } else {
                    keep_cost = keep_cost.saturating_add(u128::from(reload_cost_on_edge(
                        func,
                        planning_recipes,
                        predecessor,
                        block,
                        predecessor_value,
                    )));
                }
            }
            if next_use.anticipated_at_entry(block, VReg(value.0)) {
                drop_cost = drop_cost.saturating_add((processed.len() as u128).saturating_mul(
                    local_use_cluster_cost(func, next_use, planning_recipes, block, 0, value),
                ));
            }
            let savings = drop_cost
                .checked_sub(keep_cost)
                .filter(|saving| *saving != 0)?;
            Some((
                value,
                savings,
                logical_entry_distance(func, next_use, block, value),
            ))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| compare_join_retention_candidates(*left, *right));
    candidates
        .into_iter()
        .take(registers)
        .map(|(value, _, _)| value)
        .collect()
}

/// Prefer the values for which entry residency avoids the most coupling and
/// guaranteed-use work per instruction of live-range occupancy.  Loop exits
/// dominate straight-line distance, matching global next-use ordering.
fn compare_join_retention_candidates(
    left: (LogicalValue, u128, NextUseDistance),
    right: (LogicalValue, u128, NextUseDistance),
) -> Ordering {
    match (left.2, right.2) {
        (NextUseDistance::Dead, NextUseDistance::Dead) => left.0.cmp(&right.0),
        (NextUseDistance::Dead, _) => Ordering::Greater,
        (_, NextUseDistance::Dead) => Ordering::Less,
        (
            NextUseDistance::Finite {
                loop_exits: left_exits,
                instructions: left_instructions,
            },
            NextUseDistance::Finite {
                loop_exits: right_exits,
                instructions: right_instructions,
            },
        ) => left_exits.cmp(&right_exits).then_with(|| {
            let left_span = left_instructions as u128 + 1;
            let right_span = right_instructions as u128 + 1;
            right
                .1
                .saturating_mul(left_span)
                .cmp(&left.1.saturating_mul(right_span))
                .then_with(|| left_instructions.cmp(&right_instructions))
                .then_with(|| left.0.cmp(&right.0))
        }),
    }
}

fn init_loop_region(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    plan: &SpillPlan,
    block: usize,
    region: usize,
    registers: usize,
) -> Result<LogicalSet, SpillPlanError> {
    let mut alive = next_use.entry[block]
        .keys()
        .copied()
        .map(|value| {
            plan.logical
                .checked_of(value, Some(func.blocks[block].id), Some(0))
        })
        .collect::<Result<LogicalSet, _>>()?;
    for phi in &func.blocks[block].phis {
        alive.insert(
            plan.logical
                .checked_of(phi.dst, Some(func.blocks[block].id), Some(0))?,
        );
    }
    let Some(facts) = next_use.loop_regions.get(region) else {
        return Err(SpillPlanError::new(
            "SPILL_PLAN.NEXT_USE_REGION",
            Some(func.blocks[block].id),
            Some(0),
            Vec::new(),
            format!("next-use analysis references absent loop region {region}"),
        ));
    };
    let mut candidates = BinaryHeap::new();
    let mut live_through_count = 0;
    for &value in &alive {
        if next_use.used_in_region(region, VReg(value.0)) {
            let distance = logical_entry_distance(func, next_use, block, value);
            retain_closest(&mut candidates, registers, (distance, value));
        } else {
            live_through_count += 1;
        }
    }
    if candidates.len() >= registers {
        return Ok(candidates
            .into_sorted_vec()
            .into_iter()
            .map(|(_, value)| value)
            .collect());
    }
    let internal_pressure = facts.max_pressure.saturating_sub(live_through_count);
    let free_loop = registers.saturating_sub(internal_pressure);
    let mut live_through = BinaryHeap::new();
    if free_loop > 0 {
        for value in alive {
            if !next_use.used_in_region(region, VReg(value.0)) {
                let distance = logical_entry_distance(func, next_use, block, value);
                retain_closest(&mut live_through, free_loop, (distance, value));
            }
        }
    }
    Ok(candidates
        .into_sorted_vec()
        .into_iter()
        .chain(live_through.into_sorted_vec().into_iter().take(free_loop))
        .take(registers)
        .map(|(_, value)| value)
        .collect())
}

/// Keep only the register-sized prefix of the distance ordering. Logical IDs
/// break ties exactly as stable sorting the original ascending set did.
fn retain_closest(
    closest: &mut BinaryHeap<(NextUseDistance, LogicalValue)>,
    limit: usize,
    candidate: (NextUseDistance, LogicalValue),
) {
    if closest.len() < limit {
        closest.push(candidate);
    } else if let Some(mut furthest) = closest.peek_mut()
        && candidate < *furthest
    {
        *furthest = candidate;
    }
}

trait FutureUses {
    fn distance(&self, value: LogicalValue) -> NextUseDistance;
    fn next_point(&self, value: LogicalValue) -> Option<PointUse>;
    fn exit_reload_cost(&mut self, _value: LogicalValue) -> u32 {
        0
    }
}

struct LinearFutureUses<'a> {
    func: &'a MFunction,
    next_use: &'a NextUseAnalysis,
    block: usize,
    instruction: usize,
}

impl FutureUses for LinearFutureUses<'_> {
    fn distance(&self, value: LogicalValue) -> NextUseDistance {
        self.next_use
            .distance_at(self.func, self.block, self.instruction, VReg(value.0))
    }

    fn next_point(&self, value: LogicalValue) -> Option<PointUse> {
        let instruction =
            self.next_use
                .next_local_use(self.block, self.instruction, VReg(value.0))?;
        Some(PointUse {
            block: self.func.blocks[self.block].id,
            instruction,
            value: VReg(value.0),
        })
    }
}

#[derive(Default)]
struct RemainingUses {
    points: std::ops::Range<usize>,
    next: usize,
    count: usize,
}

struct RemainingBlockUses<'a> {
    block: BlockId,
    preferred_rank: Vec<usize>,
    remaining: HashMap<LogicalValue, RemainingUses>,
    /// All use positions of this block, partitioned into one range per value.
    /// One allocation replaces a separate growable vector for every register.
    points: Vec<(usize, usize)>,
    exit: &'a DistanceMap,
    exit_reload_costs: &'a mut dyn ExitReloadCosts,
    emitted: Vec<bool>,
    emitted_count: usize,
}

impl<'a> RemainingBlockUses<'a> {
    fn build(
        func: &MFunction,
        next_use: &'a NextUseAnalysis,
        logical: &LogicalValues,
        block: usize,
        exit_reload_costs: &'a mut dyn ExitReloadCosts,
        preferred_order: Option<&[usize]>,
    ) -> Result<Self, SpillPlanError> {
        let instructions = func.blocks[block].insts.len();
        let preferred_rank = if let Some(order) = preferred_order {
            if order.len() != instructions {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.SCHEDULE_ORDER",
                    Some(func.blocks[block].id),
                    None,
                    Vec::new(),
                    "explicit order does not cover every block instruction",
                ));
            }
            let mut ranks = vec![usize::MAX; instructions];
            for (rank, &source) in order.iter().enumerate() {
                let Some(slot) = ranks.get_mut(source) else {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.SCHEDULE_ORDER",
                        Some(func.blocks[block].id),
                        Some(source),
                        Vec::new(),
                        "explicit order references an instruction outside the block",
                    ));
                };
                if std::mem::replace(slot, rank) != usize::MAX {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.SCHEDULE_ORDER",
                        Some(func.blocks[block].id),
                        Some(source),
                        Vec::new(),
                        "explicit order references one instruction more than once",
                    ));
                }
            }
            ranks
        } else {
            (0..instructions).collect::<Vec<_>>()
        };
        let mut remaining = HashMap::<LogicalValue, RemainingUses>::default();
        let mut point_count = 0;
        for (source, inst) in func.blocks[block].insts.iter().enumerate() {
            for value in inst.uses().sorted_unique() {
                let value = logical.checked_of(value, Some(func.blocks[block].id), Some(source))?;
                remaining.entry(value).or_default().count += 1;
                point_count += 1;
            }
        }
        // Count, reserve disjoint ranges, then fill the block-local arena.
        // Range placement is independent of scheduling; each range keeps the
        // exact (preferred rank, source instruction) ordering used previously.
        let mut points = vec![(0, 0); point_count];
        let mut start = 0;
        for uses in remaining.values_mut() {
            uses.points = start..start + uses.count;
            start = uses.points.end;
        }
        for (source, inst) in func.blocks[block].insts.iter().enumerate() {
            for value in inst.uses().sorted_unique() {
                let value = logical.checked_of(value, Some(func.blocks[block].id), Some(source))?;
                let uses = remaining.get_mut(&value).expect("counted use");
                points[uses.points.start + uses.next] = (preferred_rank[source], source);
                uses.next += 1;
            }
        }
        for uses in remaining.values_mut() {
            points[uses.points.clone()].sort_unstable();
            uses.next = 0;
        }
        if let Some(value) = next_use.exit[block].first_out_of_range(logical.count) {
            logical.checked_of(value, Some(func.blocks[block].id), Some(instructions))?;
        }
        Ok(Self {
            block: func.blocks[block].id,
            preferred_rank,
            remaining,
            points,
            exit: &next_use.exit[block],
            exit_reload_costs,
            emitted: vec![false; instructions],
            emitted_count: 0,
        })
    }

    fn emit(
        &mut self,
        source: usize,
        inst: &MInst,
        logical: &LogicalValues,
    ) -> Result<Vec<LogicalValue>, SpillPlanError> {
        if source >= self.emitted.len() || std::mem::replace(&mut self.emitted[source], true) {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.SCHEDULE_ORDER",
                Some(self.block),
                Some(source),
                Vec::new(),
                "a source instruction was committed more than once",
            ));
        }
        self.emitted_count += 1;
        let uses = inst.uses().sorted_unique();
        let mut changed = Vec::with_capacity(uses.len());
        for value in uses {
            let value = logical.checked_of(value, Some(self.block), Some(source))?;
            let uses = self.remaining.get_mut(&value).ok_or_else(|| {
                SpillPlanError::new(
                    "SPILL_PLAN.SCHEDULE_ORDER",
                    Some(self.block),
                    Some(source),
                    vec![VReg(value.0)],
                    "committed use has no remaining-use entry",
                )
            })?;
            let point = (self.preferred_rank[source], source);
            let points = &self.points[uses.points.clone()];
            if points.binary_search(&point).is_err() || uses.count == 0 {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.SCHEDULE_ORDER",
                    Some(self.block),
                    Some(source),
                    vec![VReg(value.0)],
                    "committed use was absent from its remaining-use set",
                ));
            }
            uses.count -= 1;
            while points
                .get(uses.next)
                .is_some_and(|&(_, instruction)| self.emitted[instruction])
            {
                uses.next += 1;
            }
            // candidate_score only observes use counts through last-use
            // pressure (and whether a definition is live). Keep the full
            // use/next-use state current above, but do not invalidate every
            // ready consumer on decrements such as 100 -> 99. With F users,
            // those notifications otherwise cause F^2 use-list visits.
            // Live-out values never die within this block.
            if uses.count <= 1 && !self.exit.contains_key(&VReg(value.0)) {
                changed.push(value);
            }
        }
        Ok(changed)
    }

    fn remaining_uses(&self, value: LogicalValue) -> usize {
        self.remaining.get(&value).map_or(0, |uses| uses.count)
    }

    fn is_live_out(&self, value: LogicalValue) -> bool {
        self.exit.contains_key(&VReg(value.0))
    }

    fn distance(&self, value: LogicalValue) -> NextUseDistance {
        if let Some((rank, _)) = self
            .remaining
            .get(&value)
            .and_then(|uses| self.points[uses.points.clone()].get(uses.next))
            .copied()
        {
            return NextUseDistance::Finite {
                loop_exits: 0,
                instructions: rank.saturating_sub(self.emitted_count),
            };
        }
        let remaining_instructions = self.emitted.len().saturating_sub(self.emitted_count);
        match self.exit.get(&VReg(value.0)) {
            Some(NextUseDistance::Finite {
                loop_exits,
                instructions,
            }) => NextUseDistance::Finite {
                loop_exits,
                instructions: instructions.saturating_add(remaining_instructions),
            },
            _ => NextUseDistance::Dead,
        }
    }

    fn next_point(&self, value: LogicalValue) -> Option<PointUse> {
        let uses = self.remaining.get(&value)?;
        let &(_, instruction) = self.points[uses.points.clone()].get(uses.next)?;
        Some(PointUse {
            block: self.block,
            instruction,
            value: VReg(value.0),
        })
    }

    fn exit_reload_cost(&mut self, value: LogicalValue) -> u32 {
        self.exit_reload_costs.cost(value)
    }
}

struct DynamicFutureUses<'view, 'data>(&'view mut RemainingBlockUses<'data>);

impl FutureUses for DynamicFutureUses<'_, '_> {
    fn distance(&self, value: LogicalValue) -> NextUseDistance {
        self.0.distance(value)
    }

    fn next_point(&self, value: LogicalValue) -> Option<PointUse> {
        self.0.next_point(value)
    }

    fn exit_reload_cost(&mut self, value: LogicalValue) -> u32 {
        self.0.exit_reload_cost(value)
    }
}

#[allow(clippy::too_many_arguments)]
fn limit(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    homes: &SpillHomes,
    point_ops: &mut Vec<(ProgramPoint, PlannedOp)>,
    block: usize,
    point_instruction: usize,
    maximum: usize,
    pinned: &[LogicalValue],
    resident: &mut LogicalSet,
    spilled: &mut LogicalSet,
    deferred_recipe_reloads: &mut BTreeMap<LogicalValue, PointUse>,
    future_uses: &mut impl FutureUses,
) -> Result<(), SpillPlanError> {
    while resident.len() > maximum {
        let Some(victim) = resident
            .iter()
            .copied()
            .filter(|value| !pinned.contains(value))
            .max_by(|left, right| {
                compare_eviction_candidates(
                    func,
                    planning_recipes,
                    spilled,
                    future_uses,
                    (*left, future_uses.distance(*left)),
                    (*right, future_uses.distance(*right)),
                )
            })
        else {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.OPERAND_PRESSURE",
                Some(func.blocks[block].id),
                Some(point_instruction),
                pinned.iter().map(|value| VReg(value.0)).collect(),
                format!(
                    "{} simultaneously pinned operands exceed the {maximum}-register capacity",
                    pinned.len()
                ),
            ));
        };
        evict_value(
            func,
            planning_recipes,
            homes,
            point_ops,
            block,
            point_instruction,
            victim,
            spilled,
            deferred_recipe_reloads,
            future_uses,
        )?;
        resident.remove(&victim);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn evict_value(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    homes: &SpillHomes,
    point_ops: &mut Vec<(ProgramPoint, PlannedOp)>,
    block: usize,
    point_instruction: usize,
    value: LogicalValue,
    spilled: &mut LogicalSet,
    deferred_recipe_reloads: &mut BTreeMap<LogicalValue, PointUse>,
    future_uses: &mut impl FutureUses,
) -> Result<(), SpillPlanError> {
    if spilled.contains(&value) {
        return Ok(());
    }
    if let Some((point, recipe_cost)) = next_use_point_recipe(planning_recipes, value, future_uses)
    {
        let stack_cost =
            spill_cost(func, value).saturating_add(reload_cost(func, planning_recipes, value));
        if recipe_cost < stack_cost {
            if let Some(previous) = deferred_recipe_reloads.insert(value, point) {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_RELOAD_POINT",
                    Some(func.blocks[block].id),
                    Some(point_instruction),
                    vec![VReg(value.0)],
                    format!("logical value already had deferred recipe reload {previous:?}"),
                ));
            }
            return Ok(());
        }
    }
    spilled.insert(value);
    point_ops.push((
        ProgramPoint {
            block: func.blocks[block].id,
            instruction: point_instruction,
            side: PointSide::Before,
        },
        PlannedOp::Spill {
            value,
            home: homes.of_logical(value),
        },
    ));
    Ok(())
}

/// Return the exact MemorySSA recipe at the next local use.
///
/// One eviction-to-reload interval is an independent residency cluster.  The
/// value does not need a persistent stack home merely because it has another
/// use after that reload: once reloaded it is resident again, and a later
/// eviction makes a fresh home decision against the MemorySSA version at that
/// later use.
fn next_use_point_recipe(
    planning_recipes: &PlanningRecipes,
    value: LogicalValue,
    future_uses: &mut impl FutureUses,
) -> Option<(PointUse, u16)> {
    let point = future_uses.next_point(value)?;
    planning_recipes
        .point_specific_materialization_cost(point)
        .map(|cost| (point, cost))
}

/// Compare two possible split points by the cost density of keeping the value
/// resident until its next use.  Braun--Hack MIN is the equal-cost special
/// case: with equal spill/reload costs, the farther next use is still evicted.
/// For target values with different rematerialization and memory costs, the
/// numerator is the machine-instruction cost avoided by retaining the value,
/// and the denominator is the register occupancy until that use.
fn compare_eviction_candidates(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    spilled: &LogicalSet,
    future_uses: &mut impl FutureUses,
    left: (LogicalValue, NextUseDistance),
    right: (LogicalValue, NextUseDistance),
) -> Ordering {
    match (left.1, right.1) {
        (NextUseDistance::Dead, NextUseDistance::Dead) => left.0.cmp(&right.0),
        (NextUseDistance::Dead, _) => Ordering::Greater,
        (_, NextUseDistance::Dead) => Ordering::Less,
        (
            NextUseDistance::Finite {
                loop_exits: left_exits,
                instructions: left_instructions,
            },
            NextUseDistance::Finite {
                loop_exits: right_exits,
                instructions: right_instructions,
            },
        ) => left_exits.cmp(&right_exits).then_with(|| {
            let left_cost =
                eviction_cost(func, planning_recipes, spilled, left.0, future_uses) as u128;
            let right_cost =
                eviction_cost(func, planning_recipes, spilled, right.0, future_uses) as u128;
            let left_span = left_instructions as u128 + 1;
            let right_span = right_instructions as u128 + 1;
            // Lower avoided-cost density is the better eviction candidate.
            // Cross multiplication keeps the decision deterministic and free
            // of floating-point rounding.
            (right_cost * left_span)
                .cmp(&(left_cost * right_span))
                .then_with(|| left_instructions.cmp(&right_instructions))
                .then_with(|| left.0.cmp(&right.0))
        }),
    }
}

fn eviction_cost(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    spilled: &LogicalSet,
    value: LogicalValue,
    future_uses: &mut impl FutureUses,
) -> u32 {
    let has_persistent_home = spilled.contains(&value);
    let local_reload =
        reload_cost_at_next_local_use(func, planning_recipes, value, future_uses).map(u32::from);
    let reload_cost = local_reload.unwrap_or_else(|| {
        let exit_cost = future_uses.exit_reload_cost(value);
        if exit_cost == 0 {
            u32::from(reload_cost(func, planning_recipes, value))
        } else {
            exit_cost
        }
    });
    let persistent_cost = reload_cost.saturating_add(if has_persistent_home {
        0
    } else {
        u32::from(spill_cost(func, value))
    });
    if has_persistent_home {
        return persistent_cost;
    }
    next_use_point_recipe(planning_recipes, value, future_uses)
        .map_or(persistent_cost, |(_, recipe_cost)| {
            persistent_cost.min(u32::from(recipe_cost))
        })
}

fn reload_cost_at_next_local_use(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    value: LogicalValue,
    future_uses: &mut impl FutureUses,
) -> Option<u16> {
    let costs = super::cost::MachineSpillCosts::with_recipes(func, planning_recipes);
    future_uses
        .next_point(value)
        .map(|point| costs.reload_at_point(point))
}

/// Cost the complete guaranteed straight-line use cluster for a value which
/// is absent at block entry.  Point-specific MemorySSA recipes may differ
/// between uses, so every concrete point is queried independently.  If the
/// first guaranteed use lies beyond this block, retain the existing
/// cross-block fallback rather than assigning a zero cost.
fn local_use_cluster_cost(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    planning_recipes: &PlanningRecipes,
    block: usize,
    instruction: usize,
    value: LogicalValue,
) -> u128 {
    let value = VReg(value.0);
    let uses = next_use.local_uses_from(block, instruction, value);
    if uses.is_empty() {
        return u128::from(reload_cost(func, planning_recipes, LogicalValue(value.0)));
    }
    let costs = super::cost::MachineSpillCosts::with_recipes(func, planning_recipes);
    uses.iter().fold(0u128, |cost, &instruction| {
        let reload = costs.reload_at_point(PointUse {
            block: func.blocks[block].id,
            instruction,
            value,
        });
        cost.saturating_add(u128::from(reload))
    })
}

fn reload_cost_on_edge(
    func: &MFunction,
    planning_recipes: &PlanningRecipes,
    predecessor: usize,
    successor: usize,
    value: LogicalValue,
) -> u16 {
    let value = VReg(value.0);
    super::cost::MachineSpillCosts::with_recipes(func, planning_recipes).reload_on_edge(EdgeUse {
        predecessor: func.blocks[predecessor].id,
        successor: func.blocks[successor].id,
        value,
    })
}

fn reload_cost(func: &MFunction, planning_recipes: &PlanningRecipes, value: LogicalValue) -> u16 {
    super::cost::MachineSpillCosts::with_recipes(func, planning_recipes)
        .persistent_reload(VReg(value.0))
}

fn spill_cost(func: &MFunction, value: LogicalValue) -> u16 {
    super::cost::MachineSpillCosts::from_descriptors(func).spill(VReg(value.0))
}

fn logical_entry_distance(
    func: &MFunction,
    next_use: &NextUseAnalysis,
    block: usize,
    value: LogicalValue,
) -> NextUseDistance {
    next_use.distance_at(func, block, 0, VReg(value.0))
}

impl SpillPlan {
    /// Keep only states queried after scheduling. Reconstruction still needs
    /// W_entry for phi placement, but S_exit is only queried for phi sources.
    /// Full tables must be retained when spill-plan verification is enabled.
    pub(super) fn retain_reconstruction_states(
        &mut self,
        func: &MFunction,
        cfg: &NormalizedCfg,
    ) -> Result<(), SpillPlanError> {
        let mut exits = vec![LogicalSet::new(); func.blocks.len()];
        for block in &func.blocks {
            for phi in &block.phis {
                for &(predecessor, source) in &phi.sources {
                    let previous = cfg.block_index.get(&predecessor).copied().ok_or_else(|| {
                        SpillPlanError::new(
                            "SPILL_PLAN.RECONSTRUCTION_STATE",
                            Some(block.id),
                            None,
                            vec![phi.dst, source],
                            "phi predecessor is outside the normalized CFG",
                        )
                    })?;
                    let state = self.s_exit.get(previous).ok_or_else(|| {
                        SpillPlanError::new(
                            "SPILL_PLAN.RECONSTRUCTION_STATE",
                            Some(predecessor),
                            None,
                            vec![source],
                            "phi predecessor has no spill exit state",
                        )
                    })?;
                    let source = self.logical.of(source);
                    if state.contains(&source) {
                        exits[previous].insert(source);
                    }
                }
            }
        }
        self.s_exit = exits;
        self.s_entry = Vec::new();
        self.w_exit = Vec::new();
        Ok(())
    }

    /// Finalize whole-home rematerialization after the W/S plan has exposed
    /// every concrete point and edge reload.  Reconstruction must materialize
    /// this decision; it may no longer infer a different home kind on its own.
    pub(super) fn select_recipe_homes(
        &mut self,
        func: &MFunction,
        cfg: &NormalizedCfg,
        analysis: &ReloadRecipeAnalysis,
    ) -> Result<(), SpillPlanError> {
        let base_costs = super::cost::MachineSpillCosts::from_descriptors(func);
        let mut candidates = BTreeSet::<SpillHome>::new();
        let mut rejected = BTreeSet::<SpillHome>::new();
        let mut baseline_costs = BTreeMap::<SpillHome, u128>::new();
        let mut recipe_costs = BTreeMap::<SpillHome, u128>::new();
        for &(point, operation) in &self.point_ops {
            match operation {
                PlannedOp::Reload { value, home } => {
                    candidates.insert(home);
                    let query = PointUse {
                        block: point.block,
                        instruction: point.instruction,
                        value: VReg(value.0),
                    };
                    if let Some(recipe) = analysis.resolved_recipe_at_point(query) {
                        let cost = u128::try_from(recipe.steps.len().saturating_add(1))
                            .unwrap_or(u128::MAX);
                        let baseline = baseline_costs.entry(home).or_default();
                        *baseline = baseline.saturating_add(
                            if self.recipe_reloads.contains(&(
                                point.block,
                                point.instruction,
                                value,
                            )) {
                                cost
                            } else {
                                u128::from(base_costs.persistent_reload(VReg(value.0)))
                            },
                        );
                        let total = recipe_costs.entry(home).or_default();
                        *total = total.saturating_add(cost);
                    } else {
                        let baseline = baseline_costs.entry(home).or_default();
                        *baseline = baseline.saturating_add(u128::from(
                            base_costs.persistent_reload(VReg(value.0)),
                        ));
                        rejected.insert(home);
                    }
                }
                PlannedOp::Spill { value, home } => {
                    let total = baseline_costs.entry(home).or_default();
                    *total = total.saturating_add(u128::from(spill_cost(func, value)));
                }
                PlannedOp::SpillPhi { value, home } => {
                    let Some(&block) = cfg.block_index.get(&point.block) else {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.RECIPE_HOME_PHI",
                            Some(point.block),
                            Some(point.instruction),
                            vec![VReg(value.0)],
                            "recipe-home SpillPhi block is outside the normalized CFG",
                        ));
                    };
                    let Some(phi) = func.blocks[block]
                        .phis
                        .iter()
                        .find(|phi| phi.dst.0 == value.0)
                    else {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.RECIPE_HOME_PHI",
                            Some(point.block),
                            Some(point.instruction),
                            vec![VReg(value.0)],
                            "recipe-home SpillPhi has no matching MIR phi",
                        ));
                    };
                    for &(predecessor, source) in &phi.sources {
                        let Some(&predecessor) = cfg.block_index.get(&predecessor) else {
                            return Err(SpillPlanError::new(
                                "SPILL_PLAN.RECIPE_HOME_PHI",
                                Some(point.block),
                                Some(point.instruction),
                                vec![VReg(value.0), source],
                                "recipe-home SpillPhi source is outside the normalized CFG",
                            ));
                        };
                        let source = LogicalValue(source.0);
                        if !self.s_exit[predecessor].contains(&source) {
                            let total = baseline_costs.entry(home).or_default();
                            *total = total.saturating_add(u128::from(spill_cost(func, source)));
                        }
                    }
                }
            }
        }
        for (&(predecessor, successor), operations) in &self.edge_ops {
            let Some(predecessor_block) = func.blocks.get(predecessor) else {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_HOME_EDGE",
                    None,
                    None,
                    Vec::new(),
                    format!("recipe-home predecessor index {predecessor} is outside function"),
                ));
            };
            if func.blocks.get(successor).is_none() {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_HOME_EDGE",
                    Some(predecessor_block.id),
                    None,
                    Vec::new(),
                    format!("recipe-home successor index {successor} is outside function"),
                ));
            }
            let insertion = super::cfg::edge_insertion_point(func, cfg, predecessor, successor)
                .ok_or_else(|| {
                    SpillPlanError::new(
                        "SPILL_PLAN.RECIPE_HOME_EDGE",
                        Some(predecessor_block.id),
                        None,
                        Vec::new(),
                        "recipe-home edge has no single-edge materialization point",
                    )
                })?;
            let insertion_block = &func.blocks[insertion.block];
            if insertion.instruction >= insertion_block.insts.len() {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_HOME_EDGE",
                    Some(insertion_block.id),
                    Some(insertion.instruction),
                    Vec::new(),
                    "recipe-home edge insertion point is outside its MIR block",
                ));
            }
            for &operation in operations {
                match operation {
                    PlannedEdgeOp::Reload {
                        source,
                        source_home,
                        ..
                    } => {
                        candidates.insert(source_home);
                        let total = baseline_costs.entry(source_home).or_default();
                        *total = total.saturating_add(u128::from(
                            base_costs.persistent_reload(VReg(source.0)),
                        ));
                        let query = PointUse {
                            block: insertion_block.id,
                            instruction: insertion.instruction,
                            value: VReg(source.0),
                        };
                        if let Some(recipe) = analysis.resolved_recipe_at_point(query) {
                            let cost = u128::try_from(recipe.steps.len().saturating_add(1))
                                .unwrap_or(u128::MAX);
                            let total = recipe_costs.entry(source_home).or_default();
                            *total = total.saturating_add(cost);
                        } else {
                            rejected.insert(source_home);
                        }
                    }
                    PlannedEdgeOp::Spill {
                        source,
                        destination_home,
                        ..
                    } => {
                        let total = baseline_costs.entry(destination_home).or_default();
                        *total = total.saturating_add(u128::from(spill_cost(func, source)));
                    }
                }
            }
        }
        candidates.retain(|home| {
            !self.state_homes.contains_key(home)
                && !rejected.contains(home)
                && recipe_costs.get(home).copied().unwrap_or(u128::MAX)
                    < baseline_costs.get(home).copied().unwrap_or_default()
        });
        self.recipe_homes = candidates;
        Ok(())
    }

    /// Independently prove that every reload assigned to a recipe-only home
    /// has an exact recipe at its final insertion point.  This verifier does
    /// not trust the candidate/rejection sets used by selection.
    pub(super) fn verify_recipe_homes(
        &self,
        func: &MFunction,
        cfg: &NormalizedCfg,
        analysis: &ReloadRecipeAnalysis,
    ) -> Result<(), SpillPlanError> {
        for &home in &self.recipe_homes {
            let mut reloads = 0usize;
            for &(point, operation) in &self.point_ops {
                let PlannedOp::Reload {
                    value,
                    home: reload_home,
                } = operation
                else {
                    continue;
                };
                if reload_home != home {
                    continue;
                }
                reloads += 1;
                let query = PointUse {
                    block: point.block,
                    instruction: point.instruction,
                    value: VReg(value.0),
                };
                if analysis.resolved_recipe_at_point(query).is_none() {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.RECIPE_HOME_POINT",
                        Some(point.block),
                        Some(point.instruction),
                        vec![VReg(value.0)],
                        format!("recipe home {home:?} has a point reload without an exact recipe"),
                    ));
                }
            }
            for (&(predecessor, successor), operations) in &self.edge_ops {
                let Some(predecessor_block) = func.blocks.get(predecessor) else {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.RECIPE_HOME_EDGE",
                        None,
                        None,
                        Vec::new(),
                        format!("recipe home {home:?} references absent predecessor {predecessor}"),
                    ));
                };
                let Some(successor_block) = func.blocks.get(successor) else {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.RECIPE_HOME_EDGE",
                        Some(predecessor_block.id),
                        None,
                        Vec::new(),
                        format!("recipe home {home:?} references absent successor {successor}"),
                    ));
                };
                let insertion = super::cfg::edge_insertion_point(func, cfg, predecessor, successor)
                    .ok_or_else(|| {
                        SpillPlanError::new(
                            "SPILL_PLAN.RECIPE_HOME_EDGE",
                            Some(predecessor_block.id),
                            None,
                            Vec::new(),
                            "recipe-home edge has no single-edge materialization point",
                        )
                    })?;
                let insertion_block = &func.blocks[insertion.block];
                for &operation in operations {
                    let PlannedEdgeOp::Reload {
                        source,
                        source_home: reload_home,
                        ..
                    } = operation
                    else {
                        continue;
                    };
                    if reload_home != home {
                        continue;
                    }
                    reloads += 1;
                    let query = PointUse {
                        block: insertion_block.id,
                        instruction: insertion.instruction,
                        value: VReg(source.0),
                    };
                    if analysis.resolved_recipe_at_point(query).is_none() {
                        return Err(SpillPlanError::new(
                            "SPILL_PLAN.RECIPE_HOME_EDGE",
                            Some(predecessor_block.id),
                            None,
                            vec![VReg(source.0)],
                            format!(
                                "recipe home {home:?} has no exact recipe on edge {} -> {}",
                                predecessor_block.id, successor_block.id
                            ),
                        ));
                    }
                }
            }
            if reloads == 0 {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_HOME_RELOAD",
                    None,
                    None,
                    Vec::new(),
                    format!("recipe home {home:?} has no selected reload"),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn verify(
        &self,
        func: &MFunction,
        cfg: &NormalizedCfg,
        registers: usize,
    ) -> Result<(), SpillPlanError> {
        let block_count = func.blocks.len();
        if self.w_entry.len() != block_count
            || self.w_exit.len() != block_count
            || self.s_entry.len() != block_count
            || self.s_exit.len() != block_count
        {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.STATE_SHAPE",
                None,
                None,
                Vec::new(),
                format!(
                    "spill-plan state tables must all contain {block_count} rows (W_entry={}, W_exit={}, S_entry={}, S_exit={})",
                    self.w_entry.len(),
                    self.w_exit.len(),
                    self.s_entry.len(),
                    self.s_exit.len()
                ),
            ));
        }
        if self.logical.count != func.vregs.count() || self.homes.count != self.logical.count {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.STATE_SHAPE",
                None,
                None,
                Vec::new(),
                format!(
                    "spill-plan value tables cover {} logical values and {} homes, but the function has {} virtual registers",
                    self.logical.count,
                    self.homes.count,
                    func.vregs.count()
                ),
            ));
        }

        for (block, state) in self.w_entry.iter().enumerate() {
            if state.len() > registers {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.PRESSURE",
                    Some(func.blocks[block].id),
                    Some(0),
                    state.iter().map(|value| VReg(value.0)).collect(),
                    format!(
                        "W_entry contains {} residents but only {registers} registers are available",
                        state.len()
                    ),
                ));
            }
        }
        for (block, state) in self.w_exit.iter().enumerate() {
            if state.len() > registers {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.PRESSURE",
                    Some(func.blocks[block].id),
                    Some(func.blocks[block].insts.len()),
                    state.iter().map(|value| VReg(value.0)).collect(),
                    format!(
                        "W_exit contains {} residents but only {registers} registers are available",
                        state.len()
                    ),
                ));
            }
        }

        for (block, states) in (0..block_count).map(|block| {
            (
                block,
                [
                    &self.w_entry[block],
                    &self.w_exit[block],
                    &self.s_entry[block],
                    &self.s_exit[block],
                ],
            )
        }) {
            for state in states {
                if let Some(value) = state.iter().find(|value| value.0 >= self.logical.count) {
                    return Err(SpillPlanError::new(
                        "SPILL_PLAN.VALUE_RANGE",
                        Some(func.blocks[block].id),
                        None,
                        vec![VReg(value.0)],
                        format!(
                            "spill-plan state references logical value {} but the plan contains {} values",
                            value.0, self.logical.count
                        ),
                    ));
                }
            }
        }

        for (&(predecessor, successor), operations) in &self.edge_ops {
            let Some(predecessor_block) = func.blocks.get(predecessor) else {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.EDGE_EXISTS",
                    None,
                    None,
                    Vec::new(),
                    format!("edge operation predecessor index {predecessor} is out of range"),
                ));
            };
            let Some(successor_block) = func.blocks.get(successor) else {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.EDGE_EXISTS",
                    Some(predecessor_block.id),
                    None,
                    Vec::new(),
                    format!("edge operation successor index {successor} is out of range"),
                ));
            };
            if !cfg.successors[predecessor].contains(&successor) {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.EDGE_EXISTS",
                    Some(predecessor_block.id),
                    None,
                    Vec::new(),
                    format!(
                        "planned edge operation targets {}, which is not a CFG successor",
                        successor_block.id
                    ),
                ));
            }
            if super::cfg::edge_insertion_point(func, cfg, predecessor, successor).is_none() {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.EDGE_ISOLATED",
                    Some(predecessor_block.id),
                    None,
                    Vec::new(),
                    "edge operation has no single-edge materialization point",
                ));
            }
            if operations.is_empty() {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.EDGE_EXISTS",
                    Some(predecessor_block.id),
                    None,
                    Vec::new(),
                    format!(
                        "edge-operation list for {} -> {} is empty",
                        predecessor_block.id, successor_block.id
                    ),
                ));
            }
            for (index, &operation) in operations.iter().enumerate() {
                self.verify_edge_operation(
                    operation,
                    Some(predecessor_block.id),
                    edge_reload_uses_transferred_home(operations, index),
                )?;
            }
        }
        for &(point, operation) in &self.point_ops {
            let Some(&block) = cfg.block_index.get(&point.block) else {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.POINT_RANGE",
                    Some(point.block),
                    Some(point.instruction),
                    Vec::new(),
                    "planned operation references a block absent from the normalized CFG",
                ));
            };
            if point.instruction > func.blocks[block].insts.len() {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.POINT_RANGE",
                    Some(point.block),
                    Some(point.instruction),
                    Vec::new(),
                    format!(
                        "planned operation is outside the block's {} instructions",
                        func.blocks[block].insts.len()
                    ),
                ));
            }
            self.verify_operation(operation, Some(point.block), Some(point.instruction))?;
        }
        for &(block, instruction, value) in &self.recipe_reloads {
            let matching_reload = self.point_ops.iter().any(|(point, operation)| {
                point.block == block
                    && point.instruction == instruction
                    && matches!(operation, PlannedOp::Reload { value: reload, .. } if *reload == value)
            });
            if !matching_reload {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_RELOAD_POINT",
                    Some(block),
                    Some(instruction),
                    vec![VReg(value.0)],
                    "recipe-reload annotation has no matching point reload",
                ));
            }
        }
        for &home in &self.recipe_homes {
            if home.0 >= self.logical.count {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_HOME_RANGE",
                    None,
                    None,
                    Vec::new(),
                    format!(
                        "recipe home {} is outside the plan's {} logical values",
                        home.0, self.logical.count
                    ),
                ));
            }
            let has_reload = self
                .point_ops
                .iter()
                .any(|(_, operation)| {
                    matches!(operation, PlannedOp::Reload { home: reload_home, .. } if *reload_home == home)
                })
                || self.edge_ops.values().flatten().any(|operation| {
                    matches!(operation, PlannedEdgeOp::Reload { source_home, .. } if *source_home == home)
                });
            if !has_reload {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.RECIPE_HOME_RELOAD",
                    None,
                    None,
                    Vec::new(),
                    format!("recipe home {home:?} has no selected reload"),
                ));
            }
        }
        Ok(())
    }

    fn verify_operation(
        &self,
        operation: PlannedOp,
        block: Option<BlockId>,
        instruction: Option<usize>,
    ) -> Result<(), SpillPlanError> {
        let (value, home) = match operation {
            PlannedOp::Spill { value, home }
            | PlannedOp::Reload { value, home }
            | PlannedOp::SpillPhi { value, home } => (value, home),
        };
        if value.0 >= self.logical.count {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.VALUE_RANGE",
                block,
                instruction,
                vec![VReg(value.0)],
                format!(
                    "planned operation references logical value {} but the plan contains {} values",
                    value.0, self.logical.count
                ),
            ));
        }
        let expected = self.homes.of_logical(value);
        if home != expected {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.HOME",
                block,
                instruction,
                vec![VReg(value.0)],
                format!(
                    "planned operation uses spill home {} but logical value {} belongs to home {}",
                    home.0, value.0, expected.0
                ),
            ));
        }
        Ok(())
    }

    fn verify_edge_operation(
        &self,
        operation: PlannedEdgeOp,
        block: Option<BlockId>,
        transferred_home: bool,
    ) -> Result<(), SpillPlanError> {
        let (source, destination, home, expected_home) = match operation {
            PlannedEdgeOp::Reload {
                source,
                source_home,
                destination,
            } => (
                source,
                destination,
                source_home,
                self.homes.of_logical(source),
            ),
            PlannedEdgeOp::Spill {
                source,
                destination,
                destination_home,
            } => (
                source,
                destination,
                destination_home,
                self.homes.of_logical(destination),
            ),
        };
        for value in [source, destination] {
            if value.0 >= self.logical.count {
                return Err(SpillPlanError::new(
                    "SPILL_PLAN.VALUE_RANGE",
                    block,
                    None,
                    vec![VReg(value.0)],
                    format!(
                        "planned edge operation references logical value {} but the plan contains {} values",
                        value.0, self.logical.count
                    ),
                ));
            }
        }
        if home != expected_home && !transferred_home {
            return Err(SpillPlanError::new(
                "SPILL_PLAN.HOME_CLASS",
                block,
                None,
                vec![VReg(source.0), VReg(destination.0)],
                format!("planned edge operation names home {home:?}, expected {expected_home:?}"),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
