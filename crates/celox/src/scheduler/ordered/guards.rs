//! Keep the common budget check on the path it guards. All failures share
//! the same diagnostic, which receives the process, time and budget kind.
use super::*;
use std::mem::offset_of;

pub(in crate::scheduler) struct Guards {
    fault: BlockId,
    process: RegisterId,
    request: RegisterId,
    time: RegisterId,
    service: u32,
}

impl Guards {
    pub(in crate::scheduler) fn new(g: &mut Generator<'_>, service: u32) -> Self {
        let [process, request, time] = std::array::from_fn(|_| g.b.alloc_bit(64, false));
        Self {
            fault: g.b.new_block_with(vec![process, request, time]),
            process,
            request,
            time,
            service,
        }
    }

    pub(in crate::scheduler) fn begin_slot(g: &mut Generator<'_>, time: RegisterId) {
        Self::require_context(g);
        let valid = g.arena_header_load(offset_of!(ArenaHeader, guard_valid));
        let valid = g.equals(valid, 1);
        let previous = g.arena_header_load(offset_of!(ArenaHeader, guard_time));
        let same = g.binary(1, previous, BinaryOp::Eq, time);
        let same = g.binary(1, same, BinaryOp::And, valid);
        let changed = g.equals(same, 0);
        g.if_bit(changed, 0, |g| {
            let one = g.constant(1, 64);
            g.arena_header_store(offset_of!(ArenaHeader, guard_time), time);
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
        });
    }

    pub(in crate::scheduler) fn charge(
        &self,
        g: &mut Generator<'_>,
        process: u64,
        request: u64,
        time: RegisterId,
    ) {
        let process = g.constant(process, 64);
        let (counter, limit) = match request {
            native_io::SCHEDULER_RUN => {
                let next = g.arena_header_load(offset_of!(ArenaHeader, recent_next));
                let mask = g.constant(63, 64);
                let slot = g.binary(64, next, BinaryOp::And, mask);
                let base = g.constant((offset_of!(ArenaHeader, recent) / 8) as u64, 64);
                let word = g.binary(64, slot, BinaryOp::Add, base);
                let header = g.arena_header();
                g.arena_store(header, word, process);
                let one = g.constant(1, 64);
                let next = g.binary(64, next, BinaryOp::Add, one);
                g.arena_header_store(offset_of!(ArenaHeader, recent_next), next);
                (
                    offset_of!(ArenaHeader, work),
                    offset_of!(ArenaHeader, work_limit),
                )
            }
            native_io::SCHEDULER_PASS => {
                let zero = g.constant(0, 64);
                g.arena_header_store(offset_of!(ArenaHeader, work), zero);
                (
                    offset_of!(ArenaHeader, passes),
                    offset_of!(ArenaHeader, wave_limit),
                )
            }
            native_io::CHECKPOINT => (
                offset_of!(ArenaHeader, returns),
                offset_of!(ArenaHeader, return_limit),
            ),
            _ => unreachable!("unknown loop budget"),
        };
        let count = g.arena_header_load(counter);
        let limit = g.arena_header_load(limit);
        let maximum = g.equals(count, u64::MAX);
        let one = g.constant(1, 64);
        let incremented = g.binary(64, count, BinaryOp::Add, one);
        let next = g.b.alloc_bit(64, false);
        g.b.emit(SIRInstruction::Mux(next, maximum, count, incremented));
        g.arena_header_store(counter, next);
        let exceeded = g.binary(1, next, BinaryOp::GtU, limit);
        let continuation = g.b.new_block();
        let request = g.constant(request, 64);
        g.b.seal_block(SIRTerminator::Branch {
            cond: exceeded,
            true_block: (self.fault, vec![process, request, time]),
            false_block: (continuation, vec![]),
        });
        g.b.switch_to_block(continuation);
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
        g.b.switch_to_block(self.fault);
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
    }
}
