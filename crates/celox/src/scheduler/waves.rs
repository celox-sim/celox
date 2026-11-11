//! The domain waves of a slot with derived clocks, and the edges of counted
//! and gated events.
use super::*;

impl Plan {
    /// The domains of a slot of a design with derived clocks, as the
    /// reference loop's waves run them: combinational logic settles; the
    /// domains of the events with an active edge evaluate together with
    /// those of the clocks computed combinationally from a changed source
    /// whose level moved to their active edge, judged now, whose levels
    /// become their baseline; without an active edge, every tracked event
    /// whose level moved to its active edge since the slot started. After
    /// each wave commits and logic settles, the domains not run yet whose
    /// clocks moved to their active edge run in another, until none does.
    /// The domains run in the slot collect in `exit_arg`.
    /// Active-region domain waves, with NBA publication deferred until
    /// blocking-result edges have quiesced. The upper half of `exit_arg`
    /// holds the deferred-domain mask across the shared settle continuation.
    pub(super) fn batch_active(
        &self,
        g: &mut Generator<'_>,
        tracked: &[usize],
        slot_end: celox_sir::BlockId,
        finish_exit: celox_sir::BlockId,
        checks: &[WaitCheck<'_>],
        pick: celox_sir::BlockId,
        wave: celox_sir::BlockId,
    ) {
        if let Some(ordered) = self.ordered {
            self.batch_ordered(
                g,
                ordered,
                tracked,
                slot_end,
                finish_exit,
                checks,
                pick,
                wave,
            );
            return;
        }
        let slots = g.slots;
        let empty = g.constant(0, self.event_bits);
        g.store_at(slots.exit_arg, self.event_bits, self.event_bits, empty);
        let flush = g.b.new_block();
        let mask = g.b.alloc_bit(self.event_bits, false);
        let fire = g.b.new_block_with(vec![mask]);
        let judge = g.b.new_block();
        // A pass of the process phase settles before the domains; a
        // deferred one runs now.
        match &self.lazy {
            Some(_) => g.settle_then(judge),
            None => {
                g.b.seal_block(SIRTerminator::Jump(judge, Vec::new()));
            }
        }

        g.b.switch_to_block(judge);
        let active = g.load(slots.exit_arg, self.event_bits);
        let levels = g.load_levels();
        let stable = self.stable_edges(g, tracked, levels);
        let counted = self.counted_edges(g);
        let active = g.binary(self.event_bits, active, BinaryOp::Or, counted);
        let stable = g.binary(self.event_bits, stable, BinaryOp::Or, counted);
        let zero = g.constant(0, self.event_bits);
        let edged = if self.gating.is_empty() {
            g.binary(1, active, BinaryOp::Ne, zero)
        } else {
            let byte = self.source_edged(g, counted);
            g.equals(byte, 1)
        };
        let stable_extra = g.b.alloc_bit(self.event_bits, false);
        g.b.emit(SIRInstruction::Mux(stable_extra, edged, zero, stable));
        let triggers = g.binary(self.event_bits, active, BinaryOp::Or, stable_extra);
        g.store(slots.exit_arg, self.event_bits, triggers);
        let quiet = g.binary(1, triggers, BinaryOp::Eq, zero);
        let first = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: quiet,
            true_block: (flush, Vec::new()),
            false_block: (first, Vec::new()),
        });
        g.b.switch_to_block(first);
        g.b.seal_block(SIRTerminator::Jump(fire, vec![triggers]));

        // A wave after a commit: the domains not run yet whose clocks moved.
        g.b.switch_to_block(wave);
        let pending = g.load_at(slots.exit_arg, self.event_bits, self.event_bits);
        let zero = g.constant(0, self.event_bits);
        let post = g.binary(1, pending, BinaryOp::Eq, zero);
        g.if_bit(post, 0, |g| {
            self.observe_slot(g, tracked, checks, finish_exit, false)
        });
        if self
            .kernels
            .iter()
            .flatten()
            .filter_map(|kernel| kernel.wait.as_ref())
            .any(|wait| wait.reactive)
        {
            // A later derived domain can restore an expression's old value.
            // Match the reference round: remember the occurrence at this
            // settle and drain its ready processes after the domain waves.
            g.check_waits(checks);
            let ready = g.load(slots.ready, g.ready_bits);
            let pending = g.load_at(slots.exit_arg, self.event_bits, self.event_bits);
            let zero = g.constant(0, self.event_bits);
            let before_nba = g.binary(1, pending, BinaryOp::Ne, zero);
            let zero = g.constant(0, g.ready_bits);
            let ready = g.binary(1, ready, BinaryOp::Ne, zero);
            let resume = g.binary(1, before_nba, BinaryOp::And, ready);
            let drain = g.b.new_block();
            let next = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: resume,
                true_block: (drain, Vec::new()),
                false_block: (next, Vec::new()),
            });
            g.b.switch_to_block(drain);
            let flag = g.constant(1, SCHEDULER_FLAG_WIDTH);
            g.store_at(
                slots.defer_limit_settle,
                celox_design::SCHEDULER_ACTIVE_RESUME_BIT,
                SCHEDULER_FLAG_WIDTH,
                flag,
            );
            g.b.seal_block(SIRTerminator::Jump(pick, Vec::new()));
            g.b.switch_to_block(next);
        }

        let fired = g.load(slots.exit_arg, self.event_bits);
        let levels = g.load_levels();
        let stable = self.stable_edges(g, tracked, levels);
        let counted = self.counted_edges(g);
        let stable = g.binary(self.event_bits, stable, BinaryOp::Or, counted);
        let all = g.ones(self.event_bits);
        let unfired = g.binary(self.event_bits, fired, BinaryOp::Xor, all);
        let ready = g.binary(self.event_bits, stable, BinaryOp::And, unfired);
        let zero = g.constant(0, self.event_bits);
        let quiet = g.binary(1, ready, BinaryOp::Eq, zero);
        let drained = g.b.new_block();
        let next = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: quiet,
            true_block: (drained, Vec::new()),
            false_block: (next, Vec::new()),
        });
        g.b.switch_to_block(next);
        let fired = g.binary(self.event_bits, fired, BinaryOp::Or, ready);
        g.store(slots.exit_arg, self.event_bits, fired);
        g.b.seal_block(SIRTerminator::Jump(fire, vec![ready]));

        g.b.switch_to_block(drained);
        let pending = g.load_at(slots.exit_arg, self.event_bits, self.event_bits);
        let zero = g.constant(0, self.event_bits);
        let empty = g.binary(1, pending, BinaryOp::Eq, zero);
        g.b.seal_block(SIRTerminator::Branch {
            cond: empty,
            true_block: (slot_end, Vec::new()),
            false_block: (flush, Vec::new()),
        });

        // Evaluation commits blocking results immediately, while every NBA
        // stays in working state. Their Active-region edges settle and run
        // before this batch publishes any deferred domain (LRM 4.5).
        g.b.switch_to_block(fire);
        for &index in tracked {
            g.if_bit(mask, index, |g| {
                g.inline_writers(
                    self.events[index]
                        .eval_only
                        .as_ref()
                        .expect("split active domain"),
                    finish_exit,
                );
            });
        }
        let pending = g.load_at(slots.exit_arg, self.event_bits, self.event_bits);
        let pending = g.binary(self.event_bits, pending, BinaryOp::Or, mask);
        g.store_at(slots.exit_arg, self.event_bits, self.event_bits, pending);
        let mut active = g.constant(0, self.event_bits);
        for &index in &self.active {
            let bit = g.single(index, self.event_bits);
            active = g.binary(self.event_bits, active, BinaryOp::Or, bit);
        }
        let active = g.binary(self.event_bits, mask, BinaryOp::And, active);
        let zero = g.constant(0, self.event_bits);
        let committed = g.binary(1, active, BinaryOp::Ne, zero);
        let settle = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: committed,
            true_block: (settle, Vec::new()),
            false_block: (flush, Vec::new()),
        });
        g.b.switch_to_block(settle);
        g.settle_then(wave);

        // No Active work remains: publish the deferred NBA results, then
        // judge the edges that those updates cause in another wave.
        g.b.switch_to_block(flush);
        let flag = g.constant(0, SCHEDULER_FLAG_WIDTH);
        g.store_at(
            slots.defer_limit_settle,
            celox_design::SCHEDULER_ACTIVE_RESUME_BIT,
            SCHEDULER_FLAG_WIDTH,
            flag,
        );
        let pending = g.load_at(slots.exit_arg, self.event_bits, self.event_bits);
        for &index in tracked {
            g.if_bit(pending, index, |g| {
                g.inline_writers(
                    self.events[index]
                        .apply
                        .as_ref()
                        .expect("split active domain"),
                    finish_exit,
                );
            });
        }
        let staged: Vec<&Staged> = self
            .kernels
            .iter()
            .flatten()
            .filter_map(|kernel| kernel.wait.as_ref())
            .flat_map(|wait| &wait.sites)
            .collect();
        g.publish(&staged);
        g.publish_service(
            self.service_function
                .filter(|_| !self.nba_processes.is_empty()),
            checks,
        );
        let empty = g.constant(0, self.event_bits);
        g.store_at(slots.exit_arg, self.event_bits, self.event_bits, empty);
        g.settle_then(wave);
    }

    pub(super) fn batch(
        &self,
        g: &mut Generator<'_>,
        tracked: &[usize],
        slot_end: celox_sir::BlockId,
        finish_exit: celox_sir::BlockId,
        checks: &[WaitCheck<'_>],
    ) {
        let slots = g.slots;
        let wave = g.b.new_block();
        let mask = g.b.alloc_bit(self.event_bits, false);
        let fire = g.b.new_block_with(vec![mask]);
        let judge = g.b.new_block();
        // A pass of the process phase settles before the domains; a
        // deferred one runs now.
        match &self.lazy {
            Some(_) => g.settle_then(judge),
            None => {
                g.b.seal_block(SIRTerminator::Jump(judge, Vec::new()));
            }
        }

        g.b.switch_to_block(judge);
        let active = g.load(slots.exit_arg, self.event_bits);
        let levels = g.load_levels();
        let stable = self.stable_edges(g, tracked, levels);
        let counted = self.counted_edges(g);
        let active = g.binary(self.event_bits, active, BinaryOp::Or, counted);
        let stable = g.binary(self.event_bits, stable, BinaryOp::Or, counted);
        let zero = g.constant(0, self.event_bits);
        let edged = if self.gating.is_empty() {
            g.binary(1, active, BinaryOp::Ne, zero)
        } else {
            let byte = self.source_edged(g, counted);
            g.equals(byte, 1)
        };
        let stable_extra = g.b.alloc_bit(self.event_bits, false);
        g.b.emit(SIRInstruction::Mux(stable_extra, edged, zero, stable));
        let triggers = g.binary(self.event_bits, active, BinaryOp::Or, stable_extra);
        g.store(slots.exit_arg, self.event_bits, triggers);
        let quiet = g.binary(1, triggers, BinaryOp::Eq, zero);
        let first = g.b.new_block();
        let flush_only = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: quiet,
            true_block: (flush_only, Vec::new()),
            false_block: (first, Vec::new()),
        });
        g.b.switch_to_block(flush_only);
        let staged: Vec<&Staged> = self
            .kernels
            .iter()
            .flatten()
            .filter_map(|kernel| kernel.wait.as_ref())
            .flat_map(|wait| &wait.sites)
            .collect();
        g.publish(&staged);
        g.publish_service(
            self.service_function
                .filter(|_| !self.nba_processes.is_empty()),
            checks,
        );
        g.settle_then(wave);
        g.b.switch_to_block(first);
        g.b.seal_block(SIRTerminator::Jump(fire, vec![triggers]));

        // A wave after a commit: the domains not run yet whose clocks moved.
        g.b.switch_to_block(wave);
        self.observe_slot(g, tracked, checks, finish_exit, false);
        if self
            .kernels
            .iter()
            .flatten()
            .filter_map(|kernel| kernel.wait.as_ref())
            .any(|wait| wait.reactive)
        {
            // A later derived domain can restore an expression's old value.
            // Match the reference round: remember the occurrence at this
            // settle and drain its ready processes after the domain waves.
            g.check_waits(checks);
        }

        let fired = g.load(slots.exit_arg, self.event_bits);
        let levels = g.load_levels();
        let stable = self.stable_edges(g, tracked, levels);
        let counted = self.counted_edges(g);
        let stable = g.binary(self.event_bits, stable, BinaryOp::Or, counted);
        let all = g.ones(self.event_bits);
        let unfired = g.binary(self.event_bits, fired, BinaryOp::Xor, all);
        let ready = g.binary(self.event_bits, stable, BinaryOp::And, unfired);
        let zero = g.constant(0, self.event_bits);
        let quiet = g.binary(1, ready, BinaryOp::Eq, zero);
        let next = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: quiet,
            true_block: (slot_end, Vec::new()),
            false_block: (next, Vec::new()),
        });
        g.b.switch_to_block(next);
        let fired = g.binary(self.event_bits, fired, BinaryOp::Or, ready);
        g.store(slots.exit_arg, self.event_bits, fired);
        g.b.seal_block(SIRTerminator::Jump(fire, vec![ready]));

        // A wave: one domain evaluates and commits at once, several
        // evaluate, then commit, in event order; then logic settles.
        g.b.switch_to_block(fire);
        let zero = g.constant(0, self.event_bits);
        let one = g.constant(1, self.event_bits);
        let lower = g.binary(self.event_bits, mask, BinaryOp::Sub, one);
        let others = g.binary(self.event_bits, mask, BinaryOp::And, lower);
        let single = g.binary(1, others, BinaryOp::Eq, zero);
        let alone = g.b.new_block();
        let together = g.b.new_block();
        let committed = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: single,
            true_block: (alone, Vec::new()),
            false_block: (together, Vec::new()),
        });
        g.b.switch_to_block(alone);
        for &index in tracked {
            g.if_bit(mask, index, |g| {
                let event = &self.events[index];
                if event
                    .eval_apply
                    .iter()
                    .any(|unit| fails(unit) || emits_finish(unit, &self.finish_sites))
                {
                    // The reference stops after an evaluation unit that
                    // ends the run, before publishing its deferred NBAs.
                    // A combined unit would publish them before `inline`
                    // can inspect the finish flag.
                    for units in [event.eval_only.as_ref(), event.apply.as_ref()] {
                        g.inline_writers(
                            units.expect("derived domains have separate units"),
                            finish_exit,
                        );
                    }
                } else {
                    g.inline_writers(&event.eval_apply, finish_exit);
                }
            });
        }
        g.b.seal_block(SIRTerminator::Jump(committed, Vec::new()));
        g.b.switch_to_block(together);
        if tracked.len() > 1 {
            for phase in [Phase::Evaluate, Phase::Apply] {
                for &index in tracked {
                    let event = &self.events[index];
                    let units = match phase {
                        Phase::Evaluate => event.eval_only.as_ref(),
                        Phase::Apply => event.apply.as_ref(),
                    }
                    .expect("tracked events have their units (`Plan::new`)");
                    g.if_bit(mask, index, |g| {
                        g.inline_writers(units, finish_exit);
                    });
                }
            }
        }
        g.b.seal_block(SIRTerminator::Jump(committed, Vec::new()));
        g.b.switch_to_block(committed);
        g.publish(&staged);
        g.publish_service(
            self.service_function
                .filter(|_| !self.nba_processes.is_empty()),
            checks,
        );
        g.settle_then(wave);
    }

    /// Consume counted occurrences at the same settle boundaries as the
    /// reference work list. A pulse remains an event after its level returns.
    pub(super) fn counted_edges(&self, g: &mut Generator<'_>) -> RegisterId {
        let mut edges = g.constant(0, self.event_bits);
        for counted in &self.counted {
            let seen = g.load_at(g.slots.levels, counted.baseline_offset, 32);
            let current = g.load(counted.counter, 32);
            let changed = g.binary(1, seen, BinaryOp::Ne, current);
            g.store_at(g.slots.levels, counted.baseline_offset, 32, current);
            for &event in &counted.events {
                let bit = g.event_bit(changed, event);
                edges = g.binary(self.event_bits, edges, BinaryOp::Or, bit);
            }
        }
        edges
    }

    /// Whether source/counting events, rather than only pre-judged gate
    /// edges, require preserving the source-before-derived commit order.
    pub(super) fn source_edged(&self, g: &mut Generator<'_>, counted: RegisterId) -> RegisterId {
        let flag = g.load_at(
            g.slots.defer_limit_settle,
            GATE_SOURCE_EDGED_BIT,
            SCHEDULER_FLAG_WIDTH,
        );
        let prior = g.equals(flag, 1);
        let zero = g.constant(0, self.event_bits);
        let counted = g.binary(1, counted, BinaryOp::Ne, zero);
        let edged = g.binary(1, prior, BinaryOp::Or, counted);
        let byte = g.b.alloc_bit(SCHEDULER_FLAG_WIDTH, false);
        let one = g.constant(1, SCHEDULER_FLAG_WIDTH);
        let zero = g.constant(0, SCHEDULER_FLAG_WIDTH);
        g.b.emit(SIRInstruction::Mux(byte, edged, one, zero));
        byte
    }

    /// Reference `judge_derived_edges`: capture gate edges at the first
    /// settle after clock sources run, before newly woken processes run.
    /// Only positive-delay kernels and host clocks drive event signals;
    /// each runs once before this first settle. Later domain waves already
    /// track transitions from the baselines recorded here.
    pub(super) fn judge_gates(&self, g: &mut Generator<'_>, tracked: &[usize]) {
        if self.gating.is_empty() {
            return;
        }
        let slots = g.slots;
        let judged = g.load_at(
            slots.defer_limit_settle,
            GATES_JUDGED_BIT,
            SCHEDULER_FLAG_WIDTH,
        );
        let first = g.b.new_block();
        let done = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: judged,
            true_block: (done, Vec::new()),
            false_block: (first, Vec::new()),
        });
        g.b.switch_to_block(first);
        let mut active = g.load(slots.exit_arg, self.event_bits);
        let original = self.source_edged(g, active);
        g.store_at(
            slots.defer_limit_settle,
            GATE_SOURCE_EDGED_BIT,
            SCHEDULER_FLAG_WIDTH,
            original,
        );
        let levels = g.load_levels();
        let (active, updated) = match self.judge_gates_batched(g, tracked, levels, active) {
            Some(judged) => judged,
            None => {
                let mut updated = levels;
                for (source, gated) in &self.gating {
                    let was = g.saved_level(levels, position(tracked, *source));
                    let is = g.level(&self.events[*source]);
                    let changed = g.binary(1, was, BinaryOp::Ne, is);
                    for &clock in gated {
                        let bit = position(tracked, clock);
                        let was = g.saved_level(levels, bit);
                        let is = g.level(&self.events[clock]);
                        let edge = g.settled_edge(&self.events[clock], was, is);
                        let edge = g.binary(1, edge, BinaryOp::And, changed);
                        let edge = g.event_bit(edge, clock);
                        active = g.binary(self.event_bits, active, BinaryOp::Or, edge);
                        let level = g.b.alloc_bit(g.level_width(), false);
                        let prior = g.saved_level(updated, bit);
                        g.b.emit(SIRInstruction::Mux(level, changed, is, prior));
                        updated = g.replace_level(updated, level, bit);
                    }
                }
                (active, updated)
            }
        };
        g.store_levels(updated);
        g.store(slots.exit_arg, self.event_bits, active);
        let one = g.constant(1, SCHEDULER_FLAG_WIDTH);
        g.store_at(
            slots.defer_limit_settle,
            GATES_JUDGED_BIT,
            SCHEDULER_FLAG_WIDTH,
            one,
        );
        g.b.seal_block(SIRTerminator::Jump(done, Vec::new()));
        g.b.switch_to_block(done);
    }

    /// [`Self::judge_gates`] as bit planes over the tracked positions, in a
    /// two-state design whose event word is one machine word: the gated
    /// clocks a changed source judges form a mask, their current levels one
    /// word, and the edge formulas run once per word instead of once per
    /// clock. Returns the active edges and the updated levels, or `None`
    /// where the per-clock form is needed.
    fn judge_gates_batched(
        &self,
        g: &mut Generator<'_>,
        tracked: &[usize],
        levels: Levels,
        active: RegisterId,
    ) -> Option<(RegisterId, Levels)> {
        let width = self.event_bits;
        if g.four_state.is_some() || width > 64 || tracked.len() > width {
            return None;
        }
        let zero = g.constant(0, width);
        // The positions whose clocks a changed source judges.
        let mut judged = zero;
        for (source, gated) in &self.gating {
            let was = g.saved_level(levels, position(tracked, *source));
            let is = g.level(&self.events[*source]);
            let changed = g.binary(1, was, BinaryOp::Ne, is);
            let mask = gated.iter().fold(BigUint::default(), |mask, &clock| {
                mask | (BigUint::from(1u8) << position(tracked, clock))
            });
            let mask = g.wide_constant(mask, width);
            let selected = g.b.alloc_bit(width, false);
            g.b.emit(SIRInstruction::Mux(selected, changed, mask, zero));
            judged = g.binary(width, judged, BinaryOp::Or, selected);
        }
        // The current levels of the gated clocks, at their positions, and
        // the positions of each edge kind.
        let mut clocks: Vec<usize> = self
            .gating
            .iter()
            .flat_map(|(_, gated)| gated.iter().copied())
            .collect();
        clocks.sort_unstable();
        clocks.dedup();
        let mut is = zero;
        let (mut rising, mut falling, mut toggling) =
            (BigUint::default(), BigUint::default(), BigUint::default());
        for &clock in &clocks {
            let bit = position(tracked, clock);
            let level = g.level(&self.events[clock]);
            let set = g.bit_of(level, bit, width);
            is = g.binary(width, is, BinaryOp::Or, set);
            let event = &self.events[clock];
            let one = BigUint::from(1u8) << bit;
            if event.any_settled {
                toggling |= one;
            } else if event.active != 0 {
                rising |= one;
            } else {
                falling |= one;
            }
        }
        let was = levels.values;
        let all = g.ones(width);
        let not_was = g.binary(width, was, BinaryOp::Xor, all);
        let not_is = g.binary(width, is, BinaryOp::Xor, all);
        let rises = g.binary(width, not_was, BinaryOp::And, is);
        let falls = g.binary(width, was, BinaryOp::And, not_is);
        let toggles = g.binary(width, was, BinaryOp::Xor, is);
        let mut edges = zero;
        for (kind_edges, lanes) in [(rises, rising), (falls, falling), (toggles, toggling)] {
            if lanes == BigUint::default() {
                continue;
            }
            let lanes = g.wide_constant(lanes, width);
            let masked = g.binary(width, kind_edges, BinaryOp::And, lanes);
            edges = g.binary(width, edges, BinaryOp::Or, masked);
        }
        let edges = g.binary(width, edges, BinaryOp::And, judged);
        // From tracked positions to event indices.
        let edges = if clocks
            .iter()
            .all(|&clock| position(tracked, clock) == clock)
        {
            edges
        } else {
            let mut by_index = zero;
            for &clock in &clocks {
                let lane = g.mask_bit(edges, position(tracked, clock));
                let bit = g.event_bit(lane, clock);
                by_index = g.binary(width, by_index, BinaryOp::Or, bit);
            }
            by_index
        };
        let active = g.binary(width, active, BinaryOp::Or, edges);
        let kept = g.binary(width, judged, BinaryOp::Xor, all);
        let kept = g.binary(width, was, BinaryOp::And, kept);
        let moved = g.binary(width, is, BinaryOp::And, judged);
        let values = g.binary(width, kept, BinaryOp::Or, moved);
        Some((
            active,
            Levels {
                values,
                unknown: None,
            },
        ))
    }

    /// The tracked events whose levels moved to their active edge against
    /// `levels`, their baselines, as bits by event index.
    pub(super) fn stable_edges(
        &self,
        g: &mut Generator<'_>,
        tracked: &[usize],
        levels: Levels,
    ) -> RegisterId {
        if tracked.len() >= 4 {
            return self.batched_stable_edges(g, tracked, levels);
        }
        let mut edges = g.constant(0, self.event_bits);
        for (position, &index) in tracked.iter().enumerate() {
            let was = g.saved_level(levels, position);
            let is = g.level(&self.events[index]);
            let edge = g.settled_edge(&self.events[index], was, is);
            let bit = g.event_bit(edge, index);
            edges = g.binary(self.event_bits, edges, BinaryOp::Or, bit);
        }
        edges
    }

    /// Judge independent edges as bit planes, in chunks of at most one word.
    /// Baselines use tracked positions; the result uses event indices, which
    /// need not be contiguous. No level or event is published by this check.
    fn batched_stable_edges(
        &self,
        g: &mut Generator<'_>,
        tracked: &[usize],
        levels: Levels,
    ) -> RegisterId {
        let zero = g.constant(0, 1);
        let mut result = vec![zero; self.event_bits];
        for (chunk, events) in tracked.chunks(64).enumerate() {
            let width = events.len();
            let mut values = Vec::with_capacity(width);
            let mut unknowns = Vec::with_capacity(width);
            let mut falling = 0u64;
            let mut any_change = 0u64;
            for (bit, &index) in events.iter().enumerate() {
                let event = &self.events[index];
                let level = g.level(event);
                let (value, unknown) = g.level_planes(level);
                values.push(value);
                if let Some(unknown) = unknown {
                    unknowns.push(unknown);
                }
                if event.active == 0 {
                    falling |= 1u64 << bit;
                }
                if event.any_settled {
                    any_change |= 1u64 << bit;
                }
            }
            let pack = |g: &mut Generator<'_>, mut bits: Vec<RegisterId>| {
                bits.reverse();
                let result = g.b.alloc_bit(width, false);
                g.b.emit(SIRInstruction::Concat(result, bits));
                result
            };
            let slice = |g: &mut Generator<'_>, source: RegisterId| {
                let result = g.b.alloc_bit(width, false);
                g.b.emit(SIRInstruction::Slice(result, source, chunk * 64, width));
                result
            };
            let was = slice(g, levels.values);
            let is = pack(g, values);
            // Invert both value planes of falling-edge lanes. Their masks
            // stay unchanged, turning the same edge formula into negedge.
            let falling = g.constant(falling, width);
            let was = g.binary(width, was, BinaryOp::Xor, falling);
            let is = g.binary(width, is, BinaryOp::Xor, falling);
            let all = g.ones(width);
            let not_was = g.binary(width, was, BinaryOp::Xor, all);
            let mut changed = (any_change != 0).then(|| g.binary(width, was, BinaryOp::Xor, is));
            let edges = if let Some(old_unknown) = levels.unknown {
                let old_unknown = slice(g, old_unknown);
                let new_unknown = pack(g, unknowns);
                if let Some(values) = changed {
                    let masks = g.binary(width, old_unknown, BinaryOp::Xor, new_unknown);
                    changed = Some(g.binary(width, values, BinaryOp::Or, masks));
                }
                // Table 9-2: leave known zero or reach known one, excluding
                // all unknown-to-unknown transitions (including X <-> Z).
                let leaves = g.binary(width, not_was, BinaryOp::Or, old_unknown);
                let reaches = g.binary(width, is, BinaryOp::Or, new_unknown);
                let both_unknown = g.binary(width, old_unknown, BinaryOp::And, new_unknown);
                let some_known = g.binary(width, both_unknown, BinaryOp::Xor, all);
                let edges = g.binary(width, leaves, BinaryOp::And, reaches);
                g.binary(width, edges, BinaryOp::And, some_known)
            } else {
                g.binary(width, not_was, BinaryOp::And, is)
            };
            let edges = if let Some(changed) = changed {
                let any = g.constant(any_change, width);
                let ordinary = g.binary(width, any, BinaryOp::Xor, all);
                let edges = g.binary(width, edges, BinaryOp::And, ordinary);
                let changed = g.binary(width, changed, BinaryOp::And, any);
                g.binary(width, edges, BinaryOp::Or, changed)
            } else {
                edges
            };
            for (bit, &index) in events.iter().enumerate() {
                let lane = g.b.alloc_bit(1, false);
                g.b.emit(SIRInstruction::Slice(lane, edges, bit, 1));
                result[index] = lane;
            }
        }
        result.reverse();
        let edges = g.b.alloc_bit(self.event_bits, false);
        g.b.emit(SIRInstruction::Concat(edges, result));
        edges
    }
}
