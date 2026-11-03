//! Successful loop-budget checks stay in generated code. Share the guard and
//! dispatch its continuations so large process lists do not duplicate it.
use super::*;
use std::mem::offset_of;

pub(in crate::scheduler) struct Guards {
    entry: BlockId,
    process: RegisterId,
    request: RegisterId,
    time: RegisterId,
    continuation: RegisterId,
    targets: Vec<BlockId>,
    service: u32,
}

impl Guards {
    pub(in crate::scheduler) fn new(g: &mut Generator<'_>, service: u32) -> Self {
        let process = g.b.alloc_bit(64, false);
        let request = g.b.alloc_bit(64, false);
        let time = g.b.alloc_bit(64, false);
        let continuation = g.b.alloc_bit(64, false);
        Self {
            entry: g
                .b
                .new_block_with(vec![process, request, time, continuation]),
            process,
            request,
            time,
            continuation,
            targets: Vec::new(),
            service,
        }
    }

    pub(in crate::scheduler) fn charge(
        &mut self,
        g: &mut Generator<'_>,
        process: u64,
        request: u64,
        time: RegisterId,
    ) {
        let process = g.constant(process, 64);
        let request = g.constant(request, 64);
        let continuation = g.constant(self.targets.len() as u64, 64);
        let next = g.b.new_block();
        self.targets.push(next);
        g.b.seal_block(SIRTerminator::Jump(
            self.entry,
            vec![process, request, time, continuation],
        ));
        g.b.switch_to_block(next);
    }

    pub(in crate::scheduler) fn process_phase(g: &mut Generator<'_>) {
        Self::require_context(g);
        let zero = g.constant(0, 64);
        g.arena_header_store(offset_of!(ArenaHeader, returns), zero);
    }

    fn require_context(g: &mut Generator<'_>) {
        let context = g.load_at(
            g.slots.defer_limit_settle,
            celox_design::SCHEDULER_HOST_CONTEXT_BIT,
            64,
        );
        let zero = g.constant(0, 64);
        let valid = g.binary(1, context, BinaryOp::Ne, zero);
        g.arena_require(valid);
    }

    pub(in crate::scheduler) fn emit(self, g: &mut Generator<'_>, failed: BlockId) {
        g.b.switch_to_block(self.entry);
        Self::require_context(g);
        let valid = g.arena_header_load(offset_of!(ArenaHeader, guard_valid));
        let valid = g.equals(valid, 1);
        let previous = g.arena_header_load(offset_of!(ArenaHeader, guard_time));
        let same = g.binary(1, previous, BinaryOp::Eq, self.time);
        let same = g.binary(1, same, BinaryOp::And, valid);
        let one = g.constant(1, 1);
        let reset = g.binary(1, same, BinaryOp::Xor, one);
        g.if_bit(reset, 0, |g| {
            g.arena_header_store(offset_of!(ArenaHeader, guard_time), self.time);
            let one = g.constant(1, 64);
            g.arena_header_store(offset_of!(ArenaHeader, guard_valid), one);
            let zero = g.constant(0, 64);
            for offset in [
                offset_of!(ArenaHeader, passes),
                offset_of!(ArenaHeader, work),
                offset_of!(ArenaHeader, returns),
                offset_of!(ArenaHeader, recent_next),
            ] {
                g.arena_header_store(offset, zero);
            }
            let header = g.arena_header();
            g.words(64, |g, index| {
                let base = g.constant((offset_of!(ArenaHeader, recent) / 8) as u64, 64);
                let word = g.binary(64, index, BinaryOp::Add, base);
                let missing = g.constant(u64::MAX, 64);
                g.arena_store(header, word, missing);
            });
        });
        let offset = g.b.alloc_bit(64, false);
        let limit = g.b.alloc_bit(64, false);
        let charge = g.b.new_block_with(vec![offset, limit]);
        let run = g.b.new_block();
        let other = g.b.new_block();
        let is_run = g.equals(self.request, native_io::SCHEDULER_RUN);
        g.b.seal_block(SIRTerminator::Branch {
            cond: is_run,
            true_block: (run, Vec::new()),
            false_block: (other, Vec::new()),
        });
        g.b.switch_to_block(run);
        let next = g.arena_header_load(offset_of!(ArenaHeader, recent_next));
        let base = g.constant((offset_of!(ArenaHeader, recent) / 8) as u64, 64);
        let word = g.binary(64, next, BinaryOp::Add, base);
        let header = g.arena_header();
        g.arena_store(header, word, self.process);
        let one = g.constant(1, 64);
        let next = g.binary(64, next, BinaryOp::Add, one);
        let mask = g.constant(63, 64);
        let next = g.binary(64, next, BinaryOp::And, mask);
        g.arena_header_store(offset_of!(ArenaHeader, recent_next), next);
        let work = g.constant(offset_of!(ArenaHeader, work) as u64, 64);
        let work_limit = g.arena_header_load(offset_of!(ArenaHeader, work_limit));
        g.b.seal_block(SIRTerminator::Jump(charge, vec![work, work_limit]));
        g.b.switch_to_block(other);
        let pass = g.b.new_block();
        let returned = g.b.new_block();
        let is_pass = g.equals(self.request, native_io::SCHEDULER_PASS);
        g.b.seal_block(SIRTerminator::Branch {
            cond: is_pass,
            true_block: (pass, Vec::new()),
            false_block: (returned, Vec::new()),
        });
        g.b.switch_to_block(pass);
        let zero = g.constant(0, 64);
        g.arena_header_store(offset_of!(ArenaHeader, work), zero);
        let passes = g.constant(offset_of!(ArenaHeader, passes) as u64, 64);
        let round_limit = g.arena_header_load(offset_of!(ArenaHeader, wave_limit));
        g.b.seal_block(SIRTerminator::Jump(charge, vec![passes, round_limit]));
        g.b.switch_to_block(returned);
        let returns = g.constant(offset_of!(ArenaHeader, returns) as u64, 64);
        let return_limit = g.arena_header_load(offset_of!(ArenaHeader, return_limit));
        g.b.seal_block(SIRTerminator::Jump(charge, vec![returns, return_limit]));
        g.b.switch_to_block(charge);
        let header = g.arena_header();
        let count = g.native_load(Memory::ReadIndirect, vec![header, offset]);
        let maximum = g.equals(count, u64::MAX);
        let one = g.constant(1, 64);
        let incremented = g.binary(64, count, BinaryOp::Add, one);
        let next = g.b.alloc_bit(64, false);
        g.b.emit(SIRInstruction::Mux(next, maximum, count, incremented));
        g.native_store(Memory::WriteIndirect, vec![header, offset, next]);
        let exceeded = g.binary(1, next, BinaryOp::GtU, limit);
        let fault = g.b.new_block();
        let dispatch = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: exceeded,
            true_block: (fault, Vec::new()),
            false_block: (dispatch, Vec::new()),
        });
        g.b.switch_to_block(fault);
        let context = g.load_at(
            g.slots.defer_limit_settle,
            celox_design::SCHEDULER_HOST_CONTEXT_BIT,
            64,
        );
        let request = g.constant(native_io::GUARD_FAULT, 64);
        let result = g.b.alloc_bit(64, false);
        g.b.emit(SIRInstruction::ExternCall {
            dst: Some(result),
            func: self.service,
            args: vec![context, self.process, request, self.time, self.request],
        });
        g.b.seal_block(SIRTerminator::Jump(failed, Vec::new()));
        g.b.switch_to_block(dispatch);
        let targets: Vec<_> = self.targets.iter().copied().enumerate().collect();
        let bits = usize::BITS - targets.len().saturating_sub(1).leading_zeros();
        self.dispatch(
            g,
            &targets,
            bits.max(1).div_ceil(8) as usize * 8 - 8,
            failed,
        );
    }

    fn dispatch(
        &self,
        g: &mut Generator<'_>,
        targets: &[(usize, BlockId)],
        shift: usize,
        failed: BlockId,
    ) {
        let selector = g.b.alloc_bit(8, false);
        g.b.emit(SIRInstruction::Slice(selector, self.continuation, shift, 8));
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
                g.b.new_block()
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
        g.b.seal_block(SIRTerminator::Switch {
            selector,
            cases,
            default: failed,
        });
        for (block, start, end) in children {
            g.b.switch_to_block(block);
            self.dispatch(g, &targets[start..end], shift - 8, failed);
        }
    }
}
