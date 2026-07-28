use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, BinaryHeap},
};

use celox_analysis::cfg::{CfgError, ForwardControlFlowGraph};
use celox_analysis::dag_schedule::{
    DagScheduleError, schedule_min_live_values_in_domains_with_weights,
};
use celox_analysis::graph::StronglyConnectedComponents;
use celox_analysis::interval::{DisjointIntervalMap, ExactInterval};
use num_bigint::BigUint;
use num_traits::One;
use thiserror::Error;

use crate::{
    HashMap, HashSet,
    ir::{
        AbsoluteAddr, BasicBlock, BinaryOp, BlockId, ExecutionUnit, MATERIALIZATION_HOME_REGION,
        RegionedAbsoluteAddr, RegisterId, RegisterType, SIRBuilder, SIRInstruction, SIROffset,
        SIRTerminator, SIRValue, SPARSE_WORKING_REGION, STABLE_REGION, WORKING_REGION,
    },
    logic_tree::{NodeId, SLTNodeArena, SLTToSIRLowerer},
};

use super::{
    CombDefinitionId, CombRecipeId, ControlBlockId, ControlTerminator, EffectId, EffectKind,
    EventIr, EventIrError, EventProjection, ProcessId, RegionKind, ValueId, ValueKind, ValueOffset,
    ValueScope, ValueType,
    comb_value_graph::{CombValueGraph, CombValueGraphError},
};

/// A settled-state materialization may replace one comb publication Store and
/// one FF-side reload. Keep the initial production subset to a frontier leaf
/// plus at most one pure operation; larger cones require a whole-plan cost
/// comparison rather than a local expression estimate.
const SETTLED_FRONTIER_MATERIALIZATION_BUDGET: usize = 2;

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
    #[error("EIR materialization-home layout exceeds the host address space")]
    MaterializationHomeLayoutOverflow,
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
    #[error("EIR unified block scheduling failed for {block}: {error:?}")]
    BlockScheduling {
        block: ControlBlockId,
        error: DagScheduleError,
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
    let state = StatePublicationPlan::build(ir, &selected_processes, &selected);
    if projection == EventProjection::ApplyClock {
        return Ok(lower_apply_projection(&state));
    }

    let settled_comb_reads = matches!(
        projection,
        EventProjection::FusedSettledClock | EventProjection::EvaluateSettledClock
    );
    ClockBodyLowering::new(
        ir,
        arena,
        four_state,
        state,
        selected_processes,
        settled_comb_reads,
    )?
    .lower(projection)
}

/// Lower the settled clock projection as dependency packets.
///
/// A packet contains one feedback component (and any intervening processes
/// needed to keep the source order contiguous).  Between packets, every read
/// of a range written by a later packet has already completed, so a direct
/// STABLE publication remains phase-correct.  Feedback readers stay in the
/// same packet, where the ordinary stage-compute/publication scheduler keeps
/// the event-entry snapshot visible until every reader has captured it.
///
/// The publication plan is global: WORKING is seeded only in the first packet
/// and committed only in the last packet.  Splitting the executable body must
/// not turn packet boundaries into FF publication barriers.
pub fn lower_settled_clock_packets(
    ir: &EventIr,
    arena: &SLTNodeArena<AbsoluteAddr>,
    four_state: bool,
    trigger: AbsoluteAddr,
) -> Result<Vec<ExecutionUnit<RegionedAbsoluteAddr>>, EventProjectionError> {
    ir.verify()?;
    if ir.comb_graph().slt_node_count() != arena.len() {
        return Err(EventProjectionError::SltArenaMismatch {
            expected: ir.comb_graph().slt_node_count(),
            actual: arena.len(),
        });
    }
    let selected_processes = active_processes(ir, trigger)?;
    let selected = selected_processes.iter().copied().collect::<HashSet<_>>();
    let state = StatePublicationPlan::build(ir, &selected_processes, &selected);
    let packets = ordered_clock_process_packets(ir, &selected_processes);
    let packet_count = packets.len();
    packets
        .into_iter()
        .enumerate()
        .map(|(index, packet)| {
            ClockBodyLowering::new(ir, arena, four_state, state.clone(), packet, true)?
                .lower_with_publication(
                    EventProjection::FusedSettledClock,
                    index == 0,
                    index + 1 == packet_count,
                )
        })
        .collect()
}

/// Partition an already ordered clock event at exactly the feedback ranges
/// which cannot be published between processes.
///
/// `ordered_clock_processes` makes every acyclic state edge point forward
/// (reader before writer).  A remaining backward edge therefore identifies a
/// feedback component.  The interval closure below is a compact way to keep
/// all members of each such component in one contiguous packet without
/// constructing a process-pair matrix.
pub(crate) fn ordered_clock_process_packets(
    ir: &EventIr,
    ordered: &[ProcessId],
) -> Vec<Vec<ProcessId>> {
    if ordered.is_empty() {
        return vec![Vec::new()];
    }
    let position = ordered
        .iter()
        .copied()
        .enumerate()
        .map(|(position, process)| (process, position))
        .collect::<HashMap<_, _>>();
    let accesses = collect_process_state_accesses(ir, ordered);
    let mut writer_intervals = Vec::new();
    let mut writers_by_object = BTreeMap::<AbsoluteAddr, Vec<usize>>::new();
    for effect in ir.effects() {
        let EffectKind::StageNextFf {
            process, target, ..
        } = &effect.kind
        else {
            continue;
        };
        let Some(&writer) = position.get(process) else {
            continue;
        };
        let Some(length) = target
            .alias
            .msb
            .checked_sub(target.alias.lsb)
            .and_then(|width| width.checked_add(1))
        else {
            continue;
        };
        writer_intervals.push(ExactInterval {
            object: target.object,
            start: target.alias.lsb,
            length,
            value: writer,
        });
        writers_by_object
            .entry(target.object)
            .or_default()
            .push(writer);
    }
    for writers in writers_by_object.values_mut() {
        writers.sort_unstable();
        writers.dedup();
    }
    let Ok(writer_index) = DisjointIntervalMap::try_new(writer_intervals) else {
        // Invalid overlapping drivers are diagnosed elsewhere. Keeping one
        // packet is the only phase-safe result if they reach this adapter.
        return vec![ordered.to_vec()];
    };

    let mut backward_edges = Vec::new();
    for (reader, process) in ordered.iter().copied().enumerate() {
        let mut writers = Vec::new();
        for range in &accesses[process.0].static_reads {
            let Some(length) = range.width() else {
                continue;
            };
            if let Ok(overlapping) =
                writer_index.overlapping(&range.object, range.access.lsb, length)
            {
                writers.extend(overlapping);
            }
        }
        for object in &accesses[process.0].dynamic_reads {
            if let Some(object_writers) = writers_by_object.get(object) {
                writers.extend(object_writers.iter().copied());
            }
        }
        writers.sort_unstable();
        writers.dedup();
        for writer in writers {
            if writer < reader {
                backward_edges.push((writer, reader));
            }
        }
    }

    packet_ranges_from_backward_edges(ordered.len(), backward_edges)
        .into_iter()
        .map(|(start, end)| ordered[start..end].to_vec())
        .collect()
}

fn packet_ranges_from_backward_edges(
    process_count: usize,
    backward_edges: impl IntoIterator<Item = (usize, usize)>,
) -> Vec<(usize, usize)> {
    let mut span_end = (0..process_count).collect::<Vec<_>>();
    for (writer, reader) in backward_edges {
        if writer < reader && reader < process_count {
            span_end[writer] = span_end[writer].max(reader);
        }
    }
    let mut packets = Vec::new();
    let mut start = 0;
    while start < process_count {
        let mut end = span_end[start];
        let mut cursor = start + 1;
        while cursor <= end {
            end = end.max(span_end[cursor]);
            cursor += 1;
        }
        packets.push((start, end + 1));
        start = end + 1;
    }
    packets
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
        return Ok(ordered_clock_processes(
            ir,
            (0..ir.processes().len()).map(ProcessId).collect(),
        ));
    }
    if !resets.contains(&trigger) {
        return Err(EventProjectionError::InvalidTrigger {
            trigger,
            domain: ir.domain().clone(),
        });
    }
    Ok(ordered_clock_processes(
        ir,
        ir.processes()
            .iter()
            .enumerate()
            .filter_map(|(process, item)| {
                item.resets.contains(&trigger).then_some(ProcessId(process))
            })
            .collect(),
    ))
}

/// Order independent FF processes from state consumers toward state
/// producers. Every RHS observes the immutable event-entry snapshot, so this
/// changes no HDL value. It does shorten the lifetime of an upstream stage's
/// old state in an acyclic pipeline. Processes with externally observable
/// effects delimit reorderable runs.
pub(crate) fn ordered_clock_processes(ir: &EventIr, selected: Vec<ProcessId>) -> Vec<ProcessId> {
    let selected_set = selected.iter().copied().collect::<HashSet<_>>();
    let mut process_by_region = HashMap::default();
    for block in ir.blocks() {
        process_by_region.insert(block.region, block.process);
    }
    let mut pure = vec![true; ir.processes().len()];
    let mut writes = vec![Vec::<super::ObjectRange>::new(); ir.processes().len()];
    for effect in ir.effects() {
        let Some(&process) = process_by_region.get(&effect.region) else {
            continue;
        };
        if !selected_set.contains(&process) {
            continue;
        }
        match &effect.kind {
            EffectKind::StageNextFf { target, .. } => {
                writes[process.0].push(super::ObjectRange::new(target.object, target.alias));
            }
            EffectKind::CommitFfState { .. } => {}
            _ => pure[process.0] = false,
        }
    }

    let mut result = Vec::with_capacity(selected.len());
    let mut run = Vec::new();
    let flush = |run: &mut Vec<ProcessId>, result: &mut Vec<ProcessId>| {
        if run.is_empty() {
            return;
        }
        result.extend(order_pure_process_run(ir, run, &writes));
        run.clear();
    };
    for process in selected {
        if pure[process.0] {
            run.push(process);
        } else {
            flush(&mut run, &mut result);
            result.push(process);
        }
    }
    flush(&mut run, &mut result);
    let accesses = collect_process_state_accesses(ir, &result);
    let mut repaired = Vec::with_capacity(result.len());
    let mut start = 0;
    while start < result.len() {
        if !pure[result[start].0] {
            repaired.push(result[start]);
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < result.len() && pure[result[end].0] {
            end += 1;
        }
        repaired.extend(repair_publication_order(
            ir,
            result[start..end].to_vec(),
            &accesses,
            &pure,
        ));
        start = end;
    }
    repaired
}

fn interleavable_process_runs(ir: &EventIr, processes: &[ProcessId]) -> Vec<Vec<ProcessId>> {
    let is_interleavable = |process: ProcessId| {
        let item = &ir.processes()[process.0];
        if item.blocks.len() != 1 {
            return false;
        }
        let block = item.blocks[0];
        if !matches!(
            ir.blocks()[block.0].terminator,
            Some(ControlTerminator::Return)
        ) {
            return false;
        }
        ir.effects()
            .iter()
            .filter(|effect| effect.region == ir.blocks()[block.0].region)
            .all(|effect| {
                matches!(
                    effect.kind,
                    EffectKind::StageNextFf { process: owner, .. } if owner == process
                )
            })
    };

    let mut result = Vec::new();
    let mut run = Vec::new();
    for &process in processes {
        if is_interleavable(process) {
            run.push(process);
        } else {
            if !run.is_empty() {
                result.push(std::mem::take(&mut run));
            }
            result.push(vec![process]);
        }
    }
    if !run.is_empty() {
        result.push(run);
    }
    result
}

#[derive(Debug, Default)]
struct ProcessStateAccesses {
    static_reads: Vec<super::ObjectRange>,
    dynamic_reads: Vec<AbsoluteAddr>,
    comb_definitions: Vec<CombDefinitionId>,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
enum StateDependencyNode {
    Value(ValueId),
    CombDefinition(CombDefinitionId),
}

struct StateDependencyResolver<'a> {
    ir: &'a EventIr,
    writer_index: DisjointIntervalMap<AbsoluteAddr, usize>,
    writers_by_object: BTreeMap<AbsoluteAddr, Vec<usize>>,
    cache: HashMap<StateDependencyNode, Vec<usize>>,
}

impl<'a> StateDependencyResolver<'a> {
    fn new(ir: &'a EventIr, writers: &[(EffectId, super::ObjectRange)]) -> Option<Self> {
        let mut intervals = Vec::with_capacity(writers.len());
        let mut writers_by_object = BTreeMap::<AbsoluteAddr, Vec<usize>>::new();
        for (local, &(_, range)) in writers.iter().enumerate() {
            intervals.push(ExactInterval {
                object: range.object,
                start: range.access.lsb,
                length: range.width()?,
                value: local,
            });
            writers_by_object
                .entry(range.object)
                .or_default()
                .push(local);
        }
        Some(Self {
            ir,
            writer_index: DisjointIntervalMap::try_new(intervals).ok()?,
            writers_by_object,
            cache: HashMap::default(),
        })
    }

    fn resolve_roots(&mut self, roots: &[ValueId]) -> Option<Vec<usize>> {
        let mut result = Vec::new();
        for &value in roots {
            self.resolve(StateDependencyNode::Value(value))?;
            result.extend(&self.cache[&StateDependencyNode::Value(value)]);
        }
        result.sort_unstable();
        result.dedup();
        Some(result)
    }

    fn resolve(&mut self, root: StateDependencyNode) -> Option<()> {
        let mut active = HashSet::default();
        let mut stack = vec![(root, false)];
        while let Some((node, finish)) = stack.pop() {
            if self.cache.contains_key(&node) {
                continue;
            }
            if finish {
                active.remove(&node);
                let mut dependencies = self.direct_dependencies(node)?;
                for child in self.children(node) {
                    dependencies.extend(self.cache.get(&child)?);
                }
                dependencies.sort_unstable();
                dependencies.dedup();
                self.cache.insert(node, dependencies);
                continue;
            }
            if !active.insert(node) {
                // A loop-carried value needs an explicit event-entry phi
                // materialization before it can participate in direct
                // publication. Retain WORKING publication for this process.
                return None;
            }
            stack.push((node, true));
            for child in self.children(node).into_iter().rev() {
                if self.cache.contains_key(&child) {
                    continue;
                }
                if active.contains(&child) {
                    return None;
                }
                stack.push((child, false));
            }
        }
        Some(())
    }

    fn children(&self, node: StateDependencyNode) -> Vec<StateDependencyNode> {
        match node {
            StateDependencyNode::Value(value) => {
                let mut children = Vec::new();
                match &self.ir.values()[value.0].kind {
                    ValueKind::ReadCombDefinition { definition, .. } => {
                        children.push(StateDependencyNode::CombDefinition(*definition));
                    }
                    kind => kind.visit_operands(|operand| {
                        children.push(StateDependencyNode::Value(operand));
                    }),
                }
                children
            }
            StateDependencyNode::CombDefinition(definition) => {
                let recipe = &self.ir.comb_graph().recipes()
                    [self.ir.comb_definitions()[definition.0].recipe.0];
                recipe
                    .dependencies
                    .iter()
                    .map(|dependency| StateDependencyNode::CombDefinition(dependency.definition))
                    .collect()
            }
        }
    }

    fn direct_dependencies(&self, node: StateDependencyNode) -> Option<Vec<usize>> {
        let mut result = Vec::new();
        let mut add_static = |range: super::ObjectRange| -> Option<()> {
            result.extend(
                self.writer_index
                    .overlapping(&range.object, range.access.lsb, range.width()?)
                    .ok()?,
            );
            Some(())
        };
        match node {
            StateDependencyNode::Value(value) => match &self.ir.values()[value.0].kind {
                ValueKind::ReadClockSnapshot(range) => add_static(*range)?,
                ValueKind::ReadPersistentMemory {
                    object,
                    offset: ValueOffset::Static(offset),
                    width,
                } => {
                    let msb = offset.checked_add(width.saturating_sub(1))?;
                    add_static(super::ObjectRange::new(
                        *object,
                        super::BitAccess::new(*offset, msb),
                    ))?;
                }
                ValueKind::ReadPersistentMemory { object, .. } => {
                    if let Some(writers) = self.writers_by_object.get(object) {
                        result.extend(writers);
                    }
                }
                _ => {}
            },
            StateDependencyNode::CombDefinition(definition) => {
                let recipe = &self.ir.comb_graph().recipes()
                    [self.ir.comb_definitions()[definition.0].recipe.0];
                for snapshot in &recipe.snapshot_inputs {
                    add_static(snapshot.range)?;
                }
            }
        }
        Some(result)
    }
}

fn collect_process_state_accesses(
    ir: &EventIr,
    processes: &[ProcessId],
) -> Vec<ProcessStateAccesses> {
    let selected = processes.iter().copied().collect::<HashSet<_>>();
    let mut roots = vec![Vec::<ValueId>::new(); ir.processes().len()];
    let mut process_by_region = HashMap::default();
    for block in ir.blocks() {
        process_by_region.insert(block.region, block.process);
        if selected.contains(&block.process) {
            block
                .terminator
                .as_ref()
                .expect("verified EIR block is terminated")
                .visit_value_operands(|value| roots[block.process.0].push(value));
        }
    }
    for effect in ir.effects() {
        let process = match &effect.kind {
            EffectKind::StageNextFf { process, .. } => Some(*process),
            EffectKind::CommitFfState { .. } => None,
            _ => process_by_region.get(&effect.region).copied(),
        };
        let Some(process) = process.filter(|process| selected.contains(process)) else {
            continue;
        };
        effect
            .kind
            .visit_value_operands(|value| roots[process.0].push(value));
    }

    let mut result = (0..ir.processes().len())
        .map(|_| ProcessStateAccesses::default())
        .collect::<Vec<_>>();
    for &process in processes {
        let accesses = &mut result[process.0];
        let mut visited = HashSet::default();
        let mut work = std::mem::take(&mut roots[process.0]);
        while let Some(value) = work.pop() {
            if !visited.insert(value) {
                continue;
            }
            match &ir.values()[value.0].kind {
                ValueKind::ReadClockSnapshot(range) => accesses.static_reads.push(*range),
                ValueKind::ReadPersistentMemory {
                    object,
                    offset: ValueOffset::Static(offset),
                    width,
                } => {
                    if let Some(msb) = offset.checked_add(width.saturating_sub(1)) {
                        accesses.static_reads.push(super::ObjectRange::new(
                            *object,
                            super::BitAccess::new(*offset, msb),
                        ));
                    }
                }
                ValueKind::ReadPersistentMemory { object, .. } => {
                    accesses.dynamic_reads.push(*object);
                }
                ValueKind::ReadCombDefinition { definition, .. } => {
                    accesses.comb_definitions.push(*definition);
                }
                _ => {}
            }
            ir.values()[value.0]
                .kind
                .visit_operands(|operand| work.push(operand));
        }

        let mut definitions = std::mem::take(&mut accesses.comb_definitions);
        let mut visited_definitions = HashSet::default();
        while let Some(definition) = definitions.pop() {
            if !visited_definitions.insert(definition) {
                continue;
            }
            accesses.comb_definitions.push(definition);
            let recipe = &ir.comb_graph().recipes()[ir.comb_definitions()[definition.0].recipe.0];
            accesses
                .static_reads
                .extend(recipe.snapshot_inputs.iter().map(|input| input.range));
            definitions.extend(
                recipe
                    .dependencies
                    .iter()
                    .map(|dependency| dependency.definition),
            );
        }
        accesses.static_reads.sort_unstable();
        accesses.static_reads.dedup();
        accesses.dynamic_reads.sort_unstable();
        accesses.dynamic_reads.dedup();
        accesses.comb_definitions.sort_unstable();
        accesses.comb_definitions.dedup();
    }
    result
}

/// Minimize the actual state-publication components left by process ordering.
/// A static byte component either publishes directly or pays for one merged
/// WORKING seed/commit pair. An object with a dynamic write publishes directly
/// only when every one of its stages can do so; otherwise it pays for one
/// sparse commit. Counting one graph edge per reader cannot represent either
/// all-or-nothing cost.
fn repair_publication_order(
    ir: &EventIr,
    mut order: Vec<ProcessId>,
    accesses: &[ProcessStateAccesses],
    pure: &[bool],
) -> Vec<ProcessId> {
    #[derive(Clone)]
    struct Requirement {
        writer: ProcessId,
        readers: Vec<ProcessId>,
    }
    #[derive(Clone)]
    struct Candidate {
        dynamic: bool,
        cost: usize,
        requirements: Vec<Requirement>,
    }
    #[derive(Clone, Copy)]
    struct Stage {
        writer: ProcessId,
        target: super::ObjectRange,
        dynamic: bool,
        eligible: bool,
    }

    let selected = order.iter().copied().collect::<HashSet<_>>();
    let overlaps = |lhs: super::ObjectRange, rhs: super::ObjectRange| {
        lhs.object == rhs.object
            && lhs.access.lsb <= rhs.access.msb
            && rhs.access.lsb <= lhs.access.msb
    };
    let mut local_direct = HashSet::default();
    for &process in &order {
        if !pure[process.0] {
            continue;
        }
        if let Some(plan) = plan_local_stage_order(ir, process) {
            local_direct.extend(plan.direct_effects);
            local_direct.extend(plan.deferred_effects);
        }
    }
    let readers_for = |writer: ProcessId, target: super::ObjectRange| {
        order
            .iter()
            .copied()
            .filter(|reader| {
                *reader != writer
                    && (accesses[reader.0].dynamic_reads.contains(&target.object)
                        || accesses[reader.0]
                            .static_reads
                            .iter()
                            .copied()
                            .any(|read| overlaps(read, target)))
            })
            .collect::<Vec<_>>()
    };
    let mut stages_by_object = BTreeMap::<AbsoluteAddr, Vec<Stage>>::new();
    for (index, effect) in ir.effects().iter().enumerate() {
        let EffectKind::StageNextFf {
            process,
            target,
            stage_kind,
            ..
        } = &effect.kind
        else {
            continue;
        };
        if selected.contains(process) && pure[process.0] {
            let range = super::ObjectRange::new(target.object, target.alias);
            let has_local_read = accesses[process.0].dynamic_reads.contains(&target.object)
                || accesses[process.0]
                    .static_reads
                    .iter()
                    .copied()
                    .any(|read| overlaps(read, range));
            let eligible = !has_local_read
                || matches!(
                    stage_kind,
                    super::FfStageKind::FinalProcessSink | super::FfStageKind::WriteOnlyPublication
                )
                || local_direct.contains(&EffectId(index));
            stages_by_object
                .entry(target.object)
                .or_default()
                .push(Stage {
                    writer: *process,
                    target: range,
                    dynamic: !matches!(target.offset, ValueOffset::Static(_)),
                    eligible,
                });
        }
    }

    let mut candidates = Vec::new();
    for (_object, stages) in stages_by_object {
        if stages.iter().any(|stage| stage.dynamic) {
            if stages.iter().all(|stage| stage.eligible) {
                let mut readers_by_writer = BTreeMap::<ProcessId, BTreeSet<ProcessId>>::new();
                for stage in &stages {
                    readers_by_writer
                        .entry(stage.writer)
                        .or_default()
                        .extend(readers_for(stage.writer, stage.target));
                }
                let requirements = readers_by_writer
                    .into_iter()
                    .filter_map(|(writer, readers)| {
                        (!readers.is_empty()).then_some(Requirement {
                            writer,
                            readers: readers.into_iter().collect(),
                        })
                    })
                    .collect::<Vec<_>>();
                if !requirements.is_empty() {
                    candidates.push(Candidate {
                        dynamic: true,
                        cost: stages
                            .iter()
                            .map(|stage| stage.target.access.msb.saturating_add(1))
                            .max()
                            .unwrap_or(1),
                        requirements,
                    });
                }
            }
            continue;
        }

        // StatePublicationPlan merges byte-adjacent ranges across every
        // writer of one object.  Model exactly that component here: one
        // unresolved member retains the shared WORKING seed/commit pair, so
        // making only one writer direct has no publication benefit.
        let mut ranges = stages
            .into_iter()
            .map(|stage| {
                (
                    stage.target.access.lsb & !7,
                    stage.target.access.msb | 7,
                    stage,
                )
            })
            .collect::<Vec<_>>();
        ranges.sort_unstable_by_key(|(start, end, stage)| {
            (*start, *end, stage.writer, stage.target.access)
        });
        let mut component_start = 0;
        while component_start < ranges.len() {
            let mut component_end = component_start + 1;
            let mut covered_end = ranges[component_start].1;
            while component_end < ranges.len()
                && ranges[component_end].0 <= covered_end.saturating_add(1)
            {
                covered_end = covered_end.max(ranges[component_end].1);
                component_end += 1;
            }
            let component = &ranges[component_start..component_end];
            if component.iter().all(|(_, _, stage)| stage.eligible) {
                let mut readers_by_writer = BTreeMap::<ProcessId, BTreeSet<ProcessId>>::new();
                for (_, _, stage) in component {
                    readers_by_writer
                        .entry(stage.writer)
                        .or_default()
                        .extend(readers_for(stage.writer, stage.target));
                }
                let requirements = readers_by_writer
                    .into_iter()
                    .filter_map(|(writer, readers)| {
                        (!readers.is_empty()).then_some(Requirement {
                            writer,
                            readers: readers.into_iter().collect(),
                        })
                    })
                    .collect::<Vec<_>>();
                if !requirements.is_empty() {
                    candidates.push(Candidate {
                        dynamic: false,
                        cost: covered_end - ranges[component_start].0 + 1,
                        requirements,
                    });
                }
            }
            component_start = component_end;
        }
    }
    if candidates.is_empty() {
        return order;
    }
    let affinity_position = order
        .iter()
        .copied()
        .enumerate()
        .map(|(position, process)| (process, position))
        .collect::<HashMap<_, _>>();

    let unavoidable_cost = |candidate_order: &[ProcessId]| {
        let positions = candidate_order
            .iter()
            .copied()
            .enumerate()
            .map(|(position, process)| (process, position))
            .collect::<HashMap<_, _>>();
        let mut dynamic_count = 0usize;
        let mut dynamic_cost = 0usize;
        let mut static_count = 0usize;
        let mut static_cost = 0usize;
        for candidate in &candidates {
            let unresolved = candidate.requirements.iter().any(|requirement| {
                requirement
                    .readers
                    .iter()
                    .any(|reader| positions[reader] > positions[&requirement.writer])
            });
            if !unresolved {
                continue;
            }
            if candidate.dynamic {
                dynamic_count += 1;
                dynamic_cost = dynamic_cost.saturating_add(candidate.cost);
            } else {
                static_count += 1;
                static_cost = static_cost.saturating_add(candidate.cost);
            }
        }
        (dynamic_count, dynamic_cost, static_count, static_cost)
    };

    loop {
        let current_cost = unavoidable_cost(&order);
        let mut best = None::<((usize, usize, usize, usize), usize, usize, Vec<ProcessId>)>;
        for candidate in &candidates {
            for requirement in &candidate.requirements {
                let writer_position = order
                    .iter()
                    .position(|process| *process == requirement.writer)
                    .expect("a publication candidate belongs to the selected process run");
                let last_reader = requirement
                    .readers
                    .iter()
                    .filter_map(|reader| order.iter().position(|process| process == reader))
                    .max()
                    .expect("a publication candidate has a reader");
                if last_reader < writer_position {
                    continue;
                }
                let mut repaired = order.clone();
                repaired.remove(writer_position);
                let insertion = requirement
                    .readers
                    .iter()
                    .filter_map(|reader| repaired.iter().position(|process| process == reader))
                    .max()
                    .map(|position| position + 1)
                    .unwrap_or(repaired.len());
                repaired.insert(insertion, requirement.writer);
                let repaired_cost = unavoidable_cost(&repaired);
                if repaired_cost >= current_cost {
                    continue;
                }
                let movement = writer_position.abs_diff(insertion);
                let replacement = (repaired_cost, requirement.readers.len(), movement, repaired);
                if best.as_ref().is_none_or(|current| {
                    (replacement.0, replacement.1, replacement.2)
                        < (current.0, current.1, current.2)
                }) {
                    best = Some(replacement);
                }
            }
        }
        let Some((_, _, _, repaired)) = best else {
            break;
        };
        order = repaired;
    }

    // Publication has many equivalent minima. Restore every adjacent
    // affinity-order inversion which does not change that minimum. This keeps
    // shared comb consumers close without giving back a direct publication,
    // and terminates because each swap removes one inversion.
    loop {
        let cost = unavoidable_cost(&order);
        let mut changed = false;
        for index in 0..order.len().saturating_sub(1) {
            if affinity_position[&order[index]] < affinity_position[&order[index + 1]] {
                continue;
            }
            order.swap(index, index + 1);
            if unavoidable_cost(&order) == cost {
                changed = true;
            } else {
                order.swap(index, index + 1);
            }
        }
        if !changed {
            break;
        }
    }
    order
}

fn order_feedback_component(
    nodes: &[usize],
    successors: &[Vec<(usize, usize)>],
    predecessors: &[Vec<(usize, usize)>],
    affinity_rank: &[usize],
) -> Vec<usize> {
    if nodes.len() < 2 {
        return nodes.to_vec();
    }
    let node_set = nodes.iter().copied().collect::<HashSet<_>>();
    let mut active = vec![false; successors.len()];
    let mut incoming = vec![0usize; successors.len()];
    let mut outgoing = vec![0usize; successors.len()];
    let mut version = vec![0usize; successors.len()];
    for &node in nodes {
        active[node] = true;
        outgoing[node] = successors[node]
            .iter()
            .filter(|(target, _)| node_set.contains(target))
            .map(|(_, weight)| *weight)
            .sum();
        incoming[node] = predecessors[node]
            .iter()
            .filter(|(source, _)| node_set.contains(source))
            .map(|(_, weight)| *weight)
            .sum();
    }

    let mut sources = BinaryHeap::new();
    let mut sinks = BinaryHeap::new();
    let mut scores = BinaryHeap::new();
    let push = |node: usize,
                sources: &mut BinaryHeap<_>,
                sinks: &mut BinaryHeap<_>,
                scores: &mut BinaryHeap<_>,
                incoming: &[usize],
                outgoing: &[usize],
                version: &[usize]| {
        let tie = (Reverse(affinity_rank[node]), Reverse(node), version[node]);
        if incoming[node] == 0 {
            sources.push((tie.0, tie.1, tie.2));
        }
        if outgoing[node] == 0 {
            sinks.push((tie.0, tie.1, tie.2));
        }
        scores.push((
            outgoing[node] as i128 - incoming[node] as i128,
            tie.0,
            tie.1,
            tie.2,
        ));
    };
    for &node in nodes {
        push(
            node,
            &mut sources,
            &mut sinks,
            &mut scores,
            &incoming,
            &outgoing,
            &version,
        );
    }

    let mut left = Vec::new();
    let mut right = Vec::new();
    while left.len() + right.len() < nodes.len() {
        let pop_zero = |heap: &mut BinaryHeap<(Reverse<usize>, Reverse<usize>, usize)>,
                        active: &[bool],
                        version: &[usize],
                        degree: &[usize]| {
            while let Some((_, Reverse(node), item_version)) = heap.pop() {
                if active[node] && version[node] == item_version && degree[node] == 0 {
                    return Some(node);
                }
            }
            None
        };
        let selected = if let Some(source) = pop_zero(&mut sources, &active, &version, &incoming) {
            (source, true)
        } else if let Some(sink) = pop_zero(&mut sinks, &active, &version, &outgoing) {
            (sink, false)
        } else {
            let node = loop {
                let Some((_, _, Reverse(node), item_version)) = scores.pop() else {
                    return nodes.to_vec();
                };
                if active[node] && version[node] == item_version {
                    break node;
                }
            };
            (node, true)
        };
        let (node, place_left) = selected;
        active[node] = false;
        if place_left {
            left.push(node);
        } else {
            right.push(node);
        }

        for &(target, weight) in &successors[node] {
            if !active[target] {
                continue;
            }
            incoming[target] = incoming[target].saturating_sub(weight);
            version[target] += 1;
            push(
                target,
                &mut sources,
                &mut sinks,
                &mut scores,
                &incoming,
                &outgoing,
                &version,
            );
        }
        for &(source, weight) in &predecessors[node] {
            if !active[source] {
                continue;
            }
            outgoing[source] = outgoing[source].saturating_sub(weight);
            version[source] += 1;
            push(
                source,
                &mut sources,
                &mut sinks,
                &mut scores,
                &incoming,
                &outgoing,
                &version,
            );
        }
    }
    right.reverse();
    left.extend(right);
    left
}

fn order_pure_process_run(
    ir: &EventIr,
    run: &[ProcessId],
    writes: &[Vec<super::ObjectRange>],
) -> Vec<ProcessId> {
    if run.len() < 2 {
        return run.to_vec();
    }

    // Merge repeated/partial writes from one process before constructing the
    // disjoint writer index. Different FF processes may not drive overlapping
    // ranges in one clock event; if malformed input reaches this point, retain
    // source order rather than deriving a false scheduling freedom.
    let mut ranges_by_writer = BTreeMap::<(AbsoluteAddr, usize), Vec<super::BitAccess>>::new();
    for (local, process) in run.iter().copied().enumerate() {
        for range in &writes[process.0] {
            ranges_by_writer
                .entry((range.object, local))
                .or_default()
                .push(range.access);
        }
    }
    let mut intervals = Vec::new();
    let mut writers_by_object = BTreeMap::<AbsoluteAddr, Vec<usize>>::new();
    let mut writer_by_resource = Vec::<usize>::new();
    let mut resource_weights = Vec::<usize>::new();
    for ((object, writer), mut ranges) in ranges_by_writer {
        ranges.sort_unstable_by_key(|range| (range.lsb, range.msb));
        let mut merged = Vec::<super::BitAccess>::new();
        for range in ranges {
            if let Some(last) = merged.last_mut()
                && range.lsb <= last.msb.saturating_add(1)
            {
                last.msb = last.msb.max(range.msb);
            } else {
                merged.push(range);
            }
        }
        for range in merged {
            let Some(length) = range
                .msb
                .checked_sub(range.lsb)
                .and_then(|width| width.checked_add(1))
            else {
                return run.to_vec();
            };
            let resource = writer_by_resource.len();
            writer_by_resource.push(writer);
            resource_weights.push(length.saturating_add(63) / 64);
            writers_by_object.entry(object).or_default().push(resource);
            intervals.push(ExactInterval {
                object,
                start: range.lsb,
                length,
                value: resource,
            });
        }
    }
    for resources in writers_by_object.values_mut() {
        resources.sort_unstable();
        resources.dedup();
    }
    let Ok(writer_index) = DisjointIntervalMap::try_new(intervals) else {
        return run.to_vec();
    };

    // consumer -> producer is the preferred process order. It is an affinity
    // edge only: every stage still reads event-entry state and commits at the
    // common publication barrier.
    let comb_resource_base = resource_weights.len();
    resource_weights.extend(
        ir.comb_definitions()
            .iter()
            .map(|definition| definition.target.width().unwrap_or(1).saturating_add(63) / 64),
    );
    let mut process_resources = vec![Vec::<usize>::new(); run.len()];
    let mut successors = vec![Vec::<usize>::new(); run.len()];
    let add_static_read = |reader: usize,
                           range: super::ObjectRange,
                           successors: &mut [Vec<usize>],
                           process_resources: &mut [Vec<usize>]| {
        let Some(length) = range.width() else {
            return;
        };
        let Ok(overlapping) = writer_index.overlapping(&range.object, range.access.lsb, length)
        else {
            return;
        };
        for resource in overlapping {
            let writer = writer_by_resource[resource];
            if writer != reader {
                successors[reader].push(writer);
            }
            process_resources[reader].push(resource);
        }
    };
    let add_dynamic_read = |reader: usize,
                            object: AbsoluteAddr,
                            successors: &mut [Vec<usize>],
                            process_resources: &mut [Vec<usize>]| {
        if let Some(resources) = writers_by_object.get(&object) {
            for &resource in resources {
                let writer = writer_by_resource[resource];
                if writer != reader {
                    successors[reader].push(writer);
                }
                process_resources[reader].push(resource);
            }
        }
    };

    let accesses = collect_process_state_accesses(ir, run);
    for (reader, process) in run.iter().copied().enumerate() {
        for &range in &accesses[process.0].static_reads {
            add_static_read(reader, range, &mut successors, &mut process_resources);
        }
        for &object in &accesses[process.0].dynamic_reads {
            add_dynamic_read(reader, object, &mut successors, &mut process_resources);
        }
        for &definition in &accesses[process.0].comb_definitions {
            process_resources[reader].push(comb_resource_base + definition.0);
        }
    }

    for resources in &mut process_resources {
        resources.sort_unstable();
        resources.dedup();
    }
    let mut weighted_successors = vec![HashMap::<usize, usize>::default(); run.len()];
    for (reader, resources) in process_resources.iter().enumerate() {
        for &resource in resources {
            let Some(&writer) = writer_by_resource.get(resource) else {
                continue;
            };
            if reader != writer {
                *weighted_successors[reader].entry(writer).or_insert(0) +=
                    resource_weights[resource].max(1);
            }
        }
    }
    let weighted_successors = weighted_successors
        .into_iter()
        .map(|row| {
            let mut row = row.into_iter().collect::<Vec<_>>();
            row.sort_unstable();
            row
        })
        .collect::<Vec<_>>();
    let mut weighted_predecessors = vec![Vec::<(usize, usize)>::new(); run.len()];
    for (source, row) in weighted_successors.iter().enumerate() {
        for &(target, weight) in row {
            weighted_predecessors[target].push((source, weight));
        }
    }
    for row in &mut weighted_predecessors {
        row.sort_unstable();
    }
    successors = weighted_successors
        .iter()
        .map(|row| row.iter().map(|(target, _)| *target).collect())
        .collect();

    // Feedback does not remove ordering freedom. Model every shared old-state
    // range and comb definition as one value node used by process sinks. The
    // bottom-up weighted scheduler then minimizes the number of simultaneously
    // live materializations and keeps all users of one global value together.
    let mut used_resources = process_resources
        .iter()
        .flatten()
        .copied()
        .collect::<Vec<_>>();
    used_resources.sort_unstable();
    used_resources.dedup();
    let dense_resource = used_resources
        .iter()
        .copied()
        .enumerate()
        .map(|(dense, resource)| (resource, dense))
        .collect::<HashMap<_, _>>();
    let node_count = run.len() + used_resources.len();
    let mut materialization_dependencies = vec![Vec::<usize>::new(); node_count];
    let mut materialization_values = vec![Vec::<usize>::new(); node_count];
    for process in 0..run.len() {
        materialization_dependencies[process].extend(
            process_resources[process]
                .iter()
                .map(|resource| run.len() + dense_resource[resource]),
        );
        materialization_values[process] = materialization_dependencies[process].clone();
    }
    let mut weights = vec![0usize; run.len()];
    weights.extend(
        used_resources
            .iter()
            .map(|resource| resource_weights[*resource]),
    );
    let affinity_order = schedule_min_live_values_in_domains_with_weights(
        &materialization_dependencies,
        &materialization_values,
        &vec![0usize; node_count],
        &weights,
    )
    .ok()
    .map(|order| {
        order
            .into_iter()
            .filter(|node| *node < run.len())
            .collect::<Vec<_>>()
    })
    .filter(|order| order.len() == run.len())
    .unwrap_or_else(|| (0..run.len()).collect());
    let mut affinity_rank = vec![usize::MAX; run.len()];
    for (rank, process) in affinity_order.into_iter().enumerate() {
        affinity_rank[process] = rank;
    }

    let Ok(sccs) = StronglyConnectedComponents::analyze(&successors) else {
        return run.to_vec();
    };
    let component_count = sccs.components.len();
    let mut outgoing = vec![Vec::<usize>::new(); component_count];
    let mut indegree = vec![0usize; component_count];
    let mut keys = vec![usize::MAX; component_count];
    for (component, item) in sccs.components.iter().enumerate() {
        keys[component] = item
            .nodes
            .iter()
            .map(|node| affinity_rank[*node])
            .min()
            .unwrap_or(usize::MAX);
        for &node in &item.nodes {
            for &successor in &successors[node] {
                let target = sccs.component_for_node[successor];
                if component != target {
                    outgoing[component].push(target);
                }
            }
        }
        outgoing[component].sort_unstable();
        outgoing[component].dedup();
        for &target in &outgoing[component] {
            indegree[target] = indegree[target].saturating_add(1);
        }
    }
    let mut ready = BTreeSet::new();
    for component in 0..component_count {
        if indegree[component] == 0 {
            ready.insert((keys[component], component));
        }
    }
    let mut ordered = Vec::with_capacity(run.len());
    while let Some((_, component)) = ready.pop_first() {
        let nodes = order_feedback_component(
            &sccs.components[component].nodes,
            &weighted_successors,
            &weighted_predecessors,
            &affinity_rank,
        );
        ordered.extend(nodes.into_iter().map(|local| run[local]));
        for &target in &outgoing[component] {
            indegree[target] = indegree[target].saturating_sub(1);
            if indegree[target] == 0 {
                ready.insert((keys[target], target));
            }
        }
    }
    if ordered.len() == run.len() {
        ordered
    } else {
        run.to_vec()
    }
}

struct LocalStageOrder {
    ranks: Vec<(EffectId, usize)>,
    direct_effects: Vec<EffectId>,
    deferred_effects: Vec<EffectId>,
    publication_readers: Vec<(EffectId, Vec<EffectId>)>,
}

fn plan_local_stage_order(ir: &EventIr, process: ProcessId) -> Option<LocalStageOrder> {
    let process_item = &ir.processes()[process.0];
    let local_by_block = process_item
        .blocks
        .iter()
        .copied()
        .enumerate()
        .map(|(local, block)| (block, local))
        .collect::<HashMap<_, _>>();
    let successors = process_item
        .blocks
        .iter()
        .copied()
        .map(|block| {
            control_successors(ir, block)
                .into_iter()
                .map(|successor| local_by_block[&successor])
                .collect()
        })
        .collect();
    let control =
        ForwardControlFlowGraph::analyze_structure(successors, local_by_block[&process_item.entry])
            .ok()?;
    let block_by_region = process_item
        .blocks
        .iter()
        .copied()
        .map(|block| (ir.blocks()[block.0].region, block))
        .collect::<HashMap<_, _>>();
    let mut effects = Vec::<(EffectId, super::ObjectRange, ControlBlockId)>::new();
    for (index, effect) in ir.effects().iter().enumerate() {
        let EffectKind::StageNextFf {
            process: owner,
            target,
            ..
        } = &effect.kind
        else {
            continue;
        };
        if *owner != process {
            continue;
        }
        let &block = block_by_region.get(&effect.region)?;
        effects.push((
            EffectId(index),
            super::ObjectRange::new(target.object, target.alias),
            block,
        ));
    }
    if effects.is_empty() {
        return None;
    }
    let local_by_effect = effects
        .iter()
        .enumerate()
        .map(|(local, &(effect, _, _))| (effect, local))
        .collect::<HashMap<_, _>>();
    let writer_ranges = effects
        .iter()
        .map(|&(effect, range, _)| (effect, range))
        .collect::<Vec<_>>();
    let mut resolver = StateDependencyResolver::new(ir, &writer_ranges)?;
    let mut reads = vec![Vec::<usize>::new(); effects.len()];
    for (local, &(effect, _, _)) in effects.iter().enumerate() {
        let mut roots = Vec::new();
        ir.effects()[effect.0]
            .kind
            .visit_value_operands(|value| roots.push(value));
        reads[local] = resolver.resolve_roots(&roots)?;
    }

    let mut hard_successors = vec![Vec::<usize>::new(); effects.len()];
    for (local, &(effect, _, block)) in effects.iter().enumerate() {
        for predecessor in &ir.effects()[effect.0].predecessors {
            let &predecessor = local_by_effect.get(predecessor)?;
            let predecessor_block = effects[predecessor].2;
            if predecessor_block == block && predecessor != local {
                hard_successors[predecessor].push(local);
            } else if predecessor_block != block
                && !control
                    .dominators
                    .dominates(local_by_block[&predecessor_block], local_by_block[&block])
            {
                return None;
            }
        }
    }
    for successors in &mut hard_successors {
        successors.sort_unstable();
        successors.dedup();
    }

    // State edges are preferences rather than hard dependencies: a feedback
    // SCC cannot satisfy every consumer-before-writer edge. Prefer nodes which
    // consume many not-yet-overwritten resources and whose own target has few
    // remaining consumers, while always respecting effect-token order.
    let mut incoming_soft = vec![0usize; effects.len()];
    for dependencies in &reads {
        for &writer in dependencies {
            incoming_soft[writer] = incoming_soft[writer].saturating_add(1);
        }
    }
    let score = (0..effects.len())
        .map(|node| reads[node].len() as isize - incoming_soft[node] as isize)
        .collect::<Vec<_>>();
    let mut nodes_by_block = vec![Vec::new(); process_item.blocks.len()];
    for (node, &(_, _, block)) in effects.iter().enumerate() {
        nodes_by_block[local_by_block[&block]].push(node);
    }
    let mut rank = vec![usize::MAX; effects.len()];
    let mut ranks = Vec::with_capacity(effects.len());
    for nodes in nodes_by_block {
        let node_set = nodes.iter().copied().collect::<HashSet<_>>();
        let mut hard_indegree = HashMap::<usize, usize>::default();
        for &node in &nodes {
            hard_indegree.entry(node).or_insert(0);
            for &successor in &hard_successors[node] {
                if node_set.contains(&successor) {
                    *hard_indegree.entry(successor).or_insert(0) += 1;
                }
            }
        }
        let mut ready = BTreeSet::new();
        for &node in &nodes {
            if hard_indegree[&node] == 0 {
                ready.insert((-score[node], effects[node].0.0, node));
            }
        }
        let mut block_order = Vec::with_capacity(nodes.len());
        while let Some((_, _, node)) = ready.pop_first() {
            block_order.push(node);
            for &successor in &hard_successors[node] {
                if !node_set.contains(&successor) {
                    continue;
                }
                let indegree = hard_indegree.get_mut(&successor)?;
                *indegree = indegree.saturating_sub(1);
                if *indegree == 0 {
                    ready.insert((-score[successor], effects[successor].0.0, successor));
                }
            }
        }
        if block_order.len() != nodes.len() {
            return None;
        }
        for (position, node) in block_order.into_iter().enumerate() {
            rank[node] = position;
            ranks.push((effects[node].0, position));
        }
    }

    let mut control_reads = vec![Vec::<usize>::new(); process_item.blocks.len()];
    for (block_local, &block) in process_item.blocks.iter().enumerate() {
        let mut roots = Vec::new();
        ir.blocks()[block.0]
            .terminator
            .as_ref()?
            .visit_value_operands(|value| roots.push(value));
        control_reads[block_local] = resolver.resolve_roots(&roots)?;
    }
    let mut direct_effects = Vec::new();
    let mut deferred_effects = Vec::new();
    let mut publication_readers = Vec::new();
    for writer in 0..effects.len() {
        let writer_block = effects[writer].2;
        let writer_block_local = local_by_block[&writer_block];
        let writer_component = control.scc_for_block[writer_block_local];
        let writer_is_repeated = control.sccs[writer_component].cyclic;
        let control_read_is_late =
            control_reads
                .iter()
                .enumerate()
                .any(|(reader_block, dependencies)| {
                    dependencies.contains(&writer)
                        && ((writer_is_repeated
                            && control.scc_for_block[reader_block] == writer_component)
                            || reader_block == writer_block_local
                            || !control
                                .dominators
                                .dominates(reader_block, writer_block_local))
                });
        if control_read_is_late {
            continue;
        }
        let readers = reads
            .iter()
            .enumerate()
            .filter_map(|(reader, dependencies)| {
                dependencies
                    .contains(&writer)
                    .then_some((reader, effects[reader].0))
            })
            .collect::<Vec<_>>();
        let all_readers_precede = readers.iter().all(|&(reader, _)| {
            let reader_block = effects[reader].2;
            let reader_block_local = local_by_block[&reader_block];
            if writer_is_repeated && control.scc_for_block[reader_block_local] == writer_component {
                return false;
            }
            if reader == writer {
                return true;
            }
            if reader_block == writer_block {
                rank[reader] <= rank[writer]
            } else {
                control
                    .dominators
                    .dominates(reader_block_local, writer_block_local)
            }
        });
        publication_readers.push((
            effects[writer].0,
            readers.iter().map(|&(_, effect)| effect).collect(),
        ));
        if all_readers_precede {
            direct_effects.push(effects[writer].0);
        } else if readers
            .iter()
            .all(|&(reader, _)| effects[reader].2 == writer_block)
        {
            deferred_effects.push(effects[writer].0);
        }
    }
    if process_item.blocks.len() == 1 && !deferred_effects.is_empty() {
        deferred_effects.append(&mut direct_effects);
        deferred_effects.sort_unstable();
        deferred_effects.dedup();
    }
    Some(LocalStageOrder {
        ranks,
        direct_effects,
        deferred_effects,
        publication_readers,
    })
}

#[derive(Clone, Debug, Default)]
struct StatePublicationPlan {
    static_ranges: BTreeMap<AbsoluteAddr, Vec<super::BitAccess>>,
    fused_static_ranges: BTreeMap<AbsoluteAddr, Vec<super::BitAccess>>,
    sparse_widths: BTreeMap<AbsoluteAddr, usize>,
    fused_sparse_widths: BTreeMap<AbsoluteAddr, usize>,
    sparse_objects: HashSet<AbsoluteAddr>,
    fused_sparse_objects: HashSet<AbsoluteAddr>,
    direct_effects: HashSet<EffectId>,
    deferred_direct_effects: HashSet<EffectId>,
    publication_readers: HashMap<EffectId, Vec<EffectId>>,
    stage_ranks: HashMap<EffectId, usize>,
    capturable_static_components: BTreeSet<StaticPublicationComponent>,
}

impl StatePublicationPlan {
    fn build(
        ir: &EventIr,
        ordered_processes: &[ProcessId],
        selected_processes: &HashSet<ProcessId>,
    ) -> Self {
        let accesses = collect_process_state_accesses(ir, ordered_processes);
        let process_position = ordered_processes
            .iter()
            .copied()
            .enumerate()
            .map(|(position, process)| (process, position))
            .collect::<HashMap<_, _>>();
        let mut process_by_region = HashMap::default();
        for block in ir.blocks() {
            process_by_region.insert(block.region, block.process);
        }
        let mut pure = vec![true; ir.processes().len()];
        for effect in ir.effects() {
            let Some(process) = process_by_region.get(&effect.region).copied().or({
                if let EffectKind::StageNextFf { process, .. } = &effect.kind {
                    Some(*process)
                } else {
                    None
                }
            }) else {
                continue;
            };
            if selected_processes.contains(&process)
                && !matches!(
                    effect.kind,
                    EffectKind::StageNextFf { .. } | EffectKind::CommitFfState { .. }
                )
            {
                pure[process.0] = false;
            }
        }
        let overlaps = |lhs: super::ObjectRange, rhs: super::ObjectRange| {
            lhs.object == rhs.object
                && lhs.access.lsb <= rhs.access.msb
                && rhs.access.lsb <= lhs.access.msb
        };
        let mut stages = Vec::<(
            EffectId,
            ProcessId,
            super::ObjectRange,
            bool,
            super::FfStageKind,
        )>::new();
        for (index, effect) in ir.effects().iter().enumerate() {
            let EffectKind::StageNextFf {
                process,
                target,
                stage_kind,
                ..
            } = &effect.kind
            else {
                continue;
            };
            if selected_processes.contains(process) {
                stages.push((
                    EffectId(index),
                    *process,
                    super::ObjectRange::new(target.object, target.alias),
                    !matches!(target.offset, ValueOffset::Static(_)),
                    *stage_kind,
                ));
            }
        }
        let mut direct_effects = stages
            .iter()
            .filter_map(|&(effect, writer, target, _, stage_kind)| {
                let writer_position = process_position[&writer];
                let safe = ordered_processes.iter().copied().all(|reader| {
                    let reads_target = accesses[reader.0]
                        .static_reads
                        .iter()
                        .copied()
                        .any(|read| overlaps(read, target))
                        || accesses[reader.0].dynamic_reads.contains(&target.object);
                    !reads_target
                        || (reader == writer
                            && matches!(
                                stage_kind,
                                super::FfStageKind::FinalProcessSink
                                    | super::FfStageKind::WriteOnlyPublication
                            ))
                        || (reader != writer && process_position[&reader] < writer_position)
                });
                safe.then_some(effect)
            })
            .collect::<HashSet<_>>();
        let stage_by_effect = stages
            .iter()
            .map(|&(effect, process, target, ..)| (effect, (process, target)))
            .collect::<HashMap<_, _>>();
        let mut stage_ranks = HashMap::default();
        let mut deferred_direct_effects = HashSet::default();
        let mut publication_readers = HashMap::default();
        for &writer in ordered_processes {
            let writer_position = process_position[&writer];
            let Some(plan) = plan_local_stage_order(ir, writer) else {
                continue;
            };
            let LocalStageOrder {
                ranks,
                direct_effects: local_direct_effects,
                deferred_effects,
                publication_readers: local_publication_readers,
            } = plan;
            stage_ranks.extend(ranks);
            for (effect, deferred) in local_direct_effects
                .into_iter()
                .map(|effect| (effect, false))
                .chain(deferred_effects.into_iter().map(|effect| (effect, true)))
            {
                let Some(&(owner, target)) = stage_by_effect.get(&effect) else {
                    continue;
                };
                debug_assert_eq!(owner, writer);
                let cross_process_safe = ordered_processes.iter().copied().all(|reader| {
                    if reader == writer {
                        return true;
                    }
                    let reads_target = accesses[reader.0]
                        .static_reads
                        .iter()
                        .copied()
                        .any(|read| overlaps(read, target))
                        || accesses[reader.0].dynamic_reads.contains(&target.object);
                    !reads_target || process_position[&reader] < writer_position
                });
                if cross_process_safe {
                    direct_effects.insert(effect);
                    if deferred {
                        deferred_direct_effects.insert(effect);
                    }
                    if deferred
                        && let Some((_, readers)) = local_publication_readers
                            .iter()
                            .find(|(writer, _)| *writer == effect)
                    {
                        publication_readers.insert(effect, readers.clone());
                    }
                }
            }
        }

        // Straight-line FF processes are not semantic scheduling barriers.
        // Split every stage in one such run into compute/publication nodes and
        // require publication only after all run-local consumers have
        // captured the corresponding event-entry state.  This breaks
        // process-level feedback cycles without introducing WORKING memory:
        //
        //   A_next = f(B), B_next = g(A)
        //     => compute A, compute B, publish A, publish B
        //
        // Overlapping writers are left to the existing priority-preserving
        // path because the disjoint range index cannot prove one publication
        // order for them.
        for run in interleavable_process_runs(ir, ordered_processes) {
            if run.len() < 2 {
                continue;
            }
            let run_set = run.iter().copied().collect::<HashSet<_>>();
            let run_stages = stages
                .iter()
                .copied()
                .filter(|(_, process, ..)| run_set.contains(process))
                .collect::<Vec<_>>();
            let writer_ranges = run_stages
                .iter()
                .map(|&(effect, _, target, ..)| (effect, target))
                .collect::<Vec<_>>();
            let Some(mut resolver) = StateDependencyResolver::new(ir, &writer_ranges) else {
                continue;
            };
            let mut readers_by_writer = HashMap::<EffectId, Vec<EffectId>>::default();
            let mut resolved = true;
            for &(reader, ..) in &run_stages {
                let mut roots = Vec::new();
                ir.effects()[reader.0]
                    .kind
                    .visit_value_operands(|value| roots.push(value));
                let Some(writers) = resolver.resolve_roots(&roots) else {
                    resolved = false;
                    break;
                };
                for writer in writers {
                    readers_by_writer
                        .entry(writer_ranges[writer].0)
                        .or_default()
                        .push(reader);
                }
            }
            if !resolved {
                continue;
            }

            for &(effect, writer, target, ..) in &run_stages {
                let writer_position = process_position[&writer];
                let external_read_is_late = ordered_processes.iter().copied().any(|reader| {
                    if run_set.contains(&reader) {
                        return false;
                    }
                    let reads_target = accesses[reader.0]
                        .static_reads
                        .iter()
                        .copied()
                        .any(|read| overlaps(read, target))
                        || accesses[reader.0].dynamic_reads.contains(&target.object);
                    reads_target && process_position[&reader] > writer_position
                });
                if external_read_is_late {
                    continue;
                }
                direct_effects.insert(effect);
                deferred_direct_effects.insert(effect);
                let readers = readers_by_writer.entry(effect).or_default();
                readers.sort_unstable();
                readers.dedup();
                publication_readers
                    .entry(effect)
                    .or_default()
                    .extend(readers.iter().copied());
            }
        }
        for readers in publication_readers.values_mut() {
            readers.sort_unstable();
            readers.dedup();
        }

        // A sparse dynamic commit can touch any aliased bit of its object, and
        // a static WORKING commit copies complete touched bytes. Neither may
        // run after an overlapping direct STABLE store. Recompute sparse
        // objects while demoting conflicts until the publication partition is
        // stable.
        let sparse_objects = stages
            .iter()
            .filter_map(|&(_, _, target, dynamic, _)| dynamic.then_some(target.object))
            .collect::<HashSet<_>>();
        let mut stages_by_object = BTreeMap::<AbsoluteAddr, Vec<usize>>::new();
        for (stage, &(_, _, target, _, _)) in stages.iter().enumerate() {
            stages_by_object
                .entry(target.object)
                .or_default()
                .push(stage);
        }
        for stage_indices in stages_by_object.values() {
            // One non-direct dynamic write makes the sparse commit capable of
            // touching any aliased byte in the object. If every staged writer
            // belongs to an earlier process and no later process reads one of
            // those ranges, publish once at that process boundary and retain
            // direct publication for the write-only suffix.
            let has_non_direct_dynamic = stage_indices.iter().copied().any(|stage| {
                let (effect, _, _, dynamic, _) = stages[stage];
                dynamic && !direct_effects.contains(&effect)
            });
            if has_non_direct_dynamic {
                for &stage in stage_indices {
                    direct_effects.remove(&stages[stage].0);
                }
                continue;
            }

            // Static WORKING commits copy complete touched bytes. Connected
            // byte-overlap components therefore have one publication mode:
            // if one member requires WORKING, every direct member in that
            // component must be demoted. A demoted dynamic member turns the
            // whole object sparse, handled after this sweep.
            let mut byte_ranges = stage_indices
                .iter()
                .copied()
                .map(|stage| {
                    let target = stages[stage].2;
                    (target.access.lsb & !7, target.access.msb | 7, stage)
                })
                .collect::<Vec<_>>();
            byte_ranges.sort_unstable();
            let mut component_start = 0;
            let mut demoted_dynamic = false;
            while component_start < byte_ranges.len() {
                let mut component_end = component_start + 1;
                let mut covered_end = byte_ranges[component_start].1;
                while component_end < byte_ranges.len()
                    && byte_ranges[component_end].0 <= covered_end
                {
                    covered_end = covered_end.max(byte_ranges[component_end].1);
                    component_end += 1;
                }
                let requires_working = byte_ranges[component_start..component_end]
                    .iter()
                    .any(|&(_, _, stage)| !direct_effects.contains(&stages[stage].0));
                if requires_working {
                    for &(_, _, stage) in &byte_ranges[component_start..component_end] {
                        let (effect, _, _, dynamic, _) = stages[stage];
                        if direct_effects.remove(&effect) {
                            demoted_dynamic |= dynamic;
                        }
                    }
                }
                component_start = component_end;
            }
            if demoted_dynamic {
                for &stage in stage_indices {
                    direct_effects.remove(&stages[stage].0);
                }
            }
        }
        let fused_sparse_objects = stages
            .iter()
            .filter_map(|&(effect, _, target, dynamic, _)| {
                (dynamic && !direct_effects.contains(&effect)).then_some(target.object)
            })
            .collect::<HashSet<_>>();
        deferred_direct_effects.retain(|effect| direct_effects.contains(effect));
        publication_readers.retain(|effect, _| direct_effects.contains(effect));

        let mut static_ranges: BTreeMap<AbsoluteAddr, Vec<super::BitAccess>> = BTreeMap::new();
        let mut fused_static_ranges: BTreeMap<AbsoluteAddr, Vec<super::BitAccess>> =
            BTreeMap::new();
        let mut sparse_widths: BTreeMap<AbsoluteAddr, usize> = BTreeMap::new();
        let mut fused_sparse_widths: BTreeMap<AbsoluteAddr, usize> = BTreeMap::new();
        for (index, effect) in ir.effects().iter().enumerate() {
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
            if fused_sparse_objects.contains(&target.object) {
                fused_sparse_widths
                    .entry(target.object)
                    .and_modify(|width| {
                        *width = (*width).max(target.alias.msb.saturating_add(1));
                    })
                    .or_insert_with(|| target.alias.msb.saturating_add(1));
            } else if !direct_effects.contains(&EffectId(index)) {
                fused_static_ranges
                    .entry(target.object)
                    .or_default()
                    .push(target.alias);
            }
        }
        for ranges in static_ranges.values_mut() {
            *ranges = merge_ranges(std::mem::take(ranges));
        }
        for ranges in fused_static_ranges.values_mut() {
            *ranges = merge_ranges(std::mem::take(ranges));
        }
        let mut capturable_static_components = BTreeSet::new();
        for (&object, ranges) in &fused_static_ranges {
            for range in ranges {
                let component = super::ObjectRange::new(object, *range);
                let mut first = usize::MAX;
                let mut last = 0usize;
                let mut has_writer = false;
                let mut safe = true;
                for &(_, writer, target, dynamic, _) in &stages {
                    if dynamic || !overlaps(target, component) {
                        continue;
                    }
                    has_writer = true;
                    safe &= pure[writer.0];
                    let position = process_position[&writer];
                    first = first.min(position);
                    last = last.max(position);
                }
                for &reader in ordered_processes {
                    let reads_component = accesses[reader.0]
                        .static_reads
                        .iter()
                        .copied()
                        .any(|read| overlaps(read, component))
                        || accesses[reader.0].dynamic_reads.contains(&object);
                    if reads_component {
                        safe &= pure[reader.0];
                        let position = process_position[&reader];
                        first = first.min(position);
                        last = last.max(position);
                    }
                }
                if !has_writer || !safe || first == usize::MAX {
                    continue;
                }
                if ordered_processes[first..=last]
                    .iter()
                    .any(|process| !pure[process.0])
                {
                    continue;
                }
                capturable_static_components.insert(StaticPublicationComponent {
                    object,
                    offset: range.lsb,
                    width: range.msb - range.lsb + 1,
                });
            }
        }
        Self {
            static_ranges,
            fused_static_ranges,
            sparse_widths,
            fused_sparse_widths,
            sparse_objects,
            fused_sparse_objects,
            direct_effects,
            deferred_direct_effects,
            publication_readers,
            stage_ranks,
            capturable_static_components,
        }
    }

    fn seeds(&self, fused: bool) -> Vec<SIRInstruction<RegionedAbsoluteAddr>> {
        let static_ranges = if fused {
            &self.fused_static_ranges
        } else {
            &self.static_ranges
        };
        static_ranges
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

    fn commits(&self, fused: bool) -> Vec<SIRInstruction<RegionedAbsoluteAddr>> {
        let static_ranges = if fused {
            &self.fused_static_ranges
        } else {
            &self.static_ranges
        };
        let mut result = static_ranges
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
        let sparse_widths = if fused {
            &self.fused_sparse_widths
        } else {
            &self.sparse_widths
        };
        result.extend(sparse_widths.iter().map(|(&object, &width)| {
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

    fn is_direct_effect(&self, effect: EffectId) -> bool {
        self.direct_effects.contains(&effect)
    }

    fn is_deferred_direct_effect(&self, effect: EffectId) -> bool {
        self.deferred_direct_effects.contains(&effect)
    }

    fn publication_readers(&self, effect: EffectId) -> &[EffectId] {
        self.publication_readers
            .get(&effect)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    fn stage_rank(&self, effect: EffectId) -> Option<usize> {
        self.stage_ranks.get(&effect).copied()
    }

    fn stage_region(&self, object: AbsoluteAddr, fused: bool) -> u32 {
        let sparse_objects = if fused {
            &self.fused_sparse_objects
        } else {
            &self.sparse_objects
        };
        let static_ranges = if fused {
            &self.fused_static_ranges
        } else {
            &self.static_ranges
        };
        if sparse_objects.contains(&object) {
            SPARSE_WORKING_REGION
        } else if static_ranges.contains_key(&object) {
            WORKING_REGION
        } else {
            STABLE_REGION
        }
    }
}

fn merge_ranges(mut ranges: Vec<super::BitAccess>) -> Vec<super::BitAccess> {
    // Static staging storage is private for the duration of one event. Seed
    // and commit the complete touched bytes: bits outside an FF write are
    // copied from stable state before evaluation and copied back unchanged.
    // This converts narrow publication RMWs into ordinary byte copies without
    // changing the StageNextFf ranges or their priority.
    for range in &mut ranges {
        range.lsb &= !7;
        range.msb |= 7;
    }
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
        instructions: state.commits(false),
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

#[derive(Debug, Clone)]
enum BlockScheduleNode {
    CombDefinition(CombMaterializationId, CombDefinitionId),
    CombValue(ValueId),
    StageCompute(EffectId),
    StagePublish(Vec<EffectId>),
    Effects(Vec<EffectId>),
    Exit,
}

#[derive(Debug, Clone)]
struct ScheduledBlockNode {
    process: ProcessId,
    block: ControlBlockId,
    node: BlockScheduleNode,
}

#[derive(Clone)]
struct PreparedStage {
    value: RegisterId,
    offset: SIROffset,
    guard: Option<RegisterId>,
}

fn effect_guard(kind: &EffectKind) -> Option<ValueId> {
    match kind {
        EffectKind::StageNextFf { guard, .. }
        | EffectKind::WritePersistentMemory { guard, .. }
        | EffectKind::RuntimeEvent { guard, .. }
        | EffectKind::Capture { guard, .. } => *guard,
        EffectKind::TriggerPublication { .. }
        | EffectKind::CommitFfState { .. }
        | EffectKind::RuntimeObservationBarrier => None,
    }
}

type SltInputBinding = Vec<(crate::ir::VarAtomBase<AbsoluteAddr>, RegisterId)>;

#[derive(Clone, Hash, PartialEq, Eq)]
enum SltSemanticKey {
    Node {
        node: NodeId,
        children: Vec<usize>,
    },
    Input {
        node: NodeId,
        indices: Vec<usize>,
        bindings: SltInputBinding,
    },
    Opaque {
        node: NodeId,
        bindings: SltInputBinding,
    },
}

#[derive(Default)]
struct SemanticSltCache {
    identities: HashMap<SltSemanticKey, usize>,
    environment_dependent: Vec<bool>,
    values: HashMap<usize, RegisterId>,
}

impl SemanticSltCache {
    fn prepare(
        &mut self,
        root: NodeId,
        arena: &SLTNodeArena<AbsoluteAddr>,
        inputs: &HashMap<crate::ir::VarAtomBase<AbsoluteAddr>, RegisterId>,
        cache: &mut HashMap<NodeId, RegisterId>,
    ) -> HashMap<NodeId, usize> {
        let mut semantic_ids = HashMap::default();
        let mut work = vec![(root, false)];
        while let Some((node, expanded)) = work.pop() {
            if semantic_ids.contains_key(&node) {
                continue;
            }
            if !expanded {
                work.push((node, true));
                for child in slt_value_children(arena.get(node)).into_iter().rev() {
                    if !semantic_ids.contains_key(&child) {
                        work.push((child, false));
                    }
                }
                continue;
            }

            let key = match arena.get(node) {
                crate::logic_tree::SLTNode::Input {
                    variable,
                    index,
                    access,
                    ..
                } => {
                    let mut bindings = inputs
                        .iter()
                        .filter_map(|(range, register)| {
                            (range.id == *variable
                                && (!index.is_empty() || range.access.overlaps(access)))
                            .then_some((*range, *register))
                        })
                        .collect::<SltInputBinding>();
                    bindings.sort_unstable();
                    SltSemanticKey::Input {
                        node,
                        indices: index
                            .iter()
                            .map(|index| semantic_ids[&index.node])
                            .collect(),
                        bindings,
                    }
                }
                crate::logic_tree::SLTNode::ForFold { .. }
                | crate::logic_tree::SLTNode::ForFoldGroup { .. } => {
                    let mut bindings = inputs
                        .iter()
                        .map(|(range, register)| (*range, *register))
                        .collect::<SltInputBinding>();
                    bindings.sort_unstable();
                    SltSemanticKey::Opaque { node, bindings }
                }
                source => SltSemanticKey::Node {
                    node,
                    children: slt_value_children(source)
                        .into_iter()
                        .map(|child| semantic_ids[&child])
                        .collect(),
                },
            };
            let environment_dependent = match &key {
                SltSemanticKey::Node { children, .. } => children
                    .iter()
                    .any(|identity| self.environment_dependent[*identity]),
                SltSemanticKey::Input {
                    indices, bindings, ..
                } => {
                    !bindings.is_empty()
                        || indices
                            .iter()
                            .any(|identity| self.environment_dependent[*identity])
                }
                SltSemanticKey::Opaque { bindings, .. } => !bindings.is_empty(),
            };
            let identity = if let Some(&identity) = self.identities.get(&key) {
                identity
            } else {
                let identity = self.identities.len();
                self.identities.insert(key, identity);
                self.environment_dependent.push(environment_dependent);
                identity
            };
            if let Some(&register) = self.values.get(&identity) {
                cache.entry(node).or_insert(register);
            }
            semantic_ids.insert(node, identity);
        }
        semantic_ids
    }

    fn record(
        &mut self,
        semantic_ids: &HashMap<NodeId, usize>,
        cache: &HashMap<NodeId, RegisterId>,
    ) {
        for (&node, &identity) in semantic_ids {
            if let Some(&register) = cache.get(&node) {
                self.values.entry(identity).or_insert(register);
            }
        }
    }

    fn clear_environment_dependent_values(&mut self) {
        self.values
            .retain(|identity, _| !self.environment_dependent[*identity]);
    }
}

#[derive(Default)]
struct BlockMaterializationCache {
    comb: HashMap<CombDefinitionId, RegisterId>,
    semantic_slt: SemanticSltCache,
}

#[derive(Clone, Copy, Default)]
struct DefinitionConsumers {
    first: Option<ProcessId>,
    second: Option<ProcessId>,
    many_processes: bool,
}

impl DefinitionConsumers {
    fn add(&mut self, process: ProcessId) {
        if self.first == Some(process) || self.second == Some(process) {
            return;
        }
        if self.first.is_none() {
            self.first = Some(process);
        } else if self.second.is_none() {
            self.second = Some(process);
        } else {
            self.many_processes = true;
        }
    }

    fn merge(&mut self, other: Self) {
        if let Some(process) = other.first {
            self.add(process);
        }
        if let Some(process) = other.second {
            self.add(process);
        }
        self.many_processes |= other.many_processes;
    }

    fn crosses_processes(self) -> bool {
        self.second.is_some()
    }
}

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
    settled_comb_reads: bool,
    builder: SIRBuilder<AbsoluteAddr>,
    state: StatePublicationPlan,
    sir_blocks: Vec<BlockId>,
    parameter_registers: Vec<Option<RegisterId>>,
    effects_by_block: Vec<Vec<EffectId>>,
    continuation_by_process: Vec<BlockId>,
    placed_comb_values: HashMap<(ProcessId, ValueId), PlacedValue>,
    placed_comb_definitions: Vec<Option<PlacedValue>>,
    comb_materialization_by_definition: Vec<Option<CombMaterializationId>>,
    homed_comb_definitions: Vec<bool>,
    comb_home_offsets: Vec<Option<usize>>,
    comb_home_loads: HashSet<RegisterId>,
    comb_home_stores: HashSet<(BlockId, usize)>,
    stage_stores: HashSet<(BlockId, usize)>,
    direct_stage_stores: HashSet<(BlockId, usize)>,
    prepared_stages: HashMap<EffectId, PreparedStage>,
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
        settled_comb_reads: bool,
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
            settled_comb_reads,
            builder: SIRBuilder::new(),
            state,
            sir_blocks: vec![BlockId(usize::MAX); ir.blocks().len()],
            parameter_registers: vec![None; ir.values().len()],
            effects_by_block: vec![Vec::new(); ir.blocks().len()],
            continuation_by_process: Vec::new(),
            placed_comb_values: HashMap::default(),
            placed_comb_definitions: Vec::new(),
            comb_materialization_by_definition: vec![None; ir.comb_definitions().len()],
            homed_comb_definitions: vec![false; ir.comb_definitions().len()],
            comb_home_offsets: vec![None; ir.comb_definitions().len()],
            comb_home_loads: HashSet::default(),
            comb_home_stores: HashSet::default(),
            stage_stores: HashSet::default(),
            direct_stage_stores: HashSet::default(),
            prepared_stages: HashMap::default(),
            comb_values_by_block: vec![Vec::new(); ir.blocks().len()],
            comb_definitions_by_block: vec![Vec::new(); ir.blocks().len()],
            selected_processes,
            selected_process_set,
            final_block: BlockId(usize::MAX),
        })
    }

    fn lower(
        self,
        projection: EventProjection,
    ) -> Result<ExecutionUnit<RegionedAbsoluteAddr>, EventProjectionError> {
        self.lower_with_publication(projection, true, true)
    }

    fn lower_with_publication(
        mut self,
        projection: EventProjection,
        include_seed: bool,
        include_commit: bool,
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

        let mut merged_away_blocks = Vec::new();
        for run in interleavable_process_runs(self.ir, &self.selected_processes) {
            if run.len() > 1 {
                merged_away_blocks.extend(self.lower_interleaved_process_run(&run)?);
            } else {
                let process = run[0];
                for block in self.control.dominator_preorder(process) {
                    self.lower_block(process, block)?;
                }
            }
        }
        if let Some(start) = emission_start {
            eprintln!("[eir] sparse comb emission: {:?}", start.elapsed());
        }

        self.builder.switch_to_block(self.final_block);
        self.builder.seal_block(SIRTerminator::Return);
        let (mut blocks, register_map, _) = self.builder.drain();
        for block in merged_away_blocks {
            blocks.remove(&block);
        }
        let fused = matches!(
            projection,
            EventProjection::FusedClock | EventProjection::FusedSettledClock
        );
        let mut result = map_clock_body(
            blocks,
            register_map,
            &self.state,
            &self.comb_home_loads,
            &self.comb_home_stores,
            &self.stage_stores,
            &self.direct_stage_stores,
            fused,
        )?;
        if include_seed {
            result
                .blocks
                .get_mut(&result.entry_block_id)
                .expect("projection entry block exists")
                .instructions
                .splice(0..0, self.state.seeds(fused));
        }
        if fused && include_commit {
            result
                .blocks
                .get_mut(&self.final_block)
                .expect("projection final block exists")
                .instructions
                .extend(self.state.commits(true));
            capture_local_static_snapshot_publications(
                &mut result,
                &self.state.capturable_static_components,
            )?;
        }
        Ok(result)
    }

    fn create_blocks(&mut self) -> Result<(), EventProjectionError> {
        for process in self.selected_processes.clone() {
            for block_id in self.control.dominator_preorder(process) {
                let block = &self.ir.blocks()[block_id.0];
                if self.ir.processes()[block.process.0].entry == block_id
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
                self.sir_blocks[block_id.0] = self.builder.new_block_with(parameters);
            }
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

    fn block_schedule(
        &self,
        block: ControlBlockId,
    ) -> Result<Vec<ScheduledBlockNode>, EventProjectionError> {
        self.region_schedule(&[(self.ir.blocks()[block.0].process, block)])
    }

    fn region_schedule(
        &self,
        blocks: &[(ProcessId, ControlBlockId)],
    ) -> Result<Vec<ScheduledBlockNode>, EventProjectionError> {
        let node_capacity = blocks
            .iter()
            .map(|(_, block)| {
                self.comb_definitions_by_block[block.0].len()
                    + self.comb_values_by_block[block.0].len()
                    + self.effects_by_block[block.0].len()
            })
            .sum::<usize>()
            + 1;
        let mut nodes = Vec::with_capacity(node_capacity);
        let mut definition_nodes = HashMap::default();
        let mut value_nodes = HashMap::default();
        let mut effect_nodes = HashMap::default();
        let mut stage_compute_nodes = HashMap::default();
        let mut guarded_groups = HashMap::<(ProcessId, ControlBlockId, ValueId), usize>::default();
        let mut guarded_stage_publish_groups =
            HashMap::<(ProcessId, ControlBlockId, Option<ValueId>), usize>::default();
        for &(process, block) in blocks {
            for &(materialization, definition) in &self.comb_definitions_by_block[block.0] {
                let node = nodes.len();
                nodes.push(ScheduledBlockNode {
                    process,
                    block,
                    node: BlockScheduleNode::CombDefinition(materialization, definition),
                });
                definition_nodes.insert(definition, node);
            }
            for &value in &self.comb_values_by_block[block.0] {
                let node = nodes.len();
                nodes.push(ScheduledBlockNode {
                    process,
                    block,
                    node: BlockScheduleNode::CombValue(value),
                });
                value_nodes.insert(value, node);
            }
            for &effect in &self.effects_by_block[block.0] {
                let guard = effect_guard(&self.ir.effects()[effect.0].kind);
                if self.state.is_deferred_direct_effect(effect) {
                    let compute = nodes.len();
                    nodes.push(ScheduledBlockNode {
                        process,
                        block,
                        node: BlockScheduleNode::StageCompute(effect),
                    });
                    stage_compute_nodes.insert(effect, compute);

                    let publish_key = (process, block, guard);
                    let publish =
                        if let Some(&publish) = guarded_stage_publish_groups.get(&publish_key) {
                            let BlockScheduleNode::StagePublish(group) = &mut nodes[publish].node
                            else {
                                unreachable!("stage publication groups contain only stage effects");
                            };
                            group.push(effect);
                            publish
                        } else {
                            let publish = nodes.len();
                            nodes.push(ScheduledBlockNode {
                                process,
                                block,
                                node: BlockScheduleNode::StagePublish(vec![effect]),
                            });
                            guarded_stage_publish_groups.insert(publish_key, publish);
                            publish
                        };
                    effect_nodes.insert(effect, publish);
                    continue;
                }
                if let Some(guard) = guard
                    && let Some(&node) = guarded_groups.get(&(process, block, guard))
                {
                    let BlockScheduleNode::Effects(group) = &mut nodes[node].node else {
                        unreachable!("guard domains contain only effects");
                    };
                    group.push(effect);
                    effect_nodes.insert(effect, node);
                    continue;
                }
                let node = nodes.len();
                nodes.push(ScheduledBlockNode {
                    process,
                    block,
                    node: BlockScheduleNode::Effects(vec![effect]),
                });
                effect_nodes.insert(effect, node);
                if let Some(guard) = guard {
                    guarded_groups.insert((process, block, guard), node);
                }
            }
        }
        for scheduled in &mut nodes {
            if let BlockScheduleNode::Effects(effects) | BlockScheduleNode::StagePublish(effects) =
                &mut scheduled.node
            {
                effects.sort_unstable_by_key(|effect| {
                    (
                        self.state.stage_rank(*effect).unwrap_or(usize::MAX),
                        effect.0,
                    )
                });
            }
        }
        let exit = nodes.len();
        let (exit_process, exit_block) = blocks
            .first()
            .copied()
            .unwrap_or((ProcessId(0), ControlBlockId(0)));
        nodes.push(ScheduledBlockNode {
            process: exit_process,
            block: exit_block,
            node: BlockScheduleNode::Exit,
        });

        let mut dependencies = vec![Vec::new(); nodes.len()];
        let mut value_dependencies = vec![Vec::new(); nodes.len()];
        let comb_graph = self
            .comb_value_graph
            .as_ref()
            .expect("reachable comb values have a sparse graph");
        let carries_register = |producer: usize, user: usize| {
            !matches!(
                nodes[producer].node,
                BlockScheduleNode::CombDefinition(_, definition)
                    if self.homed_comb_definitions[definition.0]
                        && nodes[producer].process != nodes[user].process
            )
        };
        for (&definition, &node) in &definition_nodes {
            let recipe = self.ir.comb_definitions()[definition.0].recipe;
            for dependency in comb_graph.eager_dependencies(recipe) {
                if let Some(&producer) = definition_nodes.get(&dependency) {
                    dependencies[node].push(producer);
                    if carries_register(producer, node) {
                        value_dependencies[node].push(producer);
                    }
                }
            }
            value_dependencies[node].extend(
                dependencies[node]
                    .iter()
                    .copied()
                    .filter(|producer| carries_register(*producer, node)),
            );
        }
        for (&value, &node) in &value_nodes {
            let mut visited = HashSet::default();
            self.collect_local_value_producers(
                value,
                &value_nodes,
                &definition_nodes,
                &mut dependencies[node],
                &mut visited,
                Some(value),
            );
            value_dependencies[node].extend(
                dependencies[node]
                    .iter()
                    .copied()
                    .filter(|producer| carries_register(*producer, node)),
            );
        }
        for (&effect, &node) in &stage_compute_nodes {
            let mut visited = HashSet::default();
            self.ir.effects()[effect.0]
                .kind
                .visit_value_operands(|value| {
                    self.collect_local_value_producers(
                        value,
                        &value_nodes,
                        &definition_nodes,
                        &mut dependencies[node],
                        &mut visited,
                        None,
                    );
                });
            value_dependencies[node].extend(dependencies[node].iter().copied().filter(
                |producer| {
                    matches!(
                        &nodes[*producer].node,
                        BlockScheduleNode::CombDefinition(..) | BlockScheduleNode::CombValue(..)
                    ) && carries_register(*producer, node)
                },
            ));
        }
        for node in 0..nodes.len() {
            let BlockScheduleNode::Effects(group) = &nodes[node].node else {
                continue;
            };
            for &effect in group {
                for predecessor in &self.ir.effects()[effect.0].predecessors {
                    if let Some(&predecessor) = effect_nodes.get(predecessor)
                        && predecessor != node
                    {
                        dependencies[node].push(predecessor);
                    }
                }
                let mut visited = HashSet::default();
                self.ir.effects()[effect.0]
                    .kind
                    .visit_value_operands(|value| {
                        self.collect_local_value_producers(
                            value,
                            &value_nodes,
                            &definition_nodes,
                            &mut dependencies[node],
                            &mut visited,
                            None,
                        );
                    });
            }
            value_dependencies[node].extend(dependencies[node].iter().copied().filter(
                |producer| {
                    matches!(
                        &nodes[*producer].node,
                        BlockScheduleNode::CombDefinition(..) | BlockScheduleNode::CombValue(..)
                    ) && carries_register(*producer, node)
                },
            ));
        }
        for node in 0..nodes.len() {
            let BlockScheduleNode::StagePublish(group) = &nodes[node].node else {
                continue;
            };
            for &effect in group {
                let compute = stage_compute_nodes[&effect];
                dependencies[node].push(compute);
                value_dependencies[node].push(compute);
                for predecessor in &self.ir.effects()[effect.0].predecessors {
                    if let Some(&predecessor) = effect_nodes.get(predecessor)
                        && predecessor != node
                    {
                        dependencies[node].push(predecessor);
                    }
                }
                for &reader in self.state.publication_readers(effect) {
                    if let Some(&reader) = stage_compute_nodes.get(&reader) {
                        dependencies[node].push(reader);
                    } else if let Some(&reader) = effect_nodes.get(&reader)
                        && reader != node
                    {
                        dependencies[node].push(reader);
                    }
                }
            }
        }
        let mut rank_by_stage_node = HashMap::<usize, usize>::default();
        for (&effect, &node) in &effect_nodes {
            let Some(rank) = self.state.stage_rank(effect) else {
                continue;
            };
            rank_by_stage_node
                .entry(node)
                .and_modify(|current| *current = (*current).min(rank))
                .or_insert(rank);
        }
        let mut ranked_stage_nodes =
            HashMap::<(ProcessId, ControlBlockId), Vec<(usize, usize)>>::default();
        for (node, rank) in rank_by_stage_node {
            ranked_stage_nodes
                .entry((nodes[node].process, nodes[node].block))
                .or_default()
                .push((rank, node));
        }
        for mut block_nodes in ranked_stage_nodes.into_values() {
            block_nodes.sort_unstable();
            for pair in block_nodes.windows(2) {
                let predecessor = pair[0].1;
                let node = pair[1].1;
                if predecessor != node {
                    dependencies[node].push(predecessor);
                }
            }
        }

        // Exit is the block scheduling barrier. Every local computation and
        // effect must precede it, while only values consumed by the actual
        // terminator are live through the barrier.
        dependencies[exit].extend(0..exit);
        for &(_, block) in blocks {
            let mut visited = HashSet::default();
            self.ir.blocks()[block.0]
                .terminator
                .as_ref()
                .expect("verified EIR block is terminated")
                .visit_value_operands(|value| {
                    self.collect_local_value_producers(
                        value,
                        &value_nodes,
                        &definition_nodes,
                        &mut value_dependencies[exit],
                        &mut visited,
                        None,
                    );
                });
        }

        for row in &mut dependencies {
            row.sort_unstable();
            row.dedup();
        }
        for row in &mut value_dependencies {
            row.sort_unstable();
            row.dedup();
        }
        let domains = if blocks.len() > 1 {
            // The complete straight-line run is one materialization domain:
            // values may legally cross former process boundaries, and the
            // scheduler must be able to interleave their ready cones.
            vec![0; nodes.len()]
        } else {
            nodes.iter().map(|node| node.process.0).collect::<Vec<_>>()
        };
        let value_weights = nodes
            .iter()
            .map(|node| match node.node {
                BlockScheduleNode::CombDefinition(_, definition) => self.register_chunks(
                    self.ir.comb_definitions()[definition.0]
                        .target
                        .width()
                        .expect("verified combinational definition range"),
                    self.four_state,
                ),
                BlockScheduleNode::CombValue(value) => {
                    let ty = self.ir.values()[value.0].ty;
                    self.register_chunks(ty.width, ty.four_state)
                }
                BlockScheduleNode::StageCompute(effect) => {
                    let EffectKind::StageNextFf { target, .. } = &self.ir.effects()[effect.0].kind
                    else {
                        unreachable!("only FF stages have split compute nodes");
                    };
                    self.register_chunks(target.width, self.four_state)
                        .saturating_add(2)
                }
                BlockScheduleNode::StagePublish(_)
                | BlockScheduleNode::Effects(_)
                | BlockScheduleNode::Exit => 0,
            })
            .collect::<Vec<_>>();
        let order = schedule_min_live_values_in_domains_with_weights(
            &dependencies,
            &value_dependencies,
            &domains,
            &value_weights,
        )
        .map_err(|error| EventProjectionError::BlockScheduling {
            block: exit_block,
            error,
        })?;
        Ok(order.into_iter().map(|node| nodes[node].clone()).collect())
    }

    fn register_chunks(&self, width: usize, four_state: bool) -> usize {
        let chunks = width.saturating_add(63) / 64;
        chunks.saturating_mul(if four_state { 2 } else { 1 })
    }

    fn collect_local_value_producers(
        &self,
        value: ValueId,
        value_nodes: &HashMap<ValueId, usize>,
        definition_nodes: &HashMap<CombDefinitionId, usize>,
        result: &mut Vec<usize>,
        visited: &mut HashSet<ValueId>,
        exclude_value: Option<ValueId>,
    ) {
        if !visited.insert(value) {
            return;
        }
        if Some(value) != exclude_value
            && let Some(&producer) = value_nodes.get(&value)
        {
            result.push(producer);
            return;
        }
        if let ValueKind::ReadCombDefinition { definition, .. } = self.ir.values()[value.0].kind {
            if let Some(&producer) = definition_nodes.get(&definition) {
                result.push(producer);
            }
            return;
        }
        self.ir.values()[value.0].kind.visit_operands(|operand| {
            self.collect_local_value_producers(
                operand,
                value_nodes,
                definition_nodes,
                result,
                visited,
                exclude_value,
            );
        });
    }

    fn emit_region_schedule(
        &mut self,
        schedule: Vec<ScheduledBlockNode>,
        cache: &mut HashMap<ValueId, RegisterId>,
        materialization_cache: &mut BlockMaterializationCache,
        reset_process_local_caches: bool,
    ) -> Result<(), EventProjectionError> {
        let mut active_process = None;
        for scheduled in schedule {
            let process = scheduled.process;
            let block = scheduled.block;
            if reset_process_local_caches && active_process != Some(process) {
                if active_process.is_some() {
                    cache.clear();
                    materialization_cache.comb.clear();
                    materialization_cache
                        .semantic_slt
                        .clear_environment_dependent_values();
                }
                active_process = Some(process);
            }
            match scheduled.node {
                BlockScheduleNode::CombDefinition(materialization, definition) => {
                    let mut values = ValueMaterializer::new_with_caches(
                        self.ir,
                        self.comb_value_graph
                            .as_ref()
                            .expect("reachable comb values have a sparse graph"),
                        self.arena,
                        &self.slt,
                        self.settled_comb_reads,
                        &self.control,
                        self.event_control
                            .as_ref()
                            .expect("a non-empty clock projection has an event CFG"),
                        &self.placed_comb_values,
                        &self.comb_materialization_by_definition,
                        &self.placed_comb_definitions,
                        &self.homed_comb_definitions,
                        &self.comb_home_offsets,
                        &mut self.comb_home_loads,
                        &mut self.builder,
                        process,
                        block,
                        cache,
                        std::mem::take(materialization_cache),
                    );
                    values.scheduled_comb_definitions.insert(definition);
                    let register = values.materialize_comb_definition(definition)?;
                    if values.homed_comb_definitions[definition.0] {
                        let target = self.ir.comb_definitions()[definition.0].target;
                        let home_offset = values.comb_home_offsets[definition.0]
                            .expect("a homed definition has a private slot");
                        let store_location = (
                            values.builder.current_block(),
                            values.builder.current_instruction_index(),
                        );
                        values.builder.emit(SIRInstruction::Store(
                            target.object,
                            SIROffset::Static(home_offset),
                            target
                                .width()
                                .expect("verified combinational definition range"),
                            register,
                            Vec::new(),
                            Vec::new(),
                        ));
                        self.comb_home_stores.insert(store_location);
                    }
                    *materialization_cache = values.into_caches();
                    let previous = self.placed_comb_definitions[materialization.0]
                        .replace(PlacedValue { block, register });
                    assert!(
                        previous.is_none(),
                        "one comb materialization is emitted exactly once"
                    );
                }
                BlockScheduleNode::CombValue(value) => {
                    let mut values = ValueMaterializer::new_with_caches(
                        self.ir,
                        self.comb_value_graph
                            .as_ref()
                            .expect("reachable comb values have a sparse graph"),
                        self.arena,
                        &self.slt,
                        self.settled_comb_reads,
                        &self.control,
                        self.event_control
                            .as_ref()
                            .expect("a non-empty clock projection has an event CFG"),
                        &self.placed_comb_values,
                        &self.comb_materialization_by_definition,
                        &self.placed_comb_definitions,
                        &self.homed_comb_definitions,
                        &self.comb_home_offsets,
                        &mut self.comb_home_loads,
                        &mut self.builder,
                        process,
                        block,
                        cache,
                        std::mem::take(materialization_cache),
                    );
                    let register = values.materialize(value)?;
                    *materialization_cache = values.into_caches();
                    let previous = self
                        .placed_comb_values
                        .insert((process, value), PlacedValue { block, register });
                    assert!(
                        previous.is_none(),
                        "one placed comb value is emitted exactly once"
                    );
                }
                BlockScheduleNode::StageCompute(effect) => {
                    self.lower_stage_compute(process, block, effect, cache, materialization_cache)?;
                }
                BlockScheduleNode::StagePublish(effects) => {
                    self.lower_stage_publish_group(&effects)?;
                }
                BlockScheduleNode::Effects(effects) => {
                    self.lower_effect_group(
                        process,
                        block,
                        &effects,
                        cache,
                        materialization_cache,
                    )?;
                }
                BlockScheduleNode::Exit => break,
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
        let mut cache = self.block_anchors(block);
        let mut materialization_cache = BlockMaterializationCache::default();
        let schedule = self.block_schedule(block)?;
        self.emit_region_schedule(schedule, &mut cache, &mut materialization_cache, false)?;

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
                    self.settled_comb_reads,
                    &self.control,
                    self.event_control
                        .as_ref()
                        .expect("a non-empty clock projection has an event CFG"),
                    &self.placed_comb_values,
                    &self.comb_materialization_by_definition,
                    &self.placed_comb_definitions,
                    &self.homed_comb_definitions,
                    &self.comb_home_offsets,
                    &mut self.comb_home_loads,
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
                        self.settled_comb_reads,
                        &self.control,
                        self.event_control
                            .as_ref()
                            .expect("a non-empty clock projection has an event CFG"),
                        &self.placed_comb_values,
                        &self.comb_materialization_by_definition,
                        &self.placed_comb_definitions,
                        &self.homed_comb_definitions,
                        &self.comb_home_offsets,
                        &mut self.comb_home_loads,
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

    fn lower_interleaved_process_run(
        &mut self,
        processes: &[ProcessId],
    ) -> Result<Vec<BlockId>, EventProjectionError> {
        let blocks = processes
            .iter()
            .copied()
            .map(|process| (process, self.ir.processes()[process.0].entry))
            .collect::<Vec<_>>();
        let first = blocks[0].1;
        self.builder.switch_to_block(self.sir_blocks[first.0]);
        let mut cache = HashMap::default();
        let mut materialization_cache = BlockMaterializationCache::default();
        let schedule = self.region_schedule(&blocks)?;
        self.emit_region_schedule(schedule, &mut cache, &mut materialization_cache, true)?;
        let continuation = self.continuation_by_process[processes.last().unwrap().0];
        self.builder
            .seal_block(SIRTerminator::Jump(continuation, Vec::new()));
        Ok(blocks
            .iter()
            .skip(1)
            .map(|(_, block)| self.sir_blocks[block.0])
            .collect())
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
            self.settled_comb_reads,
            &self.control,
            self.event_control
                .as_ref()
                .expect("a non-empty clock projection has an event CFG"),
            &self.placed_comb_values,
            &self.comb_materialization_by_definition,
            &self.placed_comb_definitions,
            &self.homed_comb_definitions,
            &self.comb_home_offsets,
            &mut self.comb_home_loads,
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

    fn lower_stage_compute(
        &mut self,
        process: ProcessId,
        block: ControlBlockId,
        effect: EffectId,
        cache: &mut HashMap<ValueId, RegisterId>,
        materialization_cache: &mut BlockMaterializationCache,
    ) -> Result<(), EventProjectionError> {
        let EffectKind::StageNextFf {
            target,
            value,
            guard,
            ..
        } = &self.ir.effects()[effect.0].kind
        else {
            unreachable!("only FF stages have split compute nodes");
        };
        let target = target.clone();
        let value = *value;
        let guard = *guard;
        let mut values = ValueMaterializer::new_with_caches(
            self.ir,
            self.comb_value_graph
                .as_ref()
                .expect("reachable comb values have a sparse graph"),
            self.arena,
            &self.slt,
            self.settled_comb_reads,
            &self.control,
            self.event_control
                .as_ref()
                .expect("a non-empty clock projection has an event CFG"),
            &self.placed_comb_values,
            &self.comb_materialization_by_definition,
            &self.placed_comb_definitions,
            &self.homed_comb_definitions,
            &self.comb_home_offsets,
            &mut self.comb_home_loads,
            &mut self.builder,
            process,
            block,
            cache,
            std::mem::take(materialization_cache),
        );
        let guard = guard.map(|guard| values.materialize(guard)).transpose()?;
        let value = values.materialize(value)?;
        let offset = values.materialize_offset(&target.offset)?;
        *materialization_cache = values.into_caches();
        let previous = self.prepared_stages.insert(
            effect,
            PreparedStage {
                value,
                offset,
                guard,
            },
        );
        assert!(previous.is_none(), "one FF stage is computed exactly once");
        Ok(())
    }

    fn emit_prepared_stage(&mut self, effect: EffectId) {
        let EffectKind::StageNextFf { target, .. } = &self.ir.effects()[effect.0].kind else {
            unreachable!("only FF stages have split publication nodes");
        };
        let prepared = self.prepared_stages[&effect].clone();
        let store_location = (
            self.builder.current_block(),
            self.builder.current_instruction_index(),
        );
        self.builder.emit(SIRInstruction::Store(
            target.object,
            prepared.offset,
            target.width,
            prepared.value,
            Vec::new(),
            Vec::new(),
        ));
        self.stage_stores.insert(store_location);
        debug_assert!(self.state.is_direct_effect(effect));
        self.direct_stage_stores.insert(store_location);
    }

    fn lower_stage_publish_group(
        &mut self,
        effects: &[EffectId],
    ) -> Result<(), EventProjectionError> {
        let Some(&first) = effects.first() else {
            return Ok(());
        };
        let guard = self.prepared_stages[&first].guard;
        debug_assert!(
            effects
                .iter()
                .all(|effect| self.prepared_stages[effect].guard == guard)
        );
        let Some(condition) = guard else {
            for &effect in effects {
                self.emit_prepared_stage(effect);
            }
            return Ok(());
        };

        let effect_block = self.builder.new_block();
        let continuation = self.builder.new_block();
        self.builder.seal_block(SIRTerminator::Branch {
            cond: condition,
            true_block: (effect_block, Vec::new()),
            false_block: (continuation, Vec::new()),
        });
        self.builder.switch_to_block(effect_block);
        for &effect in effects {
            self.emit_prepared_stage(effect);
        }
        self.builder
            .seal_block(SIRTerminator::Jump(continuation, Vec::new()));
        self.builder.switch_to_block(continuation);
        Ok(())
    }

    fn lower_effect_group(
        &mut self,
        process: ProcessId,
        block: ControlBlockId,
        effects: &[EffectId],
        cache: &mut HashMap<ValueId, RegisterId>,
        materialization_cache: &mut BlockMaterializationCache,
    ) -> Result<(), EventProjectionError> {
        let Some(&first) = effects.first() else {
            return Ok(());
        };
        let guard = effect_guard(&self.ir.effects()[first.0].kind);
        debug_assert!(
            effects
                .iter()
                .all(|effect| effect_guard(&self.ir.effects()[effect.0].kind) == guard)
        );
        let Some(guard) = guard else {
            for &effect in effects {
                self.lower_effect(process, block, effect, cache, materialization_cache, true)?;
            }
            return Ok(());
        };

        let condition = {
            let mut values = ValueMaterializer::new(
                self.ir,
                self.comb_value_graph
                    .as_ref()
                    .expect("reachable comb values have a sparse graph"),
                self.arena,
                &self.slt,
                self.settled_comb_reads,
                &self.control,
                self.event_control
                    .as_ref()
                    .expect("a non-empty clock projection has an event CFG"),
                &self.placed_comb_values,
                &self.comb_materialization_by_definition,
                &self.placed_comb_definitions,
                &self.homed_comb_definitions,
                &self.comb_home_offsets,
                &mut self.comb_home_loads,
                &mut self.builder,
                process,
                block,
                cache,
            );
            values.materialize(guard)?
        };
        let effect_block = self.builder.new_block();
        let continuation = self.builder.new_block();
        self.builder.seal_block(SIRTerminator::Branch {
            cond: condition,
            true_block: (effect_block, Vec::new()),
            false_block: (continuation, Vec::new()),
        });
        self.builder.switch_to_block(effect_block);
        let mut effect_cache = cache.clone();
        let mut effect_materializations = BlockMaterializationCache::default();
        for &effect in effects {
            self.lower_effect(
                process,
                block,
                effect,
                &mut effect_cache,
                &mut effect_materializations,
                false,
            )?;
        }
        self.builder
            .seal_block(SIRTerminator::Jump(continuation, Vec::new()));
        self.builder.switch_to_block(continuation);
        Ok(())
    }

    fn lower_effect(
        &mut self,
        process: ProcessId,
        block: ControlBlockId,
        effect_id: EffectId,
        cache: &mut HashMap<ValueId, RegisterId>,
        materialization_cache: &mut BlockMaterializationCache,
        honor_guard: bool,
    ) -> Result<(), EventProjectionError> {
        let effect = &self.ir.effects()[effect_id.0];
        match &effect.kind {
            EffectKind::StageNextFf {
                target,
                value,
                guard,
                ..
            } => {
                if honor_guard && let Some(guard) = guard {
                    let mut condition_values = ValueMaterializer::new(
                        self.ir,
                        self.comb_value_graph
                            .as_ref()
                            .expect("reachable comb values have a sparse graph"),
                        self.arena,
                        &self.slt,
                        self.settled_comb_reads,
                        &self.control,
                        self.event_control
                            .as_ref()
                            .expect("a non-empty clock projection has an event CFG"),
                        &self.placed_comb_values,
                        &self.comb_materialization_by_definition,
                        &self.placed_comb_definitions,
                        &self.homed_comb_definitions,
                        &self.comb_home_offsets,
                        &mut self.comb_home_loads,
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
                    let mut store_materializations = BlockMaterializationCache::default();
                    self.emit_stage(
                        process,
                        block,
                        effect_id,
                        target,
                        *value,
                        &mut store_cache,
                        &mut store_materializations,
                    )?;
                    self.builder
                        .seal_block(SIRTerminator::Jump(continuation, Vec::new()));
                    self.builder.switch_to_block(continuation);
                } else {
                    self.emit_stage(
                        process,
                        block,
                        effect_id,
                        target,
                        *value,
                        cache,
                        materialization_cache,
                    )?;
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
                let observation_kind = matches!(effect.kind, EffectKind::RuntimeEvent { .. });
                let emit = |builder: &mut SIRBuilder<AbsoluteAddr>, arguments| {
                    if observation_kind {
                        builder.emit(SIRInstruction::RuntimeEvent {
                            site_id: *site_id,
                            args: arguments,
                        });
                    } else {
                        builder.emit(SIRInstruction::CombCaptureEvent {
                            site_id: *site_id,
                            args: arguments,
                            fatal_error_code: None,
                            consume_enabled: false,
                        });
                    }
                };
                if honor_guard && let Some(guard) = guard {
                    let condition = {
                        let mut values = ValueMaterializer::new(
                            self.ir,
                            self.comb_value_graph
                                .as_ref()
                                .expect("reachable comb values have a sparse graph"),
                            self.arena,
                            &self.slt,
                            self.settled_comb_reads,
                            &self.control,
                            self.event_control
                                .as_ref()
                                .expect("a non-empty clock projection has an event CFG"),
                            &self.placed_comb_values,
                            &self.comb_materialization_by_definition,
                            &self.placed_comb_definitions,
                            &self.homed_comb_definitions,
                            &self.comb_home_offsets,
                            &mut self.comb_home_loads,
                            &mut self.builder,
                            process,
                            block,
                            cache,
                        );
                        values.materialize(*guard)?
                    };
                    let event_block = self.builder.new_block();
                    let continuation = self.builder.new_block();
                    self.builder.seal_block(SIRTerminator::Branch {
                        cond: condition,
                        true_block: (event_block, Vec::new()),
                        false_block: (continuation, Vec::new()),
                    });
                    self.builder.switch_to_block(event_block);
                    let mut event_cache = cache.clone();
                    let arguments = {
                        let mut values = ValueMaterializer::new(
                            self.ir,
                            self.comb_value_graph
                                .as_ref()
                                .expect("reachable comb values have a sparse graph"),
                            self.arena,
                            &self.slt,
                            self.settled_comb_reads,
                            &self.control,
                            self.event_control
                                .as_ref()
                                .expect("a non-empty clock projection has an event CFG"),
                            &self.placed_comb_values,
                            &self.comb_materialization_by_definition,
                            &self.placed_comb_definitions,
                            &self.homed_comb_definitions,
                            &self.comb_home_offsets,
                            &mut self.comb_home_loads,
                            &mut self.builder,
                            process,
                            block,
                            &mut event_cache,
                        );
                        values.materialize_many(arguments)?
                    };
                    emit(&mut self.builder, arguments);
                    self.builder
                        .seal_block(SIRTerminator::Jump(continuation, Vec::new()));
                    self.builder.switch_to_block(continuation);
                } else {
                    let arguments = {
                        let mut values = ValueMaterializer::new(
                            self.ir,
                            self.comb_value_graph
                                .as_ref()
                                .expect("reachable comb values have a sparse graph"),
                            self.arena,
                            &self.slt,
                            self.settled_comb_reads,
                            &self.control,
                            self.event_control
                                .as_ref()
                                .expect("a non-empty clock projection has an event CFG"),
                            &self.placed_comb_values,
                            &self.comb_materialization_by_definition,
                            &self.placed_comb_definitions,
                            &self.homed_comb_definitions,
                            &self.comb_home_offsets,
                            &mut self.comb_home_loads,
                            &mut self.builder,
                            process,
                            block,
                            cache,
                        );
                        values.materialize_many(arguments)?
                    };
                    emit(&mut self.builder, arguments);
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
        effect: EffectId,
        target: &super::ObjectAccess,
        value: ValueId,
        cache: &mut HashMap<ValueId, RegisterId>,
        materialization_cache: &mut BlockMaterializationCache,
    ) -> Result<(), EventProjectionError> {
        let direct = self.state.is_direct_effect(effect);
        let mut values = ValueMaterializer::new_with_caches(
            self.ir,
            self.comb_value_graph
                .as_ref()
                .expect("reachable comb values have a sparse graph"),
            self.arena,
            &self.slt,
            self.settled_comb_reads,
            &self.control,
            self.event_control
                .as_ref()
                .expect("a non-empty clock projection has an event CFG"),
            &self.placed_comb_values,
            &self.comb_materialization_by_definition,
            &self.placed_comb_definitions,
            &self.homed_comb_definitions,
            &self.comb_home_offsets,
            &mut self.comb_home_loads,
            &mut self.builder,
            process,
            block,
            cache,
            std::mem::take(materialization_cache),
        );
        let value = values.materialize(value)?;
        let offset = values.materialize_offset(&target.offset)?;
        let store_location = (
            values.builder.current_block(),
            values.builder.current_instruction_index(),
        );
        values.builder.emit(SIRInstruction::Store(
            target.object,
            offset,
            target.width,
            value,
            Vec::new(),
            Vec::new(),
        ));
        *materialization_cache = values.into_caches();
        self.stage_stores.insert(store_location);
        if direct {
            self.direct_stage_stores.insert(store_location);
        }
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
        let mut definition_consumers =
            vec![DefinitionConsumers::default(); self.ir.comb_definitions().len()];
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
                                seeds.push((*guard, block));
                            }
                            // Combinational definitions are pure settled-event
                            // values. A guarded Stage controls publication, not
                            // whether its shared comb graph may be evaluated.
                            // Seed the value for event-level placement so many
                            // FF predicates do not each rebuild the same cone.
                            seeds.push((*value, block));
                            target
                                .offset
                                .visit_value_operands(|value| seeds.push((value, block)));
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
                definition_consumers[definition.0].add(process);
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

        if self.settled_comb_reads {
            let demanded_definitions = vec![false; self.ir.comb_definitions().len()];
            self.comb_value_graph = Some(
                CombValueGraph::build(
                    self.ir.comb_graph(),
                    self.arena,
                    &demanded_definitions,
                    false,
                )
                .map_err(|error| match error {
                    CombValueGraphError::DefinitionCycle(definition) => {
                        EventProjectionError::CombDefinitionCycle { definition }
                    }
                })?,
            );
            return Ok(());
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
                false,
            )
            .map_err(|error| match error {
                CombValueGraphError::DefinitionCycle(definition) => {
                    EventProjectionError::CombDefinitionCycle { definition }
                }
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
            let consumers = definition_consumers[definition.0];
            let dependency_consumers = if consumers.crosses_processes() && !self.four_state {
                let producer = placement
                    .map(|block| self.ir.blocks()[block].process)
                    .expect("a consumed combinational definition has a placement");
                self.homed_comb_definitions[definition.0] = true;
                DefinitionConsumers {
                    first: Some(producer),
                    second: None,
                    many_processes: false,
                }
            } else {
                consumers
            };
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
                // A home is the process-allocation cut. Dependencies below
                // that cut feed only the process which computes the home.
                definition_consumers[dependency.0].merge(dependency_consumers);
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
            let placement = event_control.hoist_out_of_cycles(placement);
            let block = placement;
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

        // Homes are compiler-private SSA-version slots, not aliases of the
        // packed RTL target. Give every retained home a disjoint,
        // machine-aligned logical range so narrow stores never need to
        // preserve neighboring RTL bits and distinct versions cannot
        // overwrite each other.
        let mut next_home_bit = 0usize;
        for (definition, homed) in self.homed_comb_definitions.iter().copied().enumerate() {
            if !homed {
                continue;
            }
            next_home_bit = next_home_bit
                .checked_add(63)
                .map(|bit| bit & !63)
                .ok_or(EventProjectionError::MaterializationHomeLayoutOverflow)?;
            self.comb_home_offsets[definition] = Some(next_home_bit);
            let width = self.ir.comb_definitions()[definition]
                .target
                .width()
                .expect("verified combinational definition range");
            let storage_width = width
                .checked_add(63)
                .map(|bits| bits & !63)
                .ok_or(EventProjectionError::MaterializationHomeLayoutOverflow)?;
            next_home_bit = next_home_bit
                .checked_add(storage_width)
                .ok_or(EventProjectionError::MaterializationHomeLayoutOverflow)?;
        }
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
    settled_comb_reads: bool,
    control: &'a EventControlFlow,
    event_control: &'a EventExecutionControlFlow,
    placed_cache: &'a HashMap<(ProcessId, ValueId), PlacedValue>,
    comb_materialization_by_definition: &'a [Option<CombMaterializationId>],
    placed_comb_cache: &'a [Option<PlacedValue>],
    homed_comb_definitions: &'a [bool],
    comb_home_offsets: &'a [Option<usize>],
    comb_home_loads: &'builder mut HashSet<RegisterId>,
    builder: &'builder mut SIRBuilder<AbsoluteAddr>,
    process: ProcessId,
    block: ControlBlockId,
    cache: &'cache mut HashMap<ValueId, RegisterId>,
    comb_cache: HashMap<CombDefinitionId, RegisterId>,
    comb_visiting: HashSet<CombDefinitionId>,
    scheduled_comb_definitions: HashSet<CombDefinitionId>,
    sparse_control_depth: usize,
    sparse_controlled_recipes: HashSet<CombRecipeId>,
    semantic_slt_cache: SemanticSltCache,
}

impl<'a, 'cache, 'builder> ValueMaterializer<'a, 'cache, 'builder> {
    fn new(
        ir: &'a EventIr,
        comb_value_graph: &'a CombValueGraph,
        arena: &'a SLTNodeArena<AbsoluteAddr>,
        slt: &'a SLTToSIRLowerer,
        settled_comb_reads: bool,
        control: &'a EventControlFlow,
        event_control: &'a EventExecutionControlFlow,
        placed_cache: &'a HashMap<(ProcessId, ValueId), PlacedValue>,
        comb_materialization_by_definition: &'a [Option<CombMaterializationId>],
        placed_comb_cache: &'a [Option<PlacedValue>],
        homed_comb_definitions: &'a [bool],
        comb_home_offsets: &'a [Option<usize>],
        comb_home_loads: &'builder mut HashSet<RegisterId>,
        builder: &'builder mut SIRBuilder<AbsoluteAddr>,
        process: ProcessId,
        block: ControlBlockId,
        cache: &'cache mut HashMap<ValueId, RegisterId>,
    ) -> Self {
        Self::new_with_caches(
            ir,
            comb_value_graph,
            arena,
            slt,
            settled_comb_reads,
            control,
            event_control,
            placed_cache,
            comb_materialization_by_definition,
            placed_comb_cache,
            homed_comb_definitions,
            comb_home_offsets,
            comb_home_loads,
            builder,
            process,
            block,
            cache,
            BlockMaterializationCache::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_with_caches(
        ir: &'a EventIr,
        comb_value_graph: &'a CombValueGraph,
        arena: &'a SLTNodeArena<AbsoluteAddr>,
        slt: &'a SLTToSIRLowerer,
        settled_comb_reads: bool,
        control: &'a EventControlFlow,
        event_control: &'a EventExecutionControlFlow,
        placed_cache: &'a HashMap<(ProcessId, ValueId), PlacedValue>,
        comb_materialization_by_definition: &'a [Option<CombMaterializationId>],
        placed_comb_cache: &'a [Option<PlacedValue>],
        homed_comb_definitions: &'a [bool],
        comb_home_offsets: &'a [Option<usize>],
        comb_home_loads: &'builder mut HashSet<RegisterId>,
        builder: &'builder mut SIRBuilder<AbsoluteAddr>,
        process: ProcessId,
        block: ControlBlockId,
        cache: &'cache mut HashMap<ValueId, RegisterId>,
        caches: BlockMaterializationCache,
    ) -> Self {
        Self {
            ir,
            comb_value_graph,
            arena,
            slt,
            settled_comb_reads,
            control,
            event_control,
            placed_cache,
            comb_materialization_by_definition,
            placed_comb_cache,
            homed_comb_definitions,
            comb_home_offsets,
            comb_home_loads,
            builder,
            process,
            block,
            cache,
            comb_cache: caches.comb,
            comb_visiting: HashSet::default(),
            scheduled_comb_definitions: HashSet::default(),
            sparse_control_depth: 0,
            sparse_controlled_recipes: HashSet::default(),
            semantic_slt_cache: caches.semantic_slt,
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

    fn into_caches(self) -> BlockMaterializationCache {
        BlockMaterializationCache {
            comb: self.comb_cache,
            semantic_slt: self.semantic_slt_cache,
        }
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
        if self.homed_comb_definitions[definition.0]
            && self.ir.blocks()[placed.block.0].process != self.process
        {
            return None;
        }
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
                if self.settled_comb_reads {
                    let recipe_id = self.ir.comb_definitions()[definition.0].recipe;
                    let recipe = &self.ir.comb_graph().recipes()[recipe_id.0];
                    if settled_slice_rematerialization_cost(
                        NodeId(recipe.root.0),
                        *access,
                        self.arena,
                        SETTLED_FRONTIER_MATERIALIZATION_BUDGET,
                    )
                    .is_some()
                        && recipe.pre_evaluate.is_empty()
                        && recipe.local_inputs.is_empty()
                        && recipe
                            .snapshot_inputs
                            .iter()
                            .all(|input| input.kind == super::CombSnapshotKind::EventEntry)
                        && !slt_tree_has_effects(NodeId(recipe.root.0), self.arena)
                    {
                        // The shared comb schedule has reached a fixed point
                        // before this projection starts.  Treat its stable
                        // state as a materialization frontier and rebuild only
                        // the exact FF-demanded range.  A fresh cache keeps
                        // this materialization local to the use cluster.
                        let mut cache = HashMap::default();
                        let value = self.slt.lower_region_slice(
                            self.builder,
                            NodeId(recipe.root.0),
                            *access,
                            self.arena,
                            &mut cache,
                        );
                        return Ok(self.coerce(value, item.ty));
                    }
                    let target = self.ir.comb_definitions()[definition.0].target;
                    let destination = self.alloc_type(item.ty);
                    self.builder.emit(SIRInstruction::Load(
                        destination,
                        target.object,
                        SIROffset::Static(target.access.lsb + access.lsb),
                        item.ty.width,
                    ));
                    return Ok(self.coerce(destination, item.ty));
                }
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
        if self.homed_comb_definitions[definition.0]
            && !self.scheduled_comb_definitions.contains(&definition)
        {
            let target = self.ir.comb_definitions()[definition.0].target;
            let home_offset = self.comb_home_offsets[definition.0]
                .expect("a homed definition has a private slot");
            let width = target
                .width()
                .expect("verified combinational definition range");
            let register = self.builder.alloc_logic(width);
            self.builder.emit(SIRInstruction::Load(
                register,
                target.object,
                SIROffset::Static(home_offset),
                width,
            ));
            self.comb_home_loads.insert(register);
            self.comb_cache.insert(definition, register);
            return Ok(register);
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
        if std::env::var_os("CELOX_EIR_TRACE_EMISSION").is_some() {
            eprintln!(
                "[eir-emission] definition={} process={} block={}",
                definition.0, self.process.0, self.block.0
            );
        }
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
            let semantic_ids = self
                .semantic_slt_cache
                .prepare(root, self.arena, &inputs, &mut cache);
            let result = self.slt.lower_with_scoped_inputs(
                self.builder,
                root,
                self.arena,
                &mut cache,
                &inputs,
            );
            self.semantic_slt_cache.record(&semantic_ids, &cache);
            Ok(result)
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

fn settled_slice_rematerialization_cost(
    node: NodeId,
    access: crate::ir::BitAccess,
    arena: &SLTNodeArena<AbsoluteAddr>,
    budget: usize,
) -> Option<usize> {
    use crate::logic_tree::SLTNode;

    if access.msb < access.lsb
        || access.msb >= crate::logic_tree::get_width(node, arena)
        || budget == 0
    {
        return None;
    }
    let add = |parts: &[usize]| {
        parts
            .iter()
            .try_fold(0usize, |total, part| total.checked_add(*part))
            .filter(|total| *total <= budget)
    };
    match arena.get(node) {
        SLTNode::Input { index, .. } if index.is_empty() => Some(1),
        SLTNode::Input { .. } => None,
        SLTNode::Constant(..) => Some(1),
        SLTNode::Slice {
            expr,
            access: inner,
        } if access.msb <= inner.msb - inner.lsb => settled_slice_rematerialization_cost(
            *expr,
            crate::ir::BitAccess::new(inner.lsb + access.lsb, inner.lsb + access.msb),
            arena,
            budget,
        ),
        SLTNode::Unary(
            crate::ir::UnaryOp::Ident | crate::ir::UnaryOp::ToTwoState | crate::ir::UnaryOp::BitNot,
            input,
        ) if access.msb < crate::logic_tree::get_width(*input, arena) => {
            let input = settled_slice_rematerialization_cost(*input, access, arena, budget - 1)?;
            add(&[1, input])
        }
        SLTNode::Binary(
            lhs,
            crate::ir::BinaryOp::And | crate::ir::BinaryOp::Or | crate::ir::BinaryOp::Xor,
            rhs,
        ) if access.msb < crate::logic_tree::get_width(*lhs, arena)
            && access.msb < crate::logic_tree::get_width(*rhs, arena) =>
        {
            let lhs = settled_slice_rematerialization_cost(*lhs, access, arena, budget - 1)?;
            let rhs = settled_slice_rematerialization_cost(*rhs, access, arena, budget - 1)?;
            add(&[1, lhs, rhs])
        }
        SLTNode::Binary(
            lhs,
            crate::ir::BinaryOp::Add | crate::ir::BinaryOp::Sub | crate::ir::BinaryOp::Mul,
            rhs,
        ) if access.lsb == 0
            && access.msb < crate::logic_tree::get_width(*lhs, arena)
            && access.msb < crate::logic_tree::get_width(*rhs, arena) =>
        {
            let lhs = settled_slice_rematerialization_cost(*lhs, access, arena, budget - 1)?;
            let rhs = settled_slice_rematerialization_cost(*rhs, access, arena, budget - 1)?;
            add(&[1, lhs, rhs])
        }
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } if access.msb < crate::logic_tree::get_width(*then_expr, arena)
            && access.msb < crate::logic_tree::get_width(*else_expr, arena) =>
        {
            let cond = settled_full_rematerialization_cost(*cond, arena, budget - 1)?;
            let then_cost =
                settled_slice_rematerialization_cost(*then_expr, access, arena, budget - 1)?;
            let else_cost =
                settled_slice_rematerialization_cost(*else_expr, access, arena, budget - 1)?;
            add(&[1, cond, then_cost, else_cost])
        }
        SLTNode::Concat(parts) => {
            let mut part_lsb = 0usize;
            let mut costs = Vec::new();
            for (part, width) in parts.iter().rev() {
                let part_msb = part_lsb + *width - 1;
                let overlap_lsb = access.lsb.max(part_lsb);
                let overlap_msb = access.msb.min(part_msb);
                if overlap_lsb <= overlap_msb {
                    costs.push(settled_slice_rematerialization_cost(
                        *part,
                        crate::ir::BitAccess::new(overlap_lsb - part_lsb, overlap_msb - part_lsb),
                        arena,
                        budget,
                    )?);
                }
                part_lsb += *width;
                if part_lsb > access.msb {
                    break;
                }
            }
            if costs.len() > 1 {
                costs.push(1);
            }
            add(&costs)
        }
        _ if access.lsb == 0 && access.msb + 1 == crate::logic_tree::get_width(node, arena) => {
            settled_full_rematerialization_cost(node, arena, budget)
        }
        _ => None,
    }
}

fn settled_full_rematerialization_cost(
    node: NodeId,
    arena: &SLTNodeArena<AbsoluteAddr>,
    budget: usize,
) -> Option<usize> {
    use crate::logic_tree::SLTNode;

    if budget == 0 || crate::logic_tree::get_width(node, arena) > 64 {
        return None;
    }
    let children = match arena.get(node) {
        SLTNode::Input { index, .. } => index.iter().map(|item| item.node).collect::<Vec<_>>(),
        SLTNode::Constant(..) => Vec::new(),
        SLTNode::Binary(lhs, _, rhs) => vec![*lhs, *rhs],
        SLTNode::Unary(_, input) => vec![*input],
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => vec![*cond, *then_expr, *else_expr],
        SLTNode::Concat(parts) => parts.iter().map(|(part, _)| *part).collect(),
        SLTNode::Slice { expr, access } => {
            return settled_slice_rematerialization_cost(*expr, *access, arena, budget);
        }
        SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => return None,
    };
    let mut total = 1usize;
    for child in children {
        total = total.checked_add(settled_full_rematerialization_cost(
            child,
            arena,
            budget.saturating_sub(total),
        )?)?;
        if total > budget {
            return None;
        }
    }
    Some(total)
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

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
struct StaticPublicationComponent {
    object: AbsoluteAddr,
    offset: usize,
    width: usize,
}

impl StaticPublicationComponent {
    fn overlaps(self, object: AbsoluteAddr, offset: usize, width: usize) -> bool {
        self.object == object
            && offset < self.offset.saturating_add(self.width)
            && self.offset < offset.saturating_add(width)
    }
}

/// Replace a static WORKING transaction by a direct STABLE publication only
/// when the old snapshot can be captured at a local CFG dominator.
fn capture_local_static_snapshot_publications(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    approved: &BTreeSet<StaticPublicationComponent>,
) -> Result<(), EventProjectionError> {
    if approved.is_empty() {
        return Ok(());
    }
    let mut seeds = BTreeSet::new();
    let mut commits = BTreeSet::new();
    for block in eu.blocks.values() {
        for instruction in &block.instructions {
            let SIRInstruction::Commit(source, target, SIROffset::Static(offset), width, _) =
                instruction
            else {
                continue;
            };
            if source.absolute_addr() != target.absolute_addr() {
                continue;
            }
            let component = StaticPublicationComponent {
                object: source.absolute_addr(),
                offset: *offset,
                width: *width,
            };
            if source.region == STABLE_REGION && target.region == WORKING_REGION {
                seeds.insert(component);
            } else if source.region == WORKING_REGION && target.region == STABLE_REGION {
                commits.insert(component);
            }
        }
    }
    let mut candidates = seeds
        .intersection(&commits)
        .copied()
        .filter(|component| approved.contains(component))
        .collect::<BTreeSet<_>>();
    if candidates.is_empty() {
        return Ok(());
    }

    let mut dynamic_objects = HashSet::default();
    for block in eu.blocks.values() {
        for instruction in &block.instructions {
            match instruction {
                SIRInstruction::Load(_, address, SIROffset::Dynamic(_), _)
                    if address.region == STABLE_REGION =>
                {
                    dynamic_objects.insert(address.absolute_addr());
                }
                SIRInstruction::Store(address, SIROffset::Dynamic(_), ..)
                    if address.region == WORKING_REGION =>
                {
                    dynamic_objects.insert(address.absolute_addr());
                }
                _ => {}
            }
        }
    }
    candidates.retain(|component| !dynamic_objects.contains(&component.object));
    if candidates.is_empty() {
        return Ok(());
    }

    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable();
    let local_by_block = block_ids
        .iter()
        .copied()
        .enumerate()
        .map(|(local, block)| (block, local))
        .collect::<HashMap<_, _>>();
    let successors = block_ids
        .iter()
        .map(|block| {
            let block = &eu.blocks[block];
            match &block.terminator {
                SIRTerminator::Jump(target, _) => vec![local_by_block[target]],
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => vec![
                    local_by_block[&true_block.0],
                    local_by_block[&false_block.0],
                ],
                SIRTerminator::Switch { cases, default, .. } => cases
                    .iter()
                    .map(|case| local_by_block[&case.target])
                    .chain(std::iter::once(local_by_block[default]))
                    .collect(),
                SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
            }
        })
        .collect();
    let cfg =
        ForwardControlFlowGraph::analyze_structure(successors, local_by_block[&eu.entry_block_id])
            .map_err(|source| EventProjectionError::EventControlFlowAnalysis { source })?;

    let mut component_blocks = BTreeMap::<StaticPublicationComponent, Vec<usize>>::new();
    let mut component_chunks = BTreeMap::<StaticPublicationComponent, usize>::new();
    let mut loads = Vec::<(BlockId, usize, Vec<StaticPublicationComponent>)>::new();
    for block in eu.blocks.values() {
        let local = local_by_block[&block.id];
        for (index, instruction) in block.instructions.iter().enumerate() {
            match instruction {
                SIRInstruction::Load(_, address, SIROffset::Static(offset), width)
                    if address.region == STABLE_REGION =>
                {
                    let overlapping = candidates
                        .iter()
                        .copied()
                        .filter(|component| {
                            component.overlaps(address.absolute_addr(), *offset, *width)
                        })
                        .collect::<Vec<_>>();
                    if !overlapping.is_empty() {
                        for &component in &overlapping {
                            component_blocks.entry(component).or_default().push(local);
                            *component_chunks.entry(component).or_default() +=
                                width.saturating_add(63) / 64;
                        }
                        loads.push((block.id, index, overlapping));
                    }
                }
                SIRInstruction::Store(address, SIROffset::Static(offset), width, ..)
                    if address.region == WORKING_REGION =>
                {
                    for &component in &candidates {
                        if component.overlaps(address.absolute_addr(), *offset, *width) {
                            component_blocks.entry(component).or_default().push(local);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let hoist_out_of_cycle = |mut block: usize| {
        loop {
            let component = cfg.scc_for_block[block];
            if !cfg.sccs[component].cyclic {
                return Some(block);
            }
            let mut parent = cfg.dominators.idom[block]?;
            while cfg.scc_for_block[parent] == component {
                parent = cfg.dominators.idom[parent]?;
            }
            block = parent;
        }
    };
    let mut placement = BTreeMap::<StaticPublicationComponent, usize>::new();
    for &component in &candidates {
        let Some(blocks) = component_blocks.get(&component) else {
            continue;
        };
        let Some(common) = blocks.iter().copied().reduce(|left, right| {
            cfg.dominators
                .lca(left, right)
                .expect("reachable blocks have a common dominator")
        }) else {
            continue;
        };
        let Some(common) = hoist_out_of_cycle(common) else {
            continue;
        };
        let maximum_distance = blocks
            .iter()
            .copied()
            .map(|mut block| {
                let mut distance = 0usize;
                while block != common {
                    block = cfg.dominators.idom[block]
                        .expect("a common dominator reaches every component block");
                    distance = distance.saturating_add(1);
                }
                distance
            })
            .max()
            .unwrap_or(0);
        // Entry placement recreates one whole-function live range and was
        // measured to be substantially slower than the WORKING transaction.
        // Keep the initial capture subset within four machine-register chunks
        // and six dominator edges; larger windows need pressure-aware spill
        // costing rather than a fixed publication preference.
        if common != cfg.root
            && component_chunks.get(&component).copied().unwrap_or(0) <= 4
            && maximum_distance <= 6
        {
            placement.insert(component, common);
        }
    }
    candidates.retain(|component| placement.contains_key(component));
    if candidates.is_empty() {
        return Ok(());
    }

    let mut load_placement = HashMap::<(BlockId, usize), usize>::default();
    for (block, index, overlapping) in loads {
        let mut placements = overlapping
            .into_iter()
            .filter_map(|component| placement.get(&component).copied());
        let Some(first) = placements.next() else {
            continue;
        };
        let common = placements.fold(first, |left, right| {
            cfg.dominators
                .lca(left, right)
                .expect("reachable placements have a common dominator")
        });
        load_placement.insert((block, index), common);
    }

    let mut hoisted =
        BTreeMap::<usize, Vec<(BlockId, usize, SIRInstruction<RegionedAbsoluteAddr>)>>::new();
    for block in eu.blocks.values_mut() {
        let mut rewritten = Vec::with_capacity(block.instructions.len());
        for (index, instruction) in block.instructions.drain(..).enumerate() {
            if let Some(&target) = load_placement.get(&(block.id, index)) {
                hoisted
                    .entry(target)
                    .or_default()
                    .push((block.id, index, instruction));
                continue;
            }
            match instruction {
                SIRInstruction::Commit(
                    source,
                    target,
                    SIROffset::Static(offset),
                    width,
                    triggers,
                ) => {
                    let component = StaticPublicationComponent {
                        object: source.absolute_addr(),
                        offset,
                        width,
                    };
                    let boundary = source.absolute_addr() == target.absolute_addr()
                        && ((source.region == STABLE_REGION && target.region == WORKING_REGION)
                            || (source.region == WORKING_REGION && target.region == STABLE_REGION));
                    if !boundary || !candidates.contains(&component) {
                        rewritten.push(SIRInstruction::Commit(
                            source,
                            target,
                            SIROffset::Static(offset),
                            width,
                            triggers,
                        ));
                    }
                }
                SIRInstruction::Store(
                    address,
                    SIROffset::Static(offset),
                    width,
                    source,
                    triggers,
                    captures,
                ) if address.region == WORKING_REGION
                    && candidates.iter().any(|component| {
                        component.overlaps(address.absolute_addr(), offset, width)
                    }) =>
                {
                    rewritten.push(SIRInstruction::Store(
                        RegionedAbsoluteAddr::from_absolute_addr(
                            STABLE_REGION,
                            address.absolute_addr(),
                        ),
                        SIROffset::Static(offset),
                        width,
                        source,
                        triggers,
                        captures,
                    ));
                }
                instruction => rewritten.push(instruction),
            }
        }
        block.instructions = rewritten;
    }
    for (local, mut instructions) in hoisted {
        instructions.sort_unstable_by_key(|(block, index, _)| (*block, *index));
        eu.blocks
            .get_mut(&block_ids[local])
            .expect("capture placement block exists")
            .instructions
            .splice(
                0..0,
                instructions
                    .into_iter()
                    .map(|(_, _, instruction)| instruction),
            );
    }
    Ok(())
}

fn map_clock_body(
    blocks: HashMap<BlockId, BasicBlock<AbsoluteAddr>>,
    register_map: HashMap<RegisterId, RegisterType>,
    state: &StatePublicationPlan,
    comb_home_loads: &HashSet<RegisterId>,
    comb_home_stores: &HashSet<(BlockId, usize)>,
    stage_stores: &HashSet<(BlockId, usize)>,
    direct_stage_stores: &HashSet<(BlockId, usize)>,
    fused: bool,
) -> Result<ExecutionUnit<RegionedAbsoluteAddr>, EventProjectionError> {
    let mut mapped = HashMap::default();
    for (block_id, block) in blocks {
        let mut instructions = Vec::with_capacity(block.instructions.len());
        for (instruction_index, instruction) in block.instructions.into_iter().enumerate() {
            let instruction = match instruction {
                SIRInstruction::Load(destination, address, offset, width) => {
                    let region = if comb_home_loads.contains(&destination) {
                        MATERIALIZATION_HOME_REGION
                    } else {
                        STABLE_REGION
                    };
                    SIRInstruction::Load(destination, regioned(region, address), offset, width)
                }
                SIRInstruction::Store(address, offset, width, source, triggers, captures) => {
                    let location = (block_id, instruction_index);
                    let region = if comb_home_stores.contains(&location) {
                        MATERIALIZATION_HOME_REGION
                    } else if fused && direct_stage_stores.contains(&location) {
                        STABLE_REGION
                    } else if stage_stores.contains(&location) {
                        state.stage_region(address, fused)
                    } else {
                        MATERIALIZATION_HOME_REGION
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
        ir::{BitAccess, InstanceId, LogicPathId, UnaryOp, VarAtomBase},
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

    #[test]
    fn clock_packet_ranges_keep_only_feedback_spans_together() {
        assert_eq!(
            packet_ranges_from_backward_edges(7, [(1, 3), (2, 5)]),
            vec![(0, 1), (1, 6), (6, 7)]
        );
    }

    #[test]
    fn clock_packet_ranges_leave_acyclic_processes_independent() {
        assert_eq!(
            packet_ranges_from_backward_edges(4, []),
            vec![(0, 1), (1, 2), (2, 3), (3, 4)]
        );
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
                stage_kind: crate::event_ir::FfStageKind::Fragment,
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

    fn add_process_stage(
        event: &mut EventIr,
        source_order: usize,
        target: ObjectRange,
        read: Option<ObjectRange>,
    ) -> (ProcessId, EffectId) {
        let process = event.add_process(source_order);
        let block = event.processes()[process.0].entry;
        let (scope, region) = if read.is_some() {
            (ValueScope::Event, event.root_region())
        } else {
            (ValueScope::Process(process), event.blocks()[block.0].region)
        };
        let value = event.add_value(Value {
            ty: ValueType::bit(
                target.width().expect("test target has a valid width"),
                false,
            ),
            scope,
            region,
            kind: read.map_or_else(
                || ValueKind::Constant {
                    value: BigUint::default(),
                    unknown: BigUint::default(),
                },
                ValueKind::ReadClockSnapshot,
            ),
        });
        let stage = add_stage(event, process, block, target.into(), value);
        event.set_terminator(block, ControlTerminator::Return);
        (process, stage)
    }

    #[test]
    fn publication_order_uses_transfer_cost_instead_of_reader_edge_count() {
        let mut event = EventIr::new(
            EventDomain::Clock {
                clock: object(100),
                resets: Vec::new(),
            },
            Arc::new(CombGraph::default()),
        );
        let mut processes = Vec::new();
        for source_order in 0..4 {
            let process = event.add_process(source_order);
            let block = event.processes()[process.0].entry;
            event.set_terminator(block, ControlTerminator::Return);
            processes.push(process);
        }
        for (writer, target) in [
            (processes[0], range(1, 0, 511)),
            (processes[1], range(2, 0, 511)),
        ] {
            let block = event.processes()[writer.0].entry;
            let value = event.add_value(Value {
                ty: ValueType::bit(512, false),
                scope: ValueScope::Process(writer),
                region: event.blocks()[block.0].region,
                kind: ValueKind::Constant {
                    value: BigUint::default(),
                    unknown: BigUint::default(),
                },
            });
            event.add_effect(Effect {
                region: event.blocks()[block.0].region,
                predecessors: Vec::new(),
                kind: EffectKind::StageNextFf {
                    process: writer,
                    target: target.into(),
                    value,
                    guard: None,
                    priority: 0,
                    stage_kind: crate::event_ir::FfStageKind::Fragment,
                },
            });
        }

        let mut accesses = (0..4)
            .map(|_| ProcessStateAccesses::default())
            .collect::<Vec<_>>();
        accesses[processes[0].0].static_reads.push(range(2, 0, 511));
        accesses[processes[1].0].static_reads.push(range(1, 0, 511));
        accesses[processes[2].0].static_reads.push(range(1, 0, 511));
        accesses[processes[2].0].static_reads.push(range(2, 0, 511));
        accesses[processes[3].0].static_reads.push(range(2, 0, 511));
        let repaired = repair_publication_order(&event, processes.clone(), &accesses, &[true; 4]);
        let position = repaired
            .iter()
            .copied()
            .enumerate()
            .map(|(position, process)| (process, position))
            .collect::<HashMap<_, _>>();

        assert!(position[&processes[2]] < position[&processes[0]]);
        assert!(position[&processes[1]] < position[&processes[3]]);
    }

    #[test]
    fn publication_order_treats_adjacent_writers_as_one_commit_component() {
        let mut event = EventIr::new(
            EventDomain::Clock {
                clock: object(100),
                resets: Vec::new(),
            },
            Arc::new(CombGraph::default()),
        );
        let mut processes = Vec::new();
        for (source_order, target) in [range(1, 0, 63), range(1, 64, 64)].into_iter().enumerate() {
            let process = event.add_process(source_order);
            let block = event.processes()[process.0].entry;
            let value = event.add_value(Value {
                ty: ValueType::bit(target.width().unwrap(), false),
                scope: ValueScope::Process(process),
                region: event.blocks()[block.0].region,
                kind: ValueKind::Constant {
                    value: BigUint::default(),
                    unknown: BigUint::default(),
                },
            });
            add_stage(&mut event, process, block, target.into(), value);
            event.set_terminator(block, ControlTerminator::Return);
            processes.push(process);
        }

        // Each writer consumes the other's old value.  Either process order
        // leaves one direct-publication constraint unsatisfied, so the one
        // byte-adjacent WORKING component remains in both cases.  Reordering
        // merely because the unresolved writer is narrower would be a false
        // profitability signal.
        let mut accesses = (0..2)
            .map(|_| ProcessStateAccesses::default())
            .collect::<Vec<_>>();
        accesses[processes[0].0].static_reads.push(range(1, 64, 64));
        accesses[processes[1].0].static_reads.push(range(1, 0, 63));

        assert_eq!(
            repair_publication_order(&event, processes.clone(), &accesses, &[true; 2]),
            processes
        );
    }

    #[test]
    fn clock_process_order_runs_an_acyclic_pipeline_downstream_first() {
        let mut event = EventIr::new(
            EventDomain::Clock {
                clock: object(100),
                resets: Vec::new(),
            },
            Arc::new(CombGraph::default()),
        );
        let (upstream, upstream_stage) = add_process_stage(&mut event, 0, range(1, 0, 7), None);
        let (downstream, downstream_stage) =
            add_process_stage(&mut event, 1, range(2, 0, 7), Some(range(1, 0, 7)));
        event.add_effect(Effect {
            region: event.root_region(),
            predecessors: vec![upstream_stage, downstream_stage],
            kind: EffectKind::CommitFfState {
                stages: vec![upstream_stage, downstream_stage],
            },
        });
        event.verify().unwrap();

        let ordered = ordered_clock_processes(&event, vec![upstream, downstream]);
        assert_eq!(ordered, vec![downstream, upstream]);
        assert_eq!(
            ordered_clock_process_packets(&event, &ordered),
            vec![vec![downstream], vec![upstream]]
        );

        let packets =
            lower_settled_clock_packets(&event, &SLTNodeArena::new(), false, object(100)).unwrap();
        assert_eq!(packets.len(), 2);
        for packet in &packets {
            packet.verify_result().unwrap();
        }
    }

    #[test]
    fn feedback_process_order_groups_users_of_one_old_state() {
        let mut event = EventIr::new(
            EventDomain::Clock {
                clock: object(100),
                resets: Vec::new(),
            },
            Arc::new(CombGraph::default()),
        );
        let (first, first_stage) =
            add_process_stage(&mut event, 0, range(1, 0, 7), Some(range(2, 0, 7)));
        let (second, second_stage) =
            add_process_stage(&mut event, 1, range(2, 0, 7), Some(range(1, 0, 7)));
        let (other_user, other_stage) =
            add_process_stage(&mut event, 2, range(3, 0, 7), Some(range(1, 0, 7)));
        let stages = vec![first_stage, second_stage, other_stage];
        event.add_effect(Effect {
            region: event.root_region(),
            predecessors: stages.clone(),
            kind: EffectKind::CommitFfState { stages },
        });
        event.verify().unwrap();

        let ordered = ordered_clock_processes(&event, vec![first, second, other_user]);
        assert_eq!(ordered[0], other_user);
        let packets = ordered_clock_process_packets(&event, &ordered);
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0], vec![other_user]);
        assert_eq!(packets[1].len(), 2);
        assert!(packets[1].contains(&first));
        assert!(packets[1].contains(&second));

        let arena = SLTNodeArena::new();
        let lowered_packets =
            lower_settled_clock_packets(&event, &arena, false, object(100)).unwrap();
        assert_eq!(lowered_packets.len(), 2);
        for packet in &lowered_packets {
            packet.verify_result().unwrap();
        }
        let fused = lower_event_projection(
            &event,
            EventProjection::FusedClock,
            &arena,
            false,
            object(100),
        )
        .unwrap();
        fused.verify_result().unwrap();
        let stable_stores = instructions(&fused)
            .filter(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Store(address, ..) if address.region == STABLE_REGION
                )
            })
            .count();
        let working_stores = instructions(&fused)
            .filter(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Store(address, ..) if address.region == WORKING_REGION
                )
            })
            .count();
        assert_eq!((stable_stores, working_stores), (3, 0));
    }

    #[test]
    fn local_feedback_computes_old_state_reads_before_direct_publication() {
        let mut event = EventIr::new(
            EventDomain::Clock {
                clock: object(100),
                resets: Vec::new(),
            },
            Arc::new(CombGraph::default()),
        );
        let process = event.add_process(0);
        let block = event.processes()[process.0].entry;
        let mut stages = Vec::new();
        for (target, read) in [
            (range(1, 0, 7), range(2, 0, 7)),
            (range(2, 0, 7), range(1, 0, 7)),
        ] {
            let value = event.add_value(Value {
                ty: ValueType::bit(8, false),
                scope: ValueScope::Event,
                region: event.root_region(),
                kind: ValueKind::ReadClockSnapshot(read),
            });
            stages.push(add_stage(&mut event, process, block, target.into(), value));
        }
        finish_clock_event(&mut event, block, stages);

        let arena = SLTNodeArena::new();
        let fused = lower_event_projection(
            &event,
            EventProjection::FusedClock,
            &arena,
            false,
            object(100),
        )
        .unwrap();
        fused.verify_result().unwrap();
        let stable_stores = instructions(&fused)
            .filter(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Store(address, ..) if address.region == STABLE_REGION
                )
            })
            .count();
        let working_stores = instructions(&fused)
            .filter(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Store(address, ..) if address.region == WORKING_REGION
                )
            })
            .count();
        assert_eq!((stable_stores, working_stores), (2, 0));
        let memory_kinds = instructions(&fused)
            .filter_map(|instruction| match instruction {
                SIRInstruction::Load(..) => Some("load"),
                SIRInstruction::Store(..) => Some("store"),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(memory_kinds, ["load", "load", "store", "store"]);
    }

    #[test]
    fn loop_cfg_does_not_block_publication_in_an_acyclic_suffix() {
        fn lower() -> ExecutionUnit<RegionedAbsoluteAddr> {
            let mut event = EventIr::new(
                EventDomain::Clock {
                    clock: object(100),
                    resets: Vec::new(),
                },
                Arc::new(CombGraph::default()),
            );
            let process = event.add_process(0);
            let entry = event.processes()[process.0].entry;
            let loop_block = event.add_control_block(process);
            let exit = event.add_control_block(process);
            let target = range(1, 0, 0);
            let condition = event.add_value(Value {
                ty: ValueType::bit(1, false),
                scope: ValueScope::Event,
                region: event.root_region(),
                kind: ValueKind::ReadClockSnapshot(target),
            });
            let value = event.add_value(Value {
                ty: ValueType::bit(1, false),
                scope: ValueScope::Event,
                region: event.root_region(),
                kind: ValueKind::Constant {
                    value: BigUint::from(1u8),
                    unknown: BigUint::default(),
                },
            });
            event.set_terminator(
                entry,
                ControlTerminator::Jump {
                    target: loop_block,
                    arguments: Vec::new(),
                },
            );
            event.set_terminator(
                loop_block,
                ControlTerminator::Branch {
                    condition,
                    true_target: loop_block,
                    true_arguments: Vec::new(),
                    false_target: exit,
                    false_arguments: Vec::new(),
                },
            );
            event.set_terminator(exit, ControlTerminator::Return);
            let stage = add_stage(&mut event, process, exit, target.into(), value);
            event.add_effect(Effect {
                region: event.root_region(),
                predecessors: vec![stage],
                kind: EffectKind::CommitFfState {
                    stages: vec![stage],
                },
            });
            event.verify().unwrap();

            lower_event_projection(
                &event,
                EventProjection::FusedClock,
                &SLTNodeArena::new(),
                false,
                object(100),
            )
            .unwrap()
        }

        let after_loop = lower();
        after_loop.verify_result().unwrap();
        assert!(instructions(&after_loop).any(|instruction| matches!(
            instruction,
            SIRInstruction::Store(address, ..) if address.region == STABLE_REGION
        )));
        assert!(!instructions(&after_loop).any(|instruction| matches!(
            instruction,
            SIRInstruction::Store(address, ..) if address.region == WORKING_REGION
        )));
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
    fn semantic_slt_cache_ignores_unrelated_input_bindings() {
        let source = object(1);
        let mut arena = SLTNodeArena::new();
        let input = arena
            .alloc(SLTNode::Input {
                variable: source,
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
        let root = arena
            .alloc(SLTNode::Binary(input, BinaryOp::Add, one))
            .unwrap();
        let mut semantic = SemanticSltCache::default();
        let mut first_inputs = HashMap::default();
        first_inputs.insert(VarAtomBase::new(object(2), 0, 7), RegisterId(20));
        let mut first_cache = HashMap::default();
        let first_ids = semantic.prepare(root, &arena, &first_inputs, &mut first_cache);
        first_cache.insert(root, RegisterId(30));
        semantic.record(&first_ids, &first_cache);

        let mut unrelated_inputs = HashMap::default();
        unrelated_inputs.insert(VarAtomBase::new(object(3), 0, 7), RegisterId(21));
        let mut unrelated_cache = HashMap::default();
        semantic.prepare(root, &arena, &unrelated_inputs, &mut unrelated_cache);
        assert_eq!(unrelated_cache.get(&root), Some(&RegisterId(30)));

        let mut relevant_inputs = HashMap::default();
        relevant_inputs.insert(VarAtomBase::new(source, 0, 7), RegisterId(22));
        let mut relevant_cache = HashMap::default();
        semantic.prepare(root, &arena, &relevant_inputs, &mut relevant_cache);
        assert!(!relevant_cache.contains_key(&root));
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
        assert!(
            !instructions(&fused)
                .any(|instruction| matches!(instruction, SIRInstruction::Commit(..)))
        );
        assert!(instructions(&fused).any(|instruction| matches!(
            instruction,
            SIRInstruction::Store(address, SIROffset::Static(8), 8, _, _, _)
                if address.region == STABLE_REGION
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
    fn effectful_process_can_publish_an_independent_stage_directly() {
        let (mut event, arena, process, block) = empty_clock_event();
        let region = event.blocks()[block.0].region;
        let value = event.add_value(Value {
            ty: ValueType::bit(8, false),
            scope: ValueScope::Process(process),
            region,
            kind: ValueKind::Constant {
                value: BigUint::from(0x5au8),
                unknown: BigUint::default(),
            },
        });
        let observation = event.add_effect(Effect {
            region,
            predecessors: Vec::new(),
            kind: EffectKind::RuntimeEvent {
                site_id: 0,
                arguments: vec![value],
                guard: None,
            },
        });
        let stage = event.add_effect(Effect {
            region,
            predecessors: vec![observation],
            kind: EffectKind::StageNextFf {
                process,
                target: range(1, 0, 7).into(),
                value,
                guard: None,
                priority: 0,
                stage_kind: crate::event_ir::FfStageKind::Fragment,
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
        assert!(instructions(&fused).any(|instruction| matches!(
            instruction,
            SIRInstruction::Store(address, SIROffset::Static(0), 8, _, _, _)
                if address.region == STABLE_REGION
        )));
        assert!(!instructions(&fused).any(|instruction| matches!(
            instruction,
            SIRInstruction::Store(address, ..) if address.region == WORKING_REGION
        )));
        assert!(!instructions(&fused).any(|instruction| matches!(
            instruction,
            SIRInstruction::Commit(source, destination, ..)
                if source.region == WORKING_REGION && destination.region == STABLE_REGION
        )));
    }

    #[test]
    fn independent_dynamic_stage_publishes_directly() {
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
                if address.region == STABLE_REGION
        )));
        assert!(!instructions(&fused).any(|instruction| matches!(
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
        let inverted = arena.alloc(SLTNode::Unary(UnaryOp::BitNot, input)).unwrap();
        let paths = vec![
            comb_path(intermediate, constant, []),
            comb_path(output, inverted, [VarAtomBase::new(intermediate, 0, 7)]),
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
            SIRInstruction::Unary(_, UnaryOp::BitNot, _)
        )));
        assert!(instructions(&fused).any(|instruction| matches!(
            instruction,
            SIRInstruction::Store(address, SIROffset::Static(0), 8, _, _, _)
                if address.absolute_addr() == ff && address.region == STABLE_REGION
        )));

        let settled = lower_event_projection(
            &event,
            EventProjection::FusedSettledClock,
            &arena,
            false,
            object(0),
        )
        .unwrap();
        settled.verify_result().unwrap();

        assert!(!instructions(&settled).any(|instruction| matches!(
            instruction,
            SIRInstruction::Load(_, address, SIROffset::Static(0), 8)
                if address.absolute_addr() == output && address.region == STABLE_REGION
        )));
        assert!(instructions(&settled).any(|instruction| matches!(
            instruction,
            SIRInstruction::Load(_, address, ..)
                if address.absolute_addr() == intermediate
        )));
        assert!(instructions(&settled).any(|instruction| matches!(
            instruction,
            SIRInstruction::Unary(_, UnaryOp::BitNot, _)
        )));
        assert!(instructions(&settled).any(|instruction| matches!(
            instruction,
            SIRInstruction::Store(address, SIROffset::Static(0), 8, _, _, _)
                if address.absolute_addr() == ff && address.region == STABLE_REGION
        )));
    }

    #[test]
    fn cross_definition_mux_stays_branchless_until_sir_profitability_is_known() {
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
        assert_eq!(division_blocks.len(), 1);
        assert_eq!(division_blocks[0].1, 2);
        assert!(
            fused
                .blocks
                .values()
                .all(|block| !matches!(block.terminator, SIRTerminator::Branch { .. })),
            "cross-definition control conversion needs post-SIR cost information"
        );

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
    fn same_predicate_muxes_lower_to_sparse_control() {
        let condition = object(1);
        let inputs = (2..10).map(object).collect::<Vec<_>>();
        let output = object(10);
        let ff = object(11);
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
        let condition_node = input(&mut arena, condition, BitAccess::new(0, 0));
        let values = inputs
            .iter()
            .map(|&object| input(&mut arena, object, BitAccess::new(0, 7)))
            .collect::<Vec<_>>();
        let divide = |arena: &mut SLTNodeArena<AbsoluteAddr>, lhs, rhs| {
            arena
                .alloc(SLTNode::Binary(lhs, BinaryOp::DivU, rhs))
                .unwrap()
        };
        let mux_a_then = divide(&mut arena, values[0], values[1]);
        let mux_a_else = divide(&mut arena, values[2], values[3]);
        let mux_b_then = divide(&mut arena, values[4], values[5]);
        let mux_b_else = divide(&mut arena, values[6], values[7]);
        let mux_a = arena
            .alloc(SLTNode::Mux {
                cond: condition_node,
                then_expr: mux_a_then,
                else_expr: mux_a_else,
            })
            .unwrap();
        let mux_b = arena
            .alloc(SLTNode::Mux {
                cond: condition_node,
                then_expr: mux_b_then,
                else_expr: mux_b_else,
            })
            .unwrap();
        let sum = arena
            .alloc(SLTNode::Binary(mux_a, BinaryOp::Add, mux_b))
            .unwrap();
        let sources = std::iter::once(VarAtomBase::new(condition, 0, 0))
            .chain(inputs.iter().map(|&object| VarAtomBase::new(object, 0, 7)))
            .collect::<Vec<_>>();
        let paths = vec![comb_path(output, sum, sources)];
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
                definition: CombDefinitionId(0),
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

        let branches = fused
            .blocks
            .values()
            .filter(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
            .count();
        assert_eq!(branches, 2);
        assert_eq!(
            instructions(&fused)
                .filter(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
                .count(),
            0
        );
        assert_eq!(
            instructions(&fused)
                .filter(|instruction| matches!(
                    instruction,
                    SIRInstruction::Binary(_, _, BinaryOp::DivU, _)
                ))
                .count(),
            4
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
        assert_eq!(
            instructions(&fused)
                .filter(|instruction| matches!(
                    instruction,
                    SIRInstruction::Store(address, SIROffset::Static(0), 8, _, _, _)
                        if address.region == MATERIALIZATION_HOME_REGION
                            && address.absolute_addr() == comb_output
                ))
                .count(),
            1
        );
        assert_eq!(
            instructions(&fused)
                .filter(|instruction| matches!(
                    instruction,
                    SIRInstruction::Load(_, address, SIROffset::Static(0), 8)
                        if address.region == MATERIALIZATION_HOME_REGION
                            && address.absolute_addr() == comb_output
                ))
                .count(),
            1
        );
        assert!(!instructions(&fused).any(|instruction| matches!(
            instruction,
            SIRInstruction::Store(address, ..)
                if address.region == MATERIALIZATION_HOME_REGION
                    && (address.absolute_addr() == ff_a || address.absolute_addr() == ff_b)
        )));
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
    fn guarded_stage_hoists_its_pure_comb_cone_before_the_store_guard() {
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
                stage_kind: crate::event_ir::FfStageKind::Fragment,
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
        assert!(has_payload(branch_block));
        assert!(!has_payload(store_block));
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
