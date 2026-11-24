//! Substitution of combinational nets into the paths that read them.
//!
//! A net whose storage nothing observes exists only to carry a value from
//! its driver to its readers. Stored, it costs every evaluation a store and
//! every reader a load; in a partitioned settle it also costs a comparison
//! against its previous value and a mark of its readers, because a
//! partition can forward nothing to another one.
//!
//! This pass removes such nets before the program is scheduled:
//!
//! - a net nothing reads is dropped with its driver, and so is whatever
//!   only dropped nets read;
//! - a net one path reads becomes part of that path's expression;
//! - a net several paths read does too when its expression is small enough
//!   to evaluate once per reader.
//!
//! The caller says which nets must stay ([`inline_nets`]'s `pinned`): the
//! ones anything but a combinational path reads or writes, and the ones
//! that stay observable from outside.

mod columns;

use std::fmt::{Debug, Display};
use std::hash::Hash;

use celox_design::{BitAccess, VarAtomBase};

use crate::{
    HashMap, HashSet, LogicPath, LogicPathId, LogicPathTarget, NodeId, SLTNode, SLTNodeArena,
    SLTNodeFactsError,
};

/// The cost limit for copying an expression into several readers. A plain
/// wire or slice can replace a load for free. Gathering distinct state
/// inputs into a word costs the additional reads in each copy.
pub const DUPLICATED_NET_OPERATORS: usize = 2;

/// What [`inline_nets`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetInlineReport<A> {
    /// The variables whose paths left the program, in order: nothing
    /// stores them any more, though they still name state objects.
    pub removed: Vec<A>,
    /// Nets dropped because nothing read them.
    pub dead: usize,
    /// Nets substituted into their one reader.
    pub inlined: usize,
    /// Nets substituted into each of several readers.
    pub duplicated: usize,
    pub paths_before: usize,
    pub paths_after: usize,
}

/// One path's part of a net: the range of the variable it defines.
struct Piece {
    path: usize,
    target: BitAccess,
    expr: NodeId,
}

/// A net the pass may take out of the program: the disjoint ranges plain
/// paths define of one variable, lowest first, and the cost of their
/// expressions together.
struct Candidate {
    pieces: Vec<Piece>,
    cost: usize,
}

impl Candidate {
    /// The consecutive pieces supplying this read, or None for a gap.
    fn covering(&self, access: BitAccess) -> Option<&[Piece]> {
        let start = self.pieces.partition_point(|p| p.target.msb < access.lsb);
        let mut next = access.lsb;
        for (i, piece) in self.pieces[start..].iter().enumerate() {
            if piece.target.lsb > next {
                break;
            }
            if piece.target.msb >= access.msb {
                return Some(&self.pieces[start..=start + i]);
            }
            next = piece.target.msb + 1;
        }
        None
    }
}

/// The operands of `node`.
fn operands<A: Clone + Eq + Hash>(node: &SLTNode<A>, out: &mut Vec<NodeId>) {
    match node {
        SLTNode::Binary(lhs, _, rhs) => out.extend([*lhs, *rhs]),
        SLTNode::Unary(_, operand)
        | SLTNode::Slice { expr: operand, .. }
        | SLTNode::Capture { expr: operand, .. } => out.push(*operand),
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => out.extend([*cond, *then_expr, *else_expr]),
        SLTNode::Concat(parts) => out.extend(parts.iter().map(|(part, _)| *part)),
        SLTNode::Input { index, .. } => out.extend(index.iter().map(|entry| entry.node)),
        SLTNode::Constant(..) | SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => {}
    }
}

/// Whether the rewrite rebuilds every node of the expression of `root`:
/// operators over reads, including the index expressions of a memory read.
/// Folds bind names of their own and a capture is evaluated at one position.
fn plain_expr<A: Clone + Eq + Hash>(root: NodeId, arena: &SLTNodeArena<A>) -> bool {
    let mut pending = vec![root];
    let mut seen = HashSet::default();
    while let Some(node) = pending.pop() {
        if !seen.insert(node) {
            continue;
        }
        match arena.get(node) {
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } | SLTNode::Capture { .. } => {
                return false;
            }
            node => operands(node, &mut pending),
        }
    }
    true
}

/// Whether the expression of `root` holds a constant with unknown bits. A
/// store converts them for a two-state variable; a substituted expression
/// would carry them into operators that tell them apart.
fn has_unknown_constant<A: Clone + Eq + Hash>(root: NodeId, arena: &SLTNodeArena<A>) -> bool {
    let mut pending = vec![root];
    let mut seen = HashSet::default();
    while let Some(node) = pending.pop() {
        if !seen.insert(node) {
            continue;
        }
        match arena.get(node) {
            SLTNode::Constant(_, mask, _, _) if mask.bits() != 0 => return true,
            node => operands(node, &mut pending),
        }
    }
    false
}

/// Assembling independent state inputs in a reader requires a load per
/// distinct source, even when all the drivers are just wires. Charge the
/// loads beyond the one needed to read an already assembled value.
fn extra_input_loads<A: Copy + Eq + Hash>(
    parts: impl ExactSizeIterator<Item = NodeId>,
    arena: &SLTNodeArena<A>,
    drivers: &HashMap<A, Vec<usize>>,
) -> usize {
    if parts.len() < 2 {
        return 0;
    }
    let mut inputs = HashSet::default();
    for part in parts {
        match arena.get(part) {
            SLTNode::Input { variable, .. } if !drivers.contains_key(variable) => {
                inputs.insert(*variable);
            }
            _ => return 0,
        }
    }
    inputs.len() - 1
}

/// Cost of evaluating the expressions of `roots`, counting shared nodes once.
fn expression_cost<A: Copy + Eq + Hash>(
    roots: impl Iterator<Item = NodeId>,
    arena: &SLTNodeArena<A>,
    drivers: &HashMap<A, Vec<usize>>,
) -> usize {
    let mut pending: Vec<NodeId> = roots.collect();
    let mut seen = HashSet::default();
    let mut count = 0;
    while let Some(node) = pending.pop() {
        if !seen.insert(node) {
            continue;
        }
        let node = arena.get(node);
        count += match node {
            // A shared indexed load belongs at its driver, even if the address is cheap.
            SLTNode::Input { index, .. } if !index.is_empty() => DUPLICATED_NET_OPERATORS + 1,
            SLTNode::Input { .. } | SLTNode::Constant(..) | SLTNode::Slice { .. } => 0,
            SLTNode::Concat(parts) => {
                extra_input_loads(parts.iter().map(|p| p.0), arena, drivers).max(1)
            }
            _ => 1,
        };
        operands(node, &mut pending);
    }
    count
}

/// A path the pass may rewrite or remove: a plain definition of a range of
/// a variable by an expression of plain reads.
fn plain_path<A: Clone + Eq + Hash>(path: &LogicPath<A>, arena: &SLTNodeArena<A>) -> bool {
    matches!(path.target, LogicPathTarget::Var(_))
        && path.previous_sources.is_empty()
        && path.address_sources.is_empty()
        && path.local_inputs.is_empty()
        && path.pre_lower_nodes.is_empty()
        && plain_expr(path.expr, arena)
}

/// The variables on a dependency cycle of the combinational paths. A path
/// through them settles by iteration, which a substitution would reshape.
fn cyclic_variables<A: Copy + Eq + Hash + Ord>(
    drivers: &HashMap<A, Vec<usize>>,
    reads: &[HashSet<A>],
) -> HashSet<A> {
    // Tarjan's strongly connected components over the variables, an edge
    // from each driven variable to what its drivers read.
    let mut order: Vec<A> = drivers.keys().copied().collect();
    order.sort_unstable();
    let successors = |var: &A| -> Vec<A> {
        let mut list: Vec<A> = drivers
            .get(var)
            .into_iter()
            .flatten()
            .flat_map(|&path| reads[path].iter().copied())
            .filter(|source| drivers.contains_key(source))
            .collect();
        list.sort_unstable();
        list.dedup();
        list
    };
    let mut index: HashMap<A, usize> = HashMap::default();
    let mut low: HashMap<A, usize> = HashMap::default();
    let mut on_stack: HashSet<A> = HashSet::default();
    let mut stack: Vec<A> = Vec::new();
    let mut cyclic: HashSet<A> = HashSet::default();
    let mut next = 0;
    for root in order {
        if index.contains_key(&root) {
            continue;
        }
        let mut work: Vec<(A, Vec<A>)> = Vec::new();
        index.insert(root, next);
        low.insert(root, next);
        next += 1;
        stack.push(root);
        on_stack.insert(root);
        work.push((root, successors(&root)));
        while let Some((var, pending)) = work.last_mut() {
            let var = *var;
            if let Some(successor) = pending.pop() {
                if successor == var {
                    cyclic.insert(var);
                } else if let std::collections::hash_map::Entry::Vacant(first) =
                    index.entry(successor)
                {
                    first.insert(next);
                    low.insert(successor, next);
                    next += 1;
                    stack.push(successor);
                    on_stack.insert(successor);
                    work.push((successor, successors(&successor)));
                } else if on_stack.contains(&successor) {
                    let reached = index[&successor];
                    let entry = low.get_mut(&var).expect("visited");
                    *entry = (*entry).min(reached);
                }
                continue;
            }
            work.pop();
            if let Some((parent, _)) = work.last() {
                let reached = low[&var];
                let entry = low.get_mut(parent).expect("visited");
                *entry = (*entry).min(reached);
            }
            if low[&var] == index[&var] {
                let mut members = Vec::new();
                while let Some(member) = stack.pop() {
                    on_stack.remove(&member);
                    members.push(member);
                    if member == var {
                        break;
                    }
                }
                if members.len() > 1 {
                    cyclic.extend(members);
                }
            }
        }
    }
    cyclic
}

/// Remove the nets of `paths` that only carry a value between paths, by
/// substituting them into their readers or, when nothing reads them, by
/// dropping them.
///
/// `pinned` are the variables whose storage must keep its value: every one
/// that anything other than these paths reads or writes, and every one that
/// stays observable. The paths' values otherwise depend only on the nets
/// that remain and are the same as before.
pub fn inline_nets<A: Copy + Eq + Hash + Ord + Debug + Display>(
    paths: &mut Vec<LogicPath<A>>,
    arena: &mut SLTNodeArena<A>,
    pinned: &HashSet<A>,
) -> Result<NetInlineReport<A>, SLTNodeFactsError> {
    inline_nets_with_arrays(paths, arena, pinned, &HashMap::default())
}

/// Unpacked arrays stay in state unless all their reads reduce a static
/// transpose to words. Indices and ranges here use logical packed offsets.
pub fn inline_nets_with_arrays<A: Copy + Eq + Hash + Ord + Debug + Display>(
    paths: &mut Vec<LogicPath<A>>,
    arena: &mut SLTNodeArena<A>,
    pinned: &HashSet<A>,
    element_widths: &HashMap<A, usize>,
) -> Result<NetInlineReport<A>, SLTNodeFactsError> {
    let mut report = NetInlineReport {
        removed: Vec::new(),
        dead: 0,
        inlined: 0,
        duplicated: 0,
        paths_before: paths.len(),
        paths_after: paths.len(),
    };
    let plain: Vec<bool> = paths.iter().map(|path| plain_path(path, arena)).collect();
    let mut pinned: HashSet<A> = pinned.clone();
    // What a path the pass leaves alone reads, it reads from state; a path
    // another one is ordered after stays where it is.
    let mut ordered: HashSet<usize> = HashSet::default();
    for (index, path) in paths.iter().enumerate() {
        ordered.extend(path.order_before.iter().map(|id| id.0));
        if !plain[index] {
            pinned.extend(
                path.sources
                    .iter()
                    .chain(&path.previous_sources)
                    .chain(&path.address_sources)
                    .map(|source| source.id),
            );
            pinned.extend(path.local_inputs.iter().map(|(var, _)| *var));
        }
        if let LogicPathTarget::Process {
            sensitivity_counters,
            startup,
            ..
        } = &path.target
        {
            pinned.extend(sensitivity_counters.iter().map(|(source, _)| source.id));
            if let Some(startup) = startup {
                pinned.extend(startup.inputs.iter().map(|(source, _)| source.id));
            }
        }
    }
    // The variables each plain path reads, with the ranges read. What any
    // other holder of an expression reads (a path with a fold or a capture,
    // an event's guard and arguments, a pre-lowered node), it loads from
    // state; so does a signed read, which interprets the stored value.
    let mut reads: Vec<HashSet<A>> = vec![HashSet::default(); paths.len()];
    let mut read_ranges: Vec<(A, BitAccess)> = Vec::new();
    {
        let mut seen: HashSet<NodeId> = HashSet::default();
        let mut held: Vec<NodeId> = Vec::new();
        for (index, path) in paths.iter().enumerate() {
            reads[index].extend(path.sources.iter().map(|source| source.id));
            held.extend(path.local_inputs.iter().map(|(_, node)| *node));
            held.extend(path.pre_lower_nodes.iter().copied());
            if let LogicPathTarget::CombCaptureEvent {
                guard,
                args,
                loop_runner,
                ..
            } = &path.target
            {
                held.extend(guard.iter().chain(args).chain(loop_runner).copied());
            }
            if !plain[index] {
                held.push(path.expr);
                continue;
            }
            let mut pending = vec![path.expr];
            let mut local = HashSet::default();
            while let Some(node) = pending.pop() {
                if !local.insert(node) {
                    continue;
                }
                let first = seen.insert(node);
                let node = arena.get(node);
                if let SLTNode::Input {
                    variable,
                    signed,
                    access,
                    index: indices,
                } = node
                {
                    reads[index].insert(*variable);
                    if first {
                        read_ranges.push((*variable, *access));
                        if *signed || !indices.is_empty() {
                            pinned.insert(*variable);
                        }
                    }
                }
                operands(node, &mut pending);
            }
        }
        let mut visited: HashSet<NodeId> = HashSet::default();
        while let Some(node) = held.pop() {
            if !visited.insert(node) {
                continue;
            }
            if let SLTNode::Input { variable, .. } = arena.get(node) {
                pinned.insert(*variable);
            }
            held.extend(crate::SLTToSIRLowerer::node_children(node, arena));
        }
    }
    let mut drivers: HashMap<A, Vec<usize>> = HashMap::default();
    for (index, path) in paths.iter().enumerate() {
        if let Some(var) = path.target.var() {
            drivers.entry(var.id).or_default().push(index);
        }
    }
    let mut readers: HashMap<A, Vec<usize>> = HashMap::default();
    for (index, vars) in reads.iter().enumerate() {
        if !plain[index] {
            continue;
        }
        for var in vars {
            readers.entry(*var).or_default().push(index);
        }
    }
    let cyclic = cyclic_variables(&drivers, &reads);
    let mut arrays: Vec<_> = element_widths.iter().collect();
    arrays.sort_unstable_by_key(|(array, _)| **array);
    for (&array, &rows) in arrays {
        if pinned.contains(&array) || cyclic.contains(&array) {
            pinned.insert(array);
            continue;
        }
        let Some(plan) = columns::match_columns(
            array,
            rows,
            paths,
            arena,
            &drivers,
            &readers,
            &ordered,
            &plain,
            element_widths,
        ) else {
            pinned.insert(array);
            continue;
        };
        // The shortcut follows an existing acyclic dependency. Keep the
        // reader and range indices in sync for ordinary inlining below.
        let consumers = readers.remove(&array).unwrap();
        let word = plan.reduce(arena)?;
        let words = plan.words();
        let source = plan.source;
        read_ranges.extend(words.iter().map(|atom| (source, atom.access)));
        for (index, lane) in plan.outputs {
            let path = &mut paths[index];
            path.expr = arena.alloc(SLTNode::Slice {
                expr: word,
                access: BitAccess::new(lane, lane),
            })?;
            path.sources.retain(|atom| atom.id != array);
            path.sources.extend(words.iter().copied());
            reads[index].remove(&array);
            reads[index].insert(source);
        }
        readers.entry(source).or_default().extend(consumers);
    }
    // The nets a substitution or a removal may take out of the program:
    // variables whose every driver is a plain path defining a range of its
    // own.
    let signedness = crate::SLTToSIRLowerer::new(false);
    let mut eligible: HashMap<A, Candidate> = HashMap::default();
    let mut removable = HashSet::default();
    'vars: for (var, list) in &drivers {
        if pinned.contains(var)
            || cyclic.contains(var)
            || !list.iter().all(|&i| {
                plain[i]
                    && !ordered.contains(&i)
                    && paths[i].comb_capture_enable_sites.is_empty()
                    && !paths[i].comb_capture_enable_always
                    && paths[i].order_before.is_empty()
            })
        {
            continue;
        }
        // An unread pure driver can go even if assigning it to state would
        // convert its width, signedness or unknown bits. Substitution below
        // requires those conversions to be identities.
        if readers.contains_key(var) {
            removable.insert(*var);
        }
        let mut pieces = Vec::with_capacity(list.len());
        for &index in list {
            let path = &paths[index];
            let LogicPathTarget::Var(target) = &path.target else {
                continue 'vars;
            };
            if crate::get_width(path.expr, arena) != target.access.msb - target.access.lsb + 1
                || has_unknown_constant(path.expr, arena)
                // The reads it replaces are unsigned, and the lowering
                // extends the operands of a comparison by their signedness.
                || signedness.get_bound_signed(path.expr, arena)
            {
                continue 'vars;
            }
            pieces.push(Piece {
                path: index,
                target: target.access,
                expr: path.expr,
            });
        }
        pieces.sort_unstable_by_key(|piece| piece.target.lsb);
        // Two drivers of one bit resolve in state.
        if pieces
            .windows(2)
            .any(|pair| pair[0].target.msb >= pair[1].target.lsb)
        {
            continue;
        }
        let cost = expression_cost(pieces.iter().map(|piece| piece.expr), arena, &drivers);
        eligible.insert(*var, Candidate { pieces, cost });
    }
    // Substitution introduces an implicit concat only over the pieces
    // actually read. Slices of a wide net can still be forwarded cheaply.
    for (variable, access) in read_ranges {
        if let Some(candidate) = eligible.get_mut(&variable) {
            if let Some(pieces) = candidate.covering(access) {
                let loads = extra_input_loads(pieces.iter().map(|p| p.expr), arena, &drivers);
                candidate.cost = candidate.cost.max(loads);
            } else {
                eligible.remove(&variable);
            }
        }
    }
    // A net nothing reads is dead, and so is what only dead nets read.
    let mut dead: HashSet<A> = HashSet::default();
    let mut dead_paths: HashSet<usize> = HashSet::default();
    {
        let mut live_readers: HashMap<A, usize> = eligible
            .keys()
            .map(|var| (*var, readers.get(var).map_or(0, Vec::len)))
            .collect();
        let mut work: Vec<A> = live_readers
            .iter()
            .filter(|(_, count)| **count == 0)
            .map(|(var, _)| *var)
            .collect();
        work.sort_unstable();
        while let Some(var) = work.pop() {
            if !dead.insert(var) {
                continue;
            }
            for piece in &eligible[&var].pieces {
                dead_paths.insert(piece.path);
                let mut sources: Vec<A> = reads[piece.path].iter().copied().collect();
                sources.sort_unstable();
                for source in sources {
                    if let Some(count) = live_readers.get_mut(&source) {
                        *count -= 1;
                        if *count == 0 {
                            work.push(source);
                        }
                    }
                }
            }
        }
    }
    let candidates: HashMap<A, Candidate> = eligible
        .into_iter()
        .filter(|(var, _)| !dead.contains(var))
        .collect();
    // The candidates each candidate reads.
    let sources = |var: &A| -> Vec<A> {
        let mut list: Vec<A> = candidates[var]
            .pieces
            .iter()
            .flat_map(|piece| reads[piece.path].iter())
            .filter(|source| candidates.contains_key(*source))
            .copied()
            .collect();
        list.sort_unstable();
        list.dedup();
        list
    };
    // The candidates in dependency order, each after what it reads.
    let mut order: Vec<A> = Vec::with_capacity(candidates.len());
    {
        let mut done: HashSet<A> = HashSet::default();
        let mut roots: Vec<A> = candidates.keys().copied().collect();
        roots.sort_unstable();
        for root in roots {
            if !done.insert(root) {
                continue;
            }
            // The candidates are acyclic: no variable on a cycle is one.
            let mut work: Vec<(A, Vec<A>)> = vec![(root, sources(&root))];
            while let Some((var, pending)) = work.last_mut() {
                let var = *var;
                match pending.pop() {
                    Some(source) => {
                        if done.insert(source) {
                            work.push((source, sources(&source)));
                        }
                    }
                    None => {
                        order.push(var);
                        work.pop();
                    }
                }
            }
        }
    }
    // What a copy of each net costs: its operators and those of the nets
    // it reads that are small enough to be copied themselves. A larger net
    // it reads ends there, evaluated once in its own place.
    let mut copy_cost: HashMap<A, usize> = HashMap::default();
    for var in &order {
        let cost = candidates[var].cost
            + sources(var)
                .iter()
                .map(|source| copy_cost[source])
                .filter(|cost| *cost <= DUPLICATED_NET_OPERATORS)
                .sum::<usize>();
        copy_cost.insert(*var, cost);
    }
    // Readers first, the paths that end up evaluating each net's
    // expression: one for a path that stays, and for a reading net its own
    // such paths when it is substituted, or its paths that read this one
    // when it stays. A net evaluated once joins its reader whole; one
    // evaluated in several places does when a copy of it is small.
    let piece_of: HashMap<usize, A> = candidates
        .iter()
        .flat_map(|(var, candidate)| candidate.pieces.iter().map(|piece| (piece.path, *var)))
        .collect();
    // A diamond can reach the same surviving path through several nets.
    // Its shared SLT node is evaluated once in that path.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Readers {
        One(usize),
        Several,
    }
    let mut evaluations: HashMap<A, Readers> = HashMap::default();
    let mut inlined: HashSet<A> = HashSet::default();
    for var in order.iter().rev() {
        let destinations = readers[var]
            .iter()
            .filter(|p| !dead_paths.contains(p))
            .map(|p| match piece_of.get(p) {
                Some(net) if inlined.contains(net) => evaluations[net],
                _ => Readers::One(*p),
            });
        let destination = destinations
            .reduce(|a, b| if a == b { a } else { Readers::Several })
            .expect("a live candidate has a live reader");
        evaluations.insert(*var, destination);
        if destination != Readers::Several || copy_cost[var] <= DUPLICATED_NET_OPERATORS {
            inlined.insert(*var);
        }
    }
    if inlined.is_empty() && dead.is_empty() {
        return Ok(report);
    }
    // Each substituted net's ranges as expressions over the nets that
    // remain, with the ranges of those that each one reads: a reader of
    // one range depends on that range's sources alone.
    let mut resolved: HashMap<A, Vec<(BitAccess, NodeId)>> = HashMap::default();
    let mut resolved_sources: HashMap<A, Vec<(BitAccess, HashSet<VarAtomBase<A>>)>> =
        HashMap::default();
    let mut cache: HashMap<NodeId, NodeId> = HashMap::default();
    // Operand positions in the original DAG: the condition of a folded mux
    // or the input masked by a forwarded zero.
    let mut discarded_operand = HashMap::default();
    let remaining_sources =
        |path: &LogicPath<A>,
         resolved_sources: &HashMap<A, Vec<(BitAccess, HashSet<VarAtomBase<A>>)>>| {
            let mut sources: HashSet<VarAtomBase<A>> = HashSet::default();
            for source in &path.sources {
                match resolved_sources.get(&source.id) {
                    Some(ranges) => {
                        for (target, inner) in ranges {
                            if target.msb >= source.access.lsb && target.lsb <= source.access.msb {
                                sources.extend(inner.iter().copied());
                            }
                        }
                    }
                    None => {
                        sources.insert(*source);
                    }
                }
            }
            sources
        };
    for var in &order {
        let candidate = &candidates[var];
        let mut ranges = Vec::with_capacity(candidate.pieces.len());
        for piece in &candidate.pieces {
            ranges.push((
                piece.target,
                substitute(
                    piece.expr,
                    &resolved,
                    arena,
                    &mut cache,
                    &mut discarded_operand,
                )?,
            ));
        }
        // Copy costs were estimated before substituting the drivers. A mux
        // tree that now returns a literal is cheap even with several readers.
        if !inlined.contains(var) {
            if !ranges
                .iter()
                .all(|(_, node)| matches!(arena.get(*node), SLTNode::Constant(..)))
            {
                continue;
            }
            inlined.insert(*var);
        }
        let mut sources = Vec::with_capacity(candidate.pieces.len());
        for (piece, &(_, expr)) in candidate.pieces.iter().zip(&ranges) {
            let path = &mut paths[piece.path];
            prune_folded_inputs(path, expr, &discarded_operand, arena);
            sources.push((piece.target, remaining_sources(path, &resolved_sources)));
        }
        resolved.insert(*var, ranges);
        resolved_sources.insert(*var, sources);
    }
    report.inlined = inlined
        .iter()
        .filter(|var| evaluations[*var] != Readers::Several)
        .count();
    report.duplicated = inlined.len() - report.inlined;
    let mut removed: HashSet<usize> = inlined
        .iter()
        .flat_map(|var| candidates[var].pieces.iter().map(|piece| piece.path))
        .chain(dead_paths.iter().copied())
        .collect();
    for (index, path) in paths.iter_mut().enumerate() {
        if removed.contains(&index)
            || !plain[index]
            || !reads[index].iter().any(|var| inlined.contains(var))
        {
            continue;
        }
        let expr = substitute(
            path.expr,
            &resolved,
            arena,
            &mut cache,
            &mut discarded_operand,
        )?;
        prune_folded_inputs(path, expr, &discarded_operand, arena);
        path.expr = expr;
        path.sources = remaining_sources(path, &resolved_sources);
    }
    // Revisit initially read drivers after folding. Start at paths whose
    // writes must remain and follow their reads in the rewritten graph.
    if !discarded_operand.is_empty() {
        let mut live = HashSet::default();
        let mut seen = HashSet::default();
        let mut pending: Vec<_> = paths
            .iter()
            .enumerate()
            .filter_map(|(i, path)| {
                (!removed.contains(&i)
                    && !path.target.var().is_some_and(|v| removable.contains(&v.id)))
                .then_some(i)
            })
            .collect();
        while let Some(i) = pending.pop() {
            let mut read = |var: &A| {
                if removable.contains(var) && live.insert(*var) {
                    pending.extend(
                        drivers[var]
                            .iter()
                            .copied()
                            .filter(|j| !removed.contains(j)),
                    );
                }
            };
            for source in &paths[i].sources {
                read(&source.id);
            }
            if plain[i] {
                let mut expr = vec![paths[i].expr];
                while let Some(id) = expr.pop() {
                    if !seen.insert(id) {
                        continue;
                    }
                    match arena.get(id) {
                        SLTNode::Input {
                            variable, index, ..
                        } => {
                            read(variable);
                            expr.extend(index.iter().map(|entry| entry.node));
                        }
                        node => operands(node, &mut expr),
                    }
                }
            }
        }
        for var in removable
            .difference(&live)
            .filter(|var| !inlined.contains(*var))
        {
            dead.insert(*var);
            removed.extend(drivers[var].iter().copied());
        }
    }
    report.dead = dead.len();
    report.removed = inlined.iter().chain(&dead).copied().collect();
    report.removed.sort_unstable();
    // Drop the removed paths and renumber what orders the rest.
    let mut renumber: Vec<Option<usize>> = Vec::with_capacity(paths.len());
    let mut next = 0;
    for index in 0..paths.len() {
        if removed.contains(&index) {
            renumber.push(None);
        } else {
            renumber.push(Some(next));
            next += 1;
        }
    }
    let mut index = 0;
    paths.retain(|_| {
        let keep = !removed.contains(&index);
        index += 1;
        keep
    });
    for path in paths.iter_mut() {
        path.order_before = path
            .order_before
            .iter()
            .filter_map(|id| renumber[id.0].map(LogicPathId))
            .collect();
    }
    report.paths_after = paths.len();
    Ok(report)
}

/// Drop a data dependency only when it was read solely in operands removed
/// by folding. A declared read can cover several narrower expression reads.
fn prune_folded_inputs<A: Copy + Eq + Hash>(
    path: &mut LogicPath<A>,
    expr: NodeId,
    discarded_operand: &HashMap<NodeId, usize>,
    arena: &SLTNodeArena<A>,
) {
    if expr == path.expr
        || discarded_operand.is_empty()
        || path.comb_capture_enable_always
        || !path.comb_capture_enable_sites.is_empty()
    {
        return;
    }
    let mut discarded = HashSet::default();
    let mut live = HashSet::default();
    let mut pending = vec![(path.expr, false)];
    let mut seen = HashSet::default();
    let mut children = Vec::new();
    while let Some((id, unused)) = pending.pop() {
        if !seen.insert((id, unused)) {
            continue;
        }
        let node = arena.get(id);
        match node {
            SLTNode::Input {
                variable,
                access,
                index,
                ..
            } => {
                pending.extend(index.iter().map(|entry| (entry.node, unused)));
                if unused {
                    discarded.insert(VarAtomBase {
                        id: *variable,
                        access: *access,
                    });
                } else {
                    live.insert(*variable);
                }
            }
            _ => {
                operands(node, &mut children);
                let skip = discarded_operand.get(&id).copied();
                pending.extend(
                    children
                        .drain(..)
                        .enumerate()
                        .map(|(i, child)| (child, unused || skip == Some(i))),
                );
            }
        }
    }
    path.sources
        .retain(|source| !discarded.contains(source) || live.contains(&source.id));
}

/// The bits `access` of a substituted net: the part of each of its ranges
/// that `access` takes, most significant first.
fn read_ranges<A: Clone + Eq + Hash>(
    ranges: &[(BitAccess, NodeId)],
    access: BitAccess,
    arena: &mut SLTNodeArena<A>,
) -> Result<NodeId, SLTNodeFactsError> {
    let mut parts: Vec<(NodeId, usize)> = Vec::new();
    for &(target, expr) in ranges.iter().rev() {
        if target.msb < access.lsb || target.lsb > access.msb {
            continue;
        }
        let (lsb, msb) = (access.lsb.max(target.lsb), access.msb.min(target.msb));
        let part = if lsb == target.lsb && msb == target.msb {
            expr
        } else {
            arena.alloc(SLTNode::Slice {
                expr,
                access: BitAccess {
                    lsb: lsb - target.lsb,
                    msb: msb - target.lsb,
                },
            })?
        };
        parts.push((part, msb - lsb + 1));
    }
    match parts.as_slice() {
        [(part, _)] => Ok(*part),
        _ => arena.alloc(SLTNode::Concat(parts)),
    }
}

/// The expression of `root` with each read of a substituted net replaced by
/// that net's expression.
fn substitute<A: Copy + Eq + Hash>(
    root: NodeId,
    resolved: &HashMap<A, Vec<(BitAccess, NodeId)>>,
    arena: &mut SLTNodeArena<A>,
    cache: &mut HashMap<NodeId, NodeId>,
    discarded_operand: &mut HashMap<NodeId, usize>,
) -> Result<NodeId, SLTNodeFactsError> {
    // Operands first, without recursion: the cones are deep.
    let mut work = vec![(root, false)];
    while let Some((node, expanded)) = work.pop() {
        if cache.contains_key(&node) {
            continue;
        }
        let value = arena.get(node).clone();
        if !expanded {
            let mut pending = Vec::new();
            operands(&value, &mut pending);
            pending.retain(|operand| !cache.contains_key(operand));
            if !pending.is_empty() {
                work.push((node, true));
                work.extend(pending.into_iter().map(|operand| (operand, false)));
                continue;
            }
        }
        let result = match value {
            SLTNode::Input {
                variable,
                ref index,
                access,
                ..
            } if index.is_empty() && resolved.contains_key(&variable) => {
                read_ranges(&resolved[&variable], access, arena)?
            }
            SLTNode::Input {
                variable,
                signed,
                mut index,
                access,
            } => {
                if index.iter().all(|entry| cache[&entry.node] == entry.node) {
                    node
                } else {
                    for entry in &mut index {
                        entry.node = cache[&entry.node];
                    }
                    arena.alloc(SLTNode::Input {
                        variable,
                        signed,
                        index,
                        access,
                    })?
                }
            }
            SLTNode::Constant(..) | SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => node,
            SLTNode::Slice { expr, access } => {
                let new = cache[&expr];
                if new == expr {
                    node
                } else {
                    arena.alloc(SLTNode::Slice { expr: new, access })?
                }
            }
            SLTNode::Capture { expr, key } => {
                let new = cache[&expr];
                if new == expr {
                    node
                } else {
                    arena.alloc(SLTNode::Capture { expr: new, key })?
                }
            }
            SLTNode::Binary(lhs, op, rhs) => {
                let (new_lhs, new_rhs) = (cache[&lhs], cache[&rhs]);
                if new_lhs == lhs && new_rhs == rhs {
                    node
                } else if op == celox_design::BinaryOp::And
                    && let Some((side, zero)) = [new_lhs, new_rhs].into_iter().enumerate().find(|(_, id)| {
                        matches!(arena.get(*id), SLTNode::Constant(value, mask, width, false)
                            if value.bits() == 0 && mask.bits() == 0 && Some(*width) == arena.node_width(node))
                    })
                {
                    // The unsigned zero already has the result's width and signedness.
                    discarded_operand.insert(node, 1 - side);
                    zero
                } else {
                    arena.alloc(SLTNode::Binary(new_lhs, op, new_rhs))?
                }
            }
            SLTNode::Unary(op, operand) => {
                let new = cache[&operand];
                if new == operand {
                    node
                } else {
                    arena.alloc(SLTNode::Unary(op, new))?
                }
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                let (new_cond, new_then, new_else) =
                    (cache[&cond], cache[&then_expr], cache[&else_expr]);
                if new_cond == cond && new_then == then_expr && new_else == else_expr {
                    node
                } else {
                    let result = crate::canon::intern(
                        arena,
                        SLTNode::Mux {
                            cond: new_cond,
                            then_expr: new_then,
                            else_expr: new_else,
                        },
                    )?;
                    if result == new_then && new_then == new_else {
                        discarded_operand.insert(node, 0);
                    }
                    result
                }
            }
            SLTNode::Concat(parts) => {
                let new: Vec<(NodeId, usize)> = parts
                    .iter()
                    .map(|&(part, width)| (cache[&part], width))
                    .collect();
                if new == parts {
                    node
                } else {
                    arena.alloc(SLTNode::Concat(new))?
                }
            }
        };
        cache.insert(node, result);
    }
    Ok(cache[&root])
}

#[cfg(test)]
mod tests {
    use celox_design::{BinaryOp, BitAccess, VarAtomBase};
    use num_bigint::BigUint;

    use super::{DUPLICATED_NET_OPERATORS, expression_cost, inline_nets};
    use crate::{HashSet, LogicPath, LogicPathId, LogicPathTarget, NodeId, SLTNode, SLTNodeArena};

    fn input(arena: &mut SLTNodeArena<u32>, variable: u32, lsb: usize, msb: usize) -> NodeId {
        arena
            .alloc(SLTNode::Input {
                variable,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(lsb, msb),
            })
            .unwrap()
    }

    fn and(arena: &mut SLTNodeArena<u32>, lhs: NodeId, rhs: NodeId) -> NodeId {
        arena
            .alloc(SLTNode::Binary(lhs, BinaryOp::And, rhs))
            .unwrap()
    }

    /// `target[msb:lsb] = expr`, reading `sources` whole (eight bits).
    fn path(target: u32, lsb: usize, msb: usize, expr: NodeId, sources: &[u32]) -> LogicPath<u32> {
        LogicPath {
            transparent_copy: false,
            target: LogicPathTarget::Var(VarAtomBase::new(target, lsb, msb)),
            sources: sources
                .iter()
                .map(|&source| VarAtomBase::new(source, 0, 7))
                .collect(),
            previous_sources: HashSet::default(),
            address_sources: HashSet::default(),
            local_inputs: Vec::new(),
            order_before: HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
            expr,
        }
    }

    fn pinned(vars: &[u32]) -> HashSet<u32> {
        vars.iter().copied().collect()
    }

    fn targets(paths: &[LogicPath<u32>]) -> Vec<u32> {
        let mut targets: Vec<u32> = paths
            .iter()
            .map(|path| path.target.var().unwrap().id)
            .collect();
        targets.sort_unstable();
        targets
    }

    fn sources(path: &LogicPath<u32>) -> Vec<u32> {
        let mut sources: Vec<u32> = path.sources.iter().map(|source| source.id).collect();
        sources.sort_unstable();
        sources
    }

    #[test]
    fn inline_address_wires_without_duplicating_an_indexed_load() {
        let mut arena = SLTNodeArena::new();
        let addr = input(&mut arena, 10, 0, 7);
        let index_wire = input(&mut arena, 11, 0, 7);
        let memory = arena
            .alloc(SLTNode::Input {
                variable: 20,
                signed: false,
                access: BitAccess::new(0, 7),
                index: vec![crate::SLTIndex {
                    node: index_wire,
                    stride: 8,
                    kind: crate::SLTIndexKind::Unpacked { element_width: 8 },
                }],
            })
            .unwrap();
        let result = input(&mut arena, 12, 0, 7);
        let mut paths = vec![
            path(11, 0, 7, addr, &[10]),
            path(12, 0, 7, memory, &[11, 20]),
            path(13, 0, 7, result, &[12]),
            path(14, 0, 7, result, &[12]),
        ];
        let report = inline_nets(&mut paths, &mut arena, &pinned(&[13, 14])).unwrap();
        assert_eq!(report.removed, vec![11]);
        assert_eq!(sources(&paths[0]), vec![10, 20]);
        assert!(
            matches!(arena.get(paths[0].expr), SLTNode::Input { variable: 20, index, .. }
            if index.len() == 1 && index[0].node == addr && index[0].stride == 8)
        );
    }

    /// A chain of nets with one reader each collapses into the observed
    /// one, which then reads the chain's input.
    #[test]
    fn a_chain_of_single_reader_nets_joins_its_reader() {
        let mut arena = SLTNodeArena::new();
        let in10 = input(&mut arena, 10, 0, 7);
        let in11 = input(&mut arena, 11, 0, 7);
        let in12 = input(&mut arena, 12, 0, 7);
        let mut paths = vec![
            path(13, 0, 7, in12, &[12]),
            path(12, 0, 7, in11, &[11]),
            path(11, 0, 7, in10, &[10]),
        ];
        let report = inline_nets(&mut paths, &mut arena, &pinned(&[13])).unwrap();
        assert_eq!((report.inlined, report.duplicated, report.dead), (2, 0, 0));
        assert_eq!(targets(&paths), vec![13]);
        assert_eq!(paths[0].expr, in10);
        assert_eq!(sources(&paths[0]), vec![10]);
    }

    #[test]
    fn substituted_literal_crosses_a_mux_tree_with_several_readers() {
        let mut arena = SLTNodeArena::new();
        let zero = arena
            .alloc(SLTNode::Constant(0u8.into(), 0u8.into(), 8, false))
            .unwrap();
        let mut expr = input(&mut arena, 10, 0, 7);
        let mut driver = path(11, 0, 7, expr, &[10, 99]);
        for selector in 1..=3 {
            let cond = input(&mut arena, selector, 0, 0);
            expr = arena
                .alloc(SLTNode::Mux {
                    cond,
                    then_expr: zero,
                    else_expr: expr,
                })
                .unwrap();
            driver.sources.insert(VarAtomBase::new(selector, 0, 0));
        }
        driver.expr = expr;
        let bus = input(&mut arena, 11, 0, 7);
        let mut paths = vec![
            path(10, 0, 7, zero, &[]),
            driver,
            path(20, 0, 7, bus, &[11]),
            path(21, 0, 7, bus, &[11]),
        ];
        inline_nets(&mut paths, &mut arena, &pinned(&[20, 21])).unwrap();
        assert_eq!(targets(&paths), [20, 21]);
        for reader in paths {
            assert_eq!(reader.expr, zero);
            assert_eq!(
                sources(&reader),
                [99],
                "keep the separately declared dependency"
            );
        }
    }

    #[test]
    fn a_forwarded_zero_mask_drops_only_the_masked_input_at_the_result_width() {
        let mut arena = SLTNodeArena::new();
        let zero = arena
            .alloc(SLTNode::Constant(0u8.into(), 0u8.into(), 8, false))
            .unwrap();
        let bit_zero = arena
            .alloc(SLTNode::Constant(0u8.into(), 0u8.into(), 1, false))
            .unwrap();
        let data = input(&mut arena, 1, 0, 7);
        let mask = input(&mut arena, 10, 0, 7);
        let bit_mask = input(&mut arena, 11, 0, 0);
        let masked = and(&mut arena, data, mask);
        let narrow = and(&mut arena, data, bit_mask);
        let mut narrow_reader = path(21, 0, 7, narrow, &[1]);
        narrow_reader.sources.insert(VarAtomBase::new(11, 0, 0));
        let mut paths = vec![
            path(10, 0, 7, zero, &[]),
            path(11, 0, 0, bit_zero, &[]),
            path(20, 0, 7, masked, &[1, 10, 99]),
            narrow_reader,
        ];
        inline_nets(&mut paths, &mut arena, &pinned(&[20, 21])).unwrap();
        assert_eq!(paths[0].expr, zero);
        assert_eq!(sources(&paths[0]), [99]);
        assert_eq!(crate::get_width(paths[1].expr, &arena), 8);
    }

    #[test]
    fn folded_readers_drop_a_shared_driver_only_after_its_last_read() {
        let mut arena = SLTNodeArena::new();
        let zero = arena
            .alloc(SLTNode::Constant(0u8.into(), 0u8.into(), 8, false))
            .unwrap();
        let mut expensive = input(&mut arena, 1, 0, 7);
        for var in 2..=4 {
            let next = input(&mut arena, var, 0, 7);
            expensive = and(&mut arena, expensive, next);
        }
        // The unknown default requires a conversion on assignment: the
        // driver may be removed when unread, but cannot be substituted.
        let unknown = arena
            .alloc(SLTNode::Constant(0u8.into(), 255u8.into(), 8, false))
            .unwrap();
        let select = input(&mut arena, 5, 0, 0);
        expensive = arena
            .alloc(SLTNode::Mux {
                cond: select,
                then_expr: expensive,
                else_expr: unknown,
            })
            .unwrap();
        let data = input(&mut arena, 12, 0, 7);
        let mask = input(&mut arena, 10, 0, 7);
        let masked = and(&mut arena, data, mask);
        let original = vec![
            path(10, 0, 7, zero, &[]),
            path(12, 0, 7, expensive, &[1, 2, 3, 4]),
            path(20, 0, 7, masked, &[10, 12]),
            path(21, 0, 7, masked, &[10, 12]),
        ];
        let mut paths = original.clone();
        let report = inline_nets(&mut paths, &mut arena, &pinned(&[20, 21])).unwrap();
        assert_eq!(report.dead, 1);
        assert_eq!(targets(&paths), [20, 21]);

        let mut paths = original.clone();
        inline_nets(&mut paths, &mut arena, &pinned(&[12, 20, 21])).unwrap();
        assert_eq!(
            targets(&paths),
            [12, 20, 21],
            "keep externally observed storage"
        );

        // The expression itself is also a read, even if its path has no
        // separately declared dependency on the driver.
        let mut paths = original;
        paths.push(path(22, 0, 7, data, &[]));
        inline_nets(&mut paths, &mut arena, &pinned(&[20, 21, 22])).unwrap();
        assert_eq!(targets(&paths), [12, 20, 21, 22]);
    }

    #[test]
    fn a_folded_mux_preserves_shared_reads_and_capture_dependencies() {
        let mut arena = SLTNodeArena::new();
        let data = input(&mut arena, 1, 0, 7);
        let wire = input(&mut arena, 10, 0, 7);
        let cond = input(&mut arena, 2, 0, 0);
        let mux = arena
            .alloc(SLTNode::Mux {
                cond,
                then_expr: data,
                else_expr: wire,
            })
            .unwrap();
        let mut reader = path(20, 0, 7, mux, &[1, 10]);
        reader.sources.insert(VarAtomBase::new(2, 0, 0));
        let mut capture = reader.clone();
        capture.target = LogicPathTarget::Var(VarAtomBase::new(21, 0, 7));
        capture.comb_capture_enable_always = true;
        let mut shared = reader.clone();
        shared.target = LogicPathTarget::Var(VarAtomBase::new(22, 0, 7));
        shared.expr = and(&mut arena, mux, cond);
        let mut paths = vec![path(10, 0, 7, data, &[1]), reader, capture, shared];
        inline_nets(&mut paths, &mut arena, &pinned(&[20, 21, 22])).unwrap();
        assert_eq!(paths[0].expr, data);
        assert_eq!(sources(&paths[0]), [1]);
        assert_eq!(sources(&paths[1]), [1, 2]);
        assert_eq!(sources(&paths[2]), [1, 2]);
    }

    /// A net nothing reads goes with its driver, and so does the net only
    /// it read; a pinned net stays although nothing here reads it.
    #[test]
    fn nets_nothing_reads_are_dropped() {
        let mut arena = SLTNodeArena::new();
        let in10 = input(&mut arena, 10, 0, 7);
        let in11 = input(&mut arena, 11, 0, 7);
        let mut paths = vec![
            path(11, 0, 7, in10, &[10]),
            path(12, 0, 7, in11, &[11]),
            path(20, 0, 7, in10, &[10]),
        ];
        let report = inline_nets(&mut paths, &mut arena, &pinned(&[20])).unwrap();
        assert_eq!((report.inlined, report.dead), (0, 2));
        assert_eq!(targets(&paths), vec![20]);
    }

    /// Several readers each take a copy of a small expression; a larger
    /// one stays in state for them to load.
    #[test]
    fn several_readers_copy_only_a_small_net() {
        let mut arena = SLTNodeArena::new();
        let a = input(&mut arena, 1, 0, 7);
        let b = input(&mut arena, 2, 0, 7);
        let small = and(&mut arena, a, b);
        assert!(
            expression_cost([small].into_iter(), &arena, &crate::HashMap::default())
                <= DUPLICATED_NET_OPERATORS
        );
        let mut large = small;
        for variable in 3..9 {
            let operand = input(&mut arena, variable, 0, 7);
            large = and(&mut arena, large, operand);
        }
        let large_sources: Vec<u32> = (1..9).collect();
        assert!(
            expression_cost([large].into_iter(), &arena, &crate::HashMap::default())
                > DUPLICATED_NET_OPERATORS
        );
        let in30 = input(&mut arena, 30, 0, 7);
        let in31 = input(&mut arena, 31, 0, 7);
        let both = and(&mut arena, in30, in31);
        let mut paths = vec![
            path(30, 0, 7, small, &[1, 2]),
            path(31, 0, 7, large, &large_sources),
            path(40, 0, 7, both, &[30, 31]),
            path(41, 0, 7, both, &[30, 31]),
        ];
        let report = inline_nets(&mut paths, &mut arena, &pinned(&[40, 41])).unwrap();
        assert_eq!((report.inlined, report.duplicated, report.dead), (0, 1, 0));
        assert_eq!(targets(&paths), vec![31, 40, 41]);
        let expected = and(&mut arena, small, in31);
        for reader in paths
            .iter()
            .filter(|path| path.target.var().unwrap().id >= 40)
        {
            assert_eq!(reader.expr, expected);
            assert_eq!(sources(reader), vec![1, 2, 31]);
        }
    }

    #[test]
    fn reconverging_readers_share_a_large_expression() {
        let mut arena = SLTNodeArena::new();
        let operands: Vec<_> = (1..=5).map(|id| input(&mut arena, id, 0, 7)).collect();
        let large = operands
            .iter()
            .copied()
            .reduce(|a, b| and(&mut arena, a, b))
            .unwrap();
        assert!(
            expression_cost([large].into_iter(), &arena, &crate::HashMap::default())
                > DUPLICATED_NET_OPERATORS
        );
        let shared = input(&mut arena, 10, 0, 7);
        let left = and(&mut arena, shared, operands[0]);
        let right = and(&mut arena, shared, operands[1]);
        let l = input(&mut arena, 11, 0, 7);
        let r = input(&mut arena, 12, 0, 7);
        let output = and(&mut arena, l, r);
        let mut paths = vec![
            path(10, 0, 7, large, &[1, 2, 3, 4, 5]),
            path(11, 0, 7, left, &[10, 1]),
            path(12, 0, 7, right, &[10, 2]),
            path(20, 0, 7, output, &[11, 12]),
        ];
        let report = inline_nets(&mut paths, &mut arena, &pinned(&[20])).unwrap();
        assert_eq!((report.inlined, report.duplicated), (3, 0));
        assert_eq!(targets(&paths), vec![20]);
        let left = and(&mut arena, large, operands[0]);
        let right = and(&mut arena, large, operands[1]);
        assert_eq!(paths[0].expr, and(&mut arena, left, right));
        assert_eq!(sources(&paths[0]), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn shared_concatenations_are_assembled_once() {
        for explicit_concat in [false, true] {
            let mut arena = SLTNodeArena::new();
            let bits: Vec<_> = (0..8).map(|i| input(&mut arena, i, 0, 0)).collect();
            let mut paths = if explicit_concat {
                let parts = bits.iter().rev().map(|&bit| (bit, 1)).collect();
                let word = arena.alloc(SLTNode::Concat(parts)).unwrap();
                vec![path(10, 0, 7, word, &(0..8).collect::<Vec<_>>())]
            } else {
                bits.iter()
                    .enumerate()
                    .map(|(i, &bit)| path(10, i, i, bit, &[i as u32]))
                    .collect()
            };
            let word = input(&mut arena, 10, 0, 7);
            paths.extend([path(20, 0, 7, word, &[10]), path(21, 0, 7, word, &[10])]);
            inline_nets(&mut paths, &mut arena, &pinned(&[20, 21])).unwrap();
            for consumer in paths.iter().filter(|p| p.target.var().unwrap().id >= 20) {
                assert_eq!(consumer.expr, word);
                assert_eq!(sources(consumer), [10]);
            }
        }
    }

    #[test]
    fn narrow_reads_of_separately_driven_pieces_still_inline() {
        let mut arena = SLTNodeArena::new();
        let mut paths = Vec::new();
        for i in 0..8 {
            let bit = input(&mut arena, i as u32, 0, 0);
            paths.push(path(10, i, i, bit, &[i as u32]));
        }
        for (output, i) in [(20, 1), (21, 6)] {
            let bit = input(&mut arena, 10, i, i);
            let mut reader = path(output, 0, 0, bit, &[]);
            reader.sources.insert(VarAtomBase::new(10, i, i));
            paths.push(reader);
        }
        inline_nets(&mut paths, &mut arena, &pinned(&[20, 21])).unwrap();
        assert_eq!(targets(&paths), [20, 21]);
        assert_eq!(sources(&paths[0]), [1]);
        assert_eq!(sources(&paths[1]), [6]);
    }

    /// A read of part of a net takes that part of its expression, by the
    /// position in the range its path defines.
    #[test]
    fn a_partial_read_slices_the_expression() {
        let mut arena = SLTNodeArena::new();
        let wide = input(&mut arena, 10, 0, 7);
        // Net 11 is bits [11:4] of its variable; the reader takes [9:6].
        let part = input(&mut arena, 11, 6, 9);
        let mut paths = vec![path(11, 4, 11, wide, &[10]), path(12, 0, 3, part, &[11])];
        let report = inline_nets(&mut paths, &mut arena, &pinned(&[12])).unwrap();
        assert_eq!(report.inlined, 1);
        assert_eq!(
            arena.get(paths[0].expr),
            &SLTNode::Slice {
                expr: wide,
                access: BitAccess::new(2, 5),
            }
        );
        assert_eq!(sources(&paths[0]), vec![10]);
    }

    /// A net several paths define in ranges is read range by range: a read
    /// across them concatenates their expressions, most significant first.
    #[test]
    fn a_net_defined_in_ranges_is_read_across_them() {
        let mut arena = SLTNodeArena::new();
        let low = input(&mut arena, 10, 0, 3);
        let high = input(&mut arena, 12, 0, 3);
        let whole = input(&mut arena, 11, 0, 7);
        let mut paths = vec![
            path(11, 0, 3, low, &[10]),
            path(11, 4, 7, high, &[12]),
            path(20, 0, 7, whole, &[11]),
        ];
        let report = inline_nets(&mut paths, &mut arena, &pinned(&[20])).unwrap();
        assert_eq!((report.inlined, report.paths_after), (1, 1));
        assert_eq!(
            arena.get(paths[0].expr),
            &SLTNode::Concat(vec![(high, 4), (low, 4)])
        );
        assert_eq!(sources(&paths[0]), vec![10, 12]);

        // A read no path holds would pin the net: a fresh arena.
        let mut arena = SLTNodeArena::new();
        let low = input(&mut arena, 10, 0, 3);
        let high = input(&mut arena, 12, 0, 3);
        let middle = input(&mut arena, 11, 2, 5);
        let mut paths = vec![
            path(11, 0, 3, low, &[10]),
            path(11, 4, 7, high, &[12]),
            path(21, 0, 3, middle, &[11]),
        ];
        inline_nets(&mut paths, &mut arena, &pinned(&[21])).unwrap();
        let SLTNode::Concat(parts) = arena.get(paths[0].expr).clone() else {
            panic!("a read across two ranges concatenates them");
        };
        assert_eq!(
            parts
                .iter()
                .map(|&(part, width)| (arena.get(part).clone(), width))
                .collect::<Vec<_>>(),
            vec![
                (
                    SLTNode::Slice {
                        expr: high,
                        access: BitAccess::new(0, 1),
                    },
                    2
                ),
                (
                    SLTNode::Slice {
                        expr: low,
                        access: BitAccess::new(2, 3),
                    },
                    2
                ),
            ]
        );
    }

    /// A reader of one range of a net defined in ranges depends on what
    /// that range reads, not on what the other ranges read.
    #[test]
    fn a_read_of_one_range_takes_that_ranges_sources() {
        let mut arena = SLTNodeArena::new();
        let low = input(&mut arena, 10, 0, 3);
        let high = input(&mut arena, 12, 0, 3);
        let part = input(&mut arena, 11, 0, 3);
        let mut reader = path(20, 0, 3, part, &[]);
        reader.sources.insert(VarAtomBase::new(11, 0, 3));
        let mut paths = vec![
            path(11, 0, 3, low, &[10]),
            path(11, 4, 7, high, &[12]),
            reader,
            path(21, 0, 3, part, &[11]),
        ];
        inline_nets(&mut paths, &mut arena, &pinned(&[20, 21])).unwrap();
        assert_eq!(targets(&paths), vec![20, 21]);
        assert_eq!(paths[0].expr, low);
        assert_eq!(sources(&paths[0]), vec![10]);
        // The second reader declares the whole net as its source.
        assert_eq!(sources(&paths[1]), vec![10, 12]);
    }

    /// Nets defined in ranges that only rename the ranges of the next one
    /// (a port connection cut at its reader's bit boundaries) collapse like
    /// any other chain.
    #[test]
    fn a_chain_of_nets_defined_in_ranges_collapses() {
        let mut arena = SLTNodeArena::new();
        let mut paths = Vec::new();
        for net in 11..14 {
            for (lsb, msb) in [(0, 3), (4, 7)] {
                let source = input(&mut arena, net - 1, lsb, msb);
                let mut piece = path(net, lsb, msb, source, &[]);
                piece.sources.insert(VarAtomBase::new(net - 1, lsb, msb));
                paths.push(piece);
            }
        }
        let whole = input(&mut arena, 13, 0, 7);
        paths.push(path(20, 0, 7, whole, &[13]));
        let report = inline_nets(&mut paths, &mut arena, &pinned(&[10, 20])).unwrap();
        assert_eq!((report.inlined, report.paths_after), (3, 1));
        let low = input(&mut arena, 10, 0, 3);
        let high = input(&mut arena, 10, 4, 7);
        assert_eq!(
            arena.get(paths[0].expr),
            &SLTNode::Concat(vec![(high, 4), (low, 4)])
        );
    }

    /// What must keep its storage stays: a pinned net, a net read outside
    /// the ranges its paths define, a net with two drivers of one bit, a net
    /// a signed read interprets, and a net a path with a previous-value
    /// read loads.
    #[test]
    fn nets_that_need_their_storage_stay() {
        let mut arena = SLTNodeArena::new();
        let in10 = input(&mut arena, 10, 0, 7);
        let low = input(&mut arena, 10, 0, 3);
        // 21: pinned. 22: defined [3:0], read [7:0]. 23: two drivers of
        // bits [3:2].
        let in21 = input(&mut arena, 21, 0, 7);
        let in22 = input(&mut arena, 22, 0, 7);
        let in23 = input(&mut arena, 23, 0, 7);
        // 24: read as a signed value. 25: read by a latch-like path.
        let in24 = arena
            .alloc(SLTNode::Input {
                variable: 24,
                signed: true,
                index: Vec::new(),
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let in25 = input(&mut arena, 25, 0, 7);
        let mut latch = path(35, 0, 7, in25, &[25]);
        latch.previous_sources.insert(VarAtomBase::new(35, 0, 7));
        let mut paths = vec![
            path(21, 0, 7, in10, &[10]),
            path(22, 0, 3, low, &[10]),
            path(23, 0, 3, low, &[10]),
            path(23, 2, 5, low, &[10]),
            path(24, 0, 7, in10, &[10]),
            path(25, 0, 7, in10, &[10]),
            path(31, 0, 7, in21, &[21]),
            path(32, 0, 7, in22, &[22]),
            path(33, 0, 7, in23, &[23]),
            path(34, 0, 7, in24, &[24]),
            latch,
        ];
        let before = paths.clone();
        let report =
            inline_nets(&mut paths, &mut arena, &pinned(&[21, 31, 32, 33, 34, 35])).unwrap();
        assert_eq!((report.inlined, report.duplicated, report.dead), (0, 0, 0));
        assert_eq!(paths, before);
    }

    /// A combinational loop settles by iteration over its nets: none of
    /// them is substituted, though each has one reader.
    #[test]
    fn nets_on_a_cycle_stay() {
        let mut arena = SLTNodeArena::new();
        let in10 = input(&mut arena, 10, 0, 7);
        let in11 = input(&mut arena, 11, 0, 7);
        let in12 = input(&mut arena, 12, 0, 7);
        let a = and(&mut arena, in12, in10);
        let mut paths = vec![
            path(11, 0, 7, a, &[12, 10]),
            path(12, 0, 7, in11, &[11]),
            path(20, 0, 7, in12, &[12]),
        ];
        // 12 has two readers, 11 one; both are on the cycle 11 -> 12 -> 11.
        let before = paths.clone();
        let report = inline_nets(&mut paths, &mut arena, &pinned(&[20])).unwrap();
        assert_eq!((report.inlined, report.duplicated, report.dead), (0, 0, 0));
        assert_eq!(paths, before);
    }

    /// A net whose expression is signed stays: its readers were built over
    /// an unsigned read of it, and a comparison extends by signedness.
    #[test]
    fn a_net_of_a_signed_expression_stays() {
        let mut arena = SLTNodeArena::new();
        let signed = arena
            .alloc(SLTNode::Input {
                variable: 10,
                signed: true,
                index: Vec::new(),
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let in11 = input(&mut arena, 11, 0, 7);
        let mut paths = vec![path(11, 0, 7, signed, &[10]), path(20, 0, 7, in11, &[11])];
        let before = paths.clone();
        let report = inline_nets(&mut paths, &mut arena, &pinned(&[20])).unwrap();
        assert_eq!(report.inlined, 0);
        assert_eq!(paths, before);
    }

    /// A constant with unknown bits stays behind the store that converts
    /// them for its variable.
    #[test]
    fn a_net_of_unknown_bits_stays() {
        let mut arena = SLTNodeArena::new();
        let unknown = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0x0fu8),
                BigUint::from(0x03u8),
                8,
                false,
            ))
            .unwrap();
        let in11 = input(&mut arena, 11, 0, 7);
        let mut paths = vec![path(11, 0, 7, unknown, &[]), path(20, 0, 7, in11, &[11])];
        let before = paths.clone();
        let report = inline_nets(&mut paths, &mut arena, &pinned(&[20])).unwrap();
        assert_eq!(report.inlined, 0);
        assert_eq!(paths, before);
    }

    /// The paths that remain keep their order constraints under their new
    /// numbers.
    #[test]
    fn order_constraints_follow_the_renumbered_paths() {
        let mut arena = SLTNodeArena::new();
        let in10 = input(&mut arena, 10, 0, 7);
        let in11 = input(&mut arena, 11, 0, 7);
        let mut first = path(30, 0, 7, in10, &[10]);
        first.order_before.insert(LogicPathId(3));
        let mut paths = vec![
            path(11, 0, 7, in10, &[10]),
            first,
            path(20, 0, 7, in11, &[11]),
            path(31, 0, 7, in10, &[10]),
        ];
        let report = inline_nets(&mut paths, &mut arena, &pinned(&[20, 30, 31])).unwrap();
        assert_eq!(report.inlined, 1);
        assert_eq!(targets(&paths), vec![20, 30, 31]);
        let ordering = paths
            .iter()
            .find(|path| path.target.var().unwrap().id == 30)
            .unwrap();
        let after = ordering
            .order_before
            .iter()
            .map(|id| paths[id.0].target.var().unwrap().id)
            .collect::<Vec<_>>();
        assert_eq!(after, vec![31]);
    }
}
