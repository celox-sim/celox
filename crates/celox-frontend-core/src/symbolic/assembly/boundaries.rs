//! Bit boundaries of variables, propagated through the hierarchy.

use super::*;

pub(super) fn extend_boundaries(
    boundaries: &mut HashMap<AbsoluteAddr, BTreeSet<usize>>,
    source: AbsoluteAddr,
    target: AbsoluteAddr,
) -> bool {
    if source == target || boundaries.get(&source).is_none_or(BTreeSet::is_empty) {
        return false;
    }
    boundaries.entry(target).or_default();
    let [Some(source), Some(target)] = boundaries.get_disjoint_mut([&source, &target]) else {
        unreachable!("distinct boundary keys were inserted before lookup");
    };
    let old_len = target.len();
    target.extend(source.iter().copied());
    target.len() != old_len
}

/// Add the boundaries of `source` within its range to `target`, mapping the
/// range of `source` onto the equally wide range of `target`.
pub(super) fn extend_shifted_boundaries(
    boundaries: &mut HashMap<AbsoluteAddr, BTreeSet<usize>>,
    (source, source_access): (AbsoluteAddr, BitAccess),
    (target, target_access): (AbsoluteAddr, BitAccess),
) -> bool {
    let shifted: Vec<usize> = boundaries
        .get(&source)
        .into_iter()
        .flatten()
        .filter(|&&boundary| source_access.lsb <= boundary && boundary <= source_access.msb + 1)
        .map(|&boundary| boundary - source_access.lsb + target_access.lsb)
        .collect();
    if shifted.is_empty() || (source == target && source_access == target_access) {
        return false;
    }
    let target = boundaries.entry(target).or_default();
    let old_len = target.len();
    target.extend(shifted);
    target.len() != old_len
}

pub(super) fn propagate_boundaries(
    bindings: &PackageBindings,
    expanded: &HashMap<InstancePath, InstanceId>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
) -> HashMap<AbsoluteAddr, BTreeSet<usize>> {
    let mut current_boundaries = HashMap::default();

    // Initialize with local boundaries
    for id in expanded.values() {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];
        for (var_id, boundaries) in &sim_module.comb_boundaries {
            // Every alias of a package variable adds its boundaries.
            let addr = bindings.locate(*id, *var_id);
            current_boundaries
                .entry(addr)
                .or_insert_with(BTreeSet::new)
                .extend(boundaries.iter().copied());
        }
    }

    // Propagate boundaries
    let mut changed = true;
    while changed {
        changed = false;
        for (path, id) in expanded {
            let module_id = &instance_modules[id];
            let sim_module = &modules[module_id];

            for (inst_name, glue_blocks) in &sim_module.glue_blocks {
                for (idx, glue_block) in glue_blocks.iter().enumerate() {
                    let mut child_path = path.0.clone();
                    child_path.push((inst_name.clone(), sim_module.instance_index(inst_name, idx)));
                    let child_id = expanded[&InstancePath(child_path)];

                    // Propagate from Parent to Child (Input Ports)
                    for (parent_vars, child_addr) in &glue_block.input_ports {
                        if let Some(target) = child_addr.target.var()
                            && let GlueAddr::Child(child_var_id) = target.id
                        {
                            let child_abs = bindings.locate(child_id, child_var_id);

                            // Collect boundaries from all parent variables connected to this port
                            for parent_var in parent_vars {
                                let parent_abs = bindings.locate(*id, *parent_var);
                                changed |= extend_boundaries(
                                    &mut current_boundaries,
                                    parent_abs,
                                    child_abs,
                                );
                            }

                            // Child -> Parent: where the child reads the
                            // port in parts, the connected range of a parent
                            // variable is read in the same parts, as in
                            // `a = {x[7:4], ...}` within one module. Only a
                            // connection of one range as wide as the port maps
                            // bit for bit.
                            let mut sources = child_addr.sources.iter();
                            if let (Some(source), None) = (sources.next(), sources.next())
                                && let GlueAddr::Parent(parent_var) = source.id
                                && source.access.msb - source.access.lsb
                                    == target.access.msb - target.access.lsb
                            {
                                let parent_abs = bindings.locate(*id, parent_var);
                                changed |= extend_shifted_boundaries(
                                    &mut current_boundaries,
                                    (child_abs, target.access),
                                    (parent_abs, source.access),
                                );
                            }
                        }
                    }

                    // Propagate from Child to Parent (Output Ports)
                    for (parent_vars, logic_path) in &glue_block.output_ports {
                        // logic_path.target is Parent. logic_path.sources contains Child.
                        for source in &logic_path.sources {
                            if let GlueAddr::Child(child_var_id) = source.id {
                                let child_abs = bindings.locate(child_id, child_var_id);

                                // Child -> Parent
                                for parent_var in parent_vars {
                                    let parent_abs = bindings.locate(*id, *parent_var);
                                    changed |= extend_boundaries(
                                        &mut current_boundaries,
                                        child_abs,
                                        parent_abs,
                                    );
                                }

                                // Parent -> Child (Sink -> Source propagation)
                                // If the parent wire connected to this output has boundaries (e.g. used in slices),
                                // those boundaries should propagate to the child output port so it drives them appropriately.
                                for parent_var in parent_vars {
                                    let parent_abs = bindings.locate(*id, *parent_var);
                                    changed |= extend_boundaries(
                                        &mut current_boundaries,
                                        parent_abs,
                                        child_abs,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    current_boundaries
}
