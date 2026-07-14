use std::collections::HashMap;
use std::hash::Hash;

/// Return blocks in dominator-tree preorder.
///
/// The order keeps each block after its immediate dominator, while preserving
/// a deterministic CFG order for sibling subtrees. Blocks unreachable from
/// `entry` are appended in key order so callers still get a total ordering.
pub(crate) fn dominance_order<K, I, F>(entry: K, blocks: I, mut successors: F) -> Vec<K>
where
    K: Copy + Eq + Hash + Ord,
    I: IntoIterator<Item = K>,
    F: FnMut(K) -> Vec<K>,
{
    let mut ids = blocks.into_iter().collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();

    if ids.is_empty() {
        return ids;
    }

    let Some(entry_index) = ids.iter().position(|&id| id == entry) else {
        return ids;
    };
    let index = ids
        .iter()
        .enumerate()
        .map(|(index, &id)| (id, index))
        .collect::<HashMap<_, _>>();

    let successors = ids
        .iter()
        .map(|&id| {
            let mut successors = successors(id)
                .into_iter()
                .filter_map(|successor| index.get(&successor).copied())
                .collect::<Vec<_>>();
            successors.dedup();
            successors
        })
        .collect::<Vec<_>>();

    // Discover the reachable CFG and record a reverse-postorder. The
    // discovery order is retained for deterministic dominator-tree siblings.
    let mut discovered = vec![false; ids.len()];
    let mut discovery_order = Vec::new();
    let mut postorder = Vec::new();
    discovered[entry_index] = true;
    discovery_order.push(entry_index);
    let mut stack = vec![(entry_index, false)];
    while let Some((block, expanded)) = stack.pop() {
        if expanded {
            postorder.push(block);
            continue;
        }

        if block != entry_index {
            discovery_order.push(block);
        }
        stack.push((block, true));
        for &successor in successors[block].iter().rev() {
            if discovered[successor] {
                continue;
            }
            discovered[successor] = true;
            stack.push((successor, false));
        }
    }
    postorder.reverse();

    let mut predecessors = vec![Vec::new(); ids.len()];
    for (block, successors) in successors.iter().enumerate() {
        if !discovered[block] {
            continue;
        }
        for &successor in successors {
            if discovered[successor] {
                predecessors[successor].push(block);
            }
        }
    }

    // Cooper-Harvey-Kennedy immediate dominators. RPO numbering makes the
    // intersect operation compact and also handles backedges correctly.
    let mut rpo_number = vec![usize::MAX; ids.len()];
    for (number, &block) in postorder.iter().enumerate() {
        rpo_number[block] = number;
    }
    let mut idom = vec![None; ids.len()];
    idom[entry_index] = Some(entry_index);

    let intersect = |mut left: usize, mut right: usize, idom: &[Option<usize>]| {
        while left != right {
            while rpo_number[left] > rpo_number[right] {
                left = idom[left].expect("processed dominator must have an idom");
            }
            while rpo_number[right] > rpo_number[left] {
                right = idom[right].expect("processed dominator must have an idom");
            }
        }
        left
    };

    loop {
        let mut changed = false;
        for &block in postorder.iter().skip(1) {
            let mut processed = predecessors[block]
                .iter()
                .copied()
                .filter(|&predecessor| idom[predecessor].is_some());
            let Some(first) = processed.next() else {
                continue;
            };
            let next = processed.fold(first, |current, predecessor| {
                intersect(current, predecessor, &idom)
            });
            if idom[block] != Some(next) {
                idom[block] = Some(next);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut children = vec![Vec::new(); ids.len()];
    for &block in &discovery_order {
        if block == entry_index {
            continue;
        }
        if let Some(parent) = idom[block] {
            children[parent].push(block);
        }
    }

    let mut order = Vec::with_capacity(ids.len());
    let mut tree_stack = vec![entry_index];
    while let Some(block) = tree_stack.pop() {
        order.push(block);
        tree_stack.extend(children[block].iter().rev().copied());
    }

    // A malformed or disconnected graph should still produce a total order.
    order.extend(
        ids.iter()
            .enumerate()
            .filter(|(index, _)| !discovered[*index])
            .map(|(index, _)| index),
    );
    order.into_iter().map(|index| ids[index]).collect()
}

#[cfg(test)]
mod tests {
    use super::dominance_order;

    #[test]
    fn emits_dominator_tree_preorder_instead_of_numeric_order() {
        let order = dominance_order(0, 0..6, |block| match block {
            0 => vec![4, 1],
            1 => vec![2],
            2 => vec![3],
            4 => vec![5],
            _ => Vec::new(),
        });

        assert_eq!(order, vec![0, 4, 5, 1, 2, 3]);
    }

    #[test]
    fn appends_unreachable_blocks_deterministically() {
        let order = dominance_order(0, [4, 0, 3, 2, 1], |block| match block {
            0 => vec![2],
            _ => Vec::new(),
        });

        assert_eq!(order, vec![0, 2, 1, 3, 4]);
    }

    #[test]
    fn keeps_a_join_in_dominator_tree_preorder() {
        let order = dominance_order(0, 0..5, |block| match block {
            0 => vec![3, 1],
            1 => vec![2],
            2 => vec![4],
            3 => vec![2],
            _ => Vec::new(),
        });

        assert_eq!(order, vec![0, 3, 2, 4, 1]);
    }
}
