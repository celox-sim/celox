use std::collections::BTreeMap;

use celox_analysis::cfg::{CfgError, ForwardControlFlowGraph};
use num_bigint::BigUint;
use num_traits::One;
use thiserror::Error;

use crate::{
    HashMap, HashSet,
    ir::{
        AbsoluteAddr, BasicBlock, BinaryOp, BlockId, ExecutionUnit, RegionedAbsoluteAddr,
        RegisterId, RegisterType, SIRBuilder, SIRInstruction, SIROffset, SIRTerminator, SIRValue,
        SPARSE_WORKING_REGION, STABLE_REGION, WORKING_REGION,
    },
    logic_tree::{NodeId, SLTNodeArena, SLTToSIRLowerer},
};

use super::{
    CombDefinitionId, CombRecipeId, ControlBlockId, ControlTerminator, EffectId, EffectKind,
    EventIr, EventIrError, EventProjection, ProcessId, RegionKind, ValueId, ValueKind, ValueOffset,
    ValueScope, ValueType,
    comb_value_graph::{CombValueGraph, CombValueGraphError},
};

#[derive(Debug, Error)]
pub enum EventProjectionError {
    #[error(transparent)]
    Verify(#[from] EventIrError),
    #[error("projection {projection:?} is invalid for EIR domain {domain:?}")]
    InvalidProjection {
        projection: EventProjection,
        domain: super::EventDomain,
    },
    #[error("event trigger {trigger} does not activate EIR domain {domain:?}")]
    InvalidTrigger {
        trigger: AbsoluteAddr,
        domain: super::EventDomain,
    },
    #[error("EIR comb graph names {expected} SLT nodes, but the supplied arena contains {actual}")]
    SltArenaMismatch { expected: usize, actual: usize },
    #[error("EIR {value} is unavailable while lowering {block}")]
    ValueUnavailable {
        value: ValueId,
        block: ControlBlockId,
    },
    #[error("EIR {value} has no executable SIR lowering: {kind}")]
    UnsupportedValue { value: ValueId, kind: &'static str },
    #[error("EIR {effect} has no executable SIR lowering: {kind}")]
    UnsupportedEffect {
        effect: EffectId,
        kind: &'static str,
    },
    #[error("EIR {definition} belongs to combinational convergence region {convergence}")]
    ConvergenceBoundary {
        definition: CombDefinitionId,
        convergence: super::CombConvergenceId,
    },
    #[error("EIR combinational definition cycle reaches {definition}")]
    CombDefinitionCycle { definition: CombDefinitionId },
    #[error("EIR comb value graph construction failed: {0}")]
    CombValueGraph(String),
    #[error("EIR comb recipe {recipe} contains an effectful SLT value recipe")]
    EffectfulCombValue { recipe: CombRecipeId },
    #[error("EIR process {process} has an entry block parameter")]
    ProcessEntryParameter { process: ProcessId },
    #[error("EIR control region {region} is not a process control block")]
    NonControlEffectRegion { region: super::RegionId },
    #[error("EIR control-flow analysis failed for {process}: {source}")]
    ControlFlowAnalysis {
        process: ProcessId,
        #[source]
        source: CfgError,
    },
    #[error("EIR event execution control-flow analysis failed: {source}")]
    EventControlFlowAnalysis {
        #[source]
        source: CfgError,
    },
    #[error("EIR projection produced an unexpected SIR Commit before state publication")]
    UnexpectedBodyCommit,
}

/// Lower one verified EIR graph to one executable SIR projection.
///
/// Clock projections deliberately lower through an `AbsoluteAddr` body first:
/// every body Load is an immutable event-entry read and every body Store is a
/// `StageNextFf`. The final mapping assigns STABLE/WORKING regions in one
/// place, then inserts the seed/commit phase operations. This lets intact SLT
/// recipes use the existing symbolic lowerer without cloning the 100k-node
/// flattened arena merely to change its address type.
pub fn lower_event_projection(
    ir: &EventIr,
    projection: EventProjection,
    arena: &SLTNodeArena<AbsoluteAddr>,
    four_state: bool,
    trigger: AbsoluteAddr,
) -> Result<ExecutionUnit<RegionedAbsoluteAddr>, EventProjectionError> {
    ir.verify()?;
    if !projection.is_valid_for(ir.domain()) {
        return Err(EventProjectionError::InvalidProjection {
            projection,
            domain: ir.domain().clone(),
        });
    }
    if ir.comb_graph().slt_node_count() != arena.len() {
        return Err(EventProjectionError::SltArenaMismatch {
            expected: ir.comb_graph().slt_node_count(),
            actual: arena.len(),
        });
    }
    if projection == EventProjection::Combinational {
        return Err(EventProjectionError::InvalidProjection {
            projection,
            domain: ir.domain().clone(),
        });
    }

    let selected_processes = active_processes(ir, trigger)?;
    let selected = selected_processes.iter().copied().collect::<HashSet<_>>();
    let state = StatePublicationPlan::build(ir, &selected);
    if projection == EventProjection::ApplyClock {
        return Ok(lower_apply_projection(&state));
    }

    ClockBodyLowering::new(ir, arena, four_state, state, selected_processes)?.lower(projection)
}

fn active_processes(
    ir: &EventIr,
    trigger: AbsoluteAddr,
) -> Result<Vec<ProcessId>, EventProjectionError> {
    let super::EventDomain::Clock { clock, resets } = ir.domain() else {
        return Err(EventProjectionError::InvalidTrigger {
            trigger,
            domain: ir.domain().clone(),
        });
    };
    if trigger == *clock {
        return Ok((0..ir.processes().len()).map(ProcessId).collect());
    }
    if !resets.contains(&trigger) {
        return Err(EventProjectionError::InvalidTrigger {
            trigger,
            domain: ir.domain().clone(),
        });
    }
    Ok(ir
        .processes()
        .iter()
        .enumerate()
        .filter_map(|(process, item)| item.resets.contains(&trigger).then_some(ProcessId(process)))
        .collect())
}

#[derive(Debug, Default)]
struct StatePublicationPlan {
    static_ranges: BTreeMap<AbsoluteAddr, Vec<super::BitAccess>>,
    sparse_widths: BTreeMap<AbsoluteAddr, usize>,
    sparse_objects: HashSet<AbsoluteAddr>,
}

impl StatePublicationPlan {
    fn build(ir: &EventIr, selected_processes: &HashSet<ProcessId>) -> Self {
        let mut sparse_objects = HashSet::default();
        for effect in ir.effects() {
            if let EffectKind::StageNextFf {
                process, target, ..
            } = &effect.kind
                && selected_processes.contains(process)
                && !matches!(target.offset, ValueOffset::Static(_))
            {
                sparse_objects.insert(target.object);
            }
        }

        let mut static_ranges: BTreeMap<AbsoluteAddr, Vec<super::BitAccess>> = BTreeMap::new();
        let mut sparse_widths: BTreeMap<AbsoluteAddr, usize> = BTreeMap::new();
        for effect in ir.effects() {
            let EffectKind::StageNextFf {
                process, target, ..
            } = &effect.kind
            else {
                continue;
            };
            if !selected_processes.contains(process) {
                continue;
            }
            if sparse_objects.contains(&target.object) {
                sparse_widths
                    .entry(target.object)
                    .and_modify(|width| {
                        *width = (*width).max(target.alias.msb.saturating_add(1));
                    })
                    .or_insert_with(|| target.alias.msb.saturating_add(1));
            } else {
                static_ranges
                    .entry(target.object)
                    .or_default()
                    .push(target.alias);
            }
        }
        for ranges in static_ranges.values_mut() {
            *ranges = merge_ranges(std::mem::take(ranges));
        }
        Self {
            static_ranges,
            sparse_widths,
            sparse_objects,
        }
    }

    fn seeds(&self) -> Vec<SIRInstruction<RegionedAbsoluteAddr>> {
        self.static_ranges
            .iter()
            .flat_map(|(&object, ranges)| {
                ranges.iter().map(move |range| {
                    SIRInstruction::Commit(
                        regioned(STABLE_REGION, object),
                        regioned(WORKING_REGION, object),
                        SIROffset::Static(range.lsb),
                        range.msb - range.lsb + 1,
                        Vec::new(),
                    )
                })
            })
            .collect()
    }

    fn commits(&self) -> Vec<SIRInstruction<RegionedAbsoluteAddr>> {
        let mut result = self
            .static_ranges
            .iter()
            .flat_map(|(&object, ranges)| {
                ranges.iter().map(move |range| {
                    SIRInstruction::Commit(
                        regioned(WORKING_REGION, object),
                        regioned(STABLE_REGION, object),
                        SIROffset::Static(range.lsb),
                        range.msb - range.lsb + 1,
                        Vec::new(),
                    )
                })
            })
            .collect::<Vec<_>>();
        result.extend(self.sparse_widths.iter().map(|(&object, &width)| {
            SIRInstruction::Commit(
                regioned(SPARSE_WORKING_REGION, object),
                regioned(STABLE_REGION, object),
                SIROffset::Static(0),
                width,
                Vec::new(),
            )
        }));
        result
    }
}

fn merge_ranges(mut ranges: Vec<super::BitAccess>) -> Vec<super::BitAccess> {
    ranges.sort_unstable_by_key(|range| (range.lsb, range.msb));
    let mut merged: Vec<super::BitAccess> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if let Some(previous) = merged.last_mut()
            && range.lsb <= previous.msb.saturating_add(1)
        {
            previous.msb = previous.msb.max(range.msb);
        } else {
            merged.push(range);
        }
    }
    merged
}

fn lower_apply_projection(state: &StatePublicationPlan) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let block = BasicBlock {
        id: BlockId(0),
        params: Vec::new(),
        instructions: state.commits(),
        terminator: SIRTerminator::Return,
    };
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [(BlockId(0), block)].into_iter().collect(),
        register_map: HashMap::default(),
    }
}

struct ProcessControlFlow {
    blocks: Vec<ControlBlockId>,
    local_by_block: HashMap<ControlBlockId, usize>,
    graph: ForwardControlFlowGraph,
}

struct EventControlFlow {
    processes: Vec<ProcessControlFlow>,
}

impl EventControlFlow {
    fn build(ir: &EventIr) -> Result<Self, EventProjectionError> {
        let mut processes = Vec::with_capacity(ir.processes().len());
        for (process_index, process) in ir.processes().iter().enumerate() {
            let process_id = ProcessId(process_index);
            let local_by_block = process
                .blocks
                .iter()
                .copied()
                .enumerate()
                .map(|(local, block)| (block, local))
                .collect::<HashMap<_, _>>();
            let successors = process
                .blocks
                .iter()
                .copied()
                .map(|block| {
                    control_successors(ir, block)
                        .into_iter()
                        .map(|successor| {
                            *local_by_block
                                .get(&successor)
                                .expect("verified EIR control edge stays in its process")
                        })
                        .collect()
                })
                .collect();
            let root = local_by_block[&process.entry];
            let graph =
                ForwardControlFlowGraph::analyze_structure(successors, root).map_err(|source| {
                    EventProjectionError::ControlFlowAnalysis {
                        process: process_id,
                        source,
                    }
                })?;
            processes.push(ProcessControlFlow {
                blocks: process.blocks.clone(),
                local_by_block,
                graph,
            });
        }
        Ok(Self { processes })
    }

    fn process(&self, process: ProcessId) -> &ProcessControlFlow {
        &self.processes[process.0]
    }

    fn dominates(
        &self,
        process: ProcessId,
        dominator: ControlBlockId,
        block: ControlBlockId,
    ) -> bool {
        let process = self.process(process);
        let (Some(&dominator), Some(&block)) = (
            process.local_by_block.get(&dominator),
            process.local_by_block.get(&block),
        ) else {
            return false;
        };
        process.graph.dominators.dominates(dominator, block)
    }

    fn immediate_dominator(
        &self,
        process: ProcessId,
        block: ControlBlockId,
    ) -> Option<ControlBlockId> {
        let process = self.process(process);
        let local = *process.local_by_block.get(&block)?;
        let parent = process.graph.dominators.idom[local]?;
        Some(process.blocks[parent])
    }

    fn dominator_preorder(&self, process: ProcessId) -> Vec<ControlBlockId> {
        let process = self.process(process);
        let mut order = Vec::with_capacity(process.blocks.len());
        let mut work = vec![process.graph.root];
        while let Some(block) = work.pop() {
            order.push(process.blocks[block]);
            work.extend(
                process.graph.dominators.children[block]
                    .iter()
                    .rev()
                    .copied(),
            );
        }
        order
    }

    fn local(&self, process: ProcessId, block: ControlBlockId) -> usize {
        self.process(process).local_by_block[&block]
    }

    fn merge_use_block(
        &self,
        process: ProcessId,
        current: Option<usize>,
        block: ControlBlockId,
    ) -> usize {
        self.merge_local(process, current, self.local(process, block))
    }

    fn merge_local(&self, process: ProcessId, current: Option<usize>, block: usize) -> usize {
        let process = self.process(process);
        current.map_or(block, |current| {
            process
                .graph
                .dominators
                .lca(current, block)
                .expect("verified reachable EIR blocks have a common process dominator")
        })
    }

    fn hoist_out_of_cycles(&self, process: ProcessId, mut block: usize) -> ControlBlockId {
        let process = self.process(process);
        loop {
            let component = process.graph.scc_for_block[block];
            if !process.graph.sccs[component].cyclic {
                return process.blocks[block];
            }
            let Some(parent) = process.graph.dominators.idom[block] else {
                return process.blocks[block];
            };
            block = parent;
            while process.graph.scc_for_block[block] == component {
                let Some(parent) = process.graph.dominators.idom[block] else {
                    return process.blocks[block];
                };
                block = parent;
            }
        }
    }
}

struct EventExecutionControlFlow {
    blocks: Vec<ControlBlockId>,
    local_by_block: HashMap<ControlBlockId, usize>,
    graph: ForwardControlFlowGraph,
}

impl EventExecutionControlFlow {
    fn build(
        ir: &EventIr,
        selected_processes: &[ProcessId],
    ) -> Result<Option<Self>, EventProjectionError> {
        let Some(&first_process) = selected_processes.first() else {
            return Ok(None);
        };
        let blocks = selected_processes
            .iter()
            .flat_map(|process| ir.processes()[process.0].blocks.iter().copied())
            .collect::<Vec<_>>();
        let local_by_block = blocks
            .iter()
            .copied()
            .enumerate()
            .map(|(local, block)| (block, local))
            .collect::<HashMap<_, _>>();
        let continuation = selected_processes
            .iter()
            .copied()
            .enumerate()
            .map(|(index, process)| {
                (
                    process,
                    selected_processes
                        .get(index + 1)
                        .map(|next| ir.processes()[next.0].entry),
                )
            })
            .collect::<HashMap<_, _>>();
        let successors = blocks
            .iter()
            .copied()
            .map(|block| {
                let process = ir.blocks()[block.0].process;
                let outgoing = match ir.blocks()[block.0]
                    .terminator
                    .as_ref()
                    .expect("verified EIR block is terminated")
                {
                    ControlTerminator::Jump { target, .. } => vec![*target],
                    ControlTerminator::Branch {
                        true_target,
                        false_target,
                        ..
                    } => vec![*true_target, *false_target],
                    ControlTerminator::Return => continuation[&process].into_iter().collect(),
                    ControlTerminator::Error(_) => Vec::new(),
                };
                outgoing
                    .into_iter()
                    .map(|successor| {
                        *local_by_block
                            .get(&successor)
                            .expect("selected event CFG contains every control successor")
                    })
                    .collect()
            })
            .collect();
        let root = local_by_block[&ir.processes()[first_process.0].entry];
        let graph = ForwardControlFlowGraph::analyze_structure(successors, root)
            .map_err(|source| EventProjectionError::EventControlFlowAnalysis { source })?;
        Ok(Some(Self {
            blocks,
            local_by_block,
            graph,
        }))
    }

    fn dominates(&self, dominator: ControlBlockId, block: ControlBlockId) -> bool {
        let (Some(&dominator), Some(&block)) = (
            self.local_by_block.get(&dominator),
            self.local_by_block.get(&block),
        ) else {
            return false;
        };
        self.graph.dominators.dominates(dominator, block)
    }

    fn local(&self, block: ControlBlockId) -> usize {
        self.local_by_block[&block]
    }

    fn merge_use_block(&self, current: Option<usize>, block: ControlBlockId) -> usize {
        self.merge_local(current, self.local(block))
    }

    fn merge_local(&self, current: Option<usize>, block: usize) -> usize {
        current.map_or(block, |current| {
            self.graph
                .dominators
                .lca(current, block)
                .expect("verified reachable EIR blocks have a common event dominator")
        })
    }

    fn hoist_out_of_cycles(&self, mut block: usize) -> ControlBlockId {
        loop {
            let component = self.graph.scc_for_block[block];
            if !self.graph.sccs[component].cyclic {
                return self.blocks[block];
            }
            let Some(parent) = self.graph.dominators.idom[block] else {
                return self.blocks[block];
            };
            block = parent;
            while self.graph.scc_for_block[block] == component {
                let Some(parent) = self.graph.dominators.idom[block] else {
                    return self.blocks[block];
                };
                block = parent;
            }
        }
    }
}

#[derive(Clone, Copy)]
struct PlacedValue {
    block: ControlBlockId,
    register: RegisterId,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
struct CombMaterializationId(usize);

fn control_successors(ir: &EventIr, block: ControlBlockId) -> Vec<ControlBlockId> {
    match ir.blocks()[block.0]
        .terminator
        .as_ref()
        .expect("verified EIR block is terminated")
    {
        ControlTerminator::Jump { target, .. } => vec![*target],
        ControlTerminator::Branch {
            true_target,
            false_target,
            ..
        } => vec![*true_target, *false_target],
        ControlTerminator::Return | ControlTerminator::Error(_) => Vec::new(),
    }
}

struct ClockBodyLowering<'a> {
    ir: &'a EventIr,
    arena: &'a SLTNodeArena<AbsoluteAddr>,
    control: EventControlFlow,
    event_control: Option<EventExecutionControlFlow>,
    comb_value_graph: Option<CombValueGraph>,
    slt: SLTToSIRLowerer,
    four_state: bool,
    builder: SIRBuilder<AbsoluteAddr>,
    state: StatePublicationPlan,
    sir_blocks: Vec<BlockId>,
    parameter_registers: Vec<Option<RegisterId>>,
    effects_by_block: Vec<Vec<EffectId>>,
    continuation_by_process: Vec<BlockId>,
    placed_comb_values: HashMap<(ProcessId, ValueId), PlacedValue>,
    placed_comb_definitions: Vec<Option<PlacedValue>>,
    comb_materialization_by_definition: Vec<Option<CombMaterializationId>>,
    comb_values_by_block: Vec<Vec<ValueId>>,
    comb_definitions_by_block: Vec<Vec<(CombMaterializationId, CombDefinitionId)>>,
    selected_processes: Vec<ProcessId>,
    selected_process_set: HashSet<ProcessId>,
    final_block: BlockId,
}

impl<'a> ClockBodyLowering<'a> {
    fn new(
        ir: &'a EventIr,
        arena: &'a SLTNodeArena<AbsoluteAddr>,
        four_state: bool,
        state: StatePublicationPlan,
        selected_processes: Vec<ProcessId>,
    ) -> Result<Self, EventProjectionError> {
        let selected_process_set = selected_processes.iter().copied().collect();
        let event_control = EventExecutionControlFlow::build(ir, &selected_processes)?;
        Ok(Self {
            ir,
            arena,
            control: EventControlFlow::build(ir)?,
            event_control,
            comb_value_graph: None,
            slt: SLTToSIRLowerer::new(four_state),
            four_state,
            builder: SIRBuilder::new(),
            state,
            sir_blocks: vec![BlockId(usize::MAX); ir.blocks().len()],
            parameter_registers: vec![None; ir.values().len()],
            effects_by_block: vec![Vec::new(); ir.blocks().len()],
            continuation_by_process: Vec::new(),
            placed_comb_values: HashMap::default(),
            placed_comb_definitions: Vec::new(),
            comb_materialization_by_definition: vec![None; ir.comb_definitions().len()],
            comb_values_by_block: vec![Vec::new(); ir.blocks().len()],
            comb_definitions_by_block: vec![Vec::new(); ir.blocks().len()],
            selected_processes,
            selected_process_set,
            final_block: BlockId(usize::MAX),
        })
    }

    fn lower(
        mut self,
        projection: EventProjection,
    ) -> Result<ExecutionUnit<RegionedAbsoluteAddr>, EventProjectionError> {
        let placement_start = std::env::var_os("CELOX_PHASE_TIMING")
            .is_some()
            .then(crate::timing::now);
        self.create_blocks()?;
        self.index_effects()?;
        self.build_comb_value_placements()?;
        if let Some(start) = placement_start {
            eprintln!("[eir] sparse comb placement: {:?}", start.elapsed());
        }
        let emission_start = std::env::var_os("CELOX_PHASE_TIMING")
            .is_some()
            .then(crate::timing::now);

        let first_entry = self
            .selected_processes
            .first()
            .map(|process| self.sir_blocks[self.ir.processes()[process.0].entry.0])
            .unwrap_or(self.final_block);
        self.builder
            .seal_block(SIRTerminator::Jump(first_entry, Vec::new()));

        for process in self.selected_processes.clone() {
            for block in self.control.dominator_preorder(process) {
                self.lower_block(process, block)?;
            }
        }
        if let Some(start) = emission_start {
            eprintln!("[eir] sparse comb emission: {:?}", start.elapsed());
        }

        self.builder.switch_to_block(self.final_block);
        self.builder.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = self.builder.drain();
        let mut result = map_clock_body(blocks, register_map, &self.state)?;
        result
            .blocks
            .get_mut(&result.entry_block_id)
            .expect("projection entry block exists")
            .instructions
            .splice(0..0, self.state.seeds());
        if projection == EventProjection::FusedClock {
            result
                .blocks
                .get_mut(&self.final_block)
                .expect("projection final block exists")
                .instructions
                .extend(self.state.commits());
        }
        Ok(result)
    }

    fn create_blocks(&mut self) -> Result<(), EventProjectionError> {
        for (index, block) in self.ir.blocks().iter().enumerate() {
            if !self.selected_process_set.contains(&block.process) {
                continue;
            }
            if self.ir.processes()[block.process.0].entry == ControlBlockId(index)
                && !block.parameters.is_empty()
            {
                return Err(EventProjectionError::ProcessEntryParameter {
                    process: block.process,
                });
            }
            let mut parameters = Vec::with_capacity(block.parameters.len());
            for &value in &block.parameters {
                let register = self.alloc_type(self.ir.values()[value.0].ty);
                self.parameter_registers[value.0] = Some(register);
                parameters.push(register);
            }
            self.sir_blocks[index] = self.builder.new_block_with(parameters);
        }
        self.final_block = self.builder.new_block();

        self.continuation_by_process = vec![self.final_block; self.ir.processes().len()];
        for (index, process) in self.selected_processes.iter().copied().enumerate() {
            self.continuation_by_process[process.0] = self
                .selected_processes
                .get(index + 1)
                .map(|next| self.sir_blocks[self.ir.processes()[next.0].entry.0])
                .unwrap_or(self.final_block);
        }
        Ok(())
    }

    fn index_effects(&mut self) -> Result<(), EventProjectionError> {
        let mut block_by_region = HashMap::default();
        for (index, block) in self.ir.blocks().iter().enumerate() {
            block_by_region.insert(block.region, ControlBlockId(index));
        }
        for (index, effect) in self.ir.effects().iter().enumerate() {
            if matches!(effect.kind, EffectKind::CommitFfState { .. }) {
                continue;
            }
            if let Some(&block) = block_by_region.get(&effect.region) {
                if self
                    .selected_process_set
                    .contains(&self.ir.blocks()[block.0].process)
                {
                    self.effects_by_block[block.0].push(EffectId(index));
                }
                continue;
            }
            if matches!(
                self.ir.regions()[effect.region.0].kind,
                RegionKind::FfProcess(process)
                    if !self.selected_process_set.contains(&process)
            ) {
                continue;
            }
            {
                return Err(EventProjectionError::NonControlEffectRegion {
                    region: effect.region,
                });
            }
        }
        Ok(())
    }

    fn lower_block(
        &mut self,
        process: ProcessId,
        block: ControlBlockId,
    ) -> Result<(), EventProjectionError> {
        self.builder.switch_to_block(self.sir_blocks[block.0]);
        self.prepare_block_comb_values(process, block)?;
        let mut cache = self.block_anchors(block);
        let effects = self.effects_by_block[block.0].clone();
        for effect in effects {
            self.lower_effect(process, block, effect, &mut cache)?;
        }

        let terminator = self.ir.blocks()[block.0]
            .terminator
            .as_ref()
            .expect("verified EIR block is terminated")
            .clone();
        let terminator = match terminator {
            ControlTerminator::Jump { target, arguments } => {
                let mut values = ValueMaterializer::new(
                    self.ir,
                    self.comb_value_graph
                        .as_ref()
                        .expect("reachable comb values have a sparse graph"),
                    self.arena,
                    &self.slt,
                    &self.control,
                    self.event_control
                        .as_ref()
                        .expect("a non-empty clock projection has an event CFG"),
                    &self.placed_comb_values,
                    &self.comb_materialization_by_definition,
                    &self.placed_comb_definitions,
                    &mut self.builder,
                    process,
                    block,
                    &mut cache,
                );
                SIRTerminator::Jump(
                    self.sir_blocks[target.0],
                    values.materialize_many(&arguments)?,
                )
            }
            ControlTerminator::Branch {
                condition,
                true_target,
                true_arguments,
                false_target,
                false_arguments,
            } => {
                let condition = {
                    let mut values = ValueMaterializer::new(
                        self.ir,
                        self.comb_value_graph
                            .as_ref()
                            .expect("reachable comb values have a sparse graph"),
                        self.arena,
                        &self.slt,
                        &self.control,
                        self.event_control
                            .as_ref()
                            .expect("a non-empty clock projection has an event CFG"),
                        &self.placed_comb_values,
                        &self.comb_materialization_by_definition,
                        &self.placed_comb_definitions,
                        &mut self.builder,
                        process,
                        block,
                        &mut cache,
                    );
                    values.materialize(condition)?
                };
                let true_edge = (!true_arguments.is_empty()).then(|| self.builder.new_block());
                let false_edge = (!false_arguments.is_empty()).then(|| self.builder.new_block());
                self.builder.seal_block(SIRTerminator::Branch {
                    cond: condition,
                    true_block: (
                        true_edge.unwrap_or(self.sir_blocks[true_target.0]),
                        Vec::new(),
                    ),
                    false_block: (
                        false_edge.unwrap_or(self.sir_blocks[false_target.0]),
                        Vec::new(),
                    ),
                });
                if let Some(edge) = true_edge {
                    self.lower_control_edge(
                        process,
                        block,
                        edge,
                        true_target,
                        &true_arguments,
                        &cache,
                    )?;
                }
                if let Some(edge) = false_edge {
                    self.lower_control_edge(
                        process,
                        block,
                        edge,
                        false_target,
                        &false_arguments,
                        &cache,
                    )?;
                }
                return Ok(());
            }
            ControlTerminator::Return => {
                SIRTerminator::Jump(self.continuation_by_process[process.0], Vec::new())
            }
            ControlTerminator::Error(code) => SIRTerminator::Error(code),
        };
        self.builder.seal_block(terminator);
        Ok(())
    }

    fn lower_control_edge(
        &mut self,
        process: ProcessId,
        source: ControlBlockId,
        edge: BlockId,
        target: ControlBlockId,
        arguments: &[ValueId],
        source_cache: &HashMap<ValueId, RegisterId>,
    ) -> Result<(), EventProjectionError> {
        self.builder.switch_to_block(edge);
        let mut cache = source_cache.clone();
        let mut values = ValueMaterializer::new(
            self.ir,
            self.comb_value_graph
                .as_ref()
                .expect("reachable comb values have a sparse graph"),
            self.arena,
            &self.slt,
            &self.control,
            self.event_control
                .as_ref()
                .expect("a non-empty clock projection has an event CFG"),
            &self.placed_comb_values,
            &self.comb_materialization_by_definition,
            &self.placed_comb_definitions,
            &mut self.builder,
            process,
            source,
            &mut cache,
        );
        let arguments = values.materialize_many(arguments)?;
        self.builder
            .seal_block(SIRTerminator::Jump(self.sir_blocks[target.0], arguments));
        Ok(())
    }

    fn lower_effect(
        &mut self,
        process: ProcessId,
        block: ControlBlockId,
        effect_id: EffectId,
        cache: &mut HashMap<ValueId, RegisterId>,
    ) -> Result<(), EventProjectionError> {
        let effect = &self.ir.effects()[effect_id.0];
        match &effect.kind {
            EffectKind::StageNextFf {
                target,
                value,
                guard,
                ..
            } => {
                if let Some(guard) = guard {
                    let mut condition_values = ValueMaterializer::new(
                        self.ir,
                        self.comb_value_graph
                            .as_ref()
                            .expect("reachable comb values have a sparse graph"),
                        self.arena,
                        &self.slt,
                        &self.control,
                        self.event_control
                            .as_ref()
                            .expect("a non-empty clock projection has an event CFG"),
                        &self.placed_comb_values,
                        &self.comb_materialization_by_definition,
                        &self.placed_comb_definitions,
                        &mut self.builder,
                        process,
                        block,
                        cache,
                    );
                    let condition = condition_values.materialize(*guard)?;
                    let store_block = self.builder.new_block();
                    let continuation = self.builder.new_block();
                    self.builder.seal_block(SIRTerminator::Branch {
                        cond: condition,
                        true_block: (store_block, Vec::new()),
                        false_block: (continuation, Vec::new()),
                    });
                    self.builder.switch_to_block(store_block);
                    let mut store_cache = cache.clone();
                    self.emit_stage(process, block, target, *value, &mut store_cache)?;
                    self.builder
                        .seal_block(SIRTerminator::Jump(continuation, Vec::new()));
                    self.builder.switch_to_block(continuation);
                } else {
                    self.emit_stage(process, block, target, *value, cache)?;
                }
            }
            EffectKind::RuntimeEvent {
                site_id,
                arguments,
                guard,
            }
            | EffectKind::Capture {
                site_id,
                arguments,
                guard,
            } => {
                if guard.is_some() {
                    return Err(EventProjectionError::UnsupportedEffect {
                        effect: effect_id,
                        kind: "guarded runtime observation",
                    });
                }
                let mut values = ValueMaterializer::new(
                    self.ir,
                    self.comb_value_graph
                        .as_ref()
                        .expect("reachable comb values have a sparse graph"),
                    self.arena,
                    &self.slt,
                    &self.control,
                    self.event_control
                        .as_ref()
                        .expect("a non-empty clock projection has an event CFG"),
                    &self.placed_comb_values,
                    &self.comb_materialization_by_definition,
                    &self.placed_comb_definitions,
                    &mut self.builder,
                    process,
                    block,
                    cache,
                );
                let arguments = values.materialize_many(arguments)?;
                match &effect.kind {
                    EffectKind::RuntimeEvent { .. } => {
                        self.builder.emit(SIRInstruction::RuntimeEvent {
                            site_id: *site_id,
                            args: arguments,
                        });
                    }
                    EffectKind::Capture { .. } => {
                        self.builder.emit(SIRInstruction::CombCaptureEvent {
                            site_id: *site_id,
                            args: arguments,
                            fatal_error_code: None,
                            consume_enabled: false,
                        });
                    }
                    _ => unreachable!(),
                }
            }
            EffectKind::RuntimeObservationBarrier => {}
            EffectKind::WritePersistentMemory { .. } => {
                return Err(EventProjectionError::UnsupportedEffect {
                    effect: effect_id,
                    kind: "persistent-memory write",
                });
            }
            EffectKind::TriggerPublication { .. } => {
                return Err(EventProjectionError::UnsupportedEffect {
                    effect: effect_id,
                    kind: "trigger publication",
                });
            }
            EffectKind::CommitFfState { .. } => unreachable!("commit is indexed separately"),
        }
        Ok(())
    }

    fn emit_stage(
        &mut self,
        process: ProcessId,
        block: ControlBlockId,
        target: &super::ObjectAccess,
        value: ValueId,
        cache: &mut HashMap<ValueId, RegisterId>,
    ) -> Result<(), EventProjectionError> {
        let mut values = ValueMaterializer::new(
            self.ir,
            self.comb_value_graph
                .as_ref()
                .expect("reachable comb values have a sparse graph"),
            self.arena,
            &self.slt,
            &self.control,
            self.event_control
                .as_ref()
                .expect("a non-empty clock projection has an event CFG"),
            &self.placed_comb_values,
            &self.comb_materialization_by_definition,
            &self.placed_comb_definitions,
            &mut self.builder,
            process,
            block,
            cache,
        );
        let value = values.materialize(value)?;
        let offset = values.materialize_offset(&target.offset)?;
        self.builder.emit(SIRInstruction::Store(
            target.object,
            offset,
            target.width,
            value,
            Vec::new(),
            Vec::new(),
        ));
        Ok(())
    }

    fn block_anchors(&self, block: ControlBlockId) -> HashMap<ValueId, RegisterId> {
        let mut anchors = HashMap::default();
        let process = self.ir.blocks()[block.0].process;
        let mut current = Some(block);
        while let Some(dominator) = current {
            for &value in &self.ir.blocks()[dominator.0].parameters {
                if let Some(register) = self.parameter_registers[value.0] {
                    anchors.insert(value, register);
                }
            }
            current = self.control.immediate_dominator(process, dominator);
        }
        anchors
    }

    fn build_comb_value_placements(&mut self) -> Result<(), EventProjectionError> {
        // One event-CFG dominator-tree location summarizes every use of a
        // definition. Storing the complete use-block set per transitive
        // definition can become O(definitions * blocks) on a large flattened
        // design even though placement only consumes its LCA.
        let mut definition_placements = vec![None; self.ir.comb_definitions().len()];
        let mut projection_values = Vec::new();

        // EIR values other than comb definitions are cheap process-local
        // wrappers. Place those within each process CFG and collect the exact
        // process/block demands for the shared comb graph.
        for process in self.selected_processes.clone() {
            let mut seeds = Vec::new();
            for &block in &self.ir.processes()[process.0].blocks {
                match self.ir.blocks()[block.0]
                    .terminator
                    .as_ref()
                    .expect("verified EIR block is terminated")
                {
                    ControlTerminator::Jump { arguments, .. } => {
                        seeds.extend(arguments.iter().copied().map(|value| (value, block)));
                        projection_values.extend(arguments.iter().copied());
                    }
                    ControlTerminator::Branch {
                        condition,
                        true_arguments,
                        false_arguments,
                        ..
                    } => {
                        seeds.push((*condition, block));
                        projection_values.push(*condition);
                        projection_values.extend(true_arguments.iter().copied());
                        projection_values.extend(false_arguments.iter().copied());
                    }
                    ControlTerminator::Return | ControlTerminator::Error(_) => {}
                }
                for &effect in &self.effects_by_block[block.0] {
                    match &self.ir.effects()[effect.0].kind {
                        EffectKind::StageNextFf {
                            target,
                            value,
                            guard,
                            ..
                        } => {
                            projection_values.push(*value);
                            target
                                .offset
                                .visit_value_operands(|value| projection_values.push(value));
                            if let Some(guard) = guard {
                                projection_values.push(*guard);
                            }
                            if let Some(guard) = guard {
                                seeds.push((*guard, block));
                            } else {
                                seeds.push((*value, block));
                                target
                                    .offset
                                    .visit_value_operands(|value| seeds.push((value, block)));
                            }
                        }
                        EffectKind::RuntimeEvent {
                            arguments,
                            guard: None,
                            ..
                        }
                        | EffectKind::Capture {
                            arguments,
                            guard: None,
                            ..
                        } => {
                            seeds.extend(arguments.iter().copied().map(|value| (value, block)));
                            projection_values.extend(arguments.iter().copied());
                        }
                        EffectKind::RuntimeEvent {
                            arguments,
                            guard: Some(guard),
                            ..
                        }
                        | EffectKind::Capture {
                            arguments,
                            guard: Some(guard),
                            ..
                        } => {
                            seeds.push((*guard, block));
                            projection_values.push(*guard);
                            projection_values.extend(arguments.iter().copied());
                        }
                        EffectKind::WritePersistentMemory { .. }
                        | EffectKind::TriggerPublication { .. }
                        | EffectKind::CommitFfState { .. }
                        | EffectKind::RuntimeObservationBarrier => {}
                    }
                }
            }

            let mut value_placement = vec![None; self.ir.values().len()];
            let mut value_work = Vec::new();
            for (value, block) in seeds {
                let merged = self
                    .control
                    .merge_use_block(process, value_placement[value.0], block);
                if value_placement[value.0] != Some(merged) {
                    value_placement[value.0] = Some(merged);
                    value_work.push(value);
                }
            }
            while let Some(value) = value_work.pop() {
                let block = value_placement[value.0].expect("placement work item has a use block");
                if matches!(
                    self.ir.values()[value.0].kind,
                    ValueKind::ReadCombDefinition { .. }
                ) {
                    continue;
                }
                let mut operands = Vec::new();
                self.ir.values()[value.0]
                    .kind
                    .visit_operands(|operand| operands.push(operand));
                for operand in operands {
                    let merged =
                        self.control
                            .merge_local(process, value_placement[operand.0], block);
                    if value_placement[operand.0] != Some(merged) {
                        value_placement[operand.0] = Some(merged);
                        value_work.push(operand);
                    }
                }
            }

            for (value, placement) in value_placement.into_iter().enumerate() {
                let Some(placement) = placement else {
                    continue;
                };
                let ValueKind::ReadCombDefinition { definition, .. } =
                    &self.ir.values()[value].kind
                else {
                    continue;
                };
                let definition = *definition;
                let block = self.control.hoist_out_of_cycles(process, placement);
                self.comb_values_by_block[block.0].push(ValueId(value));
                let event_control = self
                    .event_control
                    .as_ref()
                    .expect("a non-empty clock projection has an event CFG");
                definition_placements[definition.0] =
                    Some(event_control.merge_use_block(definition_placements[definition.0], block));
            }
        }

        let mut demanded_definitions = vec![false; self.ir.comb_definitions().len()];
        let mut projected_definition_roots = HashSet::default();
        let mut visited_projection_values = HashSet::default();
        while let Some(value) = projection_values.pop() {
            if !visited_projection_values.insert(value) {
                continue;
            }
            if let ValueKind::ReadCombDefinition { definition, .. } = self.ir.values()[value.0].kind
            {
                projected_definition_roots.insert(definition);
                continue;
            }
            self.ir.values()[value.0]
                .kind
                .visit_operands(|operand| projection_values.push(operand));
        }
        let mut closure = Vec::new();
        for (definition, placement) in definition_placements.iter().enumerate() {
            if placement.is_some() {
                demanded_definitions[definition] = true;
                closure.push(CombDefinitionId(definition));
            }
        }
        for definition in projected_definition_roots {
            demanded_definitions[definition.0] = true;
            closure.push(definition);
        }
        let mut reachable = vec![false; self.ir.comb_definitions().len()];
        while let Some(definition) = closure.pop() {
            if std::mem::replace(&mut reachable[definition.0], true) {
                continue;
            }
            let recipe_id = self.ir.comb_definitions()[definition.0].recipe;
            let recipe = &self.ir.comb_graph().recipes()[recipe_id.0];
            if let Some(convergence) = recipe.convergence {
                return Err(EventProjectionError::ConvergenceBoundary {
                    definition,
                    convergence,
                });
            }
            closure.extend(
                recipe
                    .dependencies
                    .iter()
                    .map(|dependency| dependency.definition),
            );
        }
        self.comb_value_graph = Some(
            CombValueGraph::build(
                self.ir.comb_graph(),
                self.arena,
                &demanded_definitions,
                !self.four_state,
            )
            .map_err(|error: CombValueGraphError| {
                EventProjectionError::CombValueGraph(error.to_string())
            })?,
        );
        if !reachable.iter().any(|reachable| *reachable) {
            return Ok(());
        }
        let direct_roots = definition_placements
            .iter()
            .enumerate()
            .filter_map(|(definition, placement)| placement.map(|_| CombDefinitionId(definition)))
            .collect::<Vec<_>>();

        // Build only the reachable eager-edge subgraph. Kahn order visits
        // users before their dependencies, so each dependency placement is
        // finalized with one LCA value and no use-set copying.
        let mut eager_reachable = vec![false; self.ir.comb_definitions().len()];
        let mut eager_indegree = vec![0usize; self.ir.comb_definitions().len()];
        let mut eager_work = direct_roots.clone();
        while let Some(definition) = eager_work.pop() {
            if std::mem::replace(&mut eager_reachable[definition.0], true) {
                continue;
            }
            let recipe = self.ir.comb_definitions()[definition.0].recipe;
            for dependency in self
                .comb_value_graph
                .as_ref()
                .expect("comb graph was built before eager-edge closure")
                .eager_dependencies(recipe)
            {
                eager_indegree[dependency.0] = eager_indegree[dependency.0].saturating_add(1);
                eager_work.push(dependency);
            }
        }

        let event_control = self
            .event_control
            .as_ref()
            .expect("a non-empty clock projection has an event CFG");
        let mut ready = eager_reachable
            .iter()
            .enumerate()
            .filter_map(|(definition, reachable)| {
                (*reachable && eager_indegree[definition] == 0)
                    .then_some(CombDefinitionId(definition))
            })
            .collect::<Vec<_>>();
        let mut processed = 0usize;
        while let Some(definition) = ready.pop() {
            processed += 1;
            let placement = definition_placements[definition.0];
            let recipe = self.ir.comb_definitions()[definition.0].recipe;
            for dependency in self
                .comb_value_graph
                .as_ref()
                .expect("comb graph was built before eager-edge placement")
                .eager_dependencies(recipe)
            {
                if let Some(placement) = placement {
                    definition_placements[dependency.0] = Some(
                        event_control.merge_local(definition_placements[dependency.0], placement),
                    );
                }
                eager_indegree[dependency.0] -= 1;
                if eager_indegree[dependency.0] == 0 {
                    ready.push(dependency);
                }
            }
        }
        if processed
            != eager_reachable
                .iter()
                .filter(|reachable| **reachable)
                .count()
        {
            let definition = eager_reachable
                .iter()
                .enumerate()
                .find_map(|(definition, reachable)| {
                    (*reachable && eager_indegree[definition] != 0)
                        .then_some(CombDefinitionId(definition))
                })
                .expect("an unprocessed eager definition exists");
            return Err(EventProjectionError::CombDefinitionCycle { definition });
        }

        // Eager dependencies receive one event-level placement. Only edges
        // deferred by the selected Mux stay arm-local; placing those here
        // would evaluate both arms before the branch.
        for (definition, placement) in definition_placements.into_iter().enumerate() {
            let Some(placement) = placement else {
                continue;
            };
            let definition = CombDefinitionId(definition);
            let block = event_control.hoist_out_of_cycles(placement);
            let materialization = CombMaterializationId(self.placed_comb_definitions.len());
            self.placed_comb_definitions.push(None);
            self.comb_definitions_by_block[block.0].push((materialization, definition));
            let previous =
                self.comb_materialization_by_definition[definition.0].replace(materialization);
            assert!(
                previous.is_none(),
                "one definition has one event-version materialization"
            );
        }

        for values in &mut self.comb_values_by_block {
            values.sort_unstable();
            values.dedup();
        }
        for definitions in &mut self.comb_definitions_by_block {
            definitions.sort_unstable();
            definitions.dedup();
        }
        Ok(())
    }

    fn prepare_block_comb_values(
        &mut self,
        process: ProcessId,
        block: ControlBlockId,
    ) -> Result<(), EventProjectionError> {
        let roots = self.comb_values_by_block[block.0].clone();
        let definitions = self.comb_definitions_by_block[block.0].clone();
        if roots.is_empty() && definitions.is_empty() {
            return Ok(());
        }
        let mut cache = self.block_anchors(block);
        let (materialized, scheduled_registers) = {
            let event_control = self
                .event_control
                .as_ref()
                .expect("a non-empty clock projection has an event CFG");
            let mut values = ValueMaterializer::new(
                self.ir,
                self.comb_value_graph
                    .as_ref()
                    .expect("reachable comb values have a sparse graph"),
                self.arena,
                &self.slt,
                &self.control,
                event_control,
                &self.placed_comb_values,
                &self.comb_materialization_by_definition,
                &self.placed_comb_definitions,
                &mut self.builder,
                process,
                block,
                &mut cache,
            );
            for &(_, definition) in &definitions {
                values.materialize_comb_definition(definition)?;
            }
            let mut materialized = Vec::with_capacity(roots.len());
            for root in roots {
                materialized.push((
                    (process, root),
                    PlacedValue {
                        block,
                        register: values.materialize(root)?,
                    },
                ));
            }
            let comb_cache = values.into_comb_cache();
            let scheduled_registers = definitions
                .iter()
                .map(|&(materialization, definition)| {
                    (materialization, definition, comb_cache[&definition])
                })
                .collect::<Vec<_>>();
            (materialized, scheduled_registers)
        };
        for (materialization, _definition, register) in scheduled_registers {
            let previous = self.placed_comb_definitions[materialization.0]
                .replace(PlacedValue { block, register });
            assert!(
                previous.is_none(),
                "one comb materialization is emitted exactly once"
            );
        }
        self.placed_comb_values.extend(materialized);
        Ok(())
    }

    fn alloc_type(&mut self, ty: ValueType) -> RegisterId {
        if ty.four_state {
            self.builder.alloc_logic(ty.width)
        } else {
            self.builder.alloc_bit(ty.width, ty.signed)
        }
    }
}

struct ValueMaterializer<'a, 'cache, 'builder> {
    ir: &'a EventIr,
    comb_value_graph: &'a CombValueGraph,
    arena: &'a SLTNodeArena<AbsoluteAddr>,
    slt: &'a SLTToSIRLowerer,
    control: &'a EventControlFlow,
    event_control: &'a EventExecutionControlFlow,
    placed_cache: &'a HashMap<(ProcessId, ValueId), PlacedValue>,
    comb_materialization_by_definition: &'a [Option<CombMaterializationId>],
    placed_comb_cache: &'a [Option<PlacedValue>],
    builder: &'builder mut SIRBuilder<AbsoluteAddr>,
    process: ProcessId,
    block: ControlBlockId,
    cache: &'cache mut HashMap<ValueId, RegisterId>,
    comb_cache: HashMap<CombDefinitionId, RegisterId>,
    comb_visiting: HashSet<CombDefinitionId>,
    sparse_control_depth: usize,
    sparse_controlled_recipes: HashSet<CombRecipeId>,
    slt_caches_by_inputs: HashMap<
        Vec<(crate::ir::VarAtomBase<AbsoluteAddr>, RegisterId)>,
        HashMap<NodeId, RegisterId>,
    >,
}

impl<'a, 'cache, 'builder> ValueMaterializer<'a, 'cache, 'builder> {
    fn new(
        ir: &'a EventIr,
        comb_value_graph: &'a CombValueGraph,
        arena: &'a SLTNodeArena<AbsoluteAddr>,
        slt: &'a SLTToSIRLowerer,
        control: &'a EventControlFlow,
        event_control: &'a EventExecutionControlFlow,
        placed_cache: &'a HashMap<(ProcessId, ValueId), PlacedValue>,
        comb_materialization_by_definition: &'a [Option<CombMaterializationId>],
        placed_comb_cache: &'a [Option<PlacedValue>],
        builder: &'builder mut SIRBuilder<AbsoluteAddr>,
        process: ProcessId,
        block: ControlBlockId,
        cache: &'cache mut HashMap<ValueId, RegisterId>,
    ) -> Self {
        Self::new_with_comb_cache(
            ir,
            comb_value_graph,
            arena,
            slt,
            control,
            event_control,
            placed_cache,
            comb_materialization_by_definition,
            placed_comb_cache,
            builder,
            process,
            block,
            cache,
            HashMap::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_with_comb_cache(
        ir: &'a EventIr,
        comb_value_graph: &'a CombValueGraph,
        arena: &'a SLTNodeArena<AbsoluteAddr>,
        slt: &'a SLTToSIRLowerer,
        control: &'a EventControlFlow,
        event_control: &'a EventExecutionControlFlow,
        placed_cache: &'a HashMap<(ProcessId, ValueId), PlacedValue>,
        comb_materialization_by_definition: &'a [Option<CombMaterializationId>],
        placed_comb_cache: &'a [Option<PlacedValue>],
        builder: &'builder mut SIRBuilder<AbsoluteAddr>,
        process: ProcessId,
        block: ControlBlockId,
        cache: &'cache mut HashMap<ValueId, RegisterId>,
        comb_cache: HashMap<CombDefinitionId, RegisterId>,
    ) -> Self {
        Self {
            ir,
            comb_value_graph,
            arena,
            slt,
            control,
            event_control,
            placed_cache,
            comb_materialization_by_definition,
            placed_comb_cache,
            builder,
            process,
            block,
            cache,
            comb_cache,
            comb_visiting: HashSet::default(),
            sparse_control_depth: 0,
            sparse_controlled_recipes: HashSet::default(),
            slt_caches_by_inputs: HashMap::default(),
        }
    }

    fn materialize_many(
        &mut self,
        values: &[ValueId],
    ) -> Result<Vec<RegisterId>, EventProjectionError> {
        values
            .iter()
            .map(|&value| self.materialize(value))
            .collect()
    }

    fn into_comb_cache(self) -> HashMap<CombDefinitionId, RegisterId> {
        self.comb_cache
    }

    fn materialize(&mut self, root: ValueId) -> Result<RegisterId, EventProjectionError> {
        let mut work = vec![(root, false)];
        while let Some((value, expanded)) = work.pop() {
            if self.has_value(value) {
                continue;
            }
            self.check_available(value)?;
            if expanded {
                let register = self.emit_value(value)?;
                self.cache.insert(value, register);
                continue;
            }
            work.push((value, true));
            let mut operands = Vec::new();
            self.ir.values()[value.0]
                .kind
                .visit_operands(|operand| operands.push(operand));
            for operand in operands.into_iter().rev() {
                if !self.has_value(operand) {
                    work.push((operand, false));
                }
            }
        }
        Ok(self.value_register(root))
    }

    fn has_value(&self, value: ValueId) -> bool {
        self.cache.contains_key(&value) || self.placed_register(value).is_some()
    }

    fn value_register(&self, value: ValueId) -> RegisterId {
        self.cache
            .get(&value)
            .copied()
            .or_else(|| self.placed_register(value))
            .expect("materialized EIR value has a register")
    }

    fn placed_register(&self, value: ValueId) -> Option<RegisterId> {
        let placed = self.placed_cache.get(&(self.process, value))?;
        self.control
            .dominates(self.process, placed.block, self.block)
            .then_some(placed.register)
    }

    fn has_comb_definition(&self, definition: CombDefinitionId) -> bool {
        self.comb_cache.contains_key(&definition) || self.placed_comb_register(definition).is_some()
    }

    fn comb_register(&self, definition: CombDefinitionId) -> RegisterId {
        self.comb_cache
            .get(&definition)
            .copied()
            .or_else(|| self.placed_comb_register(definition))
            .expect("materialized EIR comb definition has a register")
    }

    fn placed_comb_register(&self, definition: CombDefinitionId) -> Option<RegisterId> {
        let materialization = self
            .comb_materialization_by_definition
            .get(definition.0)?
            .as_ref()?;
        let placed = self.placed_comb_cache.get(materialization.0)?.as_ref()?;
        self.event_control
            .dominates(placed.block, self.block)
            .then_some(placed.register)
    }

    fn check_available(&self, value: ValueId) -> Result<(), EventProjectionError> {
        let item = &self.ir.values()[value.0];
        match item.scope {
            ValueScope::Event => Ok(()),
            ValueScope::Process(owner) if owner != self.process => {
                Err(EventProjectionError::ValueUnavailable {
                    value,
                    block: self.block,
                })
            }
            ValueScope::Process(_) => {
                let definition = match item.kind {
                    ValueKind::BlockParameter { block, .. } => block,
                    _ => match self.ir.regions()[item.region.0].kind {
                        RegionKind::ControlBlock { block, .. } => block,
                        RegionKind::FfProcess(process) => self.ir.processes()[process.0].entry,
                        RegionKind::EventRoot => {
                            return Err(EventProjectionError::ValueUnavailable {
                                value,
                                block: self.block,
                            });
                        }
                    },
                };
                if self.control.dominates(self.process, definition, self.block) {
                    Ok(())
                } else {
                    Err(EventProjectionError::ValueUnavailable {
                        value,
                        block: self.block,
                    })
                }
            }
        }
    }

    fn emit_value(&mut self, value: ValueId) -> Result<RegisterId, EventProjectionError> {
        let item = &self.ir.values()[value.0];
        let register = match &item.kind {
            ValueKind::BlockParameter { .. } => {
                return Err(EventProjectionError::ValueUnavailable {
                    value,
                    block: self.block,
                });
            }
            ValueKind::Constant { value, unknown } => {
                let destination = self.alloc_type(item.ty);
                self.builder.emit(SIRInstruction::Imm(
                    destination,
                    SIRValue::new_four_state(value.clone(), unknown.clone()),
                ));
                destination
            }
            ValueKind::ReadClockSnapshot(range) => {
                let destination = self.alloc_type(item.ty);
                self.builder.emit(SIRInstruction::Load(
                    destination,
                    range.object,
                    SIROffset::Static(range.access.lsb),
                    item.ty.width,
                ));
                destination
            }
            ValueKind::ReadPersistentMemory {
                object,
                offset,
                width,
            } => {
                let destination = self.alloc_type(item.ty);
                let offset = self.materialize_offset(offset)?;
                self.builder
                    .emit(SIRInstruction::Load(destination, *object, offset, *width));
                destination
            }
            ValueKind::ReadCombDefinition { definition, access } => {
                let source = self.materialize_comb_definition(*definition)?;
                let full_width = self.ir.comb_definitions()[definition.0]
                    .target
                    .width()
                    .expect("verified combinational definition range");
                let selected = if access.lsb == 0 && access.msb + 1 == full_width {
                    source
                } else {
                    let destination = self.alloc_type(item.ty);
                    self.builder.emit(SIRInstruction::Slice(
                        destination,
                        source,
                        access.lsb,
                        item.ty.width,
                    ));
                    destination
                };
                self.coerce(selected, item.ty)
            }
            ValueKind::Unary { op, input } => {
                let destination = self.alloc_type(item.ty);
                self.builder.emit(SIRInstruction::Unary(
                    destination,
                    *op,
                    self.value_register(*input),
                ));
                destination
            }
            ValueKind::Resize { input } => self.coerce(self.value_register(*input), item.ty),
            ValueKind::Binary { op, lhs, rhs } => {
                let destination = self.alloc_type(item.ty);
                self.builder.emit(SIRInstruction::Binary(
                    destination,
                    self.value_register(*lhs),
                    *op,
                    self.value_register(*rhs),
                ));
                destination
            }
            ValueKind::Mux {
                condition,
                then_value,
                else_value,
            } => {
                let destination = self.alloc_type(item.ty);
                self.builder.emit(SIRInstruction::Mux(
                    destination,
                    self.value_register(*condition),
                    self.value_register(*then_value),
                    self.value_register(*else_value),
                ));
                destination
            }
            ValueKind::Slice { source, access } => {
                let destination = self.alloc_type(item.ty);
                self.builder.emit(SIRInstruction::Slice(
                    destination,
                    self.value_register(*source),
                    access.lsb,
                    item.ty.width,
                ));
                destination
            }
            ValueKind::Concat { parts } => {
                let destination = self.alloc_type(item.ty);
                self.builder.emit(SIRInstruction::Concat(
                    destination,
                    parts
                        .iter()
                        .map(|part| self.value_register(*part))
                        .collect(),
                ));
                destination
            }
            ValueKind::DynamicSelect {
                source,
                offset,
                width,
            } => self.emit_dynamic_select(self.value_register(*source), offset, *width, item.ty)?,
            ValueKind::UpdateRange {
                base,
                offset,
                value: update,
                width,
            } => self.emit_update_range(
                self.value_register(*base),
                offset,
                self.value_register(*update),
                *width,
                item.ty,
            )?,
            ValueKind::ProcessPhi { .. } => {
                return Err(EventProjectionError::UnsupportedValue {
                    value,
                    kind: "ProcessPhi without a control-block parameter",
                });
            }
            ValueKind::LoopValue { .. } => {
                return Err(EventProjectionError::UnsupportedValue {
                    value,
                    kind: "LoopValue without a control-block parameter",
                });
            }
        };
        Ok(register)
    }

    fn materialize_offset(
        &mut self,
        offset: &ValueOffset,
    ) -> Result<SIROffset, EventProjectionError> {
        Ok(match offset {
            ValueOffset::Static(offset) => SIROffset::Static(*offset),
            ValueOffset::Dynamic(value) => SIROffset::Dynamic(self.materialize(*value)?),
            ValueOffset::Element {
                index,
                element_width,
                bit_offset,
                dynamic_bit_offset,
            } => SIROffset::Element {
                index: self.materialize(*index)?,
                element_width: *element_width,
                bit_offset: *bit_offset,
                dynamic_bit_offset: dynamic_bit_offset
                    .map(|value| self.materialize(value))
                    .transpose()?,
            },
        })
    }

    fn logical_offset(&mut self, offset: &ValueOffset) -> Result<RegisterId, EventProjectionError> {
        match offset {
            ValueOffset::Static(offset) => {
                let register = self.builder.alloc_bit(64, false);
                self.builder
                    .emit(SIRInstruction::Imm(register, SIRValue::new(*offset as u64)));
                Ok(register)
            }
            ValueOffset::Dynamic(value) => self.materialize(*value),
            ValueOffset::Element {
                index,
                element_width,
                bit_offset,
                dynamic_bit_offset,
            } => {
                let index = self.materialize(*index)?;
                let scale = self.builder.alloc_bit(64, false);
                self.builder.emit(SIRInstruction::Imm(
                    scale,
                    SIRValue::new(*element_width as u64),
                ));
                let mut offset = self.builder.alloc_logic(64);
                self.builder
                    .emit(SIRInstruction::Binary(offset, index, BinaryOp::Mul, scale));
                if *bit_offset != 0 {
                    let constant = self.builder.alloc_bit(64, false);
                    self.builder.emit(SIRInstruction::Imm(
                        constant,
                        SIRValue::new(*bit_offset as u64),
                    ));
                    let sum = self.builder.alloc_logic(64);
                    self.builder
                        .emit(SIRInstruction::Binary(sum, offset, BinaryOp::Add, constant));
                    offset = sum;
                }
                if let Some(dynamic) = dynamic_bit_offset {
                    let dynamic = self.materialize(*dynamic)?;
                    let sum = self.builder.alloc_logic(64);
                    self.builder
                        .emit(SIRInstruction::Binary(sum, offset, BinaryOp::Add, dynamic));
                    offset = sum;
                }
                Ok(offset)
            }
        }
    }

    fn emit_dynamic_select(
        &mut self,
        source: RegisterId,
        offset: &ValueOffset,
        width: usize,
        ty: ValueType,
    ) -> Result<RegisterId, EventProjectionError> {
        if let ValueOffset::Static(offset) = offset {
            let result = self.alloc_type(ty);
            self.builder
                .emit(SIRInstruction::Slice(result, source, *offset, width));
            return Ok(result);
        }
        let offset = self.logical_offset(offset)?;
        let source_width = self.builder.register(&source).width();
        let shifted = self.alloc_type(ValueType {
            width: source_width,
            signed: false,
            four_state: matches!(self.builder.register(&source), RegisterType::Logic { .. }),
        });
        self.builder.emit(SIRInstruction::Binary(
            shifted,
            source,
            BinaryOp::Shr,
            offset,
        ));
        if width == source_width {
            return Ok(self.coerce(shifted, ty));
        }
        let mask = self.builder.alloc_bit(width, false);
        self.builder
            .emit(SIRInstruction::Imm(mask, SIRValue::new(width_mask(width))));
        let result = self.alloc_type(ty);
        self.builder
            .emit(SIRInstruction::Binary(result, shifted, BinaryOp::And, mask));
        Ok(result)
    }

    fn emit_update_range(
        &mut self,
        base: RegisterId,
        offset: &ValueOffset,
        update: RegisterId,
        width: usize,
        ty: ValueType,
    ) -> Result<RegisterId, EventProjectionError> {
        if let ValueOffset::Static(offset) = offset {
            return Ok(self.emit_static_update(base, *offset, update, width, ty));
        }

        let logical_offset = self.logical_offset(offset)?;
        let base_width = ty.width;
        let wide_mask = self.alloc_type(ValueType::bit(base_width, false));
        self.builder.emit(SIRInstruction::Imm(
            wide_mask,
            SIRValue::new(width_mask(width)),
        ));
        let shifted_mask = self.alloc_type(ValueType::bit(base_width, false));
        self.builder.emit(SIRInstruction::Binary(
            shifted_mask,
            wide_mask,
            BinaryOp::Shl,
            logical_offset,
        ));
        let inverse_mask = self.alloc_type(ValueType::bit(base_width, false));
        self.builder.emit(SIRInstruction::Unary(
            inverse_mask,
            super::UnaryOp::BitNot,
            shifted_mask,
        ));
        let retained = self.alloc_type(ty);
        self.builder.emit(SIRInstruction::Binary(
            retained,
            base,
            BinaryOp::And,
            inverse_mask,
        ));

        let update = self.coerce(
            update,
            ValueType {
                width: base_width,
                signed: false,
                four_state: ty.four_state,
            },
        );
        let shifted_update = self.alloc_type(ty);
        self.builder.emit(SIRInstruction::Binary(
            shifted_update,
            update,
            BinaryOp::Shl,
            logical_offset,
        ));
        let selected_update = self.alloc_type(ty);
        self.builder.emit(SIRInstruction::Binary(
            selected_update,
            shifted_update,
            BinaryOp::And,
            shifted_mask,
        ));
        let result = self.alloc_type(ty);
        self.builder.emit(SIRInstruction::Binary(
            result,
            retained,
            BinaryOp::Or,
            selected_update,
        ));
        Ok(result)
    }

    fn emit_static_update(
        &mut self,
        base: RegisterId,
        offset: usize,
        update: RegisterId,
        width: usize,
        ty: ValueType,
    ) -> RegisterId {
        if offset == 0 && width == ty.width {
            return self.coerce(update, ty);
        }
        let mut parts = Vec::with_capacity(3);
        let upper_start = offset + width;
        if upper_start < ty.width {
            let upper_ty = ValueType {
                width: ty.width - upper_start,
                signed: false,
                four_state: ty.four_state,
            };
            let upper = self.alloc_type(upper_ty);
            self.builder.emit(SIRInstruction::Slice(
                upper,
                base,
                upper_start,
                upper_ty.width,
            ));
            parts.push(upper);
        }
        parts.push(self.coerce(
            update,
            ValueType {
                width,
                signed: false,
                four_state: ty.four_state,
            },
        ));
        if offset != 0 {
            let lower_ty = ValueType {
                width: offset,
                signed: false,
                four_state: ty.four_state,
            };
            let lower = self.alloc_type(lower_ty);
            self.builder
                .emit(SIRInstruction::Slice(lower, base, 0, offset));
            parts.push(lower);
        }
        let result = self.alloc_type(ty);
        self.builder.emit(SIRInstruction::Concat(result, parts));
        result
    }

    fn materialize_comb_definition(
        &mut self,
        definition: CombDefinitionId,
    ) -> Result<RegisterId, EventProjectionError> {
        if self.has_comb_definition(definition) {
            return Ok(self.comb_register(definition));
        }
        if !self.comb_visiting.insert(definition) {
            return Err(EventProjectionError::CombDefinitionCycle { definition });
        }
        let value = self.emit_comb_recipe(definition);
        self.comb_visiting.remove(&definition);
        let value = value?;
        self.comb_cache.insert(definition, value);
        Ok(value)
    }

    fn emit_comb_recipe(
        &mut self,
        definition: CombDefinitionId,
    ) -> Result<RegisterId, EventProjectionError> {
        let recipe_id = self.ir.comb_definitions()[definition.0].recipe;
        let recipe = &self.ir.comb_graph().recipes()[recipe_id.0];
        if recipe
            .pre_evaluate
            .iter()
            .any(|node| slt_tree_has_effects(NodeId(node.0), self.arena))
            || slt_tree_has_effects(NodeId(recipe.root.0), self.arena)
        {
            return Err(EventProjectionError::EffectfulCombValue { recipe: recipe_id });
        }

        let mut inputs = HashMap::default();
        for local in &recipe.local_inputs {
            let value = self.lower_recipe_region(recipe_id, NodeId(local.value.0), &inputs)?;
            inputs.insert(
                crate::ir::VarAtomBase::new(local.object, 0, local.width - 1),
                value,
            );
        }
        for &node in &recipe.pre_evaluate {
            self.lower_recipe_region(recipe_id, NodeId(node.0), &inputs)?;
        }
        let result = self.lower_recipe_region(recipe_id, NodeId(recipe.root.0), &inputs);
        self.sparse_controlled_recipes.remove(&recipe_id);
        result
    }

    fn lower_recipe_region(
        &mut self,
        recipe: CombRecipeId,
        root: NodeId,
        base_inputs: &HashMap<crate::ir::VarAtomBase<AbsoluteAddr>, RegisterId>,
    ) -> Result<RegisterId, EventProjectionError> {
        self.lower_recipe_region_with_cache(recipe, root, base_inputs, &HashMap::default())
    }

    fn lower_recipe_region_with_cache(
        &mut self,
        recipe: CombRecipeId,
        root: NodeId,
        base_inputs: &HashMap<crate::ir::VarAtomBase<AbsoluteAddr>, RegisterId>,
        seed_cache: &HashMap<NodeId, RegisterId>,
    ) -> Result<RegisterId, EventProjectionError> {
        let mut cache = seed_cache.clone();
        if !self.sparse_controlled_recipes.contains(&recipe) {
            self.prepare_control_nodes(recipe, root, base_inputs, &mut cache)?;
        }
        let mut inputs = base_inputs.clone();
        for dependency in self
            .comb_value_graph
            .dependencies_in_subtree(recipe, root, self.arena, &cache)
        {
            let value = self.materialize_comb_definition(dependency)?;
            let target = self.ir.comb_definitions()[dependency.0].target;
            inputs.insert(
                crate::ir::VarAtomBase::new(target.object, target.access.lsb, target.access.msb),
                value,
            );
        }
        if cache.is_empty() && self.sparse_control_depth == 0 {
            let mut input_key = inputs
                .iter()
                .map(|(range, register)| (*range, *register))
                .collect::<Vec<_>>();
            input_key.sort_unstable();
            let shared_cache = self.slt_caches_by_inputs.entry(input_key).or_default();
            Ok(self.slt.lower_with_scoped_inputs(
                self.builder,
                root,
                self.arena,
                shared_cache,
                &inputs,
            ))
        } else {
            Ok(self.slt.lower_with_scoped_inputs(
                self.builder,
                root,
                self.arena,
                &mut cache,
                &inputs,
            ))
        }
    }

    fn prepare_control_nodes(
        &mut self,
        recipe: CombRecipeId,
        node: NodeId,
        base_inputs: &HashMap<crate::ir::VarAtomBase<AbsoluteAddr>, RegisterId>,
        cache: &mut HashMap<NodeId, RegisterId>,
    ) -> Result<(), EventProjectionError> {
        if cache.contains_key(&node) {
            return Ok(());
        }
        let source = self.arena.get(node).clone();
        if let crate::logic_tree::SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } = &source
        {
            if self.comb_value_graph.mux_needs_control(recipe, node) {
                let result =
                    self.lower_sparse_mux(recipe, *cond, *then_expr, *else_expr, base_inputs)?;
                cache.insert(node, result);
            } else {
                // Do not emit a nested control node from an arm before its
                // enclosing branchless Mux. Its dependencies remain part of
                // this region and the ordinary lowerer keeps the original
                // evaluation order.
                self.prepare_control_nodes(recipe, *cond, base_inputs, cache)?;
            }
            return Ok(());
        }

        for child in slt_value_children(&source) {
            self.prepare_control_nodes(recipe, child, base_inputs, cache)?;
        }
        Ok(())
    }

    fn lower_sparse_mux(
        &mut self,
        recipe: CombRecipeId,
        cond: NodeId,
        then_expr: NodeId,
        else_expr: NodeId,
        base_inputs: &HashMap<crate::ir::VarAtomBase<AbsoluteAddr>, RegisterId>,
    ) -> Result<RegisterId, EventProjectionError> {
        let inserted = self.sparse_controlled_recipes.insert(recipe);
        debug_assert!(inserted, "one recipe selects at most one sparse Mux");
        let empty = HashMap::default();
        let then_dependencies = self
            .comb_value_graph
            .dependencies_in_subtree(recipe, then_expr, self.arena, &empty)
            .into_iter()
            .collect::<HashSet<_>>();
        let else_dependencies = self
            .comb_value_graph
            .dependencies_in_subtree(recipe, else_expr, self.arena, &empty)
            .into_iter()
            .collect::<HashSet<_>>();
        let mut common_inputs = base_inputs.clone();
        for dependency in then_dependencies.intersection(&else_dependencies).copied() {
            let value = self.materialize_comb_definition(dependency)?;
            let target = self.ir.comb_definitions()[dependency.0].target;
            common_inputs.insert(
                crate::ir::VarAtomBase::new(target.object, target.access.lsb, target.access.msb),
                value,
            );
        }

        let mut common_cache = HashMap::default();
        for shared in shared_slt_frontier(then_expr, else_expr, self.arena) {
            self.slt.lower_with_scoped_inputs(
                self.builder,
                shared,
                self.arena,
                &mut common_cache,
                &common_inputs,
            );
        }
        let condition =
            self.lower_recipe_region_with_cache(recipe, cond, &common_inputs, &common_cache)?;
        let width = crate::logic_tree::get_width(then_expr, self.arena)
            .max(crate::logic_tree::get_width(else_expr, self.arena));
        let result = self.builder.alloc_logic(width);
        let then_block = self.builder.new_block();
        let else_block = self.builder.new_block();
        let merge_block = self.builder.new_block_with(vec![result]);
        self.builder.seal_block(SIRTerminator::Branch {
            cond: condition,
            true_block: (then_block, Vec::new()),
            false_block: (else_block, Vec::new()),
        });

        let dominating_definitions = self.comb_cache.clone();
        self.builder.switch_to_block(then_block);
        self.sparse_control_depth += 1;
        let then_value =
            self.lower_recipe_region_with_cache(recipe, then_expr, &common_inputs, &common_cache)?;
        self.sparse_control_depth -= 1;
        let then_value = self.coerce(
            then_value,
            ValueType {
                width,
                signed: false,
                four_state: true,
            },
        );
        self.builder
            .seal_block(SIRTerminator::Jump(merge_block, vec![then_value]));

        self.comb_cache = dominating_definitions.clone();
        self.builder.switch_to_block(else_block);
        self.sparse_control_depth += 1;
        let else_value =
            self.lower_recipe_region_with_cache(recipe, else_expr, &common_inputs, &common_cache)?;
        self.sparse_control_depth -= 1;
        let else_value = self.coerce(
            else_value,
            ValueType {
                width,
                signed: false,
                four_state: true,
            },
        );
        self.builder
            .seal_block(SIRTerminator::Jump(merge_block, vec![else_value]));

        self.comb_cache = dominating_definitions;
        self.builder.switch_to_block(merge_block);
        Ok(result)
    }

    fn coerce(&mut self, mut source: RegisterId, ty: ValueType) -> RegisterId {
        let source_ty = self.builder.register(&source).clone();
        let source_four_state = matches!(source_ty, RegisterType::Logic { .. });
        if source_four_state && !ty.four_state {
            let converted = self
                .builder
                .alloc_bit(source_ty.width(), source_ty.is_signed());
            self.builder.emit(SIRInstruction::Unary(
                converted,
                super::UnaryOp::ToTwoState,
                source,
            ));
            source = converted;
        }
        let current = self.builder.register(&source);
        let exact = match current {
            RegisterType::Logic { width } => ty.four_state && *width == ty.width,
            RegisterType::Bit { width, signed } => {
                !ty.four_state && *width == ty.width && *signed == ty.signed
            }
        };
        if exact {
            return source;
        }
        let destination = self.alloc_type(ty);
        self.builder.emit(SIRInstruction::Unary(
            destination,
            super::UnaryOp::Ident,
            source,
        ));
        destination
    }

    fn alloc_type(&mut self, ty: ValueType) -> RegisterId {
        if ty.four_state {
            self.builder.alloc_logic(ty.width)
        } else {
            self.builder.alloc_bit(ty.width, ty.signed)
        }
    }
}

fn slt_value_children(node: &crate::logic_tree::SLTNode<AbsoluteAddr>) -> Vec<NodeId> {
    match node {
        crate::logic_tree::SLTNode::Input { index, .. } => {
            index.iter().map(|index| index.node).collect()
        }
        crate::logic_tree::SLTNode::Constant(..)
        | crate::logic_tree::SLTNode::ForFold { .. }
        | crate::logic_tree::SLTNode::ForFoldGroup { .. } => Vec::new(),
        crate::logic_tree::SLTNode::Binary(lhs, _, rhs) => vec![*lhs, *rhs],
        crate::logic_tree::SLTNode::Unary(_, input)
        | crate::logic_tree::SLTNode::Slice { expr: input, .. } => vec![*input],
        crate::logic_tree::SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => vec![*cond, *then_expr, *else_expr],
        crate::logic_tree::SLTNode::Concat(parts) => parts.iter().map(|(part, _)| *part).collect(),
    }
}

fn shared_slt_frontier(
    then_root: NodeId,
    else_root: NodeId,
    arena: &SLTNodeArena<AbsoluteAddr>,
) -> Vec<NodeId> {
    fn reachable(root: NodeId, arena: &SLTNodeArena<AbsoluteAddr>) -> HashSet<NodeId> {
        let mut result = HashSet::default();
        let mut work = vec![root];
        while let Some(node) = work.pop() {
            if result.insert(node) {
                work.extend(slt_value_children(arena.get(node)));
            }
        }
        result
    }

    let then_nodes = reachable(then_root, arena);
    let else_nodes = reachable(else_root, arena);
    let shared = then_nodes
        .intersection(&else_nodes)
        .copied()
        .collect::<HashSet<_>>();
    let mut shared_children = HashSet::default();
    for &node in &shared {
        for child in slt_value_children(arena.get(node)) {
            if shared.contains(&child) {
                shared_children.insert(child);
            }
        }
    }
    let mut frontier = shared
        .difference(&shared_children)
        .copied()
        .collect::<Vec<_>>();
    frontier.sort_unstable();
    frontier
}

fn slt_tree_has_effects(root: NodeId, arena: &SLTNodeArena<AbsoluteAddr>) -> bool {
    let mut visited = HashSet::default();
    let mut work = vec![root];
    while let Some(node) = work.pop() {
        if !visited.insert(node) {
            continue;
        }
        match arena.get(node) {
            crate::logic_tree::SLTNode::Input { index, .. } => {
                work.extend(index.iter().map(|index| index.node));
            }
            crate::logic_tree::SLTNode::Constant(..) => {}
            crate::logic_tree::SLTNode::Binary(lhs, _, rhs) => {
                work.push(*lhs);
                work.push(*rhs);
            }
            crate::logic_tree::SLTNode::Unary(_, input) => work.push(*input),
            crate::logic_tree::SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                work.push(*cond);
                work.push(*then_expr);
                work.push(*else_expr);
            }
            crate::logic_tree::SLTNode::ForFold {
                effects,
                start,
                end,
                initials,
                updates,
                continue_cond,
                ..
            } => {
                if !effects.is_empty() {
                    return true;
                }
                if let crate::logic_tree::SLTLoopBound::Expr(node) = start {
                    work.push(*node);
                }
                if let crate::logic_tree::SLTLoopBound::Expr(node) = end {
                    work.push(*node);
                }
                work.extend(initials.iter().map(|initial| initial.expr));
                work.extend(updates.iter().map(|update| update.expr));
                work.push(*continue_cond);
            }
            crate::logic_tree::SLTNode::ForFoldGroup {
                entry_guard,
                states,
                ..
            } => {
                work.push(*entry_guard);
                for state in states {
                    work.push(state.initial);
                    work.push(state.update);
                }
            }
            crate::logic_tree::SLTNode::Concat(parts) => {
                work.extend(parts.iter().map(|(node, _)| *node));
            }
            crate::logic_tree::SLTNode::Slice { expr, .. } => work.push(*expr),
        }
    }
    false
}

fn map_clock_body(
    blocks: HashMap<BlockId, BasicBlock<AbsoluteAddr>>,
    register_map: HashMap<RegisterId, RegisterType>,
    state: &StatePublicationPlan,
) -> Result<ExecutionUnit<RegionedAbsoluteAddr>, EventProjectionError> {
    let mut mapped = HashMap::default();
    for (block_id, block) in blocks {
        let mut instructions = Vec::with_capacity(block.instructions.len());
        for instruction in block.instructions {
            let instruction = match instruction {
                SIRInstruction::Load(destination, address, offset, width) => SIRInstruction::Load(
                    destination,
                    regioned(STABLE_REGION, address),
                    offset,
                    width,
                ),
                SIRInstruction::Store(address, offset, width, source, triggers, captures) => {
                    let region = if state.sparse_objects.contains(&address) {
                        SPARSE_WORKING_REGION
                    } else {
                        WORKING_REGION
                    };
                    SIRInstruction::Store(
                        regioned(region, address),
                        offset,
                        width,
                        source,
                        triggers,
                        captures,
                    )
                }
                SIRInstruction::Commit(..) => {
                    return Err(EventProjectionError::UnexpectedBodyCommit);
                }
                instruction => {
                    instruction.into_map_addr(|address| regioned(STABLE_REGION, address))
                }
            };
            instructions.push(instruction);
        }
        mapped.insert(
            block_id,
            BasicBlock {
                id: block.id,
                params: block.params,
                instructions,
                terminator: block.terminator,
            },
        );
    }
    Ok(ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: mapped,
        register_map,
    })
}

fn regioned(region: u32, address: AbsoluteAddr) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr::from_absolute_addr(region, address)
}

fn width_mask(width: usize) -> BigUint {
    (BigUint::one() << width) - BigUint::one()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use num_bigint::BigUint;
    use veryl_analyzer::ir::VarId;

    use super::*;
    use crate::{
        event_ir::{
            CombGraph, ControlTerminator, Effect, EventDomain, ObjectAccess, ObjectRange, Value,
            ValueKind,
        },
        ir::{BitAccess, InstanceId, LogicPathId, VarAtomBase},
        logic_tree::{LogicPath, LogicPathTarget, SLTNode, SLTNodeFacts},
    };

    fn object(raw: u32) -> AbsoluteAddr {
        AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: VarId::from_raw(raw),
        }
    }

    fn range(raw: u32, lsb: usize, msb: usize) -> ObjectRange {
        ObjectRange::new(object(raw), BitAccess::new(lsb, msb))
    }

    fn empty_clock_event() -> (
        EventIr,
        SLTNodeArena<AbsoluteAddr>,
        ProcessId,
        ControlBlockId,
    ) {
        let arena = SLTNodeArena::new();
        let mut event = EventIr::new(
            EventDomain::Clock {
                clock: object(0),
                resets: Vec::new(),
            },
            Arc::new(CombGraph::default()),
        );
        let process = event.add_process(0);
        let block = event.processes()[process.0].entry;
        (event, arena, process, block)
    }

    fn add_stage(
        event: &mut EventIr,
        process: ProcessId,
        block: ControlBlockId,
        target: ObjectAccess,
        value: ValueId,
    ) -> EffectId {
        event.add_effect(Effect {
            region: event.blocks()[block.0].region,
            predecessors: Vec::new(),
            kind: EffectKind::StageNextFf {
                process,
                target,
                value,
                guard: None,
                priority: 0,
            },
        })
    }

    fn finish_clock_event(event: &mut EventIr, block: ControlBlockId, stages: Vec<EffectId>) {
        event.set_terminator(block, ControlTerminator::Return);
        event.add_effect(Effect {
            region: event.root_region(),
            predecessors: stages.clone(),
            kind: EffectKind::CommitFfState { stages },
        });
        event.verify().unwrap();
    }

    fn instructions(
        unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    ) -> impl Iterator<Item = &SIRInstruction<RegionedAbsoluteAddr>> {
        let mut blocks = unit.blocks.keys().copied().collect::<Vec<_>>();
        blocks.sort_unstable();
        blocks
            .into_iter()
            .flat_map(move |block| unit.blocks[&block].instructions.iter())
    }

    #[test]
    fn clock_projections_share_one_stage_and_publication_contract() {
        let (mut event, arena, process, block) = empty_clock_event();
        let value = event.add_value(Value {
            ty: ValueType::bit(8, false),
            scope: ValueScope::Process(process),
            region: event.blocks()[block.0].region,
            kind: ValueKind::Constant {
                value: BigUint::from(0x5au8),
                unknown: BigUint::default(),
            },
        });
        let target = range(1, 8, 15);
        let stage = add_stage(&mut event, process, block, target.into(), value);
        finish_clock_event(&mut event, block, vec![stage]);

        let fused = lower_event_projection(
            &event,
            EventProjection::FusedClock,
            &arena,
            false,
            object(0),
        )
        .unwrap();
        fused.verify_result().unwrap();
        assert!(instructions(&fused).any(|instruction| matches!(
            instruction,
            SIRInstruction::Commit(source, destination, SIROffset::Static(8), 8, _)
                if source.region == STABLE_REGION && destination.region == WORKING_REGION
        )));
        assert!(instructions(&fused).any(|instruction| matches!(
            instruction,
            SIRInstruction::Store(address, SIROffset::Static(8), 8, _, _, _)
                if address.region == WORKING_REGION
        )));
        assert!(instructions(&fused).any(|instruction| matches!(
            instruction,
            SIRInstruction::Commit(source, destination, SIROffset::Static(8), 8, _)
                if source.region == WORKING_REGION && destination.region == STABLE_REGION
        )));

        let evaluate = lower_event_projection(
            &event,
            EventProjection::EvaluateClock,
            &arena,
            false,
            object(0),
        )
        .unwrap();
        evaluate.verify_result().unwrap();
        assert!(instructions(&evaluate).any(|instruction| matches!(
            instruction,
            SIRInstruction::Store(address, ..) if address.region == WORKING_REGION
        )));
        assert!(!instructions(&evaluate).any(|instruction| matches!(
            instruction,
            SIRInstruction::Commit(source, destination, ..)
                if source.region == WORKING_REGION && destination.region == STABLE_REGION
        )));

        let apply = lower_event_projection(
            &event,
            EventProjection::ApplyClock,
            &arena,
            false,
            object(0),
        )
        .unwrap();
        apply.verify_result().unwrap();
        assert_eq!(apply.blocks.len(), 1);
        assert_eq!(instructions(&apply).count(), 1);
        assert!(instructions(&apply).all(|instruction| matches!(
            instruction,
            SIRInstruction::Commit(source, destination, SIROffset::Static(8), 8, _)
                if source.region == WORKING_REGION && destination.region == STABLE_REGION
        )));
    }

    #[test]
    fn dynamic_stage_uses_sparse_state_without_a_dense_seed() {
        let (mut event, arena, process, block) = empty_clock_event();
        let region = event.blocks()[block.0].region;
        let index = event.add_value(Value {
            ty: ValueType::bit(6, false),
            scope: ValueScope::Process(process),
            region,
            kind: ValueKind::Constant {
                value: BigUint::from(3u8),
                unknown: BigUint::default(),
            },
        });
        let value = event.add_value(Value {
            ty: ValueType::bit(8, false),
            scope: ValueScope::Process(process),
            region,
            kind: ValueKind::Constant {
                value: BigUint::from(0xa5u8),
                unknown: BigUint::default(),
            },
        });
        let stage = add_stage(
            &mut event,
            process,
            block,
            ObjectAccess {
                object: object(1),
                offset: ValueOffset::Dynamic(index),
                width: 8,
                alias: BitAccess::new(0, 31),
            },
            value,
        );
        finish_clock_event(&mut event, block, vec![stage]);

        let fused = lower_event_projection(
            &event,
            EventProjection::FusedClock,
            &arena,
            false,
            object(0),
        )
        .unwrap();
        fused.verify_result().unwrap();
        assert!(!instructions(&fused).any(|instruction| matches!(
            instruction,
            SIRInstruction::Commit(source, destination, ..)
                if source.region == STABLE_REGION
                    && destination.region == SPARSE_WORKING_REGION
        )));
        assert!(instructions(&fused).any(|instruction| matches!(
            instruction,
            SIRInstruction::Store(address, SIROffset::Dynamic(_), 8, _, _, _)
                if address.region == SPARSE_WORKING_REGION
        )));
        assert!(instructions(&fused).any(|instruction| matches!(
            instruction,
            SIRInstruction::Commit(source, destination, SIROffset::Static(0), 32, _)
                if source.region == SPARSE_WORKING_REGION
                    && destination.region == STABLE_REGION
        )));
    }

    #[test]
    fn reset_projection_activates_only_its_processes() {
        let arena = SLTNodeArena::new();
        let reset_a = object(10);
        let reset_b = object(11);
        let mut event = EventIr::new(
            EventDomain::Clock {
                clock: object(0),
                resets: vec![reset_a, reset_b],
            },
            Arc::new(CombGraph::default()),
        );
        let mut stages = Vec::new();
        for (source_order, reset, target, payload) in [
            (0, reset_a, range(1, 0, 7), 0xaau8),
            (1, reset_b, range(2, 0, 7), 0x55u8),
        ] {
            let process = event.add_process_with_resets(source_order, vec![reset]);
            let block = event.processes()[process.0].entry;
            let value = event.add_value(Value {
                ty: ValueType::bit(8, false),
                scope: ValueScope::Process(process),
                region: event.blocks()[block.0].region,
                kind: ValueKind::Constant {
                    value: BigUint::from(payload),
                    unknown: BigUint::default(),
                },
            });
            stages.push(add_stage(&mut event, process, block, target.into(), value));
            event.set_terminator(block, ControlTerminator::Return);
        }
        event.add_effect(Effect {
            region: event.root_region(),
            predecessors: stages.clone(),
            kind: EffectKind::CommitFfState { stages },
        });
        event.verify().unwrap();

        let reset_projection =
            lower_event_projection(&event, EventProjection::FusedClock, &arena, false, reset_a)
                .unwrap();
        reset_projection.verify_result().unwrap();
        assert!(instructions(&reset_projection).any(|instruction| matches!(
            instruction,
            SIRInstruction::Store(address, ..) if address.absolute_addr() == object(1)
        )));
        assert!(!instructions(&reset_projection).any(|instruction| matches!(
            instruction,
            SIRInstruction::Store(address, ..) if address.absolute_addr() == object(2)
        )));

        let clock_projection = lower_event_projection(
            &event,
            EventProjection::FusedClock,
            &arena,
            false,
            object(0),
        )
        .unwrap();
        clock_projection.verify_result().unwrap();
        for target in [object(1), object(2)] {
            assert!(instructions(&clock_projection).any(|instruction| matches!(
                instruction,
                SIRInstruction::Store(address, ..) if address.absolute_addr() == target
            )));
        }
    }

    #[test]
    fn process_local_range_update_stays_in_ssa() {
        let (mut event, arena, process, block) = empty_clock_event();
        let region = event.blocks()[block.0].region;
        let snapshot = event.add_value(Value {
            ty: ValueType::bit(16, false),
            scope: ValueScope::Event,
            region: event.root_region(),
            kind: ValueKind::ReadClockSnapshot(range(1, 0, 15)),
        });
        let byte = event.add_value(Value {
            ty: ValueType::bit(8, false),
            scope: ValueScope::Process(process),
            region,
            kind: ValueKind::Constant {
                value: BigUint::from(0xabu8),
                unknown: BigUint::default(),
            },
        });
        let updated = event.add_value(Value {
            ty: ValueType::bit(16, false),
            scope: ValueScope::Process(process),
            region,
            kind: ValueKind::UpdateRange {
                base: snapshot,
                offset: ValueOffset::Static(4),
                value: byte,
                width: 8,
            },
        });
        let stage = add_stage(&mut event, process, block, range(1, 0, 15).into(), updated);
        finish_clock_event(&mut event, block, vec![stage]);

        let fused = lower_event_projection(
            &event,
            EventProjection::FusedClock,
            &arena,
            false,
            object(0),
        )
        .unwrap();
        fused.verify_result().unwrap();
        assert_eq!(
            instructions(&fused)
                .filter(|instruction| matches!(instruction, SIRInstruction::Load(..)))
                .count(),
            1
        );
        assert_eq!(
            instructions(&fused)
                .filter(|instruction| matches!(instruction, SIRInstruction::Store(..)))
                .count(),
            1
        );
        assert!(instructions(&fused).any(|instruction| matches!(
            instruction,
            SIRInstruction::Concat(_, parts) if parts.len() == 3
        )));
    }

    fn comb_path(
        target: AbsoluteAddr,
        expression: NodeId,
        sources: impl IntoIterator<Item = VarAtomBase<AbsoluteAddr>>,
    ) -> LogicPath<AbsoluteAddr> {
        LogicPath {
            semantic_region: Some(0),
            target: LogicPathTarget::Var(VarAtomBase::new(target, 0, 7)),
            sources: sources.into_iter().collect(),
            previous_sources: HashSet::default(),
            address_sources: HashSet::default(),
            local_inputs: Vec::new(),
            order_before: HashSet::<LogicPathId>::default(),
            comb_capture_enable_sites: Vec::new(),
            pre_lower_nodes: Vec::new(),
            expr: expression,
        }
    }

    #[test]
    fn comb_to_ff_chain_is_lowered_as_values_without_publication_traffic() {
        let source = object(1);
        let intermediate = object(2);
        let output = object(3);
        let ff = object(4);
        let mut arena = SLTNodeArena::new();
        let constant = arena
            .alloc(SLTNode::Constant(
                BigUint::from(7u8),
                BigUint::default(),
                8,
                false,
            ))
            .unwrap();
        let input = arena
            .alloc(SLTNode::Input {
                variable: intermediate,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let source_input = arena
            .alloc(SLTNode::Input {
                variable: source,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let sum = arena
            .alloc(SLTNode::Binary(input, BinaryOp::Add, source_input))
            .unwrap();
        let paths = vec![
            comb_path(intermediate, constant, []),
            comb_path(
                output,
                sum,
                [
                    VarAtomBase::new(intermediate, 0, 7),
                    VarAtomBase::new(source, 0, 7),
                ],
            ),
        ];
        let facts = SLTNodeFacts::verify(&arena).unwrap();
        let graph = Arc::new(CombGraph::import(&paths, &arena, &facts).unwrap());
        let mut event = EventIr::new(
            EventDomain::Clock {
                clock: object(0),
                resets: Vec::new(),
            },
            graph,
        );
        let process = event.add_process(0);
        let block = event.processes()[process.0].entry;
        let settled = event.add_value(Value {
            ty: ValueType::bit(8, false),
            scope: ValueScope::Event,
            region: event.root_region(),
            kind: ValueKind::ReadCombDefinition {
                definition: CombDefinitionId(1),
                access: BitAccess::new(0, 7),
            },
        });
        let stage = add_stage(
            &mut event,
            process,
            block,
            ObjectRange::new(ff, BitAccess::new(0, 7)).into(),
            settled,
        );
        finish_clock_event(&mut event, block, vec![stage]);

        let fused = lower_event_projection(
            &event,
            EventProjection::FusedClock,
            &arena,
            false,
            object(0),
        )
        .unwrap();
        fused.verify_result().unwrap();

        assert!(!instructions(&fused).any(|instruction| match instruction {
            SIRInstruction::Store(address, ..) => {
                [intermediate, output].contains(&address.absolute_addr())
            }
            _ => false,
        }));
        assert!(!instructions(&fused).any(|instruction| match instruction {
            SIRInstruction::Load(_, address, ..) => {
                [intermediate, output].contains(&address.absolute_addr())
            }
            _ => false,
        }));
        assert!(instructions(&fused).any(|instruction| matches!(
            instruction,
            SIRInstruction::Load(_, address, SIROffset::Static(0), 8)
                if address.absolute_addr() == source && address.region == STABLE_REGION
        )));
        assert!(instructions(&fused).any(|instruction| matches!(
            instruction,
            SIRInstruction::Store(address, SIROffset::Static(0), 8, _, _, _)
                if address.absolute_addr() == ff && address.region == WORKING_REGION
        )));
    }

    #[test]
    fn cross_definition_mux_materializes_only_the_selected_dependency_arm() {
        let condition = object(1);
        let numerator_a = object(2);
        let denominator_a = object(3);
        let numerator_b = object(4);
        let denominator_b = object(5);
        let arm_a = object(6);
        let arm_b = object(7);
        let selected_output = object(8);
        let ff = object(9);
        let mut arena = SLTNodeArena::new();

        let input =
            |arena: &mut SLTNodeArena<AbsoluteAddr>, variable: AbsoluteAddr, access: BitAccess| {
                arena
                    .alloc(SLTNode::Input {
                        variable,
                        signed: false,
                        index: Vec::new(),
                        access,
                    })
                    .unwrap()
            };
        let numerator_a_node = input(&mut arena, numerator_a, BitAccess::new(0, 7));
        let denominator_a_node = input(&mut arena, denominator_a, BitAccess::new(0, 7));
        let numerator_b_node = input(&mut arena, numerator_b, BitAccess::new(0, 7));
        let denominator_b_node = input(&mut arena, denominator_b, BitAccess::new(0, 7));
        let arm_a_value = arena
            .alloc(SLTNode::Binary(
                numerator_a_node,
                BinaryOp::DivU,
                denominator_a_node,
            ))
            .unwrap();
        let arm_b_value = arena
            .alloc(SLTNode::Binary(
                numerator_b_node,
                BinaryOp::DivU,
                denominator_b_node,
            ))
            .unwrap();
        let condition_node = input(&mut arena, condition, BitAccess::new(0, 0));
        let arm_a_input = input(&mut arena, arm_a, BitAccess::new(0, 7));
        let arm_b_input = input(&mut arena, arm_b, BitAccess::new(0, 7));
        let selected = arena
            .alloc(SLTNode::Mux {
                cond: condition_node,
                then_expr: arm_a_input,
                else_expr: arm_b_input,
            })
            .unwrap();

        let paths = vec![
            comb_path(
                arm_a,
                arm_a_value,
                [
                    VarAtomBase::new(numerator_a, 0, 7),
                    VarAtomBase::new(denominator_a, 0, 7),
                ],
            ),
            comb_path(
                arm_b,
                arm_b_value,
                [
                    VarAtomBase::new(numerator_b, 0, 7),
                    VarAtomBase::new(denominator_b, 0, 7),
                ],
            ),
            comb_path(
                selected_output,
                selected,
                [
                    VarAtomBase::new(condition, 0, 0),
                    VarAtomBase::new(arm_a, 0, 7),
                    VarAtomBase::new(arm_b, 0, 7),
                ],
            ),
        ];
        let facts = SLTNodeFacts::verify(&arena).unwrap();
        let graph = Arc::new(CombGraph::import(&paths, &arena, &facts).unwrap());
        let mut event = EventIr::new(
            EventDomain::Clock {
                clock: object(0),
                resets: Vec::new(),
            },
            graph,
        );
        let process = event.add_process(0);
        let block = event.processes()[process.0].entry;
        let value = event.add_value(Value {
            ty: ValueType::bit(8, false),
            scope: ValueScope::Event,
            region: event.root_region(),
            kind: ValueKind::ReadCombDefinition {
                definition: CombDefinitionId(2),
                access: BitAccess::new(0, 7),
            },
        });
        let stage = add_stage(
            &mut event,
            process,
            block,
            ObjectRange::new(ff, BitAccess::new(0, 7)).into(),
            value,
        );
        finish_clock_event(&mut event, block, vec![stage]);

        let fused = lower_event_projection(
            &event,
            EventProjection::FusedClock,
            &arena,
            false,
            object(0),
        )
        .unwrap();
        fused.verify_result().unwrap();

        let division_blocks = fused
            .blocks
            .values()
            .filter_map(|block| {
                let divisions = block
                    .instructions
                    .iter()
                    .filter(|instruction| {
                        matches!(instruction, SIRInstruction::Binary(_, _, BinaryOp::DivU, _))
                    })
                    .count();
                (divisions != 0).then_some((block.id, divisions))
            })
            .collect::<Vec<_>>();
        assert_eq!(division_blocks.len(), 2);
        assert!(division_blocks.iter().all(|(_, divisions)| *divisions == 1));
        assert!(fused.blocks.values().any(|block| {
            let SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } = &block.terminator
            else {
                return false;
            };
            let arm_blocks = [true_block.0, false_block.0];
            division_blocks
                .iter()
                .all(|(division_block, _)| arm_blocks.contains(division_block))
        }));

        let four_state =
            lower_event_projection(&event, EventProjection::FusedClock, &arena, true, object(0))
                .unwrap();
        four_state.verify_result().unwrap();
        assert!(
            four_state
                .blocks
                .values()
                .all(|block| !matches!(block.terminator, SIRTerminator::Branch { .. })),
            "an X/Z condition must not select only one combinational arm"
        );
        assert_eq!(
            instructions(&four_state)
                .filter(|instruction| matches!(
                    instruction,
                    SIRInstruction::Binary(_, _, BinaryOp::DivU, _)
                ))
                .count(),
            2
        );
    }

    #[test]
    fn recipes_share_only_structurally_identical_slt_subgraphs() {
        let shared_source = object(1);
        let source_a = object(2);
        let source_b = object(3);
        let output_a = object(4);
        let output_b = object(5);
        let ff_a = object(6);
        let ff_b = object(7);
        let mut arena = SLTNodeArena::new();
        let shared_input = arena
            .alloc(SLTNode::Input {
                variable: shared_source,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let input_a = arena
            .alloc(SLTNode::Input {
                variable: source_a,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let input_b = arena
            .alloc(SLTNode::Input {
                variable: source_b,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let one = arena
            .alloc(SLTNode::Constant(
                BigUint::from(1u8),
                BigUint::default(),
                8,
                false,
            ))
            .unwrap();
        let shared = arena
            .alloc(SLTNode::Binary(shared_input, BinaryOp::Add, one))
            .unwrap();
        let expression_a = arena
            .alloc(SLTNode::Binary(shared, BinaryOp::Add, input_a))
            .unwrap();
        let expression_b = arena
            .alloc(SLTNode::Binary(shared, BinaryOp::Add, input_b))
            .unwrap();
        let shared_range = VarAtomBase::new(shared_source, 0, 7);
        let paths = vec![
            comb_path(
                output_a,
                expression_a,
                [shared_range, VarAtomBase::new(source_a, 0, 7)],
            ),
            comb_path(
                output_b,
                expression_b,
                [shared_range, VarAtomBase::new(source_b, 0, 7)],
            ),
        ];
        let facts = SLTNodeFacts::verify(&arena).unwrap();
        let graph = Arc::new(CombGraph::import(&paths, &arena, &facts).unwrap());
        let mut event = EventIr::new(
            EventDomain::Clock {
                clock: object(0),
                resets: Vec::new(),
            },
            graph,
        );
        let process = event.add_process(0);
        let block = event.processes()[process.0].entry;
        let value_a = event.add_value(Value {
            ty: ValueType::bit(8, false),
            scope: ValueScope::Event,
            region: event.root_region(),
            kind: ValueKind::ReadCombDefinition {
                definition: CombDefinitionId(0),
                access: BitAccess::new(0, 7),
            },
        });
        let value_b = event.add_value(Value {
            ty: ValueType::bit(8, false),
            scope: ValueScope::Event,
            region: event.root_region(),
            kind: ValueKind::ReadCombDefinition {
                definition: CombDefinitionId(1),
                access: BitAccess::new(0, 7),
            },
        });
        let stage_a = add_stage(
            &mut event,
            process,
            block,
            ObjectRange::new(ff_a, BitAccess::new(0, 7)).into(),
            value_a,
        );
        let stage_b = add_stage(
            &mut event,
            process,
            block,
            ObjectRange::new(ff_b, BitAccess::new(0, 7)).into(),
            value_b,
        );
        finish_clock_event(&mut event, block, vec![stage_a, stage_b]);

        let fused = lower_event_projection(
            &event,
            EventProjection::FusedClock,
            &arena,
            false,
            object(0),
        )
        .unwrap();
        fused.verify_result().unwrap();

        assert_eq!(
            instructions(&fused)
                .filter(|instruction| matches!(
                    instruction,
                    SIRInstruction::Binary(_, _, BinaryOp::Add, _)
                ))
                .count(),
            3
        );
    }

    #[test]
    fn shared_comb_definition_has_one_event_version_across_ff_processes() {
        let comb_output = object(1);
        let ff_a = object(2);
        let ff_b = object(3);
        let mut arena = SLTNodeArena::new();
        let payload = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0x6du8),
                BigUint::default(),
                8,
                false,
            ))
            .unwrap();
        let paths = vec![comb_path(comb_output, payload, [])];
        let facts = SLTNodeFacts::verify(&arena).unwrap();
        let graph = Arc::new(CombGraph::import(&paths, &arena, &facts).unwrap());
        let mut event = EventIr::new(
            EventDomain::Clock {
                clock: object(0),
                resets: Vec::new(),
            },
            graph,
        );
        let settled = event.add_value(Value {
            ty: ValueType::bit(8, false),
            scope: ValueScope::Event,
            region: event.root_region(),
            kind: ValueKind::ReadCombDefinition {
                definition: CombDefinitionId(0),
                access: BitAccess::new(0, 7),
            },
        });
        let process_a = event.add_process(0);
        let block_a = event.processes()[process_a.0].entry;
        let stage_a = add_stage(
            &mut event,
            process_a,
            block_a,
            ObjectRange::new(ff_a, BitAccess::new(0, 7)).into(),
            settled,
        );
        event.set_terminator(block_a, ControlTerminator::Return);
        let process_b = event.add_process(1);
        let block_b = event.processes()[process_b.0].entry;
        let stage_b = add_stage(
            &mut event,
            process_b,
            block_b,
            ObjectRange::new(ff_b, BitAccess::new(0, 7)).into(),
            settled,
        );
        event.set_terminator(block_b, ControlTerminator::Return);
        event.add_effect(Effect {
            region: event.root_region(),
            predecessors: vec![stage_a, stage_b],
            kind: EffectKind::CommitFfState {
                stages: vec![stage_a, stage_b],
            },
        });
        event.verify().unwrap();

        let fused = lower_event_projection(
            &event,
            EventProjection::FusedClock,
            &arena,
            false,
            object(0),
        )
        .unwrap();
        fused.verify_result().unwrap();

        assert_eq!(
            instructions(&fused)
                .filter(|instruction| matches!(
                    instruction,
                    SIRInstruction::Imm(_, value)
                        if value.payload == BigUint::from(0x6du8)
                ))
                .count(),
            1
        );
    }

    #[test]
    fn branch_local_comb_cone_is_not_materialized_on_the_untaken_arm() {
        let condition_object = object(1);
        let comb_output = object(2);
        let ff = object(3);
        let mut arena = SLTNodeArena::new();
        let payload = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0x5au8),
                BigUint::default(),
                8,
                false,
            ))
            .unwrap();
        let paths = vec![comb_path(comb_output, payload, [])];
        let facts = SLTNodeFacts::verify(&arena).unwrap();
        let graph = Arc::new(CombGraph::import(&paths, &arena, &facts).unwrap());
        let mut event = EventIr::new(
            EventDomain::Clock {
                clock: object(0),
                resets: Vec::new(),
            },
            graph,
        );
        let process = event.add_process(0);
        let entry = event.processes()[process.0].entry;
        let true_block = event.add_control_block(process);
        let false_block = event.add_control_block(process);
        let condition = event.add_value(Value {
            ty: ValueType::bit(1, false),
            scope: ValueScope::Event,
            region: event.root_region(),
            kind: ValueKind::ReadClockSnapshot(ObjectRange::new(
                condition_object,
                BitAccess::new(0, 0),
            )),
        });
        let settled = event.add_value(Value {
            ty: ValueType::bit(8, false),
            scope: ValueScope::Event,
            region: event.root_region(),
            kind: ValueKind::ReadCombDefinition {
                definition: CombDefinitionId(0),
                access: BitAccess::new(0, 7),
            },
        });
        event.set_terminator(
            entry,
            ControlTerminator::Branch {
                condition,
                true_target: true_block,
                true_arguments: Vec::new(),
                false_target: false_block,
                false_arguments: Vec::new(),
            },
        );
        let stage = add_stage(
            &mut event,
            process,
            true_block,
            ObjectRange::new(ff, BitAccess::new(0, 7)).into(),
            settled,
        );
        event.set_terminator(true_block, ControlTerminator::Return);
        event.set_terminator(false_block, ControlTerminator::Return);
        event.add_effect(Effect {
            region: event.root_region(),
            predecessors: vec![stage],
            kind: EffectKind::CommitFfState {
                stages: vec![stage],
            },
        });
        event.verify().unwrap();

        let fused = lower_event_projection(
            &event,
            EventProjection::FusedClock,
            &arena,
            false,
            object(0),
        )
        .unwrap();
        fused.verify_result().unwrap();

        let (branch_block, true_sir, false_sir) = fused
            .blocks
            .values()
            .find_map(|block| {
                if let SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } = &block.terminator
                {
                    Some((block.id, true_block.0, false_block.0))
                } else {
                    None
                }
            })
            .expect("lowered EIR condition has a SIR branch");
        let has_payload = |block: BlockId| {
            fused.blocks[&block].instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Imm(_, value)
                        if value.payload == BigUint::from(0x5au8)
                )
            })
        };
        assert!(!has_payload(branch_block));
        assert!(has_payload(true_sir));
        assert!(!has_payload(false_sir));
    }

    #[test]
    fn guarded_stage_materializes_its_comb_cone_only_in_the_store_block() {
        let condition_object = object(1);
        let comb_output = object(2);
        let ff = object(3);
        let mut arena = SLTNodeArena::new();
        let payload = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0xa5u8),
                BigUint::default(),
                8,
                false,
            ))
            .unwrap();
        let paths = vec![comb_path(comb_output, payload, [])];
        let facts = SLTNodeFacts::verify(&arena).unwrap();
        let graph = Arc::new(CombGraph::import(&paths, &arena, &facts).unwrap());
        let mut event = EventIr::new(
            EventDomain::Clock {
                clock: object(0),
                resets: Vec::new(),
            },
            graph,
        );
        let process = event.add_process(0);
        let block = event.processes()[process.0].entry;
        let region = event.blocks()[block.0].region;
        let condition = event.add_value(Value {
            ty: ValueType::bit(1, false),
            scope: ValueScope::Event,
            region: event.root_region(),
            kind: ValueKind::ReadClockSnapshot(ObjectRange::new(
                condition_object,
                BitAccess::new(0, 0),
            )),
        });
        let settled = event.add_value(Value {
            ty: ValueType::bit(8, false),
            scope: ValueScope::Event,
            region: event.root_region(),
            kind: ValueKind::ReadCombDefinition {
                definition: CombDefinitionId(0),
                access: BitAccess::new(0, 7),
            },
        });
        let stage = event.add_effect(Effect {
            region,
            predecessors: Vec::new(),
            kind: EffectKind::StageNextFf {
                process,
                target: ObjectRange::new(ff, BitAccess::new(0, 7)).into(),
                value: settled,
                guard: Some(condition),
                priority: 0,
            },
        });
        finish_clock_event(&mut event, block, vec![stage]);

        let fused = lower_event_projection(
            &event,
            EventProjection::FusedClock,
            &arena,
            false,
            object(0),
        )
        .unwrap();
        fused.verify_result().unwrap();

        let (branch_block, store_block, continuation) = fused
            .blocks
            .values()
            .find_map(|block| {
                if let SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } = &block.terminator
                {
                    Some((block.id, true_block.0, false_block.0))
                } else {
                    None
                }
            })
            .expect("guarded stage has a SIR branch");
        let has_payload = |block: BlockId| {
            fused.blocks[&block].instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Imm(_, value)
                        if value.payload == BigUint::from(0xa5u8)
                )
            })
        };
        assert!(!has_payload(branch_block));
        assert!(has_payload(store_block));
        assert!(!has_payload(continuation));
    }

    #[test]
    fn branch_argument_cones_are_materialized_on_the_selected_edge() {
        let condition_object = object(1);
        let comb_output = object(2);
        let ff = object(3);
        let mut arena = SLTNodeArena::new();
        let payload = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0x3cu8),
                BigUint::default(),
                8,
                false,
            ))
            .unwrap();
        let paths = vec![comb_path(comb_output, payload, [])];
        let facts = SLTNodeFacts::verify(&arena).unwrap();
        let graph = Arc::new(CombGraph::import(&paths, &arena, &facts).unwrap());
        let mut event = EventIr::new(
            EventDomain::Clock {
                clock: object(0),
                resets: Vec::new(),
            },
            graph,
        );
        let process = event.add_process(0);
        let entry = event.processes()[process.0].entry;
        let join = event.add_control_block(process);
        let parameter = event.add_block_parameter(join, ValueType::bit(8, false));
        let condition = event.add_value(Value {
            ty: ValueType::bit(1, false),
            scope: ValueScope::Event,
            region: event.root_region(),
            kind: ValueKind::ReadClockSnapshot(ObjectRange::new(
                condition_object,
                BitAccess::new(0, 0),
            )),
        });
        let settled = event.add_value(Value {
            ty: ValueType::bit(8, false),
            scope: ValueScope::Event,
            region: event.root_region(),
            kind: ValueKind::ReadCombDefinition {
                definition: CombDefinitionId(0),
                access: BitAccess::new(0, 7),
            },
        });
        let zero = event.add_value(Value {
            ty: ValueType::bit(8, false),
            scope: ValueScope::Process(process),
            region: event.blocks()[entry.0].region,
            kind: ValueKind::Constant {
                value: BigUint::default(),
                unknown: BigUint::default(),
            },
        });
        event.set_terminator(
            entry,
            ControlTerminator::Branch {
                condition,
                true_target: join,
                true_arguments: vec![settled],
                false_target: join,
                false_arguments: vec![zero],
            },
        );
        let stage = add_stage(
            &mut event,
            process,
            join,
            ObjectRange::new(ff, BitAccess::new(0, 7)).into(),
            parameter,
        );
        event.set_terminator(join, ControlTerminator::Return);
        event.add_effect(Effect {
            region: event.root_region(),
            predecessors: vec![stage],
            kind: EffectKind::CommitFfState {
                stages: vec![stage],
            },
        });
        event.verify().unwrap();

        let fused = lower_event_projection(
            &event,
            EventProjection::FusedClock,
            &arena,
            false,
            object(0),
        )
        .unwrap();
        fused.verify_result().unwrap();

        let (branch_block, true_edge, false_edge) = fused
            .blocks
            .values()
            .find_map(|block| {
                if let SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } = &block.terminator
                {
                    Some((block.id, true_block.0, false_block.0))
                } else {
                    None
                }
            })
            .expect("EIR branch has a SIR branch");
        let has_payload = |block: BlockId| {
            fused.blocks[&block].instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Imm(_, value)
                        if value.payload == BigUint::from(0x3cu8)
                )
            })
        };
        assert!(!has_payload(branch_block));
        assert!(has_payload(true_edge));
        assert!(!has_payload(false_edge));
        let (true_target, true_arguments) = match &fused.blocks[&true_edge].terminator {
            SIRTerminator::Jump(target, arguments) => (*target, arguments),
            terminator => panic!("true edge has unexpected terminator {terminator:?}"),
        };
        let (false_target, false_arguments) = match &fused.blocks[&false_edge].terminator {
            SIRTerminator::Jump(target, arguments) => (*target, arguments),
            terminator => panic!("false edge has unexpected terminator {terminator:?}"),
        };
        assert_eq!(true_target, false_target);
        assert_eq!(true_arguments.len(), 1);
        assert_eq!(false_arguments.len(), 1);
    }
}
