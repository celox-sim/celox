//! Keep a computed select and the outputs it controls in the same settle partition.

use super::*;

/// Return the work boundaries that must stay inside a partition, including
/// materializations shared by the original schedule.
pub(super) fn group_mux_readers<A: Copy + Ord + Hash>(
    prepared: &mut PreparedSchedule<A>,
    arena: &SLTNodeArena<A>,
    request: &SettlePartitioning<A>,
    widths: &HashMap<A, usize>,
    unpacked: &HashMap<A, usize>,
    four_state: bool,
) -> Vec<bool> {
    let input = &prepared.input;
    let work = &prepared.scheduled_work;
    let held = held_work(prepared, arena);
    let mut position = vec![0; input.len()];
    let mut span = 0;
    let mut movable_in = vec![None; input.len()];
    for (index, item) in work.iter().enumerate() {
        let paths = work_paths(item);
        // Never cross an opaque effect, feedback component, or region that
        // relies on a particular point of materialization in the lowering cache.
        if held[index]
            || held.get(index + 1) == Some(&true)
            || matches!(item, ScheduledWork::CombScc(_))
            || paths.iter().any(|&id| {
                let path = &input[id];
                !matches!(path.target, LogicPathTarget::Var(_))
                    || path_has_effects(path)
                    || prepared.adj[id].contains(&id)
                    || is_exact_fold_path(id, &prepared.fold_group_schedule_index)
                    || !path.local_inputs.is_empty()
                    || !path.pre_lower_nodes.is_empty()
                    || !path.address_sources.is_subset(&path.sources)
            })
        {
            span += 1;
        } else if let ScheduledWork::CombPath(id) = *item {
            let path = &input[id];
            let scalar = |id: &A| !request.clock_nets.contains(id) && !unpacked.contains_key(id);
            if path.previous_sources.is_empty()
                && path.address_sources.is_empty()
                && path.target.var().is_some_and(|target| scalar(&target.id))
                && path.sources.iter().all(|src| scalar(&src.id))
            {
                movable_in[id] = Some(span);
            }
        }
        for &id in paths {
            position[id] = index;
        }
    }
    let mut predecessors = vec![Vec::new(); input.len()];
    for (id, users) in prepared.adj.iter().enumerate() {
        for &user in users {
            predecessors[user].push(id);
        }
    }
    // Original work occupies odd slots. Move a group into an even slot
    // between work items, preserving all external MemorySSA edges (including
    // anti-dependencies) and the original order of its own assignments.
    let mut slots: Vec<_> = position.iter().map(|i| 2 * i + 1).collect();
    let mut group_at = vec![None; work.len()];
    for item in work.iter().rev() {
        let ScheduledWork::CombPath(id) = *item else {
            continue;
        };
        let Some(span) = movable_in[id] else { continue };
        let path = &input[id];
        let target = path.target.var().unwrap().id;
        let users = &prepared.adj[id];
        if widths.get(&target) != Some(&1)
            || request.external_readers.contains_key(&target)
            || request.write_domains.contains_key(&target)
            || path.sources.len() < 2
            || users.len() < 2
            || users.iter().any(|&u| {
                movable_in[u] != Some(span) || !input[u].sources.iter().any(|src| src.id == target)
            })
        {
            continue;
        }
        let mut selects = HashSet::default();
        if !users
            .iter()
            .filter_map(|&u| scheduled_root_mux(&input[u], arena))
            .any(|(select, _, _)| {
                !selects.insert(select)
                    && condition_reads_target(select, &HashSet::from_iter([target]), arena)
            })
        {
            continue;
        }
        let members: HashSet<_> = users.iter().copied().chain([id]).collect();
        let mut first = 2 * position[id];
        let mut last = 2 * (users.iter().map(|&u| position[u]).max().unwrap() + 1);
        for &m in &members {
            for dep in &predecessors[m] {
                if !members.contains(dep) {
                    first = first.max(slots[*dep] + 1);
                }
            }
            for user in &prepared.adj[m] {
                if !members.contains(user) {
                    last = last.min(slots[*user].saturating_sub(1));
                }
            }
        }
        let slot = first.next_multiple_of(2);
        if slot <= last {
            for m in members {
                slots[m] = slot;
                group_at[position[m]] = Some(id);
                movable_in[m] = None;
            }
        }
    }
    if group_at.iter().all(Option::is_none) {
        return held;
    }
    let mut ordered: Vec<_> = std::mem::take(&mut prepared.scheduled_work)
        .into_iter()
        .enumerate()
        .map(|(i, work)| (slots[work_paths(&work)[0]], group_at[i], held[i], work))
        .collect();
    ordered.sort_by_key(|(slot, group, _, _)| (*slot, *group));
    let mut work = ordered.into_iter().peekable();
    let mut boundaries = Vec::with_capacity(work.len());
    let result = &mut prepared.scheduled_work;
    while let Some((_, group, held, first)) = work.next() {
        if group.is_none() {
            boundaries.push(held);
            result.push(first);
            continue;
        }
        let mut members = vec![first];
        while let Some((_, _, _, next)) = work.next_if(|(_, next, _, _)| *next == group) {
            members.push(next);
        }
        boundaries.push(false);
        result.extend(form_scheduled_guard_regions(
            members, input, arena, four_state,
        ));
        boundaries.resize(result.len(), true);
    }
    boundaries
}
