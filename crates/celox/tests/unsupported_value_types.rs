//! Unsupported string/float diagnostics and regressions for supported scalar floats.
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
fn scalar_float_operations_are_supported() {
    let mut sim = Simulator::builder(
        r#"
        module Top (a: input f64, b: input f32, sum: output f64,
                    equal: output logic, rounded: output u32, constant: output f64) {
            assign sum = a + (b as f64);
            assign equal = a == 1.5;
            assign rounded = a as u32;
            assign constant = 3;
        }
        "#,
        "Top",
    )
    .build()
    .unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    sim.modify(|io| {
        io.set(a, 1.5f64.to_bits());
        io.set(b, 2.25f32.to_bits());
    })
    .unwrap();
    assert_eq!(sim.get(sim.signal("sum")), 3.75f64.to_bits().into());
    assert_eq!(sim.get(sim.signal("equal")), 1u32.into());
    assert_eq!(sim.get(sim.signal("rounded")), 2u32.into());
    assert_eq!(sim.get(sim.signal("constant")), 3.0f64.to_bits().into());
}

#[test]
fn integer_connected_to_float_input_is_converted() {
    let mut sim = Simulator::builder(
        r#"
        module Child (i: input f64, o: output f64) { assign o = i; }
        module Top (a: input u32, o: output f64) { inst u: Child (i: a, o); }
        "#,
        "Top",
    )
    .build()
    .unwrap();
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 3u32)).unwrap();
    assert_eq!(sim.get(sim.signal("o")), 3.0f64.to_bits().into());
}

#[test]
fn testbench_float_arithmetic_is_supported() {
    let result = Simulator::builder(
        r#"
        #[test(Top)]
        module Top {
            var a: f64;
            var c: f64;
            initial {
                a = 1.5;
                c = a * 2.0;
                $assert(c == 3.0);
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
fn float_display_renders_numeric_values() {
    let mut sim = Simulator::builder(
        r#"
        module Top(clk: input clock, a: input f64) {
            always_ff (clk) { $display("%f", a); }
        }
        "#,
        "Top",
    )
    .build()
    .unwrap();
    let a = sim.signal("a");
    let clk = sim.event("clk");
    sim.modify(|io| io.set(a, 1.5f64.to_bits())).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: b"1.500000".to_vec()
        }]
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
