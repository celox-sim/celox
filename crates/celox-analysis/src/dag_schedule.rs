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

type ReadyKey = (isize, Reverse<usize>, Reverse<usize>, Reverse<usize>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DagScheduleError {
    Shape,
    InvalidNode,
    DuplicateDependency,
    ValueIsNotDependency,
    Cycle,
    ArithmeticOverflow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulePressure {
    /// Maximum weighted live set at any instruction boundary.
    pub maximum: usize,
    /// Weighted live set after each node in forward schedule order.
    pub after: Vec<usize>,
}

/// Measure the exact boundary pressure induced by one complete forward order.
///
/// This uses the same value-edge semantics as the scheduler and therefore
/// serves as the placement/materialization contract rather than a separate
/// approximation.
pub fn analyze_schedule_pressure(
    order: &[usize],
    value_dependencies: &[Vec<usize>],
    value_weights: &[usize],
) -> Result<SchedulePressure, DagScheduleError> {
    let node_count = value_dependencies.len();
    if order.len() != node_count || value_weights.len() != node_count {
        return Err(DagScheduleError::Shape);
    }
    let mut seen = vec![false; node_count];
    let mut users = vec![0usize; node_count];
    for row in value_dependencies {
        validate_row(row, node_count)?;
        for &definition in row {
            users[definition] = users[definition]
                .checked_add(1)
                .ok_or(DagScheduleError::ArithmeticOverflow)?;
        }
    }
    let mut live = 0usize;
    let mut maximum = 0usize;
    let mut after = Vec::with_capacity(node_count);
    for &node in order {
        if node >= node_count {
            return Err(DagScheduleError::InvalidNode);
        }
        if std::mem::replace(&mut seen[node], true) {
            return Err(DagScheduleError::DuplicateDependency);
        }
        if users[node] != 0 {
            live = live
                .checked_add(value_weights[node])
                .ok_or(DagScheduleError::ArithmeticOverflow)?;
            maximum = maximum.max(live);
        }
        for &definition in &value_dependencies[node] {
            users[definition] = users[definition]
                .checked_sub(1)
                .ok_or(DagScheduleError::ArithmeticOverflow)?;
            if users[definition] == 0 {
                live = live
                    .checked_sub(value_weights[definition])
                    .ok_or(DagScheduleError::ArithmeticOverflow)?;
            }
        }
        after.push(live);
    }
    if seen.iter().any(|seen| !seen) {
        return Err(DagScheduleError::Shape);
    }
    Ok(SchedulePressure { maximum, after })
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
    schedule_min_live_values_in_domains(
        dependencies,
        value_dependencies,
        &vec![0; dependencies.len()],
    )
}

/// Schedule one DAG while preferring a pressure-neutral continuation in the
/// same materialization domain.
///
/// A domain switch may invalidate local rematerialization caches or require
/// explicit home reloads. Once work in a domain is selected, its ready cone is
/// closed before selecting another domain. Cross-domain register carries must
/// therefore be represented by a larger domain rather than inferred here.
pub fn schedule_min_live_values_in_domains(
    dependencies: &[Vec<usize>],
    value_dependencies: &[Vec<usize>],
    domains: &[usize],
) -> Result<Vec<usize>, DagScheduleError> {
    schedule_min_live_values_in_domains_with_weights(
        dependencies,
        value_dependencies,
        domains,
        &vec![1; dependencies.len()],
    )
}

/// Schedule one DAG using target-machine register-chunk weights.
///
/// `value_weights[definition]` is the number of allocatable register-class
/// units occupied while that definition is live. A zero-weight definition
/// still participates in hard ordering, but does not affect pressure.
pub fn schedule_min_live_values_in_domains_with_weights(
    dependencies: &[Vec<usize>],
    value_dependencies: &[Vec<usize>],
    domains: &[usize],
    value_weights: &[usize],
) -> Result<Vec<usize>, DagScheduleError> {
    let node_count = dependencies.len();
    if value_dependencies.len() != node_count
        || domains.len() != node_count
        || value_weights.len() != node_count
    {
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
    let mut ready = BTreeSet::<ReadyKey>::new();
    let domain_count = domains
        .iter()
        .copied()
        .max()
        .and_then(|domain| domain.checked_add(1))
        .unwrap_or(0);
    let mut ready_by_domain = vec![BTreeSet::<ReadyKey>::new(); domain_count];
    for (node, users) in unscheduled_users.iter().enumerate() {
        if *users == 0 {
            insert_ready(
                node,
                dependencies,
                value_dependencies,
                value_weights,
                &live,
                &entry_depth,
                &exit_depth,
                &mut deltas,
                &mut present,
                &mut ready,
                domains,
                &mut ready_by_domain,
            )?;
        }
    }

    let mut reverse = Vec::with_capacity(node_count);
    let mut active_domain: Option<usize> = None;
    while let Some(&global) = ready.first() {
        let selected_key = active_domain
            .and_then(|domain| ready_by_domain[domain].first().copied())
            .unwrap_or(global);
        let (_, _, _, Reverse(selected)) = selected_key;
        remove_ready(
            selected,
            &entry_depth,
            &exit_depth,
            &deltas,
            &mut present,
            &mut ready,
            domains,
            &mut ready_by_domain,
        );
        active_domain = Some(domains[selected]);

        if live[selected] {
            live[selected] = false;
            update_for_value(
                selected,
                weight_as_isize(value_weights[selected])?,
                &value_users,
                &entry_depth,
                &exit_depth,
                &mut deltas,
                &present,
                &mut ready,
                domains,
                &mut ready_by_domain,
            )?;
        }
        for &definition in &value_dependencies[selected] {
            if !live[definition] {
                live[definition] = true;
                update_for_value(
                    definition,
                    weight_as_isize(value_weights[definition])?
                        .checked_neg()
                        .ok_or(DagScheduleError::ArithmeticOverflow)?,
                    &value_users,
                    &entry_depth,
                    &exit_depth,
                    &mut deltas,
                    &present,
                    &mut ready,
                    domains,
                    &mut ready_by_domain,
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
                    value_weights,
                    &live,
                    &entry_depth,
                    &exit_depth,
                    &mut deltas,
                    &mut present,
                    &mut ready,
                    domains,
                    &mut ready_by_domain,
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

fn weight_as_isize(weight: usize) -> Result<isize, DagScheduleError> {
    isize::try_from(weight).map_err(|_| DagScheduleError::ArithmeticOverflow)
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
    value_weights: &[usize],
    live: &[bool],
    entry_depth: &[usize],
    exit_depth: &[usize],
    deltas: &mut [isize],
    present: &mut [bool],
    ready: &mut BTreeSet<ReadyKey>,
    domains: &[usize],
    ready_by_domain: &mut [BTreeSet<ReadyKey>],
) -> Result<(), DagScheduleError> {
    if present[node] {
        return Err(DagScheduleError::DuplicateDependency);
    }
    let missing = value_dependencies[node]
        .iter()
        .filter(|definition| !live[**definition])
        .try_fold(0usize, |total, definition| {
            total
                .checked_add(value_weights[*definition])
                .ok_or(DagScheduleError::ArithmeticOverflow)
        })?;
    let removed = if live[node] { value_weights[node] } else { 0 };
    let delta = weight_as_isize(missing)?
        .checked_sub(weight_as_isize(removed)?)
        .ok_or(DagScheduleError::ArithmeticOverflow)?;
    debug_assert!(
        value_dependencies[node]
            .iter()
            .all(|value| dependencies[node].binary_search(value).is_ok())
    );
    deltas[node] = delta;
    present[node] = true;
    let key = (
        delta,
        Reverse(exit_depth[node]),
        Reverse(entry_depth[node]),
        Reverse(node),
    );
    ready.insert(key);
    ready_by_domain[domains[node]].insert(key);
    Ok(())
}

fn remove_ready(
    node: usize,
    entry_depth: &[usize],
    exit_depth: &[usize],
    deltas: &[isize],
    present: &mut [bool],
    ready: &mut BTreeSet<ReadyKey>,
    domains: &[usize],
    ready_by_domain: &mut [BTreeSet<ReadyKey>],
) {
    debug_assert!(present[node]);
    let key = (
        deltas[node],
        Reverse(exit_depth[node]),
        Reverse(entry_depth[node]),
        Reverse(node),
    );
    ready.remove(&key);
    ready_by_domain[domains[node]].remove(&key);
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
    ready: &mut BTreeSet<ReadyKey>,
    domains: &[usize],
    ready_by_domain: &mut [BTreeSet<ReadyKey>],
) -> Result<(), DagScheduleError> {
    for candidate in value_users[value]
        .iter()
        .copied()
        .chain(std::iter::once(value))
    {
        if !present[candidate] {
            continue;
        }
        let old_key = (
            deltas[candidate],
            Reverse(exit_depth[candidate]),
            Reverse(entry_depth[candidate]),
            Reverse(candidate),
        );
        ready.remove(&old_key);
        ready_by_domain[domains[candidate]].remove(&old_key);
        deltas[candidate] = deltas[candidate]
            .checked_add(adjustment)
            .ok_or(DagScheduleError::ArithmeticOverflow)?;
        let new_key = (
            deltas[candidate],
            Reverse(exit_depth[candidate]),
            Reverse(entry_depth[candidate]),
            Reverse(candidate),
        );
        ready.insert(new_key);
        ready_by_domain[domains[candidate]].insert(new_key);
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
    fn pressure_neutral_domain_work_stays_contiguous() {
        let dependencies = vec![vec![], vec![], vec![0], vec![1], vec![2, 3]];
        let values = vec![vec![], vec![], vec![0], vec![1], vec![]];
        let domains = vec![0, 1, 0, 1, 2];
        let scheduled =
            schedule_min_live_values_in_domains(&dependencies, &values, &domains).unwrap();
        let switches = scheduled
            .windows(2)
            .filter(|pair| domains[pair[0]] != domains[pair[1]])
            .count();

        assert_eq!(maximum_live(&scheduled, &values), 1);
        assert_eq!(switches, 2);
    }

    #[test]
    fn weighted_schedule_closes_the_wide_cone_first() {
        // 0 and 1 are independent producers. 2 consumes the four-register
        // value 0, while 3 consumes the one-register value 1.
        let dependencies = vec![vec![], vec![], vec![0], vec![1], vec![2, 3]];
        let values = vec![vec![], vec![], vec![0], vec![1], vec![]];
        let domains = vec![0; dependencies.len()];
        let weights = vec![4, 1, 0, 0, 0];
        let scheduled = schedule_min_live_values_in_domains_with_weights(
            &dependencies,
            &values,
            &domains,
            &weights,
        )
        .unwrap();
        let pressure = analyze_schedule_pressure(&scheduled, &values, &weights).unwrap();

        assert_eq!(pressure.maximum, 4);
        assert!(
            scheduled.iter().position(|node| *node == 2).unwrap()
                < scheduled.iter().position(|node| *node == 1).unwrap()
        );
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
