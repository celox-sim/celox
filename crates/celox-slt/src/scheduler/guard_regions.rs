//! Guard regions of scheduled work.

use super::*;

pub(super) fn scheduled_root_mux<A: Clone + Eq + Hash>(
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

pub(super) fn collect_pure_scheduled_nodes<A: Clone + Eq + Hash>(
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

pub(super) fn scheduled_node_cost<A: Clone + Eq + Hash>(
    node: NodeId,
    arena: &SLTNodeArena<A>,
) -> u128 {
    match arena.get(node) {
        SLTNode::Input { .. } | SLTNode::Constant(..) => 0,
        _ => crate::get_width(node, arena).div_ceil(64).max(1) as u128,
    }
}

pub(super) fn scheduled_condition_probability<A: Clone + Eq + Hash>(
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

pub(super) fn scheduled_guard_region_is_profitable<A: Clone + Eq + Hash>(
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

pub(super) fn condition_reads_target<A: Clone + Eq + Hash>(
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

pub(super) fn form_scheduled_guard_regions<A: Clone + Eq + Hash>(
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
