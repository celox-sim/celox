//! Flip-flop actions offered to an adapter for fused lowering.

use super::*;

pub(super) fn build_fused_ff_actions(
    bindings: &PackageBindings,
    modules: &HashMap<ModuleId, SimModule>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    clock_domains: &HashMap<AbsoluteAddr, AbsoluteAddr>,
    runtime_relocations: &HashMap<InstanceId, FfRuntimeRelocation>,
) -> HashMap<AbsoluteAddr, Vec<FusedFfAction>> {
    let mut instances = instance_modules.iter().collect::<Vec<_>>();
    instances.sort_unstable_by_key(|(instance, _)| instance.0);
    let mut result = HashMap::<AbsoluteAddr, Vec<FusedFfAction>>::default();
    let mut next_action_id = 0usize;

    for (&instance_id, &module_id) in instances {
        let module = &modules[&module_id];
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
            let action = FusedFfAction {
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
    result
}
