//! Non-constant `string` values in RTL and operations on f32/f64 values are
//! rejected until Celox represents them (#1400, #1401). Constants folded by
//! the analyzer and plain floating-point copies keep working.
use celox::{ParserError, Simulator, SimulatorErrorKind};

#[track_caller]
fn assert_unsupported(code: &str, issue: u32, feature: &str) {
    let err = Simulator::builder(code, "Top")
        .build()
        .expect_err("expected the design to be rejected");
    match err.kind() {
        SimulatorErrorKind::SIRParser(ParserError::Unsupported {
            issue: actual_issue,
            feature: actual_feature,
            ..
        }) => {
            assert_eq!(*actual_issue, issue, "{err}");
            assert_eq!(*actual_feature, feature, "{err}");
        }
        other => panic!("expected an unsupported error, got: {other:?}"),
    }
}

const STRING: u32 = 1400;
const FLOAT: u32 = 1401;

#[test]
fn rtl_string_variable_assignment_is_rejected() {
    assert_unsupported(
        r#"
        module Top (o: output logic) {
            var s: string;
            assign s = "abc";
            assign o = 1;
        }
        "#,
        STRING,
        "runtime string value",
    );
}

#[test]
fn rtl_string_let_read_is_rejected() {
    assert_unsupported(
        r#"
        module Top (o: output logic) {
            let s: string = "abc";
            assign o = s == "abc";
        }
        "#,
        STRING,
        "runtime string value",
    );
}

#[test]
fn ff_string_display_is_rejected() {
    assert_unsupported(
        r#"
        module Top (clk: input clock) {
            var s: string;
            always_ff (clk) {
                $display("%s", s);
            }
        }
        "#,
        STRING,
        "runtime string value",
    );
}

#[test]
fn string_read_in_function_called_from_rtl_is_rejected() {
    assert_unsupported(
        r#"
        module Top (o: output logic) {
            function is_abc (s: input string) -> logic {
                return s == "abc";
            }
            var t: string;
            assign o = is_abc(t);
        }
        "#,
        STRING,
        "runtime string value",
    );
}

#[test]
fn constant_strings_in_rtl_are_accepted() {
    let mut sim = Simulator::builder(
        r#"
        module Top #(
            param S: string = "X",
        ) (o: output logic) {
            const C: string = "X";
            let l: string = "unused";
            assign o = S == C;
        }
        "#,
        "Top",
    )
    .build()
    .unwrap();
    let o = sim.signal("o");
    assert_eq!(sim.get(o), 1u64.into());
}

#[test]
fn testbench_strings_are_accepted() {
    let result = Simulator::builder(
        r#"
        #[test(Top)]
        module Top {
            var s: string;
            initial {
                s = "abc";
                $assert(s == "abc");
                $finish();
            }
        }
        "#,
        "Top",
    )
    .run_test_detailed()
    .unwrap();
    assert!(result.passed);
}

#[test]
fn rtl_float_arithmetic_is_rejected() {
    assert_unsupported(
        r#"
        module Top (a: input f64, b: input f64, c: output f64) {
            assign c = a + b;
        }
        "#,
        FLOAT,
        "floating-point operation",
    );
}

#[test]
fn rtl_float_comparison_is_rejected() {
    assert_unsupported(
        r#"
        module Top (a: input f64, o: output logic) {
            assign o = a == 1.0;
        }
        "#,
        FLOAT,
        "floating-point operation",
    );
}

#[test]
fn float_to_integer_cast_is_rejected() {
    assert_unsupported(
        r#"
        module Top (a: input f64, o: output u32) {
            assign o = a as u32;
        }
        "#,
        FLOAT,
        "floating-point conversion",
    );
}

#[test]
fn f32_to_f64_cast_is_rejected() {
    assert_unsupported(
        r#"
        module Top (a: input f32, o: output f64) {
            assign o = a as f64;
        }
        "#,
        FLOAT,
        "floating-point conversion",
    );
}

#[test]
fn implicit_integer_to_float_assignment_is_rejected() {
    // Even a constant is stored as its integer bits, not as 3.0.
    assert_unsupported(
        r#"
        module Top (o: output f64) {
            assign o = 3;
        }
        "#,
        FLOAT,
        "floating-point conversion",
    );
}

#[test]
fn float_port_connected_to_integer_is_rejected() {
    assert_unsupported(
        r#"
        module Child (i: input f64, o: output f64) {
            assign o = i;
        }
        module Top (a: input u32, o: output f64) {
            inst u: Child (i: a, o);
        }
        "#,
        FLOAT,
        "floating-point conversion",
    );
}

#[test]
fn ff_float_condition_is_rejected() {
    assert_unsupported(
        r#"
        module Top (clk: input clock, a: input f64, o: output logic) {
            always_ff (clk) {
                if a <: 0.5 {
                    o = 1;
                }
            }
        }
        "#,
        FLOAT,
        "floating-point operation",
    );
}

#[test]
fn testbench_float_arithmetic_is_rejected() {
    assert_unsupported(
        r#"
        #[test(Top)]
        module Top {
            var a: f64;
            var c: f64;
            initial {
                a = 1.5;
                c = a * 2.0;
                $finish();
            }
        }
        "#,
        FLOAT,
        "floating-point operation",
    );
}

#[test]
fn float_display_is_rejected() {
    assert_unsupported(
        r#"
        #[test(Top)]
        module Top {
            var a: f64;
            initial {
                a = 1.5;
                $display("%f", a);
                $finish();
            }
        }
        "#,
        FLOAT,
        "floating-point formatting",
    );
}

#[test]
fn float_copies_and_constants_are_accepted() {
    let mut sim = Simulator::builder(
        r#"
        module Child (i: input f64, o: output f64) {
            assign o = i;
        }
        module Top (clk: input clock, a: input f64, q: output f64, sum: output f64) {
            var w: f64;
            inst u: Child (i: a, o: w);
            always_ff (clk) {
                q = w;
            }
            assign sum = 1.5 + 2.25;
        }
        "#,
        "Top",
    )
    .build()
    .unwrap();
    let clk = sim.event("clk");
    let a = sim.signal("a");
    let q = sim.signal("q");
    let sum = sim.signal("sum");
    sim.modify(|io| io.set(a, 2.75f64.to_bits())).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 2.75f64.to_bits().into());
    assert_eq!(sim.get(sum), 3.75f64.to_bits().into());
}
