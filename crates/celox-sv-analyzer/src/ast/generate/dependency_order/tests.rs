use super::*;

fn graph(names: &[String], dependencies: &[HashSet<String>]) -> DependencyOrder {
    DependencyOrder::new(
        names
            .iter()
            .zip(dependencies)
            .map(|(name, deps)| (name.as_str(), deps)),
    )
}

fn legacy_order(names: &[String], dependencies: &[HashSet<String>]) -> Vec<usize> {
    let mut remaining: Vec<_> = (0..names.len()).collect();
    let mut result = Vec::new();
    while !remaining.is_empty() {
        let unresolved: HashSet<_> = remaining
            .iter()
            .map(|&index| names[index].clone())
            .collect();
        let Some(position) = remaining
            .iter()
            .position(|&index| dependencies[index].is_disjoint(&unresolved))
        else {
            break;
        };
        result.push(remaining.remove(position));
    }
    result
}

#[test]
fn matches_legacy_priority_for_every_three_node_graph() {
    let names: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
    // Includes self-edges, cycles, disconnected components, and ready nodes
    // that unlock a higher-priority node. Any signal/parameter split has the
    // same total priority: all signals precede all parameters.
    for mask in 0..(1u16 << 9) {
        let dependencies: Vec<_> = (0..3)
            .map(|index| {
                (0..3)
                    .filter(|&dependency| mask & (1 << (3 * index + dependency)) != 0)
                    .map(|dependency| names[dependency].clone())
                    .collect()
            })
            .collect();
        let expected = legacy_order(&names, &dependencies);
        let mut order = graph(&names, &dependencies);
        let mut actual = Vec::new();
        while let Some(index) = order.pop_ready() {
            actual.push(index);
            order.complete(index);
        }
        assert_eq!(actual, expected, "edge mask {mask}");
        assert_eq!(order.is_complete(), actual.len() == names.len());
    }
}

#[test]
fn newly_ready_signal_precedes_already_ready_parameter() {
    let names: Vec<String> = vec![
        "signal0".into(),
        "signal1".into(),
        "parameter0".into(),
        "parameter1".into(),
    ];
    let dependencies = vec![
        HashSet::from_iter(["parameter0".into()]),
        HashSet::default(),
        HashSet::default(),
        HashSet::default(),
    ];
    let mut order = graph(&names, &dependencies);
    for expected in [1, 2, 0, 3] {
        assert_eq!(order.pop_ready(), Some(expected));
        order.complete(expected);
    }
    assert!(order.is_complete());
}

#[test]
fn external_names_do_not_block_and_cycles_keep_the_acyclic_frontier() {
    let names: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
    let dependencies = vec![
        HashSet::from_iter(["b".into()]),
        HashSet::from_iter(["a".into()]),
        HashSet::from_iter(["outer".into()]),
    ];
    let mut order = graph(&names, &dependencies);
    assert_eq!(order.pop_ready(), Some(2));
    order.complete(2);
    assert_eq!(order.pop_ready(), None);
    assert!(!order.is_complete());
}

#[test]
fn reverse_chain_releases_each_edge_once() {
    let names: Vec<_> = (0..4096).map(|index| format!("n{index}")).collect();
    let dependencies: Vec<_> = (0..names.len())
        .map(|index| {
            names
                .get(index + 1)
                .map(|next| HashSet::from_iter([next.clone()]))
                .unwrap_or_default()
        })
        .collect();
    let mut order = graph(&names, &dependencies);
    for expected in (0..names.len()).rev() {
        assert_eq!(order.pop_ready(), Some(expected));
        order.complete(expected);
    }
    assert!(order.is_complete());
    assert_eq!(order.released_edges, names.len() - 1);
}
