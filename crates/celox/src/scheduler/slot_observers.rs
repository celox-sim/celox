//! Host observation at a native domain-wave boundary. Only signal IO crosses
//! the service ABI; wait activation, settling and domain dispatch stay native.
use super::*;
use crate::simulator::native_io::{self, ArenaHeader};
use std::mem::offset_of;

impl Plan {
    pub(super) fn observe_slot(
        &self,
        g: &mut Generator<'_>,
        tracked: &[usize],
        checks: &[WaitCheck<'_>],
        finish: celox_sir::BlockId,
        first: bool,
    ) {
        let Some(service) = self.service_function else {
            return;
        };
        let enabled = g.arena_header_load(offset_of!(ArenaHeader, slot_observer));
        let enabled = g.equals(enabled, 1);
        let begin = g.b.new_block();
        let notify = g.b.new_block();
        let done = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: enabled,
            true_block: (begin, Vec::new()),
            false_block: (done, Vec::new()),
        });
        g.b.switch_to_block(begin);
        g.settle_then(notify);
        g.b.switch_to_block(notify);
        let before: Vec<_> = if first {
            self.events.iter().map(|event| g.level(event)).collect()
        } else {
            Vec::new()
        };
        let context = g.load_at(
            g.slots.defer_limit_settle,
            celox_design::SCHEDULER_HOST_CONTEXT_BIT,
            64,
        );
        let zero = g.constant(0, 64);
        let request = g.constant(native_io::BEFORE_DOMAINS, 64);
        let time = g.load(g.slots.now, 64);
        let result = g.b.alloc_bit(64, false);
        g.b.emit(SIRInstruction::ExternCall {
            dst: Some(result),
            func: service,
            args: vec![context, zero, request, time, zero],
        });
        let error = g.mask_bit(result, 0);
        let failed = g.exit_block(SchedulerExit::Internal);
        let success = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: error,
            true_block: (failed, Vec::new()),
            false_block: (success, Vec::new()),
        });
        g.b.switch_to_block(success);
        let stopped = g.mask_bit(result, 2);
        let continued = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: stopped,
            true_block: (finish, Vec::new()),
            false_block: (continued, Vec::new()),
        });
        g.b.switch_to_block(continued);
        let wrote = g.mask_bit(result, 1);
        let writes = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: wrote,
            true_block: (writes, Vec::new()),
            false_block: (done, Vec::new()),
        });
        g.b.switch_to_block(writes);
        g.mark_dirty();
        host_inputs::replay(g.program, g, checks);
        let mut edges = g.load(g.slots.exit_arg, self.event_bits);
        for (index, (event, was)) in self.events.iter().zip(before).enumerate() {
            let is = g.level(event);
            let edge = g.settled_edge(event, was, is);
            let bit = g.event_bit(edge, index);
            edges = g.binary(self.event_bits, edges, BinaryOp::Or, bit);
        }
        g.store(g.slots.exit_arg, self.event_bits, edges);
        let settled = g.b.new_block();
        g.settle_then(settled);
        g.b.switch_to_block(settled);
        g.check_waits(checks);
        self.judge_gates(g, tracked);
        g.b.seal_block(SIRTerminator::Jump(done, Vec::new()));
        g.b.switch_to_block(done);
    }
}

/// Where the code goes on after the host wrote at an update point.
#[derive(Clone, Copy)]
pub(super) enum AfterWrites {
    /// The settle that follows settles them.
    Settle,
    /// The settle starting at this block runs again.
    Again(celox_sir::BlockId),
    /// They settle here, and the waits see them, before the scheduler goes
    /// on: a kernel that runs next reads what they drive.
    Settled,
    /// The simulation ends: nothing reads them.
    Ended,
}

impl Generator<'_> {
    /// The host that watches every update sees the one just made
    /// (`AFTER_UPDATE`) when the observer flag of the arena is set. Its
    /// writes are host inputs of the slot, settled as `after` says; the
    /// settle shows the host its update in turn. The edges they give
    /// `events` are the slot's, as those of the writes before the domains.
    pub(super) fn report_update(
        &mut self,
        service: u32,
        checks: &[WaitCheck<'_>],
        events: &[Event],
        finish: celox_sir::BlockId,
        after: AfterWrites,
    ) {
        let enabled = self.arena_header_load(offset_of!(ArenaHeader, observes_updates));
        let enabled = self.equals(enabled, 1);
        let notify = self.b.new_block();
        let done = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: enabled,
            true_block: (notify, Vec::new()),
            false_block: (done, Vec::new()),
        });
        self.b.switch_to_block(notify);
        let before: Vec<_> = events.iter().map(|event| self.level(event)).collect();
        let context = self.load_at(
            self.slots.defer_limit_settle,
            celox_design::SCHEDULER_HOST_CONTEXT_BIT,
            64,
        );
        let zero = self.constant(0, 64);
        let request = self.constant(native_io::AFTER_UPDATE, 64);
        let time = self.load(self.slots.now, 64);
        let result = self.b.alloc_bit(64, false);
        self.b.emit(SIRInstruction::ExternCall {
            dst: Some(result),
            func: service,
            args: vec![context, zero, request, time, zero],
        });
        let error = self.mask_bit(result, 0);
        let failed = self.exit_block(SchedulerExit::Internal);
        let success = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: error,
            true_block: (failed, Vec::new()),
            false_block: (success, Vec::new()),
        });
        self.b.switch_to_block(success);
        let stopped = self.mask_bit(result, 2);
        let continued = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: stopped,
            true_block: (finish, Vec::new()),
            false_block: (continued, Vec::new()),
        });
        self.b.switch_to_block(continued);
        let wrote = self.mask_bit(result, 1);
        let writes = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: wrote,
            true_block: (writes, Vec::new()),
            false_block: (done, Vec::new()),
        });
        self.b.switch_to_block(writes);
        // Each settle of the host's writes is a pass, which the round
        // budget bounds (as `show_update` charges them).
        self.service_call(service, u64::MAX, native_io::SCHEDULER_PASS, time, failed);
        self.mark_dirty();
        host_inputs::replay(self.program, self, checks);
        if !events.is_empty() {
            let mut edges = self.load(self.slots.exit_arg, self.event_bits);
            for (index, (event, was)) in events.iter().zip(before).enumerate() {
                let is = self.level(event);
                let edge = self.settled_edge(event, was, is);
                let bit = self.event_bit(edge, index);
                edges = self.binary(self.event_bits, edges, BinaryOp::Or, bit);
            }
            self.store(self.slots.exit_arg, self.event_bits, edges);
        }
        match after {
            AfterWrites::Settle | AfterWrites::Ended => {
                self.b.seal_block(SIRTerminator::Jump(done, Vec::new()));
            }
            AfterWrites::Again(block) => {
                self.b.seal_block(SIRTerminator::Jump(block, Vec::new()));
            }
            AfterWrites::Settled => {
                let settled = self.b.new_block();
                self.settle_then(settled);
                self.b.switch_to_block(settled);
                self.check_waits(checks);
                self.b.seal_block(SIRTerminator::Jump(done, Vec::new()));
            }
        }
        self.b.switch_to_block(done);
    }
}

#[cfg(all(test, target_arch = "x86_64", not(feature = "arm64-codegen")))]
#[allow(
    clippy::disallowed_types,
    reason = "test-only: the tests read back what the observers they hand to the simulation record"
)]
mod tests {
    use crate::{SchedulerMode, SignalRef, SimBackend, Simulation, SlotIo, SlotObserver};
    use std::sync::{Arc, Mutex};

    struct Drive {
        clk: SignalRef,
        data: SignalRef,
        q: SignalRef,
        last: bool,
        finish: bool,
        seen: Arc<Mutex<Vec<(u64, u64)>>>,
    }

    impl<B: SimBackend> SlotObserver<B> for Drive {
        fn before_domains(&mut self, io: &mut SlotIo<'_, B>, time: u64) {
            let high = io.get(self.clk) == 1u8.into();
            if high && !self.last {
                self.seen
                    .lock()
                    .unwrap()
                    .push((time, u64::try_from(io.get(self.q)).unwrap()));
                io.set(self.data, 17u32);
                if self.finish {
                    io.request_finish();
                }
            }
            self.last = high;
        }
        fn end_of_slot(&mut self, _: &mut SlotIo<'_, B>, _: u64) {}
    }

    fn check<B: SimBackend + 'static>(
        mut sim: Simulation<B>,
        mode: SchedulerMode,
        finish: bool,
        expected: u8,
        prepare: impl FnOnce(&mut B),
    ) {
        let clk = sim.signal("clk");
        let seen = Arc::new(Mutex::new(Vec::new()));
        sim.observe_slots(
            Box::new(Drive {
                clk,
                data: sim.signal("d"),
                q: sim.signal("q"),
                last: false,
                finish,
                seen: seen.clone(),
            }),
            &[clk],
        );
        sim.run_until(1).unwrap();
        prepare(&mut sim.simulator.backend);
        let before = sim.scheduler_stats();
        sim.run_until(6).unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![(5, 0)], "{mode:?}");
        assert_eq!(
            sim.get(sim.signal("q")),
            if finish { 0u8 } else { expected }.into()
        );
        assert_eq!(
            sim.get(sim.signal("wakes")),
            if finish { 0u8 } else { 1u8 }.into(),
            "backend={} mode={mode:?} finish={finish} expected={expected} stats={:?}",
            std::any::type_name::<B>(),
            sim.scheduler_stats()
        );
        if finish {
            assert!(
                sim.simulator.host_watch.has_log(),
                "finished callback writes remain pending"
            );
        }
        if mode == SchedulerMode::Compiled {
            let after = sim.scheduler_stats();
            assert_eq!(after.reference_steps, before.reference_steps, "{after:?}");
            assert!(after.compiled_slots > before.compiled_slots);
        }
    }

    #[test]
    fn delayed_nba_observer_writes_precede_native_domains() {
        for expression in ["d", "d * 3 + 1"] {
            let source = format!(
                r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    int d = 0, q = 0, wakes = 0;
    wire [31:0] input_value = {expression};
    initial begin #1; clk = 0; clk <= #4 1; end
    always_ff @(posedge clk) q <= input_value;
    initial forever begin @(d); wakes++; end
endmodule"#
            );
            let expected = if expression == "d" { 17 } else { 52 };
            for four_state in [false, true] {
                for finish in [false, true] {
                    for mode in [SchedulerMode::Reference, SchedulerMode::Compiled] {
                        let make = || {
                            Simulation::from_sv_sources(
                                vec![(
                                    source.as_str(),
                                    std::path::Path::new("native-slot-observer.sv"),
                                )],
                                "Top",
                            )
                            .four_state(four_state)
                            .scheduler_mode(mode)
                        };
                        check(
                            make().build_native().unwrap(),
                            mode,
                            finish,
                            expected,
                            |_| {},
                        );
                        let sim = make().hotspot_jit(true).build_tiered().unwrap();
                        check(sim, mode, finish, expected, |backend| {
                            if mode == SchedulerMode::Compiled {
                                backend.publish_scheduler_for_test();
                            }
                        });
                    }
                }
            }
        }
    }
    struct Feedback {
        data: SignalRef,
        calls: usize,
    }

    impl<B: SimBackend> SlotObserver<B> for Feedback {
        fn before_domains(&mut self, io: &mut SlotIo<'_, B>, time: u64) {
            if time == 5 {
                self.calls += 1;
                assert!(self.calls < 16, "callback loop escaped the native budget");
                io.set(self.data, self.calls as u32);
            }
        }
        fn end_of_slot(&mut self, _: &mut SlotIo<'_, B>, _: u64) {}
    }

    #[test]
    fn callback_feedback_obeys_the_native_round_budget() {
        let source = r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    int d = 0, q = 0, wakes = 0;
    initial begin #1; clk = 0; clk <= #4 1; end
    always_ff @(posedge clk) q <= d;
    initial forever begin @(d); wakes++; end
endmodule"#;
        let mut sim = Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("native-observer-budget.sv"))],
            "Top",
        )
        .scheduler_mode(SchedulerMode::Compiled)
        .build_native()
        .unwrap();
        let clk = sim.signal("clk");
        sim.observe_slots(
            Box::new(Feedback {
                data: sim.signal("d"),
                calls: 0,
            }),
            &[clk],
        );
        sim.run_until(1).unwrap();
        sim.state.set_slot_budgets(3, 64);
        let before = sim.scheduler_stats().reference_steps;
        let error = sim.run_until(6).unwrap_err().to_string();
        assert!(error.contains("does not settle"), "{error}");
        assert_eq!(sim.scheduler_stats().reference_steps, before);
    }
    /// Writes `data` anew at every update of time 5.
    struct UpdateFeedback {
        data: SignalRef,
        calls: usize,
    }

    impl<B: SimBackend> SlotObserver<B> for UpdateFeedback {
        fn before_domains(&mut self, _: &mut SlotIo<'_, B>, _: u64) {}
        fn end_of_slot(&mut self, _: &mut SlotIo<'_, B>, _: u64) {}
        fn observes_updates(&self) -> bool {
            true
        }
        fn after_update(&mut self, io: &mut SlotIo<'_, B>, time: u64) {
            if time == 5 {
                self.calls += 1;
                assert!(self.calls < 64, "callback loop escaped the native budget");
                io.set(self.data, self.calls as u32);
            }
        }
    }

    #[test]
    fn update_callback_feedback_obeys_the_native_round_budget() {
        let source = r#"`timescale 1ns/1ns
module Top;
    int d = 0, x = 0;
    wire [31:0] e = d + 1;
    initial #5 x = 1;
endmodule"#;
        for mode in [SchedulerMode::Reference, SchedulerMode::Compiled] {
            let mut sim = Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("native-update-budget.sv"))],
                "Top",
            )
            .scheduler_mode(mode)
            .observable_updates(true)
            .build_native()
            .unwrap();
            let data = sim.signal("d");
            sim.observe_slots(Box::new(UpdateFeedback { data, calls: 0 }), &[data]);
            sim.run_until(1).unwrap();
            sim.state.set_slot_budgets(3, 16);
            let error = sim.run_until(6).unwrap_err().to_string();
            assert!(error.contains("does not settle"), "{mode:?}: {error}");
        }
    }

    fn host_clock<B: SimBackend + 'static>(
        mut sim: Simulation<B>,
        mode: SchedulerMode,
        finish: bool,
        prepare: impl FnOnce(&mut B),
    ) {
        let clk = sim.signal("clk");
        let seen = Arc::new(Mutex::new(Vec::new()));
        sim.modify(|io| io.set(clk, 0u8)).unwrap();
        sim.observe_slots(
            Box::new(Drive {
                clk,
                data: sim.signal("d"),
                q: sim.signal("q"),
                last: false,
                finish,
                seen: seen.clone(),
            }),
            &[clk],
        );
        sim.run_until(1).unwrap();
        prepare(&mut sim.simulator.backend);
        let before = sim.scheduler_stats();
        sim.modify(|io| io.set(clk, 1u8)).unwrap();
        sim.run_until(5).unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![(1, 0)]);
        assert_eq!(
            sim.get(sim.signal("q")),
            if finish { 0u8 } else { 52u8 }.into()
        );
        assert_eq!(
            sim.get(sim.signal("wakes")),
            if finish { 0u8 } else { 1u8 }.into()
        );
        if finish {
            assert!(sim.simulator.host_watch.has_log());
        }
        if mode == SchedulerMode::Compiled {
            let after = sim.scheduler_stats();
            assert_eq!(after.reference_steps, before.reference_steps, "{after:?}");
            assert!(after.compiled_slots > before.compiled_slots);
        }
    }

    #[test]
    fn host_observed_clock_writes_precede_native_domains() {
        let source = r#"`timescale 1ns/1ns
module Top(input bit clk);
    int d = 0, q = 0, wakes = 0;
    wire [31:0] input_value = d * 3 + 1;
    always_ff @(posedge clk) q <= input_value;
    initial forever begin @(d); wakes++; end
endmodule"#;
        for four_state in [false, true] {
            for finish in [false, true] {
                for mode in [SchedulerMode::Reference, SchedulerMode::Compiled] {
                    let make = || {
                        Simulation::from_sv_sources(
                            vec![(source, std::path::Path::new("native-host-observer.sv"))],
                            "Top",
                        )
                        .four_state(four_state)
                        .scheduler_mode(mode)
                    };
                    host_clock(make().build_native().unwrap(), mode, finish, |_| {});
                    host_clock(
                        make().hotspot_jit(true).build_tiered().unwrap(),
                        mode,
                        finish,
                        |backend| {
                            if mode == SchedulerMode::Compiled {
                                backend.publish_scheduler_for_test();
                            }
                        },
                    );
                }
            }
        }
    }
}
