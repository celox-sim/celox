//! Generating the scheduler unit of a [`Plan`]: the loop over the time slots.
use super::*;

impl Plan {
    pub(super) fn generate(
        &self,
        program: &LaidOutProgram,
        slots: &SchedulerSlots<AbsoluteAddr>,
    ) -> Unit {
        self.generate_with_outlines(program, slots, None, 0)
    }

    pub(super) fn generate_with_outlines(
        &self,
        program: &LaidOutProgram,
        slots: &SchedulerSlots<AbsoluteAddr>,
        outlined: Option<&mut Outlines>,
        outline_base: u32,
    ) -> Unit {
        let schema = &program.runtime.runtime_schema;
        let edged = self.edged();
        let tracked = self.tracked();
        let clock_bits = self.clocks.len().next_multiple_of(64).max(64);
        let mut b = Builder::new();
        let comb = b.new_block();
        let native_nbas: HashSet<_> = self
            .nba_processes
            .iter()
            .copied()
            .filter(|&index| self.kernels[index].is_some())
            .collect();
        let nbas = cfg!(all(target_arch = "x86_64", not(feature = "arm64-codegen")))
            .then_some(self.service_function)
            .flatten()
            .filter(|_| !schema.nba_sites.is_empty())
            .map(|service| nba::Sites::new(program, &native_nbas, service, &self.events));
        let mut g = Generator {
            inlined_instructions: 0,
            program,
            nbas: nbas.as_ref(),
            observers: self.observers.as_ref(),
            observer_service: self.service_function,
            outlined,
            outline_base,
            b,
            slots,
            finish_sites: &self.finish_sites,
            exits: Vec::new(),
            settle_reads: self.settle_reads.as_ref(),
            incremental: self.incremental.as_ref(),
            partitioned_glue: program
                .runtime
                .runtime_schema
                .settle_partitions
                .as_ref()
                .map(|_| (program, HashSet::default())),
            shared: program.shared_storage(),
            comb,
            continuations: Vec::new(),
            eager: None,
            eager_continuations: Vec::new(),
            four_state: program
                .layout()
                .four_state
                .then_some(&program.layout().is_4states),
            event_bits: self.event_bits,
            ready_bits: self
                .kernels
                .iter()
                .flatten()
                .count()
                .next_multiple_of(64)
                .max(64),
            cont_bits: program.layout().widths[&slots.cont],
            event_batch: {
                let capacity = program.layout().runtime_event_capacity;
                (capacity > 0
                    && (!schema.runtime_event_sites.is_empty()
                        || !self.service_processes.is_empty()))
                .then_some((capacity / 8).max(1) as u64)
            },
        };
        let mut guards = if cfg!(all(target_arch = "x86_64", not(feature = "arm64-codegen")))
            && (self.cyclic || !self.service_processes.is_empty())
        {
            self.service_function
                .map(|service| ordered::Guards::new(&mut g, service))
        } else {
            None
        };
        let waits: Vec<(usize, &Wait)> = self
            .kernels
            .iter()
            .enumerate()
            .filter_map(|(index, kernel)| Some((index, kernel.as_ref()?.wait.as_ref()?)))
            .collect();
        // The kernels that delay, with the status slot of those that also
        // wait, which tells whether they wait for their wake time.
        let delays: Vec<(usize, Option<AbsoluteAddr>)> = self
            .kernels
            .iter()
            .enumerate()
            .filter_map(|(index, kernel)| {
                let kernel = kernel.as_ref().filter(|kernel| kernel.delays)?;
                let status = kernel
                    .wait
                    .is_some()
                    .then_some(schema.processes[index].status);
                Some((index, status))
            })
            .collect();
        // The bit of each kernel in the ready mask: its place among the
        // kernels the scheduler runs, in kernel order.
        let ordinals = ordinals(&self.kernels);
        let checks = waits_of(&self.kernels, &ordinals, schema);
        let status_kernels = |status| {
            self.kernels
                .iter()
                .enumerate()
                .filter(|(index, kernel)| {
                    kernel.is_some()
                        && returns_status(
                            &program.sir.processes[*index],
                            schema.processes[*index].status,
                            status,
                        )
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>()
        };
        let observed = status_kernels(ProcessStatus::AssertionObserved);
        let reactive = status_kernels(ProcessStatus::AssertionReactive);
        let deferred = status_kernels(ProcessStatus::DeferActive);
        let assertions = !observed.is_empty() || !reactive.is_empty();
        let observed_entry = assertions.then(|| g.b.new_block());
        let reactive_entry = assertions.then(|| g.b.new_block());
        let reactive_end = assertions.then(|| g.b.new_block());
        let slot_done = assertions.then(|| g.b.new_block());
        // The caller supplies the dirty flag. Timed runs must start dirty:
        // checkpoint restores and partial-slot exits need a fresh settle.
        // A host clock batch may instead enter with already settled inputs.
        let host_inputs = cfg!(all(target_arch = "x86_64", not(feature = "arm64-codegen")))
            && self.service_function.is_some();
        let header = g.b.new_block();
        // The timed mask from the kernels' state, which the host may have
        // changed between runs; the slots keep it up to date from here on.
        let mut timed = g.constant(0, g.ready_bits);
        for &(index, status) in &delays {
            let waiting = g.waiting_for_time(index, status);
            let bit = g.bit(waiting, ordinals[index]);
            timed = g.binary(g.ready_bits, timed, BinaryOp::Or, bit);
        }
        g.store(slots.timed, g.ready_bits, timed);
        g.b.seal_block(SIRTerminator::Jump(header, Vec::new()));

        // The next slot: the earliest wake time of a kernel waiting for one.
        g.b.switch_to_block(header);
        let mut time = g.constant(u64::MAX, SCHEDULER_TIME_WIDTH);
        let ready = g.load(slots.ready, g.ready_bits);
        let zero = g.constant(0, g.ready_bits);
        let mut any = g.binary(1, ready, BinaryOp::Ne, zero);
        let now = g.load(slots.now, SCHEDULER_TIME_WIDTH);
        let start = g.b.alloc_bit(SCHEDULER_TIME_WIDTH, false);
        g.b.emit(SIRInstruction::Mux(start, any, now, time));
        time = start;
        if host_inputs {
            let host = host_inputs::pending(&mut g);
            let now = g.load(slots.now, SCHEDULER_TIME_WIDTH);
            let next = g.b.alloc_bit(SCHEDULER_TIME_WIDTH, false);
            g.b.emit(SIRInstruction::Mux(next, host, now, time));
            time = next;
            any = g.binary(1, any, BinaryOp::Or, host);
        }
        if host_inputs {
            let entry = g.arena_header_load(std::mem::offset_of!(
                crate::simulator::native_io::ArenaHeader,
                region_entry
            ));
            let entry = g.equals(entry, 1);
            let next = g.b.alloc_bit(SCHEDULER_TIME_WIDTH, false);
            g.b.emit(SIRInstruction::Mux(next, entry, now, time));
            time = next;
            any = g.binary(1, any, BinaryOp::Or, entry);
        }
        if !delays.is_empty() && g.ready_bits > 64 {
            for &(index, status) in &delays {
                let wake = g.load(slots.wake[index], SCHEDULER_TIME_WIDTH);
                let waiting = g.waiting_for_time(index, status);
                let earlier = g.binary(1, wake, BinaryOp::LtU, time);
                let earlier = g.binary(1, earlier, BinaryOp::And, waiting);
                let next = g.b.alloc_bit(SCHEDULER_TIME_WIDTH, false);
                g.b.emit(SIRInstruction::Mux(next, earlier, wake, time));
                time = next;
                any = g.binary(1, any, BinaryOp::Or, waiting);
            }
        } else if !delays.is_empty() {
            let timed = g.load(slots.timed, g.ready_bits);
            let zero = g.constant(0, g.ready_bits);
            let some = g.binary(1, timed, BinaryOp::Ne, zero);
            any = g.binary(1, any, BinaryOp::Or, some);
            time = g.scan_timed(
                timed,
                &delays,
                &ordinals,
                time,
                SCHEDULER_TIME_WIDTH,
                |g, kernels, bits, mut time| {
                    for &(index, _) in kernels {
                        let wake = g.load(slots.wake[index], SCHEDULER_TIME_WIDTH);
                        let on = g.b.alloc_bit(1, false);
                        g.b.emit(SIRInstruction::Slice(on, bits, ordinals[index], 1));
                        let earlier = g.binary(1, wake, BinaryOp::LtU, time);
                        let earlier = g.binary(1, earlier, BinaryOp::And, on);
                        let next = g.b.alloc_bit(SCHEDULER_TIME_WIDTH, false);
                        g.b.emit(SIRInstruction::Mux(next, earlier, wake, time));
                        time = next;
                    }
                    time
                },
            );
        }
        if let Some(nbas) = &nbas {
            (time, any) = nbas.next_time(&mut g, time, any);
        }
        // Or the earliest next edge of a host clock, when the run has any.
        // The clocks armed at the start of the slot, which the re-arm at its
        // end compares with.
        let mut started = None;
        if !self.clocks.is_empty() {
            let armed = g.load(slots.clock_armed, clock_bits);
            let zero = g.constant(0, clock_bits);
            let clocked = g.binary(1, armed, BinaryOp::Ne, zero);
            started = Some((armed, clocked));
            let scan = g.b.new_block();
            let joined_time = g.b.alloc_bit(SCHEDULER_TIME_WIDTH, false);
            let joined_any = g.b.alloc_bit(1, false);
            let join = g.b.new_block_with(vec![joined_time, joined_any]);
            g.b.seal_block(SIRTerminator::Branch {
                cond: clocked,
                true_block: (scan, Vec::new()),
                false_block: (join, vec![time, any]),
            });
            g.b.switch_to_block(scan);
            for bit in 0..self.clocks.len() {
                let next_edge = g.clock_next(bit);
                let on = g.mask_bit(armed, bit);
                let earlier = g.binary(1, next_edge, BinaryOp::LtU, time);
                let earlier = g.binary(1, earlier, BinaryOp::And, on);
                let next = g.b.alloc_bit(SCHEDULER_TIME_WIDTH, false);
                g.b.emit(SIRInstruction::Mux(next, earlier, next_edge, time));
                time = next;
                any = g.binary(1, any, BinaryOp::Or, on);
            }
            g.b.seal_block(SIRTerminator::Jump(join, vec![time, any]));
            g.b.switch_to_block(join);
            time = joined_time;
            any = joined_any;
        }
        let limit = g.load(slots.limit, SCHEDULER_TIME_WIDTH);
        let after = g.binary(1, time, BinaryOp::GtU, limit);
        let one = g.constant(1, 1);
        let none = g.binary(1, any, BinaryOp::Xor, one);
        let stop = g.binary(1, after, BinaryOp::Or, none);
        let run = g.b.new_block();
        let limit_exit = g.exit_block(SchedulerExit::Limit);
        // The host reads the run's results settled, unless the caller
        // observes them itself (`defer_limit_settle`).
        let stopped = match &self.lazy {
            Some(_) => g.b.new_block(),
            None => limit_exit,
        };
        // A run that stops here leaves the next slot it judged, `u64::MAX`
        // with nothing to do, which `time` still holds then. Work at the
        // last time is left unjudged, since it reads the same.
        let judged = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: stop,
            true_block: (judged, Vec::new()),
            false_block: (run, Vec::new()),
        });
        g.b.switch_to_block(judged);
        let last = g.constant(u64::MAX, SCHEDULER_TIME_WIDTH);
        let at_last = g.binary(1, time, BinaryOp::Eq, last);
        let ambiguous = g.binary(1, at_last, BinaryOp::And, any);
        let unknown = g.constant(celox_design::SCHEDULER_NEXT_UNJUDGED, SCHEDULER_TIME_WIDTH);
        let next = g.b.alloc_bit(SCHEDULER_TIME_WIDTH, false);
        g.b.emit(SIRInstruction::Mux(next, ambiguous, unknown, time));
        g.store(slots.next, SCHEDULER_TIME_WIDTH, next);
        g.b.seal_block(SIRTerminator::Jump(stopped, Vec::new()));
        if self.lazy.is_some() {
            g.b.switch_to_block(stopped);
            let deferred = g.load(slots.defer_limit_settle, SCHEDULER_FLAG_WIDTH);
            let zero = g.constant(0, SCHEDULER_FLAG_WIDTH);
            let deferred = g.binary(1, deferred, BinaryOp::Ne, zero);
            let settle = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: deferred,
                true_block: (limit_exit, Vec::new()),
                false_block: (settle, Vec::new()),
            });
            g.b.switch_to_block(settle);
            g.settle_then(limit_exit);
        }

        let internal_exit = g.exit_block(SchedulerExit::Internal);
        // The kernels due now are ready.
        g.b.switch_to_block(run);
        if !self.service_processes.is_empty() {
            if guards.is_some() {
                ordered::Guards::process_phase(&mut g);
            } else {
                g.service_call(
                    self.service_function.expect("native host services"),
                    u64::MAX,
                    crate::simulator::native_io::PROCESS_PHASE,
                    time,
                    internal_exit,
                );
            }
        }
        let mut ready = g.load(slots.ready, g.ready_bits);
        if !delays.is_empty() && g.ready_bits > 64 {
            for &(index, status) in &delays {
                let wake = g.load(slots.wake[index], SCHEDULER_TIME_WIDTH);
                let waiting = g.waiting_for_time(index, status);
                let due = g.binary(1, wake, BinaryOp::Eq, time);
                let mut due = g.binary(1, due, BinaryOp::And, waiting);
                if host_inputs && self.inactive.iter().any(|&(process, _)| process == index) {
                    let process = &schema.processes[index];
                    let status = g.load(process.status, celox_design::PROCESS_STATUS_WIDTH);
                    let inactive = g.equals(status, u64::from(ProcessStatus::InactiveDelay.code()));
                    let amount = g.load(process.delay, celox_design::PROCESS_DELAY_WIDTH);
                    let zero = g.equals(amount, 0);
                    let inactive = g.binary(1, inactive, BinaryOp::And, zero);
                    let active = g.equals(inactive, 0);
                    due = g.binary(1, due, BinaryOp::And, active);
                }
                let bit = g.bit(due, ordinals[index]);
                ready = g.binary(g.ready_bits, ready, BinaryOp::Or, bit);
            }
        } else if !delays.is_empty() {
            let timed = g.load(slots.timed, g.ready_bits);
            ready = g.scan_timed(
                timed,
                &delays,
                &ordinals,
                ready,
                g.ready_bits,
                |g, kernels, bits, ready| {
                    // The kernels at their wake time, as a word masked by
                    // the group's timed bits once.
                    let mut due_bits = g.constant(0, g.ready_bits);
                    for &(index, _) in kernels {
                        let wake = g.load(slots.wake[index], SCHEDULER_TIME_WIDTH);
                        let mut due = g.binary(1, wake, BinaryOp::Eq, time);
                        if host_inputs && self.inactive.iter().any(|&(process, _)| process == index)
                        {
                            // Imported #0 work belongs to Inactive. A
                            // computed positive delay instead resumes in
                            // Active at its future timestamp.
                            let process = &schema.processes[index];
                            let status = g.load(process.status, celox_design::PROCESS_STATUS_WIDTH);
                            let inactive =
                                g.equals(status, u64::from(ProcessStatus::InactiveDelay.code()));
                            let amount = g.load(process.delay, celox_design::PROCESS_DELAY_WIDTH);
                            let zero = g.equals(amount, 0);
                            let inactive = g.binary(1, inactive, BinaryOp::And, zero);
                            let active = g.equals(inactive, 0);
                            due = g.binary(1, due, BinaryOp::And, active);
                        }
                        let bit = g.bit(due, ordinals[index]);
                        due_bits = g.binary(g.ready_bits, due_bits, BinaryOp::Or, bit);
                    }
                    let due_bits = g.binary(g.ready_bits, due_bits, BinaryOp::And, bits);
                    g.binary(g.ready_bits, ready, BinaryOp::Or, due_bits)
                },
            );
        }
        g.store(slots.ready, g.ready_bits, ready);
        if host_inputs {
            let zero = g.constant(0, 64);
            g.arena_header_store(
                std::mem::offset_of!(crate::simulator::native_io::ArenaHeader, region_entry),
                zero,
            );
        }
        // A pending settle runs, as at the end of the last slot, before a
        // kernel that observes it may run: one due now, any waiting one,
        // which may wake in any pass, or any one in a slot after which a
        // delay may overflow.
        if let Some(lazy) = &self.lazy {
            let waits = waits.iter().any(|&(index, _)| lazy.kernels[index]);
            let observers = delays
                .iter()
                .map(|&(index, _)| index)
                .filter(|&index| lazy.kernels[index])
                .fold(BigUint::default(), |mask, index| {
                    mask | (BigUint::from(1u8) << ordinals[index])
                });
            let late = lazy.latest < u64::MAX;
            if waits || observers != BigUint::default() || late {
                let mut observed = g.constant(u64::from(waits), 1);
                if observers != BigUint::default() {
                    let mask = g.wide_constant(observers, g.ready_bits);
                    let due = g.binary(g.ready_bits, ready, BinaryOp::And, mask);
                    let zero = g.constant(0, g.ready_bits);
                    let due = g.binary(1, due, BinaryOp::Ne, zero);
                    observed = g.binary(1, observed, BinaryOp::Or, due);
                }
                if late {
                    let latest = g.constant(lazy.latest, SCHEDULER_TIME_WIDTH);
                    let after = g.binary(1, time, BinaryOp::GtU, latest);
                    observed = g.binary(1, observed, BinaryOp::Or, after);
                }
                if host_inputs {
                    // Raw host pulses advance waiting baselines before the
                    // first settle. The entry path below performs that settle.
                    let host = host_inputs::pending(&mut g);
                    let ordinary = g.equals(host, 0);
                    observed = g.binary(1, observed, BinaryOp::And, ordinary);
                }
                let settle = g.b.new_block();
                let slot = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: observed,
                    true_block: (settle, Vec::new()),
                    false_block: (slot, Vec::new()),
                });
                g.b.switch_to_block(settle);
                g.settle_then(slot);
                g.b.switch_to_block(slot);
            }
        }
        // Whether this slot is at a later timestamp than the last one: a
        // run also takes up work left at the timestamp it resumes at.
        let advanced = (!schema.sample_shadows.is_empty()).then(|| {
            let last = g.load(slots.now, SCHEDULER_TIME_WIDTH);
            g.binary(1, time, BinaryOp::Ne, last)
        });
        g.store(slots.now, SCHEDULER_TIME_WIDTH, time);
        if let Some(nbas) = &nbas {
            nbas.promote(&mut g, time);
        }
        let fresh = g.constant(0, SCHEDULER_FLAG_WIDTH);
        g.store_at(
            slots.defer_limit_settle,
            celox_design::SCHEDULER_ACTIVE_RESUME_BIT,
            SCHEDULER_FLAG_WIDTH,
            fresh,
        );
        if assertions {
            g.store_at(
                slots.defer_limit_settle,
                celox_design::SCHEDULER_ASSERTION_PHASE_BIT,
                SCHEDULER_FLAG_WIDTH,
                fresh,
            );
        }
        if !self.gating.is_empty() {
            for bit in [GATES_JUDGED_BIT, GATE_SOURCE_EDGED_BIT] {
                g.store_at(slots.defer_limit_settle, bit, SCHEDULER_FLAG_WIDTH, fresh);
            }
        }
        let count = g.load(slots.count, SCHEDULER_TIME_WIDTH);
        let one = g.constant(1, SCHEDULER_TIME_WIDTH);
        let count = g.binary(SCHEDULER_TIME_WIDTH, count, BinaryOp::Add, one);
        g.store(slots.count, SCHEDULER_TIME_WIDTH, count);
        for &slot in &schema.time_slots {
            g.store_state(slot, 0, TIME_SLOT_WIDTH, time);
        }
        // Refresh the sampled-value shadows from the state the previous
        // slot settled, as the runtime does before a later timestamp: a
        // reader in this slot sees the value of the beginning of the
        // slot, in the Preponed region (IEEE 1800-2023 4.4.2.5, 16.5.1).
        // Work taken up at the same timestamp keeps the copies it has.
        if let Some(advanced) = advanced {
            let refresh = g.b.new_block();
            let kept = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: advanced,
                true_block: (refresh, Vec::new()),
                false_block: (kept, Vec::new()),
            });
            g.b.switch_to_block(refresh);
            for &(source, copy) in &schema.sample_shadows {
                let width = program.layout().widths[&source];
                let value = g.load_state(source, 0, width);
                g.store_state(copy, 0, width, value);
            }
            g.b.seal_block(SIRTerminator::Jump(kept, Vec::new()));
            g.b.switch_to_block(kept);
        }
        if schema.time_slots.iter().any(|&slot| g.settle_reads(slot)) {
            if self.incremental.is_some() {
                // The specialized cone includes the time inputs as well as
                // the clock. Preserve any pending invalidation of other data.
                let dirty = g.load(slots.dirty, SCHEDULER_FLAG_WIDTH);
                let time_changed = g.constant(2, SCHEDULER_FLAG_WIDTH);
                let dirty = g.binary(SCHEDULER_FLAG_WIDTH, dirty, BinaryOp::Or, time_changed);
                g.store(slots.dirty, SCHEDULER_FLAG_WIDTH, dirty);
            } else {
                g.mark_dirty();
            }
        }
        // The levels the slot starts from, the baseline of its edges, for an
        // exit in the middle of the slot; those of the clocks' events below,
        // when the run has clocks, unless the slot's waves compare against
        // them: a host clock no clock drives keeps the level the host set.
        let host_entry = host_inputs.then(|| host_inputs::pending(&mut g));
        let host_levels = host_entry.map(|_| g.load_levels());
        let mut levels = g.no_levels();
        for (bit, &event) in tracked.iter().enumerate() {
            if self.clocks.contains(&event) && self.derived.is_empty() {
                continue;
            }
            let level = g.level(&self.events[event]);
            levels = g.with_level(levels, level, bit);
        }
        if let (Some(host), Some(prior)) = (host_entry, host_levels) {
            let value = g.b.alloc_bit(self.event_bits, false);
            g.b.emit(SIRInstruction::Mux(
                value,
                host,
                prior.values,
                levels.values,
            ));
            levels.values = value;
            if let (Some(old), Some(current)) = (prior.unknown, levels.unknown) {
                let unknown = g.b.alloc_bit(self.event_bits, false);
                g.b.emit(SIRInstruction::Mux(unknown, host, old, current));
                levels.unknown = Some(unknown);
            }
        }
        g.store_levels(levels);
        if let Some(host) = host_entry {
            g.if_bit(host, 0, |g| host_inputs::replay(program, g, &checks));
        }
        // The host clocks due now set their levels, in event order, before
        // any kernel runs; their edges start the slot's edges, which
        // collect in `exit_arg`. A clock whose edge applied waits for its
        // next edge until the slot ends. A run without clocks skips them.
        let zero = g.constant(0, self.event_bits);
        g.store(slots.exit_arg, self.event_bits, zero);
        if !self.gating.is_empty() {
            let clear = g.constant(0, SCHEDULER_FLAG_WIDTH);
            g.store_at(
                slots.defer_limit_settle,
                GATE_SOURCE_EDGED_BIT,
                SCHEDULER_FLAG_WIDTH,
                clear,
            );
        }
        let finish_exit = g.exit_block(SchedulerExit::Finish);
        // A host that watches every update sees each kernel run, the host
        // clocks a slot applies and each settle (IEEE 1800-2023 38.36.1).
        let updates = self.service_function.filter(|_| {
            program
                .runtime
                .runtime_schema
                .scheduler
                .as_ref()
                .is_some_and(|scheduler| scheduler.update_points)
        });
        let pick = g.b.new_block();
        let entry_done = pick;
        if let Some((armed, clocked)) = started {
            let apply = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: clocked,
                true_block: (apply, Vec::new()),
                false_block: (entry_done, Vec::new()),
            });
            g.b.switch_to_block(apply);
            // With several clocks the host sees each one's update.
            let each_clock = updates.filter(|_| self.clocks.len() > 1);
            let values = g.load(slots.clock_value, clock_bits);
            let mut fired = g.constant(0, clock_bits);
            let mut edges = g.constant(0, self.event_bits);
            let mut levels = g.load_levels();
            // Distinct polarity domains can share physical clock storage.
            // Capture every baseline before the batch writes any alias.
            let shared_clocks = self
                .clocks
                .iter()
                .map(|&index| {
                    let signal = self.events[index].signal;
                    g.shared.get(&signal).copied().unwrap_or(signal)
                })
                .collect::<HashSet<_>>()
                .len()
                != self.clocks.len();
            let before: Vec<_> = if shared_clocks {
                self.clocks
                    .iter()
                    .map(|&index| g.level(&self.events[index]))
                    .collect()
            } else {
                Vec::new()
            };
            for (bit, &index) in self.clocks.iter().enumerate() {
                let event = &self.events[index];
                let next_edge = g.clock_next(bit);
                let on = g.mask_bit(armed, bit);
                let due = g.binary(1, next_edge, BinaryOp::Eq, time);
                let due = g.binary(1, due, BinaryOp::And, on);
                let value = g.mask_bit(values, bit);
                let actual = g.level(event);
                let position = tracked
                    .iter()
                    .position(|&tracked| tracked == index)
                    .expect("clocks are tracked");
                let baseline = if shared_clocks { before[bit] } else { actual };
                let was = if let (Some(host), Some(prior)) = (host_entry, host_levels) {
                    let old = g.saved_level(prior, position);
                    let was =
                        g.b.alloc_bit(if g.four_state.is_some() { 2 } else { 1 }, false);
                    g.b.emit(SIRInstruction::Mux(was, host, old, baseline));
                    was
                } else {
                    baseline
                };
                levels = g.with_level(levels, was, position);
                let is = if g.four_state.is_none() {
                    let is = g.b.alloc_bit(1, false);
                    g.b.emit(SIRInstruction::Mux(is, due, value, actual));
                    g.store(event.signal, 1, is);
                    is
                } else {
                    // A clock that is not due keeps its state, unknown
                    // bits included; one that is gets a known level.
                    let current = g.load_state(event.signal, 0, 1);
                    let set = if g.keeps_unknowns(event.signal) {
                        g.b.alloc_logic(1)
                    } else {
                        g.b.alloc_bit(1, false)
                    };
                    g.b.emit(SIRInstruction::Mux(set, due, value, current));
                    g.store_state(event.signal, 0, 1, set);
                    let zero = g.constant(0, 1);
                    let known = g.b.alloc_bit(2, false);
                    g.b.emit(SIRInstruction::Concat(known, vec![zero, value]));
                    let is = g.b.alloc_bit(2, false);
                    g.b.emit(SIRInstruction::Mux(is, due, known, actual));
                    is
                };
                // Counters sample iff gates at this write, before the next
                // scheduled clock changes a gate (IEEE 1800-2023 9.4.2.3).
                g.count_host_clock(schema, event.signal, actual, is, due);
                let changed = g.binary(1, was, BinaryOp::Ne, is);
                if let Some(aliases) = &event.aliases {
                    for unit in aliases {
                        g.inline(unit, finish_exit);
                    }
                } else if g.settle_reads(event.signal) {
                    // Other clocks in this batch may not be due. Writing
                    // their old level does not invalidate settled inputs.
                    let dirty = g.load(slots.dirty, SCHEDULER_FLAG_WIDTH);
                    let dirty = g.binary(SCHEDULER_FLAG_WIDTH, dirty, BinaryOp::Or, changed);
                    g.store(slots.dirty, SCHEDULER_FLAG_WIDTH, dirty);
                }
                if !shared_clocks {
                    let edge = g.edge(event, was, is);
                    let edge = g.event_bit(edge, index);
                    edges = g.binary(self.event_bits, edges, BinaryOp::Or, edge);
                }
                let bit_set = g.bit_of(due, bit, clock_bits);
                fired = g.binary(clock_bits, fired, BinaryOp::Or, bit_set);
                // Each due clock is an update of its own, which its waits
                // see before the host does: a callback that restores the
                // level cannot hide the edge (as `step_round`). What the
                // host writes settles after the last clock: a settle here
                // would leave the batch's registers behind.
                if let Some(service) = each_clock {
                    g.if_bit(due, 0, |g| {
                        let enabled = g.arena_header_load(std::mem::offset_of!(
                            crate::simulator::native_io::ArenaHeader,
                            observes_updates
                        ));
                        g.if_bit(enabled, 0, |g| g.check_waits_at(&checks, false));
                        g.report_update(
                            service,
                            &checks,
                            &self.events,
                            finish_exit,
                            AfterWrites::Settle,
                        );
                    });
                }
            }
            // Judge all domains against the completed scheduled batch;
            // otherwise a preceding alias write consumes a later domain's edge.
            if shared_clocks {
                for &index in &self.clocks {
                    let was = g.saved_level(levels, position(&tracked, index));
                    let is = g.level(&self.events[index]);
                    let edge = g.edge(&self.events[index], was, is);
                    let edge = g.event_bit(edge, index);
                    edges = g.binary(self.event_bits, edges, BinaryOp::Or, edge);
                }
            }
            g.store_levels(levels);
            // With the edges the host's writes at the clocks' updates gave.
            let written = g.load(slots.exit_arg, self.event_bits);
            let edges = g.binary(self.event_bits, edges, BinaryOp::Or, written);
            g.store(slots.exit_arg, self.event_bits, edges);
            let all = g.ones(clock_bits);
            let unfired = g.binary(clock_bits, fired, BinaryOp::Xor, all);
            let armed = g.binary(clock_bits, armed, BinaryOp::And, unfired);
            g.store(slots.clock_armed, clock_bits, armed);
            if !waits.is_empty() {
                // The waits see the clocks' levels before the process phase.
                let zero = g.constant(0, clock_bits);
                let applied = g.binary(1, fired, BinaryOp::Ne, zero);
                let check = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: applied,
                    true_block: (check, Vec::new()),
                    false_block: (entry_done, Vec::new()),
                });
                g.b.switch_to_block(check);
                g.check_waits_at(&checks, false);
            }
        }
        if let Some(service) = updates {
            g.report_update(
                service,
                &checks,
                &self.events,
                finish_exit,
                AfterWrites::Settled,
            );
        }
        g.b.seal_block(SIRTerminator::Jump(entry_done, Vec::new()));

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

        let mut full_wait_check = None;
        let mut wait_continuations = vec![pick];
        for (index, run) in runs {
            let kernel = self.kernels[index].as_ref().expect("selected above");
            let process = &schema.processes[index];
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
                if let Some(guards) = &mut guards {
                    guards.charge(
                        &mut g,
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
                    nbas.capture(&mut g, index, site);
                    guards.as_mut().expect("native NBA budget guards").charge(
                        &mut g,
                        index as u64,
                        crate::simulator::native_io::CHECKPOINT,
                        time,
                    );
                    g.b.seal_block(SIRTerminator::Jump(resume, Vec::new()));
                    g.b.switch_to_block(host);
                }
                if let (Some(guards), Some(yielded)) = (&mut guards, yielded) {
                    let checkpoint = g.b.new_block();
                    let host_call = g.b.new_block();
                    g.b.seal_block(SIRTerminator::Branch {
                        cond: yielded,
                        true_block: (checkpoint, Vec::new()),
                        false_block: (host_call, Vec::new()),
                    });
                    g.b.switch_to_block(checkpoint);
                    guards.charge(
                        &mut g,
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
                g.report_update(service, &checks, &[], finish_exit, AfterWrites::Ended);
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
                    queued = Some(
                        queued.map_or(matches, |prior| g.binary(1, prior, BinaryOp::Or, matches)),
                    );
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
                    &checks,
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
                g.check_waits_after_unit(
                    &checks,
                    &program.sir.processes[index],
                    &schema.time_slots,
                );
                g.b.seal_block(SIRTerminator::Jump(pick, Vec::new()));
            }
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
                g.check_waits_at(&checks, false);
                g.b.seal_block(SIRTerminator::Jump(checked, Vec::new()));
                g.b.switch_to_block(checked);
            } else {
                g.check_waits_at(&checks, false);
            }
            g.dispatch_continuations(&wait_continuations, internal_exit);
        }

        // The settles after a pass and after the domains share one copy of
        // combinational logic. A deferred settle after a pass runs for the
        // kernels that may run in another.
        g.b.switch_to_block(pass_end);
        if host_entry.is_some() {
            let pending = host_inputs::pending(&mut g);
            g.if_bit(pending, 0, |g| {
                let zero = g.constant(0, 64);
                g.arena_header_store(
                    std::mem::offset_of!(crate::simulator::native_io::ArenaHeader, host_entry),
                    zero,
                );
                // Raw host writes wake Active processes before the first settle.
                // Judge host domains only after those processes have run, so a
                // process can overwrite the host's level before any FF commits.
                let mut edges = g.load(slots.exit_arg, self.event_bits);
                let levels = g.load_levels();
                for &index in &edged {
                    let was = g.saved_level(levels, position(&tracked, index));
                    let is = g.level(&self.events[index]);
                    let edge = g.settled_edge(&self.events[index], was, is);
                    let bit = g.event_bit(edge, index);
                    edges = g.binary(self.event_bits, edges, BinaryOp::Or, bit);
                }
                g.store(slots.exit_arg, self.event_bits, edges);
            });
        }
        if self.cyclic {
            if let Some(guards) = &mut guards {
                guards.charge(
                    &mut g,
                    u64::MAX,
                    crate::simulator::native_io::SCHEDULER_PASS,
                    time,
                );
            } else {
                g.service_call(
                    self.service_function.expect("native work-list services"),
                    u64::MAX,
                    crate::simulator::native_io::SCHEDULER_PASS,
                    time,
                    internal_exit,
                );
            }
        }
        if !self.cyclic
            && let Some(service) = self.service_function
        {
            let enabled = g.arena_header_load(std::mem::offset_of!(
                crate::simulator::native_io::ArenaHeader,
                slot_observer
            ));
            g.if_bit(enabled, 0, |g| {
                g.service_call(
                    service,
                    u64::MAX,
                    crate::simulator::native_io::SCHEDULER_PASS,
                    time,
                    internal_exit,
                );
            });
        }
        let none = g.constant(u64::MAX, SCHEDULER_TIME_WIDTH);
        g.store(slots.current, SCHEDULER_TIME_WIDTH, none);
        let settled = g.b.new_block();
        let slot_end = g.b.new_block();
        let wait_observers = self
            .lazy
            .as_ref()
            .is_some_and(|lazy| waits.iter().any(|&(index, _)| lazy.kernels[index]));
        // Gate edges must see the first settle, before the processes that
        // it wakes can change an enable. Later passes may settle lazily.
        if !self.gating.is_empty() && self.lazy.is_some() {
            let judged = g.load_at(
                slots.defer_limit_settle,
                GATES_JUDGED_BIT,
                SCHEDULER_FLAG_WIDTH,
            );
            let first = g.b.new_block();
            let later = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: judged,
                true_block: (later, Vec::new()),
                false_block: (first, Vec::new()),
            });
            g.b.switch_to_block(first);
            g.settle_then(settled);
            g.b.switch_to_block(later);
        }
        match &self.lazy {
            Some(lazy) => g.eager_then(&lazy.eager, settled),
            None => g.settle_then(settled),
        }

        // After the settle of a pass the waits see its writes; woken
        // kernels run in another pass.
        g.b.switch_to_block(settled);
        if !self.counted.is_empty() {
            let counted = self.counted_edges(&mut g);
            if !self.gating.is_empty() {
                let edged = self.source_edged(&mut g, counted);
                g.store_at(
                    slots.defer_limit_settle,
                    GATE_SOURCE_EDGED_BIT,
                    SCHEDULER_FLAG_WIDTH,
                    edged,
                );
            }
            let active = g.load(slots.exit_arg, self.event_bits);
            let active = g.binary(self.event_bits, active, BinaryOp::Or, counted);
            g.store(slots.exit_arg, self.event_bits, active);
        }
        self.judge_gates(&mut g, &tracked);
        g.check_waits(&checks);
        if !deferred.is_empty() {
            let ready = g.load(slots.ready, g.ready_bits);
            let zero = g.constant(0, g.ready_bits);
            let idle = g.binary(1, ready, BinaryOp::Eq, zero);
            let promote = g.b.new_block();
            let promoted = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: idle,
                true_block: (promote, Vec::new()),
                false_block: (promoted, Vec::new()),
            });
            g.b.switch_to_block(promote);
            let ready = g.status_ready(&deferred, &ordinals, schema, ProcessStatus::DeferActive);
            g.store(slots.ready, g.ready_bits, ready);
            g.b.seal_block(SIRTerminator::Jump(promoted, Vec::new()));
            g.b.switch_to_block(promoted);
        }
        let ready = g.load(slots.ready, g.ready_bits);
        let zero = g.constant(0, g.ready_bits);
        let more = g.binary(1, ready, BinaryOp::Ne, zero);
        let domains = g.b.new_block();
        let resume_domains =
            (!self.active.is_empty() || self.ordered.is_some()).then(|| g.b.new_block());
        let route_domains = resume_domains.map(|_| g.b.new_block()).unwrap_or(domains);
        let another = if wait_observers {
            g.b.new_block()
        } else {
            pick
        };
        g.b.seal_block(SIRTerminator::Branch {
            cond: more,
            true_block: (another, Vec::new()),
            false_block: (route_domains, Vec::new()),
        });
        if let Some(resume) = resume_domains {
            g.b.switch_to_block(route_domains);
            let pending = g.load_at(
                slots.defer_limit_settle,
                celox_design::SCHEDULER_ACTIVE_RESUME_BIT,
                SCHEDULER_FLAG_WIDTH,
            );
            g.b.seal_block(SIRTerminator::Branch {
                cond: pending,
                true_block: (resume, Vec::new()),
                false_block: (domains, Vec::new()),
            });
        }
        if wait_observers {
            g.b.switch_to_block(another);
            g.settle_then(pick);
        }

        // The domains the edges trigger, then the NBA region.
        g.b.switch_to_block(domains);
        self.observe_slot(&mut g, &tracked, &checks, finish_exit, true);
        if !self.active.is_empty() || self.ordered.is_some() {
            self.batch_active(
                &mut g,
                &tracked,
                slot_end,
                finish_exit,
                &checks,
                pick,
                resume_domains.expect("active resume block"),
            );
        } else if !self.derived.is_empty()
            || !self.counted.is_empty()
            || self.events.iter().any(|event| event.any_settled)
        {
            // Unspecified domains use rising edges for scheduled writes, but
            // any change for later settled waves. Keep that ordering when
            // another clock's commit changes the domain's inputs.
            self.batch(&mut g, &tracked, slot_end, finish_exit, &checks);
        } else {
            let staged: Vec<&Staged> = waits.iter().flat_map(|(_, wait)| &wait.sites).collect();
            // A host that sees the updates of the wave can write a clock
            // after its domains ran: the edge starts another wave.
            let rewave = updates.map(|_| g.b.new_block());
            let wave_end = rewave.map_or(slot_end, |_| g.b.new_block());
            let mask = g.load(slots.exit_arg, self.event_bits);
            let zero = g.constant(0, self.event_bits);
            let quiet = g.binary(1, mask, BinaryOp::Eq, zero);
            let triggered = g.b.new_block();
            let untriggered = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: quiet,
                true_block: (untriggered, Vec::new()),
                false_block: (triggered, Vec::new()),
            });

            // Without a domain, published nonblocking values settle once more.
            g.b.switch_to_block(untriggered);
            if let Some(lazy) = &self.lazy {
                g.publish(&staged);
                g.publish_service(
                    self.service_function
                        .filter(|_| !self.nba_processes.is_empty()),
                    &checks,
                );
                g.eager_then(&lazy.eager, slot_end);
            } else if staged.is_empty() && self.nba_processes.is_empty() {
                g.b.seal_block(SIRTerminator::Jump(slot_end, Vec::new()));
            } else {
                let mut any = g.constant(u64::from(!self.nba_processes.is_empty()), 1);
                for site in &staged {
                    let pending = g.load(site.pending, 1);
                    any = g.binary(1, any, BinaryOp::Or, pending);
                }
                g.publish(&staged);
                g.publish_service(
                    self.service_function
                        .filter(|_| !self.nba_processes.is_empty()),
                    &checks,
                );
                let publish_settle = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: any,
                    true_block: (publish_settle, Vec::new()),
                    false_block: (slot_end, Vec::new()),
                });
                g.b.switch_to_block(publish_settle);
                g.settle_then(wave_end);
            }

            g.b.switch_to_block(triggered);
            let evaluated = g.b.new_block();
            if edged.is_empty() {
                // No event can have an edge; the mask stays empty.
                g.b.seal_block(SIRTerminator::Jump(evaluated, Vec::new()));
            } else {
                let one = g.constant(1, self.event_bits);
                let lower = g.binary(self.event_bits, mask, BinaryOp::Sub, one);
                let others = g.binary(self.event_bits, mask, BinaryOp::And, lower);
                let single = g.binary(1, others, BinaryOp::Eq, zero);
                let one_domain = g.b.new_block();
                let domains = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: single,
                    true_block: (one_domain, Vec::new()),
                    false_block: (domains, Vec::new()),
                });

                // One triggered domain evaluates and applies at once. The events
                // kernels write are tested first: a run without clocks reaches
                // their domains as soon as without clock events.
                g.b.switch_to_block(one_domain);
                let written = self.written();
                let mut seen = HashSet::default();
                for &index in written
                    .iter()
                    .chain(&self.clocks)
                    .chain(&self.host_edges)
                    .filter(|&&index| seen.insert(index))
                {
                    let event = &self.events[index];
                    let bit = g.single(index, self.event_bits);
                    let this = g.binary(1, mask, BinaryOp::Eq, bit);
                    let unit = g.b.new_block();
                    let next = g.b.new_block();
                    g.b.seal_block(SIRTerminator::Branch {
                        cond: this,
                        true_block: (unit, Vec::new()),
                        false_block: (next, Vec::new()),
                    });
                    g.b.switch_to_block(unit);
                    if event.active_commit
                        && event
                            .eval_apply
                            .iter()
                            .any(|unit| emits_finish(unit, &self.finish_sites))
                        && let (Some(eval), Some(apply)) = (&event.eval_only, &event.apply)
                    {
                        // Unobserved blocking writes may let us skip Active
                        // waves, but `$finish` must still precede NBA publication.
                        if self.lazy.is_some() {
                            let evaluate = g.b.new_block();
                            g.settle_then(evaluate);
                            g.b.switch_to_block(evaluate);
                        }
                        g.inline_writers(eval, finish_exit);
                        g.inline_writers(apply, finish_exit);
                        g.b.seal_block(SIRTerminator::Jump(evaluated, Vec::new()));
                        g.b.switch_to_block(next);
                        continue;
                    }
                    let units = match self.lazy.as_ref().map(|lazy| &lazy.domains[index]) {
                        None | Some(DomainSettle::Pending) => &event.eval_apply,
                        Some(DomainSettle::Fused(units)) => {
                            // Fusion is needed only when combinational inputs
                            // changed since the last settle. Otherwise the
                            // ordinary FF unit already has its inputs.
                            let dirty = g.load(slots.dirty, SCHEDULER_FLAG_WIDTH);
                            let fused = g.b.new_block();
                            let clean = g.b.new_block();
                            g.b.seal_block(SIRTerminator::Branch {
                                cond: dirty,
                                true_block: (fused, Vec::new()),
                                false_block: (clean, Vec::new()),
                            });
                            g.b.switch_to_block(clean);
                            g.inline_writers(&event.eval_apply, finish_exit);
                            g.b.seal_block(SIRTerminator::Jump(evaluated, Vec::new()));
                            g.b.switch_to_block(fused);
                            units
                        }
                        Some(DomainSettle::Settled) => {
                            let evaluate = g.b.new_block();
                            g.settle_then(evaluate);
                            g.b.switch_to_block(evaluate);
                            &event.eval_apply
                        }
                    };
                    g.inline_writers(units, finish_exit);
                    g.b.seal_block(SIRTerminator::Jump(evaluated, Vec::new()));
                    g.b.switch_to_block(next);
                }
                g.b.seal_block(SIRTerminator::Jump(internal_exit, Vec::new()));

                // Several evaluate, then apply, each in event order.
                g.b.switch_to_block(domains);
                if edged.len() < 2 {
                    g.b.seal_block(SIRTerminator::Jump(internal_exit, Vec::new()));
                } else {
                    let mut mask = mask;
                    let mut zero = zero;
                    if self.lazy.as_ref().is_some_and(|lazy| lazy.several) {
                        let evaluate = g.b.new_block();
                        g.settle_then(evaluate);
                        g.b.switch_to_block(evaluate);
                        mask = g.load(slots.exit_arg, self.event_bits);
                        zero = g.constant(0, self.event_bits);
                    }
                    for phase in [Phase::Evaluate, Phase::Apply] {
                        for &index in &edged {
                            let event = &self.events[index];
                            let bit = g.single(index, self.event_bits);
                            let selected = g.binary(self.event_bits, mask, BinaryOp::And, bit);
                            let this = g.binary(1, selected, BinaryOp::Ne, zero);
                            let unit = g.b.new_block();
                            let next = g.b.new_block();
                            g.b.seal_block(SIRTerminator::Branch {
                                cond: this,
                                true_block: (unit, Vec::new()),
                                false_block: (next, Vec::new()),
                            });
                            g.b.switch_to_block(unit);
                            let units = match phase {
                                Phase::Evaluate => event.eval_only.as_ref(),
                                Phase::Apply => event.apply.as_ref(),
                            }
                            .expect("several edged events have their units (`Plan::new`)");
                            g.inline_writers(units, finish_exit);
                            g.b.seal_block(SIRTerminator::Jump(next, Vec::new()));
                            g.b.switch_to_block(next);
                        }
                    }
                    g.b.seal_block(SIRTerminator::Jump(evaluated, Vec::new()));
                }
            }

            // The domains' results and the nonblocking values publish before
            // combinational logic settles.
            g.b.switch_to_block(evaluated);
            if rewave.is_some() {
                // The edges are consumed: what remains at the end of the
                // wave the host wrote after them.
                let zero = g.constant(0, self.event_bits);
                g.store(slots.exit_arg, self.event_bits, zero);
            }
            g.publish(&staged);
            g.publish_service(
                self.service_function
                    .filter(|_| !self.nba_processes.is_empty()),
                &checks,
            );
            match &self.lazy {
                Some(lazy) => g.eager_then(&lazy.eager, wave_end),
                None => g.settle_then(wave_end),
            }
            if let Some(rewave) = rewave {
                g.b.switch_to_block(wave_end);
                let edges = g.load(slots.exit_arg, self.event_bits);
                let zero = g.constant(0, self.event_bits);
                let quiet = g.binary(1, edges, BinaryOp::Eq, zero);
                g.b.seal_block(SIRTerminator::Branch {
                    cond: quiet,
                    true_block: (slot_end, Vec::new()),
                    false_block: (rewave, Vec::new()),
                });
                // Another pass judges them, as after the host clocks of
                // the slot, against the levels now.
                g.b.switch_to_block(rewave);
                let mut levels = g.no_levels();
                for (bit, &event) in tracked.iter().enumerate() {
                    let level = g.level(&self.events[event]);
                    levels = g.with_level(levels, level, bit);
                }
                g.store_levels(levels);
                g.b.seal_block(SIRTerminator::Jump(pick, Vec::new()));
            }
        }

        // At the end of the slot each clock whose edge applied waits for the
        // opposite level, half a period later, in event order (as
        // `rearm_clocks`): the extra time unit of an odd period goes to the
        // high phase. The reference re-arms at the end of the slot's first
        // round (`continue_round`), before processes the NBA results wake
        // resume. On their later rounds the clocks are already armed.
        g.b.switch_to_block(slot_end);
        if let Some((started, clocked)) = started {
            let overflow_exit = g.exit_block(SchedulerExit::ClockOverflow);
            let rearms = g.b.new_block();
            let rearmed = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: clocked,
                true_block: (rearms, Vec::new()),
                false_block: (rearmed, Vec::new()),
            });
            g.b.switch_to_block(rearms);
            // The clocks whose edge applied are those the slot disarmed.
            let armed = g.load(slots.clock_armed, clock_bits);
            let all = g.ones(clock_bits);
            let unarmed = g.binary(clock_bits, armed, BinaryOp::Xor, all);
            let fired = g.binary(clock_bits, started, BinaryOp::And, unarmed);
            for bit in 0..self.clocks.len() {
                let applied = g.mask_bit(fired, bit);
                let rearm = g.b.new_block();
                let next = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: applied,
                    true_block: (rearm, Vec::new()),
                    false_block: (next, Vec::new()),
                });
                g.b.switch_to_block(rearm);
                let values = g.load(slots.clock_value, clock_bits);
                let high = g.mask_bit(values, bit);
                let period = g.load_at(
                    slots.clock_period,
                    bit * SCHEDULER_TIME_WIDTH,
                    SCHEDULER_TIME_WIDTH,
                );
                let one = g.constant(1, SCHEDULER_TIME_WIDTH);
                let half = g.binary(SCHEDULER_TIME_WIDTH, period, BinaryOp::Shr, one);
                let odd = g.binary(SCHEDULER_TIME_WIDTH, period, BinaryOp::And, one);
                let even = g.constant(0, SCHEDULER_TIME_WIDTH);
                let extra = g.b.alloc_bit(SCHEDULER_TIME_WIDTH, false);
                g.b.emit(SIRInstruction::Mux(extra, high, odd, even));
                let half = g.binary(SCHEDULER_TIME_WIDTH, half, BinaryOp::Add, extra);
                let next_edge = g.binary(SCHEDULER_TIME_WIDTH, time, BinaryOp::Add, half);
                let overflow = g.binary(1, next_edge, BinaryOp::LtU, time);
                let overflowed = g.b.new_block();
                let in_range = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: overflow,
                    true_block: (overflowed, Vec::new()),
                    false_block: (in_range, Vec::new()),
                });
                g.b.switch_to_block(overflowed);
                let clock = g.constant(bit as u64, SCHEDULER_TIME_WIDTH);
                g.store(slots.exit_arg, SCHEDULER_TIME_WIDTH, clock);
                // The slot is complete: the host reads its results settled.
                match &self.lazy {
                    Some(_) => g.settle_then(overflow_exit),
                    None => {
                        g.b.seal_block(SIRTerminator::Jump(overflow_exit, Vec::new()));
                    }
                }
                g.b.switch_to_block(in_range);
                g.store_at(
                    slots.clock_next,
                    bit * SCHEDULER_TIME_WIDTH,
                    SCHEDULER_TIME_WIDTH,
                    next_edge,
                );
                let flip = g.single(bit, clock_bits);
                let values = g.binary(clock_bits, values, BinaryOp::Xor, flip);
                g.store(slots.clock_value, clock_bits, values);
                let armed = g.load(slots.clock_armed, clock_bits);
                let armed = g.binary(clock_bits, armed, BinaryOp::Or, flip);
                g.store(slots.clock_armed, clock_bits, armed);
                g.b.seal_block(SIRTerminator::Jump(next, Vec::new()));
                g.b.switch_to_block(next);
            }
            g.b.seal_block(SIRTerminator::Jump(rearmed, Vec::new()));
            g.b.switch_to_block(rearmed);
        }
        let reactive_waits = waits.iter().any(|(_, wait)| wait.reactive);
        if reactive_waits || (self.service_function.is_some() && !waits.is_empty()) {
            // A host callback can wake an otherwise undriven input wait. Its
            // activation must drain natively after the domains too.
            let observer_done = (!reactive_waits).then(|| g.b.new_block());
            if let Some(done) = observer_done {
                let enabled = g.arena_header_load(std::mem::offset_of!(
                    crate::simulator::native_io::ArenaHeader,
                    slot_observer
                ));
                let enabled = g.equals(enabled, 1);
                let scan = g.b.new_block();
                g.b.seal_block(SIRTerminator::Branch {
                    cond: enabled,
                    true_block: (scan, Vec::new()),
                    false_block: (done, Vec::new()),
                });
                g.b.switch_to_block(scan);
            }
            // NBA publication can wake data-sensitive kernels in this same
            // time slot. Their next pass must not evaluate the domains that
            // just ran a second time: compare against the levels now and
            // consume the slot's original edge mask before returning to pick.
            g.check_waits(&checks);
            let ready = g.load(slots.ready, g.ready_bits);
            let zero = g.constant(0, g.ready_bits);
            let more = g.binary(1, ready, BinaryOp::Ne, zero);
            let resume = g.b.new_block();
            let drained = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: more,
                true_block: (resume, Vec::new()),
                false_block: (drained, Vec::new()),
            });
            g.b.switch_to_block(resume);
            if !self.service_processes.is_empty() {
                if guards.is_some() {
                    ordered::Guards::process_phase(&mut g);
                } else {
                    g.service_call(
                        self.service_function.expect("native host services"),
                        u64::MAX,
                        crate::simulator::native_io::PROCESS_PHASE,
                        time,
                        internal_exit,
                    );
                }
            }
            let mut levels = g.no_levels();
            for (bit, &event) in tracked.iter().enumerate() {
                let level = g.level(&self.events[event]);
                levels = g.with_level(levels, level, bit);
            }
            g.store_levels(levels);
            let zero = g.constant(0, self.event_bits);
            g.store(slots.exit_arg, self.event_bits, zero);
            if !self.gating.is_empty() {
                let clear = g.constant(0, SCHEDULER_FLAG_WIDTH);
                g.store_at(
                    slots.defer_limit_settle,
                    GATE_SOURCE_EDGED_BIT,
                    SCHEDULER_FLAG_WIDTH,
                    clear,
                );
            }
            g.b.seal_block(SIRTerminator::Jump(pick, Vec::new()));
            g.b.switch_to_block(drained);
            if let Some(done) = observer_done {
                g.b.seal_block(SIRTerminator::Jump(done, Vec::new()));
                g.b.switch_to_block(done);
            }
        }
        if assertions {
            // IEEE 1800-2023 4.4.2.5-6: drain all Observed evaluations,
            // then their Reactive actions, before returning to Active work.
            // Predicates may read live combinational disable conditions.
            g.settle_then(observed_entry.unwrap());
            for (entry, kernels, status, phase, empty) in [
                (
                    observed_entry.unwrap(),
                    &observed,
                    ProcessStatus::AssertionObserved,
                    1,
                    reactive_entry.unwrap(),
                ),
                (
                    reactive_entry.unwrap(),
                    &reactive,
                    ProcessStatus::AssertionReactive,
                    2,
                    slot_done.unwrap(),
                ),
            ] {
                g.b.switch_to_block(entry);
                let none = g.constant(u64::MAX, SCHEDULER_TIME_WIDTH);
                g.store(slots.current, SCHEDULER_TIME_WIDTH, none);
                let ready = g.status_ready(kernels, &ordinals, schema, status);
                g.store(slots.ready, g.ready_bits, ready);
                let phase_value = g.constant(phase, SCHEDULER_FLAG_WIDTH);
                g.store_at(
                    slots.defer_limit_settle,
                    celox_design::SCHEDULER_ASSERTION_PHASE_BIT,
                    SCHEDULER_FLAG_WIDTH,
                    phase_value,
                );
                if phase == 2 {
                    let mut levels = g.no_levels();
                    for (bit, &event) in tracked.iter().enumerate() {
                        let level = g.level(&self.events[event]);
                        levels = g.with_level(levels, level, bit);
                    }
                    g.store_levels(levels);
                    let zero = g.constant(0, self.event_bits);
                    g.store(slots.exit_arg, self.event_bits, zero);
                    if !self.gating.is_empty() {
                        let clear = g.constant(0, SCHEDULER_FLAG_WIDTH);
                        g.store_at(
                            slots.defer_limit_settle,
                            GATE_SOURCE_EDGED_BIT,
                            SCHEDULER_FLAG_WIDTH,
                            clear,
                        );
                    }
                }
                let zero = g.constant(0, g.ready_bits);
                let any = g.binary(1, ready, BinaryOp::Ne, zero);
                g.b.seal_block(SIRTerminator::Branch {
                    cond: any,
                    true_block: (pick, Vec::new()),
                    false_block: (empty, Vec::new()),
                });
            }
            g.b.switch_to_block(slot_done.unwrap());
        }
        // The run returns after the slot that reaches its budget, or in
        // which its runtime events reach a batch, so that the host drains
        // them before the ring buffer overflows.
        let count = g.load(slots.count, SCHEDULER_TIME_WIDTH);
        let budget = g.load(slots.budget, SCHEDULER_TIME_WIDTH);
        let mut returns = g.binary(1, count, BinaryOp::GeU, budget);
        if let Some(batch) = g.event_batch {
            let events = g.load(slots.events, SCHEDULER_TIME_WIDTH);
            let batch = g.constant(batch, SCHEDULER_TIME_WIDTH);
            let full = g.binary(1, events, BinaryOp::GeU, batch);
            returns = g.binary(1, returns, BinaryOp::Or, full);
        }
        // A run that returns there has not judged its next slot.
        let unjudged = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: returns,
            true_block: (unjudged, Vec::new()),
            false_block: (header, Vec::new()),
        });
        g.b.switch_to_block(unjudged);
        let unknown = g.constant(celox_design::SCHEDULER_NEXT_UNJUDGED, SCHEDULER_TIME_WIDTH);
        g.store(slots.next, SCHEDULER_TIME_WIDTH, unknown);
        g.b.seal_block(SIRTerminator::Jump(stopped, Vec::new()));
        g.emit_settle(
            &program.sir.eval_comb,
            finish_exit,
            internal_exit,
            updates.map(|service| (service, &checks[..], &self.events[..])),
        );
        if let Some(lazy) = &self.lazy {
            g.emit_eager(&lazy.eager, finish_exit, internal_exit);
        }
        if let Some(guards) = guards {
            guards.emit(&mut g, internal_exit);
        }
        g.finish()
    }
}
