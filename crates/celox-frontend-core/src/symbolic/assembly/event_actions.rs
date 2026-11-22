//! Source actions grouped by the event that activates them.

use super::*;

pub(super) fn build_event_actions(
    bindings: &PackageBindings,
    modules: &HashMap<ModuleId, SimModule>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    clock_domains: &HashMap<AbsoluteAddr, AbsoluteAddr>,
    runtime_relocations: &HashMap<InstanceId, FfRuntimeRelocation>,
) -> HashMap<AbsoluteAddr, Vec<EventAction>> {
    let mut instances = instance_modules.iter().collect::<Vec<_>>();
    instances.sort_unstable_by_key(|(instance, _)| instance.0);
    let mut result = HashMap::<AbsoluteAddr, Vec<EventAction>>::default();
    let mut uncovered = HashSet::default();
    let mut next_action_id = 0usize;

    for (&instance_id, &module_id) in instances {
        let module = &modules[&module_id];
        // A source-specific schedule can replace an entry only when it owns
        // every action on that clock, including actions reached by reset aliases.
        for trigger in module
            .event_bodies
            .keys()
            .filter(|t| !module.ff_access_summaries.contains_key(*t))
        {
            for signal in std::iter::once(&trigger.clock).chain(&trigger.resets) {
                let addr = bindings.locate(instance_id, *signal);
                uncovered.insert(clock_domains.get(&addr).copied().unwrap_or(addr));
            }
        }
        let mut summaries = module.ff_access_summaries.iter().collect::<Vec<_>>();
        summaries.sort_unstable_by_key(|(trigger, _)| (*trigger).clone());
        for (trigger, summary) in summaries {
            let relocate = |address: RegionedVarAddr| {
                bindings.locate_regioned(address.region, instance_id, address.var_id)
            };
            let summary = FfAccessSummary {
                reads: summary
                    .reads
                    .iter()
                    .map(|read| VarAtomBase {
                        id: relocate(read.id),
                        access: read.access,
                    })
                    .collect(),
                writes: summary
                    .writes
                    .iter()
                    .map(|write| VarAtomBase {
                        id: bindings.locate_regioned(STABLE_REGION, instance_id, write.id.var_id),
                        access: write.access,
                    })
                    .collect(),
                dynamic_writes: summary
                    .dynamic_writes
                    .iter()
                    .map(|address| {
                        bindings.locate_regioned(STABLE_REGION, instance_id, address.var_id)
                    })
                    .collect(),
            };
            let action = EventAction {
                id: next_action_id,
                instance_id,
                module_id,
                trigger: trigger.clone(),
                summary,
                runtime: runtime_relocations[&instance_id].clone(),
            };
            next_action_id += 1;
            let clock = bindings.locate(instance_id, trigger.clock);
            let clock = clock_domains.get(&clock).copied().unwrap_or(clock);
            result.entry(clock).or_default().push(action.clone());
            for &reset_id in &trigger.resets {
                let reset = bindings.locate(instance_id, reset_id);
                let reset = clock_domains.get(&reset).copied().unwrap_or(reset);
                result.entry(reset).or_default().push(action.clone());
            }
        }
    }
    result.retain(|event, _| !uncovered.contains(event));
    result
}
