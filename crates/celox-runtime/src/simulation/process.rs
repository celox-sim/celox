//! The process phase of a round: running ready kernels and filing them
//! where they resume.

use celox_design::ProcessStatus;

use super::SimulationState;
use super::host::SimulationExecutor;
use super::round::Round;
use crate::{SimulatorErrorCode, backend::SimBackend, timing::Halt};

impl<B: SimBackend> SimulationState<B> {
    /// Run process `process` once and file it where it resumes. Returns
    /// whether it resumes at once (a queued nonblocking assignment or a
    /// yield), in which case the caller checks the state before running it
    /// again: the return separates the writes before and after it.
    fn run_process<E>(
        &mut self,
        executor: &mut E,
        current_time: u64,
        process: usize,
        returns: &mut u64,
    ) -> Result<bool, Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        {
            executor.run_process(process)?;
            let status: u8 = executor.backend().get_as(self.processes[process].status);
            let delay = || -> u64 { executor.backend().get_as(self.processes[process].delay) };
            match ProcessStatus::from_code(status) {
                Some(ProcessStatus::Delay) => {
                    let delay = delay();
                    self.wake_after(current_time, delay, process)?;
                }
                Some(ProcessStatus::InactiveDelay) => {
                    let delay = delay();
                    if delay == 0 {
                        self.process_state.inactive.insert(process);
                    } else {
                        self.wake_after(current_time, delay, process)?;
                    }
                }
                Some(ProcessStatus::Done) => {}
                Some(ProcessStatus::Finish) => return Err(Halt::Finish),
                Some(ProcessStatus::Wait | ProcessStatus::Pending) => {
                    let site = delay() as usize;
                    self.register_waiter(executor.backend(), process, site)?;
                }
                Some(ProcessStatus::Nba) => {
                    let site = delay() as usize;
                    self.queue_nba(executor.backend_mut(), process, site)?;
                    *returns += 1;
                    self.charge(*returns, Some(process))?;
                    return Ok(true);
                }
                Some(ProcessStatus::Yield) => {
                    *returns += 1;
                    self.charge(*returns, Some(process))?;
                    return Ok(true);
                }
                None => return Err(SimulatorErrorCode::InternalError.into()),
            }
            Ok(false)
        }
    }

    /// Run the ready processes in declaration order, including those they
    /// wake, recording the event signals each one changes.
    fn run_ready<E>(
        &mut self,
        executor: &mut E,
        current_time: u64,
        round: &mut Round,
        returns: &mut u64,
    ) -> Result<bool, Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        if self.process_state.ready.is_empty() {
            return Ok(false);
        }
        self.event_values(executor.backend(), &mut round.before);
        while let Some(process) = self.process_state.ready.pop_first() {
            loop {
                let again = self.run_process(executor, current_time, process, returns)?;
                // Each run is a separate update of the event signals, so a
                // pulse across two runs is an edge.
                self.record_transitions(executor.backend(), round);
                // Before the next run, its edges must be seen. After the last
                // one, the check following the settle sees them: a settle
                // does not write what processes write.
                if again || !self.process_state.ready.is_empty() {
                    self.observe_driven(executor.backend());
                }
                if !again {
                    break;
                }
            }
        }
        Ok(true)
    }

    /// The process phase of a round: the active and inactive regions before
    /// any domain publishes (IEEE 1800-2023 4.5). Without ready processes,
    /// waits or counted signals this is one settle, the batch step's.
    pub(super) fn process_phase<E>(
        &mut self,
        executor: &mut E,
        current_time: u64,
        round: &mut Round,
        mut entry: bool,
    ) -> Result<(), Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        let mut returns = 0;
        let mut first = true;
        loop {
            self.charge_pass()?;
            // Processes run after this time's scheduled values are applied
            // and see the state settled at the previous time. The event
            // signals they change are edges of this time, like scheduled
            // events.
            let ran = self.run_ready(executor, current_time, round, &mut returns)?;
            round.kernels_ran |= ran;
            if first || ran {
                let transitions = std::mem::take(&mut round.transitions);
                self.mark_scheduled_edges(executor.backend_mut(), round, &transitions);
                round.transitions = transitions;
                round.transitions.clear();
                self.settle_marked(executor, round)?;
                first = false;
            }
            if entry {
                // Scheduled and host values, including raw writes of the
                // host's view of the state, and what the settle derived.
                entry = false;
                if !self.process_state.waiters.is_empty() {
                    self.observe(executor.backend(), None);
                }
            } else {
                self.observe_driven(executor.backend());
            }
            if !self.process_state.ready.is_empty() {
                continue;
            }
            if self.process_state.inactive.is_empty() {
                return Ok(());
            }
            // The active region is empty: the domains it triggered evaluate
            // before the inactive region's processes resume.
            self.evaluate_before_inactive(executor, current_time, round)?;
            let inactive = std::mem::take(&mut self.process_state.inactive);
            self.process_state.ready.extend(inactive);
        }
    }
}
