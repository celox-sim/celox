//! The compiled scheduler: one SIR unit that runs time slots of the process
//! kernels that only delay, as the runtime's reference loop runs them.
//!
//! In a design whose event signals only such kernels write, a time slot of
//! the reference loop is short. The kernels due at the slot run in kernel
//! order; combinational logic settles; if the kernels gave an active edge
//! of one event, that domain's `eval_apply` unit runs, or with several, the
//! `eval_only` unit of each then the `apply` unit of each, in event order;
//! combinational logic settles again. The unit does exactly this in a loop
//! over the slots up to a limit the runtime sets, with the wake time of
//! each kernel in [`SchedulerSlots::wake`]. It inlines the units the
//! reference loop calls, so their code is unchanged.
//!
//! The runtime runs the unit only for slots that touch nothing else (see
//! `celox_runtime::SimulationState::run_until_observed`): every other slot,
//! and every design or kernel this module does not qualify, runs in the
//! reference loop.

use celox_design::{
    DomainKind, ProcessStatus, RuntimeEventKind, SCHEDULER_FLAG_WIDTH, SCHEDULER_TIME_WIDTH,
    STABLE_REGION, SchedulerExit, SchedulerSlots, TIME_SLOT_WIDTH, schedule::ScheduleModel,
};
use celox_sir::{BinaryOp, SIRBuilder, SIRSwitchCase, SIRValue};

use crate::{
    HashMap, HashSet,
    ir::{
        AbsoluteAddr, ExecutionUnit, LaidOutProgram, RegionedAbsoluteAddr, RegisterId,
        SIRInstruction, SIROffset, SIRTerminator,
    },
};

type Unit = ExecutionUnit<RegionedAbsoluteAddr>;
type Builder = SIRBuilder<RegionedAbsoluteAddr>;

/// The most events whose edges one slot can record.
const MAX_EVENTS: usize = 64;

/// Generate the compiled scheduler of `program`, if it qualifies, and record
/// for every process kernel whether the scheduler runs it.
pub(crate) fn attach(program: &mut LaidOutProgram) {
    let Some(schema) = program.runtime.runtime_schema.scheduler.clone() else {
        return;
    };
    let kernels = program.runtime.runtime_schema.processes.len();
    let (fallback, unit, events) = match Plan::new(program, &schema.slots) {
        Err(reason) => (vec![Some(reason); kernels], None, Vec::new()),
        Ok(plan) => {
            let fallback = plan.fallback.clone();
            let unit = plan
                .kernels
                .iter()
                .any(Option::is_some)
                .then(|| plan.generate(program, &schema.slots));
            let events = plan
                .written()
                .into_iter()
                .map(|event| plan.events[event].signal)
                .collect();
            (fallback, unit, events)
        }
    };
    let scheduler = program
        .runtime
        .runtime_schema
        .scheduler
        .as_mut()
        .expect("checked above");
    scheduler.fallback = fallback;
    scheduler.events = events;
    if let Some(unit) = unit {
        scheduler.unit = Some(program.sir.processes.len());
        program.sir.processes.push(unit);
    }
}

/// An event the scheduler records edges of.
struct Event {
    /// The canonical event address, which is also its signal.
    signal: AbsoluteAddr,
    width: usize,
    /// The level of the signal after an active edge.
    active: u8,
    eval_apply: Vec<Unit>,
    eval_only: Vec<Unit>,
    apply: Vec<Unit>,
}

/// A kernel the scheduler runs.
struct Kernel {
    /// The events whose signal the kernel writes, as indices into
    /// [`Plan::events`].
    writes: Vec<usize>,
}

struct Plan {
    /// In event-id order, which the reference loop visits several triggered
    /// domains in.
    events: Vec<Event>,
    /// Per process kernel, `Some` when the scheduler runs it.
    kernels: Vec<Option<Kernel>>,
    fallback: Vec<Option<String>>,
    /// Runtime event sites that record a `$finish`.
    finish_sites: HashSet<u32>,
}

impl Plan {
    fn new(program: &LaidOutProgram, slots: &SchedulerSlots<AbsoluteAddr>) -> Result<Self, String> {
        let schema = &program.runtime.runtime_schema;
        let model = ScheduleModel::derive(&program.design, schema);
        if program.layout().four_state {
            return Err("the design is four-state".into());
        }
        if !model.counted.is_empty() {
            return Err("the design counts event occurrences".into());
        }
        let topology = &model.events;
        if !topology.cascaded_events.is_empty()
            || !topology.derived_events.is_empty()
            || !topology.combinational_derived.is_empty()
        {
            return Err("the design has derived clocks".into());
        }
        if !schema.comb_observers.is_empty() {
            return Err("the design observes combinational processes".into());
        }
        if slots.wake.len() != schema.processes.len() {
            return Err("the scheduler slots do not match the kernels".into());
        }
        let sir = &program.sir;
        let comb_fatal = sir.eval_comb.iter().any(|unit| {
            instructions(unit).any(|inst| {
                matches!(
                    inst,
                    SIRInstruction::CombCaptureEvent {
                        fatal_error_code: Some(_),
                        ..
                    }
                )
            })
        });
        if comb_fatal {
            return Err("combinational logic has fatal assertions".into());
        }

        let shared = program.shared_storage();
        let storage = |address: AbsoluteAddr| shared.get(&address).copied().unwrap_or(address);
        let addresses = crate::edge_plans::event_addresses(program);
        if addresses.len() > MAX_EVENTS {
            return Err(format!("the design has more than {MAX_EVENTS} events"));
        }
        let mut events = Vec::with_capacity(addresses.len());
        let mut event_storage = HashMap::default();
        for &signal in &addresses {
            let active = match model.kinds.get(&signal) {
                Some(DomainKind::ClockPosedge | DomainKind::ResetAsyncHigh) => 1,
                Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => 0,
                _ => return Err("an event is not a clock or reset edge".into()),
            };
            let units = |map: &HashMap<AbsoluteAddr, Vec<Unit>>| -> Result<Vec<Unit>, String> {
                let aliased = map
                    .keys()
                    .any(|key| *key != signal && topology.canonical(*key) == signal);
                match map.get(&signal) {
                    Some(units) if !aliased => Ok(units.clone()),
                    _ => Err("an event domain has no separate units".into()),
                }
            };
            let event = Event {
                signal,
                width: *program
                    .layout()
                    .widths
                    .get(&signal)
                    .ok_or("an event signal has no storage")?,
                active,
                eval_apply: units(&sir.eval_apply_ffs)?,
                eval_only: units(&sir.eval_only_ffs)?,
                apply: units(&sir.apply_ffs)?,
            };
            if event_storage
                .insert(storage(signal), events.len())
                .is_some()
            {
                return Err("two events share a signal".into());
            }
            events.push(event);
        }
        // Event signals change only where the scheduler sees them change.
        let written = |unit: &Unit| -> Vec<usize> {
            let mut written: Vec<usize> = instructions(unit)
                .filter_map(|inst| match inst {
                    SIRInstruction::Store(target, ..) | SIRInstruction::Commit(_, target, ..) => {
                        event_storage.get(&storage(target.absolute_addr())).copied()
                    }
                    _ => None,
                })
                .collect();
            written.sort_unstable();
            written.dedup();
            written
        };
        let rtl_units = sir
            .eval_comb
            .iter()
            .chain(sir.eval_apply_ffs.values().flatten())
            .chain(sir.eval_only_ffs.values().flatten())
            .chain(sir.apply_ffs.values().flatten());
        for unit in rtl_units {
            if !written(unit).is_empty() {
                return Err("RTL logic drives an event signal".into());
            }
        }

        let mut kernels = Vec::with_capacity(schema.processes.len());
        let mut fallback = Vec::with_capacity(schema.processes.len());
        for (index, slots) in schema.processes.iter().enumerate() {
            let process = &model.processes[index];
            let unit = &sir.processes[index];
            let reason = if slots.is_initializer {
                Some("a declaration initializer")
            } else if slots.requires_timed_simulation {
                Some("an edge procedure")
            } else if !slots.const_delays {
                Some("a delay is not a constant of at least one time unit")
            } else if !process.waits.is_empty() {
                Some("the process waits for events")
            } else if !process.nbas.is_empty() {
                Some("the process has nonblocking assignments")
            } else if !process.host_requests.is_empty() {
                Some("the process makes host requests")
            } else if !delay_statuses_only(unit, slots.status) {
                Some("the process suspends other than for a delay")
            } else {
                None
            };
            match reason {
                Some(reason) => {
                    kernels.push(None);
                    fallback.push(Some(reason.to_string()));
                }
                None => {
                    kernels.push(Some(Kernel {
                        writes: written(unit),
                    }));
                    fallback.push(None);
                }
            }
        }
        let finish_sites = schema
            .runtime_event_sites
            .iter()
            .enumerate()
            .filter(|(_, site)| matches!(site.kind, RuntimeEventKind::Finish))
            .map(|(index, _)| index as u32)
            .collect();
        Ok(Self {
            events,
            kernels,
            fallback,
            finish_sites,
        })
    }

    /// The events the scheduler's kernels write, in event order: the only
    /// ones that can have edges in its slots.
    fn written(&self) -> Vec<usize> {
        let mut written: Vec<usize> = self
            .kernels
            .iter()
            .flatten()
            .flat_map(|kernel| kernel.writes.iter().copied())
            .collect();
        written.sort_unstable();
        written.dedup();
        written
    }

    fn generate(&self, program: &LaidOutProgram, slots: &SchedulerSlots<AbsoluteAddr>) -> Unit {
        let schema = &program.runtime.runtime_schema;
        let written = self.written();
        let mut g = Generator {
            b: Builder::new(),
            slots,
            finish_sites: &self.finish_sites,
            exits: Vec::new(),
        };
        let header = g.b.new_block();
        g.b.seal_block(SIRTerminator::Jump(header, Vec::new()));

        // The next slot: the earliest wake time of a waiting kernel.
        g.b.switch_to_block(header);
        let mut time = g.constant(u64::MAX, SCHEDULER_TIME_WIDTH);
        let mut any = g.constant(0, 1);
        for (index, kernel) in self.kernels.iter().enumerate() {
            if kernel.is_some() {
                let wake = g.load(slots.wake[index], SCHEDULER_TIME_WIDTH);
                let waiting = g.waiting(index);
                let earlier = g.binary(1, wake, BinaryOp::LtU, time);
                let earlier = g.binary(1, earlier, BinaryOp::And, waiting);
                let next = g.b.alloc_bit(SCHEDULER_TIME_WIDTH, false);
                g.b.emit(SIRInstruction::Mux(next, earlier, wake, time));
                time = next;
                any = g.binary(1, any, BinaryOp::Or, waiting);
            }
        }
        let limit = g.load(slots.limit, SCHEDULER_TIME_WIDTH);
        let after = g.binary(1, time, BinaryOp::GtU, limit);
        let one = g.constant(1, 1);
        let none = g.binary(1, any, BinaryOp::Xor, one);
        let stop = g.binary(1, after, BinaryOp::Or, none);
        let run = g.b.new_block();
        let limit_exit = g.exit_block(SchedulerExit::Limit, None);
        g.b.seal_block(SIRTerminator::Branch {
            cond: stop,
            true_block: (limit_exit, Vec::new()),
            false_block: (run, Vec::new()),
        });

        g.b.switch_to_block(run);
        g.store(slots.now, SCHEDULER_TIME_WIDTH, time);
        let count = g.load(slots.count, SCHEDULER_TIME_WIDTH);
        let one = g.constant(1, SCHEDULER_TIME_WIDTH);
        let count = g.binary(SCHEDULER_TIME_WIDTH, count, BinaryOp::Add, one);
        g.store(slots.count, SCHEDULER_TIME_WIDTH, count);
        for &slot in &schema.time_slots {
            g.store(slot, TIME_SLOT_WIDTH, time);
        }
        // The levels the slot starts from, the baseline of its edges, for an
        // exit in the middle of the slot.
        let mut levels = g.constant(0, MAX_EVENTS);
        for (bit, &event) in written.iter().enumerate() {
            let level = g.level(&self.events[event]);
            let set = g.constant(1 << bit, MAX_EVENTS);
            let unset = g.constant(0, MAX_EVENTS);
            let value = g.b.alloc_bit(MAX_EVENTS, false);
            g.b.emit(SIRInstruction::Mux(value, level, set, unset));
            levels = g.binary(MAX_EVENTS, levels, BinaryOp::Or, value);
        }
        g.store(slots.levels, SCHEDULER_TIME_WIDTH, levels);
        let finish_exit = g.exit_block(SchedulerExit::Finish, None);
        let internal_exit = g.exit_block(SchedulerExit::Internal, None);

        // The kernels due now, in kernel order, and the active edges they
        // give, as a mask over the events.
        let mut mask = g.constant(0, MAX_EVENTS);
        for (index, kernel) in self.kernels.iter().enumerate() {
            let Some(kernel) = kernel else {
                continue;
            };
            let process = &schema.processes[index];
            let wake = g.load(slots.wake[index], SCHEDULER_TIME_WIDTH);
            let waiting = g.waiting(index);
            let due = g.binary(1, wake, BinaryOp::Eq, time);
            let due = g.binary(1, due, BinaryOp::And, waiting);
            let run = g.b.new_block();
            let joined = g.b.alloc_bit(MAX_EVENTS, false);
            let join = g.b.new_block_with(vec![joined]);
            g.b.seal_block(SIRTerminator::Branch {
                cond: due,
                true_block: (run, Vec::new()),
                false_block: (join, vec![mask]),
            });

            g.b.switch_to_block(run);
            let current = g.constant(index as u64, SCHEDULER_TIME_WIDTH);
            g.store(slots.current, SCHEDULER_TIME_WIDTH, current);
            let before: Vec<_> = kernel
                .writes
                .iter()
                .map(|&event| g.level(&self.events[event]))
                .collect();
            g.inline(&program.sir.processes[index], finish_exit);
            let status = g.load(process.status, celox_design::PROCESS_STATUS_WIDTH);
            let code = |status: ProcessStatus| u64::from(status.code());
            let finished = g.equals(status, code(ProcessStatus::Finish));
            let not_finished = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: finished,
                true_block: (finish_exit, Vec::new()),
                false_block: (not_finished, Vec::new()),
            });
            g.b.switch_to_block(not_finished);
            let done = g.equals(status, code(ProcessStatus::Done));
            let ended = g.b.new_block();
            let suspended = g.b.new_block();
            let resumed = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: done,
                true_block: (ended, Vec::new()),
                false_block: (suspended, Vec::new()),
            });
            g.b.switch_to_block(ended);
            let idle = g.constant(0, SCHEDULER_FLAG_WIDTH);
            g.store(slots.waiting[index], SCHEDULER_FLAG_WIDTH, idle);
            g.b.seal_block(SIRTerminator::Jump(resumed, Vec::new()));

            g.b.switch_to_block(suspended);
            let delay = g.equals(status, code(ProcessStatus::Delay));
            let inactive = g.equals(status, code(ProcessStatus::InactiveDelay));
            let delayed = g.binary(1, delay, BinaryOp::Or, inactive);
            let wait = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: delayed,
                true_block: (wait, Vec::new()),
                false_block: (internal_exit, Vec::new()),
            });
            g.b.switch_to_block(wait);
            let amount = g.load(process.delay, celox_design::PROCESS_DELAY_WIDTH);
            let wake = g.binary(SCHEDULER_TIME_WIDTH, time, BinaryOp::Add, amount);
            let overflow = g.binary(1, wake, BinaryOp::LtU, time);
            let overflow_exit = g.exit_block(SchedulerExit::DelayOverflow, None);
            let in_range = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: overflow,
                true_block: (overflow_exit, Vec::new()),
                false_block: (in_range, Vec::new()),
            });
            g.b.switch_to_block(in_range);
            g.store(slots.wake[index], SCHEDULER_TIME_WIDTH, wake);
            g.b.seal_block(SIRTerminator::Jump(resumed, Vec::new()));

            g.b.switch_to_block(resumed);
            let mut edges = mask;
            for (&event, &was) in kernel.writes.iter().zip(&before) {
                let is = g.level(&self.events[event]);
                let changed = g.binary(1, was, BinaryOp::Ne, is);
                let active = g.equals(is, u64::from(self.events[event].active));
                let edge = g.binary(1, changed, BinaryOp::And, active);
                let set = g.constant(1 << event, MAX_EVENTS);
                let unset = g.constant(0, MAX_EVENTS);
                let bit = g.b.alloc_bit(MAX_EVENTS, false);
                g.b.emit(SIRInstruction::Mux(bit, edge, set, unset));
                edges = g.binary(MAX_EVENTS, edges, BinaryOp::Or, bit);
            }
            g.b.seal_block(SIRTerminator::Jump(join, vec![edges]));
            g.b.switch_to_block(join);
            mask = joined;
        }

        // The settle after the kernels, shared with the one after the
        // domains: the mask waits in `exit_arg` meanwhile.
        let none = g.constant(u64::MAX, SCHEDULER_TIME_WIDTH);
        g.store(slots.current, SCHEDULER_TIME_WIDTH, none);
        g.store(slots.exit_arg, SCHEDULER_TIME_WIDTH, mask);
        let comb = g.b.new_block();
        g.call_comb(comb, 1);

        g.b.switch_to_block(comb);
        for unit in &program.sir.eval_comb {
            g.inline(unit, finish_exit);
        }
        let continuation = g.load(slots.cont, SCHEDULER_FLAG_WIDTH);
        let settled = g.b.new_block();
        let slot_end = g.b.new_block();
        g.b.seal_block(SIRTerminator::Switch {
            selector: continuation,
            cases: vec![
                SIRSwitchCase {
                    value: 1u8.into(),
                    target: settled,
                },
                SIRSwitchCase {
                    value: 2u8.into(),
                    target: slot_end,
                },
            ],
            default: internal_exit,
        });

        // The domains the edges trigger.
        g.b.switch_to_block(settled);
        if written.is_empty() {
            g.b.seal_block(SIRTerminator::Jump(slot_end, Vec::new()));
            g.b.switch_to_block(slot_end);
            g.b.seal_block(SIRTerminator::Jump(header, Vec::new()));
            return g.finish();
        }
        let mask = g.load(slots.exit_arg, MAX_EVENTS);
        let zero = g.constant(0, MAX_EVENTS);
        let quiet = g.binary(1, mask, BinaryOp::Eq, zero);
        let triggered = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: quiet,
            true_block: (slot_end, Vec::new()),
            false_block: (triggered, Vec::new()),
        });
        g.b.switch_to_block(triggered);
        let one = g.constant(1, MAX_EVENTS);
        let lower = g.binary(MAX_EVENTS, mask, BinaryOp::Sub, one);
        let others = g.binary(MAX_EVENTS, mask, BinaryOp::And, lower);
        let single = g.binary(1, others, BinaryOp::Eq, zero);
        let one_domain = g.b.new_block();
        let domains = g.b.new_block();
        let evaluated = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: single,
            true_block: (one_domain, Vec::new()),
            false_block: (domains, Vec::new()),
        });

        // One triggered domain evaluates and applies at once.
        g.b.switch_to_block(one_domain);
        for &index in &written {
            let event = &self.events[index];
            let bit = g.constant(1 << index, MAX_EVENTS);
            let this = g.binary(1, mask, BinaryOp::Eq, bit);
            let unit = g.b.new_block();
            let next = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: this,
                true_block: (unit, Vec::new()),
                false_block: (next, Vec::new()),
            });
            g.b.switch_to_block(unit);
            for unit in &event.eval_apply {
                g.inline(unit, finish_exit);
            }
            g.b.seal_block(SIRTerminator::Jump(evaluated, Vec::new()));
            g.b.switch_to_block(next);
        }
        g.b.seal_block(SIRTerminator::Jump(internal_exit, Vec::new()));

        // Several evaluate, then apply, each in event order.
        g.b.switch_to_block(domains);
        if written.len() < 2 {
            g.b.seal_block(SIRTerminator::Jump(internal_exit, Vec::new()));
            g.b.switch_to_block(evaluated);
            g.call_comb(comb, 2);
            g.b.switch_to_block(slot_end);
            g.b.seal_block(SIRTerminator::Jump(header, Vec::new()));
            return g.finish();
        }
        for phase in [Phase::Evaluate, Phase::Apply] {
            for &index in &written {
                let event = &self.events[index];
                let bit = g.constant(1 << index, MAX_EVENTS);
                let selected = g.binary(MAX_EVENTS, mask, BinaryOp::And, bit);
                let this = g.binary(1, selected, BinaryOp::Ne, zero);
                let unit = g.b.new_block();
                let next = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: this,
                    true_block: (unit, Vec::new()),
                    false_block: (next, Vec::new()),
                });
                g.b.switch_to_block(unit);
                let units = match phase {
                    Phase::Evaluate => &event.eval_only,
                    Phase::Apply => &event.apply,
                };
                for unit in units {
                    g.inline(unit, finish_exit);
                }
                g.b.seal_block(SIRTerminator::Jump(next, Vec::new()));
                g.b.switch_to_block(next);
            }
        }
        g.b.seal_block(SIRTerminator::Jump(evaluated, Vec::new()));

        g.b.switch_to_block(evaluated);
        g.call_comb(comb, 2);

        g.b.switch_to_block(slot_end);
        g.b.seal_block(SIRTerminator::Jump(header, Vec::new()));
        g.finish()
    }
}

enum Phase {
    Evaluate,
    Apply,
}

struct Generator<'a> {
    b: Builder,
    slots: &'a SchedulerSlots<AbsoluteAddr>,
    finish_sites: &'a HashSet<u32>,
    /// Exit blocks not yet emitted.
    exits: Vec<(celox_sir::BlockId, SchedulerExit, Option<u64>)>,
}

impl Generator<'_> {
    fn constant(&mut self, value: u64, width: usize) -> RegisterId {
        let register = self.b.alloc_bit(width, false);
        self.b
            .emit(SIRInstruction::Imm(register, SIRValue::new(value)));
        register
    }

    fn binary(
        &mut self,
        width: usize,
        lhs: RegisterId,
        op: BinaryOp,
        rhs: RegisterId,
    ) -> RegisterId {
        let register = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Binary(register, lhs, op, rhs));
        register
    }

    fn equals(&mut self, value: RegisterId, constant: u64) -> RegisterId {
        let width = self.b.register(&value).width();
        let constant = self.constant(constant, width);
        self.binary(1, value, BinaryOp::Eq, constant)
    }

    /// Whether kernel `index` waits for its wake time.
    fn waiting(&mut self, index: usize) -> RegisterId {
        let flag = self.load(self.slots.waiting[index], SCHEDULER_FLAG_WIDTH);
        let zero = self.constant(0, SCHEDULER_FLAG_WIDTH);
        self.binary(1, flag, BinaryOp::Ne, zero)
    }

    fn load(&mut self, address: AbsoluteAddr, width: usize) -> RegisterId {
        let register = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Load(
            register,
            stable(address),
            SIROffset::Static(0),
            width,
        ));
        register
    }

    fn store(&mut self, address: AbsoluteAddr, width: usize, value: RegisterId) {
        self.b.emit(SIRInstruction::Store(
            stable(address),
            SIROffset::Static(0),
            width,
            value,
            Vec::new(),
            Vec::new(),
        ));
    }

    /// The edge level of `event`'s signal, as the runtime reads it: whether
    /// its low byte, within its width, is not zero.
    fn level(&mut self, event: &Event) -> RegisterId {
        let width = event.width.min(8);
        let value = self.load(event.signal, width);
        let zero = self.constant(0, width);
        self.binary(1, value, BinaryOp::Ne, zero)
    }

    /// A block that ends the run with `exit` and its argument, emitted by
    /// [`Self::emit_exits`].
    fn exit_block(&mut self, exit: SchedulerExit, argument: Option<u64>) -> celox_sir::BlockId {
        let block = self.b.new_block();
        self.exits.push((block, exit, argument));
        block
    }

    /// The unit, once every block is sealed.
    fn finish(mut self) -> Unit {
        self.emit_exits();
        let (blocks, register_map, _) = self.b.drain();
        Unit {
            entry_block_id: celox_sir::BlockId(0),
            blocks,
            register_map,
        }
    }

    fn emit_exits(&mut self) {
        for (block, exit, argument) in std::mem::take(&mut self.exits) {
            self.b.switch_to_block(block);
            let code = self.constant(exit as u64, SCHEDULER_FLAG_WIDTH);
            self.store(self.slots.exit, SCHEDULER_FLAG_WIDTH, code);
            if let Some(argument) = argument {
                let argument = self.constant(argument, SCHEDULER_TIME_WIDTH);
                self.store(self.slots.exit_arg, SCHEDULER_TIME_WIDTH, argument);
            }
            self.b.seal_block(SIRTerminator::Return);
        }
    }

    /// Run the shared settle, continuing at case `continuation` of its end.
    fn call_comb(&mut self, comb: celox_sir::BlockId, continuation: u8) {
        let value = self.constant(u64::from(continuation), SCHEDULER_FLAG_WIDTH);
        self.store(self.slots.cont, SCHEDULER_FLAG_WIDTH, value);
        self.b.seal_block(SIRTerminator::Jump(comb, Vec::new()));
    }

    /// Inline `unit`. A `$finish` record it emits ends the run after it, as
    /// the reference loop ends the slot after the unit that emitted one.
    fn inline(&mut self, unit: &Unit, finish_exit: celox_sir::BlockId) {
        if self.finish_sites.is_empty() || !self.emits_finish(unit) {
            self.b.inline_execution_unit(unit);
            return;
        }
        let instrumented = self.instrument(unit);
        self.b.inline_execution_unit(&instrumented);
        let finished = self.load(self.slots.finish, SCHEDULER_FLAG_WIDTH);
        let zero = self.constant(0, SCHEDULER_FLAG_WIDTH);
        let set = self.binary(1, finished, BinaryOp::Ne, zero);
        let next = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: set,
            true_block: (finish_exit, Vec::new()),
            false_block: (next, Vec::new()),
        });
        self.b.switch_to_block(next);
    }

    fn emits_finish(&self, unit: &Unit) -> bool {
        instructions(unit).any(|inst| {
            matches!(inst, SIRInstruction::RuntimeEvent { site_id, .. }
                if self.finish_sites.contains(site_id))
        })
    }

    /// A copy of `unit` that sets the finish slot after every `$finish`
    /// record.
    fn instrument(&self, unit: &Unit) -> Unit {
        let mut unit = unit.clone();
        let mut next = unit
            .register_map
            .keys()
            .map(|register| register.0 + 1)
            .max()
            .unwrap_or(0);
        let mut registers = Vec::new();
        for block in unit.blocks.values_mut() {
            let mut instructions = Vec::with_capacity(block.instructions.len());
            for inst in block.instructions.drain(..) {
                let finish = matches!(&inst, SIRInstruction::RuntimeEvent { site_id, .. }
                    if self.finish_sites.contains(site_id));
                instructions.push(inst);
                if finish {
                    // Each site defines its own register.
                    let one = RegisterId(next);
                    next += 1;
                    registers.push(one);
                    instructions.push(SIRInstruction::Imm(one, SIRValue::new(1u8)));
                    instructions.push(SIRInstruction::Store(
                        stable(self.slots.finish),
                        SIROffset::Static(0),
                        SCHEDULER_FLAG_WIDTH,
                        one,
                        Vec::new(),
                        Vec::new(),
                    ));
                }
            }
            block.instructions = instructions;
        }
        for register in registers {
            unit.register_map.insert(
                register,
                crate::ir::RegisterType::Bit {
                    width: SCHEDULER_FLAG_WIDTH,
                    signed: false,
                },
            );
        }
        unit
    }
}

fn stable(address: AbsoluteAddr) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address)
}

fn instructions(unit: &Unit) -> impl Iterator<Item = &SIRInstruction<RegionedAbsoluteAddr>> {
    unit.blocks.values().flat_map(|block| &block.instructions)
}

/// Whether every status `unit` stores into `status` is a constant that
/// suspends for a delay, ends the process or finishes the simulation.
fn delay_statuses_only(unit: &Unit, status: AbsoluteAddr) -> bool {
    let constants: HashMap<RegisterId, u64> = instructions(unit)
        .filter_map(|inst| match inst {
            SIRInstruction::Imm(register, value) => u64::try_from(&value.payload)
                .ok()
                .map(|value| (*register, value)),
            _ => None,
        })
        .collect();
    let allowed = [
        ProcessStatus::Delay,
        ProcessStatus::InactiveDelay,
        ProcessStatus::Done,
        ProcessStatus::Finish,
    ]
    .map(|status| u64::from(status.code()));
    let status = stable(status);
    instructions(unit).all(|inst| match inst {
        SIRInstruction::Store(target, _, _, value, ..) if *target == status => constants
            .get(value)
            .is_some_and(|code| allowed.contains(code)),
        SIRInstruction::Commit(_, target, ..) => *target != status,
        _ => true,
    })
}
