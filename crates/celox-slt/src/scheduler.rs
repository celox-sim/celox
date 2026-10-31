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

fn ranges_cover_access(ranges: &[BitAccess], access: BitAccess) -> bool {
    let mut ranges = ranges.to_vec();
    ranges.sort_unstable_by_key(|range| (range.lsb, range.msb));
    let mut next = access.lsb;
    for range in ranges {
        if range.msb < next {
            continue;
        }
        if range.lsb > next {
            return false;
        }
        if range.msb >= access.msb {
            return true;
        }
        let Some(after) = range.msb.checked_add(1) else {
            return false;
        };
        next = after;
    }
    false
}

/// Conservatively check that every input of `address` reachable from `root`
/// is supplied by the carried ranges. Dynamic inputs retain their complete
/// declared access here; retaining an extra external dependency is safe.
fn node_reads_only_covered_ranges<Addr: Clone + Eq + Hash>(
    root: NodeId,
    address: &Addr,
    ranges: &[BitAccess],
    arena: &SLTNodeArena<Addr>,
) -> bool {
    let mut visited = HashSet::default();
    let mut work = vec![root];
    while let Some(node) = work.pop() {
        if !visited.insert(node) {
            continue;
        }
        match arena.get(node) {
            SLTNode::Input {
                variable,
                index,
                access,
                ..
            } => {
                if variable == address && !ranges_cover_access(ranges, *access) {
                    return false;
                }
                work.extend(index.iter().map(|entry| entry.node));
            }
            SLTNode::Constant(..) => {}
            SLTNode::Binary(lhs, _, rhs) => {
                work.push(*lhs);
                work.push(*rhs);
            }
            SLTNode::Unary(_, inner) | SLTNode::Capture { expr: inner, .. } => work.push(*inner),
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                work.push(*cond);
                work.push(*then_expr);
                work.push(*else_expr);
            }
            SLTNode::Concat(parts) => work.extend(parts.iter().map(|(part, _)| *part)),
            SLTNode::Slice { expr, .. } => work.push(*expr),
            SLTNode::ForFold {
                start,
                end,
                result,
                initials,
                updates,
                effects,
                continue_cond,
                ..
            } => {
                if let crate::SLTLoopBound::Expr(node)
                | crate::SLTLoopBound::TypedExpr { node, .. } = start
                {
                    work.push(*node);
                }
                if let crate::SLTLoopBound::Expr(node)
                | crate::SLTLoopBound::TypedExpr { node, .. } = end
                {
                    work.push(*node);
                }
                if let crate::SLTForFoldResult::Transient { initial, update } = result {
                    work.push(*initial);
                    work.push(*update);
                }
                work.extend(initials.iter().map(|state| state.expr));
                work.extend(updates.iter().map(|state| state.expr));
                for effect in effects {
                    match effect {
                        crate::SLTForEffect::Event { guard, args, .. } => {
                            work.extend(*guard);
                            work.extend(args.iter().copied());
                        }
                        crate::SLTForEffect::Runner(runner) => work.push(*runner),
                    }
                }
                work.push(*continue_cond);
            }
            SLTNode::ForFoldGroup {
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
        }
    }
    true
}

fn collect_node_input_deps<Addr: Clone + Eq + Hash + Debug + Copy + Display>(
    node: crate::NodeId,
    arena: &SLTNodeArena<Addr>,
    memo: &mut HashMap<crate::NodeId, HashSet<Addr>>,
    inverse_memo: &mut HashMap<Addr, HashSet<crate::NodeId>>,
) -> HashSet<Addr> {
    if let Some(found) = memo.get(&node) {
        return found.clone();
    }

    // Arena edges point to older nodes. Visit only uncached reachable nodes,
    // then evaluate each node once in dependency order on the heap.
    let root = node;
    let mut pending = vec![root];
    let mut seen = HashSet::default();
    let mut reachable = Vec::new();
    while let Some(current) = pending.pop() {
        if memo.contains_key(&current) || !seen.insert(current) {
            continue;
        }
        pending.extend(crate::SLTToSIRLowerer::node_children(current, arena));
        reachable.push(current);
    }
    reachable.sort_unstable();
    for node in reachable {
        let deps = match arena.get(node) {
            crate::SLTNode::Input {
                variable, index, ..
            } => {
                let mut set = HashSet::default();
                set.insert(*variable);
                for idx in index {
                    set.extend(memo[&idx.node].clone());
                }
                set
            }
            crate::SLTNode::Slice { expr, .. } => memo[expr].clone(),
            crate::SLTNode::Concat(parts) => {
                let mut set = HashSet::default();
                for (part, _) in parts {
                    set.extend(memo[part].clone());
                }
                set
            }
            crate::SLTNode::Binary(lhs, _, rhs) => {
                let mut set = memo[lhs].clone();
                set.extend(memo[rhs].clone());
                set
            }
            crate::SLTNode::Unary(_, inner) => memo[inner].clone(),
            crate::SLTNode::Capture { expr, .. } => memo[expr].clone(),
            crate::SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                let mut set = memo[cond].clone();
                set.extend(memo[then_expr].clone());
                set.extend(memo[else_expr].clone());
                set
            }
            crate::SLTNode::ForFold {
                loop_var,
                start,
                end,
                result,
                initials,
                updates,
                effects,
                continue_cond,
                ..
            } => {
                let mut set = HashSet::default();
                match start {
                    crate::SLTLoopBound::Const(_) => {}
                    crate::SLTLoopBound::Expr(node)
                    | crate::SLTLoopBound::TypedExpr { node, .. } => {
                        set.extend(memo[node].clone());
                    }
                }
                match end {
                    crate::SLTLoopBound::Const(_) => {}
                    crate::SLTLoopBound::Expr(node)
                    | crate::SLTLoopBound::TypedExpr { node, .. } => {
                        set.extend(memo[node].clone());
                    }
                }
                if let crate::SLTForFoldResult::Transient { initial, update } = result {
                    set.extend(memo[initial].clone());
                    set.extend(memo[update].clone());
                }
                for init in initials {
                    set.extend(memo[&init.expr].clone());
                }
                for update in updates {
                    set.extend(memo[&update.expr].clone());
                }
                for effect in effects {
                    match effect {
                        crate::SLTForEffect::Event { guard, args, .. } => {
                            if let Some(guard) = guard {
                                set.extend(memo[guard].clone());
                            }
                            for arg in args {
                                set.extend(memo[arg].clone());
                            }
                        }
                        crate::SLTForEffect::Runner(runner) => {
                            set.extend(memo[runner].clone());
                        }
                    }
                }
                set.remove(loop_var);
                set.extend(memo[continue_cond].clone());
                set.remove(loop_var);
                set
            }
            crate::SLTNode::ForFoldGroup {
                loop_var,
                entry_guard,
                states,
                ..
            } => {
                let mut set = memo[entry_guard].clone();
                for state in states {
                    set.extend(memo[&state.initial].clone());
                }
                let mut update_deps = HashSet::default();
                for state in states {
                    update_deps.extend(memo[&state.update].clone());
                }
                update_deps.remove(loop_var);

                let mut state_ranges: HashMap<Addr, Vec<BitAccess>> = HashMap::default();
                for state in states {
                    state_ranges
                        .entry(state.target.id)
                        .or_default()
                        .push(state.target.access);
                }
                for (state_id, ranges) in state_ranges {
                    if states.iter().all(|state| {
                        node_reads_only_covered_ranges(state.update, &state_id, &ranges, arena)
                    }) {
                        update_deps.remove(&state_id);
                    }
                }
                set.extend(update_deps);
                set
            }
            crate::SLTNode::Constant(_, _, _, _) => HashSet::default(),
        };

        for &addr in &deps {
            inverse_memo.entry(addr).or_default().insert(node);
        }
        memo.insert(node, deps.clone());
    }
    memo[&root].clone()
}

fn collect_logic_path_input_deps<Addr: Clone + Eq + Hash + Debug + Copy + Display>(
    path: &LogicPath<Addr>,
    arena: &SLTNodeArena<Addr>,
    memo: &mut HashMap<NodeId, HashSet<Addr>>,
    inverse_memo: &mut HashMap<Addr, HashSet<NodeId>>,
) {
    collect_node_input_deps(path.expr, arena, memo, inverse_memo);
    for (_, node) in &path.local_inputs {
        collect_node_input_deps(*node, arena, memo, inverse_memo);
    }
    for node in &path.pre_lower_nodes {
        collect_node_input_deps(*node, arena, memo, inverse_memo);
    }
}

struct TarjanContext {
    index: usize,
    stack: Vec<usize>,
    on_stack: HashSet<usize>,
    indices: Vec<Option<usize>>,
    lowlink: Vec<Option<usize>>,
    sccs: Vec<Vec<usize>>,
}

fn strong_connect(u: usize, adj: &Vec<Vec<usize>>, ctx: &mut TarjanContext) {
    ctx.indices[u] = Some(ctx.index);
    ctx.lowlink[u] = Some(ctx.index);
    ctx.index += 1;
    ctx.stack.push(u);
    ctx.on_stack.insert(u);

    for &v in &adj[u] {
        if ctx.indices[v].is_none() {
            strong_connect(v, adj, ctx);
            ctx.lowlink[u] = Some(ctx.lowlink[u].unwrap().min(ctx.lowlink[v].unwrap()));
        } else if ctx.on_stack.contains(&v) {
            ctx.lowlink[u] = Some(ctx.lowlink[u].unwrap().min(ctx.indices[v].unwrap()));
        }
    }

    if ctx.lowlink[u] == ctx.indices[u] {
        let mut scc = Vec::new();
        while let Some(w) = ctx.stack.pop() {
            ctx.on_stack.remove(&w);
            scc.push(w);
            if w == u {
                break;
            }
        }
        ctx.sccs.push(scc);
    }
}

fn component_map(adj: &Vec<Vec<usize>>) -> Vec<usize> {
    let mut ctx = TarjanContext {
        index: 0,
        stack: Vec::new(),
        on_stack: HashSet::default(),
        indices: vec![None; adj.len()],
        lowlink: vec![None; adj.len()],
        sccs: Vec::new(),
    };
    for node in 0..adj.len() {
        if ctx.indices[node].is_none() {
            strong_connect(node, adj, &mut ctx);
        }
    }
    let mut component_by_node = vec![usize::MAX; adj.len()];
    for (component, nodes) in ctx.sccs.iter().enumerate() {
        for &node in nodes {
            component_by_node[node] = component;
        }
    }
    component_by_node
}

/// Add old-state anti-dependencies which make a later FF write eligible for
/// direct publication.  These edges are profitable ordering constraints, not
/// RTL semantic requirements: WORKING staging is always a correct fallback.
///
/// Add all candidates once, identify the SCCs they create, then remove every
/// newly-added edge internal to such an SCC.  Any remaining edge is acyclic in
/// the resulting graph.  This keeps all old-state readers before a directly
/// publishable writer without turning feedback pipelines into comb/FF loops.
fn add_acyclic_ff_write_order_edges(
    adj: &mut Vec<Vec<usize>>,
    optional_edges: impl IntoIterator<Item = (usize, usize)>,
) -> HashSet<(usize, usize)> {
    let mut requested = HashSet::default();
    let mut added = vec![Vec::<usize>::new(); adj.len()];
    for (source, target) in optional_edges {
        if source == target || source >= adj.len() || target >= adj.len() {
            continue;
        }
        requested.insert((source, target));
        if !adj[source].contains(&target) {
            adj[source].push(target);
            added[source].push(target);
        }
    }
    if added.iter().all(Vec::is_empty) {
        return requested;
    }

    let component_by_node = component_map(adj);
    for (source, targets) in added.iter_mut().enumerate() {
        targets.retain(|target| component_by_node[source] == component_by_node[*target]);
        targets.sort_unstable();
        if !targets.is_empty() {
            adj[source].retain(|target| targets.binary_search(target).is_err());
        }
    }
    requested.retain(|(source, target)| adj[*source].contains(target));
    requested
}

/// Memory definitions and uses induced by one set of LogicPaths.
///
/// A variable target is one bit-range MemoryDef. A current-value source is a
/// MemoryUse of every overlapping definition; a previous-value source is a
/// live-on-entry use plus an anti-dependence which keeps that use before an
/// overlapping definition. `dependencies` contains all semantic edges while
/// `values` contains only Def-to-Use edges which can become a forwarded value
/// during later lowering. Both relations retain their forward and reverse
/// adjacency so regional scheduling can borrow either direction.
struct LogicPathMemorySsa {
    dependencies: LogicPathEdges,
    values: LogicPathEdges,
}

struct LogicPathEdges {
    users: Vec<Vec<usize>>,
    predecessors: Vec<Vec<usize>>,
}

impl LogicPathEdges {
    fn new(node_count: usize) -> Self {
        Self {
            users: vec![Vec::new(); node_count],
            predecessors: vec![Vec::new(); node_count],
        }
    }

    fn resize(&mut self, node_count: usize) {
        self.users.resize_with(node_count, Vec::new);
        self.predecessors.resize_with(node_count, Vec::new);
    }

    fn push(&mut self, predecessor: usize, user: usize) {
        self.users[predecessor].push(user);
        self.predecessors[user].push(predecessor);
    }

    fn canonicalize(&mut self) {
        for row in self.users.iter_mut().chain(&mut self.predecessors) {
            row.sort_unstable();
            row.dedup();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FfCombSchedulePlan {
    pub required_comb: Vec<bool>,
    pub comb_value_predecessors: Vec<Vec<usize>>,
    pub comb_before_direct_write: Vec<Vec<usize>>,
    pub ff_before_direct_write: Vec<Vec<usize>>,
}

fn bit_interval(access: BitAccess) -> Option<(usize, usize)> {
    Some((
        access.lsb,
        access.msb.checked_sub(access.lsb)?.checked_add(1)?,
    ))
}

fn build_logic_path_memory_ssa<Addr>(
    input: &[LogicPath<Addr>],
) -> Result<LogicPathMemorySsa, SchedulerError<Addr>>
where
    Addr: Copy + Ord + Hash + Eq + Display + Debug,
{
    let mut definition_intervals = Vec::new();
    for (path, logic_path) in input.iter().enumerate() {
        let Some(target) = logic_path.target.var() else {
            continue;
        };
        let Some((start, length)) = bit_interval(target.access) else {
            return Err(SchedulerError::InvalidDependencyGraph);
        };
        definition_intervals.push(ExactInterval {
            object: target.id,
            start,
            length,
            value: path,
        });
    }
    let definitions = match DisjointIntervalMap::try_new(definition_intervals) {
        Ok(definitions) => definitions,
        Err(DisjointIntervalError::Overlap { first, second }) => {
            return Err(SchedulerError::MultipleDriver {
                blocks: vec![input[first].clone(), input[second].clone()],
            });
        }
        Err(DisjointIntervalError::Empty { .. } | DisjointIntervalError::Overflow { .. }) => {
            return Err(SchedulerError::InvalidDependencyGraph);
        }
    };

    let mut dependencies = LogicPathEdges::new(input.len());
    let mut values = LogicPathEdges::new(input.len());
    for (user, path) in input.iter().enumerate() {
        for source in &path.sources {
            let Some((start, length)) = bit_interval(source.access) else {
                return Err(SchedulerError::InvalidDependencyGraph);
            };
            let reaching = definitions
                .overlapping(&source.id, start, length)
                .map_err(|_| SchedulerError::InvalidDependencyGraph)?;
            for definition in reaching {
                dependencies.push(definition, user);
                values.push(definition, user);
            }
        }

        // A previous-value source reads the live-on-entry version. Preserve
        // that read before every overlapping definition. A self read/write is
        // already one atomic LogicPath and needs no self anti-dependence.
        for source in &path.previous_sources {
            let Some((start, length)) = bit_interval(source.access) else {
                return Err(SchedulerError::InvalidDependencyGraph);
            };
            let reaching = definitions
                .overlapping(&source.id, start, length)
                .map_err(|_| SchedulerError::InvalidDependencyGraph)?;
            for definition in reaching.filter(|definition| *definition != user) {
                dependencies.push(user, definition);
            }
        }
        for target in &path.order_before {
            if target.0 < input.len() && target.0 != user {
                dependencies.push(user, target.0);
            }
        }
    }
    // A process publishes multiple outputs but owns one activation guard and
    // RNG state. Keep its projections ordered, including in cyclic schedules.
    let mut previous_process_path = HashMap::default();
    for (index, path) in input.iter().enumerate() {
        if let LogicPathTarget::Process { process, .. } = &path.target {
            if let Some(previous) = previous_process_path.insert(*process, index) {
                dependencies.push(previous, index);
            }
        }
    }
    dependencies.canonicalize();
    values.canonicalize();
    Ok(LogicPathMemorySsa {
        dependencies,
        values,
    })
}

/// Paths participating in a semantic dependency cycle, including self loops.
/// Frontends can use this before scheduling to refine atomic pure expressions
/// without perturbing the representation of already acyclic logic.
pub fn cyclic_logic_paths<Addr>(
    input: &[LogicPath<Addr>],
) -> Result<Vec<usize>, SchedulerError<Addr>>
where
    Addr: Copy + Ord + Hash + Eq + Display + Debug,
{
    let memory = build_logic_path_memory_ssa(input)?;
    let edges = &memory.dependencies.users;
    let components = component_map(edges);
    Ok(edges
        .iter()
        .enumerate()
        .filter_map(|(source, targets)| {
            targets
                .iter()
                .any(|&target| components[source] == components[target])
                .then_some(source)
        })
        .collect())
}

/// Group bit ranges by destination object for overlap probes.
///
/// Overlap tests pair a read with the writes of the same object, or a write
/// with the reads of the same object. Indexing one side by object id lets the
/// probe scan only atoms that share the object instead of the whole access
/// list, which keeps the FF/comb scheduling passes from nesting linearly.
fn index_accesses<'a, Addr: Copy + Eq + Hash + 'a>(
    accesses: impl IntoIterator<Item = &'a VarAtomBase<Addr>>,
) -> HashMap<Addr, Vec<BitAccess>> {
    let mut index = HashMap::<Addr, Vec<BitAccess>>::default();
    for access in accesses {
        index.entry(access.id).or_default().push(access.access);
    }
    index
}

/// Returns true when any indexed range overlaps `access`.
fn indexed_overlaps<Addr: Eq + Hash>(
    index: &HashMap<Addr, Vec<BitAccess>>,
    access: &VarAtomBase<Addr>,
) -> bool {
    index
        .get(&access.id)
        .is_some_and(|ranges| ranges.iter().any(|range| range.overlaps(&access.access)))
}

pub(crate) fn plan_ff_comb_schedule<Addr>(
    input: &[LogicPath<Addr>],
    ff: &[FfAccessSummary<Addr>],
) -> Result<FfCombSchedulePlan, SchedulerError<Addr>>
where
    Addr: Copy + Ord + Hash + Eq + Display + Debug,
{
    let memory = build_logic_path_memory_ssa(input)?;
    let mut definition_intervals = Vec::new();
    for (path, logic_path) in input.iter().enumerate() {
        let Some(target) = logic_path.target.var() else {
            continue;
        };
        let Some((start, length)) = bit_interval(target.access) else {
            return Err(SchedulerError::InvalidDependencyGraph);
        };
        definition_intervals.push(ExactInterval {
            object: target.id,
            start,
            length,
            value: path,
        });
    }
    let definitions = DisjointIntervalMap::try_new(definition_intervals)
        .map_err(|_| SchedulerError::InvalidDependencyGraph)?;

    let mut comb_value_predecessors = vec![Vec::new(); ff.len()];
    let mut required_comb = vec![false; input.len()];
    let mut work = Vec::new();
    for (ff_index, summary) in ff.iter().enumerate() {
        for read in &summary.reads {
            let Some((start, length)) = bit_interval(read.access) else {
                return Err(SchedulerError::InvalidDependencyGraph);
            };
            for definition in definitions
                .overlapping(&read.id, start, length)
                .map_err(|_| SchedulerError::InvalidDependencyGraph)?
            {
                comb_value_predecessors[ff_index].push(definition);
                if !std::mem::replace(&mut required_comb[definition], true) {
                    work.push(definition);
                }
            }
        }
    }
    for (path, logic_path) in input.iter().enumerate() {
        if logic_path_is_scheduling_barrier(logic_path)
            && !std::mem::replace(&mut required_comb[path], true)
        {
            work.push(path);
        }
    }
    for row in &mut comb_value_predecessors {
        row.sort_unstable();
        row.dedup();
    }

    while let Some(path) = work.pop() {
        for &predecessor in &memory.dependencies.predecessors[path] {
            if !std::mem::replace(&mut required_comb[predecessor], true) {
                work.push(predecessor);
            }
        }
    }

    // Index each summary's writes by object once; both loops below probe the
    // same write set repeatedly.
    let ff_writes = ff
        .iter()
        .map(|summary| index_accesses(&summary.writes))
        .collect::<Vec<_>>();

    let mut comb_before_direct_write = vec![Vec::new(); ff.len()];
    for (path_index, path) in input.iter().enumerate() {
        if !required_comb[path_index] {
            continue;
        }
        for (ff_index, writes) in ff_writes.iter().enumerate() {
            if path
                .sources
                .iter()
                .chain(&path.previous_sources)
                .any(|read| indexed_overlaps(writes, read))
            {
                comb_before_direct_write[ff_index].push(path_index);
            }
        }
    }

    let mut ff_before_direct_write = vec![Vec::new(); ff.len()];
    for (writer, writes) in ff_writes.iter().enumerate() {
        for (reader, read_summary) in ff.iter().enumerate() {
            if writer != reader
                && read_summary
                    .reads
                    .iter()
                    .any(|read| indexed_overlaps(writes, read))
            {
                ff_before_direct_write[writer].push(reader);
            }
        }
    }
    for row in comb_before_direct_write
        .iter_mut()
        .chain(&mut ff_before_direct_write)
    {
        row.sort_unstable();
        row.dedup();
    }

    Ok(FfCombSchedulePlan {
        required_comb,
        comb_value_predecessors,
        comb_before_direct_write,
        ff_before_direct_write,
    })
}

fn lower_logic_path_expr<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    lowerer: &crate::SLTToSIRLowerer,
    builder: &mut SIRBuilder<Addr>,
    path: &LogicPath<Addr>,
    arena: &SLTNodeArena<Addr>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
) -> RegisterId {
    if path.local_inputs.is_empty() {
        return lowerer.lower(builder, path.expr, arena, lower_cache);
    }

    let mut env_inputs = HashMap::default();
    for (addr, node) in &path.local_inputs {
        let reg = lowerer.lower(builder, *node, arena, lower_cache);
        let width = crate::get_width(*node, arena);
        if width > 0 {
            env_inputs.insert(VarAtomBase::new(*addr, 0, width - 1), reg);
        }
    }
    lowerer.lower_with_inputs(builder, path.expr, arena, lower_cache, env_inputs)
}

fn lower_logic_path_node<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    lowerer: &crate::SLTToSIRLowerer,
    builder: &mut SIRBuilder<Addr>,
    path: &LogicPath<Addr>,
    node: NodeId,
    arena: &SLTNodeArena<Addr>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
) -> RegisterId {
    if path.local_inputs.is_empty() {
        return lowerer.lower(builder, node, arena, lower_cache);
    }
    // Unbound captures may already have been materialized before a later
    // write. Do not rebuild them under the observer's unrelated local inputs.
    if matches!(arena.get(node), crate::SLTNode::Capture { .. })
        && let Some(reg) = lower_cache.get(&node)
    {
        return *reg;
    }
    let mut env_inputs = HashMap::default();
    for (addr, local_node) in &path.local_inputs {
        let reg = lowerer.lower(builder, *local_node, arena, lower_cache);
        let width = crate::get_width(*local_node, arena);
        if width > 0 {
            env_inputs.insert(VarAtomBase::new(*addr, 0, width - 1), reg);
        }
    }
    lowerer.lower_with_inputs(builder, node, arena, lower_cache, env_inputs)
}

fn pre_lower_logic_path_node<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    lowerer: &crate::SLTToSIRLowerer,
    builder: &mut SIRBuilder<Addr>,
    path: &LogicPath<Addr>,
    node: NodeId,
    arena: &SLTNodeArena<Addr>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
) {
    if path.local_inputs.is_empty() {
        lowerer.lower(builder, node, arena, lower_cache);
    }
}

fn static_access_offset<Addr: Eq + Hash>(
    addr: &Addr,
    access: BitAccess,
    unpacked_element_widths: &HashMap<Addr, usize>,
) -> SIROffset {
    let width = access.msb - access.lsb + 1;
    match unpacked_element_widths.get(addr).copied() {
        Some(element_width)
            if element_width != 0
                && width > element_width
                && access.lsb.is_multiple_of(element_width)
                && width.is_multiple_of(element_width) =>
        {
            SIROffset::PackedElements {
                bit_offset: access.lsb,
                element_width,
            }
        }
        _ => SIROffset::Static(access.lsb),
    }
}

fn emit_logic_path_store_with_result<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    lowerer: &crate::SLTToSIRLowerer,
    builder: &mut SIRBuilder<Addr>,
    path: &LogicPath<Addr>,
    arena: &SLTNodeArena<Addr>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
    unpacked_element_widths: &HashMap<Addr, usize>,
    prepared_result: Option<RegisterId>,
) {
    match &path.target {
        LogicPathTarget::Var(target) => {
            for node in &path.pre_lower_nodes {
                pre_lower_logic_path_node(lowerer, builder, path, *node, arena, lower_cache);
            }
            let result_reg = match prepared_result {
                Some(result) => result,
                None => lower_logic_path_expr(lowerer, builder, path, arena, lower_cache),
            };
            let width = 1 + target.access.msb - target.access.lsb;
            let offset = static_access_offset(&target.id, target.access, unpacked_element_widths);
            let old_reg =
                if path.comb_capture_enable_sites.is_empty() || path.comb_capture_enable_always {
                    None
                } else {
                    let old_reg = builder.alloc_bit(width, false);
                    builder.emit(SIRInstruction::Load(
                        old_reg,
                        target.id,
                        offset.clone(),
                        width,
                    ));
                    Some(old_reg)
                };
            builder.emit(SIRInstruction::Store(
                target.id,
                offset,
                width,
                result_reg,
                Vec::new(),
                Vec::new(),
            ));
            if !path.comb_capture_enable_sites.is_empty() {
                let (old, new) = if path.comb_capture_enable_always {
                    let old = builder.alloc_bit(1, false);
                    let new = builder.alloc_bit(1, false);
                    builder.emit(SIRInstruction::Imm(old, SIRValue::new(0u8)));
                    builder.emit(SIRInstruction::Imm(new, SIRValue::new(1u8)));
                    (old, new)
                } else {
                    (
                        old_reg.expect("changed capture enable loads the old value"),
                        result_reg,
                    )
                };
                builder.emit(SIRInstruction::CombCaptureEnableIfChanged {
                    old,
                    new,
                    sites: path.comb_capture_enable_sites.clone(),
                });
            }
        }
        LogicPathTarget::Process { unit, .. } => {
            // The unit can write every output of its process, not only this
            // path's scheduling target. Do not reuse values across the call.
            lower_cache.clear();
            builder.inline_execution_unit(unit);
            lower_cache.clear();
        }
        LogicPathTarget::CombCaptureEvent {
            site_id,
            guard,
            emit_on_true,
            args,
            loop_runner,
            fatal_error_code,
            consume_enabled,
        } => {
            debug_assert!(prepared_result.is_none());
            if let Some(loop_runner) = loop_runner {
                lower_logic_path_node(lowerer, builder, path, *loop_runner, arena, lower_cache);
                return;
            }
            let emit = |builder: &mut SIRBuilder<Addr>,
                        lower_cache: &mut HashMap<NodeId, RegisterId>| {
                let regs = args
                    .iter()
                    .map(|arg| {
                        lower_logic_path_node(lowerer, builder, path, *arg, arena, lower_cache)
                    })
                    .collect();
                builder.emit(SIRInstruction::CombCaptureEvent {
                    site_id: *site_id,
                    args: regs,
                    fatal_error_code: *fatal_error_code,
                    consume_enabled: *consume_enabled,
                });
            };
            if let Some(guard) = guard {
                let cond =
                    lower_logic_path_node(lowerer, builder, path, *guard, arena, lower_cache);
                let branch_cond = if *emit_on_true {
                    cond
                } else {
                    let inverted = builder.alloc_bit(1, false);
                    builder.emit(SIRInstruction::Unary(inverted, UnaryOp::LogicNot, cond));
                    inverted
                };
                let event_block = builder.new_block();
                let done_block = builder.new_block();
                builder.seal_block(SIRTerminator::Branch {
                    cond: branch_cond,
                    true_block: (event_block, vec![]),
                    false_block: (done_block, vec![]),
                });
                builder.switch_to_block(event_block);
                // Values first lowered inside the guarded block do not
                // dominate the join; later paths must not reuse them.
                let dominating_cache = lower_cache.clone();
                emit(builder, lower_cache);
                *lower_cache = dominating_cache;
                builder.seal_block(SIRTerminator::Jump(done_block, vec![]));
                builder.switch_to_block(done_block);
            } else {
                emit(builder, lower_cache);
            }
        }
    }
}

fn emit_logic_path_store<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    lowerer: &crate::SLTToSIRLowerer,
    builder: &mut SIRBuilder<Addr>,
    path: &LogicPath<Addr>,
    arena: &SLTNodeArena<Addr>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
    unpacked_element_widths: &HashMap<Addr, usize>,
) {
    emit_logic_path_store_with_result(
        lowerer,
        builder,
        path,
        arena,
        lower_cache,
        unpacked_element_widths,
        None,
    );
}

fn invalidate_logic_path_target<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    path: &LogicPath<Addr>,
    inverse_dep_memo: &HashMap<Addr, HashSet<NodeId>>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
) {
    let Some(target) = path.target.var() else {
        return;
    };
    if let Some(to_remove) = inverse_dep_memo.get(&target.id) {
        for node in to_remove {
            if !path.pre_lower_nodes.contains(node) {
                lower_cache.remove(node);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_scheduled_guard_region<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    lowerer: &crate::SLTToSIRLowerer,
    builder: &mut SIRBuilder<Addr>,
    condition: NodeId,
    paths: &[usize],
    input: &[LogicPath<Addr>],
    arena: &SLTNodeArena<Addr>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
    dep_memo: &mut HashMap<NodeId, HashSet<Addr>>,
    inverse_dep_memo: &mut HashMap<Addr, HashSet<NodeId>>,
    unpacked_element_widths: &HashMap<Addr, usize>,
) {
    for &path in paths {
        collect_logic_path_input_deps(&input[path], arena, dep_memo, inverse_dep_memo);
    }

    let condition_value = lowerer.lower(builder, condition, arena, lower_cache);
    let then_block = builder.new_block();
    let else_block = builder.new_block();
    let merge_block = builder.new_block();
    builder.seal_block(SIRTerminator::Branch {
        cond: condition_value,
        true_block: (then_block, Vec::new()),
        false_block: (else_block, Vec::new()),
    });

    let mut affected = paths
        .iter()
        .filter_map(|path| input[*path].target.var())
        .filter_map(|target| inverse_dep_memo.get(&target.id))
        .flatten()
        .copied()
        .collect::<Vec<_>>();
    affected.sort_unstable();
    affected.dedup();
    let saved_cache = affected
        .iter()
        .filter_map(|node| lower_cache.get(node).copied().map(|value| (*node, value)))
        .collect::<Vec<_>>();

    let emit_arm = |builder: &mut SIRBuilder<Addr>,
                    lower_cache: &mut HashMap<NodeId, RegisterId>,
                    take_true: bool| {
        let mut inserted = Vec::new();
        for &path in paths {
            let (_, then_expr, else_expr) = scheduled_root_mux(&input[path], arena)
                .expect("scheduled guard region must retain root Muxes");
            let root = if take_true { then_expr } else { else_expr };
            // Keep the existing per-LogicPath lowering model for nested
            // decisions. The scheduled region owns only the common outer
            // guard; changing fanout/cost facts for all nested Muxes at once
            // would silently replace the established branch decisions.
            let value = lowerer.lower(builder, root, arena, lower_cache);
            inserted.extend(lowerer.take_scheduled_region_insertions());
            emit_logic_path_store_with_result(
                lowerer,
                builder,
                &input[path],
                arena,
                lower_cache,
                unpacked_element_widths,
                Some(value),
            );
            invalidate_logic_path_target(&input[path], inverse_dep_memo, lower_cache);
        }
        builder.seal_block(SIRTerminator::Jump(merge_block, Vec::new()));
        for node in inserted {
            lower_cache.remove(&node);
        }
        for node in &affected {
            lower_cache.remove(node);
        }
        lower_cache.extend(saved_cache.iter().copied());
    };

    builder.switch_to_block(then_block);
    emit_arm(builder, lower_cache, true);
    builder.switch_to_block(else_block);
    emit_arm(builder, lower_cache, false);
    builder.switch_to_block(merge_block);

    // Both arms publish every path target, so cached expressions reading any
    // such target are invalid at the merge even when both arm caches happened
    // to contain the same SLT node identity.
    for node in affected {
        lower_cache.remove(&node);
    }
}

fn projected_for_fold_group<Addr: Clone + Eq + Hash>(
    mut node: NodeId,
    arena: &SLTNodeArena<Addr>,
) -> Option<NodeId> {
    loop {
        match arena.get(node) {
            SLTNode::ForFoldGroup { .. } => return Some(node),
            SLTNode::Slice { expr, .. } => node = *expr,
            _ => return None,
        }
    }
}

fn fold_group_projection_access<Addr: Clone + Eq + Hash>(
    node: NodeId,
    group: NodeId,
    arena: &SLTNodeArena<Addr>,
) -> Option<BitAccess> {
    if node == group {
        let width = crate::get_width(group, arena);
        return (width != 0).then(|| BitAccess::new(0, width - 1));
    }
    let SLTNode::Slice { expr, access } = arena.get(node) else {
        return None;
    };
    let parent = fold_group_projection_access(*expr, group, arena)?;
    let parent_width = parent.msb.checked_sub(parent.lsb)?.checked_add(1)?;
    if access.msb >= parent_width {
        return None;
    }
    Some(BitAccess::new(
        parent.lsb.checked_add(access.lsb)?,
        parent.lsb.checked_add(access.msb)?,
    ))
}

fn packed_fold_group_state_accesses<Addr: Clone + Eq + Hash>(
    states: &[crate::SLTForFoldGroupState<Addr>],
) -> Option<Vec<BitAccess>> {
    let total_width = states.iter().try_fold(0usize, |total, state| {
        let width = state
            .target
            .access
            .msb
            .checked_sub(state.target.access.lsb)?
            .checked_add(1)?;
        total.checked_add(width)
    })?;
    if total_width == 0 {
        return None;
    }

    let mut next_msb = total_width;
    let mut result = Vec::with_capacity(states.len());
    for state in states {
        let width = state.target.access.msb - state.target.access.lsb + 1;
        next_msb = next_msb.checked_sub(width)?;
        result.push(BitAccess::new(next_msb, next_msb + width - 1));
    }
    Some(result)
}

fn push_scheduler_node_children<Addr: Clone + Eq + Hash>(
    node: NodeId,
    arena: &SLTNodeArena<Addr>,
    work: &mut Vec<NodeId>,
) {
    match arena.get(node) {
        SLTNode::Input { index, .. } => work.extend(index.iter().map(|entry| entry.node)),
        SLTNode::Constant(..) => {}
        SLTNode::Binary(lhs, _, rhs) => {
            work.push(*lhs);
            work.push(*rhs);
        }
        SLTNode::Unary(_, inner) | SLTNode::Capture { expr: inner, .. } => work.push(*inner),
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => {
            work.push(*cond);
            work.push(*then_expr);
            work.push(*else_expr);
        }
        SLTNode::Concat(parts) => work.extend(parts.iter().map(|(part, _)| *part)),
        SLTNode::Slice { expr, .. } => work.push(*expr),
        SLTNode::ForFold {
            start,
            end,
            result,
            initials,
            updates,
            effects,
            continue_cond,
            ..
        } => {
            if let crate::SLTLoopBound::Expr(node) | crate::SLTLoopBound::TypedExpr { node, .. } =
                start
            {
                work.push(*node);
            }
            if let crate::SLTLoopBound::Expr(node) | crate::SLTLoopBound::TypedExpr { node, .. } =
                end
            {
                work.push(*node);
            }
            if let crate::SLTForFoldResult::Transient { initial, update } = result {
                work.push(*initial);
                work.push(*update);
            }
            work.extend(initials.iter().map(|state| state.expr));
            work.extend(updates.iter().map(|state| state.expr));
            for effect in effects {
                match effect {
                    crate::SLTForEffect::Event { guard, args, .. } => {
                        work.extend(*guard);
                        work.extend(args.iter().copied());
                    }
                    crate::SLTForEffect::Runner(runner) => work.push(*runner),
                }
            }
            work.push(*continue_cond);
        }
        SLTNode::ForFoldGroup {
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
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum NormalizedIndexExpr<Addr: Clone + Eq + Hash> {
    LoopValue {
        signed: bool,
        access: BitAccess,
    },
    Input {
        variable: Addr,
        signed: bool,
        access: BitAccess,
        index: Vec<(NormalizedIndexExpr<Addr>, usize)>,
    },
    Constant(num_bigint::BigUint, num_bigint::BigUint, usize, bool),
    Binary(
        Box<NormalizedIndexExpr<Addr>>,
        BinaryOp,
        Box<NormalizedIndexExpr<Addr>>,
    ),
    Unary(UnaryOp, Box<NormalizedIndexExpr<Addr>>),
    Mux {
        cond: Box<NormalizedIndexExpr<Addr>>,
        then_expr: Box<NormalizedIndexExpr<Addr>>,
        else_expr: Box<NormalizedIndexExpr<Addr>>,
    },
    Concat(Vec<(NormalizedIndexExpr<Addr>, usize)>),
    Slice(Box<NormalizedIndexExpr<Addr>>, BitAccess),
}

impl<Addr: Clone + Eq + Hash> NormalizedIndexExpr<Addr> {
    fn contains_loop_value(&self) -> bool {
        match self {
            Self::LoopValue { .. } => true,
            Self::Input { index, .. } | Self::Concat(index) => {
                index.iter().any(|(expr, _)| expr.contains_loop_value())
            }
            Self::Constant(..) => false,
            Self::Binary(lhs, _, rhs) => lhs.contains_loop_value() || rhs.contains_loop_value(),
            Self::Unary(_, inner) | Self::Slice(inner, _) => inner.contains_loop_value(),
            Self::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                cond.contains_loop_value()
                    || then_expr.contains_loop_value()
                    || else_expr.contains_loop_value()
            }
        }
    }

    fn operation_cost(&self) -> u128 {
        match self {
            Self::LoopValue { .. } | Self::Constant(..) => 0,
            Self::Input { index, .. } => 4u128.saturating_add(
                index
                    .iter()
                    .map(|(expr, _)| 2u128.saturating_add(expr.operation_cost()))
                    .sum(),
            ),
            Self::Binary(lhs, _, rhs) => 1u128
                .saturating_add(lhs.operation_cost())
                .saturating_add(rhs.operation_cost()),
            Self::Unary(_, inner) | Self::Slice(inner, _) => {
                1u128.saturating_add(inner.operation_cost())
            }
            Self::Mux {
                cond,
                then_expr,
                else_expr,
            } => 1u128
                .saturating_add(cond.operation_cost())
                .saturating_add(then_expr.operation_cost())
                .saturating_add(else_expr.operation_cost()),
            Self::Concat(parts) => parts.iter().fold(1u128, |cost, (part, _)| {
                cost.saturating_add(part.operation_cost())
            }),
        }
    }
}

fn normalize_index_expr<Addr: Clone + Eq + Hash + Copy>(
    node: NodeId,
    loop_var: Addr,
    arena: &SLTNodeArena<Addr>,
    memo: &mut HashMap<NodeId, Option<NormalizedIndexExpr<Addr>>>,
) -> Option<NormalizedIndexExpr<Addr>> {
    if let Some(found) = memo.get(&node) {
        return found.clone();
    }
    let normalized = match arena.get(node) {
        SLTNode::Input {
            variable,
            signed,
            index,
            access,
        } if *variable == loop_var => {
            if !index.is_empty() {
                None
            } else {
                Some(NormalizedIndexExpr::LoopValue {
                    signed: *signed,
                    access: *access,
                })
            }
        }
        SLTNode::Input {
            variable,
            signed,
            index,
            access,
        } => Some(NormalizedIndexExpr::Input {
            variable: *variable,
            signed: *signed,
            access: *access,
            index: index
                .iter()
                .map(|entry| {
                    normalize_index_expr(entry.node, loop_var, arena, memo)
                        .map(|node| (node, entry.stride))
                })
                .collect::<Option<Vec<_>>>()?,
        }),
        SLTNode::Constant(payload, mask, width, signed) => Some(NormalizedIndexExpr::Constant(
            payload.clone(),
            mask.clone(),
            *width,
            *signed,
        )),
        SLTNode::Binary(lhs, op, rhs) => Some(NormalizedIndexExpr::Binary(
            Box::new(normalize_index_expr(*lhs, loop_var, arena, memo)?),
            *op,
            Box::new(normalize_index_expr(*rhs, loop_var, arena, memo)?),
        )),
        SLTNode::Unary(op, inner) => Some(NormalizedIndexExpr::Unary(
            *op,
            Box::new(normalize_index_expr(*inner, loop_var, arena, memo)?),
        )),
        SLTNode::Capture { expr, .. } => normalize_index_expr(*expr, loop_var, arena, memo),
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => Some(NormalizedIndexExpr::Mux {
            cond: Box::new(normalize_index_expr(*cond, loop_var, arena, memo)?),
            then_expr: Box::new(normalize_index_expr(*then_expr, loop_var, arena, memo)?),
            else_expr: Box::new(normalize_index_expr(*else_expr, loop_var, arena, memo)?),
        }),
        SLTNode::Concat(parts) => Some(NormalizedIndexExpr::Concat(
            parts
                .iter()
                .map(|(part, width)| {
                    normalize_index_expr(*part, loop_var, arena, memo).map(|part| (part, *width))
                })
                .collect::<Option<Vec<_>>>()?,
        )),
        SLTNode::Slice { expr, access } => Some(NormalizedIndexExpr::Slice(
            Box::new(normalize_index_expr(*expr, loop_var, arena, memo)?),
            *access,
        )),
        SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => None,
    };
    memo.insert(node, normalized.clone());
    normalized
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ExactIndexedLoadKey<Addr: Clone + Eq + Hash> {
    base: Addr,
    access: BitAccess,
    index: Vec<(NormalizedIndexExpr<Addr>, usize)>,
}

impl<Addr: Clone + Eq + Hash> ExactIndexedLoadKey<Addr> {
    fn saved_runtime_cost(&self) -> u128 {
        let width = self.access.msb - self.access.lsb + 1;
        let chunks = width.div_ceil(64) as u128;
        let address_cost = self.index.iter().fold(1u128, |cost, (expr, _)| {
            cost.saturating_add(2).saturating_add(expr.operation_cost())
        });
        6u128.saturating_mul(chunks).saturating_add(address_cost)
    }
}

#[derive(Clone)]
struct FoldGroupReadFacts<Addr: Clone + Eq + Hash> {
    loop_var: Addr,
    state_targets: Vec<VarAtomBase<Addr>>,
    guard_reads: Vec<SchedulerInputRead<Addr>>,
    initial_reads: Vec<SchedulerInputRead<Addr>>,
    update_reads: Vec<SchedulerInputRead<Addr>>,
    indexed_loads: HashSet<ExactIndexedLoadKey<Addr>>,
    carried_chunks: u128,
}

#[derive(Clone)]
struct ExactFoldGroup<Addr: Clone + Eq + Hash> {
    root: NodeId,
    facts: FoldGroupReadFacts<Addr>,
}

struct FoldGroupScheduleInfo<Addr: Clone + Eq + Hash> {
    projection_paths: Vec<usize>,
    read_facts: Option<FoldGroupReadFacts<Addr>>,
    exact_and_exclusive: bool,
}

struct FoldGroupScheduleIndex<Addr: Clone + Eq + Hash> {
    groups: BTreeMap<NodeId, FoldGroupScheduleInfo<Addr>>,
    direct_group_by_path: Vec<Option<NodeId>>,
}

fn collect_reachable_scheduled_groups<Addr: Clone + Eq + Hash>(
    root: NodeId,
    arena: &SLTNodeArena<Addr>,
    reaches_scheduled_group: &[bool],
    scheduled_groups: &HashSet<NodeId>,
    result: &mut HashSet<NodeId>,
) {
    if !reaches_scheduled_group[root.0] {
        return;
    }
    let mut visited = HashSet::default();
    let mut work = vec![root];
    let mut children = Vec::new();
    while let Some(node) = work.pop() {
        if !visited.insert(node) || !reaches_scheduled_group[node.0] {
            continue;
        }
        if scheduled_groups.contains(&node) {
            result.insert(node);
        }
        children.clear();
        push_scheduler_node_children(node, arena, &mut children);
        work.extend(
            children
                .iter()
                .copied()
                .filter(|child| reaches_scheduled_group[child.0]),
        );
    }
}

fn exact_fold_group_paths<Addr: Clone + Eq + Hash + Copy>(
    group: NodeId,
    paths: &[usize],
    input: &[LogicPath<Addr>],
    arena: &SLTNodeArena<Addr>,
) -> bool {
    let SLTNode::ForFoldGroup { states, .. } = arena.get(group) else {
        return false;
    };
    let Some(packed_accesses) = packed_fold_group_state_accesses(states) else {
        return false;
    };
    let mut covered = vec![Vec::<BitAccess>::new(); states.len()];

    for &path_index in paths {
        let path = &input[path_index];
        if !path.local_inputs.is_empty() || !path.pre_lower_nodes.is_empty() {
            return false;
        }
        let Some(target) = path.target.var() else {
            return false;
        };
        let Some(projection) = fold_group_projection_access(path.expr, group, arena) else {
            return false;
        };
        let matches = states
            .iter()
            .zip(&packed_accesses)
            .enumerate()
            .filter_map(|(state_index, (state, packed))| {
                if target.id != state.target.id
                    || target.access.lsb < state.target.access.lsb
                    || target.access.msb > state.target.access.msb
                {
                    return None;
                }
                let relative_lsb = target.access.lsb - state.target.access.lsb;
                let relative_msb = target.access.msb - state.target.access.lsb;
                let expected = BitAccess::new(
                    packed.lsb.checked_add(relative_lsb)?,
                    packed.lsb.checked_add(relative_msb)?,
                );
                (projection == expected).then_some(state_index)
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return false;
        }
        covered[matches[0]].push(target.access);
    }

    states.iter().zip(&mut covered).all(|(state, ranges)| {
        ranges.sort_unstable_by_key(|range| (range.lsb, range.msb));
        let mut next = state.target.access.lsb;
        for range in ranges.iter() {
            if range.lsb != next || range.msb > state.target.access.msb {
                return false;
            }
            let Some(after) = range.msb.checked_add(1) else {
                return range.msb == state.target.access.msb;
            };
            next = after;
        }
        next == state.target.access.msb.saturating_add(1)
    })
}

fn build_fold_group_schedule_index<Addr: Clone + Eq + Ord + Hash + Copy>(
    input: &[LogicPath<Addr>],
    arena: &SLTNodeArena<Addr>,
) -> FoldGroupScheduleIndex<Addr> {
    let direct_group_by_path = input
        .iter()
        .map(|path| projected_for_fold_group(path.expr, arena))
        .collect::<Vec<_>>();
    let direct_roots = direct_group_by_path
        .iter()
        .flatten()
        .copied()
        .collect::<HashSet<_>>();
    if direct_roots.is_empty() {
        return FoldGroupScheduleIndex {
            groups: BTreeMap::new(),
            direct_group_by_path,
        };
    }

    // SLT children always precede their owners. Compute one boolean per node,
    // then traverse only semantic roots that can actually reach a scheduled
    // fold. This avoids carrying a root set through the entire arena.
    let mut reaches_scheduled_group = Vec::<bool>::with_capacity(arena.len());
    let mut children = Vec::new();
    for raw in 0..arena.len() {
        let node = NodeId(raw);
        children.clear();
        push_scheduler_node_children(node, arena, &mut children);
        reaches_scheduled_group.push(
            direct_roots.contains(&node)
                || children
                    .iter()
                    .any(|child| reaches_scheduled_group[child.0]),
        );
    }

    let mut groups = BTreeMap::<NodeId, FoldGroupScheduleInfo<Addr>>::new();
    for (path_index, path) in input.iter().enumerate() {
        let direct = direct_group_by_path[path_index];
        if let Some(root) = direct {
            groups
                .entry(root)
                .or_insert_with(|| FoldGroupScheduleInfo {
                    projection_paths: Vec::new(),
                    read_facts: None,
                    exact_and_exclusive: true,
                })
                .projection_paths
                .push(path_index);
        }

        let mut reached = HashSet::default();
        collect_reachable_scheduled_groups(
            path.expr,
            arena,
            &reaches_scheduled_group,
            &direct_roots,
            &mut reached,
        );
        for root in reached.drain() {
            let info = groups.entry(root).or_insert_with(|| FoldGroupScheduleInfo {
                projection_paths: Vec::new(),
                read_facts: None,
                exact_and_exclusive: true,
            });
            if direct != Some(root) {
                info.exact_and_exclusive = false;
            }
        }
        let auxiliary_roots = path
            .local_inputs
            .iter()
            .map(|(_, node)| *node)
            .chain(path.pre_lower_nodes.iter().copied())
            .chain(match &path.target {
                LogicPathTarget::Var(_) | LogicPathTarget::Process { .. } => Vec::new(),
                LogicPathTarget::CombCaptureEvent {
                    guard,
                    args,
                    loop_runner,
                    ..
                } => guard
                    .iter()
                    .chain(args)
                    .chain(loop_runner)
                    .copied()
                    .collect(),
            });
        for node in auxiliary_roots {
            reached.clear();
            collect_reachable_scheduled_groups(
                node,
                arena,
                &reaches_scheduled_group,
                &direct_roots,
                &mut reached,
            );
            for root in reached.drain() {
                groups
                    .entry(root)
                    .or_insert_with(|| FoldGroupScheduleInfo {
                        projection_paths: Vec::new(),
                        read_facts: None,
                        exact_and_exclusive: true,
                    })
                    .exact_and_exclusive = false;
            }
        }
    }

    for (&root, info) in &mut groups {
        info.exact_and_exclusive &= !info.projection_paths.is_empty()
            && exact_fold_group_paths(root, &info.projection_paths, input, arena);
        if info.exact_and_exclusive {
            info.read_facts = collect_fold_group_read_facts(root, arena);
            info.exact_and_exclusive &= info.read_facts.is_some();
        }
    }

    FoldGroupScheduleIndex {
        groups,
        direct_group_by_path,
    }
}

fn discover_exact_fold_groups<Addr: Clone + Eq + Ord + Hash + Copy>(
    indices: &[usize],
    schedule_index: &FoldGroupScheduleIndex<Addr>,
) -> Vec<ExactFoldGroup<Addr>> {
    let scheduled_indices = indices.iter().copied().collect::<HashSet<_>>();
    let mut roots = indices
        .iter()
        .filter_map(|&index| schedule_index.direct_group_by_path[index])
        .collect::<Vec<_>>();
    roots.sort_unstable();
    roots.dedup();

    roots
        .into_iter()
        .filter_map(|root| {
            let info = schedule_index.groups.get(&root)?;
            if !info.exact_and_exclusive
                || info
                    .projection_paths
                    .iter()
                    .any(|index| !scheduled_indices.contains(index))
            {
                return None;
            }
            Some(ExactFoldGroup {
                root,
                facts: info.read_facts.clone()?,
            })
        })
        .collect()
}

fn same_fold_group_domain<Addr: Clone + Eq + Hash>(
    lhs: NodeId,
    rhs: NodeId,
    arena: &SLTNodeArena<Addr>,
) -> bool {
    let SLTNode::ForFoldGroup {
        loop_width: lhs_width,
        loop_signed: lhs_signed,
        start: lhs_start,
        step: lhs_step,
        trip_count: lhs_count,
        entry_guard: lhs_guard,
        ..
    } = arena.get(lhs)
    else {
        return false;
    };
    let SLTNode::ForFoldGroup {
        loop_width: rhs_width,
        loop_signed: rhs_signed,
        start: rhs_start,
        step: rhs_step,
        trip_count: rhs_count,
        entry_guard: rhs_guard,
        ..
    } = arena.get(rhs)
    else {
        return false;
    };
    lhs_width == rhs_width
        && lhs_signed == rhs_signed
        && lhs_start == rhs_start
        && lhs_step == rhs_step
        && lhs_count == rhs_count
        && lhs_guard == rhs_guard
}

#[derive(Clone, Copy)]
struct SchedulerInputRead<Addr> {
    id: Addr,
    access: BitAccess,
    indexed: bool,
}

fn collect_scheduler_plain_reads<Addr: Clone + Eq + Hash + Copy>(
    root: NodeId,
    arena: &SLTNodeArena<Addr>,
    reads: &mut Vec<SchedulerInputRead<Addr>>,
) -> bool {
    let mut visited = HashSet::default();
    let mut work = vec![root];
    while let Some(node) = work.pop() {
        if !visited.insert(node) {
            continue;
        }
        match arena.get(node) {
            SLTNode::Input {
                variable,
                index,
                access,
                ..
            } => {
                reads.push(SchedulerInputRead {
                    id: *variable,
                    access: *access,
                    indexed: !index.is_empty(),
                });
                work.extend(index.iter().map(|entry| entry.node));
            }
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => return false,
            _ => push_scheduler_node_children(node, arena, &mut work),
        }
    }
    true
}

fn collect_fold_group_read_facts<Addr: Clone + Eq + Hash + Copy>(
    root: NodeId,
    arena: &SLTNodeArena<Addr>,
) -> Option<FoldGroupReadFacts<Addr>> {
    let SLTNode::ForFoldGroup {
        loop_var,
        entry_guard,
        states,
        ..
    } = arena.get(root)
    else {
        return None;
    };

    let mut guard_reads = Vec::new();
    collect_scheduler_plain_reads(*entry_guard, arena, &mut guard_reads).then_some(())?;
    let mut initial_reads = Vec::new();
    let mut update_reads = Vec::new();
    for state in states {
        collect_scheduler_plain_reads(state.initial, arena, &mut initial_reads).then_some(())?;
        collect_scheduler_plain_reads(state.update, arena, &mut update_reads).then_some(())?;
    }

    let state_ids = states
        .iter()
        .map(|state| state.target.id)
        .collect::<HashSet<_>>();
    let mut indexed_loads = HashSet::default();
    let mut normalize_memo = HashMap::default();
    let mut visited = HashSet::default();
    let mut work = states.iter().map(|state| state.update).collect::<Vec<_>>();
    while let Some(node) = work.pop() {
        if !visited.insert(node) {
            continue;
        }
        match arena.get(node) {
            SLTNode::Input {
                variable,
                index,
                access,
                ..
            } => {
                if *variable != *loop_var && !state_ids.contains(variable) && !index.is_empty() {
                    let normalized = index
                        .iter()
                        .map(|entry| {
                            normalize_index_expr(entry.node, *loop_var, arena, &mut normalize_memo)
                                .map(|node| (node, entry.stride))
                        })
                        .collect::<Option<Vec<_>>>();
                    if let Some(normalized) = normalized
                        && normalized
                            .iter()
                            .any(|(expr, _)| expr.contains_loop_value())
                    {
                        indexed_loads.insert(ExactIndexedLoadKey {
                            base: *variable,
                            access: *access,
                            index: normalized,
                        });
                    }
                }
                work.extend(index.iter().map(|entry| entry.node));
            }
            _ => push_scheduler_node_children(node, arena, &mut work),
        }
    }

    let carried_chunks = states.iter().fold(0u128, |chunks, state| {
        let width = state.target.access.msb - state.target.access.lsb + 1;
        chunks.saturating_add(width.div_ceil(64) as u128)
    });
    let mut state_ranges: HashMap<Addr, Vec<BitAccess>> = HashMap::default();
    for state in states {
        if state.target.id == *loop_var
            || state_ranges
                .entry(state.target.id)
                .or_default()
                .iter()
                .any(|range| range.overlaps(&state.target.access))
        {
            return None;
        }
        state_ranges
            .entry(state.target.id)
            .or_default()
            .push(state.target.access);
    }
    if guard_reads.iter().any(|read| read.id == *loop_var)
        || initial_reads.iter().any(|read| read.id == *loop_var)
        || update_reads
            .iter()
            .any(|read| read.id == *loop_var && read.indexed)
    {
        return None;
    }
    Some(FoldGroupReadFacts {
        loop_var: *loop_var,
        state_targets: states.iter().map(|state| state.target).collect(),
        guard_reads,
        initial_reads,
        update_reads,
        indexed_loads,
        carried_chunks,
    })
}

fn scheduler_read_overlaps_targets<Addr: Clone + Eq + Hash>(
    read: &SchedulerInputRead<Addr>,
    targets: &[VarAtomBase<Addr>],
) -> bool {
    targets.iter().any(|target| {
        target.id == read.id && (read.indexed || target.access.overlaps(&read.access))
    })
}

fn fold_groups_are_pairwise_independent<Addr: Clone + Eq + Hash + Copy>(
    lhs: &FoldGroupReadFacts<Addr>,
    rhs: &FoldGroupReadFacts<Addr>,
) -> bool {
    if lhs.loop_var == rhs.loop_var
        || lhs
            .state_targets
            .iter()
            .any(|target| target.id == rhs.loop_var)
        || rhs
            .state_targets
            .iter()
            .any(|target| target.id == lhs.loop_var)
        || lhs.state_targets.iter().any(|left| {
            rhs.state_targets
                .iter()
                .any(|right| left.id == right.id && left.access.overlaps(&right.access))
        })
    {
        return false;
    }

    let all_targets = lhs
        .state_targets
        .iter()
        .chain(&rhs.state_targets)
        .copied()
        .collect::<Vec<_>>();
    let guard_is_independent = lhs.guard_reads.iter().chain(&rhs.guard_reads).all(|read| {
        read.id != lhs.loop_var
            && read.id != rhs.loop_var
            && !scheduler_read_overlaps_targets(read, &all_targets)
    });
    let initials_are_independent = lhs
        .initial_reads
        .iter()
        .chain(&rhs.initial_reads)
        .all(|read| read.id != lhs.loop_var && read.id != rhs.loop_var);
    let lhs_update_is_independent = lhs.update_reads.iter().all(|read| {
        read.id != rhs.loop_var && !scheduler_read_overlaps_targets(read, &rhs.state_targets)
    });
    let rhs_update_is_independent = rhs.update_reads.iter().all(|read| {
        read.id != lhs.loop_var && !scheduler_read_overlaps_targets(read, &lhs.state_targets)
    });
    guard_is_independent
        && initials_are_independent
        && lhs_update_is_independent
        && rhs_update_is_independent
}

fn fold_groups_share_exact_load<Addr: Clone + Eq + Hash>(
    lhs: &FoldGroupReadFacts<Addr>,
    rhs: &FoldGroupReadFacts<Addr>,
) -> bool {
    let (small, large) = if lhs.indexed_loads.len() <= rhs.indexed_loads.len() {
        (&lhs.indexed_loads, &rhs.indexed_loads)
    } else {
        (&rhs.indexed_loads, &lhs.indexed_loads)
    };
    small.iter().any(|key| large.contains(key))
}

#[derive(Clone)]
struct WeightedFoldFamily {
    members: Vec<usize>,
    benefit: u128,
    pressure: u128,
}

impl WeightedFoldFamily {
    fn is_positive(&self) -> bool {
        self.members.len() >= 2 && self.benefit > self.pressure
    }

    fn cmp_net(&self, other: &Self) -> std::cmp::Ordering {
        self.benefit
            .saturating_add(other.pressure)
            .cmp(&other.benefit.saturating_add(self.pressure))
    }
}

fn weighted_fold_family<Addr: Clone + Eq + Hash>(
    members: &[usize],
    candidates: &[ExactFoldGroup<Addr>],
    four_state: bool,
) -> WeightedFoldFamily {
    const SAVED_LOOP_CONTROL_COST: u128 = 6;
    const CARRIED_CHUNK_PRESSURE_COST: u128 = 4;

    let mut users_by_load = HashMap::<ExactIndexedLoadKey<Addr>, usize>::default();
    let mut total_chunks = 0u128;
    let mut largest_separate_group = 0u128;
    for &member in members {
        let facts = &candidates[member].facts;
        total_chunks = total_chunks.saturating_add(facts.carried_chunks);
        largest_separate_group = largest_separate_group.max(facts.carried_chunks);
        for key in &facts.indexed_loads {
            *users_by_load.entry(key.clone()).or_insert(0) += 1;
        }
    }
    let load_benefit = users_by_load
        .into_iter()
        .filter(|(_, users)| *users >= 2)
        .fold(0u128, |benefit, (key, users)| {
            benefit.saturating_add(key.saved_runtime_cost().saturating_mul((users - 1) as u128))
        });
    let control_benefit =
        SAVED_LOOP_CONTROL_COST.saturating_mul(members.len().saturating_sub(1) as u128);
    let state_multiplier = if four_state { 2 } else { 1 };
    let pressure = total_chunks
        .saturating_sub(largest_separate_group)
        .saturating_mul(state_multiplier)
        .saturating_mul(CARRIED_CHUNK_PRESSURE_COST);
    WeightedFoldFamily {
        members: members.to_vec(),
        benefit: load_benefit.saturating_add(control_benefit),
        pressure,
    }
}

fn family_signature<Addr: Clone + Eq + Hash>(
    family: &WeightedFoldFamily,
    candidates: &[ExactFoldGroup<Addr>],
) -> Vec<NodeId> {
    let mut roots = family
        .members
        .iter()
        .map(|member| candidates[*member].root)
        .collect::<Vec<_>>();
    roots.sort_unstable();
    roots
}

fn better_weighted_family<Addr: Clone + Eq + Hash>(
    candidate: &WeightedFoldFamily,
    current: &WeightedFoldFamily,
    groups: &[ExactFoldGroup<Addr>],
) -> bool {
    candidate.cmp_net(current).is_gt()
        || candidate.cmp_net(current).is_eq()
            && family_signature(candidate, groups) < family_signature(current, groups)
}

fn grow_weighted_family<Addr: Clone + Eq + Hash>(
    seed: usize,
    first: usize,
    candidates: &[ExactFoldGroup<Addr>],
    available: &[bool],
    compatible: &[Vec<bool>],
    shared_load: &[Vec<bool>],
    rejected: &HashSet<Vec<NodeId>>,
    four_state: bool,
) -> Option<WeightedFoldFamily> {
    let mut members = vec![seed, first];
    let mut best = None;
    loop {
        let weighted = weighted_fold_family(&members, candidates, four_state);
        let signature = family_signature(&weighted, candidates);
        if weighted.is_positive()
            && !rejected.contains(&signature)
            && best
                .as_ref()
                .is_none_or(|current| better_weighted_family(&weighted, current, candidates))
        {
            best = Some(weighted);
        }

        let mut best_expansion: Option<(usize, WeightedFoldFamily)> = None;
        for candidate in 0..candidates.len() {
            if !available[candidate]
                || members.contains(&candidate)
                || !members.iter().all(|member| compatible[*member][candidate])
                || !members.iter().any(|member| shared_load[*member][candidate])
            {
                continue;
            }
            let mut expanded = members.clone();
            expanded.push(candidate);
            let weighted = weighted_fold_family(&expanded, candidates, four_state);
            if best_expansion
                .as_ref()
                .is_none_or(|(current_index, current)| {
                    better_weighted_family(&weighted, current, candidates)
                        || weighted.cmp_net(current).is_eq()
                            && candidates[candidate].root < candidates[*current_index].root
                })
            {
                best_expansion = Some((candidate, weighted));
            }
        }
        let Some((next, _)) = best_expansion else {
            break;
        };
        members.push(next);
    }
    best
}

fn best_weighted_fold_family<Addr: Clone + Eq + Hash>(
    candidates: &[ExactFoldGroup<Addr>],
    available: &[bool],
    compatible: &[Vec<bool>],
    shared_load: &[Vec<bool>],
    rejected: &HashSet<Vec<NodeId>>,
    four_state: bool,
) -> Option<WeightedFoldFamily> {
    let mut best = None;
    for seed in 0..candidates.len() {
        if !available[seed] {
            continue;
        }
        // Force every compatible first edge from every seed. The subsequent
        // growth is weighted, so a low-root conflicting candidate cannot hide
        // a more profitable compatible partition.
        for first in 0..candidates.len() {
            if seed == first
                || !available[first]
                || !compatible[seed][first]
                || !shared_load[seed][first]
            {
                continue;
            }
            let Some(family) = grow_weighted_family(
                seed,
                first,
                candidates,
                available,
                compatible,
                shared_load,
                rejected,
                four_state,
            ) else {
                continue;
            };
            if best
                .as_ref()
                .is_none_or(|current| better_weighted_family(&family, current, candidates))
            {
                best = Some(family);
            }
        }
    }
    best
}

fn jointly_lower_fold_group_families<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    indices: &[usize],
    schedule_index: &FoldGroupScheduleIndex<Addr>,
    lowerer: &crate::SLTToSIRLowerer,
    builder: &mut SIRBuilder<Addr>,
    arena: &SLTNodeArena<Addr>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
    four_state: bool,
) -> HashSet<NodeId> {
    // `indices` is one bounded run of exact-fold roots. Direct dependencies
    // split runs before this point, and the fixed root window bounds the
    // pairwise compatibility matrix independently of design size.
    let candidates = discover_exact_fold_groups(indices, schedule_index);
    let mut compatible = vec![vec![false; candidates.len()]; candidates.len()];
    let mut shared_load = vec![vec![false; candidates.len()]; candidates.len()];
    for lhs in 0..candidates.len() {
        for rhs in lhs + 1..candidates.len() {
            let domains_match =
                same_fold_group_domain(candidates[lhs].root, candidates[rhs].root, arena);
            let independent = fold_groups_are_pairwise_independent(
                &candidates[lhs].facts,
                &candidates[rhs].facts,
            );
            let shares =
                fold_groups_share_exact_load(&candidates[lhs].facts, &candidates[rhs].facts);
            compatible[lhs][rhs] = domains_match && independent;
            compatible[rhs][lhs] = compatible[lhs][rhs];
            shared_load[lhs][rhs] = shares;
            shared_load[rhs][lhs] = shares;
        }
    }

    let mut available = vec![true; candidates.len()];
    let mut rejected = HashSet::<Vec<NodeId>>::default();
    let mut lowered = HashSet::default();
    while let Some(family) = best_weighted_fold_family(
        &candidates,
        &available,
        &compatible,
        &shared_load,
        &rejected,
        four_state,
    ) {
        let roots = family
            .members
            .iter()
            .map(|member| candidates[*member].root)
            .collect::<Vec<_>>();
        if lowerer.lower_fold_groups_jointly(builder, &roots, arena, lower_cache) {
            for member in family.members {
                available[member] = false;
            }
            lowered.extend(roots);
        } else {
            rejected.insert(family_signature(&family, &candidates));
        }
    }
    lowered
}

#[derive(Clone, Copy)]
struct PreparedFoldProjection {
    packed_result: RegisterId,
    access: BitAccess,
}

/// Materialize each shared grouped fold once, but leave its projections
/// deferred.  A target Store may invalidate the ordinary lowering cache, so
/// the packed result is retained explicitly until every projection has been
/// consumed.  Each narrow projection is created immediately before its Store
/// instead of keeping all projected registers live simultaneously.
fn prepare_atomic_fold_group_results<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    indices: &[usize],
    input: &[LogicPath<Addr>],
    fold_group_schedule_index: &FoldGroupScheduleIndex<Addr>,
    lowerer: &crate::SLTToSIRLowerer,
    builder: &mut SIRBuilder<Addr>,
    arena: &SLTNodeArena<Addr>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
    dep_memo: &mut HashMap<NodeId, HashSet<Addr>>,
    inverse_dep_memo: &mut HashMap<Addr, HashSet<NodeId>>,
    four_state: bool,
) -> HashMap<usize, PreparedFoldProjection> {
    let jointly_lowered = jointly_lower_fold_group_families(
        indices,
        fold_group_schedule_index,
        lowerer,
        builder,
        arena,
        lower_cache,
        four_state,
    );
    let mut counts = HashMap::default();
    for &idx in indices {
        let path = &input[idx];
        if path.target.var().is_none()
            || !path.local_inputs.is_empty()
            || !path.pre_lower_nodes.is_empty()
        {
            continue;
        }
        if let Some(group) = projected_for_fold_group(path.expr, arena) {
            *counts.entry(group).or_insert(0usize) += 1;
        }
    }

    let mut prepared = HashMap::default();
    for &idx in indices {
        let path = &input[idx];
        if !path.local_inputs.is_empty() || !path.pre_lower_nodes.is_empty() {
            continue;
        }
        let Some(group) = projected_for_fold_group(path.expr, arena) else {
            continue;
        };
        if counts.get(&group).copied().unwrap_or(0) < 2 && !jointly_lowered.contains(&group) {
            continue;
        }
        let Some(access) = fold_group_projection_access(path.expr, group, arena) else {
            continue;
        };
        collect_logic_path_input_deps(path, arena, dep_memo, inverse_dep_memo);
        let packed_result = lower_cache
            .get(&group)
            .copied()
            .unwrap_or_else(|| lowerer.lower(builder, group, arena, lower_cache));
        prepared.insert(
            idx,
            PreparedFoldProjection {
                packed_result,
                access,
            },
        );
    }
    prepared
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
}

/// Request to split a combinational schedule into partitions that run only
/// when state they read changed since they last ran.
///
/// Byte `p` of `flags`, a two-state object, is partition `p`'s dirty flag:
/// the settle runs the partition only when it is not zero, and clears it.
/// A partition that runs sets the flags of the partitions reading what it
/// changed; every other writer of state a partition reads must set its flag
/// too ([`SettlePartition::reads`]). Byte `N` (for `N` partitions) is the
/// untracked byte, which runs every partition on every settle. Byte `N + 1`
/// is computed when the settle starts: it runs every partition of this
/// settle when the untracked byte or enough flags are set, and the
/// partitions then skip comparing what they wrote.
#[derive(Clone, Copy, Debug)]
pub struct SettlePartitioning<Addr> {
    pub flags: Addr,
    /// Approximate expression nodes per partition.
    pub target_cost: usize,
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

fn scheduled_root_mux<A: Clone + Eq + Hash>(
    path: &LogicPath<A>,
    arena: &SLTNodeArena<A>,
) -> Option<(NodeId, NodeId, NodeId)> {
    if path.target.var().is_none()
        || !path.local_inputs.is_empty()
        || !path.pre_lower_nodes.is_empty()
    {
        return None;
    }
    let SLTNode::Mux {
        cond,
        then_expr,
        else_expr,
    } = arena.get(path.expr)
    else {
        return None;
    };
    Some((*cond, *then_expr, *else_expr))
}

fn collect_pure_scheduled_nodes<A: Clone + Eq + Hash>(
    root: NodeId,
    arena: &SLTNodeArena<A>,
    nodes: &mut HashSet<NodeId>,
) -> bool {
    if !nodes.insert(root) {
        return true;
    }
    match arena.get(root) {
        SLTNode::Input { index, .. } => index
            .iter()
            .all(|index| collect_pure_scheduled_nodes(index.node, arena, nodes)),
        SLTNode::Constant(..) => true,
        SLTNode::Binary(lhs, _, rhs) => {
            collect_pure_scheduled_nodes(*lhs, arena, nodes)
                && collect_pure_scheduled_nodes(*rhs, arena, nodes)
        }
        SLTNode::Unary(_, inner)
        | SLTNode::Capture { expr: inner, .. }
        | SLTNode::Slice { expr: inner, .. } => collect_pure_scheduled_nodes(*inner, arena, nodes),
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => {
            collect_pure_scheduled_nodes(*cond, arena, nodes)
                && collect_pure_scheduled_nodes(*then_expr, arena, nodes)
                && collect_pure_scheduled_nodes(*else_expr, arena, nodes)
        }
        SLTNode::Concat(parts) => parts
            .iter()
            .all(|(part, _)| collect_pure_scheduled_nodes(*part, arena, nodes)),
        SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => false,
    }
}

fn scheduled_node_cost<A: Clone + Eq + Hash>(node: NodeId, arena: &SLTNodeArena<A>) -> u128 {
    match arena.get(node) {
        SLTNode::Input { .. } | SLTNode::Constant(..) => 0,
        _ => crate::get_width(node, arena).div_ceil(64).max(1) as u128,
    }
}

fn scheduled_condition_probability<A: Clone + Eq + Hash>(
    mut condition: NodeId,
    arena: &SLTNodeArena<A>,
) -> (u128, u128) {
    let mut inverted = false;
    loop {
        match arena.get(condition) {
            SLTNode::Unary(UnaryOp::Ident | UnaryOp::ToTwoState, inner) => condition = *inner,
            SLTNode::Unary(UnaryOp::LogicNot, inner) => {
                inverted = !inverted;
                condition = *inner;
            }
            _ => break,
        }
    }
    let constant = |node| matches!(arena.get(node), SLTNode::Constant(..));
    let equality = matches!(
        arena.get(condition),
        SLTNode::Binary(lhs, BinaryOp::Eq | BinaryOp::EqWildcard, rhs)
            if constant(*lhs) || constant(*rhs)
    );
    let inequality = matches!(
        arena.get(condition),
        SLTNode::Binary(lhs, BinaryOp::Ne | BinaryOp::NeWildcard, rhs)
            if constant(*lhs) || constant(*rhs)
    );
    let true_weight = if equality {
        1
    } else if inequality {
        4
    } else {
        return (1, 2);
    };
    if inverted {
        (5 - true_weight, 5)
    } else {
        (true_weight, 5)
    }
}

fn scheduled_guard_region_is_profitable<A: Clone + Eq + Hash>(
    condition: NodeId,
    paths: &[usize],
    input: &[LogicPath<A>],
    arena: &SLTNodeArena<A>,
) -> bool {
    const CONTROL_COST: u128 = 2;
    const MISPREDICT_COST: u128 = 16;

    let mut true_nodes = HashSet::default();
    let mut false_nodes = HashSet::default();
    for &path in paths {
        let Some((actual, then_expr, else_expr)) = scheduled_root_mux(&input[path], arena) else {
            return false;
        };
        if actual != condition
            || !collect_pure_scheduled_nodes(then_expr, arena, &mut true_nodes)
            || !collect_pure_scheduled_nodes(else_expr, arena, &mut false_nodes)
        {
            return false;
        }
    }
    let true_owned = true_nodes
        .difference(&false_nodes)
        .map(|node| scheduled_node_cost(*node, arena))
        .sum::<u128>();
    let false_owned = false_nodes
        .difference(&true_nodes)
        .map(|node| scheduled_node_cost(*node, arena))
        .sum::<u128>();
    let (true_weight, total_weight) = scheduled_condition_probability(condition, arena);
    let false_weight = total_weight - true_weight;
    let removed_mux_cost = total_weight.saturating_mul(paths.len() as u128);
    let saved = false_weight
        .saturating_mul(true_owned)
        .saturating_add(true_weight.saturating_mul(false_owned))
        .saturating_add(removed_mux_cost);
    let introduced = total_weight.saturating_mul(CONTROL_COST).saturating_add(
        true_weight
            .min(false_weight)
            .saturating_mul(MISPREDICT_COST),
    );
    saved > introduced
}

fn condition_reads_target<A: Clone + Eq + Hash>(
    condition: NodeId,
    targets: &HashSet<A>,
    arena: &SLTNodeArena<A>,
) -> bool {
    let mut visited = HashSet::default();
    let mut work = vec![condition];
    while let Some(node) = work.pop() {
        if !visited.insert(node) {
            continue;
        }
        match arena.get(node) {
            SLTNode::Input {
                variable, index, ..
            } => {
                if targets.contains(variable) {
                    return true;
                }
                work.extend(index.iter().map(|index| index.node));
            }
            SLTNode::Constant(..) => {}
            SLTNode::Binary(lhs, _, rhs) => work.extend([*lhs, *rhs]),
            SLTNode::Unary(_, inner)
            | SLTNode::Capture { expr: inner, .. }
            | SLTNode::Slice { expr: inner, .. } => work.push(*inner),
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => work.extend([*cond, *then_expr, *else_expr]),
            SLTNode::Concat(parts) => work.extend(parts.iter().map(|(part, _)| *part)),
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => return true,
        }
    }
    false
}

fn form_scheduled_guard_regions<A: Clone + Eq + Hash>(
    work: Vec<ScheduledWork>,
    input: &[LogicPath<A>],
    arena: &SLTNodeArena<A>,
    four_state: bool,
) -> Vec<ScheduledWork> {
    if four_state {
        return work;
    }
    let mut result = Vec::with_capacity(work.len());
    let mut pending = work.into_iter().peekable();
    while let Some(item) = pending.next() {
        let ScheduledWork::CombPath(first_path) = item else {
            result.push(item);
            continue;
        };
        let Some((condition, _, _)) = scheduled_root_mux(&input[first_path], arena) else {
            result.push(ScheduledWork::CombPath(first_path));
            continue;
        };

        let mut paths = vec![first_path];
        while let Some(ScheduledWork::CombPath(next_path)) = pending.peek() {
            if scheduled_root_mux(&input[*next_path], arena)
                .is_none_or(|(next_condition, _, _)| next_condition != condition)
            {
                break;
            }
            paths.push(*next_path);
            pending.next();
        }

        let targets = paths
            .iter()
            .filter_map(|path| input[*path].target.var().map(|target| target.id.clone()))
            .collect::<HashSet<_>>();
        if paths.len() >= 2
            && !condition_reads_target(condition, &targets, arena)
            && scheduled_guard_region_is_profitable(condition, &paths, input, arena)
        {
            result.push(ScheduledWork::GuardedComb { condition, paths });
        } else {
            result.extend(paths.into_iter().map(ScheduledWork::CombPath));
        }
    }
    result
}

/// Lower a consecutive set of exact grouped-fold paths.  The packed fold
/// results are computed atomically, then each projection is created and stored
/// in topological order.  Ordinary paths bypass this buffer entirely.
fn flush_pending_fold_paths<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    pending: &mut Vec<usize>,
    input: &[LogicPath<Addr>],
    fold_group_schedule_index: &FoldGroupScheduleIndex<Addr>,
    lowerer: &crate::SLTToSIRLowerer,
    builder: &mut SIRBuilder<Addr>,
    arena: &SLTNodeArena<Addr>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
    dep_memo: &mut HashMap<NodeId, HashSet<Addr>>,
    inverse_dep_memo: &mut HashMap<Addr, HashSet<NodeId>>,
    unpacked_element_widths: &HashMap<Addr, usize>,
    four_state: bool,
) {
    if pending.is_empty() {
        return;
    }

    let prepared_results = prepare_atomic_fold_group_results(
        pending,
        input,
        fold_group_schedule_index,
        lowerer,
        builder,
        arena,
        lower_cache,
        dep_memo,
        inverse_dep_memo,
        four_state,
    );
    for idx in pending.drain(..) {
        let path = &input[idx];
        collect_logic_path_input_deps(path, arena, dep_memo, inverse_dep_memo);
        let prepared_result = prepared_results.get(&idx).map(|projection| {
            lowerer.project_materialized(builder, projection.packed_result, projection.access)
        });
        emit_logic_path_store_with_result(
            lowerer,
            builder,
            path,
            arena,
            lower_cache,
            unpacked_element_widths,
            prepared_result,
        );
        invalidate_logic_path_target(path, inverse_dep_memo, lower_cache);
    }
}

fn is_exact_fold_path<Addr: Clone + Eq + Hash + Copy>(
    path: usize,
    fold_groups: &FoldGroupScheduleIndex<Addr>,
) -> bool {
    if let Some(root) = fold_groups.direct_group_by_path[path]
        && fold_groups
            .groups
            .get(&root)
            .is_some_and(|info| info.exact_and_exclusive)
    {
        return true;
    }
    false
}

/// Assign a scheduling domain to paths whose memory effects or packed fold
/// result benefit from staying adjacent.  A domain is only a ready-queue
/// preference: it never contracts paths into one graph node and therefore
/// cannot make a not-yet-ready path execute early.
fn logic_path_scheduling_domains<Addr: Clone + Eq + Ord + Hash + Copy>(
    input: &[LogicPath<Addr>],
    fold_groups: &FoldGroupScheduleIndex<Addr>,
) -> Vec<Option<usize>> {
    let mut next_domain = 0usize;
    let mut fold_domains = BTreeMap::<NodeId, usize>::new();
    let mut target_domains = BTreeMap::<Addr, usize>::new();
    let mut domains = Vec::with_capacity(input.len());

    for (path, logic_path) in input.iter().enumerate() {
        let exact_fold_root = fold_groups.direct_group_by_path[path].filter(|root| {
            fold_groups
                .groups
                .get(root)
                .is_some_and(|info| info.exact_and_exclusive)
        });
        let domain = if let Some(root) = exact_fold_root {
            Some(*fold_domains.entry(root).or_insert_with(|| {
                let domain = next_domain;
                next_domain += 1;
                domain
            }))
        } else {
            logic_path.target.var().map(|target| {
                *target_domains.entry(target.id).or_insert_with(|| {
                    let domain = next_domain;
                    next_domain += 1;
                    domain
                })
            })
        };
        domains.push(domain);
    }
    domains
}

/// Produce a deterministic topological order of the SCC condensation graph.
/// A ready effect domain is drained while possible, but each component remains
/// an independent queue item.  This preserves target/fold locality without
/// treating a layer or a complete ready frontier as a lowering batch.  Actual
/// register-pressure scheduling is performed later on MIR instructions and
/// their real VRegs.
fn stable_topological_sccs(
    sccs: Vec<Vec<usize>>,
    adj: &[Vec<usize>],
    path_domains: &[Option<usize>],
) -> Option<(Vec<Vec<usize>>, Vec<usize>)> {
    if path_domains.len() != adj.len() {
        return None;
    }
    let component_count = sccs.len();
    let mut component_by_path = vec![usize::MAX; adj.len()];
    let mut keys = Vec::with_capacity(component_count);
    let mut component_domains = Vec::with_capacity(component_count);
    for (component, scc) in sccs.iter().enumerate() {
        keys.push(*scc.iter().min()?);
        for &path in scc {
            if path >= adj.len() || component_by_path[path] != usize::MAX {
                return None;
            }
            component_by_path[path] = component;
        }
        let singleton = (scc.len() == 1).then_some(scc[0]);
        let acyclic = singleton.is_some_and(|path| !adj[path].contains(&path));
        component_domains.push(
            acyclic
                .then(|| path_domains[singleton.expect("acyclic SCC is a singleton")])
                .flatten(),
        );
    }
    if component_by_path.contains(&usize::MAX) {
        return None;
    }

    let mut outgoing = vec![Vec::<usize>::new(); component_count];
    for (definition, users) in adj.iter().enumerate() {
        let source = component_by_path[definition];
        for &user in users {
            let target = *component_by_path.get(user)?;
            if source != target {
                outgoing[source].push(target);
            }
        }
    }
    let mut indegree = vec![0usize; component_count];
    for edges in &mut outgoing {
        edges.sort_unstable();
        edges.dedup();
        for &target in edges.iter() {
            indegree[target] = indegree[target].checked_add(1)?;
        }
    }

    let mut ready = BTreeSet::new();
    let mut ready_by_domain = HashMap::<usize, BTreeSet<(usize, usize)>>::default();
    for (component, degree) in indegree.iter().enumerate() {
        if *degree == 0 {
            let entry = (keys[component], component);
            ready.insert(entry);
            if let Some(domain) = component_domains[component] {
                ready_by_domain.entry(domain).or_default().insert(entry);
            }
        }
    }
    let mut components = sccs.into_iter().map(Some).collect::<Vec<_>>();
    let mut ordered = Vec::with_capacity(component_count);
    let mut active_domain = None;
    while !ready.is_empty() {
        let selected = active_domain
            .and_then(|domain| ready_by_domain.get(&domain)?.iter().next().copied())
            .or_else(|| ready.iter().next().copied())?;
        let (_, component) = selected;
        ready.remove(&selected);
        if let Some(domain) = component_domains[component] {
            ready_by_domain.get_mut(&domain)?.remove(&selected);
        }
        active_domain = component_domains[component];
        ordered.push(components[component].take()?);
        for &target in &outgoing[component] {
            indegree[target] = indegree[target].checked_sub(1)?;
            if indegree[target] == 0 {
                let entry = (keys[target], target);
                ready.insert(entry);
                if let Some(domain) = component_domains[target] {
                    ready_by_domain.entry(domain).or_default().insert(entry);
                }
            }
        }
    }
    (ordered.len() == component_count).then_some((ordered, component_by_path))
}

fn logic_path_is_scheduling_barrier<Addr: Clone + Eq + Hash>(path: &LogicPath<Addr>) -> bool {
    matches!(
        path.target,
        LogicPathTarget::CombCaptureEvent { .. } | LogicPathTarget::Process { .. }
    ) || !path.comb_capture_enable_sites.is_empty()
}

fn cached_logic_path_roots<Addr: Clone + Eq + Hash>(path: &LogicPath<Addr>) -> Vec<NodeId> {
    let mut roots = path
        .local_inputs
        .iter()
        .map(|(_, node)| *node)
        .collect::<Vec<_>>();
    if path.local_inputs.is_empty() && matches!(path.target, LogicPathTarget::Var(_)) {
        roots.extend(path.pre_lower_nodes.iter().copied());
        roots.push(path.expr);
    }
    roots
}

/// Build the sparse incidence relation between LogicPaths and SLT values
/// which can actually survive in `lower_cache` across path boundaries.
///
/// A tree-only node cannot be independently reused: a cached ancestor hides
/// it from later lowering.  Only DAG joins (including roots named by multiple
/// paths) therefore become materialization tokens.  Constants are deliberately
/// omitted because target lowering can rematerialize them without retaining a
/// source register.
fn logic_path_materialization_tokens<Addr: Clone + Eq + Hash>(
    input: &[LogicPath<Addr>],
    arena: &SLTNodeArena<Addr>,
) -> (Vec<Vec<usize>>, Vec<usize>) {
    let mut references = vec![0usize; arena.len()];
    let mut children = Vec::new();
    for raw in 0..arena.len() {
        children.clear();
        push_scheduler_node_children(NodeId(raw), arena, &mut children);
        children.sort_unstable();
        children.dedup();
        for &child in &children {
            references[child.0] = references[child.0].saturating_add(1);
        }
    }
    for path in input {
        let mut roots = cached_logic_path_roots(path);
        roots.sort_unstable();
        roots.dedup();
        for root in roots {
            references[root.0] = references[root.0].saturating_add(1);
        }
    }

    let candidates = references
        .iter()
        .enumerate()
        .map(|(raw, references)| {
            *references > 1 && !matches!(arena.get(NodeId(raw)), SLTNode::Constant(..))
        })
        .collect::<Vec<_>>();
    let mut raw_tokens = vec![Vec::<usize>::new(); input.len()];
    let mut token_users = vec![0usize; arena.len()];
    let mut visited = vec![0usize; arena.len()];
    let mut epoch = 0usize;
    let mut work = Vec::new();
    for (path_index, path) in input.iter().enumerate() {
        epoch = epoch.wrapping_add(1);
        if epoch == 0 {
            visited.fill(0);
            epoch = 1;
        }
        work.extend(cached_logic_path_roots(path));
        while let Some(node) = work.pop() {
            if visited[node.0] == epoch {
                continue;
            }
            visited[node.0] = epoch;
            if candidates[node.0] {
                raw_tokens[path_index].push(node.0);
            }
            push_scheduler_node_children(node, arena, &mut work);
        }
        raw_tokens[path_index].sort_unstable();
        raw_tokens[path_index].dedup();
        for &token in &raw_tokens[path_index] {
            token_users[token] = token_users[token].saturating_add(1);
        }
    }

    let mut dense_by_raw = vec![usize::MAX; arena.len()];
    let mut weights = Vec::new();
    for (raw, users) in token_users.into_iter().enumerate() {
        if users < 2 {
            continue;
        }
        dense_by_raw[raw] = weights.len();
        let width = crate::get_width(NodeId(raw), arena);
        weights.push(width.div_ceil(64).max(1));
    }
    for row in &mut raw_tokens {
        row.retain(|raw| dense_by_raw[*raw] != usize::MAX);
        for token in row.iter_mut() {
            *token = dense_by_raw[*token];
        }
    }
    (raw_tokens, weights)
}

fn exact_native_storage_store(width: usize) -> bool {
    width != 0 && width <= 64 && matches!(width.div_ceil(8), 1 | 2 | 4 | 8)
}

fn static_store_avoids_native_rmw<Addr: Copy + Eq + Hash>(
    write: &VarAtomBase<Addr>,
    var_widths: &HashMap<Addr, usize>,
    unpacked_element_widths: &HashMap<Addr, usize>,
) -> bool {
    let Some(width) = write
        .access
        .msb
        .checked_sub(write.access.lsb)
        .and_then(|width| width.checked_add(1))
    else {
        return false;
    };

    if let Some(&element_width) = unpacked_element_widths.get(&write.id) {
        // Unpacked layout selection happens after scheduling. Only a complete
        // byte-aligned native-width element avoids RMW in both packed and
        // independently strided layouts.
        return element_width != 0
            && width == element_width
            && write.access.lsb.is_multiple_of(element_width)
            && matches!(width, 8 | 16 | 32 | 64);
    }

    let whole_object = write.access.lsb == 0
        && var_widths.get(&write.id).copied() == Some(width)
        && exact_native_storage_store(width);
    let aligned_native_range =
        write.access.lsb.is_multiple_of(8) && matches!(width, 8 | 16 | 32 | 64);
    whole_object || aligned_native_range
}

fn direct_ff_write_ranges<Addr: Copy + Eq + Hash>(
    input: &[LogicPath<Addr>],
    summaries: &[FfAccessSummary<Addr>],
    plan: &FfCombSchedulePlan,
    retained: &HashSet<(usize, usize)>,
    ff_node_base: usize,
    var_widths: &HashMap<Addr, usize>,
    unpacked_element_widths: &HashMap<Addr, usize>,
) -> Vec<Vec<VarAtomBase<Addr>>> {
    // Index reads by object so the overlap probes below only scan atoms that
    // share the object under test. Only the comb readers the plan actually
    // consults need an index; every other path would pay for an unused map.
    let mut consulted_comb_readers = vec![false; input.len()];
    for readers in &plan.comb_before_direct_write {
        for &reader in readers {
            consulted_comb_readers[reader] = true;
        }
    }
    let comb_reads = input
        .iter()
        .zip(&consulted_comb_readers)
        .map(|(path, &consulted)| {
            consulted.then(|| index_accesses(path.sources.iter().chain(&path.previous_sources)))
        })
        .collect::<Vec<_>>();
    let ff_reads = summaries
        .iter()
        .map(|summary| index_accesses(&summary.reads))
        .collect::<Vec<_>>();

    summaries
        .iter()
        .enumerate()
        .map(|(writer, summary)| {
            let mut direct = summary
                .writes
                .iter()
                .map(|write| {
                    // Direct publication is only a throughput optimization.
                    // Dynamic destinations and static bitfields requiring a
                    // physical read-modify-write stay staged.
                    if summary.dynamic_writes.contains(&write.id)
                        || !static_store_avoids_native_rmw(
                            write,
                            var_widths,
                            unpacked_element_widths,
                        )
                    {
                        return false;
                    }

                    // A read in the same always_ff action cannot be ordered
                    // before only one of its own Stores. Keep just the
                    // overlapping range staged so lowering reads pre-edge
                    // state without penalizing disjoint writes.
                    if indexed_overlaps(&ff_reads[writer], write) {
                        return false;
                    }

                    let writer_node = ff_node_base + writer;
                    let comb_readers_proven = plan.comb_before_direct_write[writer]
                        .iter()
                        .filter(|&&reader| {
                            comb_reads[reader]
                                .as_ref()
                                .is_some_and(|reads| indexed_overlaps(reads, write))
                        })
                        .all(|&reader| retained.contains(&(reader, writer_node)));
                    let ff_readers_proven = plan.ff_before_direct_write[writer]
                        .iter()
                        .filter(|&&reader| indexed_overlaps(&ff_reads[reader], write))
                        .all(|&reader| retained.contains(&(ff_node_base + reader, writer_node)));
                    comb_readers_proven && ff_readers_proven
                })
                .collect::<Vec<_>>();

            // Direct Stores publish immediately, while staged Stores publish
            // together in finish(). Mixing those destinations inside one
            // overlapping write component can reverse procedural last-write-
            // wins order. Propagate staging through the component while
            // leaving disjoint ranges eligible for direct publication.
            let mut staged = direct
                .iter()
                .enumerate()
                .filter_map(|(index, &direct)| (!direct).then_some(index))
                .collect::<Vec<_>>();
            while let Some(staged_index) = staged.pop() {
                let staged_write = summary.writes[staged_index];
                for (candidate, candidate_write) in summary.writes.iter().enumerate() {
                    if direct[candidate]
                        && staged_write.id == candidate_write.id
                        && staged_write.access.overlaps(&candidate_write.access)
                    {
                        direct[candidate] = false;
                        staged.push(candidate);
                    }
                }
            }

            summary
                .writes
                .iter()
                .zip(direct)
                .filter_map(|(write, direct)| direct.then_some(write))
                .copied()
                .collect()
        })
        .collect()
}

fn schedule_acyclic_path_region(
    paths: &[usize],
    dependencies: &LogicPathEdges,
    values: &LogicPathEdges,
    materialization_tokens: &[Vec<usize>],
    token_weights: &[usize],
    local_by_path: &mut [usize],
) -> Option<Vec<usize>> {
    for (local, &path) in paths.iter().enumerate() {
        if path >= local_by_path.len() || local_by_path[path] != usize::MAX {
            for &mapped in &paths[..local] {
                local_by_path[mapped] = usize::MAX;
            }
            return None;
        }
        local_by_path[path] = local;
    }

    let result = (|| {
        let dependency_predecessors =
            MappedGraphRows::new(&dependencies.predecessors, paths, local_by_path).ok()?;
        let value_predecessors =
            MappedGraphRows::new(&values.predecessors, paths, local_by_path).ok()?;
        let tokens = MappedNodeRows::new(materialization_tokens, paths).ok()?;
        let successors = MappedGraphRows::new(&dependencies.users, paths, local_by_path).ok()?;
        let value_users = MappedGraphRows::new(&values.users, paths, local_by_path).ok()?;
        let order = schedule_min_live_values_and_tokens_with_mapped_rows(
            dependency_predecessors,
            value_predecessors,
            tokens,
            token_weights,
            successors,
            value_users,
        )
        .ok()?;
        Some(order.into_iter().map(|local| paths[local]).collect())
    })();

    for &path in paths {
        local_by_path[path] = usize::MAX;
    }
    result
}

/// Schedule maximal acyclic runs by MemoryDef/MemoryUse liveness. Loop SCCs
/// and observable events are synchronization boundaries, so no path is moved
/// across an iteration or externally visible effect.
fn schedule_logic_path_regions<Addr: Clone + Eq + Hash>(
    topological_sccs: Vec<Vec<usize>>,
    dependencies: &LogicPathEdges,
    values: &LogicPathEdges,
    materialization_tokens: &[Vec<usize>],
    token_weights: &[usize],
    input: &[LogicPath<Addr>],
) -> Option<Vec<ScheduledWork>> {
    if dependencies.users.len() != values.users.len()
        || dependencies.users.len() != materialization_tokens.len()
        || dependencies.users.len() < input.len()
    {
        return None;
    }
    let mut local_by_path = vec![usize::MAX; dependencies.users.len()];
    let mut result = Vec::with_capacity(topological_sccs.len());
    let mut pending = Vec::new();
    let flush = |pending: &mut Vec<usize>,
                 result: &mut Vec<ScheduledWork>,
                 local_by_path: &mut [usize]|
     -> Option<()> {
        if pending.is_empty() {
            return Some(());
        }
        let ordered = schedule_acyclic_path_region(
            pending,
            dependencies,
            values,
            materialization_tokens,
            token_weights,
            local_by_path,
        )?;
        result.extend(ordered.into_iter().map(|path| {
            if path < input.len() {
                ScheduledWork::CombPath(path)
            } else {
                ScheduledWork::Ff(path - input.len())
            }
        }));
        pending.clear();
        Some(())
    };

    for scc in topological_sccs {
        if let [path] = scc.as_slice()
            && !dependencies.users[*path].contains(path)
            && (*path >= input.len() || !logic_path_is_scheduling_barrier(&input[*path]))
        {
            pending.push(*path);
        } else {
            flush(&mut pending, &mut result, &mut local_by_path)?;
            if let [path] = scc.as_slice() {
                if *path < input.len() {
                    result.push(ScheduledWork::CombPath(*path));
                } else {
                    result.push(ScheduledWork::Ff(*path - input.len()));
                }
            } else if scc.iter().all(|path| *path < input.len()) {
                result.push(ScheduledWork::CombScc(scc));
            } else {
                return None;
            }
        }
    }
    flush(&mut pending, &mut result, &mut local_by_path)?;
    Some(result)
}

/// Dependency analysis and region scheduling shared by every lowering of one
/// LogicPath set.
struct PreparedSchedule<Addr: Clone + Eq + Hash> {
    input: Vec<LogicPath<Addr>>,
    /// Semantic dependency users, indexed by path and then FF action.
    adj: Vec<Vec<usize>>,
    component_by_path: Vec<usize>,
    scheduled_work: Vec<ScheduledWork>,
    fold_group_schedule_index: FoldGroupScheduleIndex<Addr>,
    direct_ff_writes_by_action: Vec<Vec<VarAtomBase<Addr>>>,
}

/// Build the MemorySSA dependency graph, condense its SCCs, and order the
/// resulting work for lowering.
fn prepare_schedule<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    input: Vec<LogicPath<Addr>>,
    arena: &SLTNodeArena<Addr>,
    four_state: bool,
    var_widths: &HashMap<Addr, usize>,
    unpacked_element_widths: &HashMap<Addr, usize>,
    ff_summaries: Option<&[FfAccessSummary<Addr>]>,
) -> Result<PreparedSchedule<Addr>, SchedulerError<Addr>> {
    let (input, ff_plan) = if let Some(summaries) = ff_summaries {
        let mut plan = plan_ff_comb_schedule(&input, summaries)?;
        let mut old_to_new = vec![usize::MAX; input.len()];
        let mut filtered = Vec::with_capacity(
            plan.required_comb
                .iter()
                .filter(|required| **required)
                .count(),
        );
        for (old, path) in input.into_iter().enumerate() {
            if plan.required_comb[old] {
                old_to_new[old] = filtered.len();
                filtered.push(path);
            }
        }
        let remap_paths = |paths: &mut Vec<usize>| {
            for path in paths.iter_mut() {
                *path = old_to_new[*path];
            }
            paths.retain(|path| *path != usize::MAX);
            paths.sort_unstable();
            paths.dedup();
        };
        for paths in plan
            .comb_value_predecessors
            .iter_mut()
            .chain(&mut plan.comb_before_direct_write)
        {
            remap_paths(paths);
        }
        plan.required_comb = vec![true; filtered.len()];
        (filtered, Some(plan))
    } else {
        (input, None)
    };

    // 1. Build bit-range MemoryDefs/MemoryUses and their semantic graph.
    // This is the source dataflow adapter; interval indexing and DAG
    // scheduling remain IR-independent in celox-analysis.
    let n = input.len();
    let (mut materialization_tokens, token_weights) =
        logic_path_materialization_tokens(&input, arena);
    let LogicPathMemorySsa {
        mut dependencies,
        mut values,
    } = build_logic_path_memory_ssa(&input)?;

    // FF actions are ordinary sinks in the existing comb dependency graph.
    // The added nodes go through the same SCC, topological, and ready-queue
    // scheduling as LogicPaths; there is no second clock-specific scheduler.
    let ff_count = ff_summaries.map_or(0, <[FfAccessSummary<Addr>]>::len);
    let mut direct_ff_writes_by_action = vec![Vec::new(); ff_count];
    if let Some(plan) = &ff_plan {
        dependencies.resize(n + ff_count);
        values.resize(n + ff_count);
        materialization_tokens.resize_with(n + ff_count, Vec::new);
        for (ff_index, predecessors) in plan.comb_value_predecessors.iter().enumerate() {
            let ff_node = n + ff_index;
            for &definition in predecessors {
                dependencies.push(definition, ff_node);
                values.push(definition, ff_node);
            }
        }
        let write_order_edges = plan
            .comb_before_direct_write
            .iter()
            .zip(&plan.ff_before_direct_write)
            .enumerate()
            .map(|(writer, (comb_readers, ff_readers))| {
                comb_readers
                    .iter()
                    .copied()
                    .map(|reader| (reader, n + writer))
                    .chain(
                        ff_readers
                            .iter()
                            .copied()
                            .map(|reader| (n + reader, n + writer)),
                    )
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let retained = add_acyclic_ff_write_order_edges(
            &mut dependencies.users,
            write_order_edges.iter().flatten().copied(),
        );
        for &(predecessor, user) in &retained {
            dependencies.predecessors[user].push(predecessor);
        }
        direct_ff_writes_by_action = direct_ff_write_ranges(
            &input,
            ff_summaries.expect("an FF schedule plan requires FF access summaries"),
            plan,
            &retained,
            n,
            var_widths,
            unpacked_element_widths,
        );
        dependencies.canonicalize();
        values.canonicalize();
    }

    // 2. SCC extraction identifies the synchronization boundaries at which
    // the dataflow equations must be iterated rather than freely reordered.
    let mut ctx = TarjanContext {
        index: 0,
        stack: Vec::new(),
        on_stack: HashSet::default(),
        indices: vec![None; n + ff_count],
        lowlink: vec![None; n + ff_count],
        sccs: Vec::new(),
    };
    for i in 0..(n + ff_count) {
        if ctx.indices[i].is_none() {
            strong_connect(i, &dependencies.users, &mut ctx);
        }
    }
    let fold_group_schedule_index = build_fold_group_schedule_index(&input, arena);
    let mut path_domains = logic_path_scheduling_domains(&input, &fold_group_schedule_index);
    path_domains.resize(n + ff_count, None);
    let (topological_sccs, component_by_path) =
        stable_topological_sccs(ctx.sccs, &dependencies.users, &path_domains)
            .ok_or(SchedulerError::InvalidDependencyGraph)?;
    let scheduled_work = schedule_logic_path_regions(
        topological_sccs,
        &dependencies,
        &values,
        &materialization_tokens,
        &token_weights,
        &input,
    )
    .ok_or(SchedulerError::InvalidDependencyGraph)?;
    let adj = dependencies.users;
    drop(dependencies.predecessors);
    drop(values);
    let scheduled_work = form_scheduled_guard_regions(scheduled_work, &input, arena, four_state);

    Ok(PreparedSchedule {
        input,
        adj,
        component_by_path,
        scheduled_work,
        fold_group_schedule_index,
        direct_ff_writes_by_action,
    })
}

/// Read-only facts shared by every lowering of one prepared schedule.
struct WorkLoweringContext<'a, Addr: Clone + Eq + Hash> {
    input: &'a [LogicPath<Addr>],
    arena: &'a SLTNodeArena<Addr>,
    adj: &'a [Vec<usize>],
    component_by_path: &'a [usize],
    ignored_loops: &'a HashSet<(Addr, Addr)>,
    true_loops: &'a HashMap<(Addr, Addr), usize>,
    fold_group_schedule_index: &'a FoldGroupScheduleIndex<Addr>,
    unpacked_element_widths: &'a HashMap<Addr, usize>,
    direct_ff_writes_by_action: &'a [Vec<VarAtomBase<Addr>>],
    lowerer: &'a crate::SLTToSIRLowerer,
    four_state: bool,
    /// Start a new execution unit when one grows past [`EU_BLOCK_LIMIT`].
    /// Shared comb/FF lowering keeps one unit because its staged FF state
    /// publishes at the end of the same function.
    split_large_units: bool,
}

/// Maximum blocks in a single EU before flushing to a new one.
/// This prevents Cranelift from choking on massive functions.
const EU_BLOCK_LIMIT: usize = 20_000;
const UNROLL_THRESHOLD: usize = 32;
/// Pairwise joint-fold profitability is intentionally local. Keep all
/// projections of one packed root together, but never build an unbounded
/// compatibility matrix for a long run of independent roots.
const MAX_JOINT_FOLD_ROOTS: usize = 16;

/// One partition of a settle plan.
struct PlannedPartition<Addr> {
    reads: Vec<Addr>,
    writes: Vec<Addr>,
    /// The partitions whose flags it sets whenever it runs: the readers of
    /// what it writes whose width is unknown.
    marks: Vec<usize>,
    /// What it writes that other partitions read, itself included when it
    /// reads the previous value of its own writes. It sets the readers'
    /// flags only when it changes the value.
    watched: Vec<WatchedChunk<Addr>>,
    pinned: bool,
}

/// A chunk of an object a partition writes, compared before and after the
/// partition runs.
struct WatchedChunk<Addr> {
    object: Addr,
    offset: SIROffset,
    width: usize,
    readers: Vec<usize>,
    /// The readers at or before the partition in the schedule: the only ones
    /// marked when the settle runs every partition, since the others run
    /// after it in the same settle.
    earlier: Vec<usize>,
}

/// The widest chunk of a written object compared at once.
const MAX_WATCHED_WIDTH: usize = 4096;

/// The partitions of a settle, each a contiguous run of scheduled work.
struct PartitionPlan<Addr> {
    /// The first scheduled work of each partition.
    first_work: Vec<usize>,
    partitions: Vec<PlannedPartition<Addr>>,
}

/// The paths of one piece of scheduled combinational work.
fn work_paths(work: &ScheduledWork) -> &[usize] {
    match work {
        ScheduledWork::CombPath(path) => std::slice::from_ref(path),
        ScheduledWork::CombScc(paths) | ScheduledWork::GuardedComb { paths, .. } => paths,
        ScheduledWork::Ff(_) => &[],
    }
}

/// The approximate lowering cost of a path: its distinct expression nodes,
/// or a process unit's instructions.
pub fn path_cost<Addr: Clone + Eq + Hash>(
    arena: &SLTNodeArena<Addr>,
    path: &LogicPath<Addr>,
) -> usize {
    if let LogicPathTarget::Process { unit, .. } = &path.target {
        return unit
            .blocks
            .values()
            .map(|block| block.instructions.len() + 1)
            .sum();
    }
    let mut seen = HashSet::default();
    let mut stack = vec![path.expr];
    let mut cost = 0;
    while let Some(node) = stack.pop() {
        if !seen.insert(node) {
            continue;
        }
        cost += 1;
        match arena.get(node) {
            SLTNode::Binary(lhs, _, rhs) => stack.extend([*lhs, *rhs]),
            SLTNode::Unary(_, operand)
            | SLTNode::Slice { expr: operand, .. }
            | SLTNode::Capture { expr: operand, .. } => stack.push(*operand),
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => stack.extend([*cond, *then_expr, *else_expr]),
            SLTNode::Concat(parts) => stack.extend(parts.iter().map(|(part, _)| *part)),
            // A loop's cost scales with its trip count; count it as large.
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => cost += 64,
            SLTNode::Input { .. } | SLTNode::Constant(..) => {}
        }
    }
    cost
}

/// Whether a path's expression captures a value at an evaluation position.
fn path_has_capture<Addr: Clone + Eq + Hash>(
    arena: &SLTNodeArena<Addr>,
    path: &LogicPath<Addr>,
) -> bool {
    if matches!(path.target, LogicPathTarget::CombCaptureEvent { .. }) {
        return true;
    }
    let mut seen = HashSet::default();
    let mut stack = vec![path.expr];
    while let Some(node) = stack.pop() {
        if !seen.insert(node) {
            continue;
        }
        match arena.get(node) {
            SLTNode::Capture { .. } => return true,
            SLTNode::Binary(lhs, _, rhs) => stack.extend([*lhs, *rhs]),
            SLTNode::Unary(_, operand) | SLTNode::Slice { expr: operand, .. } => {
                stack.push(*operand)
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => stack.extend([*cond, *then_expr, *else_expr]),
            SLTNode::Concat(parts) => stack.extend(parts.iter().map(|(part, _)| *part)),
            _ => {}
        }
    }
    false
}

/// Whether a path does more than store its value: comb capture events,
/// activation of capture sites, and process units with runtime events or
/// extern calls. A partition holding one runs on every settle.
fn path_has_effects<Addr: Clone + Eq + Hash>(path: &LogicPath<Addr>) -> bool {
    if !path.comb_capture_enable_sites.is_empty() || path.comb_capture_enable_always {
        return true;
    }
    match &path.target {
        LogicPathTarget::CombCaptureEvent { .. } => true,
        LogicPathTarget::Process { unit, .. } => unit.blocks.values().any(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| match instruction {
                    SIRInstruction::RuntimeEvent { .. }
                    | SIRInstruction::CombCaptureEvent { .. }
                    | SIRInstruction::CombCaptureEnableIfChanged { .. }
                    | SIRInstruction::ExternCall { .. } => true,
                    SIRInstruction::Store(.., capture_sites) => !capture_sites.is_empty(),
                    _ => false,
                })
        }),
        LogicPathTarget::Var(_) => false,
    }
}

/// Cut a combinational schedule into partitions of about
/// `partitioning.target_cost`, or `None` when it would be one.
fn plan_settle_partitions<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    prepared: &PreparedSchedule<Addr>,
    arena: &SLTNodeArena<Addr>,
    var_widths: &HashMap<Addr, usize>,
    unpacked_element_widths: &HashMap<Addr, usize>,
    partitioning: SettlePartitioning<Addr>,
) -> Option<(PartitionPlan<Addr>, Addr)> {
    let input = &prepared.input;
    // A captured value or a pre-lowered node is materialized at one place
    // and reused at a later one through the lowering cache, which a
    // partition boundary between them would clear.
    if input
        .iter()
        .any(|path| !path.pre_lower_nodes.is_empty() || path_has_capture(arena, path))
    {
        return None;
    }
    // Whole-variable copies, by target.
    let mut copy_source: HashMap<Addr, Addr> = HashMap::default();
    for work in &prepared.scheduled_work {
        if let ScheduledWork::CombPath(path) = work
            && let Some((target, source)) = whole_variable_copy(arena, &input[*path], var_widths)
        {
            copy_source.insert(target, source);
        }
    }
    let mut first_work = Vec::new();
    let mut cost = 0;
    for (index, work) in prepared.scheduled_work.iter().enumerate() {
        if first_work.is_empty() || cost >= partitioning.target_cost {
            first_work.push(index);
            cost = 0;
        }
        cost += work_paths(work)
            .iter()
            .map(|&path| path_cost(arena, &input[path]))
            .sum::<usize>();
    }
    if first_work.len() < 2 {
        return None;
    }
    let mut partition_of_work = vec![0; prepared.scheduled_work.len()];
    for (partition, &first) in first_work.iter().enumerate() {
        let end = first_work
            .get(partition + 1)
            .copied()
            .unwrap_or(prepared.scheduled_work.len());
        partition_of_work[first..end].fill(partition);
    }
    let count = first_work.len();
    let mut reads = vec![BTreeSet::new(); count];
    let mut writes = vec![BTreeSet::new(); count];
    let mut previous = vec![BTreeSet::new(); count];
    let mut pinned = vec![false; count];
    for (index, work) in prepared.scheduled_work.iter().enumerate() {
        let partition = partition_of_work[index];
        for &path_index in work_paths(work) {
            let path = &input[path_index];
            pinned[partition] |= path_has_effects(path);
            let atoms = path
                .sources
                .iter()
                .chain(&path.address_sources)
                .chain(&path.previous_sources);
            reads[partition].extend(atoms.map(|atom| atom.id));
            // A process's activation guard reads its occurrence counters and
            // startup slots as well.
            if let LogicPathTarget::Process {
                sensitivity_counters,
                startup,
                unit,
                ..
            } = &path.target
            {
                // The unit may read more than its listed sources, such as
                // force controls. It runs at its process's first projection
                // and writes every output there.
                for block in unit.blocks.values() {
                    for instruction in &block.instructions {
                        match instruction {
                            SIRInstruction::Load(_, address, ..) => {
                                reads[partition].insert(*address);
                            }
                            SIRInstruction::Store(address, ..) => {
                                writes[partition].insert(*address);
                            }
                            SIRInstruction::Commit(source, target, ..) => {
                                reads[partition].insert(*source);
                                writes[partition].insert(*target);
                            }
                            _ => {}
                        }
                    }
                }
                for (atom, counter) in sensitivity_counters {
                    reads[partition].extend([atom.id, *counter]);
                }
                if let Some(startup) = startup {
                    reads[partition].insert(startup.initialized);
                    for (atom, slot) in &startup.inputs {
                        reads[partition].extend([atom.id, *slot]);
                    }
                    for (counter, slot) in &startup.counters {
                        reads[partition].extend([*counter, *slot]);
                    }
                }
            }
            previous[partition].extend(path.previous_sources.iter().map(|atom| atom.id));
            if let Some(target) = path.target.var() {
                writes[partition].insert(target.id);
            }
        }
    }
    // Reading a copy's target reads its source: the copy may become an
    // alias of its source, whose changes then reach the target without the
    // copy running.
    for read in &mut reads {
        let mut sources = Vec::new();
        for &addr in read.iter() {
            let mut current = addr;
            let mut steps = 0;
            while let Some(&source) = copy_source.get(&current) {
                sources.push(source);
                current = source;
                steps += 1;
                if steps > copy_source.len() {
                    break;
                }
            }
        }
        read.extend(sources);
    }
    let mut readers: HashMap<Addr, Vec<usize>> = HashMap::default();
    for (partition, read) in reads.iter().enumerate() {
        for &addr in read {
            readers.entry(addr).or_default().push(partition);
        }
    }
    let partitions = (0..count)
        .map(|partition| {
            let mut marks = BTreeSet::new();
            let mut watched = Vec::new();
            for addr in &writes[partition] {
                let targets: Vec<usize> = readers
                    .get(addr)
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|&reader| reader != partition || previous[partition].contains(addr))
                    .collect();
                if targets.is_empty() {
                    continue;
                }
                let Some(&width) = var_widths.get(addr) else {
                    marks.extend(targets);
                    continue;
                };
                // Compare an unpacked array in whole elements.
                let element = unpacked_element_widths
                    .get(addr)
                    .copied()
                    .filter(|&element| element != 0 && width.is_multiple_of(element))
                    .unwrap_or(1);
                let step = (MAX_WATCHED_WIDTH / element).max(1) * element;
                for lsb in (0..width).step_by(step) {
                    let chunk = (width - lsb).min(step);
                    watched.push(WatchedChunk {
                        object: *addr,
                        offset: static_access_offset(
                            addr,
                            BitAccess::new(lsb, lsb + chunk - 1),
                            unpacked_element_widths,
                        ),
                        width: chunk,
                        readers: targets.clone(),
                        earlier: targets
                            .iter()
                            .copied()
                            .filter(|&reader| reader <= partition)
                            .collect(),
                    });
                }
            }
            PlannedPartition {
                reads: reads[partition].iter().copied().collect(),
                writes: writes[partition].iter().copied().collect(),
                marks: marks.into_iter().collect(),
                watched,
                pinned: pinned[partition],
            }
        })
        .collect();
    Some((
        PartitionPlan {
            first_work,
            partitions,
        },
        partitioning.flags,
    ))
}

/// The `(target, source)` of a path that copies one whole variable into
/// another of the same width.
fn whole_variable_copy<Addr: Clone + Eq + Hash + Copy>(
    arena: &SLTNodeArena<Addr>,
    path: &LogicPath<Addr>,
    var_widths: &HashMap<Addr, usize>,
) -> Option<(Addr, Addr)> {
    if !path.transparent_copy || !path.previous_sources.is_empty() {
        return None;
    }
    let target = match &path.target {
        LogicPathTarget::Var(target) => target,
        _ => return None,
    };
    let width = *var_widths.get(&target.id)?;
    if target.access.lsb != 0 || target.access.msb + 1 != width {
        return None;
    }
    match arena.get(path.expr) {
        SLTNode::Input {
            variable,
            index,
            access,
            ..
        } if index.is_empty()
            && access.lsb == 0
            && access.msb + 1 == width
            && var_widths.get(variable) == Some(&width)
            && *variable != target.id =>
        {
            Some((target.id, *variable))
        }
        _ => None,
    }
}

/// Lowering state for one sequence of scheduled work emitted through one SIR
/// builder. Register numbers and the expression cache are local to it.
struct WorkLowering<'a, Addr: Clone + Eq + Hash> {
    context: &'a WorkLoweringContext<'a, Addr>,
    builder: SIRBuilder<Addr>,
    lower_cache: HashMap<NodeId, RegisterId>,
    dep_memo: HashMap<NodeId, HashSet<Addr>>,
    inverse_dep_memo: HashMap<Addr, HashSet<NodeId>>,
    execution_units: Vec<ExecutionUnit<Addr>>,
    pending_fold_indices: Vec<usize>,
    pending_fold_roots: HashSet<NodeId>,
    /// The processes whose unit an acyclic projection has run (see
    /// [`Self::lower`]).
    run_processes: HashSet<Addr>,
    /// Inside a partition of a partitioned settle, which units must not
    /// split.
    in_partition: bool,
    /// Where a guarded partition continues when it is skipped.
    partition_merge: Option<BlockId>,
    /// The open partition's watched chunks before it ran.
    partition_old: Vec<RegisterId>,
}

impl<'a, Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display> WorkLowering<'a, Addr> {
    fn new(context: &'a WorkLoweringContext<'a, Addr>) -> Self {
        Self {
            context,
            builder: SIRBuilder::new(),
            lower_cache: HashMap::default(),
            dep_memo: HashMap::default(),
            inverse_dep_memo: HashMap::default(),
            execution_units: Vec::new(),
            pending_fold_indices: Vec::new(),
            pending_fold_roots: HashSet::default(),
            run_processes: HashSet::default(),
            in_partition: false,
            partition_merge: None,
            partition_old: Vec::new(),
        }
    }

    /// Start a settle of `count` partitions. Skip publication comparisons
    /// when every partition will run anyway, or an untracked raw view requires
    /// a full settle. A fraction of dirty flags is not a cost estimate: a
    /// clean partition can contain most of the program's work.
    fn begin_settle(&mut self, flags: Addr, count: usize) {
        let all = self.builder.alloc_bit(count * 8, false);
        self.builder.emit(SIRInstruction::Load(
            all,
            flags,
            SIROffset::Static(0),
            count * 8,
        ));
        let count_width = UnaryOp::PopCount.result_width(count * 8);
        let dirty = self.builder.alloc_bit(count_width, false);
        self.builder
            .emit(SIRInstruction::Unary(dirty, UnaryOp::PopCount, all));
        let threshold = self.builder.alloc_bit(count_width, false);
        self.builder
            .emit(SIRInstruction::Imm(threshold, SIRValue::new(count as u64)));
        let busy = self.builder.alloc_bit(1, false);
        self.builder.emit(SIRInstruction::Binary(
            busy,
            dirty,
            BinaryOp::GeU,
            threshold,
        ));
        let untracked = self.builder.alloc_bit(8, false);
        self.builder.emit(SIRInstruction::Load(
            untracked,
            flags,
            SIROffset::Static(count * 8),
            8,
        ));
        let zero = self.builder.alloc_bit(8, false);
        self.builder
            .emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
        let raw = self.builder.alloc_bit(1, false);
        self.builder
            .emit(SIRInstruction::Binary(raw, untracked, BinaryOp::Ne, zero));
        let full = self.builder.alloc_bit(1, false);
        self.builder
            .emit(SIRInstruction::Binary(full, busy, BinaryOp::Or, raw));
        let one = self.builder.alloc_bit(8, false);
        self.builder
            .emit(SIRInstruction::Imm(one, SIRValue::new(1u64)));
        let byte = self.builder.alloc_bit(8, false);
        self.builder
            .emit(SIRInstruction::Mux(byte, full, one, zero));
        self.builder.emit(SIRInstruction::Store(
            flags,
            SIROffset::Static((count + 1) * 8),
            8,
            byte,
            Vec::new(),
            Vec::new(),
        ));
    }

    /// Whether the settle of `count` partitions runs every partition.
    fn settle_is_full(&mut self, flags: Addr, count: usize) -> RegisterId {
        let byte = self.builder.alloc_bit(8, false);
        self.builder.emit(SIRInstruction::Load(
            byte,
            flags,
            SIROffset::Static((count + 1) * 8),
            8,
        ));
        let zero = self.builder.alloc_bit(8, false);
        self.builder
            .emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
        let full = self.builder.alloc_bit(1, false);
        self.builder
            .emit(SIRInstruction::Binary(full, byte, BinaryOp::Ne, zero));
        full
    }

    /// Compare each chunk with its value before the partition ran (`old`),
    /// and set the flags of its `readers` when it changed: one change bit per
    /// reader set, then one flag update per reader.
    fn mark_changed_readers(
        &mut self,
        flags: Addr,
        one: RegisterId,
        chunks: Vec<(&WatchedChunk<Addr>, &[usize], RegisterId)>,
    ) {
        let mut set_changes: Vec<(&[usize], RegisterId)> = Vec::new();
        for (chunk, readers, old) in chunks {
            let new = self.builder.alloc_logic(chunk.width);
            self.builder.emit(SIRInstruction::Load(
                new,
                chunk.object,
                chunk.offset.clone(),
                chunk.width,
            ));
            // Both planes: an X turning into a known value is a change.
            let changed = self.builder.alloc_bit(1, false);
            self.builder
                .emit(SIRInstruction::Binary(changed, old, BinaryOp::NeCase, new));
            match set_changes.iter_mut().find(|(set, _)| *set == readers) {
                Some((_, any)) => {
                    let both = self.builder.alloc_bit(1, false);
                    self.builder
                        .emit(SIRInstruction::Binary(both, *any, BinaryOp::Or, changed));
                    *any = both;
                }
                None => set_changes.push((readers, changed)),
            }
        }
        let mut reader_changes: BTreeMap<usize, RegisterId> = BTreeMap::new();
        for (readers, changed) in set_changes {
            for &reader in readers {
                let any = match reader_changes.get(&reader).copied() {
                    Some(any) => {
                        let both = self.builder.alloc_bit(1, false);
                        self.builder
                            .emit(SIRInstruction::Binary(both, any, BinaryOp::Or, changed));
                        both
                    }
                    None => changed,
                };
                reader_changes.insert(reader, any);
            }
        }
        for (reader, changed) in reader_changes {
            let flag = self.builder.alloc_bit(8, false);
            self.builder.emit(SIRInstruction::Load(
                flag,
                flags,
                SIROffset::Static(reader * 8),
                8,
            ));
            let marked = self.builder.alloc_bit(8, false);
            self.builder
                .emit(SIRInstruction::Mux(marked, changed, one, flag));
            self.builder.emit(SIRInstruction::Store(
                flags,
                SIROffset::Static(reader * 8),
                8,
                marked,
                Vec::new(),
                Vec::new(),
            ));
        }
    }

    /// Open partition `index` of `count`: run it only when its flag or the
    /// full byte is set, and clear its flag. Values computed before it may
    /// be reused inside, but not the other way round, so the expression
    /// cache starts over.
    fn begin_partition(
        &mut self,
        flags: Addr,
        index: usize,
        count: usize,
        partition: &PlannedPartition<Addr>,
    ) {
        self.flush_pending_folds();
        if self.context.split_large_units && self.builder.block_count() >= EU_BLOCK_LIMIT {
            if let Some(eu) = self.builder.flush_eu() {
                self.execution_units.push(eu);
            }
        }
        self.lower_cache.clear();
        self.in_partition = true;
        let offset = SIROffset::Static(index * 8);
        if !partition.pinned {
            let own = self.builder.alloc_bit(8, false);
            self.builder
                .emit(SIRInstruction::Load(own, flags, offset.clone(), 8));
            let all = self.builder.alloc_bit(8, false);
            self.builder.emit(SIRInstruction::Load(
                all,
                flags,
                SIROffset::Static((count + 1) * 8),
                8,
            ));
            let flag = self.builder.alloc_bit(8, false);
            self.builder
                .emit(SIRInstruction::Binary(flag, own, BinaryOp::Or, all));
            let zero = self.builder.alloc_bit(8, false);
            self.builder
                .emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
            let dirty = self.builder.alloc_bit(1, false);
            self.builder
                .emit(SIRInstruction::Binary(dirty, flag, BinaryOp::Ne, zero));
            let body = self.builder.new_block();
            let merge = self.builder.new_block();
            self.builder.seal_block(SIRTerminator::Branch {
                cond: dirty,
                true_block: (body, Vec::new()),
                false_block: (merge, Vec::new()),
            });
            self.builder.switch_to_block(body);
            self.partition_merge = Some(merge);
        }
        let clean = self.builder.alloc_bit(8, false);
        self.builder
            .emit(SIRInstruction::Imm(clean, SIRValue::new(0u64)));
        self.builder.emit(SIRInstruction::Store(
            flags,
            offset,
            8,
            clean,
            Vec::new(),
            Vec::new(),
        ));
        self.partition_old.clear();
        for chunk in &partition.watched {
            let old = self.builder.alloc_logic(chunk.width);
            self.builder.emit(SIRInstruction::Load(
                old,
                chunk.object,
                chunk.offset.clone(),
                chunk.width,
            ));
            self.partition_old.push(old);
        }
    }

    /// Close a partition of a settle of `count`: set the flags of the
    /// partitions reading what it changed, then join the path that skipped
    /// it.
    fn end_partition(&mut self, flags: Addr, count: usize, partition: &PlannedPartition<Addr>) {
        self.flush_pending_folds();
        if !partition.watched.is_empty() {
            let one = self.builder.alloc_bit(8, false);
            self.builder
                .emit(SIRInstruction::Imm(one, SIRValue::new(1u64)));
            let old = std::mem::take(&mut self.partition_old);
            let full = self.settle_is_full(flags, count);
            let skip = self.builder.new_block();
            let compare = self.builder.new_block();
            let join = self.builder.new_block();
            self.builder.seal_block(SIRTerminator::Branch {
                cond: full,
                true_block: (skip, Vec::new()),
                false_block: (compare, Vec::new()),
            });
            // Every partition runs in this settle: only the earlier readers
            // need their flags, for the next one.
            self.builder.switch_to_block(skip);
            let earlier: Vec<_> = partition
                .watched
                .iter()
                .zip(&old)
                .filter(|(chunk, _)| !chunk.earlier.is_empty())
                .map(|(chunk, &old)| (chunk, chunk.earlier.as_slice(), old))
                .collect();
            self.mark_changed_readers(flags, one, earlier);
            self.builder
                .seal_block(SIRTerminator::Jump(join, Vec::new()));
            self.builder.switch_to_block(compare);
            let all = partition
                .watched
                .iter()
                .zip(&old)
                .map(|(chunk, &old)| (chunk, chunk.readers.as_slice(), old))
                .collect();
            self.mark_changed_readers(flags, one, all);
            self.builder
                .seal_block(SIRTerminator::Jump(join, Vec::new()));
            self.builder.switch_to_block(join);
        }
        if !partition.marks.is_empty() {
            let one = self.builder.alloc_bit(8, false);
            self.builder
                .emit(SIRInstruction::Imm(one, SIRValue::new(1u64)));
            for &reader in &partition.marks {
                self.builder.emit(SIRInstruction::Store(
                    flags,
                    SIROffset::Static(reader * 8),
                    8,
                    one,
                    Vec::new(),
                    Vec::new(),
                ));
            }
        }
        if let Some(merge) = self.partition_merge.take() {
            self.builder
                .seal_block(SIRTerminator::Jump(merge, Vec::new()));
            self.builder.switch_to_block(merge);
        }
        self.lower_cache.clear();
        self.in_partition = false;
    }

    /// Lower the buffered run of exact grouped-fold paths.
    fn flush_pending_folds(&mut self) {
        let context = self.context;
        flush_pending_fold_paths(
            &mut self.pending_fold_indices,
            context.input,
            context.fold_group_schedule_index,
            context.lowerer,
            &mut self.builder,
            context.arena,
            &mut self.lower_cache,
            &mut self.dep_memo,
            &mut self.inverse_dep_memo,
            context.unpacked_element_widths,
            context.four_state,
        );
        self.pending_fold_roots.clear();
    }

    /// Emit SIR for one logic path and keep the lowering cache coherent.
    fn emit_node(&mut self, idx: usize) {
        let context = self.context;
        let path = &context.input[idx];
        collect_logic_path_input_deps(
            path,
            context.arena,
            &mut self.dep_memo,
            &mut self.inverse_dep_memo,
        );
        emit_logic_path_store(
            context.lowerer,
            &mut self.builder,
            path,
            context.arena,
            &mut self.lower_cache,
            context.unpacked_element_widths,
        );
        invalidate_logic_path_target(path, &self.inverse_dep_memo, &mut self.lower_cache);
    }

    /// Lower one scheduled component, selecting static unrolling or dynamic
    /// convergence for cyclic SCCs.
    fn lower<E>(
        &mut self,
        work: &ScheduledWork,
        ff: Option<&mut (dyn ClockFfLowering<Addr, Error = E> + '_)>,
        runtime_errors: &mut HashMap<i64, RuntimeErrorInfo<Addr>>,
        next_runtime_error_code: &mut i64,
    ) -> Result<(), ClockSortError<Addr, E>> {
        let context = self.context;
        let input = context.input;
        let adj = context.adj;
        let component_by_path = context.component_by_path;
        let singleton;
        let scc = match work {
            ScheduledWork::CombPath(path) => {
                singleton = [*path];
                singleton.as_slice()
            }
            ScheduledWork::CombScc(scc) => scc.as_slice(),
            ScheduledWork::GuardedComb { condition, paths } => {
                self.flush_pending_folds();
                emit_scheduled_guard_region(
                    context.lowerer,
                    &mut self.builder,
                    *condition,
                    paths,
                    input,
                    context.arena,
                    &mut self.lower_cache,
                    &mut self.dep_memo,
                    &mut self.inverse_dep_memo,
                    context.unpacked_element_widths,
                );
                return Ok(());
            }
            ScheduledWork::Ff(index) => {
                self.flush_pending_folds();
                ff.ok_or(ClockSortError::Scheduler(
                    SchedulerError::InvalidDependencyGraph,
                ))?
                .lower(
                    *index,
                    &context.direct_ff_writes_by_action[*index],
                    &mut self.builder,
                )
                .map_err(ClockSortError::Lowering)?;
                // Any directly published range is ordered after all of its
                // old-state readers. Other FF state stays invisible in
                // WORKING/SPARSE_WORKING until the final publish, so values
                // computed before the FF CFG keep the ordinary lowering cache
                // valid.
                return Ok(());
            }
        };
        let component = component_by_path[scc[0]];
        let mut user_safety_limit = None;
        for &v_idx in scc {
            for &u_idx in &adj[v_idx] {
                if component_by_path[u_idx] == component {
                    if let (Some(v_target), Some(u_target)) =
                        (input[v_idx].target.var(), input[u_idx].target.var())
                    {
                        let edge = (v_target.id, u_target.id);
                        if let Some(&limit) = context.true_loops.get(&edge) {
                            user_safety_limit =
                                Some(user_safety_limit.map_or(limit, |l: usize| l.max(limit)));
                        }
                    }
                }
            }
        }
        let is_loop = scc.len() > 1 || (scc.len() == 1 && adj[scc[0]].contains(&scc[0]));

        if is_loop {
            // Exact grouped folds are atomic with respect to a loop SCC.
            self.flush_pending_folds();
            let mut authorized = user_safety_limit.is_some();
            'check_scc: for &v_idx in scc {
                for &u_idx in &adj[v_idx] {
                    if component_by_path[u_idx] == component
                        && input[v_idx]
                            .target
                            .var()
                            .zip(input[u_idx].target.var())
                            .is_some_and(|(v, u)| context.ignored_loops.contains(&(v.id, u.id)))
                    {
                        // Some loops are explicitly allowed by the user (e.g., false loops).
                        authorized = true;
                        break 'check_scc;
                    }
                }
            }

            if !authorized {
                return Err(ClockSortError::Scheduler(
                    SchedulerError::CombinationalLoop {
                        blocks: scc.iter().map(|idx| input[*idx].clone()).collect(),
                    },
                ));
            }

            // FAS Sort
            let optimized_scc_order = greedy_fas_sort(scc, adj);
            let force_strategy_b = user_safety_limit.is_some();
            let iterations = calculate_required_iterations(adj, &optimized_scc_order);
            let total_ops_estimate = optimized_scc_order.len().saturating_mul(iterations);
            if !force_strategy_b && total_ops_estimate <= UNROLL_THRESHOLD {
                // Strategy A: Static Unrolling
                // The loop is unrolled a fixed number of times based on structural dependency depth (iterations).
                for _ in 0..iterations {
                    for &idx in &optimized_scc_order {
                        self.emit_node(idx);
                    }
                }
            } else {
                let runtime_error_code = *next_runtime_error_code;
                *next_runtime_error_code += 1;
                let mut seen = HashSet::default();
                let sources = scc
                    .iter()
                    .filter_map(|idx| {
                        let addr = input[*idx].target.var()?.id;
                        seen.insert(addr).then_some(addr)
                    })
                    .collect::<Vec<_>>();
                runtime_errors.insert(
                    runtime_error_code,
                    RuntimeErrorInfo {
                        message: "Detected True Loop".to_string(),
                        signals: sources,
                    },
                );

                // Strategy B: Dynamic Convergence
                // Implements a runtime loop that continues executing the SCC until all signals converge (dirty flag is false).
                // Includes a safety limit to detect non-converging "True Loops" and avoid infinite hang.

                // 1. Determine the runtime repetition limit.
                let safety_limit = user_safety_limit.unwrap_or(iterations + 1);

                // 2. Prepare Constants and Counters
                let zero_reg = self.builder.alloc_bit(64, false);
                self.builder
                    .emit(SIRInstruction::Imm(zero_reg, SIRValue::new(0u64)));

                let limit_reg = self.builder.alloc_bit(64, false);
                self.builder.emit(SIRInstruction::Imm(
                    limit_reg,
                    SIRValue::new(safety_limit as u64),
                ));

                // 3. Blocks
                let current_counter = self.builder.alloc_bit(64, false);
                let header_block = self.builder.new_block_with(vec![current_counter]); // [counter]
                let body_block = self.builder.new_block();
                let exit_block = self.builder.new_block();
                let error_block = self.builder.new_block(); // For True Loop detection

                // Start: Jump to header with counter = 0
                self.builder
                    .seal_block(SIRTerminator::Jump(header_block, vec![zero_reg]));

                // --- Header Block ---
                self.builder.switch_to_block(header_block);

                // Check: counter < safety_limit
                let can_continue_reg = self.builder.alloc_bit(1, false);
                self.builder.emit(SIRInstruction::Binary(
                    can_continue_reg,
                    current_counter,
                    BinaryOp::LtU,
                    limit_reg,
                ));

                // If counter exceeded limit, we might have an oscillating True Loop
                self.builder.seal_block(SIRTerminator::Branch {
                    cond: can_continue_reg,
                    true_block: (body_block, vec![]),
                    false_block: (error_block, vec![]),
                });
                self.builder.switch_to_block(body_block);
                let mut current_dirty_reg = self.builder.alloc_bit(1, false);
                self.builder
                    .emit(SIRInstruction::Imm(current_dirty_reg, SIRValue::new(0u32)));
                for &idx in &optimized_scc_order {
                    let path = &input[idx];
                    let Some(target) = path.target.var() else {
                        emit_logic_path_store(
                            context.lowerer,
                            &mut self.builder,
                            path,
                            context.arena,
                            &mut self.lower_cache,
                            context.unpacked_element_widths,
                        );
                        continue;
                    };
                    let width = 1 + target.access.msb - target.access.lsb;
                    let addr = target.id;
                    let offset = static_access_offset(
                        &target.id,
                        target.access,
                        context.unpacked_element_widths,
                    );

                    // --- Dynamic Convergence Check Logic ---
                    // For each node in the SCC, we verify if its value changed after this iteration.
                    //
                    // a. Load the current value (pre-update benchmark)
                    let old_val_reg = self.builder.alloc_bit(width, false);
                    self.builder.emit(SIRInstruction::Load(
                        old_val_reg,
                        addr,
                        offset.clone(),
                        width,
                    ));
                    collect_logic_path_input_deps(
                        path,
                        context.arena,
                        &mut self.dep_memo,
                        &mut self.inverse_dep_memo,
                    );
                    // b. Compute the new value
                    let new_val_reg = lower_logic_path_expr(
                        context.lowerer,
                        &mut self.builder,
                        path,
                        context.arena,
                        &mut self.lower_cache,
                    );

                    // c. Compare: changed = (old != new)
                    let is_changed_reg = self.builder.alloc_bit(1, false);
                    self.builder.emit(SIRInstruction::Binary(
                        is_changed_reg,
                        old_val_reg,
                        BinaryOp::Ne, // Not Equal
                        new_val_reg,
                    ));
                    let new_dirty_reg = self.builder.alloc_bit(1, false);

                    // d. Accumulate dirty flag: dirty = dirty | is_changed
                    // If any signal in the SCC changes, the entire SCC requires another iteration.
                    self.builder.emit(SIRInstruction::Binary(
                        new_dirty_reg,
                        current_dirty_reg,
                        BinaryOp::Or,
                        is_changed_reg,
                    ));
                    current_dirty_reg = new_dirty_reg;
                    // e. Store the new value
                    self.builder.emit(SIRInstruction::Store(
                        addr,
                        offset,
                        width,
                        new_val_reg,
                        Vec::new(),
                        Vec::new(),
                    ));
                    if !path.comb_capture_enable_sites.is_empty() {
                        let (old, new) = if path.comb_capture_enable_always {
                            let old = self.builder.alloc_bit(1, false);
                            let new = self.builder.alloc_bit(1, false);
                            self.builder
                                .emit(SIRInstruction::Imm(old, SIRValue::new(0u8)));
                            self.builder
                                .emit(SIRInstruction::Imm(new, SIRValue::new(1u8)));
                            (old, new)
                        } else {
                            (old_val_reg, new_val_reg)
                        };
                        self.builder
                            .emit(SIRInstruction::CombCaptureEnableIfChanged {
                                old,
                                new,
                                sites: path.comb_capture_enable_sites.clone(),
                            });
                    }
                    if let Some(to_remove) = self.inverse_dep_memo.get(&addr) {
                        for node in to_remove {
                            self.lower_cache.remove(node);
                        }
                    }
                    // -------------------------------
                }

                // 4. Branch: Loop if dirty
                let one_reg = self.builder.alloc_bit(64, false);
                self.builder
                    .emit(SIRInstruction::Imm(one_reg, SIRValue::new(1u64)));
                let next_counter = self.builder.alloc_bit(64, false);
                self.builder.emit(SIRInstruction::Binary(
                    next_counter,
                    current_counter,
                    BinaryOp::Add,
                    one_reg,
                ));

                // Increment the iteration counter and branch.
                // If 'dirty' is true, return to the header block; otherwise, exit the loop.
                self.builder.seal_block(SIRTerminator::Branch {
                    cond: current_dirty_reg,
                    true_block: (header_block, vec![next_counter]),
                    false_block: (exit_block, vec![]),
                });

                // --- Error/Exit Blocks ---
                self.builder.switch_to_block(error_block);
                // Emit a trap or special instruction to indicate "Combinational Loop Oscillation"
                // self.builder.emit(SIRInstruction::Trap(1));
                self.builder
                    .seal_block(SIRTerminator::Error(runtime_error_code));

                // 5. Exit Block
                self.builder.switch_to_block(exit_block);
            }
        } else {
            // DAG Part — flush before emitting if the EU has grown too large
            if context.split_large_units
                && !self.in_partition
                && self.builder.block_count() >= EU_BLOCK_LIMIT
            {
                self.flush_pending_folds();
                if let Some(eu) = self.builder.flush_eu() {
                    self.execution_units.push(eu);
                    // Clear the lowering cache — register IDs are EU-scoped
                    self.lower_cache.clear();
                }
            }

            let idx = scc[0];
            let exact_fold = is_exact_fold_path(idx, context.fold_group_schedule_index);
            let fold_root = exact_fold
                .then_some(context.fold_group_schedule_index.direct_group_by_path[idx])
                .flatten();
            let depends_on_pending = self
                .pending_fold_indices
                .iter()
                .any(|pending| adj[*pending].binary_search(&idx).is_ok());
            let starts_new_root =
                fold_root.is_some_and(|root| !self.pending_fold_roots.contains(&root));
            let window_full =
                starts_new_root && self.pending_fold_roots.len() >= MAX_JOINT_FOLD_ROOTS;
            if exact_fold && !depends_on_pending && !window_full {
                self.pending_fold_indices.push(idx);
                self.pending_fold_roots.extend(fold_root);
            } else {
                self.flush_pending_folds();
                if exact_fold {
                    self.pending_fold_indices.push(idx);
                    self.pending_fold_roots.extend(fold_root);
                } else if let LogicPathTarget::Process { process, .. } = &input[idx].target
                    && !self.run_processes.insert(*process)
                {
                    // The unit writes every output of its process. Each
                    // projection depends on all its sources, so they were
                    // all written before the first projection ran the unit,
                    // and this projection's activation guard would find them
                    // unchanged.
                } else {
                    self.emit_node(idx);
                }
            }
        }
        Ok(())
    }

    /// Terminate the current unit and return every unit emitted so far.
    fn finish(mut self) -> Vec<ExecutionUnit<Addr>> {
        self.builder.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = self.builder.drain();
        self.execution_units.push(ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        });
        self.execution_units
    }
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
    let lowerer = crate::SLTToSIRLowerer::new(four_state)
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
        lowerer: &lowerer,
        four_state,
        split_large_units: ff.is_none(),
    };
    let mut lowering = WorkLowering::new(&context);
    if let Some(ff_lowering) = ff.as_deref_mut() {
        ff_lowering
            .begin(&mut lowering.builder, &prepared.direct_ff_writes_by_action)
            .map_err(ClockSortError::Lowering)?;
    }
    let mut runtime_errors: HashMap<i64, RuntimeErrorInfo<Addr>> = HashMap::default();
    let mut next_runtime_error_code = first_runtime_error_code;
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
                    lowering.end_partition(
                        *flags,
                        plan.partitions.len(),
                        &plan.partitions[partition - 1],
                    );
                } else {
                    lowering.begin_settle(*flags, plan.partitions.len());
                }
                lowering.begin_partition(
                    *flags,
                    partition,
                    plan.partitions.len(),
                    &plan.partitions[partition],
                );
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
        lowering.end_partition(*flags, plan.partitions.len(), last);
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
    Ok(ScheduleResult {
        execution_units: lowering.finish(),
        runtime_errors,
        direct_ff_writes,
        partitions: plan
            .map(|(plan, _)| {
                plan.partitions
                    .into_iter()
                    .map(|partition| SettlePartition {
                        reads: partition.reads,
                        writes: partition.writes,
                        pinned: partition.pinned,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        stateful_targets,
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
mod tests {
    use num_bigint::{BigInt, BigUint};

    use super::{
        ExactFoldGroup, ExactIndexedLoadKey, FfAccessSummary, FoldGroupReadFacts,
        NormalizedIndexExpr, add_acyclic_ff_write_order_edges, best_weighted_fold_family,
        build_fold_group_schedule_index, build_logic_path_memory_ssa, collect_node_input_deps,
        direct_ff_write_ranges, index_accesses, indexed_overlaps, plan_ff_comb_schedule,
        prepare_atomic_fold_group_results, sort, stable_topological_sccs,
    };
    use crate::{HashMap, HashSet};
    use crate::{
        LogicPath, LogicPathTarget, SLTForFoldGroupState, SLTNode, SLTNodeArena, SLTToSIRLowerer,
    };
    use celox_design::{BinaryOp, BitAccess, VarAtomBase};
    use celox_sir::{SIRBuilder, SIRInstruction, SIROffset, SIRTerminator};

    #[test]
    fn deep_input_dependencies_use_heap_and_preserve_inverse_index() {
        std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(|| {
                let mut arena = SLTNodeArena::<usize>::new();
                let input = |variable| SLTNode::Input {
                    variable,
                    index: Vec::new(),
                    access: BitAccess::new(0, 0),
                    signed: false,
                };
                let data = arena.alloc(input(0)).unwrap();
                let cond = arena.alloc(input(1)).unwrap();
                let mut root = data;
                for _ in 0..8192 {
                    root = arena
                        .alloc(SLTNode::Mux {
                            cond,
                            then_expr: root,
                            else_expr: data,
                        })
                        .unwrap();
                }
                let mut memo = HashMap::default();
                let mut inverse = HashMap::default();
                let deps = collect_node_input_deps(root, &arena, &mut memo, &mut inverse);
                assert_eq!(deps, HashSet::from_iter([0, 1]));
                assert!(inverse[&0].contains(&root));
                assert!(inverse[&1].contains(&root));
                assert_eq!(memo.len(), 8194);
                assert_eq!(
                    collect_node_input_deps(root, &arena, &mut memo, &mut inverse),
                    deps
                );
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn stable_scc_order_preserves_source_order_when_dependencies_allow_it() {
        // 0 and 1 are initially ready, both feed 2, and 2 feeds 3. Tarjan is
        // free to return components in any order; lowering remains stable.
        let adj = vec![vec![2], vec![2], vec![3], vec![]];
        let unordered = vec![vec![3], vec![1], vec![2], vec![0]];

        let (ordered, component_by_path) =
            stable_topological_sccs(unordered, &adj, &[None; 4]).unwrap();

        assert_eq!(ordered, vec![vec![0], vec![1], vec![2], vec![3]]);
        assert_eq!(component_by_path.len(), adj.len());
    }

    #[test]
    fn stable_scc_order_drains_only_ready_members_of_an_effect_domain() {
        let adj = vec![vec![], vec![], vec![], vec![]];
        let unordered = vec![vec![3], vec![1], vec![2], vec![0]];

        let (ordered, _) =
            stable_topological_sccs(unordered, &adj, &[Some(0), Some(1), Some(0), Some(1)])
                .unwrap();

        assert_eq!(ordered, vec![vec![0], vec![2], vec![1], vec![3]]);
    }

    #[test]
    fn effect_domain_preference_never_pulls_a_path_across_its_dependency() {
        // Path 1 shares path 0's destination domain, but it is not ready until
        // the independent path 2 has executed.
        let adj = vec![vec![], vec![], vec![1]];
        let unordered = vec![vec![1], vec![2], vec![0]];

        let (ordered, _) =
            stable_topological_sccs(unordered, &adj, &[Some(0), Some(0), Some(1)]).unwrap();

        assert_eq!(ordered, vec![vec![0], vec![2], vec![1]]);
    }

    #[test]
    fn id_indexed_overlap_probes_match_a_naive_scan() {
        // The scheduling passes index one side of each overlap test by object
        // id. A probe must agree with scanning the whole access list, including
        // when objects are interleaved or a probe shares no object with it.
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..256 {
            let accesses = (0..(next() % 12) as usize)
                .map(|_| {
                    let lsb = (next() % 32) as usize;
                    VarAtomBase::new((next() % 5) as u32, lsb, lsb + (next() % 8) as usize)
                })
                .collect::<Vec<_>>();
            let probe_lsb = (next() % 32) as usize;
            let probe = VarAtomBase::new(
                (next() % 5) as u32,
                probe_lsb,
                probe_lsb + (next() % 8) as usize,
            );

            let index = index_accesses(&accesses);
            let naive = accesses
                .iter()
                .any(|access| access.id == probe.id && access.access.overlaps(&probe.access));
            assert_eq!(indexed_overlaps(&index, &probe), naive);
        }
    }

    fn simple_path(
        arena: &mut SLTNodeArena<u32>,
        target: u32,
        source: Option<u32>,
    ) -> LogicPath<u32> {
        let expr = if let Some(source) = source {
            arena
                .alloc(SLTNode::Input {
                    variable: source,
                    signed: false,
                    index: Vec::new(),
                    access: BitAccess::new(0, 7),
                })
                .unwrap()
        } else {
            arena
                .alloc(SLTNode::Constant(
                    BigUint::from(target),
                    BigUint::from(0u8),
                    8,
                    false,
                ))
                .unwrap()
        };
        LogicPath {
            transparent_copy: false,
            target: LogicPathTarget::Var(VarAtomBase::new(target, 0, 7)),
            sources: source
                .map(|source| [VarAtomBase::new(source, 0, 7)].into_iter().collect())
                .unwrap_or_default(),
            previous_sources: crate::HashSet::default(),
            address_sources: crate::HashSet::default(),
            local_inputs: Vec::new(),
            order_before: crate::HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
            expr,
        }
    }

    #[test]
    fn scheduled_paths_share_one_high_level_guard_without_changing_store_order() {
        let mut arena = SLTNodeArena::new();
        let condition = arena
            .alloc(SLTNode::Input {
                variable: 100,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 0),
            })
            .unwrap();
        let mut paths = Vec::new();
        for target in 0u32..12 {
            let then_expr = arena
                .alloc(SLTNode::Constant(
                    BigUint::from(target),
                    BigUint::from(0u8),
                    8,
                    false,
                ))
                .unwrap();
            let else_expr = arena
                .alloc(SLTNode::Constant(
                    BigUint::from(target + 32),
                    BigUint::from(0u8),
                    8,
                    false,
                ))
                .unwrap();
            let expr = arena
                .alloc(SLTNode::Mux {
                    cond: condition,
                    then_expr,
                    else_expr,
                })
                .unwrap();
            paths.push(LogicPath {
                transparent_copy: false,
                target: LogicPathTarget::Var(VarAtomBase::new(target, 0, 7)),
                sources: [VarAtomBase::new(100, 0, 0)].into_iter().collect(),
                previous_sources: crate::HashSet::default(),
                address_sources: crate::HashSet::default(),
                local_inputs: Vec::new(),
                order_before: crate::HashSet::default(),
                comb_capture_enable_sites: Vec::new(),
                comb_capture_enable_always: false,
                pre_lower_nodes: Vec::new(),
                expr,
            });
        }

        let result = sort(
            paths,
            &arena,
            &crate::HashSet::default(),
            &crate::HashMap::default(),
            false,
            &crate::HashMap::default(),
            0,
        )
        .unwrap();
        let eu = &result.execution_units[0];
        let branches = eu
            .blocks
            .values()
            .filter(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
            .collect::<Vec<_>>();
        assert_eq!(branches.len(), 1);
        assert_eq!(
            eu.blocks
                .values()
                .flat_map(|block| &block.instructions)
                .filter(|instruction| matches!(instruction, SIRInstruction::Mux(..)))
                .count(),
            0
        );

        let SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } = &branches[0].terminator
        else {
            unreachable!()
        };
        for block in [true_block.0, false_block.0] {
            let stores = eu.blocks[&block]
                .instructions
                .iter()
                .filter_map(|instruction| match instruction {
                    SIRInstruction::Store(target, ..) => Some(*target),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(stores, (0..12).collect::<Vec<_>>());
        }
    }

    #[test]
    fn scheduled_outer_guard_keeps_nested_mux_lowering_decisions() {
        let mut arena = SLTNodeArena::new();
        let outer_condition = arena
            .alloc(SLTNode::Input {
                variable: 100,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 0),
            })
            .unwrap();
        let inner_condition = arena
            .alloc(SLTNode::Input {
                variable: 101,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 0),
            })
            .unwrap();
        let input_value = arena
            .alloc(SLTNode::Input {
                variable: 102,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let one = arena
            .alloc(SLTNode::Constant(
                BigUint::from(1u8),
                BigUint::from(0u8),
                8,
                false,
            ))
            .unwrap();
        let mut inner_true = input_value;
        let mut inner_false = input_value;
        for _ in 0..16 {
            inner_true = arena
                .alloc(SLTNode::Binary(inner_true, BinaryOp::Add, one))
                .unwrap();
            inner_false = arena
                .alloc(SLTNode::Binary(inner_false, BinaryOp::Xor, one))
                .unwrap();
        }
        let nested = arena
            .alloc(SLTNode::Mux {
                cond: inner_condition,
                then_expr: inner_true,
                else_expr: inner_false,
            })
            .unwrap();

        let mut paths = Vec::new();
        for target in 0u32..12 {
            let then_expr = if target == 0 {
                nested
            } else {
                arena
                    .alloc(SLTNode::Constant(
                        BigUint::from(target),
                        BigUint::from(0u8),
                        8,
                        false,
                    ))
                    .unwrap()
            };
            let else_expr = arena
                .alloc(SLTNode::Constant(
                    BigUint::from(target + 32),
                    BigUint::from(0u8),
                    8,
                    false,
                ))
                .unwrap();
            let expr = arena
                .alloc(SLTNode::Mux {
                    cond: outer_condition,
                    then_expr,
                    else_expr,
                })
                .unwrap();
            paths.push(LogicPath {
                transparent_copy: false,
                target: LogicPathTarget::Var(VarAtomBase::new(target, 0, 7)),
                sources: [
                    VarAtomBase::new(100, 0, 0),
                    VarAtomBase::new(101, 0, 0),
                    VarAtomBase::new(102, 0, 7),
                ]
                .into_iter()
                .collect(),
                previous_sources: crate::HashSet::default(),
                address_sources: crate::HashSet::default(),
                local_inputs: Vec::new(),
                order_before: crate::HashSet::default(),
                comb_capture_enable_sites: Vec::new(),
                comb_capture_enable_always: false,
                pre_lower_nodes: Vec::new(),
                expr,
            });
        }

        let result = sort(
            paths,
            &arena,
            &crate::HashSet::default(),
            &crate::HashMap::default(),
            false,
            &crate::HashMap::default(),
            0,
        )
        .unwrap();
        let branch_count = result.execution_units[0]
            .blocks
            .values()
            .filter(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
            .count();
        assert_eq!(
            branch_count, 2,
            "outer region and nested Mux must both branch"
        );
    }

    #[test]
    fn ff_plan_models_comb_and_ff_old_state_readers_before_writer() {
        let mut arena = SLTNodeArena::new();
        let comb_value = simple_path(&mut arena, 10, None);
        let comb_old_state_reader = simple_path(&mut arena, 11, Some(20));
        let ff = vec![
            FfAccessSummary {
                reads: vec![VarAtomBase::new(10, 0, 7), VarAtomBase::new(11, 0, 7)],
                writes: vec![VarAtomBase::new(20, 0, 7)],
                dynamic_writes: HashSet::default(),
            },
            FfAccessSummary {
                reads: vec![VarAtomBase::new(20, 0, 7)],
                writes: vec![VarAtomBase::new(30, 0, 7)],
                dynamic_writes: HashSet::default(),
            },
        ];

        let plan = plan_ff_comb_schedule(&[comb_value, comb_old_state_reader], &ff).unwrap();

        assert_eq!(plan.required_comb, vec![true, true]);
        assert_eq!(plan.comb_value_predecessors, vec![vec![0, 1], vec![]]);
        assert_eq!(plan.comb_before_direct_write, vec![vec![1], vec![]]);
        assert_eq!(plan.ff_before_direct_write, vec![vec![1], vec![]]);
    }

    #[test]
    fn ff_plan_keeps_same_recipe_old_state_reads_local_to_lowering() {
        let summary = FfAccessSummary {
            reads: vec![VarAtomBase::new(20, 0, 7)],
            writes: vec![VarAtomBase::new(20, 0, 7)],
            dynamic_writes: HashSet::default(),
        };

        let plan = plan_ff_comb_schedule(&[], &[summary]).unwrap();

        assert_eq!(plan.ff_before_direct_write, vec![Vec::<usize>::new()]);
    }

    #[test]
    fn ff_direct_write_proof_is_kept_per_bit_range() {
        let summaries = vec![
            FfAccessSummary {
                reads: vec![VarAtomBase::new(30, 0, 7)],
                writes: vec![VarAtomBase::new(20, 0, 7), VarAtomBase::new(20, 8, 15)],
                dynamic_writes: HashSet::default(),
            },
            FfAccessSummary {
                reads: vec![VarAtomBase::new(20, 0, 7)],
                writes: vec![VarAtomBase::new(30, 0, 7)],
                dynamic_writes: HashSet::default(),
            },
        ];
        let plan = plan_ff_comb_schedule(&[], &summaries).unwrap();
        // The FF0 -> FF1 edge is retained, while the inverse edge which only
        // protects FF0's low byte is dropped to break the cycle.
        let retained = [(0, 1)].into_iter().collect();
        let var_widths = [(20, 16), (30, 8)].into_iter().collect();

        let direct = direct_ff_write_ranges(
            &[],
            &summaries,
            &plan,
            &retained,
            0,
            &var_widths,
            &HashMap::default(),
        );

        assert_eq!(direct[0], vec![VarAtomBase::new(20, 8, 15)]);
        assert_eq!(direct[1], vec![VarAtomBase::new(30, 0, 7)]);

        let local_old_read = vec![FfAccessSummary {
            reads: vec![VarAtomBase::new(20, 0, 7)],
            writes: vec![VarAtomBase::new(20, 0, 7), VarAtomBase::new(20, 8, 15)],
            dynamic_writes: HashSet::default(),
        }];
        let local_plan = plan_ff_comb_schedule(&[], &local_old_read).unwrap();
        let local_direct = direct_ff_write_ranges(
            &[],
            &local_old_read,
            &local_plan,
            &HashSet::default(),
            0,
            &var_widths,
            &HashMap::default(),
        );

        assert_eq!(local_direct[0], vec![VarAtomBase::new(20, 8, 15)]);
    }

    #[test]
    fn ff_direct_write_rejects_ranges_requiring_rmw() {
        let summary = FfAccessSummary {
            reads: Vec::new(),
            writes: vec![
                VarAtomBase::new(20, 0, 3),
                VarAtomBase::new(21, 0, 0),
                VarAtomBase::new(22, 0, 7),
                VarAtomBase::new(23, 6, 11),
                VarAtomBase::new(24, 8, 15),
            ],
            dynamic_writes: [22].into_iter().collect(),
        };
        let plan = plan_ff_comb_schedule(&[], std::slice::from_ref(&summary)).unwrap();
        let var_widths = [(20, 8), (21, 1), (22, 8), (23, 24), (24, 24)]
            .into_iter()
            .collect();
        let unpacked_element_widths = [(23, 6), (24, 8)].into_iter().collect();

        let direct = direct_ff_write_ranges(
            &[],
            &[summary],
            &plan,
            &HashSet::default(),
            0,
            &var_widths,
            &unpacked_element_widths,
        );

        assert_eq!(
            direct[0],
            vec![VarAtomBase::new(21, 0, 0), VarAtomBase::new(24, 8, 15)]
        );
    }

    #[test]
    fn ff_direct_write_stages_complete_overlapping_components() {
        let summary = FfAccessSummary {
            reads: Vec::new(),
            writes: vec![
                VarAtomBase::new(20, 4, 19),
                VarAtomBase::new(20, 16, 31),
                VarAtomBase::new(20, 24, 31),
                VarAtomBase::new(20, 40, 47),
            ],
            dynamic_writes: HashSet::default(),
        };
        let plan = plan_ff_comb_schedule(&[], std::slice::from_ref(&summary)).unwrap();
        let var_widths = [(20, 64)].into_iter().collect();

        let direct = direct_ff_write_ranges(
            &[],
            &[summary],
            &plan,
            &HashSet::default(),
            0,
            &var_widths,
            &HashMap::default(),
        );

        // The unaligned first write is staged. Staging propagates through the
        // two transitively overlapping native ranges, but not the disjoint byte.
        assert_eq!(direct[0], vec![VarAtomBase::new(20, 40, 47)]);
    }

    #[test]
    fn cyclic_direct_write_preference_falls_back_to_staging() {
        let mut adj = vec![vec![1], Vec::new(), Vec::new()];

        add_acyclic_ff_write_order_edges(&mut adj, [(1, 0), (1, 2)]);

        assert_eq!(adj, vec![vec![1], vec![2], Vec::new()]);
    }

    fn partitioned_sort(
        paths: Vec<LogicPath<u32>>,
        arena: &SLTNodeArena<u32>,
    ) -> super::ScheduleResult<u32> {
        let widths = (10..14).map(|id| (id, 8)).collect();
        super::sort_with_unpacked_element_widths(
            paths,
            arena,
            &crate::HashSet::default(),
            &crate::HashMap::default(),
            false,
            &widths,
            &crate::HashMap::default(),
            1,
            Some(super::SettlePartitioning {
                flags: 99,
                target_cost: 1,
            }),
        )
        .unwrap()
    }

    #[test]
    fn settle_partitions_follow_schedule_and_guard_on_their_flags() {
        let mut arena = SLTNodeArena::new();
        let paths = vec![
            simple_path(&mut arena, 13, Some(12)),
            simple_path(&mut arena, 12, Some(11)),
            simple_path(&mut arena, 11, Some(10)),
        ];
        let result = partitioned_sort(paths, &arena);
        let plan = result
            .partitions
            .iter()
            .map(|partition| (partition.reads.clone(), partition.writes.clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            plan,
            vec![
                (vec![10], vec![11]),
                (vec![11], vec![12]),
                (vec![12], vec![13]),
            ]
        );
        assert!(result.partitions.iter().all(|partition| !partition.pinned));
        // The settle starts by reading the untracked byte (24) to decide the
        // full byte (32). Each partition loads its own flag and the full
        // byte, and sets its reader's flag when it changed what it wrote.
        let mut loaded = Vec::new();
        let mut stored = Vec::new();
        let mut compares = 0;
        for unit in &result.execution_units {
            for block in unit.blocks.values() {
                for instruction in &block.instructions {
                    match instruction {
                        SIRInstruction::Load(_, 99, SIROffset::Static(byte), 8) => {
                            loaded.push(*byte)
                        }
                        SIRInstruction::Store(99, SIROffset::Static(byte), 8, ..) => {
                            stored.push(*byte)
                        }
                        SIRInstruction::Binary(_, _, BinaryOp::NeCase, _) => compares += 1,
                        _ => {}
                    }
                }
            }
        }
        loaded.sort_unstable();
        stored.sort_unstable();
        // Guards of 0, 8 and 16, the readers' flags of partitions 1 and 2
        // before a conditional mark, the untracked byte, and the full byte
        // for the three guards and the two partitions that compare.
        assert_eq!(loaded, vec![0, 8, 8, 16, 16, 24, 32, 32, 32, 32, 32]);
        // The clears of all three flags, the marks of partitions 1 and 2,
        // and the full byte.
        assert_eq!(stored, vec![0, 8, 8, 16, 16, 32]);
        // 11 and 12 are compared before their readers are marked.
        assert_eq!(compares, 2);
    }

    #[test]
    fn settle_copy_readers_read_the_copied_source() {
        let mut arena = SLTNodeArena::new();
        let mut copy = simple_path(&mut arena, 12, Some(11));
        copy.transparent_copy = true;
        let paths = vec![
            simple_path(&mut arena, 13, Some(12)),
            copy,
            simple_path(&mut arena, 11, Some(10)),
        ];
        let result = partitioned_sort(paths, &arena);
        let plan = result
            .partitions
            .iter()
            .map(|partition| (partition.reads.clone(), partition.writes.clone()))
            .collect::<Vec<_>>();
        // The copy is guarded like other work, and its reader reads the
        // copied source too.
        assert_eq!(
            plan,
            vec![
                (vec![10], vec![11]),
                (vec![11], vec![12]),
                (vec![11, 12], vec![13]),
            ]
        );
    }

    #[test]
    fn memory_ssa_scheduler_lowers_independent_single_use_chains_contiguously() {
        let mut arena = SLTNodeArena::new();
        let paths = vec![
            simple_path(&mut arena, 10, None),
            simple_path(&mut arena, 20, None),
            simple_path(&mut arena, 11, Some(10)),
            simple_path(&mut arena, 21, Some(20)),
            simple_path(&mut arena, 12, Some(11)),
            simple_path(&mut arena, 22, Some(21)),
        ];
        let result = sort(
            paths,
            &arena,
            &crate::HashSet::default(),
            &crate::HashMap::default(),
            false,
            &crate::HashMap::default(),
            1,
        )
        .unwrap();
        let unit = &result.execution_units[0];
        let stores = unit.blocks[&unit.entry_block_id]
            .instructions
            .iter()
            .filter_map(|instruction| match instruction {
                SIRInstruction::Store(address, ..) => Some(*address),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(stores, vec![10, 11, 12, 20, 21, 22]);
    }

    #[test]
    fn previous_value_use_is_an_order_edge_not_a_forwarded_value() {
        let mut arena = SLTNodeArena::new();
        let mut previous_user = simple_path(&mut arena, 20, Some(10));
        previous_user.sources.clear();
        previous_user.previous_sources = [VarAtomBase::new(10, 0, 7)].into_iter().collect();
        let writer = simple_path(&mut arena, 10, None);
        let paths = vec![previous_user, writer];

        let memory_ssa = build_logic_path_memory_ssa(&paths).unwrap();
        assert_eq!(memory_ssa.dependencies.users, vec![vec![1], vec![]]);
        assert_eq!(memory_ssa.dependencies.predecessors, vec![vec![], vec![0]]);
        assert_eq!(
            memory_ssa.values.users,
            vec![Vec::<usize>::new(), Vec::new()]
        );
        assert_eq!(
            memory_ssa.values.predecessors,
            vec![Vec::<usize>::new(), Vec::new()]
        );

        let result = sort(
            paths,
            &arena,
            &crate::HashSet::default(),
            &crate::HashMap::default(),
            false,
            &crate::HashMap::default(),
            1,
        )
        .unwrap();
        let unit = &result.execution_units[0];
        let stores = unit.blocks[&unit.entry_block_id]
            .instructions
            .iter()
            .filter_map(|instruction| match instruction {
                SIRInstruction::Store(address, ..) => Some(*address),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(stores, vec![20, 10]);
    }

    #[test]
    fn memory_use_depends_on_each_overlapping_bit_range_definition() {
        let mut arena = SLTNodeArena::new();
        let mut low = simple_path(&mut arena, 10, None);
        low.target = LogicPathTarget::Var(VarAtomBase::new(10, 0, 3));
        let mut high = simple_path(&mut arena, 10, None);
        high.target = LogicPathTarget::Var(VarAtomBase::new(10, 4, 7));
        let mut user = simple_path(&mut arena, 20, Some(10));
        user.sources = [VarAtomBase::new(10, 2, 5)].into_iter().collect();

        let memory_ssa = build_logic_path_memory_ssa(&[low, high, user]).unwrap();
        assert_eq!(
            memory_ssa.dependencies.users,
            vec![vec![2], vec![2], vec![]]
        );
        assert_eq!(
            memory_ssa.dependencies.predecessors,
            vec![vec![], vec![], vec![0, 1]]
        );
        assert_eq!(memory_ssa.values.users, vec![vec![2], vec![2], vec![]]);
        assert_eq!(
            memory_ssa.values.predecessors,
            vec![vec![], vec![], vec![0, 1]]
        );
    }

    fn fixed_group_path(
        arena: &mut SLTNodeArena<u32>,
        guard: crate::NodeId,
        loop_var: u32,
        target: u32,
        external: u32,
        trip_count: usize,
    ) -> LogicPath<u32> {
        fixed_group_path_with_index(
            arena,
            guard,
            loop_var,
            target,
            external,
            trip_count,
            1,
            BitAccess::new(0, 7),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn fixed_group_path_with_index(
        arena: &mut SLTNodeArena<u32>,
        guard: crate::NodeId,
        loop_var: u32,
        target: u32,
        external: u32,
        trip_count: usize,
        index_scale: u8,
        load_access: BitAccess,
    ) -> LogicPath<u32> {
        let target = VarAtomBase::new(target, 0, 7);
        let initial = arena
            .alloc(SLTNode::Input {
                variable: target.id,
                signed: false,
                index: Vec::new(),
                access: target.access,
            })
            .unwrap();
        let loop_index = arena
            .alloc(SLTNode::Input {
                variable: loop_var,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let loop_index = if index_scale == 1 {
            loop_index
        } else {
            let scale = arena
                .alloc(SLTNode::Constant(
                    BigUint::from(index_scale),
                    BigUint::from(0u8),
                    8,
                    false,
                ))
                .unwrap();
            arena
                .alloc(SLTNode::Binary(loop_index, BinaryOp::Mul, scale))
                .unwrap()
        };
        let update = arena
            .alloc(SLTNode::Input {
                variable: external,
                signed: false,
                index: vec![
                    serde_json::from_value(serde_json::json!({
                        "node": loop_index,
                        "stride": 8,
                        "kind": "Packed",
                    }))
                    .unwrap(),
                ],
                access: load_access,
            })
            .unwrap();
        let group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target,
                    initial,
                    update,
                }],
            })
            .unwrap();
        LogicPath {
            transparent_copy: false,
            target: LogicPathTarget::Var(target),
            sources: [VarAtomBase::new(external, 0, 63)].into_iter().collect(),
            previous_sources: crate::HashSet::default(),
            address_sources: crate::HashSet::default(),
            local_inputs: Vec::new(),
            order_before: crate::HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
            expr: group,
        }
    }

    fn fixed_group_fixture(
        left_trip_count: usize,
        right_trip_count: usize,
    ) -> (
        SLTNodeArena<u32>,
        Vec<LogicPath<u32>>,
        crate::HashMap<u32, usize>,
    ) {
        let mut arena = SLTNodeArena::new();
        let guard = arena
            .alloc(SLTNode::Constant(
                BigUint::from(1u8),
                BigUint::from(0u8),
                1,
                false,
            ))
            .unwrap();
        let paths = vec![
            fixed_group_path(&mut arena, guard, 100, 10, 50, left_trip_count),
            fixed_group_path(&mut arena, guard, 101, 11, 50, right_trip_count),
        ];
        let widths = [(10, 8), (11, 8)].into_iter().collect();
        (arena, paths, widths)
    }

    fn schedule_branch_count(result: &super::ScheduleResult<u32>) -> usize {
        result
            .execution_units
            .iter()
            .flat_map(|unit| unit.blocks.values())
            .filter(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
            .count()
    }

    fn synthetic_exact_load(base: u32) -> ExactIndexedLoadKey<u32> {
        ExactIndexedLoadKey {
            base,
            access: BitAccess::new(0, 7),
            index: vec![(
                NormalizedIndexExpr::LoopValue {
                    signed: false,
                    access: BitAccess::new(0, 7),
                },
                8,
            )],
        }
    }

    fn synthetic_fold_group(
        root: usize,
        target: u32,
        loop_var: u32,
        indexed_loads: impl IntoIterator<Item = ExactIndexedLoadKey<u32>>,
        carried_chunks: u128,
    ) -> ExactFoldGroup<u32> {
        ExactFoldGroup {
            root: crate::NodeId(root),
            facts: FoldGroupReadFacts {
                loop_var,
                state_targets: vec![VarAtomBase::new(target, 0, 7)],
                guard_reads: Vec::new(),
                initial_reads: Vec::new(),
                update_reads: Vec::new(),
                indexed_loads: indexed_loads.into_iter().collect(),
                carried_chunks,
            },
        }
    }

    #[test]
    fn independent_exact_fold_groups_lower_jointly_and_keep_store_order() {
        let (arena, paths, widths) = fixed_group_fixture(4, 4);
        let result = sort(
            paths,
            &arena,
            &crate::HashSet::default(),
            &crate::HashMap::default(),
            false,
            &widths,
            1,
        )
        .unwrap();

        assert_eq!(schedule_branch_count(&result), 2);
        let store_block = result
            .execution_units
            .iter()
            .flat_map(|unit| unit.blocks.values())
            .find(|block| {
                block
                    .instructions
                    .iter()
                    .filter(|instruction| matches!(instruction, SIRInstruction::Store(..)))
                    .count()
                    == 2
            })
            .expect("joint results must be materialized before the ordered stores");
        let stores = store_block
            .instructions
            .iter()
            .filter_map(|instruction| match instruction {
                SIRInstruction::Store(address, _, _, _, _, _) => Some(*address),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(stores, vec![10, 11]);
    }

    #[test]
    fn different_index_expression_or_load_slice_prevents_joint_lowering() {
        let mut arena = SLTNodeArena::new();
        let guard = arena
            .alloc(SLTNode::Constant(
                BigUint::from(1u8),
                BigUint::from(0u8),
                1,
                false,
            ))
            .unwrap();
        let paths = vec![
            fixed_group_path_with_index(&mut arena, guard, 100, 10, 50, 4, 1, BitAccess::new(0, 7)),
            fixed_group_path_with_index(&mut arena, guard, 101, 11, 50, 4, 2, BitAccess::new(0, 7)),
            fixed_group_path_with_index(
                &mut arena,
                guard,
                102,
                12,
                50,
                4,
                1,
                BitAccess::new(8, 15),
            ),
        ];
        let widths = [(10, 8), (11, 8), (12, 8)].into_iter().collect();
        let result = sort(
            paths,
            &arena,
            &crate::HashSet::default(),
            &crate::HashMap::default(),
            false,
            &widths,
            1,
        )
        .unwrap();

        assert_eq!(schedule_branch_count(&result), 6);
    }

    #[test]
    fn weighted_family_selection_ignores_conflicting_first_root() {
        let shared = synthetic_exact_load(50);
        let candidates = vec![
            synthetic_fold_group(0, 10, 100, [shared.clone()], 1),
            synthetic_fold_group(1, 11, 101, [shared.clone()], 1),
            synthetic_fold_group(2, 12, 102, [shared], 1),
        ];
        let available = vec![true; 3];
        let compatible = vec![
            vec![false, false, false],
            vec![false, false, true],
            vec![false, true, false],
        ];
        let shared_load = vec![
            vec![false, true, true],
            vec![true, false, true],
            vec![true, true, false],
        ];

        let family = best_weighted_fold_family(
            &candidates,
            &available,
            &compatible,
            &shared_load,
            &crate::HashSet::default(),
            false,
        )
        .expect("the compatible B+C family has positive net benefit");
        let mut roots = family
            .members
            .iter()
            .map(|member| candidates[*member].root.0)
            .collect::<Vec<_>>();
        roots.sort_unstable();
        assert_eq!(roots, vec![1, 2]);
    }

    #[test]
    fn weighted_family_selection_rejects_benefit_not_exceeding_pressure() {
        let shared = synthetic_exact_load(50);
        let candidates = vec![
            synthetic_fold_group(0, 10, 100, [shared.clone()], 8),
            synthetic_fold_group(1, 11, 101, [shared], 8),
        ];
        let available = vec![true; 2];
        let compatible = vec![vec![false, true], vec![true, false]];
        let shared_load = compatible.clone();

        assert!(
            best_weighted_fold_family(
                &candidates,
                &available,
                &compatible,
                &shared_load,
                &crate::HashSet::default(),
                false,
            )
            .is_none()
        );
    }

    #[test]
    fn dependency_edge_separates_otherwise_joint_fold_groups() {
        let (arena, mut paths, widths) = fixed_group_fixture(4, 4);
        paths[1].sources.insert(VarAtomBase::new(10, 0, 7));
        let result = sort(
            paths,
            &arena,
            &crate::HashSet::default(),
            &crate::HashMap::default(),
            false,
            &widths,
            1,
        )
        .unwrap();

        assert_eq!(schedule_branch_count(&result), 4);
    }

    #[test]
    fn different_fold_group_domains_do_not_lower_jointly() {
        let (arena, paths, widths) = fixed_group_fixture(4, 5);
        let result = sort(
            paths,
            &arena,
            &crate::HashSet::default(),
            &crate::HashMap::default(),
            false,
            &widths,
            1,
        )
        .unwrap();

        assert_eq!(schedule_branch_count(&result), 4);
    }

    #[test]
    fn joint_fold_preparation_does_not_mutate_logic_path_metadata() {
        let (arena, mut paths, _) = fixed_group_fixture(4, 4);
        paths[0].previous_sources = [VarAtomBase::new(60, 0, 7)].into_iter().collect();
        paths[0].sources.insert(VarAtomBase::new(61, 0, 7));
        paths[0].address_sources = [VarAtomBase::new(61, 0, 7)].into_iter().collect();
        paths[0].comb_capture_enable_sites = vec![3, 7];
        let snapshot = paths.clone();
        let mut builder = SIRBuilder::new();
        let mut cache = crate::HashMap::default();
        let mut dependencies = crate::HashMap::default();
        let mut inverse_dependencies = crate::HashMap::default();
        let schedule_index = build_fold_group_schedule_index(&paths, &arena);

        let prepared = prepare_atomic_fold_group_results(
            &[0, 1],
            &paths,
            &schedule_index,
            &SLTToSIRLowerer::new(false),
            &mut builder,
            &arena,
            &mut cache,
            &mut dependencies,
            &mut inverse_dependencies,
            false,
        );

        assert_eq!(prepared.len(), 2);
        assert_eq!(paths, snapshot);
    }

    #[test]
    fn for_fold_group_dependencies_keep_initial_but_hide_loop_scoped_updates() {
        let mut arena = SLTNodeArena::<u32>::new();
        let input = |arena: &mut SLTNodeArena<u32>, variable| {
            arena
                .alloc(SLTNode::Input {
                    variable,
                    signed: false,
                    index: Vec::new(),
                    access: BitAccess::new(0, 7),
                })
                .unwrap()
        };
        let guard = arena
            .alloc(SLTNode::Constant(
                BigUint::from(1u8),
                BigUint::from(0u8),
                1,
                false,
            ))
            .unwrap();
        let initial = input(&mut arena, 2);
        let state_input = input(&mut arena, 2);
        let loop_input = input(&mut arena, 1);
        let external_input = input(&mut arena, 3);
        let scoped_sum = arena
            .alloc(SLTNode::Binary(state_input, BinaryOp::Add, loop_input))
            .unwrap();
        let update = arena
            .alloc(SLTNode::Binary(scoped_sum, BinaryOp::Add, external_input))
            .unwrap();
        let group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 1,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 2,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: VarAtomBase::new(2, 0, 7),
                    initial,
                    update,
                }],
            })
            .unwrap();
        let mut memo = crate::HashMap::default();
        let mut inverse_memo = crate::HashMap::default();

        let dependencies = collect_node_input_deps(group, &arena, &mut memo, &mut inverse_memo);

        assert!(
            dependencies.contains(&2),
            "initial state is an external dependency"
        );
        assert!(
            dependencies.contains(&3),
            "ordinary update input remains external"
        );
        assert!(
            !dependencies.contains(&1),
            "loop variable is supplied by the fold"
        );
    }

    #[test]
    fn partial_for_fold_group_state_keeps_uncovered_variable_dependency() {
        let mut arena = SLTNodeArena::<u32>::new();
        let guard = arena
            .alloc(SLTNode::Constant(
                BigUint::from(1u8),
                BigUint::from(0u8),
                1,
                false,
            ))
            .unwrap();
        let initial = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0u8),
                BigUint::from(0u8),
                8,
                false,
            ))
            .unwrap();
        let carried = arena
            .alloc(SLTNode::Input {
                variable: 2,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let uncovered = arena
            .alloc(SLTNode::Input {
                variable: 2,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(8, 15),
            })
            .unwrap();
        let update = arena
            .alloc(SLTNode::Binary(carried, BinaryOp::Add, uncovered))
            .unwrap();
        let group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 1,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 2,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: VarAtomBase::new(2, 0, 7),
                    initial,
                    update,
                }],
            })
            .unwrap();
        let mut memo = crate::HashMap::default();
        let mut inverse_memo = crate::HashMap::default();

        let dependencies = collect_node_input_deps(group, &arena, &mut memo, &mut inverse_memo);
        assert!(
            dependencies.contains(&2),
            "the uncovered high byte remains an external dependency"
        );
    }

    #[test]
    fn shared_for_fold_group_projections_materialize_once_and_store_sequentially() {
        let mut arena = SLTNodeArena::<u32>::new();
        let guard = arena
            .alloc(SLTNode::Constant(
                BigUint::from(1u8),
                BigUint::from(0u8),
                1,
                false,
            ))
            .unwrap();
        let input = |arena: &mut SLTNodeArena<u32>, variable| {
            arena
                .alloc(SLTNode::Input {
                    variable,
                    signed: false,
                    index: Vec::new(),
                    access: BitAccess::new(0, 7),
                })
                .unwrap()
        };
        let initial_a = input(&mut arena, 1);
        let initial_b = input(&mut arena, 2);
        let previous_a = input(&mut arena, 1);
        let previous_b = input(&mut arena, 2);
        let group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 3,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 3,
                entry_guard: guard,
                states: vec![
                    SLTForFoldGroupState {
                        target: VarAtomBase::new(1, 0, 7),
                        initial: initial_a,
                        update: previous_b,
                    },
                    SLTForFoldGroupState {
                        target: VarAtomBase::new(2, 0, 7),
                        initial: initial_b,
                        update: previous_a,
                    },
                ],
            })
            .unwrap();
        let high = arena
            .alloc(SLTNode::Slice {
                expr: group,
                access: BitAccess::new(8, 15),
            })
            .unwrap();
        let low = arena
            .alloc(SLTNode::Slice {
                expr: group,
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let path = |target, expr| LogicPath {
            transparent_copy: false,
            target: LogicPathTarget::Var(VarAtomBase::new(target, 0, 7)),
            // Keep the scheduling graph acyclic in this focused cache test;
            // dependency memoization still sees both initial reads.
            sources: crate::HashSet::default(),
            previous_sources: crate::HashSet::default(),
            address_sources: crate::HashSet::default(),
            local_inputs: Vec::new(),
            order_before: crate::HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
            expr,
        };
        let mut widths = crate::HashMap::default();
        widths.insert(1, 8);
        widths.insert(2, 8);

        let result = sort(
            vec![path(1, high), path(2, low)],
            &arena,
            &crate::HashSet::default(),
            &crate::HashMap::default(),
            false,
            &widths,
            1,
        )
        .unwrap();
        assert_eq!(result.execution_units.len(), 1);
        let eu = &result.execution_units[0];
        assert_eq!(
            eu.blocks
                .values()
                .filter(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
                .count(),
            2,
            "one grouped fold has only its entry and counted-loop branches"
        );
        assert_eq!(
            eu.blocks
                .values()
                .flat_map(|block| &block.instructions)
                .filter(|instruction| matches!(instruction, SIRInstruction::Load(..)))
                .count(),
            2,
            "both initial values are loaded once, not once per projection"
        );

        let store_block = eu
            .blocks
            .values()
            .find(|block| {
                block
                    .instructions
                    .iter()
                    .filter(|instruction| matches!(instruction, SIRInstruction::Store(..)))
                    .count()
                    == 2
            })
            .expect("both atomic projection stores share the materialization exit");
        let store_positions = store_block
            .instructions
            .iter()
            .enumerate()
            .filter_map(|(position, instruction)| {
                matches!(instruction, SIRInstruction::Store(..)).then_some(position)
            })
            .collect::<Vec<_>>();
        let stored_values = store_block
            .instructions
            .iter()
            .filter_map(|instruction| match instruction {
                SIRInstruction::Store(_, _, _, value, _, _) => Some(*value),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(store_positions.len(), stored_values.len());
        for (projection, value) in stored_values.into_iter().enumerate() {
            let definition = store_block
                .instructions
                .iter()
                .position(|instruction| match instruction {
                    SIRInstruction::Binary(dst, ..)
                    | SIRInstruction::Unary(dst, ..)
                    | SIRInstruction::Slice(dst, ..)
                    | SIRInstruction::Concat(dst, ..)
                    | SIRInstruction::Mux(dst, ..) => *dst == value,
                    _ => false,
                })
                .expect("stored projection has a local definition");
            assert!(definition < store_positions[projection]);
            if projection != 0 {
                assert!(
                    definition > store_positions[projection - 1],
                    "a later projection must not remain live across an earlier Store"
                );
            }
        }
    }

    /// A process writes all its outputs in one run of its unit, so the unit
    /// is emitted at its first projection only, while a downstream path of
    /// each output still follows the process.
    #[test]
    fn acyclic_process_unit_runs_at_its_first_projection_only() {
        use celox_sir::{SIROffset, SIRValue};
        const INPUT: usize = 0;
        const PROCESS: usize = 10;
        let outputs = [1, 2, 3];
        let mut builder = SIRBuilder::<usize>::new();
        let value = builder.alloc_bit(8, false);
        builder.emit(SIRInstruction::Load(value, INPUT, SIROffset::Static(0), 8));
        for output in outputs {
            builder.emit(SIRInstruction::Store(
                output,
                SIROffset::Static(0),
                8,
                value,
                Vec::new(),
                Vec::new(),
            ));
        }
        let marker = builder.alloc_bit(1, false);
        builder.emit(SIRInstruction::Imm(marker, SIRValue::new(1u8)));
        builder.emit(SIRInstruction::Store(
            PROCESS,
            SIROffset::Static(0),
            1,
            marker,
            Vec::new(),
            Vec::new(),
        ));
        builder.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = builder.drain();
        let unit = celox_sir::ExecutionUnit {
            entry_block_id: celox_sir::BlockId(0),
            blocks,
            register_map,
        };

        let mut arena = SLTNodeArena::<usize>::new();
        let input = |arena: &mut SLTNodeArena<usize>, variable| {
            arena
                .alloc(SLTNode::Input {
                    variable,
                    index: Vec::new(),
                    access: BitAccess::new(0, 7),
                    signed: false,
                })
                .unwrap()
        };
        let path = |target, expr, sources: &[usize]| LogicPath {
            transparent_copy: false,
            target,
            sources: sources
                .iter()
                .map(|&source| VarAtomBase::new(source, 0, 7))
                .collect(),
            previous_sources: crate::HashSet::default(),
            address_sources: crate::HashSet::default(),
            local_inputs: Vec::new(),
            order_before: crate::HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
            expr,
        };
        let mut paths = Vec::new();
        for output in outputs {
            let expr = input(&mut arena, output);
            paths.push(path(
                LogicPathTarget::Process {
                    process: PROCESS,
                    after_initials: false,
                    sensitivity_counters: Vec::new(),
                    startup: None,
                    target: VarAtomBase::new(output, 0, 7),
                    unit: Box::new(unit.clone()),
                },
                expr,
                &[INPUT],
            ));
            // A consumer of each output, scheduled after its projection.
            let read = input(&mut arena, output);
            paths.push(path(
                LogicPathTarget::Var(VarAtomBase::new(output + 100, 0, 7)),
                read,
                &[output],
            ));
        }
        let widths = [INPUT, PROCESS]
            .into_iter()
            .chain(outputs)
            .chain(outputs.map(|output| output + 100))
            .map(|variable| (variable, if variable == PROCESS { 1 } else { 8 }))
            .collect();
        let result = sort(
            paths,
            &arena,
            &crate::HashSet::default(),
            &crate::HashMap::default(),
            false,
            &widths,
            1,
        )
        .unwrap();
        let stores = |variable| {
            result
                .execution_units
                .iter()
                .flat_map(|unit| unit.blocks.values())
                .flat_map(|block| &block.instructions)
                .filter(|instruction| {
                    matches!(instruction, SIRInstruction::Store(target, ..) if *target == variable)
                })
                .count()
        };
        assert_eq!(stores(PROCESS), 1, "the unit runs once");
        for output in outputs {
            assert_eq!(stores(output), 1);
            assert_eq!(stores(output + 100), 1, "each consumer is emitted");
        }
    }
}
