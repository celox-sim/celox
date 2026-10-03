//! Deterministic, source-independent DAG partitioning.
//!
//! List scheduling balances estimated work while charging cross-lane edges for
//! publication/synchronization. Epochs make each returned group convex: an edge
//! either stays inside a lane/group or goes to a strictly later epoch. No graph
//! contraction, reachability matrix, source names, or filesystem is required.
use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};

/// Keep externally observable actions in their original serial position, with
/// O(V) added edges rather than connecting every before/after pair.
pub(super) fn fence_effects(users: &mut [Vec<usize>], effects: &[bool]) {
    assert_eq!(users.len(), effects.len());
    let mut previous: Option<usize> = None;
    for (i, &effect) in effects.iter().enumerate() {
        if let Some(prev) = previous {
            users[prev].push(i);
        }
        if effect {
            for row in &mut users[previous.map_or(0, |p| p + 1)..i] {
                row.push(i);
            }
            previous = Some(i);
        }
    }
}

pub(super) fn partition(users: &[Vec<usize>], costs: &[u64], lanes: usize) -> Vec<Vec<usize>> {
    let n = users.len();
    assert_eq!(n, costs.len());
    if n == 0 {
        return Vec::new();
    }
    let lanes = lanes.clamp(1, n.min(64));
    let mut preds = vec![Vec::new(); n];
    for (a, row) in users.iter().enumerate() {
        for &b in row {
            // Input IDs are the existing topological schedule (SCCs are atomic).
            assert!(a < b, "partition input must be a topological DAG");
            preds[b].push(a);
        }
    }
    let mut critical = costs.to_vec();
    for a in (0..n).rev() {
        critical[a] = costs[a]
            .max(1)
            .saturating_add(users[a].iter().map(|&b| critical[b]).max().unwrap_or(0));
    }
    let total = costs.iter().copied().fold(0u64, u64::saturating_add);
    // Static heuristic, not a runtime speed prediction. Runtime auto compares
    // the resulting code against a separately optimized serial implementation.
    let communication = (total / lanes as u64 / 64).max(1);
    let mut ready = BinaryHeap::new();
    let mut degree = preds.iter().map(Vec::len).collect::<Vec<_>>();
    for a in 0..n {
        if degree[a] == 0 {
            ready.push((critical[a], Reverse(a)));
        }
    }
    let mut available = vec![0u64; lanes];
    let mut lane_epoch = vec![0usize; lanes];
    let mut owner = vec![0; n];
    let mut end = vec![0u64; n];
    let mut epoch = vec![0usize; n];
    let mut groups = BTreeMap::<(usize, usize), Vec<usize>>::new();
    let mut last = vec![0u64; lanes];
    while let Some((_, Reverse(a))) = ready.pop() {
        // Largest predecessor finish times by lane let all lane choices be
        // evaluated in O(indegree + lanes), not O(indegree * lanes).
        last.fill(0);
        for &p in &preds[a] {
            last[owner[p]] = last[owner[p]].max(end[p]);
        }
        let mut first = (0, usize::MAX);
        let mut second = 0;
        for (lane, &time) in last.iter().enumerate() {
            if time >= first.0 {
                second = first.0;
                first = (time, lane);
            } else {
                second = second.max(time);
            }
        }
        let (start, lane) = (0..lanes)
            .map(|lane| {
                let remote = if first.1 == lane { second } else { first.0 };
                let remote = if remote == 0 {
                    0
                } else {
                    remote.saturating_add(communication)
                };
                (available[lane].max(last[lane]).max(remote), lane)
            })
            .min()
            .unwrap();
        owner[a] = lane;
        end[a] = start.saturating_add(costs[a].max(1));
        available[lane] = end[a];
        epoch[a] = preds[a].iter().fold(lane_epoch[lane], |e, &p| {
            e.max(epoch[p] + usize::from(owner[p] != lane))
        });
        lane_epoch[lane] = epoch[a];
        groups.entry((epoch[a], lane)).or_default().push(a);
        for &b in &users[a] {
            degree[b] -= 1;
            if degree[b] == 0 {
                ready.push((critical[b], Reverse(b)));
            }
        }
    }
    let groups = groups.into_values().collect::<Vec<_>>();
    // Validate the contract at the lowering boundary, independently of the
    // cost/epoch heuristic. Reuse the owner array: no reachability matrix or
    // additional per-edge storage is needed. The flattened order must be a
    // complete topological permutation, including order within each group.
    owner.fill(usize::MAX);
    let mut position = 0;
    for &node in groups.iter().flatten() {
        assert_eq!(owner[node], usize::MAX, "duplicate partition node");
        owner[node] = position;
        position += 1;
    }
    assert_eq!(position, n, "missing partition node");
    for (a, row) in users.iter().enumerate() {
        for &b in row {
            assert!(
                owner[a] < owner[b],
                "partition reverses dependency {a}->{b}"
            );
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verify(users: &[Vec<usize>], groups: &[Vec<usize>]) {
        let mut positions = vec![None; users.len()];
        for (g, group) in groups.iter().enumerate() {
            for (i, &node) in group.iter().enumerate() {
                assert!(positions[node].replace((g, i)).is_none());
            }
        }
        assert!(positions.iter().all(Option::is_some));
        for (a, row) in users.iter().enumerate() {
            for &b in row {
                assert!(positions[a] < positions[b], "edge {a}->{b}");
            }
        }
    }

    #[test]
    fn effects_fence_all_earlier_and_later_work() {
        let mut users = vec![vec![]; 7];
        fence_effects(&mut users, &[false, false, true, false, false, true, false]);
        assert_eq!(
            users,
            vec![
                vec![2],
                vec![2],
                vec![3, 4, 5],
                vec![5],
                vec![5],
                vec![6],
                vec![]
            ]
        );
        verify(&users, &partition(&users, &[10; 7], 4));
    }

    #[test]
    fn discovers_independent_chains_without_instance_names() {
        let mut users = vec![vec![]; 800];
        for (i, row) in users.iter_mut().enumerate().take(792) {
            row.push(i + 8);
        }
        let groups = partition(&users, &vec![10; 800], 8);
        verify(&users, &groups);
        assert_eq!(groups.len(), 8);
        assert!(groups.iter().all(|g| g.len() == 100));
    }

    #[test]
    fn fork_join_and_cross_lane_edges_remain_ordered() {
        let users = vec![vec![1, 2], vec![3], vec![3, 4], vec![5], vec![5], vec![]];
        for lanes in 1..=16 {
            let groups = partition(&users, &[1, 20, 20, 4, 8, 1], lanes);
            verify(&users, &groups);
            assert_eq!(groups, partition(&users, &[1, 20, 20, 4, 8, 1], lanes));
        }
    }

    #[test]
    fn large_irregular_dag_preserves_dependencies() {
        let n = 100_000;
        let mut users = vec![vec![]; n];
        for (a, row) in users.iter_mut().enumerate() {
            for step in [1, 17, 257] {
                if a + step < n && (a + step) % 7 != 0 {
                    row.push(a + step);
                }
            }
        }
        let costs = (0..n).map(|i| (i % 19 + 1) as u64).collect::<Vec<_>>();
        verify(&users, &partition(&users, &costs, 32));
    }

    #[test]
    fn serial_chain_does_not_create_spurious_parallelism() {
        let users = vec![vec![1], vec![2], vec![3], vec![]];
        assert_eq!(partition(&users, &[5; 4], 16), vec![vec![0, 1, 2, 3]]);
        assert!(partition(&[], &[], 8).is_empty());
    }
}
