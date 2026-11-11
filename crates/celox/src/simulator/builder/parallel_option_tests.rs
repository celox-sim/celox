use crate::{OptLevel, OptimizeOptions, ParallelPartition};

#[test]
fn optimization_setters_keep_thread_and_partition_requests() {
    let builders = [
        crate::Simulator::builder("", "Top")
            .threads(4)
            .parallel_partition(ParallelPartition::Always)
            .opt_level(OptLevel::O2),
        crate::Simulator::builder("", "Top")
            .threads(4)
            .parallel_partition(ParallelPartition::Always)
            .optimize(false),
        crate::Simulator::builder("", "Top")
            .threads(4)
            .parallel_partition(ParallelPartition::Always)
            .optimize_options(OptimizeOptions::default()),
    ];
    for builder in builders {
        let options = builder.optimize_options_for_test();
        assert_eq!(options.parallel_lanes(), 4);
        assert_eq!(options.parallel_partition(), ParallelPartition::Always);
    }
}
#[cfg(all(
    feature = "systemverilog",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[test]
fn native_feedback_work_lists_converge_or_report_the_slot_bound() {
    for four_state in [false, true] {
        for limit in [Some(8), None] {
            let body = limit.map_or("b = ~b;".to_string(), |limit| {
                format!("if (count < {limit}) begin count = count + 1; b = ~b; end")
            });
            let other = limit.map_or("a = ~a;".to_string(), |limit| {
                format!("if (count < {limit}) begin count = count + 1; a = ~a; end")
            });
            let source = format!(
                r#"`timescale 1ns/1ns
module Top;
    bit a = 0, b = 0;
    int count = 0;
    initial begin #1; a = 1; #20; end
    initial forever begin @(a); {body} end
    initial forever begin @(b); {other} end
endmodule"#
            );
            // Ordinary timed activations can outnumber the round limit
            // without consuming any non-suspending returns.
            let source =
                if limit.is_some() {
                    let independent: String = (0..20).map(|index| format!(
                        "int extra_{index} = 0; initial begin #1; extra_{index} = 1; #20; end\n"
                    )).collect();
                    source.replace("endmodule", &format!("{independent}endmodule"))
                } else {
                    source
                };
            for level in [crate::OptLevel::O0, crate::OptLevel::O2] {
                for mode in [
                    crate::SchedulerMode::Reference,
                    crate::SchedulerMode::Compiled,
                ] {
                    // The unbounded reference work list is deliberately not
                    // run: only the native path adds a work-list guard.
                    if limit.is_none() && mode == crate::SchedulerMode::Reference {
                        continue;
                    }
                    let mut sim = crate::Simulation::from_sv_sources(
                        vec![(source.as_str(), std::path::Path::new("native-feedback.sv"))],
                        "Top",
                    )
                    .four_state(four_state)
                    .opt_level(level)
                    .scheduler_mode(mode)
                    .build_native()
                    .unwrap();
                    sim.state.set_slot_budgets(16, 0);
                    if limit.is_some() {
                        sim.run_until(10).unwrap();
                        assert_eq!(sim.get(sim.signal("count")), 8u8.into());
                        assert_eq!(sim.get(sim.signal("extra_19")), 1u8.into());
                        if mode == crate::SchedulerMode::Compiled
                            && cfg!(all(target_arch = "x86_64", not(feature = "arm64-codegen")))
                        {
                            assert_eq!(sim.scheduler_stats().native_service_calls, 0);
                        }
                    } else {
                        let error = sim.run_until(10).unwrap_err();
                        assert!(error.to_string().contains("does not settle"), "{error}");
                        assert_eq!(sim.time(), 1);
                        assert!(error.to_string().contains("native-feedback.sv"), "{error}");
                        if cfg!(all(target_arch = "x86_64", not(feature = "arm64-codegen"))) {
                            assert_eq!(sim.scheduler_stats().native_service_calls, 1);
                        }
                    }
                    if mode == crate::SchedulerMode::Compiled {
                        assert!(
                            sim.scheduler_stats().compiled_slots > 0,
                            "{:?}",
                            sim.scheduler_stats()
                        );
                    }
                }
            }
        }
    }
}
