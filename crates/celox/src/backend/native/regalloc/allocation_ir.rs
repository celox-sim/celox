//! Off-to-the-side machine-value IR for allocation-owned splitting.
//!
//! Home selection introduces real stores, reloads, and recipe operations.  All
//! values defined by those operations must participate in the same exact
//! liveness and physical allocation as original MIR values.  This IR records
//! them against immutable original-MIR anchors without mutating `MFunction`;
//! successful allocation can later lower the complete result atomically.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt;

use crate::backend::native::features::VariableShiftEncoding;
use crate::backend::native::mir::{
    BaseReg, BlockId, MBlock, MFunction, MInst, OpSize, PhiNode, SpillDesc, Uses, VReg,
};

use super::assignment::{PhysReg, RegConstraint, clobbers, use_constraints};
use super::cfg::NormalizedCfg;
use super::home_graph::{HomeGraph, LiveBundleId, RecipeId, RecipeNode};
use super::live_interval::{
    DefinitionSite, LiveIntervalError, LiveIntervals, LivenessProgram, UseSite, analyze_program,
};
use super::reload::{PureStep, materialize_pure_step};

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct AllocationInstruction {
    origin: AllocationInstructionOrigin,
    /// Exact immutable source MIR instruction, including opcode, widths, and
    /// immediates. Synthetic instructions have no source-MIR snapshot.
    original: Option<MInst>,
    uses: Uses,
    definition: Option<VReg>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AllocationPhi {
    original_phi: usize,
    destination: VReg,
    original_sources: Vec<(BlockId, VReg)>,
    sources: Vec<(BlockId, VReg)>,
    register_sources: Vec<bool>,
    register_definition: bool,
    stack_home: Option<StackHomeId>,
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
pub(super) struct AllocationIrCompaction {
    values: Vec<Option<VReg>>,
    instructions: Vec<Option<SyntheticInstructionId>>,
}

impl AllocationIrCompaction {
    pub(super) fn value(&self, old: VReg) -> Option<VReg> {
        self.values.get(old.0 as usize).copied().flatten()
    }

    pub(super) fn instruction(
        &self,
        old: SyntheticInstructionId,
    ) -> Option<SyntheticInstructionId> {
        self.instructions.get(old.0 as usize).copied().flatten()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AllocationIr {
    original_value_count: u32,
    next_value: u32,
    next_synthetic_instruction: u32,
    block_index: HashMap<BlockId, usize>,
    blocks: Vec<AllocationBlock>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum AllocationAffinityKind {
    Copy,
    Phi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct AllocationAffinity {
    pub left: VReg,
    pub right: VReg,
    pub kind: AllocationAffinityKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AllocationInstructionConstraints {
    pub block: BlockId,
    pub instruction: usize,
    pub fixed_uses: Vec<(VReg, PhysReg)>,
    pub clobbers: Vec<PhysReg>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AllocationMachineFacts {
    pub instructions: Vec<AllocationInstructionConstraints>,
    pub affinities: Vec<AllocationAffinity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AllocationStackOperationKind {
    Store,
    Reload,
}

/// Exact location-level stack operation in the current allocation IR.
///
/// Stack-slot coloring intentionally does not infer these positions from the
/// immutable source MIR: synthetic insertion and compaction change the slot
/// domain used by allocation liveness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AllocationStackOperation {
    pub instruction: SyntheticInstructionId,
    pub block: BlockId,
    pub position: usize,
    pub home: StackHomeId,
    pub kind: AllocationStackOperationKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AllocationStackPhiDefinition {
    pub block: BlockId,
    pub phi: usize,
    pub destination: VReg,
    pub home: StackHomeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AllocationStackFacts {
    pub blocks: Vec<(BlockId, usize)>,
    pub operations: Vec<AllocationStackOperation>,
    pub phi_definitions: Vec<AllocationStackPhiDefinition>,
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
                    original_sources: phi.sources.clone(),
                    sources: phi.sources.clone(),
                    register_sources: vec![true; phi.sources.len()],
                    register_definition: true,
                    stack_home: None,
                })
                .collect();
            let instructions = block
                .insts
                .iter()
                .enumerate()
                .map(|(instruction, inst)| AllocationInstruction {
                    origin: AllocationInstructionOrigin::Original { instruction },
                    original: Some(inst.clone()),
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

    /// Materialize the complete allocation IR into a private strict-SSA MIR
    /// function. The source function is immutable and every original def/use
    /// row is matched against the snapshot captured by [`Self::from_mir`]
    /// before rewritten operands or synthetic instructions are emitted.
    pub(super) fn materialize(
        &self,
        original: &MFunction,
        graph: &HomeGraph,
        stack_offsets: &[i32],
    ) -> Result<MFunction, AllocationIrError> {
        self.verify_structure()?;
        original.verify_result().map_err(|error| {
            AllocationIrError::new(
                "ALLOCATION_IR.SOURCE_MIR",
                error.block,
                error.instruction,
                Vec::new(),
                error.message,
            )
        })?;
        if original.vregs.count() != self.original_value_count
            || original.blocks.len() != self.blocks.len()
        {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.SOURCE_SHAPE",
                original.blocks.first().map(|block| block.id),
                None,
                Vec::new(),
                "source MIR no longer has the value or block domain captured by allocation IR",
            ));
        }

        let mut output = original.clone();
        while output.vregs.count() < self.next_value {
            let expected = VReg(output.vregs.count());
            let allocated = output.vregs.try_alloc().map_err(|_| {
                AllocationIrError::new(
                    "ALLOCATION_IR.VALUE_ID_RANGE",
                    None,
                    None,
                    vec![expected],
                    "materialized MIR exhausted the VReg namespace",
                )
            })?;
            if allocated != expected {
                return Err(AllocationIrError::new(
                    "ALLOCATION_IR.VALUE_IDENTITY",
                    None,
                    None,
                    vec![expected, allocated],
                    "materialized MIR did not preserve dense allocation-value identity",
                ));
            }
            output.spill_descs.push(SpillDesc::transient());
        }

        let mut recipe_definitions = HashMap::<VReg, (LiveBundleId, RecipeId)>::new();
        let mut blocks = Vec::with_capacity(self.blocks.len());
        for (block_index, allocation_block) in self.blocks.iter().enumerate() {
            let source = &original.blocks[block_index];
            if source.id != allocation_block.id
                || source.insts.len() != allocation_block.original_instruction_count
                || source.successors() != allocation_block.successors
                || source
                    .insts
                    .last()
                    .filter(|instruction| instruction.is_terminator())
                    .map(|_| source.insts.len() - 1)
                    != allocation_block.original_terminator
            {
                return Err(AllocationIrError::new(
                    "ALLOCATION_IR.SOURCE_BLOCK",
                    Some(allocation_block.id),
                    None,
                    Vec::new(),
                    "source MIR block identity, instruction domain, or CFG changed after allocation-IR construction",
                ));
            }
            if source.phis.len() != allocation_block.phis.len() {
                return Err(AllocationIrError::new(
                    "ALLOCATION_IR.SOURCE_PHI",
                    Some(allocation_block.id),
                    None,
                    Vec::new(),
                    "source MIR phi domain changed after allocation-IR construction",
                ));
            }

            let mut block = MBlock::new(allocation_block.id);
            block.phis.reserve(allocation_block.phis.len());
            for phi in &allocation_block.phis {
                let source_phi = &source.phis[phi.original_phi];
                if source_phi.dst != phi.destination || source_phi.sources != phi.original_sources {
                    return Err(AllocationIrError::new(
                        "ALLOCATION_IR.SOURCE_PHI",
                        Some(allocation_block.id),
                        None,
                        vec![phi.destination],
                        "source MIR phi changed after allocation-IR construction",
                    ));
                }
                if phi.sources.len() != phi.original_sources.len()
                    || phi.sources.iter().zip(&phi.original_sources).any(
                        |((predecessor, _), (original_predecessor, _))| {
                            predecessor != original_predecessor
                        },
                    )
                {
                    return Err(AllocationIrError::new(
                        "ALLOCATION_IR.PHI_EDGE_IDENTITY",
                        Some(allocation_block.id),
                        None,
                        vec![phi.destination],
                        "allocation rewrite changed phi predecessor identity or order",
                    ));
                }
                block.phis.push(PhiNode {
                    dst: phi.destination,
                    sources: phi.sources.clone(),
                });
            }

            block.insts.reserve(allocation_block.instructions.len());
            for (position, instruction) in allocation_block.instructions.iter().enumerate() {
                let lowered = match instruction.origin {
                    AllocationInstructionOrigin::Original {
                        instruction: original_instruction,
                    } => {
                        let source_instruction =
                            source.insts.get(original_instruction).ok_or_else(|| {
                                AllocationIrError::new(
                                    "ALLOCATION_IR.SOURCE_INSTRUCTION",
                                    Some(allocation_block.id),
                                    Some(original_instruction),
                                    Vec::new(),
                                    "allocation IR references a missing source MIR instruction",
                                )
                            })?;
                        let original_snapshot = instruction.original.as_ref().ok_or_else(|| {
                            AllocationIrError::new(
                                "ALLOCATION_IR.SOURCE_INSTRUCTION",
                                Some(allocation_block.id),
                                Some(original_instruction),
                                Vec::new(),
                                "original allocation instruction has no immutable operand snapshot",
                            )
                        })?;
                        if source_instruction != original_snapshot {
                            return Err(AllocationIrError::new(
                                "ALLOCATION_IR.SOURCE_INSTRUCTION",
                                Some(allocation_block.id),
                                Some(original_instruction),
                                instruction.uses.to_vec(),
                                "source MIR instruction changed after allocation-IR construction",
                            ));
                        }
                        rewrite_original_instruction(
                            source_instruction,
                            original_snapshot.uses(),
                            instruction.uses,
                            self.original_value_count,
                            allocation_block.id,
                            original_instruction,
                        )?
                    }
                    AllocationInstructionOrigin::Synthetic { operation, .. } => {
                        if instruction.original.is_some() {
                            return Err(AllocationIrError::new(
                                "ALLOCATION_IR.SOURCE_INSTRUCTION",
                                Some(allocation_block.id),
                                Some(position),
                                instruction.uses.to_vec(),
                                "synthetic allocation instruction carries a source-MIR operand snapshot",
                            ));
                        }
                        materialize_synthetic_instruction(
                            graph,
                            stack_offsets,
                            &mut recipe_definitions,
                            allocation_block.id,
                            position,
                            operation,
                            instruction.uses,
                            instruction.definition,
                        )?
                    }
                };
                if lowered.uses() != instruction.uses || lowered.def() != instruction.definition {
                    return Err(AllocationIrError::new(
                        "ALLOCATION_IR.LOWERED_SIGNATURE",
                        Some(allocation_block.id),
                        Some(position),
                        instruction.uses.to_vec(),
                        "lowered MIR instruction does not preserve allocation-IR def/use identity",
                    ));
                }
                block.insts.push(lowered);
            }
            blocks.push(block);
        }
        output.blocks = blocks;
        output.verify_result().map_err(|error| {
            AllocationIrError::new(
                "ALLOCATION_IR.LOWERED_MIR",
                error.block,
                error.instruction,
                Vec::new(),
                error.message,
            )
        })?;
        Ok(output)
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
                let Some((source_index, (_, source))) = phi_row
                    .sources
                    .iter_mut()
                    .enumerate()
                    .find(|(_, (block, _))| *block == predecessor)
                else {
                    return Err(AllocationIrError::new(
                        "ALLOCATION_IR.PHI_EDGE",
                        Some(successor),
                        None,
                        vec![original],
                        "phi has no source for the requested predecessor edge",
                    ));
                };
                if !phi_row.register_sources[source_index] {
                    return Err(AllocationIrError::new(
                        "ALLOCATION_IR.PHI_EDGE_LOCATION",
                        Some(successor),
                        None,
                        vec![original],
                        "phi-edge source already has a non-register allocation location",
                    ));
                }
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

    /// Resolve one semantic phi source directly from a non-register edge
    /// location. The source VReg remains in lowered MIR for SSA semantics and
    /// out-of-SSA identity, but it is removed from allocation liveness.
    pub(super) fn assign_phi_edge_home(
        &mut self,
        site: UseSite,
        current: VReg,
        semantic: VReg,
    ) -> Result<(), AllocationIrError> {
        let UseSite::PhiEdge {
            predecessor,
            successor,
            phi,
            ..
        } = site
        else {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.EDGE_LOCATION_SITE",
                Some(site.block()),
                None,
                vec![current],
                "non-register edge locations are valid only for phi-edge uses",
            ));
        };
        let successor_index = self.block(successor)?;
        let phi_row = self.blocks[successor_index]
            .phis
            .get_mut(phi)
            .ok_or_else(|| {
                AllocationIrError::new(
                    "ALLOCATION_IR.PHI_RANGE",
                    Some(successor),
                    None,
                    vec![current],
                    "phi-edge location references a missing original phi",
                )
            })?;
        let (source_index, (_, source)) = phi_row
            .sources
            .iter()
            .enumerate()
            .find(|(_, (block, _))| *block == predecessor)
            .ok_or_else(|| {
                AllocationIrError::new(
                    "ALLOCATION_IR.PHI_EDGE",
                    Some(successor),
                    None,
                    vec![current],
                    "phi has no source for the requested predecessor edge",
                )
            })?;
        if *source != current || !phi_row.register_sources[source_index] {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.USE_IDENTITY",
                Some(successor),
                None,
                vec![current, *source],
                "phi-edge source differs from the register use being assigned a home",
            ));
        }
        if semantic.0 >= self.original_value_count {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.EDGE_SEMANTIC_VALUE",
                Some(successor),
                None,
                vec![semantic],
                "non-register phi location must retain an immutable source-MIR value identity",
            ));
        }
        phi_row.sources[source_index].1 = semantic;
        phi_row.register_sources[source_index] = false;
        Ok(())
    }

    /// Resolve a semantic phi destination directly into a stack home. This is
    /// the destination-side counterpart of [`Self::assign_phi_edge_home`]:
    /// out-of-SSA copies define the slot on every incoming edge, so the phi
    /// destination itself must not create an artificial register definition.
    pub(super) fn assign_phi_definition_home(
        &mut self,
        site: DefinitionSite,
        destination: VReg,
        home: StackHomeId,
    ) -> Result<(), AllocationIrError> {
        let DefinitionSite::Phi { block, phi, .. } = site else {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.PHI_HOME_SITE",
                Some(site.block()),
                None,
                vec![destination],
                "phi stack destination requires a phi definition site",
            ));
        };
        let block_index = self.block(block)?;
        let row = self.blocks[block_index].phis.get_mut(phi).ok_or_else(|| {
            AllocationIrError::new(
                "ALLOCATION_IR.PHI_RANGE",
                Some(block),
                None,
                vec![destination],
                "phi stack destination references a missing phi row",
            )
        })?;
        if row.destination != destination || !row.register_definition || row.stack_home.is_some() {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.PHI_HOME_IDENTITY",
                Some(block),
                None,
                vec![destination, row.destination],
                "phi destination differs or already has an allocation home",
            ));
        }
        row.register_definition = false;
        row.stack_home = Some(home);
        Ok(())
    }

    pub(super) fn verify_phi_stack_definition(
        &self,
        block: BlockId,
        phi: usize,
        destination: VReg,
        home: StackHomeId,
    ) -> Result<(), AllocationIrError> {
        let block_index = self.block(block)?;
        let row = self.blocks[block_index].phis.get(phi).ok_or_else(|| {
            AllocationIrError::new(
                "ALLOCATION_IR.PHI_RANGE",
                Some(block),
                None,
                vec![destination],
                "expanded phi stack home references a missing phi row",
            )
        })?;
        if row.destination != destination || row.register_definition || row.stack_home != Some(home)
        {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.PHI_HOME_IDENTITY",
                Some(block),
                None,
                vec![destination, row.destination],
                "expanded phi stack metadata differs from allocation IR",
            ));
        }
        Ok(())
    }

    pub(super) fn analyze(&self, cfg: &NormalizedCfg) -> Result<LiveIntervals, AllocationIrError> {
        self.verify_structure()?;
        analyze_program(self, cfg).map_err(AllocationIrError::live)
    }

    /// Rebuild target constraints and copy affinities from the current
    /// allocation-IR operands. Original opcode snapshots remain the target
    /// authority, while rewritten operands identify the machine values which
    /// must satisfy each fixed use. Synthetic recipes are classified from
    /// their exact HomeGraph node rather than inferred from def/use arity.
    pub(super) fn machine_facts(
        &self,
        graph: &HomeGraph,
        shift_encoding: VariableShiftEncoding,
    ) -> Result<AllocationMachineFacts, AllocationIrError> {
        self.verify_structure()?;
        let mut instructions = Vec::new();
        let mut affinities = BTreeSet::new();
        for block in &self.blocks {
            for phi in &block.phis {
                if !phi.register_definition {
                    continue;
                }
                for ((_, source), in_register) in phi.sources.iter().zip(&phi.register_sources) {
                    if *in_register {
                        insert_affinity(
                            &mut affinities,
                            *source,
                            phi.destination,
                            AllocationAffinityKind::Phi,
                        );
                    }
                }
            }
            for (position, instruction) in block.instructions.iter().enumerate() {
                let (fixed_uses, instruction_clobbers, copy) = match instruction.origin {
                    AllocationInstructionOrigin::Original { .. } => {
                        let original = instruction.original.as_ref().ok_or_else(|| {
                            AllocationIrError::new(
                                "ALLOCATION_IR.CONSTRAINT_SNAPSHOT",
                                Some(block.id),
                                Some(position),
                                instruction.uses.to_vec(),
                                "original allocation instruction has no target-opcode snapshot",
                            )
                        })?;
                        let constraints = use_constraints(original, shift_encoding);
                        if constraints.len() != instruction.uses.len() {
                            return Err(AllocationIrError::new(
                                "ALLOCATION_IR.CONSTRAINT_ARITY",
                                Some(block.id),
                                Some(position),
                                instruction.uses.to_vec(),
                                "target operand constraints do not match rewritten MIR arity",
                            ));
                        }
                        let fixed = instruction
                            .uses
                            .into_iter()
                            .zip(constraints)
                            .filter_map(|(value, constraint)| match constraint {
                                RegConstraint::Any => None,
                                RegConstraint::Fixed(register) => Some((value, register)),
                            })
                            .collect();
                        let copy = match original {
                            MInst::Mov { .. } | MInst::Mov32 { .. } => {
                                copy_operands(block.id, position, instruction)?
                            }
                            _ => None,
                        };
                        (fixed, clobbers(original).to_vec(), copy)
                    }
                    AllocationInstructionOrigin::Synthetic { operation, .. } => {
                        let copy = match operation {
                            SyntheticOperation::RecipeNode { node, .. }
                                if matches!(
                                    graph.recipe_nodes.get(node.0 as usize),
                                    Some(RecipeNode::Unary {
                                        operation: PureStep::Copy64 | PureStep::Copy32,
                                        ..
                                    })
                                ) =>
                            {
                                copy_operands(block.id, position, instruction)?
                            }
                            _ => None,
                        };
                        (Vec::new(), Vec::new(), copy)
                    }
                };
                if let Some((source, destination)) = copy {
                    insert_affinity(
                        &mut affinities,
                        source,
                        destination,
                        AllocationAffinityKind::Copy,
                    );
                }
                if !fixed_uses.is_empty() || !instruction_clobbers.is_empty() {
                    instructions.push(AllocationInstructionConstraints {
                        block: block.id,
                        instruction: position,
                        fixed_uses,
                        clobbers: instruction_clobbers,
                    });
                }
            }
        }
        Ok(AllocationMachineFacts {
            instructions,
            affinities: affinities.into_iter().collect(),
        })
    }

    /// Export the exact location-level def/use facts needed for stack-home
    /// liveness. The returned instruction positions are in the same current
    /// allocation-IR layout consumed by [`Self::analyze`].
    pub(super) fn stack_facts(&self) -> Result<AllocationStackFacts, AllocationIrError> {
        self.verify_structure()?;
        let mut operations = Vec::new();
        let mut phi_definitions = Vec::new();
        for block in &self.blocks {
            for phi in &block.phis {
                if let Some(home) = phi.stack_home {
                    phi_definitions.push(AllocationStackPhiDefinition {
                        block: block.id,
                        phi: phi.original_phi,
                        destination: phi.destination,
                        home,
                    });
                }
            }
            for (position, row) in block.instructions.iter().enumerate() {
                let AllocationInstructionOrigin::Synthetic {
                    id: instruction,
                    operation,
                    ..
                } = row.origin
                else {
                    continue;
                };
                let (home, kind) = match operation {
                    SyntheticOperation::StackStore { home } => {
                        (home, AllocationStackOperationKind::Store)
                    }
                    SyntheticOperation::StackReload { home } => {
                        (home, AllocationStackOperationKind::Reload)
                    }
                    SyntheticOperation::RecipeNode { .. } => continue,
                };
                operations.push(AllocationStackOperation {
                    instruction,
                    block: block.id,
                    position,
                    home,
                    kind,
                });
            }
        }
        Ok(AllocationStackFacts {
            blocks: self
                .blocks
                .iter()
                .map(|block| (block.id, block.instructions.len()))
                .collect(),
            operations,
            phi_definitions,
        })
    }

    /// Resolve an immutable original-MIR use anchor to its position in the
    /// current allocation IR. Synthetic instructions can shift both an
    /// instruction's local index and every later block's global slot range;
    /// callers must therefore not compare original [`UseSite`] slots with
    /// intervals computed after expansion.
    pub(super) fn resolve_original_use_site(
        &self,
        original: UseSite,
        intervals: &LiveIntervals,
    ) -> Result<UseSite, AllocationIrError> {
        if intervals.block_slots.len() != self.blocks.len() {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.INTERVAL_SHAPE",
                Some(original.block()),
                None,
                Vec::new(),
                "live-interval block slots do not cover the allocation IR",
            ));
        }
        match original {
            UseSite::Instruction {
                block, instruction, ..
            } => {
                let block_index = self.block(block)?;
                let position = self.original_instruction_position(block_index, instruction)?;
                let slot = intervals.block_slots[block_index]
                    .instruction_use(position)
                    .ok_or_else(|| {
                        AllocationIrError::new(
                            "ALLOCATION_IR.USE_POSITION",
                            Some(block),
                            Some(instruction),
                            Vec::new(),
                            "resolved original instruction is outside allocation-IR slots",
                        )
                    })?;
                Ok(UseSite::Instruction {
                    block,
                    instruction: position,
                    slot,
                })
            }
            UseSite::PhiEdge {
                predecessor,
                successor,
                phi,
                ..
            } => {
                let predecessor_index = self.block(predecessor)?;
                let successor_index = self.block(successor)?;
                let phi_row = self.blocks[successor_index].phis.get(phi).ok_or_else(|| {
                    AllocationIrError::new(
                        "ALLOCATION_IR.PHI_RANGE",
                        Some(successor),
                        None,
                        Vec::new(),
                        "original use anchor references a missing phi",
                    )
                })?;
                if !phi_row
                    .sources
                    .iter()
                    .any(|(block, _)| *block == predecessor)
                {
                    return Err(AllocationIrError::new(
                        "ALLOCATION_IR.PHI_EDGE",
                        Some(successor),
                        None,
                        Vec::new(),
                        "original use anchor references a missing phi predecessor",
                    ));
                }
                Ok(UseSite::PhiEdge {
                    predecessor,
                    successor,
                    phi,
                    slot: intervals.block_slots[predecessor_index].exit,
                })
            }
        }
    }

    /// Resolve and verify the fixed use introduced by one explicit stack-home
    /// store. Register-region ownership may cover only a subset of an origin
    /// value's uses; this method lets joint allocation distinguish that exact
    /// allocator-owned use from movable RTL uses without weakening ownership
    /// checks for any other instruction.
    pub(super) fn resolve_stack_store_use_site(
        &self,
        instruction: SyntheticInstructionId,
        home: StackHomeId,
        value: VReg,
        intervals: &LiveIntervals,
    ) -> Result<UseSite, AllocationIrError> {
        if intervals.block_slots.len() != self.blocks.len() {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.INTERVAL_SHAPE",
                None,
                None,
                vec![value],
                "live-interval block slots do not cover the allocation IR",
            ));
        }
        let mut found = None;
        for (block_index, block) in self.blocks.iter().enumerate() {
            for (position, candidate) in block.instructions.iter().enumerate() {
                let AllocationInstructionOrigin::Synthetic {
                    id,
                    operation:
                        SyntheticOperation::StackStore {
                            home: candidate_home,
                        },
                    ..
                } = candidate.origin
                else {
                    continue;
                };
                if id != instruction {
                    continue;
                }
                if candidate_home != home
                    || candidate.definition.is_some()
                    || candidate.uses.to_vec() != [value]
                {
                    return Err(AllocationIrError::new(
                        "ALLOCATION_IR.STACK_STORE_IDENTITY",
                        Some(block.id),
                        None,
                        vec![value],
                        "stack-home metadata does not identify the expected fixed store use",
                    ));
                }
                let slot = intervals.block_slots[block_index]
                    .instruction_use(position)
                    .ok_or_else(|| {
                        AllocationIrError::new(
                            "ALLOCATION_IR.STACK_STORE_POSITION",
                            Some(block.id),
                            None,
                            vec![value],
                            "stack-home store is outside allocation-IR slots",
                        )
                    })?;
                let site = UseSite::Instruction {
                    block: block.id,
                    instruction: position,
                    slot,
                };
                if found.replace(site).is_some() {
                    return Err(AllocationIrError::new(
                        "ALLOCATION_IR.STACK_STORE_IDENTITY",
                        Some(block.id),
                        None,
                        vec![value],
                        "synthetic stack-store identity occurs more than once",
                    ));
                }
            }
        }
        found.ok_or_else(|| {
            AllocationIrError::new(
                "ALLOCATION_IR.STACK_STORE_IDENTITY",
                None,
                None,
                vec![value],
                "expanded stack home references a missing synthetic store",
            )
        })
    }

    /// Independently prove that every synthetic stack reload observes a
    /// same-home store on every path. The proof constructs sparse Boolean SSA
    /// only for homes which are actually reloaded; it never creates a dense
    /// block-by-home table.
    pub(super) fn verify_stack_homes(&self, cfg: &NormalizedCfg) -> Result<(), AllocationIrError> {
        self.verify_structure()?;
        verify_stack_home_reaching_definitions(self, cfg)
    }

    /// Remove pure synthetic materialization DAGs which no longer reach an
    /// original instruction, phi edge, or explicit stack store. Repeated
    /// pressure splitting can replace a whole register region; keeping its old
    /// reload/recipe definitions would turn dead code into artificial fixed
    /// register pressure. Original MIR and stack stores are never removed.
    /// Surviving synthetic values and instruction identities are compacted in
    /// old-identity order and returned for metadata repair.
    pub(super) fn prune_dead_materializations(
        &mut self,
    ) -> Result<AllocationIrCompaction, AllocationIrError> {
        self.verify_structure()?;
        let mut synthetic_definitions =
            vec![None::<(SyntheticInstructionId, Uses)>; self.next_value as usize];
        let mut retained_instructions = vec![false; self.next_synthetic_instruction as usize];
        let mut needed_values = vec![false; self.next_value as usize];
        let mut queue = VecDeque::<VReg>::new();

        for block in &self.blocks {
            for phi in &block.phis {
                queue.extend(
                    phi.sources
                        .iter()
                        .zip(&phi.register_sources)
                        .filter_map(|((_, value), register)| register.then_some(*value)),
                );
            }
            for instruction in &block.instructions {
                match instruction.origin {
                    AllocationInstructionOrigin::Original { .. } => {
                        queue.extend(instruction.uses.iter().copied());
                    }
                    AllocationInstructionOrigin::Synthetic {
                        id,
                        operation: SyntheticOperation::StackStore { .. },
                        ..
                    } => {
                        retained_instructions[id.0 as usize] = true;
                        queue.extend(instruction.uses.iter().copied());
                    }
                    AllocationInstructionOrigin::Synthetic { id, operation, .. } => {
                        if !matches!(
                            operation,
                            SyntheticOperation::StackReload { .. }
                                | SyntheticOperation::RecipeNode { .. }
                        ) {
                            return Err(AllocationIrError::new(
                                "ALLOCATION_IR.DCE_OPERATION",
                                Some(block.id),
                                None,
                                instruction.uses.to_vec(),
                                "synthetic definition has an unknown effect class",
                            ));
                        }
                        let definition = instruction.definition.ok_or_else(|| {
                            AllocationIrError::new(
                                "ALLOCATION_IR.DCE_DEFINITION",
                                Some(block.id),
                                None,
                                Vec::new(),
                                "pure synthetic materialization has no definition",
                            )
                        })?;
                        if synthetic_definitions[definition.0 as usize]
                            .replace((id, instruction.uses))
                            .is_some()
                        {
                            return Err(AllocationIrError::new(
                                "ALLOCATION_IR.DCE_DEFINITION",
                                Some(block.id),
                                None,
                                vec![definition],
                                "synthetic value has more than one defining instruction",
                            ));
                        }
                    }
                }
            }
        }

        while let Some(value) = queue.pop_front() {
            let needed = needed_values.get_mut(value.0 as usize).ok_or_else(|| {
                AllocationIrError::new(
                    "ALLOCATION_IR.DCE_VALUE_RANGE",
                    None,
                    None,
                    vec![value],
                    "semantic use references a value outside the allocation IR",
                )
            })?;
            if *needed || value.0 < self.original_value_count {
                *needed = true;
                continue;
            }
            *needed = true;
            let (instruction, uses) = synthetic_definitions
                .get(value.0 as usize)
                .copied()
                .flatten()
                .ok_or_else(|| {
                    AllocationIrError::new(
                        "ALLOCATION_IR.DCE_DEFINITION",
                        None,
                        None,
                        vec![value],
                        "reachable synthetic value has no pure defining instruction",
                    )
                })?;
            retained_instructions[instruction.0 as usize] = true;
            queue.extend(uses.iter().copied());
        }

        let mut instruction_map = vec![None; self.next_synthetic_instruction as usize];
        let mut retained_instruction_count = 0usize;
        for (old, retained) in retained_instructions.iter().copied().enumerate() {
            if !retained {
                continue;
            }
            let next = retained_instruction_count;
            retained_instruction_count += 1;
            let next = u32::try_from(next).map_err(|_| {
                AllocationIrError::new(
                    "ALLOCATION_IR.DCE_INSTRUCTION_RANGE",
                    None,
                    None,
                    Vec::new(),
                    "retained synthetic instruction count exceeds u32",
                )
            })?;
            instruction_map[old] = Some(SyntheticInstructionId(next));
        }

        let mut value_map = vec![None; self.next_value as usize];
        for original in 0..self.original_value_count {
            value_map[original as usize] = Some(VReg(original));
        }
        let mut next_value = self.original_value_count;
        for old in self.original_value_count..self.next_value {
            let old_value = VReg(old);
            let Some((instruction, _)) = synthetic_definitions[old as usize] else {
                return Err(AllocationIrError::new(
                    "ALLOCATION_IR.DCE_DEFINITION",
                    None,
                    None,
                    vec![old_value],
                    "synthetic value domain contains no defining materialization",
                ));
            };
            if !retained_instructions[instruction.0 as usize] {
                continue;
            }
            value_map[old as usize] = Some(VReg(next_value));
            next_value = next_value.checked_add(1).ok_or_else(|| {
                AllocationIrError::new(
                    "ALLOCATION_IR.DCE_VALUE_RANGE",
                    None,
                    None,
                    vec![old_value],
                    "compacted synthetic value identity exceeds u32",
                )
            })?;
        }

        for block in &mut self.blocks {
            for phi in &mut block.phis {
                for (_, value) in &mut phi.sources {
                    *value = compact_value(&value_map, *value, block.id)?;
                }
            }
            let mut instructions = Vec::with_capacity(block.instructions.len());
            for mut instruction in std::mem::take(&mut block.instructions) {
                if let AllocationInstructionOrigin::Synthetic { id, .. } = instruction.origin
                    && !retained_instructions[id.0 as usize]
                {
                    continue;
                }
                instruction.uses = compact_uses(&value_map, instruction.uses, block.id)?;
                if let Some(definition) = instruction.definition {
                    instruction.definition = Some(compact_value(&value_map, definition, block.id)?);
                }
                if let AllocationInstructionOrigin::Synthetic {
                    id,
                    anchor,
                    operation,
                } = instruction.origin
                {
                    let id = instruction_map[id.0 as usize].ok_or_else(|| {
                        AllocationIrError::new(
                            "ALLOCATION_IR.DCE_INSTRUCTION_RANGE",
                            Some(block.id),
                            None,
                            Vec::new(),
                            "retained synthetic instruction has no compact identity",
                        )
                    })?;
                    instruction.origin = AllocationInstructionOrigin::Synthetic {
                        id,
                        anchor,
                        operation,
                    };
                }
                instructions.push(instruction);
            }
            block.instructions = instructions;
        }
        self.next_value = next_value;
        self.next_synthetic_instruction =
            u32::try_from(retained_instruction_count).map_err(|_| {
                AllocationIrError::new(
                    "ALLOCATION_IR.DCE_INSTRUCTION_RANGE",
                    None,
                    None,
                    Vec::new(),
                    "retained synthetic instruction count exceeds u32",
                )
            })?;
        self.verify_structure()?;
        Ok(AllocationIrCompaction {
            values: value_map,
            instructions: instruction_map,
        })
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
                original: None,
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
                if self.blocks[block].successors.as_slice() != [successor] {
                    return Err(AllocationIrError::new(
                        "ALLOCATION_IR.EDGE_NOT_ISOLATED",
                        Some(predecessor),
                        None,
                        Vec::new(),
                        format!(
                            "phi-edge insertion requires a dedicated edge block to {successor}"
                        ),
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
                phi.original_phi != index
                    || phi.destination.0 >= self.original_value_count
                    || phi
                        .original_sources
                        .iter()
                        .any(|(_, value)| value.0 >= self.original_value_count)
                    || phi
                        .sources
                        .iter()
                        .any(|(_, value)| value.0 >= self.next_value)
                    || phi.register_sources.len() != phi.sources.len()
                    || (phi.register_definition == phi.stack_home.is_some())
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
                match instruction.origin {
                    AllocationInstructionOrigin::Original { .. } => {
                        let Some(original) = instruction.original.as_ref() else {
                            return Err(AllocationIrError::new(
                                "ALLOCATION_IR.ORIGINAL_SIGNATURE",
                                Some(block.id),
                                None,
                                instruction.uses.to_vec(),
                                "original instruction has no immutable source operand snapshot",
                            ));
                        };
                        if original
                            .uses()
                            .iter()
                            .any(|value| value.0 >= self.original_value_count)
                            || original
                                .def()
                                .is_some_and(|value| value.0 >= self.original_value_count)
                            || original.def() != instruction.definition
                        {
                            return Err(AllocationIrError::new(
                                "ALLOCATION_IR.ORIGINAL_SIGNATURE",
                                Some(block.id),
                                None,
                                original.uses().to_vec(),
                                "original instruction snapshot references a synthetic value",
                            ));
                        }
                    }
                    AllocationInstructionOrigin::Synthetic {
                        anchor, operation, ..
                    } => {
                        if instruction.original.is_some() {
                            return Err(AllocationIrError::new(
                                "ALLOCATION_IR.SYNTHETIC_SIGNATURE",
                                Some(block.id),
                                None,
                                instruction.uses.to_vec(),
                                "synthetic instruction carries a source-MIR operand snapshot",
                            ));
                        }
                        self.verify_operation(
                            anchor,
                            operation,
                            instruction.uses,
                            instruction.definition.is_some(),
                        )?;
                    }
                }
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

fn copy_operands(
    block: BlockId,
    instruction_index: usize,
    instruction: &AllocationInstruction,
) -> Result<Option<(VReg, VReg)>, AllocationIrError> {
    let operands = instruction.uses.to_vec();
    let ([source], Some(destination)) = (operands.as_slice(), instruction.definition) else {
        return Err(AllocationIrError::new(
            "ALLOCATION_IR.COPY_SIGNATURE",
            Some(block),
            Some(instruction_index),
            operands,
            "copy-class machine instruction does not have one source and one destination",
        ));
    };
    Ok(Some((*source, destination)))
}

fn insert_affinity(
    affinities: &mut BTreeSet<AllocationAffinity>,
    left: VReg,
    right: VReg,
    kind: AllocationAffinityKind,
) {
    if left == right {
        return;
    }
    let (left, right) = if left < right {
        (left, right)
    } else {
        (right, left)
    };
    affinities.insert(AllocationAffinity { left, right, kind });
}

fn rewrite_original_instruction(
    source: &MInst,
    original_uses: Uses,
    rewritten_uses: Uses,
    original_value_count: u32,
    block: BlockId,
    instruction: usize,
) -> Result<MInst, AllocationIrError> {
    if original_uses.len() != rewritten_uses.len() {
        return Err(AllocationIrError::new(
            "ALLOCATION_IR.REWRITE_ARITY",
            Some(block),
            Some(instruction),
            rewritten_uses.to_vec(),
            "allocation rewrite changed an original instruction's operand arity",
        ));
    }
    let mut replacements = BTreeMap::<VReg, VReg>::new();
    for (&original, &rewritten) in original_uses.iter().zip(rewritten_uses.iter()) {
        if rewritten != original && rewritten.0 < original_value_count {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.REWRITE_IDENTITY",
                Some(block),
                Some(instruction),
                vec![original, rewritten],
                "allocator-owned rewrite replaced one source-MIR value with another source-MIR value",
            ));
        }
        if let Some(previous) = replacements.insert(original, rewritten)
            && previous != rewritten
        {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.REWRITE_DUPLICATE",
                Some(block),
                Some(instruction),
                vec![original, previous, rewritten],
                "duplicate source operand was rewritten to two different allocation values",
            ));
        }
    }
    let mut lowered = source.clone();
    for (original, rewritten) in replacements {
        if original != rewritten {
            lowered.rewrite_use(original, rewritten);
        }
    }
    if lowered.uses() != rewritten_uses {
        return Err(AllocationIrError::new(
            "ALLOCATION_IR.REWRITE_EXACT",
            Some(block),
            Some(instruction),
            rewritten_uses.to_vec(),
            "MIR operand rewriting did not produce the exact allocation-IR use row",
        ));
    }
    Ok(lowered)
}

#[allow(clippy::too_many_arguments)]
fn materialize_synthetic_instruction(
    graph: &HomeGraph,
    stack_offsets: &[i32],
    recipe_definitions: &mut HashMap<VReg, (LiveBundleId, RecipeId)>,
    block: BlockId,
    instruction: usize,
    operation: SyntheticOperation,
    uses: Uses,
    definition: Option<VReg>,
) -> Result<MInst, AllocationIrError> {
    let operands = uses.to_vec();
    match operation {
        SyntheticOperation::StackStore { home } => {
            let [source] = operands.as_slice() else {
                return Err(synthetic_signature_error(
                    block,
                    instruction,
                    operation,
                    operands,
                ));
            };
            if definition.is_some() {
                return Err(synthetic_signature_error(
                    block,
                    instruction,
                    operation,
                    operands,
                ));
            }
            Ok(MInst::Store {
                base: BaseReg::StackFrame,
                offset: stack_offset(stack_offsets, home, block, instruction)?,
                src: *source,
                size: OpSize::S64,
            })
        }
        SyntheticOperation::StackReload { home } => {
            let ([], Some(destination)) = (operands.as_slice(), definition) else {
                return Err(synthetic_signature_error(
                    block,
                    instruction,
                    operation,
                    operands,
                ));
            };
            Ok(MInst::Load {
                dst: destination,
                base: BaseReg::StackFrame,
                offset: stack_offset(stack_offsets, home, block, instruction)?,
                size: OpSize::S64,
            })
        }
        SyntheticOperation::RecipeNode { root, node } => {
            if graph
                .bundles
                .get(root.0 as usize)
                .is_none_or(|bundle| bundle.id != root)
            {
                return Err(AllocationIrError::new(
                    "ALLOCATION_IR.RECIPE_ROOT",
                    Some(block),
                    Some(instruction),
                    operands,
                    "synthetic recipe references a missing HomeGraph root",
                ));
            }
            let recipe = graph.recipe_nodes.get(node.0 as usize).ok_or_else(|| {
                AllocationIrError::new(
                    "ALLOCATION_IR.RECIPE_NODE",
                    Some(block),
                    Some(instruction),
                    operands.clone(),
                    "synthetic recipe references a missing HomeGraph node",
                )
            })?;
            let destination = definition.ok_or_else(|| {
                synthetic_signature_error(block, instruction, operation, operands.clone())
            })?;
            let lowered = match recipe {
                RecipeNode::Constant(value) => {
                    if !operands.is_empty() {
                        return Err(synthetic_signature_error(
                            block,
                            instruction,
                            operation,
                            operands,
                        ));
                    }
                    MInst::LoadImm {
                        dst: destination,
                        value: *value,
                    }
                }
                RecipeNode::State(state) => {
                    if !operands.is_empty() {
                        return Err(synthetic_signature_error(
                            block,
                            instruction,
                            operation,
                            operands,
                        ));
                    }
                    MInst::Load {
                        dst: destination,
                        base: BaseReg::SimState,
                        offset: state.load.offset,
                        size: state.load.size,
                    }
                }
                RecipeNode::Unary {
                    operation: step,
                    input,
                } => {
                    let [source] = operands.as_slice() else {
                        return Err(synthetic_signature_error(
                            block,
                            instruction,
                            operation,
                            operands,
                        ));
                    };
                    verify_recipe_input(
                        recipe_definitions,
                        *source,
                        root,
                        *input,
                        block,
                        instruction,
                    )?;
                    materialize_pure_step(*step, destination, *source)
                }
                RecipeNode::Or64 { left, right } => {
                    let [lhs, rhs] = operands.as_slice() else {
                        return Err(synthetic_signature_error(
                            block,
                            instruction,
                            operation,
                            operands,
                        ));
                    };
                    verify_recipe_input(recipe_definitions, *lhs, root, *left, block, instruction)?;
                    verify_recipe_input(
                        recipe_definitions,
                        *rhs,
                        root,
                        *right,
                        block,
                        instruction,
                    )?;
                    MInst::Or {
                        dst: destination,
                        lhs: *lhs,
                        rhs: *rhs,
                    }
                }
            };
            if recipe_definitions
                .insert(destination, (root, node))
                .is_some()
            {
                return Err(AllocationIrError::new(
                    "ALLOCATION_IR.RECIPE_DEFINITION",
                    Some(block),
                    Some(instruction),
                    vec![destination],
                    "synthetic recipe value has more than one semantic definition",
                ));
            }
            Ok(lowered)
        }
    }
}

fn synthetic_signature_error(
    block: BlockId,
    instruction: usize,
    operation: SyntheticOperation,
    values: Vec<VReg>,
) -> AllocationIrError {
    AllocationIrError::new(
        "ALLOCATION_IR.SYNTHETIC_SIGNATURE",
        Some(block),
        Some(instruction),
        values,
        format!("invalid lowered def/use signature for {operation:?}"),
    )
}

fn stack_offset(
    offsets: &[i32],
    home: StackHomeId,
    block: BlockId,
    instruction: usize,
) -> Result<i32, AllocationIrError> {
    offsets.get(home.0 as usize).copied().ok_or_else(|| {
        AllocationIrError::new(
            "ALLOCATION_IR.STACK_LAYOUT",
            Some(block),
            Some(instruction),
            Vec::new(),
            format!("stack home {home:?} has no concrete frame offset"),
        )
    })
}

fn verify_recipe_input(
    definitions: &HashMap<VReg, (LiveBundleId, RecipeId)>,
    value: VReg,
    root: LiveBundleId,
    node: RecipeId,
    block: BlockId,
    instruction: usize,
) -> Result<(), AllocationIrError> {
    if definitions.get(&value) != Some(&(root, node)) {
        return Err(AllocationIrError::new(
            "ALLOCATION_IR.RECIPE_EDGE",
            Some(block),
            Some(instruction),
            vec![value],
            format!("recipe operand does not implement edge {root:?}/{node:?}"),
        ));
    }
    Ok(())
}

fn compact_value(
    map: &[Option<VReg>],
    value: VReg,
    block: BlockId,
) -> Result<VReg, AllocationIrError> {
    map.get(value.0 as usize).copied().flatten().ok_or_else(|| {
        AllocationIrError::new(
            "ALLOCATION_IR.DCE_LIVE_REFERENCE",
            Some(block),
            None,
            vec![value],
            "retained instruction references a removed synthetic value",
        )
    })
}

fn compact_uses(
    map: &[Option<VReg>],
    uses: Uses,
    block: BlockId,
) -> Result<Uses, AllocationIrError> {
    let values = uses
        .iter()
        .map(|value| compact_value(map, *value, block))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(match values.as_slice() {
        [] => Uses::none(),
        [a] => Uses::one(*a),
        [a, b] => Uses::two(*a, *b),
        [a, b, c] => Uses::three(*a, *b, *c),
        [a, b, c, d] => Uses::four(*a, *b, *c, *d),
        [a, b, c, d, e] => Uses::five(*a, *b, *c, *d, *e),
        _ => {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.DCE_USE_ARITY",
                Some(block),
                None,
                values,
                "allocation instruction exceeds the fixed MIR use arity",
            ));
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StackDefinition {
    False,
    True,
    Phi(usize),
}

#[derive(Debug)]
struct StackPhi {
    block: usize,
    home: StackHomeId,
    inputs: Vec<StackDefinition>,
}

#[derive(Debug)]
struct StackReloadQuery {
    block: BlockId,
    instruction: SyntheticInstructionId,
    home: StackHomeId,
    definition: StackDefinition,
}

fn verify_stack_home_reaching_definitions(
    program: &AllocationIr,
    cfg: &NormalizedCfg,
) -> Result<(), AllocationIrError> {
    let block_count = program.blocks.len();
    if cfg.predecessors.len() != block_count
        || cfg.successors.len() != block_count
        || cfg.idom.len() != block_count
        || cfg.dominance_frontier.len() != block_count
        || cfg.block_index.len() != block_count
        || (0..block_count)
            .any(|block| cfg.block_index.get(&program.blocks[block].id) != Some(&block))
        || cfg
            .predecessors
            .iter()
            .chain(&cfg.successors)
            .flatten()
            .any(|&block| block >= block_count)
        || cfg
            .dominance_frontier
            .iter()
            .flatten()
            .any(|&block| block >= block_count)
    {
        return Err(AllocationIrError::new(
            "ALLOCATION_IR.STACK_HOME_MODEL",
            program.blocks.first().map(|block| block.id),
            None,
            Vec::new(),
            "normalized CFG does not exactly cover stack-home operations",
        ));
    }

    let mut required_homes = BTreeSet::<StackHomeId>::new();
    let mut definition_blocks = HashMap::<StackHomeId, BTreeSet<usize>>::new();
    for (block, row) in program.blocks.iter().enumerate() {
        for phi in &row.phis {
            if let Some(home) = phi.stack_home {
                definition_blocks.entry(home).or_default().insert(block);
            }
        }
        for instruction in &row.instructions {
            let AllocationInstructionOrigin::Synthetic { operation, .. } = instruction.origin
            else {
                continue;
            };
            match operation {
                SyntheticOperation::StackStore { home } => {
                    definition_blocks.entry(home).or_default().insert(block);
                }
                SyntheticOperation::StackReload { home } => {
                    required_homes.insert(home);
                }
                SyntheticOperation::RecipeNode { .. } => {}
            }
        }
    }
    if required_homes.is_empty() {
        return Ok(());
    }

    let mut phis = Vec::<StackPhi>::new();
    let mut phis_by_block = vec![Vec::<(StackHomeId, usize)>::new(); block_count];
    for &home in &required_homes {
        // Function entry is the explicit false definition. Store definitions
        // and their iterated dominance frontiers form sparse Boolean SSA.
        let mut definitions = definition_blocks.get(&home).cloned().unwrap_or_default();
        definitions.insert(0);
        let mut queue = definitions.iter().copied().collect::<VecDeque<_>>();
        let mut placed = BTreeSet::<usize>::new();
        while let Some(definition) = queue.pop_front() {
            for &frontier in &cfg.dominance_frontier[definition] {
                if frontier == 0 || !placed.insert(frontier) {
                    continue;
                }
                let phi = phis.len();
                phis.push(StackPhi {
                    block: frontier,
                    home,
                    inputs: Vec::with_capacity(cfg.predecessors[frontier].len()),
                });
                phis_by_block[frontier].push((home, phi));
                if definitions.insert(frontier) {
                    queue.push_back(frontier);
                }
            }
        }
    }

    let mut children = vec![Vec::<usize>::new(); block_count];
    for block in 1..block_count {
        let Some(parent) = cfg.idom[block] else {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.STACK_HOME_DOMINANCE",
                Some(program.blocks[block].id),
                None,
                Vec::new(),
                "reachable non-entry block has no immediate dominator",
            ));
        };
        if parent >= block_count {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.STACK_HOME_DOMINANCE",
                Some(program.blocks[block].id),
                None,
                Vec::new(),
                "immediate dominator is outside the allocation IR",
            ));
        }
        children[parent].push(block);
    }

    enum Action {
        Enter(usize),
        Exit(Vec<(StackHomeId, Option<StackDefinition>)>),
    }

    let mut current = HashMap::<StackHomeId, StackDefinition>::new();
    let mut queries = Vec::<StackReloadQuery>::new();
    let mut visited = 0usize;
    let mut actions = vec![Action::Enter(0)];
    while let Some(action) = actions.pop() {
        let block = match action {
            Action::Exit(changes) => {
                for (home, previous) in changes.into_iter().rev() {
                    if let Some(previous) = previous {
                        current.insert(home, previous);
                    } else {
                        current.remove(&home);
                    }
                }
                continue;
            }
            Action::Enter(block) => block,
        };
        visited = visited.checked_add(1).ok_or_else(|| {
            AllocationIrError::new(
                "ALLOCATION_IR.STACK_HOME_DOMINANCE",
                Some(program.blocks[block].id),
                None,
                Vec::new(),
                "dominator traversal count exceeds usize",
            )
        })?;
        let mut changes = Vec::<(StackHomeId, Option<StackDefinition>)>::new();
        for &(home, phi) in &phis_by_block[block] {
            set_stack_definition(&mut current, &mut changes, home, StackDefinition::Phi(phi));
        }
        for phi in &program.blocks[block].phis {
            if let Some(home) = phi.stack_home
                && required_homes.contains(&home)
            {
                set_stack_definition(&mut current, &mut changes, home, StackDefinition::True);
            }
        }
        for instruction in &program.blocks[block].instructions {
            let AllocationInstructionOrigin::Synthetic { id, operation, .. } = instruction.origin
            else {
                continue;
            };
            match operation {
                SyntheticOperation::StackStore { home } if required_homes.contains(&home) => {
                    set_stack_definition(&mut current, &mut changes, home, StackDefinition::True);
                }
                SyntheticOperation::StackReload { home } => {
                    queries.push(StackReloadQuery {
                        block: program.blocks[block].id,
                        instruction: id,
                        home,
                        definition: current
                            .get(&home)
                            .copied()
                            .unwrap_or(StackDefinition::False),
                    });
                }
                SyntheticOperation::StackStore { .. } | SyntheticOperation::RecipeNode { .. } => {}
            }
        }
        for &successor in &cfg.successors[block] {
            for &(home, phi) in &phis_by_block[successor] {
                phis[phi].inputs.push(
                    current
                        .get(&home)
                        .copied()
                        .unwrap_or(StackDefinition::False),
                );
            }
        }
        actions.push(Action::Exit(changes));
        actions.extend(
            children[block]
                .iter()
                .rev()
                .map(|&child| Action::Enter(child)),
        );
    }
    if visited != block_count {
        return Err(AllocationIrError::new(
            "ALLOCATION_IR.STACK_HOME_DOMINANCE",
            None,
            None,
            Vec::new(),
            "dominator tree does not reach every allocation-IR block",
        ));
    }

    let mut users = vec![Vec::<usize>::new(); phis.len()];
    let mut false_phis = vec![false; phis.len()];
    let mut queue = VecDeque::<usize>::new();
    for (phi, node) in phis.iter().enumerate() {
        if node.inputs.len() != cfg.predecessors[node.block].len() {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.STACK_HOME_PHI_INPUTS",
                Some(program.blocks[node.block].id),
                None,
                Vec::new(),
                format!(
                    "stack home {:?} meet has {} inputs for {} predecessors",
                    node.home,
                    node.inputs.len(),
                    cfg.predecessors[node.block].len()
                ),
            ));
        }
        let mut is_false = node.inputs.is_empty();
        for &input in &node.inputs {
            match input {
                StackDefinition::False => is_false = true,
                StackDefinition::True => {}
                StackDefinition::Phi(definition) => users[definition].push(phi),
            }
        }
        if is_false {
            false_phis[phi] = true;
            queue.push_back(phi);
        }
    }
    while let Some(phi) = queue.pop_front() {
        for &user in &users[phi] {
            if !false_phis[user] {
                false_phis[user] = true;
                queue.push_back(user);
            }
        }
    }

    for query in queries {
        let initialized = match query.definition {
            StackDefinition::False => false,
            StackDefinition::True => true,
            StackDefinition::Phi(phi) => !false_phis[phi],
        };
        if !initialized {
            return Err(AllocationIrError::new(
                "ALLOCATION_IR.STACK_RELOAD_ALL_PATH_STORE",
                Some(query.block),
                Some(query.instruction.0 as usize),
                Vec::new(),
                format!(
                    "reload from {:?} is reachable without a prior same-home store",
                    query.home
                ),
            ));
        }
    }
    Ok(())
}

fn set_stack_definition(
    current: &mut HashMap<StackHomeId, StackDefinition>,
    changes: &mut Vec<(StackHomeId, Option<StackDefinition>)>,
    home: StackHomeId,
    definition: StackDefinition,
) {
    let previous = current.get(&home).copied();
    if previous == Some(definition) {
        return;
    }
    changes.push((home, previous));
    current.insert(home, definition);
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

    fn phi_definition_in_register(&self, block: usize, phi: usize) -> bool {
        self.blocks[block].phis[phi].register_definition
    }

    fn phi_sources(&self, block: usize, phi: usize) -> &[(BlockId, VReg)] {
        &self.blocks[block].phis[phi].sources
    }

    fn phi_source_in_register(&self, block: usize, phi: usize, source: usize) -> bool {
        self.blocks[block].phis[phi].register_sources[source]
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
    use crate::backend::native::mir::{
        BaseReg, MBlock, MInst, OpSize, PhiNode, SpillDesc, VRegAllocator,
    };

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
        let resolved_use = allocation_ir
            .resolve_original_use_site(use_site, &intervals)
            .unwrap();

        assert!(intervals.intervals[0].as_ref().unwrap().uses.is_empty());
        let reload_interval = intervals.intervals[reload.0 as usize].as_ref().unwrap();
        let recipe_interval = intervals.intervals[recipe.0 as usize].as_ref().unwrap();
        assert_eq!(reload_interval.uses.len(), 1);
        assert_eq!(recipe_interval.uses.len(), 1);
        assert!(reload_interval.definition.slot() < reload_interval.uses[0].slot());
        assert!(recipe_interval.definition.slot() < recipe_interval.uses[0].slot());
        assert!(reload_interval.uses[0].slot() < recipe_interval.uses[0].slot());
        assert_eq!(recipe_interval.uses, vec![resolved_use]);
        assert_ne!(resolved_use, use_site);
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
        let definition = original.intervals[1].as_ref().unwrap().definition;
        let edge = original.intervals[1]
            .as_ref()
            .unwrap()
            .uses
            .iter()
            .copied()
            .find(|site| matches!(site, UseSite::PhiEdge { .. }))
            .unwrap();
        let mut allocation_ir = AllocationIr::from_mir(&function).unwrap();
        allocation_ir
            .insert_after_definition(
                definition,
                SyntheticOperation::StackStore {
                    home: StackHomeId(0),
                },
                Uses::one(VReg(1)),
                false,
            )
            .unwrap();

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
        let resolved_edge = allocation_ir
            .resolve_original_use_site(edge, &intervals)
            .unwrap();
        let reload_interval = intervals.intervals[reload.0 as usize].as_ref().unwrap();

        assert_eq!(reload_interval.uses, vec![resolved_edge]);
        assert_ne!(resolved_edge.slot(), edge.slot());
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

        allocation_ir.verify_stack_homes(&cfg).unwrap();
        let graph = super::super::home_graph::build(&function, &cfg).unwrap();
        let source_before = format!("{function:?}");
        let lowered = allocation_ir.materialize(&function, &graph, &[0]).unwrap();
        assert_eq!(format!("{function:?}"), source_before);
        assert_eq!(
            super::super::live_interval::analyze(&lowered, &cfg).unwrap(),
            intervals
        );
        let UseSite::PhiEdge {
            predecessor,
            successor,
            phi,
            ..
        } = resolved_edge
        else {
            unreachable!()
        };
        let edge_block = lowered
            .blocks
            .iter()
            .find(|block| block.id == predecessor)
            .unwrap();
        assert!(matches!(
            edge_block.insts.as_slice(),
            [
                MInst::Load {
                    dst,
                    base: BaseReg::StackFrame,
                    offset: 0,
                    size: OpSize::S64,
                },
                MInst::Jump { target },
            ] if *dst == reload && *target == successor
        ));
        let merge = lowered
            .blocks
            .iter()
            .find(|block| block.id == successor)
            .unwrap();
        assert_eq!(
            merge.phis[phi]
                .sources
                .iter()
                .find(|(source, _)| *source == predecessor)
                .map(|(_, value)| *value),
            Some(reload)
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

    fn stack_home_diamond() -> (MFunction, NormalizedCfg, LiveIntervals) {
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: VReg(0),
            value: 1,
        });
        entry.push(MInst::Branch {
            cond: VReg(0),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        });
        let mut left = MBlock::new(BlockId(1));
        left.push(MInst::LoadImm {
            dst: VReg(1),
            value: 11,
        });
        left.push(MInst::Jump { target: BlockId(3) });
        let mut right = MBlock::new(BlockId(2));
        right.push(MInst::LoadImm {
            dst: VReg(2),
            value: 13,
        });
        right.push(MInst::Jump { target: BlockId(3) });
        let mut merge = MBlock::new(BlockId(3));
        merge.phis.push(PhiNode {
            dst: VReg(3),
            sources: vec![(BlockId(1), VReg(1)), (BlockId(2), VReg(2))],
        });
        merge.push(MInst::Mov {
            dst: VReg(4),
            src: VReg(3),
        });
        merge.push(MInst::Return);
        let mut function = function(5, vec![entry, left, right, merge]);
        let cfg = normalize(&mut function);
        let intervals = super::super::live_interval::analyze(&function, &cfg).unwrap();
        (function, cfg, intervals)
    }

    #[test]
    fn stack_home_stores_on_every_arm_reach_a_join_reload() {
        let (function, cfg, intervals) = stack_home_diamond();
        let mut allocation_ir = AllocationIr::from_mir(&function).unwrap();
        for value in [VReg(1), VReg(2)] {
            allocation_ir
                .insert_after_definition(
                    intervals.intervals[value.0 as usize]
                        .as_ref()
                        .unwrap()
                        .definition,
                    SyntheticOperation::StackStore {
                        home: StackHomeId(0),
                    },
                    Uses::one(value),
                    false,
                )
                .unwrap();
        }
        allocation_ir
            .insert_before_use(
                intervals.intervals[3].as_ref().unwrap().uses[0],
                SyntheticOperation::StackReload {
                    home: StackHomeId(0),
                },
                Uses::none(),
                true,
            )
            .unwrap();

        allocation_ir.analyze(&cfg).unwrap();
        allocation_ir.verify_stack_homes(&cfg).unwrap();
    }

    #[test]
    fn one_arm_store_does_not_establish_a_join_stack_home() {
        let (function, cfg, intervals) = stack_home_diamond();
        let mut allocation_ir = AllocationIr::from_mir(&function).unwrap();
        allocation_ir
            .insert_after_definition(
                intervals.intervals[1].as_ref().unwrap().definition,
                SyntheticOperation::StackStore {
                    home: StackHomeId(0),
                },
                Uses::one(VReg(1)),
                false,
            )
            .unwrap();
        allocation_ir
            .insert_before_use(
                intervals.intervals[3].as_ref().unwrap().uses[0],
                SyntheticOperation::StackReload {
                    home: StackHomeId(0),
                },
                Uses::none(),
                true,
            )
            .unwrap();

        let error = allocation_ir.verify_stack_homes(&cfg).unwrap_err();
        assert_eq!(error.rule, "ALLOCATION_IR.STACK_RELOAD_ALL_PATH_STORE");
    }

    #[test]
    fn a_later_same_block_store_does_not_reach_an_earlier_reload() {
        let mut function = straight_line();
        let cfg = normalize(&mut function);
        let intervals = super::super::live_interval::analyze(&function, &cfg).unwrap();
        let mut allocation_ir = AllocationIr::from_mir(&function).unwrap();
        allocation_ir
            .insert_before_use(
                intervals.intervals[0].as_ref().unwrap().uses[0],
                SyntheticOperation::StackReload {
                    home: StackHomeId(0),
                },
                Uses::none(),
                true,
            )
            .unwrap();
        allocation_ir
            .insert_after_definition(
                intervals.intervals[1].as_ref().unwrap().definition,
                SyntheticOperation::StackStore {
                    home: StackHomeId(0),
                },
                Uses::one(VReg(1)),
                false,
            )
            .unwrap();

        let error = allocation_ir.verify_stack_homes(&cfg).unwrap_err();
        assert_eq!(error.rule, "ALLOCATION_IR.STACK_RELOAD_ALL_PATH_STORE");
    }
}
