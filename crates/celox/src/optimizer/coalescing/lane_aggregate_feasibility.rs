//! Analysis-only feasibility gate for lane-aggregate recipes.
//!
//! A recipe starts at a complete packed predicate publication and walks the
//! synchronous product of its scalar lanes.  It never rewrites SIR.  State
//! leaves are accepted only when placement analysis proves that the exact
//! read version can be materialized in the sink block.

use std::fmt;

use super::placement_analysis::{PlacementAnalysis, ValueSafety, ValueUse};
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
    Unary,
    Binary,
    ShiftConstant,
    Mux,
    Slice,
    Concat,
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
            Self::NodeBudget => "node-budget",
            Self::Cycle => "cycle",
        })
    }
}

#[derive(Debug, Clone)]
struct RecipeNode {
    kind: RecipeKind,
    lanes: Vec<RegisterId>,
    children: Vec<usize>,
    lane_width: usize,
    estimated_per_chunk: usize,
}

#[derive(Debug, Clone)]
struct Candidate {
    block: BlockId,
    root: RegisterId,
    lane_count: usize,
    nodes: Vec<RecipeNode>,
    covered_registers: HashSet<RegisterId>,
    covered_consumers: HashSet<RegisterId>,
    snapshot_frontiers: Vec<BlockId>,
    ssa_frontiers: Vec<BlockId>,
    estimated_instructions: usize,
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
    missing_registers: Vec<RegisterId>,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct LaneAggregateFeasibilityReport {
    candidates: usize,
    accepted: Vec<Candidate>,
    rejected: Vec<RejectedCandidate>,
    covered_scalar_definitions: usize,
    dead_scalar_definitions: usize,
    estimated_instructions: usize,
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
            "candidates={} accepted={} rejected={} covered_scalar_defs={} dead_scalar_defs={} estimated_insts={}",
            self.candidates,
            self.accepted.len(),
            self.rejected.len(),
            self.covered_scalar_definitions,
            self.dead_scalar_definitions,
            self.estimated_instructions,
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
    snapshot_frontiers: HashSet<BlockId>,
    ssa_frontiers: HashSet<BlockId>,
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
        kind: RecipeKind,
        children: Vec<usize>,
        lane_width: usize,
        estimated_per_chunk: usize,
    ) -> Result<usize, RejectReason> {
        if self.nodes.len() >= MAX_RECIPE_NODES {
            return Err(RejectReason::NodeBudget);
        }
        let id = self.nodes.len();
        self.nodes.push(RecipeNode {
            kind,
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
        let result = self.analyze_uncached(key.clone());
        if result.is_err() && self.failure_key.is_none() {
            self.failure_key = Some(key.clone());
        }
        self.active.remove(&key);
        result
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
            return self.insert_node(key, RecipeKind::Constant, Vec::new(), lane_width, 1);
        }

        if key.iter().all(|register| *register == first) {
            let value = self
                .placement
                .value_for_register(first)
                .and_then(|value| self.placement.value(value))
                .ok_or(RejectReason::MissingDefinition)?;
            return match value.safety {
                ValueSafety::Pure if self.placement.can_sink_to_block(value.id, self.target) => {
                    self.insert_node(key, RecipeKind::BroadcastScalar, Vec::new(), lane_width, 4)
                }
                ValueSafety::StateRead(_)
                    if self
                        .placement
                        .can_materialize_state_read_at_block(value.id, self.target) =>
                {
                    self.insert_node(key, RecipeKind::StateRead, Vec::new(), lane_width, 5)
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
                    self.insert_node(key, RecipeKind::StateRead, Vec::new(), lane_width, 5)
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
            return self.insert_node(key, RecipeKind::StateRead, Vec::new(), lane_width, 1);
        }

        if let Some(source) = self.regular_shift_source(&key, lane_width) {
            let child = self.analyze(vec![source; key.len()])?;
            return self.insert_node(key, RecipeKind::PackedExtract, vec![child], lane_width, 1);
        }

        if let Some(base) = self.affine_base(&key, lane_width) {
            let child = self.analyze(vec![base; key.len()])?;
            return self.insert_node(key, RecipeKind::Affine, vec![child], lane_width, 2);
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
                self.insert_node(key, RecipeKind::Unary, vec![child], lane_width, 1)
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
                    let shift = exact_uniform_immediate(self, &rhs_lanes)
                        .ok_or(RejectReason::UnsupportedOperation)?;
                    if shift >= lane_width {
                        return Err(RejectReason::UnsupportedOperation);
                    }
                    let child = self.analyze(lhs_lanes)?;
                    return self.insert_node(
                        key,
                        RecipeKind::ShiftConstant,
                        vec![child],
                        lane_width,
                        1,
                    );
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
                    RecipeKind::Binary,
                    vec![lhs, rhs],
                    lane_width,
                    1 + usize::from(wrap_mask),
                )
            }
            SIRInstruction::Mux(_, condition, then_value, else_value) => {
                let mut conditions = Vec::with_capacity(key.len());
                let mut then_values = Vec::with_capacity(key.len());
                let mut else_values = Vec::with_capacity(key.len());
                conditions.push(condition);
                then_values.push(then_value);
                else_values.push(else_value);
                for register in key.iter().skip(1) {
                    let Some(SIRInstruction::Mux(_, condition, then_value, else_value)) =
                        self.instruction(*register)
                    else {
                        return Err(RejectReason::HeterogeneousOperation);
                    };
                    conditions.push(*condition);
                    then_values.push(*then_value);
                    else_values.push(*else_value);
                }
                let condition = self.analyze(conditions)?;
                let then_value = self.analyze(then_values)?;
                let else_value = self.analyze(else_values)?;
                self.insert_node(
                    key,
                    RecipeKind::Mux,
                    vec![condition, then_value, else_value],
                    lane_width,
                    3,
                )
            }
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
                self.insert_node(key, RecipeKind::Slice, vec![child], lane_width, 2)
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
                self.insert_node(key, RecipeKind::Concat, children, lane_width, 1)
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

    fn verify_state_leaf(&mut self, key: &[RegisterId]) -> Result<(), RejectReason> {
        let mut address = None;
        let mut width = None;
        let mut physical_offsets = Vec::with_capacity(key.len());
        let mut values = Vec::with_capacity(key.len());
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
                return Err(RejectReason::UnstableStateVersion);
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
        if !values.iter().all(|&value| {
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
                snapshot_frontiers: HashSet::default(),
                ssa_frontiers: HashSet::default(),
            };
            let lanes = arguments.iter().rev().copied().collect::<Vec<_>>();
            match analyzer.analyze(lanes) {
                Ok(_) => {
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
                    report.estimated_instructions += estimated_instructions;
                    for node in &analyzer.nodes {
                        *report.kind_counts.entry(node.kind).or_default() += 1;
                        let _ = node.children.len();
                    }
                    report.accepted.push(Candidate {
                        block: block_id,
                        root: *root,
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
                        missing_registers,
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
