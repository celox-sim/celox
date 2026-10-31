//! Invoke ordinary compiled SIR processes and service their suspension requests.
use super::*;
use celox_design::{HostRequestKind, ProcessStatus};
use celox_testbench::{KernelRequest, TestbenchKernel};

pub(super) struct Process<'a, B: SimBackend> {
    kernel: &'a TestbenchKernel<SignalRef>,
    sites: Vec<&'a TestbenchStatement<B>>,
    started: bool,
}
impl<'a, B: SimBackend> Process<'a, B> {
    pub(super) fn compile(
        statements: &'a [TestbenchStatement<B>],
        kernel: &'a TestbenchKernel<SignalRef>,
    ) -> Result<Self, String> {
        fn collect<'a, B: SimBackend>(
            statements: &'a [TestbenchStatement<B>],
            sites: &mut Vec<&'a TestbenchStatement<B>>,
        ) {
            for statement in statements {
                sites.push(statement);
                match statement {
                    GenericTestbenchStatement::If {
                        then_block,
                        else_block,
                        ..
                    } => {
                        collect::<B>(then_block, sites);
                        collect::<B>(else_block, sites);
                    }
                    GenericTestbenchStatement::For { body, .. } => collect::<B>(body, sites),
                    _ => {}
                }
            }
        }
        let mut sites = Vec::new();
        collect::<B>(statements, &mut sites);
        Ok(Self {
            kernel,
            sites,
            started: false,
        })
    }
    pub(super) fn resume(
        &mut self,
        sim: &mut Simulator<B>,
        ctx: &mut DetailedExecContext,
        wait: &mut Option<ProcessWait<B::Event>>,
        reset_edges: &mut Vec<(B::Event, u8)>,
        scheduled: bool,
    ) -> Poll<ExecResult> {
        if !self.started {
            sim.backend.set(self.kernel.resume, 0u16);
            self.started = true;
        }
        loop {
            if !scheduled && ctx.at_tick_limit() {
                ctx.tick_limit_reached = true;
                return Poll::Ready(ExecResult::Finished);
            }
            if let Err(error) = sim.backend.run_process(self.kernel.entry) {
                return Poll::Ready(ExecResult::Fail(format!("testbench kernel: {error}")));
            }
            let status = ProcessStatus::from_code(sim.backend.get_as::<u8>(self.kernel.status));
            if status == Some(ProcessStatus::Done) {
                return Poll::Ready(ExecResult::Continue);
            }
            if status != Some(ProcessStatus::HostRequest) {
                return Poll::Ready(ExecResult::Fail(format!(
                    "invalid testbench kernel status: {status:?}"
                )));
            }
            let encoded = sim.backend.get_as::<u64>(self.kernel.request);
            let Some((HostRequestKind::Testbench, index)) = HostRequestKind::decode(encoded) else {
                return Poll::Ready(ExecResult::Fail("invalid testbench host request".into()));
            };
            let Some(request) = self.kernel.requests.get(index as usize) else {
                return Poll::Ready(ExecResult::Fail(
                    "testbench host request out of range".into(),
                ));
            };
            match request {
                KernelRequest::Settle { .. } => {
                    if let Err(error) = settle_testbench(sim) {
                        return Poll::Ready(ExecResult::Fail(format!("eval_comb: {error}")));
                    }
                }
                KernelRequest::Written { address, width } => {
                    let bytes = width.div_ceil(8);
                    sim.backend.mark_settle_range(address.offset, bytes);
                    sim.dirty |= !private_testbench_range(sim, address.offset, bytes);
                }
                KernelRequest::Error(message) => {
                    return Poll::Ready(ExecResult::Fail(message.clone()));
                }
                KernelRequest::Statement(site) => {
                    let statement = self.sites[*site];
                    if scheduled {
                        match statement {
                            GenericTestbenchStatement::ClockNext {
                                clock_event,
                                count,
                                period,
                            } => {
                                let count = match eval_clock_count(sim, count) {
                                    Ok(count) => count,
                                    Err(error) => {
                                        return Poll::Ready(ExecResult::Fail(error.to_string()));
                                    }
                                };
                                if count != 0 {
                                    *wait = Some(ProcessWait {
                                        clock: *clock_event,
                                        remaining: count,
                                        period: *period,
                                        resume_at: None,
                                        reset: None,
                                    });
                                    return Poll::Pending;
                                }
                                continue;
                            }
                            GenericTestbenchStatement::ResetAssert {
                                reset_signal,
                                reset_event,
                                clock_event,
                                duration,
                                period,
                                assert_value,
                                deassert_value,
                            } => {
                                let count = match eval_clock_count(sim, duration) {
                                    Ok(count) => count.max(1),
                                    Err(error) => {
                                        return Poll::Ready(ExecResult::Fail(error.to_string()));
                                    }
                                };
                                sim_set_u64(sim, *reset_signal, (*assert_value).into());
                                if let Some(event) = reset_event {
                                    reset_edges.push((*event, *assert_value));
                                }
                                *wait = Some(ProcessWait {
                                    clock: *clock_event,
                                    remaining: count,
                                    period: *period,
                                    resume_at: None,
                                    reset: Some((*reset_signal, *reset_event, *deassert_value)),
                                });
                                return Poll::Pending;
                            }
                            _ => {}
                        }
                    }
                    let result = exec_one_detailed(sim, statement, ctx);
                    if !matches!(result, ExecResult::Continue) {
                        return Poll::Ready(result);
                    }
                }
            }
            if !scheduled {
                sim.maybe_collect_heap([]);
            }
        }
    }
}
