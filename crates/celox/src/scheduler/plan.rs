//! Building the [`Plan`] of a design: which kernels and events the compiled
//! scheduler runs, and how its slots settle and order their domains.
mod events;
mod kernels;
mod ordering;
mod settles;

use super::*;

/// The combinational slicer of a plan, built when a phase first needs it.
#[expect(
    clippy::disallowed_types,
    reason = "single-threaded: the combinational slicer a plan builds on first use, borrowed only by the phases of one Plan::new call"
)]
type Slicer<'a, F> = std::cell::LazyCell<CombSlicer<'a>, F>;

/// The state [`Plan::new`] builds a plan in: the design, and what the phases
/// found of it so far. The phases run in the order `Plan::new` calls them.
struct PlanBuilder<'a> {
    program: &'a LaidOutProgram,
    slots: &'a SchedulerSlots<AbsoluteAddr>,
    wiring_clocks: bool,
    schema: &'a celox_design::RuntimeSchema<AbsoluteAddr>,
    model: &'a ScheduleModel<AbsoluteAddr>,
    topology: &'a celox_design::EventTopology<AbsoluteAddr>,
    sir: &'a crate::ir::SirProgram,
    /// Whether the unit can observe combinational processes natively.
    native_observers: bool,
    /// See `LaidOutProgram::shared_storage`.
    shared: &'a HashMap<AbsoluteAddr, AbsoluteAddr>,
    found: Found,
}

/// What the phases of a [`PlanBuilder`] found; each field is empty until its
/// phase ran.
#[derive(Default)]
struct Found {
    /// The event signals, in event order.
    addresses: Vec<AbsoluteAddr>,
    /// See [`Plan::event_bits`].
    event_bits: usize,
    /// See [`Plan::events`].
    events: Vec<Event>,
    /// The events of each home an event signal has.
    event_storage: HashMap<AbsoluteAddr, Vec<usize>>,
    /// The events RTL logic drives.
    rtl_written: BTreeSet<usize>,
    /// The storage RTL logic writes.
    rtl_stores: HashSet<AbsoluteAddr>,
    /// The storage each process kernel writes.
    kernel_stores: Vec<HashSet<AbsoluteAddr>>,
    /// The control slots of the process kernels.
    control_slots: HashSet<AbsoluteAddr>,
    /// The storage the wait sites read.
    waited: HashSet<AbsoluteAddr>,
    /// Storage whose changes bump an occurrence counter.
    probed: HashSet<AbsoluteAddr>,
    /// Storage a blocking result must reach before NBA publication: what can
    /// wake a process, move a clock or feed stateful combinational logic.
    active_observers: HashSet<AbsoluteAddr>,
    /// The events whose domains commit such results in the Active region.
    active_commits: HashSet<usize>,
    /// How many nonblocking sites target each storage.
    nba_targets: HashMap<AbsoluteAddr, usize>,
    /// See [`Plan::clocks`].
    clocks: Vec<usize>,
    /// See [`Plan::service_function`].
    service_function: Option<u32>,
    /// Whether the unit can run ordered waves through the native services.
    ordered_available: bool,
    /// See [`Plan::nba_processes`].
    nba_processes: HashSet<usize>,
    /// The kernels that return at loop checkpoints.
    yielding: HashSet<usize>,
    /// See [`Plan::service_processes`].
    service_processes: HashSet<usize>,
    /// See [`Plan::kernels`].
    kernels: Vec<Option<Kernel>>,
    /// See [`Plan::fallback`].
    fallback: Vec<Option<String>>,
    /// How many kernels that only delay write each storage.
    delay_writers: HashMap<AbsoluteAddr, usize>,
    /// Storage the other kernels write, waiting ones included.
    other_stores: HashSet<AbsoluteAddr>,
    /// Whether RTL logic or a kernel the scheduler may run makes host calls.
    opaque_dependencies: bool,
    /// Whether a waiting or host-service kernel writes an event signal.
    event_writing_waits: bool,
    /// See [`Plan::inactive`].
    inactive: Vec<(usize, AbsoluteAddr)>,
    /// See [`Plan::cyclic`].
    cyclic: bool,
    /// See [`Plan::finish_sites`].
    finish_sites: HashSet<u32>,
    /// Whether combinational logic has effects of its own.
    effects: bool,
    /// See [`Plan::settle_reads`].
    settle_reads: Option<HashSet<AbsoluteAddr>>,
    /// The storage combinational logic writes.
    comb_stores: HashSet<AbsoluteAddr>,
    /// The sensitivity signals and occurrence counters among it, which stay eager.
    watched_comb: HashSet<AbsoluteAddr>,
    /// The storage a settle reads from the one before (see [`retained_storage`]).
    retained: HashSet<AbsoluteAddr>,
    /// The size of the eager part of the settle that was considered.
    eager_candidate_instructions: Option<usize>,
    /// See [`Plan::lazy`].
    lazy: Option<Lazy>,
    /// See [`Plan::incremental`].
    incremental: Option<Incremental>,
    /// See [`Plan::edged`].
    edged: Vec<usize>,
    /// The event of each event signal.
    index_of: HashMap<AbsoluteAddr, usize>,
    /// See [`Plan::derived`].
    derived: BTreeSet<usize>,
    /// Whether the slots need ordered waves (see [`Plan::ordered`]).
    needs_ordered: bool,
    /// The storage the domains, the kernels and nonblocking sites write.
    scheduled_stores: HashSet<AbsoluteAddr>,
    /// The events RTL logic drives that cannot change in a compiled run.
    stable_host_events: BTreeSet<usize>,
    /// The events RTL logic drives that no clock derivation covers.
    unhandled_rtl_events: Vec<usize>,
}

/// The units of RTL logic: combinational logic and the domains of the events.
fn rtl_units_of(sir: &crate::ir::SirProgram) -> impl Iterator<Item = &Unit> {
    sir.eval_comb
        .iter()
        .chain(sir.eval_apply_ffs.values().flatten())
        .chain(sir.eval_only_ffs.values().flatten())
        .chain(sir.apply_ffs.values().flatten())
}

/// The home of an address: the representative of the storage it shares
/// (`LaidOutProgram::shared_storage`).
fn home(
    shared: &HashMap<AbsoluteAddr, AbsoluteAddr>,
) -> impl Fn(AbsoluteAddr) -> AbsoluteAddr + Copy + '_ {
    move |address: AbsoluteAddr| shared.get(&address).copied().unwrap_or(address)
}

/// The events whose signal a unit writes.
fn written_events<'a>(
    event_storage: &'a HashMap<AbsoluteAddr, Vec<usize>>,
    storage: impl Fn(AbsoluteAddr) -> AbsoluteAddr + Copy + 'a,
) -> impl Fn(&Unit) -> Vec<usize> + Copy + 'a {
    // Event signals change only where the scheduler sees them change.
    move |unit: &Unit| -> Vec<usize> {
        let mut written: Vec<usize> = instructions(unit)
            .flat_map(written_storage)
            .filter_map(|target| event_storage.get(&storage(target.absolute_addr())))
            .flatten()
            .copied()
            .collect();
        written.sort_unstable();
        written.dedup();
        written
    }
}

/// The storage each unit writes.
fn unit_stores<'a>(
    storage: impl Fn(AbsoluteAddr) -> AbsoluteAddr + Copy + 'a,
) -> impl Fn(&Unit) -> HashSet<AbsoluteAddr> + Copy + 'a {
    move |unit: &Unit| -> HashSet<AbsoluteAddr> {
        instructions(unit)
            .flat_map(written_storage)
            .map(|target| storage(target.absolute_addr()))
            .collect()
    }
}

/// Whether a unit makes host calls, whose effects on the state are unknown.
fn opaque(unit: &Unit) -> bool {
    instructions(unit).any(makes_host_calls)
}

/// Whether no design code writes a storage: neither RTL logic, a kernel
/// the design starts, nor a nonblocking site.
fn undriven_storage<'a>(
    schema: &'a celox_design::RuntimeSchema<AbsoluteAddr>,
    rtl_stores: &'a HashSet<AbsoluteAddr>,
    kernel_stores: &'a [HashSet<AbsoluteAddr>],
    nba_targets: &'a HashMap<AbsoluteAddr, usize>,
) -> impl Fn(AbsoluteAddr) -> bool + Copy + 'a {
    move |signal: AbsoluteAddr| {
        !rtl_stores.contains(&signal)
            && !kernel_stores
                .iter()
                .zip(&schema.processes)
                .any(|(stores, process)| {
                    process.kind != celox_design::ProcessKind::Callable && stores.contains(&signal)
                })
            && !nba_targets.contains_key(&signal)
    }
}

/// The events derived from an event of `plan`: the clocks registers and
/// combinational logic compute from it.
fn derived_events<'a>(
    plan: &'a Plan,
    topology: &'a celox_design::EventTopology<AbsoluteAddr>,
    index_of: &'a HashMap<AbsoluteAddr, usize>,
) -> impl Fn(usize) -> Vec<usize> + 'a {
    move |event: usize| -> Vec<usize> {
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
    }
}

impl Plan {
    pub(super) fn new(
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
        let rtl_units = || rtl_units_of(sir);
        let planned = || {
            rtl_units()
                .chain(sir.eval_comb_apply_ffs.values().flatten())
                .chain(sir.processes.iter().take(schema.processes.len()))
                .flat_map(instructions)
        };
        // A native kernel may read and write any state, which none of the
        // storage sets below can name (see `read_storage`). The raw words of
        // a wide commit are shadowed by typed commits of the same registers.
        if planned().any(|instruction| {
            instruction.accesses_unknown_state()
                && !crate::settle::is_wide_commit_access(instruction)
        }) {
            return Err("a unit may access any state".into());
        }

        let shared = program.shared_storage();
        // All dependency and settle slices inspect the same immutable units.
        // Index their definitions and control dependencies only once per plan,
        // and only if one of these consumers actually needs a slice.
        #[expect(
            clippy::disallowed_types,
            reason = "single-threaded: the combinational slicer is built on first use by the phases of this one Plan::new call"
        )]
        let comb_slicer =
            std::cell::LazyCell::new(|| CombSlicer::new(&sir.eval_comb, shared.clone()));
        let mut builder = PlanBuilder {
            program,
            slots,
            wiring_clocks,
            schema,
            model: &model,
            topology,
            sir,
            native_observers,
            shared: &shared,
            found: Found::default(),
        };
        builder.events()?;
        let wait_slicer = builder.observers(&comb_slicer);
        builder.host_clocks()?;
        builder.services();
        builder.delaying_kernels();
        builder.waiting_kernels();
        builder.wait_feedback(wait_slicer)?;
        builder.settle_inputs();
        builder.deferred_settles(&comb_slicer);
        builder.incremental_settle(&comb_slicer);
        let mut plan = builder.plan()?;
        builder.derived_clocks(&plan);
        builder.active_ordering(&plan)?;
        builder.stable_events(&plan, &comb_slicer);
        builder.orphan_clocks(&plan, &comb_slicer);
        builder.ordered_waves(&mut plan);
        builder.finish(&mut plan)?;
        Ok(plan)
    }

    /// The events whose levels the slots record and compare: those that
    /// can have edges in them and the clocks derived from those, in event
    /// order: the bits of [`SchedulerSlots::levels`].
    pub(super) fn tracked(&self) -> Vec<usize> {
        let mut tracked = self.edged();
        tracked.extend(&self.derived);
        tracked.sort_unstable();
        tracked
    }

    /// The events the scheduler's kernels write, in event order: the only
    /// ones that can have edges in its slots.
    pub(super) fn written(&self) -> Vec<usize> {
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
    pub(super) fn edged(&self) -> Vec<usize> {
        let mut edged = self.written();
        edged.extend(&self.clocks);
        edged.extend(&self.host_edges);
        edged.sort_unstable();
        edged.dedup();
        edged
    }
}
