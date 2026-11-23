//! Register publications observed only through equality with a single constant.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Predicate {
    pub(crate) width: usize,
    pub(crate) value: u64,
}

pub(crate) fn homes(program: &LaidOutProgram) -> crate::HashMap<usize, Predicate> {
    let layout = program.layout();
    let schema = &program.runtime.runtime_schema;
    // Parallel lanes have additional settle bodies outside the observation schedule.
    if layout.four_state || program.sir.parallel.is_some() {
        return Default::default();
    }
    let mut excluded = crate::HashSet::default();
    let mut candidates = crate::HashMap::default();
    // A wait flag or an edge observes transitions of the register itself.
    if let Some(settle) = &layout.settle {
        for object in &settle.objects {
            if object
                .partitions
                .iter()
                .any(|&p| p as usize >= settle.partitions)
            {
                excluded.insert(object.start);
            }
        }
    }
    let mut exclude = |addr: &crate::AbsoluteAddr| {
        if let Some(&home) = layout.offsets.get(addr) {
            excluded.insert(home);
        }
    };
    // Copies to distinct homes can forward a change without a load in the
    // consuming partition. Keep those sources sensitive to the whole value.
    for (target, source) in schema
        .settle_partitions
        .iter()
        .flat_map(|plan| &plan.copies)
    {
        if layout.offsets.get(target) != layout.offsets.get(source) {
            exclude(source);
        }
    }
    for addr in crate::edge_plans::event_addresses(program) {
        exclude(&addr);
    }
    for watch in &schema.watches {
        exclude(&watch.signal);
    }
    for observer in &schema.comb_observers {
        for input in &observer.sensitivity {
            exclude(&input.id);
        }
    }
    for (input, _) in &schema.occurrence_probes {
        exclude(&input.id);
    }
    for site in &schema.wait_sites {
        for (signal, _) in &site.terms {
            exclude(signal);
        }
        for gate in site.gates.iter().flatten() {
            for addr in gate.reads() {
                exclude(&addr);
            }
        }
    }
    for unit in &program.sir.updates.observe {
        let operands = compared_operands(unit);
        for inst in unit.blocks.values().flat_map(|b| &b.instructions) {
            if let SIRInstruction::Load(r, a, SIROffset::Static(0), width) = inst
                && a.region == STABLE_REGION
                && let Some(&Some(value)) = operands.get(r)
                && unit
                    .register_map
                    .get(r)
                    .is_some_and(|ty| ty.width() == *width)
                && (2..=64).contains(width)
                && layout.widths.get(&a.absolute_addr()) == Some(width)
                && !layout.unpacked_arrays.contains_key(&a.absolute_addr())
                && let Some(&home) = layout.offsets.get(&a.absolute_addr())
            {
                let test = Predicate {
                    width: *width,
                    value,
                };
                if candidates
                    .insert(home, test)
                    .is_some_and(|prior| prior != test)
                {
                    excluded.insert(home);
                }
                continue;
            }
            for effect in [inst.state_reads(), inst.state_writes()] {
                let Some(addresses) = effect.objects() else {
                    return Default::default();
                };
                excluded.extend(
                    addresses
                        .filter(|a| a.region == STABLE_REGION)
                        .filter_map(|a| layout.offsets.get(&a.absolute_addr())),
                );
            }
        }
    }
    candidates.retain(|home, _| !excluded.contains(home));
    candidates
}

/// A load can be projected only when every use agrees on the predicate,
/// including uses in other blocks. Block arguments and conflicting
/// definitions do not identify a constant.
fn compared_operands(
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> crate::HashMap<RegisterId, Option<u64>> {
    let mut constants = crate::HashMap::default();
    for block in unit.blocks.values() {
        for &param in &block.params {
            constants.insert(param, None);
        }
        for inst in &block.instructions {
            let Some(reg) = inst.defined_register() else {
                continue;
            };
            let value = match inst {
                SIRInstruction::Imm(_, value)
                    if value.mask.bits() == 0
                        && unit
                            .register_map
                            .get(&reg)
                            .is_some_and(|ty| value.payload.bits() <= ty.width() as u64) =>
                {
                    u64::try_from(&value.payload).ok()
                }
                _ => None,
            };
            constants
                .entry(reg)
                .and_modify(|v| *v = None)
                .or_insert(value);
        }
    }
    let mut uses = crate::HashMap::default();
    let mut observe = |reg, value| {
        uses.entry(reg)
            .and_modify(|prior| {
                if *prior != value {
                    *prior = None;
                }
            })
            .or_insert(value);
    };
    for block in unit.blocks.values() {
        for inst in &block.instructions {
            if let SIRInstruction::Binary(_, a, BinaryOp::Eq | BinaryOp::Ne, b) = inst {
                for (reg, other) in [(*a, *b), (*b, *a)] {
                    let value = constants.get(&other).copied().flatten().filter(|&value| {
                        value == 0
                            || unit.register_map.get(&reg).map(|ty| ty.width())
                                == unit.register_map.get(&other).map(|ty| ty.width())
                    });
                    observe(reg, value);
                }
            } else {
                inst.for_each_use(|reg| observe(reg, None));
            }
        }
        block.terminator.for_each_use(|reg| observe(reg, None));
    }
    uses
}
/// Compute the truth values seen by the readers on either side of a whole
/// two-state register commit. The commit itself still copies every bit.
pub(super) fn compare(
    instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    predicate: Predicate,
    before: RegisterId,
    after: RegisterId,
    mut bit_register: impl FnMut(usize) -> RegisterId,
) -> (RegisterId, RegisterId) {
    let constant = bit_register(predicate.width);
    instructions.push(SIRInstruction::Imm(
        constant,
        SIRValue::new(predicate.value),
    ));
    let mut test = |value| {
        let result = bit_register(1);
        instructions.push(SIRInstruction::Binary(
            result,
            value,
            BinaryOp::Eq,
            constant,
        ));
        result
    };
    (test(before), test(after))
}
