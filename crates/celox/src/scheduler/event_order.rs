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

/// Group registers staged by the same blocks, including banks written under
/// a common enable. Registers written only in entry blocks keep their own
/// identities. The minimum address names a bank deterministically. The key
/// groups consumers; each register still publishes its own reader mask.
pub(super) fn read_banks(
    events: &[Event],
    reads: &[HashSet<AbsoluteAddr>],
    registers: &HashSet<AbsoluteAddr>,
    home: impl Fn(AbsoluteAddr) -> AbsoluteAddr,
) -> Vec<Vec<AbsoluteAddr>> {
    let mut sites: HashMap<_, Vec<_>> = HashMap::default();
    for (unit, body) in events
        .iter()
        .flat_map(|event| event.eval_only.iter().flatten())
        .enumerate()
    {
        for block in body.blocks.values() {
            for write in block.instructions.iter().flat_map(written_storage) {
                if write.region == STABLE_REGION {
                    continue;
                }
                let target = home(write.absolute_addr());
                if registers.contains(&target) {
                    let site = (unit, block.id, block.id != body.entry_block_id);
                    let written = sites.entry(target).or_default();
                    if written.last() != Some(&site) {
                        written.push(site);
                    }
                }
            }
        }
    }
    let mut classes = HashMap::default();
    for (addr, sites) in &sites {
        if sites.iter().any(|site| site.2) {
            classes
                .entry(sites.as_slice())
                .and_modify(|min: &mut AbsoluteAddr| *min = (*min).min(*addr))
                .or_insert(*addr);
        }
    }
    reads
        .iter()
        .map(|inputs| {
            let mut bank: Vec<_> = inputs
                .intersection(registers)
                .map(|addr| {
                    sites
                        .get(addr)
                        .and_then(|s| classes.get(s.as_slice()))
                        .copied()
                        .unwrap_or(*addr)
                })
                .collect();
            bank.sort_unstable();
            bank.dedup();
            bank
        })
        .collect()
}

/// Spend the publication word on independent register consumers. A clock may
/// drive many banks: units that read different destinations can use different
/// bits even when the conservative clock predicate is identical. Preserve the
/// topological order and never combine a retained unit with a conditional one.
pub(super) fn register_groups(
    groups: Vec<(Activation, Vec<usize>)>,
    banks: &[Vec<AbsoluteAddr>],
    sizes: &[usize],
) -> Vec<(Activation, Vec<usize>)> {
    const BITS: usize = 63; // bit 63 requests a settle for retained state only
    let mut candidates = Vec::new();
    for (activation, members) in groups {
        if activation.every_settle {
            candidates.push((activation, members));
        } else {
            for bank in members.chunk_by(|&a, &b| banks[a] == banks[b]) {
                candidates.push((activation.clone(), bank.to_vec()));
            }
        }
    }

    if candidates.iter().filter(|(a, _)| !a.every_settle).count() <= BITS {
        return candidates;
    }
    // Pack the smaller adjacent banks until their groups fit in one word.
    // Keep retained units as barriers. An expensive bank keeps its own bit
    // even when its cost exceeds the packing limit.
    let costs: Vec<usize> = candidates
        .iter()
        .map(|(_, members)| members.iter().map(|&i| sizes[i]).sum())
        .collect();
    let starts = |limit: usize| {
        candidates
            .iter()
            .zip(&costs)
            .scan(None, move |pending, ((a, _), &cost)| {
                if a.every_settle {
                    *pending = None;
                    return Some((true, true));
                }
                // A change in clock level alone must not wake a data-only bank.
                let combined = pending
                    .filter(|&(_, clock)| clock == a.clock_inputs)
                    .map(|(sum, _): (usize, bool)| sum + cost)
                    .filter(|&sum| sum <= limit);
                *pending = Some((combined.unwrap_or(cost), a.clock_inputs));
                Some((combined.is_none(), false))
            })
    };
    let fits = |limit| {
        starts(limit)
            .filter(|&(start, retained)| start && !retained)
            .count()
            <= BITS
    };
    let mut low = 0;
    let mut high = costs.iter().sum();
    if !fits(high) {
        return candidates;
    }
    while low < high {
        let middle = low + (high - low) / 2;
        if fits(middle) {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    let mut cuts: Vec<_> = starts(low)
        .enumerate()
        .filter_map(|(i, (start, _))| start.then_some(i))
        .collect();
    cuts.push(candidates.len());
    align_event_boundaries(&mut cuts, &candidates, &costs);
    let mut cuts = cuts.into_iter().peekable();
    let mut result: Vec<(Activation, Vec<usize>)> = Vec::new();
    for (i, (activation, members)) in candidates.into_iter().enumerate() {
        if cuts.next_if_eq(&i).is_some() {
            result.push((activation, members));
        } else {
            let (previous, units) = result.last_mut().unwrap();
            previous.events |= activation.events;
            debug_assert_eq!(previous.clock_inputs, activation.clock_inputs);
            units.extend(members);
        }
    }
    result
}

/// A size cut can strand a small run with an unrelated, expensive domain.
/// Move it across the cut when the neighboring mixed group already subscribes
/// to the same events. The donor then has one exact trigger again; the
/// neighbor needs no additional event. Preserve the bit and unit order.
fn align_event_boundaries(
    cuts: &mut [usize],
    candidates: &[(Activation, Vec<usize>)],
    costs: &[usize],
) {
    let domain = |i: usize| &candidates[i].0;
    for group in 0..cuts.len() - 1 {
        let (start, end) = (cuts[group], cuts[group + 1]);
        let Some(split) = (start + 1..end).find(|&i| domain(i) != domain(start)) else {
            continue;
        };
        if (split + 1..end).any(|i| domain(i) != domain(split)) {
            continue;
        }

        let cost = |range: std::ops::Range<usize>| costs[range].iter().sum::<usize>();
        let (cut, neighbor, moved) = match cost(start..split).cmp(&cost(split..end)) {
            std::cmp::Ordering::Less if group > 0 && domain(start - 1) == domain(start) => {
                (group, cuts[group - 1]..start, domain(start))
            }
            std::cmp::Ordering::Greater
                if end < candidates.len() && domain(end) == domain(split) =>
            {
                (group + 1, end..cuts[group + 2], domain(split))
            }
            _ => continue,
        };
        // A homogeneous neighbor may still have a more selective register
        // bank within its clock domain. Keep its membership intact.
        if neighbor.into_iter().any(|i| domain(i) != moved) {
            cuts[cut] = split;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use celox_design::{InstanceId, StateObjectId};

    #[test]
    fn size_packing_does_not_wake_a_heavy_bank_for_a_neighboring_event() {
        let banks: Vec<_> = (0..66)
            .map(|i| {
                vec![AbsoluteAddr {
                    instance_id: InstanceId(0),
                    var_id: StateObjectId(i),
                }]
            })
            .collect();
        let domain = |bit: usize| Activation {
            events: BigUint::from(1u8) << bit,
            ..Activation::default()
        };
        let mut sizes = vec![1000; banks.len()];
        // The 61 large units leave two bits. Size alone splits [6,3,1]
        // from [2,8], putting the rare eight-instruction bank on event 70.
        sizes[61..].copy_from_slice(&[6, 3, 1, 2, 8]);
        let input = vec![
            (domain(0), (0..62).collect()),
            (domain(1), vec![62]),
            (domain(70), vec![63, 64]),
            (domain(3), vec![65]),
        ];
        let groups = register_groups(input, &banks, &sizes);
        assert_eq!(groups.len(), 63);
        assert_eq!(groups[61].1, [61, 62, 63, 64]);
        assert_eq!(
            groups[61].0.events,
            domain(0).events | domain(1).events | domain(70).events
        );
        assert_eq!(groups[62].1, [65]);
        assert_eq!(groups[62].0.events, domain(3).events);
    }

    #[test]
    fn refining_an_event_boundary_preserves_homogeneous_banks_and_clocks() {
        let domain = |bit: u8, clock_inputs| Activation {
            events: (1u8 << bit).into(),
            clock_inputs,
            ..Activation::default()
        };
        let candidates = |clock| {
            [(0, false), (1, false), (1, clock), (2, clock), (3, clock)]
                .map(|(bit, clock)| (domain(bit, clock), Vec::new()))
        };
        // Move a short suffix into an already mixed neighbor in the same
        // clock domain. A clock-level trigger or a homogeneous neighbor must
        // keep its bank separate.
        let mut cuts = [0, 2, 5];
        align_event_boundaries(&mut cuts, &candidates(false), &[8, 2, 1, 3, 6]);
        assert_eq!(cuts, [0, 1, 5]);
        let mut cuts = [0, 2, 5];
        align_event_boundaries(&mut cuts, &candidates(true), &[8, 2, 1, 3, 6]);
        assert_eq!(cuts, [0, 2, 5]);
        let units = candidates(false);
        let mut cuts = [0, 2, 3];
        align_event_boundaries(&mut cuts, &units[..3], &[8, 2, 1]);
        assert_eq!(cuts, [0, 2, 3]);
    }

    #[test]
    fn independent_readers_share_a_clock_but_keep_their_own_publication_bits() {
        let banks: Vec<_> = (0..80)
            .map(|i| {
                vec![AbsoluteAddr {
                    instance_id: InstanceId(0),
                    var_id: StateObjectId(i),
                }]
            })
            .collect();
        let clock = Activation {
            events: 1u8.into(),
            ..Activation::default()
        };
        let retained = Activation {
            every_settle: true,
            ..Activation::default()
        };
        let mut costs = vec![1; banks.len()];
        costs[60] = 100;
        let groups = register_groups(
            vec![
                (clock.clone(), (0..40).collect()),
                (retained, vec![40]),
                (clock, (41..80).collect()),
            ],
            &banks,
            &costs,
        );
        assert!(groups.iter().filter(|(a, _)| !a.every_settle).count() <= 63);
        assert!(
            groups
                .iter()
                .any(|(a, units)| a.every_settle && units == &[40])
        );
        assert!(
            groups
                .iter()
                .any(|(a, units)| !a.every_settle && units == &[60])
        );
        assert!(groups.into_iter().flat_map(|(_, units)| units).eq(0..80));
    }
}
