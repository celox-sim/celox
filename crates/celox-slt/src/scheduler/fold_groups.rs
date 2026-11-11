//! Fold groups: discovery, read facts and joint lowering.

use super::*;

pub(super) fn projected_for_fold_group<Addr: Clone + Eq + Hash>(
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

pub(super) fn fold_group_projection_access<Addr: Clone + Eq + Hash>(
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

pub(super) fn packed_fold_group_state_accesses<Addr: Clone + Eq + Hash>(
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

pub(super) fn push_scheduler_node_children<Addr: Clone + Eq + Hash>(
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
pub(super) enum NormalizedIndexExpr<Addr: Clone + Eq + Hash> {
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
    pub(super) fn contains_loop_value(&self) -> bool {
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

    pub(super) fn operation_cost(&self) -> u128 {
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

pub(super) fn normalize_index_expr<Addr: Clone + Eq + Hash + Copy>(
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
pub(super) struct ExactIndexedLoadKey<Addr: Clone + Eq + Hash> {
    pub(super) base: Addr,
    pub(super) access: BitAccess,
    pub(super) index: Vec<(NormalizedIndexExpr<Addr>, usize)>,
}

impl<Addr: Clone + Eq + Hash> ExactIndexedLoadKey<Addr> {
    pub(super) fn saved_runtime_cost(&self) -> u128 {
        let width = self.access.msb - self.access.lsb + 1;
        let chunks = width.div_ceil(64) as u128;
        let address_cost = self.index.iter().fold(1u128, |cost, (expr, _)| {
            cost.saturating_add(2).saturating_add(expr.operation_cost())
        });
        6u128.saturating_mul(chunks).saturating_add(address_cost)
    }
}

#[derive(Clone)]
pub(super) struct FoldGroupReadFacts<Addr: Clone + Eq + Hash> {
    pub(super) loop_var: Addr,
    pub(super) state_targets: Vec<VarAtomBase<Addr>>,
    pub(super) guard_reads: Vec<SchedulerInputRead<Addr>>,
    pub(super) initial_reads: Vec<SchedulerInputRead<Addr>>,
    pub(super) update_reads: Vec<SchedulerInputRead<Addr>>,
    pub(super) indexed_loads: HashSet<ExactIndexedLoadKey<Addr>>,
    pub(super) carried_chunks: u128,
}

#[derive(Clone)]
pub(super) struct ExactFoldGroup<Addr: Clone + Eq + Hash> {
    pub(super) root: NodeId,
    pub(super) facts: FoldGroupReadFacts<Addr>,
}

pub(super) struct FoldGroupScheduleInfo<Addr: Clone + Eq + Hash> {
    pub(super) projection_paths: Vec<usize>,
    pub(super) read_facts: Option<FoldGroupReadFacts<Addr>>,
    pub(super) exact_and_exclusive: bool,
}

pub(super) struct FoldGroupScheduleIndex<Addr: Clone + Eq + Hash> {
    pub(super) groups: BTreeMap<NodeId, FoldGroupScheduleInfo<Addr>>,
    pub(super) direct_group_by_path: Vec<Option<NodeId>>,
}

pub(super) fn collect_reachable_scheduled_groups<Addr: Clone + Eq + Hash>(
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

pub(super) fn exact_fold_group_paths<Addr: Clone + Eq + Hash + Copy>(
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

pub(super) fn build_fold_group_schedule_index<Addr: Clone + Eq + Ord + Hash + Copy>(
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

pub(super) fn discover_exact_fold_groups<Addr: Clone + Eq + Ord + Hash + Copy>(
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

pub(super) fn same_fold_group_domain<Addr: Clone + Eq + Hash>(
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
pub(super) struct SchedulerInputRead<Addr> {
    pub(super) id: Addr,
    pub(super) access: BitAccess,
    pub(super) indexed: bool,
}

pub(super) fn collect_scheduler_plain_reads<Addr: Clone + Eq + Hash + Copy>(
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

pub(super) fn collect_fold_group_read_facts<Addr: Clone + Eq + Hash + Copy>(
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

pub(super) fn scheduler_read_overlaps_targets<Addr: Clone + Eq + Hash>(
    read: &SchedulerInputRead<Addr>,
    targets: &[VarAtomBase<Addr>],
) -> bool {
    targets.iter().any(|target| {
        target.id == read.id && (read.indexed || target.access.overlaps(&read.access))
    })
}

pub(super) fn fold_groups_are_pairwise_independent<Addr: Clone + Eq + Hash + Copy>(
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

pub(super) fn fold_groups_share_exact_load<Addr: Clone + Eq + Hash>(
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
pub(super) struct WeightedFoldFamily {
    pub(super) members: Vec<usize>,
    pub(super) benefit: u128,
    pub(super) pressure: u128,
}

impl WeightedFoldFamily {
    pub(super) fn is_positive(&self) -> bool {
        self.members.len() >= 2 && self.benefit > self.pressure
    }

    pub(super) fn cmp_net(&self, other: &Self) -> std::cmp::Ordering {
        self.benefit
            .saturating_add(other.pressure)
            .cmp(&other.benefit.saturating_add(self.pressure))
    }
}

pub(super) fn weighted_fold_family<Addr: Clone + Eq + Hash>(
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

pub(super) fn family_signature<Addr: Clone + Eq + Hash>(
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

pub(super) fn better_weighted_family<Addr: Clone + Eq + Hash>(
    candidate: &WeightedFoldFamily,
    current: &WeightedFoldFamily,
    groups: &[ExactFoldGroup<Addr>],
) -> bool {
    candidate.cmp_net(current).is_gt()
        || candidate.cmp_net(current).is_eq()
            && family_signature(candidate, groups) < family_signature(current, groups)
}

pub(super) fn grow_weighted_family<Addr: Clone + Eq + Hash>(
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

pub(super) fn best_weighted_fold_family<Addr: Clone + Eq + Hash>(
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

pub(super) fn jointly_lower_fold_group_families<
    Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display,
>(
    indices: &[usize],
    schedule_index: &FoldGroupScheduleIndex<Addr>,
    lowerer: &mut crate::SLTToSIRLowerer,
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
pub(super) struct PreparedFoldProjection {
    pub(super) packed_result: RegisterId,
    pub(super) access: BitAccess,
}

/// Materialize each shared grouped fold once, but leave its projections
/// deferred.  A target Store may invalidate the ordinary lowering cache, so
/// the packed result is retained explicitly until every projection has been
/// consumed.  Each narrow projection is created immediately before its Store
/// instead of keeping all projected registers live simultaneously.
pub(super) fn prepare_atomic_fold_group_results<
    Addr: Clone + Eq + Ord + Hash + Debug + Copy + Display,
>(
    indices: &[usize],
    input: &[LogicPath<Addr>],
    fold_group_schedule_index: &FoldGroupScheduleIndex<Addr>,
    lowerer: &mut crate::SLTToSIRLowerer,
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
