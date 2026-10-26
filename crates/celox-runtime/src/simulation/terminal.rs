//! Terminal procedures run sequentially, without scheduling another HDL event.

use celox_design::{HostRequestKind, ProcessKind, ProcessStatus};

use super::{SimulationExecutor, SimulationState};
use crate::{SimulatorErrorCode, backend::SimBackend, timing::Halt};

impl<B: SimBackend> SimulationState<B> {
    /// Finish once, running final procedures before flushing output and freezing.
    /// A `$stop` in a final procedure suspends this phase; the next step resumes it.
    pub fn finalize_terminal<E>(&mut self, executor: &mut E) -> Result<(), SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        if self.finished {
            return Ok(());
        }
        self.stopped = false;
        if self.finalizing.is_none() {
            // An explicit host finish may replace a stopped ordinary round.
            self.suspended = None;
            self.host_pending = false;
            self.finalizing = Some(0);
        }
        let result = self.run_final_procedures(executor);
        if matches!(result, Err(Halt::Stop)) {
            self.stopped = true;
            return Ok(());
        }
        // A failed final procedure or output flush cannot make termination resumable.
        self.finished = true;
        self.finalizing = None;
        let output = self.finish_timed_boundary(executor, self.scheduler.time);
        let frozen = executor.try_freeze();
        match result {
            Ok(()) | Err(Halt::Finish) => output.and(frozen),
            Err(Halt::Error(error)) => Err(error),
            Err(Halt::Stop) => unreachable!(),
        }
    }

    /// Explicitly end simulation at its current time. Merely exhausting currently
    /// scheduled work does not end an externally driven simulation.
    pub fn finish<E>(&mut self, executor: &mut E) -> Result<(), SimulatorErrorCode>
    where
        E: SimulationExecutor<Backend = B>,
    {
        self.finalize_terminal(executor)
    }

    fn run_final_procedures<E>(&mut self, executor: &mut E) -> Result<(), Halt>
    where
        E: SimulationExecutor<Backend = B>,
    {
        let mut returns = 0;
        while let Some(index) = self
            .finalizing
            .filter(|&index| index < self.model.processes.len())
        {
            let process = &self.model.processes[index];
            if process.kind != ProcessKind::Final {
                self.finalizing = Some(index + 1);
                continue;
            }
            executor.run_process(index)?;
            returns += 1;
            self.charge(returns, Some(index))?;
            let status: u8 = executor.backend().get_as(process.status);
            match ProcessStatus::from_code(status) {
                Some(ProcessStatus::Done) => self.finalizing = Some(index + 1),
                Some(ProcessStatus::Finish) => return Err(Halt::Finish),
                Some(ProcessStatus::Stop) => return Err(Halt::Stop),
                Some(ProcessStatus::HostRequest) => {
                    let encoded: u64 = executor.backend().get_as(process.delay);
                    let (kind, site) = HostRequestKind::decode(encoded)
                        .ok_or(SimulatorErrorCode::InternalError)?;
                    let request = process
                        .host_requests
                        .iter()
                        .filter(|request| request.kind == kind)
                        .nth(site as usize)
                        .ok_or(SimulatorErrorCode::InternalError)?;
                    executor.host_request(request, self.scheduler.time)?;
                }
                // Yield is an implementation boundary, not an HDL timing control.
                // NBA events created by a final never run after termination (9.2.3).
                Some(ProcessStatus::Yield | ProcessStatus::Nba | ProcessStatus::Local) => {}
                _ => {
                    return Err(SimulatorErrorCode::Runtime {
                        message: "final procedure cannot suspend for HDL timing".into(),
                        signals: Vec::new(),
                    }
                    .into());
                }
            }
        }
        Ok(())
    }
}
