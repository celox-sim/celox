//! The variables net inlining must leave in state.

use super::*;

/// The units other than combinational paths, and the event signals, of a
/// design whose unobserved nets are inlined ([`pinned_nets`]).
pub(super) struct PinnedUnits<'a> {
    pub(super) flip_flops: Vec<&'a ExecutionUnit<RegionedAbsoluteAddr>>,
    pub(super) events: HashSet<AbsoluteAddr>,
}

/// The variables net inlining must leave in state
/// (`celox_slt::net_inline::inline_nets`): everything a unit other than a
/// plain combinational path reads or writes, everything the runtime reads
/// or the scheduler judges by its address, the observed ports, and the
/// objects that are no packed vector.
#[allow(clippy::too_many_arguments)]
pub(super) fn pinned_nets(
    policy: celox_design::DeadStorePolicy,
    root: InstanceId,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    bindings: &PackageBindings,
    units: PinnedUnits<'_>,
    clock_domains: &HashMap<AbsoluteAddr, AbsoluteAddr>,
    timing: &TimingMetadata,
    counted: impl Iterator<Item = AbsoluteAddr>,
    probed: impl Iterator<Item = AbsoluteAddr>,
    comb_observers: &[CombObserver<AbsoluteAddr>],
    comb_blocks: &[LogicPath<AbsoluteAddr>],
    unpacked_element_widths: &HashMap<AbsoluteAddr, usize>,
) -> HashSet<AbsoluteAddr> {
    let mut pinned: HashSet<AbsoluteAddr> = HashSet::default();
    for unit in units.flip_flops {
        for instruction in unit.blocks.values().flat_map(|block| &block.instructions) {
            pinned.extend(
                super::super::read_objects(instruction)
                    .chain(super::super::written_objects(instruction))
                    .map(|address| address.absolute_addr()),
            );
        }
    }
    // An imperative combinational process loads what it reads.
    for path in comb_blocks {
        if let LogicPathTarget::Process { unit, .. } = &path.target {
            for instruction in unit.blocks.values().flat_map(|block| &block.instructions) {
                pinned.extend(
                    super::super::read_objects(instruction)
                        .chain(super::super::written_objects(instruction))
                        .copied(),
                );
            }
        }
    }
    // The scheduler judges an event and every signal of its domain.
    pinned.extend(
        clock_domains
            .iter()
            .filter(|(alias, canonical)| {
                units.events.contains(*alias) || units.events.contains(*canonical)
            })
            .flat_map(|(alias, canonical)| [*alias, *canonical]),
    );
    pinned.extend(units.events);
    // The runtime reads what waits watch and what it samples.
    for site in &timing.wait_sites {
        pinned.extend(site.terms.iter().map(|(signal, _)| *signal));
        pinned.extend(site.gates.iter().flatten().map(|gate| gate.signal));
    }
    pinned.extend(
        timing
            .sample_shadows
            .iter()
            .chain(&timing.initial_copies)
            .flat_map(|(variable, copy)| [*variable, *copy]),
    );
    pinned.extend(counted);
    pinned.extend(probed);
    for observer in comb_observers {
        pinned.extend(
            observer
                .sensitivity
                .iter()
                .chain(&observer.observed_inputs)
                .chain(&observer.position_inputs)
                .chain(&observer.preceding_writes)
                .chain(observer.sensitivity_counters.iter().map(|(atom, _)| atom))
                .map(|atom| atom.id),
        );
        pinned.extend(observer.local_inputs.iter().map(|(var, _)| *var));
    }
    pinned.extend(unpacked_element_widths.keys().copied());
    for (&instance, module) in instance_modules {
        for (&var_id, variable) in &modules[module].variables {
            let port = variable.kind.is_port()
                && match policy {
                    celox_design::DeadStorePolicy::PreserveAllPorts => true,
                    celox_design::DeadStorePolicy::PreserveTopPorts => instance == root,
                    celox_design::DeadStorePolicy::PreserveListedSignals
                    | celox_design::DeadStorePolicy::Off => false,
                };
            if port
                || variable.metadata.value_kind != celox_design::ValueKind::Bits
                || !variable.metadata.array_dims.is_empty()
            {
                pinned.insert(bindings.locate(instance, var_id));
            }
        }
    }
    pinned
}
