//! The compiled scheduler: one SIR unit that runs time slots of the process
//! kernels, as the runtime's reference loop runs them. Host operations and
//! queued NBA values use synchronous services without returning to that loop.
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

mod domains;
pub(crate) mod host_inputs;
mod nba;
mod observers;
mod ordered;
mod regions;
mod slice;
mod slot_observers;
mod steps;
mod testbench;
mod waits;

use slice::CombSlicer;

use celox_design::{
    BASELINE_TERM_SHIFT, CounterKind, DomainKind, ProcessStatus, RuntimeEventKind,
    SCHEDULER_FLAG_WIDTH, SCHEDULER_TIME_WIDTH, STABLE_REGION, SchedulerExit, SchedulerSlots,
    TIME_SLOT_WIDTH, edges::LEVEL_UNKNOWN, schedule::ScheduleModel,
};
use celox_sir::{BinaryOp, SIRBuilder, SIRSwitchCase, SIRValue, StateAccess};
use num_bigint::BigUint;
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

/// A compact native scheduler whose large bodies are separately compiled.
/// Analyze the original units before outlining, so call opacity cannot change
/// dirty tracking, finish handling, or event accounting.
#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
pub(crate) fn hotspot_scheduler(program: &LaidOutProgram) -> Result<(Unit, Vec<Unit>), String> {
    let schema = program
        .runtime
        .runtime_schema
        .scheduler
        .as_ref()
        .ok_or("no scheduler schema")?;
    schema.unit.ok_or("no scheduler unit")?;
    let mut plan = Plan::new(program, &schema.slots, true)?;
    if let Some(lazy) = &mut plan.lazy {
        for domain in &mut lazy.domains {
            if matches!(domain, DomainSettle::Fused(_)) {
                *domain = DomainSettle::Settled;
            }
        }
    }
    let base = u32::try_from(program.runtime.runtime_schema.extern_functions.len())
        .map_err(|_| "too many extern functions")?;
    let mut callees = Vec::new();
    let unit = plan.generate_with_outlines(program, &schema.slots, Some(&mut callees), base);
    Ok((unit, callees))
}

/// The narrowest event mask: one 64-bit word, which every design with at
/// most 64 events uses. A design with more uses as many words as its events
/// need.
const MIN_EVENT_BITS: usize = 64;

/// The widest selector of a nonblocking site index.
const SITE_BITS: usize = 8;
// Bytes in the private scheduler control word, after the assertion phase.
const GATES_JUDGED_BIT: usize = 24;
const GATE_SOURCE_EDGED_BIT: usize = 32;

const ACTIVE_COMMIT: &str = "the process drives or waits for a clock whose domain commits blocking results in the active region";

const DELAY_STATUSES: [ProcessStatus; 7] = [
    ProcessStatus::Nba,
    ProcessStatus::Delay,
    ProcessStatus::InactiveDelay,
    ProcessStatus::Done,
    ProcessStatus::Finish,
    ProcessStatus::HostRequest,
    ProcessStatus::Yield,
];

const WAIT_STATUSES: [ProcessStatus; 10] = [
    ProcessStatus::Wait,
    ProcessStatus::Pending,
    ProcessStatus::AssertionObserved,
    ProcessStatus::AssertionReactive,
    ProcessStatus::DeferActive,
    ProcessStatus::Nba,
    ProcessStatus::Done,
    ProcessStatus::Finish,
    ProcessStatus::HostRequest,
    ProcessStatus::Yield,
];

/// The statuses of a kernel that both waits and delays.
const MIXED_STATUSES: [ProcessStatus; 12] = [
    ProcessStatus::Wait,
    ProcessStatus::Pending,
    ProcessStatus::AssertionObserved,
    ProcessStatus::AssertionReactive,
    ProcessStatus::DeferActive,
    ProcessStatus::Nba,
    ProcessStatus::Delay,
    ProcessStatus::InactiveDelay,
    ProcessStatus::Done,
    ProcessStatus::Finish,
    ProcessStatus::HostRequest,
    ProcessStatus::Yield,
];

/// The normal C ABI supplies native file writes without suspending into the
/// host scheduler. Register the symbol only for native-target compilation.
pub(crate) fn enable_native_services(program: &mut LaidOutProgram) {
    let schema = &mut program.runtime.runtime_schema;
    let name = crate::simulator::native_io::SYMBOL;
    let needs_service = schema.scheduler.is_some()
        || !schema.processes.is_empty()
        || !schema.comb_observers.is_empty()
        || schema
            .host_request_sites
            .iter()
            .any(crate::simulator::native_io::supported)
        || schema
            .processes
            .iter()
            .zip(&program.sir.processes)
            .any(|(slots, unit)| returns_status(unit, slots.status, ProcessStatus::Yield));
    if !needs_service
        || schema
            .extern_functions
            .iter()
            .any(|function| function.name == name)
    {
        return;
    }
    schema.extern_functions.push(celox_design::ExternFunction {
        name: name.into(),
        signature: crate::simulator::native_io::signature(),
    });
}

/// Flag the `$finish` records of every function that makes records, and
/// count all its records, as [`Generator::instrument`] does for the units
/// the scheduler inlines: the scheduler inlines a callee's calls, not its
/// body, so the callee does it itself. Only a compiled run reads the slots
/// this writes, and it clears them when it starts.
fn instrument_recording_functions(
    program: &mut LaidOutProgram,
    slots: &SchedulerSlots<AbsoluteAddr>,
) {
    let finish_sites: HashSet<u32> = program
        .runtime
        .runtime_schema
        .runtime_event_sites
        .iter()
        .enumerate()
        .filter(|(_, site)| matches!(site.kind, RuntimeEventKind::Finish))
        .map(|(index, _)| index as u32)
        .collect();
    let mut changed = false;
    for function in &mut program.sir.functions {
        if instructions(&function.unit)
            .any(|inst| matches!(inst, SIRInstruction::RuntimeEvent { .. }))
        {
            function.unit =
                Generator::instrument_records(&function.unit, &finish_sites, slots, true, false);
            changed = true;
        }
    }
    // The calls of a function now write the slots too.
    if changed {
        program.sir.summarize_calls();
    }
}

/// Generate the compiled scheduler of `program`, if it qualifies, and record
/// for every process kernel whether the scheduler runs it.
pub(crate) fn attach(program: &mut LaidOutProgram, wiring_clocks: bool) {
    let Some(schema) = program.runtime.runtime_schema.scheduler.clone() else {
        return;
    };
    instrument_recording_functions(program, &schema.slots);
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
                let unit = (plan.kernels.iter().any(Option::is_some)
                    || !plan.clocks.is_empty()
                    || (cfg!(all(target_arch = "x86_64", not(feature = "arm64-codegen")))
                        && plan.service_function.is_some()
                        && program.runtime.runtime_schema.scheduler.is_some()))
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
    /// Unknown-kind domains observe any settled level change.
    any_settled: bool,
    /// A clock edge rather than an asynchronous reset.
    clock: bool,
    eval_apply: Vec<Unit>,
    /// Blocking results make the reference evaluate before NBA publication.
    active_commit: bool,
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
    /// For a kernel that waits for edges, the edges and its staged
    /// nonblocking assignments; `None` for a kernel that only delays.
    wait: Option<Wait>,
    /// The kernel suspends for delays. A kernel that also waits tells the
    /// two apart by the status it suspended with.
    delays: bool,
}

/// Event controls of a kernel, including their dependency union and snapshots.
struct Wait {
    /// Expressions used by dependency analysis. A raw leaf keeps them in
    /// snapshot order; the compact path keeps distinct edges in site order.
    terms: Vec<(AbsoluteAddr, CounterKind)>,
    /// Each site's index into `terms` (compact) or `alternatives` (general).
    site_terms: Vec<usize>,
    /// Occurrences that can be hidden by later writes in the same activation.
    counters: Vec<Option<AbsoluteAddr>>,
    /// A common event list, or a leaf of the general site dispatch.
    raw: Option<celox_design::schedule::CompiledWaitLayout>,
    /// General event controls, selected by the process's full-width site id.
    /// The outer terms/counters are their union for dependency analysis.
    alternatives: Vec<Wait>,
    /// Conditions judged at the event occurrence, never on a gate-only change.
    gates: Vec<Option<celox_design::WaitGate<AbsoluteAddr>>>,
    /// RTL or another waiting kernel can change a watched expression.
    reactive: bool,
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

struct Incremental {
    source: AbsoluteAddr,
    sources: HashSet<AbsoluteAddr>,
    units: Vec<Unit>,
}

struct CountedEvent {
    counter: AbsoluteAddr,
    baseline_offset: usize,
    events: Vec<usize>,
}

struct Plan {
    /// Direct host event writes, including asynchronous resets with no clock.
    host_edges: Vec<usize>,
    /// Zero or computed delays require an Inactive pass before NBA publication.
    inactive: Vec<(usize, AbsoluteAddr)>,
    observers: Option<observers::Observers>,
    counted: Vec<CountedEvent>,
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
    service_function: Option<u32>,
    ordered: Option<ordered::Ordered>,
    service_processes: HashSet<usize>,
    nba_processes: HashSet<usize>,
    /// Native fixed-point work lists need runtime bounds, not a DAG restriction.
    cyclic: bool,
    /// The storage combinational logic reads, when a settle whose inputs did
    /// not change may be skipped; `None` when every settle runs.
    settle_reads: Option<HashSet<AbsoluteAddr>>,
    /// Where the settles run when they wait until their results are read;
    /// `None` when every settle runs where the reference loop's does.
    lazy: Option<Lazy>,
    /// A single clock write can invalidate only its forward cone. Stateful
    /// activations remain included; mixed writes request the ordinary settle.
    incremental: Option<Incremental>,
    /// The clocks derived from the events the slots change, in event order,
    /// as indices into [`Self::events`]: their domains run in the waves of
    /// a slot (see `Generator::batch`).
    derived: Vec<usize>,
    active: Vec<usize>,
    /// The events the slots change that gate clocks computed
    /// combinationally from them, with those clocks: their edges are judged
    /// when the source changes.
    gating: Vec<(usize, Vec<usize>)>,
    /// The width of an event mask: one bit per event, at least
    /// [`MIN_EVENT_BITS`] and a whole number of words.
    event_bits: usize,
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
        let topology = &model.events;
        let native_observers = cfg!(all(target_arch = "x86_64", not(feature = "arm64-codegen")))
            && schema
                .extern_functions
                .iter()
                .any(|function| function.name == crate::simulator::native_io::SYMBOL);
        if !native_observers && !schema.comb_observers.is_empty() {
            return Err("the design observes combinational processes".into());
        }
        if slots.wake.len() != schema.processes.len() {
            return Err("the scheduler slots do not match the kernels".into());
        }
        let sir = &program.sir;
        let comb_fatal = sir
            .eval_comb
            .iter()
            .any(|unit| instructions(unit).any(crate::ir::may_raise_fatal_capture));
        if comb_fatal && !native_observers {
            return Err("combinational logic has fatal assertions".into());
        }
        let rtl_units = || {
            sir.eval_comb
                .iter()
                .chain(sir.eval_apply_ffs.values().flatten())
                .chain(sir.eval_only_ffs.values().flatten())
                .chain(sir.apply_ffs.values().flatten())
        };
        let planned = || {
            rtl_units()
                .chain(sir.eval_comb_apply_ffs.values().flatten())
                .chain(sir.processes.iter().take(schema.processes.len()))
                .flat_map(instructions)
        };
        // A native kernel may read and write any state, which none of the
        // storage sets below can name (see `read_storage`).
        if planned().any(SIRInstruction::accesses_unknown_state) {
            return Err("a unit may access any state".into());
        }

        let shared = program.shared_storage();
        let storage = |address: AbsoluteAddr| shared.get(&address).copied().unwrap_or(address);
        let addresses = crate::edge_plans::event_addresses(program);
        let event_bits = addresses.len().next_multiple_of(64).max(MIN_EVENT_BITS);
        // The masks of the edges and levels of a slot live in these slots,
        // which the frontend sized for the events it expected.
        for (name, slot) in [
            ("levels", slots.levels),
            ("unknown_levels", slots.unknown_levels),
            ("exit_arg", slots.exit_arg),
        ] {
            if program.layout().widths.get(&slot).copied().unwrap_or(0) < event_bits {
                return Err(format!(
                    "the design has more events than its scheduler's {name} slot holds"
                ));
            }
        }
        let mut events = Vec::with_capacity(addresses.len());
        let mut event_storage: HashMap<AbsoluteAddr, Vec<usize>> = HashMap::default();
        for &signal in &addresses {
            let (active, clock) = match model.kinds.get(&signal) {
                Some(DomainKind::ClockPosedge) => (1, true),
                Some(DomainKind::ClockNegedge) => (0, true),
                Some(DomainKind::ResetAsyncHigh) => (1, false),
                Some(DomainKind::ResetAsyncLow) => (0, false),
                Some(DomainKind::Other) | None if native_observers => (1, true),
                _ => return Err("an event is not a clock or reset edge".into()),
            };
            let units =
                |map: &HashMap<AbsoluteAddr, Vec<Unit>>| -> Result<Option<Vec<Unit>>, String> {
                    if native_observers {
                        // Match NativeBackend::compile_ff_group exactly: aliases
                        // bind the first kernel of their canonical domain. The
                        // frontend may clone that kernel under several aliases;
                        // concatenating those copies would execute it repeatedly.
                        return Ok(map
                            .iter()
                            .find(|(key, _)| topology.canonical(**key) == signal)
                            .map(|(_, units)| units.clone()));
                    }
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
                any_settled: matches!(model.kinds.get(&signal), Some(DomainKind::Other) | None),
                clock,
                eval_apply,
                active_commit: model.active_commits.contains(&signal),
                aliases: wiring_clocks
                    .then(|| crate::clock_aliases::extract(program, signal))
                    .flatten(),
                eval_only,
                apply,
            };
            let same_signal = event_storage.entry(storage(signal)).or_default();
            if !native_observers && !same_signal.is_empty() {
                return Err("two events share a signal".into());
            }
            same_signal.push(events.len());
            events.push(event);
        }
        // Event signals change only where the scheduler sees them change.
        let written = |unit: &Unit| -> Vec<usize> {
            let mut written: Vec<usize> = instructions(unit)
                .flat_map(written_storage)
                .filter_map(|target| event_storage.get(&storage(target.absolute_addr())))
                .flatten()
                .copied()
                .collect();
            written.sort_unstable();
            written.dedup();
            written
        };
        // The events RTL logic drives: clocks derived from the events the
        // scheduler's slots change, and only those (checked below).
        let rtl_written: BTreeSet<usize> = rtl_units().flat_map(written).collect();
        // The storage each unit writes.
        let stores = |unit: &Unit| -> HashSet<AbsoluteAddr> {
            instructions(unit)
                .flat_map(written_storage)
                .map(|target| storage(target.absolute_addr()))
                .collect()
        };
        let rtl_stores: HashSet<AbsoluteAddr> = rtl_units().flat_map(stores).collect();
        let opaque = |unit: &Unit| instructions(unit).any(makes_extern_calls);
        let opaque_comb = sir.eval_comb.iter().any(opaque);
        let kernel_stores: Vec<HashSet<AbsoluteAddr>> = sir
            .processes
            .iter()
            .take(schema.processes.len())
            .map(stores)
            .collect();
        let control_slots: HashSet<_> = schema
            .processes
            .iter()
            .flat_map(|process| process.iter())
            .copied()
            .map(storage)
            .collect();
        let wait_reads: Vec<_> = schema
            .wait_sites
            .iter()
            .flat_map(celox_design::WaitSite::reads)
            .collect();
        let waited: HashSet<AbsoluteAddr> = wait_reads.iter().map(|s| storage(*s)).collect();
        // Storage whose changes bump an occurrence counter: the reference
        // counts the nonblocking updates it publishes into it, which the
        // scheduler's publication does not.
        let probed: HashSet<AbsoluteAddr> = model
            .probes
            .iter()
            .map(|(source, _)| storage(source.id))
            .chain(
                model
                    .counted
                    .iter()
                    .map(|counted| storage(counted.slots.signal)),
            )
            .collect();
        // Blocking results need an intermediate Active-region settle when
        // they can wake a process, move a clock, or feed stateful combinational
        // logic before NBA publication.
        // Otherwise the existing evaluate/apply sequence has the same visible
        // ordering: processes run first, blocking results become stable during
        // evaluation, and nonblocking results publish afterward (LRM 4.5).
        let mut active_observers: HashSet<AbsoluteAddr> = waited
            .iter()
            .chain(&probed)
            .copied()
            .chain(events.iter().map(|event| storage(event.signal)))
            .collect();
        // All dependency and settle slices inspect the same immutable units.
        // Index their definitions and control dependencies only once per plan,
        // and only if one of these consumers actually needs a slice.
        let comb_slicer =
            std::cell::LazyCell::new(|| CombSlicer::new(&sir.eval_comb, shared.clone()));
        let wait_slicer = (!wait_reads.is_empty()).then(|| &*comb_slicer);
        let observed_inputs = wait_slicer
            .as_ref()
            .map(|slicer| slicer.input_signals(wait_reads.iter().copied()))
            .unwrap_or_default();
        active_observers.extend(observed_inputs.into_iter().map(storage));
        // An external call's state dependencies cannot be recovered from SIR
        // loads. Preserve every blocking publication before NBA when the
        // combinational observer cone contains such a call.
        if opaque_comb {
            active_observers.extend(rtl_stores.iter().copied());
        }
        // A stateful combinational consumer can retain an intermediate
        // blocking result even when no waiter or clock reads it. Include all
        // combinational inputs so indirect cones are covered too. Missing
        // statefulness metadata is conservative for the same reason.
        if sir
            .eval_comb_stateful
            .as_ref()
            .is_none_or(|targets| !targets.is_empty())
        {
            active_observers.extend(sir.eval_comb.iter().flat_map(loads).map(storage));
        }
        let mut unknown_gate = false;
        for clock in topology.combinational_derived.values().flatten() {
            match &clock.gate {
                Some(gate) => active_observers.extend(gate.iter().filter_map(|op| match op {
                    celox_design::GateOp::Load { address, .. } => Some(storage(*address)),
                    _ => None,
                })),
                None => match &clock.inputs {
                    Some(inputs) => active_observers.extend(inputs.iter().copied().map(storage)),
                    None => unknown_gate = true,
                },
            }
        }
        let active_commits: HashSet<usize> = events
            .iter()
            .enumerate()
            .filter(|(_, event)| model.active_commits.contains(&event.signal))
            .filter(|(_, event)| {
                unknown_gate
                    || event.eval_only.as_ref().is_none_or(|units| {
                        units.iter().flat_map(instructions).any(|inst| {
                            makes_extern_calls(inst)
                                || written_storage(inst).any(|target| {
                                    target.region == STABLE_REGION
                                        && active_observers
                                            .contains(&storage(target.absolute_addr()))
                                })
                        })
                    })
            })
            .map(|(index, _)| index)
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
        // The resets a compiled testbench writes: its unit applies those
        // writes as edges of their slots, with its clocks'. A reset other
        // events derive from combinationally keeps its writes in the host:
        // a slot would make those derived events, which a run otherwise
        // holds stable, cost every slot their waves.
        let testbench_resets: HashSet<AbsoluteAddr> = program
            .runtime
            .testbench
            .iter()
            .flat_map(|tb| tb.statements())
            .flat_map(testbench_resets)
            .filter(|&reset| {
                topology
                    .combinational_derived
                    .get(&reset)
                    .is_none_or(|derived| derived.is_empty())
            })
            .map(storage)
            .collect();
        let clocks: Vec<usize> = events
            .iter()
            .enumerate()
            .filter(|(_, event)| {
                (event.clock || testbench_resets.contains(&storage(event.signal)))
                    && event.width == 1
                    && undriven(storage(event.signal))
            })
            .map(|(index, _)| index)
            .collect();
        if clocks.len() > program.layout().widths[&slots.clock_armed]
            || clocks.len() > program.layout().widths[&slots.clock_value]
            || clocks.len() > program.layout().widths[&slots.clock_next] / SCHEDULER_TIME_WIDTH
            || clocks.len() > program.layout().widths[&slots.clock_period] / SCHEDULER_TIME_WIDTH
        {
            return Err("the compiled host clock slots have insufficient storage".into());
        }

        let service_function = schema
            .extern_functions
            .iter()
            .position(|function| function.name == crate::simulator::native_io::SYMBOL)
            .and_then(|index| u32::try_from(index).ok());
        let ordered_available = cfg!(all(target_arch = "x86_64", not(feature = "arm64-codegen")))
            && service_function.is_some();
        let nba_processes: HashSet<usize> = model
            .processes
            .iter()
            .enumerate()
            .filter(|(_, process)| {
                service_function.is_some()
                    && !process.nbas.is_empty()
                    && process.nbas.iter().all(|site| {
                        let target = storage(site.target);
                        ordered_available
                            || (site.delay == 0
                                && (!event_storage.contains_key(&target)
                                    && site.position.is_none()
                                    && !program
                                        .layout()
                                        .unpacked_arrays
                                        .contains_key(&site.target)
                                    && !probed.contains(&target)
                                    && !rtl_stores.contains(&target)
                                    && !kernel_stores
                                        .iter()
                                        .any(|stores| stores.contains(&target))))
                    })
            })
            .map(|(index, _)| index)
            .collect();
        let yielding: HashSet<usize> = schema
            .processes
            .iter()
            .zip(&sir.processes)
            .enumerate()
            .filter(|(_, (slots, unit))| returns_status(unit, slots.status, ProcessStatus::Yield))
            .map(|(index, _)| index)
            .collect();
        let service_processes: HashSet<usize> = model
            .processes
            .iter()
            .enumerate()
            .filter(|(index, process)| {
                service_function.is_some()
                    && (!process.host_requests.is_empty()
                        || yielding.contains(index)
                        || nba_processes.contains(index))
                    && process.host_requests.iter().all(|site| {
                        crate::simulator::native_io::supported(site)
                            && site
                                .args
                                .iter()
                                .all(|arg| !program.layout().unpacked_arrays.contains_key(&arg.id))
                    })
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
            } else if slots.call_top.is_some() {
                // The runtime keeps the frames of a call it suspended inside.
                Some("the process suspends inside function calls")
            } else if slots.program.is_some() {
                Some("a program process runs in the reactive region set")
            } else if !process.host_requests.is_empty() && !service_processes.contains(&index) {
                Some("the process makes host requests")
            } else if service_processes.contains(&index)
                && !written(unit).is_empty()
                && !ordered_available
            {
                Some("a synchronous host-service process drives an event signal")
            } else if yielding.contains(&index) && !service_processes.contains(&index) {
                Some("a loop checkpoint requires native services")
            } else if !process.waits.is_empty() {
                None
            } else if !slots.const_delays && !ordered_available {
                Some("a delay is not a constant of at least one time unit")
            } else if !process.nbas.is_empty() && !nba_processes.contains(&index) {
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
                    kernels.push(Some(Kernel {
                        writes,
                        wait: None,
                        delays: true,
                    }));
                    fallback.push(None);
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

        let opaque_dependencies = rtl_units()
            .chain(
                sir.processes
                    .iter()
                    .enumerate()
                    .filter_map(|(index, unit)| {
                        (index < fallback.len() && fallback[index].is_none()).then_some(unit)
                    }),
            )
            .any(opaque);
        for index in 0..schema.processes.len() {
            let process = &model.processes[index];
            if process.waits.is_empty() || fallback[index].is_some() {
                continue;
            }
            let baseline = slots.baseline[index];
            let slots = &schema.processes[index];
            let unit = &sir.processes[index];
            let wait = (|| -> Result<(Wait, bool), &'static str> {
                let scalar = process.compact_wait_terms(|signal| {
                    program.layout().widths.get(&signal).copied().unwrap_or(0)
                });
                let raw_site = |listed: &[celox_design::schedule::TermModel<AbsoluteAddr>]| {
                    let dimensions = listed
                        .iter()
                        .map(|term| {
                            let width = *program.layout().widths.get(&term.signal)?;
                            Some((
                                width,
                                program.layout().four_state
                                    && program
                                        .layout()
                                        .is_4states
                                        .get(&term.signal)
                                        .copied()
                                        .unwrap_or(true),
                                model
                                    .counted
                                    .iter()
                                    .any(|counted| counted.slots.signal == term.signal),
                                term.gate.is_some_and(|gate| gate.counters.is_some()),
                            ))
                        })
                        .collect::<Option<Vec<_>>>()
                        .ok_or("the process waits for an unsupported signal representation")?;
                    let raw =
                        celox_design::schedule::CompiledWaitLayout::with_gate_counters(&dimensions)
                            .ok_or("the wait snapshot layout overflows")?;
                    if program.layout().widths[&baseline] < raw.bits {
                        tracing::debug!(target: "celox::scheduler", process = index,
                                available_bits = program.layout().widths[&baseline],
                                required_bits = raw.bits, scalar = raw.scalar_byte,
                                "compiled wait snapshot cannot hold the expression list");
                        return Err("the compiled wait snapshot has insufficient storage");
                    }
                    Ok(Wait {
                        terms: listed.iter().map(|term| (term.signal, term.kind)).collect(),
                        site_terms: Vec::new(),
                        counters: listed
                            .iter()
                            .map(|term| {
                                term.gate
                                    .and_then(|gate| gate.counters)
                                    .map(|counters| counters.all)
                                    .or_else(|| {
                                        model
                                            .counted
                                            .iter()
                                            .find(|counted| counted.slots.signal == term.signal)
                                            .map(|counted| counted.slots.counter(term.kind))
                                    })
                            })
                            .collect(),
                        raw: Some(raw),
                        alternatives: Vec::new(),
                        gates: listed.iter().map(|term| term.gate).collect(),
                        reactive: false,
                        sites: Vec::new(),
                    })
                };
                let (terms, site_terms, raw, alternatives, gates) = match scalar {
                    Some((terms, sites)) => (terms, sites, None, Vec::new(), Vec::new()),
                    None => {
                        if let Some(listed) = process.common_wait_terms() {
                            let wait = raw_site(listed)?;
                            (
                                wait.terms,
                                vec![0; process.waits.len()],
                                wait.raw,
                                Vec::new(),
                                wait.gates,
                            )
                        } else {
                            let mut unique = Vec::new();
                            let mut alternatives = Vec::new();
                            let mut site_terms = Vec::new();
                            for site in &process.waits {
                                let index = match unique.iter().position(|known| *known == site) {
                                    Some(index) => index,
                                    None => {
                                        let index = unique.len();
                                        unique.push(site);
                                        alternatives.push(raw_site(site)?);
                                        index
                                    }
                                };
                                site_terms.push(index);
                            }
                            let mut terms = Vec::new();
                            for term in alternatives.iter().flat_map(|wait| &wait.terms) {
                                if !terms.contains(term) {
                                    terms.push(*term);
                                }
                            }
                            (terms, site_terms, None, alternatives, Vec::new())
                        }
                    }
                };
                // Unknown effects can wake a waiter more than once per slot.
                // Use the generated work list and its budget, not the compact
                // single-writer proof, for those activations.
                let mut reactive = opaque_dependencies;
                for &(signal, _) in &terms {
                    let width = program.layout().widths.get(&signal);
                    let signal = storage(signal);
                    let delayed = delay_writers.get(&signal) == Some(&1)
                        && !rtl_stores.contains(&signal)
                        && !other_stores.contains(&signal);
                    if raw.is_none() && alternatives.is_empty() && width != Some(&1) {
                        return Err("the process waits for a non-scalar compact edge");
                    }
                    reactive |= !(delayed || undriven(signal));
                }
                let assertion =
                    returns_status(unit, slots.status, ProcessStatus::AssertionObserved)
                        || returns_status(unit, slots.status, ProcessStatus::AssertionReactive);
                if assertion
                    && (returns_status(unit, slots.status, ProcessStatus::Delay)
                        || returns_status(unit, slots.status, ProcessStatus::InactiveDelay)
                        || kernel_stores[index]
                            .iter()
                            .any(|store| waited.contains(store)))
                {
                    return Err("an assertion action suspends or drives an event expression");
                }
                let delays = !statuses_only(unit, slots.status, &WAIT_STATUSES);
                if delays
                    && ((!slots.const_delays && !ordered_available)
                        || !statuses_only(unit, slots.status, &MIXED_STATUSES))
                {
                    tracing::debug!(target: "celox::scheduler", process = index,
                        status_definitions = ?instructions(unit).filter_map(|inst| match inst {
                            SIRInstruction::Store(target, _, _, value, ..) if *target == stable(slots.status) =>
                                Some((*value, instructions(unit).find(|definition| definition.defined_register() == Some(*value)))),
                            _ => None,
                        }).collect::<Vec<_>>(), "unsupported native wait statuses");
                    return Err("the process suspends other than for an edge or a delay");
                }
                let writes = &kernel_stores[index];
                if !written(unit).is_empty() && !ordered_available {
                    return Err("the process drives an event signal");
                }
                reactive |= writes.iter().any(|store| waited.contains(store));
                if reactive && !process.nbas.is_empty() && !nba_processes.contains(&index) {
                    return Err("a reactive wait stages nonblocking assignments");
                }
                if reactive && opaque_dependencies && service_function.is_none() {
                    return Err("a reactive wait has opaque dependencies");
                }
                if (raw.is_some() || !alternatives.is_empty())
                    && !process.nbas.is_empty()
                    && !nba_processes.contains(&index)
                {
                    return Err("a raw-expression wait stages nonblocking assignments");
                }
                let sites = if nba_processes.contains(&index) {
                    Vec::new()
                } else {
                    process
                        .nbas
                        .iter()
                        .map(|site| {
                            let target = storage(site.target);
                            let pending =
                                site.pending.ok_or("a nonblocking site queues its values")?;
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
                        .collect::<Result<Vec<_>, _>>()?
                };
                if sites.len() > 1 << SITE_BITS {
                    return Err("the process has too many nonblocking sites");
                }
                // A kernel that delays, or waits for edges of several
                // signals, may run twice in a slot, staging a site again
                // before the NBA region publishes it.
                let mut signals: Vec<AbsoluteAddr> =
                    terms.iter().map(|&(signal, _)| storage(signal)).collect();
                signals.sort_unstable();
                signals.dedup();
                if !sites.is_empty() && (delays || signals.len() > 1) {
                    return Err(
                        "the process stages nonblocking assignments and may run twice in a slot",
                    );
                }
                Ok((
                    Wait {
                        counters: terms
                            .iter()
                            .enumerate()
                            .map(|(index, &(signal, kind))| {
                                gates
                                    .get(index)
                                    .copied()
                                    .flatten()
                                    .and_then(|gate| gate.counters)
                                    .map(|counters| counters.all)
                                    .or_else(|| {
                                        model
                                            .counted
                                            .iter()
                                            .find(|counted| counted.slots.signal == signal)
                                            .map(|counted| counted.slots.counter(kind))
                                    })
                            })
                            .collect(),
                        terms,
                        site_terms,
                        raw,
                        alternatives,
                        gates,
                        reactive,
                        sites,
                    },
                    delays,
                ))
            })();
            match wait {
                Ok((wait, delays)) => {
                    kernels[index] = Some(Kernel {
                        writes: written(unit),
                        wait: Some(wait),
                        delays,
                    })
                }
                Err(reason) => fallback[index] = Some(reason.to_string()),
            }
        }
        let mut remaining: HashSet<usize> = HashSet::default();
        // The combined observer cone above already includes every wait and
        // its combinational inputs. If no waiting kernel writes that cone,
        // none can wake another (or itself after settling). Assertion-only
        // observers in large RTL designs commonly satisfy this cheap proof.
        let feedback_stores: HashSet<_> = kernels
            .iter()
            .enumerate()
            .filter(|(_, kernel)| kernel.as_ref().is_some_and(|kernel| kernel.wait.is_some()))
            .flat_map(|(index, _)| kernel_stores[index].iter().copied())
            .filter(|address| {
                // The detailed analysis below likewise ignores private process
                // control in combinational cones. Keep direct waits regardless.
                waited.contains(address)
                    || (!control_slots.contains(address) && active_observers.contains(address))
            })
            .collect();
        let event_writing_waits = kernels.iter().enumerate().any(|(index, kernel)| {
            kernel.as_ref().is_some_and(|kernel| {
                !kernel.writes.is_empty()
                    && (kernel.wait.is_some() || service_processes.contains(&index))
            })
        });
        let possible_wait_feedback =
            opaque_dependencies || event_writing_waits || !feedback_stores.is_empty();
        tracing::debug!(target: "celox::scheduler", possible_wait_feedback,
            stores = ?feedback_stores.iter().take(8).map(|address| program.get_path(address)).collect::<Vec<_>>(),
            "candidate wait feedback stores");
        // Native work lists can safely bound possible feedback without proving
        // a cycle. Other backends must identify and exclude cyclic reactive
        // kernels, so retain the precise per-wait analysis for them. Avoiding
        // those repeated cones matters for large procedural RTL schedules.
        if possible_wait_feedback && service_function.is_none() {
            // Find feedback that needs the native work-list bounds. Include
            // combinational cones: writing an input of a waited
            // expression can wake its consumer after the settle, even when no
            // kernel stores the expression itself.
            // Several process kernels commonly wait on the same clock or signal
            // set. Their dependency cones are identical within this immutable
            // plan; analyze each distinct set once without materializing an
            // executable slice just to inspect its loads.
            let mut wait_dependencies = HashMap::default();
            let dependencies: Vec<(HashSet<AbsoluteAddr>, HashSet<AbsoluteAddr>)> = kernels
                .iter()
                .map(|kernel| {
                    let Some(wait) = kernel.as_ref().and_then(|kernel| kernel.wait.as_ref()) else {
                        return (HashSet::default(), HashSet::default());
                    };
                    // Input ports and clock aliases can give hundreds of names
                    // to the same storage. Slicing already uses these homes;
                    // key its cache by them too.
                    let mut signals: Vec<_> = wait
                        .terms
                        .iter()
                        .map(|&(signal, _)| storage(signal))
                        .collect();
                    signals.sort_unstable();
                    signals.dedup();
                    wait_dependencies
                        .entry(signals)
                        .or_insert_with_key(|signals| {
                            let inputs = wait_slicer
                                .as_ref()
                                .map(|slicer| slicer.input_signals(signals.iter().copied()))
                                .unwrap_or_default();
                            let comb: HashSet<_> = inputs
                                .into_iter()
                                .map(storage)
                                .filter(|input| !control_slots.contains(input))
                                .collect();
                            let all: HashSet<_> = signals
                                .iter()
                                .copied()
                                .map(storage)
                                .chain(comb.iter().copied())
                                .collect();
                            (all, comb)
                        })
                        .clone()
                })
                .collect();
            tracing::debug!(target: "celox::scheduler", kernels = kernels.len(), distinct_wait_sets = wait_dependencies.len(), "scheduler wait dependency slices");
            remaining = kernels
                .iter()
                .enumerate()
                .filter_map(|(index, kernel)| kernel.as_ref()?.wait.as_ref().map(|_| index))
                .collect();
            loop {
                let roots: Vec<usize> = remaining
                    .iter()
                    .copied()
                    .filter(|&consumer| {
                        !remaining.iter().any(|&producer| {
                            // A process is not waiting while its own body runs;
                            // registration captures direct signal writes afterward.
                            // A computed expression can still change at the later
                            // settle, after registration, and wake the same process.
                            let watched = if producer == consumer {
                                &dependencies[consumer].1
                            } else {
                                &dependencies[consumer].0
                            };
                            !kernel_stores[producer].is_disjoint(watched)
                        })
                    })
                    .collect();
                if roots.is_empty() {
                    break;
                }
                for root in roots {
                    remaining.remove(&root);
                }
            }
        } else {
            tracing::debug!(target: "celox::scheduler", possible_wait_feedback, nba_processes = nba_processes.len(), "individual wait dependency slices are unnecessary");
        }
        let inactive: Vec<_> = kernels
            .iter()
            .enumerate()
            .filter_map(|(index, kernel)| {
                (ordered_available
                    && kernel.as_ref().is_some_and(|kernel| kernel.delays)
                    && !schema.processes[index].const_delays)
                    .then_some((index, schema.processes[index].status))
            })
            .collect();
        let cyclic = service_function.is_some()
            && (possible_wait_feedback || !nba_processes.is_empty() || !inactive.is_empty());
        for index in remaining {
            if !cyclic
                && kernels[index]
                    .as_ref()
                    .and_then(|kernel| kernel.wait.as_ref())
                    .is_some_and(|wait| wait.reactive)
            {
                kernels[index] = None;
                fallback[index] = Some("reactive waits have cyclic dependencies".into());
            }
        }
        if kernels.iter().flatten().count() > program.layout().widths[&slots.ready] {
            return Err("the compiled ready mask has insufficient storage".into());
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
                SIRInstruction::Store(_, _, _, _, triggers, sites) => {
                    triggers.iter().any(|trigger| !internal_trigger(trigger)) || !sites.is_empty()
                }
                SIRInstruction::Commit(_, _, _, _, triggers) => {
                    triggers.iter().any(|trigger| !internal_trigger(trigger))
                }
                // Records, captures and external calls, a callee's too. A
                // call's effects do not say which triggers it raises, so any
                // is taken for one the scheduler does not compare itself.
                _ => inst.host_effects() != celox_sir::HostEffects::default(),
            })
        });
        // Dirty tracking compares stable homes by their representative
        // (`shared_storage`), which groups addresses only by an equal start
        // offset: no two homes may overlap from different offsets.
        debug_assert!(
            homes_overlap_only_whole(program.layout()),
            "stable homes overlap at different offsets"
        );
        // A pending settle cannot pass an opaque reader, nor can an opaque
        // writer use a sliced settle selected only from explicit stores.
        // Keep the complete generated settle in that general path.
        let settle_reads: Option<HashSet<AbsoluteAddr>> =
            (!effects && !opaque_dependencies).then(|| {
                sir.eval_comb
                    .iter()
                    .flat_map(instructions)
                    .flat_map(read_storage)
                    .map(|source| storage(source.absolute_addr()))
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
        // Keep sensitivity signals and their occurrence counters eager. A
        // slice rooted only at the final value can discard the counter writes
        // that record an intermediate pulse (even when the value is unchanged).
        let watched_comb: HashSet<_> = kernels
            .iter()
            .flatten()
            .filter_map(|kernel| kernel.wait.as_ref())
            .flat_map(|wait| &wait.terms)
            .map(|&(signal, _)| storage(signal))
            .chain(schema.wait_sites.iter().flat_map(|site| {
                site.gates
                    .iter()
                    .flatten()
                    .flat_map(celox_design::WaitGate::reads)
                    .map(storage)
            }))
            .chain(model.counted.iter().flat_map(|counted| {
                CounterKind::ALL.map(|kind| storage(counted.slots.counter(kind)))
            }))
            .filter(|signal| comb_stores.contains(signal))
            .collect();
        let written_homes: HashSet<_> = comb_stores.iter().copied().map(storage).collect();
        let retained_candidates = sir.eval_comb_stateful.as_ref().map_or_else(
            || written_homes.clone(),
            |stateful| {
                stateful
                    .iter()
                    .map(|target| storage(target.absolute_addr()))
                    .filter(|target| written_homes.contains(target))
                    .collect()
            },
        );
        let retained = retained_storage(&sir.eval_comb, &storage, &retained_candidates);
        let mut eager_candidate_instructions = None;
        let eager = settle_reads
            .as_ref()
            .filter(|_| !sir.eval_comb.iter().any(fails))
            .and_then(|_| {
                let stateful = sir.eval_comb_stateful.as_ref();
                let eager = stateful.and_then(|stateful| {
                    if stateful.is_empty() && watched_comb.is_empty() {
                        return Some(Vec::new());
                    }
                    let slicer = &*comb_slicer;
                    let targets = stateful
                        .iter()
                        .map(|target| target.absolute_addr())
                        .filter(|&target| retained.contains(&storage(target)))
                        .chain(watched_comb.iter().copied());
                    // Stateful metadata predates SIR optimization. A target
                    // with no remaining writer (including its shared-storage
                    // aliases) cannot retain a new comb value between settles.
                    // Keep every surviving writer; obsolete targets need no
                    // eager work and must not disable the whole optimization.
                    let eager = slicer.eager_slice(targets.filter(|&target| slicer.stores(target)));
                    // Large eager cones can repeat more work than they save.
                    // Keep at least half of the comb work deferred.
                    let size = |units: &[Unit]| units.iter().flat_map(instructions).count();
                    eager_candidate_instructions = Some(size(&eager));
                    (size(&eager) * 2 < size(&sir.eval_comb)).then_some(eager)
                });
                eager.or_else(|| {
                    (watched_comb.is_empty()
                        && !retains_state(&sir.eval_comb, &storage, &comb_stores))
                    .then(Vec::new)
                })
            });
        let lazy = eager.map(|eager| {
            // Pending settles may leave unrelated outputs unpublished. Every
            // combinational signal a waiter reads belongs to the eager cone.
            // Kernels that read other pending outputs request a full settle.
            let eager_stores: HashSet<_> = eager.iter().flat_map(stores).map(storage).collect();
            debug_assert!(
                watched_comb
                    .iter()
                    .all(|target| eager_stores.contains(target))
            );
            let reads = |unit: &Unit| {
                loads(unit).any(|source| {
                    let source = storage(source);
                    comb_stores.contains(&source) && !eager_stores.contains(&source)
                })
            };
            let ends = |unit: &Unit| fails(unit) || emits_finish(unit, &finish_sites);
            let observes = |unit: &Unit| reads(unit) || ends(unit);
            let mut longest = 0;
            for (index, kernel) in kernels.iter().enumerate() {
                if kernel.as_ref().is_some_and(|kernel| kernel.delays) {
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
                    // An asynchronous reset has few edges: a fused copy of
                    // the settle for them would only lengthen the build.
                    if !event.clock {
                        return DomainSettle::Settled;
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
                                        ProcessStatus::Pending,
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
        let incremental = if lazy.is_none()
            && settle_reads.is_some()
            && !sir.eval_comb.iter().any(fails)
            && sir
                .eval_comb
                .iter()
                .flat_map(instructions)
                .flat_map(read_storage)
                .all(|read| read.region == STABLE_REGION)
            && sir.eval_comb.iter().all(|unit| {
                celox_sir::cfg::SirCfg::analyze(unit).is_ok_and(|cfg| cfg.loops.is_empty())
            }) {
            kernels
                .iter()
                .enumerate()
                .filter_map(|(index, kernel)| kernel.as_ref().map(|kernel| (index, kernel)))
                .find(|(_, kernel)| kernel.delays && kernel.writes.len() == 1)
                .and_then(|(index, kernel)| {
                    let source = storage(events[kernel.writes[0]].signal);
                    if !settle_reads.as_ref()?.contains(&source) {
                        return None;
                    }
                    let sources: HashSet<_> = instructions(&sir.processes[index])
                        .flat_map(written_storage)
                        .filter(|write| write.region == STABLE_REGION)
                        .map(|write| storage(write.absolute_addr()))
                        .filter(|address| settle_reads.as_ref().unwrap().contains(address))
                        .collect();
                    // Read-before-write activations retain their existing
                    // per-settle semantics, including their downstream effects.
                    let stateful: Vec<_> = sir.eval_comb_stateful.as_ref().map_or_else(
                        || retained.iter().copied().collect(),
                        |targets| {
                            targets
                                .iter()
                                .map(|target| storage(target.absolute_addr()))
                                .filter(|target| retained.contains(target))
                                .collect()
                        },
                    );
                    let slicer = &*comb_slicer;
                    let inputs = sources
                        .iter()
                        .copied()
                        .chain(schema.time_slots.iter().copied().map(&storage))
                        .chain(stateful);
                    let units = slicer.affected_slice(inputs);
                    let size = |units: &[Unit]| units.iter().flat_map(instructions).count();
                    (size(&units) * 4 < size(&sir.eval_comb) * 3).then_some(Incremental {
                        source,
                        sources,
                        units,
                    })
                })
        } else {
            None
        };
        let mut counted_events = Vec::new();
        let counter_base = program.layout().widths[&slots.unknown_levels];
        for (index, counted) in model.counted.iter().enumerate() {
            for (kind_index, kind) in CounterKind::ALL.into_iter().enumerate() {
                let domains: Vec<_> = counted
                    .domains
                    .iter()
                    .filter(|(_, candidate)| *candidate == kind)
                    .filter_map(|(signal, _)| {
                        events.iter().position(|event| event.signal == *signal)
                    })
                    .collect();
                if domains.is_empty() {
                    continue;
                }
                let baseline_offset = counter_base + (index * 3 + kind_index) * 32;
                if baseline_offset + 32 > program.layout().widths[&slots.levels] {
                    return Err(
                        "the compiled domain counter snapshot has insufficient storage".into(),
                    );
                }
                counted_events.push(CountedEvent {
                    counter: counted.slots.counter(kind),
                    baseline_offset,
                    events: domains,
                });
            }
        }
        let host_edges = if ordered_available {
            (0..events.len())
                .filter(|event| !rtl_written.contains(event))
                .collect()
        } else {
            Vec::new()
        };
        let mut plan = Self {
            host_edges,
            inactive,
            observers: native_observers
                .then(|| observers::Observers::new(program))
                .filter(|observers| !observers.groups.is_empty()),
            counted: counted_events,
            events,
            kernels,
            clocks,
            fallback,
            finish_sites,
            service_function,
            ordered: None,
            service_processes,
            nba_processes,
            cyclic,
            settle_reads,
            lazy,
            incremental,
            derived: Vec::new(),
            active: active_commits.iter().copied().collect(),
            gating: Vec::new(),
            event_bits,
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
        let mut derived: BTreeSet<_> = plan
            .counted
            .iter()
            .flat_map(|counted| counted.events.iter().copied())
            .filter(|event| !edged.contains(event))
            .collect();
        let mut pending: Vec<_> = edged
            .iter()
            .copied()
            .chain(derived.iter().copied())
            .collect();
        while let Some(event) = pending.pop() {
            for next in derived_from(event) {
                if !edged.contains(&next) && derived.insert(next) {
                    pending.push(next);
                }
            }
        }
        // Simple Active waves retain one pending result per domain. Select an
        // ordered queue for cases that can generate a second edge:
        // mixed Active/NBA gate inputs, unknown-to-known edge sequences,
        // direct procedural clock writes, and Active-clock feedback need an
        // ordered queue rather than a domain mask.
        let mut needs_ordered = event_writing_waits || !plan.inactive.is_empty();
        if !plan.active.is_empty() {
            let mut active_stores: HashMap<AbsoluteAddr, Vec<usize>> = HashMap::default();
            for (index, event) in plan.events.iter().enumerate() {
                if !model.active_commits.contains(&event.signal) {
                    continue;
                }
                for unit in event.eval_only.iter().flatten() {
                    for target in instructions(unit).flat_map(written_storage) {
                        if target.region == STABLE_REGION {
                            active_stores
                                .entry(storage(target.absolute_addr()))
                                .or_default()
                                .push(index);
                        }
                    }
                }
            }
            if active_stores
                .keys()
                .any(|address| event_storage.contains_key(address))
            {
                if !ordered_available {
                    return Err("an Active-region update directly drives an event".into());
                }
                needs_ordered = true;
            }
            let nba_stores: HashSet<_> =
                sir.apply_ffs.values().flatten().flat_map(stores).collect();
            let primary = |index: &usize| edged.contains(index) && !rtl_written.contains(index);
            for (source, clocks) in &topology.combinational_derived {
                for clock in clocks {
                    let Some(gate) = &clock.gate else {
                        if clock.inputs.as_ref().is_some_and(|inputs| {
                            inputs
                                .iter()
                                .all(|input| !active_stores.contains_key(&storage(*input)))
                        }) {
                            continue;
                        }
                        tracing::debug!(target: "celox::scheduler",
                            source = ?program.get_path(source),
                            clock = ?program.get_path(&clock.clock),
                            active_stores = ?active_stores.keys().map(|address| program.get_path(address)).collect::<Vec<_>>(),
                            "Active clock cone requires ordered evaluation");
                        if !ordered_available {
                            return Err(
                                "an Active-region clock has an unknown combinational cone".into()
                            );
                        }
                        needs_ordered = true;
                        continue;
                    };
                    let inputs: Vec<_> = gate
                        .iter()
                        .filter_map(|op| match op {
                            celox_design::GateOp::Load { address, .. } => Some(storage(*address)),
                            _ => None,
                        })
                        .collect();
                    if !inputs.iter().any(|input| active_stores.contains_key(input)) {
                        continue;
                    }
                    let source = storage(*source);
                    let source_event = index_of.get(&topology.canonical(source));
                    if !source_event.is_some_and(primary)
                        || inputs.iter().any(|input| {
                            nba_stores.contains(input)
                                || rtl_stores.contains(input) && !active_stores.contains_key(input)
                                || active_stores
                                    .get(input)
                                    .is_some_and(|writers| !writers.iter().all(primary))
                        })
                    {
                        if !ordered_available {
                            return Err("an Active-region clock requires ordered updates".into());
                        }
                        needs_ordered = true;
                    }
                    if program.layout().four_state
                        && std::iter::once(&source).chain(&inputs).any(|input| {
                            program
                                .layout()
                                .is_4states
                                .get(input)
                                .copied()
                                .unwrap_or(true)
                        })
                    {
                        if !ordered_available {
                            return Err(
                                "an Active-region clock may have repeated four-state edges".into(),
                            );
                        }
                        needs_ordered = true;
                    }
                    if plan.kernels.iter().enumerate().any(|(index, kernel)| {
                        kernel.as_ref().is_some_and(|kernel| {
                            source_event.is_some_and(|source| kernel.writes.contains(source))
                        }) && instructions(&sir.processes[index])
                            .map(|inst| {
                                let writes = written_storage(inst)
                                    .any(|target| storage(target.absolute_addr()) == source);
                                // A call may write it any number of times.
                                match writes {
                                    false => 0,
                                    true if inst.call_effects().is_some() => 2,
                                    true => 1,
                                }
                            })
                            .sum::<usize>()
                            > 1
                    }) {
                        if !ordered_available {
                            return Err(
                                "an Active-region clock has several procedural writes".into()
                            );
                        }
                        needs_ordered = true;
                    }
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
        // Reference processes stop a compiled run before their next wakeup,
        // and ready processes, waiters and queued NBAs prevent entry. Their
        // stores are therefore stable inputs here, just like host writes.
        // Only stores that can execute inside this unit invalidate stability.
        let compiled_stores: HashSet<AbsoluteAddr> = kernel_stores
            .iter()
            .zip(&plan.kernels)
            .filter(|(_, kernel)| kernel.is_some())
            .flat_map(|(stores, _)| stores.iter().copied())
            .chain(
                model
                    .processes
                    .iter()
                    .flat_map(|process| process.nbas.iter().map(|site| storage(site.target))),
            )
            .collect();
        let stable_input = |address| {
            let address = storage(address);
            !rtl_stores.contains(&address)
                && !compiled_stores.contains(&address)
                && !clocked_storage.contains(&address)
                && !schema
                    .time_slots
                    .iter()
                    .any(|slot| storage(*slot) == address)
        };
        let mut stable_host_events: BTreeSet<usize> = topology
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
        // Partition flags cache which combinational work is pending; they
        // do not determine its settled values. Following their shared home
        // would pull every partition into each clock/reset's dependency cone.
        let partition_flags = schema.settle_partitions.as_ref().map(|s| storage(s.flags));
        // Constant clocks/resets and pure combinational cones of stable host
        // inputs need no domain waves after the entry settle. They need not
        // appear in the frontend's clock-gate topology (a constant has no
        // source clock at all). Prove stability from the actual SIR cone.
        if !effects {
            for &index in rtl_written.difference(&derived) {
                let signal = plan.events[index].signal;
                if stable_host_events.contains(&index)
                    || scheduled_stores.contains(&storage(signal))
                {
                    continue;
                }
                if comb_slicer
                    .stateless_cone(signal, partition_flags)
                    .is_some_and(|(writes, inputs)| {
                        tracing::debug!(target: "celox::scheduler", signal = ?program.get_path(&signal), scheduled = ?writes.intersection(&scheduled_stores).map(|a| program.get_path(a)).take(8).collect::<Vec<_>>(), unstable = ?inputs.iter().filter(|a| Some(**a) != partition_flags && !stable_input(**a)).map(|a| program.get_path(a)).take(8).collect::<Vec<_>>(), "event cone external dependencies");
                        writes.is_disjoint(&scheduled_stores)
                            && inputs
                                .into_iter()
                                .all(|input| Some(input) == partition_flags || stable_input(input))
                    })
                {
                    stable_host_events.insert(index);
                }
            }
        }
        // A clock need not have an upstream event domain: `clk & enable`
        // still clocks RTL when nothing waits on `clk` itself. Such clocks
        // (including latch-based gates) are observed after the same settles
        // as in the reference loop. Topology only supplies their downstream
        // FF domains; it must not be an admission requirement for the root.
        if !effects {
            let candidates: Vec<_> = rtl_written
                .difference(&derived)
                .filter(|index| !edged.contains(index) && !stable_host_events.contains(index))
                .copied()
                .filter(|&index| {
                    let signal = storage(plan.events[index].signal);
                    comb_stores.contains(&signal) && !scheduled_stores.contains(&signal)
                })
                .collect();
            if !candidates.is_empty() {
                // A blocking FF write may move this clock repeatedly before
                // NBA. Use the ordered path when its cone reads such a write.
                let active_stores: HashSet<_> = model
                    .active_commits
                    .iter()
                    .filter_map(|event| sir.eval_only_ffs.get(event))
                    .flatten()
                    .flat_map(instructions)
                    .flat_map(written_storage)
                    .filter(|target| target.region == STABLE_REGION)
                    .map(|target| storage(target.absolute_addr()))
                    .collect();
                let mut pending = Vec::new();
                for index in candidates {
                    let active = !active_stores.is_empty()
                        && comb_slicer
                            .input_signals([plan.events[index].signal])
                            .iter()
                            .any(|input| active_stores.contains(&storage(*input)));
                    if active && !ordered_available {
                        continue;
                    }
                    needs_ordered |= active;
                    pending.push(index);
                }
                while let Some(event) = pending.pop() {
                    if !edged.contains(&event) && derived.insert(event) {
                        pending.extend(derived_from(event));
                    }
                }
            }
        }
        let nba_written: BTreeSet<_> = plan
            .nba_processes
            .iter()
            .flat_map(|&process| &model.processes[process].nbas)
            .filter_map(|site| event_storage.get(&storage(site.target)))
            .flatten()
            .copied()
            .collect();
        if !nba_written.is_empty() {
            derived.extend(nba_written.iter().filter(|event| !edged.contains(event)));
            needs_ordered = true;
        }
        let unhandled_rtl_events: Vec<_> = rtl_written
            .difference(&derived)
            .filter(|event| ordered_available || !stable_host_events.contains(event))
            .copied()
            .collect();
        if ordered_available && !unhandled_rtl_events.is_empty() {
            // Dependency proofs select the smaller schedule. Their failure
            // selects generated ordered waves over every affected domain.
            // Kernel-written events are already in `edged` and must appear
            // only once in the level snapshot.
            derived.extend(
                unhandled_rtl_events
                    .iter()
                    .filter(|event| !edged.contains(event)),
            );
            needs_ordered = true;
        }
        if needs_ordered {
            // The topology-based observer pruning above predates orphan
            // clocks. Ordered waves must preserve every real Active commit,
            // including writers in a clock cone without a source domain.
            plan.active = plan
                .events
                .iter()
                .enumerate()
                .filter(|(_, event)| event.active_commit)
                .map(|(index, _)| index)
                .collect();
            plan.ordered = Some(ordered::Ordered::new(
                program,
                plan.events.len(),
                plan.service_function.unwrap(),
            ));
        }
        tracing::debug!(target: "celox::scheduler",
                incremental_instructions = plan.incremental.as_ref().map(|cone| cone.units.iter().flat_map(instructions).count()),
                incremental_source = ?plan.incremental.as_ref().map(|cone| program.get_path(&cone.source)),
                deferred_settles = plan.lazy.is_some(),
                comb_effects = effects,
                comb_fallible = sir.eval_comb.iter().any(fails),
                stateful_targets = ?sir.eval_comb_stateful.as_ref().map(Vec::len),
                watched_targets = watched_comb.len(),
                retained_targets = retained.len(),
                ?eager_candidate_instructions,
                comb_instructions = sir.eval_comb.iter().flat_map(instructions).count(),
                eager_instructions = plan.lazy.as_ref().map(|lazy| lazy.eager.iter().flat_map(instructions).count()),
                active_commits = ?active_commits.iter().map(|&event| program.get_path(&plan.events[event].signal)).collect::<Vec<_>>(),
                kernels = ?plan.fallback.iter().enumerate().map(|(index, reason)| (index, schema.process_origins.get(index).map(ToString::to_string), reason, written(&sir.processes[index]).iter().map(|&event| program.get_path(&plan.events[event].signal)).collect::<Vec<_>>())).collect::<Vec<_>>(),
                "compiled scheduler process plans");
        if !ordered_available && !unhandled_rtl_events.is_empty() {
            tracing::debug!(target: "celox::scheduler",
                signals = ?unhandled_rtl_events.iter().map(|&event| program.get_path(&plan.events[event].signal)).collect::<Vec<_>>(),
                edged = ?edged.iter().map(|&event| program.get_path(&plan.events[event].signal)).collect::<Vec<_>>(),
                "compiled scheduler rejected RTL-driven events");

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
        if (!plan.active.is_empty() || plan.ordered.is_some())
            && (program.layout().widths[&slots.exit_arg] < event_bits * 2
                || tracked.iter().any(|&event| {
                    plan.events[event].eval_only.is_none() || plan.events[event].apply.is_none()
                }))
        {
            return Err(ACTIVE_COMMIT.into());
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
        edged.extend(&self.host_edges);
        edged.sort_unstable();
        edged.dedup();
        edged
    }

    fn generate(&self, program: &LaidOutProgram, slots: &SchedulerSlots<AbsoluteAddr>) -> Unit {
        self.generate_with_outlines(program, slots, None, 0)
    }

    fn generate_with_outlines(
        &self,
        program: &LaidOutProgram,
        slots: &SchedulerSlots<AbsoluteAddr>,
        outlined: Option<&mut Vec<Unit>>,
        outline_base: u32,
    ) -> Unit {
        let schema = &program.runtime.runtime_schema;
        let edged = self.edged();
        let tracked = self.tracked();
        let clock_bits = self.clocks.len().next_multiple_of(64).max(64);
        let mut b = Builder::new();
        let comb = b.new_block();
        let native_nbas: HashSet<_> = self
            .nba_processes
            .iter()
            .copied()
            .filter(|&index| self.kernels[index].is_some())
            .collect();
        let nbas = cfg!(all(target_arch = "x86_64", not(feature = "arm64-codegen")))
            .then_some(self.service_function)
            .flatten()
            .filter(|_| !schema.nba_sites.is_empty())
            .map(|service| nba::Sites::new(program, &native_nbas, service, &self.events));
        let mut g = Generator {
            program,
            nbas: nbas.as_ref(),
            observers: self.observers.as_ref(),
            observer_service: self.service_function,
            outlined,
            outline_base,
            b,
            slots,
            finish_sites: &self.finish_sites,
            exits: Vec::new(),
            settle_reads: self.settle_reads.as_ref(),
            incremental: self.incremental.as_ref(),
            partitioned_glue: program
                .runtime
                .runtime_schema
                .settle_partitions
                .as_ref()
                .map(|_| (program, HashSet::default())),
            shared: program.shared_storage(),
            comb,
            continuations: Vec::new(),
            eager: None,
            eager_continuations: Vec::new(),
            four_state: program
                .layout()
                .four_state
                .then_some(&program.layout().is_4states),
            event_bits: self.event_bits,
            ready_bits: self
                .kernels
                .iter()
                .flatten()
                .count()
                .next_multiple_of(64)
                .max(64),
            cont_bits: program.layout().widths[&slots.cont],
            event_batch: {
                let capacity = program.layout().runtime_event_capacity;
                (capacity > 0
                    && (!schema.runtime_event_sites.is_empty()
                        || !self.service_processes.is_empty()))
                .then_some((capacity / 8).max(1) as u64)
            },
        };
        let mut guards = if cfg!(all(target_arch = "x86_64", not(feature = "arm64-codegen")))
            && (self.cyclic || !self.service_processes.is_empty())
        {
            self.service_function
                .map(|service| ordered::Guards::new(&mut g, service))
        } else {
            None
        };
        let waits: Vec<(usize, &Wait)> = self
            .kernels
            .iter()
            .enumerate()
            .filter_map(|(index, kernel)| Some((index, kernel.as_ref()?.wait.as_ref()?)))
            .collect();
        // The kernels that delay, with the status slot of those that also
        // wait, which tells whether they wait for their wake time.
        let delays: Vec<(usize, Option<AbsoluteAddr>)> = self
            .kernels
            .iter()
            .enumerate()
            .filter_map(|(index, kernel)| {
                let kernel = kernel.as_ref().filter(|kernel| kernel.delays)?;
                let status = kernel
                    .wait
                    .is_some()
                    .then_some(schema.processes[index].status);
                Some((index, status))
            })
            .collect();
        // The bit of each kernel in the ready mask: its place among the
        // kernels the scheduler runs, in kernel order.
        let ordinals = ordinals(&self.kernels);
        let checks = waits_of(&self.kernels, &ordinals, schema);
        let status_kernels = |status| {
            self.kernels
                .iter()
                .enumerate()
                .filter(|(index, kernel)| {
                    kernel.is_some()
                        && returns_status(
                            &program.sir.processes[*index],
                            schema.processes[*index].status,
                            status,
                        )
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>()
        };
        let observed = status_kernels(ProcessStatus::AssertionObserved);
        let reactive = status_kernels(ProcessStatus::AssertionReactive);
        let deferred = status_kernels(ProcessStatus::DeferActive);
        let assertions = !observed.is_empty() || !reactive.is_empty();
        let observed_entry = assertions.then(|| g.b.new_block());
        let reactive_entry = assertions.then(|| g.b.new_block());
        let reactive_end = assertions.then(|| g.b.new_block());
        let slot_done = assertions.then(|| g.b.new_block());
        // The caller supplies the dirty flag. Timed runs must start dirty:
        // checkpoint restores and partial-slot exits need a fresh settle.
        // A host clock batch may instead enter with already settled inputs.
        let host_inputs = cfg!(all(target_arch = "x86_64", not(feature = "arm64-codegen")))
            && self.service_function.is_some();
        let header = g.b.new_block();
        g.b.seal_block(SIRTerminator::Jump(header, Vec::new()));

        // The next slot: the earliest wake time of a kernel waiting for one.
        g.b.switch_to_block(header);
        let mut time = g.constant(u64::MAX, SCHEDULER_TIME_WIDTH);
        let ready = g.load(slots.ready, g.ready_bits);
        let zero = g.constant(0, g.ready_bits);
        let mut any = g.binary(1, ready, BinaryOp::Ne, zero);
        let now = g.load(slots.now, SCHEDULER_TIME_WIDTH);
        let start = g.b.alloc_bit(SCHEDULER_TIME_WIDTH, false);
        g.b.emit(SIRInstruction::Mux(start, any, now, time));
        time = start;
        if host_inputs {
            let host = host_inputs::pending(&mut g);
            let now = g.load(slots.now, SCHEDULER_TIME_WIDTH);
            let next = g.b.alloc_bit(SCHEDULER_TIME_WIDTH, false);
            g.b.emit(SIRInstruction::Mux(next, host, now, time));
            time = next;
            any = g.binary(1, any, BinaryOp::Or, host);
        }
        if host_inputs {
            let entry = g.arena_header_load(std::mem::offset_of!(
                crate::simulator::native_io::ArenaHeader,
                region_entry
            ));
            let entry = g.equals(entry, 1);
            let next = g.b.alloc_bit(SCHEDULER_TIME_WIDTH, false);
            g.b.emit(SIRInstruction::Mux(next, entry, now, time));
            time = next;
            any = g.binary(1, any, BinaryOp::Or, entry);
        }
        for &(index, status) in &delays {
            let wake = g.load(slots.wake[index], SCHEDULER_TIME_WIDTH);
            let waiting = g.waiting_for_time(index, status);
            let earlier = g.binary(1, wake, BinaryOp::LtU, time);
            let earlier = g.binary(1, earlier, BinaryOp::And, waiting);
            let next = g.b.alloc_bit(SCHEDULER_TIME_WIDTH, false);
            g.b.emit(SIRInstruction::Mux(next, earlier, wake, time));
            time = next;
            any = g.binary(1, any, BinaryOp::Or, waiting);
        }
        if let Some(nbas) = &nbas {
            (time, any) = nbas.next_time(&mut g, time, any);
        }
        // Or the earliest next edge of a host clock, when the run has any.
        // The clocks armed at the start of the slot, which the re-arm at its
        // end compares with.
        let mut started = None;
        if !self.clocks.is_empty() {
            let armed = g.load(slots.clock_armed, clock_bits);
            let zero = g.constant(0, clock_bits);
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

        let internal_exit = g.exit_block(SchedulerExit::Internal);
        // The kernels due now are ready.
        g.b.switch_to_block(run);
        if !self.service_processes.is_empty() {
            if guards.is_some() {
                ordered::Guards::process_phase(&mut g);
            } else {
                g.service_call(
                    self.service_function.expect("native host services"),
                    u64::MAX,
                    crate::simulator::native_io::PROCESS_PHASE,
                    time,
                    internal_exit,
                );
            }
        }
        let mut ready = g.load(slots.ready, g.ready_bits);
        for &(index, status) in &delays {
            let wake = g.load(slots.wake[index], SCHEDULER_TIME_WIDTH);
            let waiting = g.waiting_for_time(index, status);
            let due = g.binary(1, wake, BinaryOp::Eq, time);
            let mut due = g.binary(1, due, BinaryOp::And, waiting);
            if host_inputs && self.inactive.iter().any(|&(process, _)| process == index) {
                // Imported #0 work belongs to Inactive. A computed positive
                // delay instead resumes in Active at its future timestamp.
                let process = &schema.processes[index];
                let status = g.load(process.status, celox_design::PROCESS_STATUS_WIDTH);
                let inactive = g.equals(status, u64::from(ProcessStatus::InactiveDelay.code()));
                let amount = g.load(process.delay, celox_design::PROCESS_DELAY_WIDTH);
                let zero = g.equals(amount, 0);
                let inactive = g.binary(1, inactive, BinaryOp::And, zero);
                let active = g.equals(inactive, 0);
                due = g.binary(1, due, BinaryOp::And, active);
            }
            let bit = g.bit(due, ordinals[index]);
            ready = g.binary(g.ready_bits, ready, BinaryOp::Or, bit);
        }
        g.store(slots.ready, g.ready_bits, ready);
        if host_inputs {
            let zero = g.constant(0, 64);
            g.arena_header_store(
                std::mem::offset_of!(crate::simulator::native_io::ArenaHeader, region_entry),
                zero,
            );
        }
        // A pending settle runs, as at the end of the last slot, before a
        // kernel that observes it may run: one due now, any waiting one,
        // which may wake in any pass, or any one in a slot after which a
        // delay may overflow.
        if let Some(lazy) = &self.lazy {
            let waits = waits.iter().any(|&(index, _)| lazy.kernels[index]);
            let observers = delays
                .iter()
                .map(|&(index, _)| index)
                .filter(|&index| lazy.kernels[index])
                .fold(BigUint::default(), |mask, index| {
                    mask | (BigUint::from(1u8) << ordinals[index])
                });
            let late = lazy.latest < u64::MAX;
            if waits || observers != BigUint::default() || late {
                let mut observed = g.constant(u64::from(waits), 1);
                if observers != BigUint::default() {
                    let mask = g.wide_constant(observers, g.ready_bits);
                    let due = g.binary(g.ready_bits, ready, BinaryOp::And, mask);
                    let zero = g.constant(0, g.ready_bits);
                    let due = g.binary(1, due, BinaryOp::Ne, zero);
                    observed = g.binary(1, observed, BinaryOp::Or, due);
                }
                if late {
                    let latest = g.constant(lazy.latest, SCHEDULER_TIME_WIDTH);
                    let after = g.binary(1, time, BinaryOp::GtU, latest);
                    observed = g.binary(1, observed, BinaryOp::Or, after);
                }
                if host_inputs {
                    // Raw host pulses advance waiting baselines before the
                    // first settle. The entry path below performs that settle.
                    let host = host_inputs::pending(&mut g);
                    let ordinary = g.equals(host, 0);
                    observed = g.binary(1, observed, BinaryOp::And, ordinary);
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
        if let Some(nbas) = &nbas {
            nbas.promote(&mut g, time);
        }
        let fresh = g.constant(0, SCHEDULER_FLAG_WIDTH);
        g.store_at(
            slots.defer_limit_settle,
            celox_design::SCHEDULER_ACTIVE_RESUME_BIT,
            SCHEDULER_FLAG_WIDTH,
            fresh,
        );
        if assertions {
            g.store_at(
                slots.defer_limit_settle,
                celox_design::SCHEDULER_ASSERTION_PHASE_BIT,
                SCHEDULER_FLAG_WIDTH,
                fresh,
            );
        }
        if !self.gating.is_empty() {
            for bit in [GATES_JUDGED_BIT, GATE_SOURCE_EDGED_BIT] {
                g.store_at(slots.defer_limit_settle, bit, SCHEDULER_FLAG_WIDTH, fresh);
            }
        }
        let count = g.load(slots.count, SCHEDULER_TIME_WIDTH);
        let one = g.constant(1, SCHEDULER_TIME_WIDTH);
        let count = g.binary(SCHEDULER_TIME_WIDTH, count, BinaryOp::Add, one);
        g.store(slots.count, SCHEDULER_TIME_WIDTH, count);
        for &slot in &schema.time_slots {
            g.store_state(slot, 0, TIME_SLOT_WIDTH, time);
        }
        if schema.time_slots.iter().any(|&slot| g.settle_reads(slot)) {
            if self.incremental.is_some() {
                // The specialized cone includes the time inputs as well as
                // the clock. Preserve any pending invalidation of other data.
                let dirty = g.load(slots.dirty, SCHEDULER_FLAG_WIDTH);
                let time_changed = g.constant(2, SCHEDULER_FLAG_WIDTH);
                let dirty = g.binary(SCHEDULER_FLAG_WIDTH, dirty, BinaryOp::Or, time_changed);
                g.store(slots.dirty, SCHEDULER_FLAG_WIDTH, dirty);
            } else {
                g.mark_dirty();
            }
        }
        // The levels the slot starts from, the baseline of its edges, for an
        // exit in the middle of the slot; those of the clocks' events below,
        // when the run has clocks, unless the slot's waves compare against
        // them: a host clock no clock drives keeps the level the host set.
        let host_entry = host_inputs.then(|| host_inputs::pending(&mut g));
        let host_levels = host_entry.map(|_| g.load_levels());
        let mut levels = g.no_levels();
        for (bit, &event) in tracked.iter().enumerate() {
            if self.clocks.contains(&event) && self.derived.is_empty() {
                continue;
            }
            let level = g.level(&self.events[event]);
            levels = g.with_level(levels, level, bit);
        }
        if let (Some(host), Some(prior)) = (host_entry, host_levels) {
            let value = g.b.alloc_bit(self.event_bits, false);
            g.b.emit(SIRInstruction::Mux(
                value,
                host,
                prior.values,
                levels.values,
            ));
            levels.values = value;
            if let (Some(old), Some(current)) = (prior.unknown, levels.unknown) {
                let unknown = g.b.alloc_bit(self.event_bits, false);
                g.b.emit(SIRInstruction::Mux(unknown, host, old, current));
                levels.unknown = Some(unknown);
            }
        }
        g.store_levels(levels);
        if let Some(host) = host_entry {
            g.if_bit(host, 0, |g| host_inputs::replay(program, g, &checks));
        }
        // The host clocks due now set their levels, in event order, before
        // any kernel runs; their edges start the slot's edges, which
        // collect in `exit_arg`. A clock whose edge applied waits for its
        // next edge until the slot ends. A run without clocks skips them.
        let zero = g.constant(0, self.event_bits);
        g.store(slots.exit_arg, self.event_bits, zero);
        if !self.gating.is_empty() {
            let clear = g.constant(0, SCHEDULER_FLAG_WIDTH);
            g.store_at(
                slots.defer_limit_settle,
                GATE_SOURCE_EDGED_BIT,
                SCHEDULER_FLAG_WIDTH,
                clear,
            );
        }
        let finish_exit = g.exit_block(SchedulerExit::Finish);
        let pick = g.b.new_block();
        let entry_done = pick;
        if let Some((armed, clocked)) = started {
            let apply = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: clocked,
                true_block: (apply, Vec::new()),
                false_block: (entry_done, Vec::new()),
            });
            g.b.switch_to_block(apply);
            let values = g.load(slots.clock_value, clock_bits);
            let mut fired = g.constant(0, clock_bits);
            let mut edges = g.constant(0, self.event_bits);
            let mut levels = g.load_levels();
            // Distinct polarity domains can share physical clock storage.
            // Capture every baseline before the batch writes any alias.
            let shared_clocks = self
                .clocks
                .iter()
                .map(|&index| {
                    let signal = self.events[index].signal;
                    g.shared.get(&signal).copied().unwrap_or(signal)
                })
                .collect::<HashSet<_>>()
                .len()
                != self.clocks.len();
            let before: Vec<_> = if shared_clocks {
                self.clocks
                    .iter()
                    .map(|&index| g.level(&self.events[index]))
                    .collect()
            } else {
                Vec::new()
            };
            for (bit, &index) in self.clocks.iter().enumerate() {
                let event = &self.events[index];
                let next_edge = g.clock_next(bit);
                let on = g.mask_bit(armed, bit);
                let due = g.binary(1, next_edge, BinaryOp::Eq, time);
                let due = g.binary(1, due, BinaryOp::And, on);
                let value = g.mask_bit(values, bit);
                let actual = g.level(event);
                let position = tracked
                    .iter()
                    .position(|&tracked| tracked == index)
                    .expect("clocks are tracked");
                let baseline = if shared_clocks { before[bit] } else { actual };
                let was = if let (Some(host), Some(prior)) = (host_entry, host_levels) {
                    let old = g.saved_level(prior, position);
                    let was =
                        g.b.alloc_bit(if g.four_state.is_some() { 2 } else { 1 }, false);
                    g.b.emit(SIRInstruction::Mux(was, host, old, baseline));
                    was
                } else {
                    baseline
                };
                levels = g.with_level(levels, was, position);
                let is = if g.four_state.is_none() {
                    let is = g.b.alloc_bit(1, false);
                    g.b.emit(SIRInstruction::Mux(is, due, value, actual));
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
                    g.b.emit(SIRInstruction::Mux(is, due, known, actual));
                    is
                };
                // Counters sample iff gates at this write, before the next
                // scheduled clock changes a gate (IEEE 1800-2023 9.4.2.3).
                g.count_host_clock(schema, event.signal, actual, is, due);
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
                if !shared_clocks {
                    let edge = g.edge(event, was, is);
                    let edge = g.event_bit(edge, index);
                    edges = g.binary(self.event_bits, edges, BinaryOp::Or, edge);
                }
                let bit = g.bit_of(due, bit, clock_bits);
                fired = g.binary(clock_bits, fired, BinaryOp::Or, bit);
            }
            // Judge all domains against the completed scheduled batch;
            // otherwise a preceding alias write consumes a later domain's edge.
            if shared_clocks {
                for &index in &self.clocks {
                    let was = g.saved_level(levels, position(&tracked, index));
                    let is = g.level(&self.events[index]);
                    let edge = g.edge(&self.events[index], was, is);
                    let edge = g.event_bit(edge, index);
                    edges = g.binary(self.event_bits, edges, BinaryOp::Or, edge);
                }
            }
            g.store_levels(levels);
            g.store(slots.exit_arg, self.event_bits, edges);
            let all = g.ones(clock_bits);
            let unfired = g.binary(clock_bits, fired, BinaryOp::Xor, all);
            let armed = g.binary(clock_bits, armed, BinaryOp::And, unfired);
            g.store(slots.clock_armed, clock_bits, armed);
            if !waits.is_empty() {
                // The waits see the clocks' levels before the process phase.
                let zero = g.constant(0, clock_bits);
                let applied = g.binary(1, fired, BinaryOp::Ne, zero);
                let check = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: applied,
                    true_block: (check, Vec::new()),
                    false_block: (entry_done, Vec::new()),
                });
                g.b.switch_to_block(check);
                g.check_waits_at(&checks, false);
            }
        }
        g.b.seal_block(SIRTerminator::Jump(entry_done, Vec::new()));

        // A pass of the process phase: the lowest ready kernel runs, and
        // the waits see its writes before the next one while kernels remain
        // ready (`run_ready`); then combinational logic settles.
        g.b.switch_to_block(pick);
        let ready = g.load(slots.ready, g.ready_bits);
        let zero = g.constant(0, g.ready_bits);
        let idle = g.binary(1, ready, BinaryOp::Eq, zero);
        let pass_end = g.b.new_block();
        let chosen = g.b.new_block();
        let idle_region = if assertions {
            g.b.new_block()
        } else {
            pass_end
        };
        g.b.seal_block(SIRTerminator::Branch {
            cond: idle,
            true_block: (idle_region, Vec::new()),
            false_block: (chosen, Vec::new()),
        });
        if assertions {
            g.b.switch_to_block(idle_region);
            let phase = g.load_at(
                slots.defer_limit_settle,
                celox_design::SCHEDULER_ASSERTION_PHASE_BIT,
                SCHEDULER_FLAG_WIDTH,
            );
            let active = g.equals(phase, 0);
            let late = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: active,
                true_block: (pass_end, Vec::new()),
                false_block: (late, Vec::new()),
            });
            g.b.switch_to_block(late);
            let observed = g.equals(phase, 1);
            g.b.seal_block(SIRTerminator::Branch {
                cond: observed,
                true_block: (reactive_entry.unwrap(), Vec::new()),
                false_block: (reactive_end.unwrap(), Vec::new()),
            });
            g.b.switch_to_block(reactive_end.unwrap());
            let active = g.constant(0, SCHEDULER_FLAG_WIDTH);
            g.store_at(
                slots.defer_limit_settle,
                celox_design::SCHEDULER_ASSERTION_PHASE_BIT,
                SCHEDULER_FLAG_WIDTH,
                active,
            );
            g.b.seal_block(SIRTerminator::Jump(pass_end, Vec::new()));
        }
        g.b.switch_to_block(chosen);
        let mut runs = Vec::new();
        for (index, kernel) in self.kernels.iter().enumerate() {
            if kernel.is_none() {
                continue;
            }
            let this = g.mask_bit(ready, ordinals[index]);
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

        let mut full_wait_check = None;
        let mut wait_continuations = vec![pick];
        for (index, run) in runs {
            let kernel = self.kernels[index].as_ref().expect("selected above");
            let process = &schema.processes[index];
            g.b.switch_to_block(run);
            let clear = g.constant(0, 1);
            g.store_at(slots.ready, ordinals[index], 1, clear);
            let current = g.constant(index as u64, SCHEDULER_TIME_WIDTH);
            g.store(slots.current, SCHEDULER_TIME_WIDTH, current);
            if self.cyclic {
                if let Some(guards) = &mut guards {
                    guards.charge(
                        &mut g,
                        index as u64,
                        crate::simulator::native_io::SCHEDULER_RUN,
                        time,
                    );
                } else {
                    g.service_call(
                        self.service_function.expect("native work-list services"),
                        index as u64,
                        crate::simulator::native_io::SCHEDULER_RUN,
                        time,
                        internal_exit,
                    );
                }
            }
            let first_host = self
                .service_processes
                .contains(&index)
                .then(|| g.b.alloc_bit(64, false));
            let enter = match first_host {
                Some(first) => g.b.new_block_with(vec![first]),
                None => g.b.new_block(),
            };
            let enter_args = first_host
                .map(|_| vec![g.constant(1, 64)])
                .unwrap_or_default();
            g.b.seal_block(SIRTerminator::Jump(enter, enter_args));
            g.b.switch_to_block(enter);
            let before: Vec<_> = kernel
                .writes
                .iter()
                .map(|&event| g.level(&self.events[event]))
                .collect();
            g.inline_writer(&program.sir.processes[index], finish_exit);
            if !kernel.writes.is_empty() {
                // Capture every continuation, including returns to a host service.
                // A later continuation cannot erase an already observed pulse.
                let mut edges = g.load(slots.exit_arg, self.event_bits);
                for (&event, &was) in kernel.writes.iter().zip(&before) {
                    let is = g.level(&self.events[event]);
                    let edge = g.edge(&self.events[event], was, is);
                    let bit = g.event_bit(edge, event);
                    edges = g.binary(self.event_bits, edges, BinaryOp::Or, bit);
                }
                g.store(slots.exit_arg, self.event_bits, edges);
            }
            let status = g.load(process.status, celox_design::PROCESS_STATUS_WIDTH);
            let code = |status: ProcessStatus| u64::from(status.code());
            if self.service_processes.contains(&index) {
                let host = g.equals(status, code(ProcessStatus::HostRequest));
                let yielded = yielding_status(&program.sir.processes[index], process.status)
                    .then(|| g.equals(status, code(ProcessStatus::Yield)));
                let mut service =
                    yielded.map_or(host, |yielded| g.binary(1, host, BinaryOp::Or, yielded));
                let nba = self
                    .nba_processes
                    .contains(&index)
                    .then(|| g.equals(status, code(ProcessStatus::Nba)));
                if let Some(nba) = nba {
                    service = g.binary(1, service, BinaryOp::Or, nba);
                }
                let call = g.b.new_block();
                let returned = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: service,
                    true_block: (call, Vec::new()),
                    false_block: (returned, Vec::new()),
                });
                g.b.switch_to_block(call);
                let resume = g.b.new_block();
                if let (Some(nbas), Some(nba)) = (g.nbas, nba) {
                    let capture = g.b.new_block();
                    let host = g.b.new_block();
                    g.b.seal_block(SIRTerminator::Branch {
                        cond: nba,
                        true_block: (capture, Vec::new()),
                        false_block: (host, Vec::new()),
                    });
                    g.b.switch_to_block(capture);
                    let site = g.load(process.delay, 64);
                    nbas.capture(&mut g, index, site);
                    guards.as_mut().expect("native NBA budget guards").charge(
                        &mut g,
                        index as u64,
                        crate::simulator::native_io::CHECKPOINT,
                        time,
                    );
                    g.b.seal_block(SIRTerminator::Jump(resume, Vec::new()));
                    g.b.switch_to_block(host);
                }
                if let (Some(guards), Some(yielded)) = (&mut guards, yielded) {
                    let checkpoint = g.b.new_block();
                    let host_call = g.b.new_block();
                    g.b.seal_block(SIRTerminator::Branch {
                        cond: yielded,
                        true_block: (checkpoint, Vec::new()),
                        false_block: (host_call, Vec::new()),
                    });
                    g.b.switch_to_block(checkpoint);
                    guards.charge(
                        &mut g,
                        index as u64,
                        crate::simulator::native_io::CHECKPOINT,
                        time,
                    );
                    g.b.seal_block(SIRTerminator::Jump(resume, Vec::new()));
                    g.b.switch_to_block(host_call);
                }
                let current = g.constant(index as u64, 64);
                let request = g.load(process.delay, celox_design::PROCESS_DELAY_WIDTH);
                let request = if let Some(yielded) = yielded {
                    let checkpoint = g.constant(crate::simulator::native_io::CHECKPOINT, 64);
                    let selected = g.b.alloc_bit(64, false);
                    g.b.emit(SIRInstruction::Mux(selected, yielded, checkpoint, request));
                    selected
                } else {
                    request
                };
                let request = if let Some(nba) = nba {
                    let prefix = g.constant(crate::simulator::native_io::NBA_REQUEST, 64);
                    let encoded = g.binary(64, request, BinaryOp::Or, prefix);
                    let selected = g.b.alloc_bit(64, false);
                    g.b.emit(SIRInstruction::Mux(selected, nba, encoded, request));
                    selected
                } else {
                    request
                };
                let result = g.b.alloc_bit(64, false);
                let context = g.load_at(
                    slots.defer_limit_settle,
                    celox_design::SCHEDULER_HOST_CONTEXT_BIT,
                    64,
                );
                g.b.emit(SIRInstruction::ExternCall {
                    dst: Some(result),
                    func: self.service_function.expect("qualified native host call"),
                    args: vec![
                        context,
                        current,
                        request,
                        time,
                        first_host.expect("a native host continuation"),
                    ],
                });
                let ok = g.equals(result, 0);
                g.b.seal_block(SIRTerminator::Branch {
                    cond: ok,
                    true_block: (resume, Vec::new()),
                    false_block: (internal_exit, Vec::new()),
                });
                g.b.switch_to_block(resume);
                // The reference observes the writes on either side of a host
                // request before continuing the same process. A direct call
                // keeps that boundary without leaving the native scheduler.
                let checked = g.b.new_block();
                wait_continuations.push(checked);
                let continuation = g.constant(wait_continuations.len() as u64, g.cont_bits);
                g.store(slots.cont, g.cont_bits, continuation);
                let check = *full_wait_check.get_or_insert_with(|| g.b.new_block());
                g.b.seal_block(SIRTerminator::Jump(check, Vec::new()));
                g.b.switch_to_block(checked);
                let resumed = g.constant(0, 64);
                g.b.seal_block(SIRTerminator::Jump(enter, vec![resumed]));
                g.b.switch_to_block(returned);
            }

            let finished = g.equals(status, code(ProcessStatus::Finish));
            let not_finished = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: finished,
                true_block: (finish_exit, Vec::new()),
                false_block: (not_finished, Vec::new()),
            });
            g.b.switch_to_block(not_finished);
            let resumed = g.b.new_block();
            let done = g.equals(status, code(ProcessStatus::Done));
            let ended = g.b.new_block();
            let suspended = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: done,
                true_block: (ended, Vec::new()),
                false_block: (suspended, Vec::new()),
            });
            // A kernel woken by its edge no longer waits (`check_waits`);
            // one woken by its wake time does until it ends.
            g.b.switch_to_block(ended);
            if kernel.delays {
                let idle = g.constant(0, SCHEDULER_FLAG_WIDTH);
                g.store(slots.waiting[index], SCHEDULER_FLAG_WIDTH, idle);
            }
            g.b.seal_block(SIRTerminator::Jump(resumed, Vec::new()));

            g.b.switch_to_block(suspended);
            let queued_statuses = [
                ProcessStatus::AssertionObserved,
                ProcessStatus::AssertionReactive,
                ProcessStatus::DeferActive,
            ];
            let mut queued = None;
            for code in queued_statuses {
                if returns_status(&program.sir.processes[index], process.status, code) {
                    let matches = g.equals(status, u64::from(code.code()));
                    queued = Some(
                        queued.map_or(matches, |prior| g.binary(1, prior, BinaryOp::Or, matches)),
                    );
                }
            }
            if let Some(queued) = queued {
                let queue = g.b.new_block();
                let other = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: queued,
                    true_block: (queue, Vec::new()),
                    false_block: (other, Vec::new()),
                });
                g.b.switch_to_block(queue);
                let idle = g.constant(0, SCHEDULER_FLAG_WIDTH);
                g.store(slots.waiting[index], SCHEDULER_FLAG_WIDTH, idle);
                g.b.seal_block(SIRTerminator::Jump(resumed, Vec::new()));
                g.b.switch_to_block(other);
            }
            let edge_wait = match &kernel.wait {
                Some(wait) => {
                    // A kernel runs at most once a slot when it stages
                    // nonblocking assignments (see `Plan::new`), so a staged
                    // site is never staged again before the NBA region and
                    // the kernel never returns `Nba`.
                    let waiting = g.equals(status, code(ProcessStatus::Wait));
                    let pending = g.equals(status, code(ProcessStatus::Pending));
                    let suspends = g.binary(1, waiting, BinaryOp::Or, pending);
                    let block = g.b.new_block();
                    Some((wait, suspends, block))
                }
                None => None,
            };
            if kernel.delays {
                let delay = g.equals(status, code(ProcessStatus::Delay));
                let inactive = g.equals(status, code(ProcessStatus::InactiveDelay));
                let delayed = g.binary(1, delay, BinaryOp::Or, inactive);
                let wait = g.b.new_block();
                let other = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: delayed,
                    true_block: (wait, Vec::new()),
                    false_block: (other, Vec::new()),
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
                // Restored Active work need not have an armed wakeup, even
                // for kernels that only delay. Each new delay arms its slot.
                let flag = g.constant(1, SCHEDULER_FLAG_WIDTH);
                g.store(slots.waiting[index], SCHEDULER_FLAG_WIDTH, flag);
                g.b.seal_block(SIRTerminator::Jump(resumed, Vec::new()));
                g.b.switch_to_block(other);
            }
            match edge_wait {
                None => {
                    g.b.seal_block(SIRTerminator::Jump(internal_exit, Vec::new()));
                }
                Some((wait, suspends, block)) => {
                    g.b.seal_block(SIRTerminator::Branch {
                        cond: suspends,
                        true_block: (block, Vec::new()),
                        false_block: (internal_exit, Vec::new()),
                    });
                    // It waits again: the edge is judged against the level
                    // now (`register_waiter`).
                    g.b.switch_to_block(block);
                    let flag = g.constant(1, SCHEDULER_FLAG_WIDTH);
                    g.store(slots.waiting[index], SCHEDULER_FLAG_WIDTH, flag);
                    g.register_wait(index, process.delay, wait, resumed, internal_exit);
                }
            }
            g.b.switch_to_block(resumed);
            if assertions {
                // Observed predicates all finish before any Reactive action;
                // design work produced by actions waits for the next Active pass.
                let phase = g.load_at(
                    slots.defer_limit_settle,
                    celox_design::SCHEDULER_ASSERTION_PHASE_BIT,
                    SCHEDULER_FLAG_WIDTH,
                );
                let active = g.equals(phase, 0);
                let next = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: active,
                    true_block: (next, Vec::new()),
                    false_block: (pick, Vec::new()),
                });
                g.b.switch_to_block(next);
            }
            // Before the next run, the waits see this one's writes.
            let ready = g.load(slots.ready, g.ready_bits);
            let zero = g.constant(0, g.ready_bits);
            let more = g.binary(1, ready, BinaryOp::Ne, zero);
            let check = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: more,
                true_block: (check, Vec::new()),
                false_block: (pick, Vec::new()),
            });
            g.b.switch_to_block(check);
            if self.service_processes.contains(&index)
                || instructions(&program.sir.processes[index]).any(makes_extern_calls)
            {
                // Every opaque writer needs the same scan and returns to
                // `pick`. Share its body: copying every wait after every
                // service process makes large assertion-heavy RTL quadratic.
                let continuation = g.constant(1, g.cont_bits);
                g.store(slots.cont, g.cont_bits, continuation);
                let check = *full_wait_check.get_or_insert_with(|| g.b.new_block());
                g.b.seal_block(SIRTerminator::Jump(check, Vec::new()));
            } else {
                g.check_waits_after_unit(
                    &checks,
                    &program.sir.processes[index],
                    &schema.time_slots,
                );
                g.b.seal_block(SIRTerminator::Jump(pick, Vec::new()));
            }
        }
        if let Some(check) = full_wait_check {
            g.b.switch_to_block(check);
            if assertions {
                let phase = g.load_at(
                    slots.defer_limit_settle,
                    celox_design::SCHEDULER_ASSERTION_PHASE_BIT,
                    SCHEDULER_FLAG_WIDTH,
                );
                let active = g.equals(phase, 0);
                let scan = g.b.new_block();
                let checked = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: active,
                    true_block: (scan, Vec::new()),
                    false_block: (checked, Vec::new()),
                });
                g.b.switch_to_block(scan);
                g.check_waits_at(&checks, false);
                g.b.seal_block(SIRTerminator::Jump(checked, Vec::new()));
                g.b.switch_to_block(checked);
            } else {
                g.check_waits_at(&checks, false);
            }
            g.dispatch_continuations(&wait_continuations, internal_exit);
        }

        // The settles after a pass and after the domains share one copy of
        // combinational logic. A deferred settle after a pass runs for the
        // kernels that may run in another.
        g.b.switch_to_block(pass_end);
        if host_entry.is_some() {
            let pending = host_inputs::pending(&mut g);
            g.if_bit(pending, 0, |g| {
                let zero = g.constant(0, 64);
                g.arena_header_store(
                    std::mem::offset_of!(crate::simulator::native_io::ArenaHeader, host_entry),
                    zero,
                );
                // Raw host writes wake Active processes before the first settle.
                // Judge host domains only after those processes have run, so a
                // process can overwrite the host's level before any FF commits.
                let mut edges = g.load(slots.exit_arg, self.event_bits);
                let levels = g.load_levels();
                for &index in &edged {
                    let was = g.saved_level(levels, position(&tracked, index));
                    let is = g.level(&self.events[index]);
                    let edge = g.settled_edge(&self.events[index], was, is);
                    let bit = g.event_bit(edge, index);
                    edges = g.binary(self.event_bits, edges, BinaryOp::Or, bit);
                }
                g.store(slots.exit_arg, self.event_bits, edges);
            });
        }
        if self.cyclic {
            if let Some(guards) = &mut guards {
                guards.charge(
                    &mut g,
                    u64::MAX,
                    crate::simulator::native_io::SCHEDULER_PASS,
                    time,
                );
            } else {
                g.service_call(
                    self.service_function.expect("native work-list services"),
                    u64::MAX,
                    crate::simulator::native_io::SCHEDULER_PASS,
                    time,
                    internal_exit,
                );
            }
        }
        if !self.cyclic
            && let Some(service) = self.service_function
        {
            let enabled = g.arena_header_load(std::mem::offset_of!(
                crate::simulator::native_io::ArenaHeader,
                slot_observer
            ));
            g.if_bit(enabled, 0, |g| {
                g.service_call(
                    service,
                    u64::MAX,
                    crate::simulator::native_io::SCHEDULER_PASS,
                    time,
                    internal_exit,
                );
            });
        }
        let none = g.constant(u64::MAX, SCHEDULER_TIME_WIDTH);
        g.store(slots.current, SCHEDULER_TIME_WIDTH, none);
        let settled = g.b.new_block();
        let slot_end = g.b.new_block();
        let wait_observers = self
            .lazy
            .as_ref()
            .is_some_and(|lazy| waits.iter().any(|&(index, _)| lazy.kernels[index]));
        // Gate edges must see the first settle, before the processes that
        // it wakes can change an enable. Later passes may settle lazily.
        if !self.gating.is_empty() && self.lazy.is_some() {
            let judged = g.load_at(
                slots.defer_limit_settle,
                GATES_JUDGED_BIT,
                SCHEDULER_FLAG_WIDTH,
            );
            let first = g.b.new_block();
            let later = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: judged,
                true_block: (later, Vec::new()),
                false_block: (first, Vec::new()),
            });
            g.b.switch_to_block(first);
            g.settle_then(settled);
            g.b.switch_to_block(later);
        }
        match &self.lazy {
            Some(lazy) => g.eager_then(&lazy.eager, settled),
            None => g.settle_then(settled),
        }

        // After the settle of a pass the waits see its writes; woken
        // kernels run in another pass.
        g.b.switch_to_block(settled);
        if !self.counted.is_empty() {
            let counted = self.counted_edges(&mut g);
            if !self.gating.is_empty() {
                let edged = self.source_edged(&mut g, counted);
                g.store_at(
                    slots.defer_limit_settle,
                    GATE_SOURCE_EDGED_BIT,
                    SCHEDULER_FLAG_WIDTH,
                    edged,
                );
            }
            let active = g.load(slots.exit_arg, self.event_bits);
            let active = g.binary(self.event_bits, active, BinaryOp::Or, counted);
            g.store(slots.exit_arg, self.event_bits, active);
        }
        self.judge_gates(&mut g, &tracked);
        g.check_waits(&checks);
        if !deferred.is_empty() {
            let ready = g.load(slots.ready, g.ready_bits);
            let zero = g.constant(0, g.ready_bits);
            let idle = g.binary(1, ready, BinaryOp::Eq, zero);
            let promote = g.b.new_block();
            let promoted = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: idle,
                true_block: (promote, Vec::new()),
                false_block: (promoted, Vec::new()),
            });
            g.b.switch_to_block(promote);
            let ready = g.status_ready(&deferred, &ordinals, schema, ProcessStatus::DeferActive);
            g.store(slots.ready, g.ready_bits, ready);
            g.b.seal_block(SIRTerminator::Jump(promoted, Vec::new()));
            g.b.switch_to_block(promoted);
        }
        let ready = g.load(slots.ready, g.ready_bits);
        let zero = g.constant(0, g.ready_bits);
        let more = g.binary(1, ready, BinaryOp::Ne, zero);
        let domains = g.b.new_block();
        let resume_domains =
            (!self.active.is_empty() || self.ordered.is_some()).then(|| g.b.new_block());
        let route_domains = resume_domains.map(|_| g.b.new_block()).unwrap_or(domains);
        let another = if wait_observers {
            g.b.new_block()
        } else {
            pick
        };
        g.b.seal_block(SIRTerminator::Branch {
            cond: more,
            true_block: (another, Vec::new()),
            false_block: (route_domains, Vec::new()),
        });
        if let Some(resume) = resume_domains {
            g.b.switch_to_block(route_domains);
            let pending = g.load_at(
                slots.defer_limit_settle,
                celox_design::SCHEDULER_ACTIVE_RESUME_BIT,
                SCHEDULER_FLAG_WIDTH,
            );
            g.b.seal_block(SIRTerminator::Branch {
                cond: pending,
                true_block: (resume, Vec::new()),
                false_block: (domains, Vec::new()),
            });
        }
        if wait_observers {
            g.b.switch_to_block(another);
            g.settle_then(pick);
        }

        // The domains the edges trigger, then the NBA region.
        g.b.switch_to_block(domains);
        self.observe_slot(&mut g, &tracked, &checks, finish_exit, true);
        if !self.active.is_empty() || self.ordered.is_some() {
            self.batch_active(
                &mut g,
                &tracked,
                slot_end,
                finish_exit,
                &checks,
                pick,
                resume_domains.expect("active resume block"),
            );
        } else if !self.derived.is_empty()
            || !self.counted.is_empty()
            || self.events.iter().any(|event| event.any_settled)
        {
            // Unspecified domains use rising edges for scheduled writes, but
            // any change for later settled waves. Keep that ordering when
            // another clock's commit changes the domain's inputs.
            self.batch(&mut g, &tracked, slot_end, finish_exit, &checks);
        } else {
            let staged: Vec<&Staged> = waits.iter().flat_map(|(_, wait)| &wait.sites).collect();
            let mask = g.load(slots.exit_arg, self.event_bits);
            let zero = g.constant(0, self.event_bits);
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
                g.publish_service(
                    self.service_function
                        .filter(|_| !self.nba_processes.is_empty()),
                    &checks,
                );
                g.eager_then(&lazy.eager, slot_end);
            } else if staged.is_empty() && self.nba_processes.is_empty() {
                g.b.seal_block(SIRTerminator::Jump(slot_end, Vec::new()));
            } else {
                let mut any = g.constant(u64::from(!self.nba_processes.is_empty()), 1);
                for site in &staged {
                    let pending = g.load(site.pending, 1);
                    any = g.binary(1, any, BinaryOp::Or, pending);
                }
                g.publish(&staged);
                g.publish_service(
                    self.service_function
                        .filter(|_| !self.nba_processes.is_empty()),
                    &checks,
                );
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
                let one = g.constant(1, self.event_bits);
                let lower = g.binary(self.event_bits, mask, BinaryOp::Sub, one);
                let others = g.binary(self.event_bits, mask, BinaryOp::And, lower);
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
                let mut seen = HashSet::default();
                for &index in written
                    .iter()
                    .chain(&self.clocks)
                    .chain(&self.host_edges)
                    .filter(|&&index| seen.insert(index))
                {
                    let event = &self.events[index];
                    let bit = g.single(index, self.event_bits);
                    let this = g.binary(1, mask, BinaryOp::Eq, bit);
                    let unit = g.b.new_block();
                    let next = g.b.new_block();
                    g.b.seal_block(SIRTerminator::Branch {
                        cond: this,
                        true_block: (unit, Vec::new()),
                        false_block: (next, Vec::new()),
                    });
                    g.b.switch_to_block(unit);
                    if event.active_commit
                        && event
                            .eval_apply
                            .iter()
                            .any(|unit| emits_finish(unit, &self.finish_sites))
                        && let (Some(eval), Some(apply)) = (&event.eval_only, &event.apply)
                    {
                        // Unobserved blocking writes may let us skip Active
                        // waves, but `$finish` must still precede NBA publication.
                        if self.lazy.is_some() {
                            let evaluate = g.b.new_block();
                            g.settle_then(evaluate);
                            g.b.switch_to_block(evaluate);
                        }
                        g.inline_writers(eval, finish_exit);
                        g.inline_writers(apply, finish_exit);
                        g.b.seal_block(SIRTerminator::Jump(evaluated, Vec::new()));
                        g.b.switch_to_block(next);
                        continue;
                    }
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
                            g.inline_writers(&event.eval_apply, finish_exit);
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
                    g.inline_writers(units, finish_exit);
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
                        mask = g.load(slots.exit_arg, self.event_bits);
                        zero = g.constant(0, self.event_bits);
                    }
                    for phase in [Phase::Evaluate, Phase::Apply] {
                        for &index in &edged {
                            let event = &self.events[index];
                            let bit = g.single(index, self.event_bits);
                            let selected = g.binary(self.event_bits, mask, BinaryOp::And, bit);
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
                            g.inline_writers(units, finish_exit);
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
            g.publish_service(
                self.service_function
                    .filter(|_| !self.nba_processes.is_empty()),
                &checks,
            );
            match &self.lazy {
                Some(lazy) => g.eager_then(&lazy.eager, slot_end),
                None => g.settle_then(slot_end),
            }
        }

        // At the end of the slot each clock whose edge applied waits for the
        // opposite level, half a period later, in event order (as
        // `rearm_clocks`): the extra time unit of an odd period goes to the
        // high phase. The reference re-arms at the end of the slot's first
        // round (`continue_round`), before processes the NBA results wake
        // resume. On their later rounds the clocks are already armed.
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
            let armed = g.load(slots.clock_armed, clock_bits);
            let all = g.ones(clock_bits);
            let unarmed = g.binary(clock_bits, armed, BinaryOp::Xor, all);
            let fired = g.binary(clock_bits, started, BinaryOp::And, unarmed);
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
                let values = g.load(slots.clock_value, clock_bits);
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
                let flip = g.single(bit, clock_bits);
                let values = g.binary(clock_bits, values, BinaryOp::Xor, flip);
                g.store(slots.clock_value, clock_bits, values);
                let armed = g.load(slots.clock_armed, clock_bits);
                let armed = g.binary(clock_bits, armed, BinaryOp::Or, flip);
                g.store(slots.clock_armed, clock_bits, armed);
                g.b.seal_block(SIRTerminator::Jump(next, Vec::new()));
                g.b.switch_to_block(next);
            }
            g.b.seal_block(SIRTerminator::Jump(rearmed, Vec::new()));
            g.b.switch_to_block(rearmed);
        }
        let reactive_waits = waits.iter().any(|(_, wait)| wait.reactive);
        if reactive_waits || (self.service_function.is_some() && !waits.is_empty()) {
            // A host callback can wake an otherwise undriven input wait. Its
            // activation must drain natively after the domains too.
            let observer_done = (!reactive_waits).then(|| g.b.new_block());
            if let Some(done) = observer_done {
                let enabled = g.arena_header_load(std::mem::offset_of!(
                    crate::simulator::native_io::ArenaHeader,
                    slot_observer
                ));
                let enabled = g.equals(enabled, 1);
                let scan = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: enabled,
                    true_block: (scan, Vec::new()),
                    false_block: (done, Vec::new()),
                });
                g.b.switch_to_block(scan);
            }
            // NBA publication can wake data-sensitive kernels in this same
            // time slot. Their next pass must not evaluate the domains that
            // just ran a second time: compare against the levels now and
            // consume the slot's original edge mask before returning to pick.
            g.check_waits(&checks);
            let ready = g.load(slots.ready, g.ready_bits);
            let zero = g.constant(0, g.ready_bits);
            let more = g.binary(1, ready, BinaryOp::Ne, zero);
            let resume = g.b.new_block();
            let drained = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: more,
                true_block: (resume, Vec::new()),
                false_block: (drained, Vec::new()),
            });
            g.b.switch_to_block(resume);
            if !self.service_processes.is_empty() {
                if guards.is_some() {
                    ordered::Guards::process_phase(&mut g);
                } else {
                    g.service_call(
                        self.service_function.expect("native host services"),
                        u64::MAX,
                        crate::simulator::native_io::PROCESS_PHASE,
                        time,
                        internal_exit,
                    );
                }
            }
            let mut levels = g.no_levels();
            for (bit, &event) in tracked.iter().enumerate() {
                let level = g.level(&self.events[event]);
                levels = g.with_level(levels, level, bit);
            }
            g.store_levels(levels);
            let zero = g.constant(0, self.event_bits);
            g.store(slots.exit_arg, self.event_bits, zero);
            if !self.gating.is_empty() {
                let clear = g.constant(0, SCHEDULER_FLAG_WIDTH);
                g.store_at(
                    slots.defer_limit_settle,
                    GATE_SOURCE_EDGED_BIT,
                    SCHEDULER_FLAG_WIDTH,
                    clear,
                );
            }
            g.b.seal_block(SIRTerminator::Jump(pick, Vec::new()));
            g.b.switch_to_block(drained);
            if let Some(done) = observer_done {
                g.b.seal_block(SIRTerminator::Jump(done, Vec::new()));
                g.b.switch_to_block(done);
            }
        }
        if assertions {
            // IEEE 1800-2023 4.4.2.5-6: drain all Observed evaluations,
            // then their Reactive actions, before returning to Active work.
            // Predicates may read live combinational disable conditions.
            g.settle_then(observed_entry.unwrap());
            for (entry, kernels, status, phase, empty) in [
                (
                    observed_entry.unwrap(),
                    &observed,
                    ProcessStatus::AssertionObserved,
                    1,
                    reactive_entry.unwrap(),
                ),
                (
                    reactive_entry.unwrap(),
                    &reactive,
                    ProcessStatus::AssertionReactive,
                    2,
                    slot_done.unwrap(),
                ),
            ] {
                g.b.switch_to_block(entry);
                let none = g.constant(u64::MAX, SCHEDULER_TIME_WIDTH);
                g.store(slots.current, SCHEDULER_TIME_WIDTH, none);
                let ready = g.status_ready(kernels, &ordinals, schema, status);
                g.store(slots.ready, g.ready_bits, ready);
                let phase_value = g.constant(phase, SCHEDULER_FLAG_WIDTH);
                g.store_at(
                    slots.defer_limit_settle,
                    celox_design::SCHEDULER_ASSERTION_PHASE_BIT,
                    SCHEDULER_FLAG_WIDTH,
                    phase_value,
                );
                if phase == 2 {
                    let mut levels = g.no_levels();
                    for (bit, &event) in tracked.iter().enumerate() {
                        let level = g.level(&self.events[event]);
                        levels = g.with_level(levels, level, bit);
                    }
                    g.store_levels(levels);
                    let zero = g.constant(0, self.event_bits);
                    g.store(slots.exit_arg, self.event_bits, zero);
                    if !self.gating.is_empty() {
                        let clear = g.constant(0, SCHEDULER_FLAG_WIDTH);
                        g.store_at(
                            slots.defer_limit_settle,
                            GATE_SOURCE_EDGED_BIT,
                            SCHEDULER_FLAG_WIDTH,
                            clear,
                        );
                    }
                }
                let zero = g.constant(0, g.ready_bits);
                let any = g.binary(1, ready, BinaryOp::Ne, zero);
                g.b.seal_block(SIRTerminator::Branch {
                    cond: any,
                    true_block: (pick, Vec::new()),
                    false_block: (empty, Vec::new()),
                });
            }
            g.b.switch_to_block(slot_done.unwrap());
        }
        // The run returns after the slot that reaches its budget, or in
        // which its runtime events reach a batch, so that the host drains
        // them before the ring buffer overflows.
        let count = g.load(slots.count, SCHEDULER_TIME_WIDTH);
        let budget = g.load(slots.budget, SCHEDULER_TIME_WIDTH);
        let mut returns = g.binary(1, count, BinaryOp::GeU, budget);
        if let Some(batch) = g.event_batch {
            let events = g.load(slots.events, SCHEDULER_TIME_WIDTH);
            let batch = g.constant(batch, SCHEDULER_TIME_WIDTH);
            let full = g.binary(1, events, BinaryOp::GeU, batch);
            returns = g.binary(1, returns, BinaryOp::Or, full);
        }
        g.b.seal_block(SIRTerminator::Branch {
            cond: returns,
            true_block: (stopped, Vec::new()),
            false_block: (header, Vec::new()),
        });
        g.emit_settle(&program.sir.eval_comb, finish_exit, internal_exit);
        if let Some(lazy) = &self.lazy {
            g.emit_eager(&lazy.eager, finish_exit, internal_exit);
        }
        if let Some(guards) = guards {
            guards.emit(&mut g, internal_exit);
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
    /// Active-region domain waves, with NBA publication deferred until
    /// blocking-result edges have quiesced. The upper half of `exit_arg`
    /// holds the deferred-domain mask across the shared settle continuation.
    fn batch_active(
        &self,
        g: &mut Generator<'_>,
        tracked: &[usize],
        slot_end: celox_sir::BlockId,
        finish_exit: celox_sir::BlockId,
        checks: &[WaitCheck<'_>],
        pick: celox_sir::BlockId,
        wave: celox_sir::BlockId,
    ) {
        if let Some(ordered) = self.ordered {
            self.batch_ordered(
                g,
                ordered,
                tracked,
                slot_end,
                finish_exit,
                checks,
                pick,
                wave,
            );
            return;
        }
        let slots = g.slots;
        let empty = g.constant(0, self.event_bits);
        g.store_at(slots.exit_arg, self.event_bits, self.event_bits, empty);
        let flush = g.b.new_block();
        let mask = g.b.alloc_bit(self.event_bits, false);
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
        let active = g.load(slots.exit_arg, self.event_bits);
        let levels = g.load_levels();
        let stable = self.stable_edges(g, tracked, levels);
        let counted = self.counted_edges(g);
        let active = g.binary(self.event_bits, active, BinaryOp::Or, counted);
        let stable = g.binary(self.event_bits, stable, BinaryOp::Or, counted);
        let zero = g.constant(0, self.event_bits);
        let edged = if self.gating.is_empty() {
            g.binary(1, active, BinaryOp::Ne, zero)
        } else {
            let byte = self.source_edged(g, counted);
            g.equals(byte, 1)
        };
        let stable_extra = g.b.alloc_bit(self.event_bits, false);
        g.b.emit(SIRInstruction::Mux(stable_extra, edged, zero, stable));
        let triggers = g.binary(self.event_bits, active, BinaryOp::Or, stable_extra);
        g.store(slots.exit_arg, self.event_bits, triggers);
        let quiet = g.binary(1, triggers, BinaryOp::Eq, zero);
        let first = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: quiet,
            true_block: (flush, Vec::new()),
            false_block: (first, Vec::new()),
        });
        g.b.switch_to_block(first);
        g.b.seal_block(SIRTerminator::Jump(fire, vec![triggers]));

        // A wave after a commit: the domains not run yet whose clocks moved.
        g.b.switch_to_block(wave);
        let pending = g.load_at(slots.exit_arg, self.event_bits, self.event_bits);
        let zero = g.constant(0, self.event_bits);
        let post = g.binary(1, pending, BinaryOp::Eq, zero);
        g.if_bit(post, 0, |g| {
            self.observe_slot(g, tracked, checks, finish_exit, false)
        });
        if self
            .kernels
            .iter()
            .flatten()
            .filter_map(|kernel| kernel.wait.as_ref())
            .any(|wait| wait.reactive)
        {
            // A later derived domain can restore an expression's old value.
            // Match the reference round: remember the occurrence at this
            // settle and drain its ready processes after the domain waves.
            g.check_waits(checks);
            let ready = g.load(slots.ready, g.ready_bits);
            let pending = g.load_at(slots.exit_arg, self.event_bits, self.event_bits);
            let zero = g.constant(0, self.event_bits);
            let before_nba = g.binary(1, pending, BinaryOp::Ne, zero);
            let zero = g.constant(0, g.ready_bits);
            let ready = g.binary(1, ready, BinaryOp::Ne, zero);
            let resume = g.binary(1, before_nba, BinaryOp::And, ready);
            let drain = g.b.new_block();
            let next = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: resume,
                true_block: (drain, Vec::new()),
                false_block: (next, Vec::new()),
            });
            g.b.switch_to_block(drain);
            let flag = g.constant(1, SCHEDULER_FLAG_WIDTH);
            g.store_at(
                slots.defer_limit_settle,
                celox_design::SCHEDULER_ACTIVE_RESUME_BIT,
                SCHEDULER_FLAG_WIDTH,
                flag,
            );
            g.b.seal_block(SIRTerminator::Jump(pick, Vec::new()));
            g.b.switch_to_block(next);
        }

        let fired = g.load(slots.exit_arg, self.event_bits);
        let levels = g.load_levels();
        let stable = self.stable_edges(g, tracked, levels);
        let counted = self.counted_edges(g);
        let stable = g.binary(self.event_bits, stable, BinaryOp::Or, counted);
        let all = g.ones(self.event_bits);
        let unfired = g.binary(self.event_bits, fired, BinaryOp::Xor, all);
        let ready = g.binary(self.event_bits, stable, BinaryOp::And, unfired);
        let zero = g.constant(0, self.event_bits);
        let quiet = g.binary(1, ready, BinaryOp::Eq, zero);
        let drained = g.b.new_block();
        let next = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: quiet,
            true_block: (drained, Vec::new()),
            false_block: (next, Vec::new()),
        });
        g.b.switch_to_block(next);
        let fired = g.binary(self.event_bits, fired, BinaryOp::Or, ready);
        g.store(slots.exit_arg, self.event_bits, fired);
        g.b.seal_block(SIRTerminator::Jump(fire, vec![ready]));

        g.b.switch_to_block(drained);
        let pending = g.load_at(slots.exit_arg, self.event_bits, self.event_bits);
        let zero = g.constant(0, self.event_bits);
        let empty = g.binary(1, pending, BinaryOp::Eq, zero);
        g.b.seal_block(SIRTerminator::Branch {
            cond: empty,
            true_block: (slot_end, Vec::new()),
            false_block: (flush, Vec::new()),
        });

        // Evaluation commits blocking results immediately, while every NBA
        // stays in working state. Their Active-region edges settle and run
        // before this batch publishes any deferred domain (LRM 4.5).
        g.b.switch_to_block(fire);
        for &index in tracked {
            g.if_bit(mask, index, |g| {
                g.inline_writers(
                    self.events[index]
                        .eval_only
                        .as_ref()
                        .expect("split active domain"),
                    finish_exit,
                );
            });
        }
        let pending = g.load_at(slots.exit_arg, self.event_bits, self.event_bits);
        let pending = g.binary(self.event_bits, pending, BinaryOp::Or, mask);
        g.store_at(slots.exit_arg, self.event_bits, self.event_bits, pending);
        let mut active = g.constant(0, self.event_bits);
        for &index in &self.active {
            let bit = g.single(index, self.event_bits);
            active = g.binary(self.event_bits, active, BinaryOp::Or, bit);
        }
        let active = g.binary(self.event_bits, mask, BinaryOp::And, active);
        let zero = g.constant(0, self.event_bits);
        let committed = g.binary(1, active, BinaryOp::Ne, zero);
        let settle = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: committed,
            true_block: (settle, Vec::new()),
            false_block: (flush, Vec::new()),
        });
        g.b.switch_to_block(settle);
        g.settle_then(wave);

        // No Active work remains: publish the deferred NBA results, then
        // judge the edges that those updates cause in another wave.
        g.b.switch_to_block(flush);
        let flag = g.constant(0, SCHEDULER_FLAG_WIDTH);
        g.store_at(
            slots.defer_limit_settle,
            celox_design::SCHEDULER_ACTIVE_RESUME_BIT,
            SCHEDULER_FLAG_WIDTH,
            flag,
        );
        let pending = g.load_at(slots.exit_arg, self.event_bits, self.event_bits);
        for &index in tracked {
            g.if_bit(pending, index, |g| {
                g.inline_writers(
                    self.events[index]
                        .apply
                        .as_ref()
                        .expect("split active domain"),
                    finish_exit,
                );
            });
        }
        let staged: Vec<&Staged> = self
            .kernels
            .iter()
            .flatten()
            .filter_map(|kernel| kernel.wait.as_ref())
            .flat_map(|wait| &wait.sites)
            .collect();
        g.publish(&staged);
        g.publish_service(
            self.service_function
                .filter(|_| !self.nba_processes.is_empty()),
            checks,
        );
        let empty = g.constant(0, self.event_bits);
        g.store_at(slots.exit_arg, self.event_bits, self.event_bits, empty);
        g.settle_then(wave);
    }

    fn batch(
        &self,
        g: &mut Generator<'_>,
        tracked: &[usize],
        slot_end: celox_sir::BlockId,
        finish_exit: celox_sir::BlockId,
        checks: &[WaitCheck<'_>],
    ) {
        let slots = g.slots;
        let wave = g.b.new_block();
        let mask = g.b.alloc_bit(self.event_bits, false);
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
        let active = g.load(slots.exit_arg, self.event_bits);
        let levels = g.load_levels();
        let stable = self.stable_edges(g, tracked, levels);
        let counted = self.counted_edges(g);
        let active = g.binary(self.event_bits, active, BinaryOp::Or, counted);
        let stable = g.binary(self.event_bits, stable, BinaryOp::Or, counted);
        let zero = g.constant(0, self.event_bits);
        let edged = if self.gating.is_empty() {
            g.binary(1, active, BinaryOp::Ne, zero)
        } else {
            let byte = self.source_edged(g, counted);
            g.equals(byte, 1)
        };
        let stable_extra = g.b.alloc_bit(self.event_bits, false);
        g.b.emit(SIRInstruction::Mux(stable_extra, edged, zero, stable));
        let triggers = g.binary(self.event_bits, active, BinaryOp::Or, stable_extra);
        g.store(slots.exit_arg, self.event_bits, triggers);
        let quiet = g.binary(1, triggers, BinaryOp::Eq, zero);
        let first = g.b.new_block();
        let flush_only = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: quiet,
            true_block: (flush_only, Vec::new()),
            false_block: (first, Vec::new()),
        });
        g.b.switch_to_block(flush_only);
        let staged: Vec<&Staged> = self
            .kernels
            .iter()
            .flatten()
            .filter_map(|kernel| kernel.wait.as_ref())
            .flat_map(|wait| &wait.sites)
            .collect();
        g.publish(&staged);
        g.publish_service(
            self.service_function
                .filter(|_| !self.nba_processes.is_empty()),
            checks,
        );
        g.settle_then(wave);
        g.b.switch_to_block(first);
        g.b.seal_block(SIRTerminator::Jump(fire, vec![triggers]));

        // A wave after a commit: the domains not run yet whose clocks moved.
        g.b.switch_to_block(wave);
        self.observe_slot(g, tracked, checks, finish_exit, false);
        if self
            .kernels
            .iter()
            .flatten()
            .filter_map(|kernel| kernel.wait.as_ref())
            .any(|wait| wait.reactive)
        {
            // A later derived domain can restore an expression's old value.
            // Match the reference round: remember the occurrence at this
            // settle and drain its ready processes after the domain waves.
            g.check_waits(checks);
        }

        let fired = g.load(slots.exit_arg, self.event_bits);
        let levels = g.load_levels();
        let stable = self.stable_edges(g, tracked, levels);
        let counted = self.counted_edges(g);
        let stable = g.binary(self.event_bits, stable, BinaryOp::Or, counted);
        let all = g.ones(self.event_bits);
        let unfired = g.binary(self.event_bits, fired, BinaryOp::Xor, all);
        let ready = g.binary(self.event_bits, stable, BinaryOp::And, unfired);
        let zero = g.constant(0, self.event_bits);
        let quiet = g.binary(1, ready, BinaryOp::Eq, zero);
        let next = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: quiet,
            true_block: (slot_end, Vec::new()),
            false_block: (next, Vec::new()),
        });
        g.b.switch_to_block(next);
        let fired = g.binary(self.event_bits, fired, BinaryOp::Or, ready);
        g.store(slots.exit_arg, self.event_bits, fired);
        g.b.seal_block(SIRTerminator::Jump(fire, vec![ready]));

        // A wave: one domain evaluates and commits at once, several
        // evaluate, then commit, in event order; then logic settles.
        g.b.switch_to_block(fire);
        let zero = g.constant(0, self.event_bits);
        let one = g.constant(1, self.event_bits);
        let lower = g.binary(self.event_bits, mask, BinaryOp::Sub, one);
        let others = g.binary(self.event_bits, mask, BinaryOp::And, lower);
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
                let event = &self.events[index];
                if event
                    .eval_apply
                    .iter()
                    .any(|unit| fails(unit) || emits_finish(unit, &self.finish_sites))
                {
                    // The reference stops after an evaluation unit that
                    // ends the run, before publishing its deferred NBAs.
                    // A combined unit would publish them before `inline`
                    // can inspect the finish flag.
                    for units in [event.eval_only.as_ref(), event.apply.as_ref()] {
                        g.inline_writers(
                            units.expect("derived domains have separate units"),
                            finish_exit,
                        );
                    }
                } else {
                    g.inline_writers(&event.eval_apply, finish_exit);
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
                        g.inline_writers(units, finish_exit);
                    });
                }
            }
        }
        g.b.seal_block(SIRTerminator::Jump(committed, Vec::new()));
        g.b.switch_to_block(committed);
        g.publish(&staged);
        g.publish_service(
            self.service_function
                .filter(|_| !self.nba_processes.is_empty()),
            checks,
        );
        g.settle_then(wave);
    }

    /// Consume counted occurrences at the same settle boundaries as the
    /// reference work list. A pulse remains an event after its level returns.
    fn counted_edges(&self, g: &mut Generator<'_>) -> RegisterId {
        let mut edges = g.constant(0, self.event_bits);
        for counted in &self.counted {
            let seen = g.load_at(g.slots.levels, counted.baseline_offset, 32);
            let current = g.load(counted.counter, 32);
            let changed = g.binary(1, seen, BinaryOp::Ne, current);
            g.store_at(g.slots.levels, counted.baseline_offset, 32, current);
            for &event in &counted.events {
                let bit = g.event_bit(changed, event);
                edges = g.binary(self.event_bits, edges, BinaryOp::Or, bit);
            }
        }
        edges
    }

    /// Whether source/counting events, rather than only pre-judged gate
    /// edges, require preserving the source-before-derived commit order.
    fn source_edged(&self, g: &mut Generator<'_>, counted: RegisterId) -> RegisterId {
        let flag = g.load_at(
            g.slots.defer_limit_settle,
            GATE_SOURCE_EDGED_BIT,
            SCHEDULER_FLAG_WIDTH,
        );
        let prior = g.equals(flag, 1);
        let zero = g.constant(0, self.event_bits);
        let counted = g.binary(1, counted, BinaryOp::Ne, zero);
        let edged = g.binary(1, prior, BinaryOp::Or, counted);
        let byte = g.b.alloc_bit(SCHEDULER_FLAG_WIDTH, false);
        let one = g.constant(1, SCHEDULER_FLAG_WIDTH);
        let zero = g.constant(0, SCHEDULER_FLAG_WIDTH);
        g.b.emit(SIRInstruction::Mux(byte, edged, one, zero));
        byte
    }

    /// Reference `judge_derived_edges`: capture gate edges at the first
    /// settle after clock sources run, before newly woken processes run.
    /// Only positive-delay kernels and host clocks drive event signals;
    /// each runs once before this first settle. Later domain waves already
    /// track transitions from the baselines recorded here.
    fn judge_gates(&self, g: &mut Generator<'_>, tracked: &[usize]) {
        if self.gating.is_empty() {
            return;
        }
        let slots = g.slots;
        let judged = g.load_at(
            slots.defer_limit_settle,
            GATES_JUDGED_BIT,
            SCHEDULER_FLAG_WIDTH,
        );
        let first = g.b.new_block();
        let done = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: judged,
            true_block: (done, Vec::new()),
            false_block: (first, Vec::new()),
        });
        g.b.switch_to_block(first);
        let mut active = g.load(slots.exit_arg, self.event_bits);
        let original = self.source_edged(g, active);
        g.store_at(
            slots.defer_limit_settle,
            GATE_SOURCE_EDGED_BIT,
            SCHEDULER_FLAG_WIDTH,
            original,
        );
        let levels = g.load_levels();
        let mut updated = levels;
        for (source, gated) in &self.gating {
            let was = g.saved_level(levels, position(tracked, *source));
            let is = g.level(&self.events[*source]);
            let changed = g.binary(1, was, BinaryOp::Ne, is);
            for &clock in gated {
                let bit = position(tracked, clock);
                let was = g.saved_level(levels, bit);
                let is = g.level(&self.events[clock]);
                let edge = g.settled_edge(&self.events[clock], was, is);
                let edge = g.binary(1, edge, BinaryOp::And, changed);
                let edge = g.event_bit(edge, clock);
                active = g.binary(self.event_bits, active, BinaryOp::Or, edge);
                let level = g.b.alloc_bit(g.level_width(), false);
                let prior = g.saved_level(updated, bit);
                g.b.emit(SIRInstruction::Mux(level, changed, is, prior));
                updated = g.replace_level(updated, level, bit);
            }
        }
        g.store_levels(updated);
        g.store(slots.exit_arg, self.event_bits, active);
        let one = g.constant(1, SCHEDULER_FLAG_WIDTH);
        g.store_at(
            slots.defer_limit_settle,
            GATES_JUDGED_BIT,
            SCHEDULER_FLAG_WIDTH,
            one,
        );
        g.b.seal_block(SIRTerminator::Jump(done, Vec::new()));
        g.b.switch_to_block(done);
    }

    /// The tracked events whose levels moved to their active edge against
    /// `levels`, their baselines, as bits by event index.
    fn stable_edges(&self, g: &mut Generator<'_>, tracked: &[usize], levels: Levels) -> RegisterId {
        if tracked.len() >= 4 {
            return self.batched_stable_edges(g, tracked, levels);
        }
        let mut edges = g.constant(0, self.event_bits);
        for (position, &index) in tracked.iter().enumerate() {
            let was = g.saved_level(levels, position);
            let is = g.level(&self.events[index]);
            let edge = g.settled_edge(&self.events[index], was, is);
            let bit = g.event_bit(edge, index);
            edges = g.binary(self.event_bits, edges, BinaryOp::Or, bit);
        }
        edges
    }

    /// Judge independent edges as bit planes, in chunks of at most one word.
    /// Baselines use tracked positions; the result uses event indices, which
    /// need not be contiguous. No level or event is published by this check.
    fn batched_stable_edges(
        &self,
        g: &mut Generator<'_>,
        tracked: &[usize],
        levels: Levels,
    ) -> RegisterId {
        let zero = g.constant(0, 1);
        let mut result = vec![zero; self.event_bits];
        for (chunk, events) in tracked.chunks(64).enumerate() {
            let width = events.len();
            let mut values = Vec::with_capacity(width);
            let mut unknowns = Vec::with_capacity(width);
            let mut falling = 0u64;
            let mut any_change = 0u64;
            for (bit, &index) in events.iter().enumerate() {
                let event = &self.events[index];
                let level = g.level(event);
                let (value, unknown) = g.level_planes(level);
                values.push(value);
                if let Some(unknown) = unknown {
                    unknowns.push(unknown);
                }
                if event.active == 0 {
                    falling |= 1u64 << bit;
                }
                if event.any_settled {
                    any_change |= 1u64 << bit;
                }
            }
            let pack = |g: &mut Generator<'_>, mut bits: Vec<RegisterId>| {
                bits.reverse();
                let result = g.b.alloc_bit(width, false);
                g.b.emit(SIRInstruction::Concat(result, bits));
                result
            };
            let slice = |g: &mut Generator<'_>, source: RegisterId| {
                let result = g.b.alloc_bit(width, false);
                g.b.emit(SIRInstruction::Slice(result, source, chunk * 64, width));
                result
            };
            let was = slice(g, levels.values);
            let is = pack(g, values);
            // Invert both value planes of falling-edge lanes. Their masks
            // stay unchanged, turning the same edge formula into negedge.
            let falling = g.constant(falling, width);
            let was = g.binary(width, was, BinaryOp::Xor, falling);
            let is = g.binary(width, is, BinaryOp::Xor, falling);
            let all = g.ones(width);
            let not_was = g.binary(width, was, BinaryOp::Xor, all);
            let mut changed = (any_change != 0).then(|| g.binary(width, was, BinaryOp::Xor, is));
            let edges = if let Some(old_unknown) = levels.unknown {
                let old_unknown = slice(g, old_unknown);
                let new_unknown = pack(g, unknowns);
                if let Some(values) = changed {
                    let masks = g.binary(width, old_unknown, BinaryOp::Xor, new_unknown);
                    changed = Some(g.binary(width, values, BinaryOp::Or, masks));
                }
                // Table 9-2: leave known zero or reach known one, excluding
                // all unknown-to-unknown transitions (including X <-> Z).
                let leaves = g.binary(width, not_was, BinaryOp::Or, old_unknown);
                let reaches = g.binary(width, is, BinaryOp::Or, new_unknown);
                let both_unknown = g.binary(width, old_unknown, BinaryOp::And, new_unknown);
                let some_known = g.binary(width, both_unknown, BinaryOp::Xor, all);
                let edges = g.binary(width, leaves, BinaryOp::And, reaches);
                g.binary(width, edges, BinaryOp::And, some_known)
            } else {
                g.binary(width, not_was, BinaryOp::And, is)
            };
            let edges = if let Some(changed) = changed {
                let any = g.constant(any_change, width);
                let ordinary = g.binary(width, any, BinaryOp::Xor, all);
                let edges = g.binary(width, edges, BinaryOp::And, ordinary);
                let changed = g.binary(width, changed, BinaryOp::And, any);
                g.binary(width, edges, BinaryOp::Or, changed)
            } else {
                edges
            };
            for (bit, &index) in events.iter().enumerate() {
                let lane = g.b.alloc_bit(1, false);
                g.b.emit(SIRInstruction::Slice(lane, edges, bit, 1));
                result[index] = lane;
            }
        }
        result.reverse();
        let edges = g.b.alloc_bit(self.event_bits, false);
        g.b.emit(SIRInstruction::Concat(edges, result));
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
    program: &'a LaidOutProgram,
    nbas: Option<&'a nba::Sites>,
    observers: Option<&'a observers::Observers>,
    observer_service: Option<u32>,
    outlined: Option<&'a mut Vec<Unit>>,
    outline_base: u32,
    b: Builder,
    slots: &'a SchedulerSlots<AbsoluteAddr>,
    finish_sites: &'a HashSet<u32>,
    /// Exit blocks not yet emitted.
    exits: Vec<(celox_sir::BlockId, SchedulerExit)>,
    /// See [`Plan::settle_reads`].
    settle_reads: Option<&'a HashSet<AbsoluteAddr>>,
    incremental: Option<&'a Incremental>,
    partitioned_glue: Option<(&'a LaidOutProgram, HashSet<celox_sir::BlockId>)>,
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
    /// See [`Plan::event_bits`].
    event_bits: usize,
    /// One ready bit per compiled kernel, rounded to full words.
    ready_bits: usize,
    /// Continuation selectors must also scale beyond byte-sized designs.
    cont_bits: usize,
    /// The runtime events after which a run returns at the end of its slot
    /// ([`SchedulerSlots::events`]), when the inlined units count theirs.
    event_batch: Option<u64>,
}

impl Generator<'_> {
    fn constant(&mut self, value: u64, width: usize) -> RegisterId {
        self.wide_constant(BigUint::from(value), width)
    }

    fn wide_constant(&mut self, value: BigUint, width: usize) -> RegisterId {
        let register = self.b.alloc_bit(width, false);
        self.b
            .emit(SIRInstruction::Imm(register, SIRValue::new(value)));
        register
    }

    /// The `width`-bit value with only bit `bit` set.
    fn single(&mut self, bit: usize, width: usize) -> RegisterId {
        self.wide_constant(BigUint::from(1u8) << bit, width)
    }

    /// The `width`-bit value with every bit but `bit` set.
    fn all_but(&mut self, bit: usize, width: usize) -> RegisterId {
        let ones = (BigUint::from(1u8) << width) - 1u8;
        self.wide_constant(ones ^ (BigUint::from(1u8) << bit), width)
    }

    /// The `width`-bit value with every bit set.
    fn ones(&mut self, width: usize) -> RegisterId {
        self.wide_constant((BigUint::from(1u8) << width) - 1u8, width)
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

    fn publish_service(&mut self, function: Option<u32>, checks: &[WaitCheck<'_>]) {
        if let Some(nbas) = self.nbas {
            nbas.publish(self, checks);
            return;
        }
        let Some(function) = function else {
            return;
        };
        let again = self.b.new_block();
        let done = self.b.new_block();
        let changed = self.b.new_block();
        let failed = self.exit_block(SchedulerExit::Internal);
        self.b.seal_block(SIRTerminator::Jump(again, Vec::new()));
        self.b.switch_to_block(again);
        let context = self.load_at(
            self.slots.defer_limit_settle,
            celox_design::SCHEDULER_HOST_CONTEXT_BIT,
            64,
        );
        let process = self.constant(u64::MAX, 64);
        let request = self.constant(crate::simulator::native_io::NBA_PUBLISH, 64);
        let time = self.load(self.slots.now, 64);
        let first = self.constant(0, 64);
        let result = self.b.alloc_bit(64, false);
        self.b.emit(SIRInstruction::ExternCall {
            dst: Some(result),
            func: function,
            args: vec![context, process, request, time, first],
        });
        let ok = self.equals(result, 0);
        let next = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: ok,
            true_block: (done, Vec::new()),
            false_block: (next, Vec::new()),
        });
        self.b.switch_to_block(next);
        let update = self.equals(result, 2);
        self.b.seal_block(SIRTerminator::Branch {
            cond: update,
            true_block: (changed, Vec::new()),
            false_block: (failed, Vec::new()),
        });
        self.b.switch_to_block(changed);
        self.mark_dirty();
        self.check_waits_at(checks, false);
        self.b.seal_block(SIRTerminator::Jump(again, Vec::new()));
        self.b.switch_to_block(done);
    }

    fn service_call(
        &mut self,
        function: u32,
        process: u64,
        request: u64,
        time: RegisterId,
        failed: celox_sir::BlockId,
    ) {
        let context = self.load_at(
            self.slots.defer_limit_settle,
            celox_design::SCHEDULER_HOST_CONTEXT_BIT,
            64,
        );
        let process = self.constant(process, 64);
        let request = self.constant(request, 64);
        let first = self.constant(0, 64);
        let result = self.b.alloc_bit(64, false);
        self.b.emit(SIRInstruction::ExternCall {
            dst: Some(result),
            func: function,
            args: vec![context, process, request, time, first],
        });
        let ok = self.equals(result, 0);
        let next = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: ok,
            true_block: (next, Vec::new()),
            false_block: (failed, Vec::new()),
        });
        self.b.switch_to_block(next);
    }

    fn equals(&mut self, value: RegisterId, constant: u64) -> RegisterId {
        let width = self.b.register(&value).width();
        let constant = self.constant(constant, width);
        self.binary(1, value, BinaryOp::Eq, constant)
    }

    /// Whether the status in `status` is `expected`.
    fn equals_at(&mut self, status: AbsoluteAddr, expected: ProcessStatus) -> RegisterId {
        let status = self.load(status, celox_design::PROCESS_STATUS_WIDTH);
        self.equals(status, u64::from(expected.code()))
    }

    /// Whether the kernel of `status` suspended for a delay.
    fn delayed_at(&mut self, status: AbsoluteAddr) -> RegisterId {
        let status = self.load(status, celox_design::PROCESS_STATUS_WIDTH);
        let delay = self.equals(status, u64::from(ProcessStatus::Delay.code()));
        let inactive = self.equals(status, u64::from(ProcessStatus::InactiveDelay.code()));
        self.binary(1, delay, BinaryOp::Or, inactive)
    }

    /// The `from`-bit `value` zero-extended to `to` bits.
    fn widen_to(&mut self, value: RegisterId, from: usize, to: usize) -> RegisterId {
        let zero = self.constant(0, to - from);
        let wide = self.b.alloc_bit(to, false);
        self.b.emit(SIRInstruction::Concat(wide, vec![zero, value]));
        wide
    }

    /// Whether kernel `index`, with the status slot `status` when it also
    /// waits for edges, waits for its wake time.
    fn status_ready(
        &mut self,
        kernels: &[usize],
        ordinals: &[usize],
        schema: &celox_design::RuntimeSchema<AbsoluteAddr>,
        status: ProcessStatus,
    ) -> RegisterId {
        let mut ready = self.constant(0, self.ready_bits);
        for &index in kernels {
            let value = self.load(
                schema.processes[index].status,
                celox_design::PROCESS_STATUS_WIDTH,
            );
            let pending = self.equals(value, u64::from(status.code()));
            let bit = self.bit(pending, ordinals[index]);
            ready = self.binary(self.ready_bits, ready, BinaryOp::Or, bit);
        }
        ready
    }

    fn waiting_for_time(&mut self, index: usize, status: Option<AbsoluteAddr>) -> RegisterId {
        let waiting = self.waiting(index);
        match status {
            None => waiting,
            Some(status) => {
                let delayed = self.delayed_at(status);
                self.binary(1, waiting, BinaryOp::And, delayed)
            }
        }
    }

    /// Whether kernel `index` waits for its wake time, or for its edge.
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
        if let Some((_, blocks)) = &mut self.partitioned_glue {
            blocks.insert(self.b.current_block());
        }
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
        let selector = self.single(bit, width);
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

    fn settled_edge(&mut self, event: &Event, was: RegisterId, is: RegisterId) -> RegisterId {
        if event.any_settled {
            self.binary(1, was, BinaryOp::Ne, is)
        } else {
            self.edge(event, was, is)
        }
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
        let set = self.event_bit(value, bit);
        let values = self.binary(self.event_bits, levels.values, BinaryOp::Or, set);
        let unknown = levels.unknown.map(|levels| {
            let set = self.event_bit(unknown.expect("a four-state level"), bit);
            self.binary(self.event_bits, levels, BinaryOp::Or, set)
        });
        Levels { values, unknown }
    }

    /// `levels` with `level` in place of the one at bit `bit`.
    fn replace_level(&mut self, levels: Levels, level: RegisterId, bit: usize) -> Levels {
        let keep = self.all_but(bit, self.event_bits);
        let values = self.binary(self.event_bits, levels.values, BinaryOp::And, keep);
        let unknown = levels
            .unknown
            .map(|unknown| self.binary(self.event_bits, unknown, BinaryOp::And, keep));
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
        let values = self.constant(0, self.event_bits);
        let unknown = self
            .four_state
            .is_some()
            .then(|| self.constant(0, self.event_bits));
        Levels { values, unknown }
    }

    /// The levels of [`SchedulerSlots::levels`].
    fn load_levels(&mut self) -> Levels {
        let values = self.load(self.slots.levels, self.event_bits);
        let unknown = self
            .four_state
            .is_some()
            .then(|| self.load(self.slots.unknown_levels, self.event_bits));
        Levels { values, unknown }
    }

    fn store_levels(&mut self, levels: Levels) {
        self.store(self.slots.levels, self.event_bits, levels.values);
        if let Some(unknown) = levels.unknown {
            self.store(self.slots.unknown_levels, self.event_bits, unknown);
        }
    }

    /// Run `body` when bit `index` of `mask` is set.
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

    /// Bit `index` of a kernel or clock mask, set when `condition` holds.
    fn bit(&mut self, condition: RegisterId, index: usize) -> RegisterId {
        self.bit_of(condition, index, self.ready_bits)
    }

    /// Bit `index` of an event mask, set when `condition` holds.
    fn event_bit(&mut self, condition: RegisterId, index: usize) -> RegisterId {
        self.bit_of(condition, index, self.event_bits)
    }

    fn bit_of(&mut self, condition: RegisterId, index: usize, width: usize) -> RegisterId {
        let set = self.single(index, width);
        let unset = self.constant(0, width);
        let bit = self.b.alloc_bit(width, false);
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

    /// Store the baseline of kernel `index`, which suspended at the wait
    /// site in its `delay` slot, then continue at `target`: the level of
    /// the site's edge's signal now, with the edge's index when the kernel
    /// waits for several.
    fn register_wait(
        &mut self,
        index: usize,
        delay: AbsoluteAddr,
        wait: &Wait,
        target: celox_sir::BlockId,
        internal_exit: celox_sir::BlockId,
    ) {
        if !wait.alternatives.is_empty() {
            let site = self.load(delay, SCHEDULER_TIME_WIDTH);
            let blocks: Vec<_> = wait
                .alternatives
                .iter()
                .map(|_| self.b.new_block())
                .collect();
            let sites: Vec<_> = wait.site_terms.iter().map(|&index| blocks[index]).collect();
            self.dispatch_sites(site, &sites, internal_exit);
            for (alternative, block) in wait.alternatives.iter().zip(blocks) {
                self.b.switch_to_block(block);
                self.capture_raw_wait(index, alternative);
                self.b.seal_block(SIRTerminator::Jump(target, Vec::new()));
            }
            return;
        }
        if wait.raw.is_some() {
            self.capture_raw_wait(index, wait);
            self.b.seal_block(SIRTerminator::Jump(target, Vec::new()));
            return;
        }
        if let [(signal, _)] = wait.terms[..] {
            self.capture_compact_counter(index, wait.counters[0]);
            let level = self.signal_level(signal);
            let level = self.baseline_byte(level);
            self.store(self.slots.baseline[index], SCHEDULER_FLAG_WIDTH, level);
            self.b.seal_block(SIRTerminator::Jump(target, Vec::new()));
            return;
        }
        // The site index, which `Plan::new` bounds by a switch selector's
        // width.
        let site = self.load(delay, SITE_BITS);
        let blocks: Vec<celox_sir::BlockId> =
            wait.terms.iter().map(|_| self.b.new_block()).collect();
        let cases = wait
            .site_terms
            .iter()
            .enumerate()
            .map(|(site, &term)| SIRSwitchCase {
                value: site.into(),
                target: blocks[term],
            })
            .collect();
        self.b.seal_block(SIRTerminator::Switch {
            selector: site,
            cases,
            default: internal_exit,
        });
        for (term, (&(signal, _), block)) in wait.terms.iter().zip(blocks).enumerate() {
            self.b.switch_to_block(block);
            self.capture_compact_counter(index, wait.counters[term]);
            let level = self.signal_level(signal);
            let level = self.baseline_byte(level);
            let index_bits =
                self.constant((term as u64) << BASELINE_TERM_SHIFT, SCHEDULER_FLAG_WIDTH);
            let byte = self.binary(SCHEDULER_FLAG_WIDTH, level, BinaryOp::Or, index_bits);
            self.store(self.slots.baseline[index], SCHEDULER_FLAG_WIDTH, byte);
            self.b.seal_block(SIRTerminator::Jump(target, Vec::new()));
        }
    }

    /// The preceding check established every active wait's baseline. Between
    /// it and this boundary only this unit ran, apart from the time slots
    /// updated at slot entry. Host writes are observed before native entry.
    /// Opaque calls and host-service processes keep a full scan. Buffered
    /// event notifications do not change simulation state at this boundary;
    /// cyclic SCHEDULER_RUN calls only charge the work budget.
    fn check_waits_after_unit(
        &mut self,
        waits: &[WaitCheck],
        unit: &Unit,
        time_slots: &[AbsoluteAddr],
    ) {
        if instructions(unit).any(makes_extern_calls) {
            self.check_waits_at(waits, false);
            return;
        }
        let storage = |address| self.shared.get(&address).copied().unwrap_or(address);
        let writes: HashSet<_> = instructions(unit)
            .flat_map(written_storage)
            .filter(|write| write.region == STABLE_REGION)
            .map(|write| storage(write.absolute_addr()))
            .chain(time_slots.iter().copied().map(storage))
            .collect();
        let affected: Vec<_> = waits
            .iter()
            .copied()
            .filter(|check| {
                check
                    .wait
                    .terms
                    .iter()
                    .any(|(signal, _)| writes.contains(&storage(*signal)))
                    || check
                        .wait
                        .counters
                        .iter()
                        .flatten()
                        .any(|counter| writes.contains(&storage(*counter)))
            })
            .collect();
        tracing::debug!(target: "celox::scheduler", total=waits.len(), affected=affected.len(), "filtered post-kernel wait checks");
        self.check_waits_at(&affected, false);
    }

    /// Test the waits of `waits` against their signals now
    /// (`check_waits`): a wait whose edge occurred since its baseline wakes
    /// its kernel; every changed signal becomes the baseline.
    fn check_waits(&mut self, waits: &[WaitCheck]) {
        self.check_waits_at(waits, true);
    }

    fn check_waits_at(&mut self, waits: &[WaitCheck], settled: bool) {
        if waits.is_empty() {
            return;
        }
        let mut woken = self.constant(0, self.ready_bits);
        for check in waits {
            let wait = check.wait;
            let flag = self.waiting(check.index);
            let mut waiting = flag;
            if let Some(status) = check.status {
                let edge = self.equals_at(status, ProcessStatus::Wait);
                let pending = self.equals_at(status, ProcessStatus::Pending);
                let edge = self.binary(1, edge, BinaryOp::Or, pending);
                waiting = self.binary(1, waiting, BinaryOp::And, edge);
            }
            let wake = if !wait.alternatives.is_empty() {
                self.check_wait_site(check.index, check.delay, wait, waiting, settled)
            } else if wait.raw.is_some() {
                self.check_raw_wait(check.index, wait, waiting, settled)
            } else {
                let baseline = self.load(self.slots.baseline[check.index], SCHEDULER_FLAG_WIDTH);
                let level_width = if self.four_state.is_none() { 1 } else { 2 };
                // With several edges, the byte holds the edge's index above the
                // level (`BASELINE_TERM_SHIFT`).
                let (stored, term) = if wait.terms.len() == 1 {
                    (baseline, None)
                } else {
                    let level = self.b.alloc_bit(level_width, false);
                    self.b
                        .emit(SIRInstruction::Slice(level, baseline, 0, level_width));
                    let level = self.widen_to(level, level_width, SCHEDULER_FLAG_WIDTH);
                    let shift = self.constant(u64::from(BASELINE_TERM_SHIFT), SCHEDULER_FLAG_WIDTH);
                    let term = self.binary(SCHEDULER_FLAG_WIDTH, baseline, BinaryOp::Shr, shift);
                    (level, Some(term))
                };
                let mut occurred = self.constant(0, 1);
                let mut current = None;
                let mut levels: Vec<(AbsoluteAddr, RegisterId)> = Vec::new();
                for (ordinal, &(signal, kind)) in wait.terms.iter().enumerate() {
                    let is = match levels.iter().find(|(known, _)| *known == signal) {
                        Some(&(_, level)) => level,
                        None => {
                            let level = self.signal_level(signal);
                            levels.push((signal, level));
                            level
                        }
                    };
                    let mut edge = self.occurred(stored, is, kind);
                    if let Some(counter) = wait.counters[ordinal] {
                        let previous = self.load_at(
                            self.slots.baseline[check.index],
                            SCHEDULER_FLAG_WIDTH,
                            32,
                        );
                        let current = self.load(counter, 32);
                        let counted = self.binary(1, previous, BinaryOp::Ne, current);
                        edge = self.binary(1, edge, BinaryOp::Or, counted);
                    }
                    let (edge, is) = match term {
                        None => (edge, is),
                        Some(term) => {
                            let selected = self.equals(term, ordinal as u64);
                            let edge = self.binary(1, edge, BinaryOp::And, selected);
                            let is = match current {
                                None => is,
                                Some(other) => {
                                    let level = self.b.alloc_bit(level_width, false);
                                    self.b.emit(SIRInstruction::Mux(level, selected, is, other));
                                    level
                                }
                            };
                            (edge, is)
                        }
                    };
                    occurred = self.binary(1, occurred, BinaryOp::Or, edge);
                    current = Some(is);
                }
                let is = current.expect("a wait has an edge");
                let wake = self.binary(1, occurred, BinaryOp::And, waiting);
                let mut level = self.baseline_byte(is);
                if term.is_some() {
                    // Keep the edge's index.
                    let mask = self.constant(
                        (1 << SCHEDULER_FLAG_WIDTH) - (1 << BASELINE_TERM_SHIFT),
                        SCHEDULER_FLAG_WIDTH,
                    );
                    let index = self.binary(SCHEDULER_FLAG_WIDTH, baseline, BinaryOp::And, mask);
                    level = self.binary(SCHEDULER_FLAG_WIDTH, level, BinaryOp::Or, index);
                }
                let next = self.b.alloc_bit(SCHEDULER_FLAG_WIDTH, false);
                self.b
                    .emit(SIRInstruction::Mux(next, waiting, level, baseline));
                self.store(self.slots.baseline[check.index], SCHEDULER_FLAG_WIDTH, next);
                wake
            };
            let one = self.constant(1, 1);
            let stays = self.binary(1, wake, BinaryOp::Xor, one);
            let still = self.binary(1, flag, BinaryOp::And, stays);
            let flag = self.widen(still, SCHEDULER_FLAG_WIDTH);
            self.store(self.slots.waiting[check.index], SCHEDULER_FLAG_WIDTH, flag);
            let bit = self.bit(wake, check.ordinal);
            woken = self.binary(self.ready_bits, woken, BinaryOp::Or, bit);
        }
        let ready = self.load(self.slots.ready, self.ready_bits);
        let ready = self.binary(self.ready_bits, ready, BinaryOp::Or, woken);
        self.store(self.slots.ready, self.ready_bits, ready);
    }

    /// Whether the edge `kind` occurred from the level of the baseline byte
    /// `baseline`, its edge index cleared, to the level `is`.
    fn occurred(&mut self, baseline: RegisterId, is: RegisterId, kind: CounterKind) -> RegisterId {
        if self.four_state.is_none() {
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
        }
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
        self.inline_writer_with_exit(unit, Some(finish_exit));
    }

    // An execution phase can contain several SIR units. The backend runs
    // all of them before the reference loop observes `$finish`; preserve
    // that boundary, including blocking writes in later units.
    fn inline_writers(&mut self, units: &[Unit], finish_exit: celox_sir::BlockId) {
        for unit in units {
            self.inline_writer_with_exit(unit, None);
        }
        if units.iter().any(|unit| self.unit_finishes(unit)) {
            self.check_finish(finish_exit);
        }
    }

    fn inline_writer_with_exit(&mut self, unit: &Unit, finish_exit: Option<celox_sir::BlockId>) {
        if self.partitioned_glue.is_some() {
            // Partition instrumentation already compares every external
            // write to tracked state and publishes that same predicate to
            // slots.dirty. Preserve intermediate changes as well as X/Z-only
            // changes; do not snapshot its notification flags again.
            self.inline_with_exit(unit, finish_exit);
            // The instrumentation marks after a call's writes as well.
            if instructions(unit).any(makes_extern_calls) {
                self.mark_dirty();
            }
            return;
        }
        // Keep small stable write sets precise. Reading a bounded number of
        // words is much cheaper than repeating the whole comb settle when a
        // process merely rewrites the value already present in its outputs.
        let ranges = self.tracked_writer_ranges(unit);
        let before = ranges.as_ref().map(|ranges| {
            ranges
                .iter()
                .map(|&(address, offset, width)| self.load_state(address, offset, width))
                .collect::<Vec<_>>()
        });
        self.inline_with_exit(unit, finish_exit);
        if let (Some(ranges), Some(before)) = (ranges, before) {
            if !ranges.is_empty() {
                let clock_only = self.incremental.is_some_and(|cone| {
                    ranges.iter().all(|&(address, _, _)| {
                        cone.sources
                            .contains(&self.shared.get(&address).copied().unwrap_or(address))
                    })
                });
                let mut dirty = self.load(self.slots.dirty, SCHEDULER_FLAG_WIDTH);
                for ((address, offset, width), before) in ranges.into_iter().zip(before) {
                    let after = self.load_state(address, offset, width);
                    // Case inequality compares both planes, including X/Z-only
                    // changes, and always produces a known boolean.
                    let changed = self.binary(1, before, BinaryOp::NeCase, after);
                    let changed = if clock_only {
                        let two = self.constant(2, SCHEDULER_FLAG_WIDTH);
                        let zero = self.constant(0, SCHEDULER_FLAG_WIDTH);
                        let flag = self.b.alloc_bit(SCHEDULER_FLAG_WIDTH, false);
                        self.b.emit(SIRInstruction::Mux(flag, changed, two, zero));
                        flag
                    } else {
                        changed
                    };
                    dirty = self.binary(SCHEDULER_FLAG_WIDTH, dirty, BinaryOp::Or, changed);
                }
                self.store(self.slots.dirty, SCHEDULER_FLAG_WIDTH, dirty);
            }
        } else if instructions(unit).any(|inst| {
            makes_extern_calls(inst)
                || written_storage(inst).any(|target| self.settle_reads(target.absolute_addr()))
        }) {
            self.mark_dirty();
        }
    }

    /// Precise snapshots are bounded independently of design size. Complex
    /// addressing and large FF domains retain the existing conservative flag.
    fn tracked_writer_ranges(&self, unit: &Unit) -> Option<Vec<(AbsoluteAddr, usize, usize)>> {
        const MAX_BITS: usize = 1024;
        const MAX_RANGES: usize = 32;
        let mut ranges = Vec::new();
        let mut bits = 0;
        for instruction in instructions(unit) {
            // An opaque call can change state outside explicit Stores.
            if makes_extern_calls(instruction) {
                return None;
            }
            let (target, offset, width) = match instruction.state_writes() {
                StateAccess::Access(access) if self.settle_reads(access.addr.absolute_addr()) => {
                    (access.addr, access.offset, access.width)
                }
                // A call may write any part of these objects, which the
                // bounded snapshots do not cover.
                StateAccess::Objects(objects)
                    if objects
                        .iter()
                        .any(|target| self.settle_reads(target.absolute_addr())) =>
                {
                    return None;
                }
                _ => continue,
            };
            if target.region != STABLE_REGION {
                return None;
            }
            let SIROffset::Static(start) = offset else {
                return None;
            };
            if width > MAX_BITS {
                return None;
            }
            let mut offset = 0;
            while offset < width {
                let width = (width - offset).min(64);
                let range = (target.absolute_addr(), start + offset, width);
                if !ranges.contains(&range) {
                    bits += width;
                    if bits > MAX_BITS || ranges.len() == MAX_RANGES {
                        return None;
                    }
                    ranges.push(range);
                }
                offset += width;
            }
        }
        Some(ranges)
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
        let mut unit = Unit {
            entry_block_id: celox_sir::BlockId(0),
            blocks,
            register_map,
        };
        if let Some((program, blocks)) = self.partitioned_glue {
            crate::settle::instrument_generated_glue(program, &mut unit, &blocks);
        }
        unit
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
        let value = self.constant(case as u64 + 1, self.cont_bits);
        self.store(self.slots.cont, self.cont_bits, value);
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
            self.inline_with_exit(unit, None);
        }
        if eager.iter().any(|unit| self.unit_finishes(unit)) {
            self.check_finish(finish_exit);
        }
        self.b.seal_block(SIRTerminator::Jump(done, Vec::new()));
        self.b.switch_to_block(done);
        let targets = self.eager_continuations.clone();
        self.dispatch_continuations(&targets, internal_exit);
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
        let value = self.constant(case as u64 + 1, self.cont_bits);
        self.store(self.slots.cont, self.cont_bits, value);
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
        self.observers_before(internal_exit);
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
            if let Some(cone) = self.incremental {
                let partial = self.b.new_block();
                let full = self.b.new_block();
                let done = self.b.new_block();
                let only_clock = self.equals(dirty, 2);
                self.b.seal_block(SIRTerminator::Branch {
                    cond: only_clock,
                    true_block: (partial, Vec::new()),
                    false_block: (full, Vec::new()),
                });
                self.b.switch_to_block(partial);
                for unit in &cone.units {
                    self.inline_with_exit(unit, None);
                }
                self.b.seal_block(SIRTerminator::Jump(done, Vec::new()));
                self.b.switch_to_block(full);
                for unit in units {
                    self.inline_with_exit(unit, None);
                }
                self.b.seal_block(SIRTerminator::Jump(done, Vec::new()));
                self.b.switch_to_block(done);
            } else {
                for unit in units {
                    self.inline_with_exit(unit, None);
                }
            }
            let clean = self.constant(0, SCHEDULER_FLAG_WIDTH);
            self.store(self.slots.dirty, SCHEDULER_FLAG_WIDTH, clean);
            self.b.seal_block(SIRTerminator::Jump(settled, Vec::new()));
            self.b.switch_to_block(settled);
        } else {
            for unit in units {
                self.inline_with_exit(unit, None);
            }
        }
        self.observers_after(internal_exit);
        // Match the backend's phase boundary: a captured finish must not
        // skip later combinational writes, captures or observer snapshots.
        if units.iter().any(|unit| self.unit_finishes(unit))
            || self
                .incremental
                .is_some_and(|cone| cone.units.iter().any(|unit| self.unit_finishes(unit)))
        {
            self.check_finish(finish_exit);
        }
        let targets = self.continuations.clone();
        self.dispatch_continuations(&targets, internal_exit);
    }

    /// SIR switches have byte-sized selectors. Dispatch larger continuation
    /// numbers a byte at a time, retaining one switch for small designs.
    fn dispatch_continuations(
        &mut self,
        targets: &[celox_sir::BlockId],
        default: celox_sir::BlockId,
    ) {
        let cases: Vec<_> = targets
            .iter()
            .copied()
            .enumerate()
            .map(|(i, target)| (i + 1, target))
            .collect();
        let bits = usize::BITS - targets.len().leading_zeros();
        let shift = bits.max(1).div_ceil(8) as usize * 8 - 8;
        self.dispatch_continuation_byte(&cases, shift, default);
    }

    fn dispatch_continuation_byte(
        &mut self,
        targets: &[(usize, celox_sir::BlockId)],
        shift: usize,
        default: celox_sir::BlockId,
    ) {
        let selector = self.load_at(self.slots.cont, shift, 8);
        let mut cases = Vec::new();
        let mut children = Vec::new();
        let mut start = 0;
        while start < targets.len() {
            let value = (targets[start].0 >> shift) & 255;
            let end = start
                + targets[start..].partition_point(|&(index, _)| ((index >> shift) & 255) == value);
            let target = if shift == 0 {
                targets[start].1
            } else {
                self.b.new_block()
            };
            cases.push(SIRSwitchCase {
                value: value.into(),
                target,
            });
            if shift > 0 {
                children.push((target, start, end));
            }
            start = end;
        }
        self.b.seal_block(SIRTerminator::Switch {
            selector,
            cases,
            default,
        });
        for (block, start, end) in children {
            self.b.switch_to_block(block);
            self.dispatch_continuation_byte(&targets[start..end], shift - 8, default);
        }
    }

    /// Inline a complete process, checking its finish record after it runs.
    fn inline(&mut self, unit: &Unit, finish_exit: celox_sir::BlockId) {
        self.inline_with_exit(unit, Some(finish_exit));
    }

    fn unit_finishes(&self, unit: &Unit) -> bool {
        !self.finish_sites.is_empty()
            && (emits_finish(unit, self.finish_sites)
                || self.observers.is_some()
                    && instructions(unit).any(|inst| matches!(inst,
                        SIRInstruction::CombCaptureEvent { site_id, .. } if self.finish_sites.contains(site_id))))
    }

    fn inline_with_exit(&mut self, unit: &Unit, finish_exit: Option<celox_sir::BlockId>) {
        let captures = self.observers.is_some();
        let finishes = self.unit_finishes(unit);
        let counts = self.event_batch.is_some()
            && instructions(unit).any(|inst| {
                matches!(inst, SIRInstruction::RuntimeEvent { .. })
                    || captures && matches!(inst, SIRInstruction::CombCaptureEvent { .. })
            });
        if !finishes && !counts {
            self.inline_body(unit);
            return;
        }
        let instrumented = self.instrument(unit, counts);
        self.inline_body(&instrumented);
        if finishes && let Some(finish_exit) = finish_exit {
            self.check_finish(finish_exit);
        }
    }

    fn check_finish(&mut self, finish_exit: celox_sir::BlockId) {
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

    fn inline_body(&mut self, unit: &Unit) {
        if let Some(outlined) = &mut self.outlined
            && instructions(unit).take(32_769).count() > 32_768
        {
            let index = match outlined.iter().position(|previous| previous == unit) {
                Some(index) => index,
                None => {
                    outlined.push(unit.clone());
                    outlined.len() - 1
                }
            };
            self.b.emit(SIRInstruction::ExternCall {
                dst: None,
                func: celox_sir::extern_abi::NATIVE_KERNEL_CALL_TAG
                    | (self.outline_base + index as u32),
                args: Vec::new(),
            });
        } else {
            self.b.inline_execution_unit(unit);
        }
    }

    /// A copy of `unit` that sets the finish slot after every `$finish`
    /// record, and when `counts`, counts every record in the events slot.
    fn instrument(&self, unit: &Unit, counts: bool) -> Unit {
        Self::instrument_records(
            unit,
            self.finish_sites,
            self.slots,
            counts,
            self.observers.is_some(),
        )
    }

    /// [`Self::instrument`] of `unit`, for a scheduler of `slots` whose
    /// `$finish` records are `finish_sites`.
    fn instrument_records(
        unit: &Unit,
        finish_sites: &HashSet<u32>,
        slots: &SchedulerSlots<AbsoluteAddr>,
        counts: bool,
        captures: bool,
    ) -> Unit {
        let mut unit = unit.clone();
        let mut next = unit
            .register_map
            .keys()
            .map(|register| register.0 + 1)
            .max()
            .unwrap_or(0);
        let mut registers = Vec::new();
        let mut wide = Vec::new();
        for block in unit.blocks.values_mut() {
            let mut instructions = Vec::with_capacity(block.instructions.len());
            for inst in block.instructions.drain(..) {
                let capture = match &inst {
                    SIRInstruction::CombCaptureEvent { site_id, .. } if captures => Some(*site_id),
                    _ => None,
                };
                let enabled = capture.map(|site| {
                    use celox_sir::native_memory::NativeMemoryOp as Memory;
                    let [header, pointer, offset, word, shift, shifted, mask, enabled] =
                        std::array::from_fn(|i| RegisterId(next + i));
                    next += 8;
                    wide.extend([header, pointer, offset, word, shift, shifted, mask, enabled]);
                    instructions.extend([
                        SIRInstruction::Imm(
                            header,
                            SIRValue::new(
                                celox_state_layout::STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET
                                    as u64,
                            ),
                        ),
                        SIRInstruction::ExternCall {
                            dst: Some(pointer),
                            func: Memory::ReadState.function(),
                            args: vec![header],
                        },
                        SIRInstruction::Imm(offset, SIRValue::new(u64::from(site / 8) * 8)),
                        SIRInstruction::ExternCall {
                            dst: Some(word),
                            func: Memory::ReadIndirect.function(),
                            args: vec![pointer, offset],
                        },
                        SIRInstruction::Imm(shift, SIRValue::new(u64::from(site % 8) * 8)),
                        SIRInstruction::Binary(shifted, word, BinaryOp::Shr, shift),
                        SIRInstruction::Imm(mask, SIRValue::new(255u64)),
                        SIRInstruction::Binary(enabled, shifted, BinaryOp::And, mask),
                    ]);
                    enabled
                });
                let finish = matches!(&inst, SIRInstruction::RuntimeEvent { site_id, .. }
                    if finish_sites.contains(site_id))
                    || capture.is_some_and(|site| finish_sites.contains(&site));
                let record =
                    matches!(&inst, SIRInstruction::RuntimeEvent { .. }) || capture.is_some();
                instructions.push(inst);
                if counts && record {
                    let [count, one, sum] = std::array::from_fn(|index| RegisterId(next + index));
                    next += 3;
                    wide.extend([count, one, sum]);
                    let events = stable(slots.events);
                    instructions.extend([
                        SIRInstruction::Load(
                            count,
                            events,
                            SIROffset::Static(0),
                            SCHEDULER_TIME_WIDTH,
                        ),
                        SIRInstruction::Imm(one, SIRValue::new(1u64)),
                        SIRInstruction::Binary(sum, count, BinaryOp::Add, enabled.unwrap_or(one)),
                        SIRInstruction::Store(
                            events,
                            SIROffset::Static(0),
                            SCHEDULER_TIME_WIDTH,
                            sum,
                            Vec::new(),
                            Vec::new(),
                        ),
                    ]);
                }
                if finish {
                    let [one, previous, result] = std::array::from_fn(|i| RegisterId(next + i));
                    next += 3;
                    registers.extend([one, previous, result]);
                    instructions.push(match enabled {
                        Some(enabled) => {
                            SIRInstruction::Slice(one, enabled, 0, SCHEDULER_FLAG_WIDTH)
                        }
                        None => SIRInstruction::Imm(one, SIRValue::new(1u8)),
                    });
                    instructions.extend([
                        SIRInstruction::Load(
                            previous,
                            stable(slots.finish),
                            SIROffset::Static(0),
                            SCHEDULER_FLAG_WIDTH,
                        ),
                        SIRInstruction::Binary(result, previous, BinaryOp::Or, one),
                        SIRInstruction::Store(
                            stable(slots.finish),
                            SIROffset::Static(0),
                            SCHEDULER_FLAG_WIDTH,
                            result,
                            Vec::new(),
                            Vec::new(),
                        ),
                    ]);
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
        for register in wide {
            unit.register_map.insert(
                register,
                crate::ir::RegisterType::Bit {
                    width: SCHEDULER_TIME_WIDTH,
                    signed: false,
                },
            );
        }
        unit
    }
}

/// A kernel's wait as `Generator::check_waits` tests it.
#[derive(Clone, Copy)]
struct WaitCheck<'a> {
    index: usize,
    delay: AbsoluteAddr,
    /// Its bit in the ready mask.
    ordinal: usize,
    wait: &'a Wait,
    /// For a kernel that also delays, its status slot, which tells whether
    /// it waits for its edge.
    status: Option<AbsoluteAddr>,
}

/// The waits of the waiting kernels of `kernels`.
fn waits_of<'a>(
    kernels: &'a [Option<Kernel>],
    ordinals: &[usize],
    schema: &celox_design::RuntimeSchema<AbsoluteAddr>,
) -> Vec<WaitCheck<'a>> {
    kernels
        .iter()
        .enumerate()
        .filter_map(|(index, kernel)| {
            let kernel = kernel.as_ref()?;
            Some(WaitCheck {
                index,
                delay: schema.processes[index].delay,
                ordinal: ordinals[index],
                wait: kernel.wait.as_ref()?,
                status: kernel.delays.then_some(schema.processes[index].status),
            })
        })
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
    instructions(unit).flat_map(loaded_addresses)
}

fn loaded_addresses(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
) -> impl Iterator<Item = AbsoluteAddr> + '_ {
    read_storage(inst).map(RegionedAbsoluteAddr::absolute_addr)
}

/// Whether a settle of `units` may read what an earlier settle left: a load
/// of storage they write (`stores`, by representative) that stores earlier
/// in the same settle do not cover on every path.
fn retains_state(
    units: &[Unit],
    storage: &impl Fn(AbsoluteAddr) -> AbsoluteAddr,
    stores: &HashSet<AbsoluteAddr>,
) -> bool {
    !retained_storage(units, storage, stores).is_empty()
}

fn retained_storage(
    units: &[Unit],
    storage: &impl Fn(AbsoluteAddr) -> AbsoluteAddr,
    stores: &HashSet<AbsoluteAddr>,
) -> HashSet<AbsoluteAddr> {
    /// The bits of each home a settle has stored so far, as sorted,
    /// disjoint `[start, end)` ranges.
    type Covered = HashMap<(u32, AbsoluteAddr), Vec<(usize, usize)>>;
    let mut retained = HashSet::default();
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
    // Record every storage whose entry value can reach a read before a
    // covering write. Keep scanning so independent homes are analyzed once.
    let mut step = |block: &celox_sir::BasicBlock<RegionedAbsoluteAddr>, covered: &mut Covered| {
        for inst in &block.instructions {
            // A call may read any part of what it reads before anything in
            // the settle stored it, and what it writes it need not write on
            // every path, so it covers nothing.
            if let StateAccess::Objects(sources) = inst.state_reads() {
                retained.extend(
                    sources
                        .iter()
                        .map(|source| storage(source.absolute_addr()))
                        .filter(|home| stores.contains(home)),
                );
            }
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
                            retained.insert(home);
                        }
                    }
                }
                _ => {}
            }
            match inst {
                SIRInstruction::Store(target, offset, width, ..)
                | SIRInstruction::Commit(_, target, offset, width, _) => {
                    let home = storage(target.absolute_addr());
                    if stores.contains(&home)
                        && let Some(start) = offset.constant_bit_offset()
                    {
                        add(covered, (target.region, home), start, start + width);
                    }
                }
                _ => {}
            }
        }
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
                step(&unit.blocks[&id], &mut covered);
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
    retained
}

/// Whether `unit` may end with an error: at one of its own error exits, or
/// at a call whose callee may fail and ends it with the callee's error.
fn fails(unit: &Unit) -> bool {
    unit.blocks
        .values()
        .any(|block| matches!(block.terminator, SIRTerminator::Error(_)))
        || instructions(unit).any(SIRInstruction::may_fail)
}

/// The signals `statement` writes that may be reset events: those of
/// `rst.assert` and of assignments.
fn testbench_resets(
    statement: &celox_testbench::SemanticStatement<AbsoluteAddr>,
) -> Vec<AbsoluteAddr> {
    use celox_testbench::SemanticStatement as S;
    match statement {
        S::ResetAssert {
            reset_event: Some(event),
            ..
        } => vec![*event],
        S::Assign { dst, .. } => vec![dst.signal.address],
        S::If {
            then_block,
            else_block,
            ..
        } => then_block
            .iter()
            .chain(else_block)
            .flat_map(testbench_resets)
            .collect(),
        S::For { body, .. } => body.iter().flat_map(testbench_resets).collect(),
        _ => vec![],
    }
}

/// Whether `unit` may record a `$finish`, one of `finish_sites`.
fn emits_finish(unit: &Unit, finish_sites: &HashSet<u32>) -> bool {
    instructions(unit).any(|inst| {
        matches!(inst, SIRInstruction::RuntimeEvent { site_id, .. }
            if finish_sites.contains(site_id))
            || !finish_sites.is_empty() && calls_runtime_events(inst)
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
            // A call may store any delay.
            _ if inst.call_effects().is_some() && inst.state_writes().touches(&delay) => {
                Some(u64::MAX)
            }
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

/// The storage `inst` may read: a Load's or Commit's source, or every object
/// a call's effects read, each of which it may read in any part.
/// [`Plan::new`] rejects units that may read any state (a native kernel),
/// the one case no list names, so none is left out here.
fn read_storage(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
) -> impl Iterator<Item = &RegionedAbsoluteAddr> {
    inst.state_reads().objects().into_iter().flatten()
}

/// The storage `inst` may write: a Store's or Commit's destination, or
/// every object a call's effects write (see [`read_storage`]).
fn written_storage(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
) -> impl Iterator<Item = &RegionedAbsoluteAddr> {
    inst.state_writes().objects().into_iter().flatten()
}

/// Whether `inst` makes an external call, itself or in a function it calls.
/// The scheduler treats an external call as opaque: it may change state the
/// SIR does not show.
fn makes_extern_calls(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    inst.host_effects().extern_calls
}

/// Whether `inst` is a call whose callee may record runtime events, and so
/// may record a `$finish`, which the callee flags itself
/// (`instrument_recording_functions`).
fn calls_runtime_events(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    inst.call_effects().is_some() && inst.host_effects().runtime_events
}

fn yielding_status(unit: &Unit, status: AbsoluteAddr) -> bool {
    returns_status(unit, status, ProcessStatus::Yield)
}

fn returns_status(unit: &Unit, status: AbsoluteAddr, wanted: ProcessStatus) -> bool {
    let constants: HashMap<RegisterId, u64> = instructions(unit)
        .filter_map(|inst| match inst {
            SIRInstruction::Imm(register, value) => u64::try_from(&value.payload)
                .ok()
                .map(|value| (*register, value)),
            _ => None,
        })
        .collect();
    instructions(unit).any(|inst| {
        matches!(inst,
            SIRInstruction::Store(target, SIROffset::Static(0), _, value, ..)
                if *target == stable(status) && constants.get(value) == Some(&u64::from(wanted.code()))
        )
            // A call may store any status.
            || inst.call_effects().is_some() && inst.state_writes().touches(&stable(status))
    })
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
        // A call may store any status.
        _ if inst.call_effects().is_some() => !inst.state_writes().touches(&status),
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

/// What [`Plan::new`] decides for `program` that tests of call effects
/// check: the storage the settle reads, why each kernel falls back, and the
/// events RTL logic drives (`Plan::derived`).
#[cfg(all(test, feature = "systemverilog"))]
pub(crate) struct PlanSummary {
    pub(crate) settle_reads: Option<HashSet<AbsoluteAddr>>,
    pub(crate) fallback: Vec<Option<String>>,
    pub(crate) derived: Vec<AbsoluteAddr>,
    /// With deferred settles, per kernel whether its runs observe the
    /// settled state (`Lazy::kernels`).
    pub(crate) lazy: Option<Vec<bool>>,
    pub(crate) incremental: bool,
}

#[cfg(all(test, feature = "systemverilog"))]
pub(crate) fn plan_for_test(program: &LaidOutProgram) -> Result<PlanSummary, String> {
    let schema = program.runtime.runtime_schema.scheduler.clone().unwrap();
    let plan = Plan::new(program, &schema.slots, true)?;
    Ok(PlanSummary {
        derived: plan
            .derived
            .iter()
            .map(|&event| plan.events[event].signal)
            .collect(),
        lazy: plan.lazy.as_ref().map(|lazy| lazy.kernels.clone()),
        incremental: plan.incremental.is_some(),
        settle_reads: plan.settle_reads,
        fallback: plan.fallback,
    })
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
        .flat_map(written_storage)
        .map(|write| storage(write.absolute_addr()))
        .collect();
    (
        plan.lazy.is_some(),
        !retains_state(&program.sir.eval_comb, &storage, &stores),
    )
}

#[cfg(all(
    test,
    feature = "systemverilog",
    feature = "host-runtime",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub(crate) fn incremental_inputs_for_test(program: &LaidOutProgram) -> Option<usize> {
    let schema = program.runtime.runtime_schema.scheduler.as_ref().unwrap();
    Plan::new(program, &schema.slots, true)
        .unwrap()
        .incremental
        .map(|cone| cone.sources.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiler_clock_hints_preserve_the_image_encoding() {
        #[derive(serde::Serialize)]
        struct LegacyClock {
            clock: u32,
            gate: Option<Vec<celox_design::GateOp<u32>>>,
        }
        let legacy = LegacyClock {
            clock: 1,
            gate: None,
        };
        let clock = celox_design::CombDerivedClock {
            clock: legacy.clock,
            gate: legacy.gate.clone(),
            inputs: Some(vec![1, 2, 3]),
        };
        let bytes = postcard::to_allocvec(&clock).unwrap();
        assert_eq!(bytes, postcard::to_allocvec(&legacy).unwrap());
        let decoded: celox_design::CombDerivedClock<u32> = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.clock, clock.clock);
        assert!(decoded.inputs.is_none());
    }

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

#[cfg(all(test, feature = "systemverilog"))]
mod finish_phase_tests {
    fn check<B: crate::SimBackend>(backend: &str, mut sim: crate::Simulation<B>) {
        sim.run_until(100)
            .unwrap_or_else(|error| panic!("{backend}: {error}"));
        assert!(sim.is_finished(), "{backend}");
        assert_eq!(sim.time(), 14, "{backend}");
        let stats = sim.scheduler_stats();
        assert!(stats.compiled_slots > 0, "{backend}: {stats:?}");
        assert!(stats.checked_steps > 0, "{backend}: {stats:?}");
        assert!(stats.unchecked.is_none(), "{backend}: {stats:?}");
    }

    // Compare the final memory image and runtime events, including blocking
    // writes in later FF partitions and NBAs that must remain unpublished.
    #[test]
    fn finish_observes_the_complete_ff_evaluation_phase() {
        for four_state in [false, true] {
            for source in [
                r#"`timescale 1ns/1ns
module Top;
 bit clk=0; int count=0;
 always #2 clk=~clk;
 always @(posedge clk) count<=count+1;
 always @(posedge clk) if(count==3) $finish;
 final $display("FINAL count=%0d",count);
endmodule
"#,
                r#"`timescale 1ns/1ns
module Top;
 bit clk=0;
 int count=0;
 int cycles=0;
 always #2 clk=~clk;
 always @(posedge clk) count <= count + 1;
 always @(posedge clk) cycles++;
 always @(posedge clk) if (count==3) begin
   $display("FINISH count=%0d cycles=%0d",count,cycles);
   $finish;
 end
 final $display("FINAL count=%0d cycles=%0d",count,cycles);
endmodule
"#,
                r#"`timescale 1ns/1ns
module Utils;
 longint unsigned cycles=0;
 always @(posedge $root.Top.clk) ++cycles;
 final $display("FINAL cycles=%0d",cycles);
endmodule
module Top;
 Utils u();
 bit clk=0;
 bit en=0; initial begin #1 en=1; #30 en=0; end
 wire hfclk=clk & en;
 int count=0; int active=0; int seen=0;
 always @(posedge hfclk) active = active+1;
 initial forever @(active) seen++;
 always #2 clk=~clk;
 always @(posedge hfclk) count <= count + 1;
 always @(posedge hfclk) if (count==3) begin
   $display("FINISH count=%0d",count);
   $finish;
 end
endmodule
"#,
            ] {
                let builder = || {
                    crate::Simulation::from_sv_sources(
                        vec![(source, std::path::Path::new("finish-phase.sv"))],
                        "Top",
                    )
                    .four_state(four_state)
                    .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
                };
                check("native", builder().build().unwrap());
                #[cfg(feature = "cranelift")]
                check("cranelift", builder().build_cranelift().unwrap());
                #[cfg(feature = "wasm-runtime")]
                check("wasm", builder().build_wasm().unwrap());
            }
        }
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod wide_event_tests {
    use num_bigint::BigUint;

    const CLOCKS: usize = 70;
    const PERIODS: usize = 7;
    const END: u64 = 100;

    /// 70 clocks, each clocking a counter. One process per period toggles
    /// its clocks, so the events exceed a word while the kernels stay few.
    fn more_than_64_events() -> String {
        let mut source = String::from("`timescale 1ns/1ns\nmodule Top;\n");
        for index in 0..CLOCKS {
            source += &format!(
                "  logic c{index} = 0;\n  logic [7:0] n{index} = 0;\n  \
                 always_ff @(posedge c{index}) n{index} <= n{index} + 8'd1;\n"
            );
        }
        for period in 1..=PERIODS {
            let toggles: String = (0..CLOCKS)
                .filter(|index| index % PERIODS + 1 == period)
                .map(|index| format!(" c{index} = ~c{index};"))
                .collect();
            source += &format!("  always #{period} begin{toggles} end\n");
        }
        source + "endmodule\n"
    }

    fn builder(source: &str) -> crate::SimulatorBuilder<'_, crate::Simulation> {
        crate::Simulation::from_sv_sources(vec![(source, std::path::Path::new("events.sv"))], "Top")
    }

    /// Run to `END` and check every counter against its clock's edges;
    /// returns the slots the compiled scheduler ran.
    fn counts_every_edge<B: crate::SimBackend>(
        backend: &str,
        mut sim: crate::Simulation<B>,
    ) -> u64 {
        sim.run_until(END).unwrap();
        for index in 0..CLOCKS {
            // A clock with half period `p` rises at p, 3p, 5p, ...
            let period = (index % PERIODS + 1) as u64;
            let rises = (END / period).div_ceil(2);
            let counter = sim.signal(&format!("n{index}"));
            assert_eq!(
                sim.get(counter),
                BigUint::from(rises),
                "{backend}: n{index}"
            );
        }
        sim.scheduler_stats().compiled_slots
    }

    /// A design with more events than one word of a mask holds runs in
    /// the compiled scheduler, and every domain sees each of its edges.
    #[test]
    fn more_than_64_events_run_in_the_compiled_scheduler() {
        let source = more_than_64_events();
        let mut sim = builder(&source).build().unwrap();
        let scheduler = sim
            .simulator
            .program
            .runtime_schema
            .scheduler
            .as_ref()
            .unwrap();
        assert!(scheduler.unit.is_some(), "{:?}", scheduler.fallback);
        assert!(scheduler.events.len() > 64);
        assert!(scheduler.fallback.iter().all(Option::is_none));
        sim.run_until(1).unwrap();
        assert!(sim.scheduler_stats().compiled_slots > 0);
        counts_every_edge("native", sim);
        // Every step of the compiled scheduler agrees with the reference
        // loop.
        let mut checked = builder(&source)
            .scheduler_mode(crate::SchedulerMode::Differential)
            .build()
            .unwrap();
        checked.run_until(END).unwrap();
        assert!(checked.scheduler_stats().checked_steps > 0);
    }

    /// 70 instances on one clock: the frontend's estimate counts the clock
    /// once per instance and widens the scheduler's slots, while the design
    /// has few events. A `$finish` ends a compiled slot early.
    #[test]
    fn wide_slots_with_few_events_finish_on_every_backend() {
        let mut source = String::from(
            "`timescale 1ns/1ns\nmodule Counter(input logic clk, output logic [7:0] n);\n  \
             initial n = 0;\n  always_ff @(posedge clk) n <= n + 8'd1;\nendmodule\n\
             module Top;\n  logic clk = 0;\n  always #1 clk = ~clk;\n  initial #20 $finish;\n",
        );
        for index in 0..CLOCKS {
            source +=
                &format!("  logic [7:0] n{index};\n  Counter u{index}(.clk(clk), .n(n{index}));\n");
        }
        source += "endmodule\n";
        fn finishes<B: crate::SimBackend>(backend: &str, mut sim: crate::Simulation<B>) {
            sim.run_until(100).unwrap();
            assert!(sim.is_finished(), "{backend}");
            for index in 0..CLOCKS {
                let counter = sim.signal(&format!("n{index}"));
                assert_eq!(sim.get(counter), BigUint::from(10u8), "{backend}: n{index}");
            }
        }
        let wide = builder(&source).build().unwrap();
        let scheduler = wide
            .simulator
            .program
            .runtime_schema
            .scheduler
            .as_ref()
            .unwrap();
        assert!(scheduler.events.len() <= 64);
        assert!(wide.simulator.layout().widths[&scheduler.slots.levels] > 64);
        finishes("native", wide);
        finishes("interpreter", builder(&source).build_interpreter().unwrap());
        finishes("tiered", builder(&source).build_tiered().unwrap());
        #[cfg(feature = "cranelift")]
        finishes("cranelift", builder(&source).build_cranelift().unwrap());
        #[cfg(feature = "wasm-runtime")]
        finishes("wasm", builder(&source).build_wasm().unwrap());
    }

    /// The runtime reads the wide scheduler slots on every backend.
    #[test]
    fn more_than_64_events_run_on_every_backend() {
        let source = more_than_64_events();
        counts_every_edge("interpreter", builder(&source).build_interpreter().unwrap());
        counts_every_edge("tiered", builder(&source).build_tiered().unwrap());
        // Cranelift's wide comparisons once ran every domain (#2102).
        #[cfg(feature = "cranelift")]
        assert!(counts_every_edge("cranelift", builder(&source).build_cranelift().unwrap()) > 0);
        #[cfg(feature = "wasm-runtime")]
        counts_every_edge("wasm", builder(&source).build_wasm().unwrap());
    }
}

#[cfg(all(
    test,
    feature = "systemverilog",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod reference_reset_compiled_tests {
    #[test]
    fn reference_process_reset_is_stable_between_compiled_runs() {
        let directory = tempfile::tempdir().unwrap();
        let memory = directory.path().join("reset-memory.hex");
        std::fs::write(&memory, "a5\n").unwrap();
        let source = r#"`timescale 1ns/1ns
module Top;
    bit [7:0] q = 0, releases = 0;
    bit clk = 0, reset_n = 0;
    wire reset = !reset_n;
    logic [7:0] mem [0:0];
    initial begin
        // #0 keeps this process in the reference loop without host state.
        #0;
        $readmemh("MEMFILE", mem);
        #3 reset_n = 1;
        #13 reset_n = 0;
        #2 reset_n = 1;
    end
    always_ff @(posedge reset_n) releases <= releases + 1;
    always #1 clk = !clk;
    always_ff @(posedge clk or posedge reset)
        if (reset) q <= 0;
        else q <= q + 1;
endmodule"#
            .replace(
                "\"MEMFILE\"",
                &serde_json::to_string(&memory.to_string_lossy()).unwrap(),
            );
        for four_state in [false, true] {
            let mut simulation = crate::Simulation::from_sv_sources(
                vec![(source.as_str(), std::path::Path::new("reference-reset.sv"))],
                "Top",
            )
            .four_state(four_state)
            .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
            .build_native()
            .unwrap();
            simulation.run_until(24).unwrap();
            let q = simulation.signal("q");
            assert_eq!(simulation.get(q), num_bigint::BigUint::from(3u8));
            let releases = simulation.signal("releases");
            assert_eq!(simulation.get(releases), num_bigint::BigUint::from(2u8));
            let mem = simulation.signal("mem");
            assert_eq!(simulation.get(mem), num_bigint::BigUint::from(0xa5u8));
            let stats = simulation.scheduler_stats();
            assert!(stats.compiled_slots > 0, "{stats:?}");
            assert!(
                stats.checked_steps > 0,
                "compiled runs must match the reference: {stats:?}"
            );
        }
    }
}

#[cfg(all(
    test,
    feature = "systemverilog",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod active_commit_compiled_tests {
    #[test]
    fn unobserved_blocking_results_keep_clock_and_waiter_compiled() {
        let source = r#"`timescale 1ns/1ns
module Top;
            bit clk = 0;
            bit [7:0] count = 0, q = 0;
            always #1 clk = !clk;
            always @(posedge clk) begin count = count + 1; q <= count; end
            initial begin
                repeat (10) @(negedge clk);
                if (count != 10 || q != 10) $fatal(1, "bad count");
                $finish;
            end
        endmodule"#;
        for four_state in [false, true] {
            let mut simulation = crate::Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("active-counter.sv"))],
                "Top",
            )
            .four_state(four_state)
            .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
            .build_native()
            .unwrap();
            simulation.run_until(24).unwrap();
            assert!(simulation.is_finished());
            let stats = simulation.scheduler_stats();
            assert!(stats.compiled_slots > 0, "{stats:?}");
            assert!(stats.checked_steps > 0, "{stats:?}");
        }
    }

    #[test]
    fn blocking_gate_update_keeps_its_edge_before_nba() {
        let source = r#"`timescale 1ns/1ns
module Top;
            bit clk = 0, enable = 0, a = 0, seen = 0;
            wire gated = clk & enable;
            always #1 clk = !clk;
            always @(posedge clk) begin enable = 1; a <= 1; end
            always @(posedge gated) seen <= a;
            initial begin #2; if (seen != 0) $fatal(1, "NBA published before active edge"); $finish; end
        endmodule"#;
        for four_state in [false, true] {
            let mut simulation = crate::Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("active-gate.sv"))],
                "Top",
            )
            .four_state(four_state)
            .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
            .build_native()
            .unwrap();
            simulation.run_until(4).unwrap();
            assert!(simulation.is_finished());
            let seen = simulation.signal("seen");
            assert_eq!(simulation.get(seen), num_bigint::BigUint::from(0u8));
            let stats = simulation.scheduler_stats();
            assert!(stats.compiled_slots > 0, "{stats:?}");
            assert!(stats.checked_steps > 0, "{stats:?}");
        }
    }
}

#[cfg(all(
    test,
    feature = "systemverilog",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod repeated_active_edge_tests {
    #[test]
    fn mixed_active_and_nba_clock_inputs_preserve_both_edges() {
        let source = r#"`timescale 1ns/1ns
module Top;
    bit clk = 0, enable = 1, active_value = 0;
    bit [7:0] edges = 0;
    wire first = clk & enable;
    wire second = first ^ active_value;
    always #1 clk = !clk;
    always @(posedge clk) enable <= 0;
    always @(posedge first) active_value = 1;
    always @(posedge second) edges = edges + 1;
    initial begin #2; if (edges != 2) $fatal(1, "lost repeated edge"); $finish; end
endmodule"#;
        for four_state in [false, true] {
            let mut simulation = crate::Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("repeated-active.sv"))],
                "Top",
            )
            .four_state(four_state)
            .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
            .build_native()
            .unwrap();
            simulation.run_until(3).unwrap();
            assert!(simulation.is_finished());
            let edges = simulation.signal("edges");
            assert_eq!(simulation.get(edges), num_bigint::BigUint::from(2u8));
            #[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
            assert!(
                simulation.scheduler_stats().compiled_slots > 0,
                "{:?}",
                simulation.scheduler_stats()
            );
        }
    }

    #[test]
    fn unknown_then_known_clock_counts_both_four_state_posedges() {
        for four_state in [false, true] {
            let expected = if four_state { 2 } else { 1 };
            let source = format!(
                r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    logic enable = 1'bx;
    bit [7:0] edges = 0;
    wire gated = clk & enable;
    always #1 clk = !clk;
    always @(posedge clk) enable = 1;
    always @(posedge gated) edges = edges + 1;
    initial begin #2; if (edges != {expected}) $fatal(1, "bad unknown edges"); $finish; end
endmodule"#
            );
            let mut simulation = crate::Simulation::from_sv_sources(
                vec![(source.as_str(), std::path::Path::new("unknown-active.sv"))],
                "Top",
            )
            .four_state(four_state)
            .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
            .build_native()
            .unwrap();
            simulation.run_until(3).unwrap();
            assert!(simulation.is_finished());
            let edges = simulation.signal("edges");
            assert_eq!(
                simulation.get(edges),
                num_bigint::BigUint::from(expected as u8)
            );
            #[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
            if four_state {
                assert!(
                    simulation.scheduler_stats().compiled_slots > 0,
                    "{:?}",
                    simulation.scheduler_stats()
                );
            }
        }
    }
}

/// Rebuild only the scheduler without occurrence-probe hints. The design's
/// instrumented stores stay intact; metadata can be omitted to exercise the
/// conservative statefulness check.
#[cfg(all(
    test,
    feature = "systemverilog",
    feature = "host-runtime",
    any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    )
))]
pub(crate) fn without_probe_hints_for_test(
    mut program: LaidOutProgram,
    statefulness_known: bool,
) -> LaidOutProgram {
    program.runtime.runtime_schema.occurrence_probes.clear();
    if !statefulness_known {
        program.sir.eval_comb_stateful = None;
    }
    let schema = program.runtime.runtime_schema.scheduler.clone().unwrap();
    let plan = Plan::new(&program, &schema.slots, true).unwrap();
    assert_eq!(schema.fallback, plan.fallback);
    assert_eq!(
        schema.events,
        plan.tracked()
            .iter()
            .map(|&event| plan.events[event].signal)
            .collect::<Vec<_>>()
    );
    program.sir.processes[schema.unit.unwrap()] = plan.generate(&program, &schema.slots);
    program
}

impl Generator<'_> {
    fn raw_wait_planes(
        &mut self,
        signal: AbsoluteAddr,
        term: &celox_design::schedule::CompiledWaitTerm,
    ) -> RegisterId {
        if term.plane_bits == 1 && term.mask_offset.is_some() {
            return self.signal_level(signal);
        }
        if term.mask_offset.is_none() {
            return self.load(signal, term.width);
        }
        let mut value = self.load_state(signal, 0, term.width);
        if term.width < term.plane_bits {
            let padding = self.b.alloc_logic(term.plane_bits - term.width);
            self.b
                .emit(SIRInstruction::Imm(padding, SIRValue::new(0u64)));
            let padded = self.b.alloc_logic(term.plane_bits);
            self.b
                .emit(SIRInstruction::Concat(padded, vec![padding, value]));
            value = padded;
        }
        let planes = self.b.alloc_bit(term.plane_bits * 2, false);
        self.b.emit(SIRInstruction::Unary(
            planes,
            celox_design::UnaryOp::ToPlanes,
            value,
        ));
        planes
    }

    fn raw_wait_level(
        &mut self,
        planes: RegisterId,
        term: &celox_design::schedule::CompiledWaitTerm,
    ) -> RegisterId {
        let value = self.b.alloc_bit(1, false);
        self.b.emit(SIRInstruction::Slice(value, planes, 0, 1));
        if term.mask_offset.is_none() {
            return value;
        }
        let mask = self.b.alloc_bit(1, false);
        self.b
            .emit(SIRInstruction::Slice(mask, planes, term.plane_bits, 1));
        let level = self.b.alloc_bit(2, false);
        self.b
            .emit(SIRInstruction::Concat(level, vec![mask, value]));
        level
    }

    fn capture_compact_counter(&mut self, process: usize, counter: Option<AbsoluteAddr>) {
        if let Some(counter) = counter {
            let seen = self.load(counter, 32);
            self.store_at(self.slots.baseline[process], SCHEDULER_FLAG_WIDTH, 32, seen);
        }
    }

    fn capture_raw_wait(&mut self, process: usize, wait: &Wait) {
        for (ordinal, ((&(signal, _kind), term), counter)) in wait
            .terms
            .iter()
            .zip(&wait.raw.as_ref().unwrap().terms)
            .zip(&wait.counters)
            .enumerate()
        {
            if let (Some(offset), Some(counter)) = (term.counter_offset, counter) {
                let seen = self.load(*counter, 32);
                self.store_at(self.slots.baseline[process], offset, 32, seen);
            }
            if let Some(offset) = term.gated_counter_offset {
                let counter = wait.gates[ordinal].unwrap().counters.unwrap().gated;
                let seen = self.load(counter, 32);
                self.store_at(self.slots.baseline[process], offset, 32, seen);
            }
            let planes = self.raw_wait_planes(signal, term);
            let width = if term.mask_offset.is_some() {
                term.plane_bits * 2
            } else {
                term.width
            };
            self.store_at(
                self.slots.baseline[process],
                term.value_offset,
                width,
                planes,
            );
        }
    }

    /// A change-only OR wait can stop at its first changed expression.
    /// If none changed, its snapshot already equals the current state. If
    /// any changed, the wait is consumed and capture_raw_wait replaces the
    /// entire snapshot when the process next suspends. Edge-sensitive waits
    /// cannot do this: non-triggering changes must advance their baselines.
    fn check_change_wait(
        &mut self,
        process: usize,
        wait: &Wait,
        waiting: RegisterId,
    ) -> RegisterId {
        let zero = self.constant(0, 1);
        let one = self.constant(1, 1);
        let result = self.b.alloc_bit(1, false);
        let done = self.b.new_block_with(vec![result]);
        let body = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: waiting,
            true_block: (body, Vec::new()),
            false_block: (done, vec![zero]),
        });
        self.b.switch_to_block(body);
        let raw = wait.raw.as_ref().unwrap();
        // A small batch amortizes branches without extending the lifetime of
        // every expression in a large sensitivity list.
        for (signals, terms) in wait.terms.chunks(4).zip(raw.terms.chunks(4)) {
            let mut differences = zero;
            let mut difference_width = 1;
            for (&(signal, _), term) in signals.iter().zip(terms) {
                let width = if term.mask_offset.is_some() {
                    term.plane_bits * 2
                } else {
                    term.width
                };
                let previous = self.load_at(self.slots.baseline[process], term.value_offset, width);
                let current = self.raw_wait_planes(signal, term);
                // These are raw, two-state planes. Merge differing bits
                // before reducing to a boolean, instead of materializing a
                // comparison result for every value and mask chunk.
                let different = self.binary(width, previous, BinaryOp::Xor, current);
                difference_width = difference_width.max(width);
                differences = self.binary(difference_width, differences, BinaryOp::Or, different);
            }
            let no_difference = self.constant(0, difference_width);
            let changed = self.binary(1, differences, BinaryOp::Ne, no_difference);
            let next = self.b.new_block();
            self.b.seal_block(SIRTerminator::Branch {
                cond: changed,
                true_block: (done, vec![one]),
                false_block: (next, Vec::new()),
            });
            self.b.switch_to_block(next);
        }
        self.b.seal_block(SIRTerminator::Jump(done, vec![zero]));
        self.b.switch_to_block(done);
        result
    }

    /// Test every expression in an OR event list against its own snapshot,
    /// before publishing one ready bit and clearing the process's wait flag.
    fn check_raw_wait(
        &mut self,
        process: usize,
        wait: &Wait,
        waiting: RegisterId,
        settled: bool,
    ) -> RegisterId {
        if wait.terms.len() >= 4
            && wait.counters.iter().all(Option::is_none)
            && wait.gates.iter().all(Option::is_none)
            && wait
                .terms
                .iter()
                .all(|(_, kind)| *kind == CounterKind::Changes)
        {
            return self.check_change_wait(process, wait, waiting);
        }
        let mut occurred = self.constant(0, 1);
        for (ordinal, ((&(signal, kind), term), counter)) in wait
            .terms
            .iter()
            .zip(&wait.raw.as_ref().unwrap().terms)
            .zip(&wait.counters)
            .enumerate()
        {
            let gate = wait.gates.get(ordinal).copied().flatten();
            if !settled && gate.is_some_and(|gate| !gate.at_write && gate.counters.is_none()) {
                continue;
            }
            let width = if term.mask_offset.is_some() {
                term.plane_bits * 2
            } else {
                term.width
            };
            let previous = self.load_at(self.slots.baseline[process], term.value_offset, width);
            let current = self.raw_wait_planes(signal, term);
            let changed = self.binary(1, previous, BinaryOp::Ne, current);
            let edge = match kind {
                CounterKind::Changes => changed,
                CounterKind::Rises | CounterKind::Falls if term.mask_offset.is_some() => {
                    let was = self.raw_wait_level(previous, term);
                    let is = self.raw_wait_level(current, term);
                    self.level_edge(kind == CounterKind::Rises, was, is)
                }
                CounterKind::Rises | CounterKind::Falls => {
                    let was = self.raw_wait_level(previous, term);
                    let is = self.raw_wait_level(current, term);
                    let changed = self.binary(1, was, BinaryOp::Ne, is);
                    self.binary(
                        1,
                        changed,
                        BinaryOp::And,
                        if kind == CounterKind::Rises { is } else { was },
                    )
                }
            };
            let gate_holds = gate.map(|gate| {
                let level = self.signal_level(gate.signal);
                self.equals(level, 1)
            });
            let edge = if let (Some(offset), Some(counter)) = (term.counter_offset, counter) {
                let seen = self.load_at(self.slots.baseline[process], offset, 32);
                let current = self.load(*counter, 32);
                let counted = self.binary(1, seen, BinaryOp::Ne, current);
                let next = self.b.alloc_bit(32, false);
                self.b
                    .emit(SIRInstruction::Mux(next, waiting, current, seen));
                self.store_at(self.slots.baseline[process], offset, 32, next);
                if let Some(gated_offset) = term.gated_counter_offset {
                    let gated = gate.unwrap().counters.unwrap().gated;
                    let seen = self.load_at(self.slots.baseline[process], gated_offset, 32);
                    let current = self.load(gated, 32);
                    let accepted = self.binary(1, seen, BinaryOp::Ne, current);
                    let one = self.constant(1, 1);
                    let uncounted = self.binary(1, counted, BinaryOp::Xor, one);
                    let raw = self.binary(1, edge, BinaryOp::And, uncounted);
                    let raw = self.binary(1, raw, BinaryOp::And, gate_holds.unwrap());
                    self.binary(1, accepted, BinaryOp::Or, raw)
                } else {
                    let edge = self.binary(1, edge, BinaryOp::Or, counted);
                    gate_holds.map_or(edge, |gate| self.binary(1, edge, BinaryOp::And, gate))
                }
            } else {
                gate_holds.map_or(edge, |gate| self.binary(1, edge, BinaryOp::And, gate))
            };
            occurred = self.binary(1, occurred, BinaryOp::Or, edge);
            let next = self.b.alloc_bit(width, false);
            self.b
                .emit(SIRInstruction::Mux(next, waiting, current, previous));
            self.store_at(self.slots.baseline[process], term.value_offset, width, next);
        }
        self.binary(1, occurred, BinaryOp::And, waiting)
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod orphan_clock_tests {
    use crate::{KernelSchedule, SchedulerMode, SimBackend, Simulation};

    fn check<B: SimBackend>(mut sim: Simulation<B>, stops: &[u64], expected: &[(&str, u8)]) {
        for &stop in stops {
            sim.run_until(stop).unwrap();
        }
        assert!(sim.is_finished());
        let stats = sim.scheduler_stats();
        assert!(stats.compiled_slots > 0, "{stats:?}");
        assert!(stats.checked_steps > 0, "{stats:?}");
        assert!(stats.unchecked.is_none(), "{stats:?}");
        assert!(
            stats.kernels.iter().all(|k| *k == KernelSchedule::Compiled),
            "{stats:?}"
        );
        for &(name, value) in expected {
            let signal = sim.signal(name);
            assert_eq!(sim.get(signal), value.into(), "{name}");
        }
    }

    fn check_backends(source: &str, four_state: bool, stops: &[u64], expected: &[(&str, u8)]) {
        let build = || {
            Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("orphan-clock.sv"))],
                "Top",
            )
            .four_state(four_state)
            .scheduler_mode(SchedulerMode::DifferentialRuns)
        };
        check(build().build_native().unwrap(), stops, expected);
        #[cfg(feature = "cranelift")]
        check(build().build_cranelift().unwrap(), stops, expected);
        #[cfg(feature = "wasm-runtime")]
        check(build().build_wasm().unwrap(), stops, expected);
    }

    // An unrelated source-clock FF must not decide whether the entire design
    // can run compiled. Include a concurrent independent clock and a divided
    // domain downstream of the clock whose source has no event of its own.
    #[test]
    fn combinational_clocks_do_not_require_source_domains() {
        for definition in [
            "assign gclk = clk & en;",
            "assign gclk = en ? clk : 1'b0;",
            "assign gclk = ~clk;",
            "always_comb gclk = clk & en;",
            "logic held=0; always_latch if (!clk) held=en; assign gclk=clk & held;",
            "wire gate=clk & en; logic half=0; always_ff @(posedge gate) half<=~half; assign gclk=half;",
        ] {
            for source_domain in ["", "always_ff @(posedge clk) dummy<=~dummy;"] {
                let source = format!(
                    r#"`timescale 1ns/1ns
module Top;
 logic clk=0; logic en=0; logic gclk;
 logic other_clk=0; logic [7:0] other=0; logic dummy=0;
 logic [7:0] q=0;
 always #5 clk=~clk;
 always #7 other_clk=~other_clk;
 initial forever #17 en=~en;
 always_ff @(posedge other_clk) other<=other+1;
 {source_domain}
 {definition}
 always_ff @(posedge gclk) q<=q+1;
 initial begin #400 $display("Q %d other %d dummy %d",q,other,dummy); $finish; end
endmodule
"#
                );
                for four_state in [false, true] {
                    check_backends(&source, four_state, &[100, 333, 500], &[]);
                }
            }
        }
    }

    #[test]
    fn orphan_clock_edges_preserve_unknown_transitions() {
        let source = r#"`timescale 1ns/1ns
module Top;
 logic clk=0; logic en=1; wire gclk=clk & en;
 logic [7:0] p=0, n=0;
 always_ff @(posedge gclk) p<=p+1;
 always_ff @(negedge gclk) n<=n+1;
 initial begin #5 clk=1'bx; #5 clk=1; #5 clk=1'bz; #5 clk=0; #5 clk=1; #5 clk=0; end
 initial begin #35 en=0; #5 $display("p %d n %d",p,n); $finish; end
endmodule
"#;
        // IEEE 1800-2023 Table 9-2: three rises and three falls in the
        // sequence, plus the initial settle's X-to-0 falling edge.
        check_backends(source, true, &[10, 25, 50], &[("p", 3), ("n", 4)]);
    }
}
