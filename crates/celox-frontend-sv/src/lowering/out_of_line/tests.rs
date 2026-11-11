use super::*;

#[test]
fn sizes_sum_the_calls_that_inline() {
    // 0 calls 1 twice and 2 once; 2 is recursive, so it counts nothing.
    let own = [1, 10, 100];
    let sites = [vec![1, 1, 2], vec![], vec![2]];
    let members: HashSet<usize> = [2].into_iter().collect();
    assert_eq!(
        fully_inlined_sizes(&own, &sites, &members),
        vec![21, 10, 100]
    );
}

#[test]
fn deep_call_chains_are_sized_without_recursion() {
    // A chain of a million calls on a small stack.
    let depth = 1_000_000;
    let sizes = std::thread::Builder::new()
        .stack_size(256 << 10)
        .spawn(move || {
            let own = vec![1; depth];
            let sites: Vec<Vec<usize>> = (0..depth)
                .map(|node| {
                    if node + 1 < depth {
                        vec![node + 1]
                    } else {
                        Vec::new()
                    }
                })
                .collect();
            fully_inlined_sizes(&own, &sites, &HashSet::default())
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(sizes[0], depth as u64);
    assert_eq!(sizes[depth - 1], 1);
}
