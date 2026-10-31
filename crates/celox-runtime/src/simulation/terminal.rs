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
        // Every final body belongs to one process. Retire its exposed identity
        // only after the entire final phase ends (including explicit $finish).
        for index in 0..self.model.processes.len() {
            if self.model.processes[index].kind == ProcessKind::Final {
                self.end_activation(index);
                self.retire_activation(index);
            }
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

    /// The stable identity of the single process executing all final bodies.
    pub(super) fn final_process_identity(&self) -> Option<usize> {
        self.model
            .processes
            .iter()
            .position(|process| process.kind == ProcessKind::Final && process.frame.is_some())
    }

    fn save_final_activation(&mut self, backend: &B, index: usize) {
        self.save_activation(backend, index);
        let Some(identity) = self.final_process_identity() else {
            return;
        };
        let Some(frame) = &self.model.processes[index].frame else {
            return;
        };
        let random = backend.get_as::<u64>(frame.random);
        let shared = &mut self.process_state.activations[identity];
        shared.control.random = Some(random);
        // Keep the shared frame current so its retirement retains the phase's
        // latest RNG state even after its own body completed earlier.
        let frame = self.model.processes[identity].frame.as_ref().unwrap();
        if let Some(position) = frame
            .private
            .iter()
            .position(|&value| value == frame.random)
            && let Some(value) = shared.private.get_mut(position)
        {
            *value = (random.into(), 0u8.into());
        }
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
            if self.model.processes[index].kind != ProcessKind::Final {
                self.finalizing = Some(index + 1);
                continue;
            }
            let identity = self.final_process_identity();
            let kernel = self.restore_activation(executor.backend_mut(), index);
            if let (Some(identity), Some(frame)) = (identity, &self.model.processes[index].frame) {
                let seed = self.model.processes[identity].frame.as_ref().unwrap().seed;
                let random = self.process_state.activations[identity]
                    .control
                    .random
                    .unwrap_or(u64::from(seed) | (1 << 32));
                executor.backend_mut().set(frame.random, random);
            }
            executor.run_process(kernel)?;
            self.save_final_activation(executor.backend(), index);
            returns += 1;
            self.charge(returns, Some(index))?;
            let process = &self.model.processes[index];
            let status: u8 = executor.backend().get_as(process.status);
            match ProcessStatus::from_code(status) {
                Some(ProcessStatus::Done) => {
                    if Some(index) != identity {
                        self.end_activation(index);
                        self.retire_activation(index);
                    }
                    self.finalizing = Some(index + 1);
                }
                Some(ProcessStatus::Finish) => return Err(Halt::Finish),
                Some(ProcessStatus::Stop) => return Err(Halt::Stop),
                Some(ProcessStatus::Fork)
                    if executor.backend().get_as::<u64>(process.delay) >> 32 == 2 =>
                {
                    // A join_none child cannot start until its parent blocks or
                    // terminates (9.3.2). All finals share one nonblocking process,
                    // and no queued events run afterward (9.2.3). Resume only the
                    // parent; the kernel already recorded its continuation.
                }
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
                    if kind == HostRequestKind::Process {
                        let request = request.clone();
                        if !self.process_request(executor, self.scheduler.time, index, &request)? {
                            return Err(SimulatorErrorCode::Runtime {
                                message: "final procedure cannot suspend for HDL timing".into(),
                                signals: Vec::new(),
                            }
                            .into());
                        }
                    } else {
                        executor.host_request(request, self.scheduler.time)?;
                    }
                }
                // Yield is an implementation boundary, not an HDL timing control.
                // NBA events created by a final never run after termination (9.2.3).
                Some(
                    ProcessStatus::Yield
                    | ProcessStatus::Nba
                    | ProcessStatus::Local
                    | ProcessStatus::DisableFork,
                ) => {}
                _ => {
                    return Err(SimulatorErrorCode::Runtime {
                        message: "final procedure cannot suspend for HDL timing".into(),
                        signals: Vec::new(),
                    }
                    .into());
                }
            }
            self.save_final_activation(executor.backend(), index);
        }
        Ok(())
    }
}
