//! Ordered Active-domain evaluations and deferred publication inside native code.
mod guards;
pub(super) use guards::Guards;

use super::*;
use crate::simulator::native_io::{self, ArenaHeader};
use celox_sir::{BlockId, native_memory::NativeMemoryOp as Memory};

#[derive(Clone, Copy)]
pub(super) struct Ordered {
    working_start: usize,
    working_words: usize,
    events: usize,
    service: u32,
}

impl Ordered {
    pub(super) fn new(program: &LaidOutProgram, events: usize, service: u32) -> Self {
        let layout = program.layout();
        let start = layout.working_base_offset;
        let end = layout.triggered_bits_offset;
        assert!(start.is_multiple_of(8) && end.is_multiple_of(8));
        Self {
            working_start: start,
            working_words: (end - start) / 8,
            events,
            service,
        }
    }

    fn reset(&self, g: &mut Generator<'_>) {
        let required = g.constant((self.events * 2) as u64, 64);
        self.reserve(g, false, required);
        let zero = g.constant(0, 64);
        g.arena_header_store(std::mem::offset_of!(ArenaHeader, record_len), zero);
        g.arena_header_store(std::mem::offset_of!(ArenaHeader, image_len), zero);
        let pointer = g.arena_header_load(std::mem::offset_of!(ArenaHeader, records));
        g.words(self.events, |g, index| {
            let missing = g.constant(u64::MAX, 64);
            g.arena_store(pointer, index, missing);
        });
    }

    fn reserve(&self, g: &mut Generator<'_>, images: bool, required: RegisterId) {
        let capacity = g.arena_header_load(if images {
            std::mem::offset_of!(ArenaHeader, image_capacity)
        } else {
            std::mem::offset_of!(ArenaHeader, record_capacity)
        });
        let grow = g.binary(1, required, BinaryOp::GtU, capacity);
        g.if_bit(grow, 0, |g| {
            let request = if images {
                native_io::ARENA_RESERVE_IMAGES
            } else {
                native_io::ARENA_RESERVE_RECORDS
            };
            g.arena_service(self.service, request, required);
        });
    }

    fn record_word(&self, g: &mut Generator<'_>, record: RegisterId, field: usize) -> RegisterId {
        let two = g.constant(2, 64);
        let record = g.binary(64, record, BinaryOp::Mul, two);
        let base = g.constant((self.events * 2 + field) as u64, 64);
        g.binary(64, record, BinaryOp::Add, base)
    }

    /// Save the current complete working image, as the reference supersede
    /// operation does. Only a repeated pending evaluation needs a snapshot.
    fn capture(&self, g: &mut Generator<'_>) -> RegisterId {
        let index = g.arena_header_load(std::mem::offset_of!(ArenaHeader, image_len));
        let limit = g.constant(u64::MAX / self.working_words.max(1) as u64 - 1, 64);
        let fits = g.binary(1, index, BinaryOp::LtU, limit);
        g.arena_require(fits);
        let one = g.constant(1, 64);
        let next = g.binary(64, index, BinaryOp::Add, one);
        let words = g.constant(self.working_words as u64, 64);
        let required = g.binary(64, next, BinaryOp::Mul, words);
        self.reserve(g, true, required);
        self.copy_image(g, index, false);
        g.arena_header_store(std::mem::offset_of!(ArenaHeader, image_len), next);
        index
    }

    fn copy_image(&self, g: &mut Generator<'_>, image: RegisterId, restore: bool) {
        let pointer = g.arena_header_load(std::mem::offset_of!(ArenaHeader, images));
        let stride = g.constant(self.working_words as u64, 64);
        let image = g.binary(64, image, BinaryOp::Mul, stride);
        g.words(self.working_words, |g, index| {
            let image_word = g.binary(64, image, BinaryOp::Add, index);
            let eight = g.constant(8, 64);
            let offset = g.binary(64, index, BinaryOp::Mul, eight);
            let base = g.constant(self.working_start as u64, 64);
            let offset = g.binary(64, offset, BinaryOp::Add, base);
            if restore {
                let value = g.arena_load(pointer, image_word);
                g.native_store(Memory::WriteState, vec![offset, value]);
            } else {
                let value = g.native_load(Memory::ReadState, vec![offset]);
                g.arena_store(pointer, image_word, value);
            }
        });
    }

    fn push(&self, g: &mut Generator<'_>, event: usize) {
        let count = g.arena_header_load(std::mem::offset_of!(ArenaHeader, record_len));
        let limit = g.constant((u64::MAX - (self.events * 2) as u64) / 2 - 1, 64);
        let fits = g.binary(1, count, BinaryOp::LtU, limit);
        g.arena_require(fits);
        let required = self.record_word(g, count, 2);
        self.reserve(g, false, required);
        let pointer = g.arena_header_load(std::mem::offset_of!(ArenaHeader, records));
        let event_word = g.constant(event as u64, 64);
        let previous = g.arena_load(pointer, event_word);
        let missing = g.constant(u64::MAX, 64);
        let present = g.binary(1, previous, BinaryOp::Ne, missing);
        g.if_bit(present, 0, |g| {
            let image = self.capture(g);
            // Growing the image buffer cannot relocate the record buffer.
            let slot = self.record_word(g, previous, 1);
            g.arena_store(pointer, slot, image);
        });
        let id_slot = self.record_word(g, count, 0);
        g.arena_store(pointer, id_slot, event_word);
        let image_slot = self.record_word(g, count, 1);
        let missing = g.constant(u64::MAX, 64);
        g.arena_store(pointer, image_slot, missing);
        g.arena_store(pointer, event_word, count);
        let one = g.constant(1, 64);
        let next = g.binary(64, count, BinaryOp::Add, one);
        g.arena_header_store(std::mem::offset_of!(ArenaHeader, record_len), next);
    }
}

impl Generator<'_> {
    pub(super) fn native_load(&mut self, op: Memory, args: Vec<RegisterId>) -> RegisterId {
        let dst = self.b.alloc_bit(64, false);
        self.b.emit(SIRInstruction::ExternCall {
            dst: Some(dst),
            func: op.function(),
            args,
        });
        dst
    }

    pub(super) fn native_store(&mut self, op: Memory, args: Vec<RegisterId>) {
        self.b.emit(SIRInstruction::ExternCall {
            dst: None,
            func: op.function(),
            args,
        });
    }

    fn arena_header(&mut self) -> RegisterId {
        let context = self.load_at(
            self.slots.defer_limit_settle,
            celox_design::SCHEDULER_HOST_CONTEXT_BIT,
            64,
        );
        let offset = self.constant(0, 64);
        self.native_load(Memory::ReadIndirect, vec![context, offset])
    }

    pub(super) fn arena_header_load(&mut self, offset: usize) -> RegisterId {
        let pointer = self.arena_header();
        let offset = self.constant(offset as u64, 64);
        self.native_load(Memory::ReadIndirect, vec![pointer, offset])
    }

    pub(super) fn arena_header_store(&mut self, offset: usize, value: RegisterId) {
        let pointer = self.arena_header();
        let offset = self.constant(offset as u64, 64);
        self.native_store(Memory::WriteIndirect, vec![pointer, offset, value]);
    }

    pub(super) fn arena_load(&mut self, pointer: RegisterId, word: RegisterId) -> RegisterId {
        let eight = self.constant(8, 64);
        let offset = self.binary(64, word, BinaryOp::Mul, eight);
        self.native_load(Memory::ReadIndirect, vec![pointer, offset])
    }

    pub(super) fn arena_store(&mut self, pointer: RegisterId, word: RegisterId, value: RegisterId) {
        let eight = self.constant(8, 64);
        let offset = self.binary(64, word, BinaryOp::Mul, eight);
        self.native_store(Memory::WriteIndirect, vec![pointer, offset, value]);
    }

    fn arena_require(&mut self, condition: RegisterId) {
        let next = self.b.new_block();
        let failed = self.exit_block(SchedulerExit::Internal);
        self.b.seal_block(SIRTerminator::Branch {
            cond: condition,
            true_block: (next, Vec::new()),
            false_block: (failed, Vec::new()),
        });
        self.b.switch_to_block(next);
    }

    fn arena_service(&mut self, function: u32, request: u64, argument: RegisterId) {
        let context = self.load_at(
            self.slots.defer_limit_settle,
            celox_design::SCHEDULER_HOST_CONTEXT_BIT,
            64,
        );
        let request = self.constant(request, 64);
        let time = self.load(self.slots.now, 64);
        let first = self.constant(0, 64);
        let result = self.b.alloc_bit(64, false);
        self.b.emit(SIRInstruction::ExternCall {
            dst: Some(result),
            func: function,
            args: vec![context, argument, request, time, first],
        });
        let ok = self.equals(result, 0);
        self.arena_require(ok);
    }

    pub(super) fn words(&mut self, count: usize, body: impl FnOnce(&mut Self, RegisterId)) {
        let count = self.constant(count as u64, 64);
        self.word_range(count, body);
    }

    fn word_range(&mut self, count: RegisterId, body: impl FnOnce(&mut Self, RegisterId)) {
        let index = self.b.alloc_bit(64, false);
        let header = self.b.new_block_with(vec![index]);
        let run = self.b.new_block();
        let done = self.b.new_block();
        let zero = self.constant(0, 64);
        self.b.seal_block(SIRTerminator::Jump(header, vec![zero]));
        self.b.switch_to_block(header);
        let more = self.binary(1, index, BinaryOp::LtU, count);
        self.b.seal_block(SIRTerminator::Branch {
            cond: more,
            true_block: (run, Vec::new()),
            false_block: (done, Vec::new()),
        });
        self.b.switch_to_block(run);
        body(self, index);
        let one = self.constant(1, 64);
        let next = self.binary(64, index, BinaryOp::Add, one);
        self.b.seal_block(SIRTerminator::Jump(header, vec![next]));
        self.b.switch_to_block(done);
    }
}

impl Plan {
    pub(super) fn batch_ordered(
        &self,
        g: &mut Generator<'_>,
        ordered: Ordered,
        tracked: &[usize],
        slot_end: BlockId,
        finish_exit: BlockId,
        checks: &[WaitCheck<'_>],
        pick: BlockId,
        wave: BlockId,
    ) {
        let context = g.load_at(
            g.slots.defer_limit_settle,
            celox_design::SCHEDULER_HOST_CONTEXT_BIT,
            64,
        );
        let zero = g.constant(0, 64);
        let valid = g.binary(1, context, BinaryOp::Ne, zero);
        g.arena_require(valid);
        ordered.reset(g);
        let zero = g.constant(0, 64);
        g.arena_header_store(std::mem::offset_of!(ArenaHeader, phase), zero);
        g.arena_header_store(std::mem::offset_of!(ArenaHeader, waves), zero);
        let flush = g.b.new_block();
        let mask = g.b.alloc_bit(self.event_bits, false);
        let fire = g.b.new_block_with(vec![mask]);
        let judge = g.b.new_block();
        if self.lazy.is_some() {
            g.settle_then(judge);
        } else {
            g.b.seal_block(SIRTerminator::Jump(judge, Vec::new()));
        }
        g.b.switch_to_block(judge);
        let active = g.load(g.slots.exit_arg, self.event_bits);
        let levels = g.load_levels();
        let stable = self.stable_edges(g, tracked, levels);
        let counted = self.counted_edges(g);
        let active = g.binary(self.event_bits, active, BinaryOp::Or, counted);
        let stable = g.binary(self.event_bits, stable, BinaryOp::Or, counted);
        let zero = g.constant(0, self.event_bits);
        let edged = if self.gating.is_empty() {
            g.binary(1, active, BinaryOp::Ne, zero)
        } else {
            let source = self.source_edged(g, counted);
            g.equals(source, 1)
        };
        let extra = g.b.alloc_bit(self.event_bits, false);
        g.b.emit(SIRInstruction::Mux(extra, edged, zero, stable));
        let triggers = g.binary(self.event_bits, active, BinaryOp::Or, extra);
        g.store(g.slots.exit_arg, self.event_bits, zero);
        g.store_at(g.slots.exit_arg, self.event_bits, self.event_bits, zero);
        let quiet = g.binary(1, triggers, BinaryOp::Eq, zero);
        g.b.seal_block(SIRTerminator::Branch {
            cond: quiet,
            true_block: (flush, Vec::new()),
            false_block: (fire, vec![triggers]),
        });

        g.b.switch_to_block(wave);
        let phase = g.arena_header_load(std::mem::offset_of!(ArenaHeader, phase));
        let active_phase = g.equals(phase, 0);
        let levels = g.load_levels();
        let stable = self.stable_edges(g, tracked, levels);
        let counted = self.counted_edges(g);
        let pulses = g.load_at(g.slots.exit_arg, self.event_bits, self.event_bits);
        let ready = g.binary(self.event_bits, stable, BinaryOp::Or, counted);
        let ready = g.binary(self.event_bits, ready, BinaryOp::Or, pulses);
        let fired = g.load(g.slots.exit_arg, self.event_bits);
        let all = g.ones(self.event_bits);
        let unfired = g.binary(self.event_bits, fired, BinaryOp::Xor, all);
        let post_ready = g.binary(self.event_bits, ready, BinaryOp::And, unfired);
        // Process continuations accumulate newly approved edges in the low
        // plane. In the post-NBA phase that plane instead records fired domains.
        let active_ready = g.binary(self.event_bits, ready, BinaryOp::Or, fired);
        let selected = g.b.alloc_bit(self.event_bits, false);
        g.b.emit(SIRInstruction::Mux(
            selected,
            active_phase,
            active_ready,
            post_ready,
        ));
        g.if_bit(active_phase, 0, |g| {
            let zero = g.constant(0, self.event_bits);
            g.store(g.slots.exit_arg, self.event_bits, zero);
        });
        let zero = g.constant(0, self.event_bits);
        g.store_at(g.slots.exit_arg, self.event_bits, self.event_bits, zero);
        // Approve edges before waking Active waiters: a resumed process may
        // change the gate again, but cannot revoke an edge already observed.
        g.check_waits(checks);
        let ready = g.load(g.slots.ready, g.ready_bits);
        let no_processes = g.constant(0, g.ready_bits);
        let ready = g.binary(1, ready, BinaryOp::Ne, no_processes);
        let resume = g.binary(1, ready, BinaryOp::And, active_phase);
        let drain = g.b.new_block();
        let judge_wave = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: resume,
            true_block: (drain, Vec::new()),
            false_block: (judge_wave, Vec::new()),
        });
        g.b.switch_to_block(drain);
        g.store_at(g.slots.exit_arg, self.event_bits, self.event_bits, selected);
        self.ordered_save_levels(g, tracked);
        let one = g.constant(1, SCHEDULER_FLAG_WIDTH);
        g.store_at(
            g.slots.defer_limit_settle,
            celox_design::SCHEDULER_ACTIVE_RESUME_BIT,
            SCHEDULER_FLAG_WIDTH,
            one,
        );
        g.b.seal_block(SIRTerminator::Jump(pick, Vec::new()));
        g.b.switch_to_block(judge_wave);
        let quiet = g.binary(1, selected, BinaryOp::Eq, zero);
        let no_edges = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: quiet,
            true_block: (no_edges, Vec::new()),
            false_block: (fire, vec![selected]),
        });
        g.b.switch_to_block(no_edges);
        // Only the pre-NBA phase has deferred results left at a quiet wave.
        g.b.seal_block(SIRTerminator::Branch {
            cond: active_phase,
            true_block: (flush, Vec::new()),
            false_block: (slot_end, Vec::new()),
        });

        g.b.switch_to_block(fire);
        let waves = g.arena_header_load(std::mem::offset_of!(ArenaHeader, waves));
        let limit = g.arena_header_load(std::mem::offset_of!(ArenaHeader, wave_limit));
        let exceeded = g.binary(1, waves, BinaryOp::GeU, limit);
        g.if_bit(exceeded, 0, |g| {
            g.arena_service(ordered.service, native_io::ARENA_WAVE_LIMIT, waves);
        });
        let one = g.constant(1, 64);
        let next = g.binary(64, waves, BinaryOp::Add, one);
        g.arena_header_store(std::mem::offset_of!(ArenaHeader, waves), next);
        let mut commits = g.constant(0, self.event_bits);
        for &event in &self.active {
            let bit = g.single(event, self.event_bits);
            commits = g.binary(self.event_bits, commits, BinaryOp::Or, bit);
        }
        let commits = g.binary(self.event_bits, commits, BinaryOp::And, mask);
        let zero = g.constant(0, self.event_bits);
        let commits = g.binary(1, commits, BinaryOp::Ne, zero);
        let phase = g.arena_header_load(std::mem::offset_of!(ArenaHeader, phase));
        let pre = g.equals(phase, 0);
        let pre = if self.inactive.is_empty() {
            g.binary(1, pre, BinaryOp::And, commits)
        } else {
            // An Inactive continuation may cause another evaluation before
            // any pending nonblocking result may become visible (LRM 4.5).
            pre
        };
        let post = g.equals(pre, 0);
        g.if_bit(post, 0, |g| {
            let phase = g.constant(1, 64);
            g.arena_header_store(std::mem::offset_of!(ArenaHeader, phase), phase);
            let fired = g.load(g.slots.exit_arg, self.event_bits);
            let fired = g.binary(self.event_bits, fired, BinaryOp::Or, mask);
            g.store(g.slots.exit_arg, self.event_bits, fired);
        });
        self.ordered_save_levels(g, tracked);
        for &event in tracked {
            g.if_bit(mask, event, |g| {
                ordered.push(g, event);
                g.inline_writers(
                    self.events[event]
                        .eval_only
                        .as_ref()
                        .expect("ordered evaluation unit"),
                    finish_exit,
                );
            });
        }
        let settle = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: pre,
            true_block: (settle, Vec::new()),
            false_block: (flush, Vec::new()),
        });
        g.b.switch_to_block(settle);
        g.settle_then(wave);

        g.b.switch_to_block(flush);
        // Active-domain evaluation has settled before entering NBA. The next
        // wave compares against these values, not those before the last
        // blocking evaluation. A post-NBA wave still needs its pre-eval levels.
        let phase = g.arena_header_load(std::mem::offset_of!(ArenaHeader, phase));
        let pre = g.equals(phase, 0);
        g.if_bit(pre, 0, |g| self.ordered_save_levels(g, tracked));
        if !self.inactive.is_empty() {
            let ordinals = ordinals(&self.kernels);
            let time = g.load(g.slots.now, SCHEDULER_TIME_WIDTH);
            let mut ready = g.constant(0, g.ready_bits);
            for &(index, status) in &self.inactive {
                let waiting = g.waiting_for_time(index, Some(status));
                // SDK Delay(0) resumes after NBA, whereas InactiveDelay(0)
                // resumes before it. Computed delays can return either status.
                let inactive = g.equals_at(status, ProcessStatus::InactiveDelay);
                let waiting = g.binary(1, waiting, BinaryOp::And, inactive);
                let wake = g.load(g.slots.wake[index], SCHEDULER_TIME_WIDTH);
                let now = g.binary(1, wake, BinaryOp::Eq, time);
                let due = g.binary(1, waiting, BinaryOp::And, now);
                let bit = g.bit(due, ordinals[index]);
                ready = g.binary(g.ready_bits, ready, BinaryOp::Or, bit);
            }
            let zero = g.constant(0, g.ready_bits);
            let any = g.binary(1, ready, BinaryOp::Ne, zero);
            let promote = g.binary(1, pre, BinaryOp::And, any);
            let inactive = g.b.new_block();
            let publish = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: promote,
                true_block: (inactive, Vec::new()),
                false_block: (publish, Vec::new()),
            });
            g.b.switch_to_block(inactive);
            g.store(g.slots.ready, g.ready_bits, ready);
            let one = g.constant(1, SCHEDULER_FLAG_WIDTH);
            g.store_at(
                g.slots.defer_limit_settle,
                celox_design::SCHEDULER_ACTIVE_RESUME_BIT,
                SCHEDULER_FLAG_WIDTH,
                one,
            );
            // Keep the ordered pending records and their working images. The
            // normal Active work loop returns to `wave` after the #0 kernels.
            g.b.seal_block(SIRTerminator::Jump(pick, Vec::new()));
            g.b.switch_to_block(publish);
        }
        let zero = g.constant(0, SCHEDULER_FLAG_WIDTH);
        g.store_at(
            g.slots.defer_limit_settle,
            celox_design::SCHEDULER_ACTIVE_RESUME_BIT,
            SCHEDULER_FLAG_WIDTH,
            zero,
        );
        let one = g.constant(1, 64);
        g.arena_header_store(std::mem::offset_of!(ArenaHeader, phase), one);
        self.ordered_raw_levels(g, ordered, tracked, false);
        let images = g.arena_header_load(std::mem::offset_of!(ArenaHeader, image_len));
        let zero = g.constant(0, 64);
        let has_images = g.binary(1, images, BinaryOp::Ne, zero);
        g.if_bit(has_images, 0, |g| {
            let live = ordered.capture(g);
            self.ordered_publish(g, ordered, tracked, finish_exit, checks, true);
            ordered.copy_image(g, live, true);
        });
        self.ordered_publish(g, ordered, tracked, finish_exit, checks, false);
        self.ordered_raw_levels(g, ordered, tracked, true);
        g.check_waits_at(checks, false);
        let staged: Vec<_> = self
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
        ordered.reset(g);
        g.settle_then(wave);
    }

    fn ordered_save_levels(&self, g: &mut Generator<'_>, tracked: &[usize]) {
        let mut levels = g.no_levels();
        for (bit, &event) in tracked.iter().enumerate() {
            let level = g.level(&self.events[event]);
            levels = g.with_level(levels, level, bit);
        }
        g.store_levels(levels);
    }

    fn ordered_raw_levels(
        &self,
        g: &mut Generator<'_>,
        ordered: Ordered,
        tracked: &[usize],
        record: bool,
    ) {
        let pointer = g.arena_header_load(std::mem::offset_of!(ArenaHeader, records));
        let mut pulses = g.load_at(g.slots.exit_arg, self.event_bits, self.event_bits);
        for &event in tracked {
            let index = g.constant((ordered.events + event) as u64, 64);
            let level = g.level(&self.events[event]);
            if record {
                let previous = g.arena_load(pointer, index);
                let was = g.b.alloc_bit(g.level_width(), false);
                g.b.emit(SIRInstruction::Slice(was, previous, 0, g.level_width()));
                let edge = g.edge(&self.events[event], was, level);
                let edge = g.event_bit(edge, event);
                pulses = g.binary(self.event_bits, pulses, BinaryOp::Or, edge);
            }
            let wide = g.widen_to(level, g.level_width(), 64);
            g.arena_store(pointer, index, wide);
        }
        g.store_at(g.slots.exit_arg, self.event_bits, self.event_bits, pulses);
    }

    fn ordered_publish(
        &self,
        g: &mut Generator<'_>,
        ordered: Ordered,
        tracked: &[usize],
        finish_exit: BlockId,
        checks: &[WaitCheck<'_>],
        snapshots: bool,
    ) {
        let count = g.arena_header_load(std::mem::offset_of!(ArenaHeader, record_len));
        g.word_range(count, |g, index| {
            let pointer = g.arena_header_load(std::mem::offset_of!(ArenaHeader, records));
            let slot = ordered.record_word(g, index, 1);
            let image = g.arena_load(pointer, slot);
            let missing = g.constant(u64::MAX, 64);
            let apply = g.binary(
                1,
                image,
                if snapshots {
                    BinaryOp::Ne
                } else {
                    BinaryOp::Eq
                },
                missing,
            );
            g.if_bit(apply, 0, |g| {
                if snapshots {
                    ordered.copy_image(g, image, true);
                }
                let slot = ordered.record_word(g, index, 0);
                let event = g.arena_load(pointer, slot);
                let done = g.b.new_block();
                self.ordered_dispatch(g, event, tracked, done, finish_exit);
                g.b.switch_to_block(done);
                if snapshots {
                    self.ordered_raw_levels(g, ordered, tracked, true);
                    g.check_waits_at(checks, false);
                }
            });
        });
    }

    fn ordered_dispatch(
        &self,
        g: &mut Generator<'_>,
        event: RegisterId,
        targets: &[usize],
        done: BlockId,
        finish_exit: BlockId,
    ) {
        if targets.is_empty() {
            let failed = g.exit_block(SchedulerExit::Internal);
            g.b.seal_block(SIRTerminator::Jump(failed, Vec::new()));
        } else if targets.len() == 1 {
            let valid = g.equals(event, targets[0] as u64);
            g.arena_require(valid);
            g.inline_writers(
                self.events[targets[0]]
                    .apply
                    .as_ref()
                    .expect("ordered apply unit"),
                finish_exit,
            );
            g.b.seal_block(SIRTerminator::Jump(done, Vec::new()));
        } else {
            let middle = targets.len() / 2;
            let pivot = g.constant(targets[middle] as u64, 64);
            let lower = g.binary(1, event, BinaryOp::LtU, pivot);
            let left = g.b.new_block();
            let right = g.b.new_block();
            g.b.seal_block(SIRTerminator::Branch {
                cond: lower,
                true_block: (left, Vec::new()),
                false_block: (right, Vec::new()),
            });
            g.b.switch_to_block(left);
            self.ordered_dispatch(g, event, &targets[..middle], done, finish_exit);
            g.b.switch_to_block(right);
            self.ordered_dispatch(g, event, &targets[middle..], done, finish_exit);
        }
    }
}

#[cfg(all(
    test,
    feature = "systemverilog",
    target_arch = "x86_64",
    not(feature = "arm64-codegen")
))]
mod tests {
    use crate::{SchedulerMode, Simulation};
    use std::path::Path;

    #[test]
    fn native_ordered_snapshots_preserve_a_pulse_and_reuse_the_arena() {
        let source = r#"`timescale 1ns/1ns
        module Top;
            bit clk = 0, q = 0;
            logic enable = 1'bx;
            bit [7:0] edges = 0, rises = 0, falls = 0;
            wire gated = clk & enable;
            always #1 clk = !clk;
            always @(posedge clk) enable = 1;
            always @(posedge gated) begin edges = edges + 1; q <= edges[0]; end
            initial begin #1000; if (!(edges > 0)) $fatal; end
            initial begin @(posedge q); rises = rises + 1; end
            initial begin @(negedge q); falls = falls + 1; end
        endmodule"#;
        let mut sim =
            Simulation::from_sv_sources(vec![(source, Path::new("ordered-pulse.sv"))], "Top")
                .four_state(true)
                .scheduler_mode(SchedulerMode::DifferentialRuns)
                .build_native()
                .unwrap();
        sim.run_until(0).unwrap();
        let checkpoint = sim.checkpoint().unwrap();
        sim.run_until(2).unwrap();
        assert_eq!(sim.get(sim.signal("edges")), 2u8.into());
        assert_eq!(sim.get(sim.signal("rises")), 1u8.into());
        assert_eq!(sim.get(sim.signal("falls")), 1u8.into());
        let before = sim.scheduler_stats();
        assert!(before.compiled_slots > 0, "{before:?}");
        assert!(
            before.native_service_calls > 0,
            "the cold arena must allocate"
        );
        sim.restore(&checkpoint).unwrap();
        sim.run_until(2).unwrap();
        assert_eq!(sim.get(sim.signal("edges")), 2u8.into());
        assert_eq!(sim.get(sim.signal("rises")), 1u8.into());
        assert_eq!(sim.get(sim.signal("falls")), 1u8.into());
        let after = sim.scheduler_stats();
        assert_eq!(before.reference_steps, after.reference_steps, "{after:?}");
        assert_eq!(
            before.native_service_calls, after.native_service_calls,
            "the warmed loop must stay in generated code: {after:?}"
        );
    }

    #[test]
    fn native_ordered_queue_grows_without_reference_fallback() {
        let source = r#"`timescale 1ns/1ns
module Top;
bit clk=0;bit[15:0] a=0,b=0,q=0,limit=0;
logic [130:0] wide;
logic [72:0] memory [4];
wire first=clk && a==b && a<limit;
wire second=clk && a!=b;
always #1 clk=!clk;
always @(posedge clk) limit=limit+200;
always @(posedge first) begin
    a=a+1;q<=a;
    wide <= {a, 115'bx};
    memory[a[1:0]] <= {a,57'bz};
end
always @(posedge second) b=a;
initial begin #1000; if(a!=limit || b!=limit) $fatal; end
endmodule
"#;
        for (four_state, hotspot) in [(false, false), (true, false), (false, true), (true, true)] {
            let build = || {
                Simulation::from_sv_sources(vec![(source, Path::new("ordered-growth.sv"))], "Top")
                    .four_state(four_state)
                    .hotspot_jit(hotspot)
                    .scheduler_mode(SchedulerMode::DifferentialRuns)
            };
            let mut sim = build().build_native().unwrap();
            sim.run_until(2).unwrap();
            assert_eq!(sim.get(sim.signal("a")), 200u16.into());
            assert_eq!(sim.get(sim.signal("q")), 200u16.into());
            if four_state {
                let (value, mask) = sim.get_four_state(sim.signal("wide"));
                let unknown = (num_bigint::BigUint::from(1u8) << 115) - 1u8;
                assert_eq!(mask, unknown);
                assert_eq!(value >> 115, 200u16.into());
            }
            let before = sim.scheduler_stats();
            assert!(before.compiled_slots > 0, "{before:?}");
            assert!(before.native_service_calls > 2, "{before:?}");
            sim.run_until(4).unwrap();
            assert_eq!(sim.get(sim.signal("q")), 400u16.into());
            let after = sim.scheduler_stats();
            assert_eq!(
                after.native_service_calls, before.native_service_calls,
                "{after:?}"
            );
            assert_eq!(after.reference_steps, before.reference_steps, "{after:?}");
            let mut bounded = build()
                .scheduler_mode(SchedulerMode::Compiled)
                .build_native()
                .unwrap();
            bounded.state.set_slot_budgets(32, 1024);
            let error = bounded.run_until(2).unwrap_err().to_string();
            assert!(error.contains("round budget"), "{error}");
        }
    }
    #[test]
    fn native_ordered_waiter_cannot_revoke_an_approved_edge() {
        let source = r#"`timescale 1ns/1ns
module Top;
bit clk=0;bit[15:0] a=0,b=0,q=0,limit=0;
wire first=clk && a==b && a<limit;
wire second=clk && a!=b;
bit veto=0;
wire final_gate=second && !veto;
bit [15:0] seen=0;
always @(posedge final_gate) seen=seen+1;
initial begin wait(a==1); veto=1; end
always #1 clk=!clk;
always @(posedge clk) limit=limit+2;
always @(posedge first) begin a=a+1;q<=a;end
always @(posedge second) b=a;
initial begin #1000; if(a!=limit || b!=limit || seen!=1) $fatal; end
endmodule
"#;
        for hotspot in [false, true] {
            for four_state in [false, true] {
                for level in [crate::OptLevel::O0, crate::OptLevel::O2] {
                    let mut sim = Simulation::from_sv_sources(
                        vec![(source, Path::new("ordered-waiter.sv"))],
                        "Top",
                    )
                    .four_state(four_state)
                    .opt_level(level)
                    .hotspot_jit(hotspot)
                    .scheduler_mode(SchedulerMode::DifferentialRuns)
                    .build_native()
                    .unwrap();
                    sim.run_until(2).unwrap();
                    assert_eq!(sim.get(sim.signal("seen")), 1u8.into());
                    assert_eq!(sim.get(sim.signal("q")), 2u8.into());
                    assert!(sim.scheduler_stats().compiled_slots > 0);
                }
            }
        }
    }

    #[test]
    fn native_ordered_finish_discards_pending_publications_in_loaded_image() {
        let source = r#"`timescale 1ns/1ns
        module Top;
            bit clk = 0; bit [7:0] q = 0;
            logic enable = 1'bx;
            bit [7:0] edges = 0, rises = 0, falls = 0;
            wire gated = clk & enable;
            always #1 clk = !clk;
            always @(posedge clk) enable = 1;
            always @(posedge gated) begin edges = edges + 1; q <= edges; end
            initial begin #1000; if (!(edges > 0)) $fatal; end
            initial begin wait(edges == 2); $finish; end
        endmodule"#;
        let build = || {
            Simulation::from_sv_sources(vec![(source, Path::new("ordered-finish.sv"))], "Top")
                .four_state(true)
                .scheduler_mode(SchedulerMode::DifferentialRuns)
        };
        let image = build().compile_native().unwrap().into_program_image();
        let bytes = postcard::to_allocvec(&image).unwrap();
        let mut sim = build()
            .build_native_from_image(postcard::from_bytes(&bytes).unwrap())
            .unwrap();
        sim.run_until(2).unwrap();
        assert!(sim.is_finished());
        assert_eq!(sim.get(sim.signal("edges")), 2u8.into());
        assert_eq!(sim.get(sim.signal("q")), 0u8.into());
        assert!(sim.scheduler_stats().compiled_slots > 0);
    }
    #[test]
    fn native_ordered_orphan_clock_keeps_active_feedback() {
        let source = r#"`timescale 1ns/1ns
module Top;
bit clk=0,dummy=0;bit[15:0] a=0,b=0,q=0;
wire first=clk && a==b && a<200;
wire second=clk && a!=b;
always #1 clk=!clk;

always @(posedge first) begin a=a+1;q<=a;end
always @(posedge second) b=a;
initial begin #1000; if(a!=200 || b!=200) $fatal; end
endmodule
"#;
        for four_state in [false, true] {
            let mut sim =
                Simulation::from_sv_sources(vec![(source, Path::new("ordered-orphan.sv"))], "Top")
                    .four_state(four_state)
                    .scheduler_mode(SchedulerMode::DifferentialRuns)
                    .build_native()
                    .unwrap();
            sim.run_until(2).unwrap();
            assert_eq!(sim.get(sim.signal("a")), 200u16.into());
            assert_eq!(sim.get(sim.signal("q")), 200u16.into());
            assert!(sim.scheduler_stats().compiled_slots > 0);
            assert!(sim.scheduler_stats().native_service_calls > 0);
        }
    }
    #[test]
    fn native_ordered_domains_cross_mask_words() {
        const DOMAINS: usize = 65;
        let mut source =
            String::from("`timescale 1ns/1ns\nmodule Top; bit clk=0; always #1 clk=!clk;\n");
        for i in 0..DOMAINS {
            source.push_str(&format!(
                "logic en{i}=1'bx; bit [7:0] count{i}=0; bit q{i}=0; wire g{i}=clk & en{i};\n"
            ));
            source.push_str(&format!("always @(posedge clk) en{i}=1; always @(posedge g{i}) begin count{i}=count{i}+1; q{i}<=count{i}[0]; end\n"));
            source.push_str(&format!(
                "initial begin #1000; if(count{i}==0) $fatal; end\n"
            ));
        }
        source.push_str("endmodule");
        let mut sim = Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("ordered-wide.sv"))],
            "Top",
        )
        .four_state(true)
        .scheduler_mode(SchedulerMode::DifferentialRuns)
        .build_native()
        .unwrap();
        assert!(
            sim.program()
                .runtime_schema
                .scheduler
                .as_ref()
                .unwrap()
                .events
                .len()
                > 64
        );
        sim.run_until(2).unwrap();
        for i in 0..DOMAINS {
            assert_eq!(
                sim.get(sim.signal(&format!("count{i}"))),
                2u8.into(),
                "domain {i}"
            );
            assert_eq!(
                sim.get(sim.signal(&format!("q{i}"))),
                0u8.into(),
                "domain {i}"
            );
        }
        let before = sim.scheduler_stats();
        assert!(before.compiled_slots > 0, "{before:?}");
        sim.run_until(4).unwrap();
        for i in 0..DOMAINS {
            assert_eq!(
                sim.get(sim.signal(&format!("count{i}"))),
                3u8.into(),
                "domain {i}"
            );
            assert_eq!(
                sim.get(sim.signal(&format!("q{i}"))),
                1u8.into(),
                "domain {i}"
            );
        }
        let after = sim.scheduler_stats();
        assert_eq!(after.reference_steps, before.reference_steps);
        assert_eq!(after.native_service_calls, before.native_service_calls);
    }
    #[test]
    fn inactive_and_computed_delays_keep_pending_domains_compiled() {
        for body in [
            "always #2 begin clk = 1; #0; seen = q; clk = 0; #0; turns++; end",
            "always #2 begin clk = 1; #0; seen = q; clk = 0; #0; clk = 1; #0; clk = 0; turns++; end",
            "always begin #(turns % 3); turns++; clk = !clk; #0; seen = q; end",
        ] {
            let source = format!(
                r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    int q = 0;
    int seen = 0;
    int turns = 0;
    always_ff @(posedge clk) q <= q + 1;
    {body}
endmodule"#
            );
            for (four_state, hotspot) in
                [(false, false), (true, false), (false, true), (true, true)]
            {
                let mut sim = Simulation::from_sv_sources(
                    vec![(
                        source.as_str(),
                        std::path::Path::new("compiled-inactive.sv"),
                    )],
                    "Top",
                )
                .four_state(four_state)
                .hotspot_jit(hotspot)
                .scheduler_mode(SchedulerMode::DifferentialRuns)
                .build_native()
                .unwrap();
                sim.run_until(1).unwrap();
                let before = sim.scheduler_stats();
                assert!(
                    before
                        .kernels
                        .iter()
                        .all(|k| *k == crate::KernelSchedule::Compiled),
                    "{before:?}"
                );
                let reference_steps = before.reference_steps;
                for end in [2, 3, 7, 12, 20] {
                    sim.run_until(end).unwrap();
                }
                let after = sim.scheduler_stats();
                assert_eq!(after.reference_steps, reference_steps, "{after:?}");
                assert!(
                    after.compiled_slots > 0 && after.checked_steps > 0,
                    "{after:?}"
                );
                if body.starts_with("always #2") {
                    assert_eq!(sim.get(sim.signal("q")), 10u8.into());
                    assert_eq!(sim.get(sim.signal("seen")), 9u8.into());
                }
            }
        }
    }

    #[test]
    fn service_continuations_preserve_event_writes_in_native_code() {
        for body in [
            "always #2 begin clk = 1; $fwrite(1, \"up\"); clk = 0; $fwrite(1, \"down\"); turns++; end",
            "always #2 begin clk = !clk; $fwrite(1, \"level %b\", clk); turns++; end",
            "always #2 turns++; always @(turns) begin clk = 1; $fwrite(1, \"up\"); clk = 0; $fwrite(1, \"down\"); end",
        ] {
            let source = format!(
                r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    int q = 0;
    int turns = 0;
    always_ff @(posedge clk) q <= q + 1;
    {body}
endmodule"#
            );
            for (four_state, hotspot) in
                [(false, false), (true, false), (false, true), (true, true)]
            {
                let build = |mode| {
                    Simulation::from_sv_sources(
                        vec![(
                            source.as_str(),
                            std::path::Path::new("compiled-service-events.sv"),
                        )],
                        "Top",
                    )
                    .four_state(four_state)
                    .hotspot_jit(hotspot)
                    .scheduler_mode(mode)
                    .build_native()
                    .unwrap()
                };
                let mut sim = build(SchedulerMode::Compiled);
                let mut reference = build(SchedulerMode::Reference);
                sim.run_until(1).unwrap();
                reference.run_until(1).unwrap();
                let before = sim.scheduler_stats();
                assert!(
                    before
                        .kernels
                        .iter()
                        .all(|k| *k == crate::KernelSchedule::Compiled),
                    "{before:?}"
                );
                let reference_steps = before.reference_steps;
                for end in [2, 3, 7, 12, 20] {
                    sim.run_until(end).unwrap();
                    reference.run_until(end).unwrap();
                    assert_eq!(sim.time(), reference.time());
                    for name in ["clk", "q", "turns"] {
                        assert_eq!(
                            sim.get(sim.signal(name)),
                            reference.get(reference.signal(name)),
                            "{name}: {body}"
                        );
                    }
                    assert_eq!(sim.drain_runtime_events(), reference.drain_runtime_events());
                }
                let after = sim.scheduler_stats();
                assert_eq!(after.reference_steps, reference_steps, "{after:?}");
                assert!(after.compiled_slots > 0, "{after:?}");
                assert_eq!(sim.get(sim.signal("turns")), 10u8.into());
            }
        }
    }

    #[test]
    fn inactive_nonconvergence_is_bounded_in_generated_code() {
        let source = r#"`timescale 1ns/1ns
module Top;
    int turns = 0;
    initial begin #1; forever begin #0; turns++; end end
endmodule"#;
        for four_state in [false, true] {
            let mut sim = Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("compiled-inactive-loop.sv"))],
                "Top",
            )
            .four_state(four_state)
            .scheduler_mode(SchedulerMode::Compiled)
            .build_native()
            .unwrap();
            sim.state.set_slot_budgets(8, 32);
            sim.run_until(0).unwrap();
            let before = sim.scheduler_stats();
            let error = sim.run_until(2).unwrap_err();
            assert!(error.to_string().contains("budget"), "{error}");
            let after = sim.scheduler_stats();
            assert_eq!(after.reference_steps, before.reference_steps, "{after:?}");
            assert!(
                after
                    .kernels
                    .iter()
                    .all(|k| *k == crate::KernelSchedule::Compiled),
                "{after:?}"
            );
        }
    }
}
