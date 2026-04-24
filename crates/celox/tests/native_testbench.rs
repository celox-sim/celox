use celox::{ResetType, Simulator, TestResult};

const COUNTER: &str = r#"
    module Counter (
        clk: input  clock    ,
        rst: input  reset    ,
        cnt: output logic<32>,
    ) {
        always_ff {
            if_reset {
                cnt = 0;
            } else {
                cnt += 1;
            }
        }
    }
"#;

// ── Basic ──────────────────────────────────────────────────────────────

#[test]
fn test_counter_pass() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next  (10);
                $assert   (cnt == 32'd10);
                $finish   ();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_counter_fail() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next  (5);
                $assert   (cnt == 32'd99);
                $finish   ();
            }}
        }}
    "#
    );
    assert!(matches!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Fail(_),
    ));
}

// ── Wide signal (>64 bit) ──────────────────────────────────────────────

#[test]
fn test_wide_128bit() {
    let code = r#"
        module W (
            clk: input  clock      ,
            rst: input  reset      ,
            cnt: output logic<128> ,
        ) {
            always_ff {
                if_reset { cnt = 0; }
                else     { cnt += 1; }
            }
        }
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<128>;
            inst dut: W (clk, rst, cnt);
            initial {
                rst.assert(clk);
                clk.next  (5);
                $assert   (cnt == 128'd5);
                $finish   ();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

// ── Reset polarity ─────────────────────────────────────────────────────

/// DUT uses generic `reset` type; builder overrides to AsyncHigh.
#[test]
fn test_reset_async_high() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next  (7);
                $assert   (cnt == 32'd7);
                $finish   ();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t")
            .reset_type(ResetType::AsyncHigh)
            .run_test()
            .unwrap(),
        TestResult::Pass,
    );
}

// ── Reset duration ─────────────────────────────────────────────────────

#[test]
fn test_reset_explicit_duration() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk, 5);
                clk.next  (10);
                $assert   (cnt == 32'd10);
                $finish   ();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

// ── For loop ───────────────────────────────────────────────────────────

#[test]
fn test_for_loop_basic() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next(10);
                $assert(cnt == 32'd10);
                for _i in 0..5 {{
                    clk.next();
                }}
                $assert(cnt == 32'd15);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_step() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next(10);
                $assert(cnt == 32'd10);
                for _i in 0..10 step += 2 {{
                    clk.next(2);
                }}
                $assert(cnt == 32'd20);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_rev() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next(10);
                $assert(cnt == 32'd10);
                for _i in rev 0..5 {{
                    clk.next();
                }}
                $assert(cnt == 32'd15);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_break_exits_testbench_loop() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                for i in 0..10 {{
                    if i == 3 {{
                        break;
                    }}
                    clk.next();
                }}
                $assert(cnt == 32'd3);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_expression_bound_forward() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next(10);
                for _i in 0..(cnt >> 1) {{
                    clk.next();
                }}
                $assert(cnt == 32'd15);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_expression_bound_reverse() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next(10);
                for _i in rev 0..(cnt >> 1) {{
                    clk.next();
                }}
                $assert(cnt == 32'd15);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_expression_bound_inclusive() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next(3);
                for _i in 0..=(cnt + 32'd2) {{
                    clk.next();
                }}
                $assert(cnt == 32'd9);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_expression_bound_stepped() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next(10);
                for _i in 1..(cnt >> 1) step *= 2 {{
                    clk.next();
                }}
                $assert(cnt == 32'd13);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_expression_bound_stepped_non_progress_fails() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next(10);
                for _i in 1..cnt step *= 1 {{
                    clk.next();
                }}
                $assert(cnt == 32'd11);
                $finish();
            }}
        }}
    "#
    );
    assert!(matches!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Fail(_),
    ));
}

#[test]
fn test_for_loop_expression_bound_arith_shift_step() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next(10);
                for _i in 1..(cnt >> 1) step <<<= 1 {{
                    clk.next();
                }}
                $assert(cnt == 32'd13);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_expression_bound_large_arith_shift_stops_after_first_iteration() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next(10);
                for _i in 1..cnt step <<<= 100 {{
                    clk.next();
                }}
                $assert(cnt == 32'd11);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_expression_bound_non_progress_reports_failure() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next(10);
                for _i in 1..cnt step *= 1 {{
                    clk.next();
                }}
                $finish();
            }}
        }}
    "#
    );
    assert!(matches!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Fail(_),
    ));
}

#[test]
fn test_for_loop_expression_bound_terminal_inclusive_mul_succeeds() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next(10);
                for _i in 0..=0 step *= 2 {{
                    clk.next();
                }}
                $assert(cnt == 32'd11);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
#[ignore = "upstream Veryl does not preserve reverse zero-step loops into the native testbench IR path exercised by this test"]
fn test_for_loop_expression_bound_reverse_zero_step_singleton_succeeds() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next(10);
                for _i in rev 4..=4 step += 0 {{
                    clk.next();
                }}
                $assert(cnt == 32'd11);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_dynamic_wide_bound_overflow_reports_failure() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            var bound: logic<128>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next(10);
                bound = 128'd1;
                for _i in 0..(bound << 64) {{
                    clk.next();
                }}
                $finish();
            }}
        }}
    "#
    );
    assert!(matches!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Fail(_),
    ));
}

#[test]
fn test_for_loop_dynamic_signed_bound_preserves_negative_value() {
    let code = r#"
        #[test(t)]
        module t {
            var start: signed logic<32>;
            var hits: logic<32>;
            initial {
                start = (0 - 1) as 32;
                hits = 0;
                for _i in start..=1 {
                    hits += 1;
                }
                $assert(hits == 32'd3);
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_dynamic_inclusive_max_bound_runs_terminal_iteration() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            var bound: logic<64>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next(10);
                bound = 64'hffff_ffff_ffff_ffff;
                for _i in bound..=bound {{
                    clk.next();
                }}
                $assert(cnt == 32'd11);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_for_loop_dynamic_wide_singleton_bound_runs_once() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            var bound: logic<128>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next(10);
                bound = (128'd1 << 100);
                for _i in bound..=bound {{
                    clk.next();
                }}
                $assert(cnt == 32'd11);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

// ── Function call in testbench ──────────────────────────────────────────

#[test]
fn test_function_call() {
    let code = r#"
        module Counter2 (
            clk: input clock,
            rst: input reset,
            cnt: output logic<32>,
        ) {
            always_ff {
                if_reset { cnt = 0; }
                else { cnt += 1; }
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter2 (clk, rst, cnt);

            function step_n(n: input logic<32>) {
                clk.next(n);
            }

            initial {
                rst.assert(clk);
                step_n(5);
                step_n(5);
                $assert(cnt == 32'd10);
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

/// Factor::FunctionCall — function return value used in an expression.
#[test]
fn test_function_return_value_in_assert() {
    let code = r#"
        module Dut (
            clk: input  clock    ,
            rst: input  reset    ,
            val: output logic<8> ,
        ) {
            always_ff {
                if_reset { val = 0; }
                else     { val = 8'd42; }
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var val: logic<8>;
            inst dut: Dut (clk, rst, val);

            function double(x: input logic<8>) -> logic<8> {
                return x + x;
            }

            function add_offset(x: input logic<8>, offset: input logic<8>) -> logic<8> {
                return x + offset;
            }

            initial {
                rst.assert(clk);
                clk.next(1);
                $assert(val == 8'd42);
                $assert(double(val) == 8'd84);
                $assert(add_offset(val, 8'd8) == 8'd50);
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

// ── Dual clock ─────────────────────────────────────────────────────────

#[test]
fn test_dual_clock() {
    let code = r#"
        module DualClock (
            clk_a: input  'a clock    ,
            rst_a: input  'a reset    ,
            clk_b: input  'b clock    ,
            rst_b: input  'b reset    ,
            cnt_a: output 'a logic<32>,
            cnt_b: output 'b logic<32>,
        ) {
            always_ff (clk_a, rst_a) {
                if_reset { cnt_a = 0; }
                else     { cnt_a += 1; }
            }
            always_ff (clk_b, rst_b) {
                if_reset { cnt_b = 0; }
                else     { cnt_b += 1; }
            }
        }

        #[test(t)]
        module t {
            inst clk_a: $tb::clock_gen;
            inst rst_a: $tb::reset_gen(clk: clk_a);
            inst clk_b: $tb::clock_gen;
            inst rst_b: $tb::reset_gen(clk: clk_b);

            var cnt_a: logic<32>;
            var cnt_b: logic<32>;

            inst dut: DualClock (
                clk_a, rst_a, clk_b, rst_b, cnt_a, cnt_b,
            );

            initial {
                rst_a.assert(clk_a);
                rst_b.assert(clk_b);
                clk_a.next  (10);
                $assert     (cnt_a == 32'd10);
                $assert     (cnt_b == 32'd0);
                clk_b.next  (5);
                $assert     (cnt_a == 32'd10);
                $assert     (cnt_b == 32'd5);
                $finish     ();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

// ── Implicit $finish (no $finish → Pass) ───────────────────────────────

#[test]
fn test_no_finish_is_pass() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next(3);
                $assert(cnt == 32'd3);
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

// ── Dynamic indexing ───────────────────────────────────────────────────

#[test]
fn test_dynamic_array_index_in_for() {
    let code = r#"
        module ArrayFill (
            clk: input  clock         ,
            rst: input  reset         ,
            arr: output logic<8>   [4],
        ) {
            for i in 0..4: g {
                always_ff {
                    if_reset { arr[i] = 0; }
                    else     { arr[i] = arr[i] + i as u8 + 8'd10; }
                }
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var arr: logic<8>[4];
            inst dut: ArrayFill (clk, rst, arr);
            initial {
                rst.assert(clk);
                clk.next(1);
                // arr[0]=10, arr[1]=11, arr[2]=12, arr[3]=13
                for i in 0..4 {
                    $assert(arr[i] == i as u8 + 8'd10);
                }
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

// ── Multiple assertions ────────────────────────────────────────────────

#[test]
fn test_multiple_assertions() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                $assert(cnt == 32'd0);
                clk.next(1);
                $assert(cnt == 32'd1);
                clk.next(1);
                $assert(cnt == 32'd2);
                clk.next(8);
                $assert(cnt == 32'd10);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_assert_continue_records_failure_and_continues() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                $assert_continue(cnt == 32'd99, "first failure: cnt=%d", cnt);
                clk.next(1);
                $assert(cnt == 32'd1, "second assertion");
                $finish();
            }}
        }}
    "#
    );
    let detailed = Simulator::builder(&code, "t").run_test_detailed().unwrap();
    assert!(!detailed.passed);
    assert_eq!(detailed.assertions.len(), 2);
    assert!(!detailed.assertions[0].passed);
    assert_eq!(
        detailed.assertions[0].message.as_deref(),
        Some("first failure: cnt=0"),
    );
    assert!(detailed.assertions[1].passed);

    let result = Simulator::builder(&code, "t").run_test().unwrap();
    assert_eq!(result, TestResult::Fail("first failure: cnt=0".to_string()));
}

#[test]
fn test_assert_format_args_render_runtime_values() {
    let code = r#"
        #[test(t)]
        module t {
            initial {
                $assert_continue(1'b0, "mismatch: a=%d b=%d", 8'd3, 8'd7);
                $finish();
            }
        }
    "#;
    let detailed = Simulator::builder(code, "t").run_test_detailed().unwrap();
    assert!(!detailed.passed);
    assert_eq!(detailed.assertions.len(), 1);
    assert_eq!(
        detailed.assertions[0].message.as_deref(),
        Some("mismatch: a=3 b=7"),
    );
}

#[test]
fn test_assert_format_args_follow_veryl_single_char_specifiers() {
    let code = r#"
        #[test(t)]
        module t {
            initial {
                $assert_continue(1'b0, "cnt=%0d hex=%08x", 8'd3, 8'h0f);
                $finish();
            }
        }
    "#;
    let detailed = Simulator::builder(code, "t").run_test_detailed().unwrap();
    assert!(!detailed.passed);
    assert_eq!(detailed.assertions.len(), 1);
    assert_eq!(
        detailed.assertions[0].message.as_deref(),
        Some("cnt=%0d hex=%08x"),
    );
}

#[test]
fn test_run_test_detailed_collects_multiple_plain_assert_failures() {
    let code = r#"
        #[test(t)]
        module t {
            initial {
                $assert(1'b0, "first");
                $assert(1'b0, "second");
                $finish();
            }
        }
    "#;
    let detailed = Simulator::builder(code, "t").run_test_detailed().unwrap();
    assert!(!detailed.passed);
    assert_eq!(detailed.assertions.len(), 2);
    assert!(!detailed.assertions[0].passed);
    assert_eq!(detailed.assertions[0].message.as_deref(), Some("first"));
    assert!(!detailed.assertions[1].passed);
    assert_eq!(detailed.assertions[1].message.as_deref(), Some("second"));
}

// ── Array and bit select ───────────────────────────────────────────────

#[test]
fn test_unpacked_array_index() {
    let code = r#"
        module ArrayCounter (
            clk: input  clock         ,
            rst: input  reset         ,
            cnt: output logic<8>   [4],
        ) {
            for i in 0..4: g {
                always_ff {
                    if_reset { cnt[i] = 0; }
                    else     { cnt[i] = cnt[i] + i as u8 + 8'd1; }
                }
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<8>[4];
            inst dut: ArrayCounter (clk, rst, cnt);
            initial {
                rst.assert(clk);
                clk.next(1);
                $assert(cnt[0] == 8'd1);
                $assert(cnt[1] == 8'd2);
                $assert(cnt[2] == 8'd3);
                $assert(cnt[3] == 8'd4);
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_bit_select() {
    let code = r#"
        module BitSel (
            clk: input  clock    ,
            rst: input  reset    ,
            val: output logic<16>,
        ) {
            always_ff {
                if_reset { val = 0; }
                else     { val = 16'hABCD; }
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var val: logic<16>;
            inst dut: BitSel (clk, rst, val);
            initial {
                rst.assert(clk);
                clk.next(1);
                $assert(val == 16'hABCD);
                $assert(val[7:0] == 8'hCD);
                $assert(val[15:8] == 8'hAB);
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

// ── Concatenation ──────────────────────────────────────────────────────

#[test]
fn test_concatenation() {
    let code = r#"
        module ConcatDut (
            clk: input  clock    ,
            rst: input  reset    ,
            hi:  output logic<8> ,
            lo:  output logic<8> ,
        ) {
            always_ff {
                if_reset { hi = 0; lo = 0; }
                else     { hi = 8'hAB; lo = 8'hCD; }
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var hi: logic<8>;
            var lo: logic<8>;
            inst dut: ConcatDut (clk, rst, hi, lo);
            initial {
                rst.assert(clk);
                clk.next(1);
                $assert({hi, lo} == 16'hABCD);
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_repeat_concatenation() {
    let code = r#"
        module RepDut (
            clk: input  clock    ,
            rst: input  reset    ,
            val: output logic<4> ,
        ) {
            always_ff {
                if_reset { val = 0; }
                else     { val = 4'b1010; }
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var val: logic<4>;
            inst dut: RepDut (clk, rst, val);
            initial {
                rst.assert(clk);
                clk.next(1);
                $assert({val repeat 2} == 8'b1010_1010);
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

// ── Operators in assertions ────────────────────────────────────────────

#[test]
fn test_comparison_operators() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next(5);
                $assert(cnt == 32'd5);
                $assert(cnt != 32'd0);
                $assert(cnt >: 32'd4);
                $assert(cnt >= 32'd5);
                $assert(cnt <: 32'd6);
                $assert(cnt <= 32'd5);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}

#[test]
fn test_arithmetic_in_assert() {
    let code = format!(
        r#"
        {COUNTER}
        #[test(t)]
        module t {{
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<32>;
            inst dut: Counter (clk, rst, cnt);
            initial {{
                rst.assert(clk);
                clk.next(10);
                $assert(cnt + 32'd5 == 32'd15);
                $assert(cnt - 32'd3 == 32'd7);
                $finish();
            }}
        }}
    "#
    );
    assert_eq!(
        Simulator::builder(&code, "t").run_test().unwrap(),
        TestResult::Pass,
    );
}
