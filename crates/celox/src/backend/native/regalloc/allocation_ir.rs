//! Off-to-the-side machine-value IR for allocation-owned splitting.
//!
//! Home selection introduces real stores, reloads, and recipe operations.  All
//! values defined by those operations must participate in the same exact
//! liveness and physical allocation as original MIR values.  This IR records
//! them against immutable original-MIR anchors without mutating `MFunction`;
//! successful allocation can later lower the complete result atomically.

use std::collections::{BTreeSet, HashMap, VecDeque};
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
                queue.extend(phi.sources.iter().map(|(_, value)| *value));
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
