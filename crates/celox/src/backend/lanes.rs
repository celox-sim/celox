//! Backend-independent planning of lane-partitioned kernels.
//!
//! Compiled backends turn every planned task into one generated function and
//! execute the tasks with [`celox_runtime::parallel::LanePool`].

use celox_runtime::parallel::LaneTaskSpec;
use celox_sir::LaneUnit;
use celox_sir_opt::parallel::LaneTask;

use crate::ir::{AbsoluteAddr, ExecutionUnit, LaidOutProgram, RegionedAbsoluteAddr};

/// One partitioned kernel with its concurrency plan.
pub(crate) struct PlannedLaneKernel<'a> {
    /// `None` for the combinational settle, otherwise the canonical event.
    pub(crate) event: Option<AbsoluteAddr>,
    units: Vec<&'a LaneUnit<RegionedAbsoluteAddr>>,
    pub(crate) tasks: Vec<LaneTask>,
}

impl<'a> PlannedLaneKernel<'a> {
    /// Units of one task, in execution order.
    pub(crate) fn task_units(&self, task: usize) -> Vec<&'a ExecutionUnit<RegionedAbsoluteAddr>> {
        self.tasks[task]
            .items
            .iter()
            .map(|&unit| &self.units[unit].unit)
            .collect()
    }

    /// Runtime form of the task table.
    pub(crate) fn task_specs(&self) -> Vec<LaneTaskSpec> {
        self.tasks
            .iter()
            .map(|task| LaneTaskSpec {
                lane: task.lane,
                waits: task
                    .waits
                    .iter()
                    .map(|wait| (wait.lane, wait.completed_tasks))
                    .collect(),
            })
            .collect()
    }
}

/// Plan every partitioned kernel of `laid_out`.
///
/// Kernels whose final dependencies leave all work in one lane are dropped:
/// their sequential function is faster.
pub(crate) fn plan_lane_kernels(
    laid_out: &LaidOutProgram,
    codegen: celox_sir_opt::parallel::CodegenFootprint,
) -> Result<(u32, Vec<PlannedLaneKernel<'_>>), crate::SimulatorError> {
    let Some(parallel) = &laid_out.sir.parallel else {
        return Ok((1, Vec::new()));
    };
    let lanes = parallel.lanes;
    if lanes < 2 {
        return Ok((1, Vec::new()));
    }
    let mut candidates: Vec<(Option<AbsoluteAddr>, Vec<&LaneUnit<RegionedAbsoluteAddr>>)> =
        Vec::new();
    if !parallel.eval_comb.is_empty() {
        candidates.push((None, parallel.eval_comb.iter().collect()));
    }
    let mut events = parallel.eval_apply_ffs.keys().copied().collect::<Vec<_>>();
    events.sort_unstable();
    for event in events {
        candidates.push((
            Some(event),
            parallel.eval_apply_ffs[&event].units().collect(),
        ));
    }
    let mut planned = Vec::new();
    for (event, units) in candidates {
        let tasks = celox_sir_opt::parallel::plan_parallel_kernel(
            &units,
            laid_out.layout(),
            lanes,
            codegen,
        )
        .map_err(|error| {
            crate::SimulatorError::from(crate::CodegenError::message(format!(
                "lane-partitioned kernel: {error}"
            )))
        })?;
        let used_lanes = tasks
            .iter()
            .map(|task| task.lane)
            .collect::<std::collections::BTreeSet<_>>();
        tracing::debug!(
            "[parallel] kernel={} units={} tasks={} waits={} lanes={used_lanes:?}",
            event.map_or_else(|| "eval_comb".to_string(), |event| event.to_string()),
            units.len(),
            tasks.len(),
            tasks.iter().map(|task| task.waits.len()).sum::<usize>(),
        );
        if used_lanes.len() > 1 {
            planned.push(PlannedLaneKernel {
                event,
                units,
                tasks,
            });
        }
    }
    Ok((lanes, planned))
}

/// Runtime selection policy for a compiled program.
pub(crate) fn kernel_selection(
    options: &crate::SimulatorOptions,
) -> celox_runtime::parallel::KernelSelection {
    if options.optimize_options.parallel_partition() == crate::ParallelPartition::Always {
        celox_runtime::parallel::KernelSelection::Parallel
    } else {
        celox_runtime::parallel::KernelSelection::Measured
    }
}

/// A kernel whose partitioned and sequential implementations compete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaneKernelSlot {
    Comb,
    Event(usize),
    /// Combinational settle followed by one event, against the fused
    /// sequential function.
    Fused(usize),
}

/// Per-kernel selection state of one simulator instance.
#[derive(Debug, Default)]
pub(crate) struct LaneSelectors {
    comb: Option<celox_runtime::parallel::KernelSelector>,
    events: Vec<Option<celox_runtime::parallel::KernelSelector>>,
    fused: Vec<Option<celox_runtime::parallel::KernelSelector>>,
}

impl LaneSelectors {
    /// Account for `calls` executions of a kernel's decided implementation
    /// that ran without going through [`run_selected`], such as a native
    /// tick loop.
    pub(crate) fn advance(&mut self, slot: LaneKernelSlot, calls: u64) {
        let entry = match slot {
            LaneKernelSlot::Comb => self.comb.as_mut(),
            LaneKernelSlot::Event(id) => self.events.get_mut(id).and_then(Option::as_mut),
            LaneKernelSlot::Fused(id) => self.fused.get_mut(id).and_then(Option::as_mut),
        };
        if let Some(selector) = entry {
            selector.advance(calls);
        }
    }

    /// The settled choice of one kernel, if its selector has decided.
    pub(crate) fn decided(
        &self,
        slot: LaneKernelSlot,
    ) -> Option<celox_runtime::parallel::KernelChoice> {
        let entry = match slot {
            LaneKernelSlot::Comb => self.comb.as_ref(),
            LaneKernelSlot::Event(id) => self.events.get(id).and_then(Option::as_ref),
            LaneKernelSlot::Fused(id) => self.fused.get(id).and_then(Option::as_ref),
        };
        entry.and_then(celox_runtime::parallel::KernelSelector::decided)
    }

    fn selector(
        &mut self,
        slot: LaneKernelSlot,
        selection: celox_runtime::parallel::KernelSelection,
    ) -> &mut celox_runtime::parallel::KernelSelector {
        let entry = match slot {
            LaneKernelSlot::Comb => &mut self.comb,
            LaneKernelSlot::Event(id) | LaneKernelSlot::Fused(id) => {
                let table = if matches!(slot, LaneKernelSlot::Event(_)) {
                    &mut self.events
                } else {
                    &mut self.fused
                };
                if table.len() <= id {
                    table.resize_with(id + 1, || None);
                }
                &mut table[id]
            }
        };
        entry.get_or_insert_with(|| celox_runtime::parallel::KernelSelector::new(selection))
    }
}

/// Run one kernel through its selector, timing it while the selector
/// calibrates.
pub(crate) fn run_selected<B, R>(
    backend: &mut B,
    selectors: impl Fn(&mut B) -> &mut LaneSelectors,
    slot: LaneKernelSlot,
    selection: celox_runtime::parallel::KernelSelection,
    parallel: impl FnOnce(&mut B) -> R,
    sequential: impl FnOnce(&mut B) -> R,
) -> R {
    use celox_runtime::parallel::KernelChoice;
    let (choice, timed) = selectors(backend).selector(slot, selection).next();
    let start = timed.then(std::time::Instant::now);
    let result = match choice {
        KernelChoice::Parallel => parallel(backend),
        KernelChoice::Sequential => sequential(backend),
    };
    let selector = selectors(backend).selector(slot, selection);
    match start {
        Some(start) => {
            let nanos = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
            selector.record(choice, nanos);
        }
        None => selector.advance(1),
    }
    result
}
