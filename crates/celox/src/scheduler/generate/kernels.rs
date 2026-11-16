//! The process phase of a slot: the kernels that are ready run one at a time.
use super::*;

impl Plan {
    /// The head of a pass of the process phase: dispatch to the lowest ready
    /// kernel. Returns the block the pass ends at with no kernel ready, and the
    /// block each kernel runs in.
    pub(super) fn pick_kernel(
        &self,
        g: &mut Generator<'_>,
        cx: &Slot<'_>,
        pick: celox_sir::BlockId,
        internal_exit: celox_sir::BlockId,
    ) -> (celox_sir::BlockId, Vec<(usize, celox_sir::BlockId)>) {
        let &Slot {
            slots,
            ordinals,
            assertions,
            reactive_entry,
            reactive_end,
            ..
        } = cx;
        // A pass of the process phase: the lowest ready kernel runs, and
        // the waits see its writes before the next one while kernels remain
        // ready (`run_ready`); then combinational logic settles.
        g.b.switch_to_block(pick);
        let ready = g.load(slots.ready, g.ready_bits);
        let zero = g.constant(0, g.ready_bits);
        let idle = g.binary(1, ready, BinaryOp::Eq, zero);
        let pass_end = g.b.new_block();
        let chosen = g.b.new_block();
        let idle_region = if assertions {
            g.b.new_block()
        } else {
            pass_end
        };
        g.b.seal_block(SIRTerminator::Branch {
            cond: idle,
            true_block: (idle_region, Vec::new()),
            false_block: (chosen, Vec::new()),
        });
        if assertions {
            g.b.switch_to_block(idle_region);
            let phase = g.load_at(
                slots.defer_limit_settle,
                celox_design::SCHEDULER_ASSERTION_PHASE_BIT,
                SCHEDULER_FLAG_WIDTH,
            );
            let active = g.equals(phase, 0);
            let late = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: active,
                true_block: (pass_end, Vec::new()),
                false_block: (late, Vec::new()),
            });
            g.b.switch_to_block(late);
            let observed = g.equals(phase, 1);
            g.b.seal_block(SIRTerminator::Branch {
                cond: observed,
                true_block: (reactive_entry.unwrap(), Vec::new()),
                false_block: (reactive_end.unwrap(), Vec::new()),
            });
            g.b.switch_to_block(reactive_end.unwrap());
            let active = g.constant(0, SCHEDULER_FLAG_WIDTH);
            g.store_at(
                slots.defer_limit_settle,
                celox_design::SCHEDULER_ASSERTION_PHASE_BIT,
                SCHEDULER_FLAG_WIDTH,
                active,
            );
            g.b.seal_block(SIRTerminator::Jump(pass_end, Vec::new()));
        }
        g.b.switch_to_block(chosen);
        let mut runs = Vec::new();
        if g.ready_bits <= 64 {
            // The lowest ready bit names the kernel: one dispatch instead
            // of a test per kernel. The mask is not zero here.
            let lowest = g.b.alloc_bit(g.ready_bits, false);
            g.b.emit(SIRInstruction::Unary(
                lowest,
                UnaryOp::CountTrailingZeros,
                ready,
            ));
            let selector = g.b.alloc_bit(8, false);
            g.b.emit(SIRInstruction::Slice(selector, lowest, 0, 8));
            let mut cases = Vec::new();
            for (index, kernel) in self.kernels.iter().enumerate() {
                if kernel.is_none() {
                    continue;
                }
                let run = g.b.new_block();
                cases.push(SIRSwitchCase {
                    value: ordinals[index].into(),
                    target: run,
                });
                runs.push((index, run));
            }
            g.b.seal_block(SIRTerminator::Switch {
                selector,
                cases,
                default: internal_exit,
            });
        } else {
            for (index, kernel) in self.kernels.iter().enumerate() {
                if kernel.is_none() {
                    continue;
                }
                let this = g.mask_bit(ready, ordinals[index]);
                let run = g.b.new_block();
                let next = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: this,
                    true_block: (run, Vec::new()),
                    false_block: (next, Vec::new()),
                });
                runs.push((index, run));
                g.b.switch_to_block(next);
            }
            g.b.seal_block(SIRTerminator::Jump(internal_exit, Vec::new()));
        }
        (pass_end, runs)
    }

    /// The kernels of a pass: each runs in its block of `runs`, then the waits
    /// see its writes and the pass picks again.
    pub(super) fn run_kernels(
        &self,
        g: &mut Generator<'_>,
        cx: &Slot<'_>,
        guards: &mut Option<ordered::Guards>,
        runs: Vec<(usize, celox_sir::BlockId)>,
        time: RegisterId,
        pick: celox_sir::BlockId,
        finish_exit: celox_sir::BlockId,
        internal_exit: celox_sir::BlockId,
        updates: Option<u32>,
    ) {
        let &Slot {
            slots,
            schema,
            checks,
            assertions,
            ..
        } = cx;
        let mut full_wait_check = None;
        let mut wait_continuations = vec![pick];
        for (index, run) in runs {
            let kernel = self.kernels[index].as_ref().expect("selected above");
            let process = &schema.processes[index];
            let status = self.run_kernel(
                g,
                cx,
                guards,
                index,
                run,
                kernel,
                process,
                time,
                finish_exit,
                internal_exit,
                &mut full_wait_check,
                &mut wait_continuations,
            );
            self.suspend_kernel(
                g,
                cx,
                index,
                kernel,
                process,
                status,
                time,
                pick,
                finish_exit,
                internal_exit,
                updates,
                &mut full_wait_check,
            );
        }
        if let Some(check) = full_wait_check {
            g.b.switch_to_block(check);
            if assertions {
                let phase = g.load_at(
                    slots.defer_limit_settle,
                    celox_design::SCHEDULER_ASSERTION_PHASE_BIT,
                    SCHEDULER_FLAG_WIDTH,
                );
                let active = g.equals(phase, 0);
                let scan = g.b.new_block();
                let checked = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: active,
                    true_block: (scan, Vec::new()),
                    false_block: (checked, Vec::new()),
                });
                g.b.switch_to_block(scan);
                g.check_waits_at(checks, false);
                g.b.seal_block(SIRTerminator::Jump(checked, Vec::new()));
                g.b.switch_to_block(checked);
            } else {
                g.check_waits_at(checks, false);
            }
            g.dispatch_continuations(&wait_continuations, internal_exit);
        }
    }

    /// Run kernel `index` in `run`, serving the host requests, checkpoints and
    /// queued nonblocking assignments it suspends for. Returns the status it
    /// suspended with.
    fn run_kernel(
        &self,
        g: &mut Generator<'_>,
        cx: &Slot<'_>,
        guards: &mut Option<ordered::Guards>,
        index: usize,
        run: celox_sir::BlockId,
        kernel: &Kernel,
        process: &celox_design::ProcessSlots<AbsoluteAddr>,
        time: RegisterId,
        finish_exit: celox_sir::BlockId,
        internal_exit: celox_sir::BlockId,
        full_wait_check: &mut Option<celox_sir::BlockId>,
        wait_continuations: &mut Vec<celox_sir::BlockId>,
    ) -> RegisterId {
        let &Slot {
            program,
            slots,
            ordinals,
            ..
        } = cx;
        g.b.switch_to_block(run);
        let clear = g.constant(0, 1);
        g.store_at(slots.ready, ordinals[index], 1, clear);
        let current = g.constant(index as u64, SCHEDULER_TIME_WIDTH);
        g.store(slots.current, SCHEDULER_TIME_WIDTH, current);
        // The work budget bounds zero-time loops. A kernel that only
        // suspends for positive constant delays runs once in the slot it
        // is due in and cannot take part in one, so it is not charged.
        let budgeted = kernel.wait.is_some()
            || self.inactive.iter().any(|&(process, _)| process == index)
            || shortest_delay(&program.sir.processes[index], process.delay) == 0;
        if self.cyclic && budgeted {
            if let Some(guards) = guards {
                guards.charge(
                    g,
                    index as u64,
                    crate::simulator::native_io::SCHEDULER_RUN,
                    time,
                );
            } else {
                g.service_call(
                    self.service_function.expect("native work-list services"),
                    index as u64,
                    crate::simulator::native_io::SCHEDULER_RUN,
                    time,
                    internal_exit,
                );
            }
        }
        let first_host = self
            .service_processes
            .contains(&index)
            .then(|| g.b.alloc_bit(64, false));
        let enter = match first_host {
            Some(first) => g.b.new_block_with(vec![first]),
            None => g.b.new_block(),
        };
        let enter_args = first_host
            .map(|_| vec![g.constant(1, 64)])
            .unwrap_or_default();
        g.b.seal_block(SIRTerminator::Jump(enter, enter_args));
        g.b.switch_to_block(enter);
        let before: Vec<_> = kernel
            .writes
            .iter()
            .map(|&event| g.level(&self.events[event]))
            .collect();
        g.inline_writer(&program.sir.processes[index], finish_exit);
        if !kernel.writes.is_empty() {
            // Capture every continuation, including returns to a host service.
            // A later continuation cannot erase an already observed pulse.
            let mut edges = g.load(slots.exit_arg, self.event_bits);
            for (&event, &was) in kernel.writes.iter().zip(&before) {
                let is = g.level(&self.events[event]);
                let edge = g.edge(&self.events[event], was, is);
                let bit = g.event_bit(edge, event);
                edges = g.binary(self.event_bits, edges, BinaryOp::Or, bit);
            }
            g.store(slots.exit_arg, self.event_bits, edges);
        }
        let status = g.load(process.status, celox_design::PROCESS_STATUS_WIDTH);
        let code = |status: ProcessStatus| u64::from(status.code());
        if self.service_processes.contains(&index) {
            let host = g.equals(status, code(ProcessStatus::HostRequest));
            let yielded = yielding_status(&program.sir.processes[index], process.status)
                .then(|| g.equals(status, code(ProcessStatus::Yield)));
            let mut service =
                yielded.map_or(host, |yielded| g.binary(1, host, BinaryOp::Or, yielded));
            let nba = self
                .nba_processes
                .contains(&index)
                .then(|| g.equals(status, code(ProcessStatus::Nba)));
            if let Some(nba) = nba {
                service = g.binary(1, service, BinaryOp::Or, nba);
            }
            let call = g.b.new_block();
            let returned = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: service,
                true_block: (call, Vec::new()),
                false_block: (returned, Vec::new()),
            });
            g.b.switch_to_block(call);
            let resume = g.b.new_block();
            if let (Some(nbas), Some(nba)) = (g.nbas, nba) {
                let capture = g.b.new_block();
                let host = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: nba,
                    true_block: (capture, Vec::new()),
                    false_block: (host, Vec::new()),
                });
                g.b.switch_to_block(capture);
                let site = g.load(process.delay, 64);
                nbas.capture(g, index, site);
                guards.as_mut().expect("native NBA budget guards").charge(
                    g,
                    index as u64,
                    crate::simulator::native_io::CHECKPOINT,
                    time,
                );
                g.b.seal_block(SIRTerminator::Jump(resume, Vec::new()));
                g.b.switch_to_block(host);
            }
            if let (Some(guards), Some(yielded)) = (&mut *guards, yielded) {
                let checkpoint = g.b.new_block();
                let host_call = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: yielded,
                    true_block: (checkpoint, Vec::new()),
                    false_block: (host_call, Vec::new()),
                });
                g.b.switch_to_block(checkpoint);
                guards.charge(
                    g,
                    index as u64,
                    crate::simulator::native_io::CHECKPOINT,
                    time,
                );
                g.b.seal_block(SIRTerminator::Jump(resume, Vec::new()));
                g.b.switch_to_block(host_call);
            }
            let current = g.constant(index as u64, 64);
            let request = g.load(process.delay, celox_design::PROCESS_DELAY_WIDTH);
            let request = if let Some(yielded) = yielded {
                let checkpoint = g.constant(crate::simulator::native_io::CHECKPOINT, 64);
                let selected = g.b.alloc_bit(64, false);
                g.b.emit(SIRInstruction::Mux(selected, yielded, checkpoint, request));
                selected
            } else {
                request
            };
            let request = if let Some(nba) = nba {
                let prefix = g.constant(crate::simulator::native_io::NBA_REQUEST, 64);
                let encoded = g.binary(64, request, BinaryOp::Or, prefix);
                let selected = g.b.alloc_bit(64, false);
                g.b.emit(SIRInstruction::Mux(selected, nba, encoded, request));
                selected
            } else {
                request
            };
            let result = g.b.alloc_bit(64, false);
            let context = g.load_at(
                slots.defer_limit_settle,
                celox_design::SCHEDULER_HOST_CONTEXT_BIT,
                64,
            );
            g.b.emit(SIRInstruction::ExternCall {
                dst: Some(result),
                func: self.service_function.expect("qualified native host call"),
                args: vec![
                    context,
                    current,
                    request,
                    time,
                    first_host.expect("a native host continuation"),
                ],
            });
            let ok = g.equals(result, 0);
            g.b.seal_block(SIRTerminator::Branch {
                cond: ok,
                true_block: (resume, Vec::new()),
                false_block: (internal_exit, Vec::new()),
            });
            g.b.switch_to_block(resume);
            // The reference observes the writes on either side of a host
            // request before continuing the same process. A direct call
            // keeps that boundary without leaving the native scheduler.
            let checked = g.b.new_block();
            wait_continuations.push(checked);
            let continuation = g.constant(wait_continuations.len() as u64, g.cont_bits);
            g.store(slots.cont, g.cont_bits, continuation);
            let check = *full_wait_check.get_or_insert_with(|| g.b.new_block());
            g.b.seal_block(SIRTerminator::Jump(check, Vec::new()));
            g.b.switch_to_block(checked);
            let resumed = g.constant(0, 64);
            g.b.seal_block(SIRTerminator::Jump(enter, vec![resumed]));
            g.b.switch_to_block(returned);
        }
        status
    }

    /// What kernel `index` suspended with `status` for: it ends, queues for an
    /// assertion region, waits for its wake time or registers its wait; then
    /// the waits see its writes before the next kernel runs.
    fn suspend_kernel(
        &self,
        g: &mut Generator<'_>,
        cx: &Slot<'_>,
        index: usize,
        kernel: &Kernel,
        process: &celox_design::ProcessSlots<AbsoluteAddr>,
        status: RegisterId,
        time: RegisterId,
        pick: celox_sir::BlockId,
        finish_exit: celox_sir::BlockId,
        internal_exit: celox_sir::BlockId,
        updates: Option<u32>,
        full_wait_check: &mut Option<celox_sir::BlockId>,
    ) {
        let &Slot {
            program,
            slots,
            schema,
            ordinals,
            checks,
            assertions,
            ..
        } = cx;
        let code = |status: ProcessStatus| u64::from(status.code());
        let finished = g.equals(status, code(ProcessStatus::Finish));
        let not_finished = g.b.new_block();
        // A run that finishes the simulation made its updates all the
        // same.
        let finishes = match updates {
            Some(_) => g.b.new_block(),
            None => finish_exit,
        };
        g.b.seal_block(SIRTerminator::Branch {
            cond: finished,
            true_block: (finishes, Vec::new()),
            false_block: (not_finished, Vec::new()),
        });
        if let Some(service) = updates {
            g.b.switch_to_block(finishes);
            g.report_update(service, checks, &[], finish_exit, AfterWrites::Ended);
            g.b.seal_block(SIRTerminator::Jump(finish_exit, Vec::new()));
        }
        g.b.switch_to_block(not_finished);
        let resumed = g.b.new_block();
        let done = g.equals(status, code(ProcessStatus::Done));
        let ended = g.b.new_block();
        let suspended = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: done,
            true_block: (ended, Vec::new()),
            false_block: (suspended, Vec::new()),
        });
        // A kernel woken by its edge no longer waits (`check_waits`);
        // one woken by its wake time does until it ends.
        g.b.switch_to_block(ended);
        if kernel.delays {
            let idle = g.constant(0, SCHEDULER_FLAG_WIDTH);
            g.store(slots.waiting[index], SCHEDULER_FLAG_WIDTH, idle);
            g.set_timed(ordinals[index], false);
        }
        g.b.seal_block(SIRTerminator::Jump(resumed, Vec::new()));

        g.b.switch_to_block(suspended);
        let queued_statuses = [
            ProcessStatus::AssertionObserved,
            ProcessStatus::AssertionReactive,
            ProcessStatus::DeferActive,
        ];
        let mut queued = None;
        for code in queued_statuses {
            if returns_status(&program.sir.processes[index], process.status, code) {
                let matches = g.equals(status, u64::from(code.code()));
                queued =
                    Some(queued.map_or(matches, |prior| g.binary(1, prior, BinaryOp::Or, matches)));
            }
        }
        if let Some(queued) = queued {
            let queue = g.b.new_block();
            let other = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: queued,
                true_block: (queue, Vec::new()),
                false_block: (other, Vec::new()),
            });
            g.b.switch_to_block(queue);
            let idle = g.constant(0, SCHEDULER_FLAG_WIDTH);
            g.store(slots.waiting[index], SCHEDULER_FLAG_WIDTH, idle);
            if kernel.delays {
                g.set_timed(ordinals[index], false);
            }
            g.b.seal_block(SIRTerminator::Jump(resumed, Vec::new()));
            g.b.switch_to_block(other);
        }
        let edge_wait = match &kernel.wait {
            Some(wait) => {
                // A kernel runs at most once a slot when it stages
                // nonblocking assignments (see `Plan::new`), so a staged
                // site is never staged again before the NBA region and
                // the kernel never returns `Nba`.
                let waiting = g.equals(status, code(ProcessStatus::Wait));
                let pending = g.equals(status, code(ProcessStatus::Pending));
                let suspends = g.binary(1, waiting, BinaryOp::Or, pending);
                let block = g.b.new_block();
                Some((wait, suspends, block))
            }
            None => None,
        };
        if kernel.delays {
            let delay = g.equals(status, code(ProcessStatus::Delay));
            let inactive = g.equals(status, code(ProcessStatus::InactiveDelay));
            let delayed = g.binary(1, delay, BinaryOp::Or, inactive);
            let wait = g.b.new_block();
            let other = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: delayed,
                true_block: (wait, Vec::new()),
                false_block: (other, Vec::new()),
            });
            g.b.switch_to_block(wait);
            let amount = g.load(process.delay, celox_design::PROCESS_DELAY_WIDTH);
            let wake = g.binary(SCHEDULER_TIME_WIDTH, time, BinaryOp::Add, amount);
            let overflow = g.binary(1, wake, BinaryOp::LtU, time);
            let overflow_exit = g.exit_block(SchedulerExit::DelayOverflow);
            let in_range = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: overflow,
                true_block: (overflow_exit, Vec::new()),
                false_block: (in_range, Vec::new()),
            });
            g.b.switch_to_block(in_range);
            g.store(slots.wake[index], SCHEDULER_TIME_WIDTH, wake);
            // Restored Active work need not have an armed wakeup, even
            // for kernels that only delay. Each new delay arms its slot.
            let flag = g.constant(1, SCHEDULER_FLAG_WIDTH);
            g.store(slots.waiting[index], SCHEDULER_FLAG_WIDTH, flag);
            g.set_timed(ordinals[index], true);
            g.b.seal_block(SIRTerminator::Jump(resumed, Vec::new()));
            g.b.switch_to_block(other);
        }
        match edge_wait {
            None => {
                g.b.seal_block(SIRTerminator::Jump(internal_exit, Vec::new()));
            }
            Some((wait, suspends, block)) => {
                g.b.seal_block(SIRTerminator::Branch {
                    cond: suspends,
                    true_block: (block, Vec::new()),
                    false_block: (internal_exit, Vec::new()),
                });
                // It waits again: the edge is judged against the level
                // now (`register_waiter`).
                g.b.switch_to_block(block);
                let flag = g.constant(1, SCHEDULER_FLAG_WIDTH);
                g.store(slots.waiting[index], SCHEDULER_FLAG_WIDTH, flag);
                if kernel.delays {
                    g.set_timed(ordinals[index], false);
                }
                g.register_wait(index, process.delay, wait, resumed, internal_exit);
            }
        }
        g.b.switch_to_block(resumed);
        if let Some(service) = updates {
            g.report_update(
                service,
                checks,
                &self.events,
                finish_exit,
                AfterWrites::Settled,
            );
        }
        if assertions {
            // Observed predicates all finish before any Reactive action;
            // design work produced by actions waits for the next Active pass.
            let phase = g.load_at(
                slots.defer_limit_settle,
                celox_design::SCHEDULER_ASSERTION_PHASE_BIT,
                SCHEDULER_FLAG_WIDTH,
            );
            let active = g.equals(phase, 0);
            let next = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: active,
                true_block: (next, Vec::new()),
                false_block: (pick, Vec::new()),
            });
            g.b.switch_to_block(next);
        }
        // Before the next run, the waits see this one's writes.
        let ready = g.load(slots.ready, g.ready_bits);
        let zero = g.constant(0, g.ready_bits);
        let more = g.binary(1, ready, BinaryOp::Ne, zero);
        let check = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: more,
            true_block: (check, Vec::new()),
            false_block: (pick, Vec::new()),
        });
        g.b.switch_to_block(check);
        if self.service_processes.contains(&index)
            || instructions(&program.sir.processes[index]).any(makes_extern_calls)
        {
            // Every opaque writer needs the same scan and returns to
            // `pick`. Share its body: copying every wait after every
            // service process makes large assertion-heavy RTL quadratic.
            let continuation = g.constant(1, g.cont_bits);
            g.store(slots.cont, g.cont_bits, continuation);
            let check = *full_wait_check.get_or_insert_with(|| g.b.new_block());
            g.b.seal_block(SIRTerminator::Jump(check, Vec::new()));
        } else {
            g.check_waits_after_unit(checks, &program.sir.processes[index], &schema.time_slots);
            g.b.seal_block(SIRTerminator::Jump(pick, Vec::new()));
        }
    }
}
