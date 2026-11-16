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

    /// Record that state combinational logic reads may have changed.
    pub(super) fn mark_dirty(&mut self) {
        if self.settle_reads.is_some() {
            let one = self.constant(1, SCHEDULER_FLAG_WIDTH);
            self.store(self.slots.dirty, SCHEDULER_FLAG_WIDTH, one);
        }
    }

    /// Run the stateful part of a deferred settle (`Lazy::eager`) where the
    /// reference settles, then continue at `target`, as
    /// [`Self::settle_then`]: one copy serves every caller.
    pub(super) fn eager_then(&mut self, eager: &[Unit], target: celox_sir::BlockId) {
        if eager.is_empty() {
            self.b.seal_block(SIRTerminator::Jump(target, Vec::new()));
            return;
        }
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
        let run = self.b.new_block();
        let done = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: changed,
            true_block: (run, Vec::new()),
            false_block: (done, Vec::new()),
        });
        self.b.switch_to_block(run);
        for unit in eager {
            self.inline_with_exit(unit, None);
        }
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
    pub(super) fn settle_then(&mut self, target: celox_sir::BlockId) {
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
        // The updates the settle follows, then its own.
        if let Some((service, checks, events)) = updates {
            self.report_update(service, checks, events, finish_exit, AfterWrites::Settle);
        }
        self.observers_before(internal_exit);
        if self.settle_reads.is_some() {
            let dirty = self.load(self.slots.dirty, SCHEDULER_FLAG_WIDTH);
            let zero = self.constant(0, SCHEDULER_FLAG_WIDTH);
            let changed = self.binary(1, dirty, BinaryOp::Ne, zero);
            let settle = self.b.new_block();
            let settled = self.b.new_block();
            self.b.seal_block(SIRTerminator::Branch {
                cond: changed,
                true_block: (settle, Vec::new()),
                false_block: (settled, Vec::new()),
            });
            self.b.switch_to_block(settle);
            if let Some(cone) = self.incremental {
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
                for unit in &cone.units {
                    self.inline_with_exit(unit, None);
                }
                self.b.seal_block(SIRTerminator::Jump(done, Vec::new()));
                self.b.switch_to_block(full);
                for unit in units {
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
        let targets = self.continuations.clone();
        self.dispatch_continuations(&targets, internal_exit);
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
