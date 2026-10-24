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
//! The runtime runs the unit only for slots that touch nothing else (see
//! `celox_runtime::SimulationState::run_until_observed`): every other slot,
//! and every design or kernel this module does not qualify, runs in the
//! reference loop.

use celox_design::{
    CounterKind, DomainKind, ProcessStatus, RuntimeEventKind, SCHEDULER_CLOCKS,
    SCHEDULER_FLAG_WIDTH, SCHEDULER_TIME_WIDTH, STABLE_REGION, SchedulerExit, SchedulerSlots,
    TIME_SLOT_WIDTH, schedule::ScheduleModel,
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
pub(crate) fn attach(program: &mut LaidOutProgram) {
    let Some(schema) = program.runtime.runtime_schema.scheduler.clone() else {
        return;
    };
    let kernels = program.runtime.runtime_schema.processes.len();
    let (fallback, unit, events, clocks) = match Plan::new(program, &schema.slots) {
        Err(reason) => (vec![Some(reason); kernels], None, Vec::new(), Vec::new()),
        Ok(plan) => {
            let fallback = plan.fallback.clone();
            // Without kernels, slots of host clocks alone.
            let unit = (plan.kernels.iter().any(Option::is_some) || !plan.clocks.is_empty())
                .then(|| plan.generate(program, &schema.slots));
            let events = plan
                .edged()
                .into_iter()
                .map(|event| plan.events[event].signal)
                .collect();
            let clocks = plan
                .clocks
                .iter()
                .map(|&event| plan.events[event].signal)
                .collect();
            (fallback, unit, events, clocks)
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
    /// A clock edge rather than an asynchronous reset.
    clock: bool,
    eval_apply: Vec<Unit>,
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
/// reference's state is the last settle's.
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
                    let aliased = map
                        .keys()
                        .any(|key| *key != signal && topology.canonical(*key) == signal);
                    if aliased {
                        return Err("an event domain has no separate units".into());
                    }
                    Ok(map.get(&signal).cloned())
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
                eval_apply: units(&sir.eval_apply_ffs)?
                    .ok_or("an event domain has no separate units")?,
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
        let rtl_units = || {
            sir.eval_comb
                .iter()
                .chain(sir.eval_apply_ffs.values().flatten())
                .chain(sir.eval_only_ffs.values().flatten())
                .chain(sir.apply_ffs.values().flatten())
        };
        for unit in rtl_units() {
            if !written(unit).is_empty() {
                return Err("RTL logic drives an event signal".into());
            }
        }
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
                && !kernel_stores.iter().any(|stores| stores.contains(&signal))
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
            let reason = if slots.is_initializer {
                Some("a declaration initializer")
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
        // captures, external calls or trigger marks.
        let effects = sir.eval_comb.iter().any(|unit| {
            instructions(unit).any(|inst| match inst {
                SIRInstruction::RuntimeEvent { .. }
                | SIRInstruction::CombCaptureEvent { .. }
                | SIRInstruction::CombCaptureEnableIfChanged { .. }
                | SIRInstruction::ExternCall { .. } => true,
                SIRInstruction::Store(_, _, _, _, triggers, sites) => {
                    !triggers.is_empty() || !sites.is_empty()
                }
                SIRInstruction::Commit(_, _, _, _, triggers) => !triggers.is_empty(),
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
        // so in its own slot.
        let lazy = settle_reads
            .as_ref()
            .filter(|_| !sir.eval_comb.iter().any(fails))
            .map(|_| {
                let comb_stores: HashSet<AbsoluteAddr> =
                    sir.eval_comb.iter().flat_map(stores).collect();
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
                        let bound =
                            delay_bound(&sir.processes[index], schema.processes[index].delay);
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
                }
            });
        let plan = Self {
            events,
            kernels,
            clocks,
            fallback,
            finish_sites,
            settle_reads,
            lazy,
        };
        let edged = plan.edged();
        if edged.len() > 1
            && edged.iter().any(|&event| {
                plan.events[event].eval_only.is_none() || plan.events[event].apply.is_none()
            })
        {
            return Err("an event domain has no separate units".into());
        }
        Ok(plan)
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
        // Every run starts dirty, and this mark is required, not a margin:
        // it is the only cover for a unit that exited to the `$finish` exit
        // partway through, whose mark after the unit never ran, and for
        // state a checkpoint or a state file restored between runs, which
        // no mark sees.
        g.mark_dirty();
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
        // The host reads the run's results settled.
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
            g.store(slot, TIME_SLOT_WIDTH, time);
        }
        if schema.time_slots.iter().any(|&slot| g.settle_reads(slot)) {
            g.mark_dirty();
        }
        // The levels the slot starts from, the baseline of its edges, for an
        // exit in the middle of the slot; those of the clocks' events below,
        // when the run has clocks.
        let mut levels = g.constant(0, MAX_EVENTS);
        for (bit, &event) in edged.iter().enumerate() {
            if self.clocks.contains(&event) {
                continue;
            }
            let level = g.level(&self.events[event]);
            let set = g.constant(1 << bit, MAX_EVENTS);
            let unset = g.constant(0, MAX_EVENTS);
            let value = g.b.alloc_bit(MAX_EVENTS, false);
            g.b.emit(SIRInstruction::Mux(value, level, set, unset));
            levels = g.binary(MAX_EVENTS, levels, BinaryOp::Or, value);
        }
        g.store(slots.levels, SCHEDULER_TIME_WIDTH, levels);
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
            let mut levels = g.load(slots.levels, MAX_EVENTS);
            for (bit, &index) in self.clocks.iter().enumerate() {
                let event = &self.events[index];
                let next_edge = g.clock_next(bit);
                let on = g.mask_bit(armed, bit);
                let due = g.binary(1, next_edge, BinaryOp::Eq, time);
                let due = g.binary(1, due, BinaryOp::And, on);
                let value = g.mask_bit(values, bit);
                let was = g.level(event);
                let position = edged
                    .iter()
                    .position(|&edged| edged == index)
                    .expect("clocks are edged");
                let level = g.bit(was, position);
                levels = g.binary(MAX_EVENTS, levels, BinaryOp::Or, level);
                let is = g.b.alloc_bit(1, false);
                g.b.emit(SIRInstruction::Mux(is, due, value, was));
                g.store(event.signal, 1, is);
                if g.settle_reads(event.signal) {
                    g.mark_dirty();
                }
                let changed = g.binary(1, was, BinaryOp::Ne, is);
                let active = g.equals(is, u64::from(event.active));
                let edge = g.binary(1, changed, BinaryOp::And, active);
                let edge = g.bit(edge, index);
                edges = g.binary(MAX_EVENTS, edges, BinaryOp::Or, edge);
                let bit = g.bit(due, bit);
                fired = g.binary(SCHEDULER_CLOCKS, fired, BinaryOp::Or, bit);
            }
            g.store(slots.levels, SCHEDULER_TIME_WIDTH, levels);
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
                        let changed = g.binary(1, was, BinaryOp::Ne, is);
                        let active = g.equals(is, u64::from(self.events[event].active));
                        let edge = g.binary(1, changed, BinaryOp::And, active);
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
                    let level = g.widen(level, SCHEDULER_FLAG_WIDTH);
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
            Some(_) => {
                g.b.seal_block(SIRTerminator::Jump(settled, Vec::new()));
            }
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
        if self.lazy.is_some() {
            g.publish(&staged);
            g.b.seal_block(SIRTerminator::Jump(slot_end, Vec::new()));
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
                    Some(DomainSettle::Fused(units)) => units,
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
            Some(_) => {
                g.b.seal_block(SIRTerminator::Jump(slot_end, Vec::new()));
            }
            None => g.settle_then(slot_end),
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
    exits: Vec<(celox_sir::BlockId, SchedulerExit)>,
    /// See [`Plan::settle_reads`].
    settle_reads: Option<&'a HashSet<AbsoluteAddr>>,
    shared: HashMap<AbsoluteAddr, AbsoluteAddr>,
    /// The shared settle, emitted by [`Self::emit_settle`].
    comb: celox_sir::BlockId,
    /// Where the shared settle continues, by case: case `i + 1` continues
    /// at `continuations[i]`.
    continuations: Vec<celox_sir::BlockId>,
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
    /// its low byte, within its width, is not zero.
    fn level(&mut self, event: &Event) -> RegisterId {
        let width = event.width.min(8);
        let value = self.load(event.signal, width);
        let zero = self.constant(0, width);
        self.binary(1, value, BinaryOp::Ne, zero)
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

    /// The value of the one-bit signal `address`.
    fn signal_level(&mut self, address: AbsoluteAddr) -> RegisterId {
        self.load(address, 1)
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
            let zero = self.constant(0, SCHEDULER_FLAG_WIDTH);
            let was = self.binary(1, baseline, BinaryOp::Ne, zero);
            let is = self.signal_level(signal);
            let changed = self.binary(1, was, BinaryOp::Ne, is);
            let occurred = match kind {
                CounterKind::Changes => changed,
                CounterKind::Rises => self.binary(1, changed, BinaryOp::And, is),
                CounterKind::Falls => self.binary(1, changed, BinaryOp::And, was),
            };
            let wake = self.binary(1, occurred, BinaryOp::And, waiting);
            let level = self.widen(is, SCHEDULER_FLAG_WIDTH);
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
            let value = self.load(site.value, site.width);
            self.b.emit(SIRInstruction::Store(
                stable(site.target),
                SIROffset::Static(site.lsb),
                site.width,
                value,
                Vec::new(),
                Vec::new(),
            ));
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
