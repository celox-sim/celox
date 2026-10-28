//! Runs of a design's compiled scheduler: the time slots it can take over
//! from the reference loop, and the hand-over of the scheduling state.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use celox_design::SchedulerExit;

use super::{Baseline, CompiledSlots, SchedulerMode, SimulationExecutor, SimulationState};
use crate::{
    SimulatorErrorCode,
    backend::{EventHandle, SimBackend},
    schedule::SchedulerRefs,
    scheduler::{ClockDef, SimEvent},
    timing::{Halt, copy_planes},
};

/// The clock slots of `scheduler` whose host clock it runs: the periodic
/// clocks with exactly one queued edge, one bit per clock slot.
fn compiled_clocks<B: SimBackend>(
    scheduler: &SchedulerRefs,
    queue: &BinaryHeap<SimEvent<B>>,
    clocks: &[Option<ClockDef>],
) -> u64 {
    let mut seen = 0u64;
    let mut twice = 0u64;
    for event in queue {
        if let Some(bit) = clock_slot(scheduler, event, clocks) {
            twice |= seen & 1 << bit;
            seen |= 1 << bit;
        }
    }
    seen & !twice
}

/// The clock slot of `scheduler` that may run the queued `event`.
fn clock_slot<B: SimBackend>(
    scheduler: &SchedulerRefs,
    event: &SimEvent<B>,
    clocks: &[Option<ClockDef>],
) -> Option<usize> {
    let id = event.event_ref.id();
    if !event.periodic || clocks.get(id).is_none_or(Option::is_none) {
        return None;
    }
    scheduler.clock_of(id, event.signal)
}

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
            || !pending.observed.is_empty()
            || !pending.ready.is_empty()
            || !pending.inactive.is_empty()
            || !pending.nba_queue.is_empty()
            // A derived clock changes with the slots of its sources, which
            // the scheduler does not tell apart.
            || self.observed_events & scheduler.derived_events != 0
            || pending.waiters.iter().any(|(process, _)| {
                scheduler.waits.get(process).is_none_or(Option::is_none)
                    || self.writes_observed(scheduler, process)
            })
        {
            return None;
        }
        let mut first = u64::MAX;
        let mut other = u64::MAX;
        for &Reverse((time, process)) in self.process_wakeups.iter() {
            if scheduler.wake.get(process).is_some_and(Option::is_some)
                && !self.writes_observed(scheduler, process)
            {
                first = first.min(time);
            } else {
                other = other.min(time);
            }
        }
        // The host clocks the scheduler runs are its work; every other
        // scheduled event is the reference loop's.
        let queue = &self.scheduler.event_queue;
        if scheduler.clocks.is_empty() {
            other = other.min(self.scheduler.next_event_time().unwrap_or(u64::MAX));
        } else {
            let clocks = &self.scheduler.clocks;
            let compiled = compiled_clocks(scheduler, queue, clocks);
            for event in queue {
                if clock_slot(scheduler, event, clocks).is_some_and(|bit| {
                    compiled >> bit & 1 != 0
                        && scheduler.clock_events[bit] & self.observed_events == 0
                }) {
                    first = first.min(event.time);
                } else {
                    other = other.min(event.time);
                }
            }
        }
        let reference = other;
        // Time zero starts processes in their own order; the reference loop
        // runs it.
        if first == u64::MAX || first == 0 || first > end || first >= reference {
            return None;
        }
        let one_slot = match executor.compiled_slots() {
            CompiledSlots::None => return None,
            CompiledSlots::One => true,
            CompiledSlots::Any => one_slot || self.one_slot_runs(),
        };
        Some(if one_slot {
            first
        } else {
            end.min(reference - 1)
        })
    }

    /// Whether `process` may change an event the host observes, so that
    /// its slots run in the reference loop.
    fn writes_observed(&self, scheduler: &SchedulerRefs, process: usize) -> bool {
        scheduler
            .kernel_events
            .get(process)
            .is_some_and(|&events| events & self.observed_events != 0)
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
                if matches!(&error, SimulatorErrorCode::Fatal { .. }) {
                    let _ = self.finalize_terminal(executor);
                    return Err(error);
                }
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
        // Designs with sampled values are limited to one slot, so `limit`
        // is that slot's timestamp. Keep its baseline on same-time reentry.
        if self.one_slot_runs() && limit > self.scheduler.time {
            self.sample_values(executor.backend_mut());
        }
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
        // The host clocks it runs wait in its clock slots during the run.
        let mut clocks = Vec::new();
        if !scheduler.clocks.is_empty() {
            let queue = &mut self.scheduler.event_queue;
            let compiled = compiled_clocks(scheduler, queue, &self.scheduler.clocks);
            clocks.resize(scheduler.clocks.len(), None);
            let mut values = 0u64;
            for event in queue.iter() {
                let Some(bit) = clock_slot(scheduler, event, &self.scheduler.clocks)
                    .filter(|&bit| compiled >> bit & 1 != 0)
                else {
                    continue;
                };
                let period = self.scheduler.clocks[event.event_ref.id()]
                    .as_ref()
                    .map_or(2, |clock| clock.period.max(2));
                backend.set(
                    SchedulerRefs::clock_element(scheduler.clock_next, bit),
                    event.time,
                );
                backend.set(
                    SchedulerRefs::clock_element(scheduler.clock_period, bit),
                    period,
                );
                values |= u64::from(event.next_val != 0) << bit;
                clocks[bit] = Some(event.event_ref);
            }
            queue.retain(|event| {
                clock_slot(scheduler, event, &self.scheduler.clocks)
                    .is_none_or(|bit| compiled >> bit & 1 == 0)
            });
            backend.set(scheduler.clock_armed, compiled);
            backend.set(scheduler.clock_value, values);
        }
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
        // A clock that overflows ends the run after its slot completed.
        let slot_complete = complete || exit == Some(SchedulerExit::ClockOverflow);
        let overflowed = scheduler
            .clocks
            .get(backend.get_as::<u64>(scheduler.exit_arg) as usize)
            .map(|&(event, _)| event);
        // Armed clocks wait for their next edge in the event queue again; a
        // clock whose edge applied in a slot the run left unfinished is gone,
        // as the reference loop leaves it until the slot re-arms it.
        let armed: u64 = backend.get_as(scheduler.clock_armed);
        let values: u64 = backend.get_as(scheduler.clock_value);
        let clocked = clocks.iter().any(Option::is_some);
        for (bit, event_ref) in clocks.into_iter().enumerate() {
            let Some(event_ref) = event_ref.filter(|_| armed >> bit & 1 != 0) else {
                continue;
            };
            self.scheduler.push(SimEvent {
                time: backend.get_as(SchedulerRefs::clock_element(scheduler.clock_next, bit)),
                event_ref,
                signal: scheduler.clocks[bit].1,
                next_val: u8::from(values >> bit & 1 != 0),
                periodic: true,
            });
        }
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
            let site = (backend.get_as::<u64>(self.model.processes[process].delay)
                & !celox_design::PROCESS_LEVEL_WAIT) as usize;
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
        if slot_complete {
            // The slots ended with their levels as the baseline of the next.
            self.resample_clock_values(executor.backend());
        } else if count > 0 {
            // The baseline is the levels slot `now` started from; events the
            // kernels do not write kept theirs.
            let levels: u64 = executor.backend().get_as(scheduler.levels);
            // A run without clocks records no level of their events, which
            // only the clocks change.
            let scheduler = self
                .model
                .scheduler
                .as_ref()
                .ok_or(SimulatorErrorCode::InternalError)?;
            let recorded: Vec<(usize, bool)> = scheduler
                .events
                .iter()
                .enumerate()
                .filter(|&(_, &id)| {
                    clocked || scheduler.clocks.iter().all(|&(clock, _)| clock != id)
                })
                .map(|(bit, &id)| (id, levels >> bit & 1 != 0))
                .collect();
            for (id, level) in recorded {
                self.record_level(id, u8::from(level));
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
            Some(SchedulerExit::ClockOverflow) => {
                let event = overflowed.ok_or(SimulatorErrorCode::InternalError)?;
                return Err(Halt::Error(SimulatorErrorCode::Runtime {
                    message: format!("event {event} overflows simulation time"),
                    signals: Vec::new(),
                }));
            }
            Some(SchedulerExit::Internal) | None => {
                return Err(SimulatorErrorCode::InternalError.into());
            }
        }
        self.retain_activation_roots(executor);
        executor.try_finish_timed_step(now)?;
        // What the host writes after the slot is an update of its time.
        if executor.take_slot_writes() {
            self.host_pending = true;
        }
        // A run of one slot ends it unless work is left at its time.
        if (self.one_slot_runs() || executor.compiled_slots() == CompiledSlots::One)
            && !self.host_pending
            && self.next_work_time() != Some(now)
        {
            self.end_time_slot(executor, now)?;
        }
        Ok(now)
    }

    /// Whether every run of the compiled scheduler takes one slot: the
    /// sampled copies are made before entering each later timestamp.
    fn one_slot_runs(&self) -> bool {
        !self.model.sample_shadows.is_empty()
    }
}
