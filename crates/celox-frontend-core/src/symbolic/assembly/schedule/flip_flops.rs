//! Scheduling the flip-flop domains of a design.

use super::*;

/// Schedule the flip-flop actions of every event through the fused factory,
/// if there is one: alone where an event has several, replacing its units in
/// `eval_apply_ffs`, and together with the combinational program where the
/// ticks are merged. Returns the merged units and the ranges their
/// flip-flops write directly, by event.
pub(super) fn schedule_fused_ffs(
    fused_ff_factory: Option<&dyn FusedFfLoweringFactory>,
    fused_inputs: Option<FusedInputs>,
    eval_apply_ffs: &mut FfUnits,
    merge_comb_apply: bool,
    four_state: bool,
    next_runtime_error_code: i64,
    flatten_timing: bool,
) -> Result<
    (
        FfUnits,
        HashMap<AbsoluteAddr, Vec<VarAtomBase<RegionedAbsoluteAddr>>>,
    ),
    ParserError,
> {
    let mut eval_comb_apply_ffs = HashMap::default();
    let mut fused_direct_ff_writes = HashMap::default();
    if let (
        Some(factory),
        Some((
            actions,
            clock_arena,
            clock_comb_blocks,
            clock_var_widths,
            clock_unpacked_element_widths,
            clock_ignored_loops,
            clock_true_loops,
        )),
    ) = (fused_ff_factory, fused_inputs)
    {
        let mut fused_schedule_cache = HashMap::<
            Vec<usize>,
            (
                Option<Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
                Option<(
                    Vec<ExecutionUnit<RegionedAbsoluteAddr>>,
                    Vec<VarAtomBase<RegionedAbsoluteAddr>>,
                )>,
            ),
        >::default();
        let schedule = |paths, actions| {
            let mut ff_lowering = factory.create(actions)?;
            match scheduler::sort_clock(
                paths,
                &clock_arena,
                &clock_ignored_loops,
                &clock_true_loops,
                four_state,
                &clock_var_widths,
                &clock_unpacked_element_widths,
                next_runtime_error_code,
                ff_lowering.as_mut(),
            ) {
                Ok(schedule) => Ok(schedule),
                Err(scheduler::ClockSortError::Lowering(error)) => Err(error),
                Err(scheduler::ClockSortError::Scheduler(error)) => {
                    let mut error_arena = SLTNodeArena::new();
                    let error =
                        error.map_addr(&clock_arena, &mut error_arena, &|addr| addr.to_string())?;
                    Err(ParserError::Scheduler(error))
                }
            }
        };
        for (trigger, actions) in actions {
            let action_ids = actions.iter().map(|action| action.id).collect::<Vec<_>>();
            if let Some((ff_only, fused)) = fused_schedule_cache.get(&action_ids) {
                if let Some(ff_only) = ff_only {
                    eval_apply_ffs.insert(trigger, ff_only.clone());
                }
                if let Some((units, direct_ff_writes)) = fused {
                    eval_comb_apply_ffs.insert(trigger, units.clone());
                    fused_direct_ff_writes.insert(trigger, direct_ff_writes.clone());
                }
                continue;
            }
            let fused_start = flatten_timing.then(std::time::Instant::now);
            // The ordinary tick already settled comb. Reuse the existing
            // old-state dependency planner with no comb paths: acyclic readers
            // run before writers, and only cyclic/aliased ranges need staging.
            // Keep the merged eval-then-apply fallback for adapters without
            // source-aware lowering and the existing single-action fast path.
            let ff_only = if actions.len() > 1 {
                Some(schedule(Vec::new(), actions.clone())?.execution_units)
            } else {
                None
            };
            let fused = if merge_comb_apply {
                let fused = schedule(clock_comb_blocks.clone(), actions)?;
                Some((fused.execution_units, fused.direct_ff_writes))
            } else {
                None
            };
            if let Some(start) = fused_start {
                tracing::debug!("[flatten] scheduler::sort_clock: {:?}", start.elapsed());
            }
            fused_schedule_cache.insert(action_ids, (ff_only.clone(), fused.clone()));
            if let Some(ff_only) = ff_only {
                eval_apply_ffs.insert(trigger, ff_only);
            }
            if let Some((units, direct_ff_writes)) = fused {
                eval_comb_apply_ffs.insert(trigger, units);
                fused_direct_ff_writes.insert(trigger, direct_ff_writes);
            }
        }
    }
    Ok((eval_comb_apply_ffs, fused_direct_ff_writes))
}

/// The lane-partitioned program of a parallel schedule.
pub(super) fn plan_parallel_sir(
    parallel: &ParallelScheduleOptions,
    eval_apply_ffs: &FfUnits,
    parallel_ff_units: FfParts,
    lane0_objects: &Option<HashSet<AbsoluteAddr>>,
    parallel_eval_comb: Vec<LaneUnit<RegionedAbsoluteAddr>>,
    object_sizes: super::super::super::parallel::ObjectSizes<'_>,
) -> Option<ParallelSirProgram<AbsoluteAddr, RegionedAbsoluteAddr>> {
    parallel.enabled().then(|| {
        let mut events = eval_apply_ffs
            .iter()
            .filter(|(_, units)| !units.is_empty())
            .map(|(event, _)| *event)
            .collect::<Vec<_>>();
        events.sort_unstable();
        let eval_apply_ffs = super::super::super::parallel::plan_parallel_ff_kernels(
            &events,
            parallel_ff_units,
            lane0_objects
                .as_ref()
                .expect("parallel scheduling computes lane-0 objects"),
            &parallel_eval_comb,
            object_sizes,
            parallel,
        );
        for (event, kernel) in &eval_apply_ffs {
            let lanes = kernel
                .units()
                .map(|unit| unit.lane)
                .collect::<BTreeSet<_>>();
            tracing::debug!(
                "[parallel] event={event} evaluations={} applications={} lanes={lanes:?}",
                kernel.evaluations.len(),
                kernel.applications.len(),
            );
        }
        ParallelSirProgram {
            lanes: parallel.lanes,
            eval_comb: parallel_eval_comb,
            eval_apply_ffs,
        }
    })
}

/// Commit the observed blocking results of the domains of a design with
/// process kernels in the active region. Returns those domains.
pub(super) fn commit_observed_domains(
    units: &mut Units,
    split_commits: SplitCommits,
    comb_sources: &HashMap<AbsoluteAddr, HashSet<AbsoluteAddr>>,
    clock_domains: &HashMap<AbsoluteAddr, AbsoluteAddr>,
) -> Vec<AbsoluteAddr> {
    let Units {
        eval_apply_ffs,
        eval_only_ffs,
        apply_ffs,
        processes,
        timing,
        ..
    } = units;

    // With process kernels, a domain whose blocking results another unit
    // observes commits them with its evaluation, in the active region, and
    // only its nonblocking results in the NBA region (IEEE 1800-2023 4.5,
    // 10.4). Every other domain commits both together, as before.
    let mut active_commits = Vec::new();
    if !processes.is_empty() {
        let events: HashSet<AbsoluteAddr> = eval_apply_ffs
            .keys()
            .copied()
            .chain(clock_domains.keys().copied())
            .collect();
        active_commits = split_observed_commits(
            eval_only_ffs,
            apply_ffs,
            split_commits,
            comb_sources,
            processes.iter().map(|process| &process.kernel),
            timing.wait_sites.iter().flat_map(WaitSite::reads),
            &events,
            clock_domains,
        );
    }
    active_commits
}
