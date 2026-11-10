use crate::{KernelSchedule, SchedulerMode, Simulation};

#[test]
fn queued_nbas_reuse_native_storage_without_host_scheduling() {
    for body in [
        "repeat (130) begin q <= q + 1; q <= q + 3; end",
        "q <= 73'h123456789abcdef0123; q <= 73'bx; q <= 73'bz; q[68:3] <= ticks;",
    ] {
        let source = format!(
            r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    logic [72:0] q = 0;
    int ticks = 0;
    int wakes = 0;
    always #1 clk = !clk;
    initial forever begin
        @(posedge clk); ticks++;
        {body}
    end
    initial forever begin @(q); wakes++; end
endmodule"#
        );
        for (four_state, hotspot) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut sim = Simulation::from_sv_sources(
                vec![(
                    source.as_str(),
                    std::path::Path::new("native-nba-storage.sv"),
                )],
                "Top",
            )
            .four_state(four_state)
            .hotspot_jit(hotspot)
            .scheduler_mode(SchedulerMode::DifferentialRuns)
            .build_scheduler_test()
            .unwrap();
            sim.run_until(4)
                .unwrap_or_else(|e| panic!("four_state={four_state}, hotspot={hotspot}: {e:?}"));
            let before = sim.scheduler_stats();
            assert!(
                before
                    .kernels
                    .iter()
                    .all(|k| *k == KernelSchedule::Compiled),
                "{before:?}"
            );
            for end in [5, 6, 9, 14, 20] {
                sim.run_until(end).unwrap();
            }
            let after = sim.scheduler_stats();
            assert_eq!(after.reference_steps, before.reference_steps, "{after:?}");
            assert_eq!(
                after.native_service_calls, before.native_service_calls,
                "a warmed NBA queue must not call the host: {after:?}"
            );
            assert!(
                after.compiled_slots > before.compiled_slots && after.checked_steps > 0,
                "{after:?}"
            );
            assert_eq!(sim.get(sim.signal("ticks")), 10u8.into());
        }
    }
}

#[test]
fn unfinished_native_nbas_are_exported_on_budget_failure() {
    let source = r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    logic [72:0] q = 0;
    always #1 clk = !clk;
    initial forever begin
        @(posedge clk);
        repeat (12) begin q <= 73'bx; q <= 73'h123456789abcdef0123; end
    end
endmodule"#;
    for four_state in [false, true] {
        let mut pending = Vec::new();
        for mode in [SchedulerMode::Reference, SchedulerMode::Compiled] {
            let mut sim = Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("native-nba-budget.sv"))],
                "Top",
            )
            .four_state(four_state)
            .scheduler_mode(mode)
            .build_native()
            .unwrap();
            sim.run_until(0).unwrap();
            sim.state.set_slot_budgets(32, 4);
            let before = sim.scheduler_stats().reference_steps;
            let error = sim.run_until(2).unwrap_err();
            assert!(error.to_string().contains("budget"), "{error}");
            if mode == SchedulerMode::Compiled {
                assert_eq!(sim.scheduler_stats().reference_steps, before);
            }
            let state = sim.save_state().unwrap();
            let queued = state.schedule.unwrap().processes.nba_queue;
            assert!(!queued.is_empty(), "the interrupted update must survive");
            pending.push(queued);
        }
        assert_eq!(pending[0], pending[1]);
    }
}

#[test]
fn dynamic_and_strided_nba_destinations_remain_compiled() {
    for updates in [
        "bits[(ticks * 7) % 89 +: 13] <= ticks; bits[(ticks * 7) % 89 +: 13] <= 'x;",
        "words[ticks % 4] <= ticks; words[ticks % 4][7:3] <= 'z;",
        "words[0] <= ticks; words[1] <= ticks+1; words[2] <= 'x;",
        "for (int j=0; j<3; j++) words[j] <= ticks + j;",
    ] {
        let source = format!(
            r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    logic [88:0] bits = 0;
    logic [12:0] words [3];
    int ticks = 0;
    int observed = 0;
    initial begin words[0]=0; words[1]=0; words[2]=0; end
    always #1 clk = !clk;
    initial forever begin @(posedge clk); ticks++; {updates} end
    initial forever begin @(bits or words[0] or words[1] or words[2]); observed++; end
endmodule"#
        );
        for (four_state, hotspot) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut sim = Simulation::from_sv_sources(
                vec![(
                    source.as_str(),
                    std::path::Path::new("native-dynamic-nba.sv"),
                )],
                "Top",
            )
            .four_state(four_state)
            .hotspot_jit(hotspot)
            .scheduler_mode(SchedulerMode::DifferentialRuns)
            .build_scheduler_test()
            .unwrap();
            sim.run_until(4)
                .unwrap_or_else(|e| panic!("four_state={four_state}, hotspot={hotspot}: {e:?}"));
            let before = sim.scheduler_stats();
            assert!(
                before
                    .kernels
                    .iter()
                    .all(|k| *k == KernelSchedule::Compiled),
                "{before:?}"
            );
            for end in [7, 12, 21, 40] {
                sim.run_until(end).unwrap();
            }
            let after = sim.scheduler_stats();
            assert_eq!(after.reference_steps, before.reference_steps, "{after:?}");
            assert_eq!(
                after.native_service_calls, before.native_service_calls,
                "{after:?}"
            );
            assert!(after.checked_steps > 0, "{after:?}");
            assert_eq!(sim.get(sim.signal("ticks")), 20u8.into());
        }
    }
}

#[test]
fn full_width_nba_site_dispatch_keeps_every_queued_value() {
    let assignments = (0..300)
        .map(|n| format!("q <= {n};"))
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(
        r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    int q = 0;
    int seen = 0;
    always #1 clk = !clk;
    initial forever begin @(posedge clk); {assignments} end
    initial forever begin @(q); seen = q; end
endmodule"#
    );
    for four_state in [false, true] {
        let mut sim = Simulation::from_sv_sources(
            vec![(
                source.as_str(),
                std::path::Path::new("native-nba-many-sites.sv"),
            )],
            "Top",
        )
        .four_state(four_state)
        .scheduler_mode(SchedulerMode::DifferentialRuns)
        .build_native()
        .unwrap();
        sim.run_until(2).unwrap();
        let before = sim.scheduler_stats();
        sim.run_until(6).unwrap();
        let after = sim.scheduler_stats();
        assert_eq!(after.reference_steps, before.reference_steps, "{after:?}");
        assert_eq!(
            after.native_service_calls, before.native_service_calls,
            "{after:?}"
        );
        assert!(after.checked_steps > 0, "{after:?}");
        assert_eq!(sim.get(sim.signal("q")), 299u16.into());
        assert_eq!(sim.get(sim.signal("seen")), 299u16.into());
    }
}

#[test]
fn nba_clock_pulses_trigger_domains_without_rust_scheduling() {
    for body in [
        "pulse <= 1; pulse <= 0;",
        "pulse <= 'x; pulse <= 0; pulse <= 1; pulse <= 0;",
    ] {
        let source = format!(
            r#"`timescale 1ns/1ns
module Top;
 bit clk = 0;
 logic pulse = 0;
 int ticks = 0;
 int rises = 0;
 int falls = 0;
 always #1 clk = !clk;
 initial forever begin @(posedge clk); ticks++; {body} end
 always @(posedge pulse) rises <= rises + 1;
 always @(negedge pulse) falls <= falls + 1;
endmodule"#
        );
        for (four_state, hotspot) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut sim = Simulation::from_sv_sources(
                vec![(source.as_str(), std::path::Path::new("native-nba-clock.sv"))],
                "Top",
            )
            .four_state(four_state)
            .hotspot_jit(hotspot)
            .scheduler_mode(SchedulerMode::DifferentialRuns)
            .build_scheduler_test()
            .unwrap();
            sim.run_until(4)
                .unwrap_or_else(|e| panic!("four_state={four_state}, hotspot={hotspot}: {e:?}"));
            let before = sim.scheduler_stats();
            assert!(
                before
                    .kernels
                    .iter()
                    .all(|k| *k == KernelSchedule::Compiled),
                "{before:?}"
            );
            for end in [7, 12, 21, 40] {
                sim.run_until(end).unwrap();
            }
            let after = sim.scheduler_stats();
            assert_eq!(after.reference_steps, before.reference_steps, "{after:?}");
            assert!(after.checked_steps > 0, "{after:?}");
            assert_eq!(sim.get(sim.signal("ticks")), 20u8.into());
            assert!(sim.get(sim.signal("rises")) > 0u8.into());
        }
    }
}

#[test]
fn delayed_nbas_keep_native_time_order_across_state_restore() {
    let source = r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    logic [72:0] q = 0;
    logic [12:0] words [3];
    int ticks = 0;
    int changes = 0;
    initial begin words[0]=0; words[1]=0; words[2]=0; end
    always #1 clk = !clk;
    initial forever begin
        @(posedge clk); ticks++;
        repeat (70) begin
            q <= #3 ticks;
            q <= #1 'x;
            q <= #7 'z;
        end
        words[ticks % 3] <= #2 ticks;
        q[ticks % 60 +: 13] <= #5 ticks;
    end
    initial forever begin @(q); changes++; end
endmodule"#;
    for four_state in [false, true] {
        for hotspot in [false, true] {
            let make = |mode| {
                Simulation::from_sv_sources(
                    vec![(source, std::path::Path::new("native-delayed-nba.sv"))],
                    "Top",
                )
                .four_state(four_state)
                .hotspot_jit(hotspot)
                .scheduler_mode(mode)
                .build_scheduler_test()
                .unwrap()
            };
            let mut native = make(SchedulerMode::Compiled);
            let mut reference = make(SchedulerMode::Reference);
            native.run_until(0).unwrap();
            reference.run_until(0).unwrap();
            let before = native.scheduler_stats().reference_steps;
            for end in [1, 2, 4, 7, 12, 19] {
                native.run_until(end).unwrap();
                reference.run_until(end).unwrap();
                assert_eq!(
                    native.save_state().unwrap(),
                    reference.save_state().unwrap(),
                    "at {end}, four_state={four_state}, hotspot={hotspot}"
                );
                let saved = native.save_state().unwrap();
                native.load_state(&saved).unwrap();
            }
            assert_eq!(native.scheduler_stats().reference_steps, before);
            assert!(native.scheduler_stats().compiled_slots > 0);
        }
    }
}

#[test]
fn delayed_nba_can_be_the_only_remaining_native_work() {
    for setup in ["#1;", ""] {
        let source = format!(
            r#"`timescale 1ns/1ns
module Top;
    logic [72:0] q = 0;
    initial begin {setup} q <= #4 'x; q <= #2 'z; q <= #4 7; end
endmodule"#
        );
        for four_state in [false, true] {
            let make = |mode| {
                Simulation::from_sv_sources(
                    vec![(
                        source.as_str(),
                        std::path::Path::new("native-delayed-only.sv"),
                    )],
                    "Top",
                )
                .four_state(four_state)
                .scheduler_mode(mode)
                .build_native()
                .unwrap()
            };
            let mut native = make(SchedulerMode::Compiled);
            let mut reference = make(SchedulerMode::Reference);
            native.run_until(0).unwrap();
            reference.run_until(0).unwrap();
            let before = native.scheduler_stats().reference_steps;
            for end in 1..=7 {
                native.run_until(end).unwrap();
                reference.run_until(end).unwrap();
                assert_eq!(
                    native.save_state().unwrap(),
                    reference.save_state().unwrap(),
                    "setup={setup} at {end}"
                );
            }
            assert_eq!(native.scheduler_stats().reference_steps, before);
            assert_eq!(native.get(native.signal("q")), 7u8.into());
        }
    }
}

#[test]
fn native_delayed_nba_reports_timestamp_overflow() {
    let source = r#"`timescale 1ns/1ns
module Top;
    bit q;
    initial begin #9223372036854775808; q <= #9223372036854775808 1; end
endmodule"#;
    for mode in [SchedulerMode::Reference, SchedulerMode::Compiled] {
        let mut sim = Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("native-nba-time-overflow.sv"))],
            "Top",
        )
        .scheduler_mode(mode)
        .build_native()
        .unwrap();
        sim.run_until(0).unwrap();
        let before = sim.scheduler_stats().reference_steps;
        let error = sim.run_until(u64::MAX).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("nonblocking assignment delay overflows simulation time"),
            "{error}"
        );
        if mode == SchedulerMode::Compiled {
            assert_eq!(sim.scheduler_stats().reference_steps, before);
        }
    }
}

#[test]
fn restored_ready_processes_enter_the_native_active_region() {
    let source = r#"`timescale 1ns/1ns
module Top;
 bit trigger = 0;
 int q = 0, seen = 0;
 initial begin #1; forever begin @(trigger); q <= q + 1; end end
 initial begin #1; forever begin @(trigger); seen = q; end end
endmodule"#;
    for four_state in [false, true] {
        for hotspot in [false, true] {
            let make = |mode| {
                Simulation::from_sv_sources(
                    vec![(source, std::path::Path::new("native-ready-restore.sv"))],
                    "Top",
                )
                .four_state(four_state)
                .hotspot_jit(hotspot)
                .scheduler_mode(mode)
                .build_scheduler_test()
                .unwrap()
            };
            let mut reference = make(SchedulerMode::Reference);
            reference.run_until(2).unwrap();
            let mut saved = reference.save_state().unwrap();
            let processes = &mut saved.schedule.as_mut().unwrap().processes;
            assert_eq!(processes.waiters.len(), 2);
            processes
                .ready
                .extend(processes.waiters.drain(..).map(|waiter| waiter.process));
            reference.load_state(&saved).unwrap();
            let mut native = make(SchedulerMode::Compiled);
            native.load_state(&saved).unwrap();
            let before = native.scheduler_stats().reference_steps;
            reference.run_until(2).unwrap();
            native.run_until(2).unwrap();
            assert_eq!(
                native.save_state().unwrap(),
                reference.save_state().unwrap()
            );
            assert_eq!(native.scheduler_stats().reference_steps, before);
            assert_eq!(native.get(native.signal("q")), 1u8.into());
            assert_eq!(native.get(native.signal("seen")), 0u8.into());
        }
    }
}

#[test]
fn restored_ready_delay_only_process_rearms_future_wakeups() {
    fn check<B: crate::backend::SimBackend>(
        mut reference: Simulation<B>,
        mut compiled: Simulation<B>,
        publish: impl FnOnce(&mut B),
    ) {
        reference.run_until(0).unwrap();
        let mut saved = reference.save_state().unwrap();
        let schedule = saved.schedule.as_mut().unwrap();
        // Restore a timestamp whose due wakeup has already moved to Active,
        // but whose process has not run yet.
        assert_eq!(schedule.processes.wakeups.len(), 1);
        let (process, time) = schedule.processes.wakeups.pop().unwrap();
        assert_eq!(time, 2);
        schedule.time = time;
        schedule.processes.ready.push(process);
        reference.load_state(&saved).unwrap();
        compiled.load_state(&saved).unwrap();
        publish(&mut compiled.simulator.backend);
        let before = compiled.scheduler_stats();
        assert!(
            before
                .kernels
                .iter()
                .all(|k| *k == KernelSchedule::Compiled)
        );

        for end in [2, 4, 6, 10] {
            reference.run_until(end).unwrap();
            compiled.run_until(end).unwrap();
            assert_eq!(compiled.get(compiled.signal("q")), (end as u32 / 2).into());
            let saved = compiled.save_state().unwrap();
            let schedule = saved.schedule.as_ref().unwrap();
            assert_eq!(schedule.processes.wakeups.len(), 1, "time={end}");
            assert_eq!(schedule.processes.wakeups[0].1, end + 2);
            assert!(schedule.processes.ready.is_empty());
            assert_eq!(saved, reference.save_state().unwrap(), "time={end}");
            let after = compiled.scheduler_stats();
            assert_eq!(after.reference_steps, before.reference_steps, "{after:?}");
            assert!(after.compiled_slots > before.compiled_slots);
            if end == 4 {
                // The next delay must survive another save/load as well as
                // consecutive native runs.
                compiled.load_state(&saved).unwrap();
                reference.load_state(&saved).unwrap();
            }
        }
    }

    let source = r#"`timescale 1ns/1ns
module Top;
 int q = 0;
 initial forever begin #2; q++; end
endmodule"#;
    for four_state in [false, true] {
        let make = |mode| {
            Simulation::from_sv_sources(
                vec![(
                    source,
                    std::path::Path::new("native-ready-delay-restore.sv"),
                )],
                "Top",
            )
            .four_state(four_state)
            .scheduler_mode(mode)
        };
        check(
            make(SchedulerMode::Reference).build_native().unwrap(),
            make(SchedulerMode::Compiled).build_native().unwrap(),
            |_| {},
        );
        check(
            make(SchedulerMode::Reference)
                .hotspot_jit(true)
                .build_tiered()
                .unwrap(),
            make(SchedulerMode::Compiled)
                .hotspot_jit(true)
                .build_tiered()
                .unwrap(),
            |backend| backend.publish_scheduler_for_test(),
        );
    }
}

#[test]
fn hotspot_scheduler_replaces_its_budgeted_stage_with_inlined_kernels() {
    // Kernels large enough for a budgeted first stage to call them.
    let mut source = String::from(
        "`timescale 1ns/1ns\nmodule Top;\n    bit clk = 0;\n    always #1 clk = !clk;\n",
    );
    for index in 0..6 {
        let previous = (index + 5) % 6;
        source += &format!(
            "    logic [63:0] r{index} = {index};\n    initial forever begin\n        @(posedge clk);\n"
        );
        for step in 0..40 {
            source += &format!(
                "        if (r{index}[{bit}]) r{index} = r{index} * 3 + {step}; else r{index} = r{index} ^ (r{index} >> 3);\n",
                bit = step % 64
            );
        }
        source += &format!("        r{index} <= #1 r{index} + r{previous};\n    end\n");
    }
    source += "endmodule\n";
    let names: Vec<_> = (0..6).map(|index| format!("r{index}")).collect();
    for four_state in [false, true] {
        let make = |mode| {
            Simulation::from_sv_sources(
                vec![(source.as_str(), std::path::Path::new("staged-scheduler.sv"))],
                "Top",
            )
            .four_state(four_state)
            .hotspot_jit(true)
            .scheduler_mode(mode)
            .build_tiered()
            .unwrap()
        };
        let mut reference = make(SchedulerMode::Reference);
        let mut staged = make(SchedulerMode::Compiled);
        staged
            .simulator
            .backend
            .interp_for_test()
            .set_hotspot_inline_budget_for_test(0);
        staged.simulator.backend.publish_scheduler_for_test();
        let compare = |reference: &mut Simulation<_>, staged: &mut Simulation<_>, end| {
            reference.run_until(end).unwrap();
            staged.run_until(end).unwrap();
            for name in &names {
                assert_eq!(
                    staged.get_four_state(staged.signal(name)),
                    reference.get_four_state(reference.signal(name)),
                    "{name} at {end}, four_state={four_state}"
                );
            }
        };
        compare(&mut reference, &mut staged, 21);
        let first = staged.scheduler_stats();
        assert!(first.compiled_slots > 0, "{first:?}");
        staged
            .simulator
            .backend
            .interp_for_test()
            .wait_for_scheduler_stages_for_test(2);
        compare(&mut reference, &mut staged, 64);
        let last = staged.scheduler_stats();
        assert!(last.compiled_slots > first.compiled_slots, "{last:?}");
        assert_eq!(last.reference_steps, first.reference_steps, "{last:?}");
    }
}

/// A fused flip-flop whose delayed nonblocking update would be due past
/// the last timestamp reports the overflow in every executor, as the
/// queued kernel path does; it does not schedule the update at the last
/// timestamp instead.
#[test]
fn fused_delayed_nba_reports_timestamp_overflow() {
    let source = r#"`timescale 1ns/1ns
module Top(output logic [7:0] q);
    bit clk = 0;
    always #5 clk = ~clk;
    always @(posedge clk) q <= #18446744073709551615 8'h11;
endmodule"#;
    let builder = || {
        Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("fused-nba-time-overflow.sv"))],
            "Top",
        )
    };
    let check = |name: &str, result: Result<(), crate::RuntimeErrorCode>| {
        let error = result.expect_err(name);
        assert!(
            error
                .to_string()
                .contains("nonblocking assignment delay overflows simulation time"),
            "{name}: {error}"
        );
    };
    for mode in [SchedulerMode::Reference, SchedulerMode::Compiled] {
        let mut native = builder().scheduler_mode(mode).build_native().unwrap();
        check("native", native.run_until(u64::MAX));
        let mut cranelift = builder().scheduler_mode(mode).build_cranelift().unwrap();
        check("cranelift", cranelift.run_until(u64::MAX));
        let mut interpreter = builder().scheduler_mode(mode).build_interpreter().unwrap();
        check("interpreter", interpreter.run_until(u64::MAX));
        let mut tiered = builder().scheduler_mode(mode).build_tiered().unwrap();
        check("tiered", tiered.run_until(u64::MAX));
    }
}

/// An intra-assignment delay on a nonblocking assignment does not suspend
/// its process (IEEE 1800-2023 9.4.5), so `always_ff` takes it as a plain
/// `always` does: the right-hand side is read at the edge and the update
/// lands at its due time.
#[test]
fn always_ff_takes_delayed_nonblocking_assignments() {
    // A whole-variable target fuses into the clock domain; a part-select
    // keeps the queued path.
    for (keyword, target) in [
        ("always", "q"),
        ("always_ff", "q"),
        ("always", "q[7:0]"),
        ("always_ff", "q[7:0]"),
    ] {
        let source = format!(
            r#"`timescale 1ns/1ns
module Top(output logic [7:0] q);
    bit clk = 0;
    logic [7:0] d = 8'h11;
    always #5 clk = ~clk;
    {keyword} @(posedge clk) {target} <= #1 d;
    initial begin
        #12 d = 8'h22;
        #20 $finish;
    end
endmodule"#
        );
        for mode in [SchedulerMode::Reference, SchedulerMode::Compiled] {
            let mut sim = Simulation::from_sv_sources(
                vec![(source.as_str(), std::path::Path::new("always-ff-delay.sv"))],
                "Top",
            )
            .scheduler_mode(mode)
            .build_native()
            .unwrap_or_else(|error| panic!("{keyword} {target}: {error}"));
            // The edge at 5 reads 8'h11 and updates at 6; the edge at 15
            // reads 8'h22 and updates at 16.
            sim.run_until(5).unwrap();
            assert_eq!(sim.get(sim.signal("q")), 0u8.into(), "{keyword} {target}");
            sim.run_until(6).unwrap();
            assert_eq!(
                sim.get(sim.signal("q")),
                0x11u8.into(),
                "{keyword} {target}"
            );
            sim.run_until(15).unwrap();
            assert_eq!(
                sim.get(sim.signal("q")),
                0x11u8.into(),
                "{keyword} {target}"
            );
            sim.run_until(16).unwrap();
            assert_eq!(
                sim.get(sim.signal("q")),
                0x22u8.into(),
                "{keyword} {target}"
            );
        }
    }
}
