//! The inputs of the top-level modules that only waiting processes sense.

use super::*;

/// Type as clocks the inputs of the top-level instances `tops` that a
/// process waits for and that are no events yet. Returns how many.
///
/// A host moves a signal through simulated time, as a periodic clock or by
/// a scheduled value, only when the signal is an event. A frontend types as
/// events the signals its edge-triggered logic is sensitive to. An input
/// that only a waiting process senses (`initial forever @(posedge clk) ...`)
/// has no such type: nothing but host writes between the runs could move
/// it, so nothing could clock the design.
///
/// Such an input becomes a rising clock with an empty domain, as a clock
/// that only feeds a gated clock is one: a one-bit input port of a
/// top-level instance that a wait site names, itself or by a whole-variable
/// copy of it (port connections and continuous assignments,
/// `clock_domains`). An input that such a copy already makes an event, at
/// the copy's edge, is left as it is. The waits observe the signal
/// themselves, at whatever edge or level they wait for; the kind only names
/// the edge of the empty domain.
///
/// Every other signal keeps its type: a design without such a wait keeps
/// its events, and a wait for a signal inside the design makes no event. The
/// type is the module's, as the type of a flip-flop clock is: a top-level
/// module that another top instantiates as well senses the port as a clock
/// in that instance too.
pub(super) fn declare_waited_inputs(
    bindings: &PackageBindings,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &mut HashMap<ModuleId, SimModule>,
    clock_domains: &HashMap<AbsoluteAddr, AbsoluteAddr>,
    tops: &HashSet<InstanceId>,
) -> usize {
    let variable = |address: &AbsoluteAddr| {
        modules[&instance_modules[&address.instance_id]]
            .variables
            .get(&address.var_id)
    };
    let mut waited = BTreeSet::new();
    for (&instance, module) in instance_modules {
        for site in &modules[module].wait_sites {
            for &(term, _) in &site.terms {
                let term = bindings.locate(instance, term);
                let source = clock_domains.get(&term).copied().unwrap_or(term);
                if tops.contains(&source.instance_id)
                    && variable(&source).is_some_and(|variable| {
                        variable.kind == crate::VariableKind::Input
                            && variable.metadata.kind == DomainKind::Other
                            && variable.metadata.value_kind == celox_design::ValueKind::Bits
                            && variable.metadata.width == 1
                            && variable.metadata.array_dims.is_empty()
                    })
                {
                    waited.insert(source);
                }
            }
        }
    }
    if waited.is_empty() {
        return 0;
    }
    for (copy, source) in clock_domains {
        if waited.contains(source)
            && variable(copy).is_some_and(|variable| {
                celox_design::edges::polarity(variable.metadata.kind).is_some()
            })
        {
            waited.remove(source);
        }
    }
    for address in &waited {
        if let Some(variable) = modules
            .get_mut(&instance_modules[&address.instance_id])
            .and_then(|module| module.variables.get_mut(&address.var_id))
        {
            variable.metadata.kind = DomainKind::ClockPosedge;
        }
    }
    waited.len()
}
