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
mod generate;
mod generator;
pub(crate) mod host_inputs;
mod inline;
mod nba;
mod observers;
mod ordered;
mod plan;
mod regions;
mod settle;
mod slice;
mod slot_observers;
use slot_observers::AfterWrites;
mod steps;
mod testbench;
mod units;
mod wait_checks;
mod waits;
mod waves;

use slice::CombSlicer;

use units::*;
use wait_checks::*;

use celox_design::{
    BASELINE_TERM_SHIFT, CounterKind, DomainKind, ProcessStatus, RuntimeEventKind,
    SCHEDULER_FLAG_WIDTH, SCHEDULER_TIME_WIDTH, STABLE_REGION, SchedulerExit, SchedulerSlots,
    TIME_SLOT_WIDTH, edges::LEVEL_UNKNOWN, schedule::ScheduleModel,
};
use celox_sir::{BinaryOp, SIRBuilder, SIRSwitchCase, SIRValue, StateAccess, UnaryOp};
use num_bigint::BigUint;
use std::collections::{BTreeMap, BTreeSet};

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
///
/// Units larger than [`OUTLINE_INSTRUCTIONS`] are always callees. With an
/// `inline_budget`, so are units of at least [`OUTLINE_MIN_INSTRUCTIONS`]
/// once the inlined ones exceed it: a smaller function that compiles sooner.
#[cfg(hotspot_jit)]
pub(crate) fn hotspot_scheduler(
    program: &LaidOutProgram,
    inline_budget: Option<usize>,
    cancel: Option<&crate::backend::compile_cancel::CompileCancel>,
) -> Result<HotspotScheduler, String> {
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
    let mut outlines = Outlines {
        callees: Vec::new(),
        inline_budget,
        budgeted: false,
        cancel: cancel.cloned(),
        cancelled: false,
    };
    let unit = plan.generate_with_outlines(program, &schema.slots, Some(&mut outlines), base);
    if outlines.cancelled {
        return Err("scheduler generation was cancelled".into());
    }
    Ok(HotspotScheduler {
        unit,
        callees: outlines.callees,
        budgeted: outlines.budgeted,
    })
}

/// A generated hotspot scheduler and its separately compiled bodies.
#[cfg(hotspot_jit)]
pub(crate) struct HotspotScheduler {
    pub unit: Unit,
    pub callees: Vec<Unit>,
    /// The inline budget outlined a unit: generating without it inlines more.
    pub budgeted: bool,
}

/// The bodies a generated scheduler calls instead of inlining them.
struct Outlines {
    callees: Vec<Unit>,
    inline_budget: Option<usize>,
    budgeted: bool,
    /// Generation stops inlining bodies once this is cancelled, and its
    /// unit is discarded.
    cancel: Option<crate::backend::compile_cancel::CompileCancel>,
    cancelled: bool,
}

/// With callees available, a unit larger than this always compiles as one.
const OUTLINE_INSTRUCTIONS: usize = 32_768;
/// Inlined units beyond this many instructions make the compiled scheduler a
/// function too large to allocate quickly; with this budget, later units of
/// at least [`OUTLINE_MIN_INSTRUCTIONS`] compile as callees instead.
#[cfg(hotspot_jit)]
pub(crate) const INLINE_BUDGET_INSTRUCTIONS: usize = 262_144;
const OUTLINE_MIN_INSTRUCTIONS: usize = 256;

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
    let schema = &program.runtime.runtime_schema;
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
    if needs_service {
        require_native_services(program);
    }
}

/// Declare the native services of `program`, unless it already does.
pub(crate) fn require_native_services(program: &mut LaidOutProgram) {
    let schema = &mut program.runtime.runtime_schema;
    let name = crate::simulator::native_io::SYMBOL;
    if schema
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
    // The points exist only in a scheduler that can call the host.
    let mut update_points = false;
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
                update_points = schema.update_points && plan.service_function.is_some();
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
    let domain_waits = domain_waits(program);
    let scheduler = program
        .runtime
        .runtime_schema
        .scheduler
        .as_mut()
        .expect("checked above");
    scheduler.fallback = fallback;
    scheduler.update_points = update_points;
    scheduler.events = events;
    scheduler.clocks = clocks;
    scheduler.kernel_events = kernel_events;
    scheduler.domain_waits = domain_waits;
    // The generated unit refreshes the sampled-value shadows at every
    // slot it starts (see `generate_with_outlines`).
    scheduler.refreshes_sample_shadows = !program.runtime.runtime_schema.sample_shadows.is_empty();
    if let Some(unit) = unit {
        scheduler.unit = Some(program.sir.processes.len());
        program.sir.processes.push(unit);
    }
    testbench::attach(program, lazy);
}

/// [`celox_design::SchedulerSchema::domain_waits`] of `program`.
fn domain_waits(program: &LaidOutProgram) -> Vec<bool> {
    let schema = &program.runtime.runtime_schema;
    let sir = &program.sir;
    let mut written = HashSet::default();
    for unit in sir
        .eval_comb
        .iter()
        .chain(sir.eval_apply_ffs.values().flatten())
        .chain(sir.eval_comb_apply_ffs.values().flatten())
        .chain(sir.eval_only_ffs.values().flatten())
        .chain(sir.apply_ffs.values().flatten())
        .chain(
            sir.parallel
                .iter()
                .flat_map(|parallel| parallel.units().map(|unit| &unit.unit)),
        )
    {
        for instruction in unit.blocks.values().flat_map(|block| &block.instructions) {
            match instruction.state_writes() {
                celox_sir::StateAccess::Access(write) => {
                    written.insert(write.addr.absolute_addr());
                }
                access => match access.objects() {
                    Some(objects) => {
                        written.extend(objects.map(|object| object.absolute_addr()));
                    }
                    // A write anywhere may reach every wait.
                    None => return vec![true; schema.processes.len()],
                },
            }
        }
    }
    written.extend(schema.nba_sites.iter().map(|site| site.target));
    let mut domain = vec![false; schema.processes.len()];
    for site in &schema.wait_sites {
        let probes = schema
            .process_frames
            .get(site.process)
            .and_then(Option::as_ref)
            .map_or(&[][..], |frame| &frame.event_probes[..]);
        let settled = site.terms.iter().enumerate().any(|(index, &(signal, _))| {
            written.contains(&signal)
                || site.gate(index).is_some_and(|gate| gate.counters.is_none())
                || probes.iter().any(|probe| probe.result == signal)
        });
        if let Some(domain) = domain.get_mut(site.process) {
            *domain |= settled;
        }
    }
    domain
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
    /// The flags of a partitioned settle and its partition count: the cone
    /// runs only when the untracked byte is clear, as a raw view of the
    /// state dirties every partition.
    untracked: Option<(AbsoluteAddr, usize)>,
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
    outlined: Option<&'a mut Outlines>,
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
    /// Instructions of the units inlined so far, for outlining.
    inlined_instructions: usize,
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

#[cfg(test)]
mod test_support;
#[cfg(test)]
pub(crate) use test_support::*;

#[cfg(all(
    test,
    feature = "systemverilog",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod active_commit_compiled_tests;
#[cfg(all(test, feature = "systemverilog"))]
mod finish_phase_tests;
#[cfg(all(test, feature = "systemverilog"))]
mod orphan_clock_tests;
#[cfg(all(
    test,
    feature = "systemverilog",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod reference_reset_compiled_tests;
#[cfg(all(
    test,
    feature = "systemverilog",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod repeated_active_edge_tests;
#[cfg(test)]
mod tests;
#[cfg(all(test, feature = "systemverilog"))]
mod wide_event_tests;
