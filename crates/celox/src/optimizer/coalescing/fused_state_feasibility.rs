//! Analysis-only feasibility probe for range-aware fused StateSSA.
//!
//! The probe starts from static STABLE loads in the FF suffix and resolves
//! demanded bit ranges through one access-based MemorySSA graph. It never
//! rewrites SIR and never expands every access over every logical segment.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use celox_analysis::ssa::{self, Event, SparseSsa, Version};

use super::state_ssa::StatePhaseMap;
use crate::ir::cfg::SirCfg;
use crate::ir::{
    BinaryOp, BlockId, ExecutionUnit, RegionedAbsoluteAddr, RegisterId, SIRInstruction, SIROffset,
    STABLE_REGION,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct ProgramPoint {
    block: usize,
    instruction: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct BitRange {
    object: RegionedAbsoluteAddr,
    start: usize,
    end: usize,
}

impl BitRange {
    fn new(object: RegionedAbsoluteAddr, start: usize, width: usize) -> Option<Self> {
        let end = start.checked_add(width)?;
        (start < end).then_some(Self { object, start, end })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DefinitionKind {
    Exact(BitRange),
    UnknownObject(RegionedAbsoluteAddr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct MemoryDefinition {
    point: ProgramPoint,
    kind: DefinitionKind,
    in_ff_suffix: bool,
    effectful: bool,
}

type ObjectVersion = Version<RegionedAbsoluteAddr, ProgramPoint>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Usage {
    DefinitionInput(ProgramPoint),
    FfLoad(ProgramPoint),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct DemandKey {
    version: ObjectVersion,
    range: BitRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum LogicalSource {
    LiveOnEntry,
    Definition(ProgramPoint),
    Unknown(ProgramPoint),
    Phi {
        object: RegionedAbsoluteAddr,
        block: usize,
        start: usize,
        end: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Piece {
    start: usize,
    end: usize,
    source: LogicalSource,
}

#[derive(Debug)]
enum DefPart {
    Child(DemandKey),
    Definition {
        start: usize,
        end: usize,
        source: LogicalSource,
    },
}

#[derive(Debug)]
enum RangeNode {
    Uniform(ObjectVersion),
    Branch {
        left: Arc<RangeNode>,
        right: Arc<RangeNode>,
    },
}

struct PersistentRangeStates<'a> {
    endpoints: &'a BTreeMap<RegionedAbsoluteAddr, Vec<usize>>,
    definitions: &'a BTreeMap<ProgramPoint, MemoryDefinition>,
    definition_inputs: &'a BTreeMap<ProgramPoint, ObjectVersion>,
    roots: HashMap<ObjectVersion, Arc<RangeNode>>,
}

impl<'a> PersistentRangeStates<'a> {
    fn new(
        endpoints: &'a BTreeMap<RegionedAbsoluteAddr, Vec<usize>>,
        definitions: &'a BTreeMap<ProgramPoint, MemoryDefinition>,
        definition_inputs: &'a BTreeMap<ProgramPoint, ObjectVersion>,
    ) -> Self {
        Self {
            endpoints,
            definitions,
            definition_inputs,
            roots: HashMap::new(),
        }
    }

    fn root(&mut self, version: ObjectVersion) -> Result<Arc<RangeNode>, FeasibilityError> {
        if let Some(root) = self.roots.get(&version) {
            return Ok(Arc::clone(root));
        }
        let mut pending = Vec::new();
        let mut cursor = version;
        let mut root = loop {
            if let Some(root) = self.roots.get(&cursor) {
                break Arc::clone(root);
            }
            match cursor {
                Version::Entry(_) | Version::Phi { .. } => {
                    let root = Arc::new(RangeNode::Uniform(cursor));
                    self.roots.insert(cursor, Arc::clone(&root));
                    break root;
                }
                Version::Definition { definition, .. } => {
                    pending.push(cursor);
                    cursor = *self.definition_inputs.get(&definition).ok_or(
                        FeasibilityError::InvalidMemoryGraph(
                            "definition has no incoming object version",
                        ),
                    )?;
                }
            }
        };
        while let Some(definition_version) = pending.pop() {
            let Version::Definition { definition, .. } = definition_version else {
                unreachable!("only definitions are pending");
            };
            let definition = self.definitions[&definition];
            let points = &self.endpoints[&definition_object(definition)];
            let segments = points.len().saturating_sub(1);
            let (start, end) = match definition.kind {
                DefinitionKind::Exact(range) => (
                    points.binary_search(&range.start).unwrap(),
                    points.binary_search(&range.end).unwrap(),
                ),
                DefinitionKind::UnknownObject(_) => (0, segments),
            };
            root = assign_range(root, 0, segments, start, end, definition_version);
            self.roots.insert(definition_version, Arc::clone(&root));
        }
        Ok(root)
    }

    fn pieces(
        &mut self,
        version: ObjectVersion,
        range: BitRange,
    ) -> Result<Vec<(usize, usize, ObjectVersion)>, FeasibilityError> {
        let points = &self.endpoints[&range.object];
        let start = points.binary_search(&range.start).unwrap();
        let end = points.binary_search(&range.end).unwrap();
        let root = self.root(version)?;
        let mut pieces = Vec::new();
        collect_range(&root, points, 0, points.len() - 1, start, end, &mut pieces);
        Ok(merge_version_pieces(pieces))
    }
}

fn definition_object(definition: MemoryDefinition) -> RegionedAbsoluteAddr {
    match definition.kind {
        DefinitionKind::Exact(range) => range.object,
        DefinitionKind::UnknownObject(object) => object,
    }
}

fn assign_range(
    node: Arc<RangeNode>,
    low: usize,
    high: usize,
    start: usize,
    end: usize,
    source: ObjectVersion,
) -> Arc<RangeNode> {
    if end <= low || high <= start || low == high {
        return node;
    }
    if start <= low && high <= end {
        return Arc::new(RangeNode::Uniform(source));
    }
    let middle = low + (high - low) / 2;
    let (left, right) = match node.as_ref() {
        RangeNode::Uniform(version) => (
            Arc::new(RangeNode::Uniform(*version)),
            Arc::new(RangeNode::Uniform(*version)),
        ),
        RangeNode::Branch { left, right } => (Arc::clone(left), Arc::clone(right)),
    };
    let left = assign_range(left, low, middle, start, end, source);
    let right = assign_range(right, middle, high, start, end, source);
    if let (RangeNode::Uniform(left_source), RangeNode::Uniform(right_source)) =
        (left.as_ref(), right.as_ref())
        && left_source == right_source
    {
        Arc::new(RangeNode::Uniform(*left_source))
    } else {
        Arc::new(RangeNode::Branch { left, right })
    }
}

fn collect_range(
    node: &Arc<RangeNode>,
    points: &[usize],
    low: usize,
    high: usize,
    start: usize,
    end: usize,
    output: &mut Vec<(usize, usize, ObjectVersion)>,
) {
    if end <= low || high <= start || low == high {
        return;
    }
    match node.as_ref() {
        RangeNode::Uniform(source) => {
            let covered_start = low.max(start);
            let covered_end = high.min(end);
            output.push((points[covered_start], points[covered_end], *source));
        }
        RangeNode::Branch { left, right } => {
            let middle = low + (high - low) / 2;
            collect_range(left, points, low, middle, start, end, output);
            collect_range(right, points, middle, high, start, end, output);
        }
    }
}

fn merge_version_pieces(
    pieces: Vec<(usize, usize, ObjectVersion)>,
) -> Vec<(usize, usize, ObjectVersion)> {
    let mut merged = Vec::<(usize, usize, ObjectVersion)>::new();
    for piece in pieces {
        if let Some(previous) = merged.last_mut()
            && previous.1 == piece.0
            && previous.2 == piece.2
        {
            previous.1 = piece.1;
        } else {
            merged.push(piece);
        }
    }
    merged
}

#[derive(Debug)]
enum ResolveFrame {
    Enter(DemandKey),
    FinishDefinition {
        key: DemandKey,
        parts: Vec<DefPart>,
        children: usize,
    },
    FinishPhi {
        key: DemandKey,
        inputs: usize,
    },
}

#[derive(Debug, Default)]
struct ResolverStats {
    materialized_fragments: usize,
    phi_operands: usize,
    phi_versions: BTreeSet<(RegionedAbsoluteAddr, usize, usize, usize)>,
    phi_sources: BTreeMap<LogicalSource, Vec<LogicalSource>>,
    maximum_versions_per_object: BTreeMap<RegionedAbsoluteAddr, BTreeSet<LogicalSource>>,
}

struct DemandResolver<'a> {
    ssa: &'a SparseSsa<RegionedAbsoluteAddr, ProgramPoint, Usage>,
    ranges: PersistentRangeStates<'a>,
    phis: BTreeMap<(RegionedAbsoluteAddr, usize), usize>,
    memo: HashMap<DemandKey, Vec<Piece>>,
    active: HashSet<DemandKey>,
    frames: Vec<ResolveFrame>,
    values: Vec<Vec<Piece>>,
    stats: ResolverStats,
}

impl<'a> DemandResolver<'a> {
    fn new(
        ssa: &'a SparseSsa<RegionedAbsoluteAddr, ProgramPoint, Usage>,
        endpoints: &'a BTreeMap<RegionedAbsoluteAddr, Vec<usize>>,
        definitions: &'a BTreeMap<ProgramPoint, MemoryDefinition>,
        definition_inputs: &'a BTreeMap<ProgramPoint, ObjectVersion>,
    ) -> Self {
        Self {
            ssa,
            ranges: PersistentRangeStates::new(endpoints, definitions, definition_inputs),
            phis: ssa
                .phis
                .iter()
                .enumerate()
                .map(|(index, phi)| ((phi.variable, phi.block), index))
                .collect(),
            memo: HashMap::new(),
            active: HashSet::new(),
            frames: Vec::new(),
            values: Vec::new(),
            stats: ResolverStats::default(),
        }
    }

    fn resolve(&mut self, key: DemandKey) -> Result<Vec<Piece>, FeasibilityError> {
        self.frames.clear();
        self.values.clear();
        self.frames.push(ResolveFrame::Enter(key));
        while let Some(frame) = self.frames.pop() {
            match frame {
                ResolveFrame::Enter(key) => {
                    if let Some(value) = self.memo.get(&key) {
                        self.values.push(value.clone());
                        continue;
                    }
                    if !self.active.insert(key) {
                        let block = match key.version {
                            Version::Phi { block, .. } => block,
                            _ => {
                                return Err(FeasibilityError::InvalidMemoryGraph(
                                    "a non-phi version formed a cycle",
                                ));
                            }
                        };
                        self.values.push(vec![Piece {
                            start: key.range.start,
                            end: key.range.end,
                            source: LogicalSource::Phi {
                                object: key.range.object,
                                block,
                                start: key.range.start,
                                end: key.range.end,
                            },
                        }]);
                        continue;
                    }
                    match key.version {
                        version @ (Version::Entry(_) | Version::Definition { .. }) => {
                            let mut parts = Vec::new();
                            for (start, end, source) in self.ranges.pieces(version, key.range)? {
                                match source {
                                    Version::Entry(_) => parts.push(DefPart::Definition {
                                        start,
                                        end,
                                        source: LogicalSource::LiveOnEntry,
                                    }),
                                    Version::Definition { definition, .. } => {
                                        let definition = self.ranges.definitions[&definition];
                                        let source = match definition.kind {
                                            DefinitionKind::Exact(_) => {
                                                LogicalSource::Definition(definition.point)
                                            }
                                            DefinitionKind::UnknownObject(_) => {
                                                LogicalSource::Unknown(definition.point)
                                            }
                                        };
                                        parts.push(DefPart::Definition { start, end, source });
                                    }
                                    Version::Phi { .. } => {
                                        parts.push(DefPart::Child(DemandKey {
                                            version: source,
                                            range: BitRange {
                                                object: key.range.object,
                                                start,
                                                end,
                                            },
                                        }));
                                    }
                                }
                            }
                            let children = parts
                                .iter()
                                .filter(|part| matches!(part, DefPart::Child(_)))
                                .count();
                            let child_keys = parts
                                .iter()
                                .filter_map(|part| match part {
                                    DefPart::Child(child) => Some(*child),
                                    DefPart::Definition { .. } => None,
                                })
                                .collect::<Vec<_>>();
                            self.frames.push(ResolveFrame::FinishDefinition {
                                key,
                                parts,
                                children,
                            });
                            for child in child_keys.into_iter().rev() {
                                self.frames.push(ResolveFrame::Enter(child));
                            }
                        }
                        Version::Phi { variable, block } => {
                            let phi = &self.ssa.phis[self.phis[&(variable, block)]];
                            let children = phi
                                .inputs
                                .iter()
                                .map(|(_, version)| DemandKey {
                                    version: *version,
                                    range: key.range,
                                })
                                .collect::<Vec<_>>();
                            self.frames.push(ResolveFrame::FinishPhi {
                                key,
                                inputs: children.len(),
                            });
                            for child in children.into_iter().rev() {
                                self.frames.push(ResolveFrame::Enter(child));
                            }
                        }
                    }
                }
                ResolveFrame::FinishDefinition {
                    key,
                    parts,
                    children,
                } => {
                    let first = self.values.len().checked_sub(children).ok_or(
                        FeasibilityError::InvalidMemoryGraph(
                            "definition child result stack underflow",
                        ),
                    )?;
                    let child_values = self.values.drain(first..).collect::<Vec<_>>();
                    let mut child = 0usize;
                    let mut result = Vec::new();
                    for part in parts {
                        match part {
                            DefPart::Child(_) => {
                                result.extend(child_values[child].iter().copied());
                                child += 1;
                            }
                            DefPart::Definition { start, end, source } => {
                                result.push(Piece { start, end, source });
                            }
                        }
                    }
                    self.finish(key, merge_adjacent(result));
                }
                ResolveFrame::FinishPhi { key, inputs } => {
                    let first = self.values.len().checked_sub(inputs).ok_or(
                        FeasibilityError::InvalidMemoryGraph("phi input result stack underflow"),
                    )?;
                    let input_values = self.values.drain(first..).collect::<Vec<_>>();
                    let mut endpoints = BTreeSet::from([key.range.start, key.range.end]);
                    for pieces in &input_values {
                        for piece in pieces {
                            endpoints.insert(piece.start);
                            endpoints.insert(piece.end);
                        }
                    }
                    let endpoints = endpoints.into_iter().collect::<Vec<_>>();
                    let mut result = Vec::new();
                    for pair in endpoints.windows(2) {
                        let start = pair[0];
                        let end = pair[1];
                        if start == end {
                            continue;
                        }
                        let sources = input_values
                            .iter()
                            .map(|pieces| source_at(pieces, start, end))
                            .collect::<Result<Vec<_>, _>>()?;
                        let source = if sources.windows(2).all(|pair| pair[0] == pair[1]) {
                            sources[0]
                        } else {
                            let block = match key.version {
                                Version::Phi { block, .. } => block,
                                _ => unreachable!("FinishPhi retains a phi version"),
                            };
                            let identity = (key.range.object, block, start, end);
                            if self.stats.phi_versions.insert(identity) {
                                self.stats.phi_operands += sources.len();
                            }
                            let phi = LogicalSource::Phi {
                                object: key.range.object,
                                block,
                                start,
                                end,
                            };
                            self.stats.phi_sources.insert(phi, sources);
                            phi
                        };
                        result.push(Piece { start, end, source });
                    }
                    self.finish(key, merge_adjacent(result));
                }
            }
        }
        if self.values.len() != 1 {
            return Err(FeasibilityError::InvalidMemoryGraph(
                "one demand did not produce one result",
            ));
        }
        Ok(self.values.pop().unwrap())
    }

    fn finish(&mut self, key: DemandKey, value: Vec<Piece>) {
        self.active.remove(&key);
        self.stats.materialized_fragments += value.len();
        let versions = self
            .stats
            .maximum_versions_per_object
            .entry(key.range.object)
            .or_default();
        versions.extend(value.iter().map(|piece| piece.source));
        self.memo.insert(key, value.clone());
        self.values.push(value);
    }
}

fn source_at(
    pieces: &[Piece],
    start: usize,
    end: usize,
) -> Result<LogicalSource, FeasibilityError> {
    pieces
        .iter()
        .find(|piece| piece.start <= start && end <= piece.end)
        .map(|piece| piece.source)
        .ok_or(FeasibilityError::InvalidMemoryGraph(
            "phi input does not cover demanded range",
        ))
}

fn merge_adjacent(mut pieces: Vec<Piece>) -> Vec<Piece> {
    pieces.sort_unstable_by_key(|piece| piece.start);
    let mut merged = Vec::<Piece>::new();
    for piece in pieces {
        if let Some(previous) = merged.last_mut()
            && previous.end == piece.start
            && previous.source == piece.source
        {
            previous.end = piece.end;
        } else {
            merged.push(piece);
        }
    }
    merged
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct OriginCounts {
    pub memory_traffic: usize,
    pub range_extraction: usize,
    pub range_insertion: usize,
    pub mask_generation: usize,
    pub mux_lowering: usize,
    pub unrelated_arithmetic: usize,
}

#[derive(Debug, Default)]
pub(crate) struct FeasibilityReport {
    pub blocks: usize,
    pub instructions: usize,
    pub static_accesses: usize,
    pub logical_segments: usize,
    pub memory_def_fragments: usize,
    pub overlap_edges: usize,
    pub memory_phi_operands: usize,
    pub maximum_fragments_per_access: usize,
    pub maximum_versions_per_object: usize,
    pub demanded_ff_loads: usize,
    pub candidate_removable_loads: usize,
    pub candidate_backing_stores: usize,
    pub rejected_live_on_entry: usize,
    pub rejected_unknown: usize,
    pub rejected_ff_definition: usize,
    pub rejected_effectful_definition: usize,
    pub materialized_range_fragments: usize,
    pub origins: OriginCounts,
}

impl fmt::Display for FeasibilityReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "blocks={} instructions={} static_accesses={} logical_segments={} \
             memory_def_fragments={} overlap_edges={} memory_phi_operands={} \
             maximum_fragments_per_access={} maximum_versions_per_object={} \
             demanded_ff_loads={} candidate_removable_loads={} \
             candidate_backing_stores={} rejected_live_on_entry={} \
             rejected_unknown={} rejected_ff_definition={} \
             rejected_effectful_definition={} \
             materialized_range_fragments={} origin_memory={} \
             origin_extract={} origin_insert={} origin_mask={} origin_mux={} \
             origin_other={}",
            self.blocks,
            self.instructions,
            self.static_accesses,
            self.logical_segments,
            self.memory_def_fragments,
            self.overlap_edges,
            self.memory_phi_operands,
            self.maximum_fragments_per_access,
            self.maximum_versions_per_object,
            self.demanded_ff_loads,
            self.candidate_removable_loads,
            self.candidate_backing_stores,
            self.rejected_live_on_entry,
            self.rejected_unknown,
            self.rejected_ff_definition,
            self.rejected_effectful_definition,
            self.materialized_range_fragments,
            self.origins.memory_traffic,
            self.origins.range_extraction,
            self.origins.range_insertion,
            self.origins.mask_generation,
            self.origins.mux_lowering,
            self.origins.unrelated_arithmetic,
        )
    }
}

#[derive(Debug)]
pub(crate) enum FeasibilityError {
    Cfg(crate::ir::cfg::SirCfgError),
    Phase(super::state_ssa::StateSsaError),
    StateSsa(ssa::SsaError),
    InvalidMemoryGraph(&'static str),
}

impl fmt::Display for FeasibilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cfg(error) => write!(f, "CFG: {error}"),
            Self::Phase(error) => write!(f, "phase: {error}"),
            Self::StateSsa(error) => write!(f, "object StateSSA: {error}"),
            Self::InvalidMemoryGraph(message) => write!(f, "range resolver: {message}"),
        }
    }
}

impl std::error::Error for FeasibilityError {}

pub(crate) fn analyze(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    ff_entry: BlockId,
) -> Result<FeasibilityReport, FeasibilityError> {
    let cfg = SirCfg::analyze(eu).map_err(FeasibilityError::Cfg)?;
    let phases = StatePhaseMap::fused(eu, &cfg, ff_entry).map_err(FeasibilityError::Phase)?;
    let ff_blocks = phases.ff_blocks().expect("fused phases expose FF blocks");
    let mut events =
        vec![Vec::<Event<RegionedAbsoluteAddr, ProgramPoint, Usage>>::new(); cfg.block_ids.len()];
    let mut definitions = BTreeMap::<ProgramPoint, MemoryDefinition>::new();
    let mut accesses = Vec::<(BitRange, bool)>::new();
    let mut demanded_loads = Vec::<(ProgramPoint, BitRange)>::new();
    let mut store_sources = BTreeMap::<ProgramPoint, RegisterId>::new();

    for (block, &block_id) in cfg.block_ids.iter().enumerate() {
        for (instruction, inst) in eu.blocks[&block_id].instructions.iter().enumerate() {
            let point = ProgramPoint { block, instruction };
            match inst {
                SIRInstruction::Load(_, address, offset, width)
                    if address.region == STABLE_REGION =>
                {
                    if let SIROffset::Static(start) = offset
                        && let Some(range) = BitRange::new(*address, *start, *width)
                    {
                        accesses.push((range, false));
                        if ff_blocks.contains(&block_id) {
                            demanded_loads.push((point, range));
                            events[block].push(Event::Use {
                                variable: *address,
                                usage: Usage::FfLoad(point),
                            });
                        }
                    }
                }
                SIRInstruction::Store(address, offset, width, source, triggers, captures)
                    if address.region == STABLE_REGION =>
                {
                    let kind = match offset {
                        SIROffset::Static(start) => {
                            if let Some(range) = BitRange::new(*address, *start, *width) {
                                accesses.push((range, true));
                                DefinitionKind::Exact(range)
                            } else {
                                DefinitionKind::UnknownObject(*address)
                            }
                        }
                        SIROffset::Dynamic(_) | SIROffset::Element { .. } => {
                            DefinitionKind::UnknownObject(*address)
                        }
                    };
                    let definition = MemoryDefinition {
                        point,
                        kind,
                        in_ff_suffix: ff_blocks.contains(&block_id),
                        effectful: !triggers.is_empty() || !captures.is_empty(),
                    };
                    definitions.insert(point, definition);
                    store_sources.insert(point, *source);
                    events[block].push(Event::Use {
                        variable: *address,
                        usage: Usage::DefinitionInput(point),
                    });
                    events[block].push(Event::Definition {
                        variable: *address,
                        definition: point,
                    });
                }
                SIRInstruction::Commit(_, destination, offset, width, triggers)
                    if destination.region == STABLE_REGION =>
                {
                    let kind = match offset {
                        SIROffset::Static(start) => BitRange::new(*destination, *start, *width)
                            .map_or(DefinitionKind::UnknownObject(*destination), |range| {
                                accesses.push((range, true));
                                DefinitionKind::Exact(range)
                            }),
                        SIROffset::Dynamic(_) | SIROffset::Element { .. } => {
                            DefinitionKind::UnknownObject(*destination)
                        }
                    };
                    let definition = MemoryDefinition {
                        point,
                        kind,
                        in_ff_suffix: ff_blocks.contains(&block_id),
                        effectful: !triggers.is_empty(),
                    };
                    definitions.insert(point, definition);
                    events[block].push(Event::Use {
                        variable: *destination,
                        usage: Usage::DefinitionInput(point),
                    });
                    events[block].push(Event::Definition {
                        variable: *destination,
                        definition: point,
                    });
                }
                _ => {}
            }
        }
    }

    let state_ssa = ssa::build(&cfg, &events).map_err(FeasibilityError::StateSsa)?;
    let definition_inputs = definitions
        .keys()
        .map(|&point| {
            state_ssa
                .uses
                .get(&Usage::DefinitionInput(point))
                .copied()
                .map(|version| (point, version))
                .ok_or(FeasibilityError::InvalidMemoryGraph(
                    "definition has no incoming object version",
                ))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let endpoints = range_endpoints(&accesses);
    let mut report = range_shape_report(&accesses, &endpoints);
    report.blocks = eu.blocks.len();
    report.instructions = eu
        .blocks
        .values()
        .map(|block| block.instructions.len() + 1)
        .sum();
    report.demanded_ff_loads = demanded_loads.len();

    let mut resolver =
        DemandResolver::new(&state_ssa, &endpoints, &definitions, &definition_inputs);
    let mut candidate_stores = BTreeSet::<ProgramPoint>::new();
    for (point, range) in demanded_loads {
        let start = state_ssa.uses.get(&Usage::FfLoad(point)).copied().ok_or(
            FeasibilityError::InvalidMemoryGraph("FF load has no reaching object version"),
        )?;
        let pieces = resolver.resolve(DemandKey {
            version: start,
            range,
        })?;
        let mut candidate = true;
        let mut load_stores = BTreeSet::<ProgramPoint>::new();
        let mut visited_sources = BTreeSet::new();
        for piece in &pieces {
            candidate &= classify_candidate_source(
                piece.source,
                &resolver.stats.phi_sources,
                &definitions,
                &mut load_stores,
                &mut visited_sources,
                &mut report,
            );
        }
        if candidate {
            report.candidate_removable_loads += 1;
            candidate_stores.extend(load_stores);
        }
    }
    report.candidate_backing_stores = candidate_stores.len();
    report.memory_phi_operands = resolver.stats.phi_operands;
    report.materialized_range_fragments = resolver.stats.materialized_fragments;
    report.maximum_versions_per_object = resolver
        .stats
        .maximum_versions_per_object
        .values()
        .map(BTreeSet::len)
        .max()
        .unwrap_or(0);
    report.origins = classify_candidate_origins(eu, &candidate_stores, &store_sources);
    Ok(report)
}

fn classify_candidate_source(
    source: LogicalSource,
    phi_sources: &BTreeMap<LogicalSource, Vec<LogicalSource>>,
    definitions: &BTreeMap<ProgramPoint, MemoryDefinition>,
    stores: &mut BTreeSet<ProgramPoint>,
    visited: &mut BTreeSet<LogicalSource>,
    report: &mut FeasibilityReport,
) -> bool {
    if !visited.insert(source) {
        return true;
    }
    match source {
        LogicalSource::Definition(point) => {
            let definition = definitions[&point];
            let mut admissible = true;
            if definition.in_ff_suffix {
                report.rejected_ff_definition += 1;
                admissible = false;
            }
            if definition.effectful {
                report.rejected_effectful_definition += 1;
                admissible = false;
            }
            if admissible {
                stores.insert(point);
            }
            admissible
        }
        LogicalSource::LiveOnEntry => {
            report.rejected_live_on_entry += 1;
            false
        }
        LogicalSource::Unknown(_) => {
            report.rejected_unknown += 1;
            false
        }
        phi @ LogicalSource::Phi { .. } => {
            let Some(inputs) = phi_sources.get(&phi) else {
                report.rejected_unknown += 1;
                return false;
            };
            inputs.iter().copied().fold(true, |admissible, input| {
                classify_candidate_source(input, phi_sources, definitions, stores, visited, report)
                    && admissible
            })
        }
    }
}

fn range_endpoints(accesses: &[(BitRange, bool)]) -> BTreeMap<RegionedAbsoluteAddr, Vec<usize>> {
    let mut endpoints = BTreeMap::<RegionedAbsoluteAddr, BTreeSet<usize>>::new();
    for &(range, _) in accesses {
        endpoints
            .entry(range.object)
            .or_default()
            .extend([range.start, range.end]);
    }
    endpoints
        .into_iter()
        .map(|(object, points)| (object, points.into_iter().collect::<Vec<_>>()))
        .collect()
}

fn range_shape_report(
    accesses: &[(BitRange, bool)],
    endpoint_vectors: &BTreeMap<RegionedAbsoluteAddr, Vec<usize>>,
) -> FeasibilityReport {
    let mut report = FeasibilityReport {
        static_accesses: accesses.len(),
        logical_segments: endpoint_vectors
            .values()
            .map(|points| points.len().saturating_sub(1))
            .sum(),
        ..FeasibilityReport::default()
    };
    for &(range, definition) in accesses {
        let points = &endpoint_vectors[&range.object];
        let start = points.binary_search(&range.start).unwrap();
        let end = points.binary_search(&range.end).unwrap();
        let fragments = end - start;
        report.overlap_edges += fragments;
        report.memory_def_fragments += usize::from(definition) * fragments;
        report.maximum_fragments_per_access = report.maximum_fragments_per_access.max(fragments);
    }
    report
}

fn classify_candidate_origins(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    stores: &BTreeSet<ProgramPoint>,
    store_sources: &BTreeMap<ProgramPoint, RegisterId>,
) -> OriginCounts {
    let mut definitions = HashMap::<RegisterId, &SIRInstruction<RegionedAbsoluteAddr>>::new();
    for block in eu.blocks.values() {
        for inst in &block.instructions {
            if let Some(definition) = super::shared::def_reg(inst) {
                definitions.insert(definition, inst);
            }
        }
    }
    let mut work = stores
        .iter()
        .filter_map(|store| store_sources.get(store).copied())
        .collect::<Vec<_>>();
    let mut visited = HashSet::new();
    let mut counts = OriginCounts {
        memory_traffic: stores.len(),
        ..OriginCounts::default()
    };
    while let Some(value) = work.pop() {
        if !visited.insert(value) {
            continue;
        }
        let Some(inst) = definitions.get(&value).copied() else {
            continue;
        };
        match inst {
            SIRInstruction::Load(..) => counts.memory_traffic += 1,
            SIRInstruction::Slice(..) => counts.range_extraction += 1,
            SIRInstruction::Concat(..) => counts.range_insertion += 1,
            SIRInstruction::Binary(_, _, BinaryOp::And | BinaryOp::Shl | BinaryOp::Shr, _) => {
                counts.mask_generation += 1;
            }
            SIRInstruction::Mux(..) => counts.mux_lowering += 1,
            SIRInstruction::Imm(..) | SIRInstruction::Binary(..) | SIRInstruction::Unary(..) => {
                counts.unrelated_arithmetic += 1
            }
            SIRInstruction::Store(..)
            | SIRInstruction::Commit(..)
            | SIRInstruction::RuntimeEvent { .. }
            | SIRInstruction::CombCaptureEvent { .. }
            | SIRInstruction::CombCaptureEnableIfChanged { .. } => {}
        }
        work.extend(instruction_operands(inst));
    }
    counts
}

fn instruction_operands(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Vec<RegisterId> {
    match inst {
        SIRInstruction::Imm(..) | SIRInstruction::Load(..) | SIRInstruction::Commit(..) => {
            Vec::new()
        }
        SIRInstruction::Binary(_, lhs, _, rhs) => vec![*lhs, *rhs],
        SIRInstruction::Unary(_, _, source) | SIRInstruction::Slice(_, source, ..) => vec![*source],
        SIRInstruction::Store(_, _, _, source, _, _) => vec![*source],
        SIRInstruction::Concat(_, sources) => sources.clone(),
        SIRInstruction::Mux(_, condition, then_value, else_value) => {
            vec![*condition, *then_value, *else_value]
        }
        SIRInstruction::RuntimeEvent { args, .. }
        | SIRInstruction::CombCaptureEvent { args, .. } => args.clone(),
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => vec![*old, *new],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        BasicBlock, InstanceId, RegisterType, SIRTerminator, SIRValue, WORKING_REGION,
    };
    use veryl_analyzer::ir::VarId;

    fn address(region: u32) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region,
            instance_id: InstanceId(0),
            var_id: VarId::default(),
        }
    }

    fn bit(width: usize) -> RegisterType {
        RegisterType::Bit {
            width,
            signed: false,
        }
    }

    #[test]
    fn resolves_partial_comb_stores_for_one_ff_demand() {
        let stable = address(STABLE_REGION);
        let working = address(WORKING_REGION);
        let comb = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(0x12u64)),
                SIRInstruction::Store(
                    stable,
                    SIROffset::Static(0),
                    8,
                    RegisterId(0),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Imm(RegisterId(1), SIRValue::new(0x34u64)),
                SIRInstruction::Store(
                    stable,
                    SIROffset::Static(8),
                    8,
                    RegisterId(1),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
        };
        let ff = BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Load(RegisterId(2), stable, SIROffset::Static(0), 16),
                SIRInstruction::Store(
                    working,
                    SIROffset::Static(0),
                    16,
                    RegisterId(2),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            terminator: SIRTerminator::Return,
        };
        let eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(BlockId(0), comb), (BlockId(1), ff)].into_iter().collect(),
            register_map: [
                (RegisterId(0), bit(8)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(16)),
            ]
            .into_iter()
            .collect(),
        };

        let before = eu.to_string();
        let report = analyze(&eu, BlockId(1)).unwrap();
        assert_eq!(eu.to_string(), before);
        assert_eq!(report.demanded_ff_loads, 1);
        assert_eq!(report.candidate_removable_loads, 1);
        assert_eq!(report.candidate_backing_stores, 2);
        assert_eq!(report.logical_segments, 2);
        assert_eq!(report.maximum_fragments_per_access, 2);
    }

    #[test]
    fn reports_quadratic_eager_overlap_without_materializing_edges() {
        const NARROW: usize = 128;
        let object = address(STABLE_REGION);
        let mut accesses = Vec::new();
        for index in 0..NARROW {
            accesses.push((BitRange::new(object, index, 1).unwrap(), index % 2 == 0));
            accesses.push((BitRange::new(object, 0, NARROW).unwrap(), index % 2 != 0));
        }

        let endpoints = range_endpoints(&accesses);
        let report = range_shape_report(&accesses, &endpoints);
        assert_eq!(report.logical_segments, NARROW);
        assert_eq!(report.maximum_fragments_per_access, NARROW);
        assert!(report.overlap_edges >= NARROW * NARROW);
        assert_eq!(report.materialized_range_fragments, 0);
    }

    #[test]
    fn admits_diamond_memory_phi_with_two_comb_definitions() {
        let stable = address(STABLE_REGION);
        let working = address(WORKING_REGION);
        let blocks = [
            (
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u64))],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), Vec::new()),
                        false_block: (BlockId(2), Vec::new()),
                    },
                },
            ),
            (
                BlockId(1),
                BasicBlock {
                    id: BlockId(1),
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Imm(RegisterId(1), SIRValue::new(0x12u64)),
                        SIRInstruction::Store(
                            stable,
                            SIROffset::Static(0),
                            8,
                            RegisterId(1),
                            Vec::new(),
                            Vec::new(),
                        ),
                    ],
                    terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
                },
            ),
            (
                BlockId(2),
                BasicBlock {
                    id: BlockId(2),
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Imm(RegisterId(2), SIRValue::new(0x34u64)),
                        SIRInstruction::Store(
                            stable,
                            SIROffset::Static(0),
                            8,
                            RegisterId(2),
                            Vec::new(),
                            Vec::new(),
                        ),
                    ],
                    terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
                },
            ),
            (
                BlockId(3),
                BasicBlock {
                    id: BlockId(3),
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Jump(BlockId(4), Vec::new()),
                },
            ),
            (
                BlockId(4),
                BasicBlock {
                    id: BlockId(4),
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Load(RegisterId(3), stable, SIROffset::Static(0), 8),
                        SIRInstruction::Store(
                            working,
                            SIROffset::Static(0),
                            8,
                            RegisterId(3),
                            Vec::new(),
                            Vec::new(),
                        ),
                    ],
                    terminator: SIRTerminator::Return,
                },
            ),
        ];
        let eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: blocks.into_iter().collect(),
            register_map: [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
                (RegisterId(3), bit(8)),
            ]
            .into_iter()
            .collect(),
        };

        let report = analyze(&eu, BlockId(4)).unwrap();
        assert_eq!(report.demanded_ff_loads, 1);
        assert_eq!(report.candidate_removable_loads, 1);
        assert_eq!(report.candidate_backing_stores, 2);
        assert_eq!(report.memory_phi_operands, 2);
    }

    #[test]
    fn distinct_range_demands_do_not_walk_all_preceding_stores() {
        const RANGES: usize = 128;
        let stable = address(STABLE_REGION);
        let working = address(WORKING_REGION);
        let mut comb_instructions = Vec::new();
        let mut ff_instructions = Vec::new();
        let mut register_map = crate::HashMap::default();
        for index in 0..RANGES {
            let source = RegisterId(index);
            let loaded = RegisterId(RANGES + index);
            register_map.insert(source, bit(1));
            register_map.insert(loaded, bit(1));
            comb_instructions.push(SIRInstruction::Imm(
                source,
                SIRValue::new((index & 1) as u64),
            ));
            comb_instructions.push(SIRInstruction::Store(
                stable,
                SIROffset::Static(index),
                1,
                source,
                Vec::new(),
                Vec::new(),
            ));
            ff_instructions.push(SIRInstruction::Load(
                loaded,
                stable,
                SIROffset::Static(index),
                1,
            ));
            ff_instructions.push(SIRInstruction::Store(
                working,
                SIROffset::Static(index),
                1,
                loaded,
                Vec::new(),
                Vec::new(),
            ));
        }
        let eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [
                (
                    BlockId(0),
                    BasicBlock {
                        id: BlockId(0),
                        params: Vec::new(),
                        instructions: comb_instructions,
                        terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
                    },
                ),
                (
                    BlockId(1),
                    BasicBlock {
                        id: BlockId(1),
                        params: Vec::new(),
                        instructions: ff_instructions,
                        terminator: SIRTerminator::Return,
                    },
                ),
            ]
            .into_iter()
            .collect(),
            register_map,
        };

        let report = analyze(&eu, BlockId(1)).unwrap();
        assert_eq!(report.demanded_ff_loads, RANGES);
        assert_eq!(report.candidate_removable_loads, RANGES);
        assert_eq!(report.candidate_backing_stores, RANGES);
        assert_eq!(report.materialized_range_fragments, RANGES);
    }
}
