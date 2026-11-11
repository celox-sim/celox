//! After the process phase of a slot: the settle of a pass, the domains
//! without waves, and the end of the slot.
use super::*;

impl Plan {
    /// The end of a pass of the process phase: combinational logic settles,
    /// the waits see the pass, and the kernels they wake run in another.
    /// Returns the block the slot ends at, the block its domains run in, and
    /// the block Active-region waves resume at, when the plan has any.
    pub(super) fn settle_pass(
        &self,
        g: &mut Generator<'_>,
        cx: &Slot<'_>,
        guards: &mut Option<ordered::Guards>,
        time: RegisterId,
        pass_end: celox_sir::BlockId,
        host_entry: Option<RegisterId>,
        pick: celox_sir::BlockId,
        internal_exit: celox_sir::BlockId,
    ) -> (
        celox_sir::BlockId,
        celox_sir::BlockId,
        Option<celox_sir::BlockId>,
    ) {
        let &Slot {
            slots,
            schema,
            edged,
            tracked,
            waits,
            ordinals,
            checks,
            deferred,
            ..
        } = cx;
        // The settles after a pass and after the domains share one copy of
        // combinational logic. A deferred settle after a pass runs for the
        // kernels that may run in another.
        g.b.switch_to_block(pass_end);
        if host_entry.is_some() {
            let pending = host_inputs::pending(g);
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
                for &index in edged {
                    let was = g.saved_level(levels, position(tracked, index));
                    let is = g.level(&self.events[index]);
                    let edge = g.settled_edge(&self.events[index], was, is);
                    let bit = g.event_bit(edge, index);
                    edges = g.binary(self.event_bits, edges, BinaryOp::Or, bit);
                }
                g.store(slots.exit_arg, self.event_bits, edges);
            });
        }
        if self.cyclic {
            if let Some(guards) = guards {
                guards.charge(
                    g,
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
            let counted = self.counted_edges(g);
            if !self.gating.is_empty() {
                let edged = self.source_edged(g, counted);
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
        self.judge_gates(g, tracked);
        g.check_waits(checks);
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
            let ready = g.status_ready(deferred, ordinals, schema, ProcessStatus::DeferActive);
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
        (slot_end, domains, resume_domains)
    }

    /// The domains of a slot without derived clocks, counted events or
    /// Active-region commits: the domain of one edge evaluates and applies at
    /// once, those of several evaluate, then apply; the nonblocking values
    /// publish and combinational logic settles.
    pub(super) fn simple_domains(
        &self,
        g: &mut Generator<'_>,
        cx: &Slot<'_>,
        updates: Option<u32>,
        slot_end: celox_sir::BlockId,
        pick: celox_sir::BlockId,
        finish_exit: celox_sir::BlockId,
        internal_exit: celox_sir::BlockId,
    ) {
        let &Slot {
            slots,
            edged,
            tracked,
            waits,
            checks,
            ..
        } = cx;
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
                checks,
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
                checks,
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
                    for &index in edged {
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
            checks,
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

    /// Re-arm the host clocks whose edge applied in the slot.
    pub(super) fn rearm_clocks(
        &self,
        g: &mut Generator<'_>,
        cx: &Slot<'_>,
        slot_end: celox_sir::BlockId,
        started: Option<(RegisterId, RegisterId)>,
        time: RegisterId,
    ) {
        let &Slot {
            slots, clock_bits, ..
        } = cx;
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
    }

    /// The kernels the slot's nonblocking updates or a host callback woke run
    /// in another pass of the same slot.
    pub(super) fn drain_woken(
        &self,
        g: &mut Generator<'_>,
        cx: &Slot<'_>,
        guards: &Option<ordered::Guards>,
        time: RegisterId,
        pick: celox_sir::BlockId,
        internal_exit: celox_sir::BlockId,
    ) {
        let &Slot {
            slots,
            tracked,
            waits,
            checks,
            ..
        } = cx;
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
            g.check_waits(checks);
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
    }

    /// The Observed and Reactive regions of a slot with assertions.
    pub(super) fn assertion_regions(
        &self,
        g: &mut Generator<'_>,
        cx: &Slot<'_>,
        pick: celox_sir::BlockId,
    ) {
        let &Slot {
            slots,
            schema,
            tracked,
            ordinals,
            observed,
            reactive,
            assertions,
            observed_entry,
            reactive_entry,
            slot_done,
            ..
        } = cx;
        if assertions {
            // IEEE 1800-2023 4.4.2.5-6: drain all Observed evaluations,
            // then their Reactive actions, before returning to Active work.
            // Predicates may read live combinational disable conditions.
            g.settle_then(observed_entry.unwrap());
            for (entry, kernels, status, phase, empty) in [
                (
                    observed_entry.unwrap(),
                    observed,
                    ProcessStatus::AssertionObserved,
                    1,
                    reactive_entry.unwrap(),
                ),
                (
                    reactive_entry.unwrap(),
                    reactive,
                    ProcessStatus::AssertionReactive,
                    2,
                    slot_done.unwrap(),
                ),
            ] {
                g.b.switch_to_block(entry);
                let none = g.constant(u64::MAX, SCHEDULER_TIME_WIDTH);
                g.store(slots.current, SCHEDULER_TIME_WIDTH, none);
                let ready = g.status_ready(kernels, ordinals, schema, status);
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
    }
}
