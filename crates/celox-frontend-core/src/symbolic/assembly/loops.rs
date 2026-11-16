//! Loop options resolved to addresses, and the source locations of scheduler errors.

use super::*;

pub(super) fn create_absolute_addr(
    instance_path: &[(String, usize)],
    var_path: &[String],
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    expanded: &HashMap<InstancePath, InstanceId>,
) -> AbsoluteAddr {
    let instance_path = InstancePath(instance_path.to_vec());
    let instance_id = expanded[&instance_path];
    let module_id = instance_modules[&instance_id];
    let module = &modules[&module_id];
    let var_id = *module
        .variables
        .iter()
        .find(|(_, variable)| variable.path == var_path)
        .unwrap()
        .0;
    AbsoluteAddr {
        instance_id,
        var_id,
    }
}

pub(super) fn parse_ignored_loops(
    ignored_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )],
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    expanded: &HashMap<InstancePath, InstanceId>,
) -> HashSet<(AbsoluteAddr, AbsoluteAddr)> {
    let mut res = HashSet::default();

    for ((from_instance_path, from_var_path), (to_instance_path, to_var_path)) in ignored_loops {
        let from = create_absolute_addr(
            from_instance_path,
            from_var_path,
            instance_modules,
            modules,
            expanded,
        );
        let to = create_absolute_addr(
            to_instance_path,
            to_var_path,
            instance_modules,
            modules,
            expanded,
        );
        res.insert((from, to));
    }
    res
}

pub(super) fn parse_true_loops(
    true_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )],
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    expanded: &HashMap<InstancePath, InstanceId>,
) -> HashMap<(AbsoluteAddr, AbsoluteAddr), usize> {
    let mut res = HashMap::default();

    for ((from_instance_path, from_var_path), (to_instance_path, to_var_path), max_iter) in
        true_loops
    {
        let from = create_absolute_addr(
            from_instance_path,
            from_var_path,
            instance_modules,
            modules,
            expanded,
        );
        let to = create_absolute_addr(
            to_instance_path,
            to_var_path,
            instance_modules,
            modules,
            expanded,
        );
        res.insert((from, to), *max_iter);
    }
    res
}

pub(super) fn scheduler_source_locations(
    error: &SchedulerError<AbsoluteAddr>,
    modules: &HashMap<ModuleId, SimModule>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
) -> Vec<SourceLocation> {
    let blocks = match error {
        SchedulerError::CombinationalLoop { blocks } => blocks,
        SchedulerError::MultipleDriver { blocks } => blocks,
        SchedulerError::InvalidDependencyGraph => return Vec::new(),
    };
    let mut seen = HashSet::default();
    blocks
        .iter()
        .filter_map(|block| {
            let addr = block.target.var()?.id;
            if !seen.insert(addr) {
                return None;
            }
            let module_id = instance_modules.get(&addr.instance_id)?;
            let module = modules.get(module_id)?;
            let var = module.variables.get(&addr.var_id)?;
            var.source.clone()
        })
        .collect()
}
