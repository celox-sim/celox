use crate::HashMap;
use crate::HashSet;
use crate::ir::BinaryOp;
use crate::ir::RegisterId;
use crate::ir::SIRBuilder;
use crate::ir::SIRInstruction;
use crate::ir::SIROffset;
use crate::ir::SIRTerminator;
use crate::ir::SIRValue;
use crate::ir::{BitAccess, BlockId, ExecutionUnit, RuntimeErrorInfo};
use crate::logic_tree::NodeId;
use crate::logic_tree::{LogicPath, LogicPathTarget, SLTNode, SLTNodeArena};
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

fn emit_logic_path_store<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    lowerer: &crate::logic_tree::SLTToSIRLowerer,
    builder: &mut SIRBuilder<Addr>,
    path: &LogicPath<Addr>,
    arena: &SLTNodeArena<Addr>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
) {
    match &path.target {
        LogicPathTarget::Var(target) => {
            for node in &path.pre_lower_nodes {
                pre_lower_logic_path_node(lowerer, builder, path, *node, arena, lower_cache);
            }
            let result_reg = lower_logic_path_expr(lowerer, builder, path, arena, lower_cache);
            let width = 1 + target.access.msb - target.access.lsb;
            let old_reg = if path.comb_capture_enable_sites.is_empty() {
                None
            } else {
                let old_reg = builder.alloc_bit(width, false);
                builder.emit(SIRInstruction::Load(
                    old_reg,
                    target.id,
                    SIROffset::Static(target.access.lsb),
                    width,
                ));
                Some(old_reg)
            };
            builder.emit(SIRInstruction::Store(
                target.id,
                SIROffset::Static(target.access.lsb),
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

fn slice_source<Addr: Clone + Eq + Hash>(
    node: NodeId,
    target_width: usize,
    arena: &SLTNodeArena<Addr>,
) -> Option<(NodeId, BitAccess)> {
    if target_width == 0 {
        return None;
    }
    match arena.get(node) {
        SLTNode::Slice { expr, access } => Some((*expr, *access)),
        _ if crate::logic_tree::get_width(node, arena) == target_width => {
            Some((node, BitAccess::new(0, target_width - 1)))
        }
        _ => None,
    }
}

fn try_emit_common_slice_store<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    sorted_by_lsb: &[usize],
    input: &[LogicPath<Addr>],
    lowerer: &crate::logic_tree::SLTToSIRLowerer,
    builder: &mut SIRBuilder<Addr>,
    arena: &SLTNodeArena<Addr>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
    dep_memo: &mut HashMap<NodeId, HashSet<Addr>>,
    inverse_dep_memo: &mut HashMap<Addr, HashSet<NodeId>>,
    target_addr: Addr,
    merged_lsb: usize,
    merged_width: usize,
) -> bool {
    let Some((&first_idx, rest)) = sorted_by_lsb.split_first() else {
        return false;
    };
    let first_path = &input[first_idx];
    if !first_path.local_inputs.is_empty() || !first_path.pre_lower_nodes.is_empty() {
        return false;
    }
    let first_target = first_path.target.var().unwrap();
    let first_width = first_target.access.msb - first_target.access.lsb + 1;
    let Some((common_expr, first_access)) = slice_source(first_path.expr, first_width, arena)
    else {
        return false;
    };
    let source_lsb = first_access.lsb;
    let mut source_msb = first_access.msb;
    for &idx in rest {
        let path = &input[idx];
        if !path.local_inputs.is_empty() || !path.pre_lower_nodes.is_empty() {
            return false;
        }
        let target = path.target.var().unwrap();
        let target_width = target.access.msb - target.access.lsb + 1;
        let Some((expr, access)) = slice_source(path.expr, target_width, arena) else {
            return false;
        };
        if expr != common_expr {
            return false;
        }
        if access.msb - access.lsb + 1 != target_width {
            return false;
        }
        let Some(lhs) = target.access.lsb.checked_add(first_access.lsb) else {
            return false;
        };
        let Some(rhs) = first_target.access.lsb.checked_add(access.lsb) else {
            return false;
        };
        if lhs != rhs {
            return false;
        }
        if access.lsb != source_msb + 1 {
            return false;
        }
        source_msb = access.msb;
    }

    let source_width = crate::logic_tree::get_width(common_expr, arena);
    if source_msb >= source_width || source_msb - source_lsb + 1 != merged_width {
        return false;
    }

    for &idx in sorted_by_lsb {
        collect_logic_path_input_deps(&input[idx], arena, dep_memo, inverse_dep_memo);
    }

    let value_reg = lowerer.lower_region_slice(
        builder,
        common_expr,
        BitAccess::new(source_lsb, source_msb),
        arena,
        lower_cache,
    );
    builder.emit(SIRInstruction::Store(
        target_addr,
        SIROffset::Static(merged_lsb),
        merged_width,
        value_reg,
        Vec::new(),
        Vec::new(),
    ));
    true
}

#[derive(Error, Debug, PartialEq, Eq)]
pub enum SchedulerError<A: Display + Debug + Eq + Hash + Clone> {
    #[error("Combinational loop detected: {}", .blocks.iter().map(|v| format!("{}", v)).collect::<Vec<_>>().join(" -> "))]
    CombinationalLoop { blocks: Vec<LogicPath<A>> },
    #[error("Multiple driver detected: {}", .blocks.iter().map(|v| format!("{}", v)).collect::<Vec<_>>().join(","))]
    MultipleDriver { blocks: Vec<LogicPath<A>> },
}

impl<A: Display + Debug + Eq + Hash + Clone> SchedulerError<A> {
    pub fn map_addr<B: Display + Debug + Eq + Hash + Clone, F>(
        self,
        arena: &SLTNodeArena<A>,
        target_arena: &mut SLTNodeArena<B>,
        f: &F,
    ) -> SchedulerError<B>
    where
        F: Fn(&A) -> B,
    {
        let mut cache = HashMap::default();
        match self {
            SchedulerError::CombinationalLoop { blocks } => SchedulerError::CombinationalLoop {
                blocks: blocks
                    .into_iter()
                    .map(|b| b.map_addr(arena, target_arena, &mut cache, f))
                    .collect(),
            },
            SchedulerError::MultipleDriver { blocks } => SchedulerError::MultipleDriver {
                blocks: blocks
                    .into_iter()
                    .map(|b| b.map_addr(arena, target_arena, &mut cache, f))
                    .collect(),
            },
        }
    }
}

pub struct ScheduleResult<Addr> {
    pub execution_units: Vec<ExecutionUnit<Addr>>,
    pub runtime_errors: HashMap<i64, RuntimeErrorInfo<Addr>>,
}

/// Flush pending DAG nodes, optionally coalescing contiguous stores to the
/// same variable into a single `Concat` + `Store`.
fn flush_pending_coalesce<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    pending: &mut Vec<usize>,
    input: &[LogicPath<Addr>],
    _atoms_map: &HashMap<Addr, Vec<(BitAccess, usize)>>,
    lowerer: &crate::logic_tree::SLTToSIRLowerer,
    builder: &mut SIRBuilder<Addr>,
    arena: &SLTNodeArena<Addr>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
    dep_memo: &mut HashMap<NodeId, HashSet<Addr>>,
    inverse_dep_memo: &mut HashMap<Addr, HashSet<NodeId>>,
    four_state: bool,
    var_widths: &HashMap<Addr, usize>,
) {
    if pending.is_empty() {
        return;
    }

    // Only attempt coalescing when we have multiple paths AND not in four_state mode.
    let can_coalesce = pending.len() > 1 && !four_state;

    if can_coalesce {
        if pending
            .iter()
            .any(|&idx| !matches!(input[idx].target, LogicPathTarget::Var(_)))
        {
            // Observer storage paths are standalone stores. They are ordered by
            // scheduler edges, not coalesced with variable writes.
        } else {
            // Sort a COPY by lsb to check contiguity — don't mutate pending (preserve topo order).
            let mut sorted_by_lsb: Vec<usize> = pending.clone();
            sorted_by_lsb.sort_by_key(|&idx| input[idx].target.var().unwrap().access.lsb);

            // Check contiguity: every next path's lsb == previous path's msb + 1
            let contiguous = sorted_by_lsb.windows(2).all(|w| {
                input[w[1]].target.var().unwrap().access.lsb
                    == input[w[0]].target.var().unwrap().access.msb + 1
            });

            // Check total merged width doesn't exceed variable's declared width.
            let target_addr = input[sorted_by_lsb[0]].target.var().unwrap().id;
            let merged_lsb = input[sorted_by_lsb[0]].target.var().unwrap().access.lsb;
            let merged_msb = input[*sorted_by_lsb.last().unwrap()]
                .target
                .var()
                .unwrap()
                .access
                .msb;
            let merged_width = merged_msb - merged_lsb + 1;
            let within_var_width = var_widths
                .get(&target_addr)
                .is_some_and(|&vw| merged_width <= vw);

            // Don't coalesce if any path has a self-reference (source reads from same var as target).
            let has_self_ref = sorted_by_lsb.iter().any(|&idx| {
                let path = &input[idx];
                path.target
                    .var()
                    .is_some_and(|target| path.sources.iter().any(|s| s.id == target.id))
            });
            let no_comb_capture_enable_sites = sorted_by_lsb
                .iter()
                .all(|idx| input[*idx].comb_capture_enable_sites.is_empty());

            if contiguous && within_var_width && !has_self_ref && no_comb_capture_enable_sites {
                if try_emit_common_slice_store(
                    &sorted_by_lsb,
                    input,
                    lowerer,
                    builder,
                    arena,
                    lower_cache,
                    dep_memo,
                    inverse_dep_memo,
                    target_addr,
                    merged_lsb,
                    merged_width,
                ) {
                    if let Some(to_remove) = inverse_dep_memo.get(&target_addr) {
                        for node in to_remove {
                            lower_cache.remove(node);
                        }
                    }

                    pending.clear();
                    return;
                }

                // Coalesce: lower each path expression, then concat + single wide store.
                // SIR Concat order is [MSB, ..., LSB], so reverse after lsb sort.
                let mut regs: Vec<(RegisterId, usize)> = Vec::with_capacity(sorted_by_lsb.len());
                for &idx in &sorted_by_lsb {
                    let path = &input[idx];
                    collect_logic_path_input_deps(path, arena, dep_memo, inverse_dep_memo);
                    for node in &path.pre_lower_nodes {
                        pre_lower_logic_path_node(
                            lowerer,
                            builder,
                            path,
                            *node,
                            arena,
                            lower_cache,
                        );
                    }
                    let reg = lower_logic_path_expr(lowerer, builder, path, arena, lower_cache);
                    let target = path.target.var().unwrap();
                    let w = 1 + target.access.msb - target.access.lsb;
                    regs.push((reg, w));
                }

                // Reverse so that MSB comes first (Concat order).
                regs.reverse();

                let concat_reg = builder.alloc_bit(merged_width, false);
                builder.emit(SIRInstruction::Concat(
                    concat_reg,
                    regs.iter().map(|(r, _)| *r).collect(),
                ));

                builder.emit(SIRInstruction::Store(
                    target_addr,
                    SIROffset::Static(merged_lsb),
                    merged_width,
                    concat_reg,
                    Vec::new(),
                    Vec::new(),
                ));

                // Invalidate cache for the target variable.
                if let Some(to_remove) = inverse_dep_memo.get(&target_addr) {
                    for node in to_remove {
                        lower_cache.remove(node);
                    }
                }

                pending.clear();
                return;
            }
        }
    }

    // Fallback: emit in original topological order (don't sort pending).
    for &idx in pending.iter() {
        let path = &input[idx];
        collect_logic_path_input_deps(path, arena, dep_memo, inverse_dep_memo);
        emit_logic_path_store(lowerer, builder, path, arena, lower_cache);
        invalidate_logic_path_target(path, inverse_dep_memo, lower_cache);
    }

    pending.clear();
}

fn flush_pending_layer<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    pending: &mut Vec<usize>,
    input: &[LogicPath<Addr>],
    atoms_map: &HashMap<Addr, Vec<(BitAccess, usize)>>,
    lowerer: &crate::logic_tree::SLTToSIRLowerer,
    builder: &mut SIRBuilder<Addr>,
    arena: &SLTNodeArena<Addr>,
    lower_cache: &mut HashMap<NodeId, RegisterId>,
    dep_memo: &mut HashMap<NodeId, HashSet<Addr>>,
    inverse_dep_memo: &mut HashMap<Addr, HashSet<NodeId>>,
    four_state: bool,
    var_widths: &HashMap<Addr, usize>,
) {
    if pending.is_empty() {
        return;
    }

    let mut segment = Vec::new();
    let flush_var_segment = |segment: &mut Vec<usize>,
                             lower_cache: &mut HashMap<NodeId, RegisterId>,
                             dep_memo: &mut HashMap<NodeId, HashSet<Addr>>,
                             inverse_dep_memo: &mut HashMap<Addr, HashSet<NodeId>>,
                             builder: &mut SIRBuilder<Addr>| {
        if segment.is_empty() {
            return;
        }
        let mut groups: Vec<(Addr, Vec<usize>)> = Vec::new();
        for &idx in segment.iter() {
            let target = input[idx].target.var().unwrap().id;
            if let Some((_, group)) = groups
                .iter_mut()
                .find(|(group_target, _)| *group_target == target)
            {
                group.push(idx);
            } else {
                groups.push((target, vec![idx]));
            }
        }
        for (_, mut group) in groups {
            flush_pending_coalesce(
                &mut group,
                input,
                atoms_map,
                lowerer,
                builder,
                arena,
                lower_cache,
                dep_memo,
                inverse_dep_memo,
                four_state,
                var_widths,
            );
        }
        segment.clear();
    };

    for idx in pending.drain(..) {
        if input[idx].target.var().is_some() {
            segment.push(idx);
        } else {
            flush_var_segment(
                &mut segment,
                lower_cache,
                dep_memo,
                inverse_dep_memo,
                builder,
            );
            let mut singleton = vec![idx];
            flush_pending_coalesce(
                &mut singleton,
                input,
                atoms_map,
                lowerer,
                builder,
                arena,
                lower_cache,
                dep_memo,
                inverse_dep_memo,
                four_state,
                var_widths,
            );
        }
    }
    flush_var_segment(
        &mut segment,
        lower_cache,
        dep_memo,
        inverse_dep_memo,
        builder,
    );
}

/// Reorders consecutive runs of DAG SCCs (single-node, no self-loop) so that
/// paths targeting the same variable at the same topological layer are adjacent.
/// This enables `flush_pending_coalesce` to merge them into wide Concat + Store.
fn reorder_dag_runs<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    sccs: &[Vec<usize>],
    adj: &[Vec<usize>],
    layer: &[usize],
    input: &[LogicPath<Addr>],
) -> Vec<Vec<usize>> {
    let is_dag_scc = |scc: &[usize]| scc.len() == 1 && !adj[scc[0]].contains(&scc[0]);

    let mut result: Vec<Vec<usize>> = Vec::with_capacity(sccs.len());
    let mut run_start: Option<usize> = None;

    let flush_run =
        |result: &mut Vec<Vec<usize>>, sccs: &[Vec<usize>], start: usize, end: usize| {
            if end - start <= 1 {
                // Single SCC, no reordering needed
                result.extend(sccs[start..end].iter().cloned());
                return;
            }
            // Collect indices, stable sort by (layer, target_id)
            let mut indices: Vec<usize> = (start..end).collect();
            indices.sort_by(|&a, &b| {
                let na = sccs[a][0];
                let nb = sccs[b][0];
                (
                    layer[na],
                    input[na].target.var().map(|target| target.id),
                    matches!(input[na].target, LogicPathTarget::CombCaptureEvent { .. }),
                )
                    .cmp(&(
                        layer[nb],
                        input[nb].target.var().map(|target| target.id),
                        matches!(input[nb].target, LogicPathTarget::CombCaptureEvent { .. }),
                    ))
            });
            for i in indices {
                result.push(sccs[i].clone());
            }
        };

    for (i, scc) in sccs.iter().enumerate() {
        if is_dag_scc(scc) {
            if run_start.is_none() {
                run_start = Some(i);
            }
        } else {
            if let Some(start) = run_start.take() {
                flush_run(&mut result, sccs, start, i);
            }
            result.push(scc.clone());
        }
    }
    if let Some(start) = run_start {
        flush_run(&mut result, sccs, start, sccs.len());
    }
    result
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
pub fn sort<Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display>(
    input: Vec<LogicPath<Addr>>,
    arena: &SLTNodeArena<Addr>,
    ignored_loops: &HashSet<(Addr, Addr)>,
    true_loops: &HashMap<(Addr, Addr), usize>,
    four_state: bool,
    var_widths: &HashMap<Addr, usize>,
    first_runtime_error_code: i64,
) -> Result<ScheduleResult<Addr>, SchedulerError<Addr>> {
    // 1. Build Atom Map & Multiple Driver Check
    let mut atoms_map: HashMap<Addr, Vec<(BitAccess, usize)>> = HashMap::default();
    for (i, path) in input.iter().enumerate() {
        if let Some(target) = path.target.var() {
            atoms_map
                .entry(target.id)
                .or_default()
                .push((target.access, i));
        }
    }
    for entries in atoms_map.values_mut() {
        entries.sort_by_key(|(access, _)| access.lsb);
        for window in entries.windows(2) {
            if window[0].0.msb >= window[1].0.lsb {
                let blocks = vec![input[window[0].1].clone(), input[window[1].1].clone()];
                return Err(SchedulerError::MultipleDriver { blocks });
            }
        }
    }

    // 2. Build Dependency Graph
    let n = input.len();
    let mut adj = vec![Vec::new(); n];
    for (u, path) in input.iter().enumerate() {
        for source in &path.sources {
            if let Some(candidates) = atoms_map.get(&source.id) {
                for (target_access, v) in candidates {
                    if source.access.overlaps(target_access) {
                        adj[*v].push(u); // Dependency: v must be evaluated for u
                    }
                }
            }
        }
        for target in &path.order_before {
            if target.0 < n {
                adj[u].push(target.0);
            }
        }
    }
    // 3. SCC Extraction (Tarjan)
    let mut ctx = TarjanContext {
        index: 0,
        stack: Vec::new(),
        on_stack: HashSet::default(),
        indices: vec![None; n],
        lowlink: vec![None; n],
        sccs: Vec::new(),
    };
    for i in 0..n {
        if ctx.indices[i].is_none() {
            strong_connect(i, &adj, &mut ctx);
        }
    }
    ctx.sccs.reverse();

    // ── Layer computation + DAG reordering ──
    // Compute topological layers so that same-target paths at the same layer
    // are adjacent, enabling flush_pending_coalesce to merge them.
    let (sccs, layer) = {
        // Build reverse adjacency: rev_adj[u] = predecessors of u
        let mut rev_adj = vec![Vec::new(); n];
        for (v, neighbors) in adj.iter().enumerate() {
            for &u in neighbors {
                rev_adj[u].push(v);
            }
        }

        // Compute layer[node] = 1 + max(layer[pred]) in topo order
        let mut layer = vec![0usize; n];
        for scc in &ctx.sccs {
            let is_dag = scc.len() == 1 && !adj[scc[0]].contains(&scc[0]);
            if is_dag {
                let node = scc[0];
                for &pred in &rev_adj[node] {
                    layer[node] = layer[node].max(layer[pred] + 1);
                }
            } else {
                let mut max_layer = 0usize;
                for &node in scc {
                    for &pred in &rev_adj[node] {
                        if !scc.contains(&pred) {
                            max_layer = max_layer.max(layer[pred] + 1);
                        }
                    }
                }
                for &node in scc {
                    layer[node] = max_layer;
                }
            }
        }

        // Reorder consecutive DAG SCCs by (layer, target_id) so that
        // same-target paths at the same layer become adjacent.
        (reorder_dag_runs(&ctx.sccs, &adj, &layer, &input), layer)
    };

    let mut builder = SIRBuilder::new();
    let lowerer = crate::logic_tree::SLTToSIRLowerer::new(four_state);

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
        emit_logic_path_store(&lowerer, builder, path, arena, lower_cache);
        invalidate_logic_path_target(path, inverse_dep_memo, lower_cache);
    };
    // Maximum blocks in a single EU before flushing to a new one.
    // This prevents Cranelift from choking on massive functions.
    const EU_BLOCK_LIMIT: usize = 20_000;

    let mut result_eus: Vec<ExecutionUnit<Addr>> = Vec::new();
    let mut runtime_errors: HashMap<i64, RuntimeErrorInfo<Addr>> = HashMap::default();
    let mut next_runtime_error_code = first_runtime_error_code;

    let mut pending_layer_indices: Vec<usize> = Vec::new();
    let mut pending_layer: Option<usize> = None;

    // 4. Scheduling: Process each SCC by selecting either Static Unrolling (A) or Dynamic Convergence (B).
    for scc in sccs {
        let mut user_safety_limit = None;
        for &v_idx in &scc {
            for &u_idx in &adj[v_idx] {
                if scc.contains(&u_idx) {
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
            // Flush any buffered DAG nodes before entering a loop SCC.
            flush_pending_layer(
                &mut pending_layer_indices,
                &input,
                &atoms_map,
                &lowerer,
                &mut builder,
                arena,
                &mut lower_cache,
                &mut dep_memo,
                &mut inverse_dep_memo,
                four_state,
                var_widths,
            );
            pending_layer = None;
            let mut authorized = user_safety_limit.is_some();
            'check_scc: for &v_idx in &scc {
                for &u_idx in &adj[v_idx] {
                    if scc.contains(&u_idx)
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
                return Err(SchedulerError::CombinationalLoop {
                    blocks: scc.into_iter().map(|idx| input[idx].clone()).collect(),
                });
            }

            // FAS Sort
            let optimized_scc_order = greedy_fas_sort(&scc, &adj);
            let force_strategy_b = user_safety_limit.is_some();
            let iterations = calculate_required_iterations(&adj, &optimized_scc_order);
            let total_ops_estimate = optimized_scc_order.len() * iterations;
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
                        );
                        continue;
                    };
                    let width = 1 + target.access.msb - target.access.lsb;
                    let addr = target.id;

                    // --- Dynamic Convergence Check Logic ---
                    // For each node in the SCC, we verify if its value changed after this iteration.
                    //
                    // a. Load the current value (pre-update benchmark)
                    let old_val_reg = builder.alloc_bit(width, false);
                    builder.emit(SIRInstruction::Load(
                        old_val_reg,
                        addr,
                        SIROffset::Static(target.access.lsb),
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
                        SIROffset::Static(target.access.lsb),
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
            if builder.block_count() >= EU_BLOCK_LIMIT {
                flush_pending_layer(
                    &mut pending_layer_indices,
                    &input,
                    &atoms_map,
                    &lowerer,
                    &mut builder,
                    arena,
                    &mut lower_cache,
                    &mut dep_memo,
                    &mut inverse_dep_memo,
                    four_state,
                    var_widths,
                );
                pending_layer = None;
                if let Some(eu) = builder.flush_eu() {
                    result_eus.push(eu);
                    // Clear the lowering cache — register IDs are EU-scoped
                    lower_cache.clear();
                }
            }

            let idx = scc[0];
            let this_layer = layer[idx];

            if pending_layer == Some(this_layer) {
                pending_layer_indices.push(idx);
            } else {
                flush_pending_layer(
                    &mut pending_layer_indices,
                    &input,
                    &atoms_map,
                    &lowerer,
                    &mut builder,
                    arena,
                    &mut lower_cache,
                    &mut dep_memo,
                    &mut inverse_dep_memo,
                    four_state,
                    var_widths,
                );
                pending_layer = Some(this_layer);
                pending_layer_indices.push(idx);
            }
        }
    }

    // Flush remaining pending DAG nodes after the SCC loop.
    flush_pending_layer(
        &mut pending_layer_indices,
        &input,
        &atoms_map,
        &lowerer,
        &mut builder,
        arena,
        &mut lower_cache,
        &mut dep_memo,
        &mut inverse_dep_memo,
        four_state,
        var_widths,
    );

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
    })
}
