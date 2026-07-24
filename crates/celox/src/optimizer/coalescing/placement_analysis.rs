//! Whole-unit value occurrence and execution-safety analysis.
//!
//! SIR `RegisterId`s are SSA names, but placement must retain the concrete
//! occurrence which produced a name and, for a load, the exact state version
//! observed by that occurrence.  This module is deliberately analysis-only:
//! a later atomic placement pass consumes these facts instead of mutating the
//! CFG while it is still discovering a decision region.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use super::shared::def_reg;
use super::state_ssa::{MemoryVersionId, StateFragment, StatePhaseMap, StateSsa, StateSsaError};
use crate::HashMap;
use crate::ir::cfg::{SirCfg, SirCfgError};
use crate::ir::{
    BlockId, ExecutionUnit, RegionedAbsoluteAddr, RegisterId, SIRInstruction, SIROffset,
    SIRTerminator,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct ValueId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum ValueOrigin {
    Parameter { block: BlockId, index: usize },
    Instruction { block: BlockId, index: usize },
}

impl ValueOrigin {
    pub fn block(self) -> BlockId {
        match self {
            Self::Parameter { block, .. } | Self::Instruction { block, .. } => block,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct StateToken {
    pub fragment: StateFragment,
    pub slot: usize,
    pub version: MemoryVersionId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PinReason {
    BlockParameter,
    UnversionedStateRead,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ValueSafety {
    Pure,
    StateRead(StateToken),
    Pinned(PinReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ValueUse {
    Instruction {
        block: BlockId,
        index: usize,
        operand: usize,
    },
    BranchCondition {
        block: BlockId,
    },
    EdgeArgument {
        predecessor: BlockId,
        successor: BlockId,
        argument: usize,
        truth: Option<bool>,
    },
}

impl ValueUse {
    /// Edge arguments are consumed on the predecessor edge, like an SSA phi
    /// use, rather than at the successor block entry.
    pub fn execution_block(self) -> BlockId {
        match self {
            Self::Instruction { block, .. } | Self::BranchCondition { block } => block,
            Self::EdgeArgument { predecessor, .. } => predecessor,
        }
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)] // All fields are consumed by the Step 4c placement plan.
pub(super) struct ValueOccurrence {
    pub id: ValueId,
    pub register: RegisterId,
    pub origin: ValueOrigin,
    pub safety: ValueSafety,
    pub operands: Vec<ValueId>,
    pub uses: Vec<ValueUse>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct EffectId(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum EffectToken {
    Entry,
    Phi(BlockId),
    Occurrence(EffectId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EffectKind {
    StateWrite,
    Commit,
    RuntimeEvent,
    CaptureEvent,
    CaptureEnable,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EffectLocation {
    Instruction { block: BlockId, index: usize },
    Terminator { block: BlockId },
}

impl EffectLocation {
    fn block(self) -> BlockId {
        match self {
            Self::Instruction { block, .. } | Self::Terminator { block } => block,
        }
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)] // `kind` and identity are consumed by plan verification.
pub(super) struct EffectOccurrence {
    pub id: EffectId,
    pub location: EffectLocation,
    pub kind: EffectKind,
    pub input: EffectToken,
    pub output: EffectToken,
    /// Branch blocks on which this occurrence is control-dependent.
    pub control_domain: Vec<BlockId>,
}

#[derive(Clone, Debug)]
pub(super) struct EffectPhi {
    pub block: BlockId,
    pub token: EffectToken,
    /// `None` is the virtual entry edge when the entry itself is a loop header.
    pub incoming: Vec<(Option<BlockId>, EffectToken)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PlacementBounds {
    pub earliest: BlockId,
    pub latest: BlockId,
    /// Legal existing blocks from earliest to latest.  The original block
    /// denotes the original instruction point; descendants denote block entry.
    pub legal_blocks: Vec<BlockId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MaterializationFrontierCone {
    /// Values which must be moved or cloned, in operand-before-user order.
    pub instructions: Vec<ValueId>,
    /// Leaves supplied independently at the target instead of recursively
    /// reproducing their defining cones.
    pub frontier: Vec<ValueId>,
}

#[derive(Debug)]
pub(super) enum PlacementAnalysisError {
    InvalidSir,
    Cfg(SirCfgError),
    State(StateSsaError),
    DuplicateDefinition(RegisterId),
    MissingDefinition(RegisterId),
    InvalidEffectSsa(&'static str),
}

impl fmt::Display for PlacementAnalysisError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSir => formatter.write_str("placement input is not valid SIR SSA"),
            Self::Cfg(error) => write!(formatter, "placement CFG analysis failed: {error}"),
            Self::State(error) => write!(formatter, "placement StateSSA analysis failed: {error}"),
            Self::DuplicateDefinition(register) => {
                write!(
                    formatter,
                    "placement found duplicate definition of r{}",
                    register.0
                )
            }
            Self::MissingDefinition(register) => {
                write!(
                    formatter,
                    "placement found use without definition of r{}",
                    register.0
                )
            }
            Self::InvalidEffectSsa(message) => {
                write!(formatter, "invalid placement effect SSA: {message}")
            }
        }
    }
}

impl std::error::Error for PlacementAnalysisError {}

impl From<SirCfgError> for PlacementAnalysisError {
    fn from(error: SirCfgError) -> Self {
        Self::Cfg(error)
    }
}

impl From<StateSsaError> for PlacementAnalysisError {
    fn from(error: StateSsaError) -> Self {
        Self::State(error)
    }
}

#[allow(dead_code)] // The atomic region rewriter is added in the next slice.
pub(super) struct PlacementAnalysis {
    pub cfg: SirCfg,
    pub values: Vec<ValueOccurrence>,
    pub effects: Vec<EffectOccurrence>,
    pub effect_phis: Vec<EffectPhi>,
    register_values: HashMap<RegisterId, ValueId>,
    state: BTreeMap<u32, StateSsa>,
    loop_depth: Vec<usize>,
    loop_membership: Vec<Vec<usize>>,
}

#[allow(dead_code)]
impl PlacementAnalysis {
    pub fn analyze(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    ) -> Result<Self, PlacementAnalysisError> {
        Self::analyze_impl(eu, true, false)
    }

    /// Build placement facts using the physical two-state storage contract.
    /// Source-level `bit`/`logic` differences do not split a MemorySSA slot
    /// when the native backend has no unknown-value plane.
    pub fn analyze_two_state(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    ) -> Result<Self, PlacementAnalysisError> {
        Self::analyze_impl(eu, true, true)
    }

    /// Build only CFG and register value occurrences. State reads remain
    /// pinned and effect SSA is omitted; this is the bounded-memory query used
    /// while classifying strictly pure rematerialization cones.
    pub fn analyze_pure_values(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    ) -> Result<Self, PlacementAnalysisError> {
        Self::analyze_impl(eu, false, false)
    }

    fn analyze_impl(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        include_state_and_effects: bool,
        two_state_memory: bool,
    ) -> Result<Self, PlacementAnalysisError> {
        if eu.verify_result().is_err() {
            return Err(PlacementAnalysisError::InvalidSir);
        }
        let cfg = if include_state_and_effects {
            SirCfg::analyze(eu)?
        } else {
            SirCfg::analyze_forward(eu)?
        };
        let state = if include_state_and_effects {
            analyze_state_versions(eu, &cfg, two_state_memory)?
        } else {
            BTreeMap::new()
        };

        let mut values = Vec::<ValueOccurrence>::new();
        let mut register_values = HashMap::<RegisterId, ValueId>::default();
        for &block_id in &cfg.block_ids {
            let block = &eu.blocks[&block_id];
            for (index, &register) in block.params.iter().enumerate() {
                insert_value(
                    &mut values,
                    &mut register_values,
                    register,
                    ValueOrigin::Parameter {
                        block: block_id,
                        index,
                    },
                    ValueSafety::Pinned(PinReason::BlockParameter),
                )?;
            }
            for (index, instruction) in block.instructions.iter().enumerate() {
                let Some(register) = def_reg(instruction) else {
                    continue;
                };
                let safety = match instruction {
                    SIRInstruction::Imm(..)
                    | SIRInstruction::Binary(..)
                    | SIRInstruction::Unary(..)
                    | SIRInstruction::Concat(..)
                    | SIRInstruction::Slice(..)
                    | SIRInstruction::Mux(..) => ValueSafety::Pure,
                    SIRInstruction::Load(destination, address, ..) => {
                        if include_state_and_effects {
                            state
                                .get(&address.region)
                                .and_then(|state| state.read_version(block_id, index, *destination))
                                .map(|(slot, version)| {
                                    ValueSafety::StateRead(StateToken {
                                        fragment: state[&address.region].slots[slot].fragment,
                                        slot,
                                        version,
                                    })
                                })
                                .unwrap_or(ValueSafety::Pinned(PinReason::UnversionedStateRead))
                        } else {
                            ValueSafety::Pinned(PinReason::UnversionedStateRead)
                        }
                    }
                    SIRInstruction::Store(..)
                    | SIRInstruction::Commit(..)
                    | SIRInstruction::RuntimeEvent { .. }
                    | SIRInstruction::CombCaptureEvent { .. }
                    | SIRInstruction::CombCaptureEnableIfChanged { .. } => {
                        unreachable!("effect instruction cannot define a SIR register")
                    }
                };
                insert_value(
                    &mut values,
                    &mut register_values,
                    register,
                    ValueOrigin::Instruction {
                        block: block_id,
                        index,
                    },
                    safety,
                )?;
            }
        }

        collect_value_uses(eu, &cfg, &register_values, &mut values)?;
        let (effects, effect_phis) = if include_state_and_effects {
            build_effect_ssa(eu, &cfg)?
        } else {
            (Vec::new(), Vec::new())
        };
        let mut loop_membership = vec![Vec::new(); cfg.block_ids.len()];
        for (loop_index, natural_loop) in cfg.loops.iter().enumerate() {
            for &block in &natural_loop.blocks {
                loop_membership[block].push(loop_index);
            }
        }
        let loop_depth = loop_membership.iter().map(Vec::len).collect();

        Ok(Self {
            cfg,
            values,
            effects,
            effect_phis,
            register_values,
            state,
            loop_depth,
            loop_membership,
        })
    }

    pub fn value_for_register(&self, register: RegisterId) -> Option<ValueId> {
        self.register_values.get(&register).copied()
    }

    pub fn value(&self, value: ValueId) -> Option<&ValueOccurrence> {
        self.values.get(value.0)
    }

    /// Return the complete executable producer cone when every value can be
    /// placed at `target` without changing its state version or execution
    /// frequency. The result is postorder and therefore directly cloneable.
    pub fn rematerialization_cone(&self, root: ValueId, target: BlockId) -> Option<Vec<ValueId>> {
        let mut visited = BTreeSet::new();
        let mut active = BTreeSet::new();
        let mut output = Vec::new();
        let mut work = vec![(root, false)];
        while let Some((value, exiting)) = work.pop() {
            if exiting {
                active.remove(&value);
                output.push(value);
                continue;
            }
            if visited.contains(&value) {
                continue;
            }
            let occurrence = self.value(value)?;
            if matches!(occurrence.safety, ValueSafety::Pinned(_))
                || !self.can_sink_to_block(value, target)
                || !active.insert(value)
            {
                return None;
            }
            visited.insert(value);
            work.push((value, true));
            for operand in occurrence.operands.iter().rev().copied() {
                work.push((operand, false));
            }
        }
        Some(output)
    }

    /// Form a use-local cone while stopping at values already used in the
    /// target block and at pinned/state-read leaves. The caller must
    /// independently prove a concrete source for every returned frontier
    /// value before this becomes an executable rematerialization recipe.
    pub fn materialization_frontier_cone(
        &self,
        root: ValueId,
        target: BlockId,
    ) -> Option<MaterializationFrontierCone> {
        let mut visited = BTreeSet::new();
        let mut active = BTreeSet::new();
        let mut instructions = Vec::new();
        let mut frontier = Vec::new();
        let mut work = vec![(root, false)];
        while let Some((value, exiting)) = work.pop() {
            if exiting {
                active.remove(&value);
                instructions.push(value);
                continue;
            }
            if visited.contains(&value) {
                continue;
            }
            let occurrence = self.value(value)?;
            if value != root
                && occurrence
                    .uses
                    .iter()
                    .any(|usage| usage.execution_block() == target)
            {
                visited.insert(value);
                frontier.push(value);
                continue;
            }
            match occurrence.safety {
                ValueSafety::Pure => {
                    if !self.can_sink_to_block(value, target) || !active.insert(value) {
                        return None;
                    }
                    visited.insert(value);
                    work.push((value, true));
                    for operand in occurrence.operands.iter().rev().copied() {
                        work.push((operand, false));
                    }
                }
                ValueSafety::StateRead(_) | ValueSafety::Pinned(_) => {
                    visited.insert(value);
                    frontier.push(value);
                }
            }
        }
        Some(MaterializationFrontierCone {
            instructions,
            frontier,
        })
    }

    /// Check one concrete placement without constructing the full
    /// ScheduleEarly/ScheduleLate path. Cone discovery calls this for every
    /// visited value, so allocating that path here would make a sparse query
    /// retain memory proportional to all repeatedly explored paths.
    pub fn can_sink_to_block(&self, value: ValueId, target: BlockId) -> bool {
        let Some(occurrence) = self.value(value) else {
            return false;
        };
        let Some(origin) = self.cfg.block_index(occurrence.origin.block()) else {
            return false;
        };
        let Some(target) = self.cfg.block_index(target) else {
            return false;
        };
        self.cfg.dominators.dominates(origin, target)
            && self.execution_safe_at_block(occurrence, origin, target)
    }

    /// Whether a state-read value has an executable reload of the exact same
    /// memory version at `target`.
    ///
    /// This is deliberately weaker than moving the original Load occurrence:
    /// rematerializing a persistent-state leaf does not require the original
    /// Load's block to dominate the target. The target's entry StateVersion is
    /// the materialization source named by the plan.
    pub fn can_materialize_state_read_at_block(&self, value: ValueId, target: BlockId) -> bool {
        let Some(occurrence) = self.value(value) else {
            return false;
        };
        let ValueSafety::StateRead(token) = occurrence.safety else {
            return false;
        };
        self.state_for(token)
            .and_then(|state| state.entry_version(target, token.slot))
            == Some(token.version)
    }

    /// Find the closest dominator of `target` whose entry exposes every named
    /// state-read version. This is the legal point for one aggregate snapshot
    /// when those versions are overwritten before the final use cluster.
    pub fn latest_common_state_materialization_block(
        &self,
        values: &[ValueId],
        target: BlockId,
    ) -> Option<BlockId> {
        let tokens = values
            .iter()
            .map(|&value| match self.value(value)?.safety {
                ValueSafety::StateRead(token) => Some(token),
                ValueSafety::Pure | ValueSafety::Pinned(_) => None,
            })
            .collect::<Option<Vec<_>>>()?;
        let mut block = self.cfg.block_index(target)?;
        loop {
            let block_id = self.cfg.block_ids[block];
            if tokens.iter().all(|&token| {
                self.state_for(token)
                    .and_then(|state| state.entry_version(block_id, token.slot))
                    == Some(token.version)
            }) {
                return Some(block_id);
            }
            block = self.cfg.dominators.idom[block]?;
        }
    }

    /// Find the earliest block on the target's dominator path at which all
    /// SSA occurrences are available. This is the placement for aggregating
    /// several exact SSA leaves before their separate live ranges continue to
    /// a distant sink.
    pub fn earliest_common_dominating_value_block(
        &self,
        values: &[ValueId],
        target: BlockId,
    ) -> Option<BlockId> {
        let origins = values
            .iter()
            .map(|&value| self.cfg.block_index(self.value(value)?.origin.block()))
            .collect::<Option<Vec<_>>>()?;
        let target = self.cfg.block_index(target)?;
        if origins
            .iter()
            .any(|&origin| !self.cfg.dominators.dominates(origin, target))
        {
            return None;
        }
        let mut path = Vec::new();
        let mut cursor = Some(target);
        while let Some(block) = cursor {
            path.push(block);
            cursor = self.cfg.dominators.idom[block];
        }
        path.reverse();
        path.into_iter()
            .find(|&block| {
                origins
                    .iter()
                    .all(|&origin| self.cfg.dominators.dominates(origin, block))
            })
            .map(|block| self.cfg.block_ids[block])
    }

    pub fn loop_depth(&self, block: BlockId) -> Option<usize> {
        self.cfg
            .block_index(block)
            .map(|block| self.loop_depth[block])
    }

    pub fn sink_bounds(&self, value: ValueId) -> Option<PlacementBounds> {
        let occurrence = self.value(value)?;
        self.sink_bounds_for_use_blocks(
            value,
            occurrence.uses.iter().map(|site| site.execution_block()),
        )
    }

    /// Compute ScheduleEarly/ScheduleLate for a prospective use envelope.
    /// This is the query used by an atomic decision plan after it has replaced
    /// a Mux-arm use by a synthetic edge-local use.
    pub fn sink_bounds_for_use_blocks(
        &self,
        value: ValueId,
        use_blocks: impl IntoIterator<Item = BlockId>,
    ) -> Option<PlacementBounds> {
        let occurrence = self.value(value)?;
        let origin_id = occurrence.origin.block();
        let origin = self.cfg.block_index(origin_id)?;
        let use_blocks = use_blocks
            .into_iter()
            .map(|block| self.cfg.block_index(block))
            .collect::<Option<BTreeSet<_>>>()?;
        let mut uses = use_blocks.into_iter();
        let mut latest = uses.next()?;
        for block in uses {
            latest = self.cfg.dominators.lca(latest, block)?;
        }
        if !self.cfg.dominators.dominates(origin, latest) {
            return None;
        }

        if matches!(occurrence.safety, ValueSafety::Pinned(_)) {
            return self
                .cfg
                .dominators
                .dominates(origin, latest)
                .then_some(PlacementBounds {
                    earliest: origin_id,
                    latest: origin_id,
                    legal_blocks: vec![origin_id],
                });
        }

        let mut path = Vec::new();
        let mut current = latest;
        loop {
            path.push(current);
            if current == origin {
                break;
            }
            current = self.cfg.dominators.idom[current]?;
        }
        path.reverse();
        let legal_blocks = path
            .into_iter()
            .filter(|&candidate| self.execution_safe_at_block(occurrence, origin, candidate))
            .map(|block| self.cfg.block_ids[block])
            .collect::<Vec<_>>();
        let (&earliest, &latest) = (legal_blocks.first()?, legal_blocks.last()?);
        Some(PlacementBounds {
            earliest,
            latest,
            legal_blocks,
        })
    }

    /// Test a synthetic block inserted on an existing predecessor edge.  A
    /// state read observes the predecessor's exit version; pure values need no
    /// token.  Pinned values and values crossing a cyclic SCC are rejected.
    pub fn can_sink_to_edge(&self, value: ValueId, predecessor: BlockId) -> bool {
        let Some(occurrence) = self.value(value) else {
            return false;
        };
        let Some(origin) = self.cfg.block_index(occurrence.origin.block()) else {
            return false;
        };
        let Some(predecessor_index) = self.cfg.block_index(predecessor) else {
            return false;
        };
        if !self.cfg.dominators.dominates(origin, predecessor_index)
            || !self.preserves_execution_frequency(origin, predecessor_index)
        {
            return false;
        }
        match occurrence.safety {
            ValueSafety::Pure => true,
            ValueSafety::StateRead(token) => {
                self.state_for(token)
                    .and_then(|state| state.exit_version(predecessor, token.slot))
                    == Some(token.version)
            }
            ValueSafety::Pinned(_) => false,
        }
    }

    fn execution_safe_at_block(
        &self,
        occurrence: &ValueOccurrence,
        origin: usize,
        candidate: usize,
    ) -> bool {
        if candidate == origin {
            return true;
        }
        if !self.preserves_execution_frequency(origin, candidate) {
            return false;
        }
        match occurrence.safety {
            ValueSafety::Pure => true,
            ValueSafety::StateRead(token) => {
                self.state_for(token).and_then(|state| {
                    state.entry_version(self.cfg.block_ids[candidate], token.slot)
                }) == Some(token.version)
            }
            ValueSafety::Pinned(_) => false,
        }
    }

    fn state_for(&self, token: StateToken) -> Option<&StateSsa> {
        self.state.get(&token.fragment.addr.region)
    }

    fn preserves_execution_frequency(&self, origin: usize, candidate: usize) -> bool {
        let origin_scc = self.cfg.scc_for_block[origin];
        let candidate_scc = self.cfg.scc_for_block[candidate];
        if self.cfg.sccs[origin_scc].cyclic {
            // A dominated block in the same reducible SCC and the exact same
            // natural-loop nest is a legal ScheduleLate destination: it
            // cannot enter a sibling or nested loop which repeats at a
            // different frequency. Irreducible SCCs have no single iteration
            // boundary, so keep their occurrences pinned.
            return candidate_scc == origin_scc
                && self.cfg.sccs[origin_scc].reducible_header.is_some()
                && self.loop_membership[candidate] == self.loop_membership[origin];
        }
        if self.cfg.sccs[candidate_scc].cyclic {
            return false;
        }
        self.loop_depth[candidate] <= self.loop_depth[origin]
    }
}

fn insert_value(
    values: &mut Vec<ValueOccurrence>,
    register_values: &mut HashMap<RegisterId, ValueId>,
    register: RegisterId,
    origin: ValueOrigin,
    safety: ValueSafety,
) -> Result<(), PlacementAnalysisError> {
    let id = ValueId(values.len());
    if register_values.insert(register, id).is_some() {
        return Err(PlacementAnalysisError::DuplicateDefinition(register));
    }
    values.push(ValueOccurrence {
        id,
        register,
        origin,
        safety,
        operands: Vec::new(),
        uses: Vec::new(),
    });
    Ok(())
}

fn analyze_state_versions(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    two_state: bool,
) -> Result<BTreeMap<u32, StateSsa>, PlacementAnalysisError> {
    let regions = cfg
        .block_ids
        .iter()
        .flat_map(|block| eu.blocks[block].instructions.iter())
        .filter_map(|instruction| match instruction {
            SIRInstruction::Load(_, address, _, _) => Some(address.region),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    regions
        .into_iter()
        .map(|region| {
            Ok((
                region,
                if two_state {
                    StateSsa::analyze_all_loads_two_state(
                        eu,
                        cfg,
                        region,
                        &StatePhaseMap::default(),
                    )?
                } else {
                    StateSsa::analyze_all_loads(eu, cfg, region, &StatePhaseMap::default())?
                },
            ))
        })
        .collect()
}

fn collect_value_uses(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    register_values: &HashMap<RegisterId, ValueId>,
    values: &mut [ValueOccurrence],
) -> Result<(), PlacementAnalysisError> {
    for &block_id in &cfg.block_ids {
        let block = &eu.blocks[&block_id];
        for (index, instruction) in block.instructions.iter().enumerate() {
            let operands = instruction_operands(instruction);
            if let Some(destination) = def_reg(instruction) {
                let value = *register_values
                    .get(&destination)
                    .ok_or(PlacementAnalysisError::MissingDefinition(destination))?;
                values[value.0].operands = operands
                    .iter()
                    .map(|register| {
                        register_values
                            .get(register)
                            .copied()
                            .ok_or(PlacementAnalysisError::MissingDefinition(*register))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
            }
            for (operand, register) in operands.into_iter().enumerate() {
                let value = *register_values
                    .get(&register)
                    .ok_or(PlacementAnalysisError::MissingDefinition(register))?;
                values[value.0].uses.push(ValueUse::Instruction {
                    block: block_id,
                    index,
                    operand,
                });
            }
        }
        match &block.terminator {
            SIRTerminator::Jump(successor, arguments) => {
                collect_edge_uses(
                    block_id,
                    *successor,
                    None,
                    arguments,
                    register_values,
                    values,
                )?;
            }
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                let condition = *register_values
                    .get(cond)
                    .ok_or(PlacementAnalysisError::MissingDefinition(*cond))?;
                values[condition.0]
                    .uses
                    .push(ValueUse::BranchCondition { block: block_id });
                collect_edge_uses(
                    block_id,
                    true_block.0,
                    Some(true),
                    &true_block.1,
                    register_values,
                    values,
                )?;
                collect_edge_uses(
                    block_id,
                    false_block.0,
                    Some(false),
                    &false_block.1,
                    register_values,
                    values,
                )?;
            }
            SIRTerminator::Switch { selector, .. } => {
                let selector = *register_values
                    .get(selector)
                    .ok_or(PlacementAnalysisError::MissingDefinition(*selector))?;
                values[selector.0]
                    .uses
                    .push(ValueUse::BranchCondition { block: block_id });
            }
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    Ok(())
}

fn collect_edge_uses(
    predecessor: BlockId,
    successor: BlockId,
    truth: Option<bool>,
    arguments: &[RegisterId],
    register_values: &HashMap<RegisterId, ValueId>,
    values: &mut [ValueOccurrence],
) -> Result<(), PlacementAnalysisError> {
    for (argument, &register) in arguments.iter().enumerate() {
        let value = *register_values
            .get(&register)
            .ok_or(PlacementAnalysisError::MissingDefinition(register))?;
        values[value.0].uses.push(ValueUse::EdgeArgument {
            predecessor,
            successor,
            argument,
            truth,
        });
    }
    Ok(())
}

fn instruction_operands(instruction: &SIRInstruction<RegionedAbsoluteAddr>) -> Vec<RegisterId> {
    match instruction {
        SIRInstruction::Imm(..) => Vec::new(),
        SIRInstruction::Binary(_, lhs, _, rhs) => vec![*lhs, *rhs],
        SIRInstruction::Unary(_, _, source) | SIRInstruction::Slice(_, source, _, _) => {
            vec![*source]
        }
        SIRInstruction::Load(_, _, offset, _) => offset_operands(offset),
        SIRInstruction::Store(_, offset, _, source, _, _) => offset_operands(offset)
            .into_iter()
            .chain(std::iter::once(*source))
            .collect(),
        SIRInstruction::Commit(_, _, offset, _, _) => offset_operands(offset),
        SIRInstruction::Concat(_, arguments)
        | SIRInstruction::RuntimeEvent {
            args: arguments, ..
        }
        | SIRInstruction::CombCaptureEvent {
            args: arguments, ..
        } => arguments.clone(),
        SIRInstruction::Mux(_, condition, true_value, false_value) => {
            vec![*condition, *true_value, *false_value]
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => vec![*old, *new],
    }
}

fn offset_operands(offset: &SIROffset) -> Vec<RegisterId> {
    offset.dynamic_registers().into_iter().flatten().collect()
}

fn build_effect_ssa(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
) -> Result<(Vec<EffectOccurrence>, Vec<EffectPhi>), PlacementAnalysisError> {
    let mut effects = Vec::<EffectOccurrence>::new();
    let mut effects_by_block = vec![Vec::<EffectId>::new(); cfg.block_ids.len()];
    let mut def_blocks = BTreeSet::<usize>::new();
    for (block, &block_id) in cfg.block_ids.iter().enumerate() {
        for (index, instruction) in eu.blocks[&block_id].instructions.iter().enumerate() {
            let kind = match instruction {
                SIRInstruction::Store(..) => Some(EffectKind::StateWrite),
                SIRInstruction::Commit(..) => Some(EffectKind::Commit),
                SIRInstruction::RuntimeEvent { .. } => Some(EffectKind::RuntimeEvent),
                SIRInstruction::CombCaptureEvent { .. } => Some(EffectKind::CaptureEvent),
                SIRInstruction::CombCaptureEnableIfChanged { .. } => {
                    Some(EffectKind::CaptureEnable)
                }
                SIRInstruction::Imm(..)
                | SIRInstruction::Binary(..)
                | SIRInstruction::Unary(..)
                | SIRInstruction::Load(..)
                | SIRInstruction::Concat(..)
                | SIRInstruction::Slice(..)
                | SIRInstruction::Mux(..) => None,
            };
            if let Some(kind) = kind {
                push_effect(
                    &mut effects,
                    &mut effects_by_block[block],
                    EffectLocation::Instruction {
                        block: block_id,
                        index,
                    },
                    kind,
                    cfg,
                );
                def_blocks.insert(block);
            }
        }
        if matches!(eu.blocks[&block_id].terminator, SIRTerminator::Error(_)) {
            push_effect(
                &mut effects,
                &mut effects_by_block[block],
                EffectLocation::Terminator { block: block_id },
                EffectKind::Error,
                cfg,
            );
            def_blocks.insert(block);
        }
    }

    let mut phi_blocks = BTreeSet::<usize>::new();
    let mut work = VecDeque::from_iter(def_blocks.iter().copied());
    while let Some(block) = work.pop_front() {
        for &frontier in &cfg.dominance_frontier[block] {
            if phi_blocks.insert(frontier) && !def_blocks.contains(&frontier) {
                work.push_back(frontier);
            }
        }
    }
    let mut phis = phi_blocks
        .iter()
        .map(|&block| EffectPhi {
            block: cfg.block_ids[block],
            token: EffectToken::Phi(cfg.block_ids[block]),
            incoming: (block == 0)
                .then_some((None, EffectToken::Entry))
                .into_iter()
                .collect(),
        })
        .collect::<Vec<_>>();
    let phi_index = phis
        .iter()
        .enumerate()
        .map(|(index, phi)| (cfg.index[&phi.block], index))
        .collect::<HashMap<_, _>>();

    enum Visit {
        Enter(usize),
        Exit(usize),
    }
    let mut tokens = vec![EffectToken::Entry];
    let mut visits = vec![Visit::Enter(0)];
    while let Some(visit) = visits.pop() {
        match visit {
            Visit::Exit(pushed) => {
                let next_len = tokens.len().checked_sub(pushed).ok_or(
                    PlacementAnalysisError::InvalidEffectSsa("token stack underflow"),
                )?;
                tokens.truncate(next_len);
            }
            Visit::Enter(block) => {
                let mut pushed = 0usize;
                if let Some(&phi) = phi_index.get(&block) {
                    tokens.push(phis[phi].token);
                    pushed += 1;
                }
                for &effect in &effects_by_block[block] {
                    let input = *tokens
                        .last()
                        .ok_or(PlacementAnalysisError::InvalidEffectSsa(
                            "empty token stack",
                        ))?;
                    let output = EffectToken::Occurrence(effect);
                    effects[effect.0].input = input;
                    effects[effect.0].output = output;
                    tokens.push(output);
                    pushed += 1;
                }
                let exit = *tokens
                    .last()
                    .ok_or(PlacementAnalysisError::InvalidEffectSsa(
                        "empty block-exit token",
                    ))?;
                for &successor in &cfg.successors[block] {
                    if let Some(&phi) = phi_index.get(&successor) {
                        phis[phi].incoming.push((Some(cfg.block_ids[block]), exit));
                    }
                }
                visits.push(Visit::Exit(pushed));
                for &child in cfg.dom_children[block].iter().rev() {
                    visits.push(Visit::Enter(child));
                }
            }
        }
    }

    for phi in &mut phis {
        phi.incoming.sort_by_key(|(predecessor, _)| *predecessor);
        phi.incoming.dedup_by_key(|(predecessor, _)| *predecessor);
        let block = cfg.index[&phi.block];
        let expected = cfg.predecessors[block].len() + usize::from(block == 0);
        if phi.incoming.len() != expected {
            return Err(PlacementAnalysisError::InvalidEffectSsa(
                "effect phi has incomplete incoming tokens",
            ));
        }
    }
    Ok((effects, phis))
}

fn push_effect(
    effects: &mut Vec<EffectOccurrence>,
    block_effects: &mut Vec<EffectId>,
    location: EffectLocation,
    kind: EffectKind,
    cfg: &SirCfg,
) {
    let id = EffectId(effects.len());
    let block = cfg.index[&location.block()];
    effects.push(EffectOccurrence {
        id,
        location,
        kind,
        input: EffectToken::Entry,
        output: EffectToken::Occurrence(id),
        control_domain: cfg.controllers[block]
            .iter()
            .map(|&controller| cfg.block_ids[controller])
            .collect(),
    });
    block_effects.push(id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        BasicBlock, BinaryOp, InstanceId, RegisterType, SIRValue, STABLE_REGION, UnaryOp,
    };
    use veryl_analyzer::ir::VarId;

    fn bit(width: usize) -> RegisterType {
        RegisterType::Bit {
            width,
            signed: false,
        }
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
        blocks: impl IntoIterator<Item = BasicBlock<RegionedAbsoluteAddr>>,
        registers: impl IntoIterator<Item = (RegisterId, RegisterType)>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: blocks.into_iter().map(|block| (block.id, block)).collect(),
            register_map: registers.into_iter().collect(),
        }
    }

    #[test]
    fn equal_expressions_remain_distinct_occurrences() {
        let eu = unit(
            [block(
                0,
                vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(7u8)),
                    SIRInstruction::Imm(RegisterId(1), SIRValue::new(7u8)),
                    SIRInstruction::Binary(
                        RegisterId(2),
                        RegisterId(0),
                        BinaryOp::Add,
                        RegisterId(1),
                    ),
                ],
                SIRTerminator::Return,
            )],
            [
                (RegisterId(0), bit(8)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
            ],
        );
        let analysis = PlacementAnalysis::analyze(&eu).unwrap();
        let first = analysis.value_for_register(RegisterId(0)).unwrap();
        let second = analysis.value_for_register(RegisterId(1)).unwrap();

        assert_ne!(first, second);
        assert_ne!(
            analysis.value(first).unwrap().origin,
            analysis.value(second).unwrap().origin
        );
        assert_eq!(
            analysis
                .value(analysis.value_for_register(RegisterId(2)).unwrap())
                .unwrap()
                .operands,
            vec![first, second]
        );
    }

    #[test]
    fn pure_value_analysis_uses_the_forward_only_cfg() {
        let eu = unit(
            [block(
                0,
                vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(7u8))],
                SIRTerminator::Return,
            )],
            [(RegisterId(0), bit(8))],
        );
        let analysis = PlacementAnalysis::analyze_pure_values(&eu).unwrap();
        assert!(analysis.cfg.postdominators.is_none());
        assert!(analysis.cfg.controllers.iter().all(Vec::is_empty));
        assert!(analysis.effects.is_empty());
        assert!(analysis.effect_phis.is_empty());
        assert!(matches!(
            analysis
                .value(analysis.value_for_register(RegisterId(0)).unwrap())
                .unwrap()
                .safety,
            ValueSafety::Pure
        ));
    }

    #[test]
    fn state_tokens_distinguish_loads_separated_by_a_store() {
        let state = address(0);
        let eu = unit(
            [block(
                0,
                vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(9u8)),
                    SIRInstruction::Load(RegisterId(1), state, SIROffset::Static(0), 8),
                    SIRInstruction::Store(
                        state,
                        SIROffset::Static(0),
                        8,
                        RegisterId(0),
                        vec![],
                        vec![],
                    ),
                    SIRInstruction::Load(RegisterId(2), state, SIROffset::Static(0), 8),
                ],
                SIRTerminator::Return,
            )],
            [
                (RegisterId(0), bit(8)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
            ],
        );
        let analysis = PlacementAnalysis::analyze(&eu).unwrap();
        let token = |register| match analysis
            .value(analysis.value_for_register(register).unwrap())
            .unwrap()
            .safety
        {
            ValueSafety::StateRead(token) => token,
            other => panic!("expected versioned state read, got {other:?}"),
        };

        assert_ne!(token(RegisterId(1)).version, token(RegisterId(2)).version);
    }

    #[test]
    fn read_only_state_can_sink_to_an_unchanged_edge() {
        let input = address(0);
        let eu = unit(
            [
                block(
                    0,
                    vec![
                        SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                        SIRInstruction::Load(RegisterId(1), input, SIROffset::Static(0), 8),
                    ],
                    SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                ),
                block(
                    1,
                    vec![SIRInstruction::Unary(
                        RegisterId(2),
                        UnaryOp::Ident,
                        RegisterId(1),
                    )],
                    SIRTerminator::Return,
                ),
                block(2, vec![], SIRTerminator::Return),
            ],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
            ],
        );
        let analysis = PlacementAnalysis::analyze(&eu).unwrap();
        let value = analysis.value_for_register(RegisterId(1)).unwrap();

        assert!(analysis.can_sink_to_edge(value, BlockId(0)));
        assert_eq!(
            analysis.sink_bounds(value).unwrap(),
            PlacementBounds {
                earliest: BlockId(0),
                latest: BlockId(1),
                legal_blocks: vec![BlockId(0), BlockId(1)],
            }
        );
    }

    #[test]
    fn exact_state_version_can_reload_without_original_load_dominance() {
        let input = address(0);
        let eu = unit(
            [
                block(
                    0,
                    vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8))],
                    SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                ),
                block(
                    1,
                    vec![SIRInstruction::Load(
                        RegisterId(1),
                        input,
                        SIROffset::Static(0),
                        8,
                    )],
                    SIRTerminator::Jump(BlockId(3), vec![]),
                ),
                block(2, vec![], SIRTerminator::Jump(BlockId(3), vec![])),
                block(3, vec![], SIRTerminator::Return),
            ],
            [(RegisterId(0), bit(1)), (RegisterId(1), bit(8))],
        );
        let analysis = PlacementAnalysis::analyze(&eu).unwrap();
        let value = analysis.value_for_register(RegisterId(1)).unwrap();

        assert!(!analysis.can_sink_to_block(value, BlockId(3)));
        assert!(analysis.can_materialize_state_read_at_block(value, BlockId(3)));
    }

    #[test]
    fn aggregate_ssa_frontier_is_first_block_after_all_definitions() {
        let first = address(0);
        let second = address(1);
        let eu = unit(
            [
                block(
                    0,
                    vec![SIRInstruction::Load(
                        RegisterId(0),
                        first,
                        SIROffset::Static(0),
                        8,
                    )],
                    SIRTerminator::Jump(BlockId(1), vec![]),
                ),
                block(
                    1,
                    vec![SIRInstruction::Load(
                        RegisterId(1),
                        second,
                        SIROffset::Static(0),
                        8,
                    )],
                    SIRTerminator::Jump(BlockId(2), vec![]),
                ),
                block(2, vec![], SIRTerminator::Return),
            ],
            [(RegisterId(0), bit(8)), (RegisterId(1), bit(8))],
        );
        let analysis = PlacementAnalysis::analyze(&eu).unwrap();
        let values = [
            analysis.value_for_register(RegisterId(0)).unwrap(),
            analysis.value_for_register(RegisterId(1)).unwrap(),
        ];

        assert_eq!(
            analysis.earliest_common_dominating_value_block(&values, BlockId(2)),
            Some(BlockId(1))
        );
    }

    #[test]
    fn aggregate_state_frontier_stops_before_a_branch_write() {
        let state = address(0);
        let eu = unit(
            [
                block(
                    0,
                    vec![
                        SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                        SIRInstruction::Imm(RegisterId(1), SIRValue::new(9u8)),
                        SIRInstruction::Load(RegisterId(2), state, SIROffset::Static(0), 8),
                    ],
                    SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                ),
                block(
                    1,
                    vec![SIRInstruction::Store(
                        state,
                        SIROffset::Static(0),
                        8,
                        RegisterId(1),
                        vec![],
                        vec![],
                    )],
                    SIRTerminator::Jump(BlockId(3), vec![]),
                ),
                block(2, vec![], SIRTerminator::Jump(BlockId(3), vec![])),
                block(3, vec![], SIRTerminator::Return),
            ],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
            ],
        );
        let analysis = PlacementAnalysis::analyze(&eu).unwrap();
        let value = analysis.value_for_register(RegisterId(2)).unwrap();

        assert!(!analysis.can_materialize_state_read_at_block(value, BlockId(3)));
        assert_eq!(
            analysis.latest_common_state_materialization_block(&[value], BlockId(3)),
            Some(BlockId(0))
        );
    }

    #[test]
    fn read_only_state_can_sink_within_the_same_loop_iteration() {
        let input = address(0);
        let eu = unit(
            [
                block(
                    0,
                    vec![
                        SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                        SIRInstruction::Imm(RegisterId(1), SIRValue::new(0u8)),
                    ],
                    SIRTerminator::Jump(BlockId(1), vec![]),
                ),
                block(
                    1,
                    vec![SIRInstruction::Load(
                        RegisterId(2),
                        input,
                        SIROffset::Static(0),
                        8,
                    )],
                    SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(2), vec![]),
                        false_block: (BlockId(3), vec![]),
                    },
                ),
                block(
                    2,
                    vec![SIRInstruction::Unary(
                        RegisterId(3),
                        UnaryOp::Ident,
                        RegisterId(2),
                    )],
                    SIRTerminator::Jump(BlockId(3), vec![]),
                ),
                block(
                    3,
                    vec![],
                    SIRTerminator::Branch {
                        cond: RegisterId(1),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(4), vec![]),
                    },
                ),
                block(4, vec![], SIRTerminator::Return),
            ],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(1)),
                (RegisterId(2), bit(8)),
                (RegisterId(3), bit(8)),
            ],
        );
        let analysis = PlacementAnalysis::analyze(&eu).unwrap();
        let value = analysis.value_for_register(RegisterId(2)).unwrap();

        assert_eq!(
            analysis.sink_bounds(value).unwrap().legal_blocks,
            vec![BlockId(1), BlockId(2)]
        );
    }

    #[test]
    fn schedule_late_does_not_move_an_outer_value_into_a_nested_loop() {
        let eu = unit(
            [
                block(
                    0,
                    vec![
                        SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                        SIRInstruction::Imm(RegisterId(1), SIRValue::new(0u8)),
                        SIRInstruction::Imm(RegisterId(2), SIRValue::new(7u8)),
                    ],
                    SIRTerminator::Jump(BlockId(1), vec![]),
                ),
                block(
                    1,
                    vec![SIRInstruction::Unary(
                        RegisterId(3),
                        UnaryOp::BitNot,
                        RegisterId(2),
                    )],
                    SIRTerminator::Jump(BlockId(2), vec![]),
                ),
                block(
                    2,
                    vec![],
                    SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(3), vec![]),
                        false_block: (BlockId(4), vec![]),
                    },
                ),
                block(
                    3,
                    vec![SIRInstruction::Unary(
                        RegisterId(4),
                        UnaryOp::Ident,
                        RegisterId(3),
                    )],
                    SIRTerminator::Jump(BlockId(2), vec![]),
                ),
                block(
                    4,
                    vec![],
                    SIRTerminator::Branch {
                        cond: RegisterId(1),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(5), vec![]),
                    },
                ),
                block(5, vec![], SIRTerminator::Return),
            ],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(1)),
                (RegisterId(2), bit(8)),
                (RegisterId(3), bit(8)),
                (RegisterId(4), bit(8)),
            ],
        );
        let analysis = PlacementAnalysis::analyze(&eu).unwrap();
        let value = analysis.value_for_register(RegisterId(3)).unwrap();

        assert_eq!(
            analysis.sink_bounds(value).unwrap().legal_blocks,
            vec![BlockId(1)]
        );
    }

    #[test]
    fn write_after_load_closes_the_state_execution_domain() {
        let state = address(0);
        let eu = unit(
            [
                block(
                    0,
                    vec![
                        SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                        SIRInstruction::Imm(RegisterId(1), SIRValue::new(3u8)),
                        SIRInstruction::Load(RegisterId(2), state, SIROffset::Static(0), 8),
                        SIRInstruction::Store(
                            state,
                            SIROffset::Static(0),
                            8,
                            RegisterId(1),
                            vec![],
                            vec![],
                        ),
                    ],
                    SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                ),
                block(
                    1,
                    vec![SIRInstruction::Unary(
                        RegisterId(3),
                        UnaryOp::Ident,
                        RegisterId(2),
                    )],
                    SIRTerminator::Return,
                ),
                block(2, vec![], SIRTerminator::Return),
            ],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
                (RegisterId(3), bit(8)),
            ],
        );
        let analysis = PlacementAnalysis::analyze(&eu).unwrap();
        let value = analysis.value_for_register(RegisterId(2)).unwrap();

        assert!(!analysis.can_sink_to_edge(value, BlockId(0)));
        assert_eq!(
            analysis.sink_bounds(value).unwrap().legal_blocks,
            vec![BlockId(0)]
        );
    }

    #[test]
    fn overlapping_partial_store_changes_the_load_token() {
        let state = address(0);
        let eu = unit(
            [
                block(
                    0,
                    vec![
                        SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                        SIRInstruction::Imm(RegisterId(1), SIRValue::new(0xaau8)),
                        SIRInstruction::Load(RegisterId(2), state, SIROffset::Static(0), 16),
                        SIRInstruction::Store(
                            state,
                            SIROffset::Static(8),
                            8,
                            RegisterId(1),
                            vec![],
                            vec![],
                        ),
                    ],
                    SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                ),
                block(
                    1,
                    vec![SIRInstruction::Unary(
                        RegisterId(3),
                        UnaryOp::Ident,
                        RegisterId(2),
                    )],
                    SIRTerminator::Return,
                ),
                block(2, vec![], SIRTerminator::Return),
            ],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(16)),
                (RegisterId(3), bit(16)),
            ],
        );
        let analysis = PlacementAnalysis::analyze(&eu).unwrap();

        assert!(!analysis.can_sink_to_edge(
            analysis.value_for_register(RegisterId(2)).unwrap(),
            BlockId(0)
        ));
    }

    #[test]
    fn schedule_late_keeps_a_value_shared_by_both_arms_in_the_head() {
        let eu = unit(
            [
                block(
                    0,
                    vec![
                        SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                        SIRInstruction::Imm(RegisterId(1), SIRValue::new(7u8)),
                    ],
                    SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                ),
                block(
                    1,
                    vec![SIRInstruction::Unary(
                        RegisterId(2),
                        UnaryOp::Ident,
                        RegisterId(1),
                    )],
                    SIRTerminator::Return,
                ),
                block(
                    2,
                    vec![SIRInstruction::Unary(
                        RegisterId(3),
                        UnaryOp::Ident,
                        RegisterId(1),
                    )],
                    SIRTerminator::Return,
                ),
            ],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
                (RegisterId(3), bit(8)),
            ],
        );
        let analysis = PlacementAnalysis::analyze(&eu).unwrap();
        let bounds = analysis
            .sink_bounds(analysis.value_for_register(RegisterId(1)).unwrap())
            .unwrap();

        assert_eq!(bounds.latest, BlockId(0));
        assert_eq!(bounds.legal_blocks, vec![BlockId(0)]);
    }

    #[test]
    fn merge_arguments_are_edge_uses_and_parameters_are_pinned_occurrences() {
        let mut merge = block(
            3,
            vec![SIRInstruction::Unary(
                RegisterId(3),
                UnaryOp::Ident,
                RegisterId(2),
            )],
            SIRTerminator::Return,
        );
        merge.params.push(RegisterId(2));
        let eu = unit(
            [
                block(
                    0,
                    vec![
                        SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                        SIRInstruction::Imm(RegisterId(1), SIRValue::new(7u8)),
                    ],
                    SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                ),
                block(
                    1,
                    vec![],
                    SIRTerminator::Jump(BlockId(3), vec![RegisterId(1)]),
                ),
                block(
                    2,
                    vec![],
                    SIRTerminator::Jump(BlockId(3), vec![RegisterId(1)]),
                ),
                merge,
            ],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
                (RegisterId(3), bit(8)),
            ],
        );
        let analysis = PlacementAnalysis::analyze(&eu).unwrap();
        let incoming = analysis
            .value(analysis.value_for_register(RegisterId(1)).unwrap())
            .unwrap();
        let mut edge_blocks = incoming
            .uses
            .iter()
            .filter_map(|site| match site {
                ValueUse::EdgeArgument { predecessor, .. } => Some(*predecessor),
                _ => None,
            })
            .collect::<Vec<_>>();
        edge_blocks.sort_unstable();

        assert_eq!(edge_blocks, vec![BlockId(1), BlockId(2)]);
        assert_eq!(
            analysis.sink_bounds(incoming.id).unwrap().latest,
            BlockId(0)
        );
        assert_eq!(
            analysis
                .value(analysis.value_for_register(RegisterId(2)).unwrap())
                .unwrap()
                .safety,
            ValueSafety::Pinned(PinReason::BlockParameter)
        );
    }

    #[test]
    fn observable_occurrences_are_chained_through_an_effect_phi() {
        let eu = unit(
            [
                block(
                    0,
                    vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8))],
                    SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                ),
                block(
                    1,
                    vec![SIRInstruction::RuntimeEvent {
                        site_id: 4,
                        args: vec![],
                    }],
                    SIRTerminator::Jump(BlockId(3), vec![]),
                ),
                block(2, vec![], SIRTerminator::Jump(BlockId(3), vec![])),
                block(
                    3,
                    vec![SIRInstruction::RuntimeEvent {
                        site_id: 5,
                        args: vec![],
                    }],
                    SIRTerminator::Return,
                ),
            ],
            [(RegisterId(0), bit(1))],
        );
        let analysis = PlacementAnalysis::analyze(&eu).unwrap();
        let branch_effect = analysis
            .effects
            .iter()
            .find(|effect| effect.location.block() == BlockId(1))
            .unwrap();
        let join_effect = analysis
            .effects
            .iter()
            .find(|effect| effect.location.block() == BlockId(3))
            .unwrap();

        assert_eq!(branch_effect.input, EffectToken::Entry);
        assert_eq!(branch_effect.control_domain, vec![BlockId(0)]);
        assert_eq!(join_effect.input, EffectToken::Phi(BlockId(3)));
        assert!(join_effect.control_domain.is_empty());
        assert_eq!(analysis.effect_phis.len(), 1);
        assert_eq!(analysis.effect_phis[0].block, BlockId(3));
        assert_eq!(analysis.effect_phis[0].incoming.len(), 2);
    }

    #[test]
    fn dynamic_load_uses_an_address_wide_state_version() {
        let eu = unit(
            [block(
                0,
                vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(0u8)),
                    SIRInstruction::Load(
                        RegisterId(1),
                        address(0),
                        SIROffset::Dynamic(RegisterId(0)),
                        8,
                    ),
                ],
                SIRTerminator::Return,
            )],
            [(RegisterId(0), bit(8)), (RegisterId(1), bit(8))],
        );
        let analysis = PlacementAnalysis::analyze(&eu).unwrap();
        let value = analysis
            .value(analysis.value_for_register(RegisterId(1)).unwrap())
            .unwrap();

        let ValueSafety::StateRead(token) = value.safety else {
            panic!("dynamic load must receive a conservative state version");
        };
        assert!(token.fragment.dynamic);
    }

    #[test]
    fn dynamic_load_does_not_sink_across_an_aliasing_store() {
        let state = address(0);
        let eu = unit(
            [
                block(
                    0,
                    vec![
                        SIRInstruction::Imm(RegisterId(0), SIRValue::new(0u8)),
                        SIRInstruction::Imm(RegisterId(1), SIRValue::new(1u8)),
                        SIRInstruction::Imm(RegisterId(2), SIRValue::new(9u8)),
                        SIRInstruction::Load(
                            RegisterId(3),
                            state,
                            SIROffset::Dynamic(RegisterId(0)),
                            8,
                        ),
                        SIRInstruction::Store(
                            state,
                            SIROffset::Static(0),
                            8,
                            RegisterId(2),
                            vec![],
                            vec![],
                        ),
                    ],
                    SIRTerminator::Branch {
                        cond: RegisterId(1),
                        true_block: (BlockId(1), vec![]),
                        false_block: (BlockId(2), vec![]),
                    },
                ),
                block(
                    1,
                    vec![SIRInstruction::Unary(
                        RegisterId(4),
                        UnaryOp::Ident,
                        RegisterId(3),
                    )],
                    SIRTerminator::Return,
                ),
                block(2, vec![], SIRTerminator::Return),
            ],
            [
                (RegisterId(0), bit(8)),
                (RegisterId(1), bit(1)),
                (RegisterId(2), bit(8)),
                (RegisterId(3), bit(8)),
                (RegisterId(4), bit(8)),
            ],
        );
        let analysis = PlacementAnalysis::analyze(&eu).unwrap();
        let value = analysis.value_for_register(RegisterId(3)).unwrap();

        assert_eq!(
            analysis.sink_bounds(value).unwrap().legal_blocks,
            vec![BlockId(0)]
        );
    }
}
