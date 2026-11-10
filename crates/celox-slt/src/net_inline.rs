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

use std::fmt::{Debug, Display};
use std::hash::Hash;

use celox_design::{BitAccess, VarAtomBase};

use crate::{
    HashMap, HashSet, LogicPath, LogicPathId, LogicPathTarget, NodeId, SLTNode, SLTNodeArena,
    SLTNodeFactsError,
};

/// The most operators a net evaluated in several places may have, counting
/// those of the nets copied into it: each place evaluates its own copy, in
/// place of a load of the stored net. Reads, constants and bit selections
/// are no operators: a copy of a net that only renames or selects bits
/// costs its readers nothing.
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
/// paths define of one variable, lowest first, and the operators of their
/// expressions together.
struct Candidate {
    pieces: Vec<Piece>,
    operators: usize,
}

impl Candidate {
    /// Whether the pieces define every bit of `access`.
    fn covers(&self, access: BitAccess) -> bool {
        let mut next = access.lsb;
        for piece in &self.pieces {
            if piece.target.msb < next {
                continue;
            }
            if piece.target.lsb > next {
                return false;
            }
            if piece.target.msb >= access.msb {
                return true;
            }
            next = piece.target.msb + 1;
        }
        false
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
/// operators over plain reads. Folds bind names of their own, a capture is
/// evaluated at one position, and an indexed read addresses its variable in
/// state.
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
            SLTNode::Input { index, .. } if !index.is_empty() => return false,
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

/// The operators in the expressions of `roots`, each shared node once:
/// everything but reads, constants and bit selections.
fn operators_of<A: Clone + Eq + Hash>(
    roots: impl Iterator<Item = NodeId>,
    arena: &SLTNodeArena<A>,
) -> usize {
    let mut pending: Vec<NodeId> = roots.collect();
    let mut seen = HashSet::default();
    let mut count = 0;
    while let Some(node) = pending.pop() {
        if !seen.insert(node) {
            continue;
        }
        let node = arena.get(node);
        if !matches!(
            node,
            SLTNode::Input { .. } | SLTNode::Constant(..) | SLTNode::Slice { .. }
        ) {
            count += 1;
        }
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
                    ..
                } = node
                {
                    reads[index].insert(*variable);
                    if first {
                        read_ranges.push((*variable, *access));
                        if *signed {
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
    // The nets a substitution or a removal may take out of the program:
    // variables whose every driver is a plain path defining a range of its
    // own.
    let signedness = crate::SLTToSIRLowerer::new(false);
    let mut eligible: HashMap<A, Candidate> = HashMap::default();
    'vars: for (var, list) in &drivers {
        if pinned.contains(var) || cyclic.contains(var) {
            continue;
        }
        let mut pieces = Vec::with_capacity(list.len());
        for &index in list {
            let path = &paths[index];
            let LogicPathTarget::Var(target) = &path.target else {
                continue 'vars;
            };
            if !plain[index]
                || ordered.contains(&index)
                || !path.comb_capture_enable_sites.is_empty()
                || path.comb_capture_enable_always
                || !path.order_before.is_empty()
                || crate::get_width(path.expr, arena) != target.access.msb - target.access.lsb + 1
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
        let operators = operators_of(pieces.iter().map(|piece| piece.expr), arena);
        eligible.insert(*var, Candidate { pieces, operators });
    }
    // Every read of such a net lies in the ranges its paths define.
    for (variable, access) in &read_ranges {
        if eligible
            .get(variable)
            .is_some_and(|candidate| !candidate.covers(*access))
        {
            eligible.remove(variable);
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
    report.dead = dead.len();
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
        let cost = candidates[var].operators
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
    let mut evaluations: HashMap<A, usize> = HashMap::default();
    let mut inlined: HashSet<A> = HashSet::default();
    for var in order.iter().rev() {
        let mut reading_paths = 0;
        let mut reading_nets: HashMap<A, usize> = HashMap::default();
        for reader in &readers[var] {
            if dead_paths.contains(reader) {
                continue;
            }
            match piece_of.get(reader) {
                Some(net) => *reading_nets.entry(*net).or_default() += 1,
                None => reading_paths += 1,
            }
        }
        let count = reading_paths
            + reading_nets
                .iter()
                .map(|(net, pieces)| {
                    if inlined.contains(net) {
                        evaluations[net]
                    } else {
                        *pieces
                    }
                })
                .sum::<usize>();
        evaluations.insert(*var, count);
        if count <= 1 || copy_cost[var] <= DUPLICATED_NET_OPERATORS {
            inlined.insert(*var);
        }
    }
    report.inlined = inlined.iter().filter(|var| evaluations[*var] <= 1).count();
    report.duplicated = inlined.len() - report.inlined;
    report.removed = inlined.iter().chain(&dead).copied().collect();
    report.removed.sort_unstable();
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
        if !inlined.contains(var) {
            continue;
        }
        let mut ranges = Vec::with_capacity(candidates[var].pieces.len());
        let mut sources = Vec::with_capacity(candidates[var].pieces.len());
        for piece in &candidates[var].pieces {
            ranges.push((
                piece.target,
                substitute(piece.expr, &resolved, arena, &mut cache)?,
            ));
            sources.push((
                piece.target,
                remaining_sources(&paths[piece.path], &resolved_sources),
            ));
        }
        resolved.insert(*var, ranges);
        resolved_sources.insert(*var, sources);
    }
    let removed: HashSet<usize> = inlined
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
        path.expr = substitute(path.expr, &resolved, arena, &mut cache)?;
        path.sources = remaining_sources(path, &resolved_sources);
    }
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
            SLTNode::Input { .. }
            | SLTNode::Constant(..)
            | SLTNode::ForFold { .. }
            | SLTNode::ForFoldGroup { .. } => node,
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
                    arena.alloc(SLTNode::Mux {
                        cond: new_cond,
                        then_expr: new_then,
                        else_expr: new_else,
                    })?
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

    use super::{DUPLICATED_NET_OPERATORS, inline_nets, operators_of};
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
        assert!(operators_of([small].into_iter(), &arena) <= DUPLICATED_NET_OPERATORS);
        let mut large = small;
        for variable in 3..9 {
            let operand = input(&mut arena, variable, 0, 7);
            large = and(&mut arena, large, operand);
        }
        let large_sources: Vec<u32> = (1..9).collect();
        assert!(operators_of([large].into_iter(), &arena) > DUPLICATED_NET_OPERATORS);
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
