use celox::{ResetType, Simulator, TestResult};

#[test]
fn test_counter_native_tb() {
    let code = r#"
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

        #[test(test_counter)]
        module test_counter {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen;

            var cnt: logic<32>;

            inst dut: Counter (
                clk: clk,
                rst: rst,
                cnt: cnt,
            );

            initial {
                rst.assert(clk);
                clk.next  (10);
                $assert   (cnt == 32'd10);
                $finish   ();
            }
        }
    "#;

    let result = Simulator::builder(code, "test_counter")
        .run_test()
        .unwrap();

    assert_eq!(result, TestResult::Pass);
}

#[test]
fn test_assert_failure() {
    let code = r#"
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

        #[test(test_fail)]
        module test_fail {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen;

            var cnt: logic<32>;

            inst dut: Counter (
                clk: clk,
                rst: rst,
                cnt: cnt,
            );

            initial {
                rst.assert(clk);
                clk.next  (5);
                $assert   (cnt == 32'd99);
                $finish   ();
            }
        }
    "#;

    let result = Simulator::builder(code, "test_fail")
        .run_test()
        .unwrap();

    assert!(matches!(result, TestResult::Fail(_)));
}

#[test]
fn test_wide_signal() {
    let code = r#"
        module WideCounter (
            clk: input  clock      ,
            rst: input  reset      ,
            cnt: output logic<128> ,
        ) {
            always_ff {
                if_reset {
                    cnt = 0;
                } else {
                    cnt += 1;
                }
            }
        }

        #[test(test_wide)]
        module test_wide {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen;

            var cnt: logic<128>;

            inst dut: WideCounter (
                clk: clk,
                rst: rst,
                cnt: cnt,
            );

            initial {
                rst.assert(clk);
                clk.next  (5);
                $assert   (cnt == 128'd5);
                $finish   ();
            }
        }
    "#;

    let result = Simulator::builder(code, "test_wide")
        .run_test()
        .unwrap();

    assert_eq!(result, TestResult::Pass);
}

/// Verify that `rst.assert()` correctly drives active-high polarity
/// when the project reset type is configured as `AsyncHigh`.
/// The DUT uses generic `reset` type, resolved by `.reset_type()`.
#[test]
fn test_reset_async_high_polarity() {
    let code = r#"
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

        #[test(test_polarity)]
        module test_polarity {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen;

            var cnt: logic<32>;

            inst dut: Counter (
                clk: clk,
                rst: rst,
                cnt: cnt,
            );

            initial {
                rst.assert(clk);
                clk.next  (7);
                $assert   (cnt == 32'd7);
                $finish   ();
            }
        }
    "#;

    let result = Simulator::builder(code, "test_polarity")
        .reset_type(ResetType::AsyncHigh)
        .run_test()
        .unwrap();

    assert_eq!(result, TestResult::Pass);
}
