use crate::HashMap;
use crate::HashSet;
use crate::ir::BinaryOp;
use crate::ir::RegisterId;
use crate::ir::SIRBuilder;
use crate::ir::SIRInstruction;
use crate::ir::SIROffset;
use crate::ir::SIRTerminator;
use crate::ir::SIRValue;
use crate::ir::{BitAccess, BlockId, ExecutionUnit};
use crate::logic_tree::NodeId;
use crate::logic_tree::{LogicPath, SLTNodeArena};
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
        crate::logic_tree::SLTNode::Constant(_, _, _) => HashSet::default(),
    };

    for &addr in &deps {
        inverse_memo.entry(addr).or_default().insert(node);
    }
    memo.insert(node, deps.clone());
    deps
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
        B: Hash,
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

/// Schedules and transforms LogicPaths into Simulation Intermediate Representation (SIR).
///
/// This process performs:
/// 1. Dependency analysis to detect multiple drivers and combinational loops.
/// 2. SCC detection via Tarjan's algorithm.
/// 3. Scheduling based on two primary strategies:
///    - **Strategy A (Static Unrolling)**: For DAG parts or loops with small, predictable convergence bounds.
///    - **Strategy B (Dynamic Convergence)**: For complex SCCs or potential "True Loops", implementing
///      runtime oscillation detection and convergence-based repetition.
pub fn sort<Addr: Clone + Eq + Hash + Debug + Copy + Display>(
    input: Vec<LogicPath<Addr>>,
    arena: &SLTNodeArena<Addr>,
    ignored_loops: &HashSet<(Addr, Addr)>,
    true_loops: &HashMap<(Addr, Addr), usize>,
    four_state: bool,
) -> Result<Vec<ExecutionUnit<Addr>>, SchedulerError<Addr>> {
    // 1. Build Atom Map & Multiple Driver Check
    let mut atoms_map: HashMap<Addr, Vec<(BitAccess, usize)>> = HashMap::default();
    for (i, path) in input.iter().enumerate() {
        atoms_map
            .entry(path.target.id)
            .or_default()
            .push((path.target.access, i));
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

        let result_reg = lowerer.lower(builder, path.expr, arena, lower_cache);

        collect_node_input_deps(path.expr, arena, dep_memo, inverse_dep_memo);
        let width = 1 + path.target.access.msb - path.target.access.lsb;
        let addr = path.target.id;

        // Store instructions don't return values, so no alloc needed
        builder.emit(SIRInstruction::Store(
            addr,
            SIROffset::Static(path.target.access.lsb),
            width,
            result_reg,
            Vec::new(),
        ));

        if let Some(to_remove) = inverse_dep_memo.get(&addr) {
            for node in to_remove {
                lower_cache.remove(node);
            }
        }
    };
    // 4. Scheduling: Process each SCC by selecting either Static Unrolling (A) or Dynamic Convergence (B).
    for scc in ctx.sccs {
        let mut user_safety_limit = None;
        for &v_idx in &scc {
            for &u_idx in &adj[v_idx] {
                if scc.contains(&u_idx) {
                    let edge = (input[v_idx].target.id, input[u_idx].target.id);
                    if let Some(&limit) = true_loops.get(&edge) {
                        user_safety_limit =
                            Some(user_safety_limit.map_or(limit, |l: usize| l.max(limit)));
                    }
                }
            }
        }
        let is_loop = scc.len() > 1 || (scc.len() == 1 && adj[scc[0]].contains(&scc[0]));

        if is_loop {
            let mut authorized = user_safety_limit.is_some();
            'check_scc: for &v_idx in &scc {
                for &u_idx in &adj[v_idx] {
                    if scc.contains(&u_idx)
                        && ignored_loops.contains(&(input[v_idx].target.id, input[u_idx].target.id))
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
                    let width = 1 + path.target.access.msb - path.target.access.lsb;
                    let addr = path.target.id;

                    // --- Dynamic Convergence Check Logic ---
                    // For each node in the SCC, we verify if its value changed after this iteration.
                    //
                    // a. Load the current value (pre-update benchmark)
                    let old_val_reg = builder.alloc_bit(width, false);
                    builder.emit(SIRInstruction::Load(
                        old_val_reg,
                        addr,
                        SIROffset::Static(path.target.access.lsb),
                        width,
                    ));
                    collect_node_input_deps(path.expr, arena, &mut dep_memo, &mut inverse_dep_memo);
                    let width = 1 + path.target.access.msb - path.target.access.lsb;
                    let addr = path.target.id;
                    // b. Compute the new value
                    let new_val_reg =
                        lowerer.lower(&mut builder, path.expr, arena, &mut lower_cache);

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
                        SIROffset::Static(path.target.access.lsb),
                        width,
                        new_val_reg,
                        Vec::new(),
                    ));
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
                builder.seal_block(SIRTerminator::Error(1));

                // 5. Exit Block
                builder.switch_to_block(exit_block);
            }
        } else {
            // DAG Part
            emit_node(
                &mut builder,
                scc[0],
                &mut lower_cache,
                &mut dep_memo,
                &mut inverse_dep_memo,
            );
        }
    }

    builder.seal_block(SIRTerminator::Return);
    let (blocks, reg_map, _) = builder.drain();
    Ok(vec![ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map: reg_map,
    }])
}
