//! Exact stack-home liveness and sparse stack-slot coloring.
//!
//! A spill home is a location-level SSA value: an explicit store or a
//! stack-resident phi defines it, reloads and direct phi-edge locations use
//! it. Reusing a frame offset is therefore an ordinary interference-coloring
//! decision over the same CFG-sparse interval model as machine VRegs, not a
//! lifetime approximation based on instruction layout or last reloads.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt;

use crate::backend::native::mir::{BlockId, MFunction, SpillKind, Uses, VReg};

use super::allocation_expand::{
    ExpandedAllocationProblem, ExpandedEdgeLocation, ExpandedStackDefinition,
    ExpandedStackHomeKind, ExpandedUseSource,
};
use super::allocation_ir::{
    AllocationIrError, AllocationStackOperationKind, StackHomeId, SyntheticInstructionId,
};
use super::cfg::NormalizedCfg;
use super::interval_union::{AllocationBundleId, DynamicIntervalMatrix, IntervalUnionError};
use super::live_interval::{
    BlockSlots, LiveIntervalError, LiveIntervals, LiveSegment, LivenessProgram, SlotIndex, UseSite,
    analyze_program,
};
use super::spill_plan::{LogicalValue, PlannedOp, SpillHome, SpillPlan};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StackColoring {
    pub offsets: Vec<i32>,
    pub frame_size: u32,
    pub slot_count: usize,
    pub intervals: LiveIntervals,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StackColorError {
    pub rule: &'static str,
    pub block: Option<BlockId>,
    pub instruction: Option<usize>,
    pub homes: Vec<StackHomeId>,
    pub values: Vec<VReg>,
    pub message: String,
}

impl StackColorError {
    fn new(
        rule: &'static str,
        block: Option<BlockId>,
        instruction: Option<usize>,
        homes: impl IntoIterator<Item = StackHomeId>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            block,
            instruction,
            homes: homes.into_iter().collect(),
            values: Vec::new(),
            message: message.into(),
        }
    }

    fn ir(error: AllocationIrError) -> Self {
        Self {
            rule: error.rule,
            block: error.block,
            instruction: error.instruction,
            homes: Vec::new(),
            values: error.values,
            message: error.message,
        }
    }

    fn live(error: LiveIntervalError) -> Self {
        Self::new(
            error.rule,
            error.block,
            error.instruction,
            error.values.into_iter().map(|value| StackHomeId(value.0)),
            error.message,
        )
    }

    fn union(error: IntervalUnionError) -> Self {
        Self::new(
            error.rule,
            error.block,
            None,
            error
                .bundles
                .into_iter()
                .map(|bundle| StackHomeId(bundle.0)),
            error.message,
        )
    }

    fn state_home(error: super::ssa_state_home::StateHomeError) -> Self {
        Self {
            rule: error.rule,
            block: error.block,
            instruction: error.instruction,
            homes: Vec::new(),
            values: error.values,
            message: error.message,
        }
    }
}

impl fmt::Display for StackColorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.rule)?;
        if let Some(block) = self.block {
            write!(formatter, " at {block}")?;
        }
        if let Some(instruction) = self.instruction {
            write!(formatter, "/i{instruction}")?;
        }
        if !self.homes.is_empty() {
            write!(formatter, " homes={:?}", self.homes)?;
        }
        if !self.values.is_empty() {
            write!(formatter, " values={:?}", self.values)?;
        }
        write!(formatter, ": {}", self.message)
    }
}

impl std::error::Error for StackColorError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StackInstruction {
    uses: Uses,
    definition: Option<VReg>,
}

impl Default for StackInstruction {
    fn default() -> Self {
        Self {
            uses: Uses::none(),
            definition: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StackPhi {
    home: VReg,
    sources: Vec<(BlockId, VReg)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct StackEdgeUse {
    predecessor: BlockId,
    home: VReg,
    phi: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StackBlock {
    id: BlockId,
    phis: Vec<StackPhi>,
    instructions: Vec<StackInstruction>,
    edge_uses: Vec<StackEdgeUse>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StackLivenessProgram {
    home_count: u32,
    blocks: Vec<StackBlock>,
    block_slots: Vec<BlockSlots>,
}

impl StackLivenessProgram {
    fn build(
        expanded: &ExpandedAllocationProblem,
        cfg: &NormalizedCfg,
    ) -> Result<Self, StackColorError> {
        let home_count = u32::try_from(expanded.stack_homes.len()).map_err(|_| {
            StackColorError::new(
                "STACK_COLOR.HOME_ID_RANGE",
                None,
                None,
                [],
                "stack-home count exceeds the liveness VReg domain",
            )
        })?;
        for (index, home) in expanded.stack_homes.iter().enumerate() {
            if home.id.0 as usize != index {
                return Err(StackColorError::new(
                    "STACK_COLOR.HOME_IDENTITY",
                    None,
                    None,
                    [home.id],
                    "expanded stack homes are not a dense identity-ordered domain",
                ));
            }
        }

        let facts = expanded.ir.stack_facts().map_err(StackColorError::ir)?;
        let synthetic_instruction_index = expanded
            .ir
            .index_synthetic_instructions()
            .map_err(StackColorError::ir)?;
        if facts.blocks.len() != cfg.successors.len()
            || facts
                .blocks
                .iter()
                .enumerate()
                .any(|(index, (block, _))| cfg.block_index.get(block).copied() != Some(index))
        {
            return Err(StackColorError::new(
                "STACK_COLOR.CFG_SHAPE",
                facts.blocks.first().map(|(block, _)| *block),
                None,
                [],
                "allocation stack facts do not cover the normalized CFG in block order",
            ));
        }
        let mut blocks = facts
            .blocks
            .iter()
            .map(|&(id, instruction_count)| StackBlock {
                id,
                phis: Vec::new(),
                instructions: vec![StackInstruction::default(); instruction_count],
                edge_uses: Vec::new(),
            })
            .collect::<Vec<_>>();

        let mut expected_stores = BTreeMap::<SyntheticInstructionId, (StackHomeId, VReg)>::new();
        let mut expected_phis = BTreeMap::<(BlockId, usize), (StackHomeId, VReg)>::new();
        for home in &expanded.stack_homes {
            match home.definition {
                ExpandedStackDefinition::Store { instruction, value } => {
                    if expected_stores
                        .insert(instruction, (home.id, value))
                        .is_some()
                    {
                        return Err(StackColorError::new(
                            "STACK_COLOR.DEFINITION_IDENTITY",
                            None,
                            None,
                            [home.id],
                            "two stack homes claim the same synthetic store definition",
                        ));
                    }
                    expanded
                        .ir
                        .resolve_stack_store_use_site_indexed(
                            instruction,
                            home.id,
                            value,
                            &expanded.intervals,
                            &synthetic_instruction_index,
                        )
                        .map_err(StackColorError::ir)?;
                }
                ExpandedStackDefinition::Phi {
                    block,
                    phi,
                    destination,
                } => {
                    if expected_phis
                        .insert((block, phi), (home.id, destination))
                        .is_some()
                    {
                        return Err(StackColorError::new(
                            "STACK_COLOR.DEFINITION_IDENTITY",
                            Some(block),
                            None,
                            [home.id],
                            "two stack homes claim the same phi definition",
                        ));
                    }
                    expanded
                        .ir
                        .verify_phi_stack_definition(block, phi, destination, home.id)
                        .map_err(StackColorError::ir)?;
                }
            }
        }

        let mut definition_count = vec![0_u8; expanded.stack_homes.len()];
        let mut seen_operations = BTreeSet::new();
        for operation in facts.operations {
            let home_index = operation.home.0 as usize;
            if home_index >= expanded.stack_homes.len() {
                return Err(StackColorError::new(
                    "STACK_COLOR.HOME_RANGE",
                    Some(operation.block),
                    Some(operation.position),
                    [operation.home],
                    "allocation stack operation references a missing home",
                ));
            }
            if !seen_operations.insert(operation.instruction) {
                return Err(StackColorError::new(
                    "STACK_COLOR.OPERATION_IDENTITY",
                    Some(operation.block),
                    Some(operation.position),
                    [operation.home],
                    "synthetic stack-operation identity occurs more than once",
                ));
            }
            let block_index = cfg.block_index[&operation.block];
            let row = blocks[block_index]
                .instructions
                .get_mut(operation.position)
                .ok_or_else(|| {
                    StackColorError::new(
                        "STACK_COLOR.INSTRUCTION_RANGE",
                        Some(operation.block),
                        Some(operation.position),
                        [operation.home],
                        "stack operation is outside the allocation-IR instruction layout",
                    )
                })?;
            if row.uses != Uses::none() || row.definition.is_some() {
                return Err(StackColorError::new(
                    "STACK_COLOR.OPERATION_POSITION",
                    Some(operation.block),
                    Some(operation.position),
                    [operation.home],
                    "two stack operations occupy one allocation instruction",
                ));
            }
            match operation.kind {
                AllocationStackOperationKind::Store => {
                    let Some((expected_home, _)) = expected_stores.remove(&operation.instruction)
                    else {
                        return Err(StackColorError::new(
                            "STACK_COLOR.STORE_DEFINITION",
                            Some(operation.block),
                            Some(operation.position),
                            [operation.home],
                            "allocation IR contains a stack store without expanded-home ownership",
                        ));
                    };
                    if expected_home != operation.home {
                        return Err(StackColorError::new(
                            "STACK_COLOR.STORE_DEFINITION",
                            Some(operation.block),
                            Some(operation.position),
                            [expected_home, operation.home],
                            "expanded-home metadata and allocation stack store disagree",
                        ));
                    }
                    definition_count[home_index] =
                        definition_count[home_index].checked_add(1).ok_or_else(|| {
                            StackColorError::new(
                                "STACK_COLOR.MULTIPLE_DEFINITIONS",
                                Some(operation.block),
                                Some(operation.position),
                                [operation.home],
                                "stack home has too many location definitions",
                            )
                        })?;
                    row.definition = Some(VReg(operation.home.0));
                }
                AllocationStackOperationKind::Reload => {
                    row.uses = Uses::one(VReg(operation.home.0));
                }
            }
        }
        if let Some((&instruction, &(home, _))) = expected_stores.first_key_value() {
            return Err(StackColorError::new(
                "STACK_COLOR.STORE_DEFINITION",
                None,
                None,
                [home],
                format!("expanded stack home references missing store {instruction:?}"),
            ));
        }

        for definition in facts.phi_definitions {
            let home_index = definition.home.0 as usize;
            if home_index >= expanded.stack_homes.len() {
                return Err(StackColorError::new(
                    "STACK_COLOR.HOME_RANGE",
                    Some(definition.block),
                    None,
                    [definition.home],
                    "stack-resident phi references a missing home",
                ));
            }
            let Some((expected_home, expected_destination)) =
                expected_phis.remove(&(definition.block, definition.phi))
            else {
                return Err(StackColorError::new(
                    "STACK_COLOR.PHI_DEFINITION",
                    Some(definition.block),
                    None,
                    [definition.home],
                    "allocation IR contains a stack phi without expanded-home ownership",
                ));
            };
            if expected_home != definition.home || expected_destination != definition.destination {
                return Err(StackColorError::new(
                    "STACK_COLOR.PHI_DEFINITION",
                    Some(definition.block),
                    None,
                    [expected_home, definition.home],
                    "expanded-home metadata and allocation stack phi disagree",
                ));
            }
            definition_count[home_index] =
                definition_count[home_index].checked_add(1).ok_or_else(|| {
                    StackColorError::new(
                        "STACK_COLOR.MULTIPLE_DEFINITIONS",
                        Some(definition.block),
                        None,
                        [definition.home],
                        "stack home has too many location definitions",
                    )
                })?;
            let block_index = cfg.block_index[&definition.block];
            let sources = cfg.predecessors[block_index]
                .iter()
                .map(|&predecessor| (blocks[predecessor].id, VReg(definition.home.0)))
                .collect();
            blocks[block_index].phis.push(StackPhi {
                home: VReg(definition.home.0),
                sources,
            });
        }
        if let Some((&(block, _), &(home, _))) = expected_phis.first_key_value() {
            return Err(StackColorError::new(
                "STACK_COLOR.PHI_DEFINITION",
                Some(block),
                None,
                [home],
                "expanded stack home references a missing stack-resident phi",
            ));
        }
        if let Some((home, _)) = definition_count
            .iter()
            .enumerate()
            .find(|(_, count)| **count != 1)
        {
            return Err(StackColorError::new(
                "STACK_COLOR.DEFINITION_COVERAGE",
                None,
                None,
                [StackHomeId(home as u32)],
                "every stack home must have exactly one location-level SSA definition",
            ));
        }

        let mut edge_use_owners = BTreeSet::new();
        let mut edge_recipe_uses = BTreeSet::new();
        for root in &expanded.roots {
            for use_ in &root.uses {
                let ExpandedUseSource::Edge(ExpandedEdgeLocation::Stack { home }) = use_.source
                else {
                    continue;
                };
                if home.0 >= home_count {
                    return Err(StackColorError::new(
                        "STACK_COLOR.HOME_RANGE",
                        Some(use_.site.block()),
                        None,
                        [home],
                        "direct phi-edge source references a missing stack home",
                    ));
                }
                let UseSite::PhiEdge {
                    predecessor,
                    successor,
                    phi,
                    ..
                } = use_.site
                else {
                    return Err(StackColorError::new(
                        "STACK_COLOR.EDGE_USE_SITE",
                        Some(use_.site.block()),
                        None,
                        [home],
                        "direct stack location is attached to an instruction use",
                    ));
                };
                let Some(&successor_index) = cfg.block_index.get(&successor) else {
                    return Err(StackColorError::new(
                        "STACK_COLOR.EDGE_USE_CFG",
                        Some(successor),
                        None,
                        [home],
                        "direct stack location references a missing successor",
                    ));
                };
                if !edge_use_owners.insert((predecessor, successor, phi)) {
                    return Err(StackColorError::new(
                        "STACK_COLOR.EDGE_USE_IDENTITY",
                        Some(predecessor),
                        None,
                        [home],
                        "one stack home claims the same direct phi-edge use more than once",
                    ));
                }
                blocks[successor_index].edge_uses.push(StackEdgeUse {
                    predecessor,
                    home: VReg(home.0),
                    phi,
                });
                edge_recipe_uses.insert((root.id, use_.id, home));
            }
        }
        for use_ in &expanded.machine_edge_uses {
            let home_index = use_.home.0 as usize;
            let Some(home) = expanded.stack_homes.get(home_index) else {
                return Err(StackColorError::new(
                    "STACK_COLOR.HOME_RANGE",
                    Some(use_.site.block()),
                    None,
                    [use_.home],
                    "machine phi-edge source references a missing stack home",
                ));
            };
            if home.root != use_.root
                || home.kind != (ExpandedStackHomeKind::Machine { value: use_.value })
            {
                return Err(StackColorError::new(
                    "STACK_COLOR.MACHINE_EDGE_HOME",
                    Some(use_.site.block()),
                    None,
                    [use_.home],
                    "machine phi-edge source and conventional spill home disagree",
                ));
            }
            let UseSite::PhiEdge {
                predecessor,
                successor,
                phi,
                ..
            } = use_.site
            else {
                return Err(StackColorError::new(
                    "STACK_COLOR.EDGE_USE_SITE",
                    Some(use_.site.block()),
                    None,
                    [use_.home],
                    "machine edge stack location is attached to an instruction use",
                ));
            };
            let Some(&successor_index) = cfg.block_index.get(&successor) else {
                return Err(StackColorError::new(
                    "STACK_COLOR.EDGE_USE_CFG",
                    Some(successor),
                    None,
                    [use_.home],
                    "machine edge stack location references a missing successor",
                ));
            };
            if !edge_use_owners.insert((predecessor, successor, phi)) {
                return Err(StackColorError::new(
                    "STACK_COLOR.EDGE_USE_IDENTITY",
                    Some(predecessor),
                    None,
                    [use_.home],
                    "one phi source has more than one direct stack location",
                ));
            }
            blocks[successor_index].edge_uses.push(StackEdgeUse {
                predecessor,
                home: VReg(use_.home.0),
                phi,
            });
        }
        for home in &expanded.stack_homes {
            if let ExpandedStackHomeKind::EdgeRecipe { use_id } = home.kind
                && !edge_recipe_uses.contains(&(home.root, use_id, home.id))
            {
                return Err(StackColorError::new(
                    "STACK_COLOR.EDGE_HOME_USE",
                    None,
                    None,
                    [home.id],
                    "edge-recipe stack home is not consumed by its exact expanded phi use",
                ));
            }
        }
        for block in &mut blocks {
            block.edge_uses.sort_unstable();
        }

        Ok(Self {
            home_count,
            blocks,
            block_slots: expanded.intervals.block_slots.clone(),
        })
    }
}

impl LivenessProgram for StackLivenessProgram {
    fn value_count(&self) -> u32 {
        self.home_count
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
        self.blocks[block].phis[phi].home
    }

    fn phi_sources(&self, block: usize, phi: usize) -> &[(BlockId, VReg)] {
        &self.blocks[block].phis[phi].sources
    }

    fn phi_source_in_register(&self, _block: usize, _phi: usize, _source: usize) -> bool {
        false
    }

    fn extra_phi_edge_use_count(&self, successor: usize) -> usize {
        self.blocks[successor].edge_uses.len()
    }

    fn extra_phi_edge_use(&self, successor: usize, edge_use: usize) -> (BlockId, VReg, usize) {
        let use_ = self.blocks[successor].edge_uses[edge_use];
        (use_.predecessor, use_.home, use_.phi)
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

    fn block_entry_slot(&self, block: usize) -> Option<SlotIndex> {
        self.block_slots.get(block).map(|slots| slots.entry)
    }

    fn phi_definition_slot(&self, block: usize) -> Option<SlotIndex> {
        self.block_slots.get(block).map(|slots| slots.phi_def)
    }

    fn instruction_use_slot(&self, block: usize, instruction: usize) -> Option<SlotIndex> {
        self.block_slots.get(block)?.instruction_use(instruction)
    }

    fn block_exit_slot(&self, block: usize) -> Option<SlotIndex> {
        self.block_slots.get(block).map(|slots| slots.exit)
    }
}

/// Colored production spill homes consumed directly by SSA reconstruction.
///
/// `SpillHome` identifiers are sparse VReg representatives, so the public
/// result is a map instead of the dense allocation-IR vector used above.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlannedStackColoring {
    pub offsets: HashMap<SpillHome, i32>,
    pub frame_size: u32,
    pub slot_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlannedStackEventKind {
    Store,
    Reload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlannedStackEvent {
    instruction: usize,
    sequence: usize,
    home: SpillHome,
    kind: PlannedStackEventKind,
    definition: Option<VReg>,
    reaching: Option<VReg>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedStackPhi {
    home: SpillHome,
    destination: VReg,
    sources: Vec<(BlockId, VReg)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedStackLivenessProgram {
    blocks: Vec<StackBlock>,
    version_homes: Vec<SpillHome>,
}

impl LivenessProgram for PlannedStackLivenessProgram {
    fn value_count(&self) -> u32 {
        u32::try_from(self.version_homes.len())
            .expect("planned stack SSA construction checked the VReg domain")
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
        self.blocks[block].phis[phi].home
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

fn planned_live_error(error: LiveIntervalError, version_homes: &[SpillHome]) -> StackColorError {
    let homes = error
        .values
        .iter()
        .filter_map(|value| version_homes.get(value.0 as usize))
        .map(|home| StackHomeId(home.0))
        .collect();
    StackColorError {
        rule: error.rule,
        block: error.block,
        instruction: error.instruction,
        homes,
        values: error.values,
        message: error.message,
    }
}

fn planned_union_error(error: IntervalUnionError, bundle_homes: &[SpillHome]) -> StackColorError {
    let homes = error
        .bundles
        .iter()
        .filter_map(|bundle| bundle_homes.get(bundle.0 as usize))
        .map(|home| StackHomeId(home.0))
        .collect();
    StackColorError {
        rule: error.rule,
        block: error.block,
        instruction: None,
        homes,
        values: Vec::new(),
        message: error.message,
    }
}

fn rematerialized_logical(func: &MFunction, value: LogicalValue) -> bool {
    func.spill_desc(VReg(value.0))
        .is_some_and(|desc| matches!(desc.kind, SpillKind::Remat { .. }))
}

fn rematerialized_home(func: &MFunction, plan: &SpillPlan, home: SpillHome) -> bool {
    let mut value = None;
    for member in plan.homes.members(home) {
        let Some(desc) = func.spill_desc(member) else {
            return false;
        };
        let SpillKind::Remat {
            value: member_value,
        } = desc.kind
        else {
            return false;
        };
        if value.is_some_and(|value| value != member_value) {
            return false;
        }
        value = Some(member_value);
    }
    value.is_some()
}

fn is_stack_store(func: &MFunction, plan: &SpillPlan, home: SpillHome) -> bool {
    !plan.recipe_homes.contains(&home)
        && !plan.state_homes.contains_key(&home)
        && !rematerialized_home(func, plan, home)
}

fn is_point_stack_reload(
    func: &MFunction,
    plan: &SpillPlan,
    block: BlockId,
    instruction: usize,
    value: LogicalValue,
    home: SpillHome,
) -> bool {
    !rematerialized_logical(func, value)
        && !plan.recipe_homes.contains(&home)
        && !plan.state_homes.contains_key(&home)
        && !plan.recipe_reloads.contains(&(block, instruction, value))
}

fn is_edge_stack_reload(
    func: &MFunction,
    plan: &SpillPlan,
    value: LogicalValue,
    home: SpillHome,
) -> bool {
    !rematerialized_logical(func, value)
        && !plan.recipe_homes.contains(&home)
        && !plan.state_homes.contains_key(&home)
}

fn collect_planned_stack_events(
    func: &MFunction,
    cfg: &NormalizedCfg,
    plan: &SpillPlan,
) -> Result<(Vec<Vec<PlannedStackEvent>>, BTreeSet<SpillHome>), StackColorError> {
    if func.blocks.len() != cfg.predecessors.len()
        || func.blocks.len() != cfg.successors.len()
        || func.blocks.len() != cfg.idom.len()
    {
        return Err(StackColorError::new(
            "STACK_COLOR.PLANNED_CFG_SHAPE",
            func.blocks.first().map(|block| block.id),
            None,
            [],
            "normalized CFG does not cover every production spill-plan block",
        ));
    }
    let mut events = vec![Vec::<PlannedStackEvent>::new(); func.blocks.len()];
    let mut homes = BTreeSet::<SpillHome>::new();
    let mut sequence = 0usize;

    for spill in super::ssa_state_home::planned_spills(func, cfg, plan)
        .map_err(StackColorError::state_home)?
    {
        if !is_stack_store(func, plan, spill.home) {
            continue;
        }
        let Some(block) = func.blocks.get(spill.block) else {
            return Err(StackColorError::new(
                "STACK_COLOR.PLANNED_STORE_BLOCK",
                None,
                Some(spill.instruction),
                [StackHomeId(spill.home.0)],
                "materialized spill references a block outside the function",
            ));
        };
        if spill.instruction >= block.insts.len() {
            return Err(StackColorError::new(
                "STACK_COLOR.PLANNED_STORE_POINT",
                Some(block.id),
                Some(spill.instruction),
                [StackHomeId(spill.home.0)],
                "materialized spill is not before an existing MIR instruction",
            ));
        }
        homes.insert(spill.home);
        events[spill.block].push(PlannedStackEvent {
            instruction: spill.instruction,
            sequence,
            home: spill.home,
            kind: PlannedStackEventKind::Store,
            definition: None,
            reaching: None,
        });
        sequence = sequence.checked_add(1).ok_or_else(|| {
            StackColorError::new(
                "STACK_COLOR.PLANNED_EVENT_RANGE",
                Some(block.id),
                Some(spill.instruction),
                [StackHomeId(spill.home.0)],
                "production stack-event count exceeds usize",
            )
        })?;
    }

    for &(point, operation) in &plan.point_ops {
        let PlannedOp::Reload { value, home } = operation else {
            continue;
        };
        if !is_point_stack_reload(func, plan, point.block, point.instruction, value, home) {
            continue;
        }
        let Some(&block) = cfg.block_index.get(&point.block) else {
            return Err(StackColorError::new(
                "STACK_COLOR.PLANNED_RELOAD_BLOCK",
                Some(point.block),
                Some(point.instruction),
                [StackHomeId(home.0)],
                "stack reload references a block outside the normalized CFG",
            ));
        };
        if point.instruction >= func.blocks[block].insts.len() {
            return Err(StackColorError::new(
                "STACK_COLOR.PLANNED_RELOAD_POINT",
                Some(point.block),
                Some(point.instruction),
                [StackHomeId(home.0)],
                "stack reload is not before an existing MIR instruction",
            ));
        }
        events[block].push(PlannedStackEvent {
            instruction: point.instruction,
            sequence,
            home,
            kind: PlannedStackEventKind::Reload,
            definition: None,
            reaching: None,
        });
        sequence = sequence.checked_add(1).ok_or_else(|| {
            StackColorError::new(
                "STACK_COLOR.PLANNED_EVENT_RANGE",
                Some(point.block),
                Some(point.instruction),
                [StackHomeId(home.0)],
                "production stack-event count exceeds usize",
            )
        })?;
    }

    for (&(predecessor, successor), operations) in &plan.edge_ops {
        let Some(predecessor_block) = func.blocks.get(predecessor) else {
            return Err(StackColorError::new(
                "STACK_COLOR.PLANNED_EDGE_BLOCK",
                None,
                None,
                [],
                "stack edge reload references a predecessor outside the function",
            ));
        };
        let insertion = super::cfg::edge_insertion_point(func, cfg, predecessor, successor)
            .ok_or_else(|| {
                StackColorError::new(
                    "STACK_COLOR.PLANNED_EDGE_POINT",
                    Some(predecessor_block.id),
                    None,
                    [],
                    "stack edge reload has no single-edge materialization point",
                )
            })?;
        let block = &func.blocks[insertion.block];
        for &operation in operations {
            let PlannedOp::Reload { value, home } = operation else {
                continue;
            };
            if !is_edge_stack_reload(func, plan, value, home) {
                continue;
            }
            events[insertion.block].push(PlannedStackEvent {
                instruction: insertion.instruction,
                sequence,
                home,
                kind: PlannedStackEventKind::Reload,
                definition: None,
                reaching: None,
            });
            sequence = sequence.checked_add(1).ok_or_else(|| {
                StackColorError::new(
                    "STACK_COLOR.PLANNED_EVENT_RANGE",
                    Some(block.id),
                    Some(insertion.instruction),
                    [StackHomeId(home.0)],
                    "production stack-event count exceeds usize",
                )
            })?;
        }
    }

    for block_events in &mut events {
        block_events.sort_unstable_by_key(|event| {
            let phase = match event.kind {
                PlannedStackEventKind::Store => 0u8,
                PlannedStackEventKind::Reload => 1u8,
            };
            (event.instruction, phase, event.sequence)
        });
    }
    Ok((events, homes))
}

fn allocate_planned_version(
    version_homes: &mut Vec<SpillHome>,
    home: SpillHome,
) -> Result<VReg, StackColorError> {
    let value = VReg(u32::try_from(version_homes.len()).map_err(|_| {
        StackColorError::new(
            "STACK_COLOR.PLANNED_VERSION_RANGE",
            None,
            None,
            [StackHomeId(home.0)],
            "production stack MemorySSA exceeds the VReg identifier domain",
        )
    })?);
    version_homes.push(home);
    Ok(value)
}

fn build_planned_stack_program(
    func: &MFunction,
    cfg: &NormalizedCfg,
    plan: &SpillPlan,
) -> Result<(PlannedStackLivenessProgram, BTreeSet<SpillHome>), StackColorError> {
    let (events, homes) = collect_planned_stack_events(func, cfg, plan)?;
    build_planned_stack_program_from_events(func, cfg, events, homes)
}

fn build_planned_stack_program_from_events(
    func: &MFunction,
    cfg: &NormalizedCfg,
    mut events: Vec<Vec<PlannedStackEvent>>,
    homes: BTreeSet<SpillHome>,
) -> Result<(PlannedStackLivenessProgram, BTreeSet<SpillHome>), StackColorError> {
    if events.len() != func.blocks.len() {
        return Err(StackColorError::new(
            "STACK_COLOR.PLANNED_EVENT_SHAPE",
            func.blocks.first().map(|block| block.id),
            None,
            [],
            "production stack-event rows do not cover every MIR block",
        ));
    }
    if homes.is_empty() {
        return Ok((
            PlannedStackLivenessProgram {
                blocks: func
                    .blocks
                    .iter()
                    .map(|block| StackBlock {
                        id: block.id,
                        phis: Vec::new(),
                        instructions: Vec::new(),
                        edge_uses: Vec::new(),
                    })
                    .collect(),
                version_homes: Vec::new(),
            },
            homes,
        ));
    }

    // Sparse pruned MemorySSA.  `definitions` and `live_in` contain only
    // relations which actually occur; no home-by-block matrix is built.
    let mut definitions = HashSet::<(SpillHome, usize)>::new();
    let mut upward_uses = HashSet::<(SpillHome, usize)>::new();
    for (block, block_events) in events.iter().enumerate() {
        let mut locally_defined = HashSet::<SpillHome>::new();
        for event in block_events {
            match event.kind {
                PlannedStackEventKind::Store => {
                    locally_defined.insert(event.home);
                    definitions.insert((event.home, block));
                }
                PlannedStackEventKind::Reload => {
                    if !locally_defined.contains(&event.home) {
                        upward_uses.insert((event.home, block));
                    }
                }
            }
        }
    }
    let mut live_in = upward_uses.clone();
    let mut live_work = upward_uses.iter().copied().collect::<VecDeque<_>>();
    while let Some((home, block)) = live_work.pop_front() {
        for &predecessor in &cfg.predecessors[block] {
            let relation = (home, predecessor);
            if !definitions.contains(&relation) && live_in.insert(relation) {
                live_work.push_back(relation);
            }
        }
    }

    let mut phi_relations = HashSet::<(SpillHome, usize)>::new();
    let mut queued = definitions.clone();
    let mut phi_work = definitions.iter().copied().collect::<Vec<_>>();
    while let Some((home, block)) = phi_work.pop() {
        for &frontier in &cfg.dominance_frontier[block] {
            let relation = (home, frontier);
            if frontier == 0 || !live_in.contains(&relation) || !phi_relations.insert(relation) {
                continue;
            }
            if queued.insert(relation) {
                phi_work.push(relation);
            }
        }
    }

    let mut version_homes = Vec::<SpillHome>::new();
    let mut phis_by_block = vec![Vec::<PlannedStackPhi>::new(); func.blocks.len()];
    let mut ordered_phis = phi_relations.into_iter().collect::<Vec<_>>();
    ordered_phis.sort_unstable_by_key(|(home, block)| (*block, *home));
    for (home, block) in ordered_phis {
        let destination = allocate_planned_version(&mut version_homes, home)?;
        phis_by_block[block].push(PlannedStackPhi {
            home,
            destination,
            sources: Vec::with_capacity(cfg.predecessors[block].len()),
        });
    }
    for block_events in &mut events {
        for event in block_events {
            if matches!(event.kind, PlannedStackEventKind::Store) {
                event.definition = Some(allocate_planned_version(&mut version_homes, event.home)?);
            }
        }
    }

    let mut children = vec![Vec::<usize>::new(); func.blocks.len()];
    for block in 1..func.blocks.len() {
        let parent = cfg.idom[block].ok_or_else(|| {
            StackColorError::new(
                "STACK_COLOR.PLANNED_DOMINATOR_TREE",
                Some(func.blocks[block].id),
                None,
                [],
                "reachable production spill block has no immediate dominator",
            )
        })?;
        children[parent].push(block);
    }
    enum RenameAction {
        Enter(usize),
        Exit(Vec<(SpillHome, Option<VReg>)>),
    }
    let mut current = HashMap::<SpillHome, VReg>::new();
    let mut actions = vec![RenameAction::Enter(0)];
    while let Some(action) = actions.pop() {
        let block = match action {
            RenameAction::Exit(changes) => {
                for (home, previous) in changes.into_iter().rev() {
                    if let Some(previous) = previous {
                        current.insert(home, previous);
                    } else {
                        current.remove(&home);
                    }
                }
                continue;
            }
            RenameAction::Enter(block) => block,
        };
        let mut changes = Vec::<(SpillHome, Option<VReg>)>::new();
        for phi in &phis_by_block[block] {
            changes.push((phi.home, current.insert(phi.home, phi.destination)));
        }
        for event in &mut events[block] {
            match event.kind {
                PlannedStackEventKind::Store => {
                    let definition = event
                        .definition
                        .expect("every production stack store has one MemorySSA definition");
                    changes.push((event.home, current.insert(event.home, definition)));
                }
                PlannedStackEventKind::Reload => {
                    event.reaching = current.get(&event.home).copied();
                    if event.reaching.is_none() {
                        return Err(StackColorError::new(
                            "STACK_COLOR.PLANNED_RELOAD_REACHING_STORE",
                            Some(func.blocks[block].id),
                            Some(event.instruction),
                            [StackHomeId(event.home.0)],
                            "stack reload has no reaching store in sparse MemorySSA",
                        ));
                    }
                }
            }
        }
        for &successor in &cfg.successors[block] {
            for phi in &mut phis_by_block[successor] {
                let Some(&source) = current.get(&phi.home) else {
                    return Err(StackColorError::new(
                        "STACK_COLOR.PLANNED_PHI_REACHING_STORE",
                        Some(func.blocks[successor].id),
                        None,
                        [StackHomeId(phi.home.0)],
                        format!(
                            "stack MemorySSA phi has no reaching store from {}",
                            func.blocks[block].id
                        ),
                    ));
                };
                phi.sources.push((func.blocks[block].id, source));
            }
        }
        actions.push(RenameAction::Exit(changes));
        actions.extend(
            children[block]
                .iter()
                .rev()
                .copied()
                .map(RenameAction::Enter),
        );
    }

    for (block, phis) in phis_by_block.iter_mut().enumerate() {
        for phi in phis {
            phi.sources
                .sort_unstable_by_key(|(predecessor, _)| cfg.block_index[predecessor]);
            if phi.sources.len() != cfg.predecessors[block].len()
                || phi.sources.iter().zip(&cfg.predecessors[block]).any(
                    |((actual, _), expected)| {
                        cfg.block_index.get(actual).copied() != Some(*expected)
                    },
                )
            {
                return Err(StackColorError::new(
                    "STACK_COLOR.PLANNED_PHI_INPUTS",
                    Some(func.blocks[block].id),
                    None,
                    [StackHomeId(phi.home.0)],
                    "stack MemorySSA phi does not cover every CFG predecessor exactly once",
                ));
            }
        }
    }

    let blocks = func
        .blocks
        .iter()
        .enumerate()
        .map(|(block, mir_block)| StackBlock {
            id: mir_block.id,
            phis: phis_by_block[block]
                .iter()
                .map(|phi| StackPhi {
                    home: phi.destination,
                    sources: phi.sources.clone(),
                })
                .collect(),
            instructions: events[block]
                .iter()
                .map(|event| match event.kind {
                    PlannedStackEventKind::Store => StackInstruction {
                        uses: Uses::none(),
                        definition: event.definition,
                    },
                    PlannedStackEventKind::Reload => StackInstruction {
                        uses: Uses::one(
                            event
                                .reaching
                                .expect("verified production stack reload has a reaching version"),
                        ),
                        definition: None,
                    },
                })
                .collect(),
            edge_uses: Vec::new(),
        })
        .collect();
    Ok((
        PlannedStackLivenessProgram {
            blocks,
            version_homes,
        },
        homes,
    ))
}

fn merge_home_segments(
    intervals: &LiveIntervals,
    version_homes: &[SpillHome],
    homes: &BTreeSet<SpillHome>,
) -> Result<BTreeMap<SpillHome, Vec<LiveSegment>>, StackColorError> {
    if intervals.intervals.len() != version_homes.len() {
        return Err(StackColorError::new(
            "STACK_COLOR.PLANNED_INTERVAL_SHAPE",
            None,
            None,
            [],
            "stack MemorySSA interval and version tables have different lengths",
        ));
    }
    let mut ranges = homes
        .iter()
        .copied()
        .map(|home| (home, Vec::<LiveSegment>::new()))
        .collect::<BTreeMap<_, _>>();
    for (version, (&home, interval)) in version_homes.iter().zip(&intervals.intervals).enumerate() {
        let interval = interval.as_ref().ok_or_else(|| {
            StackColorError::new(
                "STACK_COLOR.PLANNED_INTERVAL_COVERAGE",
                None,
                None,
                [StackHomeId(home.0)],
                format!("stack MemorySSA version v{version} has no exact live interval"),
            )
        })?;
        let Some(range) = ranges.get_mut(&home) else {
            return Err(StackColorError::new(
                "STACK_COLOR.PLANNED_HOME_COVERAGE",
                Some(interval.definition.block()),
                None,
                [StackHomeId(home.0)],
                "stack MemorySSA version belongs to a non-materialized home",
            ));
        };
        range.extend(interval.segments.iter().copied());
    }
    for (&home, range) in &mut ranges {
        range.sort_unstable_by_key(|segment| (segment.block, segment.start, segment.end));
        let mut merged = Vec::<LiveSegment>::with_capacity(range.len());
        for segment in std::mem::take(range) {
            if let Some(previous) = merged.last_mut()
                && previous.block == segment.block
                && segment.start <= previous.end
            {
                previous.end = previous.end.max(segment.end);
            } else {
                merged.push(segment);
            }
        }
        if merged.is_empty() {
            return Err(StackColorError::new(
                "STACK_COLOR.PLANNED_HOME_RANGE",
                None,
                None,
                [StackHomeId(home.0)],
                "materialized stack home has no occupied range",
            ));
        }
        *range = merged;
    }
    Ok(ranges)
}

/// Color the stack slots emitted by the production W/S spill plan.
///
/// The plan's mutable homes are first converted to pruned sparse MemorySSA.
/// Strict-SSA liveness is then analyzed and independently verified by the
/// ordinary interval engine. Versions of one physical `SpillHome` are unioned
/// only after that proof, preserving the existing one-home/one-offset
/// reconstruction contract while allowing noninterfering homes to share.
pub(super) fn color_spill_plan(
    func: &MFunction,
    cfg: &NormalizedCfg,
    plan: &SpillPlan,
) -> Result<PlannedStackColoring, StackColorError> {
    let (program, homes) = build_planned_stack_program(func, cfg, plan)?;
    color_planned_stack_program(program, homes, cfg)
}

fn color_planned_stack_program(
    program: PlannedStackLivenessProgram,
    homes: BTreeSet<SpillHome>,
    cfg: &NormalizedCfg,
) -> Result<PlannedStackColoring, StackColorError> {
    if homes.is_empty() {
        return Ok(PlannedStackColoring {
            offsets: HashMap::new(),
            frame_size: 0,
            slot_count: 0,
        });
    }
    let intervals = analyze_program(&program, cfg)
        .map_err(|error| planned_live_error(error, &program.version_homes))?;
    intervals
        .verify_program(&program, cfg)
        .map_err(|error| planned_live_error(error, &program.version_homes))?;
    let ranges = merge_home_segments(&intervals, &program.version_homes, &homes)?;
    let bundle_homes = homes.iter().copied().collect::<Vec<_>>();
    let mut bundle_ranges = Vec::with_capacity(bundle_homes.len());
    for &home in &bundle_homes {
        bundle_ranges.push(ranges[&home].clone());
    }

    // General live-range unions are not necessarily chordal after all
    // MemorySSA versions of a physical home are coalesced. Largest ranges
    // first is the conventional bounded greedy choice; exact interference
    // checks still come from the sparse interval matrix.
    let mut order = (0..bundle_homes.len())
        .map(|bundle| {
            let segments = &bundle_ranges[bundle];
            let length = segments.iter().fold(0u128, |total, segment| {
                total.saturating_add(u128::from(
                    segment.start.distance_to(segment.end).unwrap_or(u64::MAX),
                ))
            });
            (bundle, length, segments.len())
        })
        .collect::<Vec<_>>();
    order.sort_unstable_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| bundle_homes[left.0].cmp(&bundle_homes[right.0]))
    });

    let mut matrix = DynamicIntervalMatrix::new(cfg)
        .map_err(|error| planned_union_error(error, &bundle_homes))?;
    for (bundle, _, _) in order {
        let range = matrix
            .make_range(bundle_ranges[bundle].clone())
            .map_err(|error| planned_union_error(error, &bundle_homes))?;
        let slot = matrix
            .first_available_validated(range.validated())
            .map_err(|error| planned_union_error(error, &bundle_homes))?;
        matrix
            .assign_validated(AllocationBundleId(bundle as u32), slot, range.validated())
            .map_err(|error| planned_union_error(error, &bundle_homes))?;
    }
    matrix
        .verify()
        .map_err(|error| planned_union_error(error, &bundle_homes))?;

    // Rebuild from the final immutable assignment, independently of the
    // mutation order used by first-fit coloring.
    let mut rebuilt = DynamicIntervalMatrix::new(cfg)
        .map_err(|error| planned_union_error(error, &bundle_homes))?;
    let mut rebuild_order = (0..bundle_homes.len())
        .map(|bundle| {
            matrix
                .slot(AllocationBundleId(bundle as u32))
                .map(|slot| (slot, bundle))
                .ok_or_else(|| {
                    StackColorError::new(
                        "STACK_COLOR.PLANNED_ASSIGNMENT_COVERAGE",
                        None,
                        None,
                        [StackHomeId(bundle_homes[bundle].0)],
                        "production stack home has no colored frame slot",
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    rebuild_order.sort_unstable();
    for (slot, bundle) in rebuild_order {
        let range = rebuilt
            .make_range(bundle_ranges[bundle].clone())
            .map_err(|error| planned_union_error(error, &bundle_homes))?;
        rebuilt
            .assign_validated(AllocationBundleId(bundle as u32), slot, range.validated())
            .map_err(|error| planned_union_error(error, &bundle_homes))?;
    }
    rebuilt
        .verify()
        .map_err(|error| planned_union_error(error, &bundle_homes))?;
    if rebuilt != matrix {
        return Err(StackColorError::new(
            "STACK_COLOR.PLANNED_MATRIX_IDENTITY",
            None,
            None,
            [],
            "rebuilt production stack-slot matrix differs from completed coloring",
        ));
    }

    let mut offsets = HashMap::with_capacity(bundle_homes.len());
    for (bundle, &home) in bundle_homes.iter().enumerate() {
        let slot = matrix
            .slot(AllocationBundleId(bundle as u32))
            .ok_or_else(|| {
                StackColorError::new(
                    "STACK_COLOR.PLANNED_ASSIGNMENT_COVERAGE",
                    None,
                    None,
                    [StackHomeId(home.0)],
                    "production stack home has no colored frame slot",
                )
            })?;
        let offset = slot
            .checked_mul(8)
            .and_then(|bytes| i32::try_from(bytes).ok())
            .ok_or_else(|| {
                StackColorError::new(
                    "STACK_COLOR.PLANNED_FRAME_RANGE",
                    None,
                    None,
                    [StackHomeId(home.0)],
                    "colored production stack offset exceeds MIR's signed domain",
                )
            })?;
        offsets.insert(home, offset);
    }
    let slot_count = matrix.slot_count();
    let frame_size = slot_count
        .checked_mul(8)
        .and_then(|bytes| u32::try_from(bytes).ok())
        .ok_or_else(|| {
            StackColorError::new(
                "STACK_COLOR.PLANNED_FRAME_RANGE",
                None,
                None,
                [],
                "colored production stack frame exceeds u32",
            )
        })?;
    Ok(PlannedStackColoring {
        offsets,
        frame_size,
        slot_count,
    })
}

pub(super) fn color(
    expanded: &ExpandedAllocationProblem,
    cfg: &NormalizedCfg,
) -> Result<StackColoring, StackColorError> {
    let program = StackLivenessProgram::build(expanded, cfg)?;
    let intervals = analyze_program(&program, cfg).map_err(StackColorError::live)?;
    // Re-run the verifier from the exported interval object. This does not
    // trust construction worklists or their live-in/live-out sets.
    intervals
        .verify_program(&program, cfg)
        .map_err(StackColorError::live)?;
    if intervals.block_slots != expanded.intervals.block_slots {
        return Err(StackColorError::new(
            "STACK_COLOR.SLOT_IDENTITY",
            None,
            None,
            [],
            "stack-home and machine-value liveness use different allocation-IR layouts",
        ));
    }

    let order = definition_order(&intervals, cfg)?;
    let mut matrix = DynamicIntervalMatrix::new(cfg).map_err(StackColorError::union)?;
    for home in order {
        let interval = intervals.intervals[home].as_ref().ok_or_else(|| {
            StackColorError::new(
                "STACK_COLOR.INTERVAL_COVERAGE",
                None,
                None,
                [StackHomeId(home as u32)],
                "defined stack home has no exact live interval",
            )
        })?;
        let range = matrix
            .make_range(interval.segments.clone())
            .map_err(StackColorError::union)?;
        let slot = matrix
            .first_available_validated(range.validated())
            .map_err(StackColorError::union)?;
        matrix
            .assign_validated(AllocationBundleId(home as u32), slot, range.validated())
            .map_err(StackColorError::union)?;
    }
    matrix.verify().map_err(StackColorError::union)?;

    // Rebuild every sparse union from final home->slot assignments. This
    // independently checks that no mutable coloring operation left a stale
    // membership or accepted an interfering pair.
    let mut rebuilt = DynamicIntervalMatrix::new(cfg).map_err(StackColorError::union)?;
    let mut rebuild_order = Vec::with_capacity(intervals.intervals.len());
    for (home, interval) in intervals.intervals.iter().enumerate() {
        let interval = interval.as_ref().ok_or_else(|| {
            StackColorError::new(
                "STACK_COLOR.INTERVAL_COVERAGE",
                None,
                None,
                [StackHomeId(home as u32)],
                "defined stack home has no exact live interval",
            )
        })?;
        let bundle = AllocationBundleId(home as u32);
        let slot = matrix.slot(bundle).ok_or_else(|| {
            StackColorError::new(
                "STACK_COLOR.ASSIGNMENT_COVERAGE",
                Some(interval.definition.block()),
                None,
                [StackHomeId(home as u32)],
                "stack home has no colored frame slot",
            )
        })?;
        rebuild_order.push((slot, home));
    }
    rebuild_order.sort_unstable();
    for (slot, home) in rebuild_order {
        let interval = intervals.intervals[home]
            .as_ref()
            .expect("rebuild order contains only verified stack intervals");
        let bundle = AllocationBundleId(home as u32);
        let range = rebuilt
            .make_range(interval.segments.clone())
            .map_err(StackColorError::union)?;
        rebuilt
            .assign_validated(bundle, slot, range.validated())
            .map_err(StackColorError::union)?;
    }
    rebuilt.verify().map_err(StackColorError::union)?;
    if rebuilt != matrix {
        return Err(StackColorError::new(
            "STACK_COLOR.MATRIX_IDENTITY",
            None,
            None,
            [],
            "rebuilt sparse stack-slot matrix differs from the completed coloring",
        ));
    }

    let mut offsets = Vec::with_capacity(intervals.intervals.len());
    for home in 0..intervals.intervals.len() {
        let slot = matrix
            .slot(AllocationBundleId(home as u32))
            .ok_or_else(|| {
                StackColorError::new(
                    "STACK_COLOR.ASSIGNMENT_COVERAGE",
                    None,
                    None,
                    [StackHomeId(home as u32)],
                    "stack home has no colored frame slot",
                )
            })?;
        let bytes = slot.checked_mul(8).ok_or_else(|| {
            StackColorError::new(
                "STACK_COLOR.FRAME_RANGE",
                None,
                None,
                [StackHomeId(home as u32)],
                "stack slot byte offset exceeds usize",
            )
        })?;
        offsets.push(i32::try_from(bytes).map_err(|_| {
            StackColorError::new(
                "STACK_COLOR.FRAME_RANGE",
                None,
                None,
                [StackHomeId(home as u32)],
                "stack slot byte offset exceeds MIR's signed offset domain",
            )
        })?);
    }
    let slot_count = matrix.slot_count();
    let frame_size = slot_count
        .checked_mul(8)
        .and_then(|bytes| u32::try_from(bytes).ok())
        .ok_or_else(|| {
            StackColorError::new(
                "STACK_COLOR.FRAME_RANGE",
                None,
                None,
                [],
                "colored stack frame exceeds u32",
            )
        })?;
    Ok(StackColoring {
        offsets,
        frame_size,
        slot_count,
        intervals,
    })
}

fn definition_order(
    intervals: &LiveIntervals,
    cfg: &NormalizedCfg,
) -> Result<Vec<usize>, StackColorError> {
    if cfg.idom.is_empty() || cfg.idom[0].is_some() {
        return Err(StackColorError::new(
            "STACK_COLOR.DOMINATOR_SHAPE",
            None,
            None,
            [],
            "stack coloring requires a dominator tree rooted at CFG entry",
        ));
    }
    let mut children = vec![Vec::new(); cfg.idom.len()];
    for (block, parent) in cfg.idom.iter().copied().enumerate().skip(1) {
        let parent = parent.ok_or_else(|| {
            StackColorError::new(
                "STACK_COLOR.DOMINATOR_SHAPE",
                None,
                None,
                [],
                format!("reachable block {block} has no immediate dominator"),
            )
        })?;
        let Some(row) = children.get_mut(parent) else {
            return Err(StackColorError::new(
                "STACK_COLOR.DOMINATOR_SHAPE",
                None,
                None,
                [],
                format!("block {block} has out-of-range dominator {parent}"),
            ));
        };
        row.push(block);
    }
    for row in &mut children {
        row.sort_unstable();
    }
    let mut rank = vec![usize::MAX; cfg.idom.len()];
    let mut stack = vec![0_usize];
    let mut next = 0_usize;
    while let Some(block) = stack.pop() {
        if rank[block] != usize::MAX {
            return Err(StackColorError::new(
                "STACK_COLOR.DOMINATOR_SHAPE",
                None,
                None,
                [],
                "dominator tree contains a cycle or duplicate child",
            ));
        }
        rank[block] = next;
        next += 1;
        stack.extend(children[block].iter().rev().copied());
    }
    if next != cfg.idom.len() {
        return Err(StackColorError::new(
            "STACK_COLOR.DOMINATOR_SHAPE",
            None,
            None,
            [],
            "dominator tree does not reach every CFG block",
        ));
    }

    let mut order = Vec::with_capacity(intervals.intervals.len());
    for (home, interval) in intervals.intervals.iter().enumerate() {
        let interval = interval.as_ref().ok_or_else(|| {
            StackColorError::new(
                "STACK_COLOR.INTERVAL_COVERAGE",
                None,
                None,
                [StackHomeId(home as u32)],
                "stack-home liveness omitted a defined home",
            )
        })?;
        let block = cfg.block_index[&interval.definition.block()];
        order.push((rank[block], interval.definition.slot(), home));
    }
    order.sort_unstable();
    Ok(order.into_iter().map(|(_, _, home)| home).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::features::VariableShiftEncoding;
    use crate::backend::native::mir::{
        BaseReg, MBlock, MFunction, MInst, OpSize, PhiNode, SpillDesc, VRegAllocator,
    };

    use super::super::allocation_expand::{
        ExpandedRoot, ExpandedStackHome, ExpandedStackHomeKind, ExpandedUse,
    };
    use super::super::allocation_ir::{AllocationIr, SyntheticOperation};
    use super::super::cfg;
    use super::super::home_graph::{BundleUseId, LiveBundleId};
    use super::super::live_interval;

    fn function(value_count: u32, instructions: Vec<MInst>) -> MFunction {
        let mut values = VRegAllocator::new();
        for _ in 0..value_count {
            values.alloc();
        }
        let mut function =
            MFunction::new(values, vec![SpillDesc::transient(); value_count as usize]);
        let mut block = MBlock::new(BlockId(0));
        block.insts = instructions;
        function.blocks.push(block);
        function
    }

    fn spill_values(
        source: &MFunction,
        cfg: &NormalizedCfg,
        values: &[VReg],
    ) -> ExpandedAllocationProblem {
        let source_intervals = live_interval::analyze(source, cfg).unwrap();
        let mut ir = AllocationIr::from_mir(source).unwrap();
        let mut stack_homes = Vec::new();
        for (index, &value) in values.iter().enumerate() {
            let interval = source_intervals.intervals[value.0 as usize]
                .as_ref()
                .unwrap();
            let [use_site] = interval.uses.as_slice() else {
                panic!("test value must have one exact use")
            };
            let home = StackHomeId(index as u32);
            let store = ir
                .insert_after_definition(
                    interval.definition,
                    SyntheticOperation::StackStore { home },
                    Uses::one(value),
                    false,
                )
                .unwrap()
                .instruction;
            let reload = ir
                .insert_before_use(
                    *use_site,
                    SyntheticOperation::StackReload { home },
                    Uses::none(),
                    true,
                )
                .unwrap()
                .definition
                .unwrap();
            ir.rewrite_use(*use_site, value, reload).unwrap();
            stack_homes.push(ExpandedStackHome {
                id: home,
                root: LiveBundleId(index as u32),
                definition: ExpandedStackDefinition::Store {
                    instruction: store,
                    value,
                },
                kind: ExpandedStackHomeKind::Root,
            });
        }
        let intervals = ir.analyze(cfg).unwrap();
        let incremental_liveness =
            live_interval::IncrementalLiveness::build(&ir, cfg, &intervals).unwrap();
        ExpandedAllocationProblem {
            ir,
            intervals,
            incremental_liveness,
            shift_encoding: VariableShiftEncoding::Bmi2,
            roots: Vec::new(),
            machine_edge_uses: Vec::new(),
            register_regions: Vec::new(),
            region_rows: BTreeMap::new(),
            region_by_value: BTreeMap::new(),
            next_register_region: 0,
            stack_homes,
            state_homes: Vec::new(),
        }
    }

    fn color_planned_events(
        func: &MFunction,
        cfg: &NormalizedCfg,
        rows: &[(BlockId, Vec<(PlannedStackEventKind, SpillHome)>)],
    ) -> Result<PlannedStackColoring, StackColorError> {
        let mut events = vec![Vec::new(); func.blocks.len()];
        let mut homes = BTreeSet::new();
        let mut sequence = 0usize;
        for (block, row) in rows {
            let block = cfg.block_index[block];
            for (instruction, &(kind, home)) in row.iter().enumerate() {
                if matches!(kind, PlannedStackEventKind::Store) {
                    homes.insert(home);
                }
                events[block].push(PlannedStackEvent {
                    instruction,
                    sequence,
                    home,
                    kind,
                    definition: None,
                    reaching: None,
                });
                sequence += 1;
            }
        }
        let (program, homes) = build_planned_stack_program_from_events(func, cfg, events, homes)?;
        color_planned_stack_program(program, homes, cfg)
    }

    fn diamond_function() -> MFunction {
        let mut values = VRegAllocator::new();
        let condition = values.alloc();
        let mut function = MFunction::new(values, vec![SpillDesc::transient()]);
        let mut entry = MBlock::new(BlockId(0));
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
        join.push(MInst::Return);
        function.blocks = vec![entry, left, right, join];
        function
    }

    #[test]
    fn production_sequential_homes_reuse_one_slot() {
        let mut source = function(0, vec![MInst::Return]);
        let cfg = cfg::normalize(&mut source).unwrap();
        let coloring = color_planned_events(
            &source,
            &cfg,
            &[(
                BlockId(0),
                vec![
                    (PlannedStackEventKind::Store, SpillHome(0)),
                    (PlannedStackEventKind::Reload, SpillHome(0)),
                    (PlannedStackEventKind::Store, SpillHome(1)),
                    (PlannedStackEventKind::Reload, SpillHome(1)),
                ],
            )],
        )
        .unwrap();

        assert_eq!(coloring.slot_count, 1);
        assert_eq!(coloring.frame_size, 8);
        assert_eq!(coloring.offsets[&SpillHome(0)], 0);
        assert_eq!(coloring.offsets[&SpillHome(1)], 0);
    }

    #[test]
    fn production_overlapping_homes_keep_distinct_slots() {
        let mut source = function(0, vec![MInst::Return]);
        let cfg = cfg::normalize(&mut source).unwrap();
        let coloring = color_planned_events(
            &source,
            &cfg,
            &[(
                BlockId(0),
                vec![
                    (PlannedStackEventKind::Store, SpillHome(0)),
                    (PlannedStackEventKind::Store, SpillHome(1)),
                    (PlannedStackEventKind::Reload, SpillHome(0)),
                    (PlannedStackEventKind::Reload, SpillHome(1)),
                ],
            )],
        )
        .unwrap();

        assert_eq!(coloring.slot_count, 2);
        assert_eq!(coloring.frame_size, 16);
        assert_ne!(
            coloring.offsets[&SpillHome(0)],
            coloring.offsets[&SpillHome(1)]
        );
    }

    #[test]
    fn production_mutually_exclusive_arms_share_one_slot() {
        let mut source = diamond_function();
        let cfg = cfg::normalize(&mut source).unwrap();
        let coloring = color_planned_events(
            &source,
            &cfg,
            &[
                (
                    BlockId(1),
                    vec![
                        (PlannedStackEventKind::Store, SpillHome(0)),
                        (PlannedStackEventKind::Reload, SpillHome(0)),
                    ],
                ),
                (
                    BlockId(2),
                    vec![
                        (PlannedStackEventKind::Store, SpillHome(1)),
                        (PlannedStackEventKind::Reload, SpillHome(1)),
                    ],
                ),
            ],
        )
        .unwrap();

        assert_eq!(coloring.slot_count, 1);
        assert_eq!(coloring.offsets[&SpillHome(0)], 0);
        assert_eq!(coloring.offsets[&SpillHome(1)], 0);
    }

    #[test]
    fn production_loop_carried_home_interferes_with_body_home() {
        let mut values = VRegAllocator::new();
        let condition = values.alloc();
        let mut source = MFunction::new(values, vec![SpillDesc::transient()]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::LoadImm {
            dst: condition,
            value: 1,
        });
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut header = MBlock::new(BlockId(1));
        header.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId(2),
            false_bb: BlockId(3),
        });
        let mut body = MBlock::new(BlockId(2));
        body.push(MInst::Jump { target: BlockId(1) });
        let mut exit = MBlock::new(BlockId(3));
        exit.push(MInst::Return);
        source.blocks = vec![entry, header, body, exit];
        let cfg = cfg::normalize(&mut source).unwrap();
        let coloring = color_planned_events(
            &source,
            &cfg,
            &[
                (
                    BlockId(0),
                    vec![(PlannedStackEventKind::Store, SpillHome(0))],
                ),
                (
                    BlockId(1),
                    vec![(PlannedStackEventKind::Reload, SpillHome(0))],
                ),
                (
                    BlockId(2),
                    vec![
                        (PlannedStackEventKind::Store, SpillHome(1)),
                        (PlannedStackEventKind::Reload, SpillHome(1)),
                    ],
                ),
            ],
        )
        .unwrap();

        assert_eq!(coloring.slot_count, 2);
        assert_ne!(
            coloring.offsets[&SpillHome(0)],
            coloring.offsets[&SpillHome(1)]
        );
    }

    #[test]
    fn production_one_arm_store_cannot_satisfy_join_reload() {
        let mut source = diamond_function();
        let cfg = cfg::normalize(&mut source).unwrap();
        let error = color_planned_events(
            &source,
            &cfg,
            &[
                (
                    BlockId(1),
                    vec![(PlannedStackEventKind::Store, SpillHome(0))],
                ),
                (
                    BlockId(3),
                    vec![(PlannedStackEventKind::Reload, SpillHome(0))],
                ),
            ],
        )
        .unwrap_err();

        assert_eq!(error.rule, "STACK_COLOR.PLANNED_PHI_REACHING_STORE");
    }

    #[test]
    fn noninterfering_stack_homes_share_one_exact_sparse_slot() {
        let mut source = function(
            4,
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 1,
                },
                MInst::Mov {
                    dst: VReg(1),
                    src: VReg(0),
                },
                MInst::LoadImm {
                    dst: VReg(2),
                    value: 2,
                },
                MInst::Mov {
                    dst: VReg(3),
                    src: VReg(2),
                },
                MInst::Return,
            ],
        );
        let cfg = cfg::normalize(&mut source).unwrap();
        let expanded = spill_values(&source, &cfg, &[VReg(0), VReg(2)]);

        let coloring = color(&expanded, &cfg).unwrap();

        assert_eq!(coloring.offsets, vec![0, 0]);
        assert_eq!(coloring.slot_count, 1);
        assert_eq!(coloring.frame_size, 8);
    }

    #[test]
    fn interfering_stack_homes_receive_distinct_sparse_slots() {
        let mut source = function(
            3,
            vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 1,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 2,
                },
                MInst::Add {
                    dst: VReg(2),
                    lhs: VReg(0),
                    rhs: VReg(1),
                },
                MInst::Return,
            ],
        );
        let cfg = cfg::normalize(&mut source).unwrap();
        let expanded = spill_values(&source, &cfg, &[VReg(0), VReg(1)]);

        let coloring = color(&expanded, &cfg).unwrap();

        assert_eq!(coloring.offsets, vec![0, 8]);
        assert_eq!(coloring.slot_count, 2);
        assert_eq!(coloring.frame_size, 16);
    }

    #[test]
    fn mutually_exclusive_direct_phi_edge_homes_share_one_slot() {
        let mut values = VRegAllocator::new();
        for _ in 0..4 {
            values.alloc();
        }
        let mut source = MFunction::new(values, vec![SpillDesc::transient(); 4]);
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::Load {
            dst: VReg(0),
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
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
            value: 22,
        });
        right.push(MInst::Jump { target: BlockId(3) });
        let mut merge = MBlock::new(BlockId(3));
        merge.phis.push(PhiNode {
            dst: VReg(3),
            sources: vec![(BlockId(1), VReg(1)), (BlockId(2), VReg(2))],
        });
        merge.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: VReg(3),
            size: OpSize::S64,
        });
        merge.push(MInst::Return);
        source.blocks = vec![entry, left, right, merge];

        let cfg = cfg::normalize(&mut source).unwrap();
        let source_intervals = live_interval::analyze(&source, &cfg).unwrap();
        let mut ir = AllocationIr::from_mir(&source).unwrap();
        let mut homes = Vec::new();
        let mut original_sites = Vec::new();
        for (home_index, value) in [VReg(1), VReg(2)].into_iter().enumerate() {
            let interval = source_intervals.intervals[value.0 as usize]
                .as_ref()
                .unwrap();
            let [site] = interval.uses.as_slice() else {
                panic!("arm value must have one phi-edge use")
            };
            assert!(matches!(site, UseSite::PhiEdge { .. }));
            let home = StackHomeId(home_index as u32);
            let store = ir
                .insert_after_definition(
                    interval.definition,
                    SyntheticOperation::StackStore { home },
                    Uses::one(value),
                    false,
                )
                .unwrap()
                .instruction;
            ir.assign_phi_edge_home(*site, value, value).unwrap();
            homes.push(ExpandedStackHome {
                id: home,
                root: LiveBundleId(home_index as u32),
                definition: ExpandedStackDefinition::Store {
                    instruction: store,
                    value,
                },
                kind: ExpandedStackHomeKind::Root,
            });
            original_sites.push(*site);
        }
        let intervals = ir.analyze(&cfg).unwrap();
        let roots = original_sites
            .iter()
            .copied()
            .enumerate()
            .map(|(index, original_site)| {
                let value = VReg(index as u32 + 1);
                ExpandedRoot {
                    id: LiveBundleId(index as u32),
                    origin: value,
                    uses: vec![ExpandedUse {
                        id: BundleUseId(0),
                        original_site,
                        site: ir
                            .resolve_original_use_site(original_site, &intervals)
                            .unwrap(),
                        value,
                        source: ExpandedUseSource::Edge(ExpandedEdgeLocation::Stack {
                            home: StackHomeId(index as u32),
                        }),
                    }],
                }
            })
            .collect::<Vec<_>>();
        let expanded = ExpandedAllocationProblem {
            incremental_liveness: live_interval::IncrementalLiveness::build(&ir, &cfg, &intervals)
                .unwrap(),
            ir,
            intervals,
            shift_encoding: VariableShiftEncoding::Bmi2,
            roots,
            machine_edge_uses: Vec::new(),
            register_regions: Vec::new(),
            region_rows: BTreeMap::new(),
            region_by_value: BTreeMap::new(),
            next_register_region: 0,
            stack_homes: homes,
            state_homes: Vec::new(),
        };

        let coloring = color(&expanded, &cfg).unwrap();

        assert_eq!(coloring.offsets, vec![0, 0]);
        assert_eq!(coloring.slot_count, 1);
        assert!(coloring.intervals.intervals.iter().all(|interval| {
            matches!(
                interval.as_ref().unwrap().uses.as_slice(),
                [UseSite::PhiEdge { .. }]
            )
        }));
    }
}
