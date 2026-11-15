//! Scheduling the symbolic RTL of a design.

use super::*;

/// Evaluate `$body`, logging how long it took as `$label` if
/// `$flatten_timing`.
macro_rules! timed_sub {
    ($flatten_timing:expr, $label:expr, $body:expr) => {{
        if $flatten_timing {
            let start = std::time::Instant::now();
            let result = $body;
            tracing::debug!("[flatten] {}: {:?}", $label, start.elapsed());
            result
        } else {
            $body
        }
    }};
}

mod comb;
mod design;
mod flip_flops;
mod instrument;
mod projection;
mod state;
use comb::*;
use design::*;
use flip_flops::*;
use instrument::*;
use projection::*;
use state::*;

/// The hierarchy of a design and what every phase of scheduling it reads of
/// its instances and variables.
struct Design {
    modules: HashMap<ModuleId, SimModule>,
    module_names: HashMap<ModuleId, String>,
    root_id: ModuleId,
    time_precision: celox_design::TimePrecision,
    expanded: HashMap<InstancePath, InstanceId>,
    instance_modules: HashMap<InstanceId, ModuleId>,
    indexed_instances: HashSet<InstanceId>,
    /// The process kernels of all instances.
    process_count: usize,
    scheduler_slots: Option<celox_design::SchedulerSlots<SourceVarId>>,
    package_instances: HashMap<ModuleId, InstanceId>,
    instance_time_scales: HashMap<InstanceId, celox_design::TimeScale>,
    bindings: PackageBindings,
    global_boundaries: HashMap<AbsoluteAddr, BTreeSet<usize>>,
    unpacked_element_widths: HashMap<AbsoluteAddr, usize>,
    clock_domains: HashMap<AbsoluteAddr, AbsoluteAddr>,
    /// The readiness bit gated `always_comb` units read, if any is gated.
    startup_ready: Option<AbsoluteAddr>,
}

/// The combinational logic of the relocated design while it is symbolic.
struct CombLogic {
    global_arena: SLTNodeArena<AbsoluteAddr>,
    comb_blocks: Vec<LogicPath<AbsoluteAddr>>,
    comb_observers: Vec<CombObserver<AbsoluteAddr>>,
}

/// The relocated units of a design and what relocating them numbered.
struct Units {
    eval_apply_ffs: FfUnits,
    eval_only_ffs: FfUnits,
    apply_ffs: FfUnits,
    processes: Vec<RelocatedProcess>,
    functions: Vec<celox_sir::FunctionUnit<RegionedAbsoluteAddr>>,
    function_frames: Vec<celox_design::FunctionFrame>,
    ff_runtime_relocations: HashMap<InstanceId, FfRuntimeRelocation>,
    runtime_errors: HashMap<i64, RuntimeErrorInfo<AbsoluteAddr>>,
    runtime_event_sites: Vec<RuntimeEventSite>,
    extern_functions: Vec<ExternFunction>,
    next_runtime_error_code: i64,
    timing: TimingMetadata,
}

pub fn schedule_symbolic_rtl_reporting_loops(
    symbolic: SymbolicRtl,
    fused_ff_factory: Option<&dyn FusedFfLoweringFactory>,
    ignored_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )],
    true_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )],
    four_state: bool,
    parallel: &ParallelScheduleOptions,
    trace_opts: &FrontendTraceOptions,
    mut trace: Option<&mut FrontendTrace>,
    loop_members: &mut Vec<LoopMember>,
) -> Result<ScheduledRtlOutput, ParserError> {
    #[expect(
        clippy::disallowed_types,
        reason = "cross-thread: process-wide trace ID counter; simulator builds on different caller threads (parallel test threads, embedders) each draw a unique source-program ID"
    )]
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::Ordering;
    #[expect(
        clippy::disallowed_types,
        reason = "cross-thread: process-wide trace ID counter; simulator builds on different caller threads (parallel test threads, embedders) each draw a unique source-program ID"
    )]
    static NEXT_SOURCE_PROGRAM: AtomicU64 = AtomicU64::new(1);
    let _diagnostic = tracing::trace_span!(target: "celox::parallel", "source_program",
        source_program = NEXT_SOURCE_PROGRAM
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("source program identity exhausted").to_string())
    .entered();
    let flatten_timing = trace_opts.phase_timing;
    let (mut design, instance_values) =
        expand_design(symbolic, flatten_timing, trace_opts, &mut trace)?;
    let (mut logic, mut units, mut parallel_ff_units, mut split_commits) =
        relocate_design(&design, parallel, flatten_timing, trace_opts, &mut trace)?;
    let variables = variable_shapes(&design);
    let (counted, declaration_probes, occurrence_probes) = instrument_occurrences(
        &design,
        &mut logic,
        &mut units,
        &mut parallel_ff_units,
        &mut split_commits,
        &variables,
        four_state,
    );
    // Combinational values and the variables they are computed from.
    let comb_sources = comb_sources(&logic.comb_blocks, &logic.global_arena);
    check_package_variable_drivers(
        &design.bindings,
        &design.instance_modules,
        &design.modules,
        &design.package_instances,
        &logic.comb_blocks,
    )?;
    let ignored_loops = parse_ignored_loops(
        ignored_loops,
        &design.instance_modules,
        &design.modules,
        &design.expanded,
    );
    let true_loops = parse_true_loops(
        true_loops,
        &design.instance_modules,
        &design.modules,
        &design.expanded,
    );

    flattening::refine_cyclic_logic_paths(
        &mut logic.comb_blocks,
        &design.global_boundaries,
        &design.unpacked_element_widths,
        &mut logic.global_arena,
    )?;

    let reset_clock_map = reset_clocks(&design);

    let clocks = timed_sub!(
        flatten_timing,
        "analyze_clock_dependencies",
        analyze_clock_dependencies(
            &design.bindings,
            &mut units.eval_apply_ffs,
            &mut units.eval_only_ffs,
            &mut units.apply_ffs,
            &logic.comb_blocks,
            &logic.global_arena,
            &design.clock_domains,
            &design.expanded,
            &design.instance_modules,
            &design.modules,
        )
    );

    let var_widths = prepare_comb_logic(&design, &mut logic, &mut units, trace_opts, &mut trace)?;

    let fused_inputs = fused_ff_inputs(
        fused_ff_factory,
        &design,
        &logic,
        &units.ff_runtime_relocations,
        &var_widths,
        &ignored_loops,
        &true_loops,
    )?;

    // The event signals and their aliases: a partitioned settle keeps their
    // paths apart from the rest.
    let clock_nets = event_nets(&clocks.topological_clocks, &design.clock_domains);
    // Stores to event signals update shared trigger bytes; their writers stay
    // in one lane of a partitioned schedule.
    let lane0_objects = parallel.enabled().then(|| clock_nets.clone());
    let (mut persistent_comb_state, comb_startups) =
        comb_process_state(&logic, &variables, four_state);
    let inlined_nets = inline_unobserved_nets(
        parallel,
        fused_ff_factory,
        four_state,
        &design,
        &mut logic,
        &units,
        &parallel_ff_units,
        &split_commits,
        &clocks.topological_clocks,
        &clock_nets,
        &counted,
        &occurrence_probes,
        &declaration_probes,
    )?;
    let parallel_comb_blocks = parallel.enabled().then(|| logic.comb_blocks.clone());
    // The flag object of a partitioned settle is declared once the
    // partitions are known; reserve its address now.
    let settle_flags = plan_settle_flags(parallel, &design, &logic, &units, &counted, &clock_nets);
    let sched_start = flatten_timing.then(std::time::Instant::now);
    let schedule = match scheduler::sort_with_unpacked_element_widths(
        logic.comb_blocks,
        &logic.global_arena,
        &ignored_loops,
        &true_loops,
        four_state,
        &var_widths,
        &design.unpacked_element_widths,
        units.next_runtime_error_code,
        settle_flags.clone(),
    ) {
        Ok(schedule) => schedule,
        Err(error) => {
            return Err(scheduler_error(
                error,
                &design,
                &logic.global_arena,
                loop_members,
            )?);
        }
    };
    let settle_partitions = schedule.partitions.clone();
    let settle_copies = schedule.copies.clone();
    trace_settle_plan(&mut trace, trace_opts, &settle_partitions, &design);

    let settle_flags = settle_flags.filter(|_| !settle_partitions.is_empty());
    declare_partitioned_settle(
        &mut design,
        &settle_flags,
        &settle_partitions,
        &mut persistent_comb_state,
    );
    let object_sizes = super::super::parallel::ObjectSizes {
        widths: &var_widths,
        element_widths: &design.unpacked_element_widths,
    };
    let parallel_comb = sort_comb_lanes(
        parallel_comb_blocks,
        parallel,
        &lane0_objects,
        &design,
        &units,
        &parallel_ff_units,
        object_sizes,
        &logic.global_arena,
        &schedule,
        &ignored_loops,
        &true_loops,
        &var_widths,
        four_state,
    )?;
    // The comb schedule no longer refers to SLT nodes. Release its arena before
    // constructing the clock-fused schedules, whose arena already owns its
    // remapped nodes.
    drop(logic.global_arena);
    if let Some(s) = sched_start {
        tracing::debug!("[flatten] scheduler::sort: {:?}", s.elapsed());
    }
    let (eval_comb_stateful, eval_comb, parallel_eval_comb) =
        stable_comb_units(schedule, parallel_comb, &mut units.runtime_errors);
    // Every event's combined comb-then-FF tick gets its own program, with
    // the combinational program in it, only while there are a handful of
    // events; that copy lets the FF part use comb values in registers.
    // Beyond that the copies would multiply the design by its event count,
    // so the ticks share the one `eval_comb` program instead.
    // A merged program drops comb stores the next full settle would redo;
    // a partitioned settle keeps what its skipped partitions stored.
    let merge_comb_apply =
        units.eval_apply_ffs.len() <= MERGED_COMB_APPLY_EVENTS && settle_flags.is_none();
    let (eval_comb_apply_ffs, fused_direct_ff_writes) = schedule_fused_ffs(
        fused_ff_factory,
        fused_inputs,
        &mut units.eval_apply_ffs,
        merge_comb_apply,
        four_state,
        units.next_runtime_error_code,
        flatten_timing,
    )?;

    if let Some(t) = trace
        && trace_opts.scheduled_units
    {
        t.scheduled_units = Some(eval_comb.clone());
    }

    // The unified function is the normal fast path.  Split evaluator/apply
    // functions are needed only when scheduling can evaluate several active
    // domains or must cascade through a derived clock.
    let active_ff_domains = units
        .eval_apply_ffs
        .values()
        .filter(|units| !units.is_empty())
        .count();
    // With process kernels, a round in which a zero delay is pending after
    // domains triggered evaluates them before the inactive region and
    // publishes them in the NBA region (IEEE 1800-2023 4.5). Only those
    // rounds use the split units; the fused units stay the default.
    let needs_split_path =
        active_ff_domains > 1 || !clocks.cascaded_clocks.is_empty() || !units.processes.is_empty();
    let parallel_sir = plan_parallel_sir(
        parallel,
        &units.eval_apply_ffs,
        parallel_ff_units,
        &lane0_objects,
        parallel_eval_comb,
        object_sizes,
    );
    let active_commits = commit_observed_domains(
        &mut units,
        split_commits,
        &comb_sources,
        &design.clock_domains,
    );
    (units.eval_only_ffs, units.apply_ffs) = if needs_split_path {
        (units.eval_only_ffs, units.apply_ffs)
    } else {
        (HashMap::default(), HashMap::default())
    };

    size_scheduler_slots(&mut design, &counted, &units.timing, four_state);
    let (mod_vars, mod_path_idx) = module_variables(&design.modules);
    let (initial_memory_values, state_objects, declaration_startup) = initial_state(
        &design,
        instance_values,
        &settle_flags,
        &settle_partitions,
        &units.processes,
        &comb_startups,
        declaration_probes,
        four_state,
    )?;
    let runtime_comb_observers = runtime_comb_observers(&logic.comb_observers);
    stage_nba_sites(
        &design,
        &clocks,
        &mut units,
        &eval_comb,
        &eval_comb_apply_ffs,
        &runtime_comb_observers,
        &comb_sources,
    );
    check_function_stores(&design, &clocks, &units, &eval_comb, &eval_comb_apply_ffs)?;
    project_design(
        design,
        clocks,
        units,
        reset_clock_map,
        eval_comb,
        eval_comb_stateful,
        eval_comb_apply_ffs,
        merge_comb_apply,
        parallel_sir,
        state_objects,
        initial_memory_values,
        runtime_comb_observers,
        fused_direct_ff_writes,
        counted,
        active_commits,
        settle_flags,
        settle_partitions,
        settle_copies,
        declaration_startup,
        occurrence_probes,
        persistent_comb_state,
        inlined_nets,
        mod_vars,
        mod_path_idx,
        four_state,
    )
}
