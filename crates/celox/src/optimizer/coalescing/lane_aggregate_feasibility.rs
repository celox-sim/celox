//! Analysis-only feasibility gate for lane-aggregate recipes.
//!
//! A recipe starts at a complete packed predicate publication and walks the
//! synchronous product of its scalar lanes.  It never rewrites SIR.  State
//! leaves are accepted only when placement analysis proves that the exact
//! read version can be materialized in the sink block.

use std::fmt;

use super::placement_analysis::{PlacementAnalysis, ValueOrigin, ValueSafety, ValueUse};
use super::shared::sir_value_to_u64;
use crate::backend::MemoryLayout;
use crate::ir::{
    BinaryOp, BlockId, ExecutionUnit, RegionedAbsoluteAddr, RegisterId, SIRInstruction, SIROffset,
    UnaryOp,
};
use crate::{HashMap, HashSet};

const MAX_RECIPE_NODES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RecipeKind {
    StateRead,
    Constant,
    BroadcastScalar,
    Affine,
    PackedExtract,
    SsaPack,
    Unary,
    Binary,
    ShiftConstant,
    OneHotDecode,
    Mux,
    ControlMux,
    ScalarInsert,
    Slice,
    Concat,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum RecipeOp {
    StateRead,
    Constant,
    BroadcastScalar,
    Affine,
    PackedExtract,
    SsaPack,
    Unary(UnaryOp),
    Binary(BinaryOp),
    ShiftConstant { operation: BinaryOp, amount: usize },
    OneHotDecode { shift_width: usize },
    Mux,
    ControlMux,
    ScalarInsert,
    Slice { offset: usize, width: usize },
    Concat { operand_widths: Vec<usize> },
}

impl RecipeOp {
    fn kind(&self) -> RecipeKind {
        match self {
            Self::StateRead => RecipeKind::StateRead,
            Self::Constant => RecipeKind::Constant,
            Self::BroadcastScalar => RecipeKind::BroadcastScalar,
            Self::Affine => RecipeKind::Affine,
            Self::PackedExtract => RecipeKind::PackedExtract,
            Self::SsaPack => RecipeKind::SsaPack,
            Self::Unary(_) => RecipeKind::Unary,
            Self::Binary(_) => RecipeKind::Binary,
            Self::ShiftConstant { .. } => RecipeKind::ShiftConstant,
            Self::OneHotDecode { .. } => RecipeKind::OneHotDecode,
            Self::Mux => RecipeKind::Mux,
            Self::ControlMux => RecipeKind::ControlMux,
            Self::ScalarInsert => RecipeKind::ScalarInsert,
            Self::Slice { .. } => RecipeKind::Slice,
            Self::Concat { .. } => RecipeKind::Concat,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RejectReason {
    MissingDefinition,
    PinnedLeaf,
    NonStridedStateLeaf,
    UnstableStateVersion,
    UnsupportedOperation,
    HeterogeneousOperation,
    LaneWidth,
    InvalidRecipe,
    NodeBudget,
    Cycle,
}

impl fmt::Display for RejectReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MissingDefinition => "missing-definition",
            Self::PinnedLeaf => "pinned-leaf",
            Self::NonStridedStateLeaf => "non-strided-state-leaf",
            Self::UnstableStateVersion => "unstable-state-version",
            Self::UnsupportedOperation => "unsupported-operation",
            Self::HeterogeneousOperation => "heterogeneous-operation",
            Self::LaneWidth => "unsupported-lane-width",
            Self::InvalidRecipe => "invalid-recipe",
            Self::NodeBudget => "node-budget",
            Self::Cycle => "cycle",
        })
    }
}

#[derive(Debug, Clone)]
struct RecipeNode {
    operation: RecipeOp,
    lanes: Vec<RegisterId>,
    children: Vec<usize>,
    lane_width: usize,
    estimated_per_chunk: usize,
}

#[derive(Debug, Clone)]
struct Candidate {
    block: BlockId,
    root: RegisterId,
    recipe_root: usize,
    lane_count: usize,
    nodes: Vec<RecipeNode>,
    covered_registers: HashSet<RegisterId>,
    covered_consumers: HashSet<RegisterId>,
    snapshot_frontiers: Vec<BlockId>,
    ssa_frontiers: Vec<BlockId>,
    estimated_instructions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SharedRecipeKey {
    operation: RecipeOp,
    lanes: Vec<RegisterId>,
    children: Vec<usize>,
    lane_width: usize,
}

#[derive(Debug, Clone)]
struct SharedRecipePlan {
    nodes: Vec<RecipeNode>,
    roots: Vec<(BlockId, RegisterId, usize)>,
}

#[derive(Debug, Default, Clone, Copy)]
struct LocalPressure {
    gpr: usize,
    xmm: usize,
}

#[derive(Debug, Clone)]
struct RejectedCandidate {
    block: BlockId,
    root: RegisterId,
    lane_count: usize,
    reason: RejectReason,
    sample_register: Option<RegisterId>,
    sample_widths: Vec<usize>,
    sample_instruction: &'static str,
    sample_shapes: Vec<(&'static str, usize)>,
    sample_examples: Vec<String>,
    sample_operand_examples: Vec<String>,
    missing_registers: Vec<RegisterId>,
    failure_path: Vec<(Vec<usize>, Vec<(&'static str, usize)>, String)>,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct LaneAggregateFeasibilityReport {
    candidates: usize,
    accepted: Vec<Candidate>,
    rejected: Vec<RejectedCandidate>,
    covered_scalar_definitions: usize,
    dead_scalar_definitions: usize,
    summed_estimated_instructions: usize,
    unique_estimated_instructions: usize,
    shared_recipe_nodes: usize,
    shared_prefix_nodes: usize,
    shared_boundary_values: usize,
    peak_prefix_gpr_values: usize,
    peak_prefix_xmm_values: usize,
    peak_suffix_gpr_values: usize,
    peak_suffix_xmm_values: usize,
    kind_counts: HashMap<RecipeKind, usize>,
    reject_counts: HashMap<RejectReason, usize>,
}

impl LaneAggregateFeasibilityReport {
    pub(crate) fn detail_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for candidate in &self.accepted {
            lines.push(format!(
                "status=accepted block={} root=r{} lanes={} nodes={} covered={} estimated={}",
                candidate.block.0,
                candidate.root.0,
                candidate.lane_count,
                candidate.nodes.len(),
                candidate.covered_registers.len(),
                candidate.estimated_instructions,
            ));
            if !candidate.snapshot_frontiers.is_empty() {
                lines.push(format!(
                    "status=accepted-frontier block={} root=r{} snapshot_blocks={:?}",
                    candidate.block.0,
                    candidate.root.0,
                    candidate
                        .snapshot_frontiers
                        .iter()
                        .map(|block| block.0)
                        .collect::<Vec<_>>(),
                ));
            }
            if !candidate.ssa_frontiers.is_empty() {
                lines.push(format!(
                    "status=accepted-ssa-frontier block={} root=r{} aggregate_blocks={:?}",
                    candidate.block.0,
                    candidate.root.0,
                    candidate
                        .ssa_frontiers
                        .iter()
                        .map(|block| block.0)
                        .collect::<Vec<_>>(),
                ));
            }
        }
        for candidate in &self.rejected {
            lines.push(format!(
                "status=rejected block={} root=r{} lanes={} reason={}",
                candidate.block.0, candidate.root.0, candidate.lane_count, candidate.reason,
            ));
            if let Some(register) = candidate.sample_register {
                lines.push(format!(
                    "status=rejected-context block={} root=r{} sample=r{} widths={:?} instruction={}",
                    candidate.block.0,
                    candidate.root.0,
                    register.0,
                    candidate.sample_widths,
                    candidate.sample_instruction,
                ));
                lines.push(format!(
                    "status=rejected-shapes block={} root=r{} shapes={:?}",
                    candidate.block.0, candidate.root.0, candidate.sample_shapes,
                ));
                for example in &candidate.sample_examples {
                    lines.push(format!(
                        "status=rejected-example block={} root=r{} {example}",
                        candidate.block.0, candidate.root.0,
                    ));
                }
                for example in &candidate.sample_operand_examples {
                    lines.push(format!(
                        "status=rejected-operand block={} root=r{} {example}",
                        candidate.block.0, candidate.root.0,
                    ));
                }
                if !candidate.missing_registers.is_empty() {
                    lines.push(format!(
                        "status=rejected-missing block={} root=r{} registers={:?}",
                        candidate.block.0,
                        candidate.root.0,
                        candidate
                            .missing_registers
                            .iter()
                            .map(|register| register.0)
                            .collect::<Vec<_>>(),
                    ));
                }
                for (depth, (widths, shapes, example)) in candidate.failure_path.iter().enumerate()
                {
                    lines.push(format!(
                        "status=rejected-path block={} root=r{} depth={} widths={widths:?} shapes={shapes:?} example={example}",
                        candidate.block.0, candidate.root.0, depth
                    ));
                }
            }
        }
        let mut kinds = self.kind_counts.iter().collect::<Vec<_>>();
        kinds.sort_unstable_by_key(|(kind, _)| format!("{kind:?}"));
        for (kind, count) in kinds {
            lines.push(format!("kind={kind:?} count={count}"));
        }
        let mut reasons = self.reject_counts.iter().collect::<Vec<_>>();
        reasons.sort_unstable_by_key(|(reason, _)| format!("{reason:?}"));
        for (reason, count) in reasons {
            lines.push(format!("reject={reason} count={count}"));
        }
        lines
    }
}

impl fmt::Display for LaneAggregateFeasibilityReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "candidates={} accepted={} rejected={} covered_scalar_defs={} dead_scalar_defs={} estimated_sum={} estimated_unique={} shared_recipe_nodes={} shared_prefix_nodes={} shared_boundary_values={} peak_prefix_gpr={} peak_prefix_xmm={} peak_suffix_gpr={} peak_suffix_xmm={}",
            self.candidates,
            self.accepted.len(),
            self.rejected.len(),
            self.covered_scalar_definitions,
            self.dead_scalar_definitions,
            self.summed_estimated_instructions,
            self.unique_estimated_instructions,
            self.shared_recipe_nodes,
            self.shared_prefix_nodes,
            self.shared_boundary_values,
            self.peak_prefix_gpr_values,
            self.peak_prefix_xmm_values,
            self.peak_suffix_gpr_values,
            self.peak_suffix_xmm_values,
        )
    }
}

struct Analyzer<'a> {
    eu: &'a ExecutionUnit<RegionedAbsoluteAddr>,
    layout: &'a MemoryLayout,
    placement: &'a PlacementAnalysis,
    definitions: &'a HashMap<RegisterId, (BlockId, usize)>,
    memo: HashMap<Vec<RegisterId>, usize>,
    active: HashSet<Vec<RegisterId>>,
    nodes: Vec<RecipeNode>,
    target: BlockId,
    failure_key: Option<Vec<RegisterId>>,
    failure_path: Vec<(Vec<usize>, Vec<(&'static str, usize)>, String)>,
    snapshot_frontiers: HashSet<BlockId>,
    ssa_frontiers: HashSet<BlockId>,
}

#[derive(Debug, Clone, Copy)]
struct NormalizedMux {
    condition: RegisterId,
    then_value: RegisterId,
    else_value: RegisterId,
    from_control_merge: bool,
}

impl<'a> Analyzer<'a> {
    fn instruction(&self, register: RegisterId) -> Option<&SIRInstruction<RegionedAbsoluteAddr>> {
        let &(block, index) = self.definitions.get(&register)?;
        self.eu.blocks.get(&block)?.instructions.get(index)
    }

    fn value_width(&self, register: RegisterId) -> Option<usize> {
        self.eu.register_map.get(&register).map(|ty| ty.width())
    }

    fn insert_node(
        &mut self,
        key: Vec<RegisterId>,
        operation: RecipeOp,
        children: Vec<usize>,
        lane_width: usize,
        estimated_per_chunk: usize,
    ) -> Result<usize, RejectReason> {
        if self.nodes.len() >= MAX_RECIPE_NODES {
            return Err(RejectReason::NodeBudget);
        }
        let id = self.nodes.len();
        self.nodes.push(RecipeNode {
            operation,
            lanes: key.clone(),
            children,
            lane_width,
            estimated_per_chunk,
        });
        self.memo.insert(key, id);
        Ok(id)
    }

    fn analyze(&mut self, key: Vec<RegisterId>) -> Result<usize, RejectReason> {
        if let Some(&known) = self.memo.get(&key) {
            return Ok(known);
        }
        if key.is_empty() || !self.active.insert(key.clone()) {
            return Err(RejectReason::Cycle);
        }
        let mut result = self.analyze_uncached(key.clone());
        if result.is_err()
            && let Some(frontier) = self.ssa_pack_frontier(&key)
        {
            self.ssa_frontiers.insert(frontier);
            result = self.insert_node(
                key.clone(),
                RecipeOp::SsaPack,
                Vec::new(),
                1,
                key.len().saturating_mul(2).saturating_sub(1),
            );
        }
        if result.is_err() {
            if self.failure_key.is_none() {
                self.failure_key = Some(key.clone());
            }
            if self.failure_path.len() < 16 {
                let mut widths = key
                    .iter()
                    .filter_map(|register| self.value_width(*register))
                    .collect::<Vec<_>>();
                widths.sort_unstable();
                widths.dedup();
                let mut shapes = HashMap::<&'static str, usize>::default();
                for &register in &key {
                    *shapes
                        .entry(instruction_name(self.instruction(register)))
                        .or_default() += 1;
                }
                let mut shapes = shapes.into_iter().collect::<Vec<_>>();
                shapes.sort_unstable();
                let example = key
                    .first()
                    .map(|register| format!("r{}={:?}", register.0, self.instruction(*register)))
                    .unwrap_or_else(|| "<empty>".into());
                self.failure_path.push((widths, shapes, example));
            }
        }
        self.active.remove(&key);
        result
    }

    fn ssa_pack_frontier(&self, key: &[RegisterId]) -> Option<BlockId> {
        if key.len() < 8
            || key
                .iter()
                .any(|register| self.value_width(*register) != Some(1))
        {
            return None;
        }
        let values = key
            .iter()
            .map(|register| self.placement.value_for_register(*register))
            .collect::<Option<Vec<_>>>()?;
        let frontier = self
            .placement
            .earliest_common_dominating_value_block(&values, self.target)?;
        (frontier != self.target).then_some(frontier)
    }

    fn analyze_uncached(&mut self, key: Vec<RegisterId>) -> Result<usize, RejectReason> {
        let first = *key.first().ok_or(RejectReason::MissingDefinition)?;
        let lane_width = self
            .value_width(first)
            .ok_or(RejectReason::MissingDefinition)?;
        if lane_width == 0
            || lane_width > 64
            || key
                .iter()
                .any(|register| self.value_width(*register) != Some(lane_width))
        {
            return Err(RejectReason::LaneWidth);
        }

        if key
            .iter()
            .all(|register| matches!(self.instruction(*register), Some(SIRInstruction::Imm(..))))
        {
            return self.insert_node(key, RecipeOp::Constant, Vec::new(), lane_width, 1);
        }

        if key.iter().all(|register| *register == first) {
            let value = self
                .placement
                .value_for_register(first)
                .and_then(|value| self.placement.value(value))
                .ok_or(RejectReason::MissingDefinition)?;
            return match value.safety {
                ValueSafety::Pure if self.placement.can_sink_to_block(value.id, self.target) => {
                    self.insert_node(key, RecipeOp::BroadcastScalar, Vec::new(), lane_width, 4)
                }
                ValueSafety::Pure | ValueSafety::Pinned(_)
                    if self
                        .placement
                        .cfg
                        .dominates(value.origin.block(), self.target) =>
                {
                    // This is a concrete DominatingSSA frontier, not a
                    // relocation proof.  Only one scalar crosses the
                    // boundary regardless of the lane count.
                    self.insert_node(key, RecipeOp::BroadcastScalar, Vec::new(), lane_width, 4)
                }
                ValueSafety::StateRead(_)
                    if self
                        .placement
                        .can_materialize_state_read_at_block(value.id, self.target) =>
                {
                    self.insert_node(key, RecipeOp::StateRead, Vec::new(), lane_width, 5)
                }
                ValueSafety::StateRead(_) => {
                    if let Some(frontier) = self
                        .placement
                        .latest_common_state_materialization_block(&[value.id], self.target)
                    {
                        self.snapshot_frontiers.insert(frontier);
                    } else {
                        let frontier = self
                            .placement
                            .earliest_common_dominating_value_block(&[value.id], self.target)
                            .ok_or(RejectReason::UnstableStateVersion)?;
                        self.ssa_frontiers.insert(frontier);
                    }
                    self.insert_node(key, RecipeOp::StateRead, Vec::new(), lane_width, 5)
                }
                ValueSafety::Pure => Err(RejectReason::UnstableStateVersion),
                ValueSafety::Pinned(_) => Err(RejectReason::PinnedLeaf),
            };
        }

        if key
            .iter()
            .all(|register| matches!(self.instruction(*register), Some(SIRInstruction::Load(..))))
        {
            self.verify_state_leaf(&key)?;
            return self.insert_node(key, RecipeOp::StateRead, Vec::new(), lane_width, 1);
        }

        if let Some(source) = self.regular_shift_source(&key, lane_width) {
            let child = self.analyze(vec![source; key.len()])?;
            return self.insert_node(key, RecipeOp::PackedExtract, vec![child], lane_width, 1);
        }

        if let Some(base) = self.affine_base(&key, lane_width) {
            let child = self.analyze(vec![base; key.len()])?;
            return self.insert_node(key, RecipeOp::Affine, vec![child], lane_width, 2);
        }

        let muxes = key
            .iter()
            .map(|register| self.normalized_mux(*register))
            .collect::<Vec<_>>();
        if muxes.iter().all(Option::is_some) {
            let muxes = muxes.into_iter().flatten().collect::<Vec<_>>();
            let from_control_merge = muxes.iter().any(|mux| mux.from_control_merge);
            let conditions = muxes.iter().map(|mux| mux.condition).collect();
            let then_values = muxes.iter().map(|mux| mux.then_value).collect();
            let else_values = muxes.iter().map(|mux| mux.else_value).collect();
            let condition = self.analyze(conditions)?;
            let then_value = self.analyze(then_values)?;
            let else_value = self.analyze(else_values)?;
            return self.insert_node(
                key,
                if from_control_merge {
                    RecipeOp::ControlMux
                } else {
                    RecipeOp::Mux
                },
                vec![condition, then_value, else_value],
                lane_width,
                3,
            );
        }
        if let Some(result) = self.analyze_mux_with_scalar_inserts(&key, &muxes, lane_width) {
            return result;
        }

        let first_instruction = self
            .instruction(first)
            .cloned()
            .ok_or(RejectReason::MissingDefinition)?;
        match first_instruction {
            SIRInstruction::Unary(_, operation, operand) => {
                if !matches!(
                    operation,
                    UnaryOp::Ident | UnaryOp::BitNot | UnaryOp::LogicNot
                ) {
                    return Err(RejectReason::UnsupportedOperation);
                }
                let mut operands = Vec::with_capacity(key.len());
                operands.push(operand);
                for register in key.iter().skip(1) {
                    let Some(SIRInstruction::Unary(_, current, operand)) =
                        self.instruction(*register)
                    else {
                        return Err(RejectReason::HeterogeneousOperation);
                    };
                    if *current != operation {
                        return Err(RejectReason::HeterogeneousOperation);
                    }
                    operands.push(*operand);
                }
                let child = self.analyze(operands)?;
                self.insert_node(key, RecipeOp::Unary(operation), vec![child], lane_width, 1)
            }
            SIRInstruction::Binary(_, lhs, operation, rhs) => {
                let mut lhs_lanes = Vec::with_capacity(key.len());
                let mut rhs_lanes = Vec::with_capacity(key.len());
                lhs_lanes.push(lhs);
                rhs_lanes.push(rhs);
                for register in key.iter().skip(1) {
                    let Some(SIRInstruction::Binary(_, current_lhs, current, current_rhs)) =
                        self.instruction(*register)
                    else {
                        return Err(RejectReason::HeterogeneousOperation);
                    };
                    if *current != operation {
                        return Err(RejectReason::HeterogeneousOperation);
                    }
                    lhs_lanes.push(*current_lhs);
                    rhs_lanes.push(*current_rhs);
                }
                if matches!(operation, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar) {
                    if let Some(shift) = exact_uniform_immediate(self, &rhs_lanes) {
                        if shift >= lane_width {
                            return Err(RejectReason::UnsupportedOperation);
                        }
                        let child = self.analyze(lhs_lanes)?;
                        return self.insert_node(
                            key,
                            RecipeOp::ShiftConstant {
                                operation,
                                amount: shift,
                            },
                            vec![child],
                            lane_width,
                            1,
                        );
                    }
                    if operation == BinaryOp::Shl
                        && lhs_lanes
                            .iter()
                            .all(|register| self.immediate(*register) == Some(1))
                    {
                        let shift_width = rhs_lanes
                            .first()
                            .and_then(|register| self.value_width(*register))
                            .ok_or(RejectReason::MissingDefinition)?;
                        if shift_width < usize::BITS as usize
                            && rhs_lanes
                                .iter()
                                .all(|register| self.value_width(*register) == Some(shift_width))
                            && (1usize << shift_width).saturating_sub(1) < lane_width
                        {
                            let child = self.analyze(rhs_lanes)?;
                            let lanes_per_chunk = (128 / lane_width).max(1);
                            return self.insert_node(
                                key,
                                RecipeOp::OneHotDecode { shift_width },
                                vec![child],
                                lane_width,
                                lanes_per_chunk,
                            );
                        }
                    }
                    return Err(RejectReason::UnsupportedOperation);
                }
                if !supported_binary(operation) {
                    return Err(RejectReason::UnsupportedOperation);
                }
                let lhs = self.analyze(lhs_lanes)?;
                let rhs = self.analyze(rhs_lanes)?;
                let wrap_mask = matches!(operation, BinaryOp::Add | BinaryOp::Sub)
                    && !lane_width.is_power_of_two();
                self.insert_node(
                    key,
                    RecipeOp::Binary(operation),
                    vec![lhs, rhs],
                    lane_width,
                    binary_cost_per_chunk(operation, lane_width) + usize::from(wrap_mask),
                )
            }
            SIRInstruction::Mux(..) => Err(RejectReason::HeterogeneousOperation),
            SIRInstruction::Slice(_, source, offset, width) => {
                let mut sources = Vec::with_capacity(key.len());
                sources.push(source);
                for register in key.iter().skip(1) {
                    let Some(SIRInstruction::Slice(_, source, current_offset, current_width)) =
                        self.instruction(*register)
                    else {
                        return Err(RejectReason::HeterogeneousOperation);
                    };
                    if *current_offset != offset || *current_width != width {
                        return Err(RejectReason::HeterogeneousOperation);
                    }
                    sources.push(*source);
                }
                let child = self.analyze(sources)?;
                self.insert_node(
                    key,
                    RecipeOp::Slice { offset, width },
                    vec![child],
                    lane_width,
                    2,
                )
            }
            SIRInstruction::Concat(_, arguments) => {
                let operand_count = arguments.len();
                if operand_count == 0 {
                    return Err(RejectReason::UnsupportedOperation);
                }
                let expected_widths = arguments
                    .iter()
                    .map(|register| self.value_width(*register))
                    .collect::<Option<Vec<_>>>()
                    .ok_or(RejectReason::MissingDefinition)?;
                let mut operand_lanes = arguments
                    .iter()
                    .map(|register| vec![*register])
                    .collect::<Vec<_>>();
                for register in key.iter().skip(1) {
                    let Some(SIRInstruction::Concat(_, current_arguments)) =
                        self.instruction(*register)
                    else {
                        return Err(RejectReason::HeterogeneousOperation);
                    };
                    if current_arguments.len() != operand_count
                        || current_arguments.iter().zip(&expected_widths).any(
                            |(operand, expected)| self.value_width(*operand) != Some(*expected),
                        )
                    {
                        return Err(RejectReason::HeterogeneousOperation);
                    }
                    for (lanes, operand) in operand_lanes.iter_mut().zip(current_arguments) {
                        lanes.push(*operand);
                    }
                }
                let mut children = Vec::with_capacity(operand_count);
                for lanes in operand_lanes {
                    children.push(self.analyze(lanes)?);
                }
                self.insert_node(
                    key,
                    RecipeOp::Concat {
                        operand_widths: expected_widths,
                    },
                    children,
                    lane_width,
                    1,
                )
            }
            _ => Err(RejectReason::UnsupportedOperation),
        }
    }

    fn regular_shift_source(&self, key: &[RegisterId], width: usize) -> Option<RegisterId> {
        let mut source = None;
        let mut offsets = Vec::with_capacity(key.len());
        for &register in key {
            let (current_source, offset) = match self.instruction(register) {
                Some(SIRInstruction::Binary(_, lhs, BinaryOp::Shr, rhs)) => {
                    (*lhs, usize::try_from(self.immediate(*rhs)?).ok()?)
                }
                _ => (register, 0),
            };
            if offset >= width || source.is_some_and(|known| known != current_source) {
                return None;
            }
            source.get_or_insert(current_source);
            offsets.push(offset);
        }
        if offsets.len() < 2 {
            return None;
        }
        let stride = offsets[1] as isize - offsets[0] as isize;
        if stride == 0
            || offsets
                .windows(2)
                .any(|pair| pair[1] as isize - pair[0] as isize != stride)
        {
            return None;
        }
        source
    }

    fn affine_base(&self, key: &[RegisterId], width: usize) -> Option<RegisterId> {
        let mut base = None;
        let mut saw_offset = false;
        for &register in key {
            let (current_base, offset) = self.resolve_affine(register, width)?;
            if base.is_some_and(|known| known != current_base) {
                return None;
            }
            base.get_or_insert(current_base);
            saw_offset |= offset != 0;
        }
        saw_offset.then_some(base?)
    }

    fn resolve_affine(&self, register: RegisterId, width: usize) -> Option<(RegisterId, u64)> {
        let mask = if width == 64 {
            u64::MAX
        } else {
            (1u64 << width) - 1
        };
        let mut current = register;
        let mut offset = 0u64;
        for _ in 0..MAX_RECIPE_NODES {
            let Some(SIRInstruction::Binary(_, lhs, operation, rhs)) = self.instruction(current)
            else {
                return Some((current, offset & mask));
            };
            let lhs_immediate = self.immediate(*lhs);
            let rhs_immediate = self.immediate(*rhs);
            match (*operation, lhs_immediate, rhs_immediate) {
                (BinaryOp::Add, _, Some(immediate)) => {
                    offset = offset.wrapping_add(immediate) & mask;
                    current = *lhs;
                }
                (BinaryOp::Add, Some(immediate), _) => {
                    offset = offset.wrapping_add(immediate) & mask;
                    current = *rhs;
                }
                (BinaryOp::Sub, _, Some(immediate)) => {
                    offset = offset.wrapping_sub(immediate) & mask;
                    current = *lhs;
                }
                _ => return Some((current, offset & mask)),
            }
        }
        None
    }

    fn immediate(&self, register: RegisterId) -> Option<u64> {
        let SIRInstruction::Imm(_, value) = self.instruction(register)? else {
            return None;
        };
        sir_value_to_u64(value)
    }

    fn normalized_mux(&self, register: RegisterId) -> Option<NormalizedMux> {
        if let Some(SIRInstruction::Mux(_, condition, then_value, else_value)) =
            self.instruction(register)
        {
            return Some(NormalizedMux {
                condition: *condition,
                then_value: *then_value,
                else_value: *else_value,
                from_control_merge: false,
            });
        }

        let value = self
            .placement
            .value_for_register(register)
            .and_then(|value| self.placement.value(value))?;
        let ValueOrigin::Parameter {
            block: merge,
            index: parameter,
        } = value.origin
        else {
            return None;
        };
        let merge_index = *self.placement.cfg.index.get(&merge)?;
        let predecessors = self.placement.cfg.predecessors.get(merge_index)?;
        if predecessors.len() != 2 {
            return None;
        }

        let mut edges = Vec::with_capacity(2);
        for &predecessor in predecessors {
            let edge = self.placement.cfg.block_ids[predecessor];
            let block = self.eu.blocks.get(&edge)?;
            if !block.instructions.is_empty() {
                return None;
            }
            let crate::ir::SIRTerminator::Jump(target, arguments) = &block.terminator else {
                return None;
            };
            if *target != merge {
                return None;
            }
            let argument = *arguments.get(parameter)?;
            let edge_predecessors = self.placement.cfg.predecessors.get(predecessor)?;
            if edge_predecessors.len() != 1 {
                return None;
            }
            edges.push((edge, edge_predecessors[0], argument));
        }
        if edges[0].1 != edges[1].1 {
            return None;
        }
        let head = self.placement.cfg.block_ids[edges[0].1];
        let crate::ir::SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } = &self.eu.blocks.get(&head)?.terminator
        else {
            return None;
        };
        let edge_argument = |target: BlockId| {
            edges
                .iter()
                .find_map(|(edge, _, argument)| (*edge == target).then_some(*argument))
        };
        let mut condition = *cond;
        if let Some(SIRInstruction::Unary(_, UnaryOp::ToTwoState, source)) =
            self.instruction(condition)
        {
            condition = *source;
        }
        Some(NormalizedMux {
            condition,
            then_value: edge_argument(true_block.0)?,
            else_value: edge_argument(false_block.0)?,
            from_control_merge: true,
        })
    }

    fn analyze_mux_with_scalar_inserts(
        &mut self,
        key: &[RegisterId],
        muxes: &[Option<NormalizedMux>],
        lane_width: usize,
    ) -> Option<Result<usize, RejectReason>> {
        let mux_lanes = key
            .iter()
            .zip(muxes)
            .filter_map(|(&register, mux)| mux.is_some().then_some(register))
            .collect::<Vec<_>>();
        let scalar_lanes = key
            .iter()
            .zip(muxes)
            .filter_map(|(&register, mux)| mux.is_none().then_some(register))
            .collect::<Vec<_>>();
        if mux_lanes.len() < key.len().saturating_sub(2)
            || scalar_lanes.is_empty()
            || scalar_lanes.len() > 2
        {
            return None;
        }
        let scalar_values = scalar_lanes
            .iter()
            .map(|register| {
                let value = self.placement.value_for_register(*register)?;
                let occurrence = self.placement.value(value)?;
                if !matches!(
                    occurrence.safety,
                    ValueSafety::Pure | ValueSafety::Pinned(_)
                ) || !self
                    .placement
                    .cfg
                    .dominates(occurrence.origin.block(), self.target)
                {
                    return None;
                }
                Some(value)
            })
            .collect::<Option<Vec<_>>>()?;
        let all_values = key
            .iter()
            .map(|register| self.placement.value_for_register(*register))
            .collect::<Option<Vec<_>>>()?;
        let frontier = self
            .placement
            .earliest_common_dominating_value_block(&all_values, self.target)?;
        if frontier == self.target {
            return None;
        }
        self.ssa_frontiers.insert(frontier);

        Some((|| {
            let aggregate = self.analyze(mux_lanes)?;
            let inserts = self.insert_node(
                scalar_lanes.clone(),
                RecipeOp::ScalarInsert,
                Vec::new(),
                lane_width,
                scalar_values.len().saturating_mul(2),
            )?;
            self.insert_node(
                key.to_vec(),
                RecipeOp::ControlMux,
                vec![aggregate, inserts],
                lane_width,
                1,
            )
        })())
    }

    fn verify_state_leaf(&mut self, key: &[RegisterId]) -> Result<(), RejectReason> {
        let mut address = None;
        let mut width = None;
        let mut physical_offsets = Vec::with_capacity(key.len());
        let mut values = Vec::with_capacity(key.len());
        let mut all_versioned_state_reads = true;
        for &register in key {
            let value_id = self
                .placement
                .value_for_register(register)
                .ok_or(RejectReason::MissingDefinition)?;
            let value = self
                .placement
                .value(value_id)
                .ok_or(RejectReason::MissingDefinition)?;
            if !matches!(value.safety, ValueSafety::StateRead(_)) {
                if !matches!(value.safety, ValueSafety::Pure | ValueSafety::Pinned(_))
                    || !self
                        .placement
                        .cfg
                        .dominates(value.origin.block(), self.target)
                {
                    return Err(RejectReason::UnstableStateVersion);
                }
                all_versioned_state_reads = false;
            }
            values.push(value_id);
            let Some(SIRInstruction::Load(
                _,
                current_address,
                SIROffset::Static(offset),
                current_width,
            )) = self.instruction(register)
            else {
                return Err(RejectReason::NonStridedStateLeaf);
            };
            if address.is_some_and(|known| known != *current_address)
                || width.is_some_and(|known| known != *current_width)
            {
                return Err(RejectReason::NonStridedStateLeaf);
            }
            address.get_or_insert(*current_address);
            width.get_or_insert(*current_width);
            let (byte, bit) = self
                .layout
                .map_static_bit_offset(&current_address.absolute_addr(), *offset);
            physical_offsets.push((byte, bit));
        }
        if !all_versioned_state_reads {
            let frontier = self
                .placement
                .earliest_common_dominating_value_block(&values, self.target)
                .ok_or(RejectReason::UnstableStateVersion)?;
            if frontier != self.target {
                self.ssa_frontiers.insert(frontier);
            }
        } else if !values.iter().all(|&value| {
            self.placement
                .can_materialize_state_read_at_block(value, self.target)
        }) {
            if let Some(frontier) = self
                .placement
                .latest_common_state_materialization_block(&values, self.target)
            {
                self.snapshot_frontiers.insert(frontier);
            } else {
                let frontier = self
                    .placement
                    .earliest_common_dominating_value_block(&values, self.target)
                    .ok_or(RejectReason::UnstableStateVersion)?;
                self.ssa_frontiers.insert(frontier);
            }
        }
        if physical_offsets.len() > 1 {
            let first_stride = physical_offsets[1].0 as isize - physical_offsets[0].0 as isize;
            if first_stride == 0
                || physical_offsets.windows(2).any(|pair| {
                    pair[1].1 != pair[0].1
                        || pair[1].0 as isize - pair[0].0 as isize != first_stride
                })
            {
                return Err(RejectReason::NonStridedStateLeaf);
            }
        }
        Ok(())
    }
}

fn exact_uniform_immediate(analyzer: &Analyzer<'_>, lanes: &[RegisterId]) -> Option<usize> {
    let mut value = None;
    for &lane in lanes {
        let SIRInstruction::Imm(_, immediate) = analyzer.instruction(lane)? else {
            return None;
        };
        let current = usize::try_from(sir_value_to_u64(immediate)?).ok()?;
        if value.is_some_and(|known| known != current) {
            return None;
        }
        value = Some(current);
    }
    value
}

fn supported_binary(operation: BinaryOp) -> bool {
    matches!(
        operation,
        BinaryOp::And
            | BinaryOp::Or
            | BinaryOp::Xor
            | BinaryOp::LogicAnd
            | BinaryOp::LogicOr
            | BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::LtU
            | BinaryOp::LeU
            | BinaryOp::GtU
            | BinaryOp::GeU
            | BinaryOp::LtS
            | BinaryOp::LeS
            | BinaryOp::GtS
            | BinaryOp::GeS
    )
}

fn binary_cost_per_chunk(operation: BinaryOp, lane_width: usize) -> usize {
    match operation {
        BinaryOp::Eq | BinaryOp::Ne if lane_width == 64 => 6,
        BinaryOp::LtU
        | BinaryOp::LeU
        | BinaryOp::GtU
        | BinaryOp::GeU
        | BinaryOp::LtS
        | BinaryOp::LeS
        | BinaryOp::GtS
        | BinaryOp::GeS
            if lane_width == 64 =>
        {
            8
        }
        BinaryOp::Eq | BinaryOp::Ne => 3,
        BinaryOp::LtU
        | BinaryOp::LeU
        | BinaryOp::GtU
        | BinaryOp::GeU
        | BinaryOp::LtS
        | BinaryOp::LeS
        | BinaryOp::GtS
        | BinaryOp::GeS => 6,
        BinaryOp::And
        | BinaryOp::Or
        | BinaryOp::Xor
        | BinaryOp::LogicAnd
        | BinaryOp::LogicOr
        | BinaryOp::Add
        | BinaryOp::Sub => 1,
        _ => unreachable!("unsupported operations are rejected before costing"),
    }
}

fn instruction_name(instruction: Option<&SIRInstruction<RegionedAbsoluteAddr>>) -> &'static str {
    match instruction {
        Some(SIRInstruction::Imm(..)) => "imm",
        Some(SIRInstruction::Binary(..)) => "binary",
        Some(SIRInstruction::Unary(..)) => "unary",
        Some(SIRInstruction::Concat(..)) => "concat",
        Some(SIRInstruction::Slice(..)) => "slice",
        Some(SIRInstruction::Mux(..)) => "mux",
        Some(SIRInstruction::Load(..)) => "load",
        Some(SIRInstruction::Store(..)) => "store",
        Some(SIRInstruction::Commit(..)) => "commit",
        Some(SIRInstruction::RuntimeEvent { .. }) => "runtime-event",
        Some(SIRInstruction::CombCaptureEvent { .. }) => "capture-event",
        Some(SIRInstruction::CombCaptureEnableIfChanged { .. }) => "capture-enable",
        None => "missing",
    }
}

fn complete_publication_root(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: BlockId,
    index: usize,
    root: RegisterId,
    lane_count: usize,
) -> bool {
    let instructions = &eu.blocks[&block].instructions;
    if index + lane_count * 2 >= instructions.len() {
        return false;
    }
    let mut destination = None;
    let mut first_offset = None;
    for lane in 0..lane_count {
        let slice_index = index + 1 + lane * 2;
        let store_index = slice_index + 1;
        let SIRInstruction::Slice(slice, source, offset, 1) = instructions[slice_index] else {
            return false;
        };
        let SIRInstruction::Store(
            address,
            SIROffset::Static(store_offset),
            1,
            stored,
            ref triggers,
            ref captures,
        ) = instructions[store_index]
        else {
            return false;
        };
        if source != root
            || offset != lane
            || stored != slice
            || !triggers.is_empty()
            || !captures.is_empty()
        {
            return false;
        }
        let start = *first_offset.get_or_insert(store_offset);
        if destination.is_some_and(|known| known != address) || store_offset != start + lane {
            return false;
        }
        destination.get_or_insert(address);
    }
    true
}

fn collect_definitions(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, (BlockId, usize)> {
    let mut definitions = HashMap::default();
    for (&block_id, block) in &eu.blocks {
        for (index, instruction) in block.instructions.iter().enumerate() {
            let register = match instruction {
                SIRInstruction::Imm(register, ..)
                | SIRInstruction::Binary(register, ..)
                | SIRInstruction::Unary(register, ..)
                | SIRInstruction::Concat(register, ..)
                | SIRInstruction::Slice(register, ..)
                | SIRInstruction::Mux(register, ..)
                | SIRInstruction::Load(register, ..) => Some(*register),
                _ => None,
            };
            if let Some(register) = register {
                definitions.insert(register, (block_id, index));
            }
        }
    }
    definitions
}

fn use_is_covered(
    use_site: ValueUse,
    covered_consumers: &HashSet<RegisterId>,
    instruction_definitions: &HashMap<(BlockId, usize), RegisterId>,
) -> bool {
    let (block, index) = match use_site {
        ValueUse::Instruction { block, index, .. } => (block, index),
        ValueUse::BranchCondition { .. } | ValueUse::EdgeArgument { .. } => return false,
    };
    instruction_definitions
        .get(&(block, index))
        .is_some_and(|register| covered_consumers.contains(register))
}

fn verify_recipe(nodes: &[RecipeNode], root: usize, lane_count: usize) -> bool {
    if nodes
        .get(root)
        .is_none_or(|node| node.lanes.len() != lane_count)
    {
        return false;
    }
    for (index, node) in nodes.iter().enumerate() {
        if node.lanes.is_empty()
            || node.lane_width == 0
            || node.children.iter().any(|child| *child >= index)
        {
            return false;
        }
        let child = |slot: usize| node.children.get(slot).and_then(|child| nodes.get(*child));
        let valid = match &node.operation {
            RecipeOp::StateRead
            | RecipeOp::Constant
            | RecipeOp::BroadcastScalar
            | RecipeOp::SsaPack
            | RecipeOp::ScalarInsert => node.children.is_empty(),
            RecipeOp::Affine | RecipeOp::PackedExtract => node.children.len() == 1,
            RecipeOp::Unary(operation) => {
                node.children.len() == 1
                    && child(0).is_some_and(|input| {
                        operation.result_width(input.lane_width) == node.lane_width
                    })
            }
            RecipeOp::Binary(_) => node.children.len() == 2,
            RecipeOp::ShiftConstant { operation, amount } => {
                node.children.len() == 1
                    && matches!(operation, BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar)
                    && child(0).is_some_and(|input| *amount < input.lane_width)
            }
            RecipeOp::OneHotDecode { shift_width } => {
                node.children.len() == 1
                    && *shift_width < usize::BITS as usize
                    && (1usize << shift_width).saturating_sub(1) < node.lane_width
                    && child(0).is_some_and(|input| input.lane_width == *shift_width)
            }
            RecipeOp::Mux => {
                node.children.len() == 3
                    && node
                        .children
                        .iter()
                        .all(|child| nodes[*child].lanes.len() == node.lanes.len())
            }
            RecipeOp::ControlMux => match node.children.as_slice() {
                [condition, then_value, else_value] => [condition, then_value, else_value]
                    .iter()
                    .all(|child| nodes[**child].lanes.len() == node.lanes.len()),
                [aggregate, inserts] => {
                    let aggregate = &nodes[*aggregate];
                    let inserts = &nodes[*inserts];
                    aggregate.lanes.len() + inserts.lanes.len() == node.lanes.len()
                        && inserts.operation == RecipeOp::ScalarInsert
                        && node.lanes.iter().all(|lane| {
                            aggregate.lanes.contains(lane) ^ inserts.lanes.contains(lane)
                        })
                }
                _ => false,
            },
            RecipeOp::Slice { offset, width } => {
                node.children.len() == 1
                    && *width == node.lane_width
                    && child(0).is_some_and(|input| {
                        offset
                            .checked_add(*width)
                            .is_some_and(|end| end <= input.lane_width)
                    })
            }
            RecipeOp::Concat { operand_widths } => {
                node.children.len() == operand_widths.len()
                    && operand_widths.iter().sum::<usize>() == node.lane_width
                    && node
                        .children
                        .iter()
                        .zip(operand_widths)
                        .all(|(child, width)| nodes[*child].lane_width == *width)
            }
        };
        if !valid {
            return false;
        }
    }
    true
}

fn build_shared_recipe_plan(candidates: &[Candidate]) -> Option<SharedRecipePlan> {
    let mut nodes = Vec::<RecipeNode>::new();
    let mut identities = HashMap::<SharedRecipeKey, usize>::default();
    let mut roots = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let mut local_to_shared = Vec::with_capacity(candidate.nodes.len());
        for node in &candidate.nodes {
            let children = node
                .children
                .iter()
                .map(|child| local_to_shared.get(*child).copied())
                .collect::<Option<Vec<_>>>()?;
            let key = SharedRecipeKey {
                operation: node.operation.clone(),
                lanes: node.lanes.clone(),
                children: children.clone(),
                lane_width: node.lane_width,
            };
            let shared = if let Some(&shared) = identities.get(&key) {
                nodes[shared].estimated_per_chunk = nodes[shared]
                    .estimated_per_chunk
                    .max(node.estimated_per_chunk);
                shared
            } else {
                let shared = nodes.len();
                nodes.push(RecipeNode {
                    operation: node.operation.clone(),
                    lanes: node.lanes.clone(),
                    children,
                    lane_width: node.lane_width,
                    estimated_per_chunk: node.estimated_per_chunk,
                });
                identities.insert(key, shared);
                shared
            };
            local_to_shared.push(shared);
        }
        roots.push((
            candidate.block,
            candidate.root,
            *local_to_shared.get(candidate.recipe_root)?,
        ));
    }
    Some(SharedRecipePlan { nodes, roots })
}

fn recipe_ancestors(nodes: &[RecipeNode], root: usize) -> Option<HashSet<usize>> {
    let mut ancestors = HashSet::default();
    let mut work = vec![root];
    while let Some(node) = work.pop() {
        if !ancestors.insert(node) {
            continue;
        }
        work.extend(nodes.get(node)?.children.iter().copied());
    }
    Some(ancestors)
}

fn peak_local_pressure(
    nodes: &[RecipeNode],
    produced: &HashSet<usize>,
    initial: &HashSet<usize>,
    roots: &[usize],
) -> Option<LocalPressure> {
    let mut remaining = vec![0usize; nodes.len()];
    for &node in produced {
        for &child in &nodes.get(node)?.children {
            if !produced.contains(&child) && !initial.contains(&child) {
                return None;
            }
            remaining[child] = remaining[child].checked_add(1)?;
        }
    }
    for &root in roots {
        remaining[root] = remaining.get(root)?.checked_add(1)?;
    }
    let mut active = HashSet::default();
    let mut current = LocalPressure::default();
    let mut peak = LocalPressure::default();
    let add = |node: usize, active: &mut HashSet<usize>, current: &mut LocalPressure| {
        if active.insert(node) {
            if nodes[node].lane_width == 1 {
                current.gpr += 1;
            } else {
                current.xmm += 1;
            }
        }
    };
    let remove = |node: usize, active: &mut HashSet<usize>, current: &mut LocalPressure| {
        if active.remove(&node) {
            if nodes[node].lane_width == 1 {
                current.gpr -= 1;
            } else {
                current.xmm -= 1;
            }
        }
    };
    for &node in initial {
        if remaining[node] != 0 {
            add(node, &mut active, &mut current);
        }
    }
    peak.gpr = peak.gpr.max(current.gpr);
    peak.xmm = peak.xmm.max(current.xmm);
    for node in 0..nodes.len() {
        if !produced.contains(&node) {
            continue;
        }
        if remaining[node] != 0 {
            add(node, &mut active, &mut current);
        }
        peak.gpr = peak.gpr.max(current.gpr);
        peak.xmm = peak.xmm.max(current.xmm);
        for &child in &nodes[node].children {
            remaining[child] = remaining[child].checked_sub(1)?;
            if remaining[child] == 0 {
                remove(child, &mut active, &mut current);
            }
        }
    }
    for &root in roots {
        remaining[root] = remaining[root].checked_sub(1)?;
        if remaining[root] == 0 {
            remove(root, &mut active, &mut current);
        }
    }
    Some(peak)
}

fn shared_recipe_pressure(
    plan: &SharedRecipePlan,
) -> Option<(usize, usize, LocalPressure, LocalPressure)> {
    let mut root_ancestors = plan
        .roots
        .iter()
        .map(|(_, _, root)| recipe_ancestors(&plan.nodes, *root))
        .collect::<Option<Vec<_>>>()?;
    let mut common = root_ancestors.pop()?;
    for ancestors in &root_ancestors {
        common.retain(|node| ancestors.contains(node));
    }
    let mut users = vec![Vec::new(); plan.nodes.len()];
    for (user, node) in plan.nodes.iter().enumerate() {
        for &child in &node.children {
            users.get_mut(child)?.push(user);
        }
    }
    let boundary = common
        .iter()
        .copied()
        .filter(|node| users[*node].iter().any(|user| !common.contains(user)))
        .collect::<HashSet<_>>();
    let prefix_peak = peak_local_pressure(
        &plan.nodes,
        &common,
        &HashSet::default(),
        &boundary.iter().copied().collect::<Vec<_>>(),
    )?;
    let mut suffix_peak = LocalPressure::default();
    for (_, _, root) in &plan.roots {
        let ancestors = recipe_ancestors(&plan.nodes, *root)?;
        let suffix = ancestors
            .difference(&common)
            .copied()
            .collect::<HashSet<_>>();
        let inputs = boundary
            .iter()
            .copied()
            .filter(|boundary| {
                suffix
                    .iter()
                    .any(|node| plan.nodes[*node].children.contains(boundary))
            })
            .collect::<HashSet<_>>();
        let peak = peak_local_pressure(&plan.nodes, &suffix, &inputs, &[*root])?;
        suffix_peak.gpr = suffix_peak.gpr.max(peak.gpr);
        suffix_peak.xmm = suffix_peak.xmm.max(peak.xmm);
    }
    Some((common.len(), boundary.len(), prefix_peak, suffix_peak))
}

pub(crate) fn analyze(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    layout: &MemoryLayout,
) -> Result<LaneAggregateFeasibilityReport, String> {
    let placement = PlacementAnalysis::analyze(eu).map_err(|error| error.to_string())?;
    let definitions = collect_definitions(eu);
    let instruction_definitions = definitions
        .iter()
        .map(|(&register, &(block, index))| ((block, index), register))
        .collect::<HashMap<_, _>>();
    let mut report = LaneAggregateFeasibilityReport::default();
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable();
    for block_id in block_ids {
        let block = &eu.blocks[&block_id];
        for (index, instruction) in block.instructions.iter().enumerate() {
            let SIRInstruction::Concat(root, arguments) = instruction else {
                continue;
            };
            let lane_count = arguments.len();
            if !(8..=64).contains(&lane_count)
                || !complete_publication_root(eu, block_id, index, *root, lane_count)
            {
                continue;
            }
            report.candidates += 1;
            let mut analyzer = Analyzer {
                eu,
                layout,
                placement: &placement,
                definitions: &definitions,
                memo: HashMap::default(),
                active: HashSet::default(),
                nodes: Vec::new(),
                target: block_id,
                failure_key: None,
                failure_path: Vec::new(),
                snapshot_frontiers: HashSet::default(),
                ssa_frontiers: HashSet::default(),
            };
            let lanes = arguments.iter().rev().copied().collect::<Vec<_>>();
            let recipe = analyzer.analyze(lanes).and_then(|root| {
                verify_recipe(&analyzer.nodes, root, lane_count)
                    .then_some(root)
                    .ok_or(RejectReason::InvalidRecipe)
            });
            match recipe {
                Ok(recipe_root) => {
                    let covered_registers = analyzer
                        .nodes
                        .iter()
                        .flat_map(|node| node.lanes.iter().copied())
                        .collect::<HashSet<_>>();
                    let mut covered_consumers = covered_registers.clone();
                    covered_consumers.insert(*root);
                    for lane in 0..lane_count {
                        if let SIRInstruction::Slice(slice, ..) =
                            block.instructions[index + 1 + lane * 2]
                        {
                            covered_consumers.insert(slice);
                        }
                    }
                    let estimated_instructions = analyzer
                        .nodes
                        .iter()
                        .map(|node| {
                            let lanes_per_chunk = (128 / node.lane_width.max(1)).max(1);
                            node.estimated_per_chunk * lane_count.div_ceil(lanes_per_chunk)
                        })
                        .sum::<usize>()
                        .saturating_add(lane_count.div_ceil(16) * 2);
                    report.summed_estimated_instructions += estimated_instructions;
                    for node in &analyzer.nodes {
                        *report.kind_counts.entry(node.operation.kind()).or_default() += 1;
                        let _ = node.children.len();
                    }
                    report.accepted.push(Candidate {
                        block: block_id,
                        root: *root,
                        recipe_root,
                        lane_count,
                        nodes: analyzer.nodes,
                        covered_registers,
                        covered_consumers,
                        snapshot_frontiers: {
                            let mut frontiers =
                                analyzer.snapshot_frontiers.into_iter().collect::<Vec<_>>();
                            frontiers.sort_unstable();
                            frontiers
                        },
                        ssa_frontiers: {
                            let mut frontiers =
                                analyzer.ssa_frontiers.into_iter().collect::<Vec<_>>();
                            frontiers.sort_unstable();
                            frontiers
                        },
                        estimated_instructions,
                    });
                }
                Err(reason) => {
                    let failure_key = analyzer.failure_key.as_deref().unwrap_or(&[]);
                    let mut sample_widths = failure_key
                        .iter()
                        .filter_map(|register| eu.register_map.get(register).map(|ty| ty.width()))
                        .collect::<Vec<_>>();
                    sample_widths.sort_unstable();
                    sample_widths.dedup();
                    let sample_register = failure_key.first().copied();
                    let sample_instruction = instruction_name(
                        sample_register.and_then(|register| analyzer.instruction(register)),
                    );
                    let mut shape_counts = HashMap::<&'static str, usize>::default();
                    for &register in failure_key {
                        *shape_counts
                            .entry(instruction_name(analyzer.instruction(register)))
                            .or_default() += 1;
                    }
                    let mut sample_shapes = shape_counts.into_iter().collect::<Vec<_>>();
                    sample_shapes.sort_unstable();
                    let sample_examples = failure_key
                        .iter()
                        .take(4)
                        .map(|register| {
                            format!("r{}={:?}", register.0, analyzer.instruction(*register))
                        })
                        .collect();
                    let sample_operand_examples = sample_register
                        .and_then(|register| analyzer.instruction(register))
                        .map(|instruction| match instruction {
                            SIRInstruction::Mux(_, condition, then_value, else_value) => {
                                vec![*condition, *then_value, *else_value]
                            }
                            SIRInstruction::Binary(_, lhs, _, rhs) => vec![*lhs, *rhs],
                            SIRInstruction::Unary(_, _, source)
                            | SIRInstruction::Slice(_, source, ..) => vec![*source],
                            SIRInstruction::Concat(_, arguments) => arguments.clone(),
                            _ => Vec::new(),
                        })
                        .unwrap_or_default()
                        .into_iter()
                        .map(|register| {
                            let origin = analyzer
                                .placement
                                .value_for_register(register)
                                .and_then(|value| analyzer.placement.value(value))
                                .map(|value| format!("{:?}", value.origin))
                                .unwrap_or_else(|| "unknown".into());
                            format!(
                                "r{}={:?} origin={origin}",
                                register.0,
                                analyzer.instruction(register)
                            )
                        })
                        .collect();
                    let missing_registers = failure_key
                        .iter()
                        .copied()
                        .filter(|register| analyzer.instruction(*register).is_none())
                        .collect();
                    *report.reject_counts.entry(reason).or_default() += 1;
                    report.rejected.push(RejectedCandidate {
                        block: block_id,
                        root: *root,
                        lane_count,
                        reason,
                        sample_register,
                        sample_widths,
                        sample_instruction,
                        sample_shapes,
                        sample_examples,
                        sample_operand_examples,
                        missing_registers,
                        failure_path: analyzer.failure_path,
                    });
                }
            }
        }
    }

    let covered = report
        .accepted
        .iter()
        .flat_map(|candidate| candidate.covered_registers.iter().copied())
        .collect::<HashSet<_>>();
    let covered_consumers = report
        .accepted
        .iter()
        .flat_map(|candidate| candidate.covered_consumers.iter().copied())
        .collect::<HashSet<_>>();
    report.covered_scalar_definitions = covered.len();
    report.dead_scalar_definitions = covered
        .iter()
        .filter(|register| {
            placement
                .value_for_register(**register)
                .and_then(|value| placement.value(value))
                .is_some_and(|value| {
                    value.uses.iter().all(|use_site| {
                        use_is_covered(*use_site, &covered_consumers, &instruction_definitions)
                    })
                })
        })
        .count();
    let shared_plan =
        build_shared_recipe_plan(&report.accepted).ok_or("invalid shared recipe plan")?;
    let total_nodes = report
        .accepted
        .iter()
        .map(|candidate| candidate.nodes.len())
        .sum::<usize>();
    let lane_count = report
        .accepted
        .first()
        .map_or(0, |candidate| candidate.lane_count);
    let unique_node_cost = shared_plan
        .nodes
        .iter()
        .map(|node| {
            let lanes_per_chunk = (128 / node.lane_width.max(1)).max(1);
            node.estimated_per_chunk * lane_count.div_ceil(lanes_per_chunk)
        })
        .sum::<usize>();
    let publication_cost = report
        .accepted
        .iter()
        .map(|candidate| candidate.lane_count.div_ceil(16) * 2)
        .sum::<usize>();
    report.unique_estimated_instructions = unique_node_cost + publication_cost;
    report.shared_recipe_nodes = total_nodes.saturating_sub(shared_plan.nodes.len());
    debug_assert_eq!(shared_plan.roots.len(), report.accepted.len());
    if let Some((prefix_nodes, boundary_values, prefix_peak, suffix_peak)) =
        shared_recipe_pressure(&shared_plan)
    {
        report.shared_prefix_nodes = prefix_nodes;
        report.shared_boundary_values = boundary_values;
        report.peak_prefix_gpr_values = prefix_peak.gpr;
        report.peak_prefix_xmm_values = prefix_peak.xmm;
        report.peak_suffix_gpr_values = suffix_peak.gpr;
        report.peak_suffix_xmm_values = suffix_peak.xmm;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::memory_layout::{MemoryLayoutMode, UnpackedArrayLayout};
    use crate::ir::{
        AbsoluteAddr, BasicBlock, InstanceId, RegisterType, SIRTerminator, SIRValue, STABLE_REGION,
    };
    use veryl_analyzer::ir::VarId;

    fn layout(absolute: AbsoluteAddr) -> MemoryLayout {
        let mut layout = MemoryLayout {
            four_state: false,
            mode: MemoryLayoutMode::ElementStrided,
            offsets: HashMap::default(),
            widths: HashMap::default(),
            is_4states: HashMap::default(),
            unpacked_arrays: HashMap::default(),
            total_size: 8,
            working_offsets: HashMap::default(),
            working_base_offset: 8,
            sparse_offsets: HashMap::default(),
            sparse_base_offset: 8,
            sparse_layouts: HashMap::default(),
            sparse_active_bits_offset: 8,
            sparse_active_capacity: 0,
            merged_total_size: 8,
            triggered_bits_offset: 8,
            triggered_bits_total_size: 0,
            scratch_base_offset: 8,
            scratch_size: 0,
            runtime_event_capacity: 0,
            runtime_event_slot_size: 0,
            runtime_event_buffer_size: 0,
            runtime_event_site_layouts: Vec::new(),
        };
        layout.offsets.insert(absolute, 0);
        layout.widths.insert(absolute, 8);
        layout.is_4states.insert(absolute, false);
        layout.unpacked_arrays.insert(
            absolute,
            UnpackedArrayLayout {
                element_width: 1,
                element_count: 8,
                element_stride: 1,
                plane_size: 8,
            },
        );
        layout
    }

    fn fixture(variable_shift: bool) -> (ExecutionUnit<RegionedAbsoluteAddr>, MemoryLayout) {
        let absolute = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: VarId::default(),
        };
        let address = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute);
        let destination = RegionedAbsoluteAddr::from_absolute_addr(
            STABLE_REGION,
            AbsoluteAddr {
                var_id: VarId::from_raw(1),
                ..absolute
            },
        );
        let mut register_map = HashMap::default();
        let mut instructions = Vec::new();
        let shift = RegisterId(0);
        register_map.insert(
            shift,
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
        instructions.push(SIRInstruction::Imm(shift, SIRValue::new(0u8)));
        let mut lanes = Vec::new();
        for lane in 0..8 {
            let load = RegisterId(1 + lane * 3);
            let lane_shift = if variable_shift {
                RegisterId(2 + lane * 3)
            } else {
                shift
            };
            let result = RegisterId(3 + lane * 3);
            register_map.insert(
                load,
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
            instructions.push(SIRInstruction::Load(
                load,
                address,
                SIROffset::Static(lane),
                1,
            ));
            if variable_shift {
                register_map.insert(
                    lane_shift,
                    RegisterType::Bit {
                        width: 1,
                        signed: false,
                    },
                );
                instructions.push(SIRInstruction::Load(
                    lane_shift,
                    address,
                    SIROffset::Static((lane + 1) % 8),
                    1,
                ));
            }
            register_map.insert(
                result,
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
            instructions.push(SIRInstruction::Binary(
                result,
                load,
                BinaryOp::Shl,
                lane_shift,
            ));
            lanes.push(result);
        }
        let root = RegisterId(25);
        register_map.insert(
            root,
            RegisterType::Bit {
                width: 8,
                signed: false,
            },
        );
        instructions.push(SIRInstruction::Concat(
            root,
            lanes.iter().rev().copied().collect(),
        ));
        for lane in 0..8 {
            let slice = RegisterId(26 + lane);
            register_map.insert(
                slice,
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
            instructions.push(SIRInstruction::Slice(slice, root, lane, 1));
            instructions.push(SIRInstruction::Store(
                destination,
                SIROffset::Static(lane),
                1,
                slice,
                Vec::new(),
                Vec::new(),
            ));
        }
        let block = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions,
            terminator: SIRTerminator::Return,
        };
        (
            ExecutionUnit {
                entry_block_id: BlockId(0),
                blocks: [(BlockId(0), block)].into_iter().collect(),
                register_map,
            },
            layout(absolute),
        )
    }

    #[test]
    fn accepts_exact_state_leaves_and_uniform_shift() {
        let (eu, layout) = fixture(false);
        eu.verify();
        let report = analyze(&eu, &layout).unwrap();
        assert_eq!(report.candidates, 1);
        assert_eq!(report.accepted.len(), 1);
        assert!(report.rejected.is_empty());
        assert!(report.kind_counts[&RecipeKind::StateRead] >= 1);
        assert!(report.kind_counts[&RecipeKind::ShiftConstant] >= 1);
    }

    #[test]
    fn rejects_lane_variable_shift_without_exact_simd_semantics() {
        let (eu, layout) = fixture(true);
        eu.verify();
        let report = analyze(&eu, &layout).unwrap();
        assert_eq!(report.candidates, 1);
        assert!(report.accepted.is_empty());
        assert_eq!(report.reject_counts[&RejectReason::UnsupportedOperation], 1);
    }

    #[test]
    fn recognizes_range_proven_one_hot_decode() {
        let absolute = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: VarId::default(),
        };
        let destination = RegionedAbsoluteAddr::from_absolute_addr(
            STABLE_REGION,
            AbsoluteAddr {
                var_id: VarId::from_raw(1),
                ..absolute
            },
        );
        let mut register_map = HashMap::default();
        let mut instructions = Vec::new();
        let one = RegisterId(0);
        let zero = RegisterId(1);
        for register in [one, zero] {
            register_map.insert(
                register,
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
        }
        instructions.push(SIRInstruction::Imm(one, SIRValue::new(1u8)));
        instructions.push(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
        let mut predicates = Vec::new();
        let mut next = 2;
        for lane in 0..8 {
            let shift = RegisterId(next);
            let decoded = RegisterId(next + 1);
            let predicate = RegisterId(next + 2);
            next += 3;
            register_map.insert(
                shift,
                RegisterType::Bit {
                    width: 2,
                    signed: false,
                },
            );
            register_map.insert(
                decoded,
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
            register_map.insert(
                predicate,
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
            instructions.push(SIRInstruction::Imm(shift, SIRValue::new((lane % 4) as u8)));
            instructions.push(SIRInstruction::Binary(decoded, one, BinaryOp::Shl, shift));
            instructions.push(SIRInstruction::Binary(
                predicate,
                decoded,
                BinaryOp::Ne,
                zero,
            ));
            predicates.push(predicate);
        }
        let root = RegisterId(next);
        next += 1;
        register_map.insert(
            root,
            RegisterType::Bit {
                width: 8,
                signed: false,
            },
        );
        instructions.push(SIRInstruction::Concat(
            root,
            predicates.iter().rev().copied().collect(),
        ));
        for lane in 0..8 {
            let slice = RegisterId(next);
            next += 1;
            register_map.insert(
                slice,
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
            instructions.push(SIRInstruction::Slice(slice, root, lane, 1));
            instructions.push(SIRInstruction::Store(
                destination,
                SIROffset::Static(lane),
                1,
                slice,
                Vec::new(),
                Vec::new(),
            ));
        }
        let eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: Vec::new(),
                    instructions,
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map,
        };
        eu.verify();

        let report = analyze(&eu, &layout(absolute)).unwrap();
        assert_eq!(report.accepted.len(), 1);
        assert_eq!(report.kind_counts[&RecipeKind::OneHotDecode], 1);
    }

    #[test]
    fn executable_recipe_verifier_rejects_forward_edges_and_invalid_shift_ranges() {
        let lanes = (0..8).map(RegisterId).collect::<Vec<_>>();
        let mut nodes = vec![
            RecipeNode {
                operation: RecipeOp::Constant,
                lanes: lanes.clone(),
                children: Vec::new(),
                lane_width: 2,
                estimated_per_chunk: 1,
            },
            RecipeNode {
                operation: RecipeOp::OneHotDecode { shift_width: 2 },
                lanes: lanes.clone(),
                children: vec![0],
                lane_width: 4,
                estimated_per_chunk: 1,
            },
        ];
        assert!(verify_recipe(&nodes, 1, 8));

        nodes[0].children.push(1);
        assert!(!verify_recipe(&nodes, 1, 8));
        nodes[0].children.clear();
        nodes[1].lane_width = 3;
        assert!(!verify_recipe(&nodes, 1, 8));
    }

    #[test]
    fn shared_plan_interns_only_identical_typed_nodes_and_children() {
        let lanes = (0..8).map(RegisterId).collect::<Vec<_>>();
        let nodes = vec![
            RecipeNode {
                operation: RecipeOp::Constant,
                lanes: lanes.clone(),
                children: Vec::new(),
                lane_width: 8,
                estimated_per_chunk: 1,
            },
            RecipeNode {
                operation: RecipeOp::Unary(UnaryOp::BitNot),
                lanes: lanes.clone(),
                children: vec![0],
                lane_width: 8,
                estimated_per_chunk: 1,
            },
        ];
        let candidate = |block, root, nodes: Vec<RecipeNode>| Candidate {
            block,
            root,
            recipe_root: 1,
            lane_count: 8,
            nodes,
            covered_registers: HashSet::default(),
            covered_consumers: HashSet::default(),
            snapshot_frontiers: Vec::new(),
            ssa_frontiers: Vec::new(),
            estimated_instructions: 0,
        };
        let mut distinct = nodes.clone();
        distinct[1].operation = RecipeOp::Unary(UnaryOp::LogicNot);
        distinct[1].lane_width = 1;
        let plan = build_shared_recipe_plan(&[
            candidate(BlockId(0), RegisterId(20), nodes.clone()),
            candidate(BlockId(1), RegisterId(21), nodes),
            candidate(BlockId(2), RegisterId(22), distinct),
        ])
        .unwrap();
        assert_eq!(plan.nodes.len(), 3);
        assert_eq!(plan.roots.len(), 3);
        assert_eq!(plan.roots[0].2, plan.roots[1].2);
        assert_ne!(plan.roots[0].2, plan.roots[2].2);
        let (prefix, boundary, prefix_peak, suffix_peak) = shared_recipe_pressure(&plan).unwrap();
        assert_eq!(prefix, 1);
        assert_eq!(boundary, 1);
        assert_eq!(prefix_peak.xmm, 1);
        assert!(suffix_peak.xmm >= 1);
    }

    #[test]
    fn packs_one_bit_ssa_frontier_before_a_distant_sink() {
        let (mut eu, layout) = fixture(true);
        let mut original = eu.blocks.remove(&BlockId(0)).unwrap();
        let root_index = original
            .instructions
            .iter()
            .position(|instruction| matches!(instruction, SIRInstruction::Concat(..)))
            .unwrap();
        let sink_instructions = original.instructions.split_off(root_index);
        original.terminator = SIRTerminator::Jump(BlockId(1), vec![]);
        eu.blocks.insert(BlockId(0), original);
        eu.blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: sink_instructions,
                terminator: SIRTerminator::Return,
            },
        );
        eu.verify();

        let report = analyze(&eu, &layout).unwrap();
        assert_eq!(report.accepted.len(), 1);
        assert_eq!(report.kind_counts[&RecipeKind::SsaPack], 1);
        assert_eq!(report.accepted[0].ssa_frontiers, vec![BlockId(0)]);
    }

    #[test]
    fn inserts_a_scalar_control_phi_into_an_aggregate_at_its_merge() {
        let (mut eu, layout) = fixture(false);
        let mut original = eu.blocks.remove(&BlockId(0)).unwrap();
        let root_index = original
            .instructions
            .iter()
            .position(|instruction| matches!(instruction, SIRInstruction::Concat(..)))
            .unwrap();
        let mut sink = original.instructions.split_off(root_index);
        let condition = RegisterId(40);
        let then_value = RegisterId(41);
        let else_value = RegisterId(42);
        let parameter = RegisterId(43);
        let false_copy = RegisterId(51);
        eu.register_map.insert(
            condition,
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
        for register in [then_value, else_value, parameter, false_copy] {
            eu.register_map.insert(
                register,
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
        }
        original.instructions.extend([
            SIRInstruction::Imm(condition, SIRValue::new(1u8)),
            SIRInstruction::Imm(then_value, SIRValue::new(1u8)),
            SIRInstruction::Imm(else_value, SIRValue::new(0u8)),
        ]);
        original.terminator = SIRTerminator::Branch {
            cond: condition,
            true_block: (BlockId(1), Vec::new()),
            false_block: (BlockId(2), Vec::new()),
        };
        eu.blocks.insert(BlockId(0), original);
        eu.blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: Vec::new(),
                terminator: SIRTerminator::Jump(BlockId(3), vec![then_value]),
            },
        );
        eu.blocks.insert(
            BlockId(2),
            BasicBlock {
                id: BlockId(2),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Unary(
                    false_copy,
                    UnaryOp::Ident,
                    else_value,
                )],
                terminator: SIRTerminator::Jump(BlockId(3), vec![false_copy]),
            },
        );
        let mut mux_instructions = Vec::new();
        let SIRInstruction::Concat(_, lanes) = &mut sink[0] else {
            unreachable!();
        };
        let mut values = vec![parameter];
        for lane in 1..lanes.len() {
            let mux = RegisterId(43 + lane);
            eu.register_map.insert(
                mux,
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            );
            mux_instructions.push(SIRInstruction::Mux(mux, condition, then_value, else_value));
            values.push(mux);
        }
        let zero = RegisterId(52);
        eu.register_map.insert(
            zero,
            RegisterType::Bit {
                width: 64,
                signed: false,
            },
        );
        mux_instructions.push(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
        for (lane, (output, value)) in lanes.iter_mut().zip(values).enumerate() {
            let predicate = RegisterId(53 + lane);
            eu.register_map.insert(
                predicate,
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
            mux_instructions.push(SIRInstruction::Binary(predicate, value, BinaryOp::Ne, zero));
            *output = predicate;
        }
        eu.blocks.insert(
            BlockId(3),
            BasicBlock {
                id: BlockId(3),
                params: vec![parameter],
                instructions: mux_instructions,
                terminator: SIRTerminator::Jump(BlockId(4), Vec::new()),
            },
        );
        eu.blocks.insert(
            BlockId(4),
            BasicBlock {
                id: BlockId(4),
                params: Vec::new(),
                instructions: sink,
                terminator: SIRTerminator::Return,
            },
        );
        eu.verify();

        let report = analyze(&eu, &layout).unwrap();
        assert_eq!(report.accepted.len(), 1);
        assert_eq!(
            report.kind_counts.get(&RecipeKind::ControlMux),
            Some(&1),
            "{:?}",
            report.kind_counts
        );
        assert_eq!(
            report.kind_counts.get(&RecipeKind::ScalarInsert),
            Some(&1),
            "{:?}",
            report.kind_counts
        );
        assert_eq!(report.accepted[0].ssa_frontiers, vec![BlockId(3)]);
    }

    #[test]
    fn recognizes_passthrough_lane_as_zero_offset_affine_member() {
        let absolute = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: VarId::default(),
        };
        let address = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute);
        let destination = RegionedAbsoluteAddr::from_absolute_addr(
            STABLE_REGION,
            AbsoluteAddr {
                var_id: VarId::from_raw(1),
                ..absolute
            },
        );
        let mut register_map = HashMap::default();
        let mut instructions = Vec::new();
        let mut next = 0usize;
        let base = RegisterId(next);
        next += 1;
        register_map.insert(
            base,
            RegisterType::Bit {
                width: 64,
                signed: false,
            },
        );
        instructions.push(SIRInstruction::Load(
            base,
            address,
            SIROffset::Static(0),
            64,
        ));
        let threshold = RegisterId(next);
        next += 1;
        register_map.insert(
            threshold,
            RegisterType::Bit {
                width: 64,
                signed: false,
            },
        );
        instructions.push(SIRInstruction::Imm(threshold, SIRValue::new(4u8)));
        let mut predicates = Vec::new();
        for lane in 0..8 {
            let value = if lane == 0 {
                base
            } else {
                let immediate = RegisterId(next);
                next += 1;
                register_map.insert(
                    immediate,
                    RegisterType::Bit {
                        width: 64,
                        signed: false,
                    },
                );
                instructions.push(SIRInstruction::Imm(immediate, SIRValue::new(lane as u64)));
                let value = RegisterId(next);
                next += 1;
                register_map.insert(
                    value,
                    RegisterType::Bit {
                        width: 64,
                        signed: false,
                    },
                );
                instructions.push(SIRInstruction::Binary(
                    value,
                    base,
                    BinaryOp::Add,
                    immediate,
                ));
                value
            };
            let predicate = RegisterId(next);
            next += 1;
            register_map.insert(
                predicate,
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
            instructions.push(SIRInstruction::Binary(
                predicate,
                value,
                BinaryOp::GtU,
                threshold,
            ));
            predicates.push(predicate);
        }
        let root = RegisterId(next);
        next += 1;
        register_map.insert(
            root,
            RegisterType::Bit {
                width: 8,
                signed: false,
            },
        );
        instructions.push(SIRInstruction::Concat(
            root,
            predicates.iter().rev().copied().collect(),
        ));
        for lane in 0..8 {
            let slice = RegisterId(next);
            next += 1;
            register_map.insert(
                slice,
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
            instructions.push(SIRInstruction::Slice(slice, root, lane, 1));
            instructions.push(SIRInstruction::Store(
                destination,
                SIROffset::Static(lane),
                1,
                slice,
                Vec::new(),
                Vec::new(),
            ));
        }
        let eu = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: Vec::new(),
                    instructions,
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map,
        };
        eu.verify();
        let mut layout = layout(absolute);
        layout.unpacked_arrays.clear();
        layout.widths.insert(absolute, 64);

        let report = analyze(&eu, &layout).unwrap();
        assert_eq!(report.accepted.len(), 1);
        assert_eq!(report.kind_counts[&RecipeKind::Affine], 1);
    }
}
