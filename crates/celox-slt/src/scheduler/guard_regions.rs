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
            && crate::condition_groups::profitable(
                arena,
                condition,
                paths.iter().map(|&i| input[i].expr),
            )
        {
            result.push(ScheduledWork::GuardedComb { condition, paths });
        } else {
            result.extend(paths.into_iter().map(ScheduledWork::CombPath));
        }
    }
    result
}
