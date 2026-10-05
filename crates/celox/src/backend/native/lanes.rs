//! Lane-partitioned kernels of the native backend.
//!
//! Every task of a partitioned kernel is compiled into its own function. The
//! functions of one lane share a private spill/scratch arena placed after the
//! arenas of all lower lanes, so concurrently running tasks never share
//! native temporary storage. Lane 0 reuses the ordinary arena: it runs on the
//! dispatching thread, and no sequential kernel runs during a partitioned one.

use std::sync::atomic::{AtomicUsize, Ordering};

use celox_runtime::parallel::{LaneSchedule, LaneTaskRunner, LaneTaskSpec};
use celox_sir_opt::parallel::Footprint;
use serde::{Deserialize, Serialize};

use super::backend::{
    CompiledNativeFunction, NativeCodeEntry, NativeCodeSymbol, NativeSimFunc, append_native_code,
    codegen_message, compile_unit_refs_inspected, native_function_at,
};
use super::jit_mem;
use crate::backend::compile_cancel::CompileCancel;
use crate::backend::lanes::LaneKernelKind;
use crate::ir::{AbsoluteAddr, LaidOutProgram};
use crate::{HashMap, SimulatorError, SimulatorOptions};

/// Alignment of the first arena of each lane.
const LANE_ARENA_ALIGN: usize = celox_state_layout::LANE_SEGMENT_ALIGN;

/// Packed-image form of every partitioned kernel.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct NativeLaneImage {
    pub(super) lanes: u32,
    pub(super) comb: Option<NativeLaneKernelImage>,
    /// Kernels of canonical events, sorted by address.
    pub(super) events: Vec<(AbsoluteAddr, NativeLaneKernelImage)>,
    /// Settle-plus-event kernels of canonical events, sorted by address.
    pub(super) fused: Vec<(AbsoluteAddr, NativeLaneKernelImage)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct NativeLaneKernelImage {
    pub(super) tasks: Vec<LaneTaskSpec>,
    /// Image offset of every task function.
    pub(super) offsets: Vec<usize>,
}

impl NativeLaneImage {
    pub(super) fn kernels(&self) -> impl Iterator<Item = &NativeLaneKernelImage> {
        self.comb.iter().chain(
            self.events
                .iter()
                .chain(&self.fused)
                .map(|(_, kernel)| kernel),
        )
    }

    pub(super) fn validate(
        &self,
        entry_offsets: &crate::HashSet<usize>,
        events: &HashMap<AbsoluteAddr, impl Sized>,
    ) -> Result<(), String> {
        if self.lanes < 2 || self.lanes > crate::MAX_SIMULATION_THREADS {
            return Err(format!("invalid lane count {}", self.lanes));
        }
        for kernel in self.kernels() {
            if kernel.tasks.len() != kernel.offsets.len() || kernel.tasks.is_empty() {
                return Err("lane kernel task table is malformed".into());
            }
            if kernel
                .offsets
                .iter()
                .any(|offset| !entry_offsets.contains(offset))
            {
                return Err("a lane task offset does not name an image entry".into());
            }
            LaneSchedule::new(self.lanes, kernel.tasks.clone())
                .map_err(|error| format!("invalid lane schedule: {error}"))?;
        }
        if self
            .events
            .iter()
            .chain(&self.fused)
            .any(|(event, _)| !events.contains_key(event))
        {
            return Err("a lane kernel names an unknown event".into());
        }
        Ok(())
    }
}

/// One partitioned kernel whose task functions are compiled but not packed.
pub(super) struct CompiledLaneKernel {
    tasks: Vec<LaneTaskSpec>,
    functions: Vec<CompiledNativeFunction>,
}

/// Every partitioned kernel of a program, compiled.
pub(super) struct CompiledLaneKernels {
    lanes: u32,
    comb: Option<CompiledLaneKernel>,
    events: Vec<(AbsoluteAddr, CompiledLaneKernel)>,
    fused: Vec<(AbsoluteAddr, CompiledLaneKernel)>,
}

impl CompiledLaneKernels {
    pub(super) fn functions(&self) -> impl Iterator<Item = &CompiledNativeFunction> {
        self.comb
            .iter()
            .chain(
                self.events
                    .iter()
                    .chain(&self.fused)
                    .map(|(_, kernel)| kernel),
            )
            .flat_map(|kernel| &kernel.functions)
    }

    /// Append every task function to the packed image.
    pub(super) fn pack(
        self,
        image: &mut Vec<u8>,
        entries: &mut Vec<NativeCodeEntry>,
        symbols: &mut Vec<NativeCodeSymbol>,
    ) -> Result<NativeLaneImage, SimulatorError> {
        let mut pack_kernel = |name: &str, kernel: CompiledLaneKernel| {
            let offsets = kernel
                .functions
                .iter()
                .enumerate()
                .map(|(task, function)| {
                    append_native_code(
                        image,
                        entries,
                        symbols,
                        format!("{name}.task[{task}]"),
                        function,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, SimulatorError>(NativeLaneKernelImage {
                tasks: kernel.tasks,
                offsets,
            })
        };
        let comb = self
            .comb
            .map(|kernel| pack_kernel("lane_eval_comb", kernel))
            .transpose()?;
        let events = self
            .events
            .into_iter()
            .enumerate()
            .map(|(index, (event, kernel))| {
                Ok((
                    event,
                    pack_kernel(&format!("lane_eval_apply_ff[{index}]"), kernel)?,
                ))
            })
            .collect::<Result<Vec<_>, SimulatorError>>()?;
        let fused = self
            .fused
            .into_iter()
            .enumerate()
            .map(|(index, (event, kernel))| {
                Ok((event, pack_kernel(&format!("lane_fused[{index}]"), kernel)?))
            })
            .collect::<Result<Vec<_>, SimulatorError>>()?;
        Ok(NativeLaneImage {
            lanes: self.lanes,
            comb,
            events,
            fused,
        })
    }
}

/// Plan and compile every partitioned kernel of `laid_out`.
pub(super) fn compile_lane_kernels(
    laid_out: &LaidOutProgram,
    options: &SimulatorOptions,
    cancel: Option<&CompileCancel>,
) -> Result<Option<CompiledLaneKernels>, SimulatorError> {
    let codegen = celox_sir_opt::parallel::CodegenFootprint::Native;
    let (lanes, planned) = crate::backend::lanes::plan_lane_kernels(laid_out, codegen)?;
    if planned.is_empty() {
        return Ok(None);
    }
    let layout = laid_out.layout();
    let semantic_size = layout
        .merged_total_size
        .checked_add(layout.triggered_bits_total_size)
        .ok_or_else(|| codegen_message("native semantic-memory size overflow"))?;
    let mut functions = planned
        .iter()
        .map(|kernel| {
            (0..kernel.tasks.len())
                .map(|_| None)
                .collect::<Vec<Option<(CompiledNativeFunction, Footprint)>>>()
        })
        .collect::<Vec<_>>();
    // Lane 0 shares the ordinary arena. Every later lane starts after the
    // largest arena used by any lower lane.
    let mut arena_floor = semantic_size;
    for lane in 0..lanes {
        let jobs = planned
            .iter()
            .enumerate()
            .flat_map(|(kernel, planned)| {
                planned
                    .tasks
                    .iter()
                    .enumerate()
                    .filter(move |(_, task)| task.lane == lane)
                    .map(move |(task, _)| (kernel, task))
            })
            .collect::<Vec<_>>();
        if jobs.is_empty() {
            continue;
        }
        let mut lane_options = options.x86_options.clone();
        lane_options.arena_base = (lane > 0).then_some(arena_floor);
        let compiled = compile_jobs(&jobs, |&(kernel, task)| {
            let units = planned[kernel].task_units(task);
            let mut footprint = None;
            let function = compile_unit_refs_inspected(
                &units,
                layout,
                options.four_state,
                "lane_task",
                None,
                &lane_options,
                false,
                &options.optimize_options.diagnostics,
                cancel,
                &mut |prepared| footprint = Some(Footprint::of_unit(prepared, layout, codegen)),
            )?;
            let footprint = footprint.unwrap_or_else(Footprint::empty);
            Ok((function, footprint))
        })?;
        let lane_end = compiled
            .iter()
            .map(|(function, _)| function.required_state_size)
            .max()
            .unwrap_or(arena_floor);
        arena_floor = arena_floor
            .max(lane_end)
            .div_ceil(LANE_ARENA_ALIGN)
            .saturating_mul(LANE_ARENA_ALIGN);
        for (&(kernel, task), function) in jobs.iter().zip(compiled) {
            functions[kernel][task] = Some(function);
        }
    }

    let mut comb = None;
    let mut events = Vec::new();
    let mut fused = Vec::new();
    for (mut planned, functions) in planned.into_iter().zip(functions) {
        let (functions, footprints): (Vec<_>, Vec<_>) = functions
            .into_iter()
            .map(|compiled| compiled.expect("every lane task was compiled"))
            .unzip();
        // Merged-SIR rewrites may change the bytes a task writes; order the
        // tasks by their final effects.
        let task_lanes = planned
            .tasks
            .iter()
            .map(|task| task.lane)
            .collect::<Vec<_>>();
        let final_tasks = celox_sir_opt::parallel::plan_fixed_tasks(lanes, &task_lanes, footprints)
            .map_err(|error| codegen_message(format!("lane-partitioned kernel: {error}")))?;
        for (task, final_task) in planned.tasks.iter_mut().zip(final_tasks) {
            task.waits = final_task.waits;
        }
        let kernel = CompiledLaneKernel {
            tasks: planned.task_specs(),
            functions,
        };
        match planned.kind {
            LaneKernelKind::Comb => comb = Some(kernel),
            LaneKernelKind::Event(event) => events.push((event, kernel)),
            LaneKernelKind::Fused(event) => fused.push((event, kernel)),
        }
    }
    Ok(Some(CompiledLaneKernels {
        lanes,
        comb,
        events,
        fused,
    }))
}

/// Compile independent jobs on up to four host threads, preserving order.
fn compile_jobs<J: Sync, T: Send>(
    jobs: &[J],
    compile: impl Fn(&J) -> Result<T, SimulatorError> + Sync,
) -> Result<Vec<T>, SimulatorError> {
    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(4)
        .min(jobs.len());
    if workers <= 1 {
        return jobs.iter().map(&compile).collect();
    }
    let next = AtomicUsize::new(0);
    let results = std::thread::scope(|scope| {
        let handles = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut compiled = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(job) = jobs.get(index) else {
                            break;
                        };
                        compiled.push((index, compile(job)?));
                    }
                    Ok::<_, SimulatorError>(compiled)
                })
            })
            .collect::<Vec<_>>();
        let mut results = Vec::with_capacity(jobs.len());
        for handle in handles {
            results.extend(
                handle
                    .join()
                    .map_err(|_| codegen_message("native lane-task compile thread panicked"))??,
            );
        }
        Ok::<_, SimulatorError>(results)
    })?;
    let mut ordered = results;
    ordered.sort_unstable_by_key(|(index, _)| *index);
    Ok(ordered.into_iter().map(|(_, function)| function).collect())
}

/// Executable form of one partitioned kernel.
pub(super) struct NativeLaneKernel {
    pub(super) schedule: LaneSchedule,
    pub(super) functions: Vec<NativeSimFunc>,
}

/// Executable partitioned kernels of a loaded image.
pub(super) struct NativeLaneKernels {
    pub(super) lanes: u32,
    pub(super) comb: Option<NativeLaneKernel>,
    /// Kernels indexed by event id.
    pub(super) events: Vec<Option<NativeLaneKernel>>,
    /// Settle-plus-event kernels indexed by event id.
    pub(super) fused: Vec<Option<NativeLaneKernel>>,
}

impl NativeLaneKernels {
    pub(super) fn materialize(
        image: &NativeLaneImage,
        code: &jit_mem::JitCode,
        event_ids: impl Fn(&AbsoluteAddr) -> Option<usize>,
        event_count: usize,
    ) -> Result<Self, SimulatorError> {
        let kernel = |kernel: &NativeLaneKernelImage| {
            Ok::<_, SimulatorError>(NativeLaneKernel {
                schedule: LaneSchedule::new(image.lanes, kernel.tasks.clone()).map_err(
                    |error| codegen_message(format!("invalid native lane schedule: {error}")),
                )?,
                functions: kernel
                    .offsets
                    .iter()
                    .map(|&offset| native_function_at(code, offset))
                    .collect::<Result<Vec<_>, _>>()?,
            })
        };
        let by_event = |kernels: &[(AbsoluteAddr, NativeLaneKernelImage)]| {
            let mut table = (0..event_count).map(|_| None).collect::<Vec<_>>();
            for (event, image) in kernels {
                let id = event_ids(event)
                    .filter(|id| *id < event_count)
                    .ok_or_else(|| codegen_message("native lane kernel names an unknown event"))?;
                table[id] = Some(kernel(image)?);
            }
            Ok::<_, SimulatorError>(table)
        };
        let events = by_event(&image.events)?;
        let fused = by_event(&image.fused)?;
        Ok(Self {
            lanes: image.lanes,
            comb: image.comb.as_ref().map(kernel).transpose()?,
            events,
            fused,
        })
    }
}

/// Runs native task functions against one simulation state image.
pub(super) struct NativeTaskRunner<'a> {
    pub(super) memory: *mut u8,
    pub(super) functions: &'a [NativeSimFunc],
}

// SAFETY: the lane schedule only runs tasks concurrently when their state
// accesses are ordered or disjoint, and each lane uses its own native arena.
unsafe impl Sync for NativeTaskRunner<'_> {}

impl LaneTaskRunner for NativeTaskRunner<'_> {
    fn run_task(&self, task: usize) -> i64 {
        // SAFETY: `memory` is the backend's live state image, sized for every
        // compiled function including the per-lane arenas.
        unsafe { (self.functions[task])(self.memory) }
    }
}
