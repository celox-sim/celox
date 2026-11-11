//! Input dependencies of logic paths and the order of flip-flop writes.

use super::*;

pub(super) fn ranges_cover_access(ranges: &[BitAccess], access: BitAccess) -> bool {
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
pub(super) fn node_reads_only_covered_ranges<Addr: Clone + Eq + Hash>(
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

pub(super) fn collect_node_input_deps<Addr: Clone + Eq + Hash + Debug + Copy + Display>(
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

pub(super) fn collect_logic_path_input_deps<Addr: Clone + Eq + Hash + Debug + Copy + Display>(
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

pub(super) struct TarjanContext {
    pub(super) index: usize,
    pub(super) stack: Vec<usize>,
    pub(super) on_stack: HashSet<usize>,
    pub(super) indices: Vec<Option<usize>>,
    pub(super) lowlink: Vec<Option<usize>>,
    pub(super) sccs: Vec<Vec<usize>>,
}

pub(super) fn strong_connect(u: usize, adj: &Vec<Vec<usize>>, ctx: &mut TarjanContext) {
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

pub(super) fn component_map(adj: &Vec<Vec<usize>>) -> Vec<usize> {
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
pub(super) fn add_acyclic_ff_write_order_edges(
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
