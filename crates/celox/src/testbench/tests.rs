use std::error::Error as _;

use super::*;

use crate::{Simulator, TestResult};

#[test]
fn evaluation_error_preserves_runtime_error_source() {
    let error = TestbenchEvaluationError::EvalComb {
        source: RuntimeErrorCode::InternalError,
    };

    assert!(error.source().unwrap().is::<RuntimeErrorCode>());
}

#[test]
fn random_values_sign_extend_for_wider_destinations() {
    assert_eq!(
        random_value_for_destination(0x80, 8, true, 16),
        BigUint::from(0xff80u16)
    );
    assert_eq!(
        random_value_for_destination(0x7f, 8, true, 16),
        BigUint::from(0x007fu16)
    );
    assert_eq!(
        random_value_for_destination(0x80, 8, false, 16),
        BigUint::from(0x0080u16)
    );
}

#[test]
fn component_returns_resize_with_declared_signedness() {
    assert_eq!(
        resize_component_return(BigUint::from(0x80u8), 8, true, 16),
        BigUint::from(0xff80u16)
    );
    assert_eq!(
        resize_component_return(BigUint::from(0x80u8), 8, false, 16),
        BigUint::from(0x0080u16)
    );
    assert_eq!(
        resize_component_return(BigUint::from(0x1234u16), 16, false, 8),
        BigUint::from(0x34u8)
    );
}

#[test]
fn traced_build_registers_compiled_testbench_runtime_event_sites() {
    let code = r#"
            #[test(t)]
            module t {
                initial {
                    $assert_continue(1'b0, "continue failure");
                    $finish();
                }
            }
        "#;
    let mut sim = Simulator::builder(code, "t").build_with_trace().unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();

    assert_eq!(
        run_compiled_testbench(&mut sim, &tb),
        TestResult::Fail("continue failure".to_string()),
    );
}

#[test]
fn expression_evaluation_preserves_sparse_vcd_tracking() {
    let code = r#"
            #[test(t)]
            module t {
                var a: logic<8>;
                var b: logic<8>;
                initial {
                    a = 1;
                    b = a;
                    if b == 1 {
                        $assert(a == b);
                        $display("a=%h", a);
                    }
                    $finish();
                }
            }
        "#;
    let dir = tempfile::tempdir().unwrap();
    let mut sim = Simulator::builder(code, "t")
        .vcd(dir.path().join("testbench.vcd"))
        .build()
        .unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();
    sim.dump(0);
    assert_eq!(run_compiled_testbench(&mut sim, &tb), TestResult::Pass);
    sim.dump(1);
    let before = sim.vcd_statistics().unwrap();
    sim.dump(2);
    assert_eq!(
        sim.vcd_statistics().unwrap().comparisons,
        before.comparisons
    );
}

#[test]
fn private_tb_writes_do_not_invalidate_hardware_but_port_aliases_do() {
    let source = r#"
            module Dut(a: input logic<8>, q: output logic<8>) { assign q = a + 1; }
            #[test(Top)] module Top {
                var a: logic<8>;
                var q: logic<8>;
                var scratch: logic<8>;
                inst dut: Dut(a, q);
                initial { scratch = 3; a = scratch; $assert(q == 4); $finish(); }
            }
        "#;
    let mut sim = Simulator::builder(source, "Top")
        .build_interpreter()
        .unwrap();
    let scratch = sim.signal("scratch");
    let a = sim.signal("a");
    let q = sim.signal("q");
    settle_testbench(&mut sim).unwrap();
    sim_set_u64(&mut sim, scratch, 3);
    assert!(!sim.dirty);
    sim_set_u64(&mut sim, a, 3);
    assert!(sim.dirty);
    settle_testbench(&mut sim).unwrap();
    assert_eq!(sim.get_as::<u8>(q), 4);
}

// Retained comb state keeps TB edges out of the compiled scheduler, so a
// two-state case covering every selector value must not retain any.
#[test]
fn complete_binary_case_retains_no_comb_state() {
    let arms = (0..4)
        .map(|i| format!("2'd{i}: q = d + 8'd{i};"))
        .collect::<String>();
    let source = format!(
        "module Top(s: input logic<2>, d: input logic<8>, q: output logic<8>) {{
                always_comb {{ case s {{ {arms} }} }}
            }}"
    );
    let sim = Simulator::builder(&source, "Top")
        .build_interpreter()
        .unwrap();
    assert!(sim.program.runtime_schema.persistent_comb_state.is_empty());
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn native_image_retains_latch_observation_and_tb_dependencies() {
    let source = include_str!("../../testdata/veryl/latch_initial.veryl");
    let image = Simulator::builder(source, "Top")
        .allow_inferred_latches(true)
        .compile_native()
        .unwrap()
        .into_program_image();
    let image =
        crate::NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap())
            .unwrap();
    let mut sim = Simulator::builder("", "Top")
        .build_native_from_image(image)
        .unwrap();
    assert!(!sim.program.runtime_schema.persistent_comb_state.is_empty());
    assert!(!sim.program.rtl_accesses.is_empty());
    let tb = compile_initial_testbench(&sim).unwrap();
    assert_eq!(
        run_compiled_testbench_to_finish(&mut sim, &tb),
        TestResult::Pass
    );
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn native_clock_batches_preserve_odd_periods_and_tb_before_edge_order() {
    let source = r#"
            module Counter(clk: input clock, rst: input reset_async_low, q: output logic<8>) {
                always_ff (clk, rst) { if_reset { q = 0; } else { q += 1; } }
            }
            #[test(Top)] module Top {
                inst fast: $tb::clock_gen #(period: 3);
                inst slow: $tb::clock_gen #(period: 4);
                var rst_n: reset_async_low;
                var enable: logic;
                var gated: reset_async_low;
                assign gated = rst_n && enable;
                var a: logic<8>;
                var b: logic<8>;
                inst ca: Counter(clk: fast, rst: gated, q: a);
                inst cb: Counter(clk: slow, rst: gated, q: b);
                initial {
                    rst_n = 1;
                    enable = 0;
                    fast.next(4);
                    $assert(a == 0 && b == 0);
                    enable = 1;
                    fast.next(4);
                    // At time 24 both clocks are due, but the TB runs first.
                    $assert(a == 4 && b == 3);
                    fast.next(4);
                    $assert(a == 8 && b == 6);
                    $finish();
                }
                initial { slow.next(100); }
            }
        "#;
    for mode in [
        crate::SchedulerMode::Reference,
        crate::SchedulerMode::Compiled,
    ] {
        let mut sim = Simulator::builder(source, "Top")
            .four_state(false)
            .scheduler_mode(mode)
            .build_native()
            .unwrap();
        let batch = clock_batch::ClockBatch::new(&sim);
        assert_eq!(batch.is_some(), mode != crate::SchedulerMode::Reference);
        let tb = compile_initial_testbench(&sim).unwrap();
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass
        );
        if let Some(batch) = batch {
            assert_eq!(sim.backend.get_as::<u64>(batch.count_slot()), 1);
        }
    }
}

#[test]
fn native_tb_scheduler_runs_processes_and_multiple_clocks_without_host_roundtrips() {
    let source = r#"
            module Counter(clk: input clock, q: output bit<32>) {
                always_ff { q += 1; }
            }
            #[test(Top)] module Top {
                inst fast: $tb::clock_gen #(period: 3);
                inst slow: $tb::clock_gen #(period: 4);
                var a: u32; var b: u32; var observed: u32;
                inst ca: Counter(clk: fast, q: a);
                inst cb: Counter(clk: slow, q: b);
                initial { for i in 0..128 { fast.next(); observed = a; } $finish(); }
                initial { for i in 0..128 { slow.next(); } }
            }
        "#;
    // No clock-wait batching in the source: both TB processes resume each cycle.
    let mut results = Vec::new();
    for mode in [
        crate::SchedulerMode::Reference,
        crate::SchedulerMode::Compiled,
    ] {
        let mut sim = Simulator::builder(source, "Top")
            .four_state(false)
            .scheduler_mode(mode)
            .build_native()
            .unwrap();
        let tb = compile_initial_testbench(&sim).unwrap();
        compiled::take_counters(&mut sim);
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass
        );
        let counters = compiled::take_counters(&mut sim);
        results.push((
            sim.get_as::<u32>(sim.signal("a")),
            sim.get_as::<u32>(sim.signal("b")),
            sim.get_as::<u32>(sim.signal("observed")),
        ));
        if mode == crate::SchedulerMode::Compiled {
            assert!(counters.0 < 10, "native entries: {counters:?}");
            assert!(counters.1 < 3, "host edge batches: {counters:?}");
        }
    }
    assert_eq!(results[0], results[1]);
    assert_eq!(results[1], (128, 96, 128));
}

/// Run `source` with the reference loop and with the compiled TB
/// scheduler, and return each result with the compiled run's counters.
fn run_both_schedulers(
    source: &str,
    signals: &[&str],
) -> [(TestResult, Vec<u64>, (usize, usize, usize)); 2] {
    [
        crate::SchedulerMode::Reference,
        crate::SchedulerMode::Compiled,
    ]
    .map(|mode| {
        let mut sim = Simulator::builder(source, "Top")
            .four_state(false)
            .scheduler_mode(mode)
            .build_native()
            .unwrap();
        let tb = compile_initial_testbench(&sim).unwrap();
        compiled::take_counters(&mut sim);
        let result = run_compiled_testbench(&mut sim, &tb);
        let counters = compiled::take_counters(&mut sim);
        assert_eq!(
            counters.0 != 0,
            mode == crate::SchedulerMode::Compiled,
            "generated scheduler runs: {counters:?}"
        );
        let values = signals
            .iter()
            .map(|name| sim.get_as::<u64>(sim.signal(name)))
            .collect();
        (result, values, counters)
    })
}

#[test]
fn compiled_tb_statements_stay_in_the_generated_scheduler() {
    let source = r#"
            module Counter(clk: input clock, q: output logic<8>) {
                always_ff { q += 1; }
            }
            #[test(Top)] module Top {
                inst clk: $tb::clock_gen;
                var q: logic<8>;
                var total: logic<16>;
                inst dut: Counter(clk, q);
                initial {
                    total = 0;
                    for round in 0..4 {
                        clk.next(5);
                        $assert(q == 5 * (round as 8 + 1), "round %d q=%d", round, q);
                        $display("round %d q=%d at %t", round, q);
                        total += q as 16;
                    }
                    $finish();
                }
            }
        "#;
    let [reference, compiled] = run_both_schedulers(source, &["q", "total"]);
    assert_eq!(reference.0, TestResult::Pass);
    assert_eq!(compiled.0, TestResult::Pass);
    assert_eq!(reference.1, compiled.1);
    assert_eq!(compiled.1, vec![20, 50]);
    // Assertions, displays and the finish run in generated code.
    assert_eq!(compiled.2.2, 0, "host statements: {:?}", compiled.2);
}

#[test]
fn compiled_tb_resets_and_their_edges_stay_in_the_generated_scheduler() {
    let source = r#"
            module Counter(clk: input clock, rst: input reset, q: output logic<8>) {
                always_ff { if_reset { q = 0; } else { q += 1; } }
            }
            #[test(Top)] module Top {
                inst clk: $tb::clock_gen;
                inst rst: $tb::reset_gen (clk);
                var q: logic<8>;
                var total: logic<16>;
                inst dut: Counter(clk, rst, q);
                initial {
                    total = 0;
                    for round in 0..4 {
                        rst.assert(2);
                        $assert(q == 0);
                        clk.next(5);
                        $assert(q == 5);
                        total += q as 16;
                    }
                    // A reset the TB assigns moves its event too.
                    rst = 0;
                    clk.next(1);
                    $assert(q == 0);
                    rst = 1;
                    clk.next(3);
                    total += q as 16;
                    $finish();
                }
            }
        "#;
    let [reference, compiled] = run_both_schedulers(source, &["q", "total"]);
    assert_eq!(reference.0, TestResult::Pass);
    assert_eq!(compiled.0, TestResult::Pass);
    assert_eq!(reference.1, compiled.1);
    assert_eq!(compiled.1, vec![3, 23]);
    // The resets, their releases and their edges run in generated code.
    assert_eq!(compiled.2.1, 0, "host edge batches: {:?}", compiled.2);
    assert_eq!(compiled.2.2, 0, "host statements: {:?}", compiled.2);
}

#[test]
fn compiled_tb_resets_with_derived_resets_match_the_reference() {
    // The design derives its own reset from the TB's: those writes stay
    // in the host, without costing every slot of the run.
    let source = r#"
            module Counter(clk: input clock, rst: input reset, run: input logic,
                           q: output logic<8>) {
                let core_rst: reset = rst & run;
                always_ff (clk, core_rst) { if_reset { q = 0; } else { q += 1; } }
            }
            #[test(Top)] module Top {
                inst clk: $tb::clock_gen;
                inst rst: $tb::reset_gen (clk);
                var run: logic;
                var q: logic<8>;
                inst dut: Counter(clk, rst, run, q);
                initial {
                    run = 1;
                    rst.assert(2);
                    clk.next(5);
                    $assert(q == 5);
                    rst.assert(1);
                    $assert(q == 0);
                    clk.next(3);
                    $finish();
                }
            }
        "#;
    let [reference, compiled] = run_both_schedulers(source, &["q"]);
    assert_eq!(reference.0, TestResult::Pass);
    assert_eq!(compiled.0, TestResult::Pass);
    assert_eq!(reference.1, compiled.1);
    assert_eq!(compiled.2.2, 0, "host statements: {:?}", compiled.2);
}

#[test]
fn compiled_tb_failures_keep_their_messages_and_times() {
    // A second process reports physical event times instead of ticks.
    for (extra, expected) in [
        ("", "continue t=3 q=3\nfatal t=5 q=5"),
        (
            "initial { clk.next(100); }",
            "continue t=6 q=3\nfatal t=10 q=5",
        ),
    ] {
        let source = format!(
            r#"
                module Counter(clk: input clock, q: output logic<8>) {{
                    always_ff {{ q += 1; }}
                }}
                #[test(Top)] module Top {{
                    inst clk: $tb::clock_gen;
                    var q: logic<8>;
                    inst dut: Counter(clk, q);
                    initial {{
                        clk.next(3);
                        $assert_continue(q == 100, "continue t=%t q=%d", q);
                        clk.next(2);
                        $assert(q == 200, "fatal t=%t q=%d", q);
                        clk.next(2);
                        $finish();
                    }}
                    {extra}
                }}
            "#
        );
        let [reference, compiled] = run_both_schedulers(&source, &["q"]);
        assert_eq!(reference.0, TestResult::Fail(expected.into()));
        assert_eq!(reference.0, compiled.0);
        assert_eq!(reference.1, compiled.1);
        assert_eq!(compiled.2.2, 0, "host statements: {:?}", compiled.2);
    }
}

#[test]
fn compiled_tb_rtl_reports_keep_their_times() {
    // The generated scheduler runs many times before the host drains
    // these reports.
    let source = r#"
            module Counter(clk: input clock, q: output logic<8>) {
                always_ff {
                    q += 1;
                    if q[1:0] == 2'd2 { $assert_continue(1'b0, "rtl t=%t q=%d", q); }
                }
            }
            #[test(Top)] module Top {
                inst clk: $tb::clock_gen;
                var q: logic<8>;
                inst dut: Counter(clk, q);
                initial { clk.next(12); $finish(); }
            }
        "#;
    let [reference, compiled] = run_both_schedulers(source, &["q"]);
    assert!(
        matches!(&reference.0, TestResult::Fail(message) if message.lines().count() == 3),
        "{:?}",
        reference.0
    );
    assert_eq!(reference.0, compiled.0);
    assert_eq!(compiled.2.1, 0, "host edge batches: {:?}", compiled.2);
}

#[test]
fn tb_messages_format_unknown_bits() {
    // Digits whose bits are all unknown print `x`, partly unknown ones
    // `X` (IEEE 1800-2023 21.2.1.3). Veryl prints any unknown decimal as
    // `x`.
    let source = r#"
            #[test(Top)] module Top {
                inst clk: $tb::clock_gen;
                var mem: logic<16> [4];
                var padded: logic<12> [4];
                var p: logic<8>;
                var u: logic<8>;
                assign p = 8'bxx0x0101;
                initial {
                    clk.next(1);
                    $assert_continue(1'b0, "m=%h n=%h p=%h d=%d", mem[2], padded[1], p, u);
                    $assert(1'b1, "p=%h", p);
                    $finish();
                }
            }
        "#;
    fn check<B: SimBackend>(build: impl Fn() -> Simulator<B>, context: &str) {
        let mut sim = build();
        let tb = compile_initial_testbench(&sim).unwrap();
        let detailed = run_testbench_detailed(&mut sim, &tb);
        let messages: Vec<_> = detailed
            .assertions
            .iter()
            .map(|assertion| {
                (
                    assertion.passed,
                    String::from_utf8(assertion.message.clone().unwrap()).unwrap(),
                )
            })
            .collect();
        assert_eq!(
            messages,
            [
                (false, "m=xxxx n=xxx p=X5 d=x".to_string()),
                (true, "p=X5".to_string())
            ],
            "{context}"
        );
        let mut sim = build();
        let tb = compile_initial_testbench(&sim).unwrap();
        assert_eq!(
            run_compiled_testbench(&mut sim, &tb),
            TestResult::Fail("m=xxxx n=xxx p=X5 d=x".into()),
            "{context}"
        );
    }
    let builder = |mode| {
        Simulator::builder(source, "Top")
            .four_state(true)
            .scheduler_mode(mode)
    };
    for mode in [
        crate::SchedulerMode::Reference,
        crate::SchedulerMode::Compiled,
    ] {
        check(
            || builder(mode).build_native().unwrap(),
            &format!("native {mode:?}"),
        );
        #[cfg(feature = "cranelift")]
        check(
            || builder(mode).build_cranelift().unwrap(),
            &format!("cranelift {mode:?}"),
        );
        check(
            || builder(mode).build_interpreter().unwrap(),
            &format!("interpreter {mode:?}"),
        );
    }
}

#[test]
fn compiled_tb_finish_in_one_process_stops_the_others() {
    let source = r#"
            module Counter(clk: input clock, q: output logic<8>) {
                always_ff { q += 1; }
            }
            #[test(Top)] module Top {
                inst clk: $tb::clock_gen;
                var q: logic<8>;
                var loops: logic<8>;
                inst dut: Counter(clk, q);
                initial { clk.next(10); $finish(); }
                initial {
                    loops = 0;
                    for i in 0..100 { clk.next(); loops += 1; }
                }
            }
        "#;
    let [reference, compiled] = run_both_schedulers(source, &["q", "loops"]);
    assert_eq!(compiled.0, TestResult::Pass);
    assert_eq!(reference.1, compiled.1);
    assert_eq!(compiled.2.2, 0, "host statements: {:?}", compiled.2);
}

#[test]
fn compiled_tb_drains_events_before_the_buffer_overflows() {
    // More records in one time than the runtime event buffer holds.
    let source = r#"
            #[test(Top)] module Top {
                inst clk: $tb::clock_gen;
                initial {
                    for i in 0..3000 { $write(""); }
                    clk.next();
                    $finish();
                }
            }
        "#;
    let [reference, compiled] = run_both_schedulers(source, &[]);
    assert_eq!(reference.0, TestResult::Pass);
    assert_eq!(compiled.0, TestResult::Pass);
    assert_eq!(compiled.2.2, 0, "host statements: {:?}", compiled.2);
}

#[test]
fn detailed_compiled_tb_runs_record_passing_assertions() {
    let source = r#"
            module Counter(clk: input clock, q: output logic<8>) {
                always_ff { q += 1; }
            }
            #[test(Top)] module Top {
                inst clk: $tb::clock_gen;
                var q: logic<8>;
                inst dut: Counter(clk, q);
                initial {
                    for i in 0..3 {
                        clk.next();
                        $assert(q != 0, "q=%d", q);
                    }
                    $finish();
                }
            }
        "#;
    let mut sim = Simulator::builder(source, "Top")
        .four_state(false)
        .build_native()
        .unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();
    let detailed = run_testbench_detailed(&mut sim, &tb);
    assert!(detailed.passed);
    let messages: Vec<_> = detailed
        .assertions
        .iter()
        .map(|assertion| (assertion.passed, assertion.message.clone()))
        .collect();
    assert_eq!(
        messages,
        [1, 2, 3].map(|q| (true, Some(format!("q={q}").into_bytes())))
    );
}

#[test]
fn passing_assertions_keep_the_writes_of_their_messages() {
    let source = r#"
            #[test(Top)] module Top {
                inst clk: $tb::clock_gen;
                var count: u8;
                function bump() -> u8 {
                    count = count + 1;
                    return count;
                }
                initial {
                    count = 0;
                    $assert(1'b1, "%d", bump());
                    $assert(count == 1, "%d", bump());
                    clk.next();
                    $finish();
                }
            }
        "#;
    let [reference, compiled] = run_both_schedulers(source, &["count"]);
    assert_eq!(reference.0, TestResult::Pass);
    assert_eq!(compiled.0, TestResult::Pass);
    assert_eq!(reference.1, vec![2]);
    assert_eq!(compiled.1, vec![2]);
}

#[test]
fn compiled_tb_drains_rtl_event_bursts() {
    // Bursts of records in successive times that fit the runtime event
    // buffer one at a time, but not together.
    let source = r#"
            module Burst(clk: input clock, q: output logic<8>) {
                always_ff {
                    q += 1;
                    if q == 1 { for i in 0..400 { $write(""); } }
                    if q == 2 { for i in 0..800 { $write(""); } }
                }
            }
            #[test(Top)] module Top {
                inst clk: $tb::clock_gen;
                var q: logic<8>;
                inst dut: Burst(clk, q);
                initial { clk.next(4); $finish(); }
            }
        "#;
    let [reference, compiled] = run_both_schedulers(source, &["q"]);
    assert_eq!(reference.0, TestResult::Pass);
    assert_eq!(compiled.0, TestResult::Pass);
    assert_eq!(compiled.1, reference.1);
}

#[test]
fn compiled_tb_settle_continuations_are_not_limited_to_one_byte() {
    let mut source =
        String::from("#[test(Top)] module Top { var a: bit<8>; let q: bit<8> = a + 1; ");
    for _ in 0..260 {
        source.push_str("initial { $assert(q == 1); } ");
    }
    source.push_str("initial { $finish(); } }");
    let mut sim = Simulator::builder(&source, "Top")
        .four_state(false)
        .build_interpreter()
        .unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();
    assert_eq!(
        run_compiled_testbench_to_finish(&mut sim, &tb),
        TestResult::Pass
    );
}

#[test]
fn native_tb_dynamic_clock_count_preserves_dut_writes() {
    let source = r#"
            module Register(clk: input clock, d: input bit<32>, q: output bit<32>) {
                let next_value: bit<32> = d + 1;
                always_ff { q = next_value; }
            }
            #[test(Top)] module Top {
                inst clk: $tb::clock_gen;
                var d: u32; var q: u32;
                inst dut: Register(clk, d, q);
                function change_input() -> u32 { d = 7; return 1; }
                initial {
                    d=1; clk.next(); $assert(q==2);
                    clk.next(change_input()); $assert(q==8); $finish();
                }
            }
        "#;
    fn run<B: SimBackend>(mut sim: Simulator<B>) {
        let tb = compile_initial_testbench(&sim).unwrap();
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass
        );
    }
    run(Simulator::builder(source, "Top")
        .four_state(false)
        .build_native()
        .unwrap());
    run(Simulator::builder(source, "Top")
        .four_state(false)
        .build_interpreter()
        .unwrap());
    #[cfg(feature = "cranelift")]
    run(Simulator::builder(source, "Top")
        .four_state(false)
        .build()
        .unwrap());
}

#[test]
fn concurrent_tb_preserves_direct_async_reset_writes() {
    let source = r#"
            module Counter(clk: input clock, rst: input reset_async_high,
                           q: output logic<8>) {
                always_ff (clk, rst) { if_reset { q = 0; } else { q += 1; } }
            }
            #[test(Top)] module Top {
                inst fast: $tb::clock_gen #(period: 2);
                inst slow: $tb::clock_gen #(period: 20);
                var rst: reset_async_high;
                var q: logic<8>;
                inst dut: Counter(clk: slow, rst, q);
                initial {
                    rst = 0;
                    fast.next();
                    $assert(q == 1);
                    rst = 1;
                    __OBSERVE__
                    fast.next();
                    $assert(q == 0);
                    $finish();
                }
                initial { slow.next(100); }
            }
        "#;
    // Reading the reset settles hardware and clears `dirty`; its edge
    // must still be detected against the queue's previous sample.
    for observation in ["", "$assert(rst == 1);"] {
        let source = source.replace("__OBSERVE__", observation);
        let mut sim = Simulator::builder(&source, "Top")
            .build_interpreter()
            .unwrap();
        let tb = compile_initial_testbench(&sim).unwrap();
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass
        );
    }
}

/// A reset that `rst.assert` moves and a TB write then makes unknown
/// keeps its unknown level when its edges fall back to the host. The
/// move from 1 is a negedge (IEEE 1800-2023 9.4.2), and the flip-flop's
/// unknown reset condition is false (12.4): it counts once more.
fn unknown_tb_reset_levels<B: SimBackend>(build: impl Fn(crate::SchedulerMode) -> Simulator<B>) {
    for mode in [
        crate::SchedulerMode::Reference,
        crate::SchedulerMode::Compiled,
    ] {
        let mut sim = build(mode);
        let tb = compile_initial_testbench(&sim).unwrap();
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass
        );
        let [rst, count, c0] =
            ["rst", "count", "c0"].map(|name| sim.get_four_state(sim.signal(name)));
        assert_ne!(rst.1, 0u8.into(), "{mode:?}: rst stays unknown: {rst:?}");
        assert_eq!(c0, (4u8.into(), 0u8.into()), "{mode:?}");
        // The reference scheduler does not run the negedge of a TB
        // write to an unknown level (#2650).
        if mode == crate::SchedulerMode::Compiled {
            assert_eq!(count, (9u8.into(), 0u8.into()), "{mode:?}");
        }
    }
}

const UNKNOWN_TB_RESET: &str = r#"
        module Counter(clk: input clock, rst: input reset, count: output logic<8>) {
            always_ff (clk, rst) { if_reset { count = 0; } else { count += 1; } }
        }
        #[test(Top)] module Top {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen (clk);
            var unknown: logic;
            var count: logic<8>;
            var c0: logic<8>;
            inst dut: Counter(clk, rst, count);
            initial {
                rst.assert(2);
                clk.next(4);
                c0 = count;
                rst = unknown;
                clk.next(4);
                $finish();
            }
        }
    "#;

#[test]
fn unknown_tb_reset_levels_survive_host_edges_on_the_interpreter() {
    unknown_tb_reset_levels(|mode| {
        Simulator::builder(UNKNOWN_TB_RESET, "Top")
            .four_state(true)
            .scheduler_mode(mode)
            .build_interpreter()
            .unwrap()
    });
}

#[cfg(target_arch = "x86_64")]
#[test]
fn unknown_tb_reset_levels_survive_host_edges_natively() {
    unknown_tb_reset_levels(|mode| {
        Simulator::builder(UNKNOWN_TB_RESET, "Top")
            .four_state(true)
            .scheduler_mode(mode)
            .build_native()
            .unwrap()
    });
}

/// A four-state design's TB edges run in the generated scheduler, as
/// the edge engine runs them: a reset, a clock enable and so a gated
/// clock that the TB sets to x or z, and flip-flops that start at x.
fn four_state_tb_edges<B: SimBackend>(build: impl Fn(crate::SchedulerMode) -> Simulator<B>) {
    let names = [
        "count", "gated", "c0", "c1", "c2", "c3", "c4", "g1", "g2", "g3", "g4",
    ];
    let mut results = Vec::new();
    for mode in [
        crate::SchedulerMode::Reference,
        crate::SchedulerMode::Compiled,
    ] {
        let mut sim = build(mode);
        let tb = compile_initial_testbench(&sim).unwrap();
        compiled::take_counters(&mut sim);
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass
        );
        let counters = compiled::take_counters(&mut sim);
        let values: Vec<_> = names
            .iter()
            .map(|name| sim.get_four_state(sim.signal(name)))
            .collect();
        results.push((values, counters.1));
    }
    // The reference scheduler misses the negedge of `rst = 'x` (#2650):
    // from then on its count is one lower.
    let mut reference = results[0].0.clone();
    for (index, name) in names.iter().enumerate() {
        if ["count", "c3", "c4"].contains(name) {
            reference[index].0 += 1u8;
        }
    }
    assert_eq!(reference, results[1].0);
    // A TB write moves the reset in the unit unless the reset moves to
    // or from x or z: its writes from its initial x to 0, from 1 to x,
    // from x to z and from z to 1 go to the host, and so does the first
    // step.
    assert_eq!(results[1].1, 5, "host edge batches");
    let (value, mask) = &results[1].0[2];
    assert!(*mask != 0u8.into(), "count starts at x: {value}/{mask}");
}

const FOUR_STATE_TB: &str = r#"
        module Gate(clk: input clock, rst: input reset_async_low, en: input logic,
                    count: output logic<8>, gated: output logic<8>) {
            let gclk: '_ clock = clk & en;
            always_ff (clk, rst) { if_reset { count = 0; } else { count += 1; } }
            always_ff (gclk) { gated += 1; }
        }
        #[test(Top)] module Top {
            inst clk: $tb::clock_gen;
            var rst: reset_async_low;
            var en: logic;
            var count: logic<8>;
            var gated: logic<8>;
            var c0: logic<8>; var c1: logic<8>; var c2: logic<8>;
            var c3: logic<8>; var c4: logic<8>;
            var g1: logic<8>; var g2: logic<8>; var g3: logic<8>; var g4: logic<8>;
            inst dut: Gate(clk, rst, en, count, gated);
            initial {
                clk.next(8);
                c0 = count;
                rst = 0;
                clk.next(1);
                rst = 1;
                en = 'x;
                clk.next(8);
                c1 = count; g1 = gated;
                en = 1;
                clk.next(8);
                c2 = count; g2 = gated;
                rst = 'x;
                clk.next(8);
                c3 = count; g3 = gated;
                rst = 'z;
                en = 'z;
                clk.next(4);
                rst = 1;
                en = 0;
                clk.next(8);
                c4 = count; g4 = gated;
                $finish();
            }
        }
    "#;

#[test]
fn four_state_tb_edges_run_in_the_generated_scheduler_on_the_interpreter() {
    four_state_tb_edges(|mode| {
        Simulator::builder(FOUR_STATE_TB, "Top")
            .four_state(true)
            .scheduler_mode(mode)
            .build_interpreter()
            .unwrap()
    });
}

#[cfg(target_arch = "x86_64")]
#[test]
fn four_state_tb_edges_run_in_the_generated_scheduler_natively() {
    four_state_tb_edges(|mode| {
        Simulator::builder(FOUR_STATE_TB, "Top")
            .four_state(true)
            .scheduler_mode(mode)
            .build_native()
            .unwrap()
    });
}

#[cfg(feature = "cranelift")]
#[test]
fn four_state_tb_edges_run_in_the_generated_scheduler_with_cranelift() {
    four_state_tb_edges(|mode| {
        Simulator::builder(FOUR_STATE_TB, "Top")
            .four_state(true)
            .scheduler_mode(mode)
            .build_cranelift()
            .unwrap()
    });
}

#[cfg(feature = "wasm-runtime")]
#[test]
fn four_state_tb_edges_run_in_the_generated_scheduler_on_wasm() {
    four_state_tb_edges(|mode| {
        Simulator::builder(FOUR_STATE_TB, "Top")
            .four_state(true)
            .scheduler_mode(mode)
            .build_wasm()
            .unwrap()
    });
}

const UNKNOWN_WRITES_TB: &str = r#"
        module Top2(clk: input clock, a: input logic, q: output logic) {
            always_ff (clk) { q = a; }
        }
        #[test(Top)] module Top {
            inst clk: $tb::clock_gen;
            var r: logic; var s: logic; var a: logic; var q: logic;
            var w: logic<8>; var m: logic<8>;
            var n: logic; var o: logic; var e: logic; var t: logic<8>;
            var i1: logic<8>; var i2: logic<8>;
            var v: logic<8>; var idx: logic<3>; var b: bit<8>;
            var y: logic; var f: bit;
            var k4: logic<4>; var wm: logic; var ux: logic<8>; var cb: bit<8>;
            var ir: logic; var ua: logic<8> [4]; var idx2: logic<2>; var ue: logic<8>;
            var pz: logic<4>; var sel: logic;
            inst dut: Top2(clk, a, q);
            function pass_bit(x: input bit) -> bit {
                return x;
            }
            initial {
                r = 1; s = 0; a = 0; v = 0;
                clk.next(1);
                r = 'x; s = 'z; a = 'x;
                w = 'x;
                m = 8'b01xz01xz;
                n = r & 1; o = r | 1; e = r == 1;
                t = if r ? 8'd3 : 8'd5;
                if r { i1 = 1; } else { i1 = 2; }
                if r == 1 { i2 = 1; } else { i2 = 2; }
                idx = 'x;
                v[idx] = 1;
                b = m as u8;
                y = s;
                f = pass_bit(r);
                k4 = 4'b0101;
                wm = k4 ==? 4'b01xx;
                ux = 'x;
                cb = (ux as u8) + 1;
                ir = v[idx];
                ua[0] = 8'h12;
                idx2 = 'x;
                ue = ua[idx2];
                pz = 4'b0z10;
                sel = pz[2];
                clk.next(2);
                $finish();
            }
        }
    "#;

/// A TB write stores x and z bits in a four-state build, in both
/// schedulers: constants, operator results, and through the DUT. An
/// unknown condition is false (IEEE 1800-2023 12.4), an unknown ternary
/// condition combines both arms (11.4.11), a write at an unknown index
/// writes nothing and a read there is x (11.5.1), a copy or a selection keeps z bits, a
/// wildcard pattern's unknown bits match anything (11.4.6), and a
/// two-state cast, variable or function argument reads unknown bits as
/// 0 (6.11.2). A two-state build reads every unknown bit as 0, a
/// wildcard pattern's too. A two-state build reads every unknown bit as 0.
fn tb_writes_keep_unknown_bits<B: SimBackend>(
    build: impl Fn(bool, crate::SchedulerMode) -> Simulator<B>,
) {
    let names = [
        "r", "s", "a", "q", "w", "m", "n", "o", "e", "t", "i1", "i2", "v", "b", "y", "f", "wm",
        "cb", "ir", "ue", "sel",
    ];
    // Celox encodes an X bit as (1, 1) and a Z bit as (0, 1).
    let four_state: [(u64, u64); 21] = [
        (1, 1),
        (0, 1),
        (1, 1),
        (1, 1),
        (0xff, 0xff),
        (0x66, 0x33),
        (1, 1),
        (1, 0),
        (1, 1),
        (7, 6),
        (2, 0),
        (2, 0),
        (0, 0),
        (0x44, 0),
        (0, 1),
        (0, 0),
        (1, 0),
        (1, 0),
        (1, 1),
        (0xff, 0xff),
        (0, 1),
    ];
    let two_state: [(u64, u64); 21] = [
        (0, 0),
        (0, 0),
        (0, 0),
        (0, 0),
        (0, 0),
        (0x44, 0),
        (0, 0),
        (1, 0),
        (0, 0),
        (5, 0),
        (2, 0),
        (2, 0),
        (1, 0),
        (0x44, 0),
        (0, 0),
        (0, 0),
        (0, 0),
        (1, 0),
        (1, 0),
        (0x12, 0),
        (0, 0),
    ];
    for (four, expected) in [(true, four_state), (false, two_state)] {
        for mode in [
            crate::SchedulerMode::Reference,
            crate::SchedulerMode::Compiled,
        ] {
            let mut sim = build(four, mode);
            let tb = compile_initial_testbench(&sim).unwrap();
            assert_eq!(
                run_compiled_testbench_to_finish(&mut sim, &tb),
                TestResult::Pass
            );
            let values: Vec<_> = names
                .iter()
                .map(|name| sim.get_four_state(sim.signal(name)))
                .collect();
            let expected: Vec<_> = expected
                .iter()
                .map(|&(value, mask)| (BigUint::from(value), BigUint::from(mask)))
                .collect();
            assert_eq!(values, expected, "four_state={four} {mode:?} {names:?}");
        }
    }
}

#[test]
fn tb_writes_keep_unknown_bits_on_the_interpreter() {
    tb_writes_keep_unknown_bits(|four_state, mode| {
        Simulator::builder(UNKNOWN_WRITES_TB, "Top")
            .four_state(four_state)
            .scheduler_mode(mode)
            .build_interpreter()
            .unwrap()
    });
}

#[cfg(target_arch = "x86_64")]
#[test]
fn tb_writes_keep_unknown_bits_natively() {
    tb_writes_keep_unknown_bits(|four_state, mode| {
        Simulator::builder(UNKNOWN_WRITES_TB, "Top")
            .four_state(four_state)
            .scheduler_mode(mode)
            .build_native()
            .unwrap()
    });
}

#[cfg(feature = "cranelift")]
#[test]
fn tb_writes_keep_unknown_bits_with_cranelift() {
    tb_writes_keep_unknown_bits(|four_state, mode| {
        Simulator::builder(UNKNOWN_WRITES_TB, "Top")
            .four_state(four_state)
            .scheduler_mode(mode)
            .build_cranelift()
            .unwrap()
    });
}

#[cfg(feature = "wasm-runtime")]
#[test]
fn tb_writes_keep_unknown_bits_on_wasm() {
    tb_writes_keep_unknown_bits(|four_state, mode| {
        Simulator::builder(UNKNOWN_WRITES_TB, "Top")
            .four_state(four_state)
            .scheduler_mode(mode)
            .build_wasm()
            .unwrap()
    });
}

/// An immediate assertion of an unknown value fails (IEEE 1800-2023
/// 16.3), and its message keeps the unknown bits of a computed argument.
#[test]
fn tb_assertions_of_unknown_values_fail() {
    let source = r#"
            #[test(Top)] module Top {
                inst clk: $tb::clock_gen;
                var r: logic; var p: logic<8>;
                initial {
                    r = 'x;
                    p = 8'b0000_01x1;
                    clk.next(1);
                    $assert(r | 1, "known");
                    $assert_continue(r == 1, "eq=%b sum=%h", r == 1, p + 1);
                    $assert_continue(r, "plain");
                    $finish();
                }
            }
        "#;
    for mode in [
        crate::SchedulerMode::Reference,
        crate::SchedulerMode::Compiled,
    ] {
        let mut sim = Simulator::builder(source, "Top")
            .four_state(true)
            .scheduler_mode(mode)
            .build_native()
            .unwrap();
        let tb = compile_initial_testbench(&sim).unwrap();
        let detailed = run_testbench_detailed(&mut sim, &tb);
        let messages: Vec<_> = detailed
            .assertions
            .iter()
            .map(|assertion| {
                (
                    assertion.passed,
                    String::from_utf8(assertion.message.clone().unwrap()).unwrap(),
                )
            })
            .collect();
        assert_eq!(
            messages,
            [
                (true, "known".to_string()),
                (false, "eq=x sum=xxxxxxxx".to_string()),
                (false, "plain".to_string()),
            ],
            "{mode:?}"
        );
    }
}

// Latches, including ones that read the clock, keep TB edges in the
// generated scheduler: a TB write settles there before the edges.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn latch_designs_run_tb_edges_natively() {
    let icg = r#"
            module Icg(clk: input clock, en: input logic, count: output logic<8>) {
                var en_l: logic;
                always_comb { if !clk { en_l = en; } }
                let gclk: '_ clock = clk & en_l;
                always_ff (gclk) { count += 1; }
            }
            module Pipeline(clk: input clock, upstream: output logic<8>,
                            held: output logic<8>, downstream: output logic<8>) {
                always_ff (clk) { upstream += 1; downstream = held; }
                always_comb { if !clk { held = upstream; } }
            }
            #[test(Top)] module Top {
                inst clk: $tb::clock_gen;
                var en: logic;
                var count: logic<8>;
                var upstream: logic<8>;
                var held: logic<8>;
                var downstream: logic<8>;
                inst gate: Icg(clk, en, count);
                inst pipe: Pipeline(clk, upstream, held, downstream);
                initial {
                    en = 0;
                    clk.next(3);
                    $assert(count == 0);
                    $assert(upstream == 3 && held == 3 && downstream == 2);
                    en = 1;
                    clk.next(4);
                    $assert(count == 4);
                    en = 0;
                    clk.next(2);
                    $assert(count == 4);
                    en = 1;
                    clk.next(1);
                    $assert(count == 5);
                    $finish();
                }
            }
        "#;
    for source in [
        include_str!("../../testdata/veryl/latch_initial.veryl"),
        icg,
    ] {
        let mut sim = Simulator::builder(source, "Top")
            .allow_inferred_latches(true)
            .build_native()
            .unwrap();
        assert!(!sim.program.runtime_schema.persistent_comb_state.is_empty());
        let tb = compile_initial_testbench(&sim).unwrap();
        compiled::take_counters(&mut sim);
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass
        );
        let counters = compiled::take_counters(&mut sim);
        assert_eq!(counters.1, 0, "host edge batches: {counters:?}");
    }
}

#[test]
fn concurrent_tb_preserves_low_phase_latch_updates() {
    let source = r#"
            module Pipeline(clk: input clock, upstream: output logic<8>,
                            held: output logic<8>, downstream: output logic<8>) {
                always_ff (clk) { upstream += 1; downstream = held; }
                always_comb { if !clk { held = upstream; } }
            }
            #[test(Top)] module Top {
                inst clk: $tb::clock_gen #(period: 2);
                var upstream: logic<8>;
                var held: logic<8>;
                var downstream: logic<8>;
                inst dut: Pipeline(clk, upstream, held, downstream);
                initial {
                    clk.next(4);
                    $assert(upstream == 4);
                    $assert(held == 4);
                    $assert(downstream == 3);
                    $finish();
                }
                initial { clk.next(100); }
            }
        "#;
    let mut sim = Simulator::builder(source, "Top")
        .allow_inferred_latches(true)
        .build_interpreter()
        .unwrap();
    assert!(!sim.program.runtime_schema.persistent_comb_state.is_empty());
    let tb = compile_initial_testbench(&sim).unwrap();
    assert_eq!(
        run_compiled_testbench_to_finish(&mut sim, &tb),
        TestResult::Pass
    );
}

// Clocks that only pace TB processes, with no RTL domain, still run
// their edges in the generated scheduler.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn tb_only_clocks_run_edges_natively() {
    let source = r#"
            #[test(Top)]
            module Top {
                inst slow: $tb::clock_gen #(period: 10);
                inst fast: $tb::clock_gen #(period: 2);
                var observed: logic;
                initial {
                    slow.next(1);
                    $assert(observed == 1'd1);
                    $finish();
                }
                initial {
                    fast.next(3);
                    observed = 1'd1;
                }
            }
        "#;
    let mut sim = Simulator::builder(source, "Top").build_native().unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();
    compiled::take_counters(&mut sim);
    assert_eq!(
        run_compiled_testbench_to_finish(&mut sim, &tb),
        TestResult::Pass
    );
    let counters = compiled::take_counters(&mut sim);
    assert_eq!(counters.1, 0, "host edge batches: {counters:?}");
}

// Clock masks have their own width: one TB process can drive clock
// positions beyond either one machine word or its process-ready mask.
#[test]
fn wide_tb_clock_handoff_preserves_every_edge() {
    for reset in [false, true] {
        let count = if reset { 33 } else { 65 };
        let mut source = String::from(if reset {
            "module Counter(clk: input clock, rst: input reset_async_high, q: output bit<8>) {\n\
                 always_ff (clk, rst) { if_reset { q = 0; } else { q += 1; } } }\n"
        } else {
            "module Counter(clk: input clock, q: output bit<8>) {\n\
                 always_ff (clk) { q += 1; } }\n"
        });
        source.push_str("#[test(Top)] module Top {\n");
        for index in 0..count {
            source.push_str(&format!(
                "inst c{index}: $tb::clock_gen #(period: 2); var q{index}: bit<8>;\n"
            ));
            let reset_port = if reset {
                source.push_str(&format!("inst r{index}: $tb::reset_gen(clk: c{index});\n"));
                format!(", rst: r{index}")
            } else {
                String::new()
            };
            source.push_str(&format!(
                "inst d{index}: Counter(clk: c{index}, q: q{index}{reset_port});\n"
            ));
        }
        source.push_str("initial {\n");
        if reset {
            // Normalize initial X levels together; subsequent assertions
            // must exercise the native reset-mask handoff.
            for index in 0..count {
                source.push_str(&format!("r{index} = 0;\n"));
            }
        }
        for index in 0..count {
            if reset {
                source.push_str(&format!("c{index}.next(1); r{index}.assert(1);\n"));
            }
            source.push_str(&format!("c{index}.next(1);\n"));
        }
        // A subsequent handoff must clear all other bits too.
        source.push_str("c0.next(2);\n");
        for index in 0..count {
            let expected = if index == 0 { 3 } else { 1 };
            source.push_str(&format!("$assert(q{index} == 8'd{expected});\n"));
        }
        source.push_str("$finish(); } }");
        fn check<B: SimBackend>(
            mut sim: Simulator<B>,
            generated: bool,
            reset: bool,
            four_state: bool,
        ) {
            let scheduler = sim.program.runtime_schema.scheduler.as_ref().unwrap();
            assert!(
                scheduler.clocks.len() > 64,
                "reset={reset}: {} slots",
                scheduler.clocks.len()
            );
            assert_eq!(sim.backend.resolve_signal(&scheduler.slots.ready).width, 64);
            let meta = sim.program.testbench.as_ref().unwrap().scheduler().unwrap();
            let clock_signals: Vec<_> = meta
                .events
                .iter()
                .map(|event| sim.backend.resolve_signal(event))
                .collect();
            assert!(
                scheduler.clocks.iter().enumerate().any(|(bit, clock)| {
                    bit >= 64 && clock_signals.contains(&sim.backend.resolve_signal(clock)) != reset
                }),
                "the fixture must exercise an upper {} bit",
                if reset { "reset" } else { "clock" }
            );
            let tb = compile_initial_testbench(&sim).unwrap();
            compiled::take_counters(&mut sim);
            assert_eq!(
                run_compiled_testbench_to_finish(&mut sim, &tb),
                TestResult::Pass
            );
            if generated {
                let counters = compiled::take_counters(&mut sim);
                assert!(counters.0 > 0, "compiled TB did not run: {counters:?}");
                assert_eq!(
                    counters.1,
                    usize::from(reset && four_state),
                    "only initial unknown reset levels may use host edges: {counters:?}"
                );
            }
        }
        for four_state in [false, true] {
            let build = || {
                Simulator::builder(&source, "Top")
                    .reset_type(crate::ResetType::AsyncHigh)
                    .four_state(four_state)
            };
            check(build().build_native().unwrap(), true, reset, four_state);
            check(
                build().build_interpreter().unwrap(),
                false,
                reset,
                four_state,
            );
            #[cfg(feature = "cranelift")]
            check(build().build_cranelift().unwrap(), true, reset, four_state);
            // This large inlined TB exceeds wasmparser's 50,000-local
            // function limit. Smaller clock/reset handoffs are covered
            // by the four_state_tb_edges_* tests on Wasm too.
        }
    }
}

#[test]
fn concurrent_tb_publishes_each_clock_phase_to_vcd() {
    let source = r#"
            module Counter(clk: input clock, q: output logic<8>) {
                always_ff (clk) { q += 1; }
            }
            #[test(Top)] module Top {
                inst clk: $tb::clock_gen #(period: 2);
                var q: logic<8>;
                inst dut: Counter(clk, q);
                initial { clk.next(4); $assert(q == 4); $finish(); }
                initial { clk.next(100); }
            }
        "#;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("concurrent.vcd");
    let mut sim = Simulator::builder(source, "Top")
        .vcd(&path)
        .build_interpreter()
        .unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();
    assert_eq!(
        run_compiled_testbench_to_finish(&mut sim, &tb),
        TestResult::Pass
    );
    sim.flush_vcd().unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    // Native edges publish each time step the same way.
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        let native_path = directory.path().join("native.vcd");
        let mut sim = Simulator::builder(source, "Top")
            .vcd(&native_path)
            .build_native()
            .unwrap();
        let tb = compile_initial_testbench(&sim).unwrap();
        compiled::take_counters(&mut sim);
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass
        );
        let counters = compiled::take_counters(&mut sim);
        assert!(
            counters.0 > 0,
            "compiled scheduler did not run: {counters:?}"
        );
        assert_eq!(counters.1, 0, "host edge batches: {counters:?}");
        sim.flush_vcd().unwrap();
        let native = std::fs::read_to_string(&native_path).unwrap();
        let body = |text: &str| text.split_once("$enddefinitions").unwrap().1.to_string();
        assert_eq!(body(&native), body(&text));
    }
    let timestamps: Vec<_> = text
        .lines()
        .filter_map(|line| line.strip_prefix('#'))
        .map(|time| time.parse::<u64>().unwrap())
        .collect();
    for falling_edge in [1, 3, 5, 7] {
        assert!(
            timestamps.contains(&falling_edge),
            "missing low phase {falling_edge}: {timestamps:?}"
        );
    }
}

// A `$finish` an RTL domain runs on a native edge still publishes the
// terminating time step to the VCD.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn rtl_finish_on_native_edge_publishes_terminal_step() {
    let source = r#"
            module Counter(clk: input clock, q: output logic<8>) {
                always_ff (clk) {
                    q += 1;
                    if q == 3 { $finish(); }
                }
            }
            #[test(Top)] module Top {
                inst clk: $tb::clock_gen #(period: 2);
                var q: logic<8>;
                inst dut: Counter(clk, q);
                initial { clk.next(100); }
            }
        "#;
    let directory = tempfile::tempdir().unwrap();
    let unpublished = {
        let mut sim = Simulator::builder(source, "Top").build_native().unwrap();
        let tb = compile_initial_testbench(&sim).unwrap();
        run_compiled_testbench_with_tick_limit(&mut sim, &tb, 1000)
    };
    assert_eq!(unpublished.result, TestResult::Pass);
    let mut bodies = Vec::new();
    for native in [false, true] {
        let path = directory.path().join(format!("{native}.vcd"));
        let builder = Simulator::builder(source, "Top").vcd(&path);
        let result = if native {
            let mut sim = builder.build_native().unwrap();
            let tb = compile_initial_testbench(&sim).unwrap();
            compiled::take_counters(&mut sim);
            let result = run_compiled_testbench_with_tick_limit(&mut sim, &tb, 1000);
            let counters = compiled::take_counters(&mut sim);
            assert_eq!(counters.1, 0, "host edge batches: {counters:?}");
            sim.flush_vcd().unwrap();
            // Publishing the step does not drop its tick.
            assert_eq!(result, unpublished);
            result.result
        } else {
            let mut sim = builder.build_interpreter().unwrap();
            let tb = compile_initial_testbench(&sim).unwrap();
            let result = run_compiled_testbench_to_finish(&mut sim, &tb);
            sim.flush_vcd().unwrap();
            result
        };
        assert_eq!(result, TestResult::Pass);
        let text = std::fs::read_to_string(&path).unwrap();
        bodies.push(text.split_once("$enddefinitions").unwrap().1.to_string());
    }
    assert_eq!(bodies[1], bodies[0]);
    assert!(bodies[0].contains("#6\n"), "{}", bodies[0]);
}

#[test]
fn constant_clock_wait_loop_binds_as_one_wait() {
    let source = r#"
            module Counter(clk: input clock, q: output logic<32>) {
                always_ff (clk) { q += 1; }
            }
            #[test(Top)] module Top {
                inst clk: $tb::clock_gen;
                var q: logic<32>;
                inst dut: Counter(clk, q);
                initial {
                    for _i in 0..(2 * (2 + 2)) { clk.next(1); }
                    $assert(q == 8);
                    $finish();
                }
            }
        "#;
    let mut sim = Simulator::builder(source, "Top")
        .build_interpreter()
        .unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();
    assert!(matches!(
        tb.statements().first(),
        Some(GenericTestbenchStatement::ClockNext {
            count: ClockCount::Static(8),
            ..
        })
    ));
    assert_eq!(
        run_compiled_testbench_to_finish(&mut sim, &tb),
        TestResult::Pass,
        "q={}, counters={:?}",
        sim.get_as::<u32>(sim.signal("q")),
        compiled::take_counters(&mut sim)
    );
}

#[test]
fn compiled_reads_share_settlement_until_the_next_hardware_write() {
    let mut sim = Simulator::builder(
        r#"
            #[test(Top)] module Top {
                var a: logic<8>; var q: logic<8>;
                var x: logic<8>; var y: logic<8>; var z: logic<8>;
                assign q = a + 1;
                initial {
                    a = 1;
                    x = q;
                    y = q;
                    a = 7;
                    z = q;
                    $assert(x == 2 && y == 2 && z == 8);
                    $finish();
                }
            }
        "#,
        "Top",
    )
    .build()
    .unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();
    let settlements = tb.kernels()[0]
        .requests
        .iter()
        .filter(|request| matches!(request, celox_testbench::KernelRequest::Settle { .. }))
        .count();
    assert_eq!(settlements, 2);
    assert_eq!(
        run_compiled_testbench_to_finish(&mut sim, &tb),
        TestResult::Pass
    );
}

#[test]
fn compiled_kernel_stores_invalidate_settled_hardware() {
    let mut sim = Simulator::builder(
        r#"
            #[test(Top)] module Top {
                var a: logic<8>; var q: logic<8>;
                assign q = a + 1;
                initial { a = 7; $finish(); }
            }
        "#,
        "Top",
    )
    .dead_store_policy(crate::DeadStorePolicy::Off)
    .build_interpreter()
    .unwrap();
    let q = sim.signal("q");
    let tb = compile_initial_testbench(&sim).unwrap();
    settle_testbench(&mut sim).unwrap();
    let GenericTestbenchStatement::Assign { expr, .. } = &tb.statements()[0] else {
        panic!("assignment")
    };
    assert_eq!(eval_expr(&mut sim, expr).to_u64(), 7);
    assert!(!sim.dirty);
    assert_eq!(
        run_compiled_testbench_to_finish(&mut sim, &tb),
        TestResult::Pass
    );
    assert!(sim.dirty);
    settle_testbench(&mut sim).unwrap();
    assert_eq!(sim.get_as::<u8>(q), 8);
}

#[test]
fn compiled_kernel_stores_notify_vcd_without_exposing_memory() {
    let dir = tempfile::tempdir().unwrap();
    let mut sim = Simulator::builder(
        r#"
            #[test(Top)] module Top {var a: logic<8>; initial {a=7; $finish();}}
        "#,
        "Top",
    )
    .vcd(dir.path().join("expression.vcd"))
    .build()
    .unwrap();
    let a = sim.signal("a");
    let tb = compile_initial_testbench(&sim).unwrap();
    sim.dump(0);
    let before = sim.vcd_statistics().unwrap();
    assert_eq!(
        run_compiled_testbench_to_finish(&mut sim, &tb),
        TestResult::Pass
    );
    assert_eq!(sim.get_as::<u8>(a), 7);
    sim.dump(1);
    let after = sim.vcd_statistics().unwrap();
    assert_eq!(after.changes, before.changes + 1);
    sim.dump(2);
    assert_eq!(sim.vcd_statistics().unwrap().comparisons, after.comparisons);
}

#[test]
fn compiles_initial_display_and_write() {
    let code = r#"
            #[test(t)]
            module t {
                initial {
                    $display("answer=%h", 8'h2a);
                    $write("!");
                    $finish();
                }
            }
        "#;
    let sim = Simulator::builder(code, "t").build_with_trace().unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();

    assert!(matches!(
        tb.statements().first(),
        Some(GenericTestbenchStatement::Display { newline: true, .. })
    ));
    assert!(matches!(
        tb.statements().get(1),
        Some(GenericTestbenchStatement::Display { newline: false, .. })
    ));
    assert!(matches!(
        tb.statements().get(2),
        Some(GenericTestbenchStatement::Finish)
    ));
}

#[test]
fn limited_runner_stops_without_completing_the_testbench() {
    let code = r#"
            #[test(t)]
            module t {
                inst clk: $tb::clock_gen;
                initial {
                    clk.next(10);
                    $finish();
                }
            }
        "#;
    let mut sim = Simulator::builder(code, "t").build_with_trace().unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();

    assert_eq!(
        run_compiled_testbench_with_tick_limit(&mut sim, &tb, 3),
        LimitedTestbenchResult {
            result: TestResult::Pass,
            ticks: 3,
            tick_limit_reached: true,
        }
    );
}
#[test]
fn zero_tick_limit_does_not_evaluate_compiled_control_flow() {
    let source = r#"
            #[test(Top)] module Top {
                var touched: u32;
                function touch() -> u32 { touched = 1; return 1; }
                initial { if touch() == 1 { $finish(); } }
            }
        "#;
    let mut sim = Simulator::builder(source, "Top")
        .build_interpreter()
        .unwrap();
    let touched = sim.signal("touched");
    let program = compile_initial_testbench(&sim).unwrap();
    let result = run_compiled_testbench_with_tick_limit(&mut sim, &program, 0);
    assert!(result.tick_limit_reached);
    assert_eq!(result.ticks, 0);
    assert_eq!(sim.get_as::<u32>(touched), 0);
}
