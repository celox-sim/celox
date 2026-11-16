//! The start of a slot: its time, the kernels due, the levels its edges are
//! judged against, and the host clocks that apply.
use super::*;

impl Plan {
    /// The head of the slot loop: the time of the next slot, the earliest wake
    /// time of a kernel, queued nonblocking update or host clock, and whether
    /// the run stops before it. Returns that time, the host clocks armed at the
    /// start of the slot with whether any is, the block the slot runs in, and
    /// the block a run that stops continues at.
    pub(super) fn next_slot(
        &self,
        g: &mut Generator<'_>,
        cx: &Slot<'_>,
        nbas: &Option<nba::Sites>,
    ) -> (
        RegisterId,
        Option<(RegisterId, RegisterId)>,
        celox_sir::BlockId,
        celox_sir::BlockId,
    ) {
        let &Slot {
            slots,
            clock_bits,
            delays,
            ordinals,
            host_inputs,
            header,
            ..
        } = cx;
        // The timed mask from the kernels' state, which the host may have
        // changed between runs; the slots keep it up to date from here on.
        let mut timed = g.constant(0, g.ready_bits);
        for &(index, status) in delays {
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
            let host = host_inputs::pending(g);
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
            for &(index, status) in delays {
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
                delays,
                ordinals,
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
        if let Some(nbas) = nbas {
            (time, any) = nbas.next_time(g, time, any);
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
        (time, started, run, stopped)
    }

    /// The start of a slot at `time`: the kernels due become ready, a pending
    /// settle runs for those that observe it, and the slot records its time and
    /// the levels its edges are judged against. Returns the pending host input
    /// flag and the levels the host left, in a unit that takes host inputs.
    pub(super) fn start_slot(
        &self,
        g: &mut Generator<'_>,
        cx: &Slot<'_>,
        nbas: &Option<nba::Sites>,
        guards: &Option<ordered::Guards>,
        time: RegisterId,
        run: celox_sir::BlockId,
        internal_exit: celox_sir::BlockId,
    ) -> (Option<RegisterId>, Option<Levels>) {
        let &Slot {
            program,
            slots,
            schema,
            tracked,
            waits,
            delays,
            ordinals,
            checks,
            assertions,
            host_inputs,
            ..
        } = cx;
        // The kernels due now are ready.
        g.b.switch_to_block(run);
        if !self.service_processes.is_empty() {
            if guards.is_some() {
                ordered::Guards::process_phase(g);
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
            for &(index, status) in delays {
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
                delays,
                ordinals,
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
                    let host = host_inputs::pending(g);
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
        if let Some(nbas) = nbas {
            nbas.promote(g, time);
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
        let host_entry = host_inputs.then(|| host_inputs::pending(g));
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
            g.if_bit(host, 0, |g| host_inputs::replay(program, g, checks));
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
        (host_entry, host_levels)
    }

    /// The host clocks due in the slot set their levels before any kernel
    /// runs, and the process phase starts at `entry_done`.
    pub(super) fn apply_host_clocks(
        &self,
        g: &mut Generator<'_>,
        cx: &Slot<'_>,
        started: Option<(RegisterId, RegisterId)>,
        time: RegisterId,
        host_entry: Option<RegisterId>,
        host_levels: Option<Levels>,
        updates: Option<u32>,
        finish_exit: celox_sir::BlockId,
        entry_done: celox_sir::BlockId,
    ) {
        let &Slot {
            slots,
            schema,
            tracked,
            clock_bits,
            waits,
            checks,
            ..
        } = cx;
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
                        g.if_bit(enabled, 0, |g| g.check_waits_at(checks, false));
                        g.report_update(
                            service,
                            checks,
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
                    let was = g.saved_level(levels, position(tracked, index));
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
                g.check_waits_at(checks, false);
            }
        }
        if let Some(service) = updates {
            g.report_update(
                service,
                checks,
                &self.events,
                finish_exit,
                AfterWrites::Settled,
            );
        }
        g.b.seal_block(SIRTerminator::Jump(entry_done, Vec::new()));
    }
}
