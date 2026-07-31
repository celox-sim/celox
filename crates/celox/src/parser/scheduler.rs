use crate::HashMap;
use crate::HashSet;
use crate::ir::BinaryOp;
use crate::ir::RegisterId;
use crate::ir::SIRBuilder;
use crate::ir::SIRInstruction;
use crate::ir::SIROffset;
use crate::ir::SIRTerminator;
use crate::ir::SIRValue;
use crate::ir::{BitAccess, BlockId, ExecutionUnit, RuntimeErrorInfo, VarAtomBase};
use crate::logic_tree::NodeId;
use crate::logic_tree::{LogicPath, LogicPathTarget, SLTNode, SLTNodeArena, SLTNodeFactsError};
use celox_analysis::dag_schedule::schedule_min_live_values_and_tokens;
use celox_analysis::interval::{DisjointIntervalError, DisjointIntervalMap, ExactInterval};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::fmt::Display;
use std::hash::Hash;
use thiserror::Error;

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
            SLTNode::Unary(_, inner) => work.push(*inner),
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
                initials,
                updates,
                effects,
                continue_cond,
                ..
            } => {
                if let crate::logic_tree::SLTLoopBound::Expr(node) = start {
                    work.push(*node);
                }
                if let crate::logic_tree::SLTLoopBound::Expr(node) = end {
                    work.push(*node);
                }
                work.extend(initials.iter().map(|state| state.expr));
                work.extend(updates.iter().map(|state| state.expr));
                for effect in effects {
                    work.extend(effect.guard);
                    work.extend(effect.args.iter().copied());
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
    node: crate::logic_tree::NodeId,
    arena: &SLTNodeArena<Addr>,
    memo: &mut HashMap<crate::logic_tree::NodeId, HashSet<Addr>>,
    inverse_memo: &mut HashMap<Addr, HashSet<crate::logic_tree::NodeId>>,
) -> HashSet<Addr> {
    if let Some(found) = memo.get(&node) {
        return found.clone();
    }

    let deps = match arena.get(node) {
        crate::logic_tree::SLTNode::Input {
            variable, index, ..
        } => {
            let mut set = HashSet::default();
            set.insert(*variable);
            for idx in index {
                set.extend(collect_node_input_deps(idx.node, arena, memo, inverse_memo));
            }
            set
        }
        crate::logic_tree::SLTNode::Slice { expr, .. } => {
            collect_node_input_deps(*expr, arena, memo, inverse_memo)
        }
        crate::logic_tree::SLTNode::Concat(parts) => {
            let mut set = HashSet::default();
            for (part, _) in parts {
                set.extend(collect_node_input_deps(*part, arena, memo, inverse_memo));
            }
            set
        }
        crate::logic_tree::SLTNode::Binary(lhs, _, rhs) => {
            let mut set = collect_node_input_deps(*lhs, arena, memo, inverse_memo);
            set.extend(collect_node_input_deps(*rhs, arena, memo, inverse_memo));
            set
        }
        crate::logic_tree::SLTNode::Unary(_, inner) => {
            collect_node_input_deps(*inner, arena, memo, inverse_memo)
        }
        crate::logic_tree::SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => {
            let mut set = collect_node_input_deps(*cond, arena, memo, inverse_memo);
            set.extend(collect_node_input_deps(
                *then_expr,
                arena,
                memo,
                inverse_memo,
            ));
            set.extend(collect_node_input_deps(
                *else_expr,
                arena,
                memo,
                inverse_memo,
            ));
            set
        }
        crate::logic_tree::SLTNode::ForFold {
            loop_var,
            start,
            end,
            initials,
            updates,
            effects,
            continue_cond,
            ..
        } => {
            let mut set = HashSet::default();
            match start {
                crate::logic_tree::SLTLoopBound::Const(_) => {}
                crate::logic_tree::SLTLoopBound::Expr(node) => {
                    set.extend(collect_node_input_deps(*node, arena, memo, inverse_memo));
                }
            }
            match end {
                crate::logic_tree::SLTLoopBound::Const(_) => {}
                crate::logic_tree::SLTLoopBound::Expr(node) => {
                    set.extend(collect_node_input_deps(*node, arena, memo, inverse_memo));
                }
            }
            for init in initials {
                set.extend(collect_node_input_deps(
                    init.expr,
                    arena,
                    memo,
                    inverse_memo,
                ));
            }
            for update in updates {
                set.extend(collect_node_input_deps(
                    update.expr,
                    arena,
                    memo,
                    inverse_memo,
                ));
            }
            for effect in effects {
                if let Some(guard) = effect.guard {
                    set.extend(collect_node_input_deps(guard, arena, memo, inverse_memo));
                }
                for arg in &effect.args {
                    set.extend(collect_node_input_deps(*arg, arena, memo, inverse_memo));
                }
            }
            set.remove(loop_var);
            set.extend(collect_node_input_deps(
                *continue_cond,
                arena,
                memo,
                inverse_memo,
            ));
            set.remove(loop_var);
            set
        }
        crate::logic_tree::SLTNode::ForFoldGroup {
            loop_var,
            entry_guard,
            states,
            ..
        } => {
            let mut set = collect_node_input_deps(*entry_guard, arena, memo, inverse_memo);
            for state in states {
                set.extend(collect_node_input_deps(
                    state.initial,
                    arena,
                    memo,
                    inverse_memo,
                ));
            }
            let mut update_deps = HashSet::default();
            for state in states {
                update_deps.extend(collect_node_input_deps(
                    state.update,
                    arena,
                    memo,
                    inverse_memo,
                ));
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
        crate::logic_tree::SLTNode::Constant(_, _, _, _) => HashSet::default(),
    };

    for &addr in &deps {
        inverse_memo.entry(addr).or_default().insert(node);
    }
    memo.insert(node, deps.clone());
    deps
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
/// overlapping definition. `successors` contains all semantic edges while
/// `value_users` contains only Def-to-Use edges which can become a forwarded
/// value during later lowering. Both tables are indexed by the predecessor.
struct LogicPathMemorySsa {
    successors: Vec<Vec<usize>>,
    value_users: Vec<Vec<usize>>,
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

    let mut successors = vec![Vec::new(); input.len()];
    let mut value_users = vec![Vec::new(); input.len()];
    for (user, path) in input.iter().enumerate() {
        for source in &path.sources {
            let Some((start, length)) = bit_interval(source.access) else {
                return Err(SchedulerError::InvalidDependencyGraph);
            };
            let reaching = definitions
                .overlapping(&source.id, start, length)
                .map_err(|_| SchedulerError::InvalidDependencyGraph)?;
            for definition in reaching {
                successors[definition].push(user);
                value_users[definition].push(user);
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
                successors[user].push(definition);
            }
        }
        for target in &path.order_before {
            if target.0 < input.len() && target.0 != user {
                successors[user].push(target.0);
            }
        }
    }
    for edges in &mut successors {
        edges.sort_unstable();
        edges.dedup();
    }
    for edges in &mut value_users {
        edges.sort_unstable();
        edges.dedup();
    }
    Ok(LogicPathMemorySsa {
        successors,
        value_users,
    })
}

pub(crate) fn plan_ff_comb_schedule<Addr>(
    input: &[LogicPath<Addr>],
    ff: &[crate::ir::FfAccessSummary<Addr>],
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

    let mut predecessors = vec![Vec::new(); input.len()];
    for (definition, users) in memory.successors.iter().enumerate() {
        for &user in users {
            predecessors[user].push(definition);
        }
    }
    while let Some(path) = work.pop() {
        for &predecessor in &predecessors[path] {
            if !std::mem::replace(&mut required_comb[predecessor], true) {
                work.push(predecessor);
            }
        }
    }

    let mut comb_before_direct_write = vec![Vec::new(); ff.len()];
    for (path_index, path) in input.iter().enumerate() {
        if !required_comb[path_index] {
            continue;
        }
        for (ff_index, summary) in ff.iter().enumerate() {
            if path
                .sources
                .iter()
                .chain(&path.previous_sources)
                .any(|read| {
                    summary
                        .writes
                        .iter()
                        .any(|write| read.id == write.id && read.access.overlaps(&write.access))
                })
            {
                comb_before_direct_write[ff_index].push(path_index);
            }
        }
    }

    let mut ff_before_direct_write = vec![Vec::new(); ff.len()];
    for (writer, write_summary) in ff.iter().enumerate() {
        for (reader, read_summary) in ff.iter().enumerate() {
            if writer != reader
                && read_summary.reads.iter().any(|read| {
                    write_summary
                        .writes
                        .iter()
                        .any(|write| read.id == write.id && read.access.overlaps(&write.access))
                })
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
    lowerer: &crate::logic_tree::SLTToSIRLowerer,
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
        let width = crate::logic_tree::get_width(*node, arena);
        if width > 0 {
            env_inputs.insert(crate::ir::VarAtomBase::new(*addr, 0, width - 1), reg);
        }
    }
    lowerer.lower_with_inputs(builder, path.expr, arena, lower_cache, env_inputs)
}

fn lower_logic_path_node<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    lowerer: &crate::logic_tree::SLTToSIRLowerer,
    builder: &mut SIRBuilder<Addr>,
    path: &LogicPath<Addr>,
    node: NodeId,
    arena: &SLTNodeArena<Addr>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
) -> RegisterId {
    let mut env_inputs = HashMap::default();
    for (addr, local_node) in &path.local_inputs {
        let reg = lowerer.lower(builder, *local_node, arena, lower_cache);
        let width = crate::logic_tree::get_width(*local_node, arena);
        if width > 0 {
            env_inputs.insert(crate::ir::VarAtomBase::new(*addr, 0, width - 1), reg);
        }
    }
    lowerer.lower_with_inputs(builder, node, arena, lower_cache, env_inputs)
}

fn pre_lower_logic_path_node<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    lowerer: &crate::logic_tree::SLTToSIRLowerer,
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
    lowerer: &crate::logic_tree::SLTToSIRLowerer,
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
            let old_reg = if path.comb_capture_enable_sites.is_empty() {
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
            if let Some(old) = old_reg {
                builder.emit(SIRInstruction::CombCaptureEnableIfChanged {
                    old,
                    new: result_reg,
                    sites: path.comb_capture_enable_sites.clone(),
                });
            }
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
                    builder.emit(SIRInstruction::Unary(
                        inverted,
                        crate::ir::UnaryOp::LogicNot,
                        cond,
                    ));
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
                emit(builder, lower_cache);
                builder.seal_block(SIRTerminator::Jump(done_block, vec![]));
                builder.switch_to_block(done_block);
            } else {
                emit(builder, lower_cache);
            }
        }
    }
}

fn emit_logic_path_store<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    lowerer: &crate::logic_tree::SLTToSIRLowerer,
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
            lower_cache.remove(node);
        }
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
        let width = crate::logic_tree::get_width(group, arena);
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
    states: &[crate::logic_tree::SLTForFoldGroupState<Addr>],
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
        SLTNode::Unary(_, inner) => work.push(*inner),
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
            initials,
            updates,
            effects,
            continue_cond,
            ..
        } => {
            if let crate::logic_tree::SLTLoopBound::Expr(node) = start {
                work.push(*node);
            }
            if let crate::logic_tree::SLTLoopBound::Expr(node) = end {
                work.push(*node);
            }
            work.extend(initials.iter().map(|state| state.expr));
            work.extend(updates.iter().map(|state| state.expr));
            for effect in effects {
                work.extend(effect.guard);
                work.extend(effect.args.iter().copied());
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
    Unary(crate::ir::UnaryOp, Box<NormalizedIndexExpr<Addr>>),
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
    state_targets: Vec<crate::ir::VarAtomBase<Addr>>,
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
                LogicPathTarget::Var(_) => Vec::new(),
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
    targets: &[crate::ir::VarAtomBase<Addr>],
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
    lowerer: &crate::logic_tree::SLTToSIRLowerer,
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
    lowerer: &crate::logic_tree::SLTToSIRLowerer,
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
    /// Exact comb range definitions grouped by their source semantic process.
    /// This survives physical EU coalescing and is not an ordering constraint.
    pub semantic_regions: HashMap<VarAtomBase<Addr>, u64>,
    /// Persistent-state ranges published directly by FF actions in the shared
    /// comb/FF schedule. These Stores are semantic state updates rather than
    /// disposable comb publications.
    pub direct_ff_writes: Vec<VarAtomBase<Addr>>,
}

pub(crate) trait ClockFfLowering<Addr> {
    fn summaries(&self) -> &[crate::ir::FfAccessSummary<Addr>];
    fn begin(
        &mut self,
        builder: &mut SIRBuilder<Addr>,
        direct: &[bool],
    ) -> Result<(), super::ParserError>;
    fn lower(
        &mut self,
        index: usize,
        direct: bool,
        builder: &mut SIRBuilder<Addr>,
    ) -> Result<(), super::ParserError>;
    fn finish(
        &mut self,
        builder: &mut SIRBuilder<Addr>,
        direct: &[bool],
    ) -> Result<(), super::ParserError>;
}

pub(crate) enum ClockSortError<Addr: Display + Debug + Eq + Hash + Clone> {
    Scheduler(SchedulerError<Addr>),
    Lowering(super::ParserError),
}

enum ScheduledWork {
    Comb(Vec<usize>),
    Ff(usize),
}

/// Lower a consecutive set of exact grouped-fold paths.  The packed fold
/// results are computed atomically, then each projection is created and stored
/// in topological order.  Ordinary paths bypass this buffer entirely.
fn flush_pending_fold_paths<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    pending: &mut Vec<usize>,
    input: &[LogicPath<Addr>],
    fold_group_schedule_index: &FoldGroupScheduleIndex<Addr>,
    lowerer: &crate::logic_tree::SLTToSIRLowerer,
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
    matches!(path.target, LogicPathTarget::CombCaptureEvent { .. })
        || !path.comb_capture_enable_sites.is_empty()
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
        let width = crate::logic_tree::get_width(NodeId(raw), arena);
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

fn schedule_acyclic_path_region(
    paths: &[usize],
    dependencies: &[Vec<usize>],
    value_dependencies: &[Vec<usize>],
    materialization_tokens: &[Vec<usize>],
    token_weights: &[usize],
    local_by_path: &mut [usize],
) -> Option<Vec<Vec<usize>>> {
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
        let mut local_dependencies = vec![Vec::<usize>::new(); paths.len()];
        let mut local_values = vec![Vec::<usize>::new(); paths.len()];
        let mut local_tokens = vec![Vec::<usize>::new(); paths.len()];
        for (definition, &path) in paths.iter().enumerate() {
            local_tokens[definition] = materialization_tokens.get(path)?.clone();
            for &user_path in dependencies.get(path)? {
                let user = *local_by_path.get(user_path)?;
                if user != usize::MAX && definition != user {
                    local_dependencies[user].push(definition);
                }
            }
            for &user_path in value_dependencies.get(path)? {
                let user = *local_by_path.get(user_path)?;
                if user != usize::MAX && definition != user {
                    local_values[user].push(definition);
                }
            }
        }
        for row in &mut local_dependencies {
            row.sort_unstable();
            row.dedup();
        }
        for row in &mut local_values {
            row.sort_unstable();
            row.dedup();
        }
        let order = schedule_min_live_values_and_tokens(
            &local_dependencies,
            &local_values,
            &local_tokens,
            token_weights,
        )
        .ok()?;
        Some(order.into_iter().map(|local| vec![paths[local]]).collect())
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
    dependencies: &[Vec<usize>],
    value_dependencies: &[Vec<usize>],
    materialization_tokens: &[Vec<usize>],
    token_weights: &[usize],
    input: &[LogicPath<Addr>],
) -> Option<Vec<Vec<usize>>> {
    if dependencies.len() != value_dependencies.len()
        || dependencies.len() != materialization_tokens.len()
        || dependencies.len() < input.len()
    {
        return None;
    }
    let mut local_by_path = vec![usize::MAX; dependencies.len()];
    let mut result = Vec::with_capacity(topological_sccs.len());
    let mut pending = Vec::new();
    let flush = |pending: &mut Vec<usize>,
                 result: &mut Vec<Vec<usize>>,
                 local_by_path: &mut [usize]|
     -> Option<()> {
        if pending.is_empty() {
            return Some(());
        }
        result.extend(schedule_acyclic_path_region(
            pending,
            dependencies,
            value_dependencies,
            materialization_tokens,
            token_weights,
            local_by_path,
        )?);
        pending.clear();
        Some(())
    };

    for scc in topological_sccs {
        if let [path] = scc.as_slice()
            && !dependencies[*path].contains(path)
            && (*path >= input.len() || !logic_path_is_scheduling_barrier(&input[*path]))
        {
            pending.push(*path);
        } else {
            flush(&mut pending, &mut result, &mut local_by_path)?;
            result.push(scc);
        }
    }
    flush(&mut pending, &mut result, &mut local_by_path)?;
    Some(result)
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
fn sort_impl<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    input: Vec<LogicPath<Addr>>,
    arena: &SLTNodeArena<Addr>,
    ignored_loops: &HashSet<(Addr, Addr)>,
    true_loops: &HashMap<(Addr, Addr), usize>,
    four_state: bool,
    _var_widths: &HashMap<Addr, usize>,
    unpacked_element_widths: &HashMap<Addr, usize>,
    first_runtime_error_code: i64,
    mut ff: Option<&mut dyn ClockFfLowering<Addr>>,
) -> Result<ScheduleResult<Addr>, ClockSortError<Addr>> {
    let (input, ff_plan) = if let Some(ff_lowering) = ff.as_deref() {
        let mut plan = plan_ff_comb_schedule(&input, ff_lowering.summaries())
            .map_err(ClockSortError::Scheduler)?;
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
    let semantic_regions = input
        .iter()
        .filter_map(|path| Some((*path.target.var()?, path.semantic_region?)))
        .collect();
    let LogicPathMemorySsa {
        successors: mut adj,
        mut value_users,
    } = build_logic_path_memory_ssa(&input).map_err(ClockSortError::Scheduler)?;

    // FF actions are ordinary sinks in the existing comb dependency graph.
    // The added nodes go through the same SCC, topological, and ready-queue
    // scheduling as LogicPaths; there is no second clock-specific scheduler.
    let ff_count = ff
        .as_deref()
        .map_or(0, |lowering| lowering.summaries().len());
    let mut direct_ff = vec![false; ff_count];
    if let Some(plan) = &ff_plan {
        adj.resize_with(n + ff_count, Vec::new);
        value_users.resize_with(n + ff_count, Vec::new);
        materialization_tokens.resize_with(n + ff_count, Vec::new);
        for (ff_index, predecessors) in plan.comb_value_predecessors.iter().enumerate() {
            let ff_node = n + ff_index;
            for &definition in predecessors {
                adj[definition].push(ff_node);
                value_users[definition].push(ff_node);
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
        let retained =
            add_acyclic_ff_write_order_edges(&mut adj, write_order_edges.iter().flatten().copied());
        for (writer, requirements) in write_order_edges.iter().enumerate() {
            // An FF action with no old-state users is directly publishable.
            // Otherwise every reader-before-writer anti-dependence must have
            // survived cycle removal.  A partial set is not a proof.
            direct_ff[writer] = requirements.iter().all(|edge| retained.contains(edge));
        }
        for edges in adj.iter_mut().chain(&mut value_users) {
            edges.sort_unstable();
            edges.dedup();
        }
    }
    let direct_ff_writes =
        ff.as_deref()
            .map(|lowering| {
                lowering
                    .summaries()
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| direct_ff.get(*index).copied().unwrap_or(false))
                    .flat_map(|(_, summary)| {
                        summary.writes.iter().filter_map(|write| {
                            let reads_old_value = summary.reads.iter().any(|read| {
                                read.id == write.id && read.access.overlaps(&write.access)
                            });
                            (!reads_old_value).then_some(*write)
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

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
            strong_connect(i, &adj, &mut ctx);
        }
    }
    let fold_group_schedule_index = build_fold_group_schedule_index(&input, arena);
    let mut path_domains = logic_path_scheduling_domains(&input, &fold_group_schedule_index);
    path_domains.resize(n + ff_count, None);
    let (topological_sccs, component_by_path) =
        stable_topological_sccs(ctx.sccs, &adj, &path_domains).ok_or(ClockSortError::Scheduler(
            SchedulerError::InvalidDependencyGraph,
        ))?;
    let scheduled_nodes = schedule_logic_path_regions(
        topological_sccs,
        &adj,
        &value_users,
        &materialization_tokens,
        &token_weights,
        &input,
    )
    .ok_or(ClockSortError::Scheduler(
        SchedulerError::InvalidDependencyGraph,
    ))?;
    let mut scheduled_work = Vec::with_capacity(scheduled_nodes.len());
    for nodes in scheduled_nodes {
        if nodes.iter().all(|node| *node < n) {
            scheduled_work.push(ScheduledWork::Comb(nodes));
        } else if let [node] = nodes.as_slice()
            && *node >= n
        {
            scheduled_work.push(ScheduledWork::Ff(*node - n));
        } else {
            return Err(ClockSortError::Scheduler(
                SchedulerError::InvalidDependencyGraph,
            ));
        }
    }

    let mut builder = SIRBuilder::new();
    if let Some(ff_lowering) = ff.as_deref_mut() {
        ff_lowering
            .begin(&mut builder, &direct_ff)
            .map_err(ClockSortError::Lowering)?;
    }
    let lowerer = crate::logic_tree::SLTToSIRLowerer::new(four_state)
        .with_unpacked_input_types(arena, unpacked_element_widths);

    let mut lower_cache = HashMap::default();
    let mut dep_memo = HashMap::default();
    let mut inverse_dep_memo = HashMap::default();

    const UNROLL_THRESHOLD: usize = 32;

    // Helper: Emits SIR for a logic path and manages the lowering cache.
    // lowerer.lower allocates registers and emits instructions for sub-expressions.
    let emit_node = |builder: &mut SIRBuilder<Addr>,
                     idx: usize,
                     lower_cache: &mut HashMap<NodeId, RegisterId>,
                     dep_memo: &mut HashMap<NodeId, HashSet<Addr>>,
                     inverse_dep_memo: &mut HashMap<Addr, HashSet<NodeId>>| {
        let path = &input[idx];

        collect_logic_path_input_deps(path, arena, dep_memo, inverse_dep_memo);
        emit_logic_path_store(
            &lowerer,
            builder,
            path,
            arena,
            lower_cache,
            unpacked_element_widths,
        );
        invalidate_logic_path_target(path, inverse_dep_memo, lower_cache);
    };
    // Maximum blocks in a single EU before flushing to a new one.
    // This prevents Cranelift from choking on massive functions.
    const EU_BLOCK_LIMIT: usize = 20_000;

    let mut result_eus: Vec<ExecutionUnit<Addr>> = Vec::new();
    let mut runtime_errors: HashMap<i64, RuntimeErrorInfo<Addr>> = HashMap::default();
    let mut next_runtime_error_code = first_runtime_error_code;

    // Pairwise joint-fold profitability is intentionally local. Keep all
    // projections of one packed root together, but never build an unbounded
    // compatibility matrix for a long run of independent roots.
    const MAX_JOINT_FOLD_ROOTS: usize = 16;
    let mut pending_fold_indices: Vec<usize> = Vec::new();
    let mut pending_fold_roots = HashSet::default();

    // 4. Lower each scheduled component, selecting static unrolling or
    // dynamic convergence for cyclic SCCs.
    for work in scheduled_work {
        let scc = match work {
            ScheduledWork::Comb(scc) => scc,
            ScheduledWork::Ff(index) => {
                flush_pending_fold_paths(
                    &mut pending_fold_indices,
                    &input,
                    &fold_group_schedule_index,
                    &lowerer,
                    &mut builder,
                    arena,
                    &mut lower_cache,
                    &mut dep_memo,
                    &mut inverse_dep_memo,
                    unpacked_element_widths,
                    four_state,
                );
                pending_fold_roots.clear();
                ff.as_deref_mut()
                    .ok_or(ClockSortError::Scheduler(
                        SchedulerError::InvalidDependencyGraph,
                    ))?
                    .lower(index, direct_ff[index], &mut builder)
                    .map_err(ClockSortError::Lowering)?;
                // FF state is staged in WORKING/SPARSE_WORKING and is invisible
                // to the STABLE-only comb graph until the final publish. Values
                // computed before the FF CFG dominate its merge block, so the
                // ordinary comb lowering cache remains valid.
                continue;
            }
        };
        let component = component_by_path[scc[0]];
        let mut user_safety_limit = None;
        for &v_idx in &scc {
            for &u_idx in &adj[v_idx] {
                if component_by_path[u_idx] == component {
                    if let (Some(v_target), Some(u_target)) =
                        (input[v_idx].target.var(), input[u_idx].target.var())
                    {
                        let edge = (v_target.id, u_target.id);
                        if let Some(&limit) = true_loops.get(&edge) {
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
            flush_pending_fold_paths(
                &mut pending_fold_indices,
                &input,
                &fold_group_schedule_index,
                &lowerer,
                &mut builder,
                arena,
                &mut lower_cache,
                &mut dep_memo,
                &mut inverse_dep_memo,
                unpacked_element_widths,
                four_state,
            );
            pending_fold_roots.clear();
            let mut authorized = user_safety_limit.is_some();
            'check_scc: for &v_idx in &scc {
                for &u_idx in &adj[v_idx] {
                    if component_by_path[u_idx] == component
                        && input[v_idx]
                            .target
                            .var()
                            .zip(input[u_idx].target.var())
                            .is_some_and(|(v, u)| ignored_loops.contains(&(v.id, u.id)))
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
                        blocks: scc.into_iter().map(|idx| input[idx].clone()).collect(),
                    },
                ));
            }

            // FAS Sort
            let optimized_scc_order = greedy_fas_sort(&scc, &adj);
            let force_strategy_b = user_safety_limit.is_some();
            let iterations = calculate_required_iterations(&adj, &optimized_scc_order);
            let total_ops_estimate = optimized_scc_order.len().saturating_mul(iterations);
            if !force_strategy_b && total_ops_estimate <= UNROLL_THRESHOLD {
                // Strategy A: Static Unrolling
                // The loop is unrolled a fixed number of times based on structural dependency depth (iterations).
                for _ in 0..iterations {
                    for &idx in &optimized_scc_order {
                        emit_node(
                            &mut builder,
                            idx,
                            &mut lower_cache,
                            &mut dep_memo,
                            &mut inverse_dep_memo,
                        );
                    }
                }
            } else {
                let runtime_error_code = next_runtime_error_code;
                next_runtime_error_code += 1;
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
                let zero_reg = builder.alloc_bit(64, false);
                builder.emit(SIRInstruction::Imm(zero_reg, SIRValue::new(0u64)));

                let limit_reg = builder.alloc_bit(64, false);
                builder.emit(SIRInstruction::Imm(
                    limit_reg,
                    SIRValue::new(safety_limit as u64),
                ));

                // 3. Blocks
                let current_counter = builder.alloc_bit(64, false);
                let header_block = builder.new_block_with(vec![current_counter]); // [counter]
                let body_block = builder.new_block();
                let exit_block = builder.new_block();
                let error_block = builder.new_block(); // For True Loop detection

                // Start: Jump to header with counter = 0
                builder.seal_block(SIRTerminator::Jump(header_block, vec![zero_reg]));

                // --- Header Block ---
                builder.switch_to_block(header_block);

                // Check: counter < safety_limit
                let can_continue_reg = builder.alloc_bit(1, false);
                builder.emit(SIRInstruction::Binary(
                    can_continue_reg,
                    current_counter,
                    BinaryOp::LtU,
                    limit_reg,
                ));

                // If counter exceeded limit, we might have an oscillating True Loop
                builder.seal_block(SIRTerminator::Branch {
                    cond: can_continue_reg,
                    true_block: (body_block, vec![]),
                    false_block: (error_block, vec![]),
                });
                builder.switch_to_block(body_block);
                let mut current_dirty_reg = builder.alloc_bit(1, false);
                builder.emit(SIRInstruction::Imm(current_dirty_reg, SIRValue::new(0u32)));
                for &idx in &optimized_scc_order {
                    let path = &input[idx];
                    let Some(target) = path.target.var() else {
                        emit_logic_path_store(
                            &lowerer,
                            &mut builder,
                            path,
                            arena,
                            &mut lower_cache,
                            unpacked_element_widths,
                        );
                        continue;
                    };
                    let width = 1 + target.access.msb - target.access.lsb;
                    let addr = target.id;
                    let offset =
                        static_access_offset(&target.id, target.access, unpacked_element_widths);

                    // --- Dynamic Convergence Check Logic ---
                    // For each node in the SCC, we verify if its value changed after this iteration.
                    //
                    // a. Load the current value (pre-update benchmark)
                    let old_val_reg = builder.alloc_bit(width, false);
                    builder.emit(SIRInstruction::Load(
                        old_val_reg,
                        addr,
                        offset.clone(),
                        width,
                    ));
                    collect_logic_path_input_deps(
                        path,
                        arena,
                        &mut dep_memo,
                        &mut inverse_dep_memo,
                    );
                    // b. Compute the new value
                    let new_val_reg = lower_logic_path_expr(
                        &lowerer,
                        &mut builder,
                        path,
                        arena,
                        &mut lower_cache,
                    );

                    // c. Compare: changed = (old != new)
                    let is_changed_reg = builder.alloc_bit(1, false);
                    builder.emit(SIRInstruction::Binary(
                        is_changed_reg,
                        old_val_reg,
                        BinaryOp::Ne, // Not Equal
                        new_val_reg,
                    ));
                    let new_dirty_reg = builder.alloc_bit(1, false);

                    // d. Accumulate dirty flag: dirty = dirty | is_changed
                    // If any signal in the SCC changes, the entire SCC requires another iteration.
                    builder.emit(SIRInstruction::Binary(
                        new_dirty_reg,
                        current_dirty_reg,
                        BinaryOp::Or,
                        is_changed_reg,
                    ));
                    current_dirty_reg = new_dirty_reg;
                    // e. Store the new value
                    builder.emit(SIRInstruction::Store(
                        addr,
                        offset,
                        width,
                        new_val_reg,
                        Vec::new(),
                        Vec::new(),
                    ));
                    if !path.comb_capture_enable_sites.is_empty() {
                        builder.emit(SIRInstruction::CombCaptureEnableIfChanged {
                            old: old_val_reg,
                            new: new_val_reg,
                            sites: path.comb_capture_enable_sites.clone(),
                        });
                    }
                    if let Some(to_remove) = inverse_dep_memo.get(&addr) {
                        for node in to_remove {
                            lower_cache.remove(node);
                        }
                    }
                    // -------------------------------
                }

                // 4. Branch: Loop if dirty
                let one_reg = builder.alloc_bit(64, false);
                builder.emit(SIRInstruction::Imm(one_reg, SIRValue::new(1u64)));
                let next_counter = builder.alloc_bit(64, false);
                builder.emit(SIRInstruction::Binary(
                    next_counter,
                    current_counter,
                    BinaryOp::Add,
                    one_reg,
                ));

                // Increment the iteration counter and branch.
                // If 'dirty' is true, return to the header block; otherwise, exit the loop.
                builder.seal_block(SIRTerminator::Branch {
                    cond: current_dirty_reg,
                    true_block: (header_block, vec![next_counter]),
                    false_block: (exit_block, vec![]),
                });

                // --- Error/Exit Blocks ---
                builder.switch_to_block(error_block);
                // Emit a trap or special instruction to indicate "Combinational Loop Oscillation"
                // builder.emit(SIRInstruction::Trap(1));
                builder.seal_block(SIRTerminator::Error(runtime_error_code));

                // 5. Exit Block
                builder.switch_to_block(exit_block);
            }
        } else {
            // DAG Part — flush before emitting if the EU has grown too large
            if ff.is_none() && builder.block_count() >= EU_BLOCK_LIMIT {
                flush_pending_fold_paths(
                    &mut pending_fold_indices,
                    &input,
                    &fold_group_schedule_index,
                    &lowerer,
                    &mut builder,
                    arena,
                    &mut lower_cache,
                    &mut dep_memo,
                    &mut inverse_dep_memo,
                    unpacked_element_widths,
                    four_state,
                );
                pending_fold_roots.clear();
                if let Some(eu) = builder.flush_eu() {
                    result_eus.push(eu);
                    // Clear the lowering cache — register IDs are EU-scoped
                    lower_cache.clear();
                }
            }

            let idx = scc[0];
            let exact_fold = is_exact_fold_path(idx, &fold_group_schedule_index);
            let fold_root = exact_fold
                .then_some(fold_group_schedule_index.direct_group_by_path[idx])
                .flatten();
            let depends_on_pending = pending_fold_indices
                .iter()
                .any(|pending| adj[*pending].binary_search(&idx).is_ok());
            let starts_new_root = fold_root.is_some_and(|root| !pending_fold_roots.contains(&root));
            let window_full = starts_new_root && pending_fold_roots.len() >= MAX_JOINT_FOLD_ROOTS;
            if exact_fold && !depends_on_pending && !window_full {
                pending_fold_indices.push(idx);
                pending_fold_roots.extend(fold_root);
            } else {
                flush_pending_fold_paths(
                    &mut pending_fold_indices,
                    &input,
                    &fold_group_schedule_index,
                    &lowerer,
                    &mut builder,
                    arena,
                    &mut lower_cache,
                    &mut dep_memo,
                    &mut inverse_dep_memo,
                    unpacked_element_widths,
                    four_state,
                );
                pending_fold_roots.clear();
                if exact_fold {
                    pending_fold_indices.push(idx);
                    pending_fold_roots.extend(fold_root);
                } else {
                    emit_node(
                        &mut builder,
                        idx,
                        &mut lower_cache,
                        &mut dep_memo,
                        &mut inverse_dep_memo,
                    );
                }
            }
        }
    }

    // Flush the final exact grouped-fold run after the SCC loop.
    flush_pending_fold_paths(
        &mut pending_fold_indices,
        &input,
        &fold_group_schedule_index,
        &lowerer,
        &mut builder,
        arena,
        &mut lower_cache,
        &mut dep_memo,
        &mut inverse_dep_memo,
        unpacked_element_widths,
        four_state,
    );
    pending_fold_roots.clear();

    if let Some(ff_lowering) = ff {
        ff_lowering
            .finish(&mut builder, &direct_ff)
            .map_err(ClockSortError::Lowering)?;
    }
    builder.seal_block(SIRTerminator::Return);
    let (blocks, reg_map, _) = builder.drain();
    result_eus.push(ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map: reg_map,
    });
    Ok(ScheduleResult {
        execution_units: result_eus,
        runtime_errors,
        semantic_regions,
        direct_ff_writes,
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
    )
}

pub(crate) fn sort_with_unpacked_element_widths<
    Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display,
>(
    input: Vec<LogicPath<Addr>>,
    arena: &SLTNodeArena<Addr>,
    ignored_loops: &HashSet<(Addr, Addr)>,
    true_loops: &HashMap<(Addr, Addr), usize>,
    four_state: bool,
    var_widths: &HashMap<Addr, usize>,
    unpacked_element_widths: &HashMap<Addr, usize>,
    first_runtime_error_code: i64,
) -> Result<ScheduleResult<Addr>, SchedulerError<Addr>> {
    match sort_impl(
        input,
        arena,
        ignored_loops,
        true_loops,
        four_state,
        var_widths,
        unpacked_element_widths,
        first_runtime_error_code,
        None,
    ) {
        Ok(result) => Ok(result),
        Err(ClockSortError::Scheduler(error)) => Err(error),
        Err(ClockSortError::Lowering(_)) => {
            unreachable!("ordinary comb scheduling has no FF lowering callback")
        }
    }
}

pub(crate) fn sort_clock<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    input: Vec<LogicPath<Addr>>,
    arena: &SLTNodeArena<Addr>,
    ignored_loops: &HashSet<(Addr, Addr)>,
    true_loops: &HashMap<(Addr, Addr), usize>,
    four_state: bool,
    var_widths: &HashMap<Addr, usize>,
    unpacked_element_widths: &HashMap<Addr, usize>,
    first_runtime_error_code: i64,
    ff: &mut dyn ClockFfLowering<Addr>,
) -> Result<ScheduleResult<Addr>, ClockSortError<Addr>> {
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
    )
}

#[cfg(test)]
mod tests {
    use num_bigint::{BigInt, BigUint};

    use super::{
        ExactFoldGroup, ExactIndexedLoadKey, FoldGroupReadFacts, NormalizedIndexExpr,
        add_acyclic_ff_write_order_edges, best_weighted_fold_family,
        build_fold_group_schedule_index, build_logic_path_memory_ssa, collect_node_input_deps,
        plan_ff_comb_schedule, prepare_atomic_fold_group_results, sort, stable_topological_sccs,
    };
    use crate::HashSet;
    use crate::ir::{BinaryOp, BitAccess, SIRBuilder, SIRInstruction, SIRTerminator, VarAtomBase};
    use crate::logic_tree::{
        LogicPath, LogicPathTarget, SLTForFoldGroupState, SLTNode, SLTNodeArena, SLTToSIRLowerer,
    };

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
            semantic_region: None,
            target: LogicPathTarget::Var(VarAtomBase::new(target, 0, 7)),
            sources: source
                .map(|source| [VarAtomBase::new(source, 0, 7)].into_iter().collect())
                .unwrap_or_default(),
            previous_sources: crate::HashSet::default(),
            address_sources: crate::HashSet::default(),
            local_inputs: Vec::new(),
            order_before: crate::HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            pre_lower_nodes: Vec::new(),
            expr,
        }
    }

    #[test]
    fn ff_plan_models_comb_and_ff_old_state_readers_before_writer() {
        let mut arena = SLTNodeArena::new();
        let comb_value = simple_path(&mut arena, 10, None);
        let comb_old_state_reader = simple_path(&mut arena, 11, Some(20));
        let ff = vec![
            crate::ir::FfAccessSummary {
                reads: vec![VarAtomBase::new(10, 0, 7), VarAtomBase::new(11, 0, 7)],
                writes: vec![VarAtomBase::new(20, 0, 7)],
                dynamic_writes: HashSet::default(),
            },
            crate::ir::FfAccessSummary {
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
        let summary = crate::ir::FfAccessSummary {
            reads: vec![VarAtomBase::new(20, 0, 7)],
            writes: vec![VarAtomBase::new(20, 0, 7)],
            dynamic_writes: HashSet::default(),
        };

        let plan = plan_ff_comb_schedule(&[], &[summary]).unwrap();

        assert_eq!(plan.ff_before_direct_write, vec![Vec::<usize>::new()]);
    }

    #[test]
    fn cyclic_direct_write_preference_falls_back_to_staging() {
        let mut adj = vec![vec![1], Vec::new(), Vec::new()];

        add_acyclic_ff_write_order_edges(&mut adj, [(1, 0), (1, 2)]);

        assert_eq!(adj, vec![vec![1], vec![2], Vec::new()]);
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
    fn physical_comb_unit_preserves_independent_semantic_regions() {
        let mut arena = SLTNodeArena::new();
        let mut first = simple_path(&mut arena, 10, None);
        first.semantic_region = Some(7);
        let mut second = simple_path(&mut arena, 20, None);
        second.semantic_region = Some(9);

        let result = sort(
            vec![first, second],
            &arena,
            &crate::HashSet::default(),
            &crate::HashMap::default(),
            false,
            &crate::HashMap::default(),
            1,
        )
        .unwrap();

        assert_eq!(result.execution_units.len(), 1);
        assert_eq!(result.semantic_regions[&VarAtomBase::new(10, 0, 7)], 7);
        assert_eq!(result.semantic_regions[&VarAtomBase::new(20, 0, 7)], 9);
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
        assert_eq!(memory_ssa.successors, vec![vec![1], vec![]]);
        assert_eq!(
            memory_ssa.value_users,
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
        assert_eq!(memory_ssa.successors, vec![vec![2], vec![2], vec![]]);
        assert_eq!(memory_ssa.value_users, vec![vec![2], vec![2], vec![]]);
    }

    fn fixed_group_path(
        arena: &mut SLTNodeArena<u32>,
        guard: crate::logic_tree::NodeId,
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
        guard: crate::logic_tree::NodeId,
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
            semantic_region: None,
            target: LogicPathTarget::Var(target),
            sources: [VarAtomBase::new(external, 0, 63)].into_iter().collect(),
            previous_sources: crate::HashSet::default(),
            address_sources: crate::HashSet::default(),
            local_inputs: Vec::new(),
            order_before: crate::HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
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
            root: crate::logic_tree::NodeId(root),
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
            semantic_region: None,
            target: LogicPathTarget::Var(VarAtomBase::new(target, 0, 7)),
            // Keep the scheduling graph acyclic in this focused cache test;
            // dependency memoization still sees both initial reads.
            sources: crate::HashSet::default(),
            previous_sources: crate::HashSet::default(),
            address_sources: crate::HashSet::default(),
            local_inputs: Vec::new(),
            order_before: crate::HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
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
}
