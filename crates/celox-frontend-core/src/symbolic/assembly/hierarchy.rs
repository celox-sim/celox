//! The expanded instance hierarchy and the variables of its modules.

use super::*;

pub(super) fn module_variables(
    modules: &HashMap<ModuleId, SimModule>,
) -> (
    HashMap<ModuleId, HashMap<SourceVarId, VariableInfo>>,
    HashMap<ModuleId, HashMap<Vec<String>, Option<SourceVarId>>>,
) {
    let mut res = HashMap::default();
    let mut path_index = HashMap::default();
    for (id, module) in modules {
        let mut variables = HashMap::default();
        let mut paths: HashMap<Vec<String>, Option<SourceVarId>> = HashMap::default();
        for (&source_id, variable) in &module.variables {
            // A package variable belongs to the package's instance, and a
            // hierarchically named one to its own instance.
            if module.package_bindings.contains_key(&source_id)
                || module.hierarchical_bindings.contains_key(&source_id)
            {
                continue;
            }
            // Only module-scope variables are externally addressable. Locals
            // may share the same VarPath, but must not make a legal
            // hierarchical or public lookup appear ambiguous.
            if variable.module_affiliated {
                match paths.entry(variable.path.clone()) {
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(Some(source_id));
                    }
                    std::collections::hash_map::Entry::Occupied(mut e) => {
                        // Duplicate visible VarPath — mark as ambiguous.
                        e.insert(None);
                    }
                }
            }
            variables.insert(
                source_id,
                VariableInfo {
                    id: source_id,
                    path: variable.path.clone(),
                    var_kind: variable.kind,
                    signed: variable.signed,
                    packed_dims: variable.packed_dims.clone(),
                    unpacked_ranges: variable.unpacked_ranges.clone(),
                    metadata: variable.metadata.clone(),
                },
            );
        }
        res.insert(*id, variables);
        path_index.insert(*id, paths);
    }
    (res, path_index)
}

/// The parent of every expanded instance, indexed by instance id, with
/// `usize::MAX` for the root.
pub(super) fn instance_parents(expanded: &HashMap<InstancePath, InstanceId>) -> Vec<usize> {
    let count = expanded
        .values()
        .map(|instance| instance.0 + 1)
        .max()
        .unwrap_or(0);
    let mut parents = vec![usize::MAX; count];
    for (path, instance) in expanded {
        if let Some((_, parent_path)) = path.0.split_last()
            && let Some(parent) = expanded.get(&InstancePath(parent_path.to_vec()))
        {
            parents[instance.0] = parent.0;
        }
    }
    parents
}

pub(super) fn expand_hierarchy(
    top: &ModuleId,
    modules: &HashMap<ModuleId, SimModule>,
) -> (
    HashMap<InstancePath, InstanceId>,
    HashMap<InstanceId, ModuleId>,
    HashSet<InstanceId>,
) {
    let mut expanded = HashMap::default();
    let mut instance_modules = HashMap::default();
    let mut indexed_instances = HashSet::default();
    let mut instance_id = 0;
    let path = vec![];
    let id = InstanceId(instance_id);
    instance_modules.insert(id, *top);
    expanded.insert(InstancePath(path.clone()), id);
    instance_id += 1;
    expand(
        top,
        path,
        modules,
        &mut expanded,
        &mut instance_modules,
        &mut indexed_instances,
        &mut instance_id,
    );
    (expanded, instance_modules, indexed_instances)
}

pub(super) fn expand(
    target: &ModuleId,
    path: Vec<(String, usize)>,
    modules: &HashMap<ModuleId, SimModule>,
    expanded: &mut HashMap<InstancePath, InstanceId>,
    instance_modules: &mut HashMap<InstanceId, ModuleId>,
    indexed_instances: &mut HashSet<InstanceId>,
    instance_id: &mut usize,
) {
    let module = &modules[target];
    for (inst_name, gbs) in &module.glue_blocks {
        // Generate loops can elaborate several scalar declarations under the
        // same flattened name. Keep those scopes distinct until generate
        // hierarchy segments become part of InstancePath.
        let indexed = module.indexed_instance_names.contains(inst_name) || gbs.len() > 1;
        for (idx, gb) in gbs.iter().enumerate() {
            let mut path = path.clone();
            path.push((inst_name.clone(), module.instance_index(inst_name, idx)));
            let id = InstanceId(*instance_id);
            expanded.insert(InstancePath(path.clone()), id);
            instance_modules.insert(id, gb.module_id);
            if indexed {
                indexed_instances.insert(id);
            }
            *instance_id += 1;
            expand(
                &gb.module_id,
                path,
                modules,
                expanded,
                instance_modules,
                indexed_instances,
                instance_id,
            );
        }
    }
}
