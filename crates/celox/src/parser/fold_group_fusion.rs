use std::collections::{BTreeMap, BTreeSet};

use num_bigint::BigInt;
use veryl_analyzer::ir::VarId;

use super::ParserError;
use crate::ir::{BitAccess, CombObserver, LogicPathId, VarAtomBase};
use crate::logic_tree::{
    LogicPath, LogicPathTarget, NodeId, SLTForFoldGroupState, SLTIndex, SLTNode, SLTNodeArena,
    SymbolicStore, get_width,
};
use crate::{HashMap, HashSet};

fn discover_recovered_fold_groups(
    paths: &[LogicPath<VarId>],
    arena: &SLTNodeArena<VarId>,
) -> Vec<RecoveredFoldGroup> {
    let mut groups: BTreeMap<NodeId, Vec<usize>> = BTreeMap::new();
    for (local_index, path) in paths.iter().enumerate() {
        let Some(group) = projected_group(path.expr, arena) else {
            continue;
        };
        groups.entry(group).or_default().push(local_index);
    }
    groups
        .into_iter()
        .map(|(group, paths)| RecoveredFoldGroup { group, paths })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecoveredFoldGroup {
    group: NodeId,
    paths: Vec<usize>,
}

pub(super) fn fuse_recovered_fold_groups(
    arena: &mut SLTNodeArena<VarId>,
    paths: &mut [LogicPath<VarId>],
    store: &mut SymbolicStore<VarId>,
    observers: &[CombObserver<VarId>],
) -> Result<(), ParserError> {
    let recovered = discover_recovered_fold_groups(paths, arena);
    let specs = recovered
        .iter()
        .filter_map(|group| group_spec(group, arena))
        .collect::<Vec<_>>();

    let mut candidates: BTreeMap<VarId, Vec<usize>> = BTreeMap::new();
    for (index, spec) in specs.iter().enumerate() {
        for base in loop_indexed_external_bases(spec, arena) {
            candidates.entry(base).or_default().push(index);
        }
    }
    let mut families = candidates
        .into_values()
        .flat_map(|indices| {
            let mut by_domain: Vec<Vec<usize>> = Vec::new();
            for index in indices {
                if let Some(family) = by_domain.iter_mut().find(|family| {
                    same_domain_and_guard(specs[family[0]].node, specs[index].node, arena)
                }) {
                    family.push(index);
                } else {
                    by_domain.push(vec![index]);
                }
            }
            by_domain
        })
        .filter(|family| family.len() >= 2)
        .collect::<Vec<_>>();
    for family in &mut families {
        family.sort_unstable_by_key(|index| specs[*index].node);
    }
    families.sort_unstable_by(|lhs, rhs| {
        rhs.len().cmp(&lhs.len()).then_with(|| {
            lhs.iter()
                .map(|index| specs[*index].node)
                .cmp(rhs.iter().map(|index| specs[*index].node))
        })
    });
    families.dedup();

    let mut consumed = HashSet::default();
    for family in families {
        if family.iter().any(|index| consumed.contains(index)) {
            continue;
        }
        let family_specs = family
            .iter()
            .map(|index| specs[*index].clone())
            .collect::<Vec<_>>();
        if try_fuse_family(arena, paths, store, observers, &family_specs)? {
            consumed.extend(family);
        }
    }
    Ok(())
}

fn same_domain_and_guard(lhs: NodeId, rhs: NodeId, arena: &SLTNodeArena<VarId>) -> bool {
    let SLTNode::ForFoldGroup {
        loop_width: lhs_width,
        loop_signed: lhs_signed,
        start: lhs_start,
        step: lhs_step,
        trip_count: lhs_count,
        entry_guard: lhs_guard,
        ..
    } = arena.get(lhs)
    else {
        return false;
    };
    let SLTNode::ForFoldGroup {
        loop_width: rhs_width,
        loop_signed: rhs_signed,
        start: rhs_start,
        step: rhs_step,
        trip_count: rhs_count,
        entry_guard: rhs_guard,
        ..
    } = arena.get(rhs)
    else {
        return false;
    };
    lhs_width == rhs_width
        && lhs_signed == rhs_signed
        && lhs_start == rhs_start
        && lhs_step == rhs_step
        && lhs_count == rhs_count
        && lhs_guard == rhs_guard
}

#[derive(Clone)]
struct GroupSpec {
    node: NodeId,
    loop_var: VarId,
    loop_width: usize,
    loop_signed: bool,
    start: BigInt,
    step: BigInt,
    trip_count: usize,
    entry_guard: NodeId,
    states: Vec<SLTForFoldGroupState<VarId>>,
    state_packed_accesses: Vec<BitAccess>,
    paths: Vec<usize>,
}

fn group_spec(recovered: &RecoveredFoldGroup, arena: &SLTNodeArena<VarId>) -> Option<GroupSpec> {
    let SLTNode::ForFoldGroup {
        loop_var,
        loop_width,
        loop_signed,
        start,
        step,
        trip_count,
        entry_guard,
        states,
    } = arena.get(recovered.group)
    else {
        return None;
    };
    let (_, state_packed_accesses) = packed_state_accesses(states)?;
    Some(GroupSpec {
        node: recovered.group,
        loop_var: *loop_var,
        loop_width: *loop_width,
        loop_signed: *loop_signed,
        start: start.clone(),
        step: step.clone(),
        trip_count: *trip_count,
        entry_guard: *entry_guard,
        states: states.clone(),
        state_packed_accesses,
        paths: recovered.paths.clone(),
    })
}

fn packed_state_accesses(
    states: &[SLTForFoldGroupState<VarId>],
) -> Option<(usize, Vec<BitAccess>)> {
    let total = states.iter().try_fold(0usize, |sum, state| {
        let width = state
            .target
            .access
            .msb
            .checked_sub(state.target.access.lsb)?
            .checked_add(1)?;
        sum.checked_add(width)
    })?;
    if total == 0 {
        return None;
    }
    let mut next_msb = total;
    let mut accesses = Vec::with_capacity(states.len());
    for state in states {
        let width = state.target.access.msb - state.target.access.lsb + 1;
        next_msb = next_msb.checked_sub(width)?;
        accesses.push(BitAccess::new(next_msb, next_msb + width - 1));
    }
    Some((total, accesses))
}

fn projected_group(mut node: NodeId, arena: &SLTNodeArena<VarId>) -> Option<NodeId> {
    loop {
        match arena.get(node) {
            SLTNode::ForFoldGroup { .. } => return Some(node),
            SLTNode::Slice { expr, .. } => node = *expr,
            _ => return None,
        }
    }
}

fn projection_access(
    node: NodeId,
    group: NodeId,
    arena: &SLTNodeArena<VarId>,
) -> Option<BitAccess> {
    if node == group {
        let width = get_width(group, arena);
        return (width != 0).then(|| BitAccess::new(0, width - 1));
    }
    let SLTNode::Slice { expr, access } = arena.get(node) else {
        return None;
    };
    let parent = projection_access(*expr, group, arena)?;
    if access.msb >= parent.msb - parent.lsb + 1 {
        return None;
    }
    Some(BitAccess::new(
        parent.lsb.checked_add(access.lsb)?,
        parent.lsb.checked_add(access.msb)?,
    ))
}

fn try_fuse_family(
    arena: &mut SLTNodeArena<VarId>,
    paths: &mut [LogicPath<VarId>],
    store: &mut SymbolicStore<VarId>,
    observers: &[CombObserver<VarId>],
    specs: &[GroupSpec],
) -> Result<bool, ParserError> {
    if specs.len() < 2 || !family_is_independent(&specs, arena) {
        return Ok(false);
    }

    let Some(preflight) = preflight_roots(&specs, paths, store, observers, arena) else {
        return Ok(false);
    };

    let canonical_loop_var = specs[0].loop_var;
    let mut remap_cache = HashMap::default();
    let mut fused_states = Vec::new();
    for spec in specs {
        for state in &spec.states {
            let update = remap_loop_input(
                state.update,
                spec.loop_var,
                canonical_loop_var,
                arena,
                &mut remap_cache,
            )?;
            fused_states.push(SLTForFoldGroupState {
                target: state.target,
                initial: state.initial,
                update,
            });
        }
    }
    let fused_group = arena.alloc(SLTNode::ForFoldGroup {
        loop_var: canonical_loop_var,
        loop_width: specs[0].loop_width,
        loop_signed: specs[0].loop_signed,
        start: specs[0].start.clone(),
        step: specs[0].step.clone(),
        trip_count: specs[0].trip_count,
        entry_guard: specs[0].entry_guard,
        states: fused_states.clone(),
    })?;
    let (_, fused_accesses) = packed_state_accesses(&fused_states).ok_or_else(|| {
        ParserError::illegal_context(
            "ForFoldGroup product fusion",
            "fused state packing is not representable",
            None,
        )
    })?;

    let mut target_projection = HashMap::default();
    for (state, access) in fused_states.iter().zip(fused_accesses) {
        let projection = arena.alloc(SLTNode::Slice {
            expr: fused_group,
            access,
        })?;
        target_projection.insert(state.target, projection);
    }

    commit_rewrite(&specs, &preflight, &target_projection, paths, store);

    let old_groups = specs.iter().map(|spec| spec.node).collect::<HashSet<_>>();
    if semantic_roots_reach_groups(paths, store, observers, arena, &old_groups) {
        rollback_rewrite(paths, store, preflight);
        return Err(ParserError::illegal_context(
            "ForFoldGroup product fusion",
            "an old grouped fold remains reachable after projection rewrite",
            None,
        ));
    }
    Ok(true)
}

struct Preflight {
    path_snapshots: Vec<(usize, LogicPath<VarId>)>,
    store_snapshots: Vec<(VarId, usize, Option<(NodeId, HashSet<VarAtomBase<VarId>>)>)>,
    sources: HashSet<VarAtomBase<VarId>>,
    previous_sources: HashSet<VarAtomBase<VarId>>,
    address_sources: HashSet<VarAtomBase<VarId>>,
    order_before: HashSet<LogicPathId>,
}

fn preflight_roots(
    specs: &[GroupSpec],
    paths: &[LogicPath<VarId>],
    store: &SymbolicStore<VarId>,
    observers: &[CombObserver<VarId>],
    arena: &SLTNodeArena<VarId>,
) -> Option<Preflight> {
    let old_groups = specs.iter().map(|spec| spec.node).collect::<HashSet<_>>();
    let family_paths = specs
        .iter()
        .flat_map(|spec| spec.paths.iter().copied())
        .collect::<BTreeSet<_>>();
    if family_paths.len() != specs.iter().map(|spec| spec.states.len()).sum::<usize>() {
        return None;
    }

    let mut expected_paths = HashMap::default();
    for spec in specs {
        if spec.paths.len() != spec.states.len() {
            return None;
        }
        for (state, expected_access) in spec.states.iter().zip(&spec.state_packed_accesses) {
            let matches = spec
                .paths
                .iter()
                .copied()
                .filter(|&index| {
                    paths.get(index).is_some_and(|path| {
                        path.target.var() == Some(&state.target)
                            && projection_access(path.expr, spec.node, arena)
                                == Some(*expected_access)
                    })
                })
                .collect::<Vec<_>>();
            if matches.len() != 1 || expected_paths.insert(matches[0], state.target).is_some() {
                return None;
            }
        }
    }

    let mut sources = HashSet::default();
    let mut previous_sources = HashSet::default();
    let mut address_sources = HashSet::default();
    let mut order_before = HashSet::default();
    let mut path_snapshots = Vec::with_capacity(family_paths.len());
    for (index, path) in paths.iter().enumerate() {
        let permitted = expected_paths.contains_key(&index);
        if reaches_groups(path.expr, arena, &old_groups) != permitted {
            return None;
        }
        if path
            .local_inputs
            .iter()
            .any(|(_, node)| reaches_groups(*node, arena, &old_groups))
            || path
                .pre_lower_nodes
                .iter()
                .any(|node| reaches_groups(*node, arena, &old_groups))
            || target_reaches_groups(&path.target, arena, &old_groups)
        {
            return None;
        }
        if permitted {
            if !path.local_inputs.is_empty() || !path.pre_lower_nodes.is_empty() {
                return None;
            }
            if path
                .order_before
                .iter()
                .any(|ordered| family_paths.contains(&ordered.0))
            {
                return None;
            }
            sources.extend(path.sources.iter().copied());
            previous_sources.extend(path.previous_sources.iter().copied());
            address_sources.extend(path.address_sources.iter().copied());
            order_before.extend(path.order_before.iter().copied());
            path_snapshots.push((index, path.clone()));
        }
    }
    if temporal_source_classes_conflict(&path_snapshots) {
        return None;
    }

    for observer in observers {
        if observer_roots(observer)
            .into_iter()
            .any(|node| reaches_groups(node, arena, &old_groups))
        {
            return None;
        }
    }

    let state_targets = specs
        .iter()
        .flat_map(|spec| spec.states.iter().map(|state| state.target))
        .collect::<HashSet<_>>();
    let mut store_snapshots = Vec::new();
    let mut seen_store_targets = HashSet::default();
    for (id, ranges) in store {
        for (&lsb, (value, width, _origin)) in &ranges.ranges {
            let Some((node, _)) = value else {
                continue;
            };
            if !reaches_groups(*node, arena, &old_groups) {
                continue;
            }
            let msb = lsb.checked_add(*width)?.checked_sub(1)?;
            let target = VarAtomBase::new(*id, lsb, msb);
            if !state_targets.contains(&target) {
                return None;
            }
            let spec = specs.iter().find(|spec| {
                spec.states.iter().any(|state| state.target == target)
                    && projection_access(*node, spec.node, arena).is_some()
            })?;
            let state_index = spec
                .states
                .iter()
                .position(|state| state.target == target)?;
            if projection_access(*node, spec.node, arena)
                != Some(spec.state_packed_accesses[state_index])
                || !seen_store_targets.insert(target)
            {
                return None;
            }
            store_snapshots.push((*id, lsb, value.clone()));
        }
    }

    Some(Preflight {
        path_snapshots,
        store_snapshots,
        sources,
        previous_sources,
        address_sources,
        order_before,
    })
}

fn commit_rewrite(
    specs: &[GroupSpec],
    preflight: &Preflight,
    target_projection: &HashMap<VarAtomBase<VarId>, NodeId>,
    paths: &mut [LogicPath<VarId>],
    store: &mut SymbolicStore<VarId>,
) {
    let family_paths = specs
        .iter()
        .flat_map(|spec| spec.paths.iter().copied())
        .collect::<HashSet<_>>();
    for index in family_paths {
        let path = &mut paths[index];
        let target = *path
            .target
            .var()
            .expect("preflight requires a variable target");
        path.expr = target_projection[&target];
        path.sources = preflight.sources.clone();
        path.previous_sources = preflight.previous_sources.clone();
        path.address_sources = preflight.address_sources.clone();
        path.order_before = preflight.order_before.clone();
    }
    for (id, lsb, _) in &preflight.store_snapshots {
        let entry = store
            .get_mut(id)
            .and_then(|ranges| ranges.ranges.get_mut(lsb))
            .expect("preflight recorded an existing symbolic-store entry");
        let width = entry.1;
        let target = VarAtomBase::new(*id, *lsb, *lsb + width - 1);
        entry.0 = Some((target_projection[&target], preflight.sources.clone()));
    }
}

fn rollback_rewrite(
    paths: &mut [LogicPath<VarId>],
    store: &mut SymbolicStore<VarId>,
    preflight: Preflight,
) {
    for (index, path) in preflight.path_snapshots {
        paths[index] = path;
    }
    for (id, lsb, value) in preflight.store_snapshots {
        if let Some(entry) = store
            .get_mut(&id)
            .and_then(|ranges| ranges.ranges.get_mut(&lsb))
        {
            entry.0 = value;
        }
    }
}

fn temporal_source_classes_conflict(paths: &[(usize, LogicPath<VarId>)]) -> bool {
    for (_, lhs) in paths {
        for lhs_source in &lhs.sources {
            for (_, rhs) in paths {
                for rhs_source in &rhs.sources {
                    if rhs_source.id != lhs_source.id
                        || !rhs_source.access.overlaps(&lhs_source.access)
                    {
                        continue;
                    }

                    let overlap = BitAccess::new(
                        lhs_source.access.lsb.max(rhs_source.access.lsb),
                        lhs_source.access.msb.min(rhs_source.access.msb),
                    );
                    let mut atom_starts = vec![overlap.lsb];
                    for ranges in [
                        &lhs.previous_sources,
                        &lhs.address_sources,
                        &rhs.previous_sources,
                        &rhs.address_sources,
                    ] {
                        for range in ranges {
                            if range.id != lhs_source.id || !range.access.overlaps(&overlap) {
                                continue;
                            }
                            atom_starts.push(range.access.lsb.max(overlap.lsb));
                            if let Some(after) = range.access.msb.min(overlap.msb).checked_add(1)
                                && after <= overlap.msb
                            {
                                atom_starts.push(after);
                            }
                        }
                    }
                    atom_starts.sort_unstable();
                    atom_starts.dedup();
                    if atom_starts.into_iter().any(|bit| {
                        temporal_source_class(lhs, &lhs_source.id, bit)
                            != temporal_source_class(rhs, &lhs_source.id, bit)
                    }) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TemporalSourceClass {
    CurrentOnly,
    PreviousOnly,
    Both,
}

fn temporal_source_class(path: &LogicPath<VarId>, id: &VarId, bit: usize) -> TemporalSourceClass {
    let contains = |sources: &HashSet<VarAtomBase<VarId>>| {
        sources
            .iter()
            .any(|source| &source.id == id && source.access.lsb <= bit && bit <= source.access.msb)
    };
    let previous = contains(&path.previous_sources);
    let address = contains(&path.address_sources);
    match (previous, address) {
        (false, _) => TemporalSourceClass::CurrentOnly,
        (true, false) => TemporalSourceClass::PreviousOnly,
        (true, true) => TemporalSourceClass::Both,
    }
}

fn loop_indexed_external_bases(spec: &GroupSpec, arena: &SLTNodeArena<VarId>) -> BTreeSet<VarId> {
    let state_ids = spec
        .states
        .iter()
        .map(|state| state.target.id)
        .collect::<HashSet<_>>();
    let mut bases = BTreeSet::new();
    let mut visited = HashSet::default();
    let mut work = spec
        .states
        .iter()
        .map(|state| state.update)
        .collect::<Vec<_>>();
    while let Some(node) = work.pop() {
        if !visited.insert(node) {
            continue;
        }
        match arena.get(node) {
            SLTNode::Input {
                variable, index, ..
            } => {
                if *variable != spec.loop_var
                    && !state_ids.contains(variable)
                    && index
                        .iter()
                        .any(|entry| node_reads_variable(entry.node, spec.loop_var, arena))
                {
                    bases.insert(*variable);
                }
                work.extend(index.iter().map(|entry| entry.node));
            }
            _ => push_children(node, arena, &mut work),
        }
    }
    bases
}

fn node_reads_variable(root: NodeId, variable: VarId, arena: &SLTNodeArena<VarId>) -> bool {
    let mut visited = HashSet::default();
    let mut work = vec![root];
    while let Some(node) = work.pop() {
        if !visited.insert(node) {
            continue;
        }
        if matches!(arena.get(node), SLTNode::Input { variable: found, .. } if *found == variable) {
            return true;
        }
        push_children(node, arena, &mut work);
    }
    false
}

fn family_is_independent(specs: &[GroupSpec], arena: &SLTNodeArena<VarId>) -> bool {
    let mut state_owners: HashMap<VarId, Vec<(usize, BitAccess)>> = HashMap::default();
    for (owner, spec) in specs.iter().enumerate() {
        for state in &spec.states {
            let ranges = state_owners.entry(state.target.id).or_default();
            if ranges
                .iter()
                .any(|(_, range)| range.overlaps(&state.target.access))
            {
                return false;
            }
            ranges.push((owner, state.target.access));
        }
    }
    let loop_vars = specs
        .iter()
        .map(|spec| spec.loop_var)
        .collect::<HashSet<_>>();
    if loop_vars
        .iter()
        .any(|loop_var| state_owners.contains_key(loop_var))
    {
        return false;
    }
    let mut reads = Vec::new();
    if !collect_reads(specs[0].entry_guard, arena, &mut reads)
        || reads
            .iter()
            .any(|read| read_overlaps_states(read, &state_owners) || loop_vars.contains(&read.id))
    {
        return false;
    }
    for (owner, spec) in specs.iter().enumerate() {
        for state in &spec.states {
            reads.clear();
            if !collect_reads(state.initial, arena, &mut reads)
                || reads.iter().any(|read| {
                    read_overlaps_states(read, &state_owners) || loop_vars.contains(&read.id)
                })
            {
                return false;
            }
            reads.clear();
            if !collect_reads(state.update, arena, &mut reads) {
                return false;
            }
            for read in &reads {
                if loop_vars.contains(&read.id) {
                    if read.id != spec.loop_var || read.indexed {
                        return false;
                    }
                    continue;
                }
                let Some(ranges) = state_owners.get(&read.id) else {
                    continue;
                };
                if read.indexed
                    || ranges.iter().any(|(state_owner, range)| {
                        *state_owner != owner && range.overlaps(&read.access)
                    })
                {
                    return false;
                }
            }
        }
    }
    true
}

#[derive(Clone, Copy)]
struct InputRead {
    id: VarId,
    access: BitAccess,
    indexed: bool,
}

fn read_overlaps_states(
    read: &InputRead,
    owners: &HashMap<VarId, Vec<(usize, BitAccess)>>,
) -> bool {
    owners.get(&read.id).is_some_and(|ranges| {
        read.indexed || ranges.iter().any(|(_, range)| range.overlaps(&read.access))
    })
}

fn collect_reads(root: NodeId, arena: &SLTNodeArena<VarId>, reads: &mut Vec<InputRead>) -> bool {
    let mut visited = HashSet::default();
    let mut work = vec![root];
    while let Some(node) = work.pop() {
        if !visited.insert(node) {
            continue;
        }
        match arena.get(node) {
            SLTNode::Input {
                variable,
                index,
                access,
                ..
            } => {
                reads.push(InputRead {
                    id: *variable,
                    access: *access,
                    indexed: !index.is_empty(),
                });
                work.extend(index.iter().map(|index| index.node));
            }
            SLTNode::Constant(..) => {}
            SLTNode::Binary(lhs, _, rhs) => {
                work.push(*lhs);
                work.push(*rhs);
            }
            SLTNode::Unary(_, inner) => work.push(*inner),
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                work.push(*cond);
                work.push(*then_expr);
                work.push(*else_expr);
            }
            SLTNode::Concat(parts) => work.extend(parts.iter().map(|(part, _)| *part)),
            SLTNode::Slice { expr, .. } => work.push(*expr),
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => return false,
        }
    }
    true
}

fn remap_loop_input(
    root: NodeId,
    old_loop_var: VarId,
    canonical_loop_var: VarId,
    arena: &mut SLTNodeArena<VarId>,
    cache: &mut HashMap<(VarId, NodeId), NodeId>,
) -> Result<NodeId, ParserError> {
    if old_loop_var == canonical_loop_var {
        return Ok(root);
    }
    if let Some(mapped) = cache.get(&(old_loop_var, root)) {
        return Ok(*mapped);
    }
    let original = arena.get(root).clone();
    let mapped = match original {
        SLTNode::Input {
            variable,
            signed,
            index,
            access,
        } => {
            let mut changed = variable == old_loop_var;
            let mut mapped_index = Vec::with_capacity(index.len());
            for entry in index {
                let node =
                    remap_loop_input(entry.node, old_loop_var, canonical_loop_var, arena, cache)?;
                changed |= node != entry.node;
                mapped_index.push(SLTIndex {
                    node,
                    stride: entry.stride,
                });
            }
            if !changed {
                root
            } else {
                arena.alloc(SLTNode::Input {
                    variable: if variable == old_loop_var {
                        canonical_loop_var
                    } else {
                        variable
                    },
                    signed,
                    index: mapped_index,
                    access,
                })?
            }
        }
        SLTNode::Constant(..) => root,
        SLTNode::Binary(lhs, op, rhs) => {
            let mapped_lhs = remap_loop_input(lhs, old_loop_var, canonical_loop_var, arena, cache)?;
            let mapped_rhs = remap_loop_input(rhs, old_loop_var, canonical_loop_var, arena, cache)?;
            if mapped_lhs == lhs && mapped_rhs == rhs {
                root
            } else {
                arena.alloc(SLTNode::Binary(mapped_lhs, op, mapped_rhs))?
            }
        }
        SLTNode::Unary(op, inner) => {
            let mapped_inner =
                remap_loop_input(inner, old_loop_var, canonical_loop_var, arena, cache)?;
            if mapped_inner == inner {
                root
            } else {
                arena.alloc(SLTNode::Unary(op, mapped_inner))?
            }
        }
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => {
            let mapped_cond =
                remap_loop_input(cond, old_loop_var, canonical_loop_var, arena, cache)?;
            let mapped_then =
                remap_loop_input(then_expr, old_loop_var, canonical_loop_var, arena, cache)?;
            let mapped_else =
                remap_loop_input(else_expr, old_loop_var, canonical_loop_var, arena, cache)?;
            if mapped_cond == cond && mapped_then == then_expr && mapped_else == else_expr {
                root
            } else {
                arena.alloc(SLTNode::Mux {
                    cond: mapped_cond,
                    then_expr: mapped_then,
                    else_expr: mapped_else,
                })?
            }
        }
        SLTNode::Concat(parts) => {
            let mut changed = false;
            let mut mapped_parts = Vec::with_capacity(parts.len());
            for (part, width) in parts {
                let mapped =
                    remap_loop_input(part, old_loop_var, canonical_loop_var, arena, cache)?;
                changed |= mapped != part;
                mapped_parts.push((mapped, width));
            }
            if changed {
                arena.alloc(SLTNode::Concat(mapped_parts))?
            } else {
                root
            }
        }
        SLTNode::Slice { expr, access } => {
            let mapped_expr =
                remap_loop_input(expr, old_loop_var, canonical_loop_var, arena, cache)?;
            if mapped_expr == expr {
                root
            } else {
                arena.alloc(SLTNode::Slice {
                    expr: mapped_expr,
                    access,
                })?
            }
        }
        SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => {
            return Err(ParserError::illegal_context(
                "ForFoldGroup product fusion",
                "a state update contains a nested fold",
                None,
            ));
        }
    };
    cache.insert((old_loop_var, root), mapped);
    Ok(mapped)
}

fn reaches_groups(root: NodeId, arena: &SLTNodeArena<VarId>, groups: &HashSet<NodeId>) -> bool {
    let mut visited = HashSet::default();
    let mut work = vec![root];
    while let Some(node) = work.pop() {
        if groups.contains(&node) {
            return true;
        }
        if !visited.insert(node) {
            continue;
        }
        push_children(node, arena, &mut work);
    }
    false
}

fn push_children(node: NodeId, arena: &SLTNodeArena<VarId>, work: &mut Vec<NodeId>) {
    match arena.get(node) {
        SLTNode::Input { index, .. } => work.extend(index.iter().map(|index| index.node)),
        SLTNode::Constant(..) => {}
        SLTNode::Binary(lhs, _, rhs) => {
            work.push(*lhs);
            work.push(*rhs);
        }
        SLTNode::Unary(_, inner) => work.push(*inner),
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => {
            work.push(*cond);
            work.push(*then_expr);
            work.push(*else_expr);
        }
        SLTNode::ForFold {
            start,
            end,
            initials,
            updates,
            effects,
            continue_cond,
            ..
        } => {
            if let crate::logic_tree::SLTLoopBound::Expr(node) = start {
                work.push(*node);
            }
            if let crate::logic_tree::SLTLoopBound::Expr(node) = end {
                work.push(*node);
            }
            work.extend(initials.iter().map(|state| state.expr));
            work.extend(updates.iter().map(|state| state.expr));
            for effect in effects {
                work.extend(effect.guard);
                work.extend(effect.args.iter().copied());
            }
            work.push(*continue_cond);
        }
        SLTNode::ForFoldGroup {
            entry_guard,
            states,
            ..
        } => {
            work.push(*entry_guard);
            for state in states {
                work.push(state.initial);
                work.push(state.update);
            }
        }
        SLTNode::Concat(parts) => work.extend(parts.iter().map(|(part, _)| *part)),
        SLTNode::Slice { expr, .. } => work.push(*expr),
    }
}

fn target_reaches_groups(
    target: &LogicPathTarget<VarId>,
    arena: &SLTNodeArena<VarId>,
    groups: &HashSet<NodeId>,
) -> bool {
    match target {
        LogicPathTarget::Var(_) => false,
        LogicPathTarget::CombCaptureEvent {
            guard,
            args,
            loop_runner,
            ..
        } => guard
            .iter()
            .chain(args)
            .chain(loop_runner)
            .copied()
            .any(|node| reaches_groups(node, arena, groups)),
    }
}

fn observer_roots(observer: &CombObserver<VarId>) -> Vec<NodeId> {
    observer
        .guard
        .iter()
        .chain(&observer.args)
        .chain(observer.loop_runner.iter())
        .copied()
        .chain(observer.local_inputs.iter().map(|(_, node)| *node))
        .collect()
}

fn semantic_roots_reach_groups(
    paths: &[LogicPath<VarId>],
    store: &SymbolicStore<VarId>,
    observers: &[CombObserver<VarId>],
    arena: &SLTNodeArena<VarId>,
    groups: &HashSet<NodeId>,
) -> bool {
    paths.iter().any(|path| {
        reaches_groups(path.expr, arena, groups)
            || path
                .local_inputs
                .iter()
                .any(|(_, node)| reaches_groups(*node, arena, groups))
            || path
                .pre_lower_nodes
                .iter()
                .any(|node| reaches_groups(*node, arena, groups))
            || target_reaches_groups(&path.target, arena, groups)
    }) || store.values().any(|ranges| {
        ranges.ranges.values().any(|(value, _, _)| {
            value
                .as_ref()
                .is_some_and(|(node, _)| reaches_groups(*node, arena, groups))
        })
    }) || observers.iter().any(|observer| {
        observer_roots(observer)
            .into_iter()
            .any(|node| reaches_groups(node, arena, groups))
    })
}

#[cfg(test)]
mod tests {
    use num_bigint::{BigInt, BigUint};
    use veryl_analyzer::ir::VarId;

    use super::{
        discover_recovered_fold_groups, fuse_recovered_fold_groups, node_reads_variable,
        packed_state_accesses, projected_group, projection_access, push_children, reaches_groups,
    };
    use crate::ir::{BinaryOp, BitAccess, LogicPathId, SIRInstruction, SIRTerminator, VarAtomBase};
    use crate::logic_tree::range_store::RangeStore;
    use crate::logic_tree::{
        LogicPath, LogicPathTarget, NodeId, SLTForFoldGroupState, SLTIndex, SLTNode, SLTNodeArena,
        SymbolicStore,
    };
    use crate::{HashMap, HashSet};

    fn var(raw: u32) -> VarId {
        let mut id = VarId::default();
        for _ in 0..raw {
            id.inc();
        }
        id
    }

    fn constant(arena: &mut SLTNodeArena<VarId>, width: usize, value: u8) -> NodeId {
        arena
            .alloc(SLTNode::Constant(
                BigUint::from(value),
                BigUint::from(0u8),
                width,
                false,
            ))
            .unwrap()
    }

    fn input(arena: &mut SLTNodeArena<VarId>, variable: VarId, width: usize) -> NodeId {
        arena
            .alloc(SLTNode::Input {
                variable,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, width - 1),
            })
            .unwrap()
    }

    fn indexed_input(
        arena: &mut SLTNodeArena<VarId>,
        variable: VarId,
        loop_var: VarId,
        width: usize,
    ) -> NodeId {
        let index = input(arena, loop_var, 8);
        arena
            .alloc(SLTNode::Input {
                variable,
                signed: false,
                index: vec![SLTIndex {
                    node: index,
                    stride: 8,
                }],
                access: BitAccess::new(0, width - 1),
            })
            .unwrap()
    }

    fn atoms(values: &[(VarId, usize, usize)]) -> HashSet<VarAtomBase<VarId>> {
        values
            .iter()
            .map(|(id, lsb, msb)| VarAtomBase::new(*id, *lsb, *msb))
            .collect()
    }

    fn path(
        target: VarAtomBase<VarId>,
        expr: NodeId,
        sources: HashSet<VarAtomBase<VarId>>,
        address_sources: HashSet<VarAtomBase<VarId>>,
        order_before: HashSet<LogicPathId>,
    ) -> LogicPath<VarId> {
        LogicPath {
            target: LogicPathTarget::Var(target),
            sources,
            previous_sources: HashSet::default(),
            address_sources,
            local_inputs: Vec::new(),
            order_before,
            comb_capture_enable_sites: Vec::new(),
            pre_lower_nodes: Vec::new(),
            expr,
        }
    }

    fn install_store_value(
        store: &mut SymbolicStore<VarId>,
        target: VarAtomBase<VarId>,
        expr: NodeId,
        sources: HashSet<VarAtomBase<VarId>>,
    ) {
        let width = target.access.msb - target.access.lsb + 1;
        assert_eq!(target.access.lsb, 0);
        store.insert(target.id, RangeStore::new(Some((expr, sources)), width));
    }

    struct Fixture {
        arena: SLTNodeArena<VarId>,
        paths: Vec<LogicPath<VarId>>,
        store: SymbolicStore<VarId>,
        old_groups: [NodeId; 2],
        targets: Vec<VarAtomBase<VarId>>,
        loop_vars: [VarId; 2],
        external: VarId,
    }

    fn positive_fixture() -> Fixture {
        let mut arena = SLTNodeArena::new();
        let guard = constant(&mut arena, 1, 1);
        let external = var(50);
        let loop_a = var(100);
        let loop_b = var(101);
        let target_a0 = VarAtomBase::new(var(10), 0, 7);
        let target_a1 = VarAtomBase::new(var(11), 0, 2);
        let target_b0 = VarAtomBase::new(var(12), 0, 4);

        let initial_a0 = constant(&mut arena, 8, 0);
        let initial_a1 = constant(&mut arena, 3, 0);
        let update_a0 = indexed_input(&mut arena, external, loop_a, 8);
        let update_a1 = indexed_input(&mut arena, external, loop_a, 3);
        let group_a_states = vec![
            SLTForFoldGroupState {
                target: target_a0,
                initial: initial_a0,
                update: update_a0,
            },
            SLTForFoldGroupState {
                target: target_a1,
                initial: initial_a1,
                update: update_a1,
            },
        ];
        let group_a = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: loop_a,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 4,
                entry_guard: guard,
                states: group_a_states.clone(),
            })
            .unwrap();
        let (_, accesses_a) = packed_state_accesses(&group_a_states).unwrap();
        let projections_a = accesses_a
            .into_iter()
            .map(|access| {
                arena
                    .alloc(SLTNode::Slice {
                        expr: group_a,
                        access,
                    })
                    .unwrap()
            })
            .collect::<Vec<_>>();

        let initial_b0 = constant(&mut arena, 5, 0);
        let carried_b0 = input(&mut arena, target_b0.id, 5);
        let external_b0 = indexed_input(&mut arena, external, loop_b, 5);
        let update_b0 = arena
            .alloc(SLTNode::Binary(carried_b0, BinaryOp::Add, external_b0))
            .unwrap();
        let group_b_states = vec![SLTForFoldGroupState {
            target: target_b0,
            initial: initial_b0,
            update: update_b0,
        }];
        let group_b = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: loop_b,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 4,
                entry_guard: guard,
                states: group_b_states.clone(),
            })
            .unwrap();
        let projection_b = arena
            .alloc(SLTNode::Slice {
                expr: group_b,
                access: BitAccess::new(0, 4),
            })
            .unwrap();

        let sources_a = atoms(&[(external, 0, 127), (var(60), 0, 7)]);
        let sources_b = atoms(&[(external, 0, 127), (var(61), 0, 4)]);
        let address_a = atoms(&[(var(70), 0, 2)]);
        let address_b = atoms(&[(var(71), 0, 3)]);
        let order_a = [LogicPathId(1000)].into_iter().collect::<HashSet<_>>();
        let order_b = [LogicPathId(1001)].into_iter().collect::<HashSet<_>>();
        let paths = vec![
            path(
                target_a0,
                projections_a[0],
                sources_a.clone(),
                address_a.clone(),
                order_a.clone(),
            ),
            path(
                target_a1,
                projections_a[1],
                sources_a.clone(),
                address_a,
                order_a,
            ),
            path(
                target_b0,
                projection_b,
                sources_b.clone(),
                address_b,
                order_b,
            ),
        ];
        let mut store = SymbolicStore::default();
        install_store_value(&mut store, target_a0, projections_a[0], sources_a.clone());
        install_store_value(&mut store, target_a1, projections_a[1], sources_a);
        install_store_value(&mut store, target_b0, projection_b, sources_b);

        Fixture {
            arena,
            paths,
            store,
            old_groups: [group_a, group_b],
            targets: vec![target_a0, target_a1, target_b0],
            loop_vars: [loop_a, loop_b],
            external,
        }
    }

    fn two_lane_fixture(trip_count_b: usize, cross_lane_read: bool) -> Fixture {
        let mut arena = SLTNodeArena::new();
        let guard = constant(&mut arena, 1, 1);
        let external = var(50);
        let loop_a = var(100);
        let loop_b = var(101);
        let target_a = VarAtomBase::new(var(10), 0, 7);
        let target_b = VarAtomBase::new(var(11), 0, 7);
        let initial = constant(&mut arena, 8, 0);
        let update_a = indexed_input(&mut arena, external, loop_a, 8);
        let group_a = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: loop_a,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 4,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: target_a,
                    initial,
                    update: update_a,
                }],
            })
            .unwrap();
        let projection_a = arena
            .alloc(SLTNode::Slice {
                expr: group_a,
                access: BitAccess::new(0, 7),
            })
            .unwrap();

        let indexed_b = indexed_input(&mut arena, external, loop_b, 8);
        let update_b = if cross_lane_read {
            let read_a = input(&mut arena, target_a.id, 8);
            arena
                .alloc(SLTNode::Binary(indexed_b, BinaryOp::Add, read_a))
                .unwrap()
        } else {
            indexed_b
        };
        let group_b = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: loop_b,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: trip_count_b,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: target_b,
                    initial,
                    update: update_b,
                }],
            })
            .unwrap();
        let projection_b = arena
            .alloc(SLTNode::Slice {
                expr: group_b,
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        let sources = atoms(&[(external, 0, 127)]);
        let paths = vec![
            path(
                target_a,
                projection_a,
                sources.clone(),
                HashSet::default(),
                HashSet::default(),
            ),
            path(
                target_b,
                projection_b,
                sources.clone(),
                HashSet::default(),
                HashSet::default(),
            ),
        ];
        let mut store = SymbolicStore::default();
        install_store_value(&mut store, target_a, projection_a, sources.clone());
        install_store_value(&mut store, target_b, projection_b, sources);
        Fixture {
            arena,
            paths,
            store,
            old_groups: [group_a, group_b],
            targets: vec![target_a, target_b],
            loop_vars: [loop_a, loop_b],
            external,
        }
    }

    fn fused_group(fixture: &Fixture) -> NodeId {
        let groups = discover_recovered_fold_groups(&fixture.paths, &fixture.arena);
        assert_eq!(groups.len(), 1);
        groups[0].group
    }

    fn indexed_node_for_variable(
        root: NodeId,
        variable: VarId,
        arena: &SLTNodeArena<VarId>,
    ) -> Option<NodeId> {
        let mut visited = HashSet::default();
        let mut work = vec![root];
        while let Some(node) = work.pop() {
            if !visited.insert(node) {
                continue;
            }
            if let SLTNode::Input {
                variable: found,
                index,
                ..
            } = arena.get(node)
                && *found == variable
                && !index.is_empty()
            {
                return Some(index[0].node);
            }
            push_children(node, arena, &mut work);
        }
        None
    }

    #[test]
    fn p1_product_fuses_independent_groups_with_different_state_shapes() {
        let mut fixture = positive_fixture();

        fuse_recovered_fold_groups(
            &mut fixture.arena,
            &mut fixture.paths,
            &mut fixture.store,
            &[],
        )
        .unwrap();

        let group = fused_group(&fixture);
        let SLTNode::ForFoldGroup { states, .. } = fixture.arena.get(group) else {
            unreachable!()
        };
        assert_eq!(states.len(), 3);
        assert_eq!(
            states.iter().map(|state| state.target).collect::<Vec<_>>(),
            fixture.targets
        );
        let old_groups = fixture.old_groups.into_iter().collect();
        assert!(fixture.paths.iter().all(|path| !reaches_groups(
            path.expr,
            &fixture.arena,
            &old_groups
        )));

        let expected_sources =
            atoms(&[(fixture.external, 0, 127), (var(60), 0, 7), (var(61), 0, 4)]);
        let expected_address_sources = atoms(&[(var(70), 0, 2), (var(71), 0, 3)]);
        let expected_order = [LogicPathId(1000), LogicPathId(1001)]
            .into_iter()
            .collect::<HashSet<_>>();
        for path in &fixture.paths {
            assert_eq!(path.sources, expected_sources);
            assert_eq!(path.address_sources, expected_address_sources);
            assert_eq!(path.order_before, expected_order);
        }
    }

    #[test]
    fn p2_product_canonicalizes_loop_inputs_and_shared_indices() {
        let mut fixture = two_lane_fixture(4, false);
        fuse_recovered_fold_groups(
            &mut fixture.arena,
            &mut fixture.paths,
            &mut fixture.store,
            &[],
        )
        .unwrap();

        let group = fused_group(&fixture);
        let SLTNode::ForFoldGroup {
            loop_var, states, ..
        } = fixture.arena.get(group)
        else {
            unreachable!()
        };
        assert_eq!(*loop_var, fixture.loop_vars[0]);
        let first_index =
            indexed_node_for_variable(states[0].update, fixture.external, &fixture.arena).unwrap();
        let remapped_index =
            indexed_node_for_variable(states[1].update, fixture.external, &fixture.arena).unwrap();
        assert_eq!(first_index, remapped_index);
        assert_eq!(
            states[0].update, states[1].update,
            "canonical loop remapping must expose the complete indexed Input to arena CSE"
        );
        assert!(node_reads_variable(
            states[1].update,
            fixture.loop_vars[0],
            &fixture.arena
        ));
        assert!(!node_reads_variable(
            states[1].update,
            fixture.loop_vars[1],
            &fixture.arena
        ));
    }

    #[test]
    fn p3_product_rewrites_every_projection_and_symbolic_store_atom() {
        let mut fixture = positive_fixture();
        fuse_recovered_fold_groups(
            &mut fixture.arena,
            &mut fixture.paths,
            &mut fixture.store,
            &[],
        )
        .unwrap();

        let group = fused_group(&fixture);
        let expected = [
            BitAccess::new(8, 15),
            BitAccess::new(5, 7),
            BitAccess::new(0, 4),
        ];
        for ((path, target), access) in fixture.paths.iter().zip(&fixture.targets).zip(expected) {
            assert_eq!(path.target.var(), Some(target));
            assert_eq!(
                projection_access(path.expr, group, &fixture.arena),
                Some(access)
            );
            let (stored, _, _) = fixture.store[&target.id].ranges.get(&0).unwrap();
            let (stored, stored_sources) = stored.as_ref().unwrap();
            assert_eq!(*stored, path.expr);
            assert_eq!(stored_sources, &path.sources);
        }
    }

    #[test]
    fn p4_later_comb_store_overwrite_does_not_block_path_fusion() {
        let mut fixture = positive_fixture();
        for target in &fixture.targets {
            let width = target.access.msb - target.access.lsb + 1;
            fixture
                .store
                .insert(target.id, RangeStore::new(None, width));
        }

        fuse_recovered_fold_groups(
            &mut fixture.arena,
            &mut fixture.paths,
            &mut fixture.store,
            &[],
        )
        .unwrap();

        let group = fused_group(&fixture);
        assert!(matches!(
            fixture.arena.get(group),
            SLTNode::ForFoldGroup { states, .. } if states.len() == 3
        ));
        for target in &fixture.targets {
            let (stored, _, _) = fixture.store[&target.id].ranges.get(&0).unwrap();
            assert!(stored.is_none());
        }
    }

    #[test]
    fn p6_equal_previous_only_temporal_classes_fuse_without_address_sources() {
        let mut fixture = two_lane_fixture(4, false);
        let previous = atoms(&[(fixture.external, 0, 127)]);
        for path in &mut fixture.paths {
            path.previous_sources = previous.clone();
            path.address_sources.clear();
        }

        fuse_recovered_fold_groups(
            &mut fixture.arena,
            &mut fixture.paths,
            &mut fixture.store,
            &[],
        )
        .unwrap();

        let group = fused_group(&fixture);
        assert!(matches!(
            fixture.arena.get(group),
            SLTNode::ForFoldGroup { states, .. } if states.len() == 2
        ));
        assert!(
            fixture.paths.iter().all(|path| {
                path.previous_sources == previous && path.address_sources.is_empty()
            })
        );
    }

    #[test]
    fn p5_fused_projections_lower_atomically_in_one_counted_loop() {
        let mut fixture = positive_fixture();
        fuse_recovered_fold_groups(
            &mut fixture.arena,
            &mut fixture.paths,
            &mut fixture.store,
            &[],
        )
        .unwrap();

        let mut widths = HashMap::default();
        for target in &fixture.targets {
            widths.insert(target.id, target.access.msb - target.access.lsb + 1);
        }
        widths.insert(fixture.external, 128);
        widths.insert(var(60), 8);
        widths.insert(var(61), 5);
        widths.insert(var(70), 3);
        widths.insert(var(71), 4);
        let result = super::super::scheduler::sort(
            fixture.paths,
            &fixture.arena,
            &HashSet::default(),
            &HashMap::default(),
            false,
            &widths,
            1,
        )
        .unwrap();
        assert_eq!(result.execution_units.len(), 1);
        let eu = &result.execution_units[0];
        assert_eq!(
            eu.blocks
                .values()
                .filter(|block| matches!(block.terminator, SIRTerminator::Branch { .. }))
                .count(),
            2
        );
        let store_block = eu
            .blocks
            .values()
            .find(|block| {
                block
                    .instructions
                    .iter()
                    .filter(|instruction| matches!(instruction, SIRInstruction::Store(..)))
                    .count()
                    == 3
            })
            .expect("all fused projections are stored from one materialization block");
        let first_store = store_block
            .instructions
            .iter()
            .position(|instruction| matches!(instruction, SIRInstruction::Store(..)))
            .unwrap();
        for value in store_block
            .instructions
            .iter()
            .filter_map(|instruction| match instruction {
                SIRInstruction::Store(_, _, _, value, _, _) => Some(*value),
                _ => None,
            })
        {
            let definition = store_block
                .instructions
                .iter()
                .position(|instruction| match instruction {
                    SIRInstruction::Binary(dst, ..)
                    | SIRInstruction::Unary(dst, ..)
                    | SIRInstruction::Slice(dst, ..)
                    | SIRInstruction::Concat(dst, ..)
                    | SIRInstruction::Mux(dst, ..) => *dst == value,
                    _ => false,
                })
                .expect("stored projection has a local definition");
            assert!(definition < first_store);
        }
    }

    #[test]
    fn n1_different_iteration_domains_do_not_fuse() {
        let mut fixture = two_lane_fixture(5, false);
        let original_paths = fixture.paths.clone();
        let original_store = fixture.store.clone();
        let original_len = fixture.arena.len();

        fuse_recovered_fold_groups(
            &mut fixture.arena,
            &mut fixture.paths,
            &mut fixture.store,
            &[],
        )
        .unwrap();

        assert_eq!(fixture.paths, original_paths);
        assert_eq!(fixture.store, original_store);
        assert_eq!(fixture.arena.len(), original_len);
        assert_eq!(
            discover_recovered_fold_groups(&fixture.paths, &fixture.arena).len(),
            2
        );
    }

    #[test]
    fn n3_cross_group_state_read_does_not_fuse() {
        let mut fixture = two_lane_fixture(4, true);
        let original_paths = fixture.paths.clone();
        let original_store = fixture.store.clone();
        let original_len = fixture.arena.len();

        fuse_recovered_fold_groups(
            &mut fixture.arena,
            &mut fixture.paths,
            &mut fixture.store,
            &[],
        )
        .unwrap();

        assert_eq!(fixture.paths, original_paths);
        assert_eq!(fixture.store, original_store);
        assert_eq!(fixture.arena.len(), original_len);
    }

    #[test]
    fn n4_previous_only_and_previous_address_both_do_not_fuse() {
        let mut fixture = two_lane_fixture(4, false);
        let previous = atoms(&[(fixture.external, 0, 127)]);
        for path in &mut fixture.paths {
            path.previous_sources = previous.clone();
            path.address_sources.clear();
        }
        fixture.paths[0].address_sources = previous;
        let original_paths = fixture.paths.clone();
        let original_store = fixture.store.clone();
        let original_len = fixture.arena.len();

        fuse_recovered_fold_groups(
            &mut fixture.arena,
            &mut fixture.paths,
            &mut fixture.store,
            &[],
        )
        .unwrap();

        assert_eq!(fixture.paths, original_paths);
        assert_eq!(fixture.store, original_store);
        assert_eq!(fixture.arena.len(), original_len);
    }

    #[test]
    fn n7_missing_state_projection_rejects_the_whole_family() {
        let mut fixture = positive_fixture();
        fixture.paths.remove(1);
        let original_paths = fixture.paths.clone();
        let original_store = fixture.store.clone();
        let original_len = fixture.arena.len();

        fuse_recovered_fold_groups(
            &mut fixture.arena,
            &mut fixture.paths,
            &mut fixture.store,
            &[],
        )
        .unwrap();

        assert_eq!(fixture.paths, original_paths);
        assert_eq!(fixture.store, original_store);
        assert_eq!(fixture.arena.len(), original_len);
    }

    #[test]
    fn n8_existing_store_root_requires_the_exact_state_projection_interval() {
        let mut fixture = positive_fixture();
        let target = fixture.targets[0];
        let wrong_projection = fixture
            .arena
            .alloc(SLTNode::Slice {
                expr: fixture.old_groups[0],
                access: BitAccess::new(0, 7),
            })
            .unwrap();
        install_store_value(
            &mut fixture.store,
            target,
            wrong_projection,
            fixture.paths[0].sources.clone(),
        );
        let original_paths = fixture.paths.clone();
        let original_store = fixture.store.clone();
        let original_len = fixture.arena.len();

        fuse_recovered_fold_groups(
            &mut fixture.arena,
            &mut fixture.paths,
            &mut fixture.store,
            &[],
        )
        .unwrap();

        assert_eq!(fixture.paths, original_paths);
        assert_eq!(fixture.store, original_store);
        assert_eq!(fixture.arena.len(), original_len);
    }

    #[test]
    fn n12_failed_semantic_root_preflight_is_atomic() {
        let mut fixture = positive_fixture();
        fixture.paths[0].pre_lower_nodes.push(fixture.old_groups[0]);
        let original_paths = fixture.paths.clone();
        let original_store = fixture.store.clone();
        let original_len = fixture.arena.len();

        fuse_recovered_fold_groups(
            &mut fixture.arena,
            &mut fixture.paths,
            &mut fixture.store,
            &[],
        )
        .unwrap();

        assert_eq!(fixture.paths, original_paths);
        assert_eq!(fixture.store, original_store);
        assert_eq!(fixture.arena.len(), original_len);
        assert_eq!(
            projected_group(fixture.paths[0].expr, &fixture.arena),
            Some(fixture.old_groups[0])
        );
    }
}
