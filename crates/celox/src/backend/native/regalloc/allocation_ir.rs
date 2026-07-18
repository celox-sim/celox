//! Off-to-the-side machine-value IR for allocation-owned splitting.
//!
//! Home selection introduces real stores, reloads, and recipe operations.  All
//! values defined by those operations must participate in the same exact
//! liveness and physical allocation as original MIR values.  This IR records
//! them against immutable original-MIR anchors without mutating `MFunction`;
//! successful allocation can later lower the complete result atomically.

use std::collections::{BTreeSet, HashMap};
use std::fmt;

use crate::backend::native::mir::{BlockId, MFunction, Uses, VReg};

use super::cfg::NormalizedCfg;
use super::home_graph::{LiveBundleId, RecipeId};
use super::live_interval::{
    DefinitionSite, LiveIntervalError, LiveIntervals, LivenessProgram, UseSite, analyze_program,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct StackHomeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct SyntheticInstructionId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SyntheticOperation {
    StackStore { home: StackHomeId },
    StackReload { home: StackHomeId },
    RecipeNode { root: LiveBundleId, node: RecipeId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyntheticAnchor {
    BlockEntry {
        block: BlockId,
    },
    BeforeInstruction {
        block: BlockId,
        instruction: usize,
    },
    AfterInstruction {
        block: BlockId,
        instruction: usize,
    },
    BeforePhiEdge {
        predecessor: BlockId,
        successor: BlockId,
        phi: usize,
    },
}

impl SyntheticAnchor {
    fn block(self) -> BlockId {
        match self {
            Self::BlockEntry { block }
            | Self::BeforeInstruction { block, .. }
            | Self::AfterInstruction { block, .. } => block,
            Self::BeforePhiEdge { predecessor, .. } => predecessor,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AllocationInstructionOrigin {
    Original {
        instruction: usize,
    },
    Synthetic {
        id: SyntheticInstructionId,
        anchor: SyntheticAnchor,
        operation: SyntheticOperation,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AllocationInstruction {
    origin: AllocationInstructionOrigin,
    uses: Uses,
    definition: Option<VReg>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AllocationPhi {
    original_phi: usize,
    destination: VReg,
    sources: Vec<(BlockId, VReg)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AllocationBlock {
    id: BlockId,
    original_instruction_count: usize,
    original_terminator: Option<usize>,
    successors: Vec<BlockId>,
    phis: Vec<AllocationPhi>,
    instructions: Vec<AllocationInstruction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct InsertedSynthetic {
    pub instruction: SyntheticInstructionId,
    pub definition: Option<VReg>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AllocationIr {
    original_value_count: u32,
    next_value: u32,
    next_synthetic_instruction: u32,
    block_index: HashMap<BlockId, usize>,
    blocks: Vec<AllocationBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AllocationIrError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub values: Vec<VReg>,
    pub message: String,
}

impl AllocationIrError {
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

    fn live(error: LiveIntervalError) -> Self {
        Self::new(
            error.rule,
            error.block,
            error.instruction,
            error.values,
            error.message,
        )
    }
}

impl fmt::Display for AllocationIrError {
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

impl std::error::Error for AllocationIrError {}

impl AllocationIr {
    pub(super) fn from_mir(func: &MFunction) -> Result<Self, AllocationIrError> {
        let mut block_index = HashMap::with_capacity(func.blocks.len());
        let mut blocks = Vec::with_capacity(func.blocks.len());
        for block in &func.blocks {
            let index = blocks.len();
            if block_index.insert(block.id, index).is_some() {
                return Err(AllocationIrError::new(
                    "ALLOCATION_IR.DUPLICATE_BLOCK",
                    Some(block.id),
                    None,
                    Vec::new(),
                    "input MIR contains the same block identity more than once",
                ));
            }
            let phis = block
                .phis
                .iter()
                .enumerate()
                .map(|(original_phi, phi)| AllocationPhi {
                    original_phi,
                    destination: phi.dst,
                    sources: phi.sources.clone(),
                })
                .collect();
            let instructions = block
                .insts
                .iter()
                .enumerate()
                .map(|(instruction, inst)| AllocationInstruction {
                    origin: AllocationInstructionOrigin::Original { instruction },
                    uses: inst.uses(),
                    definition: inst.def(),
                })
                .collect();
            blocks.push(AllocationBlock {
                id: block.id,
                original_instruction_count: block.insts.len(),
                original_terminator: block
                    .insts
                    .last()
                    .filter(|instruction| instruction.is_terminator())
                    .map(|_| block.insts.len() - 1),
                successors: block.successors(),
                phis,
                instructions,
            });
        }
        let result = Self {
            original_value_count: func.vregs.count(),
            next_value: func.vregs.count(),
            next_synthetic_instruction: 0,
            block_index,
            blocks,
        };
        result.verify_structure()?;
        Ok(result)
    }

    pub(super) fn value_count(&self) -> u32 {
        self.next_value
    }

    pub(super) fn insert_before_use(
        &mut self,
        site: UseSite,
        operation: SyntheticOperation,
        uses: Uses,
        defines_value: bool,
    ) -> Result<InsertedSynthetic, AllocationIrError> {
        let anchor = match site {
            UseSite::Instruction {
                block, instruction, ..
            } => SyntheticAnchor::BeforeInstruction { block, instruction },
            UseSite::PhiEdge {
                predecessor,
                successor,
                phi,
                ..
            } => SyntheticAnchor::BeforePhiEdge {
                predecessor,
                successor,
                phi,
            },
        };
        self.insert_synthetic(anchor, operation, uses, defines_value)
    }

    pub(super) fn insert_after_definition(
        &mut self,
        site: DefinitionSite,
        operation: SyntheticOperation,
        uses: Uses,
        defines_value: bool,
    ) -> Result<InsertedSynthetic, AllocationIrError> {
        let anchor = match site {
            DefinitionSite::Phi { block, .. } => SyntheticAnchor::BlockEntry { block },
            DefinitionSite::Instruction {
                block, instruction, ..
            } => SyntheticAnchor::AfterInstruction { block, instruction },
        };
        self.insert_synthetic(anchor, operation, uses, defines_value)
    }

    pub(super) fn rewrite_use(
        &mut self,
        site: UseSite,
        original: VReg,
        replacement: VReg,
    ) -> Result<(), AllocationIrError> {
        if replacement.0 >= self.next_value {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.VALUE_RANGE",
                Some(site.block()),
                match site {
                    UseSite::Instruction { instruction, .. } => Some(instruction),
                    UseSite::PhiEdge { .. } => None,
                },
                vec![replacement],
                "replacement value is outside the allocation IR",
            ));
        }
        match site {
            UseSite::Instruction {
                block, instruction, ..
            } => {
                let block_index = self.block(block)?;
                let position = self.original_instruction_position(block_index, instruction)?;
                if !self.blocks[block_index].instructions[position]
                    .uses
                    .replace(original, replacement)
                {
                    return Err(AllocationIrError::new(
                        "ALLOCATION_IR.USE_IDENTITY",
                        Some(block),
                        Some(instruction),
                        vec![original],
                        "original instruction does not use the value being rewritten",
                    ));
                }
            }
            UseSite::PhiEdge {
                predecessor,
                successor,
                phi,
                ..
            } => {
                let successor_index = self.block(successor)?;
                let Some(phi_row) = self.blocks[successor_index].phis.get_mut(phi) else {
                    return Err(AllocationIrError::new(
                        "ALLOCATION_IR.PHI_RANGE",
                        Some(successor),
                        None,
                        vec![original],
                        "phi-edge use references a missing original phi",
                    ));
                };
                let Some((_, source)) = phi_row
                    .sources
                    .iter_mut()
                    .find(|(block, _)| *block == predecessor)
                else {
                    return Err(AllocationIrError::new(
                        "ALLOCATION_IR.PHI_EDGE",
                        Some(successor),
                        None,
                        vec![original],
                        "phi has no source for the requested predecessor edge",
                    ));
                };
                if *source != original {
                    return Err(AllocationIrError::new(
                        "ALLOCATION_IR.USE_IDENTITY",
                        Some(successor),
                        None,
                        vec![original, *source],
                        "phi-edge source differs from the value being rewritten",
                    ));
                }
                *source = replacement;
            }
        }
        Ok(())
    }

    pub(super) fn analyze(&self, cfg: &NormalizedCfg) -> Result<LiveIntervals, AllocationIrError> {
        self.verify_structure()?;
        analyze_program(self, cfg).map_err(AllocationIrError::live)
    }

    fn insert_synthetic(
        &mut self,
        anchor: SyntheticAnchor,
        operation: SyntheticOperation,
        uses: Uses,
        defines_value: bool,
    ) -> Result<InsertedSynthetic, AllocationIrError> {
        self.verify_operation(anchor, operation, uses, defines_value)?;
        let block = self.block(anchor.block())?;
        let position = self.insertion_position(block, anchor)?;
        let Some(next_instruction) = self.next_synthetic_instruction.checked_add(1) else {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.INSTRUCTION_ID_RANGE",
                Some(anchor.block()),
                None,
                Vec::new(),
                "synthetic instruction identity exceeds u32",
            ));
        };
        let instruction = SyntheticInstructionId(self.next_synthetic_instruction);
        let (definition, next_value) = if defines_value {
            let Some(next_value) = self.next_value.checked_add(1) else {
                return Err(AllocationIrError::new(
                    "ALLOCATION_IR.VALUE_ID_RANGE",
                    Some(anchor.block()),
                    None,
                    Vec::new(),
                    "synthetic machine-value identity exceeds u32",
                ));
            };
            (Some(VReg(self.next_value)), next_value)
        } else {
            (None, self.next_value)
        };
        self.blocks[block].instructions.insert(
            position,
            AllocationInstruction {
                origin: AllocationInstructionOrigin::Synthetic {
                    id: instruction,
                    anchor,
                    operation,
                },
                uses,
                definition,
            },
        );
        self.next_synthetic_instruction = next_instruction;
        self.next_value = next_value;
        Ok(InsertedSynthetic {
            instruction,
            definition,
        })
    }

    fn verify_operation(
        &self,
        anchor: SyntheticAnchor,
        operation: SyntheticOperation,
        uses: Uses,
        defines_value: bool,
    ) -> Result<(), AllocationIrError> {
        let valid = match operation {
            SyntheticOperation::StackStore { .. } => uses.len() == 1 && !defines_value,
            SyntheticOperation::StackReload { .. } => uses.is_empty() && defines_value,
            SyntheticOperation::RecipeNode { .. } => uses.len() <= 2 && defines_value,
        };
        if !valid {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.SYNTHETIC_SIGNATURE",
                Some(anchor.block()),
                None,
                uses.to_vec(),
                format!("invalid def/use signature for {operation:?}"),
            ));
        }
        for value in uses {
            if value.0 >= self.next_value {
                return Err(AllocationIrError::new(
                    "ALLOCATION_IR.VALUE_RANGE",
                    Some(anchor.block()),
                    None,
                    vec![value],
                    "synthetic operation uses a value outside the allocation IR",
                ));
            }
        }
        Ok(())
    }

    fn block(&self, id: BlockId) -> Result<usize, AllocationIrError> {
        self.block_index.get(&id).copied().ok_or_else(|| {
            AllocationIrError::new(
                "ALLOCATION_IR.BLOCK_RANGE",
                Some(id),
                None,
                Vec::new(),
                "anchor references a block outside the allocation IR",
            )
        })
    }

    fn original_instruction_position(
        &self,
        block: usize,
        instruction: usize,
    ) -> Result<usize, AllocationIrError> {
        self.blocks[block]
            .instructions
            .iter()
            .position(|candidate| {
                candidate.origin == AllocationInstructionOrigin::Original { instruction }
            })
            .ok_or_else(|| {
                AllocationIrError::new(
                    "ALLOCATION_IR.INSTRUCTION_RANGE",
                    Some(self.blocks[block].id),
                    Some(instruction),
                    Vec::new(),
                    "anchor references a missing original instruction",
                )
            })
    }

    fn insertion_position(
        &self,
        block: usize,
        anchor: SyntheticAnchor,
    ) -> Result<usize, AllocationIrError> {
        match anchor {
            SyntheticAnchor::BlockEntry { .. } => Ok(self.blocks[block]
                .instructions
                .iter()
                .take_while(|instruction| {
                    matches!(
                        instruction.origin,
                        AllocationInstructionOrigin::Synthetic {
                            anchor: SyntheticAnchor::BlockEntry { .. },
                            ..
                        }
                    )
                })
                .count()),
            SyntheticAnchor::BeforeInstruction { instruction, .. } => {
                self.original_instruction_position(block, instruction)
            }
            SyntheticAnchor::AfterInstruction { instruction, .. } => {
                let mut position = self
                    .original_instruction_position(block, instruction)?
                    .checked_add(1)
                    .ok_or_else(|| {
                        AllocationIrError::new(
                            "ALLOCATION_IR.INSTRUCTION_ID_RANGE",
                            Some(self.blocks[block].id),
                            Some(instruction),
                            Vec::new(),
                            "instruction insertion position exceeds usize",
                        )
                    })?;
                while self.blocks[block]
                    .instructions
                    .get(position)
                    .is_some_and(|candidate| {
                        matches!(
                            candidate.origin,
                            AllocationInstructionOrigin::Synthetic {
                                anchor: candidate_anchor,
                                ..
                            } if candidate_anchor == anchor
                        )
                    })
                {
                    position += 1;
                }
                Ok(position)
            }
            SyntheticAnchor::BeforePhiEdge {
                predecessor,
                successor,
                phi,
            } => {
                if !self.blocks[block].successors.contains(&successor) {
                    return Err(AllocationIrError::new(
                        "ALLOCATION_IR.PHI_EDGE",
                        Some(predecessor),
                        None,
                        Vec::new(),
                        format!("block has no CFG edge to {successor}"),
                    ));
                }
                let successor_index = self.block(successor)?;
                let Some(phi_row) = self.blocks[successor_index].phis.get(phi) else {
                    return Err(AllocationIrError::new(
                        "ALLOCATION_IR.PHI_RANGE",
                        Some(successor),
                        None,
                        Vec::new(),
                        "edge anchor references a missing original phi",
                    ));
                };
                if !phi_row
                    .sources
                    .iter()
                    .any(|(source, _)| *source == predecessor)
                {
                    return Err(AllocationIrError::new(
                        "ALLOCATION_IR.PHI_EDGE",
                        Some(successor),
                        None,
                        Vec::new(),
                        "edge anchor references a phi without that predecessor",
                    ));
                }
                let terminator = self.blocks[block].original_terminator.ok_or_else(|| {
                    AllocationIrError::new(
                        "ALLOCATION_IR.EDGE_INSERTION",
                        Some(predecessor),
                        None,
                        Vec::new(),
                        "phi-edge predecessor has no original terminator",
                    )
                })?;
                self.original_instruction_position(block, terminator)
            }
        }
    }

    fn verify_structure(&self) -> Result<(), AllocationIrError> {
        if self.blocks.is_empty()
            || self.block_index.len() != self.blocks.len()
            || self.next_value < self.original_value_count
        {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.MODEL_SHAPE",
                self.blocks.first().map(|block| block.id),
                None,
                Vec::new(),
                "allocation IR does not cover a non-empty input MIR",
            ));
        }
        let mut synthetic_ids = BTreeSet::new();
        let mut synthetic_definitions = BTreeSet::new();
        for (block_index, block) in self.blocks.iter().enumerate() {
            if self.block_index.get(&block.id) != Some(&block_index) {
                return Err(AllocationIrError::new(
                    "ALLOCATION_IR.BLOCK_INDEX",
                    Some(block.id),
                    None,
                    Vec::new(),
                    "block identity differs from its dense allocation-IR row",
                ));
            }
            if block.phis.iter().enumerate().any(|(index, phi)| {
                phi.original_phi != index || phi.destination.0 >= self.original_value_count
            }) {
                return Err(AllocationIrError::new(
                    "ALLOCATION_IR.PHI_IDENTITY",
                    Some(block.id),
                    None,
                    Vec::new(),
                    "allocation phi identity or destination differs from input MIR",
                ));
            }
            let originals = block
                .instructions
                .iter()
                .filter_map(|instruction| match instruction.origin {
                    AllocationInstructionOrigin::Original { instruction } => Some(instruction),
                    AllocationInstructionOrigin::Synthetic { .. } => None,
                })
                .collect::<Vec<_>>();
            if originals != (0..block.original_instruction_count).collect::<Vec<_>>() {
                return Err(AllocationIrError::new(
                    "ALLOCATION_IR.ORIGINAL_ORDER",
                    Some(block.id),
                    None,
                    Vec::new(),
                    "synthetic insertion changed or duplicated original instruction order",
                ));
            }
            for instruction in &block.instructions {
                for value in instruction.uses {
                    if value.0 >= self.next_value {
                        return Err(AllocationIrError::new(
                            "ALLOCATION_IR.VALUE_RANGE",
                            Some(block.id),
                            None,
                            vec![value],
                            "instruction use is outside the allocation value table",
                        ));
                    }
                }
                if let Some(value) = instruction.definition {
                    if value.0 >= self.next_value {
                        return Err(AllocationIrError::new(
                            "ALLOCATION_IR.VALUE_RANGE",
                            Some(block.id),
                            None,
                            vec![value],
                            "instruction definition is outside the allocation value table",
                        ));
                    }
                }
                if let AllocationInstructionOrigin::Synthetic { id, anchor, .. } =
                    instruction.origin
                {
                    if anchor.block() != block.id || !synthetic_ids.insert(id) {
                        return Err(AllocationIrError::new(
                            "ALLOCATION_IR.SYNTHETIC_IDENTITY",
                            Some(block.id),
                            None,
                            Vec::new(),
                            "synthetic instruction has the wrong block or duplicate identity",
                        ));
                    }
                    if let Some(definition) = instruction.definition
                        && (definition.0 < self.original_value_count
                            || !synthetic_definitions.insert(definition))
                    {
                        return Err(AllocationIrError::new(
                            "ALLOCATION_IR.SYNTHETIC_DEFINITION",
                            Some(block.id),
                            None,
                            vec![definition],
                            "synthetic value aliases input MIR or has multiple definitions",
                        ));
                    }
                }
            }
        }
        let expected_instruction_ids = (0..self.next_synthetic_instruction)
            .map(SyntheticInstructionId)
            .collect::<BTreeSet<_>>();
        if synthetic_ids != expected_instruction_ids {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.SYNTHETIC_COVERAGE",
                None,
                None,
                Vec::new(),
                "synthetic instruction table has a missing or unallocated identity",
            ));
        }
        let expected_definitions = (self.original_value_count..self.next_value)
            .map(VReg)
            .collect::<BTreeSet<_>>();
        if synthetic_definitions != expected_definitions {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.SYNTHETIC_COVERAGE",
                None,
                None,
                expected_definitions
                    .symmetric_difference(&synthetic_definitions)
                    .copied()
                    .collect(),
                "synthetic machine values do not have one exact definition each",
            ));
        }
        Ok(())
    }
}

impl LivenessProgram for AllocationIr {
    fn value_count(&self) -> u32 {
        self.next_value
    }

    fn block_count(&self) -> usize {
        self.blocks.len()
    }

    fn block_id(&self, block: usize) -> BlockId {
        self.blocks[block].id
    }

    fn phi_count(&self, block: usize) -> usize {
        self.blocks[block].phis.len()
    }

    fn phi_definition(&self, block: usize, phi: usize) -> VReg {
        self.blocks[block].phis[phi].destination
    }

    fn phi_sources(&self, block: usize, phi: usize) -> &[(BlockId, VReg)] {
        &self.blocks[block].phis[phi].sources
    }

    fn instruction_count(&self, block: usize) -> usize {
        self.blocks[block].instructions.len()
    }

    fn instruction_uses(&self, block: usize, instruction: usize) -> Uses {
        self.blocks[block].instructions[instruction].uses
    }

    fn instruction_definition(&self, block: usize, instruction: usize) -> Option<VReg> {
        self.blocks[block].instructions[instruction].definition
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::mir::{MBlock, MInst, PhiNode, SpillDesc, VRegAllocator};

    fn function(value_count: u32, blocks: Vec<MBlock>) -> MFunction {
        let mut values = VRegAllocator::new();
        for _ in 0..value_count {
            values.alloc();
        }
        let mut function =
            MFunction::new(values, vec![SpillDesc::transient(); value_count as usize]);
        function.blocks = blocks;
        function
    }

    fn normalize(function: &mut MFunction) -> NormalizedCfg {
        super::super::cfg::normalize(function).unwrap()
    }

    fn straight_line() -> MFunction {
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: VReg(0),
            value: 7,
        });
        block.push(MInst::Mov {
            dst: VReg(1),
            src: VReg(0),
        });
        block.push(MInst::Return);
        function(2, vec![block])
    }

    #[test]
    fn unchanged_allocation_ir_has_exactly_mir_liveness() {
        let mut function = straight_line();
        let cfg = normalize(&mut function);
        let expected = super::super::live_interval::analyze(&function, &cfg).unwrap();
        let allocation_ir = AllocationIr::from_mir(&function).unwrap();
        let actual = allocation_ir.analyze(&cfg).unwrap();

        assert_eq!(actual, expected);
        assert_eq!(allocation_ir.value_count(), function.vregs.count());
    }

    #[test]
    fn definition_to_stack_store_is_a_real_short_live_range() {
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: VReg(0),
            value: 7,
        });
        block.push(MInst::Return);
        let mut function = function(1, vec![block]);
        let cfg = normalize(&mut function);
        let original = super::super::live_interval::analyze(&function, &cfg).unwrap();
        let definition = original.intervals[0].as_ref().unwrap().definition;
        let before = format!("{function:?}");
        let mut allocation_ir = AllocationIr::from_mir(&function).unwrap();

        let inserted = allocation_ir
            .insert_after_definition(
                definition,
                SyntheticOperation::StackStore {
                    home: StackHomeId(0),
                },
                Uses::one(VReg(0)),
                false,
            )
            .unwrap();
        assert_eq!(inserted.definition, None);
        let intervals = allocation_ir.analyze(&cfg).unwrap();
        let value = intervals.intervals[0].as_ref().unwrap();

        assert_eq!(value.uses.len(), 1);
        assert!(matches!(
            value.uses[0],
            UseSite::Instruction {
                block: BlockId(0),
                instruction: 1,
                ..
            }
        ));
        assert_eq!(format!("{function:?}"), before);
    }

    #[test]
    fn reload_and_recipe_results_reenter_exact_liveness() {
        let mut function = straight_line();
        let cfg = normalize(&mut function);
        let original = super::super::live_interval::analyze(&function, &cfg).unwrap();
        let use_site = original.intervals[0].as_ref().unwrap().uses[0];
        let mut allocation_ir = AllocationIr::from_mir(&function).unwrap();

        let reload = allocation_ir
            .insert_before_use(
                use_site,
                SyntheticOperation::StackReload {
                    home: StackHomeId(0),
                },
                Uses::none(),
                true,
            )
            .unwrap()
            .definition
            .unwrap();
        let recipe = allocation_ir
            .insert_before_use(
                use_site,
                SyntheticOperation::RecipeNode {
                    root: LiveBundleId(0),
                    node: RecipeId(0),
                },
                Uses::one(reload),
                true,
            )
            .unwrap()
            .definition
            .unwrap();
        allocation_ir
            .rewrite_use(use_site, VReg(0), recipe)
            .unwrap();
        let intervals = allocation_ir.analyze(&cfg).unwrap();

        assert!(intervals.intervals[0].as_ref().unwrap().uses.is_empty());
        let reload_interval = intervals.intervals[reload.0 as usize].as_ref().unwrap();
        let recipe_interval = intervals.intervals[recipe.0 as usize].as_ref().unwrap();
        assert_eq!(reload_interval.uses.len(), 1);
        assert_eq!(recipe_interval.uses.len(), 1);
        assert!(reload_interval.definition.slot() < reload_interval.uses[0].slot());
        assert!(recipe_interval.definition.slot() < recipe_interval.uses[0].slot());
        assert!(reload_interval.uses[0].slot() < recipe_interval.uses[0].slot());
    }

    #[test]
    fn phi_edge_reload_is_defined_on_only_that_normalized_edge() {
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 1,
        });
        entry.push(MInst::LoadImm {
            dst: VReg(1),
            value: 11,
        });
        entry.push(MInst::Branch {
            cond: VReg(0),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::Jump { target: BlockId(3) });
        let mut merge = MBlock::new(BlockId(3));
        merge.phis.push(PhiNode {
            dst: VReg(2),
            sources: vec![(BlockId(1), VReg(1)), (BlockId(2), VReg(1))],
        });
        merge.push(MInst::Mov {
            dst: VReg(3),
            src: VReg(2),
        });
        merge.push(MInst::Return);
        let mut function = function(4, vec![entry, left, right, merge]);
        let cfg = normalize(&mut function);
        let original = super::super::live_interval::analyze(&function, &cfg).unwrap();
        let edge = original.intervals[1]
            .as_ref()
            .unwrap()
            .uses
            .iter()
            .copied()
            .find(|site| matches!(site, UseSite::PhiEdge { .. }))
            .unwrap();
        let mut allocation_ir = AllocationIr::from_mir(&function).unwrap();

        let reload = allocation_ir
            .insert_before_use(
                edge,
                SyntheticOperation::StackReload {
                    home: StackHomeId(0),
                },
                Uses::none(),
                true,
            )
            .unwrap()
            .definition
            .unwrap();
        allocation_ir.rewrite_use(edge, VReg(1), reload).unwrap();
        let intervals = allocation_ir.analyze(&cfg).unwrap();
        let reload_interval = intervals.intervals[reload.0 as usize].as_ref().unwrap();

        assert_eq!(reload_interval.uses.len(), 1);
        assert!(matches!(
            reload_interval.uses[0],
            UseSite::PhiEdge {
                predecessor,
                successor,
                ..
            } if predecessor == edge.block() && successor != predecessor
        ));
        assert_eq!(reload_interval.definition.block(), edge.block());
        assert_eq!(
            intervals.intervals[1]
                .as_ref()
                .unwrap()
                .uses
                .iter()
                .filter(|site| matches!(site, UseSite::PhiEdge { .. }))
                .count(),
            1,
            "the sibling phi edge must still use the original value"
        );
    }

    #[test]
    fn malformed_synthetic_signatures_are_rejected_without_mutation() {
        let mut function = straight_line();
        let cfg = normalize(&mut function);
        let intervals = super::super::live_interval::analyze(&function, &cfg).unwrap();
        let site = intervals.intervals[0].as_ref().unwrap().uses[0];
        let mut allocation_ir = AllocationIr::from_mir(&function).unwrap();
        let before = allocation_ir.clone();

        let error = allocation_ir
            .insert_before_use(
                site,
                SyntheticOperation::StackReload {
                    home: StackHomeId(0),
                },
                Uses::one(VReg(0)),
                true,
            )
            .unwrap_err();
        assert_eq!(error.rule, "ALLOCATION_IR.SYNTHETIC_SIGNATURE");
        assert_eq!(allocation_ir, before);
    }
}
