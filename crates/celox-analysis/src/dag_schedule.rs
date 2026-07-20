//! Bottom-up scheduling of a dependency DAG with explicit value edges.
//!
//! Hard dependencies constrain order.  Value dependencies are a subset of
//! those edges and additionally describe liveness: scheduling a user backward
//! makes the producer value live, while scheduling the producer kills it.  A
//! ready node with the smallest live-value delta is selected first.  This is
//! the conventional register-pressure list-scheduling model without attaching
//! target-specific widths to source IR values.
//!
//! For `N` nodes, `E` hard edges, and `V` value edges, scheduling costs
//! `O((N + E + V) log N)` time and `O(N + E + V)` space.  A value changes
//! liveness at most twice, so priority maintenance visits each incident value
//! edge only a constant number of times.

use std::cmp::Reverse;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DagScheduleError {
    Shape,
    InvalidNode,
    DuplicateDependency,
    ValueIsNotDependency,
    Cycle,
    ArithmeticOverflow,
}

/// Return a deterministic forward order for one DAG.
///
/// `dependencies[user]` contains every predecessor which must be scheduled
/// before `user`. `value_dependencies[user]` contains the subset whose result
/// remains live until this user. Rows must be sorted and duplicate-free.
pub fn schedule_min_live_values(
    dependencies: &[Vec<usize>],
    value_dependencies: &[Vec<usize>],
) -> Result<Vec<usize>, DagScheduleError> {
    let node_count = dependencies.len();
    if value_dependencies.len() != node_count {
        return Err(DagScheduleError::Shape);
    }

    let mut successors = vec![Vec::<usize>::new(); node_count];
    let mut indegree = vec![0usize; node_count];
    let mut value_users = vec![Vec::<usize>::new(); node_count];
    for user in 0..node_count {
        validate_row(&dependencies[user], node_count)?;
        validate_row(&value_dependencies[user], node_count)?;
        if value_dependencies[user]
            .iter()
            .any(|value| dependencies[user].binary_search(value).is_err())
        {
            return Err(DagScheduleError::ValueIsNotDependency);
        }
        indegree[user] = dependencies[user].len();
        for &definition in &dependencies[user] {
            successors[definition].push(user);
        }
        for &definition in &value_dependencies[user] {
            value_users[definition].push(user);
        }
    }

    let (topological, entry_depth) = topological_order(&successors, indegree)?;
    let mut exit_depth = vec![1usize; node_count];
    for &node in topological.iter().rev() {
        let successor_depth = exit_depth[node]
            .checked_add(1)
            .ok_or(DagScheduleError::ArithmeticOverflow)?;
        for &dependency in &dependencies[node] {
            exit_depth[dependency] = exit_depth[dependency].max(successor_depth);
        }
    }

    let mut unscheduled_users = successors.iter().map(Vec::len).collect::<Vec<_>>();
    let mut live = vec![false; node_count];
    let mut deltas = vec![0isize; node_count];
    let mut present = vec![false; node_count];
    let mut ready = BTreeSet::<(isize, Reverse<usize>, Reverse<usize>, Reverse<usize>)>::new();
    for (node, users) in unscheduled_users.iter().enumerate() {
        if *users == 0 {
            insert_ready(
                node,
                dependencies,
                value_dependencies,
                &live,
                &entry_depth,
                &exit_depth,
                &mut deltas,
                &mut present,
                &mut ready,
            )?;
        }
    }

    let mut reverse = Vec::with_capacity(node_count);
    while let Some(&(_, _, _, Reverse(selected))) = ready.first() {
        remove_ready(
            selected,
            &entry_depth,
            &exit_depth,
            &deltas,
            &mut present,
            &mut ready,
        );

        if live[selected] {
            live[selected] = false;
            update_for_value(
                selected,
                1,
                &value_users,
                &entry_depth,
                &exit_depth,
                &mut deltas,
                &present,
                &mut ready,
            )?;
        }
        for &definition in &value_dependencies[selected] {
            if !live[definition] {
                live[definition] = true;
                update_for_value(
                    definition,
                    -1,
                    &value_users,
                    &entry_depth,
                    &exit_depth,
                    &mut deltas,
                    &present,
                    &mut ready,
                )?;
            }
        }
        for &dependency in &dependencies[selected] {
            unscheduled_users[dependency] = unscheduled_users[dependency]
                .checked_sub(1)
                .ok_or(DagScheduleError::ArithmeticOverflow)?;
            if unscheduled_users[dependency] == 0 {
                insert_ready(
                    dependency,
                    dependencies,
                    value_dependencies,
                    &live,
                    &entry_depth,
                    &exit_depth,
                    &mut deltas,
                    &mut present,
                    &mut ready,
                )?;
            }
        }
        reverse.push(selected);
    }

    if reverse.len() != node_count {
        return Err(DagScheduleError::Cycle);
    }
    reverse.reverse();
    Ok(reverse)
}

fn validate_row(row: &[usize], node_count: usize) -> Result<(), DagScheduleError> {
    let mut previous = None;
    for &node in row {
        if node >= node_count {
            return Err(DagScheduleError::InvalidNode);
        }
        if previous.is_some_and(|previous| previous >= node) {
            return Err(DagScheduleError::DuplicateDependency);
        }
        previous = Some(node);
    }
    Ok(())
}

fn topological_order(
    successors: &[Vec<usize>],
    mut indegree: Vec<usize>,
) -> Result<(Vec<usize>, Vec<usize>), DagScheduleError> {
    let mut ready = BTreeSet::new();
    let mut entry_depth = vec![1usize; successors.len()];
    for (node, degree) in indegree.iter().enumerate() {
        if *degree == 0 {
            ready.insert(node);
        }
    }
    let mut order = Vec::with_capacity(successors.len());
    while let Some(node) = ready.pop_first() {
        order.push(node);
        let next_depth = entry_depth[node]
            .checked_add(1)
            .ok_or(DagScheduleError::ArithmeticOverflow)?;
        for &successor in &successors[node] {
            entry_depth[successor] = entry_depth[successor].max(next_depth);
            indegree[successor] = indegree[successor]
                .checked_sub(1)
                .ok_or(DagScheduleError::ArithmeticOverflow)?;
            if indegree[successor] == 0 {
                ready.insert(successor);
            }
        }
    }
    if order.len() != successors.len() {
        return Err(DagScheduleError::Cycle);
    }
    Ok((order, entry_depth))
}

#[allow(clippy::too_many_arguments)]
fn insert_ready(
    node: usize,
    dependencies: &[Vec<usize>],
    value_dependencies: &[Vec<usize>],
    live: &[bool],
    entry_depth: &[usize],
    exit_depth: &[usize],
    deltas: &mut [isize],
    present: &mut [bool],
    ready: &mut BTreeSet<(isize, Reverse<usize>, Reverse<usize>, Reverse<usize>)>,
) -> Result<(), DagScheduleError> {
    if present[node] {
        return Err(DagScheduleError::DuplicateDependency);
    }
    let missing = value_dependencies[node]
        .iter()
        .filter(|definition| !live[**definition])
        .count();
    let removed = usize::from(live[node]);
    let delta = isize::try_from(missing)
        .ok()
        .and_then(|missing| {
            isize::try_from(removed)
                .ok()
                .and_then(|removed| missing.checked_sub(removed))
        })
        .ok_or(DagScheduleError::ArithmeticOverflow)?;
    debug_assert!(
        value_dependencies[node]
            .iter()
            .all(|value| dependencies[node].binary_search(value).is_ok())
    );
    deltas[node] = delta;
    present[node] = true;
    ready.insert((
        delta,
        Reverse(exit_depth[node]),
        Reverse(entry_depth[node]),
        Reverse(node),
    ));
    Ok(())
}

fn remove_ready(
    node: usize,
    entry_depth: &[usize],
    exit_depth: &[usize],
    deltas: &[isize],
    present: &mut [bool],
    ready: &mut BTreeSet<(isize, Reverse<usize>, Reverse<usize>, Reverse<usize>)>,
) {
    debug_assert!(present[node]);
    ready.remove(&(
        deltas[node],
        Reverse(exit_depth[node]),
        Reverse(entry_depth[node]),
        Reverse(node),
    ));
    present[node] = false;
}

#[allow(clippy::too_many_arguments)]
fn update_for_value(
    value: usize,
    adjustment: isize,
    value_users: &[Vec<usize>],
    entry_depth: &[usize],
    exit_depth: &[usize],
    deltas: &mut [isize],
    present: &[bool],
    ready: &mut BTreeSet<(isize, Reverse<usize>, Reverse<usize>, Reverse<usize>)>,
) -> Result<(), DagScheduleError> {
    for candidate in value_users[value]
        .iter()
        .copied()
        .chain(std::iter::once(value))
    {
        if !present[candidate] {
            continue;
        }
        ready.remove(&(
            deltas[candidate],
            Reverse(exit_depth[candidate]),
            Reverse(entry_depth[candidate]),
            Reverse(candidate),
        ));
        deltas[candidate] = deltas[candidate]
            .checked_add(adjustment)
            .ok_or(DagScheduleError::ArithmeticOverflow)?;
        ready.insert((
            deltas[candidate],
            Reverse(exit_depth[candidate]),
            Reverse(entry_depth[candidate]),
            Reverse(candidate),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(successors: &[Vec<usize>]) -> Vec<Vec<usize>> {
        let mut result = vec![Vec::new(); successors.len()];
        for (definition, users) in successors.iter().enumerate() {
            for &user in users {
                result[user].push(definition);
            }
        }
        for row in &mut result {
            row.sort_unstable();
            row.dedup();
        }
        result
    }

    fn maximum_live(order: &[usize], value_dependencies: &[Vec<usize>]) -> usize {
        let mut users = vec![0usize; value_dependencies.len()];
        for row in value_dependencies {
            for &definition in row {
                users[definition] += 1;
            }
        }
        let mut live = 0usize;
        let mut maximum = 0usize;
        for &node in order {
            if users[node] != 0 {
                live += 1;
                maximum = maximum.max(live);
            }
            for &definition in &value_dependencies[node] {
                users[definition] -= 1;
                if users[definition] == 0 {
                    live -= 1;
                }
            }
        }
        maximum
    }

    #[test]
    fn independent_single_use_chains_stay_contiguous() {
        let successors = vec![vec![2], vec![3], vec![4], vec![5], vec![], vec![]];
        let dependencies = rows(&successors);
        let scheduled = schedule_min_live_values(&dependencies, &dependencies).unwrap();

        assert!(
            maximum_live(&scheduled, &dependencies)
                < maximum_live(&[0, 1, 2, 3, 4, 5], &dependencies)
        );
        for chain in [[0, 2, 4], [1, 3, 5]] {
            let positions =
                chain.map(|node| scheduled.iter().position(|item| *item == node).unwrap());
            assert_eq!(positions[1], positions[0] + 1);
            assert_eq!(positions[2], positions[1] + 1);
        }
    }

    #[test]
    fn order_only_edges_do_not_create_live_values() {
        let dependencies = vec![vec![], vec![0], vec![1]];
        let values = vec![vec![], vec![], vec![]];
        assert_eq!(
            schedule_min_live_values(&dependencies, &values).unwrap(),
            vec![0, 1, 2]
        );
        assert_eq!(maximum_live(&[0, 1, 2], &values), 0);
    }

    #[test]
    fn rejects_a_value_edge_without_a_hard_dependency() {
        assert_eq!(
            schedule_min_live_values(&[vec![], vec![]], &[vec![], vec![0]]),
            Err(DagScheduleError::ValueIsNotDependency)
        );
    }

    #[test]
    fn wide_independent_ready_set_schedules_without_pairwise_state() {
        const NODES: usize = 4096;
        let graph = vec![Vec::new(); NODES];
        let order = schedule_min_live_values(&graph, &graph).unwrap();
        assert_eq!(order, (0..NODES).collect::<Vec<_>>());
    }
}
