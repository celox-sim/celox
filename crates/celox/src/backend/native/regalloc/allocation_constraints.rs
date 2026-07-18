//! Target-register constraints and copy affinities for allocation IR.
//!
//! Constraint permutation boundaries split long SSA ranges before home
//! selection. This model then rebuilds fixed operands and clobber exclusions
//! from the rewritten allocation IR, so synthetic reloads and recipes cannot
//! inherit stale source-MIR constraints. Copy and phi affinities are hints;
//! allowed-register masks are mandatory correctness constraints.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::backend::native::mir::{BlockId, VReg};

use super::allocation_expand::ExpandedAllocationProblem;
use super::allocation_ir::{AllocationAffinity, AllocationAffinityKind, AllocationMachineFacts};
use super::assignment::PhysReg;
use super::cfg::NormalizedCfg;
use super::home_graph::HomeGraph;
use super::live_interval::{LiveInterval, SlotIndex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RegisterMask(u16);

impl RegisterMask {
    fn empty() -> Self {
        Self(0)
    }

    pub(super) fn from_registers(registers: &[PhysReg]) -> Self {
        Self(
            registers
                .iter()
                .copied()
                .fold(0, |mask, register| mask | register_bit(register)),
        )
    }

    fn intersect(&mut self, other: Self) {
        self.0 &= other.0;
    }

    fn remove(&mut self, registers: &[PhysReg]) {
        for &register in registers {
            self.0 &= !register_bit(register);
        }
    }

    pub(super) fn contains(self, register: PhysReg) -> bool {
        self.0 & register_bit(register) != 0
    }

    pub(super) fn count(self) -> u32 {
        self.0.count_ones()
    }

    pub(super) fn is_empty(self) -> bool {
        self.0 == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct WeightedAffinity {
    pub left: VReg,
    pub right: VReg,
    pub weight: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AllocationConstraintModel {
    value_count: u32,
    allowed: Vec<RegisterMask>,
    pub affinities: Vec<WeightedAffinity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FixedConstraintPoint {
    block: BlockId,
    instruction: usize,
    register: PhysReg,
}

/// Persistent target-constraint facts for one allocation session.
///
/// Synthetic insertion can shift every clobber point in one block, while a
/// rewritten fixed operand or copy changes only that block's semantic facts.
/// The index replaces those rows and recomputes masks only for values whose
/// sparse interval or local facts changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct IncrementalConstraintModel {
    registers: Vec<PhysReg>,
    block_facts: Vec<AllocationMachineFacts>,
    fixed_by_value: Vec<Vec<FixedConstraintPoint>>,
    affinity_counts: BTreeMap<AllocationAffinity, u32>,
    model: AllocationConstraintModel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AllocationConstraintError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub values: Vec<VReg>,
    pub message: String,
}

impl AllocationConstraintError {
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

impl fmt::Display for AllocationConstraintError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.rule)?;
        if let Some(block) = self.block {
            write!(formatter, " at {block}")?;
        }
        if let Some(instruction) = self.instruction {
            write!(formatter, "/i{instruction}")?;
        }
        if !self.values.is_empty() {
            write!(formatter, " values={:?}", self.values)?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for AllocationConstraintError {}

#[derive(Debug, Clone)]
struct ClobberPoint {
    use_slot: SlotIndex,
    def_slot: SlotIndex,
    block: BlockId,
    instruction: usize,
    registers: Vec<PhysReg>,
}

impl AllocationConstraintModel {
    pub(super) fn build(
        expanded: &ExpandedAllocationProblem,
        cfg: &NormalizedCfg,
        graph: &HomeGraph,
        registers: &[PhysReg],
    ) -> Result<Self, AllocationConstraintError> {
        Self::compute(expanded, cfg, graph, registers)
    }

    /// Independently rebuild the target model twice at a publication boundary.
    /// Allocation-session updates use [`Self::build`] once; they must not turn
    /// an independent verifier into work repeated after every range split.
    pub(super) fn build_verified(
        expanded: &ExpandedAllocationProblem,
        cfg: &NormalizedCfg,
        graph: &HomeGraph,
        registers: &[PhysReg],
    ) -> Result<Self, AllocationConstraintError> {
        let result = Self::build(expanded, cfg, graph, registers)?;
        result.verify(expanded, cfg, graph, registers)?;
        Ok(result)
    }

    pub(super) fn allowed(&self, value: VReg) -> Option<RegisterMask> {
        self.allowed.get(value.0 as usize).copied()
    }

    pub(super) fn verify(
        &self,
        expanded: &ExpandedAllocationProblem,
        cfg: &NormalizedCfg,
        graph: &HomeGraph,
        registers: &[PhysReg],
    ) -> Result<(), AllocationConstraintError> {
        if self.value_count != expanded.ir.value_count()
            || self.allowed.len() != self.value_count as usize
        {
            return Err(AllocationConstraintError::new(
                "ALLOCATION_CONSTRAINT.MODEL_SHAPE",
                None,
                None,
                Vec::new(),
                "constraint table does not cover the allocation value domain",
            ));
        }
        let expected = Self::compute(expanded, cfg, graph, registers)?;
        if *self != expected {
            return Err(AllocationConstraintError::new(
                "ALLOCATION_CONSTRAINT.REBUILD_IDENTITY",
                None,
                None,
                Vec::new(),
                "stored target constraints or affinities differ from an independent rebuild",
            ));
        }
        Ok(())
    }

    fn compute(
        expanded: &ExpandedAllocationProblem,
        cfg: &NormalizedCfg,
        graph: &HomeGraph,
        registers: &[PhysReg],
    ) -> Result<Self, AllocationConstraintError> {
        let register_set = registers.iter().copied().collect::<BTreeSet<_>>();
        if register_set.len() != registers.len() || registers.is_empty() {
            return Err(AllocationConstraintError::new(
                "ALLOCATION_CONSTRAINT.REGISTER_SET",
                None,
                None,
                Vec::new(),
                "target register set is empty or contains duplicates",
            ));
        }
        if expanded.intervals.intervals.len() != expanded.ir.value_count() as usize
            || expanded.intervals.block_slots.len() != cfg.successors.len()
        {
            return Err(AllocationConstraintError::new(
                "ALLOCATION_CONSTRAINT.INTERVAL_SHAPE",
                None,
                None,
                Vec::new(),
                "allocation intervals do not cover the value or CFG domain",
            ));
        }
        let facts = expanded
            .ir
            .machine_facts(graph, expanded.shift_encoding)
            .map_err(|error| {
                AllocationConstraintError::new(
                    error.rule,
                    error.block,
                    error.instruction,
                    error.values,
                    error.message,
                )
            })?;
        let target = RegisterMask::from_registers(registers);
        let mut allowed = vec![target; expanded.ir.value_count() as usize];
        let mut clobbers_by_block = BTreeMap::<BlockId, Vec<ClobberPoint>>::new();
        for instruction in &facts.instructions {
            let block = cfg
                .block_index
                .get(&instruction.block)
                .copied()
                .ok_or_else(|| {
                    AllocationConstraintError::new(
                        "ALLOCATION_CONSTRAINT.INSTRUCTION_BLOCK",
                        Some(instruction.block),
                        Some(instruction.instruction),
                        Vec::new(),
                        "constrained instruction is outside the normalized CFG",
                    )
                })?;
            let slots = expanded.intervals.block_slots[block];
            let use_slot = slots
                .instruction_use(instruction.instruction)
                .ok_or_else(|| {
                    AllocationConstraintError::new(
                        "ALLOCATION_CONSTRAINT.INSTRUCTION_SLOT",
                        Some(instruction.block),
                        Some(instruction.instruction),
                        Vec::new(),
                        "constrained instruction has no allocation use slot",
                    )
                })?;
            let def_slot = slots
                .instruction_def(instruction.instruction)
                .ok_or_else(|| {
                    AllocationConstraintError::new(
                        "ALLOCATION_CONSTRAINT.INSTRUCTION_SLOT",
                        Some(instruction.block),
                        Some(instruction.instruction),
                        Vec::new(),
                        "constrained instruction has no allocation definition slot",
                    )
                })?;
            for &(value, required) in &instruction.fixed_uses {
                let mask = allowed.get_mut(value.0 as usize).ok_or_else(|| {
                    AllocationConstraintError::new(
                        "ALLOCATION_CONSTRAINT.FIXED_VALUE",
                        Some(instruction.block),
                        Some(instruction.instruction),
                        vec![value],
                        "fixed operand is outside the allocation value domain",
                    )
                })?;
                mask.intersect(if register_set.contains(&required) {
                    RegisterMask(register_bit(required))
                } else {
                    RegisterMask::empty()
                });
                if mask.is_empty() {
                    return Err(AllocationConstraintError::new(
                        "ALLOCATION_CONSTRAINT.FIXED_MASK",
                        Some(instruction.block),
                        Some(instruction.instruction),
                        vec![value],
                        format!("fixed operand has no common target color including {required}"),
                    ));
                }
            }
            if !instruction.clobbers.is_empty() {
                let registers = instruction
                    .clobbers
                    .iter()
                    .copied()
                    .filter(|register| register_set.contains(register))
                    .collect::<Vec<_>>();
                if !registers.is_empty() {
                    clobbers_by_block
                        .entry(instruction.block)
                        .or_default()
                        .push(ClobberPoint {
                            use_slot,
                            def_slot,
                            block: instruction.block,
                            instruction: instruction.instruction,
                            registers,
                        });
                }
            }
        }
        for points in clobbers_by_block.values_mut() {
            points.sort_unstable_by_key(|point| (point.use_slot, point.instruction));
        }
        for interval in expanded.intervals.intervals.iter().flatten() {
            apply_clobber_constraints(interval, &clobbers_by_block, &mut allowed)?;
            if allowed[interval.value.0 as usize].is_empty() {
                return Err(AllocationConstraintError::new(
                    "ALLOCATION_CONSTRAINT.EMPTY_MASK",
                    Some(interval.definition.block()),
                    None,
                    vec![interval.value],
                    "machine value has no physical register satisfying every split-range constraint",
                ));
            }
        }

        let mut affinity_weights = BTreeMap::<(VReg, VReg), u32>::new();
        for affinity in facts.affinities {
            if expanded
                .intervals
                .intervals
                .get(affinity.left.0 as usize)
                .is_none_or(Option::is_none)
                || expanded
                    .intervals
                    .intervals
                    .get(affinity.right.0 as usize)
                    .is_none_or(Option::is_none)
            {
                continue;
            }
            let weight = match affinity.kind {
                AllocationAffinityKind::Copy => 2,
                AllocationAffinityKind::Phi => 1,
            };
            let entry = affinity_weights
                .entry((affinity.left, affinity.right))
                .or_default();
            *entry = entry.checked_add(weight).ok_or_else(|| {
                AllocationConstraintError::new(
                    "ALLOCATION_CONSTRAINT.AFFINITY_WEIGHT",
                    None,
                    None,
                    vec![affinity.left, affinity.right],
                    "copy/phi affinity weight exceeds u32",
                )
            })?;
        }
        let affinities = affinity_weights
            .into_iter()
            .map(|((left, right), weight)| WeightedAffinity {
                left,
                right,
                weight,
            })
            .collect();
        Ok(Self {
            value_count: expanded.ir.value_count(),
            allowed,
            affinities,
        })
    }
}

impl IncrementalConstraintModel {
    pub(super) fn build(
        expanded: &ExpandedAllocationProblem,
        cfg: &NormalizedCfg,
        graph: &HomeGraph,
        registers: &[PhysReg],
    ) -> Result<Self, AllocationConstraintError> {
        let model = AllocationConstraintModel::build(expanded, cfg, graph, registers)?;
        let block_ids = dense_block_ids(cfg)?;
        let mut block_facts = Vec::with_capacity(block_ids.len());
        for block in block_ids {
            block_facts.push(
                expanded
                    .ir
                    .machine_facts_for_block(block, graph, expanded.shift_encoding)
                    .map_err(machine_fact_error)?,
            );
        }
        let mut result = Self {
            registers: registers.to_vec(),
            block_facts,
            fixed_by_value: vec![Vec::new(); expanded.ir.value_count() as usize],
            affinity_counts: BTreeMap::new(),
            model,
        };
        for facts in result.block_facts.clone() {
            result.add_facts(&facts)?;
        }
        let rebuilt_affinities = weighted_affinities(&result.affinity_counts, &expanded.intervals)?;
        if result.model.affinities != rebuilt_affinities {
            return Err(AllocationConstraintError::new(
                "ALLOCATION_CONSTRAINT.BLOCK_FACT_IDENTITY",
                None,
                None,
                Vec::new(),
                "block-indexed affinities differ from the complete machine-fact rebuild",
            ));
        }
        Ok(result)
    }

    pub(super) fn model(&self) -> &AllocationConstraintModel {
        &self.model
    }

    pub(super) fn update(
        &mut self,
        expanded: &ExpandedAllocationProblem,
        cfg: &NormalizedCfg,
        graph: &HomeGraph,
        changed_blocks: &BTreeSet<BlockId>,
        changed_values: &[VReg],
    ) -> Result<Vec<VReg>, AllocationConstraintError> {
        if self.registers.is_empty()
            || self.block_facts.len() != cfg.successors.len()
            || expanded.ir.value_count() < self.model.value_count
        {
            return Err(AllocationConstraintError::new(
                "ALLOCATION_CONSTRAINT.SESSION_SHAPE",
                None,
                None,
                Vec::new(),
                "incremental constraint state is outside its stable allocation session",
            ));
        }
        let target = RegisterMask::from_registers(&self.registers);
        let value_count = expanded.ir.value_count() as usize;
        self.fixed_by_value.resize_with(value_count, Vec::new);
        self.model.allowed.resize(value_count, target);
        self.model.value_count = expanded.ir.value_count();

        let mut affected = changed_values.iter().copied().collect::<BTreeSet<_>>();
        for block in changed_blocks {
            let row = cfg.block_index.get(block).copied().ok_or_else(|| {
                AllocationConstraintError::new(
                    "ALLOCATION_CONSTRAINT.SESSION_BLOCK",
                    Some(*block),
                    None,
                    Vec::new(),
                    "changed constraint block is outside the normalized CFG",
                )
            })?;
            let old = self.block_facts[row].clone();
            affected.extend(fact_values(&old));
            self.remove_facts(&old)?;
            let next = expanded
                .ir
                .machine_facts_for_block(*block, graph, expanded.shift_encoding)
                .map_err(machine_fact_error)?;
            affected.extend(fact_values(&next));
            self.add_facts(&next)?;
            self.block_facts[row] = next;
        }

        let mut changed = Vec::new();
        for value in affected {
            let index = value.0 as usize;
            if index >= value_count {
                return Err(AllocationConstraintError::new(
                    "ALLOCATION_CONSTRAINT.VALUE_RANGE",
                    None,
                    None,
                    vec![value],
                    "changed target fact references a value outside the allocation session",
                ));
            }
            let next = self.allowed_for_value(expanded, cfg, value)?;
            if self.model.allowed[index] != next {
                changed.push(value);
                self.model.allowed[index] = next;
            }
        }
        self.model.affinities = weighted_affinities(&self.affinity_counts, &expanded.intervals)?;
        Ok(changed)
    }

    fn add_facts(
        &mut self,
        facts: &AllocationMachineFacts,
    ) -> Result<(), AllocationConstraintError> {
        for instruction in &facts.instructions {
            for &(value, register) in &instruction.fixed_uses {
                let row = self
                    .fixed_by_value
                    .get_mut(value.0 as usize)
                    .ok_or_else(|| {
                        AllocationConstraintError::new(
                            "ALLOCATION_CONSTRAINT.FIXED_VALUE",
                            Some(instruction.block),
                            Some(instruction.instruction),
                            vec![value],
                            "fixed operand is outside the incremental value domain",
                        )
                    })?;
                row.push(FixedConstraintPoint {
                    block: instruction.block,
                    instruction: instruction.instruction,
                    register,
                });
            }
        }
        for &affinity in &facts.affinities {
            let count = self.affinity_counts.entry(affinity).or_default();
            *count = count.checked_add(1).ok_or_else(|| {
                AllocationConstraintError::new(
                    "ALLOCATION_CONSTRAINT.AFFINITY_COUNT",
                    None,
                    None,
                    vec![affinity.left, affinity.right],
                    "block affinity reference count exceeds u32",
                )
            })?;
        }
        Ok(())
    }

    fn remove_facts(
        &mut self,
        facts: &AllocationMachineFacts,
    ) -> Result<(), AllocationConstraintError> {
        for instruction in &facts.instructions {
            for &(value, register) in &instruction.fixed_uses {
                let point = FixedConstraintPoint {
                    block: instruction.block,
                    instruction: instruction.instruction,
                    register,
                };
                let row = self
                    .fixed_by_value
                    .get_mut(value.0 as usize)
                    .ok_or_else(|| {
                        AllocationConstraintError::new(
                            "ALLOCATION_CONSTRAINT.FIXED_VALUE",
                            Some(instruction.block),
                            Some(instruction.instruction),
                            vec![value],
                            "removed fixed operand is outside the incremental value domain",
                        )
                    })?;
                let position = row
                    .iter()
                    .position(|candidate| *candidate == point)
                    .ok_or_else(|| {
                        AllocationConstraintError::new(
                            "ALLOCATION_CONSTRAINT.FIXED_IDENTITY",
                            Some(instruction.block),
                            Some(instruction.instruction),
                            vec![value],
                            "cached fixed operand is absent from its value index",
                        )
                    })?;
                row.swap_remove(position);
            }
        }
        for &affinity in &facts.affinities {
            let count = self.affinity_counts.get_mut(&affinity).ok_or_else(|| {
                AllocationConstraintError::new(
                    "ALLOCATION_CONSTRAINT.AFFINITY_IDENTITY",
                    None,
                    None,
                    vec![affinity.left, affinity.right],
                    "cached block affinity is absent from the global index",
                )
            })?;
            *count -= 1;
            if *count == 0 {
                self.affinity_counts.remove(&affinity);
            }
        }
        Ok(())
    }

    fn allowed_for_value(
        &self,
        expanded: &ExpandedAllocationProblem,
        cfg: &NormalizedCfg,
        value: VReg,
    ) -> Result<RegisterMask, AllocationConstraintError> {
        let mut allowed = RegisterMask::from_registers(&self.registers);
        for point in &self.fixed_by_value[value.0 as usize] {
            allowed.intersect(if self.registers.contains(&point.register) {
                RegisterMask(register_bit(point.register))
            } else {
                RegisterMask::empty()
            });
            if allowed.is_empty() {
                return Err(AllocationConstraintError::new(
                    "ALLOCATION_CONSTRAINT.FIXED_MASK",
                    Some(point.block),
                    Some(point.instruction),
                    vec![value],
                    format!(
                        "fixed operand has no common target color including {}",
                        point.register
                    ),
                ));
            }
        }
        let Some(interval) = expanded
            .intervals
            .intervals
            .get(value.0 as usize)
            .and_then(Option::as_ref)
        else {
            return Ok(allowed);
        };
        for segment in &interval.segments {
            let block = cfg
                .block_index
                .get(&segment.block)
                .copied()
                .ok_or_else(|| {
                    AllocationConstraintError::new(
                        "ALLOCATION_CONSTRAINT.SEGMENT_BLOCK",
                        Some(segment.block),
                        None,
                        vec![value],
                        "live segment is outside the block constraint index",
                    )
                })?;
            let slots = expanded.intervals.block_slots[block];
            for point in self.block_facts[block]
                .instructions
                .iter()
                .filter(|point| !point.clobbers.is_empty())
            {
                let use_slot = slots.instruction_use(point.instruction).ok_or_else(|| {
                    AllocationConstraintError::new(
                        "ALLOCATION_CONSTRAINT.INSTRUCTION_SLOT",
                        Some(point.block),
                        Some(point.instruction),
                        vec![value],
                        "incremental clobber has no allocation use slot",
                    )
                })?;
                let def_slot = slots.instruction_def(point.instruction).ok_or_else(|| {
                    AllocationConstraintError::new(
                        "ALLOCATION_CONSTRAINT.INSTRUCTION_SLOT",
                        Some(point.block),
                        Some(point.instruction),
                        vec![value],
                        "incremental clobber has no allocation definition slot",
                    )
                })?;
                if segment.contains(use_slot) && segment.contains(def_slot) {
                    allowed.remove(&point.clobbers);
                    if allowed.is_empty() {
                        return Err(AllocationConstraintError::new(
                            "ALLOCATION_CONSTRAINT.CLOBBER_MASK",
                            Some(point.block),
                            Some(point.instruction),
                            vec![value],
                            "value live through a target clobber has no remaining register",
                        ));
                    }
                }
            }
        }
        Ok(allowed)
    }
}

fn dense_block_ids(cfg: &NormalizedCfg) -> Result<Vec<BlockId>, AllocationConstraintError> {
    let mut result = vec![None; cfg.successors.len()];
    for (&block, &row) in &cfg.block_index {
        if row >= result.len() || result[row].replace(block).is_some() {
            return Err(AllocationConstraintError::new(
                "ALLOCATION_CONSTRAINT.BLOCK_INDEX",
                Some(block),
                None,
                Vec::new(),
                "normalized CFG block index is not a dense bijection",
            ));
        }
    }
    result
        .into_iter()
        .enumerate()
        .map(|(row, block)| {
            block.ok_or_else(|| {
                AllocationConstraintError::new(
                    "ALLOCATION_CONSTRAINT.BLOCK_INDEX",
                    None,
                    None,
                    Vec::new(),
                    format!("normalized CFG has no block identity for row {row}"),
                )
            })
        })
        .collect()
}

fn machine_fact_error(error: super::allocation_ir::AllocationIrError) -> AllocationConstraintError {
    AllocationConstraintError::new(
        error.rule,
        error.block,
        error.instruction,
        error.values,
        error.message,
    )
}

fn fact_values(facts: &AllocationMachineFacts) -> impl Iterator<Item = VReg> + '_ {
    facts
        .instructions
        .iter()
        .flat_map(|instruction| instruction.fixed_uses.iter().map(|(value, _)| *value))
        .chain(
            facts
                .affinities
                .iter()
                .flat_map(|affinity| [affinity.left, affinity.right]),
        )
}

fn weighted_affinities(
    counts: &BTreeMap<AllocationAffinity, u32>,
    intervals: &super::live_interval::LiveIntervals,
) -> Result<Vec<WeightedAffinity>, AllocationConstraintError> {
    let mut weights = BTreeMap::<(VReg, VReg), u32>::new();
    for (affinity, count) in counts {
        if *count == 0 {
            continue;
        }
        if intervals
            .intervals
            .get(affinity.left.0 as usize)
            .is_none_or(Option::is_none)
            || intervals
                .intervals
                .get(affinity.right.0 as usize)
                .is_none_or(Option::is_none)
        {
            continue;
        }
        let weight = match affinity.kind {
            AllocationAffinityKind::Copy => 2,
            AllocationAffinityKind::Phi => 1,
        };
        let entry = weights.entry((affinity.left, affinity.right)).or_default();
        *entry = entry.checked_add(weight).ok_or_else(|| {
            AllocationConstraintError::new(
                "ALLOCATION_CONSTRAINT.AFFINITY_WEIGHT",
                None,
                None,
                vec![affinity.left, affinity.right],
                "copy/phi affinity weight exceeds u32",
            )
        })?;
    }
    Ok(weights
        .into_iter()
        .map(|((left, right), weight)| WeightedAffinity {
            left,
            right,
            weight,
        })
        .collect())
}

fn apply_clobber_constraints(
    interval: &LiveInterval,
    clobbers_by_block: &BTreeMap<BlockId, Vec<ClobberPoint>>,
    allowed: &mut [RegisterMask],
) -> Result<(), AllocationConstraintError> {
    let mask = allowed.get_mut(interval.value.0 as usize).ok_or_else(|| {
        AllocationConstraintError::new(
            "ALLOCATION_CONSTRAINT.VALUE_RANGE",
            Some(interval.definition.block()),
            None,
            vec![interval.value],
            "live interval is outside the target-constraint table",
        )
    })?;
    for segment in &interval.segments {
        let Some(points) = clobbers_by_block.get(&segment.block) else {
            continue;
        };
        let first = points.partition_point(|point| point.use_slot < segment.start);
        for point in &points[first..] {
            if point.use_slot >= segment.end {
                break;
            }
            if segment.contains(point.use_slot) && segment.contains(point.def_slot) {
                mask.remove(&point.registers);
                if mask.is_empty() {
                    return Err(AllocationConstraintError::new(
                        "ALLOCATION_CONSTRAINT.CLOBBER_MASK",
                        Some(point.block),
                        Some(point.instruction),
                        vec![interval.value],
                        "value live through a target clobber has no remaining register",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn register_bit(register: PhysReg) -> u16 {
    1u16 << register as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::features::X86Features;
    use crate::backend::native::mir::{
        BaseReg, MBlock, MFunction, MInst, OpSize, SpillDesc, VRegAllocator,
    };

    use super::super::allocation_expand::expand;
    use super::super::allocation_ir::{StackHomeId, SyntheticOperation};
    use super::super::allocation_reallocate::{JointAllocationOutcome, JointAllocationProblem};
    use super::super::assignment::ALLOCATABLE_REGS;
    use super::super::home_graph;
    use super::super::interval_allocator::allocate_roots;
    use super::super::legalize::materialize_allocation_constraint_perms;

    fn expanded(
        mut function: MFunction,
    ) -> (
        MFunction,
        NormalizedCfg,
        HomeGraph,
        ExpandedAllocationProblem,
    ) {
        let initial = super::super::cfg::normalize(&mut function).unwrap();
        let (cfg, _) = materialize_allocation_constraint_perms(&mut function, &initial).unwrap();
        let graph = home_graph::build(&function, &cfg).unwrap();
        let plan = allocate_roots(&graph, &cfg, ALLOCATABLE_REGS).unwrap();
        let problem = expand(&function, &cfg, &graph, &plan, ALLOCATABLE_REGS).unwrap();
        (function, cfg, graph, problem)
    }

    #[test]
    fn fixed_use_and_clobber_masks_apply_to_split_machine_ranges() {
        let mut values = VRegAllocator::new();
        let lhs = values.alloc();
        let amount = values.alloc();
        let shifted = values.alloc();
        let live_through = values.alloc();
        let quotient = values.alloc();
        let observed = values.alloc();
        let mut function = MFunction::new(values, vec![SpillDesc::transient(); 6]);
        function.target_features = X86Features::for_test(false);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm { dst: lhs, value: 8 });
        block.push(MInst::LoadImm {
            dst: amount,
            value: 1,
        });
        block.push(MInst::Shl {
            dst: shifted,
            lhs,
            rhs: amount,
        });
        block.push(MInst::LoadImm {
            dst: live_through,
            value: 9,
        });
        block.push(MInst::UDiv {
            dst: quotient,
            lhs: shifted,
            rhs: amount,
        });
        block.push(MInst::Add {
            dst: observed,
            lhs: quotient,
            rhs: live_through,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: observed,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        function.push_block(block);

        let (function, cfg, graph, expanded) = expanded(function);
        let model =
            AllocationConstraintModel::build(&expanded, &cfg, &graph, ALLOCATABLE_REGS).unwrap();
        let shift = function
            .blocks
            .iter()
            .find_map(|block| {
                block
                    .insts
                    .iter()
                    .find_map(|instruction| match instruction {
                        MInst::Shl { rhs, .. } => Some(*rhs),
                        _ => None,
                    })
            })
            .unwrap();
        assert_eq!(model.allowed(shift).unwrap().count(), 1);
        assert!(model.allowed(shift).unwrap().contains(PhysReg::RCX));

        let division_block = function
            .blocks
            .iter()
            .find(|block| {
                block
                    .insts
                    .iter()
                    .any(|inst| matches!(inst, MInst::UDiv { .. }))
            })
            .unwrap();
        let live_through = match division_block.insts.last() {
            Some(MInst::Return) => division_block
                .insts
                .iter()
                .find_map(|inst| match inst {
                    MInst::Add { rhs, .. } => Some(*rhs),
                    _ => None,
                })
                .unwrap(),
            _ => unreachable!(),
        };
        let mask = model.allowed(live_through).unwrap();
        assert!(!mask.contains(PhysReg::RAX));
        assert!(!mask.contains(PhysReg::RDX));
        let joint =
            JointAllocationProblem::build(&expanded, &cfg, &graph, ALLOCATABLE_REGS).unwrap();
        let JointAllocationOutcome::Complete(allocation) =
            joint.allocate(&cfg, ALLOCATABLE_REGS).unwrap()
        else {
            panic!("constraint-split fixture must fit without another home split");
        };
        assert_eq!(allocation.assignments[shift.0 as usize], Some(PhysReg::RCX));
        assert!(!matches!(
            allocation.assignments[live_through.0 as usize],
            Some(PhysReg::RAX | PhysReg::RDX)
        ));
    }

    #[test]
    fn mov_and_phi_edges_become_weighted_affinities() {
        let mut values = VRegAllocator::new();
        let source = values.alloc();
        let copied = values.alloc();
        let condition = values.alloc();
        let merged = values.alloc();
        let mut function = MFunction::new(values, vec![SpillDesc::transient(); 4]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: source,
            value: 7,
        });
        entry.push(MInst::Mov {
            dst: copied,
            src: source,
        });
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Jump { target: BlockId(3) });
        let mut join = MBlock::new(BlockId(3));
        join.phis.push(crate::backend::native::mir::PhiNode {
            dst: merged,
            sources: vec![(BlockId(1), copied), (BlockId(2), source)],
        });
        join.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: merged,
            size: OpSize::S64,
        });
        join.push(MInst::Return);
        function.blocks = vec![entry, left, right, join];

        let (_function, cfg, graph, expanded) = expanded(function);
        let model =
            AllocationConstraintModel::build(&expanded, &cfg, &graph, ALLOCATABLE_REGS).unwrap();
        assert!(model.affinities.iter().any(|affinity| {
            affinity.left == source && affinity.right == copied && affinity.weight >= 2
        }));
        assert!(model.affinities.iter().any(|affinity| {
            affinity.right == merged
                && matches!(affinity.left, value if value == source || value == copied)
        }));
        let joint =
            JointAllocationProblem::build(&expanded, &cfg, &graph, ALLOCATABLE_REGS).unwrap();
        let JointAllocationOutcome::Complete(allocation) =
            joint.allocate(&cfg, ALLOCATABLE_REGS).unwrap()
        else {
            panic!("copy/phi affinity fixture must fit without splitting");
        };
        let merged_register = allocation.assignments[merged.0 as usize];
        assert!(
            merged_register == allocation.assignments[source.0 as usize]
                || merged_register == allocation.assignments[copied.0 as usize],
            "the phi destination should coalesce with one non-interfering edge source"
        );
    }

    #[test]
    fn incremental_constraints_match_a_full_rebuild_after_local_slot_shift() {
        let mut values = VRegAllocator::new();
        let lhs = values.alloc();
        let rhs = values.alloc();
        let quotient = values.alloc();
        let live_through = values.alloc();
        let observed = values.alloc();
        let mut function = MFunction::new(values, vec![SpillDesc::transient(); 5]);
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm { dst: lhs, value: 8 });
        block.push(MInst::LoadImm { dst: rhs, value: 2 });
        block.push(MInst::LoadImm {
            dst: live_through,
            value: 9,
        });
        block.push(MInst::UDiv {
            dst: quotient,
            lhs,
            rhs,
        });
        block.push(MInst::Add {
            dst: observed,
            lhs: quotient,
            rhs: live_through,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: observed,
            size: OpSize::S64,
        });
        block.push(MInst::Return);
        function.push_block(block);

        let (_function, cfg, graph, mut expanded) = expanded(function);
        let mut incremental =
            IncrementalConstraintModel::build(&expanded, &cfg, &graph, ALLOCATABLE_REGS).unwrap();
        let site = expanded.intervals.intervals[observed.0 as usize]
            .as_ref()
            .unwrap()
            .uses[0];
        expanded
            .ir
            .insert_before_use(
                site,
                SyntheticOperation::StackReload {
                    home: StackHomeId(0),
                },
                crate::backend::native::mir::Uses::none(),
                true,
            )
            .unwrap();
        let changed_blocks = BTreeSet::from([site.block()]);
        let changed_values = expanded
            .incremental_liveness
            .update(&expanded.ir, &cfg, &mut expanded.intervals, &changed_blocks)
            .unwrap();
        incremental
            .update(&expanded, &cfg, &graph, &changed_blocks, &changed_values)
            .unwrap();

        assert_eq!(
            incremental.model(),
            &AllocationConstraintModel::build(&expanded, &cfg, &graph, ALLOCATABLE_REGS).unwrap()
        );
    }

    #[test]
    fn incremental_constraints_rebuild_the_phi_successor_affinity_row() {
        let mut values = VRegAllocator::new();
        let source = values.alloc();
        let copied = values.alloc();
        let condition = values.alloc();
        let merged = values.alloc();
        let mut function = MFunction::new(values, vec![SpillDesc::transient(); 4]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: source,
            value: 7,
        });
        entry.push(MInst::Mov {
            dst: copied,
            src: source,
        });
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Jump { target: BlockId(3) });
        let mut join = MBlock::new(BlockId(3));
        join.phis.push(crate::backend::native::mir::PhiNode {
            dst: merged,
            sources: vec![(BlockId(1), copied), (BlockId(2), source)],
        });
        join.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: merged,
            size: OpSize::S64,
        });
        join.push(MInst::Return);
        function.blocks = vec![entry, left, right, join];

        let (_function, cfg, graph, mut expanded) = expanded(function);
        let mut incremental =
            IncrementalConstraintModel::build(&expanded, &cfg, &graph, ALLOCATABLE_REGS).unwrap();
        let site = expanded.intervals.intervals[source.0 as usize]
            .as_ref()
            .unwrap()
            .uses
            .iter()
            .copied()
            .find(|site| {
                matches!(
                    site,
                    super::super::live_interval::UseSite::PhiEdge {
                        predecessor: BlockId(2),
                        successor: BlockId(3),
                        ..
                    }
                )
            })
            .unwrap();
        expanded.ir.rewrite_use(site, source, copied).unwrap();
        let liveness_blocks = BTreeSet::from([BlockId(2)]);
        let changed_values = expanded
            .incremental_liveness
            .update(
                &expanded.ir,
                &cfg,
                &mut expanded.intervals,
                &liveness_blocks,
            )
            .unwrap();
        incremental
            .update(
                &expanded,
                &cfg,
                &graph,
                &BTreeSet::from([BlockId(3)]),
                &changed_values,
            )
            .unwrap();

        assert_eq!(
            incremental.model(),
            &AllocationConstraintModel::build(&expanded, &cfg, &graph, ALLOCATABLE_REGS).unwrap()
        );
    }
}
