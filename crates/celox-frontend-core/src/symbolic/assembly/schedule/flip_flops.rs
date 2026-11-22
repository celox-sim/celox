//! Scheduling the flip-flop domains of a design.

use super::*;

/// Lower source actions into the event's transition and, when requested, its
/// standalone entry. Clock and reset aliases share the scheduling work.
pub(super) fn schedule_source_updates(
    event_factory: Option<&dyn EventLoweringFactory>,
    source_inputs: Option<SourceUpdateInputs>,
    updates: &mut ScheduledUpdates,
    specialize_inputs: bool,
    four_state: bool,
    next_runtime_error_code: i64,
    flatten_timing: bool,
) -> Result<HashMap<AbsoluteAddr, Vec<VarAtomBase<RegionedAbsoluteAddr>>>, ParserError> {
    let mut direct_writes_by_event = HashMap::default();
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
    ) = (event_factory, source_inputs)
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
                    updates
                        .events
                        .entry(trigger)
                        .or_default()
                        .set(UpdateEntry::Transition, ff_only.clone());
                }
                if let Some((units, direct_ff_writes)) = fused {
                    updates
                        .events
                        .entry(trigger)
                        .or_default()
                        .set(UpdateEntry::FromInputs, units.clone());
                    direct_writes_by_event.insert(trigger, direct_ff_writes.clone());
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
            let fused = if specialize_inputs {
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
                updates
                    .events
                    .entry(trigger)
                    .or_default()
                    .set(UpdateEntry::Transition, ff_only);
            }
            if let Some((units, direct_ff_writes)) = fused {
                updates
                    .events
                    .entry(trigger)
                    .or_default()
                    .set(UpdateEntry::FromInputs, units);
                direct_writes_by_event.insert(trigger, direct_ff_writes);
            }
        }
    }
    Ok(direct_writes_by_event)
}

/// The lane-partitioned program of a parallel schedule.
pub(super) fn plan_parallel_sir(
    parallel: &ParallelScheduleOptions,
    updates: &ScheduledUpdates,
    parallel_ff_units: FfParts,
    lane0_objects: &Option<HashSet<AbsoluteAddr>>,
    parallel_eval_comb: Vec<LaneUnit<RegionedAbsoluteAddr>>,
    object_sizes: super::super::super::parallel::ObjectSizes<'_>,
) -> Option<ParallelSirProgram<AbsoluteAddr, RegionedAbsoluteAddr>> {
    parallel.enabled().then(|| {
        let mut events = updates
            .groups(UpdateEntry::Transition)
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
            observe: parallel_eval_comb,
            events: eval_apply_ffs,
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
        updates,
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
        let events: HashSet<AbsoluteAddr> = updates
            .groups(UpdateEntry::Transition)
            .map(|(event, _)| event)
            .copied()
            .chain(clock_domains.keys().copied())
            .collect();
        active_commits = split_observed_commits(
            updates,
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
