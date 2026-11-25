//! The shared settle of combinational logic, its deferred stateful part, and
//! the continuations they return to.
use super::*;

impl Generator<'_> {
    /// Whether combinational logic reads `address`, when settles may be
    /// skipped.
    pub(super) fn settle_reads(&self, address: AbsoluteAddr) -> bool {
        let storage = self.shared.get(&address).copied().unwrap_or(address);
        self.settle_reads
            .is_some_and(|reads| reads.contains(&storage))
    }

    /// A generated NBA store already marks its partition readers, including
    /// the dirty cause consumed by the clock cone. An additional coarse mark
    /// would overwrite that cause. Keep it for inputs outside that metadata.
    pub(super) fn partition_tracks_publication(&self, target: AbsoluteAddr) -> bool {
        if !self
            .incremental
            .is_some_and(|plan| plan.untracked.is_some())
        {
            return false;
        }
        let layout = self.program.layout();
        let Some((settle, offset)) = layout.settle.as_ref().zip(layout.offsets.get(&target)) else {
            return false;
        };
        !self.settle_reads(target)
            || settle
                .objects
                .binary_search_by_key(offset, |object| object.start)
                .is_ok_and(|index| !settle.objects[index].partitions.is_empty())
    }

    /// Record that state combinational logic reads may have changed.
    pub(super) fn mark_dirty(&mut self) {
        if self.settle_reads.is_some() {
            let one = self.constant(1, SCHEDULER_FLAG_WIDTH);
            self.store(self.slots.dirty, SCHEDULER_FLAG_WIDTH, one);
        }
    }

    /// Preserve all pending publications until the next settle. Even a target
    /// without downstream units can require retained logic to run.
    pub(super) fn mark_publication(&mut self, index: Option<usize>) {
        if let Some(index) = index {
            let schedule = self
                .incremental
                .and_then(|plan| plan.events.as_ref())
                .unwrap();
            if let Some(readers) = &schedule.event_readers {
                let mask = readers[index];
                if mask != 0 {
                    let offset = schedule.publication_offset;
                    let pending = self.load_at(self.slots.settle_events, offset, 64);
                    let bits = self.constant(mask, 64);
                    let pending = self.binary(64, pending, BinaryOp::Or, bits);
                    self.store_at(self.slots.settle_events, offset, 64, pending);
                }
            } else {
                let one = self.constant(1, 1);
                self.store_at(self.slots.settle_events, index, 1, one);
            }
        }
        let pending = self.load(self.slots.dirty, SCHEDULER_FLAG_WIDTH);
        let bit = self.constant(PUBLISHED, SCHEDULER_FLAG_WIDTH);
        let pending = self.binary(SCHEDULER_FLAG_WIDTH, pending, BinaryOp::Or, bit);
        self.store(self.slots.dirty, SCHEDULER_FLAG_WIDTH, pending);
    }

    /// Run the stateful part of a deferred settle (`Lazy::eager`) where the
    /// reference settles, then continue at `target`, as
    /// [`Self::settle_then`]: one copy serves every caller.
    pub(super) fn eager_then(
        &mut self,
        reason: &'static str,
        eager: &[Unit],
        target: celox_sir::BlockId,
    ) {
        if eager.is_empty() {
            self.b.seal_block(SIRTerminator::Jump(target, Vec::new()));
            return;
        }
        self.count(ActivationKind::EagerSettle, "requests", |_| reason.into());
        let case = match self
            .eager_continuations
            .iter()
            .position(|&block| block == target)
        {
            Some(index) => index,
            None => {
                self.eager_continuations.push(target);
                self.eager_continuations.len() - 1
            }
        };
        let value = self.constant(case as u64 + 1, self.cont_bits);
        self.store(self.slots.cont, self.cont_bits, value);
        let block = *self.eager.get_or_insert_with(|| self.b.new_block());
        self.b.seal_block(SIRTerminator::Jump(block, Vec::new()));
    }

    /// Emit the shared stateful part of the settle, when a caller uses it:
    /// skipped when the settle's inputs did not change since the last one.
    pub(super) fn emit_eager(
        &mut self,
        eager: &[Unit],
        finish_exit: celox_sir::BlockId,
        internal_exit: celox_sir::BlockId,
    ) {
        let Some(block) = self.eager else {
            return;
        };
        self.b.switch_to_block(block);
        let dirty = self.load(self.slots.dirty, SCHEDULER_FLAG_WIDTH);
        let zero = self.constant(0, SCHEDULER_FLAG_WIDTH);
        let changed = self.binary(1, dirty, BinaryOp::Ne, zero);
        self.count_if(ActivationKind::EagerSettle, "dirty", changed, |_| {
            String::new()
        });
        let run = self.b.new_block();
        let done = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: changed,
            true_block: (run, Vec::new()),
            false_block: (done, Vec::new()),
        });
        self.b.switch_to_block(run);
        let previous = std::mem::replace(&mut self.role, "eager settle".into());
        for unit in eager {
            self.inline_with_exit(unit, None);
        }
        self.role = previous;
        if eager.iter().any(|unit| self.unit_finishes(unit)) {
            self.check_finish(finish_exit);
        }
        self.b.seal_block(SIRTerminator::Jump(done, Vec::new()));
        self.b.switch_to_block(done);
        let targets = self.eager_continuations.clone();
        self.dispatch_continuations(&targets, internal_exit);
    }

    /// Run the shared settle, then continue at `target`. The registers
    /// `target` and its successors use must be defined in it or in the slot
    /// loop's header, which alone dominates every entry of the settle.
    ///
    /// `reason` names the request in an activation profile: what woke the
    /// settle.
    pub(super) fn settle_then(&mut self, reason: &'static str, target: celox_sir::BlockId) {
        self.count(ActivationKind::SettleCall, "requests", |_| reason.into());
        let case = match self.continuations.iter().position(|&block| block == target) {
            Some(index) => index,
            None => {
                self.continuations.push(target);
                self.continuations.len() - 1
            }
        };
        let value = self.constant(case as u64 + 1, self.cont_bits);
        self.store(self.slots.cont, self.cont_bits, value);
        self.b
            .seal_block(SIRTerminator::Jump(self.comb, Vec::new()));
    }

    /// Emit the shared settle: combinational logic, skipped when its inputs
    /// did not change since the last one, as it would write what it already
    /// holds; then the continuation of its caller.
    pub(super) fn emit_settle(
        &mut self,
        units: &[Unit],
        finish_exit: celox_sir::BlockId,
        internal_exit: celox_sir::BlockId,
        updates: Option<(u32, &[WaitCheck<'_>], &[Event])>,
    ) {
        self.b.switch_to_block(self.comb);
        self.count(ActivationKind::Settle, "entered", |_| String::new());
        let previous = std::mem::replace(&mut self.role, "settle".into());
        // The updates the settle follows, then its own.
        if let Some((service, checks, events)) = updates {
            self.report_update(service, checks, events, finish_exit, AfterWrites::Settle);
        }
        let observed = self.observers_before(internal_exit);
        if self.settle_reads.is_some() {
            let dirty = self.load(self.slots.dirty, SCHEDULER_FLAG_WIDTH);
            let zero = self.constant(0, SCHEDULER_FLAG_WIDTH);
            let changed = self.binary(1, dirty, BinaryOp::Ne, zero);
            // An observer whose reads changed records in this settle.
            let changed = match observed {
                Some(observed) => self.binary(1, changed, BinaryOp::Or, observed),
                None => changed,
            };
            if self.profiling() {
                self.count_if(ActivationKind::Settle, "dirty", changed, |_| String::new());
                // Why it runs: a change of anything it reads (1), of
                // clocks or time alone (2), of published values (4).
                for (bit, measure) in [
                    (0, "dirty: inputs"),
                    (1, "dirty: clock/time"),
                    (2, "dirty: published"),
                ] {
                    let set = self.mask_bit(dirty, bit);
                    self.count_if(ActivationKind::Settle, measure, set, |_| String::new());
                }
            }
            let settle = self.b.new_block();
            let settled = self.b.new_block();
            self.b.seal_block(SIRTerminator::Branch {
                cond: changed,
                true_block: (settle, Vec::new()),
                false_block: (settled, Vec::new()),
            });
            self.b.switch_to_block(settle);
            if let Some(schedule) = self.incremental.and_then(|plan| plan.events.as_ref()) {
                self.emit_event_schedule(schedule, units, dirty);
            } else if let Some(cone) = self.incremental {
                let partial = self.b.new_block();
                let full = self.b.new_block();
                let done = self.b.new_block();
                let mut only_clock = self.equals(dirty, 2);
                if let Some((flags, partitions)) = cone.untracked {
                    let untracked = self.load_at(
                        flags,
                        celox_design::settle_untracked_flag(partitions) * 8,
                        SCHEDULER_FLAG_WIDTH,
                    );
                    let tracked = self.equals(untracked, 0);
                    only_clock = self.binary(1, only_clock, BinaryOp::And, tracked);
                }
                self.b.seal_block(SIRTerminator::Branch {
                    cond: only_clock,
                    true_block: (partial, Vec::new()),
                    false_block: (full, Vec::new()),
                });
                self.b.switch_to_block(partial);
                self.count(ActivationKind::Settle, "partial cone", |_| String::new());
                for unit in &cone.units {
                    self.inline_with_exit(unit, None);
                }
                self.b.seal_block(SIRTerminator::Jump(done, Vec::new()));
                self.b.switch_to_block(full);
                let units = match (cone.untracked, self.program.layout().settle.as_ref()) {
                    (Some((flags, _)), Some(settle)) => crate::settle::full_settle(
                        units,
                        stable(flags),
                        settle,
                        &self.program.layout().offsets,
                    ),
                    _ => std::borrow::Cow::Borrowed(units),
                };
                for unit in units.iter() {
                    self.inline_with_exit(unit, None);
                }
                self.b.seal_block(SIRTerminator::Jump(done, Vec::new()));
                self.b.switch_to_block(done);
            } else {
                for unit in units {
                    self.inline_with_exit(unit, None);
                }
            }
            let clean = self.constant(0, SCHEDULER_FLAG_WIDTH);
            self.store(self.slots.dirty, SCHEDULER_FLAG_WIDTH, clean);
            self.b.seal_block(SIRTerminator::Jump(settled, Vec::new()));
            self.b.switch_to_block(settled);
        } else {
            for unit in units {
                self.inline_with_exit(unit, None);
            }
        }
        self.observers_after(internal_exit);
        // Match the backend's phase boundary: a captured finish must not
        // skip later combinational writes, captures or observer snapshots.
        if units.iter().any(|unit| self.unit_finishes(unit))
            || self
                .incremental
                .is_some_and(|cone| cone.units.iter().any(|unit| self.unit_finishes(unit)))
        {
            self.check_finish(finish_exit);
        }
        if let Some((service, checks, events)) = updates {
            let comb = self.comb;
            self.report_update(
                service,
                checks,
                events,
                finish_exit,
                AfterWrites::Again(comb),
            );
        }
        self.role = previous;
        let targets = self.continuations.clone();
        self.dispatch_continuations(&targets, internal_exit);
    }

    fn emit_event_schedule(&mut self, schedule: &EventSchedule, units: &[Unit], dirty: RegisterId) {
        let conditional = schedule.groups.iter().filter(|(a, _)| !a.every_settle);
        let mut guards = if schedule.event_readers.is_some() {
            // Producers already use the group's bit. Fold clock-only and full
            // invalidations into that same word before running any consumers.
            let offset = schedule.publication_offset;
            let zero = self.constant(0, 64);
            let pending = self.load_at(self.slots.settle_events, offset, 64);
            self.store_at(self.slots.settle_events, offset, 64, zero);
            let mut active = pending;
            let clock_mask = conditional
                .clone()
                .enumerate()
                .fold(0, |mask, (i, (a, _))| {
                    mask | (u64::from(a.clock_inputs) << i)
                });
            for (index, mask) in [(1, clock_mask), (0, u64::MAX)] {
                if mask != 0 {
                    let condition = self.mask_bit(dirty, index);
                    let bits = self.constant(mask, 64);
                    let set = if mask == u64::MAX {
                        bits
                    } else {
                        self.binary(64, active, BinaryOp::Or, bits)
                    };
                    let selected = self.b.alloc_bit(64, false);
                    self.b
                        .emit(SIRInstruction::Mux(selected, condition, set, active));
                    active = selected;
                }
            }
            (0..conditional.count()).map(|i| (active, i)).collect()
        } else {
            self.event_predicates(schedule, dirty)
        }
        .into_iter();
        let mut updated = None;
        for (group, (a, members)) in schedule.groups.iter().enumerate() {
            let run = |g: &mut Self| {
                let name = profile::group_name(
                    g.program,
                    group,
                    a,
                    members.iter().map(|&index| &units[index]),
                );
                g.count(ActivationKind::SettleGroup, "runs", |_| name.clone());
                let previous = std::mem::replace(&mut g.role, format!("settle {name}"));
                for &index in members {
                    g.inline_with_exit(&units[index], None);
                }
                g.role = previous;
            };
            if a.every_settle {
                run(self);
            } else {
                let (mask, bit) = guards.next().unwrap();
                let mask = updated.unwrap_or(mask);
                if let Some(outputs) = schedule.changed_outputs.get(&bit) {
                    updated = Some(self.run_publishing_group(mask, bit, outputs, run));
                } else {
                    self.if_bit(mask, bit, run);
                }
            }
        }
    }

    fn event_predicates(
        &mut self,
        schedule: &EventSchedule,
        dirty: RegisterId,
    ) -> Vec<(RegisterId, usize)> {
        // Masks can span many words in designs with procedural NBA sites.
        // Read only words used by a predicate, and share their masked tests.
        let mut words = HashMap::default();
        let mut tests = HashMap::default();
        let zero = self.constant(0, 64);
        let full = self.mask_bit(dirty, 0);
        let clock = self.mask_bit(dirty, 1);
        let full_or_clock = self.binary(1, full, BinaryOp::Or, clock);
        // Compute each distinct domain predicate once in a dominating block.
        let mut predicates = HashMap::default();
        for (a, _) in &schedule.groups {
            if !a.every_settle {
                predicates
                    .entry((&a.events, a.clock_inputs))
                    .or_insert_with(|| {
                        let base = if a.clock_inputs { full_or_clock } else { full };
                        if a.events == BigUint::ZERO {
                            return base;
                        }
                        let mut active = base;
                        for (word, mask) in a
                            .events
                            .iter_u64_digits()
                            .enumerate()
                            .filter(|(_, mask)| *mask != 0)
                        {
                            let pending = *words.entry(word).or_insert_with(|| {
                                let value = self.load_at(self.slots.settle_events, word * 64, 64);
                                self.store_at(self.slots.settle_events, word * 64, 64, zero);
                                value
                            });
                            let hit = *tests.entry((word, mask)).or_insert_with(|| {
                                let mask = self.constant(mask, 64);
                                let masked = self.binary(64, mask, BinaryOp::And, pending);
                                self.binary(1, masked, BinaryOp::Ne, zero)
                            });
                            active = self.binary(1, active, BinaryOp::Or, hit);
                        }
                        active
                    });
            }
        }
        schedule
            .groups
            .iter()
            .filter(|(a, _)| !a.every_settle)
            .map(|(a, _)| (predicates[&(&a.events, a.clock_inputs)], 0))
            .collect()
    }

    /// SIR switches have byte-sized selectors. Dispatch larger continuation
    /// numbers a byte at a time, retaining one switch for small designs.
    pub(super) fn dispatch_continuations(
        &mut self,
        targets: &[celox_sir::BlockId],
        default: celox_sir::BlockId,
    ) {
        let cases: Vec<_> = targets
            .iter()
            .copied()
            .enumerate()
            .map(|(i, target)| (i + 1, target))
            .collect();
        let bits = usize::BITS - targets.len().leading_zeros();
        let shift = bits.max(1).div_ceil(8) as usize * 8 - 8;
        self.dispatch_continuation_byte(&cases, shift, default);
    }

    fn dispatch_continuation_byte(
        &mut self,
        targets: &[(usize, celox_sir::BlockId)],
        shift: usize,
        default: celox_sir::BlockId,
    ) {
        let selector = self.load_at(self.slots.cont, shift, 8);
        let mut cases = Vec::new();
        let mut children = Vec::new();
        let mut start = 0;
        while start < targets.len() {
            let value = (targets[start].0 >> shift) & 255;
            let end = start
                + targets[start..].partition_point(|&(index, _)| ((index >> shift) & 255) == value);
            let target = if shift == 0 {
                targets[start].1
            } else {
                self.b.new_block()
            };
            cases.push(SIRSwitchCase {
                value: value.into(),
                target,
            });
            if shift > 0 {
                children.push((target, start, end));
            }
            start = end;
        }
        self.b.seal_block(SIRTerminator::Switch {
            selector,
            cases,
            default,
        });
        for (block, start, end) in children {
            self.b.switch_to_block(block);
            self.dispatch_continuation_byte(&targets[start..end], shift - 8, default);
        }
    }
}
