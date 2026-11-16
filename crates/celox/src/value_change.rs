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
/// or a function unit of `sir` with a value change point. A flip-flop
/// commit or a settle shows its updates when the unit ends. A function
/// unit runs inside the unit that calls it: inside a kernel, its points show
/// the host the updates as the kernel's own; inside a flip-flop or settle
/// unit, which runs with no host context, they do nothing and the updates
/// show when that unit ends.
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
    let mut functions = false;
    for function in &mut sir.functions {
        functions |= instrument_unit(&mut function.unit, stable);
    }
    // A call's effects include the host interaction of its points.
    if functions {
        sir.summarize_calls();
    }
}

/// Whether `unit` has a value change point, or calls a function of `sir`
/// that has one, directly or through further calls.
fn reaches_points<A, U>(unit: &ExecutionUnit<U>, sir: &celox_sir::SirProgram<A, U>) -> bool {
    let mut seen = HashSet::default();
    let mut pending = vec![unit];
    while let Some(unit) = pending.pop() {
        if has_points(unit) {
            return true;
        }
        for instruction in unit.blocks.values().flat_map(|block| &block.instructions) {
            if let SIRInstruction::Call { function, .. } = instruction
                && seen.insert(function.0)
                && let Some(callee) = sir.functions.get(function.0 as usize)
            {
                pending.push(&callee.unit);
            }
        }
    }
    false
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

/// What a value change point becomes: the instructions in its place and,
/// when the call may fail, the one-bit register that is set when it did.
pub(crate) struct Lowered<A> {
    pub instructions: Vec<SIRInstruction<A>>,
    pub failed: Option<RegisterId>,
}

/// Replace every value change point of `unit` with what `lower` makes of it,
/// given its site register. A point that may fail ends its block: the unit
/// goes on in a new block when the call succeeded and ends with an error
/// otherwise, so that nothing runs after a failed callback.
pub(crate) fn lower_unit<A>(
    unit: &mut ExecutionUnit<A>,
    mut lower: impl FnMut(&mut ExecutionUnit<A>, RegisterId) -> Lowered<A>,
) {
    let is_point = |instruction: &SIRInstruction<A>| {
        matches!(
            instruction,
            SIRInstruction::ExternCall { func, .. } if *func == VALUE_CHANGE_ID
        )
    };
    let mut next_block = unit.blocks.keys().map(|id| id.0 + 1).max().unwrap_or(0);
    let mut error = None;
    let mut pending: Vec<_> = unit
        .blocks
        .iter()
        .filter(|(_, block)| block.instructions.iter().any(is_point))
        .map(|(id, _)| *id)
        .collect();
    while let Some(id) = pending.pop() {
        let block = unit.blocks.get_mut(&id).expect("listed block");
        let mut old = std::mem::take(&mut block.instructions).into_iter();
        let mut instructions = Vec::with_capacity(old.len());
        let mut split = None;
        for instruction in old.by_ref() {
            let SIRInstruction::ExternCall { func, args, .. } = &instruction else {
                instructions.push(instruction);
                continue;
            };
            if *func != VALUE_CHANGE_ID {
                instructions.push(instruction);
                continue;
            }
            let lowered = lower(unit, args[0]);
            instructions.extend(lowered.instructions);
            if let Some(failed) = lowered.failed {
                split = Some(failed);
                break;
            }
        }
        let rest: Vec<_> = old.collect();
        let block = unit.blocks.get_mut(&id).expect("listed block");
        block.instructions = instructions;
        let Some(failed) = split else {
            continue;
        };
        let error = *error.get_or_insert_with(|| {
            let id = celox_sir::BlockId(next_block);
            next_block += 1;
            unit.blocks.insert(
                id,
                celox_sir::BasicBlock {
                    id,
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: celox_sir::SIRTerminator::Error(1),
                },
            );
            id
        });
        let continued = celox_sir::BlockId(next_block);
        next_block += 1;
        let block = unit.blocks.get_mut(&id).expect("listed block");
        let terminator = std::mem::replace(
            &mut block.terminator,
            celox_sir::SIRTerminator::Branch {
                cond: failed,
                true_block: (error, Vec::new()),
                false_block: (continued, Vec::new()),
            },
        );
        let more = rest.iter().any(is_point);
        unit.blocks.insert(
            continued,
            celox_sir::BasicBlock {
                id: continued,
                params: Vec::new(),
                instructions: rest,
                terminator,
            },
        );
        if more {
            pending.push(continued);
        }
    }
}

/// Make every value change point of `program` a call of its native services
/// (`VALUE_CHANGE`), which shows the host the update, or remove the points
/// when the build has no native services or no scheduler slots to pass the
/// host context through. Returns whether points remain.
#[cfg(feature = "host-runtime")]
pub(crate) fn lower(program: &mut crate::ir::LaidOutProgram, native: bool) -> bool {
    use celox_sir::SIROffset;

    let any = program.sir.processes.iter().any(has_points)
        || program
            .sir
            .functions
            .iter()
            .any(|function| has_points(&function.unit));
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
            .filter(|(_, unit)| reaches_points(unit, &program.sir))
            .map(|(index, _)| index)
            .collect();
    }
    let units = program.sir.processes.iter_mut().chain(
        program
            .sir
            .functions
            .iter_mut()
            .map(|function| &mut function.unit),
    );
    for unit in units {
        lower_unit(unit, |unit, site| {
            let Some((service, (context_slot, now))) = service else {
                return Lowered {
                    instructions: Vec::new(),
                    failed: None,
                };
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
            let (context, request, time, zero, status) =
                (register(), register(), register(), register(), register());
            let failed = RegisterId(next);
            unit.register_map.insert(
                failed,
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            );
            let stable = |address| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address);
            let instructions = vec![
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
                    dst: Some(status),
                    func: service,
                    args: vec![context, site, request, time, zero],
                },
                // A callback that panicked leaves its failure in the scope.
                SIRInstruction::Binary(failed, status, celox_sir::BinaryOp::Ne, zero),
            ];
            Lowered {
                instructions,
                failed: Some(failed),
            }
        });
    }
    // The calls of functions whose points changed have new effects.
    if !program.sir.functions.is_empty() {
        program.sir.summarize_calls();
    }
    service.is_some()
}

#[cfg(all(test, target_arch = "x86_64", not(feature = "arm64-codegen")))]
#[allow(
    clippy::disallowed_types,
    reason = "test-only: the tests read back what the observers they hand to the simulation record"
)]
mod tests {
    use crate::{SignalRef, SimBackend, Simulation, SlotIo, SlotObserver};
    use std::sync::{Arc, Mutex};

    /// Records the value of `signal` at each update.
    struct Values {
        signal: SignalRef,
        seen: Arc<Mutex<Vec<(u64, u8)>>>,
    }

    impl<B: SimBackend> SlotObserver<B> for Values {
        fn before_domains(&mut self, _: &mut SlotIo<'_, B>, _: u64) {}
        fn end_of_slot(&mut self, _: &mut SlotIo<'_, B>, _: u64) {}
        fn observes_updates(&self) -> bool {
            true
        }
        fn after_update(&mut self, io: &mut SlotIo<'_, B>, time: u64) {
            let value = u8::try_from(io.get(self.signal)).unwrap();
            let mut seen = self.seen.lock().unwrap();
            if seen.last().is_none_or(|&(_, last)| last != value) {
                seen.push((time, value));
            }
        }
    }

    #[test]
    fn a_tiered_build_shows_the_same_updates_in_each_tier() {
        let source = r#"`timescale 1ns/1ns
module Top;
    logic x = 0;
    initial forever begin
        #5 x = 1;
        x = 0;
    end
endmodule"#;
        let mut sim = Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("value-change-tiers.sv"))],
            "Top",
        )
        .observable_updates(true)
        .value_change_signals(["Top.x"])
        .hotspot_jit(true)
        .build_tiered()
        .unwrap();
        let signal = sim.signal("x");
        let seen = Arc::new(Mutex::new(vec![(0, 0)]));
        sim.observe_slots(
            Box::new(Values {
                signal,
                seen: seen.clone(),
            }),
            &[signal],
        );
        sim.run_until(7).unwrap();
        sim.simulator.backend.publish_scheduler_for_test();
        sim.run_until(12).unwrap();
        // The interpreted tier has no host context in its units: neither
        // tier shows the pulse inside the run.
        assert_eq!(*seen.lock().unwrap(), vec![(0, 0)]);
    }
}
