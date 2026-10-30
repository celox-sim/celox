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
//! A settle whose results nothing reads before the next one is deferred
//! (see [`Lazy`]): it runs only before what observes it, at the point the
//! reference loop ran it.
//!
//! Periodic host clocks (`add_clock`) on clock events no design code writes
//! run in the same loop: their edges apply at the start of a slot, before
//! any kernel, and re-arm at its end. A design without kernels gets the
//! unit for its host clocks alone.
//!
//! Clocks derived from the events the slots change (gated, inverted or
//! divided clocks, which RTL logic drives) run in the same slots: their
//! domains evaluate in the reference loop's waves, which judge each clock
//! against its level at the slot's start (see `Plan::batch`).
//!
//! The runtime runs the unit only for slots that touch nothing else (see
//! `celox_runtime::SimulationState::run_until_observed`): every other slot,
//! and every design or kernel this module does not qualify, runs in the
//! reference loop.

mod slice;
mod testbench;

use slice::CombSlicer;

use celox_design::{
    CounterKind, DomainKind, ProcessStatus, RuntimeEventKind, SCHEDULER_CLOCKS,
    SCHEDULER_FLAG_WIDTH, SCHEDULER_TIME_WIDTH, STABLE_REGION, SchedulerExit, SchedulerSlots,
    TIME_SLOT_WIDTH, edges::LEVEL_UNKNOWN, schedule::ScheduleModel,
};
use celox_sir::{BinaryOp, SIRBuilder, SIRSwitchCase, SIRValue};
use std::collections::BTreeSet;

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

/// The most kernels the ready mask of a slot holds.
const MAX_KERNELS: usize = 64;

/// The widest selector of a nonblocking site index.
const SITE_BITS: usize = 8;

const ACTIVE_COMMIT: &str = "the process drives or waits for a clock whose domain commits blocking results in the active region";

const DELAY_STATUSES: [ProcessStatus; 4] = [
    ProcessStatus::Delay,
    ProcessStatus::InactiveDelay,
    ProcessStatus::Done,
    ProcessStatus::Finish,
];

const WAIT_STATUSES: [ProcessStatus; 4] = [
    ProcessStatus::Wait,
    ProcessStatus::Nba,
    ProcessStatus::Done,
    ProcessStatus::Finish,
];

/// Generate the compiled scheduler of `program`, if it qualifies, and record
/// for every process kernel whether the scheduler runs it.
pub(crate) fn attach(program: &mut LaidOutProgram, wiring_clocks: bool) {
    let Some(schema) = program.runtime.runtime_schema.scheduler.clone() else {
        return;
    };
    let kernels = program.runtime.runtime_schema.processes.len();
    let mut lazy = false;
    let (fallback, unit, events, clocks, kernel_events) =
        match Plan::new(program, &schema.slots, wiring_clocks) {
            Err(reason) => (
                vec![Some(reason); kernels],
                None,
                Vec::new(),
                Vec::new(),
                vec![Vec::new(); kernels],
            ),
            Ok(plan) => {
                lazy = plan.lazy.is_some();
                let fallback = plan.fallback.clone();
                // Without kernels, slots of host clocks alone.
                let unit = (plan.kernels.iter().any(Option::is_some) || !plan.clocks.is_empty())
                    .then(|| plan.generate(program, &schema.slots));
                let tracked = plan.tracked();
                let kernel_events = plan
                    .kernels
                    .iter()
                    .map(|kernel| {
                        kernel
                            .iter()
                            .flat_map(|kernel| &kernel.writes)
                            .map(|event| {
                                tracked
                                    .binary_search(event)
                                    .expect("the scheduler's kernels write tracked events")
                            })
                            .collect()
                    })
                    .collect();
                let events = tracked
                    .into_iter()
                    .map(|event| plan.events[event].signal)
                    .collect();
                let clocks = plan
                    .clocks
                    .iter()
                    .map(|&event| plan.events[event].signal)
                    .collect();
                (fallback, unit, events, clocks, kernel_events)
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
    scheduler.clocks = clocks;
    scheduler.kernel_events = kernel_events;
    if let Some(unit) = unit {
        scheduler.unit = Some(program.sir.processes.len());
        program.sir.processes.push(unit);
    }
    testbench::attach(program, lazy);
}

/// `program` with its compiled scheduler, and the testbench unit that
/// inlines it, generated without fused domain units: a domain that reads
/// combinational state settles first, so the units hold one copy of the
/// settle instead of one per domain. A first execution tier compiles them
/// sooner and runs the same schedule.
pub(crate) fn without_fused_domains(
    mut program: LaidOutProgram,
    wiring_clocks: bool,
) -> LaidOutProgram {
    let Some(schema) = program.runtime.runtime_schema.scheduler.clone() else {
        return program;
    };
    let Some(unit) = schema.unit else {
        return program;
    };
    let Ok(mut plan) = Plan::new(&program, &schema.slots, wiring_clocks) else {
        return program;
    };
    let Some(lazy) = &mut plan.lazy else {
        return program;
    };
    let mut fused = false;
    for domain in &mut lazy.domains {
        if matches!(domain, DomainSettle::Fused(_)) {
            *domain = DomainSettle::Settled;
            fused = true;
        }
    }
    if !fused {
        return program;
    }
    program.sir.processes[unit] = plan.generate(&program, &schema.slots);
    testbench::regenerate(&mut program, true);
    program
}

/// An event the scheduler records edges of.
struct Event {
    /// The canonical event address, which is also its signal.
    signal: AbsoluteAddr,
    width: usize,
    /// The level of the signal after an active edge.
    active: u8,
    /// A clock edge rather than an asynchronous reset.
    clock: bool,
    eval_apply: Vec<Unit>,
    /// Wiring-only clock changes do not invalidate the datapath.
    aliases: Option<Vec<Unit>>,
    /// The units of the path of several triggered domains, which a design
    /// without kernels lacks; required when more than one event can have an
    /// edge in a slot.
    eval_only: Option<Vec<Unit>>,
    apply: Option<Vec<Unit>>,
}

/// A kernel the scheduler runs.
struct Kernel {
    /// The events whose signal the kernel writes, as indices into
    /// [`Plan::events`].
    writes: Vec<usize>,
    /// For a kernel that waits for one edge, the edge and its staged
    /// nonblocking assignments; `None` for a kernel that only delays.
    wait: Option<Wait>,
}

/// The single edge a kernel waits for at every wait site.
struct Wait {
    signal: AbsoluteAddr,
    kind: CounterKind,
    /// Its nonblocking sites, by local index.
    sites: Vec<Staged>,
}

/// A nonblocking site that stages its value and sets a pending flag.
struct Staged {
    pending: AbsoluteAddr,
    value: AbsoluteAddr,
    target: AbsoluteAddr,
    lsb: usize,
    width: usize,
}

struct Plan {
    /// In event-id order, which the reference loop visits several triggered
    /// domains in.
    events: Vec<Event>,
    /// Per process kernel, `Some` when the scheduler runs it.
    kernels: Vec<Option<Kernel>>,
    /// The events a host periodic clock may drive in the scheduler's slots,
    /// as indices into [`Self::events`], in event order: the bits of its
    /// clock slots.
    clocks: Vec<usize>,
    fallback: Vec<Option<String>>,
    /// Runtime event sites that record a `$finish`.
    finish_sites: HashSet<u32>,
    /// The storage combinational logic reads, when a settle whose inputs did
    /// not change may be skipped; `None` when every settle runs.
    settle_reads: Option<HashSet<AbsoluteAddr>>,
    /// Where the settles run when they wait until their results are read;
    /// `None` when every settle runs where the reference loop's does.
    lazy: Option<Lazy>,
    /// The clocks derived from the events the slots change, in event order,
    /// as indices into [`Self::events`]: their domains run in the waves of
    /// a slot (see `Generator::batch`).
    derived: Vec<usize>,
    /// The events the slots change that gate clocks computed
    /// combinationally from them, with those clocks: their edges are judged
    /// when the source changes.
    gating: Vec<(usize, Vec<usize>)>,
}

/// Deferred settles. The reference loop settles after every pass of the
/// process phase and after the NBA region; the unit keeps such a settle
/// pending (the dirty flag stays set) until something observes its
/// results, and runs it where the reference ran it last, so every observer
/// sees what it sees in the reference: a kernel, before the slot it may run
/// in; a domain, before it evaluates (or the fused unit computes what it
/// reads); the host, before the run returns. Observers of the settled state
/// are units that load storage combinational logic writes, and units that
/// may end the run in the middle of a slot (`$finish`, an error), where the
/// reference's state is the last settle's. Only a settle that is a function
/// of its inputs may wait: one that reads what an earlier settle left (see
/// [`retains_state`]) is an activation of its own.
struct Lazy {
    /// Per process kernel, whether its runs observe the settled state.
    kernels: Vec<bool>,
    /// Per event, how its domain evaluates with a settle pending.
    domains: Vec<DomainSettle>,
    /// Whether the units of several triggered domains observe the settled
    /// state.
    several: bool,
    /// The latest slot from which no delay of a kernel overflows: a run that
    /// returns for an overflow in the middle of a later slot returns the last
    /// settle's state.
    latest: u64,
    /// The part of the settle that keeps state from one settle to the next,
    /// with what it reads: it runs wherever the reference settles, so its
    /// state sees every activation the reference's does. Rerunning it on
    /// unchanged inputs changes nothing, so the deferred settle may run it
    /// again. Empty when the settle is a function of its inputs.
    eager: Vec<Unit>,
}

/// How a domain evaluates while a settle is pending.
enum DomainSettle {
    /// It does not observe the settled state.
    Pending,
    /// It reads what combinational logic writes: its fused units compute
    /// what it reads, then evaluate and apply, leaving the settle pending.
    Fused(Vec<Unit>),
    /// The settle runs first.
    Settled,
}

impl Plan {
    fn new(
        program: &LaidOutProgram,
        slots: &SchedulerSlots<AbsoluteAddr>,
        wiring_clocks: bool,
    ) -> Result<Self, String> {
        let schema = &program.runtime.runtime_schema;
        let model = ScheduleModel::derive(&program.design, schema);
        if !model.counted.is_empty() {
            return Err("the design counts event occurrences".into());
        }
        let topology = &model.events;
        if !schema.comb_observers.is_empty() {
            return Err("the design observes combinational processes".into());
        }
        const _: () = assert!(MAX_EVENTS <= SCHEDULER_CLOCKS);
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
            let (active, clock) = match model.kinds.get(&signal) {
                Some(DomainKind::ClockPosedge) => (1, true),
                Some(DomainKind::ClockNegedge) => (0, true),
                Some(DomainKind::ResetAsyncHigh) => (1, false),
                Some(DomainKind::ResetAsyncLow) => (0, false),
                _ => return Err("an event is not a clock or reset edge".into()),
            };
            let units =
                |map: &HashMap<AbsoluteAddr, Vec<Unit>>| -> Result<Option<Vec<Unit>>, String> {
                    // An alias's domain must be the event's own; one with
                    // nothing to run is no domain at all.
                    let aliased = map.iter().any(|(key, units)| {
                        *key != signal && topology.canonical(*key) == signal && !units.is_empty()
                    });
                    if aliased {
                        return Err("an event domain has no separate units".into());
                    }
                    Ok(map.get(&signal).cloned())
                };
            let eval_only = units(&sir.eval_only_ffs)?;
            let apply = units(&sir.apply_ffs)?;
            // An event no RTL process waits for, such as a TB clock that
            // only paces initial processes, has an empty domain.
            let eval_apply = match units(&sir.eval_apply_ffs)? {
                Some(units) => units,
                None if eval_only.is_none() && apply.is_none() => Vec::new(),
                None => return Err("an event domain has no separate units".into()),
            };
            // Nothing to evaluate is also nothing to apply.
            let (eval_only, apply) = if eval_apply.is_empty() {
                (eval_only.or(Some(Vec::new())), apply.or(Some(Vec::new())))
            } else {
                (eval_only, apply)
            };
            let event = Event {
                signal,
                width: *program
                    .layout()
                    .widths
                    .get(&signal)
                    .ok_or("an event signal has no storage")?,
                active,
                clock,
                eval_apply,
                aliases: wiring_clocks
                    .then(|| crate::clock_aliases::extract(program, signal))
                    .flatten(),
                eval_only,
                apply,
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
        let rtl_units = || {
            sir.eval_comb
                .iter()
                .chain(sir.eval_apply_ffs.values().flatten())
                .chain(sir.eval_only_ffs.values().flatten())
                .chain(sir.apply_ffs.values().flatten())
        };
        // The events RTL logic drives: clocks derived from the events the
        // scheduler's slots change, and only those (checked below).
        let rtl_written: BTreeSet<usize> = rtl_units().flat_map(written).collect();
        // A domain that commits its blocking results in the active region
        // evaluates before the NBA region when it triggers; a kernel that
        // may trigger it keeps the reference loop.
        let active_commits: HashSet<usize> = events
            .iter()
            .enumerate()
            .filter(|(_, event)| model.active_commits.contains(&event.signal))
            .map(|(index, _)| index)
            .collect();
        // The storage each unit writes.
        let stores = |unit: &Unit| -> HashSet<AbsoluteAddr> {
            instructions(unit)
                .filter_map(|inst| match inst {
                    SIRInstruction::Store(target, ..) | SIRInstruction::Commit(_, target, ..) => {
                        Some(storage(target.absolute_addr()))
                    }
                    _ => None,
                })
                .collect()
        };
        let rtl_stores: HashSet<AbsoluteAddr> = rtl_units().flat_map(stores).collect();
        let kernel_stores: Vec<HashSet<AbsoluteAddr>> = sir
            .processes
            .iter()
            .take(schema.processes.len())
            .map(stores)
            .collect();
        let waited: HashSet<AbsoluteAddr> = model.waited.iter().map(|s| storage(*s)).collect();
        // Storage whose changes bump an occurrence counter: the reference
        // counts the nonblocking updates it publishes into it, which the
        // scheduler's publication does not.
        let probed: HashSet<AbsoluteAddr> = model
            .probes
            .iter()
            .map(|(source, _)| storage(source.id))
            .collect();
        let mut nba_targets: HashMap<AbsoluteAddr, usize> = HashMap::default();
        for process in &model.processes {
            for site in &process.nbas {
                *nba_targets.entry(storage(site.target)).or_default() += 1;
            }
        }
        // Storage no design code writes: only the host changes it, by
        // writes between runs or by a periodic clock at the start of a slot.
        // Clock slots go to the clock events among it: each costs the unit
        // its domains' code even when no clock runs, and the host drives
        // asynchronous resets by writes.
        let undriven = |signal: AbsoluteAddr| {
            !rtl_stores.contains(&signal)
                && !kernel_stores
                    .iter()
                    .zip(&schema.processes)
                    .any(|(stores, process)| {
                        process.kind != celox_design::ProcessKind::Callable
                            && stores.contains(&signal)
                    })
                && !nba_targets.contains_key(&signal)
        };
        let clocks: Vec<usize> = events
            .iter()
            .enumerate()
            .filter(|(index, event)| {
                event.clock
                    && event.width == 1
                    && !active_commits.contains(index)
                    && undriven(storage(event.signal))
            })
            .map(|(index, _)| index)
            .collect();

        let mut kernels: Vec<Option<Kernel>> = Vec::with_capacity(schema.processes.len());
        let mut fallback = Vec::with_capacity(schema.processes.len());
        // Kernels that only delay first: the signals they write are those a
        // waiting kernel's edge may be on.
        for (index, slots) in schema.processes.iter().enumerate() {
            let process = &model.processes[index];
            let unit = &sir.processes[index];
            let reason = if slots.kind == celox_design::ProcessKind::Callable {
                Some("an explicitly invoked kernel")
            } else if slots.kind == celox_design::ProcessKind::Initializer {
                Some("a declaration initializer")
            } else if slots.kind == celox_design::ProcessKind::Final {
                Some("a final procedure")
            } else if process.frame.is_some() {
                Some("the process has dynamic activation frames")
            } else if slots.program.is_some() {
                Some("a program process runs in the reactive region set")
            } else if slots.requires_timed_simulation {
                Some("an edge procedure")
            } else if !process.host_requests.is_empty() {
                Some("the process makes host requests")
            } else if !process.waits.is_empty() {
                None
            } else if !slots.const_delays {
                Some("a delay is not a constant of at least one time unit")
            } else if !process.nbas.is_empty() {
                Some("the process has nonblocking assignments")
            } else if !statuses_only(unit, slots.status, &DELAY_STATUSES) {
                Some("the process suspends other than for a delay")
            } else {
                None
            };
            match reason {
                Some(reason) => {
                    kernels.push(None);
                    fallback.push(Some(reason.to_string()));
                }
                None if process.waits.is_empty() => {
                    let writes = written(unit);
                    if writes.iter().any(|event| active_commits.contains(event)) {
                        kernels.push(None);
                        fallback.push(Some(ACTIVE_COMMIT.to_string()));
                    } else {
                        kernels.push(Some(Kernel { writes, wait: None }));
                        fallback.push(None);
                    }
                }
                None => {
                    kernels.push(None);
                    fallback.push(None);
                }
            }
        }
        // How many kernels that only delay write each storage. A waiting
        // kernel's signal has one such writer, which runs once a slot, so
        // the waiting kernel wakes at most once a slot.
        let mut delay_writers: HashMap<AbsoluteAddr, usize> = HashMap::default();
        for (_, stores) in kernels
            .iter()
            .zip(&kernel_stores)
            .filter(|(kernel, _)| kernel.is_some())
        {
            for store in stores {
                *delay_writers.entry(*store).or_default() += 1;
            }
        }
        // Storage the other kernels write, waiting ones included.
        let other_stores: HashSet<AbsoluteAddr> = kernels
            .iter()
            .zip(&kernel_stores)
            .filter(|(kernel, _)| kernel.is_none())
            .flat_map(|(_, stores)| stores.iter().copied())
            .collect();
        for index in 0..schema.processes.len() {
            let process = &model.processes[index];
            if process.waits.is_empty() || fallback[index].is_some() {
                continue;
            }
            let slots = &schema.processes[index];
            let unit = &sir.processes[index];
            let term = &process.waits[0].first();
            let wait = (|| -> Result<Wait, &'static str> {
                let term = term.ok_or("the process waits for a level")?;
                // The compiled wait tests the edge alone (IEEE 1800-2023
                // 9.4.2.3: an `iff` condition is judged at the occurrence).
                if term.gate.is_some() {
                    return Err("the process waits with an iff condition");
                }
                if process
                    .waits
                    .iter()
                    .any(|terms| terms.len() != 1 || terms[0] != *term)
                {
                    return Err("the process waits for more than one edge");
                }
                let signal = storage(term.signal);
                let delayed = delay_writers.get(&signal) == Some(&1)
                    && !rtl_stores.contains(&signal)
                    && !other_stores.contains(&signal);
                if program.layout().widths.get(&term.signal) != Some(&1)
                    || !(delayed || undriven(signal))
                {
                    return Err(
                        "the process waits for a signal that neither one kernel that only delays nor the host alone drives",
                    );
                }
                if event_storage
                    .get(&signal)
                    .is_some_and(|event| active_commits.contains(event))
                {
                    return Err(ACTIVE_COMMIT);
                }
                if !statuses_only(unit, slots.status, &WAIT_STATUSES) {
                    return Err("the process suspends other than for one edge");
                }
                let writes = &kernel_stores[index];
                if !written(unit).is_empty() || writes.iter().any(|store| waited.contains(store)) {
                    return Err("the process drives an event or a waited signal");
                }
                let sites = process
                    .nbas
                    .iter()
                    .map(|site| {
                        let target = storage(site.target);
                        let pending = site.pending.ok_or("a nonblocking site queues its values")?;
                        // A position computed at run time, or an element of
                        // an unpacked array, which the runtime merges through
                        // the array's layout, needs the runtime's publication.
                        if site.position.is_some()
                            || program.layout().unpacked_arrays.contains_key(&site.target)
                            || nba_targets.get(&target) != Some(&1)
                            || rtl_stores.contains(&target)
                            || kernel_stores.iter().any(|stores| stores.contains(&target))
                            || waited.contains(&target)
                            || probed.contains(&target)
                            || event_storage.contains_key(&target)
                        {
                            return Err("a nonblocking target has other writers or observers");
                        }
                        Ok(Staged {
                            pending,
                            value: site.value,
                            target: site.target,
                            lsb: site.lsb,
                            width: site.width,
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if sites.len() > 1 << SITE_BITS {
                    return Err("the process has too many nonblocking sites");
                }
                Ok(Wait {
                    signal: term.signal,
                    kind: term.kind,
                    sites,
                })
            })();
            match wait {
                Ok(wait) => {
                    kernels[index] = Some(Kernel {
                        writes: Vec::new(),
                        wait: Some(wait),
                    })
                }
                Err(reason) => fallback[index] = Some(reason.to_string()),
            }
        }
        if kernels.iter().flatten().count() > MAX_KERNELS {
            return Err(format!(
                "the design has more than {MAX_KERNELS} kernels to compile"
            ));
        }
        let finish_sites = schema
            .runtime_event_sites
            .iter()
            .enumerate()
            .filter(|(_, site)| matches!(site.kind, RuntimeEventKind::Finish))
            .map(|(index, _)| index as u32)
            .collect();
        // A settle is the same function of the state it reads every time,
        // unless combinational logic has effects of its own: records,
        // captures or external calls. Clock/reset trigger bits are internal
        // notifications: this scheduler compares event levels itself, and
        // its caller clears the bits before returning to the reference loop.
        // Keep unknown notifications conservative.
        let internal_trigger = |trigger: &celox_design::TriggerIdWithKind| {
            addresses
                .get(trigger.id)
                .is_some_and(|signal| model.kinds.get(signal) == Some(&trigger.kind))
        };
        let effects = sir.eval_comb.iter().any(|unit| {
            instructions(unit).any(|inst| match inst {
                SIRInstruction::RuntimeEvent { .. }
                | SIRInstruction::CombCaptureEvent { .. }
                | SIRInstruction::CombCaptureEnableIfChanged { .. }
                | SIRInstruction::ExternCall { .. } => true,
                SIRInstruction::Store(_, _, _, _, triggers, sites) => {
                    triggers.iter().any(|trigger| !internal_trigger(trigger)) || !sites.is_empty()
                }
                SIRInstruction::Commit(_, _, _, _, triggers) => {
                    triggers.iter().any(|trigger| !internal_trigger(trigger))
                }
                _ => false,
            })
        });
        // Dirty tracking compares stable homes by their representative
        // (`shared_storage`), which groups addresses only by an equal start
        // offset: no two homes may overlap from different offsets.
        debug_assert!(
            homes_overlap_only_whole(program.layout()),
            "stable homes overlap at different offsets"
        );
        let settle_reads = (!effects).then(|| {
            sir.eval_comb
                .iter()
                .flat_map(instructions)
                .filter_map(|inst| match inst {
                    SIRInstruction::Load(_, source, ..) | SIRInstruction::Commit(source, ..) => {
                        Some(storage(source.absolute_addr()))
                    }
                    _ => None,
                })
                .collect()
        });
        // A settle may wait for its readers only when it is a function of
        // its inputs that cannot fail: when it would end the run, it must do
        // so in its own slot; when it reads what an earlier settle left (a
        // latch, a guarded process's snapshots of its inputs, feedback), the
        // settles in between are activations of their own. The comb
        // scheduler knows this from its bit-level dependencies; its loads
        // alone can only bound it, since a load may cover bits the settle
        // has not stored yet and then discard them.
        let comb_stores: HashSet<AbsoluteAddr> = sir.eval_comb.iter().flat_map(stores).collect();
        let eager = settle_reads
            .as_ref()
            .filter(|_| !sir.eval_comb.iter().any(fails))
            .and_then(|_| {
                let stateful = sir.eval_comb_stateful.as_ref();
                let eager = stateful.and_then(|stateful| {
                    if stateful.is_empty() {
                        return Some(Vec::new());
                    }
                    let slicer = CombSlicer::new(&sir.eval_comb, shared.clone());
                    let targets = stateful.iter().map(|target| target.absolute_addr());
                    // Optimization may have moved a target's storage.
                    if !targets.clone().all(|target| slicer.stores(target)) {
                        return None;
                    }
                    let eager = slicer.slice(targets);
                    // A part nearly the size of the settle saves nothing.
                    let size = |units: &[Unit]| units.iter().flat_map(instructions).count();
                    (size(&eager) * 2 < size(&sir.eval_comb)).then_some(eager)
                });
                eager.or_else(|| {
                    (!retains_state(&sir.eval_comb, &storage, &comb_stores)).then(Vec::new)
                })
            });
        let lazy = eager.map(|eager| {
            // The waits never watch what combinational logic writes, so
            // they see the same with a settle pending.
            debug_assert!(
                kernels
                    .iter()
                    .flatten()
                    .filter_map(|kernel| kernel.wait.as_ref())
                    .all(|wait| !comb_stores.contains(&storage(wait.signal))),
                "a compiled wait watches a combinational output"
            );
            let reads =
                |unit: &Unit| loads(unit).any(|source| comb_stores.contains(&storage(source)));
            let ends = |unit: &Unit| fails(unit) || emits_finish(unit, &finish_sites);
            let observes = |unit: &Unit| reads(unit) || ends(unit);
            let mut longest = 0;
            for (index, kernel) in kernels.iter().enumerate() {
                if kernel.as_ref().is_some_and(|kernel| kernel.wait.is_none()) {
                    let bound = delay_bound(&sir.processes[index], schema.processes[index].delay);
                    longest = longest.max(bound);
                }
            }
            let domains = events
                .iter()
                .map(|event| {
                    if event.eval_apply.iter().any(ends) {
                        return DomainSettle::Settled;
                    }
                    if !event.eval_apply.iter().any(reads) {
                        return DomainSettle::Pending;
                    }
                    let fused = &sir.eval_comb_apply_ffs;
                    let aliased = fused.keys().any(|key| {
                        *key != event.signal && topology.canonical(*key) == event.signal
                    });
                    match fused.get(&event.signal) {
                        Some(units) if !aliased => DomainSettle::Fused(units.clone()),
                        _ => DomainSettle::Settled,
                    }
                })
                .collect();
            let several = events.iter().any(|event| {
                event
                    .eval_only
                    .iter()
                    .chain(&event.apply)
                    .flatten()
                    .any(observes)
            });
            Lazy {
                // A kernel's `$finish` is its status.
                kernels: kernels
                    .iter()
                    .enumerate()
                    .map(|(index, kernel)| {
                        let unit = &sir.processes[index];
                        kernel.is_some()
                            && (observes(unit)
                                || !statuses_only(
                                    unit,
                                    schema.processes[index].status,
                                    &[
                                        ProcessStatus::Delay,
                                        ProcessStatus::InactiveDelay,
                                        ProcessStatus::Wait,
                                        ProcessStatus::Nba,
                                        ProcessStatus::Done,
                                    ],
                                ))
                    })
                    .collect(),
                domains,
                several,
                latest: u64::MAX - longest,
                eager,
            }
        });
        let mut plan = Self {
            events,
            kernels,
            clocks,
            fallback,
            finish_sites,
            settle_reads,
            lazy,
            derived: Vec::new(),
            gating: Vec::new(),
        };
        let edged = plan.edged();
        // The clocks derived from the events the slots change, transitively:
        // their levels change only through those events' domains and
        // combinational logic.
        let index_of: HashMap<AbsoluteAddr, usize> = plan
            .events
            .iter()
            .enumerate()
            .map(|(index, event)| (event.signal, index))
            .collect();
        let derived_from = |event: usize| -> Vec<usize> {
            let signal = plan.events[event].signal;
            topology
                .derived_events
                .get(&signal)
                .into_iter()
                .flatten()
                .copied()
                .chain(
                    topology
                        .combinational_derived
                        .get(&signal)
                        .into_iter()
                        .flatten()
                        .map(|clock| clock.clock),
                )
                .filter_map(|address| index_of.get(&topology.canonical(address)).copied())
                .collect()
        };
        let mut derived = BTreeSet::new();
        let mut pending = edged.clone();
        while let Some(event) = pending.pop() {
            for next in derived_from(event) {
                if !edged.contains(&next) && derived.insert(next) {
                    pending.push(next);
                }
            }
        }
        // A combinational reset (or clock) driven only by host-owned inputs
        // is stable throughout a compiled run. Host writes are settled and
        // their edges handled before entering the scheduler. Callable kernels,
        // like host writes, can only run outside this unit.
        let scheduled_stores: HashSet<AbsoluteAddr> = sir
            .eval_apply_ffs
            .values()
            .flatten()
            .chain(sir.eval_only_ffs.values().flatten())
            .chain(sir.apply_ffs.values().flatten())
            .flat_map(stores)
            .chain(
                kernel_stores
                    .iter()
                    .zip(&schema.processes)
                    .filter(|(_, process)| process.kind != celox_design::ProcessKind::Callable)
                    .flat_map(|(stores, _)| stores.iter().copied()),
            )
            .chain(nba_targets.keys().copied())
            .collect();
        let clocked_storage: HashSet<_> = edged
            .iter()
            .map(|&event| storage(plan.events[event].signal))
            .collect();
        let stable_input = |address| {
            let address = storage(address);
            undriven(address) && !clocked_storage.contains(&address)
        };
        let stable_host_events: BTreeSet<usize> = topology
            .combinational_derived
            .iter()
            .filter(|(source, _)| stable_input(**source))
            .flat_map(|(_, clocks)| clocks)
            .filter(|clock| !scheduled_stores.contains(&storage(clock.clock)))
            .filter(|clock| {
                clock.gate.as_ref().is_some_and(|gate| {
                    gate.iter().all(|op| match op {
                        celox_design::GateOp::Load { address, .. } => stable_input(*address),
                        _ => true,
                    })
                })
            })
            .filter_map(|clock| index_of.get(&topology.canonical(clock.clock)).copied())
            .collect();
        if rtl_written
            .difference(&derived)
            .any(|event| !stable_host_events.contains(event))
        {
            return Err("RTL logic drives an event signal".into());
        }
        let gating: Vec<(usize, Vec<usize>)> = edged
            .iter()
            .filter_map(|&source| {
                let signal = plan.events[source].signal;
                let gated: Vec<usize> = topology
                    .combinational_derived
                    .get(&signal)
                    .into_iter()
                    .flatten()
                    .filter_map(|clock| index_of.get(&topology.canonical(clock.clock)).copied())
                    .filter(|gated| derived.contains(gated))
                    .collect();
                (!gated.is_empty()).then_some((source, gated))
            })
            .collect();
        plan.derived = derived.into_iter().collect();
        plan.gating = gating;
        let tracked = plan.tracked();
        if !plan.derived.is_empty() {
            // The domains of a slot evaluate in waves until none triggers
            // (see `Generator::batch`), which these the reference loop
            // runs otherwise keep out of.
            if plan
                .kernels
                .iter()
                .flatten()
                .filter_map(|kernel| kernel.wait.as_ref())
                .any(|wait| !wait.sites.is_empty())
            {
                return Err("a design with derived clocks stages nonblocking assignments".into());
            }
            if tracked.iter().any(|event| active_commits.contains(event)) {
                return Err(ACTIVE_COMMIT.into());
            }
            // A gated clock is judged at the settle after its source changes,
            // before a kernel the change wakes runs in another pass; what that
            // kernel writes moves the clock only for the waves after the
            // commit. The waves judge once, after every pass.
            if !plan.gating.is_empty() {
                let comb_reads: HashSet<AbsoluteAddr> =
                    sir.eval_comb.iter().flat_map(loads).map(storage).collect();
                let feeds_gates = plan.kernels.iter().enumerate().any(|(index, kernel)| {
                    kernel.as_ref().is_some_and(|kernel| kernel.wait.is_some())
                        && kernel_stores[index]
                            .iter()
                            .any(|store| comb_reads.contains(store))
                });
                if feeds_gates {
                    return Err(
                        "a waiting kernel writes what combinational logic of a gated clock reads"
                            .into(),
                    );
                }
            }
            let ends = |unit: &Unit| fails(unit) || emits_finish(unit, &plan.finish_sites);
            if tracked.iter().any(|&event| {
                let event = &plan.events[event];
                event
                    .eval_apply
                    .iter()
                    .chain(event.eval_only.iter().flatten())
                    .chain(event.apply.iter().flatten())
                    .any(ends)
            }) {
                return Err("a domain of a design with derived clocks may end the run".into());
            }
        }
        if tracked.len() > 1
            && tracked.iter().any(|&event| {
                plan.events[event].eval_only.is_none() || plan.events[event].apply.is_none()
            })
        {
            return Err("an event domain has no separate units".into());
        }
        Ok(plan)
    }

    /// The events whose levels the slots record and compare: those that
    /// can have edges in them and the clocks derived from those, in event
    /// order: the bits of [`SchedulerSlots::levels`].
    fn tracked(&self) -> Vec<usize> {
        let mut tracked = self.edged();
        tracked.extend(&self.derived);
        tracked.sort_unstable();
        tracked
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

    /// The events that can have edges in the scheduler's slots, in event
    /// order: those its kernels write and those its host clocks may drive.
    fn edged(&self) -> Vec<usize> {
        let mut edged = self.written();
        edged.extend(&self.clocks);
        edged.sort_unstable();
        edged.dedup();
        edged
    }

    fn generate(&self, program: &LaidOutProgram, slots: &SchedulerSlots<AbsoluteAddr>) -> Unit {
        let schema = &program.runtime.runtime_schema;
        let edged = self.edged();
        let tracked = self.tracked();
        let mut b = Builder::new();
        let comb = b.new_block();
        let mut g = Generator {
            b,
            slots,
            finish_sites: &self.finish_sites,
            exits: Vec::new(),
            settle_reads: self.settle_reads.as_ref(),
            shared: program.shared_storage(),
            comb,
            continuations: Vec::new(),
            eager: None,
            eager_continuations: Vec::new(),
            four_state: program
                .layout()
                .four_state
                .then_some(&program.layout().is_4states),
        };
        let waits: Vec<(usize, &Wait)> = self
            .kernels
            .iter()
            .enumerate()
            .filter_map(|(index, kernel)| Some((index, kernel.as_ref()?.wait.as_ref()?)))
            .collect();
        let delays: Vec<usize> = self
            .kernels
            .iter()
            .enumerate()
            .filter(|(_, kernel)| kernel.as_ref().is_some_and(|kernel| kernel.wait.is_none()))
            .map(|(index, _)| index)
            .collect();
        // The bit of each kernel in the ready mask: its place among the
        // kernels the scheduler runs, in kernel order.
        let ordinals = ordinals(&self.kernels);
        // The caller supplies the dirty flag. Timed runs must start dirty:
        // checkpoint restores and partial-slot exits need a fresh settle.
        // A host clock batch may instead enter with already settled inputs.
        let header = g.b.new_block();
        g.b.seal_block(SIRTerminator::Jump(header, Vec::new()));

        // The next slot: the earliest wake time of a kernel waiting for one.
        g.b.switch_to_block(header);
        let mut time = g.constant(u64::MAX, SCHEDULER_TIME_WIDTH);
        let mut any = g.constant(0, 1);
        for &index in &delays {
            let wake = g.load(slots.wake[index], SCHEDULER_TIME_WIDTH);
            let waiting = g.waiting(index);
            let earlier = g.binary(1, wake, BinaryOp::LtU, time);
            let earlier = g.binary(1, earlier, BinaryOp::And, waiting);
            let next = g.b.alloc_bit(SCHEDULER_TIME_WIDTH, false);
            g.b.emit(SIRInstruction::Mux(next, earlier, wake, time));
            time = next;
            any = g.binary(1, any, BinaryOp::Or, waiting);
        }
        // Or the earliest next edge of a host clock, when the run has any.
        // The clocks armed at the start of the slot, which the re-arm at its
        // end compares with.
        let mut started = None;
        if !self.clocks.is_empty() {
            let armed = g.load(slots.clock_armed, SCHEDULER_TIME_WIDTH);
            let zero = g.constant(0, SCHEDULER_CLOCKS);
            let clocked = g.binary(1, armed, BinaryOp::Ne, zero);
            started = Some((armed, clocked));
            let scan = g.b.new_block();
            let joined_time = g.b.alloc_bit(SCHEDULER_TIME_WIDTH, false);
            let joined_any = g.b.alloc_bit(1, false);
            let join = g.b.new_block_with(vec![joined_time, joined_any]);
            g.b.seal_block(SIRTerminator::Branch {
                cond: clocked,
                true_block: (scan, Vec::new()),
                false_block: (join, vec![time, any]),
            });
            g.b.switch_to_block(scan);
            for bit in 0..self.clocks.len() {
                let next_edge = g.clock_next(bit);
                let on = g.mask_bit(armed, bit);
                let earlier = g.binary(1, next_edge, BinaryOp::LtU, time);
                let earlier = g.binary(1, earlier, BinaryOp::And, on);
                let next = g.b.alloc_bit(SCHEDULER_TIME_WIDTH, false);
                g.b.emit(SIRInstruction::Mux(next, earlier, next_edge, time));
                time = next;
                any = g.binary(1, any, BinaryOp::Or, on);
            }
            g.b.seal_block(SIRTerminator::Jump(join, vec![time, any]));
            g.b.switch_to_block(join);
            time = joined_time;
            any = joined_any;
        }
        let limit = g.load(slots.limit, SCHEDULER_TIME_WIDTH);
        let after = g.binary(1, time, BinaryOp::GtU, limit);
        let one = g.constant(1, 1);
        let none = g.binary(1, any, BinaryOp::Xor, one);
        let stop = g.binary(1, after, BinaryOp::Or, none);
        let run = g.b.new_block();
        let limit_exit = g.exit_block(SchedulerExit::Limit);
        // The host reads the run's results settled, unless the caller
        // observes them itself (`defer_limit_settle`).
        let stopped = match &self.lazy {
            Some(_) => g.b.new_block(),
            None => limit_exit,
        };
        g.b.seal_block(SIRTerminator::Branch {
            cond: stop,
            true_block: (stopped, Vec::new()),
            false_block: (run, Vec::new()),
        });
        if self.lazy.is_some() {
            g.b.switch_to_block(stopped);
            let deferred = g.load(slots.defer_limit_settle, SCHEDULER_FLAG_WIDTH);
            let zero = g.constant(0, SCHEDULER_FLAG_WIDTH);
            let deferred = g.binary(1, deferred, BinaryOp::Ne, zero);
            let settle = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: deferred,
                true_block: (limit_exit, Vec::new()),
                false_block: (settle, Vec::new()),
            });
            g.b.switch_to_block(settle);
            g.settle_then(limit_exit);
        }

        // The kernels due now are ready.
        g.b.switch_to_block(run);
        let mut ready = g.constant(0, MAX_KERNELS);
        for &index in &delays {
            let wake = g.load(slots.wake[index], SCHEDULER_TIME_WIDTH);
            let waiting = g.waiting(index);
            let due = g.binary(1, wake, BinaryOp::Eq, time);
            let due = g.binary(1, due, BinaryOp::And, waiting);
            let bit = g.bit(due, ordinals[index]);
            ready = g.binary(MAX_KERNELS, ready, BinaryOp::Or, bit);
        }
        g.store(slots.ready, SCHEDULER_TIME_WIDTH, ready);
        // A pending settle runs, as at the end of the last slot, before a
        // kernel that observes it may run: one due now, any waiting one,
        // which may wake in any pass, or any one in a slot after which a
        // delay may overflow.
        if let Some(lazy) = &self.lazy {
            let waits = waits.iter().any(|&(index, _)| lazy.kernels[index]);
            let observers = delays
                .iter()
                .filter(|&&index| lazy.kernels[index])
                .fold(0u64, |mask, &index| mask | 1 << ordinals[index]);
            let late = lazy.latest < u64::MAX;
            if waits || observers != 0 || late {
                let mut observed = g.constant(u64::from(waits), 1);
                if observers != 0 {
                    let mask = g.constant(observers, MAX_KERNELS);
                    let due = g.binary(MAX_KERNELS, ready, BinaryOp::And, mask);
                    let zero = g.constant(0, MAX_KERNELS);
                    let due = g.binary(1, due, BinaryOp::Ne, zero);
                    observed = g.binary(1, observed, BinaryOp::Or, due);
                }
                if late {
                    let latest = g.constant(lazy.latest, SCHEDULER_TIME_WIDTH);
                    let after = g.binary(1, time, BinaryOp::GtU, latest);
                    observed = g.binary(1, observed, BinaryOp::Or, after);
                }
                let settle = g.b.new_block();
                let slot = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: observed,
                    true_block: (settle, Vec::new()),
                    false_block: (slot, Vec::new()),
                });
                g.b.switch_to_block(settle);
                g.settle_then(slot);
                g.b.switch_to_block(slot);
            }
        }
        g.store(slots.now, SCHEDULER_TIME_WIDTH, time);
        let count = g.load(slots.count, SCHEDULER_TIME_WIDTH);
        let one = g.constant(1, SCHEDULER_TIME_WIDTH);
        let count = g.binary(SCHEDULER_TIME_WIDTH, count, BinaryOp::Add, one);
        g.store(slots.count, SCHEDULER_TIME_WIDTH, count);
        for &slot in &schema.time_slots {
            g.store_state(slot, 0, TIME_SLOT_WIDTH, time);
        }
        if schema.time_slots.iter().any(|&slot| g.settle_reads(slot)) {
            g.mark_dirty();
        }
        // The levels the slot starts from, the baseline of its edges, for an
        // exit in the middle of the slot; those of the clocks' events below,
        // when the run has clocks, unless the slot's waves compare against
        // them: a host clock no clock drives keeps the level the host set.
        let mut levels = g.no_levels();
        for (bit, &event) in tracked.iter().enumerate() {
            if self.clocks.contains(&event) && self.derived.is_empty() {
                continue;
            }
            let level = g.level(&self.events[event]);
            levels = g.with_level(levels, level, bit);
        }
        g.store_levels(levels);
        // The host clocks due now set their levels, in event order, before
        // any kernel runs; their edges start the slot's edges, which
        // collect in `exit_arg`. A clock whose edge applied waits for its
        // next edge until the slot ends. A run without clocks skips them.
        let zero = g.constant(0, MAX_EVENTS);
        g.store(slots.exit_arg, SCHEDULER_TIME_WIDTH, zero);
        let finish_exit = g.exit_block(SchedulerExit::Finish);
        let internal_exit = g.exit_block(SchedulerExit::Internal);
        let pick = g.b.new_block();
        if let Some((armed, clocked)) = started {
            let apply = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: clocked,
                true_block: (apply, Vec::new()),
                false_block: (pick, Vec::new()),
            });
            g.b.switch_to_block(apply);
            let values = g.load(slots.clock_value, SCHEDULER_TIME_WIDTH);
            let mut fired = g.constant(0, SCHEDULER_CLOCKS);
            let mut edges = g.constant(0, MAX_EVENTS);
            let mut levels = g.load_levels();
            for (bit, &index) in self.clocks.iter().enumerate() {
                let event = &self.events[index];
                let next_edge = g.clock_next(bit);
                let on = g.mask_bit(armed, bit);
                let due = g.binary(1, next_edge, BinaryOp::Eq, time);
                let due = g.binary(1, due, BinaryOp::And, on);
                let value = g.mask_bit(values, bit);
                let was = g.level(event);
                let position = tracked
                    .iter()
                    .position(|&tracked| tracked == index)
                    .expect("clocks are tracked");
                levels = g.with_level(levels, was, position);
                let is = if g.four_state.is_none() {
                    let is = g.b.alloc_bit(1, false);
                    g.b.emit(SIRInstruction::Mux(is, due, value, was));
                    g.store(event.signal, 1, is);
                    is
                } else {
                    // A clock that is not due keeps its state, unknown
                    // bits included; one that is gets a known level.
                    let current = g.load_state(event.signal, 0, 1);
                    let set = if g.keeps_unknowns(event.signal) {
                        g.b.alloc_logic(1)
                    } else {
                        g.b.alloc_bit(1, false)
                    };
                    g.b.emit(SIRInstruction::Mux(set, due, value, current));
                    g.store_state(event.signal, 0, 1, set);
                    let zero = g.constant(0, 1);
                    let known = g.b.alloc_bit(2, false);
                    g.b.emit(SIRInstruction::Concat(known, vec![zero, value]));
                    let is = g.b.alloc_bit(2, false);
                    g.b.emit(SIRInstruction::Mux(is, due, known, was));
                    is
                };
                let changed = g.binary(1, was, BinaryOp::Ne, is);
                if let Some(aliases) = &event.aliases {
                    for unit in aliases {
                        g.inline(unit, finish_exit);
                    }
                } else if g.settle_reads(event.signal) {
                    // Other clocks in this batch may not be due. Writing
                    // their old level does not invalidate settled inputs.
                    let dirty = g.load(slots.dirty, SCHEDULER_FLAG_WIDTH);
                    let dirty = g.binary(SCHEDULER_FLAG_WIDTH, dirty, BinaryOp::Or, changed);
                    g.store(slots.dirty, SCHEDULER_FLAG_WIDTH, dirty);
                }
                let edge = g.edge(event, was, is);
                let edge = g.bit(edge, index);
                edges = g.binary(MAX_EVENTS, edges, BinaryOp::Or, edge);
                let bit = g.bit(due, bit);
                fired = g.binary(SCHEDULER_CLOCKS, fired, BinaryOp::Or, bit);
            }
            g.store_levels(levels);
            g.store(slots.exit_arg, SCHEDULER_TIME_WIDTH, edges);
            let all = g.constant(u64::MAX, SCHEDULER_CLOCKS);
            let unfired = g.binary(SCHEDULER_CLOCKS, fired, BinaryOp::Xor, all);
            let armed = g.binary(SCHEDULER_CLOCKS, armed, BinaryOp::And, unfired);
            g.store(slots.clock_armed, SCHEDULER_TIME_WIDTH, armed);
            if !waits.is_empty() {
                // The waits see the clocks' levels before the process phase.
                let zero = g.constant(0, SCHEDULER_CLOCKS);
                let applied = g.binary(1, fired, BinaryOp::Ne, zero);
                let check = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: applied,
                    true_block: (check, Vec::new()),
                    false_block: (pick, Vec::new()),
                });
                g.b.switch_to_block(check);
                g.check_waits(&waits_of(&waits, &ordinals));
            }
        }
        g.b.seal_block(SIRTerminator::Jump(pick, Vec::new()));

        // A pass of the process phase: the lowest ready kernel runs, and
        // the waits see its writes before the next one while kernels remain
        // ready (`run_ready`); then combinational logic settles.
        g.b.switch_to_block(pick);
        let ready = g.load(slots.ready, MAX_KERNELS);
        let zero = g.constant(0, MAX_KERNELS);
        let idle = g.binary(1, ready, BinaryOp::Eq, zero);
        let pass_end = g.b.new_block();
        let chosen = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: idle,
            true_block: (pass_end, Vec::new()),
            false_block: (chosen, Vec::new()),
        });
        g.b.switch_to_block(chosen);
        let mut runs = Vec::new();
        for (index, kernel) in self.kernels.iter().enumerate() {
            if kernel.is_none() {
                continue;
            }
            let bit = g.constant(1 << ordinals[index], MAX_KERNELS);
            let selected = g.binary(MAX_KERNELS, ready, BinaryOp::And, bit);
            let this = g.binary(1, selected, BinaryOp::Ne, zero);
            let run = g.b.new_block();
            let next = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: this,
                true_block: (run, Vec::new()),
                false_block: (next, Vec::new()),
            });
            runs.push((index, run));
            g.b.switch_to_block(next);
        }
        g.b.seal_block(SIRTerminator::Jump(internal_exit, Vec::new()));

        for (index, run) in runs {
            let kernel = self.kernels[index].as_ref().expect("selected above");
            let process = &schema.processes[index];
            g.b.switch_to_block(run);
            let ready = g.load(slots.ready, MAX_KERNELS);
            let keep = g.constant(!(1u64 << ordinals[index]), MAX_KERNELS);
            let ready = g.binary(MAX_KERNELS, ready, BinaryOp::And, keep);
            g.store(slots.ready, SCHEDULER_TIME_WIDTH, ready);
            let current = g.constant(index as u64, SCHEDULER_TIME_WIDTH);
            g.store(slots.current, SCHEDULER_TIME_WIDTH, current);
            let before: Vec<_> = kernel
                .writes
                .iter()
                .map(|&event| g.level(&self.events[event]))
                .collect();
            let enter = g.b.new_block();
            g.b.seal_block(SIRTerminator::Jump(enter, Vec::new()));
            g.b.switch_to_block(enter);
            g.inline_writer(&program.sir.processes[index], finish_exit);
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
            let resumed = g.b.new_block();
            match &kernel.wait {
                None => {
                    let done = g.equals(status, code(ProcessStatus::Done));
                    let ended = g.b.new_block();
                    let suspended = g.b.new_block();
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
                    let overflow_exit = g.exit_block(SchedulerExit::DelayOverflow);
                    let in_range = g.b.new_block();
                    g.b.seal_block(SIRTerminator::Branch {
                        cond: overflow,
                        true_block: (overflow_exit, Vec::new()),
                        false_block: (in_range, Vec::new()),
                    });
                    g.b.switch_to_block(in_range);
                    g.store(slots.wake[index], SCHEDULER_TIME_WIDTH, wake);
                    g.b.seal_block(SIRTerminator::Jump(resumed, Vec::new()));

                    // The edges of this run, against the levels before it.
                    g.b.switch_to_block(resumed);
                    let mut edges = g.load(slots.exit_arg, MAX_EVENTS);
                    for (&event, &was) in kernel.writes.iter().zip(&before) {
                        let is = g.level(&self.events[event]);
                        let edge = g.edge(&self.events[event], was, is);
                        let bit = g.bit(edge, event);
                        edges = g.binary(MAX_EVENTS, edges, BinaryOp::Or, bit);
                    }
                    g.store(slots.exit_arg, SCHEDULER_TIME_WIDTH, edges);
                }
                Some(wait) => {
                    // A kernel runs at most once a slot (see `Plan::new`), so
                    // a staged site is never staged again before the NBA
                    // region and the kernel never returns `Nba`.
                    let done = g.equals(status, code(ProcessStatus::Done));
                    let suspends = g.equals(status, code(ProcessStatus::Wait));
                    let other = g.b.new_block();
                    let suspended = g.b.new_block();
                    g.b.seal_block(SIRTerminator::Branch {
                        cond: done,
                        true_block: (resumed, Vec::new()),
                        false_block: (other, Vec::new()),
                    });
                    g.b.switch_to_block(other);
                    g.b.seal_block(SIRTerminator::Branch {
                        cond: suspends,
                        true_block: (suspended, Vec::new()),
                        false_block: (internal_exit, Vec::new()),
                    });

                    // It waits again: the edge is judged against the level
                    // now (`register_waiter`).
                    g.b.switch_to_block(suspended);
                    let flag = g.constant(1, SCHEDULER_FLAG_WIDTH);
                    g.store(slots.waiting[index], SCHEDULER_FLAG_WIDTH, flag);
                    let level = g.signal_level(wait.signal);
                    let level = g.baseline_byte(level);
                    g.store(slots.baseline[index], SCHEDULER_FLAG_WIDTH, level);
                    g.b.seal_block(SIRTerminator::Jump(resumed, Vec::new()));
                    g.b.switch_to_block(resumed);
                }
            }
            // Before the next run, the waits see this one's writes.
            let ready = g.load(slots.ready, MAX_KERNELS);
            let zero = g.constant(0, MAX_KERNELS);
            let more = g.binary(1, ready, BinaryOp::Ne, zero);
            let check = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: more,
                true_block: (check, Vec::new()),
                false_block: (pick, Vec::new()),
            });
            g.b.switch_to_block(check);
            g.check_waits(&waits_of(&waits, &ordinals));
            g.b.seal_block(SIRTerminator::Jump(pick, Vec::new()));
        }

        // The settles after a pass and after the domains share one copy of
        // combinational logic. A deferred settle after a pass runs for the
        // kernels that may run in another.
        g.b.switch_to_block(pass_end);
        let none = g.constant(u64::MAX, SCHEDULER_TIME_WIDTH);
        g.store(slots.current, SCHEDULER_TIME_WIDTH, none);
        let settled = g.b.new_block();
        let slot_end = g.b.new_block();
        let wait_observers = self
            .lazy
            .as_ref()
            .is_some_and(|lazy| waits.iter().any(|&(index, _)| lazy.kernels[index]));
        match &self.lazy {
            Some(lazy) => g.eager_then(&lazy.eager, settled),
            None => g.settle_then(settled),
        }

        // After the settle of a pass the waits see its writes; woken
        // kernels run in another pass.
        g.b.switch_to_block(settled);
        g.check_waits(&waits_of(&waits, &ordinals));
        let ready = g.load(slots.ready, MAX_KERNELS);
        let zero = g.constant(0, MAX_KERNELS);
        let more = g.binary(1, ready, BinaryOp::Ne, zero);
        let domains = g.b.new_block();
        let another = if wait_observers {
            g.b.new_block()
        } else {
            pick
        };
        g.b.seal_block(SIRTerminator::Branch {
            cond: more,
            true_block: (another, Vec::new()),
            false_block: (domains, Vec::new()),
        });
        if wait_observers {
            g.b.switch_to_block(another);
            g.settle_then(pick);
        }

        // The domains the edges trigger, then the NBA region.
        g.b.switch_to_block(domains);
        if !self.derived.is_empty() {
            self.batch(&mut g, &tracked, slot_end, finish_exit);
        } else {
            let staged: Vec<&Staged> = waits.iter().flat_map(|(_, wait)| &wait.sites).collect();
            let mask = g.load(slots.exit_arg, MAX_EVENTS);
            let zero = g.constant(0, MAX_EVENTS);
            let quiet = g.binary(1, mask, BinaryOp::Eq, zero);
            let triggered = g.b.new_block();
            let untriggered = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: quiet,
                true_block: (untriggered, Vec::new()),
                false_block: (triggered, Vec::new()),
            });

            // Without a domain, published nonblocking values settle once more.
            g.b.switch_to_block(untriggered);
            if let Some(lazy) = &self.lazy {
                g.publish(&staged);
                g.eager_then(&lazy.eager, slot_end);
            } else if staged.is_empty() {
                g.b.seal_block(SIRTerminator::Jump(slot_end, Vec::new()));
            } else {
                let mut any = g.constant(0, 1);
                for site in &staged {
                    let pending = g.load(site.pending, 1);
                    any = g.binary(1, any, BinaryOp::Or, pending);
                }
                g.publish(&staged);
                let publish_settle = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: any,
                    true_block: (publish_settle, Vec::new()),
                    false_block: (slot_end, Vec::new()),
                });
                g.b.switch_to_block(publish_settle);
                g.settle_then(slot_end);
            }

            g.b.switch_to_block(triggered);
            let evaluated = g.b.new_block();
            if edged.is_empty() {
                // No event can have an edge; the mask stays empty.
                g.b.seal_block(SIRTerminator::Jump(evaluated, Vec::new()));
            } else {
                let one = g.constant(1, MAX_EVENTS);
                let lower = g.binary(MAX_EVENTS, mask, BinaryOp::Sub, one);
                let others = g.binary(MAX_EVENTS, mask, BinaryOp::And, lower);
                let single = g.binary(1, others, BinaryOp::Eq, zero);
                let one_domain = g.b.new_block();
                let domains = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: single,
                    true_block: (one_domain, Vec::new()),
                    false_block: (domains, Vec::new()),
                });

                // One triggered domain evaluates and applies at once. The events
                // kernels write are tested first: a run without clocks reaches
                // their domains as soon as without clock events.
                g.b.switch_to_block(one_domain);
                let written = self.written();
                for &index in written.iter().chain(&self.clocks) {
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
                    let units = match self.lazy.as_ref().map(|lazy| &lazy.domains[index]) {
                        None | Some(DomainSettle::Pending) => &event.eval_apply,
                        Some(DomainSettle::Fused(units)) => {
                            // Fusion is needed only when combinational inputs
                            // changed since the last settle. Otherwise the
                            // ordinary FF unit already has its inputs.
                            let dirty = g.load(slots.dirty, SCHEDULER_FLAG_WIDTH);
                            let fused = g.b.new_block();
                            let clean = g.b.new_block();
                            g.b.seal_block(SIRTerminator::Branch {
                                cond: dirty,
                                true_block: (fused, Vec::new()),
                                false_block: (clean, Vec::new()),
                            });
                            g.b.switch_to_block(clean);
                            for unit in &event.eval_apply {
                                g.inline_writer(unit, finish_exit);
                            }
                            g.b.seal_block(SIRTerminator::Jump(evaluated, Vec::new()));
                            g.b.switch_to_block(fused);
                            units
                        }
                        Some(DomainSettle::Settled) => {
                            let evaluate = g.b.new_block();
                            g.settle_then(evaluate);
                            g.b.switch_to_block(evaluate);
                            &event.eval_apply
                        }
                    };
                    for unit in units {
                        g.inline_writer(unit, finish_exit);
                    }
                    g.b.seal_block(SIRTerminator::Jump(evaluated, Vec::new()));
                    g.b.switch_to_block(next);
                }
                g.b.seal_block(SIRTerminator::Jump(internal_exit, Vec::new()));

                // Several evaluate, then apply, each in event order.
                g.b.switch_to_block(domains);
                if edged.len() < 2 {
                    g.b.seal_block(SIRTerminator::Jump(internal_exit, Vec::new()));
                } else {
                    let mut mask = mask;
                    let mut zero = zero;
                    if self.lazy.as_ref().is_some_and(|lazy| lazy.several) {
                        let evaluate = g.b.new_block();
                        g.settle_then(evaluate);
                        g.b.switch_to_block(evaluate);
                        mask = g.load(slots.exit_arg, MAX_EVENTS);
                        zero = g.constant(0, MAX_EVENTS);
                    }
                    for phase in [Phase::Evaluate, Phase::Apply] {
                        for &index in &edged {
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
                                Phase::Evaluate => event.eval_only.as_ref(),
                                Phase::Apply => event.apply.as_ref(),
                            }
                            .expect("several edged events have their units (`Plan::new`)");
                            for unit in units {
                                g.inline_writer(unit, finish_exit);
                            }
                            g.b.seal_block(SIRTerminator::Jump(next, Vec::new()));
                            g.b.switch_to_block(next);
                        }
                    }
                    g.b.seal_block(SIRTerminator::Jump(evaluated, Vec::new()));
                }
            }

            // The domains' results and the nonblocking values publish before
            // combinational logic settles.
            g.b.switch_to_block(evaluated);
            g.publish(&staged);
            match &self.lazy {
                Some(lazy) => g.eager_then(&lazy.eager, slot_end),
                None => g.settle_then(slot_end),
            }
        }

        // At the end of the slot each clock whose edge applied waits for the
        // opposite level, half a period later, in event order (as
        // `rearm_clocks`): the extra time unit of an odd period goes to the
        // high phase. The reference re-arms at the end of the slot's first
        // round (`continue_round`); a compiled slot has a single round, as
        // no kernel it runs resumes in the same slot.
        g.b.switch_to_block(slot_end);
        if let Some((started, clocked)) = started {
            let overflow_exit = g.exit_block(SchedulerExit::ClockOverflow);
            let rearms = g.b.new_block();
            let rearmed = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: clocked,
                true_block: (rearms, Vec::new()),
                false_block: (rearmed, Vec::new()),
            });
            g.b.switch_to_block(rearms);
            // The clocks whose edge applied are those the slot disarmed.
            let armed = g.load(slots.clock_armed, SCHEDULER_TIME_WIDTH);
            let all = g.constant(u64::MAX, SCHEDULER_CLOCKS);
            let unarmed = g.binary(SCHEDULER_CLOCKS, armed, BinaryOp::Xor, all);
            let fired = g.binary(SCHEDULER_CLOCKS, started, BinaryOp::And, unarmed);
            for bit in 0..self.clocks.len() {
                let applied = g.mask_bit(fired, bit);
                let rearm = g.b.new_block();
                let next = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: applied,
                    true_block: (rearm, Vec::new()),
                    false_block: (next, Vec::new()),
                });
                g.b.switch_to_block(rearm);
                let values = g.load(slots.clock_value, SCHEDULER_TIME_WIDTH);
                let high = g.mask_bit(values, bit);
                let period = g.load_at(
                    slots.clock_period,
                    bit * SCHEDULER_TIME_WIDTH,
                    SCHEDULER_TIME_WIDTH,
                );
                let one = g.constant(1, SCHEDULER_TIME_WIDTH);
                let half = g.binary(SCHEDULER_TIME_WIDTH, period, BinaryOp::Shr, one);
                let odd = g.binary(SCHEDULER_TIME_WIDTH, period, BinaryOp::And, one);
                let even = g.constant(0, SCHEDULER_TIME_WIDTH);
                let extra = g.b.alloc_bit(SCHEDULER_TIME_WIDTH, false);
                g.b.emit(SIRInstruction::Mux(extra, high, odd, even));
                let half = g.binary(SCHEDULER_TIME_WIDTH, half, BinaryOp::Add, extra);
                let next_edge = g.binary(SCHEDULER_TIME_WIDTH, time, BinaryOp::Add, half);
                let overflow = g.binary(1, next_edge, BinaryOp::LtU, time);
                let overflowed = g.b.new_block();
                let in_range = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: overflow,
                    true_block: (overflowed, Vec::new()),
                    false_block: (in_range, Vec::new()),
                });
                g.b.switch_to_block(overflowed);
                let clock = g.constant(bit as u64, SCHEDULER_TIME_WIDTH);
                g.store(slots.exit_arg, SCHEDULER_TIME_WIDTH, clock);
                // The slot is complete: the host reads its results settled.
                match &self.lazy {
                    Some(_) => g.settle_then(overflow_exit),
                    None => {
                        g.b.seal_block(SIRTerminator::Jump(overflow_exit, Vec::new()));
                    }
                }
                g.b.switch_to_block(in_range);
                g.store_at(
                    slots.clock_next,
                    bit * SCHEDULER_TIME_WIDTH,
                    SCHEDULER_TIME_WIDTH,
                    next_edge,
                );
                let flip = g.constant(1 << bit, SCHEDULER_CLOCKS);
                let values = g.binary(SCHEDULER_CLOCKS, values, BinaryOp::Xor, flip);
                g.store(slots.clock_value, SCHEDULER_TIME_WIDTH, values);
                let armed = g.load(slots.clock_armed, SCHEDULER_TIME_WIDTH);
                let armed = g.binary(SCHEDULER_CLOCKS, armed, BinaryOp::Or, flip);
                g.store(slots.clock_armed, SCHEDULER_TIME_WIDTH, armed);
                g.b.seal_block(SIRTerminator::Jump(next, Vec::new()));
                g.b.switch_to_block(next);
            }
            g.b.seal_block(SIRTerminator::Jump(rearmed, Vec::new()));
            g.b.switch_to_block(rearmed);
        }
        g.b.seal_block(SIRTerminator::Jump(header, Vec::new()));
        g.emit_settle(&program.sir.eval_comb, finish_exit, internal_exit);
        if let Some(lazy) = &self.lazy {
            g.emit_eager(&lazy.eager, finish_exit, internal_exit);
        }
        g.finish()
    }
}

impl Plan {
    /// The domains of a slot of a design with derived clocks, as the
    /// reference loop's waves run them: combinational logic settles; the
    /// domains of the events with an active edge evaluate together with
    /// those of the clocks computed combinationally from a changed source
    /// whose level moved to their active edge, judged now, whose levels
    /// become their baseline; without an active edge, every tracked event
    /// whose level moved to its active edge since the slot started. After
    /// each wave commits and logic settles, the domains not run yet whose
    /// clocks moved to their active edge run in another, until none does.
    /// The domains run in the slot collect in `exit_arg`.
    fn batch(
        &self,
        g: &mut Generator<'_>,
        tracked: &[usize],
        slot_end: celox_sir::BlockId,
        finish_exit: celox_sir::BlockId,
    ) {
        let slots = g.slots;
        let wave = g.b.new_block();
        let mask = g.b.alloc_bit(MAX_EVENTS, false);
        let fire = g.b.new_block_with(vec![mask]);
        let judge = g.b.new_block();
        // A pass of the process phase settles before the domains; a
        // deferred one runs now.
        match &self.lazy {
            Some(_) => g.settle_then(judge),
            None => {
                g.b.seal_block(SIRTerminator::Jump(judge, Vec::new()));
            }
        }

        g.b.switch_to_block(judge);
        let active = g.load(slots.exit_arg, MAX_EVENTS);
        let levels = g.load_levels();
        let stable = self.stable_edges(g, tracked, levels);
        let mut gated_edges = g.constant(0, MAX_EVENTS);
        let mut judged = levels;
        for (source, gated) in &self.gating {
            let source_bit = position(tracked, *source);
            let was = g.saved_level(levels, source_bit);
            let is = g.level(&self.events[*source]);
            let changed = g.binary(1, was, BinaryOp::Ne, is);
            for &clock in gated {
                let clock_bit = position(tracked, clock);
                let was = g.saved_level(levels, clock_bit);
                let is = g.level(&self.events[clock]);
                let edge = g.edge(&self.events[clock], was, is);
                let edge = g.binary(1, edge, BinaryOp::And, changed);
                let bit = g.bit(edge, clock);
                gated_edges = g.binary(MAX_EVENTS, gated_edges, BinaryOp::Or, bit);
                // The judged level is the clock's baseline from now on.
                let level = g.b.alloc_bit(g.level_width(), false);
                g.b.emit(SIRInstruction::Mux(level, changed, is, was));
                judged = g.replace_level(judged, level, clock_bit);
            }
        }
        g.store_levels(judged);
        let zero = g.constant(0, MAX_EVENTS);
        let edged = g.binary(1, active, BinaryOp::Ne, zero);
        let sources = g.binary(MAX_EVENTS, active, BinaryOp::Or, gated_edges);
        let triggers = g.b.alloc_bit(MAX_EVENTS, false);
        g.b.emit(SIRInstruction::Mux(triggers, edged, sources, stable));
        g.store(slots.exit_arg, SCHEDULER_TIME_WIDTH, triggers);
        let quiet = g.binary(1, triggers, BinaryOp::Eq, zero);
        let first = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: quiet,
            true_block: (slot_end, Vec::new()),
            false_block: (first, Vec::new()),
        });
        g.b.switch_to_block(first);
        g.b.seal_block(SIRTerminator::Jump(fire, vec![triggers]));

        // A wave after a commit: the domains not run yet whose clocks moved.
        g.b.switch_to_block(wave);
        let fired = g.load(slots.exit_arg, MAX_EVENTS);
        let levels = g.load_levels();
        let stable = self.stable_edges(g, tracked, levels);
        let all = g.constant(u64::MAX, MAX_EVENTS);
        let unfired = g.binary(MAX_EVENTS, fired, BinaryOp::Xor, all);
        let ready = g.binary(MAX_EVENTS, stable, BinaryOp::And, unfired);
        let zero = g.constant(0, MAX_EVENTS);
        let quiet = g.binary(1, ready, BinaryOp::Eq, zero);
        let next = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: quiet,
            true_block: (slot_end, Vec::new()),
            false_block: (next, Vec::new()),
        });
        g.b.switch_to_block(next);
        let fired = g.binary(MAX_EVENTS, fired, BinaryOp::Or, ready);
        g.store(slots.exit_arg, SCHEDULER_TIME_WIDTH, fired);
        g.b.seal_block(SIRTerminator::Jump(fire, vec![ready]));

        // A wave: one domain evaluates and commits at once, several
        // evaluate, then commit, in event order; then logic settles.
        g.b.switch_to_block(fire);
        let zero = g.constant(0, MAX_EVENTS);
        let one = g.constant(1, MAX_EVENTS);
        let lower = g.binary(MAX_EVENTS, mask, BinaryOp::Sub, one);
        let others = g.binary(MAX_EVENTS, mask, BinaryOp::And, lower);
        let single = g.binary(1, others, BinaryOp::Eq, zero);
        let alone = g.b.new_block();
        let together = g.b.new_block();
        let committed = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: single,
            true_block: (alone, Vec::new()),
            false_block: (together, Vec::new()),
        });
        g.b.switch_to_block(alone);
        for &index in tracked {
            g.if_bit(mask, index, |g| {
                for unit in &self.events[index].eval_apply {
                    g.inline_writer(unit, finish_exit);
                }
            });
        }
        g.b.seal_block(SIRTerminator::Jump(committed, Vec::new()));
        g.b.switch_to_block(together);
        if tracked.len() > 1 {
            for phase in [Phase::Evaluate, Phase::Apply] {
                for &index in tracked {
                    let event = &self.events[index];
                    let units = match phase {
                        Phase::Evaluate => event.eval_only.as_ref(),
                        Phase::Apply => event.apply.as_ref(),
                    }
                    .expect("tracked events have their units (`Plan::new`)");
                    g.if_bit(mask, index, |g| {
                        for unit in units {
                            g.inline_writer(unit, finish_exit);
                        }
                    });
                }
            }
        }
        g.b.seal_block(SIRTerminator::Jump(committed, Vec::new()));
        g.b.switch_to_block(committed);
        g.settle_then(wave);
    }

    /// The tracked events whose levels moved to their active edge against
    /// `levels`, their baselines, as bits by event index.
    fn stable_edges(&self, g: &mut Generator<'_>, tracked: &[usize], levels: Levels) -> RegisterId {
        let mut edges = g.constant(0, MAX_EVENTS);
        for (position, &index) in tracked.iter().enumerate() {
            let was = g.saved_level(levels, position);
            let is = g.level(&self.events[index]);
            let edge = g.edge(&self.events[index], was, is);
            let bit = g.bit(edge, index);
            edges = g.binary(MAX_EVENTS, edges, BinaryOp::Or, bit);
        }
        edges
    }
}

/// Edge levels of the tracked events, one bit per event (see
/// [`SchedulerSlots::levels`]).
#[derive(Clone, Copy)]
struct Levels {
    values: RegisterId,
    /// Their unknown bits, in a four-state design.
    unknown: Option<RegisterId>,
}

/// The place of event `index` among `tracked`, the bit of its level.
fn position(tracked: &[usize], index: usize) -> usize {
    tracked.binary_search(&index).expect("the event is tracked")
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
    exits: Vec<(celox_sir::BlockId, SchedulerExit)>,
    /// See [`Plan::settle_reads`].
    settle_reads: Option<&'a HashSet<AbsoluteAddr>>,
    shared: HashMap<AbsoluteAddr, AbsoluteAddr>,
    /// The shared settle, emitted by [`Self::emit_settle`].
    comb: celox_sir::BlockId,
    /// Where the shared settle continues, by case: case `i + 1` continues
    /// at `continuations[i]`.
    continuations: Vec<celox_sir::BlockId>,
    /// The shared stateful part of a deferred settle, emitted by
    /// [`Self::emit_eager`] once a caller uses it.
    eager: Option<celox_sir::BlockId>,
    /// Where it continues, as [`Self::continuations`].
    eager_continuations: Vec<celox_sir::BlockId>,
    /// In a four-state design, which state objects keep unknown bits: their
    /// loads and stores move both planes, and edge levels are four-state
    /// (see [`Self::level_at`]). `None` in a two-state design.
    four_state: Option<&'a crate::HashMap<AbsoluteAddr, bool>>,
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
        self.load_at(address, 0, width)
    }

    fn store(&mut self, address: AbsoluteAddr, width: usize, value: RegisterId) {
        self.store_at(address, 0, width, value);
    }

    fn load_at(&mut self, address: AbsoluteAddr, offset: usize, width: usize) -> RegisterId {
        let register = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Load(
            register,
            stable(address),
            SIROffset::Static(offset),
            width,
        ));
        register
    }

    fn store_at(&mut self, address: AbsoluteAddr, offset: usize, width: usize, value: RegisterId) {
        self.b.emit(SIRInstruction::Store(
            stable(address),
            SIROffset::Static(offset),
            width,
            value,
            Vec::new(),
            Vec::new(),
        ));
    }

    /// The time of host clock `bit`'s next edge.
    fn clock_next(&mut self, bit: usize) -> RegisterId {
        self.load_at(
            self.slots.clock_next,
            bit * SCHEDULER_TIME_WIDTH,
            SCHEDULER_TIME_WIDTH,
        )
    }

    /// Whether bit `bit` of `mask` is set.
    fn mask_bit(&mut self, mask: RegisterId, bit: usize) -> RegisterId {
        let width = self.b.register(&mask).width();
        let selector = self.constant(1 << bit, width);
        let selected = self.binary(width, mask, BinaryOp::And, selector);
        let zero = self.constant(0, width);
        self.binary(1, selected, BinaryOp::Ne, zero)
    }

    /// The edge level of `event`'s signal, as the runtime reads it: whether
    /// its low byte, within its width, is not zero (see [`Self::level_at`]).
    fn level(&mut self, event: &Event) -> RegisterId {
        self.level_at(event.signal, event.width.min(8))
    }

    /// The edge level of the low `width` bits of `address`: one bit, whether
    /// they are not zero, or in a four-state design two, as
    /// `celox_design::edges` encodes it: that bit, and above it whether any
    /// of them is unknown (`edge_level`).
    fn level_at(&mut self, address: AbsoluteAddr, width: usize) -> RegisterId {
        if self.four_state.is_none() {
            let value = self.load(address, width);
            let zero = self.constant(0, width);
            return self.binary(1, value, BinaryOp::Ne, zero);
        }
        let (value, unknown) = if self.keeps_unknowns(address) {
            // Both planes, from the 64-bit planes of the bits widened.
            let bits = self.load_state(address, 0, width);
            debug_assert!(width < 64, "an edge level reads the low byte");
            let wide = self.b.alloc_logic(64);
            let zero = self.b.alloc_logic(64 - width);
            self.b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
            self.b.emit(SIRInstruction::Concat(wide, vec![zero, bits]));
            let planes = self.b.alloc_bit(128, false);
            self.b.emit(SIRInstruction::Unary(
                planes,
                celox_design::UnaryOp::ToPlanes,
                wide,
            ));
            let plane = |g: &mut Self, offset: usize| {
                let bits = g.b.alloc_bit(width, false);
                g.b.emit(SIRInstruction::Slice(bits, planes, offset, width));
                let zero = g.constant(0, width);
                g.binary(1, bits, BinaryOp::Ne, zero)
            };
            (plane(self, 0), plane(self, 64))
        } else {
            let value = self.load(address, width);
            let zero = self.constant(0, width);
            (
                self.binary(1, value, BinaryOp::Ne, zero),
                self.constant(0, 1),
            )
        };
        let level = self.b.alloc_bit(2, false);
        self.b
            .emit(SIRInstruction::Concat(level, vec![unknown, value]));
        level
    }

    /// The width of an edge level (see [`Self::level_at`]).
    fn level_width(&self) -> usize {
        if self.four_state.is_some() { 2 } else { 1 }
    }

    /// Whether a change of `event`'s level from `was` to `is` is its active
    /// edge.
    fn edge(&mut self, event: &Event, was: RegisterId, is: RegisterId) -> RegisterId {
        self.level_edge(event.active != 0, was, is)
    }

    /// Whether a change of an edge level from `was` to `is` is a rising
    /// (`rising`) or a falling edge, as `celox_design::edges::level_edge`
    /// judges it: in a four-state design, one that leaves the level the edge
    /// starts from or reaches the one it ends at, other than between unknown
    /// levels (IEEE 1800-2023 Table 9-2).
    fn level_edge(&mut self, rising: bool, was: RegisterId, is: RegisterId) -> RegisterId {
        let (from, to) = if rising { (0, 1) } else { (1, 0) };
        if self.four_state.is_none() {
            let changed = self.binary(1, was, BinaryOp::Ne, is);
            let active = self.equals(is, to);
            return self.binary(1, changed, BinaryOp::And, active);
        }
        let to = self.constant(to, 2);
        let leaves = self.binary(1, was, BinaryOp::Ne, to);
        let from = self.constant(from, 2);
        let reaches = self.binary(1, is, BinaryOp::Ne, from);
        let edge = self.binary(1, leaves, BinaryOp::And, reaches);
        let both = self.binary(2, was, BinaryOp::And, is);
        let unknown = self.constant(u64::from(LEVEL_UNKNOWN), 2);
        let both = self.binary(2, both, BinaryOp::And, unknown);
        let zero = self.constant(0, 2);
        let known = self.binary(1, both, BinaryOp::Eq, zero);
        self.binary(1, edge, BinaryOp::And, known)
    }

    /// Whether `address` keeps unknown bits: a four-state object of a
    /// four-state design.
    fn keeps_unknowns(&self, address: AbsoluteAddr) -> bool {
        self.four_state
            .is_some_and(|objects| objects.get(&address).copied().unwrap_or(false))
    }

    /// Load design state, both planes of an object that keeps unknown bits.
    fn load_state(&mut self, address: AbsoluteAddr, offset: usize, width: usize) -> RegisterId {
        if !self.keeps_unknowns(address) {
            return self.load_at(address, offset, width);
        }
        let register = self.b.alloc_logic(width);
        self.b.emit(SIRInstruction::Load(
            register,
            stable(address),
            SIROffset::Static(offset),
            width,
        ));
        register
    }

    /// Store `value` into design state: into an object that keeps unknown
    /// bits as a four-state value, a two-state one with its mask clear;
    /// into one that does not, without its unknown bits.
    fn store_state(
        &mut self,
        address: AbsoluteAddr,
        offset: usize,
        width: usize,
        value: RegisterId,
    ) {
        let logic = matches!(
            self.b.register(&value),
            celox_sir::RegisterType::Logic { .. }
        );
        let value = match (self.keeps_unknowns(address), logic) {
            (true, false) => {
                let zero = self.b.alloc_logic(width);
                self.b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
                let known = self.b.alloc_logic(width);
                self.b
                    .emit(SIRInstruction::Binary(known, value, BinaryOp::Or, zero));
                known
            }
            (false, true) => {
                let known = self.b.alloc_bit(width, false);
                self.b.emit(SIRInstruction::Unary(
                    known,
                    celox_design::UnaryOp::ToTwoState,
                    value,
                ));
                known
            }
            _ => value,
        };
        self.store_at(address, offset, width, value);
    }

    /// The saved level of the event at bit `bit` of `levels`.
    fn saved_level(&mut self, levels: Levels, bit: usize) -> RegisterId {
        let value = self.mask_bit(levels.values, bit);
        match levels.unknown {
            None => value,
            Some(unknown) => {
                let unknown = self.mask_bit(unknown, bit);
                let level = self.b.alloc_bit(2, false);
                self.b
                    .emit(SIRInstruction::Concat(level, vec![unknown, value]));
                level
            }
        }
    }

    /// `levels` with `level` at bit `bit`, which was clear.
    fn with_level(&mut self, levels: Levels, level: RegisterId, bit: usize) -> Levels {
        let (value, unknown) = self.level_planes(level);
        let set = self.bit(value, bit);
        let values = self.binary(MAX_EVENTS, levels.values, BinaryOp::Or, set);
        let unknown = levels.unknown.map(|levels| {
            let set = self.bit(unknown.expect("a four-state level"), bit);
            self.binary(MAX_EVENTS, levels, BinaryOp::Or, set)
        });
        Levels { values, unknown }
    }

    /// `levels` with `level` in place of the one at bit `bit`.
    fn replace_level(&mut self, levels: Levels, level: RegisterId, bit: usize) -> Levels {
        let keep = self.constant(!(1u64 << bit), MAX_EVENTS);
        let values = self.binary(MAX_EVENTS, levels.values, BinaryOp::And, keep);
        let unknown = levels
            .unknown
            .map(|unknown| self.binary(MAX_EVENTS, unknown, BinaryOp::And, keep));
        self.with_level(Levels { values, unknown }, level, bit)
    }

    /// The value bit of an edge level, and in a four-state design its
    /// unknown bit.
    fn level_planes(&mut self, level: RegisterId) -> (RegisterId, Option<RegisterId>) {
        if self.four_state.is_none() {
            return (level, None);
        }
        let plane = |g: &mut Self, offset: usize| {
            let bit = g.b.alloc_bit(1, false);
            g.b.emit(SIRInstruction::Slice(bit, level, offset, 1));
            bit
        };
        (plane(self, 0), Some(plane(self, 1)))
    }

    /// No saved levels: every event at 0.
    fn no_levels(&mut self) -> Levels {
        let values = self.constant(0, MAX_EVENTS);
        let unknown = self
            .four_state
            .is_some()
            .then(|| self.constant(0, MAX_EVENTS));
        Levels { values, unknown }
    }

    /// The levels of [`SchedulerSlots::levels`].
    fn load_levels(&mut self) -> Levels {
        let values = self.load(self.slots.levels, MAX_EVENTS);
        let unknown = self
            .four_state
            .is_some()
            .then(|| self.load(self.slots.unknown_levels, MAX_EVENTS));
        Levels { values, unknown }
    }

    fn store_levels(&mut self, levels: Levels) {
        self.store(self.slots.levels, SCHEDULER_TIME_WIDTH, levels.values);
        if let Some(unknown) = levels.unknown {
            self.store(self.slots.unknown_levels, SCHEDULER_TIME_WIDTH, unknown);
        }
    }

    /// Run `body` when bit `index` of the 64-bit `mask` is set.
    fn if_bit(&mut self, mask: RegisterId, index: usize, body: impl FnOnce(&mut Self)) {
        let set = self.mask_bit(mask, index);
        let run = self.b.new_block();
        let next = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: set,
            true_block: (run, Vec::new()),
            false_block: (next, Vec::new()),
        });
        self.b.switch_to_block(run);
        body(self);
        self.b.seal_block(SIRTerminator::Jump(next, Vec::new()));
        self.b.switch_to_block(next);
    }

    /// A block that ends the run with `exit`, emitted by
    /// [`Self::emit_exits`].
    fn exit_block(&mut self, exit: SchedulerExit) -> celox_sir::BlockId {
        let block = self.b.new_block();
        self.exits.push((block, exit));
        block
    }

    /// Bit `index` of a 64-bit mask, set when `condition` holds.
    fn bit(&mut self, condition: RegisterId, index: usize) -> RegisterId {
        let set = self.constant(1 << index, MAX_KERNELS);
        let unset = self.constant(0, MAX_KERNELS);
        let bit = self.b.alloc_bit(MAX_KERNELS, false);
        self.b.emit(SIRInstruction::Mux(bit, condition, set, unset));
        bit
    }

    /// The edge level of the one-bit signal `address` (see
    /// [`Self::level_at`]).
    fn signal_level(&mut self, address: AbsoluteAddr) -> RegisterId {
        if self.four_state.is_none() {
            return self.load(address, 1);
        }
        self.level_at(address, 1)
    }

    /// An edge level as the byte of a wait's baseline slot.
    fn baseline_byte(&mut self, level: RegisterId) -> RegisterId {
        if self.four_state.is_none() {
            return self.widen(level, SCHEDULER_FLAG_WIDTH);
        }
        let zero = self.constant(0, SCHEDULER_FLAG_WIDTH - 2);
        let byte = self.b.alloc_bit(SCHEDULER_FLAG_WIDTH, false);
        self.b.emit(SIRInstruction::Concat(byte, vec![zero, level]));
        byte
    }

    /// The one-bit `value` as a `width`-bit number.
    fn widen(&mut self, value: RegisterId, width: usize) -> RegisterId {
        let set = self.constant(1, width);
        let unset = self.constant(0, width);
        let wide = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Mux(wide, value, set, unset));
        wide
    }

    /// Test the waits of `waits`, (kernel, ready bit, signal, edge), against their
    /// signals now (`check_waits`): a wait whose edge occurred since its
    /// baseline wakes its kernel; every changed signal becomes the
    /// baseline.
    fn check_waits(&mut self, waits: &[(usize, usize, AbsoluteAddr, CounterKind)]) {
        if waits.is_empty() {
            return;
        }
        let mut woken = self.constant(0, MAX_KERNELS);
        for &(index, ordinal, signal, kind) in waits {
            let waiting = self.waiting(index);
            let baseline = self.load(self.slots.baseline[index], SCHEDULER_FLAG_WIDTH);
            let is = self.signal_level(signal);
            let occurred = if self.four_state.is_none() {
                let zero = self.constant(0, SCHEDULER_FLAG_WIDTH);
                let was = self.binary(1, baseline, BinaryOp::Ne, zero);
                let changed = self.binary(1, was, BinaryOp::Ne, is);
                match kind {
                    CounterKind::Changes => changed,
                    CounterKind::Rises => self.binary(1, changed, BinaryOp::And, is),
                    CounterKind::Falls => self.binary(1, changed, BinaryOp::And, was),
                }
            } else {
                // The baseline byte holds the level (`baseline_byte`); the
                // edges are those of `timing::occurs`, Table 9-2's.
                let was = self.b.alloc_bit(2, false);
                self.b.emit(SIRInstruction::Slice(was, baseline, 0, 2));
                match kind {
                    CounterKind::Changes => self.binary(1, was, BinaryOp::Ne, is),
                    CounterKind::Rises => self.level_edge(true, was, is),
                    CounterKind::Falls => self.level_edge(false, was, is),
                }
            };
            let wake = self.binary(1, occurred, BinaryOp::And, waiting);
            let level = self.baseline_byte(is);
            let next = self.b.alloc_bit(SCHEDULER_FLAG_WIDTH, false);
            self.b
                .emit(SIRInstruction::Mux(next, waiting, level, baseline));
            self.store(self.slots.baseline[index], SCHEDULER_FLAG_WIDTH, next);
            let one = self.constant(1, 1);
            let stays = self.binary(1, wake, BinaryOp::Xor, one);
            let still = self.binary(1, waiting, BinaryOp::And, stays);
            let flag = self.widen(still, SCHEDULER_FLAG_WIDTH);
            self.store(self.slots.waiting[index], SCHEDULER_FLAG_WIDTH, flag);
            let bit = self.bit(wake, ordinal);
            woken = self.binary(MAX_KERNELS, woken, BinaryOp::Or, bit);
        }
        let ready = self.load(self.slots.ready, MAX_KERNELS);
        let ready = self.binary(MAX_KERNELS, ready, BinaryOp::Or, woken);
        self.store(self.slots.ready, SCHEDULER_TIME_WIDTH, ready);
    }

    /// Publish the staged values of `sites` whose pending flag is set into
    /// their targets, clearing the flags (`publish_nba`).
    fn publish(&mut self, sites: &[&Staged]) {
        for site in sites {
            let pending = self.load(site.pending, 1);
            let publish = self.b.new_block();
            let next = self.b.new_block();
            self.b.seal_block(SIRTerminator::Branch {
                cond: pending,
                true_block: (publish, Vec::new()),
                false_block: (next, Vec::new()),
            });
            self.b.switch_to_block(publish);
            let clear = self.constant(0, 1);
            self.store(site.pending, 1, clear);
            let value = self.load_state(site.value, 0, site.width);
            self.store_state(site.target, site.lsb, site.width, value);
            if self.settle_reads(site.target) {
                self.mark_dirty();
            }
            self.b.seal_block(SIRTerminator::Jump(next, Vec::new()));
            self.b.switch_to_block(next);
        }
    }

    /// Whether combinational logic reads `address`, when settles may be
    /// skipped.
    fn settle_reads(&self, address: AbsoluteAddr) -> bool {
        let storage = self.shared.get(&address).copied().unwrap_or(address);
        self.settle_reads
            .is_some_and(|reads| reads.contains(&storage))
    }

    /// Record that state combinational logic reads may have changed.
    fn mark_dirty(&mut self) {
        if self.settle_reads.is_some() {
            let one = self.constant(1, SCHEDULER_FLAG_WIDTH);
            self.store(self.slots.dirty, SCHEDULER_FLAG_WIDTH, one);
        }
    }

    /// Inline `unit` as [`Self::inline`] does, and record a change of what
    /// combinational logic reads after it when it may write any.
    fn inline_writer(&mut self, unit: &Unit, finish_exit: celox_sir::BlockId) {
        self.inline(unit, finish_exit);
        let writes = instructions(unit).any(|inst| match inst {
            SIRInstruction::Store(target, ..) | SIRInstruction::Commit(_, target, ..) => {
                self.settle_reads(target.absolute_addr())
            }
            _ => false,
        });
        if writes {
            self.mark_dirty();
        }
    }

    /// The unit, once every block is sealed.
    fn finish(mut self) -> Unit {
        self.emit_exits();
        let (mut blocks, register_map, _) = self.b.drain();
        // Exits a unit never takes are left out: the `$finish` exit of one
        // without kernels whose domains and combinational logic record no
        // `$finish`, and the exits of the paths it does not generate.
        let mut reached = HashSet::default();
        let mut pending = vec![celox_sir::BlockId(0)];
        while let Some(block) = pending.pop() {
            if !reached.insert(block) {
                continue;
            }
            match &blocks[&block].terminator {
                SIRTerminator::Jump(target, _) => pending.push(*target),
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => pending.extend([true_block.0, false_block.0]),
                SIRTerminator::Switch { cases, default, .. } => {
                    pending.extend(cases.iter().map(|case| case.target));
                    pending.push(*default);
                }
                SIRTerminator::Return | SIRTerminator::Error(_) => {}
            }
        }
        blocks.retain(|block, _| reached.contains(block));
        Unit {
            entry_block_id: celox_sir::BlockId(0),
            blocks,
            register_map,
        }
    }

    fn emit_exits(&mut self) {
        for (block, exit) in std::mem::take(&mut self.exits) {
            self.b.switch_to_block(block);
            let code = self.constant(exit as u64, SCHEDULER_FLAG_WIDTH);
            self.store(self.slots.exit, SCHEDULER_FLAG_WIDTH, code);
            self.b.seal_block(SIRTerminator::Return);
        }
    }

    /// Run the stateful part of a deferred settle (`Lazy::eager`) where the
    /// reference settles, then continue at `target`, as
    /// [`Self::settle_then`]: one copy serves every caller.
    fn eager_then(&mut self, eager: &[Unit], target: celox_sir::BlockId) {
        if eager.is_empty() {
            self.b.seal_block(SIRTerminator::Jump(target, Vec::new()));
            return;
        }
        let case = match self
            .eager_continuations
            .iter()
            .position(|&block| block == target)
        {
            Some(index) => index,
            None => {
                self.eager_continuations.push(target);
                self.eager_continuations.len() - 1
            }
        };
        let value = self.constant(case as u64 + 1, SCHEDULER_FLAG_WIDTH);
        self.store(self.slots.cont, SCHEDULER_FLAG_WIDTH, value);
        let block = *self.eager.get_or_insert_with(|| self.b.new_block());
        self.b.seal_block(SIRTerminator::Jump(block, Vec::new()));
    }

    /// Emit the shared stateful part of the settle, when a caller uses it:
    /// skipped when the settle's inputs did not change since the last one.
    fn emit_eager(
        &mut self,
        eager: &[Unit],
        finish_exit: celox_sir::BlockId,
        internal_exit: celox_sir::BlockId,
    ) {
        let Some(block) = self.eager else {
            return;
        };
        self.b.switch_to_block(block);
        let dirty = self.load(self.slots.dirty, SCHEDULER_FLAG_WIDTH);
        let zero = self.constant(0, SCHEDULER_FLAG_WIDTH);
        let changed = self.binary(1, dirty, BinaryOp::Ne, zero);
        let run = self.b.new_block();
        let done = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: changed,
            true_block: (run, Vec::new()),
            false_block: (done, Vec::new()),
        });
        self.b.switch_to_block(run);
        for unit in eager {
            self.inline(unit, finish_exit);
        }
        self.b.seal_block(SIRTerminator::Jump(done, Vec::new()));
        self.b.switch_to_block(done);
        let continuation = self.load(self.slots.cont, SCHEDULER_FLAG_WIDTH);
        let cases = self
            .eager_continuations
            .iter()
            .enumerate()
            .map(|(index, &target)| SIRSwitchCase {
                value: (index as u8 + 1).into(),
                target,
            })
            .collect();
        self.b.seal_block(SIRTerminator::Switch {
            selector: continuation,
            cases,
            default: internal_exit,
        });
    }

    /// Run the shared settle, then continue at `target`. The registers
    /// `target` and its successors use must be defined in it or in the slot
    /// loop's header, which alone dominates every entry of the settle.
    fn settle_then(&mut self, target: celox_sir::BlockId) {
        let case = match self.continuations.iter().position(|&block| block == target) {
            Some(index) => index,
            None => {
                self.continuations.push(target);
                self.continuations.len() - 1
            }
        };
        let value = self.constant(case as u64 + 1, SCHEDULER_FLAG_WIDTH);
        self.store(self.slots.cont, SCHEDULER_FLAG_WIDTH, value);
        self.b
            .seal_block(SIRTerminator::Jump(self.comb, Vec::new()));
    }

    /// Emit the shared settle: combinational logic, skipped when its inputs
    /// did not change since the last one, as it would write what it already
    /// holds; then the continuation of its caller.
    fn emit_settle(
        &mut self,
        units: &[Unit],
        finish_exit: celox_sir::BlockId,
        internal_exit: celox_sir::BlockId,
    ) {
        self.b.switch_to_block(self.comb);
        if self.settle_reads.is_some() {
            let dirty = self.load(self.slots.dirty, SCHEDULER_FLAG_WIDTH);
            let zero = self.constant(0, SCHEDULER_FLAG_WIDTH);
            let changed = self.binary(1, dirty, BinaryOp::Ne, zero);
            let settle = self.b.new_block();
            let settled = self.b.new_block();
            self.b.seal_block(SIRTerminator::Branch {
                cond: changed,
                true_block: (settle, Vec::new()),
                false_block: (settled, Vec::new()),
            });
            self.b.switch_to_block(settle);
            for unit in units {
                self.inline(unit, finish_exit);
            }
            let clean = self.constant(0, SCHEDULER_FLAG_WIDTH);
            self.store(self.slots.dirty, SCHEDULER_FLAG_WIDTH, clean);
            self.b.seal_block(SIRTerminator::Jump(settled, Vec::new()));
            self.b.switch_to_block(settled);
        } else {
            for unit in units {
                self.inline(unit, finish_exit);
            }
        }
        let continuation = self.load(self.slots.cont, SCHEDULER_FLAG_WIDTH);
        let cases = self
            .continuations
            .iter()
            .enumerate()
            .map(|(index, &target)| SIRSwitchCase {
                value: (index as u8 + 1).into(),
                target,
            })
            .collect();
        self.b.seal_block(SIRTerminator::Switch {
            selector: continuation,
            cases,
            default: internal_exit,
        });
    }

    /// Inline `unit`. A `$finish` record it emits ends the run after it, as
    /// the reference loop ends the slot after the unit that emitted one.
    fn inline(&mut self, unit: &Unit, finish_exit: celox_sir::BlockId) {
        if self.finish_sites.is_empty() || !emits_finish(unit, self.finish_sites) {
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

/// The waits of `waits` as (kernel, its bit in the ready mask, signal,
/// edge).
fn waits_of(
    waits: &[(usize, &Wait)],
    ordinals: &[usize],
) -> Vec<(usize, usize, AbsoluteAddr, CounterKind)> {
    waits
        .iter()
        .map(|(index, wait)| (*index, ordinals[*index], wait.signal, wait.kind))
        .collect()
}

/// The place of each kernel among those the scheduler runs, in kernel
/// order; unused for the others.
fn ordinals(kernels: &[Option<Kernel>]) -> Vec<usize> {
    let mut next = 0;
    kernels
        .iter()
        .map(|kernel| {
            let ordinal = next;
            next += usize::from(kernel.is_some());
            ordinal
        })
        .collect()
}

/// Whether every two stable homes of `layout` either start at the same
/// offset or do not overlap.
fn homes_overlap_only_whole(layout: &crate::backend::MemoryLayout) -> bool {
    let mut homes: Vec<(usize, usize)> = layout
        .offsets
        .iter()
        .map(|(address, &offset)| {
            let bytes = match layout.unpacked_arrays.get(address) {
                Some(array) => array.plane_size,
                None => layout
                    .widths
                    .get(address)
                    .map_or(0, |width| width.div_ceil(8)),
            };
            (offset, offset + bytes)
        })
        .collect();
    // The widest of the homes that start at one offset stands for them.
    homes.sort_unstable_by_key(|&(start, end)| (start, std::cmp::Reverse(end)));
    homes.dedup_by_key(|home| home.0);
    homes.windows(2).all(|pair| pair[0].1 <= pair[1].0)
}

/// The storage `unit` reads.
fn loads(unit: &Unit) -> impl Iterator<Item = AbsoluteAddr> + '_ {
    instructions(unit).filter_map(|inst| match inst {
        SIRInstruction::Load(_, source, ..) | SIRInstruction::Commit(source, ..) => {
            Some(source.absolute_addr())
        }
        _ => None,
    })
}

/// Whether a settle of `units` may read what an earlier settle left: a load
/// of storage they write (`stores`, by representative) that stores earlier
/// in the same settle do not cover on every path.
fn retains_state(
    units: &[Unit],
    storage: &impl Fn(AbsoluteAddr) -> AbsoluteAddr,
    stores: &HashSet<AbsoluteAddr>,
) -> bool {
    /// The bits of each home a settle has stored so far, as sorted,
    /// disjoint `[start, end)` ranges.
    type Covered = HashMap<(u32, AbsoluteAddr), Vec<(usize, usize)>>;
    fn add(covered: &mut Covered, key: (u32, AbsoluteAddr), start: usize, end: usize) {
        let ranges = covered.entry(key).or_default();
        ranges.push((start, end));
        ranges.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
        for &(start, end) in ranges.iter() {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        *ranges = merged;
    }
    fn meet(a: &Covered, b: &Covered) -> Covered {
        let mut met = Covered::default();
        for (key, left) in a {
            let Some(right) = b.get(key) else { continue };
            let both: Vec<(usize, usize)> = left
                .iter()
                .flat_map(|&(ls, le)| {
                    right.iter().filter_map(move |&(rs, re)| {
                        let (start, end) = (ls.max(rs), le.min(re));
                        (start < end).then_some((start, end))
                    })
                })
                .collect();
            if !both.is_empty() {
                met.insert(*key, both);
            }
        }
        met
    }
    // Runs `block` from `covered`; `false` when a load reads what it does
    // not cover.
    let step = |block: &celox_sir::BasicBlock<RegionedAbsoluteAddr>, covered: &mut Covered| {
        for inst in &block.instructions {
            match inst {
                SIRInstruction::Load(_, source, offset, width)
                | SIRInstruction::Commit(source, _, offset, width, _) => {
                    let home = storage(source.absolute_addr());
                    if stores.contains(&home) {
                        let read = offset.constant_bit_offset().is_some_and(|start| {
                            covered.get(&(source.region, home)).is_some_and(|ranges| {
                                ranges
                                    .iter()
                                    .any(|&(s, e)| s <= start && start + width <= e)
                            })
                        });
                        if !read {
                            return false;
                        }
                    }
                }
                _ => {}
            }
            match inst {
                SIRInstruction::Store(target, offset, width, ..)
                | SIRInstruction::Commit(_, target, offset, width, _) => {
                    if let Some(start) = offset.constant_bit_offset() {
                        add(
                            covered,
                            (target.region, storage(target.absolute_addr())),
                            start,
                            start + width,
                        );
                    }
                }
                _ => {}
            }
        }
        true
    };
    let mut entry = Covered::default();
    for unit in units {
        // What every path stores before each block, from the paths found
        // so far: a fixed point from above, as loops only remove.
        let mut before: HashMap<celox_sir::BlockId, Covered> = HashMap::default();
        before.insert(unit.entry_block_id, entry.clone());
        let mut after: HashMap<celox_sir::BlockId, Covered> = HashMap::default();
        let mut changed = true;
        while changed {
            changed = false;
            let mut ids: Vec<_> = unit.blocks.keys().copied().collect();
            ids.sort_unstable();
            for id in ids {
                let Some(start) = before.get(&id).cloned() else {
                    continue;
                };
                let mut covered = start;
                if !step(&unit.blocks[&id], &mut covered) {
                    return true;
                }
                if after.get(&id) == Some(&covered) {
                    continue;
                }
                let successors: Vec<_> = match &unit.blocks[&id].terminator {
                    SIRTerminator::Jump(target, _) => vec![*target],
                    SIRTerminator::Branch {
                        true_block,
                        false_block,
                        ..
                    } => vec![true_block.0, false_block.0],
                    SIRTerminator::Switch { cases, default, .. } => cases
                        .iter()
                        .map(|case| case.target)
                        .chain([*default])
                        .collect(),
                    SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
                };
                for successor in successors {
                    let next = match before.get(&successor) {
                        Some(known) => meet(known, &covered),
                        None => covered.clone(),
                    };
                    before.insert(successor, next);
                }
                after.insert(id, covered);
                changed = true;
            }
        }
        // The next unit starts from what every return of this one stored.
        let mut returns = unit
            .blocks
            .iter()
            .filter(|(_, block)| matches!(block.terminator, SIRTerminator::Return))
            .filter_map(|(id, _)| after.get(id));
        entry = match returns.next() {
            Some(first) => returns.fold(first.clone(), |met, next| meet(&met, next)),
            None => Covered::default(),
        };
    }
    false
}

/// Whether `unit` may end with an error.
fn fails(unit: &Unit) -> bool {
    unit.blocks
        .values()
        .any(|block| matches!(block.terminator, SIRTerminator::Error(_)))
}

/// Whether `unit` may record a `$finish`, one of `finish_sites`.
fn emits_finish(unit: &Unit, finish_sites: &HashSet<u32>) -> bool {
    instructions(unit).any(|inst| {
        matches!(inst, SIRInstruction::RuntimeEvent { site_id, .. }
            if finish_sites.contains(site_id))
    })
}

/// The longest delay `unit` stores into `delay`: `u64::MAX` unless every
/// one is a constant.
fn delay_bound(unit: &Unit, delay: AbsoluteAddr) -> u64 {
    let constants: HashMap<RegisterId, u64> = instructions(unit)
        .filter_map(|inst| match inst {
            SIRInstruction::Imm(register, value) => u64::try_from(&value.payload)
                .ok()
                .map(|value| (*register, value)),
            _ => None,
        })
        .collect();
    let delay = stable(delay);
    instructions(unit)
        .filter_map(|inst| match inst {
            SIRInstruction::Store(target, offset, _, value, ..) if *target == delay => {
                Some(match offset {
                    SIROffset::Static(0) => constants.get(value).copied().unwrap_or(u64::MAX),
                    _ => u64::MAX,
                })
            }
            SIRInstruction::Commit(_, target, ..) if *target == delay => Some(u64::MAX),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

fn stable(address: AbsoluteAddr) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address)
}

fn instructions(unit: &Unit) -> impl Iterator<Item = &SIRInstruction<RegionedAbsoluteAddr>> {
    unit.blocks.values().flat_map(|block| &block.instructions)
}

/// Whether every status `unit` stores into `status` is a constant among
/// `allowed`.
fn statuses_only(unit: &Unit, status: AbsoluteAddr, allowed: &[ProcessStatus]) -> bool {
    let constants: HashMap<RegisterId, u64> = instructions(unit)
        .filter_map(|inst| match inst {
            SIRInstruction::Imm(register, value) => u64::try_from(&value.payload)
                .ok()
                .map(|value| (*register, value)),
            _ => None,
        })
        .collect();
    let allowed: Vec<u64> = allowed
        .iter()
        .map(|status| u64::from(status.code()))
        .collect();
    let status = stable(status);
    instructions(unit).all(|inst| match inst {
        SIRInstruction::Store(target, _, _, value, ..) if *target == status => constants
            .get(value)
            .is_some_and(|code| allowed.contains(code)),
        SIRInstruction::Commit(_, target, ..) => *target != status,
        _ => true,
    })
}

/// Instrument the generated settle after eligibility/retention analysis, so
/// measuring it does not turn a pure combinational cone into a stateful one.
#[cfg(all(test, any(target_arch = "x86_64", target_arch = "aarch64")))]
pub(crate) fn count_test_settles(
    program: &mut LaidOutProgram,
    counter: AbsoluteAddr,
    fused_counter: AbsoluteAddr,
) -> usize {
    fn counted(units: &[Unit], counter: AbsoluteAddr) -> Vec<Unit> {
        let mut b = Builder::new();
        let count = b.alloc_bit(64, false);
        let one = b.alloc_bit(64, false);
        let next = b.alloc_bit(64, false);
        let address = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, counter);
        b.emit(SIRInstruction::Load(
            count,
            address,
            SIROffset::Static(0),
            64,
        ));
        b.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
        b.emit(SIRInstruction::Binary(next, count, BinaryOp::Add, one));
        b.emit(SIRInstruction::Store(
            address,
            SIROffset::Static(0),
            64,
            next,
            vec![],
            vec![],
        ));
        for unit in units {
            b.inline_execution_unit(unit);
        }
        b.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = b.drain();
        vec![Unit {
            entry_block_id: celox_sir::BlockId(0),
            blocks,
            register_map,
        }]
    }
    let schema = program.runtime.runtime_schema.scheduler.clone().unwrap();
    let mut plan = Plan::new(program, &schema.slots, true).unwrap();
    let mut fused = 0;
    if let Some(lazy) = &mut plan.lazy {
        for domain in &mut lazy.domains {
            if let DomainSettle::Fused(units) = domain {
                *units = counted(units, fused_counter);
                fused += 1;
            }
        }
    }
    let instrumented = counted(&program.sir.eval_comb, counter);
    let original = std::mem::replace(&mut program.sir.eval_comb, instrumented);
    let generated = plan.generate(program, &schema.slots);
    program.sir.eval_comb = original;
    program.sir.processes[schema.unit.unwrap()] = generated;
    fused
}

/// Whether the compiled scheduler of `program` defers its settles, and
/// whether its loads alone would have allowed that.
#[cfg(test)]
pub(crate) fn defers_settles_for_test(program: &LaidOutProgram) -> (bool, bool) {
    let schema = program.runtime.runtime_schema.scheduler.clone().unwrap();
    let plan = Plan::new(program, &schema.slots, true).unwrap();
    let shared = program.shared_storage();
    let storage = |address: AbsoluteAddr| shared.get(&address).copied().unwrap_or(address);
    let stores: HashSet<AbsoluteAddr> = program
        .sir
        .eval_comb
        .iter()
        .flat_map(instructions)
        .filter_map(|inst| {
            inst.memory_write()
                .map(|write| storage(write.addr.absolute_addr()))
        })
        .collect();
    (
        plan.lazy.is_some(),
        !retains_state(&program.sir.eval_comb, &storage, &stores),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_uses_logical_ranges_for_packed_elements() {
        let address = AbsoluteAddr {
            instance_id: celox_design::InstanceId(0),
            var_id: celox_design::StateObjectId(0),
        };
        let offsets = [
            SIROffset::Static(0),
            SIROffset::PackedElements {
                bit_offset: 0,
                element_width: 4,
            },
        ];
        for store in &offsets {
            for load in &offsets {
                for width in [8, 32] {
                    let mut b = Builder::new();
                    let value = b.alloc_bit(32, false);
                    let read = b.alloc_bit(32, false);
                    b.emit(SIRInstruction::Imm(value, SIRValue::new(0u32)));
                    b.emit(SIRInstruction::Store(
                        stable(address),
                        store.clone(),
                        width,
                        value,
                        vec![],
                        vec![],
                    ));
                    b.emit(SIRInstruction::Load(
                        read,
                        stable(address),
                        load.clone(),
                        32,
                    ));
                    b.seal_block(SIRTerminator::Return);
                    let (blocks, register_map, _) = b.drain();
                    let unit = Unit {
                        entry_block_id: celox_sir::BlockId(0),
                        blocks,
                        register_map,
                    };
                    assert_eq!(
                        retains_state(&[unit], &|a| a, &HashSet::from_iter([address])),
                        width < 32,
                        "store={store:?} load={load:?} width={width}"
                    );
                }
            }
        }
    }
}
