//! Time slots: advancing time, periodic clocks, process wakeups and the
//! work budget of a slot.

use std::cmp::Reverse;

use super::SimulationState;
use super::host::SimulationExecutor;
use crate::{
    SignalRef, SimulatorErrorCode,
    backend::{EventHandle, SimBackend},
    scheduler::{ClockDef, SimEvent},
    timing::Halt,
};

impl<B: SimBackend> SimulationState<B> {
    pub fn add_clock(
        &mut self,
        event: B::Event,
        signal: SignalRef,
        period: u64,
        initial_delay: u64,
    ) {
        let event_id = event.id();
        if event_id >= self.scheduler.clocks.len() {
            self.scheduler.clocks.resize(event_id + 1, None);
        }
        self.scheduler.clocks[event_id] = Some(ClockDef {
            period: period.max(2),
        });
        self.scheduler.push(SimEvent {
            time: initial_delay,
            event_ref: event,
            signal,
            next_val: 1,
            periodic: true,
        });
    }

    pub fn schedule(&mut self, event: B::Event, signal: SignalRef, time: u64, value: u8) {
        self.scheduler.push(SimEvent {
            time,
            event_ref: event,
            signal,
            next_val: value,
            periodic: false,
        });
    }

    /// Re-schedule the periodic clock edges among `events`, which fired at
    /// `current_time`: each clock's opposite level, half a period later.
    pub(super) fn rearm_clocks(
        &mut self,
        current_time: u64,
        events: &[SimEvent<B>],
    ) -> Result<(), Halt> {
        for event in events {
            if !event.periodic {
                continue;
            }
            let event_id = event.event_ref.id();
            if let Some(Some(clock)) = self.scheduler.clocks.get(event_id) {
                // Keep the extra odd-period tick in the high phase, matching
                // the process/testbench clock model.
                // A clock needs at least one time unit in each phase.
                let period = clock.period.max(2);
                let half = (period / 2) + ((period & 1) & u64::from(event.next_val != 0));
                let time = current_time.checked_add(half).ok_or_else(|| {
                    Halt::Error(SimulatorErrorCode::Runtime {
                        message: format!("event {event_id} overflows simulation time"),
                        signals: Vec::new(),
                    })
                })?;
                self.scheduler.push(SimEvent {
                    time,
                    event_ref: event.event_ref,
                    signal: event.signal,
                    next_val: 1 - event.next_val,
                    periodic: true,
                });
            }
        }
        Ok(())
    }

    /// Settle host writes at the current time, then run the next time with
    /// work; nothing after `limit` runs. Host writes do not hold a step at
    /// the current time: the step goes on to the next event.
    pub(super) fn step_time<E>(&mut self, executor: &mut E, limit: u64) -> Result<Option<u64>, Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        if self.finished {
            return Ok(None);
        }
        self.initialize_declarations_inner(executor)?;
        if self.finished || self.stopped {
            return Ok(Some(self.scheduler.time));
        }
        // A suspended time slot continues where the `$stop` left it.
        if let Some(mut suspended) = self.suspended.take() {
            let current_time = self.scheduler.time;
            if std::mem::take(&mut self.host_pending) {
                // Host updates belong to the suspended slot. Show logged
                // pulses to existing waits, then settle raw and derived values
                // before any continuation reads them. Keep the round's pending
                // edges and NBA publications intact.
                for (signal, value) in executor.take_host_writes() {
                    self.observe_host_write(signal, &value);
                }
                self.settle_host_resume(executor, &mut suspended.round, &suspended.levels)?;
                self.observe(executor.backend(), None);
            }
            self.continue_round(
                executor,
                current_time,
                suspended.round,
                suspended.events,
                false,
            )?;
            self.end_time(executor, current_time)?;
            return Ok(Some(current_time));
        }
        let mut settled = None;
        loop {
            let Some(current_time) = self.next_event_time() else {
                return Ok(settled);
            };
            if current_time > limit {
                return Ok(settled);
            }
            // Host writes alone: settle them, then go on to the next time.
            let host_only = self.host_pending && self.next_work_time() != Some(current_time);
            self.run_time(executor, current_time)?;
            if !host_only || self.stopped {
                return Ok(Some(current_time));
            }
            settled = Some(current_time);
        }
    }

    /// Run the time slot `current_time`: its scheduled events, the
    /// processes due then, and the rounds they cause.
    fn run_time<E>(&mut self, executor: &mut E, current_time: u64) -> Result<(), Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        self.host_pending = false;
        self.slot_passes = 0;
        let events_to_process = if self.scheduler.next_event_time() == Some(current_time) {
            self.scheduler
                .pop_all_at_next_time()
                .map(|(_, events)| events)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut ready = self.take_ready_processes(current_time);
        if current_time == 0 {
            // Converted always_ff kernels have a leading event control rather
            // than an initial body. Arm it before scheduled inputs change, as
            // for ordinary FF domains; leave initial processes in their usual
            // process phase and declaration order (IEEE 1800-2023 4.5, 9.2.2.4).
            let mut returns = 0;
            for &process in &ready {
                if self.model.processes[process].starts_with_event_wait
                    && self.run_process(executor, current_time, process, &mut returns)?
                {
                    return Err(SimulatorErrorCode::InternalError.into());
                }
            }
            ready.retain(|&process| !self.model.processes[process].starts_with_event_wait);
        }
        self.step_round(executor, current_time, events_to_process, ready, true)?;
        self.end_time(executor, current_time)
    }

    /// The rounds of time slot `current_time` after its first.
    fn end_time<E>(&mut self, executor: &mut E, current_time: u64) -> Result<(), Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        // A process that waited for zero time, or that the round woke,
        // resumes in a later round of this time, after the previous round's
        // edges have been handled.
        loop {
            let ready = self.take_ready_processes(current_time);
            if ready.is_empty() && self.process_state.ready.is_empty() {
                break;
            }
            self.charge_pass()?;
            self.step_round(executor, current_time, Vec::new(), ready, false)?;
        }
        executor.try_finish_timed_step(current_time)?;
        if self.next_work_time() != Some(current_time) {
            self.end_time_slot(executor, current_time)?;
        }
        Ok(())
    }

    /// The time slot `current_time` is over: the host's Observed and
    /// Postponed work, then the sampled copies.
    pub(super) fn end_time_slot<E>(
        &mut self,
        executor: &mut E,
        current_time: u64,
    ) -> Result<(), Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        executor.try_end_time_slot(current_time)?;
        self.sample_values(executor.backend_mut());
        Ok(())
    }

    /// Count a round or a pass of the process phase against the slot's
    /// budget, so that processes resuming at one time forever end the step
    /// with an error instead of hanging it.
    pub(super) fn charge_pass(&mut self) -> Result<(), Halt> {
        self.slot_passes += 1;
        self.charge(self.slot_passes, None)
    }

    pub(super) fn charge(&self, returns: u64, process: Option<usize>) -> Result<(), Halt> {
        let budget = if process.is_some() {
            self.return_budget
        } else {
            self.round_budget
        };
        if returns <= budget {
            return Ok(());
        }
        Err(Halt::Error(SimulatorErrorCode::Runtime {
            message: match process {
                Some(process) => {
                    format!("process {process} does not suspend within one time slot")
                }
                None => format!(
                    "time {} does not settle: processes keep resuming",
                    self.scheduler.time
                ),
            },
            signals: Vec::new(),
        }))
    }

    /// Remove the processes waiting for `time`, in declaration order.
    fn take_ready_processes(&mut self, time: u64) -> Vec<usize> {
        let mut ready = Vec::new();
        while let Some(&Reverse((wakeup, process))) = self.process_wakeups.peek() {
            if wakeup != time {
                break;
            }
            self.process_wakeups.pop();
            ready.push(process);
        }
        ready
    }

    pub(super) fn wake_after(
        &mut self,
        current_time: u64,
        delay: u64,
        process: usize,
    ) -> Result<(), Halt> {
        let time = current_time.checked_add(delay).ok_or_else(|| {
            Halt::Error(SimulatorErrorCode::Runtime {
                message: format!("process {process} delay overflows simulation time"),
                signals: Vec::new(),
            })
        })?;
        self.process_wakeups.push(Reverse((time, process)));
        Ok(())
    }

    /// The next time with scheduled events or processes to run, apart
    /// from host writes to settle.
    pub fn next_work_time(&self) -> Option<u64> {
        if self.finished {
            return None;
        }
        if !self.process_state.ready.is_empty() {
            return Some(self.scheduler.time);
        }
        let process = self.process_wakeups.peek().map(|&Reverse((time, _))| time);
        match (self.scheduler.next_event_time(), process) {
            (Some(event), Some(process)) => Some(event.min(process)),
            (event, process) => event.or(process),
        }
    }
}
