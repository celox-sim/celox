use celox::testbench::{
    compile_initial_testbench, run_compiled_testbench, run_compiled_testbench_to_finish,
    run_compiled_testbench_with_tick_limit,
};
use celox::{Simulator, TestResult};

// These runtime-specific tests use package-local snapshots of the shared suite's
// testbench fixtures. Keep them in celox's archive: sibling crate directories
// are unavailable when crates.io verifies the packaged tests.

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
    let code = include_str!("../testdata/veryl/concurrent_initial_shared_clock.veryl");
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
            (false, Some(b"child".as_slice())),
            (false, Some(b"worker".as_slice())),
            (true, Some(b"last".as_slice()))
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
        include_str!("../testdata/veryl/concurrent_initial_shared_clock.veryl"),
        include_str!("../testdata/veryl/concurrent_initial_clock_periods.veryl"),
        include_str!("../testdata/veryl/concurrent_initial_hierarchy.veryl"),
        include_str!("../testdata/veryl/concurrent_initial_reset_between_edges.veryl"),
        include_str!("../testdata/veryl/concurrent_initial_reset_only_clock.veryl"),
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
        assert!(restored.program().runtime_schema.processes.len() >= 2);
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
    let source = include_str!("../testdata/veryl/concurrent_initial_reset_between_edges.veryl")
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

#[test]
fn child_hierarchical_readmem_waits_for_its_process() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("words.hex");
    std::fs::write(&path, "ab\ncd\n").unwrap();
    let code = format!(
        r#"
        module Memory (word: output logic<8>) {{
            #[allow(unassign_variable)]
            var words: logic<8>[2];
            assign word = words[0];
        }}
        #[test(Loader)] module Loader (word: output logic<8>) {{
            inst clk: $tb::clock_gen;
            inst memory: Memory (word);
            initial {{ clk.next(1); $readmemh("{}", memory.words); }}
        }}
        #[test(Top)] module Top {{
            inst clk: $tb::clock_gen;
            var word: logic<8>;
            inst loader: Loader (word);
            initial {{
                $assert(loader.memory.words[0] == 0, "loaded before child ran");
                clk.next(2);
                $assert(loader.memory.words[0] == 8'hab);
                $finish();
            }}
        }}
    "#,
        path.display()
    );
    fn check<B: celox::SimBackend>(mut sim: Simulator<B>, expected: u32) {
        let tb = compile_initial_testbench(&sim).unwrap();
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass
        );
        assert_eq!(sim.get(sim.signal("word")), expected.into());
    }
    let skipped = code
        .replace(
            "clk.next(1); $readmemh",
            "clk.next(1); var load: logic; load = 0; if load { $readmemh",
        )
        .replace("memory.words);", "memory.words); }")
        .replace("== 8'hab", "== 0");
    let finished = code.replace(
        "clk.next(1); $readmemh",
        "clk.next(1); $finish(); $readmemh",
    );
    for (code, expected) in [(&code, 0xab), (&skipped, 0), (&finished, 0)] {
        check(
            Simulator::builder(code, "Top").build_interpreter().unwrap(),
            expected,
        );
        check(
            Simulator::builder(code, "Top").build_cranelift().unwrap(),
            expected,
        );
        check(
            Simulator::builder(code, "Top").build_wasm().unwrap(),
            expected,
        );
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        {
            let original = Simulator::builder(code, "Top").build_native().unwrap();
            let bytes = original
                .shared_code()
                .program_image()
                .to_container_bytes()
                .unwrap();
            let image = celox::NativeProgramImage::from_container_bytes(&bytes).unwrap();
            check(original, expected);
            check(
                Simulator::builder("unused", "unused")
                    .build_native_from_image(image)
                    .unwrap(),
                expected,
            );
        }
    }
}

#[test]
fn clock_period_accepts_module_constant_array_element() {
    let code = include_str!("../testdata/veryl/concurrent_initial_reset_only_clock.veryl").replace(
        "const PERIOD: u32 = P + 0;",
        "const PERIODS: u8[2] = '{P as u8, (P + 2) as u8}; const PERIOD: u32 = PERIODS[0];",
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
fn tick_limit_drains_falling_edges_and_reset_release_without_resuming_processes() {
    let code = r#"
    module FallingCounter (clk: input clock, count: output logic<8>) {
        always_ff (clk) { count += 1; }
    }
    #[test(Top)] module Top {
        inst clk: $tb::clock_gen #(period: 10);
        inst rst: $tb::reset_gen(clk);
        inst fast: $tb::clock_gen #(period: 2);
        let inverted: clock = ~clk;
        var count: logic<8>;
        inst counter: FallingCounter (clk: inverted, count);
        initial { fast.next(10); $assert(1'd0, "fast resumed past limit"); }
        initial { rst.assert(1); $assert(1'd0, "resumed past limit"); }
        initial { clk.next(2); $assert(1'd0, "resumed past limit"); }
    }"#;
    fn check<B: celox::SimBackend>(mut sim: Simulator<B>) {
        let tb = compile_initial_testbench(&sim).unwrap();
        let result = run_compiled_testbench_with_tick_limit(&mut sim, &tb, 1);
        assert_eq!(result.result, TestResult::Pass);
        assert_eq!(result.ticks, 1);
        assert!(result.tick_limit_reached);
        assert_eq!(sim.get(sim.signal("clk")), 0u32.into());
        assert_eq!(sim.get(sim.signal("fast")), 0u32.into());
        assert_eq!(sim.get(sim.signal("rst")), 1u32.into());
        assert_eq!(sim.get(sim.signal("count")), 1u32.into());
    }
    check(Simulator::builder(code, "Top").build_interpreter().unwrap());
    check(Simulator::builder(code, "Top").build_cranelift().unwrap());
    check(Simulator::builder(code, "Top").build_wasm().unwrap());
    check(Simulator::builder(code, "Top").build_tiered().unwrap());
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    check(Simulator::builder(code, "Top").build_native().unwrap());
}

#[test]
fn completed_process_dispatches_reset_release() {
    let code = r#"
    module Counter(clk: input clock, count: output logic<8>) {
        always_ff(clk) { count += 1; }
    }
    #[test(Top)] module Top {
        inst clk: $tb::clock_gen #(period: 10);
        inst rst: $tb::reset_gen(clk);
        let released: clock = ~rst;
        var count: logic<8>;
        inst dut: Counter(clk: released, count);
        initial { rst.assert(1); }
        initial { clk.next(1); }
    }"#;
    fn check<B: celox::SimBackend>(mut sim: Simulator<B>) {
        let tb = compile_initial_testbench(&sim).unwrap();
        assert_eq!(run_compiled_testbench(&mut sim, &tb), TestResult::Pass);
        assert_eq!(sim.get(sim.signal("count")), 1u32.into());
    }
    for code in [
        code.to_owned(),
        code.replace("rst.assert(1);", "rst.assert(1); $finish();"),
    ] {
        macro_rules! build {
            ($method:ident) => {
                check(
                    Simulator::builder(&code, "Top")
                        .reset_type(celox::ResetType::AsyncHigh)
                        .$method()
                        .unwrap(),
                );
            };
        }
        build!(build_interpreter);
        build!(build_cranelift);
        build!(build_wasm);
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        build!(build_native);
    }
}

#[test]
fn concurrent_formatted_time_uses_scheduler_timestamp() {
    let code = r#"#[test(Top)] module Top {
        inst clk: $tb::clock_gen #(period: 10);
        initial { clk.next(1); $assert_continue(1'd0, "time=%t"); $finish(); }
        initial { clk.next(3); }
    }"#;
    let result = Simulator::builder(code, "Top").run_test_detailed().unwrap();
    assert_eq!(
        result.assertions.last().unwrap().message.as_deref(),
        Some(b"time=10".as_slice())
    );
}

#[test]
fn concurrent_runtime_assertions_use_event_time() {
    let code = r#"
    module Check(clk: input clock) {
        always_ff(clk) { $assert_continue(1'd0, "ff time=%t"); }
    }
    #[test(Top)] module Top {
        inst clk: $tb::clock_gen #(period: 10);
        inst dut: Check(clk);
        initial { clk.next(3); $finish(); }
        initial { clk.next(4); }
    }"#;
    let result = Simulator::builder(code, "Top").run_test_detailed().unwrap();
    let messages: Vec<_> = result
        .assertions
        .iter()
        .map(|a| a.message.as_deref())
        .collect();
    assert_eq!(
        messages,
        vec![
            Some(b"ff time=0".as_slice()),
            Some(b"ff time=10".as_slice()),
            Some(b"ff time=20".as_slice())
        ]
    );
}

fn check_passes_on_every_backend(code: &str) {
    fn check<B: celox::SimBackend>(mut sim: Simulator<B>) {
        let tb = compile_initial_testbench(&sim).unwrap();
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass,
            "backend={}",
            std::any::type_name::<B>()
        );
    }
    check(Simulator::builder(code, "Top").build_interpreter().unwrap());
    check(Simulator::builder(code, "Top").build_cranelift().unwrap());
    check(Simulator::builder(code, "Top").build_wasm().unwrap());
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    check(Simulator::builder(code, "Top").build_native().unwrap());
}

/// A read after a branch whose one side stored and whose other side waited
/// sees the settled logic.
#[test]
fn read_after_branch_settles_the_store_of_the_other_side() {
    let code = r#"#[test(Top)] module Top {
        inst clk: $tb::clock_gen;
        var source: logic<8>;
        var cond: logic;
        let derived: logic<8> = source + 8'd1;
        initial {
            cond = 1;
            source = 8'd1;
            if cond { source = 8'd41; } else { clk.next(); }
            $assert(derived == 8'd42, "derived=%d", derived);
            case cond {
                1'b0: clk.next();
                default: source = 8'd7;
            }
            $assert(derived == 8'd8, "derived=%d", derived);
            $finish();
        }
    }"#;
    check_passes_on_every_backend(code);
}

/// Constructs a constant keeps out of the kernel take no scratch state, so
/// a later statement gets the scratch planned for it.
#[test]
fn unreachable_loops_take_no_scratch_state() {
    let code = r#"#[test(Top)] module Top {
        inst clk: $tb::clock_gen;
        var r: $tb::random::<u8>;
        var x: u8;
        initial {
            if 1'b0 {
                for i in 0..3 { clk.next(); }
            }
            for j in 3..3 { clk.next(); }
            r.seed(7);
            x = r.get_range(5, 5);
            $assert(x == 8'd5, "x=%d", x);
            $finish();
        }
    }"#;
    check_passes_on_every_backend(code);
}

/// A host request (a random number here) is a zero-time call: no other
/// process runs between the statements around it.
#[test]
fn host_requests_do_not_interleave_other_processes() {
    let code = r#"#[test(Top)] module Top {
        var r: $tb::random::<u8>;
        var x: u8;
        var v: u8;
        var seen: u8;
        initial { x = 8'd1; v = r.get(); x = 8'd2; }
        initial { seen = x; $assert(seen == 8'd2, "seen=%d", seen); $finish(); }
    }"#;
    check_passes_on_every_backend(code);
}

/// A `$finish` of the design's own clocked logic completes the run.
#[test]
fn finish_from_design_logic_completes_the_run() {
    let code = r#"#[test(Top)] module Top {
        inst clk: $tb::clock_gen;
        var count: logic<8>;
        always_ff (clk) {
            if count == 8'd3 { $finish(); }
            count += 1;
        }
        initial { clk.next(10); }
    }"#;
    check_passes_on_every_backend(code);
    fn check<B: celox::SimBackend>(mut sim: Simulator<B>) {
        let tb = compile_initial_testbench(&sim).unwrap();
        let result = run_compiled_testbench_with_tick_limit(&mut sim, &tb, 100);
        assert_eq!(result.result, TestResult::Pass);
        assert!(!result.tick_limit_reached);
    }
    check(Simulator::builder(code, "Top").build_interpreter().unwrap());
    check(Simulator::builder(code, "Top").build_cranelift().unwrap());
}

/// A run starts its process clocks low, as the bytecode testbench did: a
/// clock the host drove high still counts a real first edge.
#[test]
fn a_run_starts_its_process_clocks_low() {
    let code = r#"
    module Counter(clk: input clock, count: output logic<8>) {
        always_ff(clk) { count += 1; }
    }
    #[test(Top)] module Top {
        inst clk: $tb::clock_gen;
        var count: logic<8>;
        inst dut: Counter(clk, count);
        initial { clk.next(1); $finish(); }
    }"#;
    let mut sim = Simulator::builder(code, "Top").build_cranelift().unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();
    let clk = sim.signal("clk");
    sim.set_wide(clk, 1u8.into());
    assert_eq!(
        run_compiled_testbench_to_finish(&mut sim, &tb),
        TestResult::Pass
    );
    assert_eq!(sim.get(sim.signal("count")), 1u32.into());
}

/// More runtime events than the ring holds, in one uninterrupted block:
/// the kernel yields to the host before the ring wraps.
#[test]
fn many_events_in_one_run_are_not_missed() {
    let code = r#"#[test(Top)] module Top {
        var x: logic<8>;
        initial {
            x = 1;
            for i in 0..3000 { $assert(x == 1, "x=%d", x); }
            for i in 0..1500 { $display("line %d", i); }
            $finish();
        }
    }"#;
    check_passes_on_every_backend(code);
}

/// A fatal assertion with a formatted message fails with that message
/// alone.
#[test]
fn a_fatal_assertion_reports_its_rendered_message_once() {
    let code = r#"#[test(Top)] module Top {
        var x: logic<8>;
        initial { x = 5; $assert(x == 6, "x=%d", x); $finish(); }
    }"#;
    let mut sim = Simulator::builder(code, "Top").build_cranelift().unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();
    assert_eq!(
        run_compiled_testbench(&mut sim, &tb),
        TestResult::Fail("x=5".to_string())
    );
}

/// `clk.next(0)` continues at once: no other block runs in between.
#[test]
fn zero_count_clock_waits_do_not_interleave_other_processes() {
    let code = r#"#[test(Top)] module Top {
        inst clk: $tb::clock_gen;
        var x: u8;
        var seen: u8;
        initial { x = 8'd1; clk.next(0); x = 8'd2; }
        initial { seen = x; $assert(seen == 8'd2, "seen=%d", seen); $finish(); }
    }"#;
    check_passes_on_every_backend(code);
}

/// A block that ends right after a store settles, so the next block reads
/// the logic derived from it.
#[test]
fn a_block_ending_after_a_store_settles_for_the_next_block() {
    let code = r#"#[test(Top)] module Top {
        var source: logic<8>;
        let derived: logic<8> = source + 8'd1;
        initial { source = 8'd41; }
        initial { $assert(derived == 8'd42, "derived=%d", derived); $finish(); }
    }"#;
    check_passes_on_every_backend(code);
}

#[test]
fn expression_helper_finish_stops_continuations_and_other_processes() {
    let code = r#"#[test(Top)] module Top {
        var value: u32;
        function stop() -> u32 { $finish(); return 7; }
        initial { value = stop(); for i in 0..10000 { $assert(1'd0, "continued after finish"); } }
        initial { $assert(1'd0, "other process ran after finish"); }
    }"#;
    check_passes_on_every_backend(code);
}

#[test]
fn expression_helper_output_stores_settle_derived_reads() {
    let code = r#"#[test(Top)] module Top {
        var source: logic<8>;
        let derived: logic<8> = source + 8'd1;
        function update(value: output logic<8>) -> logic { value = 8'd41; return 1'd1; }
        initial { if update(source) { $assert(derived == 8'd42); } $finish(); }
    }"#;
    check_passes_on_every_backend(code);
}

#[test]
fn effectful_case_target_is_evaluated_once() {
    let code = r#"#[test(Top)] module Top {
        var calls: u32;
        function next() -> u32 { calls += 1; return calls; }
        initial { case next() { 0: $assert(1'd0); 1: $assert(calls == 1); default: $assert(1'd0); } $finish(); }
    }"#;
    check_passes_on_every_backend(code);
}

#[test]
fn loop_backedges_settle_the_previous_iteration_store() {
    let code = r#"#[test(Top)] module Top {
        var source: u32;
        let derived: u32 = source + 1;
        initial { for i in 0..3 { $assert(derived == i + 1); source += 1; } $finish(); }
    }"#;
    check_passes_on_every_backend(code);
}

#[test]
fn expression_values_survive_nested_helper_clock_waits() {
    let code = r#"#[test(Top)] module Top {
        inst clk: $tb::clock_gen;
        var value: u32;
        function later(value: input u32) -> u32 { clk.next(); return value; }
        initial { value = later(2) + later(5); $assert(value == 7); $finish(); }
    }"#;
    check_passes_on_every_backend(code);
}

#[test]
fn effectful_expression_helpers_preserve_short_circuit_and_branch_selection() {
    let code = r#"#[test(Top)] module Top {
        var calls: u32;
        var value: logic;
        function next() -> logic { calls += 1; return 1'd1; }
        initial {
            value = 1'd0 && next(); $assert(value == 1'd0);
            value = 1'd1 || next(); $assert(value == 1'd1);
            value = if 1'd1 ? next() : next();
            $assert(calls == 1); $finish();
        }
    }"#;
    check_passes_on_every_backend(code);
}

#[test]
fn expression_helpers_publish_passing_assertions_and_yield_before_ring_wrap() {
    let code = r#"#[test(Top)] module Top {
        var value: u32;
        function emit_event() -> u32 { $write(""); $assert(1'd1); return 1; }
        initial { for i in 0..1500 { value = emit_event(); } $finish(); }
    }"#;
    let detailed = Simulator::builder(code, "Top").run_test_detailed().unwrap();
    assert!(detailed.passed, "{detailed:?}");
    assert_eq!(detailed.assertions.len(), 1500);
    assert!(
        detailed
            .assertions
            .iter()
            .all(|assertion| assertion.passed && assertion.message.is_none())
    );
    check_passes_on_every_backend(code);
}

#[test]
fn fatal_combinational_step_returns_failure_without_event_drain_panic() {
    let code = r#"#[test(Top)] module Top {
        inst clk: $tb::clock_gen;
        var count: u32;
        always_ff(clk) { count += 1; }
        always_comb { $assert(count <: 2, "comb failed"); }
        initial { clk.next(10); $finish(); }
    }"#;
    let detailed = Simulator::builder(code, "Top").run_test_detailed().unwrap();
    assert!(!detailed.passed);
    assert!(
        detailed
            .error
            .as_deref()
            .is_some_and(|error| error.contains("comb failed")),
        "{detailed:?}"
    );
}

#[test]
fn dynamic_signed_wide_singleton_uses_the_wide_loop_path() {
    let code = r#"#[test(Top)] module Top {
        var bound: signed logic<192>;
        var count: u32;
        initial { bound = -(192'd1 << 150); for i in bound..=bound { count += 1; } $assert(count == 1); $finish(); }
    }"#;
    check_passes_on_every_backend(code);
}

#[test]
fn unreachable_clock_waits_leave_a_host_driven_clock_unchanged() {
    let code = r#"#[test(Top)] module Top {
        inst clk: $tb::clock_gen;
        initial { if 1'd0 { clk.next(); } for i in 3..3 { clk.next(); } $assert(clk == 1'd1); $finish(); }
    }"#;
    let mut sim = Simulator::builder(code, "Top").build_interpreter().unwrap();
    let clk = sim.signal("clk");
    sim.set(clk, 1u8);
    let tb = compile_initial_testbench(&sim).unwrap();
    assert_eq!(
        run_compiled_testbench_to_finish(&mut sim, &tb),
        TestResult::Pass
    );
    assert_eq!(sim.get(clk), 1u8.into());
}

#[test]
fn reset_duration_reads_the_value_before_assertion() {
    let code = r#"#[test(Top)] module Top {
        inst clk: $tb::clock_gen #(period: 2);
        inst rst: $tb::reset_gen(clk);
        initial { rst.assert(1); rst.assert((rst as u32) + 1); $assert(1'd1, "time=%t"); $finish(); }
    }"#;
    let detailed = Simulator::builder(code, "Top").run_test_detailed().unwrap();
    assert!(detailed.passed, "{detailed:?}");
    assert_eq!(detailed.assertions[0].message.as_deref(), Some("time=6"));
    check_passes_on_every_backend(code);
}

#[test]
fn statement_call_arguments_preserve_order_and_each_nested_result() {
    let code = r#"#[test(Top)] module Top {
        var calls: u32;
        function probe(value: input u32) -> u32 { calls = calls * 10 + value; return value; }
        function accept(first: input u32, second: input u32) { $assert(first == 1 && second == 2); }
        initial { accept(probe(1), probe(2)); $assert(calls == 12); $finish(); }
    }"#;
    check_passes_on_every_backend(code);
}

#[test]
fn selected_method_and_function_outputs_validate_destination_bounds() {
    for operation in ["words[2] = random.get();", "fill(words[2]);"] {
        let code = format!(
            r#"#[test(Top)] module Top {{
            var random: $tb::random::<u8>;
            var words: u8[2];
            function fill(value: output u8) {{ value=7; }}
            initial {{ {operation} $finish(); }}
        }}"#
        );
        let error = Simulator::builder(&code, "Top")
            .build_interpreter()
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("testbench selected destination") || error.contains("invalid select"),
            "{error}"
        );
    }
}

#[test]
fn display_and_assertion_arguments_use_process_helper_effects() {
    let code = r#"#[test(Top)] module Top {
        var calls: u32;
        function probe(value: input u32) -> u32 { calls += 1; $assert(1'd1); return value; }
        initial { $write("%d",probe(1)); $assert(1'd1,"value=%d",probe(2)); $assert(calls == 2); $finish(); }
    }"#;
    let detailed = Simulator::builder(code, "Top").run_test_detailed().unwrap();
    assert!(detailed.passed, "{detailed:?}");
    assert_eq!(detailed.assertions.len(), 4);
    assert_eq!(detailed.assertions[2].message.as_deref(), Some("value=2"));
    check_passes_on_every_backend(code);
}

#[test]
fn loop_and_random_bounds_survive_a_suspending_expression_call() {
    let code = r#"#[test(Top)] module Top {
        inst clk: $tb::clock_gen;
        var random: $tb::random::<u8>;
        var count: u32;
        var value: u8;
        function bound(value: input u32) -> u32 { var sample:u8; sample=random.get(); clk.next(); return value + (sample & 0); }
        initial { for i in bound(0)..3 { count += 1; } value=random.get_range(7,bound(7)); $assert(count == 3 && value == 7); $finish(); }
    }"#;
    check_passes_on_every_backend(code);
}

#[test]
fn unknown_conditional_helpers_preserve_value_masks_and_both_effects() {
    let code = r#"#[test(Top)] module Top(value: output logic<8>, calls: output u32) {
        var selector: logic;
        function one() -> logic<8> {calls += 1;return 8'haa;}
        function two() -> logic<8> {calls += 1;return 8'haf;}
        initial {selector=1'bx;value=if selector ? one() : two();$finish();}
    }"#;
    fn check<B: celox::SimBackend>(mut sim: Simulator<B>) {
        let tb = compile_initial_testbench(&sim).unwrap();
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass
        );
        assert_eq!(sim.get(sim.signal("calls")), 2u8.into());
        assert_eq!(
            sim.get_four_state(sim.signal("value")),
            (0xafu8.into(), 5u8.into())
        );
    }
    macro_rules! backend {
        ($build:ident) => {
            check(
                Simulator::builder(code, "Top")
                    .four_state(true)
                    .$build()
                    .unwrap(),
            )
        };
    }
    backend!(build_interpreter);
    backend!(build_cranelift);
    backend!(build_wasm);
    backend!(build_tiered);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    backend!(build_native);
}

#[test]
fn statically_skipped_expression_helpers_do_not_install_clocks() {
    let code = r#"#[test(Top)] module Top {
        inst clk: $tb::clock_gen;
        var value: logic;
        function wait_on_clk() -> logic { clk.next(); return 1'd1; }
        initial {
            value = 1'd0 && wait_on_clk();
            value = 1'd1 || wait_on_clk();
            value = if 1'd0 ? wait_on_clk() : 1'd1;
            value = if 1'd1 ? 1'd1 : wait_on_clk();
            $assert(clk == 1'd1); $finish();
        }
    }"#;
    let mut sim = Simulator::builder(code, "Top").build_interpreter().unwrap();
    let clk = sim.signal("clk");
    sim.set(clk, 1u8);
    let tb = compile_initial_testbench(&sim).unwrap();
    assert_eq!(
        run_compiled_testbench_to_finish(&mut sim, &tb),
        TestResult::Pass
    );
    assert_eq!(sim.get(clk), 1u8.into());
}

#[test]
fn dirty_break_edges_settle_before_loop_successors() {
    let code = r#"#[test(Top)] module Top {
        var source: u32; var derived: u32;
        assign derived = source + 1;
        initial { for i in 0..4 { source = 41; break; } $assert(derived == 42); $finish(); }
    }"#;
    check_passes_on_every_backend(code);
}

#[test]
fn overlapping_function_outputs_copy_in_declaration_order() {
    let code = r#"#[test(Top)] module Top {
        var value: u32;
        function update(a: output u32, b: output u32) { a = 1; b = 2; }
        initial { update(value, value); $assert(value == 2); $finish(); }
    }"#;
    check_passes_on_every_backend(code);
}

#[test]
fn finished_paths_do_not_initialize_unreachable_clocks() {
    for tail in [
        "$finish(); clk.next();",
        "if 1'd1 { $finish(); } clk.next();",
    ] {
        let code =
            format!("#[test(Top)] module Top {{ inst clk: $tb::clock_gen; initial {{ {tail} }} }}");
        let mut sim = Simulator::builder(&code, "Top")
            .build_interpreter()
            .unwrap();
        let clk = sim.signal("clk");
        sim.set(clk, 1u8);
        let tb = compile_initial_testbench(&sim).unwrap();
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass
        );
        assert_eq!(sim.get(clk), 1u8.into());
    }
}

#[test]
fn design_finish_during_settle_stops_kernel_continuations() {
    let code = r#"#[test(Top)] module Top {
        var go: logic;
        var after: logic;
        always_comb { if go { $finish(); } }
        initial { go=1; after=1; $assert(1'd0, "continued after finish"); }
    }"#;
    fn check<B: celox::SimBackend>(mut sim: Simulator<B>) {
        let tb = compile_initial_testbench(&sim).unwrap();
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass
        );
        assert_eq!(sim.get(sim.signal("after")), 0u8.into());
    }
    check(Simulator::builder(code, "Top").build_interpreter().unwrap());
    check(Simulator::builder(code, "Top").build_cranelift().unwrap());
    check(Simulator::builder(code, "Top").build_wasm().unwrap());
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    check(Simulator::builder(code, "Top").build_native().unwrap());
}

#[test]
fn overlapping_clock_outputs_retain_the_intermediate_rising_edge() {
    let code = r#"#[test(Top)] module Top {
        var clk: clock;
        var q: logic;
        function update(a: output clock, b: output clock) { a=1; b=0; }
        always_ff (clk) { q=1; }
        initial { clk=0; update(clk, clk); $assert(clk == 0); $finish(); }
    }"#;
    fn check<B: celox::SimBackend>(mut sim: Simulator<B>) {
        let tb = compile_initial_testbench(&sim).unwrap();
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass
        );
        assert_eq!(sim.get(sim.signal("q")), 1u8.into());
        assert_eq!(sim.get(sim.signal("clk")), 0u8.into());
    }
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
        check(
            Simulator::builder(code, "Top")
                .four_state(four_state)
                .build_native()
                .unwrap(),
        );
    }
}

#[test]
fn conditional_finish_calls_preserve_reachable_clocks() {
    for expression in ["if flag ? stop() : 0", "flag && stop()", "!flag || stop()"] {
        let code = format!(
            r#"#[test(Top)] module Top {{
            inst clk: $tb::clock_gen;
            var flag: logic;
            var value: logic;
            function stop() -> logic {{ $finish(); return 1; }}
            initial {{ flag=0; value={expression}; clk.next(); $finish(); }}
        }}"#
        );
        check_passes_on_every_backend(&code);
    }
}

#[test]
fn consecutive_reset_helpers_share_the_first_clock_edge() {
    for body in [
        "for i in 1..=3 { rst.assert(i); }",
        "reset_for(2); reset_for(4);",
    ] {
        let code = format!(
            r#"module Counter(clk: input clock, rst: input reset, q: output u32) {{
            always_ff {{ if_reset {{ q+=1; }} else {{ q+=1; }} }}
        }}
        #[test(Top)] module Top {{
            inst clk: $tb::clock_gen; inst rst: $tb::reset_gen(clk); var q:u32;
            inst dut: Counter(clk,rst,q);
            function reset_for(n: input u32) {{ rst.assert(n); }}
            initial {{ {body} $assert(q==6,"ticks=%d",q); $finish(); }}
        }}"#
        );
        check_passes_on_every_backend(&code);
        let mut fallback = Simulator::builder(&code, "Top")
            .testbench_kernels(false)
            .build_interpreter()
            .unwrap();
        let tb = compile_initial_testbench(&fallback).unwrap();
        assert_eq!(
            run_compiled_testbench_to_finish(&mut fallback, &tb),
            TestResult::Pass
        );
    }
}

#[test]
fn empty_dynamic_loop_bounds_settle_their_effects() {
    let code = r#"#[test(Top)] module Top {
        var source:u32; let derived:u32=source+1; var limit:u32;
        function bound(value:output u32)->u32 { value=7; return 1; }
        initial { limit=0; for i in bound(source)..limit { source=9; } $assert(derived==8); $finish(); }
    }"#;
    check_passes_on_every_backend(code);
}
#[test]
fn random_arguments_settle_their_effects() {
    for op in [
        "random.seed(bound(source));",
        "value=random.get_range(bound(source),bound(source));",
    ] {
        let code = format!(
            r#"#[test(Top)] module Top {{
            var random:$tb::random::<u32>; var source:u32; let derived:u32=source+1; var value:u32;
            function bound(value:output u32)->u32 {{ value=7; return 3; }}
            initial {{ {op} $assert(derived==8); $finish(); }}
        }}"#
        );
        check_passes_on_every_backend(&code);
    }
}
#[test]
fn tick_limit_settles_reset_releases_without_domains() {
    let code = r#"#[test(Top)] module Top {
        inst clk:$tb::clock_gen; inst rst:$tb::reset_gen(clk); let released:logic=rst;
        initial { rst.assert(1); $finish(); }
    }"#;
    fn check<B: celox::SimBackend>(mut sim: Simulator<B>) {
        let tb = compile_initial_testbench(&sim).unwrap();
        let result = run_compiled_testbench_with_tick_limit(&mut sim, &tb, 1);
        assert_eq!(result.result, TestResult::Pass);
        assert_eq!(result.ticks, 1);
        assert_eq!(sim.get(sim.signal("rst")), sim.get(sim.signal("released")));
        assert_eq!(sim.get(sim.signal("released")), 1u8.into());
    }
    check(Simulator::builder(code, "Top").build_interpreter().unwrap());
    check(Simulator::builder(code, "Top").build_cranelift().unwrap());
    check(Simulator::builder(code, "Top").build_wasm().unwrap());
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    check(Simulator::builder(code, "Top").build_native().unwrap());
}

#[test]
fn assignment_selection_helpers_use_process_semantics() {
    let code = r#"#[test(Top)] module Top { inst clk:$tb::clock_gen; var words:u8[2];
   function index()->u32 { clk.next(); $assert(1,"selected"); return 1; }
   initial { words[index()]=7; $assert(words[1]==7); $finish(); }
 }"#;
    check_passes_on_every_backend(code);
}
#[cfg(feature = "systemverilog")]
#[test]
fn testbench_fallthrough_ignores_background_design_processes() {
    let veryl =
        "#[test(Top)] module Top { var x:logic; inst dut:$sv::Worker(x); initial { $assert(1); } }";
    let sv = "module Worker(output logic x);initial forever #1 x=~x;endmodule";
    let mut sim = Simulator::from_mixed_sources(
        vec![(veryl, std::path::Path::new("top.veryl"))],
        vec![(sv, std::path::Path::new("worker.sv"))],
        "Top",
    )
    .build_interpreter()
    .unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();
    assert_eq!(run_compiled_testbench(&mut sim, &tb), TestResult::Pass);
}
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn traced_builds_preserve_kernel_clock_pulses() {
    let code = r#"#[test(Top)] module Top {var clk:clock;var q:logic;
 function pulse(a:output clock,b:output clock) {a=1;b=0;} always_ff(clk){q=1;}
 initial{clk=0;pulse(clk,clk);$finish();} }"#;
    let mut sim = Simulator::builder(code, "Top")
        .build_with_trace()
        .res
        .unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();
    assert_eq!(
        run_compiled_testbench_to_finish(&mut sim, &tb),
        TestResult::Pass
    );
    assert_eq!(sim.get(sim.signal("q")), 1u8.into());
}

#[test]
fn private_assignments_and_event_yields_do_not_replay_clean_comb_logic() {
    let count = |body: &str| {
        let code = format!(
            r#"#[test(Top)] module Top {{
          always_comb {{ $assert(1,"comb"); }}
          function helper()->u32 {{var tmp:u32;tmp=1;return tmp;}}
          initial {{ {body} $finish(); }}
        }}"#
        );
        let result = Simulator::builder(&code, "Top")
            .run_test_detailed()
            .unwrap();
        assert!(result.passed, "{result:?}");
        result
            .assertions
            .iter()
            .filter(|a| a.message.as_deref() == Some("comb"))
            .count()
    };
    assert_eq!(count("helper();"), count(""));
    assert_eq!(count(&"$assert(1);".repeat(600)), count(""));
}

#[test]
fn copyback_selection_helpers_resume_for_random_and_function_outputs() {
    for action in [
        "words[index()]=random.get_range(7,7);",
        "give(words[index()]);",
    ] {
        let code = format!(
            r#"#[test(Top)]module Top{{inst clk:$tb::clock_gen;var random:$tb::random::<u8>;var words:u8[2];
   function index()->u32{{clk.next();$assert(1,"index");return 1;}}
   function give(value:output u8){{value=7;}}
   initial{{{action}$assert(words[1]==7);$finish();}}
  }}"#
        );
        check_passes_on_every_backend(&code);
    }
}
#[test]
fn guaranteed_finish_loops_do_not_register_unreachable_clocks() {
    let code = r#"#[test(Top)]module Top{inst clk:$tb::clock_gen;initial{for i in 0..1{$finish();}clk.next();}}"#;
    let mut sim = Simulator::builder(code, "Top").build_interpreter().unwrap();
    let clk = sim.signal("clk");
    sim.set(clk, 1u8);
    let tb = compile_initial_testbench(&sim).unwrap();
    assert_eq!(run_compiled_testbench(&mut sim, &tb), TestResult::Pass);
    assert_eq!(sim.get(clk), 1u8.into());
}
#[test]
fn long_clock_waits_drain_design_runtime_events() {
    let code = r#"#[test(Top)]module Top{inst clk:$tb::clock_gen;var q:u32;always_ff(clk){$display("edge");q+=1;}initial{clk.next(1500);$assert(q==1500);$finish();}}"#;
    check_passes_on_every_backend(code);
}

#[cfg(feature = "systemverilog")]
#[test]
fn optimized_mixed_testbenches_preserve_sv_process_pulses() {
    let veryl = r#"#[test(Top)]module Top{inst clk:$tb::clock_gen;var q:logic;inst dut:$sv::Pulse(q);initial{clk.next(2);$assert(q==1);$finish();}}"#;
    let sv = "module Pulse(output bit q);bit clk;always @(posedge clk)q<=1;initial begin #1;clk=1;clk=0;end endmodule";
    let builder = || {
        Simulator::from_mixed_sources(
            vec![(veryl, std::path::Path::new("top.veryl"))],
            vec![(sv, std::path::Path::new("pulse.sv"))],
            "Top",
        )
        .opt_level(celox::OptLevel::O2)
    };
    fn check<B: celox::SimBackend>(mut sim: Simulator<B>) {
        let tb = compile_initial_testbench(&sim).unwrap();
        assert_eq!(run_compiled_testbench(&mut sim, &tb), TestResult::Pass);
    }
    check(builder().build_interpreter().unwrap());
    check(builder().build_cranelift().unwrap());
    check(builder().build_wasm().unwrap());
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    check(builder().build_native().unwrap());
}

#[test]
fn unselected_constant_case_arms_leave_host_clocks_unchanged() {
    let code = r#"#[test(Top)]module Top{inst clk:$tb::clock_gen;initial{case 1 {1:{$assert(1);}2:{clk.next();}default:{$assert(0);}}$finish();}}"#;
    let mut sim = Simulator::builder(code, "Top").build_interpreter().unwrap();
    let clk = sim.signal("clk");
    sim.set(clk, 1u8);
    let tb = compile_initial_testbench(&sim).unwrap();
    assert_eq!(run_compiled_testbench(&mut sim, &tb), TestResult::Pass);
    assert_eq!(sim.get(clk), 1u8.into());
}
