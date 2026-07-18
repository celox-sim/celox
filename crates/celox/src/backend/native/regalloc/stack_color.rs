//! Exact stack-home liveness and sparse stack-slot coloring.
//!
//! A spill home is a location-level SSA value: an explicit store or a
//! stack-resident phi defines it, reloads and direct phi-edge locations use
//! it. Reusing a frame offset is therefore an ordinary interference-coloring
//! decision over the same CFG-sparse interval model as machine VRegs, not a
//! lifetime approximation based on instruction layout or last reloads.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::backend::native::mir::{BlockId, Uses, VReg};

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
    LiveIntervalError, LiveIntervals, LivenessProgram, UseSite, analyze_program,
};

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
                        .resolve_stack_store_use_site(
                            instruction,
                            home.id,
                            value,
                            &expanded.intervals,
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
                if !edge_use_owners.insert((predecessor, successor, phi, home)) {
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

        Ok(Self { home_count, blocks })
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
        ExpandedRoot, ExpandedStackHome, ExpandedStackHomeKind, ExpandedUse, ExpandedUseIndex,
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
            use_index: ExpandedUseIndex::build(&[], cfg).unwrap(),
            shift_encoding: VariableShiftEncoding::Bmi2,
            roots: Vec::new(),
            register_regions: Vec::new(),
            stack_homes,
        }
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
        let use_index = ExpandedUseIndex::build(&roots, &cfg).unwrap();
        let expanded = ExpandedAllocationProblem {
            incremental_liveness: live_interval::IncrementalLiveness::build(&ir, &cfg, &intervals)
                .unwrap(),
            ir,
            intervals,
            use_index,
            shift_encoding: VariableShiftEncoding::Bmi2,
            roots,
            register_regions: Vec::new(),
            stack_homes: homes,
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
