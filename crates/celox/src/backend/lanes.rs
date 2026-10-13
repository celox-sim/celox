//! Backend-independent planning of lane-partitioned kernels.
//!
//! Compiled backends turn every planned task into one generated function and
//! execute the tasks with [`celox_runtime::parallel::LanePool`].

use celox_runtime::parallel::LaneTaskSpec;
use celox_sir::LaneUnit;
use celox_sir_opt::parallel::LaneTask;

use crate::ir::{AbsoluteAddr, ExecutionUnit, LaidOutProgram, RegionedAbsoluteAddr};

/// The simulation phase a partitioned kernel implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaneKernelKind {
    /// The combinational settle.
    Comb,
    /// The FF update of one canonical event.
    Event(AbsoluteAddr),
    /// The combinational settle followed by one event's FF update, as one
    /// kernel: a lane starts its FF work as soon as the settle work it
    /// depends on is done instead of after every lane's settle.
    Fused(AbsoluteAddr),
}

/// One partitioned kernel with its concurrency plan.
pub(crate) struct PlannedLaneKernel<'a> {
    pub(crate) kind: LaneKernelKind,
    pub(crate) diagnostic_id: String,
    pub(crate) diagnostic_program: String,
    units: Vec<&'a LaneUnit<RegionedAbsoluteAddr>>,
    pub(crate) tasks: Vec<LaneTask>,
    /// Index of an earlier planned kernel with the same units, whose
    /// compiled tasks this kernel reuses. Events that trigger the same FF
    /// units, such as a clock and an asynchronous reset of one domain, would
    /// otherwise compile identical code twice.
    pub(crate) same_as: Option<usize>,
}

impl<'a> PlannedLaneKernel<'a> {
    /// Link the final runtime schedule to the units from which it was made.
    pub(crate) fn trace_schedule(&self, hazard_level: &str) {
        if !tracing::enabled!(target: "celox::parallel", tracing::Level::TRACE) {
            return;
        }
        let schedule = &self.diagnostic_id;
        let program = &self.diagnostic_program;
        for (task, spec) in self.tasks.iter().enumerate() {
            tracing::trace!(target: "celox::parallel", event = "compiled_task", schedule, program, hazard_level,
                kind = ?self.kind, task, lane = spec.lane, units = ?spec.items,
                waits = ?spec.waits.iter().map(|wait| [wait.lane, wait.completed_tasks]).collect::<Vec<_>>(),
                sir = ?self.task_units(task));
        }
    }

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
    let program = celox_runtime::parallel::new_diagnostic_id();
    if tracing::enabled!(target: "celox::parallel", tracing::Level::TRACE) {
        let layout = laid_out.layout();
        for (address, offset) in &layout.offsets {
            tracing::trace!(target: "celox::parallel", event = "signal", program, address = %address,
                name = laid_out.runtime().design.get_path(address), instance = address.instance_id.0,
                object = address.var_id.0, offset, width = layout.widths.get(address).copied().unwrap_or(0),
                working_offset = ?layout.working_offsets.get(address).map(|offset| layout.working_base_offset + offset),
                sparse_offset = ?layout.sparse_offsets.get(address).map(|offset| layout.sparse_base_offset + offset));
        }
    }
    if lanes < 2 {
        return Ok((1, Vec::new()));
    }
    let mut candidates: Vec<(LaneKernelKind, Vec<&LaneUnit<RegionedAbsoluteAddr>>)> = Vec::new();
    if !parallel.eval_comb.is_empty() {
        candidates.push((LaneKernelKind::Comb, parallel.eval_comb.iter().collect()));
    }
    let mut events = parallel.eval_apply_ffs.keys().copied().collect::<Vec<_>>();
    events.sort_unstable();
    for &event in &events {
        candidates.push((
            LaneKernelKind::Event(event),
            parallel.eval_apply_ffs[&event].units().collect(),
        ));
    }
    if !parallel.eval_comb.is_empty() {
        for &event in &events {
            candidates.push((
                LaneKernelKind::Fused(event),
                parallel
                    .eval_comb
                    .iter()
                    .chain(parallel.eval_apply_ffs[&event].units())
                    .collect(),
            ));
        }
    }
    let mut planned: Vec<PlannedLaneKernel<'_>> = Vec::new();
    for (kind, units) in candidates {
        let diagnostic_id = planned
            .iter()
            .find(|kernel| kernel.units == units)
            .map(|kernel| kernel.diagnostic_id.clone())
            .unwrap_or_else(celox_runtime::parallel::new_diagnostic_id);
        let _diagnostic = tracing::trace_span!(target: "celox::parallel", "kernel_plan", program = program.as_str(), schedule = diagnostic_id.as_str(), hazard_level = "units", kind = ?kind, codegen = ?codegen).entered();
        if let Some(index) = planned.iter().position(|kernel| kernel.units == units) {
            let tasks = planned[index].tasks.clone();
            planned.push(PlannedLaneKernel {
                kind,
                diagnostic_id,
                diagnostic_program: program.clone(),
                units,
                tasks,
                same_as: Some(index),
            });
            continue;
        }
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
            "[parallel] kernel={kind:?} units={} tasks={} waits={} lanes={used_lanes:?}",
            units.len(),
            tasks.len(),
            tasks.iter().map(|task| task.waits.len()).sum::<usize>(),
        );
        if used_lanes.len() > 1 {
            planned.push(PlannedLaneKernel {
                kind,
                diagnostic_id,
                diagnostic_program: program.clone(),
                units,
                tasks,
                same_as: None,
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
    // A capture subscriber can open a bounded window before work starts,
    // without tracing worker tasks during selector calibration.
    let _capture_scope =
        tracing::trace_span!(target: "celox::parallel::call", "kernel_selection").entered();
    tracing::trace!(target: "celox::parallel::call", event = "kernel_call", slot = ?slot);
    let (choice, timed) = selectors(backend).selector(slot, selection).next();
    let diagnostic = tracing::enabled!(target: "celox::parallel", tracing::Level::TRACE);
    let start = (timed || diagnostic).then(std::time::Instant::now);
    let result = match choice {
        KernelChoice::Parallel => parallel(backend),
        KernelChoice::Sequential => sequential(backend),
    };
    let selector = selectors(backend).selector(slot, selection);
    if diagnostic {
        tracing::trace!(target: "celox::parallel", event = "kernel_choice", slot = ?slot,
            choice = ?choice, calibrating = timed,
            elapsed_ns = start.map_or(0, |start| start.elapsed().as_nanos() as u64));
    }
    match start {
        Some(start) if timed => {
            let nanos = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
            selector.record(choice, nanos);
            if let Some(decision) = selector.decided() {
                tracing::debug!(target: "celox::parallel::selection", event = "selector_slot",
                    selector = selector.diagnostic_id(), slot = ?slot, decision = ?decision);
            }
        }
        _ => selector.advance(1),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use celox_runtime::parallel::KernelSelection;

    #[test]
    fn selector_identity_survives_table_growth_and_is_unique_per_simulator() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..2 {
            let mut selectors = LaneSelectors::default();
            for slot in [LaneKernelSlot::Event(0), LaneKernelSlot::Fused(0)] {
                let id = selectors
                    .selector(slot, KernelSelection::Measured)
                    .diagnostic_id()
                    .to_owned();
                assert!(seen.insert(id.clone()));
                // Grow both tables after recording the first slot's identity.
                for index in 1..64 {
                    selectors.selector(LaneKernelSlot::Event(index), KernelSelection::Measured);
                    selectors.selector(LaneKernelSlot::Fused(index), KernelSelection::Measured);
                }
                while selectors.decided(slot).is_none() {
                    run_selected(
                        &mut selectors,
                        |selectors| selectors,
                        slot,
                        KernelSelection::Measured,
                        |_| (),
                        |_| (),
                    );
                }
                assert_eq!(
                    selectors
                        .selector(slot, KernelSelection::Measured)
                        .diagnostic_id(),
                    id
                );
            }
        }
    }
}
