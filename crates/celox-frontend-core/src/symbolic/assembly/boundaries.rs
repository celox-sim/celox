//! Bit boundaries of variables, propagated through the hierarchy.

use super::*;

/// How a boundary of the source of a [`BoundaryEdge`] maps onto its target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BoundaryMap {
    /// Every boundary, unchanged.
    All,
    /// The boundaries within `source` (its `lsb` through `msb + 1`), moved
    /// onto the equally wide range `target`.
    Shifted {
        source: BitAccess,
        target: BitAccess,
    },
}

impl BoundaryMap {
    fn apply(self, boundary: usize) -> Option<usize> {
        match self {
            BoundaryMap::All => Some(boundary),
            BoundaryMap::Shifted { source, target } => (source.lsb <= boundary
                && boundary <= source.msb + 1)
                .then(|| boundary - source.lsb + target.lsb),
        }
    }
}

/// One constraint of the propagation: the boundaries of `source`, mapped by
/// `map`, are boundaries of `target`.
#[derive(Clone, Copy, Debug)]
struct BoundaryEdge {
    source: AbsoluteAddr,
    target: AbsoluteAddr,
    map: BoundaryMap,
}

/// The constraints the port connections of every instance put on the
/// boundaries of the variables they connect.
fn boundary_edges(
    bindings: &PackageBindings,
    expanded: &HashMap<InstancePath, InstanceId>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
) -> Vec<BoundaryEdge> {
    let mut edges = Vec::new();
    let mut push = |source: AbsoluteAddr, target: AbsoluteAddr, map: BoundaryMap| {
        // A variable onto itself, bit for bit, adds nothing.
        let identity = match map {
            BoundaryMap::All => true,
            BoundaryMap::Shifted { source, target } => source == target,
        };
        if !(source == target && identity) {
            edges.push(BoundaryEdge {
                source,
                target,
                map,
            });
        }
    };
    for (path, id) in expanded {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];
        for (inst_name, glue_blocks) in &sim_module.glue_blocks {
            for (idx, glue_block) in glue_blocks.iter().enumerate() {
                let mut child_path = path.0.clone();
                child_path.push((inst_name.clone(), sim_module.instance_index(inst_name, idx)));
                let child_id = expanded[&InstancePath(child_path)];

                // Input ports.
                for (parent_vars, child_addr) in &glue_block.input_ports {
                    if let Some(target) = child_addr.target.var()
                        && let GlueAddr::Child(child_var_id) = target.id
                    {
                        let child_abs = bindings.locate(child_id, child_var_id);
                        // Parent -> child: the boundaries of every parent
                        // variable connected to the port.
                        for parent_var in parent_vars {
                            push(
                                bindings.locate(*id, *parent_var),
                                child_abs,
                                BoundaryMap::All,
                            );
                        }
                        // Child -> parent: where the child reads the port in
                        // parts, the connected range of a parent variable is
                        // read in the same parts, as in `a = {x[7:4], ...}`
                        // within one module. Only a connection of one range
                        // as wide as the port maps bit for bit.
                        let mut sources = child_addr.sources.iter();
                        if let (Some(source), None) = (sources.next(), sources.next())
                            && let GlueAddr::Parent(parent_var) = source.id
                            && source.access.msb - source.access.lsb
                                == target.access.msb - target.access.lsb
                        {
                            push(
                                child_abs,
                                bindings.locate(*id, parent_var),
                                BoundaryMap::Shifted {
                                    source: target.access,
                                    target: source.access,
                                },
                            );
                        }
                    }
                }

                // Output ports: the target is the parent, the sources are
                // the child.
                for (parent_vars, logic_path) in &glue_block.output_ports {
                    for source in &logic_path.sources {
                        if let GlueAddr::Child(child_var_id) = source.id {
                            let child_abs = bindings.locate(child_id, child_var_id);
                            for parent_var in parent_vars {
                                let parent_abs = bindings.locate(*id, *parent_var);
                                // Child -> parent.
                                push(child_abs, parent_abs, BoundaryMap::All);
                                // Parent -> child: where the parent wire is
                                // used in slices, the child output port
                                // drives the same parts.
                                push(parent_abs, child_abs, BoundaryMap::All);
                            }
                        }
                    }
                }
            }
        }
    }
    edges
}

/// The least boundary sets that contain `initial` and satisfy every edge.
///
/// A worklist moves only the boundaries a variable gained since it was last
/// visited along its edges, so every boundary crosses every edge at most once.
fn solve_boundaries(
    mut boundaries: HashMap<AbsoluteAddr, BTreeSet<usize>>,
    edges: &[BoundaryEdge],
) -> HashMap<AbsoluteAddr, BTreeSet<usize>> {
    let mut outgoing: HashMap<AbsoluteAddr, Vec<(AbsoluteAddr, BoundaryMap)>> = HashMap::default();
    for edge in edges {
        outgoing
            .entry(edge.source)
            .or_default()
            .push((edge.target, edge.map));
    }
    // The boundaries each queued variable gained since it was last visited.
    let mut gained: HashMap<AbsoluteAddr, Vec<usize>> = HashMap::default();
    let mut queue = std::collections::VecDeque::new();
    for (addr, set) in &boundaries {
        if !set.is_empty() && outgoing.contains_key(addr) {
            gained.insert(*addr, set.iter().copied().collect());
            queue.push_back(*addr);
        }
    }
    while let Some(source) = queue.pop_front() {
        let delta = gained.remove(&source).unwrap_or_default();
        for &(target, map) in outgoing.get(&source).into_iter().flatten() {
            let mut added = Vec::new();
            for boundary in delta.iter().filter_map(|&boundary| map.apply(boundary)) {
                if boundaries
                    .get(&target)
                    .is_none_or(|set| !set.contains(&boundary))
                {
                    added.push(boundary);
                }
            }
            if added.is_empty() {
                continue;
            }
            boundaries
                .entry(target)
                .or_default()
                .extend(added.iter().copied());
            if !outgoing.contains_key(&target) {
                continue;
            }
            match gained.entry(target) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    entry.get_mut().extend(added);
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(added);
                    queue.push_back(target);
                }
            }
        }
    }
    boundaries
}

pub(super) fn propagate_boundaries(
    bindings: &PackageBindings,
    expanded: &HashMap<InstancePath, InstanceId>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
) -> HashMap<AbsoluteAddr, BTreeSet<usize>> {
    let mut initial = HashMap::default();
    for id in expanded.values() {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];
        for (var_id, boundaries) in &sim_module.comb_boundaries {
            // Every alias of a package variable adds its boundaries.
            let addr = bindings.locate(*id, *var_id);
            initial
                .entry(addr)
                .or_insert_with(BTreeSet::new)
                .extend(boundaries.iter().copied());
        }
    }
    let edges = boundary_edges(bindings, expanded, instance_modules, modules);
    solve_boundaries(initial, &edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(var: u32) -> AbsoluteAddr {
        AbsoluteAddr {
            instance_id: InstanceId(var as usize % 3),
            var_id: SourceVarId(var),
        }
    }

    /// The propagation the worklist replaced: apply every edge to every
    /// boundary of its source until no edge adds one.
    fn reference(
        mut boundaries: HashMap<AbsoluteAddr, BTreeSet<usize>>,
        edges: &[BoundaryEdge],
    ) -> HashMap<AbsoluteAddr, BTreeSet<usize>> {
        let mut changed = true;
        while changed {
            changed = false;
            for edge in edges {
                let mapped: Vec<usize> = boundaries
                    .get(&edge.source)
                    .into_iter()
                    .flatten()
                    .filter_map(|&boundary| edge.map.apply(boundary))
                    .collect();
                if mapped.is_empty() {
                    continue;
                }
                let target = boundaries.entry(edge.target).or_default();
                let old_len = target.len();
                target.extend(mapped);
                changed |= target.len() != old_len;
            }
        }
        boundaries
    }

    #[test]
    fn worklist_matches_the_fixpoint_iteration() {
        // A small deterministic generator: cyclic, shifted and self edges.
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = |bound: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % bound as u64) as usize
        };
        for _ in 0..200 {
            let vars = 2 + next(12);
            let mut initial = HashMap::default();
            for _ in 0..next(6) {
                let set: BTreeSet<usize> = (0..1 + next(4)).map(|_| next(40)).collect();
                initial
                    .entry(addr(next(vars) as u32))
                    .or_insert_with(BTreeSet::new)
                    .extend(set);
            }
            // An initially empty set stays a key.
            initial.entry(addr(next(vars) as u32)).or_default();
            let edges: Vec<BoundaryEdge> = (0..next(3 * vars))
                .map(|_| {
                    let map = if next(2) == 0 {
                        BoundaryMap::All
                    } else {
                        let width = next(16);
                        let source = next(24);
                        let target = next(24);
                        BoundaryMap::Shifted {
                            source: BitAccess::new(source, source + width),
                            target: BitAccess::new(target, target + width),
                        }
                    };
                    BoundaryEdge {
                        source: addr(next(vars) as u32),
                        target: addr(next(vars) as u32),
                        map,
                    }
                })
                .collect();
            assert_eq!(
                solve_boundaries(initial.clone(), &edges),
                reference(initial, &edges)
            );
        }
    }
}
