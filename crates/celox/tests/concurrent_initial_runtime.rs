use celox::testbench::{
    compile_initial_testbench, run_compiled_testbench, run_compiled_testbench_to_finish,
    run_compiled_testbench_with_tick_limit,
};
use celox::{Simulator, TestResult};

#[test]
fn later_process_failure_is_not_hidden_by_an_earlier_wait() {
    let code = r#"#[test(Top)] module Top {
        inst clk: $tb::clock_gen;
        initial { clk.next(4); $finish(); }
        initial { clk.next(1); $assert(1'd0, "worker failure"); }
    }"#;
    let mut sim = Simulator::builder(code, "Top").build_interpreter().unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();
    assert_eq!(
        run_compiled_testbench(&mut sim, &tb),
        TestResult::Fail("worker failure".into())
    );
}

#[test]
fn finish_stops_processes_that_have_not_started() {
    let code = r#"#[test(Top)] module Top {
        initial { $finish(); }
        initial { $assert(1'd0); }
    }"#;
    let mut sim = Simulator::builder(code, "Top").build_interpreter().unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();
    assert_eq!(
        run_compiled_testbench_to_finish(&mut sim, &tb),
        TestResult::Pass
    );
}

#[test]
fn all_processes_falling_through_is_not_explicit_completion() {
    let code = r#"#[test(Top)] module Top {
        inst clk: $tb::clock_gen;
        initial { clk.next(1); }
        initial { clk.next(2); }
    }"#;
    let mut sim = Simulator::builder(code, "Top").build_interpreter().unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();
    let result = run_compiled_testbench_to_finish(&mut sim, &tb);
    assert!(
        matches!(result, TestResult::Fail(message) if message.contains("without reaching $finish"))
    );
}

#[test]
fn tick_limit_counts_shared_edges_once() {
    let code = include_str!(
        "../../celox-test-suite-veryl/fixtures/testbench/concurrent_initial_shared_clock.veryl"
    );
    for limit in [0, 1, 2] {
        let mut sim = Simulator::builder(code, "Top").build_interpreter().unwrap();
        let tb = compile_initial_testbench(&sim).unwrap();
        let result = run_compiled_testbench_with_tick_limit(&mut sim, &tb, limit);
        assert_eq!(result.ticks, limit);
        assert!(result.tick_limit_reached);
        assert_eq!(result.result, TestResult::Pass);
    }
}

#[test]
fn detailed_assertions_keep_distinct_sites_across_processes_and_instances() {
    let code = r#"
        #[test(Child)] module Child {
            initial { $assert_continue(1'd0, "child"); }
        }
        #[test(Top)] module Top {
            inst clk: $tb::clock_gen;
            inst child: Child;
            initial { clk.next(2); $assert(1'd1, "last"); $finish(); }
            initial { clk.next(1); $assert_continue(1'd0, "worker"); }
        }
    "#;
    let result = Simulator::builder(code, "Top").run_test_detailed().unwrap();
    assert!(!result.passed);
    assert_eq!(
        result
            .assertions
            .iter()
            .map(|a| (a.passed, a.message.as_deref()))
            .collect::<Vec<_>>(),
        [
            (false, Some("child")),
            (false, Some("worker")),
            (true, Some("last"))
        ]
    );
}

#[test]
fn child_initial_runs_without_a_root_initial() {
    let code = r#"
        #[test(Child)] module Child { initial { $assert(1'd1); $finish(); } }
        #[test(Top)] module Top { inst child: Child; }
    "#;
    let mut sim = Simulator::builder(code, "Top").build_interpreter().unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();
    assert_eq!(
        run_compiled_testbench_to_finish(&mut sim, &tb),
        TestResult::Pass
    );
}

#[test]
fn suspended_wide_loop_keeps_the_existing_progress_guard() {
    let code = r#"#[test(Top)] module Top {
        inst clk: $tb::clock_gen;
        var bound: logic<128>;
        initial { clk.next(4); $finish(); }
        initial {
            bound = (128'd1 << 100);
            for _i in bound..=bound step *= 2 { clk.next(1); }
        }
    }"#;
    let mut sim = Simulator::builder(code, "Top").build_interpreter().unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();
    assert!(
        matches!(run_compiled_testbench(&mut sim, &tb), TestResult::Fail(message)
        if message.contains("non-progressing stepped for loop"))
    );
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn native_image_roundtrip_preserves_concurrent_processes_and_periods() {
    for code in [
        include_str!(
            "../../celox-test-suite-veryl/fixtures/testbench/concurrent_initial_shared_clock.veryl"
        ),
        include_str!(
            "../../celox-test-suite-veryl/fixtures/testbench/concurrent_initial_clock_periods.veryl"
        ),
        include_str!(
            "../../celox-test-suite-veryl/fixtures/testbench/concurrent_initial_hierarchy.veryl"
        ),
        include_str!(
            "../../celox-test-suite-veryl/fixtures/testbench/concurrent_initial_reset_between_edges.veryl"
        ),
        include_str!(
            "../../celox-test-suite-veryl/fixtures/testbench/concurrent_initial_reset_only_clock.veryl"
        ),
    ] {
        let original = Simulator::builder(code, "Top").build_native().unwrap();
        let bytes = original
            .shared_code()
            .program_image()
            .to_container_bytes()
            .unwrap();
        let image = celox::NativeProgramImage::from_container_bytes(&bytes).unwrap();
        let mut restored = Simulator::builder("deliberately invalid source", "unused")
            .build_native_from_image(image)
            .unwrap();
        let tb = compile_initial_testbench(&restored).unwrap();
        assert!(tb.processes().count() >= 2);
        assert_eq!(
            run_compiled_testbench_to_finish(&mut restored, &tb),
            TestResult::Pass
        );
    }
}

#[test]
fn readmem_after_another_process_finish_is_prepared_and_runs_at_its_wait() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("words.hex");
    std::fs::write(&path, "ab\ncd\n").unwrap();
    let code = format!(
        r#"
        module Memory {{
            #[allow(unassign_variable)]
            var words: logic<8>[2];
        }}
        #[test(Top)] module Top {{
            inst clk: $tb::clock_gen;
            inst dut: Memory;
            initial {{
                $assert(dut.words[0] == 0);
                clk.next(2);
                $assert(dut.words[0] == 8'hab);
                $assert(dut.words[1] == 8'hcd);
                $finish();
            }}
            initial {{ clk.next(1); $readmemh("{}", dut.words); }}
        }}
    "#,
        path.display()
    );
    let mut sim = Simulator::builder(&code, "Top")
        .build_interpreter()
        .unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();
    assert_eq!(
        run_compiled_testbench_to_finish(&mut sim, &tb),
        TestResult::Pass
    );
}

#[test]
fn concurrent_helper_calls_preserve_each_calls_arguments_across_waits() {
    let code = r#"#[test(Top)] module Top {
        inst clk: $tb::clock_gen;
        var a: u32;
        var b: u32;
        #[allow(multiple_assign)]
        var total: u32;
        function add_later(value: input u32) {
            let saved: u32 = value;
            clk.next(1);
            total += saved;
        }
        initial { a = 2; add_later(a); }
        initial { b = 5; add_later(b); }
        initial { clk.next(2); $assert(total == 7, "total=%d", total); $finish(); }
    }"#;
    fn check<B: celox::SimBackend>(mut sim: Simulator<B>) {
        let tb = compile_initial_testbench(&sim).unwrap();
        assert!(!tb.private_signals().is_empty());
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass
        );
    }
    let array_code = code
        .replace(
            "let saved: u32 = value;",
            "var saved: logic<32>[2]; saved[0] = value; saved[1] = value + 1;",
        )
        .replace("total += saved;", "total += saved[1];")
        .replace("total == 7", "total == 9");
    for code in [code, &array_code] {
        for four_state in [false, true] {
            check(
                Simulator::builder(code, "Top")
                    .four_state(four_state)
                    .build_interpreter()
                    .unwrap(),
            );
            check(
                Simulator::builder(code, "Top")
                    .four_state(four_state)
                    .build_cranelift()
                    .unwrap(),
            );
            check(
                Simulator::builder(code, "Top")
                    .four_state(four_state)
                    .build_wasm()
                    .unwrap(),
            );
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            {
                let original = Simulator::builder(code, "Top")
                    .four_state(four_state)
                    .build_native()
                    .unwrap();
                let image = celox::NativeProgramImage::from_container_bytes(
                    &original
                        .shared_code()
                        .program_image()
                        .to_container_bytes()
                        .unwrap(),
                )
                .unwrap();
                check(original);
                check(
                    Simulator::builder("", "")
                        .build_native_from_image(image)
                        .unwrap(),
                );
            }
        }
    }
}

#[test]
fn reset_between_edges_respects_polarity_and_sync_mode_without_a_clock_event() {
    use celox::ResetType;
    // Isolate the assertion at time 12: neither clock polarity is scheduled
    // then. A separate observer resumes at time 14, before slow's time-20 edge.
    let source = include_str!(
        "../../celox-test-suite-veryl/fixtures/testbench/concurrent_initial_reset_between_edges.veryl"
    )
    .replace(
        "    inst slow:",
        "    inst observer: $tb::clock_gen #(period: 14);\n    inst slow:",
    )
    .replace("fast.next(7);", "observer.next(1);");
    fn check<B: celox::SimBackend>(mut sim: Simulator<B>, reset_type: ResetType, four_state: bool) {
        let tb = compile_initial_testbench(&sim).unwrap();
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass,
            "{reset_type:?} four_state={four_state} backend={}",
            std::any::type_name::<B>()
        );
    }
    for reset_type in [
        ResetType::AsyncLow,
        ResetType::AsyncHigh,
        ResetType::SyncLow,
        ResetType::SyncHigh,
    ] {
        let source = if matches!(reset_type, ResetType::SyncLow | ResetType::SyncHigh) {
            source.replace("count == 8'd0", "count == 8'd1")
        } else {
            source.clone()
        };
        for four_state in [false, true] {
            macro_rules! check_backend {
                ($build:ident) => {
                    check(
                        Simulator::builder(&source, "Top")
                            .reset_type(reset_type)
                            .four_state(four_state)
                            .$build()
                            .unwrap(),
                        reset_type,
                        four_state,
                    );
                };
            }
            check_backend!(build_interpreter);
            check_backend!(build_cranelift);
            check_backend!(build_wasm);
            #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
            check_backend!(build_native);
        }
    }
}
