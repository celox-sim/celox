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

/// Sequences with several ways to match (IEEE 1800-2023 16.7, 16.9.7): an
/// attempt passes at the first tick one way matches, once, and fails at the
/// first tick none can match any more.
#[test]
fn sequences_with_delay_ranges_and_or_match_their_first_way() {
    let source = r#"`timescale 1ns/1ns
module Top;
 bit clk;
 int cnt;
 int a_passed, a_failed, b_passed, b_failed, b_early, c_passed, c_failed;
 int d_passed, d_failed, e_passed, e_failed;
 always @(posedge clk) cnt <= cnt + 1;
 // A range: `cnt % 4 == 2` at the first tick misses, `== 3` at the second.
 assert property (@(posedge clk) cnt % 4 == 1 ##[1:2] cnt % 4 == 3) a_passed++; else a_failed++;
 // The shorter way matches first; the action runs then, when `cnt`, one
 // more than sampled, is 2 modulo 4.
 sequence b; @(posedge clk) (cnt % 4 == 0 ##1 cnt % 4 == 1) or (cnt % 4 == 0 ##2 cnt % 4 == 2); endsequence
 assert property (b) begin b_passed++; if (cnt % 4 == 2) b_early++; end else b_failed++;
 // Neither way matches: an attempt fails when the longer one misses, at
 // its third tick.
 assert property (@(posedge clk) (1 ##1 cnt < 0) or (1 ##2 cnt < 0)) c_passed++; else c_failed++;
 // Every later delay of the range matches too; the attempt passes once.
 assert property (@(posedge clk) cnt % 2 == 0 ##[1:3] cnt % 2 == 1) d_passed++; else d_failed++;
 // A leading range: the attempt waits for the first tick that matches.
 assert property (@(posedge clk) ##[0:3] cnt % 4 == 3) e_passed++; else e_failed++;
 initial repeat (20) begin #1 clk = 1; #1 clk = 0; end
endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("alternatives.sv"))], "Top")
            .build()
            .unwrap();
    sim.run_until(41).unwrap();
    // Ticks 0 to 19 sample `cnt` 0 to 19.
    for (name, expected) in [
        ("a_passed", 5u32),
        ("a_failed", 15),
        ("b_passed", 5),
        ("b_early", 5),
        ("b_failed", 15),
        ("c_passed", 0),
        // Those of ticks 18 and 19 end with the simulation undecided.
        ("c_failed", 18),
        ("d_passed", 10),
        ("d_failed", 10),
        // Every attempt reaches a tick with `cnt % 4 == 3` within three
        // ticks, by tick 19.
        ("e_passed", 20),
        ("e_failed", 0),
    ] {
        assert_eq!(sim.get(sim.signal(name)), expected.into(), "{name}");
    }
}

/// `and` matches when both operands have matched, at the later match, and
/// `intersect` when both match at one tick (IEEE 1800-2023 16.9.5,
/// 16.9.6).
#[test]
fn sequences_with_and_and_intersect_match_when_both_operands_have() {
    let source = r#"`timescale 1ns/1ns
module Top;
 bit clk;
 int cnt;
 int f_passed, f_failed, f_late, g_passed, g_failed, h_passed, h_failed, i_passed, i_failed;
 always @(posedge clk) cnt <= cnt + 1;
 // The left operand matches a tick after the start, the right one three:
 // the action runs at the third, when `cnt` is 0 modulo 4 after the NBA.
 assert property (@(posedge clk) (cnt % 4 == 0 ##1 1) and (cnt % 4 == 0 ##3 cnt % 4 == 3))
  begin f_passed++; if (cnt % 4 == 0) f_late++; end else f_failed++;
 // The right operand never matches: an attempt the left one starts fails
 // at its third tick, every other at its first.
 assert property (@(posedge clk) (cnt % 4 == 0 ##1 cnt % 4 == 1) and (1 ##2 cnt < 0)) g_passed++; else g_failed++;
 // Only the left way of two ticks has the right operand's length.
 assert property (@(posedge clk) (1 ##[1:3] cnt % 4 == 3) intersect (1 ##2 1)) h_passed++; else h_failed++;
 // Ways of both operands that match at one tick pass the attempt once.
 assert property (@(posedge clk) (cnt % 2 == 0 ##[1:2] 1) and (1 ##[0:1] 1)) i_passed++; else i_failed++;
 initial repeat (20) begin #1 clk = 1; #1 clk = 0; end
endmodule"#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("and.sv"))], "Top")
        .build()
        .unwrap();
    sim.run_until(41).unwrap();
    // Ticks 0 to 19 sample `cnt` 0 to 19.
    for (name, expected) in [
        ("f_passed", 5u32),
        ("f_late", 5),
        ("f_failed", 15),
        ("g_passed", 0),
        ("g_failed", 20),
        // Attempts at 1, 5, 9, 13 and 17 pass at their third tick; the
        // others up to tick 17 fail there, and those of 18 and 19 end with
        // the simulation.
        ("h_passed", 5),
        ("h_failed", 13),
        ("i_passed", 10),
        ("i_failed", 10),
    ] {
        assert_eq!(sim.get(sim.signal(name)), expected.into(), "{name}");
    }
}

#[test]
fn sequences_with_several_ways_to_match_name_what_they_do_not_support() {
    let long = vec!["a"; 1025].join(" ##0 ");
    let long = format!("@(posedge clk) {long}");
    for (property, message) in [
        (
            long.as_str(),
            "property with more than 1024 Boolean operands",
        ),
        ("@(posedge clk) a ##[1:$] b", "without an upper bound"),
        (
            "@(posedge clk) (a, note()) or b",
            "match item in a sequence with more than one way to match",
        ),
        (
            "@(posedge clk) a |-> (a ##1 b) or b",
            "implication whose consequent sequence has more than one way to match",
        ),
        (
            "@(posedge clk) (a or b) ##1 b |-> b",
            "implication whose antecedent sequence has more than one way to match",
        ),
        (
            "@(posedge clk) a ##[0:40] b ##[0:40] a",
            "sequence with more than 1024 ways to match",
        ),
        // Many ways over many ticks would make too large a monitor.
        (
            "@(posedge clk) a ##[0:1000] b",
            "sequence whose monitor takes more than 1048576 terms",
        ),
        (
            "@(posedge clk) (a, note()) and b",
            "match item in an operand of and",
        ),
        (
            "@(posedge clk) (a ##1 b) intersect (a ##2 b)",
            "intersect of sequences without ways to match of one length",
        ),
        // The tick `|=>` adds may not overflow the first delay.
        (
            "@(posedge clk) a |=> ##4294967295 b",
            "cycle delay longer than 2^32 - 1 ticks",
        ),
    ] {
        let source = format!(
            "module Top; bit clk, a, b; function automatic void note(); endfunction
             assert property ({property}); endmodule"
        );
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("unsupported_sequence.sv"))],
            "Top",
        )
        .build()
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
        assert!(error.contains(message), "{property}: {error}");
    }
}
