use crate::{
    HashMap, HashSet, LogicPath, LogicPathTarget, NodeId, SLTNode, SLTNodeArena, SLTNodeFactsError,
};
use celox_analysis::dag_schedule::{
    MappedGraphRows, MappedNodeRows, schedule_min_live_values_and_tokens_with_mapped_rows,
};
use celox_analysis::interval::{DisjointIntervalError, DisjointIntervalMap, ExactInterval};
use celox_design::{BinaryOp, BitAccess, RuntimeErrorInfo, UnaryOp, VarAtomBase};
use celox_sir::{
    BlockId, ExecutionUnit, RegisterId, SIRBuilder, SIRInstruction, SIROffset, SIRTerminator,
    SIRValue,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::fmt::Display;
use std::hash::Hash;
use thiserror::Error;

mod lanes;
pub use lanes::{InstanceHierarchy, LanePartitionOptions, LaneScheduleResult, sort_lanes};

mod deps;
mod emit;
mod ff_comb;
mod fold_groups;
mod guard_regions;
mod partitions;
mod prepare;
mod work;
use deps::*;
use emit::*;
pub use ff_comb::*;
use fold_groups::*;
use guard_regions::*;
pub use partitions::*;
use prepare::*;
use work::*;

/// Sparse scheduler-facing memory effects for one same-trigger FF group.
#[derive(Debug, Clone, Default)]
pub struct FfAccessSummary<A> {
    pub reads: Vec<VarAtomBase<A>>,
    pub writes: Vec<VarAtomBase<A>>,
    pub dynamic_writes: HashSet<A>,
}

fn greedy_fas_sort(scc: &[usize], global_adj: &[Vec<usize>]) -> Vec<usize> {
    let scc_set: HashSet<usize> = scc.iter().cloned().collect();
    let mut local_adj: HashMap<usize, Vec<usize>> = HashMap::default();
    let mut in_degree: HashMap<usize, usize> = HashMap::default();

    for &u in scc {
        in_degree.entry(u).or_insert(0);
        let entries = local_adj.entry(u).or_default();
        for &v in &global_adj[u] {
            if scc_set.contains(&v) {
                entries.push(v);
                *in_degree.entry(v).or_insert(0) += 1;
            }
        }
    }

    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut current_nodes: HashSet<usize> = scc.iter().cloned().collect();

    while !current_nodes.is_empty() {
        // 1. Sinks
        while let Some(&u) = current_nodes
            .iter()
            .find(|&&u| local_adj.get(&u).is_none_or(|v| v.is_empty()))
        {
            right.push(u);
            current_nodes.remove(&u);
        }
        // 2. Sources
        while let Some(&u) = current_nodes
            .iter()
            .find(|&&u| in_degree.get(&u).is_none_or(|&d| d == 0))
        {
            left.push(u);
            current_nodes.remove(&u);
            if let Some(neighbors) = local_adj.remove(&u) {
                for v in neighbors {
                    if let Some(d) = in_degree.get_mut(&v) {
                        *d -= 1;
                    }
                }
            }
        }
        if current_nodes.is_empty() {
            break;
        }
        // 3. Maximum Degree Difference
        let &u = current_nodes
            .iter()
            .max_by_key(|&&u| {
                let out_d = local_adj.get(&u).map_or(0, |v| v.len());
                let in_d = in_degree.get(&u).cloned().unwrap_or(0);
                out_d as i32 - in_d as i32
            })
            .unwrap();

        left.push(u);
        current_nodes.remove(&u);
        if let Some(neighbors) = local_adj.remove(&u) {
            for v in neighbors {
                if let Some(d) = in_degree.get_mut(&v) {
                    *d -= 1;
                }
            }
        }
    }
    right.reverse();
    left.extend(right);
    left
}
fn calculate_required_iterations(adj: &[Vec<usize>], order: &[usize]) -> usize {
    let pos: HashMap<usize, usize> = order.iter().enumerate().map(|(i, &n)| (n, i)).collect();
    let scc_nodes: HashSet<usize> = order.iter().cloned().collect();

    // Record already visited nodes to ensure a "simple path"
    fn find_longest_backedge_path(
        u: usize,
        visited: &mut Vec<bool>,
        adj: &[Vec<usize>],
        pos: &HashMap<usize, usize>,
        scc_nodes: &HashSet<usize>,
    ) -> usize {
        visited[u] = true;
        let mut max_delay = 0;

        for &v in &adj[u] {
            if scc_nodes.contains(&v) && !visited[v] {
                // 0 if forward direction, 1 if back-edge
                let weight = if pos[&u] >= pos[&v] { 1 } else { 0 };
                max_delay = max_delay
                    .max(weight + find_longest_backedge_path(v, visited, adj, pos, scc_nodes));
            }
        }

        visited[u] = false; // backtrack
        max_delay
    }

    let mut overall_max_delay = 0;
    let mut visited = vec![false; adj.len()];

    // Search for the longest "waiting time (number of back-edges)" starting from each node
    for &start_node in order {
        overall_max_delay = overall_max_delay.max(find_longest_backedge_path(
            start_node,
            &mut visited,
            adj,
            &pos,
            &scc_nodes,
        ));
    }

    // Base execution (1) + number of times signals loop back (overall_max_delay)
    overall_max_delay + 1
}

#[derive(Error, Debug, PartialEq, Eq)]
pub enum SchedulerError<A: Display + Debug + Eq + Hash + Clone> {
    #[error("Combinational loop detected: {}", .blocks.iter().map(|v| format!("{}", v)).collect::<Vec<_>>().join(" -> "))]
    CombinationalLoop { blocks: Vec<LogicPath<A>> },
    #[error("Multiple driver detected: {}", .blocks.iter().map(|v| format!("{}", v)).collect::<Vec<_>>().join(","))]
    MultipleDriver { blocks: Vec<LogicPath<A>> },
    #[error("internal logic-path SCC condensation graph is invalid")]
    InvalidDependencyGraph,
}

impl<A: Display + Debug + Eq + Hash + Clone> SchedulerError<A> {
    pub fn map_addr<B: Display + Debug + Eq + Hash + Clone, F>(
        self,
        arena: &SLTNodeArena<A>,
        target_arena: &mut SLTNodeArena<B>,
        f: &F,
    ) -> Result<SchedulerError<B>, SLTNodeFactsError>
    where
        F: Fn(&A) -> B,
    {
        let mut cache = HashMap::default();
        Ok(match self {
            SchedulerError::CombinationalLoop { blocks } => SchedulerError::CombinationalLoop {
                blocks: blocks
                    .into_iter()
                    .map(|b| b.map_addr(arena, target_arena, &mut cache, f))
                    .collect::<Result<Vec<_>, _>>()?,
            },
            SchedulerError::MultipleDriver { blocks } => SchedulerError::MultipleDriver {
                blocks: blocks
                    .into_iter()
                    .map(|b| b.map_addr(arena, target_arena, &mut cache, f))
                    .collect::<Result<Vec<_>, _>>()?,
            },
            SchedulerError::InvalidDependencyGraph => SchedulerError::InvalidDependencyGraph,
        })
    }
}

pub struct ScheduleResult<Addr> {
    pub execution_units: Vec<ExecutionUnit<Addr>>,
    pub runtime_errors: HashMap<i64, RuntimeErrorInfo<Addr>>,
    /// Persistent-state ranges published directly by FF actions in the shared
    /// comb/FF schedule. These Stores are semantic state updates rather than
    /// disposable comb publications.
    pub direct_ff_writes: Vec<VarAtomBase<Addr>>,
    /// The partitions of a partitioned settle, in schedule order; empty when
    /// the schedule runs as one pass.
    pub partitions: Vec<SettlePartition<Addr>>,
    /// The targets of the paths that keep state from one settle to the next:
    /// paths that read a previous value or loop, and guarded processes.
    /// Every other path computes what it stores from its inputs alone (a
    /// bit-level order may still load a range before the bits it needs are
    /// stored, and then discards the stale bits). `None` when that is not
    /// known per path: capture events, and partitioned settles, whose flags
    /// keep what changed since the last one.
    pub stateful_targets: Option<Vec<VarAtomBase<Addr>>>,
    /// The whole-variable copies of a partitioned settle, target then
    /// source: a reader of the target counts as a reader of the source in
    /// [`SettlePartition::reads`], so an alias may replace the copy.
    pub copies: Vec<(Addr, Addr)>,
}

/// Request to split a combinational schedule into partitions that run only
/// when state they read changed since they last ran.
///
/// Byte `p` of `flags`, a two-state object, is partition `p`'s dirty flag:
/// the settle runs the partition only when it is not zero, and clears it.
/// A partition that runs sets the flags of the partitions reading what it
/// changed; every other writer of state a partition reads must set its flag
/// too ([`SettlePartition::reads`]). Byte `N` (for `N` partitions) is the
/// untracked byte ([`celox_design::settle_untracked_flag`]): when it is
/// set, the settle starts by setting every partition's flag, so that every
/// partition runs on every settle.
#[derive(Clone, Debug)]
pub struct SettlePartitioning<Addr> {
    pub flags: Addr,
    /// Approximate expression nodes per partition.
    pub target_cost: usize,
    /// Readers outside the settle of the state it writes, by the index of
    /// their flag byte after the untracked byte
    /// ([`celox_design::settle_wait_flag`]): the partitions changing the
    /// state set those flags as they set the flags of the partitions reading
    /// it.
    pub external_readers: HashMap<Addr, Vec<usize>>,
    /// The event signals and their aliases. A path reading or writing one
    /// changes on every edge, so such paths get partitions of their own: the
    /// logic sharing a partition with them would run on every edge too.
    pub clock_nets: HashSet<Addr>,
    /// Whether the settle counts its work in run counters after the flags
    /// (`celox_design::settle_profile_offset`); `waits` is the number of
    /// wait flags, which the counters follow.
    pub profile: Option<usize>,
}

/// One partition of a partitioned settle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettlePartition<Addr> {
    /// The state objects the partition reads. A change to any of them must
    /// set the partition's flag before the next settle.
    pub reads: Vec<Addr>,
    /// The state objects the partition writes.
    pub writes: Vec<Addr>,
    /// Whether the partition runs on every settle, because it has effects
    /// beyond its stores.
    pub pinned: bool,
    /// How the partition was planned, for diagnostics.
    pub plan: SettlePartitionPlan<Addr>,
}

/// The plan of one partition of a partitioned settle: what it costs, which
/// bits it reads, and what it compares after it ran and whom it marks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettlePartitionPlan<Addr> {
    /// Approximate expression nodes of its paths.
    pub cost: usize,
    /// The bit ranges (`lsb`, `msb`) of each object it reads, `None` for
    /// all of the object.
    pub read_bits: Vec<(Addr, Option<Vec<(usize, usize)>>)>,
    /// The chunks it compares after it ran, with the readers it marks when
    /// one changed.
    pub watched: Vec<SettleWatch<Addr>>,
    /// The flags it sets whenever it runs.
    pub marks: Vec<usize>,
}

/// A chunk of an object a partition compares after it ran
/// ([`SettlePartitionPlan::watched`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettleWatch<Addr> {
    pub object: Addr,
    /// First bit of the chunk in the object.
    pub lsb: usize,
    pub width: usize,
    /// The flags it sets when the chunk changed: partitions, then wait
    /// flags ([`celox_design::settle_wait_flag`]).
    pub readers: Vec<usize>,
}

pub trait ClockFfLowering<Addr> {
    type Error;

    fn summaries(&self) -> &[FfAccessSummary<Addr>];
    fn begin(
        &mut self,
        builder: &mut SIRBuilder<Addr>,
        direct_writes: &[Vec<VarAtomBase<Addr>>],
    ) -> Result<(), Self::Error>;
    fn lower(
        &mut self,
        index: usize,
        direct_writes: &[VarAtomBase<Addr>],
        builder: &mut SIRBuilder<Addr>,
    ) -> Result<(), Self::Error>;
    fn finish(
        &mut self,
        builder: &mut SIRBuilder<Addr>,
        direct_writes: &[Vec<VarAtomBase<Addr>>],
    ) -> Result<(), Self::Error>;
}

pub enum ClockSortError<Addr: Display + Debug + Eq + Hash + Clone, E> {
    Scheduler(SchedulerError<Addr>),
    Lowering(E),
}

enum ScheduledWork {
    CombPath(usize),
    CombScc(Vec<usize>),
    /// A dependency-ordered run of state publications selected by one SLT
    /// condition.  The path scheduler has already fixed the order; lowering
    /// only preserves the exclusivity which would otherwise become several
    /// independent Muxes and later need to be rediscovered from SIR.
    GuardedComb {
        condition: NodeId,
        paths: Vec<usize>,
    },
    Ff(usize),
}

/// Schedules and transforms LogicPaths into Simulation Intermediate Representation (SIR).
///
/// This process performs:
/// 1. Dependency analysis to detect multiple drivers and combinational loops.
/// 2. SCC detection via Tarjan's algorithm.
/// 3. Scheduling based on two primary strategies:
///    - **Strategy A (Static Unrolling)**: For DAG parts or loops with small, predictable convergence bounds.
///    - **Strategy B (Dynamic Convergence)**: For complex SCCs or potential "True Loops", implementing
///      runtime oscillation detection and convergence-based repetition.
fn sort_impl<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display, E>(
    input: Vec<LogicPath<Addr>>,
    arena: &SLTNodeArena<Addr>,
    ignored_loops: &HashSet<(Addr, Addr)>,
    true_loops: &HashMap<(Addr, Addr), usize>,
    four_state: bool,
    var_widths: &HashMap<Addr, usize>,
    unpacked_element_widths: &HashMap<Addr, usize>,
    first_runtime_error_code: i64,
    mut ff: Option<&mut dyn ClockFfLowering<Addr, Error = E>>,
    partitioning: Option<SettlePartitioning<Addr>>,
) -> Result<ScheduleResult<Addr>, ClockSortError<Addr, E>> {
    let prepared = prepare_schedule(
        input,
        arena,
        four_state,
        var_widths,
        unpacked_element_widths,
        ff.as_deref().map(|lowering| lowering.summaries()),
    )
    .map_err(ClockSortError::Scheduler)?;
    let component = &prepared.component_by_path;
    let looping: HashSet<usize> = prepared
        .adj
        .iter()
        .enumerate()
        .flat_map(|(v, successors)| {
            successors
                .iter()
                .filter(move |&&u| component[u] == component[v])
                .flat_map(move |&u| [u, v])
        })
        .collect();
    let stateful_targets = prepared
        .input
        .iter()
        .enumerate()
        .filter_map(|(index, path)| match &path.target {
            LogicPathTarget::CombCaptureEvent { .. } => Some(None),
            LogicPathTarget::Process { target, .. } => Some(Some(*target)),
            LogicPathTarget::Var(target)
                if !path.previous_sources.is_empty() || looping.contains(&index) =>
            {
                Some(Some(*target))
            }
            LogicPathTarget::Var(_) => None,
        })
        .collect::<Option<Vec<_>>>();
    let direct_ff_writes = prepared
        .direct_ff_writes_by_action
        .iter()
        .flatten()
        .copied()
        .collect();
    let mut lowerer = crate::SLTToSIRLowerer::new(four_state)
        .with_unpacked_input_types(arena, unpacked_element_widths);
    let context = WorkLoweringContext {
        input: &prepared.input,
        arena,
        adj: &prepared.adj,
        component_by_path: &prepared.component_by_path,
        ignored_loops,
        true_loops,
        fold_group_schedule_index: &prepared.fold_group_schedule_index,
        unpacked_element_widths,
        direct_ff_writes_by_action: &prepared.direct_ff_writes_by_action,
        four_state,
        split_large_units: ff.is_none(),
    };
    let mut lowering = WorkLowering::new(&context, &mut lowerer);
    if let Some(ff_lowering) = ff.as_deref_mut() {
        ff_lowering
            .begin(&mut lowering.builder, &prepared.direct_ff_writes_by_action)
            .map_err(ClockSortError::Lowering)?;
    }
    let mut runtime_errors: HashMap<i64, RuntimeErrorInfo<Addr>> = HashMap::default();
    let mut next_runtime_error_code = first_runtime_error_code;
    lowering.settle_profile_waits = partitioning
        .as_ref()
        .filter(|_| ff.is_none())
        .and_then(|partitioning| partitioning.profile);
    let plan = partitioning
        .filter(|_| ff.is_none())
        .and_then(|partitioning| {
            plan_settle_partitions(
                &prepared,
                arena,
                var_widths,
                unpacked_element_widths,
                partitioning,
            )
        });
    for (index, work) in prepared.scheduled_work.iter().enumerate() {
        if let Some((plan, flags)) = &plan {
            if let Some(partition) = plan.first_work.iter().position(|&first| first == index) {
                if partition > 0 {
                    lowering.end_partition(*flags, partition - 1, &plan.partitions[partition - 1]);
                } else {
                    lowering.begin_settle(*flags, plan.partitions.len());
                }
                if partition % 8 == 0 {
                    lowering.end_partition_group();
                    lowering.begin_partition_group(
                        *flags,
                        partition,
                        &plan.partitions[partition..(partition + 8).min(plan.partitions.len())],
                    );
                }
                lowering.begin_partition(*flags, partition, &plan.partitions[partition]);
            }
        }
        lowering.lower(
            work,
            ff.as_deref_mut(),
            &mut runtime_errors,
            &mut next_runtime_error_code,
        )?;
    }
    if let Some((plan, flags)) = &plan
        && let Some(last) = plan.partitions.last()
    {
        lowering.end_partition(*flags, plan.partitions.len() - 1, last);
        lowering.end_partition_group();
    }
    // Flush the final exact grouped-fold run after the SCC loop.
    lowering.flush_pending_folds();
    if let Some(ff_lowering) = ff {
        ff_lowering
            .finish(&mut lowering.builder, &prepared.direct_ff_writes_by_action)
            .map_err(ClockSortError::Lowering)?;
    }
    // Partition flags carry what changed from one settle to the next.
    let stateful_targets = stateful_targets.filter(|_| plan.is_none());
    let (partitions, copies) = plan
        .map(|(plan, _)| {
            (
                plan.partitions
                    .into_iter()
                    .map(|partition| SettlePartition {
                        plan: SettlePartitionPlan {
                            cost: partition.cost,
                            read_bits: partition.read_bits,
                            watched: partition
                                .watched
                                .into_iter()
                                .map(|chunk| SettleWatch {
                                    object: chunk.object,
                                    lsb: chunk.lsb,
                                    width: chunk.width,
                                    readers: chunk.readers,
                                })
                                .collect(),
                            marks: partition.marks,
                        },
                        reads: partition.reads,
                        writes: partition.writes,
                        pinned: partition.pinned,
                    })
                    .collect(),
                plan.copies,
            )
        })
        .unwrap_or_default();
    Ok(ScheduleResult {
        execution_units: lowering.finish(),
        runtime_errors,
        direct_ff_writes,
        partitions,
        stateful_targets,
        copies,
    })
}

#[cfg(test)]
pub fn sort<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    input: Vec<LogicPath<Addr>>,
    arena: &SLTNodeArena<Addr>,
    ignored_loops: &HashSet<(Addr, Addr)>,
    true_loops: &HashMap<(Addr, Addr), usize>,
    four_state: bool,
    var_widths: &HashMap<Addr, usize>,
    first_runtime_error_code: i64,
) -> Result<ScheduleResult<Addr>, SchedulerError<Addr>> {
    sort_with_unpacked_element_widths(
        input,
        arena,
        ignored_loops,
        true_loops,
        four_state,
        var_widths,
        &HashMap::default(),
        first_runtime_error_code,
        None,
    )
}

pub fn sort_with_unpacked_element_widths<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    input: Vec<LogicPath<Addr>>,
    arena: &SLTNodeArena<Addr>,
    ignored_loops: &HashSet<(Addr, Addr)>,
    true_loops: &HashMap<(Addr, Addr), usize>,
    four_state: bool,
    var_widths: &HashMap<Addr, usize>,
    unpacked_element_widths: &HashMap<Addr, usize>,
    first_runtime_error_code: i64,
    partitioning: Option<SettlePartitioning<Addr>>,
) -> Result<ScheduleResult<Addr>, SchedulerError<Addr>> {
    match sort_impl::<Addr, std::convert::Infallible>(
        input,
        arena,
        ignored_loops,
        true_loops,
        four_state,
        var_widths,
        unpacked_element_widths,
        first_runtime_error_code,
        None,
        partitioning,
    ) {
        Ok(result) => Ok(result),
        Err(ClockSortError::Scheduler(error)) => Err(error),
        Err(ClockSortError::Lowering(_)) => {
            unreachable!("ordinary comb scheduling has no FF lowering callback")
        }
    }
}

pub fn sort_clock<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display, E>(
    input: Vec<LogicPath<Addr>>,
    arena: &SLTNodeArena<Addr>,
    ignored_loops: &HashSet<(Addr, Addr)>,
    true_loops: &HashMap<(Addr, Addr), usize>,
    four_state: bool,
    var_widths: &HashMap<Addr, usize>,
    unpacked_element_widths: &HashMap<Addr, usize>,
    first_runtime_error_code: i64,
    ff: &mut dyn ClockFfLowering<Addr, Error = E>,
) -> Result<ScheduleResult<Addr>, ClockSortError<Addr, E>> {
    sort_impl(
        input,
        arena,
        ignored_loops,
        true_loops,
        four_state,
        var_widths,
        unpacked_element_widths,
        first_runtime_error_code,
        Some(ff),
        None,
    )
}

#[cfg(test)]
mod tests;
