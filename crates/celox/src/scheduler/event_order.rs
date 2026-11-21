//! Group work with the same trigger mask while respecting every conflicting
//! access to design storage. Registers are local to an execution unit.
use super::*;

pub(super) fn order(
    activations: Vec<Activation>,
    reads: &[HashSet<AbsoluteAddr>],
    writes: &[HashSet<AbsoluteAddr>],
) -> Vec<(Activation, Vec<usize>)> {
    let count = activations.len();
    let mut successors = vec![Vec::new(); count];
    let mut indegree = vec![0; count];
    let mut last_writer = HashMap::default();
    let mut readers: HashMap<_, Vec<usize>> = HashMap::default();
    for index in 0..count {
        let mut dependencies = HashSet::default();
        for address in reads[index].union(&writes[index]) {
            dependencies.extend(last_writer.get(address).copied());
        }
        for address in &writes[index] {
            last_writer.insert(*address, index);
            if let Some(prior_reads) = readers.remove(address) {
                dependencies.extend(prior_reads);
            }
        }
        for address in reads[index].difference(&writes[index]) {
            readers.entry(*address).or_default().push(index);
        }
        indegree[index] = dependencies.len();
        for previous in dependencies {
            successors[previous].push(index);
        }
    }

    // A ready unit in the current domain extends its straight-line body.
    // Choose domain ids in original first-use order for deterministic ties.
    let mut ids = HashMap::default();
    let domains: Vec<_> = activations
        .iter()
        .map(|activation| {
            let next = ids.len();
            *ids.entry(activation).or_insert(next)
        })
        .collect();
    let mut ready: BTreeSet<_> = (0..count)
        .filter(|&i| indegree[i] == 0)
        .map(|i| (domains[i], i))
        .collect();
    let mut current = 0;
    let mut groups: Vec<(Activation, Vec<usize>)> = Vec::new();
    while let Some(&(domain, unit)) = ready
        .range((current, 0)..=(current, usize::MAX))
        .next()
        .or_else(|| ready.first())
    {
        ready.remove(&(domain, unit));
        if groups.is_empty() || current != domain {
            groups.push((activations[unit].clone(), Vec::new()));
        }
        groups.last_mut().unwrap().1.push(unit);
        current = domain;
        for &next in &successors[unit] {
            indegree[next] -= 1;
            if indegree[next] == 0 {
                ready.insert((domains[next], next));
            }
        }
    }
    debug_assert_eq!(
        groups.iter().map(|(_, units)| units.len()).sum::<usize>(),
        count
    );
    groups
}
