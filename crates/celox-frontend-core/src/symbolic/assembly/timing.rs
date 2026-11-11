//! Timing metadata of the whole design.

use super::*;

/// Counter slots, wait sites, nonblocking sites and time slots of the whole
/// design.
pub(super) struct TimingMetadata {
    pub(super) counter_slots: Vec<CounterSlots<AbsoluteAddr>>,
    pub(super) pending_slots: Vec<crate::watch::PendingSlots<AbsoluteAddr>>,
    pub(super) wait_sites: Vec<WaitSite<AbsoluteAddr>>,
    pub(super) nba_sites: Vec<NbaSite<AbsoluteAddr>>,
    pub(super) host_request_sites: Vec<HostRequestSite<AbsoluteAddr>>,
    pub(super) time_slots: Vec<AbsoluteAddr>,
    pub(super) sample_shadows: Vec<(AbsoluteAddr, AbsoluteAddr)>,
    pub(super) initial_copies: Vec<(AbsoluteAddr, AbsoluteAddr)>,
    pub(super) nba_switches: Vec<NbaSwitch<AbsoluteAddr>>,
}

/// Place the timing metadata of every instance. Sites name the global index
/// of their process; a kernel numbers its sites in the order they appear
/// here, which is the module's order.
pub(super) fn relocate_timing(
    bindings: &PackageBindings,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    processes: &[RelocatedProcess],
) -> TimingMetadata {
    let process_index: HashMap<(InstanceId, usize), usize> = processes
        .iter()
        .enumerate()
        .map(|(index, process)| (process.key, index))
        .collect();
    let mut instances = instance_modules.iter().collect::<Vec<_>>();
    instances.sort_unstable_by_key(|(instance, _)| **instance);
    let mut metadata = TimingMetadata {
        counter_slots: Vec::new(),
        pending_slots: Vec::new(),
        wait_sites: Vec::new(),
        nba_sites: Vec::new(),
        host_request_sites: Vec::new(),
        time_slots: Vec::new(),
        sample_shadows: Vec::new(),
        initial_copies: Vec::new(),
        nba_switches: Vec::new(),
    };
    for (&instance, module) in instances {
        let module = &modules[module];
        let locate = |var_id| bindings.locate(instance, var_id);
        metadata
            .counter_slots
            .extend(module.counter_slots.iter().map(|slots| slots.map(locate)));
        metadata
            .pending_slots
            .extend(module.pending_slots.iter().map(|slots| slots.map(locate)));
        metadata
            .wait_sites
            .extend(module.wait_sites.iter().map(|site| WaitSite {
                process: process_index[&(instance, site.process)],
                ..site.map(locate)
            }));
        metadata
            .nba_sites
            .extend(module.nba_sites.iter().map(|site| NbaSite {
                process: process_index[&(instance, site.process)],
                ..site.map(locate)
            }));
        metadata
            .host_request_sites
            .extend(module.host_request_sites.iter().map(|site| {
                HostRequestSite {
                    process: site
                        .process
                        .with_index(process_index[&(instance, site.process.index())]),
                    ..site.map(locate)
                }
            }));
        metadata.time_slots.extend(module.time_slot.map(locate));
        metadata.sample_shadows.extend(
            module
                .sample_shadows
                .iter()
                .map(|&(variable, copy)| (locate(variable), locate(copy))),
        );
        metadata.initial_copies.extend(
            module
                .initial_copies
                .iter()
                .map(|&(variable, copy)| (locate(variable), locate(copy))),
        );
        metadata.nba_switches.extend(
            module
                .nba_switches
                .iter()
                .map(|switch| switch.map(|&var| locate(var))),
        );
    }
    metadata
}
