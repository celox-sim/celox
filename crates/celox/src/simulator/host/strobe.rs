//! Delayed evaluation of strobe output, with no runnable HDL continuation.
use super::*;
use celox_design::{HostRequestKind, HostRequestSite, ProcessStatus};
use celox_runtime::Halt;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct StrobeCall {
    site: usize,
    descriptor: Option<u32>,
}

impl<B: SimBackend> Simulator<B> {
    pub(crate) fn queue_strobe(&mut self, site: &HostRequestSite<SignalRef>) -> Result<(), Halt> {
        let descriptor = if let Some(arg) = site.args.first() {
            let (value, mask) = self.backend.get_four_state(arg.id);
            if mask != BigUint::default() {
                return Err(RuntimeErrorCode::Runtime {
                    message: "$fstrobe descriptor contains X or Z".into(),
                    signals: Vec::new(),
                }
                .into());
            }
            Some(value.to_u32_digits().first().copied().unwrap_or(0))
        } else {
            None
        };
        let index = self
            .program
            .runtime_schema
            .host_request_sites
            .iter()
            .position(|candidate| {
                candidate.process == site.process
                    && candidate.kind == site.kind
                    && candidate.operation == 16
                    && candidate.strings == site.strings
            })
            .ok_or(RuntimeErrorCode::InternalError)?;
        // The scheduler charges each registration to its process-return
        // budget. Diagnostic ring capacity must not drop real file writes.
        self.pending_host_events
            .lock()
            .unwrap()
            .strobes
            .push(StrobeCall {
                site: index,
                descriptor,
            });
        Ok(())
    }

    pub(crate) fn valid_strobe_calls(&self, calls: &[StrobeCall]) -> bool {
        calls.iter().all(|call| {
            self.program
                .runtime_schema
                .host_request_sites
                .get(call.site)
                .is_some_and(|site| {
                    site.kind == HostRequestKind::FileIo
                        && site.operation == 16
                        && site.args.is_empty() == call.descriptor.is_none()
                })
        })
    }

    pub(crate) fn strobe_calls(&self) -> Vec<StrobeCall> {
        if !self.postponed_output {
            return Vec::new();
        }
        self.pending_host_events.lock().unwrap().strobes.clone()
    }

    pub(crate) fn restore_strobe_calls(&mut self, calls: &[StrobeCall]) {
        if !self.postponed_output {
            return;
        }
        self.pending_host_events.lock().unwrap().strobes = calls.to_vec();
    }

    pub(crate) fn cancel_file_strobes(&mut self, descriptor: u32) {
        self.pending_host_events
            .lock()
            .unwrap()
            .strobes
            .retain_mut(|call| {
                let Some(fd) = call.descriptor else {
                    return true;
                };
                if descriptor & 0x8000_0000 != 0 {
                    if fd == descriptor || descriptor == 0x8000_0001 && fd & 0x8000_0000 == 0 {
                        call.descriptor = Some(if fd == descriptor { 0 } else { fd & !1 });
                    }
                } else if fd & 0x8000_0000 == 0 {
                    call.descriptor = Some(fd & !descriptor);
                } else if fd == 0x8000_0001 && descriptor & 1 != 0 {
                    call.descriptor = Some(0);
                }
                call.descriptor != Some(0)
            });
    }

    pub(crate) fn run_strobes(&mut self, timestamp: u64) -> Result<(), Halt> {
        if !self.postponed_output || self.frozen {
            return Ok(());
        }
        let calls = std::mem::take(&mut self.pending_host_events.lock().unwrap().strobes);
        for call in calls {
            let site = self.program.runtime_schema.host_request_sites[call.site].clone();
            let process = site.process;
            let slots = self.program.runtime_schema.processes[process]
                .map(|address| self.backend.resolve_signal(&address));
            let saved: Vec<_> = slots
                .iter()
                .map(|signal| (*signal, self.backend.get_four_state(*signal)))
                .collect();
            let fd = site
                .args
                .first()
                .map(|arg| self.backend.resolve_signal(&arg.id));
            let saved_fd = fd.map(|signal| (signal, self.backend.get_four_state(signal)));
            if let (Some(signal), Some(value)) = (fd, call.descriptor) {
                self.backend.set(signal, value);
            }
            let resume: u32 = site.strings[0]
                .parse()
                .map_err(|_| RuntimeErrorCode::InternalError)?;
            self.backend.set(slots.resume, resume);
            let dirty = self.dirty;
            let result: Result<(), Halt> = (|| {
                loop {
                    self.run_process_checked(process)?;
                    match ProcessStatus::from_code(
                        self.backend
                            .get(slots.status)
                            .to_u32_digits()
                            .first()
                            .copied()
                            .unwrap_or(0) as u8,
                    ) {
                        Some(ProcessStatus::EventProbe) => return Ok(()),
                        Some(ProcessStatus::HostRequest) => {
                            let word = self
                                .backend
                                .get(slots.delay)
                                .to_u64_digits()
                                .first()
                                .copied()
                                .unwrap_or(0);
                            let (kind, index) = HostRequestKind::decode(word)
                                .ok_or(RuntimeErrorCode::InternalError)?;
                            let request = self
                                .program
                                .runtime_schema
                                .host_request_sites
                                .iter()
                                .filter(|request| {
                                    request.process == process && request.kind == kind
                                })
                                .nth(index as usize)
                                .cloned()
                                .ok_or(RuntimeErrorCode::InternalError)?;
                            if kind != HostRequestKind::FileIo
                                || !matches!(request.operation, 2 | 3)
                            {
                                return Err(RuntimeErrorCode::InternalError.into());
                            }
                            let request =
                                request.map(|address| self.backend.resolve_signal(&address));
                            self.run_file_io_request(&request, timestamp)?;
                        }
                        _ => return Err(RuntimeErrorCode::InternalError.into()),
                    }
                }
            })();
            for (signal, (value, mask)) in saved.into_iter().chain(saved_fd) {
                self.backend.set_four_state(signal, value, mask);
            }
            self.dirty = dirty;
            result?;
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "systemverilog"))]
mod tests {
    use super::*;

    #[test]
    fn pending_strobe_checkpoint_restores_output_and_preserves_control_slots() {
        let source = "module Top; timeunit 1ps; timeprecision 1ps; int value=1; initial begin $strobe(\"value=%0d\",value); #1; end endmodule";
        let mut sim = crate::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("pending_strobe.sv"))],
            "Top",
        )
        .build_interpreter()
        .unwrap();
        sim.run_until(0).unwrap();
        sim.drain_runtime_events();
        let core = &mut sim.simulator;
        let site = core
            .program
            .runtime_schema
            .host_request_sites
            .iter()
            .find(|site| site.kind == HostRequestKind::FileIo && site.operation == 16)
            .unwrap()
            .clone();
        let site = site.map(|address| core.backend.resolve_signal(&address));
        core.queue_strobe(&site).unwrap();
        let checkpoint = core.checkpoint().unwrap();
        let wire = postcard::to_allocvec(&checkpoint.persisted()).unwrap();
        let decoded: crate::simulator::checkpoint::PersistedCheckpoint =
            postcard::from_bytes(&wire).unwrap();
        let checkpoint = decoded.bind(core).unwrap();
        let slots = core.program.runtime_schema.processes[site.process]
            .map(|address| core.backend.resolve_signal(&address));
        let values: Vec<_> = slots
            .iter()
            .map(|signal| core.backend.get(*signal))
            .collect();
        core.run_strobes(0).unwrap();
        assert_eq!(
            core.drain_runtime_events(),
            vec![crate::RuntimeEvent::Display {
                message: b"value=1".to_vec()
            }]
        );
        core.restore(&checkpoint).unwrap();
        core.run_strobes(0).unwrap();
        assert_eq!(
            core.drain_runtime_events(),
            vec![crate::RuntimeEvent::Display {
                message: b"value=1".to_vec()
            }]
        );
        assert_eq!(
            slots
                .iter()
                .map(|signal| core.backend.get(*signal))
                .collect::<Vec<_>>(),
            values
        );
        for bad in [
            serde_json::json!({"site":usize::MAX,"descriptor":null}),
            serde_json::json!({"site":core.program.runtime_schema.host_request_sites.iter().position(|site| site.kind==HostRequestKind::FileIo && site.operation==16).unwrap(),"descriptor":1}),
        ] {
            let decoded: StrobeCall = serde_json::from_value(bad).unwrap();
            assert!(!core.valid_strobe_calls(&[decoded]));
        }
    }
}
