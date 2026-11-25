//! Complete an initial wave that cannot publish updates or start another wave.
use super::*;

impl Event {
    fn read_only_monitor(&self) -> bool {
        !self.active_commit
            && self.apply.as_ref().is_some_and(|units| {
                units.iter().all(|u| {
                    u.blocks.len() == 1
                        && u.blocks[&u.entry_block_id].instructions.is_empty()
                        && matches!(
                            u.blocks[&u.entry_block_id].terminator,
                            SIRTerminator::Return
                        )
                })
            })
            && self.eval_only.as_ref().is_some_and(|units| {
                units.iter().all(|u| {
                    instructions(u).all(|inst| {
                        !inst.has_side_effects()
                            || matches!(inst, SIRInstruction::RuntimeEvent { .. })
                    })
                })
            })
    }
}

impl Plan {
    pub(super) fn finish_read_only_wave(
        &self,
        g: &mut Generator<'_>,
        ordered: Ordered,
        tracked: &[usize],
        triggers: RegisterId,
        slot_end: BlockId,
        finish_exit: BlockId,
        checks: &[WaitCheck<'_>],
    ) {
        let independent = self.inactive.is_empty()
            && self.nba_processes.is_empty()
            && self
                .kernels
                .iter()
                .flatten()
                .filter_map(|kernel| kernel.wait.as_ref())
                .all(|wait| wait.sites.is_empty())
            && g.nbas.is_none()
            && g.observers.is_none()
            && g.settle_reads.is_some();
        if !independent {
            return;
        }
        let dispatch = g.b.new_block();
        let monitors: Vec<_> = tracked
            .iter()
            .copied()
            .filter(|&i| self.events[i].read_only_monitor())
            .collect();
        let monitored = g.event_mask(&monitors);
        let all = g.ones(self.event_bits);
        let writers = g.binary(self.event_bits, monitored, BinaryOp::Xor, all);
        let writes = g.binary(self.event_bits, triggers, BinaryOp::And, writers);
        let read_only = g.equals(writes, 0);
        let observe = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: read_only,
            true_block: (observe, vec![]),
            false_block: (dispatch, vec![]),
        });
        g.b.switch_to_block(observe);
        let idle = g.no_host_observers();
        let dirty = g.load(g.slots.dirty, SCHEDULER_FLAG_WIDTH);
        let clean = g.equals(dirty, 0);
        let skip = g.binary(1, idle, BinaryOp::And, clean);
        let done = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: skip,
            true_block: (done, vec![]),
            false_block: (dispatch, vec![]),
        });
        g.b.switch_to_block(done);
        if !monitors.is_empty() {
            let run = g.b.new_block();
            let finished = g.b.new_block();
            let quiet = g.equals(triggers, 0);
            g.b.seal_block(SIRTerminator::Branch {
                cond: quiet,
                true_block: (finished, vec![]),
                false_block: (run, vec![]),
            });
            g.b.switch_to_block(run);
            let waves = g.arena_header_load(std::mem::offset_of!(ArenaHeader, wave_limit));
            let exceeded = g.equals(waves, 0);
            g.if_bit(exceeded, 0, |g| {
                let zero = g.constant(0, 64);
                g.arena_service(ordered.service, native_io::ARENA_WAVE_LIMIT, zero);
            });
            let one = g.constant(1, 64);
            g.arena_header_store(std::mem::offset_of!(ArenaHeader, waves), one);
            g.store(g.slots.exit_arg, self.event_bits, triggers);
            g.for_selected_events(triggers, &monitors, |g, i| {
                let event = &self.events[i];
                g.inline_event_writers(event, event.eval_only.as_ref().unwrap(), finish_exit);
            });
            g.b.seal_block(SIRTerminator::Jump(finished, vec![]));
            g.b.switch_to_block(finished);
        }
        let one = g.constant(1, 64);
        g.arena_header_store(std::mem::offset_of!(ArenaHeader, phase), one);
        let zero = g.constant(0, SCHEDULER_FLAG_WIDTH);
        g.store_at(
            g.slots.defer_limit_settle,
            celox_design::SCHEDULER_ACTIVE_RESUME_BIT,
            SCHEDULER_FLAG_WIDTH,
            zero,
        );
        g.check_waits(checks);
        g.b.seal_block(SIRTerminator::Jump(slot_end, vec![]));
        g.b.switch_to_block(dispatch);
    }
}
