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
    SIRTerminator, STABLE_REGION,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct ProgramPoint {
    block: BlockId,
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
    Load(ProgramPoint),
    Exit(BlockId),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RejectionReason {
    LiveOnEntry,
    UnknownWrite,
    FfDefinition,
    EffectfulDefinition,
    UnresolvedPhi,
}

impl fmt::Display for RejectionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::LiveOnEntry => "live-on-entry",
            Self::UnknownWrite => "unknown-write",
            Self::FfDefinition => "ff-definition",
            Self::EffectfulDefinition => "effectful-definition",
            Self::UnresolvedPhi => "unresolved-phi",
        })
    }
}

#[derive(Debug)]
struct RangeDecision {
    point: ProgramPoint,
    range: BitRange,
    rejection_reasons: BTreeSet<RejectionReason>,
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
    pub packed_address_calculation: usize,
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
    pub candidate_removable_stores: usize,
    pub candidate_backing_stores: usize,
    pub candidate_extract_merge_instructions: usize,
    pub candidate_producer_instructions: usize,
    pub rejected_live_on_entry: usize,
    pub rejected_unknown: usize,
    pub rejected_ff_definition: usize,
    pub rejected_effectful_definition: usize,
    pub materialized_range_fragments: usize,
    pub admitted_objects: usize,
    pub partially_admitted_objects: usize,
    pub rejected_objects: usize,
    pub verified_demands: usize,
    pub verified_roots: usize,
    pub verifier_passed: bool,
    pub rss_before_kib: usize,
    pub rss_after_kib: usize,
    pub process_peak_rss_kib: usize,
    pub origins: OriginCounts,
    range_decisions: Vec<RangeDecision>,
    block_origins: BTreeMap<BlockId, OriginCounts>,
}

impl fmt::Display for FeasibilityReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "blocks={} instructions={} static_accesses={} logical_segments={} \
             memory_def_fragments={} overlap_edges={} memory_phi_operands={} \
             maximum_fragments_per_access={} maximum_versions_per_object={} \
             demanded_ff_loads={} candidate_removable_loads={} \
             candidate_removable_stores={} candidate_backing_stores={} \
             candidate_extract_merge_instructions={} \
             candidate_producer_instructions={} rejected_live_on_entry={} \
             rejected_unknown={} rejected_ff_definition={} \
             rejected_effectful_definition={} \
             materialized_range_fragments={} admitted_objects={} \
             partially_admitted_objects={} rejected_objects={} \
             verified_demands={} verified_roots={} verifier_passed={} \
             rss_before_kib={} rss_after_kib={} process_peak_rss_kib={} \
             origin_address={} origin_memory={} \
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
            self.candidate_removable_stores,
            self.candidate_backing_stores,
            self.candidate_extract_merge_instructions,
            self.candidate_producer_instructions,
            self.rejected_live_on_entry,
            self.rejected_unknown,
            self.rejected_ff_definition,
            self.rejected_effectful_definition,
            self.materialized_range_fragments,
            self.admitted_objects,
            self.partially_admitted_objects,
            self.rejected_objects,
            self.verified_demands,
            self.verified_roots,
            self.verifier_passed,
            self.rss_before_kib,
            self.rss_after_kib,
            self.process_peak_rss_kib,
            self.origins.packed_address_calculation,
            self.origins.memory_traffic,
            self.origins.range_extraction,
            self.origins.range_insertion,
            self.origins.mask_generation,
            self.origins.mux_lowering,
            self.origins.unrelated_arithmetic,
        )
    }
}

impl FeasibilityReport {
    pub(crate) fn detail_lines(&self) -> impl Iterator<Item = String> + '_ {
        self.range_decisions
            .iter()
            .map(|decision| {
                let disposition = if decision.rejection_reasons.is_empty() {
                    "admitted".to_owned()
                } else {
                    decision
                        .rejection_reasons
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                };
                format!(
                    "kind=range point=b{}:i{} object={:?} range={}..{} \
                     disposition={disposition}",
                    decision.point.block.0,
                    decision.point.instruction,
                    decision.range.object,
                    decision.range.start,
                    decision.range.end,
                )
            })
            .chain(self.block_origins.iter().map(|(block, origins)| {
                format!(
                    "kind=candidate-block block=b{} address={} memory={} extract={} insert={} \
                     mask={} mux={} other={}",
                    block.0,
                    origins.packed_address_calculation,
                    origins.memory_traffic,
                    origins.range_extraction,
                    origins.range_insertion,
                    origins.mask_generation,
                    origins.mux_lowering,
                    origins.unrelated_arithmetic,
                )
            }))
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
    let rss_before_kib = resident_memory_kib().map_or(0, |(resident, _)| resident);
    let cfg = SirCfg::analyze(eu).map_err(FeasibilityError::Cfg)?;
    let phases = StatePhaseMap::fused(eu, &cfg, ff_entry).map_err(FeasibilityError::Phase)?;
    let ff_blocks = phases.ff_blocks().expect("fused phases expose FF blocks");
    let mut object_events = BTreeMap::<
        RegionedAbsoluteAddr,
        Vec<(usize, Event<RegionedAbsoluteAddr, ProgramPoint, Usage>)>,
    >::new();
    let mut definitions = BTreeMap::<ProgramPoint, MemoryDefinition>::new();
    let mut accesses = Vec::<(BitRange, bool)>::new();
    let mut demanded_loads = Vec::<(ProgramPoint, BitRange)>::new();
    let mut object_loads =
        BTreeMap::<RegionedAbsoluteAddr, Vec<(ProgramPoint, Option<BitRange>)>>::new();
    let mut store_sources = BTreeMap::<ProgramPoint, RegisterId>::new();

    for (block, &block_id) in cfg.block_ids.iter().enumerate() {
        for (instruction, inst) in eu.blocks[&block_id].instructions.iter().enumerate() {
            let point = ProgramPoint {
                block: block_id,
                instruction,
            };
            match inst {
                SIRInstruction::Load(_, address, offset, width)
                    if address.region == STABLE_REGION =>
                {
                    let range = match offset {
                        SIROffset::Static(start) => BitRange::new(*address, *start, *width),
                        SIROffset::Dynamic(_) | SIROffset::Element { .. } => None,
                    };
                    if let Some(range) = range {
                        accesses.push((range, false));
                        if ff_blocks.contains(&block_id) {
                            demanded_loads.push((point, range));
                        }
                    }
                    object_loads
                        .entry(*address)
                        .or_default()
                        .push((point, range));
                    object_events.entry(*address).or_default().push((
                        block,
                        Event::Use {
                            variable: *address,
                            usage: Usage::Load(point),
                        },
                    ));
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
                    let events = object_events.entry(*address).or_default();
                    events.push((
                        block,
                        Event::Use {
                            variable: *address,
                            usage: Usage::DefinitionInput(point),
                        },
                    ));
                    events.push((
                        block,
                        Event::Definition {
                            variable: *address,
                            definition: point,
                        },
                    ));
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
                    let events = object_events.entry(*destination).or_default();
                    events.push((
                        block,
                        Event::Use {
                            variable: *destination,
                            usage: Usage::DefinitionInput(point),
                        },
                    ));
                    events.push((
                        block,
                        Event::Definition {
                            variable: *destination,
                            definition: point,
                        },
                    ));
                }
                _ => {}
            }
        }
    }

    let endpoints = range_endpoints(&accesses);
    let mut report = range_shape_report(&accesses, &endpoints);
    report.blocks = eu.blocks.len();
    report.instructions = eu
        .blocks
        .values()
        .map(|block| block.instructions.len() + 1)
        .sum();
    report.demanded_ff_loads = demanded_loads.len();

    let mut demanded_by_object =
        BTreeMap::<RegionedAbsoluteAddr, Vec<(ProgramPoint, BitRange)>>::new();
    for (point, range) in demanded_loads {
        demanded_by_object
            .entry(range.object)
            .or_default()
            .push((point, range));
    }
    let mut candidate_stores = BTreeSet::<ProgramPoint>::new();
    let mut candidate_loads = BTreeSet::<ProgramPoint>::new();
    let mut required_stores = BTreeSet::<ProgramPoint>::new();
    let exit_blocks = cfg
        .block_ids
        .iter()
        .enumerate()
        .filter_map(|(block, block_id)| {
            matches!(
                eu.blocks[block_id].terminator,
                SIRTerminator::Return | SIRTerminator::Error(_)
            )
            .then_some((block, *block_id))
        })
        .collect::<Vec<_>>();
    for (object, demanded_loads) in demanded_by_object {
        let sparse_events =
            object_events
                .remove(&object)
                .ok_or(FeasibilityError::InvalidMemoryGraph(
                    "demanded object has no StateSSA events",
                ))?;
        let definition_points = sparse_events
            .iter()
            .filter_map(|(_, event)| match event {
                Event::Definition { definition, .. } => Some(*definition),
                Event::Use { .. } => None,
            })
            .collect::<Vec<_>>();
        let mut events = vec![
            Vec::<Event<RegionedAbsoluteAddr, ProgramPoint, Usage>>::new();
            cfg.block_ids.len()
        ];
        for (block, event) in sparse_events {
            events[block].push(event);
        }
        for &(block, block_id) in &exit_blocks {
            events[block].push(Event::Use {
                variable: object,
                usage: Usage::Exit(block_id),
            });
        }
        let state_ssa = ssa::build(&cfg, &events).map_err(FeasibilityError::StateSsa)?;
        let definition_inputs = definition_points
            .into_iter()
            .map(|point| {
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
        let mut resolver =
            DemandResolver::new(&state_ssa, &endpoints, &definitions, &definition_inputs);
        for (point, range) in demanded_loads {
            let start = state_ssa.uses.get(&Usage::Load(point)).copied().ok_or(
                FeasibilityError::InvalidMemoryGraph("FF load has no reaching object version"),
            )?;
            let pieces = resolver.resolve(DemandKey {
                version: start,
                range,
            })?;
            verify_resolved_pieces(range, &pieces, &definitions, &resolver.stats.phi_sources)?;
            report.verified_demands += 1;
            let mut candidate = true;
            let mut load_stores = BTreeSet::<ProgramPoint>::new();
            let mut visited_sources = BTreeSet::new();
            let mut rejection_reasons = BTreeSet::new();
            for piece in &pieces {
                candidate &= classify_candidate_source(
                    piece.source,
                    &resolver.stats.phi_sources,
                    &definitions,
                    &mut load_stores,
                    &mut visited_sources,
                    &mut rejection_reasons,
                    &mut report,
                );
            }
            report.range_decisions.push(RangeDecision {
                point,
                range,
                rejection_reasons,
            });
            if candidate {
                report.candidate_removable_loads += 1;
                candidate_loads.insert(point);
                candidate_stores.extend(load_stores);
            }
        }
        let points = &endpoints[&object];
        let full_range = BitRange {
            object,
            start: points[0],
            end: *points.last().unwrap(),
        };
        for (point, range) in object_loads.remove(&object).unwrap_or_default() {
            if candidate_loads.contains(&point) {
                continue;
            }
            let version = state_ssa.uses.get(&Usage::Load(point)).copied().ok_or(
                FeasibilityError::InvalidMemoryGraph("root load has no reaching object version"),
            )?;
            let range = range.unwrap_or(full_range);
            let pieces = resolver.resolve(DemandKey { version, range })?;
            verify_resolved_pieces(range, &pieces, &definitions, &resolver.stats.phi_sources)?;
            collect_required_sources(&pieces, &resolver.stats.phi_sources, &mut required_stores)?;
            report.verified_roots += 1;
        }
        for &(_, block_id) in &exit_blocks {
            let version = state_ssa.uses.get(&Usage::Exit(block_id)).copied().ok_or(
                FeasibilityError::InvalidMemoryGraph("exit root has no reaching object version"),
            )?;
            let pieces = resolver.resolve(DemandKey {
                version,
                range: full_range,
            })?;
            verify_resolved_pieces(
                full_range,
                &pieces,
                &definitions,
                &resolver.stats.phi_sources,
            )?;
            collect_required_sources(&pieces, &resolver.stats.phi_sources, &mut required_stores)?;
            report.verified_roots += 1;
        }
        report.memory_phi_operands += resolver.stats.phi_operands;
        report.materialized_range_fragments += resolver.stats.materialized_fragments;
        report.maximum_versions_per_object = report.maximum_versions_per_object.max(
            resolver
                .stats
                .maximum_versions_per_object
                .values()
                .map(BTreeSet::len)
                .max()
                .unwrap_or(0),
        );
    }
    report.candidate_backing_stores = candidate_stores.len();
    report.candidate_removable_stores = candidate_stores.difference(&required_stores).count();
    (report.origins, report.block_origins) =
        classify_candidate_origins(eu, &candidate_loads, &candidate_stores, &store_sources);
    report.candidate_extract_merge_instructions =
        report.origins.range_extraction + report.origins.range_insertion;
    report.candidate_producer_instructions = report
        .origins
        .range_extraction
        .saturating_add(report.origins.range_insertion)
        .saturating_add(report.origins.mask_generation)
        .saturating_add(report.origins.mux_lowering)
        .saturating_add(report.origins.unrelated_arithmetic);
    classify_object_coverage(&mut report);
    report.verifier_passed = report.verified_demands == report.demanded_ff_loads;
    if let Some((resident, peak)) = resident_memory_kib() {
        report.rss_before_kib = rss_before_kib;
        report.rss_after_kib = resident;
        report.process_peak_rss_kib = peak;
    }
    Ok(report)
}

fn classify_candidate_source(
    source: LogicalSource,
    phi_sources: &BTreeMap<LogicalSource, Vec<LogicalSource>>,
    definitions: &BTreeMap<ProgramPoint, MemoryDefinition>,
    stores: &mut BTreeSet<ProgramPoint>,
    visited: &mut BTreeSet<LogicalSource>,
    reasons: &mut BTreeSet<RejectionReason>,
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
                reasons.insert(RejectionReason::FfDefinition);
                admissible = false;
            }
            if definition.effectful {
                report.rejected_effectful_definition += 1;
                reasons.insert(RejectionReason::EffectfulDefinition);
                admissible = false;
            }
            if admissible {
                stores.insert(point);
            }
            admissible
        }
        LogicalSource::LiveOnEntry => {
            report.rejected_live_on_entry += 1;
            reasons.insert(RejectionReason::LiveOnEntry);
            false
        }
        LogicalSource::Unknown(_) => {
            report.rejected_unknown += 1;
            reasons.insert(RejectionReason::UnknownWrite);
            false
        }
        phi @ LogicalSource::Phi { .. } => {
            let Some(inputs) = phi_sources.get(&phi) else {
                report.rejected_unknown += 1;
                reasons.insert(RejectionReason::UnresolvedPhi);
                return false;
            };
            inputs.iter().copied().fold(true, |admissible, input| {
                classify_candidate_source(
                    input,
                    phi_sources,
                    definitions,
                    stores,
                    visited,
                    reasons,
                    report,
                ) && admissible
            })
        }
    }
}

fn collect_required_sources(
    pieces: &[Piece],
    phi_sources: &BTreeMap<LogicalSource, Vec<LogicalSource>>,
    stores: &mut BTreeSet<ProgramPoint>,
) -> Result<(), FeasibilityError> {
    let mut work = pieces.iter().map(|piece| piece.source).collect::<Vec<_>>();
    let mut visited = BTreeSet::new();
    while let Some(source) = work.pop() {
        if !visited.insert(source) {
            continue;
        }
        match source {
            LogicalSource::Definition(point) | LogicalSource::Unknown(point) => {
                stores.insert(point);
            }
            LogicalSource::LiveOnEntry => {}
            phi @ LogicalSource::Phi { .. } => {
                let Some(inputs) = phi_sources.get(&phi) else {
                    return Err(FeasibilityError::InvalidMemoryGraph(
                        "semantic root reaches an unresolved phi",
                    ));
                };
                work.extend(inputs.iter().copied());
            }
        }
    }
    Ok(())
}

fn verify_resolved_pieces(
    demand: BitRange,
    pieces: &[Piece],
    definitions: &BTreeMap<ProgramPoint, MemoryDefinition>,
    phi_sources: &BTreeMap<LogicalSource, Vec<LogicalSource>>,
) -> Result<(), FeasibilityError> {
    let mut next = demand.start;
    for piece in pieces {
        if piece.start != next || piece.start >= piece.end || piece.end > demand.end {
            return Err(FeasibilityError::InvalidMemoryGraph(
                "resolved fragments do not exactly partition the demand",
            ));
        }
        match piece.source {
            LogicalSource::Definition(point) => {
                let Some(definition) = definitions.get(&point) else {
                    return Err(FeasibilityError::InvalidMemoryGraph(
                        "resolved definition is absent",
                    ));
                };
                let DefinitionKind::Exact(range) = definition.kind else {
                    return Err(FeasibilityError::InvalidMemoryGraph(
                        "exact source names an unknown definition",
                    ));
                };
                if range.object != demand.object
                    || piece.start < range.start
                    || range.end < piece.end
                {
                    return Err(FeasibilityError::InvalidMemoryGraph(
                        "resolved definition does not cover its fragment",
                    ));
                }
            }
            LogicalSource::Unknown(point) => {
                let Some(definition) = definitions.get(&point) else {
                    return Err(FeasibilityError::InvalidMemoryGraph(
                        "resolved unknown definition is absent",
                    ));
                };
                if definition_object(*definition) != demand.object {
                    return Err(FeasibilityError::InvalidMemoryGraph(
                        "unknown definition belongs to another object",
                    ));
                }
            }
            phi @ LogicalSource::Phi {
                object, start, end, ..
            } => {
                if object != demand.object
                    || start != piece.start
                    || end != piece.end
                    || !phi_sources.contains_key(&phi)
                {
                    return Err(FeasibilityError::InvalidMemoryGraph(
                        "resolved phi has no exact source certificate",
                    ));
                }
            }
            LogicalSource::LiveOnEntry => {}
        }
        next = piece.end;
    }
    if next != demand.end {
        return Err(FeasibilityError::InvalidMemoryGraph(
            "resolved fragments leave an uncovered suffix",
        ));
    }
    Ok(())
}

fn classify_object_coverage(report: &mut FeasibilityReport) {
    let mut objects = BTreeMap::<RegionedAbsoluteAddr, (bool, bool)>::new();
    for decision in &report.range_decisions {
        let entry = objects.entry(decision.range.object).or_default();
        if decision.rejection_reasons.is_empty() {
            entry.0 = true;
        } else {
            entry.1 = true;
        }
    }
    for (has_admitted, has_rejected) in objects.into_values() {
        match (has_admitted, has_rejected) {
            (true, false) => report.admitted_objects += 1,
            (true, true) => report.partially_admitted_objects += 1,
            (false, true) => report.rejected_objects += 1,
            (false, false) => unreachable!("an object has at least one range decision"),
        }
    }
}

fn resident_memory_kib() -> Option<(usize, usize)> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let value = |name: &str| {
        status.lines().find_map(|line| {
            let value = line.strip_prefix(name)?;
            value.split_ascii_whitespace().next()?.parse::<usize>().ok()
        })
    };
    Some((value("VmRSS:")?, value("VmHWM:")?))
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
    loads: &BTreeSet<ProgramPoint>,
    stores: &BTreeSet<ProgramPoint>,
    store_sources: &BTreeMap<ProgramPoint, RegisterId>,
) -> (OriginCounts, BTreeMap<BlockId, OriginCounts>) {
    let mut definitions =
        HashMap::<RegisterId, (ProgramPoint, &SIRInstruction<RegionedAbsoluteAddr>)>::new();
    for block in eu.blocks.values() {
        for (instruction, inst) in block.instructions.iter().enumerate() {
            if let Some(definition) = super::shared::def_reg(inst) {
                definitions.insert(
                    definition,
                    (
                        ProgramPoint {
                            block: block.id,
                            instruction,
                        },
                        inst,
                    ),
                );
            }
        }
    }
    let mut work = stores
        .iter()
        .filter_map(|store| store_sources.get(store).copied())
        .collect::<Vec<_>>();
    let mut visited = HashSet::new();
    let mut counts = OriginCounts {
        memory_traffic: stores.len() + loads.len(),
        ..OriginCounts::default()
    };
    let mut block_counts = BTreeMap::<BlockId, OriginCounts>::new();
    for point in loads.iter().chain(stores) {
        block_counts.entry(point.block).or_default().memory_traffic += 1;
    }
    while let Some(value) = work.pop() {
        if !visited.insert(value) {
            continue;
        }
        let Some((point, inst)) = definitions.get(&value).copied() else {
            continue;
        };
        let in_block = block_counts.entry(point.block).or_default();
        match inst {
            SIRInstruction::Load(..) => {
                counts.memory_traffic += 1;
                in_block.memory_traffic += 1;
            }
            SIRInstruction::Slice(..) => {
                counts.range_extraction += 1;
                in_block.range_extraction += 1;
            }
            SIRInstruction::Concat(..) => {
                counts.range_insertion += 1;
                in_block.range_insertion += 1;
            }
            SIRInstruction::Binary(_, _, BinaryOp::And | BinaryOp::Shl | BinaryOp::Shr, _) => {
                counts.mask_generation += 1;
                in_block.mask_generation += 1;
            }
            SIRInstruction::Mux(..) => {
                counts.mux_lowering += 1;
                in_block.mux_lowering += 1;
            }
            SIRInstruction::Imm(..) | SIRInstruction::Binary(..) | SIRInstruction::Unary(..) => {
                counts.unrelated_arithmetic += 1;
                in_block.unrelated_arithmetic += 1;
            }
            SIRInstruction::Store(..)
            | SIRInstruction::Commit(..)
            | SIRInstruction::RuntimeEvent { .. }
            | SIRInstruction::CombCaptureEvent { .. }
            | SIRInstruction::CombCaptureEnableIfChanged { .. } => {}
        }
        work.extend(instruction_operands(inst));
    }
    (counts, block_counts)
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
    fn marks_a_comb_store_dead_when_ff_publication_replaces_its_home() {
        let stable = address(STABLE_REGION);
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
            ],
            terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
        };
        let ff = BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Load(RegisterId(1), stable, SIROffset::Static(0), 8),
                SIRInstruction::Store(
                    stable,
                    SIROffset::Static(0),
                    8,
                    RegisterId(1),
                    Vec::new(),
                    Vec::new(),
                ),
            ],
            terminator: SIRTerminator::Return,
        };
        let eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(BlockId(0), comb), (BlockId(1), ff)].into_iter().collect(),
            register_map: [(RegisterId(0), bit(8)), (RegisterId(1), bit(8))]
                .into_iter()
                .collect(),
        };

        let report = analyze(&eu, BlockId(1)).unwrap();
        assert_eq!(report.candidate_removable_loads, 1);
        assert_eq!(report.candidate_backing_stores, 1);
        assert_eq!(report.candidate_removable_stores, 1);
        assert!(report.verifier_passed);
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
    fn verifies_loop_phi_with_an_unchanged_range() {
        let stable = address(STABLE_REGION);
        let working = address(WORKING_REGION);
        let eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [
                (
                    BlockId(0),
                    BasicBlock {
                        id: BlockId(0),
                        params: Vec::new(),
                        instructions: vec![
                            SIRInstruction::Imm(RegisterId(0), SIRValue::new(0x12u64)),
                            SIRInstruction::Store(
                                stable,
                                SIROffset::Static(8),
                                8,
                                RegisterId(0),
                                Vec::new(),
                                Vec::new(),
                            ),
                        ],
                        terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
                    },
                ),
                (
                    BlockId(1),
                    BasicBlock {
                        id: BlockId(1),
                        params: Vec::new(),
                        instructions: vec![
                            SIRInstruction::Imm(RegisterId(1), SIRValue::new(0x34u64)),
                            SIRInstruction::Store(
                                stable,
                                SIROffset::Static(0),
                                8,
                                RegisterId(1),
                                Vec::new(),
                                Vec::new(),
                            ),
                            SIRInstruction::Imm(RegisterId(2), SIRValue::new(0u64)),
                        ],
                        terminator: SIRTerminator::Branch {
                            cond: RegisterId(2),
                            true_block: (BlockId(1), Vec::new()),
                            false_block: (BlockId(2), Vec::new()),
                        },
                    },
                ),
                (
                    BlockId(2),
                    BasicBlock {
                        id: BlockId(2),
                        params: Vec::new(),
                        instructions: vec![
                            SIRInstruction::Load(RegisterId(3), stable, SIROffset::Static(0), 16),
                            SIRInstruction::Store(
                                working,
                                SIROffset::Static(0),
                                16,
                                RegisterId(3),
                                Vec::new(),
                                Vec::new(),
                            ),
                        ],
                        terminator: SIRTerminator::Return,
                    },
                ),
            ]
            .into_iter()
            .collect(),
            register_map: [
                (RegisterId(0), bit(8)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(1)),
                (RegisterId(3), bit(16)),
            ]
            .into_iter()
            .collect(),
        };

        let report = analyze(&eu, BlockId(2)).unwrap();
        assert_eq!(report.candidate_removable_loads, 1);
        assert_eq!(report.candidate_backing_stores, 2);
        assert_eq!(report.memory_phi_operands, 2);
        assert!(report.verifier_passed);
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
        // One fragment per FF demand plus one complete exit-root partition.
        assert_eq!(report.materialized_range_fragments, RANGES * 2);
    }
}
