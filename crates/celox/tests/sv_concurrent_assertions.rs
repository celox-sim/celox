use std::path::Path;

#[test]
fn sampled_predicate_and_live_reactive_action() {
    let source = r#"`timescale 1ns/1ns
module Top;
 bit clk, data;
 int passed, failed, action_value;
 always @(posedge clk) data <= 1;
 p: assert property (@(posedge clk) data == 0)
    begin passed++; action_value = data; end
    else failed++;
 initial begin #1 clk = 1; #1 clk = 0; #1 clk = 1; end
endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("concurrent.sv"))], "Top")
            .build()
            .unwrap();
    sim.run_until(3).unwrap();
    for (name, expected) in [("passed", 1u32), ("failed", 1), ("action_value", 1)] {
        assert_eq!(sim.get(sim.signal(name)), expected.into(), "{name}");
    }
}

#[test]
fn pending_observed_assertions_resume_after_stop() {
    let source = r#"`timescale 1ns/1ns
module Top(output int passed, failed);
 bit clk;
 p: assert property (@(posedge clk) 1) passed++; else failed++;
 initial begin #1 clk=1; #1 clk=0; #1 clk=1; end
 initial begin #1 $stop; end
endmodule"#;
    for four_state in [false, true] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source, Path::new("observed_stop.sv"))],
            "Top",
        )
        .four_state(four_state)
        .build()
        .unwrap();
        sim.run_until(3).unwrap();
        assert!(sim.is_stopped());
        assert_eq!(sim.get(sim.signal("passed")), 0u32.into());
        sim.run_until(3).unwrap();
        assert_eq!(sim.get(sim.signal("passed")), 2u32.into());
        assert_eq!(sim.get(sim.signal("failed")), 0u32.into());
    }
}

#[test]
fn concurrent_assertions_resume_from_portable_state() {
    let source = r#"`timescale 1ns/1ns
module Top(output int passed);
 bit clk;
 p: assert property (@(posedge clk) 1) passed++;
 initial begin #1 clk=1; #1 clk=0; #1 clk=1; end
endmodule"#;
    let build = || {
        celox::Simulation::from_sv_sources(vec![(source, Path::new("assertion_state.sv"))], "Top")
    };
    let mut sim = build().build_interpreter().unwrap();
    sim.run_until(1).unwrap();
    let mut bytes = Vec::new();
    sim.save_state().unwrap().write_to(&mut bytes).unwrap();
    let file = celox::StateFile::read_from(bytes.as_slice()).unwrap();
    let mut loaded = build().build_native().unwrap();
    loaded.load_state(&file).unwrap();
    let checkpoint = loaded.checkpoint().unwrap();
    loaded.run_until(3).unwrap();
    loaded.restore(&checkpoint).unwrap();
    loaded.run_until(3).unwrap();
    assert_eq!(loaded.get(loaded.signal("passed")), 2u32.into());

    let mut invalid = file;
    let p = &mut invalid.schedule.as_mut().unwrap().processes;
    let waiter = p.waiters.remove(0);
    p.observed.push(waiter.process);
    assert!(loaded.load_state(&invalid).is_err());
}

#[test]
fn default_failure_reports_the_assertion_label_and_clock_time() {
    let source = r#"`timescale 1ns/1ns
module Top;
 bit clk;
 bad: assert property (@(posedge clk) 0);
 initial #1 clk=1;
endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("assertion_report.sv"))], "Top")
            .build()
            .unwrap();
    sim.run_until(2).unwrap();
    let reports: Vec<_> = sim
        .drain_runtime_events()
        .into_iter()
        .filter_map(|event| {
            if let celox::RuntimeEvent::Report(report) = event {
                Some(report)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].time, Some(1));
    assert_eq!(reports[0].severity, celox::Severity::Error);
    let site = reports[0].assertion.as_ref().unwrap();
    assert_eq!(site.label.as_deref(), Some("bad"));
    assert_eq!(site.outcome, celox::AssertionOutcome::Fail);
}

/// An X-check over stored data can never fail in two-state mode, where
/// nothing carries X (IEEE 1800-2023 11.4.10, 16.5.1): the monitor is
/// dropped. In four-state mode the same check is live and must fire on X.
/// A pass action keeps its monitor in both modes and must still count.
#[test]
fn two_state_vacuous_x_check_is_dropped_but_four_state_fires() {
    let source = r#"`timescale 1ns/1ns
module Top;
 bit clk;
 logic [7:0] dat;
 int passes;
 always #1 clk = ~clk;
 always @(posedge clk) dat <= dat + 8'd1;
 check: assert property (@(posedge clk) ((^dat) !== 1'bx))
    else $fatal("x detected");
 counted: assert property (@(posedge clk) ((^dat) !== 1'bx)) passes++;
endmodule"#;
    let build = |four_state| {
        celox::Simulation::from_sv_sources(vec![(source, Path::new("xcheck.sv"))], "Top")
            .four_state(four_state)
            .build()
            .unwrap()
    };
    fn fails<B: celox::SimBackend>(sim: &mut celox::Simulation<B>) -> usize {
        sim.drain_runtime_events()
            .into_iter()
            .filter(|event| {
                matches!(
                    event,
                    celox::RuntimeEvent::Report(report)
                        if report.assertion.as_ref().is_some_and(|site| {
                            site.outcome == celox::AssertionOutcome::Fail
                        })
                )
            })
            .count()
    }

    let mut sim = build(false);
    sim.run_until(9).unwrap();
    assert_eq!(fails(&mut sim), 0);
    let passes = sim.signal("passes");
    assert_eq!(sim.get(passes), 5u32.into());

    let mut sim = build(true);
    let _ = sim.run_until(9);
    assert!(fails(&mut sim) > 0, "four-state X check must still fire");
}

/// Dropping a vacuous monitor must not strand the `$past` history it
/// shares with live code: every process that reads a history declares
/// and advances its own chain, so the live assertion below still sees
/// `dat` move one tick back even though the vacuous reader of the same
/// history is gone in two-state mode.
#[test]
fn dropped_vacuous_monitor_keeps_shared_history_advancing() {
    let source = r#"`timescale 1ns/1ns
module Top;
 bit clk;
 logic [7:0] dat;
 always #1 clk = ~clk;
 always @(posedge clk) dat <= dat + 8'd1;
 vacuous: assert property (@(posedge clk) (($past(dat, 2)) !== 1'bx))
    else $fatal("x detected");
 live: assert property (@(posedge clk) (dat == 8'd0 || $past(dat) == dat - 8'd1))
    else $fatal("past broken");
endmodule"#;
    assert_eq!(two_state_fail_count(source, "shared-history.sv"), 0);
}

fn two_state_fail_count(source: &str, path: &'static str) -> usize {
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new(path))], "Top")
        .four_state(false)
        .build()
        .unwrap();
    let _ = sim.run_until(9);
    sim.drain_runtime_events()
        .into_iter()
        .filter(|event| {
            matches!(
                event,
                celox::RuntimeEvent::Report(report)
                    if report.assertion.as_ref().is_some_and(|site| {
                        site.outcome == celox::AssertionOutcome::Fail
                    })
            )
        })
        .count()
}

/// Constant predicates that are false must keep their monitors in
/// two-state mode: signed operands extend by sign to the common width
/// (11.6.1), `~X` stays X rather than becoming Z (11.4.9, 11.4.10), a
/// cast extends a signed operand by its sign (6.24.1, 10.7), and operators
/// work at the width of the enclosing comparison (11.6.1).
#[test]
fn constant_false_predicates_still_fire_in_two_state() {
    for (predicate, path) in [
        ("(8'sh0f === 4'shf)", "signed-extension.sv"),
        ("(~1'bx === 1'bz)", "bitnot-x.sv"),
        ("(8'(4'shf) !== 8'hff)", "signed-cast.sv"),
        ("(~4'hf !== 8'hf0)", "context-bitnot.sv"),
        ("((4'hf + 4'h1) !== 5'h10)", "context-add.sv"),
    ] {
        let source = format!(
            "`timescale 1ns/1ns\nmodule Top;\n bit clk;\n always #1 clk = ~clk;\n check: assert property (@(posedge clk) {predicate})\n    else $fatal(\"must fire\");\nendmodule"
        );
        assert!(
            two_state_fail_count(&source, path) > 0,
            "{predicate} must fire in two-state mode"
        );
    }
}

/// An X-check keeps its monitor in two-state mode when its operand can be
/// X although every stored value is known: `inside` over an unknown
/// literal (11.4.13), a select at a position outside the vector (11.5.1),
/// a zero divisor (11.4.3), and a sampled-value function or a cast of an
/// unknown literal (16.9.3, 6.24.1).
#[test]
fn x_checks_over_unknown_results_still_fire_in_two_state() {
    for (predicate, path) in [
        ("((1'bx inside {1'b0}) !== 1'bx)", "inside-x.sv"),
        ("(dat[position] !== 1'bx)", "select-out-of-range.sv"),
        ("((^(dat / 8'd0)) !== 1'bx)", "divide-by-zero.sv"),
        ("((^(dat % 8'd0)) !== 1'bx)", "modulo-by-zero.sv"),
        ("($past(1'bx, 2) !== 1'bx)", "past-x.sv"),
        ("($signed(1'bx) !== 1'bx)", "signed-x.sv"),
        ("(8'hff !== '1)", "unbased-fill.sv"),
    ] {
        let source = format!(
            "`timescale 1ns/1ns\nmodule Top;\n bit clk;\n logic [7:0] dat;\n int position = -1;\n always #1 clk = ~clk;\n check: assert property (@(posedge clk) {predicate})\n    else $fatal(\"must fire\");\nendmodule"
        );
        assert!(
            two_state_fail_count(&source, path) > 0,
            "{predicate} must fire in two-state mode"
        );
    }
}

/// A sampled literal still carries its X bits into case equality in
/// two-state mode, so `$sampled(1'bx) !== 1'bx` is false and its monitor
/// must not be dropped as vacuous (IEEE 1800-2023 11.4.10, 16.9.3).
#[test]
fn sampled_literal_x_check_still_fires_in_two_state() {
    let source = r#"`timescale 1ns/1ns
module Top;
 bit clk;
 always #1 clk = ~clk;
 check: assert property (@(posedge clk) ($sampled(1'bx) !== 1'bx))
    else $fatal("x detected");
endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("sampled-x.sv"))], "Top")
            .four_state(false)
            .build()
            .unwrap();
    let _ = sim.run_until(9);
    let fails = sim
        .drain_runtime_events()
        .into_iter()
        .filter(|event| {
            matches!(
                event,
                celox::RuntimeEvent::Report(report)
                    if report.assertion.as_ref().is_some_and(|site| {
                        site.outcome == celox::AssertionOutcome::Fail
                    })
            )
        })
        .count();
    assert!(
        fails > 0,
        "sampled X check must still fire in two-state mode"
    );
}

#[test]
fn unsupported_temporal_and_procedural_forms_are_not_boolean_checks() {
    for body in [
        "assert property (@(posedge clk) 1 |=> 0[*2]);",
        "initial assert property (@(posedge clk) 1);",
        "assert property (@(posedge clk) 1) q <= 1;",
    ] {
        let source = format!("module Top; bit clk, q; {body} endmodule");
        assert!(
            celox::Simulation::from_sv_sources(
                vec![(source.as_str(), Path::new("unsupported_property.sv"))],
                "Top"
            )
            .build()
            .is_err(),
            "{body}"
        );
    }
}
