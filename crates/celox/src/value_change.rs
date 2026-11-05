//! Value change points: the host sees every update of a watched object right
//! after it is made, with the rest of the state as the program has it then
//! (IEEE 1800-2023 38.36.1, 4.10).
//!
//! Before optimization, each store or commit of a watched object's stable
//! state is followed by a call of [`VALUE_CHANGE_ID`]. Every pass treats that
//! call as reading and writing all state, so the stores the program made
//! before it are in memory and none made after it moves above it. Units that
//! write no watched object are unchanged. After layout, [`lower`] replaces
//! each call with a call of the native services, or removes it when the build
//! has none; the host then sees updates at the boundaries of units only.

use celox_sir::extern_abi::VALUE_CHANGE_ID;
use celox_sir::{ExecutionUnit, RegisterId, RegisterType, SIRInstruction, SIRValue};

use crate::HashSet;
use crate::ir::{AbsoluteAddr, RegionedAbsoluteAddr, STABLE_REGION, SirProgram};

/// The value change point after an update of `watched`'s stable state.
fn site<A>(instruction: &SIRInstruction<A>, watched: impl Fn(&A) -> bool) -> bool {
    match instruction {
        SIRInstruction::Store(address, ..) | SIRInstruction::Commit(_, address, ..) => {
            watched(address)
        }
        _ => false,
    }
}

/// Follow every update of a `watched` object in `unit` with a value change
/// point, and return whether there was one.
fn instrument_unit<A>(unit: &mut ExecutionUnit<A>, watched: impl Fn(&A) -> bool) -> bool {
    let mut next = unit
        .register_map
        .keys()
        .map(|id| id.0 + 1)
        .max()
        .unwrap_or(0);
    let mut any = false;
    for block in unit.blocks.values_mut() {
        if !block.instructions.iter().any(|i| site(i, &watched)) {
            continue;
        }
        any = true;
        let mut instructions = Vec::with_capacity(block.instructions.len() + 2);
        for instruction in std::mem::take(&mut block.instructions) {
            let point = site(&instruction, &watched);
            instructions.push(instruction);
            if point {
                let site = RegisterId(next);
                next += 1;
                unit.register_map.insert(
                    site,
                    RegisterType::Bit {
                        width: 64,
                        signed: false,
                    },
                );
                instructions.push(SIRInstruction::Imm(site, SIRValue::new(0u64)));
                instructions.push(SIRInstruction::ExternCall {
                    dst: None,
                    func: VALUE_CHANGE_ID,
                    args: vec![site],
                });
            }
        }
        block.instructions = instructions;
    }
    any
}

/// Follow every blocking update of a `watched` object in a process kernel
/// of `sir` with a value change point. A flip-flop commit or a settle shows
/// its updates when the unit ends, and so does a function, whose units run
/// in the phases too.
pub(crate) fn instrument(sir: &mut SirProgram, watched: &HashSet<AbsoluteAddr>) {
    if watched.is_empty() {
        return;
    }
    let stable = |address: &RegionedAbsoluteAddr| {
        address.region == STABLE_REGION && watched.contains(&address.absolute_addr())
    };
    for unit in &mut sir.processes {
        instrument_unit(unit, stable);
    }
}

/// Whether `unit` has a value change point.
pub(crate) fn has_points<A>(unit: &ExecutionUnit<A>) -> bool {
    unit.blocks.values().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                SIRInstruction::ExternCall { func, .. } if *func == VALUE_CHANGE_ID
            )
        })
    })
}

/// Replace every value change point of `unit` with what `lower` makes of it:
/// the instructions to emit in its place, given its site register.
pub(crate) fn lower_unit<A>(
    unit: &mut ExecutionUnit<A>,
    mut lower: impl FnMut(&mut ExecutionUnit<A>, RegisterId) -> Vec<SIRInstruction<A>>,
) {
    let ids: Vec<_> = unit.blocks.keys().copied().collect();
    for id in ids {
        let block = unit.blocks.get_mut(&id).expect("listed block");
        if !block.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                SIRInstruction::ExternCall { func, .. } if *func == VALUE_CHANGE_ID
            )
        }) {
            continue;
        }
        let old = std::mem::take(&mut block.instructions);
        let mut instructions = Vec::with_capacity(old.len());
        for instruction in old {
            match instruction {
                SIRInstruction::ExternCall { func, args, .. } if func == VALUE_CHANGE_ID => {
                    instructions.extend(lower(unit, args[0]));
                }
                instruction => instructions.push(instruction),
            }
        }
        unit.blocks.get_mut(&id).expect("listed block").instructions = instructions;
    }
}

/// Make every value change point of `program` a call of its native services
/// (`VALUE_CHANGE`), which shows the host the update, or remove the points
/// when the build has no native services or no scheduler slots to pass the
/// host context through. Returns whether points remain.
#[cfg(feature = "host-runtime")]
pub(crate) fn lower(program: &mut crate::ir::LaidOutProgram, native: bool) -> bool {
    use celox_sir::SIROffset;

    let any = program.sir.processes.iter().any(has_points);
    if !any {
        return false;
    }
    let slots = program
        .runtime
        .runtime_schema
        .scheduler
        .as_ref()
        .map(|scheduler| (scheduler.slots.defer_limit_settle, scheduler.slots.now));
    let service = match slots {
        Some(slots) if native => {
            crate::scheduler::require_native_services(program);
            let service = program
                .runtime
                .runtime_schema
                .extern_functions
                .iter()
                .position(|function| function.name == crate::simulator::native_io::SYMBOL)
                .expect("native services were just required") as u32;
            Some((service, slots))
        }
        _ => None,
    };
    if service.is_some()
        && let Some(scheduler) = program.runtime.runtime_schema.scheduler.as_mut()
    {
        scheduler.value_change_kernels = program
            .sir
            .processes
            .iter()
            .enumerate()
            .filter(|(_, unit)| has_points(unit))
            .map(|(index, _)| index)
            .collect();
    }
    for unit in &mut program.sir.processes {
        lower_unit(unit, |unit, site| {
            let Some((service, (context_slot, now))) = service else {
                return Vec::new();
            };
            let mut next = unit
                .register_map
                .keys()
                .map(|id| id.0 + 1)
                .max()
                .unwrap_or(0);
            let mut register = || {
                let id = RegisterId(next);
                next += 1;
                unit.register_map.insert(
                    id,
                    RegisterType::Bit {
                        width: 64,
                        signed: false,
                    },
                );
                id
            };
            let (context, request, time, zero) = (register(), register(), register(), register());
            let stable = |address| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address);
            vec![
                SIRInstruction::Load(
                    context,
                    stable(context_slot),
                    SIROffset::Static(celox_design::SCHEDULER_HOST_CONTEXT_BIT),
                    64,
                ),
                SIRInstruction::Imm(
                    request,
                    SIRValue::new(crate::simulator::native_io::VALUE_CHANGE),
                ),
                SIRInstruction::Load(time, stable(now), SIROffset::Static(0), 64),
                SIRInstruction::Imm(zero, SIRValue::new(0u64)),
                SIRInstruction::ExternCall {
                    dst: None,
                    func: service,
                    args: vec![context, site, request, time, zero],
                },
            ]
        });
    }
    service.is_some()
}
