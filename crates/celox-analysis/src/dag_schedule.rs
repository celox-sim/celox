//! Bottom-up scheduling of a dependency DAG with explicit value edges and
//! shared materialization tokens.
//!
//! Hard dependencies constrain order.  Value dependencies are a subset of
//! those edges and additionally describe liveness: scheduling a user backward
//! makes the producer value live, while scheduling the producer kills it.  A
//! ready node with the smallest live-value delta is selected first.  This is
//! the conventional register-pressure list-scheduling model without attaching
//! target-specific widths to source IR values.  A materialization token is
//! used by an arbitrary set of nodes rather than being defined by one DAG
//! node.  It models cached source expressions whose producer is whichever use
//! gets scheduled first.
//!
//! For `N` nodes, `E` hard edges, `V` value edges, and `I` node/token
//! incidences, scheduling costs `O((N + E + V + I) log N)` time and
//! `O(N + E + V + I)` space.  A value or token changes the contribution to
//! ready-node priorities at most twice, so each incidence is revisited only a
//! constant number of times.

use std::cmp::Reverse;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DagScheduleError {
    Shape,
    InvalidNode,
    InvalidToken,
    DuplicateDependency,
    DuplicateToken,
    ValueIsNotDependency,
    UsersAreNotReverseDependencies,
    Cycle,
    ArithmeticOverflow,
}

trait NodeRows {
    fn len(&self) -> usize;
    fn row(&self, node: usize) -> &[usize];
}

struct LocalNodeRows<'a>(&'a [Vec<usize>]);

impl NodeRows for LocalNodeRows<'_> {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn row(&self, node: usize) -> &[usize] {
        &self.0[node]
    }
}

/// Borrow rows indexed by a larger graph through a local-to-external node map.
///
/// The row values themselves are not remapped. This is suitable for stable
/// global IDs such as materialization tokens.
#[derive(Clone, Copy)]
pub struct MappedNodeRows<'a> {
    rows_by_external_node: &'a [Vec<usize>],
    external_by_local: &'a [usize],
}

impl<'a> MappedNodeRows<'a> {
    pub fn new(
        rows_by_external_node: &'a [Vec<usize>],
        external_by_local: &'a [usize],
    ) -> Result<Self, DagScheduleError> {
        if external_by_local
            .iter()
            .any(|external| *external >= rows_by_external_node.len())
        {
            return Err(DagScheduleError::InvalidNode);
        }
        Ok(Self {
            rows_by_external_node,
            external_by_local,
        })
    }
}

impl NodeRows for MappedNodeRows<'_> {
    fn len(&self) -> usize {
        self.external_by_local.len()
    }

    fn row(&self, node: usize) -> &[usize] {
        &self.rows_by_external_node[self.external_by_local[node]]
    }
}

trait GraphRows {
    type Iter<'a>: Iterator<Item = usize>
    where
        Self: 'a;

    fn len(&self) -> usize;
    fn nodes(&self, row: usize) -> Self::Iter<'_>;
    fn contains(&self, row: usize, node: usize) -> bool;
}

struct LocalGraphRows<'a>(&'a [Vec<usize>]);

impl<'a> LocalGraphRows<'a> {
    fn new(rows: &'a [Vec<usize>]) -> Result<Self, DagScheduleError> {
        for row in rows {
            validate_row(row, rows.len())?;
        }
        Ok(Self(rows))
    }
}

impl GraphRows for LocalGraphRows<'_> {
    type Iter<'a>
        = std::iter::Copied<std::slice::Iter<'a, usize>>
    where
        Self: 'a;

    fn len(&self) -> usize {
        self.0.len()
    }

    fn nodes(&self, row: usize) -> Self::Iter<'_> {
        self.0[row].iter().copied()
    }

    fn contains(&self, row: usize, node: usize) -> bool {
        self.0[row].binary_search(&node).is_ok()
    }
}

struct MappedGraphRowsIter<'a> {
    external_nodes: std::slice::Iter<'a, usize>,
    local_by_external: &'a [usize],
    row: usize,
}

impl Iterator for MappedGraphRowsIter<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        self.external_nodes.find_map(|external| {
            let local = self.local_by_external[*external];
            (local != usize::MAX && local != self.row).then_some(local)
        })
    }
}

/// Borrow adjacency rows from a larger graph and expose only nodes mapped into
/// the current local DAG.
#[derive(Clone, Copy)]
pub struct MappedGraphRows<'a> {
    rows_by_external_node: &'a [Vec<usize>],
    external_by_local: &'a [usize],
    local_by_external: &'a [usize],
}

impl<'a> MappedGraphRows<'a> {
    pub fn new(
        rows_by_external_node: &'a [Vec<usize>],
        external_by_local: &'a [usize],
        local_by_external: &'a [usize],
    ) -> Result<Self, DagScheduleError> {
        for (local, &external) in external_by_local.iter().enumerate() {
            if external >= rows_by_external_node.len()
                || external >= local_by_external.len()
                || local_by_external[external] != local
            {
                return Err(DagScheduleError::InvalidNode);
            }
            validate_mapped_user_row(
                &rows_by_external_node[external],
                external_by_local,
                local_by_external,
            )?;
        }
        Ok(Self {
            rows_by_external_node,
            external_by_local,
            local_by_external,
        })
    }
}

impl GraphRows for MappedGraphRows<'_> {
    type Iter<'a>
        = MappedGraphRowsIter<'a>
    where
        Self: 'a;

    fn len(&self) -> usize {
        self.external_by_local.len()
    }

    fn nodes(&self, row: usize) -> Self::Iter<'_> {
        MappedGraphRowsIter {
            external_nodes: self.rows_by_external_node[self.external_by_local[row]].iter(),
            local_by_external: self.local_by_external,
            row,
        }
    }

    fn contains(&self, row: usize, node: usize) -> bool {
        row != node
            && self.rows_by_external_node[self.external_by_local[row]]
                .binary_search(&self.external_by_local[node])
                .is_ok()
    }
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
    let tokens = vec![Vec::new(); dependencies.len()];
    schedule_min_live_values_and_tokens(dependencies, value_dependencies, &tokens, &[])
}

/// Return a deterministic forward order while also minimizing shared
/// materialization-token pressure.
///
/// `tokens_by_node[node]` contains the sorted, duplicate-free token IDs used
/// by that node. `token_weights[token]` is the number of pressure units kept
/// live between the first and last scheduled users of the token.
pub fn schedule_min_live_values_and_tokens(
    dependencies: &[Vec<usize>],
    value_dependencies: &[Vec<usize>],
    tokens_by_node: &[Vec<usize>],
    token_weights: &[usize],
) -> Result<Vec<usize>, DagScheduleError> {
    let dependencies = LocalGraphRows::new(dependencies)?;
    let value_dependencies = LocalGraphRows::new(value_dependencies)?;
    let tokens_by_node = LocalNodeRows(tokens_by_node);
    let token_users = validate_inputs(
        &dependencies,
        &value_dependencies,
        &tokens_by_node,
        token_weights,
    )?;
    let node_count = dependencies.len();
    let mut successors = vec![Vec::<usize>::new(); node_count];
    let mut value_users = vec![Vec::<usize>::new(); node_count];
    for user in 0..node_count {
        for definition in dependencies.nodes(user) {
            successors[definition].push(user);
        }
        for definition in value_dependencies.nodes(user) {
            value_users[definition].push(user);
        }
    }
    let successors = LocalGraphRows::new(&successors)?;
    let value_users = LocalGraphRows::new(&value_users)?;
    schedule_with_users(
        &dependencies,
        &value_dependencies,
        &tokens_by_node,
        token_weights,
        &token_users,
        &successors,
        &value_users,
    )
}

/// Schedule a local DAG while borrowing dependency, user, and token rows from
/// a larger graph.
///
/// This avoids reconstructing either direction of the hard- and value-edge
/// adjacency. The mapped user rows must be the exact reverse of the mapped
/// dependency rows.
pub fn schedule_min_live_values_and_tokens_with_mapped_rows(
    dependencies: MappedGraphRows<'_>,
    value_dependencies: MappedGraphRows<'_>,
    tokens_by_node: MappedNodeRows<'_>,
    token_weights: &[usize],
    successors: MappedGraphRows<'_>,
    value_users: MappedGraphRows<'_>,
) -> Result<Vec<usize>, DagScheduleError> {
    let token_users = validate_inputs(
        &dependencies,
        &value_dependencies,
        &tokens_by_node,
        token_weights,
    )?;
    validate_reverse_users(&successors, &dependencies)?;
    validate_reverse_users(&value_users, &value_dependencies)?;
    schedule_with_users(
        &dependencies,
        &value_dependencies,
        &tokens_by_node,
        token_weights,
        &token_users,
        &successors,
        &value_users,
    )
}

fn validate_inputs<D: GraphRows, V: GraphRows, T: NodeRows>(
    dependencies: &D,
    value_dependencies: &V,
    tokens_by_node: &T,
    token_weights: &[usize],
) -> Result<Vec<Vec<usize>>, DagScheduleError> {
    let node_count = dependencies.len();
    if value_dependencies.len() != node_count || tokens_by_node.len() != node_count {
        return Err(DagScheduleError::Shape);
    }
    let mut token_users = vec![Vec::<usize>::new(); token_weights.len()];
    for user in 0..node_count {
        validate_token_row(tokens_by_node.row(user), token_weights.len())?;
        if value_dependencies
            .nodes(user)
            .any(|value| !dependencies.contains(user, value))
        {
            return Err(DagScheduleError::ValueIsNotDependency);
        }
        for &token in tokens_by_node.row(user) {
            token_users[token].push(user);
        }
    }
    Ok(token_users)
}

fn validate_reverse_users<U: GraphRows, D: GraphRows>(
    users: &U,
    dependencies: &D,
) -> Result<(), DagScheduleError> {
    if users.len() != dependencies.len() {
        return Err(DagScheduleError::Shape);
    }
    let expected_edges = (0..dependencies.len()).try_fold(0usize, |total, row| {
        total
            .checked_add(dependencies.nodes(row).count())
            .ok_or(DagScheduleError::ArithmeticOverflow)
    })?;
    let mut actual_edges = 0usize;
    for definition in 0..users.len() {
        for user in users.nodes(definition) {
            if !dependencies.contains(user, definition) {
                return Err(DagScheduleError::UsersAreNotReverseDependencies);
            }
            actual_edges = actual_edges
                .checked_add(1)
                .ok_or(DagScheduleError::ArithmeticOverflow)?;
        }
    }
    if actual_edges != expected_edges {
        return Err(DagScheduleError::UsersAreNotReverseDependencies);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn schedule_with_users<D: GraphRows, V: GraphRows, T: NodeRows, S: GraphRows, U: GraphRows>(
    dependencies: &D,
    value_dependencies: &V,
    tokens_by_node: &T,
    token_weights: &[usize],
    token_users: &[Vec<usize>],
    successors: &S,
    value_users: &U,
) -> Result<Vec<usize>, DagScheduleError> {
    let node_count = dependencies.len();
    if successors.len() != node_count || value_users.len() != node_count {
        return Err(DagScheduleError::Shape);
    }
    let indegree = (0..node_count)
        .map(|node| dependencies.nodes(node).count())
        .collect();

    let (topological, entry_depth) = topological_order(successors, indegree)?;
    let mut exit_depth = vec![1usize; node_count];
    for &node in topological.iter().rev() {
        let successor_depth = exit_depth[node]
            .checked_add(1)
            .ok_or(DagScheduleError::ArithmeticOverflow)?;
        for dependency in dependencies.nodes(node) {
            exit_depth[dependency] = exit_depth[dependency].max(successor_depth);
        }
    }

    let mut unscheduled_users = (0..node_count)
        .map(|node| successors.nodes(node).count())
        .collect::<Vec<_>>();
    let mut live = vec![false; node_count];
    let mut remaining_token_users = token_users.iter().map(Vec::len).collect::<Vec<_>>();
    let mut live_tokens = vec![false; token_weights.len()];
    let mut deltas = vec![0isize; node_count];
    let mut present = vec![false; node_count];
    let mut ready = BTreeSet::<(isize, Reverse<usize>, Reverse<usize>, Reverse<usize>)>::new();
    for (node, users) in unscheduled_users.iter().enumerate() {
        if *users == 0 {
            insert_ready(
                node,
                dependencies,
                value_dependencies,
                tokens_by_node,
                token_weights,
                &live,
                &live_tokens,
                &remaining_token_users,
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
                value_users,
                &entry_depth,
                &exit_depth,
                &mut deltas,
                &present,
                &mut ready,
            )?;
        }
        for definition in value_dependencies.nodes(selected) {
            if !live[definition] {
                live[definition] = true;
                update_for_value(
                    definition,
                    -1,
                    value_users,
                    &entry_depth,
                    &exit_depth,
                    &mut deltas,
                    &present,
                    &mut ready,
                )?;
            }
        }
        for &token in tokens_by_node.row(selected) {
            let before = remaining_token_users[token];
            remaining_token_users[token] = before
                .checked_sub(1)
                .ok_or(DagScheduleError::ArithmeticOverflow)?;
            let weight = isize::try_from(token_weights[token])
                .map_err(|_| DagScheduleError::ArithmeticOverflow)?;
            if !live_tokens[token] && before > 1 {
                live_tokens[token] = true;
                let adjustment = if before == 2 {
                    weight
                        .checked_mul(-2)
                        .ok_or(DagScheduleError::ArithmeticOverflow)?
                } else {
                    -weight
                };
                update_for_token(
                    token,
                    adjustment,
                    token_users,
                    &entry_depth,
                    &exit_depth,
                    &mut deltas,
                    &present,
                    &mut ready,
                )?;
            } else if live_tokens[token] && before == 2 {
                update_for_token(
                    token,
                    -weight,
                    token_users,
                    &entry_depth,
                    &exit_depth,
                    &mut deltas,
                    &present,
                    &mut ready,
                )?;
            } else if live_tokens[token] && before == 1 {
                live_tokens[token] = false;
            }
        }
        for dependency in dependencies.nodes(selected) {
            unscheduled_users[dependency] = unscheduled_users[dependency]
                .checked_sub(1)
                .ok_or(DagScheduleError::ArithmeticOverflow)?;
            if unscheduled_users[dependency] == 0 {
                insert_ready(
                    dependency,
                    dependencies,
                    value_dependencies,
                    tokens_by_node,
                    token_weights,
                    &live,
                    &live_tokens,
                    &remaining_token_users,
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

fn validate_mapped_user_row(
    row: &[usize],
    external_by_local: &[usize],
    local_by_external: &[usize],
) -> Result<(), DagScheduleError> {
    let mut previous = None;
    for &external in row {
        if external >= local_by_external.len() {
            return Err(DagScheduleError::InvalidNode);
        }
        if previous.is_some_and(|previous| previous >= external) {
            return Err(DagScheduleError::DuplicateDependency);
        }
        let local = local_by_external[external];
        if local != usize::MAX
            && (local >= external_by_local.len() || external_by_local[local] != external)
        {
            return Err(DagScheduleError::InvalidNode);
        }
        previous = Some(external);
    }
    Ok(())
}

fn validate_token_row(row: &[usize], token_count: usize) -> Result<(), DagScheduleError> {
    let mut previous = None;
    for &token in row {
        if token >= token_count {
            return Err(DagScheduleError::InvalidToken);
        }
        if previous.is_some_and(|previous| previous >= token) {
            return Err(DagScheduleError::DuplicateToken);
        }
        previous = Some(token);
    }
    Ok(())
}

fn topological_order<U: GraphRows>(
    successors: &U,
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
        for successor in successors.nodes(node) {
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
fn insert_ready<D: GraphRows, V: GraphRows, T: NodeRows>(
    node: usize,
    dependencies: &D,
    value_dependencies: &V,
    tokens_by_node: &T,
    token_weights: &[usize],
    live: &[bool],
    live_tokens: &[bool],
    remaining_token_users: &[usize],
    entry_depth: &[usize],
    exit_depth: &[usize],
    deltas: &mut [isize],
    present: &mut [bool],
    ready: &mut BTreeSet<(isize, Reverse<usize>, Reverse<usize>, Reverse<usize>)>,
) -> Result<(), DagScheduleError> {
    if present[node] {
        return Err(DagScheduleError::DuplicateDependency);
    }
    let missing = value_dependencies
        .nodes(node)
        .filter(|definition| !live[*definition])
        .count();
    let removed = usize::from(live[node]);
    let value_delta = isize::try_from(missing)
        .ok()
        .and_then(|missing| {
            isize::try_from(removed)
                .ok()
                .and_then(|removed| missing.checked_sub(removed))
        })
        .ok_or(DagScheduleError::ArithmeticOverflow)?;
    let token_delta = tokens_by_node
        .row(node)
        .iter()
        .try_fold(0isize, |delta, &token| {
            let contribution = if !live_tokens[token] && remaining_token_users[token] > 1 {
                isize::try_from(token_weights[token])
                    .map_err(|_| DagScheduleError::ArithmeticOverflow)?
            } else if live_tokens[token] && remaining_token_users[token] == 1 {
                -isize::try_from(token_weights[token])
                    .map_err(|_| DagScheduleError::ArithmeticOverflow)?
            } else {
                0
            };
            delta
                .checked_add(contribution)
                .ok_or(DagScheduleError::ArithmeticOverflow)
        })?;
    let delta = value_delta
        .checked_add(token_delta)
        .ok_or(DagScheduleError::ArithmeticOverflow)?;
    debug_assert!(
        value_dependencies
            .nodes(node)
            .all(|value| dependencies.contains(node, value))
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

#[allow(clippy::too_many_arguments)]
fn update_for_token(
    token: usize,
    adjustment: isize,
    token_users: &[Vec<usize>],
    entry_depth: &[usize],
    exit_depth: &[usize],
    deltas: &mut [isize],
    present: &[bool],
    ready: &mut BTreeSet<(isize, Reverse<usize>, Reverse<usize>, Reverse<usize>)>,
) -> Result<(), DagScheduleError> {
    for &candidate in &token_users[token] {
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
fn update_for_value<U: GraphRows>(
    value: usize,
    adjustment: isize,
    value_users: &U,
    entry_depth: &[usize],
    exit_depth: &[usize],
    deltas: &mut [isize],
    present: &[bool],
    ready: &mut BTreeSet<(isize, Reverse<usize>, Reverse<usize>, Reverse<usize>)>,
) -> Result<(), DagScheduleError> {
    for candidate in value_users.nodes(value).chain(std::iter::once(value)) {
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

    fn maximum_live_tokens(order: &[usize], tokens_by_node: &[Vec<usize>]) -> usize {
        let token_count = tokens_by_node
            .iter()
            .flatten()
            .copied()
            .max()
            .map_or(0, |maximum| maximum + 1);
        let mut remaining = vec![0usize; token_count];
        for tokens in tokens_by_node {
            for &token in tokens {
                remaining[token] += 1;
            }
        }
        let mut live = vec![false; token_count];
        let mut live_count = 0usize;
        let mut maximum = 0usize;
        for &node in order {
            for &token in &tokens_by_node[node] {
                if !live[token] && remaining[token] > 1 {
                    live[token] = true;
                    live_count += 1;
                    maximum = maximum.max(live_count);
                }
                remaining[token] -= 1;
                if live[token] && remaining[token] == 0 {
                    live[token] = false;
                    live_count -= 1;
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
    fn mapped_rows_match_owned_reverse_relations() {
        let dependencies = vec![vec![], vec![0], vec![0, 1]];
        let values = dependencies.clone();
        let tokens = vec![vec![0], vec![0, 1], vec![1]];
        let expected =
            schedule_min_live_values_and_tokens(&dependencies, &values, &tokens, &[2, 1]).unwrap();

        let external_by_local = vec![4, 1, 3];
        let local_by_external = vec![usize::MAX, 1, usize::MAX, 2, 0];
        let mut external_users = vec![Vec::new(); 5];
        external_users[1] = vec![3];
        external_users[4] = vec![1, 3];
        let mut external_dependencies = vec![Vec::new(); 5];
        external_dependencies[1] = vec![4];
        external_dependencies[3] = vec![1, 4];
        let mut external_tokens = vec![Vec::new(); 5];
        external_tokens[4] = vec![0];
        external_tokens[1] = vec![0, 1];
        external_tokens[3] = vec![1];

        let actual = schedule_min_live_values_and_tokens_with_mapped_rows(
            MappedGraphRows::new(
                &external_dependencies,
                &external_by_local,
                &local_by_external,
            )
            .unwrap(),
            MappedGraphRows::new(
                &external_dependencies,
                &external_by_local,
                &local_by_external,
            )
            .unwrap(),
            MappedNodeRows::new(&external_tokens, &external_by_local).unwrap(),
            &[2, 1],
            MappedGraphRows::new(&external_users, &external_by_local, &local_by_external).unwrap(),
            MappedGraphRows::new(&external_users, &external_by_local, &local_by_external).unwrap(),
        )
        .unwrap();

        assert_eq!(actual, expected);
    }

    #[test]
    fn mapped_rows_reject_an_incomplete_reverse_relation() {
        let dependencies = vec![vec![], vec![0]];
        let external_by_local = vec![0, 1];
        let local_by_external = vec![0, 1];
        let missing_users = vec![Vec::new(), Vec::new()];
        let empty_tokens = vec![Vec::new(), Vec::new()];
        let dependencies =
            MappedGraphRows::new(&dependencies, &external_by_local, &local_by_external).unwrap();
        let users =
            MappedGraphRows::new(&missing_users, &external_by_local, &local_by_external).unwrap();

        assert_eq!(
            schedule_min_live_values_and_tokens_with_mapped_rows(
                dependencies,
                dependencies,
                MappedNodeRows::new(&empty_tokens, &external_by_local).unwrap(),
                &[],
                users,
                users,
            ),
            Err(DagScheduleError::UsersAreNotReverseDependencies)
        );
    }

    #[test]
    fn mapped_rows_reject_an_out_of_range_reverse_map_entry() {
        let users = vec![vec![1], vec![]];

        assert!(matches!(
            MappedGraphRows::new(&users, &[0], &[0, 1]),
            Err(DagScheduleError::InvalidNode)
        ));
    }

    #[test]
    fn mapped_rows_reject_an_alias_in_the_reverse_map() {
        let users = vec![vec![1], vec![]];

        assert!(matches!(
            MappedGraphRows::new(&users, &[0], &[0, 0]),
            Err(DagScheduleError::InvalidNode)
        ));
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

    #[test]
    fn shared_materializations_stay_contiguous() {
        let dependencies = vec![vec![], vec![], vec![], vec![]];
        let values = dependencies.clone();
        let tokens = vec![vec![0], vec![1], vec![0], vec![1]];
        let order =
            schedule_min_live_values_and_tokens(&dependencies, &values, &tokens, &[1, 1]).unwrap();

        assert!(maximum_live_tokens(&order, &tokens) < maximum_live_tokens(&[0, 1, 2, 3], &tokens));
        for users in [[0, 2], [1, 3]] {
            let positions = users.map(|node| order.iter().position(|item| *item == node).unwrap());
            assert_eq!(positions[1], positions[0] + 1);
        }
    }

    #[test]
    fn materialization_pressure_never_breaks_hard_dependencies() {
        let dependencies = vec![vec![], vec![0], vec![], vec![1, 2]];
        let values = vec![vec![], vec![], vec![], vec![]];
        let tokens = vec![vec![0], vec![1], vec![0], vec![1]];
        let order =
            schedule_min_live_values_and_tokens(&dependencies, &values, &tokens, &[8, 1]).unwrap();
        let position = |node| order.iter().position(|item| *item == node).unwrap();

        assert!(position(0) < position(1));
        assert!(position(1) < position(3));
        assert!(position(2) < position(3));
    }

    #[test]
    fn sparse_materialization_incidence_scales_to_wide_ready_sets() {
        const NODES: usize = 4096;
        let graph = vec![Vec::new(); NODES];
        let tokens = (0..NODES).map(|node| vec![node / 2]).collect::<Vec<_>>();
        let weights = vec![1; NODES / 2];
        let order = schedule_min_live_values_and_tokens(&graph, &graph, &tokens, &weights).unwrap();

        assert_eq!(order.len(), NODES);
        assert_eq!(maximum_live_tokens(&order, &tokens), 1);
    }
}
