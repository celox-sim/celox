//! Runs of a design's compiled scheduler: the time slots it can take over
//! from the reference loop, and the hand-over of the scheduling state.

use std::cmp::Reverse;

use celox_design::SchedulerExit;

use super::{Baseline, CompiledSlots, SchedulerMode, SimulationExecutor, SimulationState};
use crate::{
    SimulatorErrorCode,
    backend::SimBackend,
    timing::{Halt, copy_planes},
};

impl<B: SimBackend> SimulationState<B> {
    /// Whether a compiled scheduler may run slots of this simulation.
    pub(super) fn compiles_slots(&self) -> bool {
        self.model.scheduler.is_some() && self.stats.mode != SchedulerMode::Reference
    }

    /// The limit of a run of the compiled scheduler that may start now and
    /// end by `end`, running one slot only if `one_slot`, or `None` when the
    /// next slot needs the reference loop.
    ///
    /// A run takes the slots due to the kernels the scheduler runs while
    /// nothing else is pending: no process waits for an event or is ready,
    /// no nonblocking assignment is queued, no host change awaits a settle,
    /// and the slot comes before every scheduled event and every wakeup of
    /// another kernel.
    pub(super) fn compiled_limit<E>(&self, executor: &E, end: u64, one_slot: bool) -> Option<u64>
    where
        E: SimulationExecutor<Backend = B>,
    {
        let scheduler = self.model.scheduler.as_ref()?;
        let pending = &self.process_state;
        if self.finished
            || self.stopped
            || self.suspended.is_some()
            || self.declarations_pending
            || self.host_pending
            || !pending.ready.is_empty()
            || !pending.inactive.is_empty()
            || !pending.nba_queue.is_empty()
            || pending
                .waiters
                .iter()
                .any(|(process, _)| scheduler.waits.get(process).is_none_or(Option::is_none))
        {
            return None;
        }
        let mut first = u64::MAX;
        let mut other = u64::MAX;
        for &Reverse((time, process)) in self.process_wakeups.iter() {
            if scheduler.wake.get(process).is_some_and(Option::is_some) {
                first = first.min(time);
            } else {
                other = other.min(time);
            }
        }
        let reference = other.min(self.scheduler.next_event_time().unwrap_or(u64::MAX));
        // Time zero starts processes in their own order; the reference loop
        // runs it.
        if first == u64::MAX || first == 0 || first > end || first >= reference {
            return None;
        }
        let one_slot = match executor.compiled_slots() {
            CompiledSlots::None => return None,
            CompiledSlots::One => true,
            CompiledSlots::Any => one_slot,
        };
        Some(if one_slot {
            first
        } else {
            end.min(reference - 1)
        })
    }

    /// Run the compiled scheduler for the slots up to `limit`. Returns, like
    /// [`Self::step_until`], the time of the last slot run.
    pub(super) fn run_compiled<E>(
        &mut self,
        executor: &mut E,
        limit: u64,
    ) -> Result<Option<u64>, SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        self.stopped = false;
        match self.run_compiled_slots(executor, limit) {
            Ok(time) => Ok(Some(time)),
            Err(Halt::Finish) => {
                self.finalize_terminal(executor)?;
                Ok(Some(self.scheduler.time))
            }
            Err(Halt::Stop) => Ok(Some(self.scheduler.time)),
            Err(Halt::Error(error)) => {
                let _ = self.finish_timed_boundary(executor, self.scheduler.time);
                let _ = executor.flush_output();
                Err(error)
            }
        }
    }

    fn run_compiled_slots<E>(&mut self, executor: &mut E, limit: u64) -> Result<u64, Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        let scheduler = self
            .model
            .scheduler
            .as_ref()
            .ok_or(SimulatorErrorCode::InternalError)?;
        let backend = executor.backend_mut();
        backend.set(scheduler.limit, limit);
        backend.set(scheduler.exit, 0u8);
        backend.set(scheduler.finish, 0u8);
        backend.set(scheduler.count, 0u64);
        for &(_, waiting) in scheduler.wake.iter().flatten() {
            backend.set(waiting, 0u8);
        }
        backend.set(scheduler.ready, 0u64);
        // The scheduler's waiting kernels wait in its slots during the run,
        // with the level each wait last saw.
        for (process, slots) in scheduler.waits.iter().enumerate() {
            let Some((waiting, baseline)) = *slots else {
                continue;
            };
            let level = match self.process_state.waiters.get(process) {
                Some(waiter) => match waiter.baselines.first() {
                    Some(Baseline::Raw(raw)) => raw.first().is_some_and(|byte| byte & 1 != 0),
                    Some(Baseline::Value((value, _))) => value.bit(0),
                    _ => return Err(SimulatorErrorCode::InternalError.into()),
                },
                None => {
                    backend.set(waiting, 0u8);
                    continue;
                }
            };
            backend.set(waiting, 1u8);
            backend.set(baseline, u8::from(level));
            self.process_state.waiters.remove(process);
        }
        // The scheduler's kernels wait in its wake slots during the run.
        let mut kept = Vec::with_capacity(self.process_wakeups.len());
        for wakeup @ Reverse((time, process)) in self.process_wakeups.drain() {
            match scheduler.wake.get(process).copied().flatten() {
                Some((wake, waiting)) => {
                    backend.set(wake, time);
                    backend.set(waiting, 1u8);
                }
                None => kept.push(wakeup),
            }
        }
        self.process_wakeups.extend(kept);
        let unit = scheduler.unit;
        let result = executor.run_process(unit);

        let scheduler = self
            .model
            .scheduler
            .as_ref()
            .ok_or(SimulatorErrorCode::InternalError)?;
        let backend = executor.backend();
        let count: u64 = backend.get_as(scheduler.count);
        let now: u64 = backend.get_as(scheduler.now);
        let exit = SchedulerExit::from_code(backend.get_as(scheduler.exit));
        let current: u64 = backend.get_as(scheduler.current);
        let ready: u64 = backend.get_as(scheduler.ready);
        let complete = result.is_ok() && exit == Some(SchedulerExit::Limit);
        // A run that returned in the middle of slot `now` leaves the kernel
        // that ended it filed nowhere, as the reference loop leaves it, and
        // the kernels still ready in the slot ready.
        let ordinals = &scheduler.ordinals;
        let is_ready = |process: usize| {
            ordinals
                .get(process)
                .copied()
                .flatten()
                .is_some_and(|ordinal| ready >> ordinal & 1 != 0)
        };
        let pending =
            |process: usize| !complete && (process as u64 == current || is_ready(process));
        for (process, wake) in scheduler.wake.iter().enumerate() {
            let Some((wake, waiting)) = *wake else {
                continue;
            };
            if pending(process) || backend.get_as::<u8>(waiting) == 0 {
                continue;
            }
            let time: u64 = backend.get_as(wake);
            self.process_wakeups.push(Reverse((time, process)));
        }
        for (process, slots) in scheduler.waits.iter().enumerate() {
            let Some((waiting, baseline)) = *slots else {
                continue;
            };
            if pending(process) || backend.get_as::<u8>(waiting) == 0 {
                continue;
            }
            let site = backend.get_as::<u64>(self.model.processes[process].delay) as usize;
            let terms = self.model.processes[process]
                .wait_sites
                .get(site)
                .ok_or_else(|| Self::site_error(process, site))?;
            let level = [backend.get_as::<u8>(baseline)];
            // The slot keeps its storage, as `register_waiter` reuses it.
            self.process_state.waiters.register(
                process,
                terms.iter().any(|term| term.driven),
                |waiter| {
                    waiter.site = site;
                    waiter.baselines.truncate(1);
                    match waiter.baselines.first_mut() {
                        Some(Baseline::Raw(raw)) => copy_planes(raw, &level, 1),
                        _ => {
                            let mut raw = Vec::with_capacity(1);
                            copy_planes(&mut raw, &level, 1);
                            waiter.baselines.clear();
                            waiter.baselines.push(Baseline::Raw(raw));
                        }
                    }
                },
            );
        }
        if !complete {
            for process in 0..ordinals.len() {
                if is_ready(process) && process as u64 != current {
                    self.process_state.ready.insert(process);
                }
            }
        }
        self.stats.compiled_slots += count;
        if count > 0 {
            self.scheduler.time = now;
        }
        if complete {
            // The slots ended with their levels as the baseline of the next.
            self.resample_clock_values(executor.backend());
        } else if count > 0 {
            // The baseline is the levels slot `now` started from; events the
            // kernels do not write kept theirs.
            let levels: u64 = executor.backend().get_as(scheduler.levels);
            for (bit, &id) in scheduler.events.clone().iter().enumerate() {
                self.record_level(id, u8::from(levels >> bit & 1 != 0));
            }
        }
        // Domains mark trigger bits as they run; between slots none is set.
        executor.backend_mut().clear_triggered_bits();
        result?;
        match exit {
            Some(SchedulerExit::Limit) => {}
            Some(SchedulerExit::Finish) => return Err(Halt::Finish),
            Some(SchedulerExit::DelayOverflow) => {
                return Err(Halt::Error(SimulatorErrorCode::Runtime {
                    message: format!("process {current} delay overflows simulation time"),
                    signals: Vec::new(),
                }));
            }
            Some(SchedulerExit::Internal) | None => {
                return Err(SimulatorErrorCode::InternalError.into());
            }
        }
        executor.try_finish_timed_step(now)?;
        Ok(now)
    }
}
