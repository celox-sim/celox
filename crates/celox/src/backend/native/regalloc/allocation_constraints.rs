//! Target-register constraints and copy affinities for allocation IR.
//!
//! Local SSA fragments carry fixed-operand requirements. Target clobbers are
//! instead immutable physical-register intervals over exact instruction
//! barriers; they never remove a color from an entire VReg. This model rebuilds
//! both from allocation IR, so synthetic reloads and recipes cannot inherit
//! stale source-MIR constraints. Copy and phi affinities remain hints.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::backend::native::mir::{BlockId, VReg};

use super::allocation_expand::ExpandedAllocationProblem;
use super::allocation_ir::{AllocationAffinity, AllocationAffinityKind, AllocationMachineFacts};
use super::assignment::PhysReg;
use super::cfg::NormalizedCfg;
use super::home_graph::HomeGraph;
use super::interval_union::FixedRegisterReservation;
use super::live_interval::LiveSegment;

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
    pub fixed_reservations: Vec<FixedRegisterReservation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FixedConstraintPoint {
    block: BlockId,
    instruction: usize,
    register: PhysReg,
}

/// Persistent target-constraint facts for one allocation session.
///
/// Synthetic insertion can shift every fixed interval in one block, while a
/// rewritten fixed operand or copy changes only that block's semantic facts.
/// The index replaces those rows and recomputes fixed-use masks only for values
/// whose local facts changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct IncrementalConstraintModel {
    registers: Vec<PhysReg>,
    block_facts: Vec<AllocationMachineFacts>,
    reservations_by_block: Vec<Vec<FixedRegisterReservation>>,
    fixed_by_value: Vec<Vec<FixedConstraintPoint>>,
    active_values: Vec<bool>,
    affinity_counts: BTreeMap<AllocationAffinity, u32>,
    /// Sparse bidirectional index over unique affinity kinds. Most RTL values
    /// have no copy/phi edge, so a dense Vec per VReg would dominate memory.
    affinities_by_value: BTreeMap<VReg, Vec<AllocationAffinity>>,
    /// Published active endpoint weights, maintained directly from block-fact
    /// and liveness deltas instead of rescanning every affinity each round.
    affinity_weights: BTreeMap<(VReg, VReg), u32>,
    model: AllocationConstraintModel,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct IncrementalConstraintUpdate {
    pub changed_values: Vec<VReg>,
    pub affinities_changed: bool,
    pub fixed_reservations_changed: bool,
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
        let mut fixed_reservations = Vec::new();
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
            let slots = &expanded.intervals.block_slots[block];
            let clobber_slot = slots
                .instruction_clobber(instruction.instruction)
                .ok_or_else(|| {
                    AllocationConstraintError::new(
                        "ALLOCATION_CONSTRAINT.INSTRUCTION_SLOT",
                        Some(instruction.block),
                        Some(instruction.instruction),
                        Vec::new(),
                        "constrained instruction has no allocation clobber slot",
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
                for register in instruction
                    .clobbers
                    .iter()
                    .copied()
                    .filter(|register| register_set.contains(register))
                {
                    fixed_reservations.push(FixedRegisterReservation {
                        register,
                        segment: LiveSegment {
                            block: instruction.block,
                            start: clobber_slot,
                            end: def_slot,
                        },
                    });
                }
            }
        }
        canonicalize_reservations(&mut fixed_reservations)?;

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
            fixed_reservations,
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
        for &block in &block_ids {
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
            reservations_by_block: vec![Vec::new(); cfg.successors.len()],
            fixed_by_value: vec![Vec::new(); expanded.ir.value_count() as usize],
            active_values: expanded
                .intervals
                .intervals
                .iter()
                .map(Option::is_some)
                .collect(),
            affinity_counts: BTreeMap::new(),
            affinities_by_value: BTreeMap::new(),
            affinity_weights: BTreeMap::new(),
            model,
        };
        for (block, &block_id) in block_ids.iter().enumerate() {
            let facts = result.block_facts[block].clone();
            result.add_facts(&facts)?;
            result.reservations_by_block[block] = reservations_for_block(
                block_id,
                &facts,
                &expanded.intervals.block_slots[block],
                &result.registers,
            )?;
        }
        let rebuilt_affinities = weighted_affinities(&result.affinity_counts, &expanded.intervals)?;
        if result.model.affinities != rebuilt_affinities
            || result.model.affinities != result.published_affinities()
        {
            return Err(AllocationConstraintError::new(
                "ALLOCATION_CONSTRAINT.BLOCK_FACT_IDENTITY",
                None,
                None,
                Vec::new(),
                "block-indexed affinities differ from the complete machine-fact rebuild",
            ));
        }
        let rebuilt_reservations = flatten_reservations(&result.reservations_by_block)?;
        if result.model.fixed_reservations != rebuilt_reservations {
            return Err(AllocationConstraintError::new(
                "ALLOCATION_CONSTRAINT.BLOCK_RESERVATION_IDENTITY",
                None,
                None,
                Vec::new(),
                "block-indexed fixed reservations differ from the complete machine-fact rebuild",
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
        range_changed_values: &[VReg],
    ) -> Result<IncrementalConstraintUpdate, AllocationConstraintError> {
        if self.registers.is_empty()
            || self.block_facts.len() != cfg.successors.len()
            || self.reservations_by_block.len() != cfg.successors.len()
            || self.active_values.len() != self.model.value_count as usize
            || expanded.intervals.intervals.len() != expanded.ir.value_count() as usize
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
        let previous_value_count = self.active_values.len();
        self.fixed_by_value.resize_with(value_count, Vec::new);
        self.active_values.resize(value_count, false);
        self.model.allowed.resize(value_count, target);
        self.model.value_count = expanded.ir.value_count();

        let mut affinities_changed = false;
        for index in previous_value_count..value_count {
            let value = VReg(index as u32);
            affinities_changed |=
                self.set_value_active(value, expanded.intervals.intervals[index].is_some())?;
        }
        for &value in range_changed_values {
            let index = value.0 as usize;
            let active = expanded
                .intervals
                .intervals
                .get(index)
                .ok_or_else(|| {
                    AllocationConstraintError::new(
                        "ALLOCATION_CONSTRAINT.VALUE_RANGE",
                        None,
                        None,
                        vec![value],
                        "changed live range is outside the allocation session",
                    )
                })?
                .is_some();
            affinities_changed |= self.set_value_active(value, active)?;
        }

        let mut affected = BTreeSet::new();
        let mut fixed_reservations_changed = false;
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
            affected.extend(fixed_fact_values(&old));
            let next = expanded
                .ir
                .machine_facts_for_block(*block, graph, expanded.shift_encoding)
                .map_err(machine_fact_error)?;
            affected.extend(fixed_fact_values(&next));

            self.remove_fixed_facts(&old)?;
            self.add_fixed_facts(&next)?;
            if old.affinities != next.affinities {
                affinities_changed |= self.remove_affinity_facts(&old)?;
                affinities_changed |= self.add_affinity_facts(&next)?;
            }

            let next_reservations = reservations_for_block(
                *block,
                &next,
                &expanded.intervals.block_slots[row],
                &self.registers,
            )?;
            if self.reservations_by_block[row] != next_reservations {
                self.reservations_by_block[row] = next_reservations;
                fixed_reservations_changed = true;
            }
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
            let next = self.allowed_for_value(value)?;
            if self.model.allowed[index] != next {
                changed.push(value);
                self.model.allowed[index] = next;
            }
        }
        if affinities_changed {
            self.model.affinities = self.published_affinities();
        }
        if fixed_reservations_changed {
            self.model.fixed_reservations = flatten_reservations(&self.reservations_by_block)?;
        }
        Ok(IncrementalConstraintUpdate {
            changed_values: changed,
            affinities_changed,
            fixed_reservations_changed,
        })
    }

    fn add_facts(
        &mut self,
        facts: &AllocationMachineFacts,
    ) -> Result<(), AllocationConstraintError> {
        self.add_fixed_facts(facts)?;
        self.add_affinity_facts(facts)?;
        Ok(())
    }

    fn add_fixed_facts(
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
        Ok(())
    }

    fn remove_fixed_facts(
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
        Ok(())
    }

    fn add_affinity_facts(
        &mut self,
        facts: &AllocationMachineFacts,
    ) -> Result<bool, AllocationConstraintError> {
        let mut changed = false;
        for &affinity in &facts.affinities {
            self.validate_affinity_range(affinity)?;
            let previous = self.affinity_counts.get(&affinity).copied().unwrap_or(0);
            let next = previous.checked_add(1).ok_or_else(|| {
                AllocationConstraintError::new(
                    "ALLOCATION_CONSTRAINT.AFFINITY_COUNT",
                    None,
                    None,
                    vec![affinity.left, affinity.right],
                    "block affinity reference count exceeds u32",
                )
            })?;
            if previous == 0 {
                self.insert_affinity_adjacency(affinity)?;
            }
            self.affinity_counts.insert(affinity, next);
            if self.affinity_is_active(affinity)? {
                self.add_affinity_weight(affinity, 1)?;
                changed = true;
            }
        }
        Ok(changed)
    }

    fn remove_affinity_facts(
        &mut self,
        facts: &AllocationMachineFacts,
    ) -> Result<bool, AllocationConstraintError> {
        let mut changed = false;
        for &affinity in &facts.affinities {
            let previous = self
                .affinity_counts
                .get(&affinity)
                .copied()
                .ok_or_else(|| {
                    AllocationConstraintError::new(
                        "ALLOCATION_CONSTRAINT.AFFINITY_IDENTITY",
                        None,
                        None,
                        vec![affinity.left, affinity.right],
                        "cached block affinity is absent from the global index",
                    )
                })?;
            if previous == 0 {
                return Err(AllocationConstraintError::new(
                    "ALLOCATION_CONSTRAINT.AFFINITY_IDENTITY",
                    None,
                    None,
                    vec![affinity.left, affinity.right],
                    "cached block affinity has a zero reference count",
                ));
            }
            if self.affinity_is_active(affinity)? {
                self.remove_affinity_weight(affinity, 1)?;
                changed = true;
            }
            if previous == 1 {
                self.affinity_counts.remove(&affinity);
                self.remove_affinity_adjacency(affinity)?;
            } else {
                self.affinity_counts.insert(affinity, previous - 1);
            }
        }
        Ok(changed)
    }

    fn set_value_active(
        &mut self,
        value: VReg,
        active: bool,
    ) -> Result<bool, AllocationConstraintError> {
        let index = value.0 as usize;
        let previous = *self.active_values.get(index).ok_or_else(|| {
            AllocationConstraintError::new(
                "ALLOCATION_CONSTRAINT.VALUE_RANGE",
                None,
                None,
                vec![value],
                "live-range activation is outside the affinity value domain",
            )
        })?;
        if previous == active {
            return Ok(false);
        }
        let incident = self
            .affinities_by_value
            .get(&value)
            .cloned()
            .unwrap_or_default();
        let mut changed = false;
        for affinity in incident {
            let other = if affinity.left == value {
                affinity.right
            } else if affinity.right == value {
                affinity.left
            } else {
                return Err(AllocationConstraintError::new(
                    "ALLOCATION_CONSTRAINT.AFFINITY_ADJACENCY",
                    None,
                    None,
                    vec![value, affinity.left, affinity.right],
                    "affinity adjacency row contains a non-incident edge",
                ));
            };
            if self
                .active_values
                .get(other.0 as usize)
                .copied()
                .ok_or_else(|| {
                    AllocationConstraintError::new(
                        "ALLOCATION_CONSTRAINT.VALUE_RANGE",
                        None,
                        None,
                        vec![other],
                        "affinity endpoint is outside the active-value table",
                    )
                })?
            {
                let count = self
                    .affinity_counts
                    .get(&affinity)
                    .copied()
                    .ok_or_else(|| {
                        AllocationConstraintError::new(
                            "ALLOCATION_CONSTRAINT.AFFINITY_ADJACENCY",
                            None,
                            None,
                            vec![affinity.left, affinity.right],
                            "affinity adjacency edge has no global reference count",
                        )
                    })?;
                if active {
                    self.add_affinity_weight(affinity, count)?;
                } else {
                    self.remove_affinity_weight(affinity, count)?;
                }
                changed = true;
            }
        }
        self.active_values[index] = active;
        Ok(changed)
    }

    fn validate_affinity_range(
        &self,
        affinity: AllocationAffinity,
    ) -> Result<(), AllocationConstraintError> {
        if affinity.left == affinity.right
            || affinity.left.0 as usize >= self.active_values.len()
            || affinity.right.0 as usize >= self.active_values.len()
        {
            return Err(AllocationConstraintError::new(
                "ALLOCATION_CONSTRAINT.AFFINITY_RANGE",
                None,
                None,
                vec![affinity.left, affinity.right],
                "affinity endpoints are equal or outside the allocation value domain",
            ));
        }
        Ok(())
    }

    fn affinity_is_active(
        &self,
        affinity: AllocationAffinity,
    ) -> Result<bool, AllocationConstraintError> {
        self.validate_affinity_range(affinity)?;
        Ok(self.active_values[affinity.left.0 as usize]
            && self.active_values[affinity.right.0 as usize])
    }

    fn insert_affinity_adjacency(
        &mut self,
        affinity: AllocationAffinity,
    ) -> Result<(), AllocationConstraintError> {
        for value in [affinity.left, affinity.right] {
            let row = self.affinities_by_value.entry(value).or_default();
            match row.binary_search(&affinity) {
                Ok(_) => {
                    return Err(AllocationConstraintError::new(
                        "ALLOCATION_CONSTRAINT.AFFINITY_ADJACENCY",
                        None,
                        None,
                        vec![affinity.left, affinity.right],
                        "new global affinity already exists in an endpoint adjacency row",
                    ));
                }
                Err(position) => row.insert(position, affinity),
            }
        }
        Ok(())
    }

    fn remove_affinity_adjacency(
        &mut self,
        affinity: AllocationAffinity,
    ) -> Result<(), AllocationConstraintError> {
        for value in [affinity.left, affinity.right] {
            let remove_row = {
                let row = self.affinities_by_value.get_mut(&value).ok_or_else(|| {
                    AllocationConstraintError::new(
                        "ALLOCATION_CONSTRAINT.AFFINITY_ADJACENCY",
                        None,
                        None,
                        vec![affinity.left, affinity.right],
                        "removed affinity has no endpoint adjacency row",
                    )
                })?;
                let position = row.binary_search(&affinity).map_err(|_| {
                    AllocationConstraintError::new(
                        "ALLOCATION_CONSTRAINT.AFFINITY_ADJACENCY",
                        None,
                        None,
                        vec![affinity.left, affinity.right],
                        "removed affinity is absent from an endpoint adjacency row",
                    )
                })?;
                row.remove(position);
                row.is_empty()
            };
            if remove_row {
                self.affinities_by_value.remove(&value);
            }
        }
        Ok(())
    }

    fn add_affinity_weight(
        &mut self,
        affinity: AllocationAffinity,
        count: u32,
    ) -> Result<(), AllocationConstraintError> {
        let delta = affinity_weight(affinity.kind)
            .checked_mul(count)
            .ok_or_else(|| affinity_weight_error(affinity))?;
        let weight = self
            .affinity_weights
            .entry((affinity.left, affinity.right))
            .or_default();
        *weight = weight
            .checked_add(delta)
            .ok_or_else(|| affinity_weight_error(affinity))?;
        Ok(())
    }

    fn remove_affinity_weight(
        &mut self,
        affinity: AllocationAffinity,
        count: u32,
    ) -> Result<(), AllocationConstraintError> {
        let delta = affinity_weight(affinity.kind)
            .checked_mul(count)
            .ok_or_else(|| affinity_weight_error(affinity))?;
        let pair = (affinity.left, affinity.right);
        let weight = self.affinity_weights.get_mut(&pair).ok_or_else(|| {
            AllocationConstraintError::new(
                "ALLOCATION_CONSTRAINT.AFFINITY_WEIGHT_IDENTITY",
                None,
                None,
                vec![affinity.left, affinity.right],
                "active affinity has no published pair weight",
            )
        })?;
        *weight = weight.checked_sub(delta).ok_or_else(|| {
            AllocationConstraintError::new(
                "ALLOCATION_CONSTRAINT.AFFINITY_WEIGHT_IDENTITY",
                None,
                None,
                vec![affinity.left, affinity.right],
                "active affinity pair weight is smaller than its removed contribution",
            )
        })?;
        if *weight == 0 {
            self.affinity_weights.remove(&pair);
        }
        Ok(())
    }

    fn published_affinities(&self) -> Vec<WeightedAffinity> {
        self.affinity_weights
            .iter()
            .map(|(&(left, right), &weight)| WeightedAffinity {
                left,
                right,
                weight,
            })
            .collect()
    }

    fn allowed_for_value(&self, value: VReg) -> Result<RegisterMask, AllocationConstraintError> {
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

fn fixed_fact_values(facts: &AllocationMachineFacts) -> impl Iterator<Item = VReg> + '_ {
    facts
        .instructions
        .iter()
        .flat_map(|instruction| instruction.fixed_uses.iter().map(|(value, _)| *value))
}

fn affinity_weight(kind: AllocationAffinityKind) -> u32 {
    match kind {
        AllocationAffinityKind::Copy => 2,
        AllocationAffinityKind::Phi => 1,
    }
}

fn affinity_weight_error(affinity: AllocationAffinity) -> AllocationConstraintError {
    AllocationConstraintError::new(
        "ALLOCATION_CONSTRAINT.AFFINITY_WEIGHT",
        None,
        None,
        vec![affinity.left, affinity.right],
        "copy/phi affinity weight exceeds u32",
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
        let weight = affinity_weight(affinity.kind);
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

fn reservations_for_block(
    block: BlockId,
    facts: &AllocationMachineFacts,
    slots: &super::live_interval::BlockSlots,
    registers: &[PhysReg],
) -> Result<Vec<FixedRegisterReservation>, AllocationConstraintError> {
    let register_set = registers.iter().copied().collect::<BTreeSet<_>>();
    let mut reservations = Vec::new();
    for instruction in &facts.instructions {
        if instruction.block != block {
            return Err(AllocationConstraintError::new(
                "ALLOCATION_CONSTRAINT.BLOCK_FACT_IDENTITY",
                Some(block),
                Some(instruction.instruction),
                Vec::new(),
                "machine-fact instruction belongs to another block",
            ));
        }
        let start = slots
            .instruction_clobber(instruction.instruction)
            .ok_or_else(|| {
                AllocationConstraintError::new(
                    "ALLOCATION_CONSTRAINT.INSTRUCTION_SLOT",
                    Some(instruction.block),
                    Some(instruction.instruction),
                    Vec::new(),
                    "block-indexed clobber has no allocation barrier slot",
                )
            })?;
        let end = slots
            .instruction_def(instruction.instruction)
            .ok_or_else(|| {
                AllocationConstraintError::new(
                    "ALLOCATION_CONSTRAINT.INSTRUCTION_SLOT",
                    Some(instruction.block),
                    Some(instruction.instruction),
                    Vec::new(),
                    "block-indexed clobber has no allocation definition slot",
                )
            })?;
        reservations.extend(
            instruction
                .clobbers
                .iter()
                .copied()
                .filter(|register| register_set.contains(register))
                .map(|register| FixedRegisterReservation {
                    register,
                    segment: LiveSegment {
                        block: instruction.block,
                        start,
                        end,
                    },
                }),
        );
    }
    canonicalize_reservations(&mut reservations)?;
    Ok(reservations)
}

fn flatten_reservations(
    blocks: &[Vec<FixedRegisterReservation>],
) -> Result<Vec<FixedRegisterReservation>, AllocationConstraintError> {
    let mut reservations = blocks.iter().flatten().copied().collect::<Vec<_>>();
    canonicalize_reservations(&mut reservations)?;
    Ok(reservations)
}

fn canonicalize_reservations(
    reservations: &mut [FixedRegisterReservation],
) -> Result<(), AllocationConstraintError> {
    reservations.sort_unstable_by_key(|reservation| {
        (
            reservation.register,
            reservation.segment.block,
            reservation.segment.start,
            reservation.segment.end,
        )
    });
    if let Some(pair) = reservations.windows(2).find(|pair| pair[0] == pair[1]) {
        return Err(AllocationConstraintError::new(
            "ALLOCATION_CONSTRAINT.RESERVATION_IDENTITY",
            Some(pair[0].segment.block),
            None,
            Vec::new(),
            "machine facts contain a duplicate fixed-register reservation",
        ));
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
    use super::super::legalize::materialize_allocation_fixed_use_fragments;

    fn expanded(
        mut function: MFunction,
    ) -> (
        MFunction,
        NormalizedCfg,
        HomeGraph,
        ExpandedAllocationProblem,
    ) {
        let cfg = super::super::cfg::normalize(&mut function).unwrap();
        materialize_allocation_fixed_use_fragments(&mut function).unwrap();
        let graph = home_graph::build(&function, &cfg).unwrap();
        let plan = allocate_roots(&graph, &cfg, ALLOCATABLE_REGS).unwrap();
        let problem = expand(&function, &cfg, &graph, &plan, ALLOCATABLE_REGS).unwrap();
        (function, cfg, graph, problem)
    }

    #[test]
    fn fixed_use_mask_and_exact_clobber_reservations_are_separate() {
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
        assert!(mask.contains(PhysReg::RAX));
        assert!(mask.contains(PhysReg::RDX));
        let division = division_block
            .insts
            .iter()
            .position(|instruction| matches!(instruction, MInst::UDiv { .. }))
            .unwrap();
        let block = cfg.block_index[&division_block.id];
        let slots = &expanded.intervals.block_slots[block];
        assert_eq!(
            model.fixed_reservations,
            vec![
                FixedRegisterReservation {
                    register: PhysReg::RAX,
                    segment: LiveSegment {
                        block: division_block.id,
                        start: slots.instruction_clobber(division).unwrap(),
                        end: slots.instruction_def(division).unwrap(),
                    },
                },
                FixedRegisterReservation {
                    register: PhysReg::RDX,
                    segment: LiveSegment {
                        block: division_block.id,
                        start: slots.instruction_clobber(division).unwrap(),
                        end: slots.instruction_def(division).unwrap(),
                    },
                },
            ]
        );
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
    fn loop_snapshot_descriptor_does_not_enter_greedy_affinities() {
        let mut values = VRegAllocator::new();
        let initial = values.alloc();
        let condition = values.alloc();
        let intermediate = values.alloc();
        let snapshot = values.alloc();
        let merged = values.alloc();
        let mut function = MFunction::new(values, vec![SpillDesc::transient(); 5]);

        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: initial,
            value: 0,
        });
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::Jump { target: BlockId(1) });

        let mut header = MBlock::new(BlockId(1));
        header.phis.push(crate::backend::native::mir::PhiNode {
            dst: merged,
            sources: vec![(BlockId(0), initial), (BlockId(2), snapshot)],
        });
        header.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(2),
            false_bb: BlockId(3),
        });

        let mut backedge = MBlock::new(BlockId(2));
        backedge.push(MInst::LoadImm {
            dst: intermediate,
            value: 4,
        });
        backedge.push(MInst::Mov {
            dst: snapshot,
            src: intermediate,
        });
        backedge.push(MInst::Jump { target: BlockId(1) });

        let mut exit = MBlock::new(BlockId(3));
        exit.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: merged,
            size: OpSize::S64,
        });
        exit.push(MInst::Return);
        function.blocks = vec![entry, header, backedge, exit];

        let (_function, cfg, graph, expanded) = expanded(function);
        assert!(expanded.loop_backedge_affinities.iter().any(|affinity| {
            affinity.header == BlockId(1)
                && affinity.source == intermediate
                && affinity.snapshot == snapshot
                && affinity.destination == merged
        }));
        let model =
            AllocationConstraintModel::build(&expanded, &cfg, &graph, ALLOCATABLE_REGS).unwrap();
        assert!(model.affinities.iter().all(|affinity| {
            (affinity.left, affinity.right) != (intermediate.min(merged), intermediate.max(merged))
        }));
        let incremental =
            IncrementalConstraintModel::build(&expanded, &cfg, &graph, ALLOCATABLE_REGS).unwrap();
        assert_eq!(incremental.model().affinities, model.affinities);
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
        let constraint_update = incremental
            .update(&expanded, &cfg, &graph, &changed_blocks, &changed_values)
            .unwrap();
        assert!(!constraint_update.affinities_changed);
        assert!(!constraint_update.fixed_reservations_changed);

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
        let constraint_update = incremental
            .update(
                &expanded,
                &cfg,
                &graph,
                &BTreeSet::from([BlockId(3)]),
                &changed_values,
            )
            .unwrap();
        assert!(constraint_update.affinities_changed);
        assert!(!constraint_update.fixed_reservations_changed);

        assert_eq!(
            incremental.model(),
            &AllocationConstraintModel::build(&expanded, &cfg, &graph, ALLOCATABLE_REGS).unwrap()
        );
    }
}
