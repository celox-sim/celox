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
    let two_state_kernels = sim.scheduler_stats().kernels.len();
    sim.run_until(9).unwrap();
    assert_eq!(fails(&mut sim), 0);
    let passes = sim.signal("passes");
    assert_eq!(sim.get(passes), 5u32.into());

    let mut sim = build(true);
    // Only the four-state design needs a scheduler entry for the X check.
    assert_eq!(sim.scheduler_stats().kernels.len(), two_state_kernels + 1);
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

/// The 1024-way cap counts the ways of `intersect` operands of one length,
/// not every pair: 33 ways on each side share one length (IEEE 1800-2023
/// 16.9.6).
#[test]
fn intersect_counts_only_ways_of_one_length() {
    let source = r#"`timescale 1ns/1ns
module Top;
 bit clk;
 int passed, failed;
 assert property (@(posedge clk) (1 ##[0:32] 1) intersect (1 ##[32:64] 1)) passed++; else failed++;
 initial repeat (40) begin #1 clk = 1; #1 clk = 0; end
endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("intersect.sv"))], "Top")
            .build()
            .unwrap();
    sim.run_until(81).unwrap();
    // The attempts of ticks 0 to 7 match 32 ticks later; the others end
    // with the simulation.
    assert_eq!(sim.get(sim.signal("passed")), 8u32.into());
    assert_eq!(sim.get(sim.signal("failed")), 0u32.into());
}

/// `b throughout s` holds `b` at each tick from the start of `s` to its
/// match (IEEE 1800-2023 16.9.9), and a named sequence is an operand as
/// its body (16.8).
#[test]
fn throughout_holds_its_condition_over_each_tick_of_the_sequence() {
    let source = r#"`timescale 1ns/1ns
module Top;
 bit clk;
 int cnt;
 int j_passed, j_failed, k_passed, k_failed, l_passed, l_failed;
 always @(posedge clk) cnt <= cnt + 1;
 sequence s; @(posedge clk) cnt % 8 == 0 ##3 1; endsequence
 // Attempts at ticks 0 and 8 hold `cnt < 12` over their four ticks; the
 // one at 16 misses it at its first tick.
 assert property (@(posedge clk) cnt < 12 throughout s) j_passed++; else j_failed++;
 // `cnt % 8 != 3` misses at the fourth tick of the attempts at 0, 8 and
 // 16, which fail then, the last at tick 19.
 assert property (@(posedge clk) cnt % 8 != 3 throughout cnt % 8 == 0 ##3 1) k_passed++; else k_failed++;
 // The shorter way passes before the condition misses.
 assert property (@(posedge clk) cnt % 8 < 2 throughout (cnt % 8 == 0 ##[1:5] 1)) l_passed++; else l_failed++;
 initial repeat (20) begin #1 clk = 1; #1 clk = 0; end
endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("throughout.sv"))], "Top")
            .build()
            .unwrap();
    sim.run_until(41).unwrap();
    // Ticks 0 to 19 sample `cnt` 0 to 19: the attempts at 0, 8 and 16 start
    // the sequences, the other 17 fail at their first tick.
    for (name, expected) in [
        ("j_passed", 2u32),
        ("j_failed", 18),
        ("k_passed", 0),
        ("k_failed", 20),
        ("l_passed", 3),
        ("l_failed", 17),
    ] {
        assert_eq!(sim.get(sim.signal(name)), expected.into(), "{name}");
    }
}

/// `disable iff` of a sequence over cycles disables each attempt in progress
/// whenever its condition is true, also between the ticks of the clock,
/// and the attempt that starts at a tick where it is true: a disabled
/// attempt neither passes nor fails, and the attempts that start later
/// are not affected (IEEE 1800-2023 16.12). The condition is read with the
/// current values of its variables, not sampled ones. Verilator 5.054
/// prints the same lines.
#[test]
fn disable_iff_disables_attempts_of_sequences_over_cycles() {
    let cases = [
        // An implication with `|=>`: the attempt of tick 25 is disabled at
        // its second tick, that of tick 65 by a pulse between two ticks.
        (
            r#"`timescale 1ns/1ns
module top;
  logic clk = 0, rst = 0, a = 0, b = 0;
  always #5 clk = ~clk;
  // a at edge n requires b at edge n+1
  assert property (@(posedge clk) disable iff (rst) a |=> b)
    $display("%0t pass", $time); else $display("%0t fail", $time);
  initial begin
    // attempt at 5 (a), b missing at 15 -> fail at 15
    a = 1; #10 a = 0;            // t=10
    #10;                          // t=20
    // attempt at 25 (a), rst during the attempt at edge 35 -> disabled
    a = 1; #10 a = 0; rst = 1;    // t=30
    #10 rst = 0;                  // t=40
    // attempt at 45, b at 55 -> pass
    #2 a = 1; #10 a = 0; b = 1; #10 b = 0;  // t=62
    // attempt at 65 (a), rst pulse between edges 65 and 75 only -> disabled per LRM
    #1 a = 1; #4 a = 0; #2 rst = 1; #1 rst = 0;   // a sampled at 65; rst 70..71
    #20 $finish;
  end
endmodule"#,
            &[
                "15 fail", "15 pass", "55 pass", "55 pass", "75 pass", "85 pass",
            ][..],
        ),
        // Several ways to match: the attempt of tick 45 is disabled before
        // it decides.
        (
            r#"`timescale 1ns/1ns
module top;
  logic clk = 0, rst = 1, a = 0, b = 0;
  always #5 clk = ~clk;
  // Alternatives: b one or two ticks after a.
  assert property (@(posedge clk) disable iff (rst) a ##[1:2] b)
    $display("%0t pass", $time); else $display("%0t fail", $time);
  initial begin
    #12 rst = 0; a = 1;        // t=12: attempt at 15 (a)
    #10 a = 0;                  // t=22
    #10 b = 1;                  // t=32: b at 35 -> pass at 35 (two ticks after 15)
    #10 b = 0; a = 1;           // t=42: attempt at 45
    #10 a = 0; rst = 1;         // t=52: rst at 55 -> attempt 45 disabled, 55 disabled
    #10 rst = 0;                // t=62
    #20 $finish;                // t=82
  end
endmodule"#,
            &["25 fail", "35 pass", "35 fail", "65 fail", "75 fail"][..],
        ),
        // Attempts one tick apart: those of ticks 65 and 75 are disabled,
        // that of tick 85 fails.
        (
            r#"`timescale 1ns/1ns
module top;
  logic clk = 0, rst = 0, a = 0, b = 0, c = 0;
  always #5 clk = ~clk;
  // a, then b one tick later and c the tick after.
  assert property (@(posedge clk) disable iff (rst) a |=> b ##1 c)
    else $display("%0t fail", $time);
  initial begin
    a = 1;                      // attempts at 5, 15, 25, 35 (a high)
    #40 a = 0;                  // t=40
    // b never rises: attempt 5 fails at 15 (b), 15 at 25, 25 at 35, 35 at 45
    #20 a = 1;                  // t=60: attempts at 65, 75, 85
    #10 rst = 1;                // t=70: rst at 75 aborts 65 and 75
    #10 rst = 0;                // t=80
    #10 a = 0;                  // t=90: attempt 85 fails at 95 (b)
    #20 $finish;                // t=110
  end
endmodule"#,
            &["15 fail", "25 fail", "35 fail", "45 fail", "95 fail"][..],
        ),
    ];
    for (source, expected) in cases {
        for four_state in [false, true] {
            let mut sim =
                celox::Simulation::from_sv_sources(vec![(source, Path::new("disable.sv"))], "top")
                    .four_state(four_state)
                    .build()
                    .unwrap();
            sim.run_until(200).unwrap();
            let lines: Vec<_> = sim
                .drain_runtime_events()
                .into_iter()
                .filter_map(|event| match event {
                    celox::RuntimeEvent::Display { message } => {
                        Some(String::from_utf8_lossy(&message).into_owned())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(lines, expected, "four_state={four_state}\n{source}");
        }
    }
}

/// An implication whose consequent has several ways to match, as Caliptra's
/// VeeR writes `a |-> ##[1:2] b`: an attempt whose antecedent misses at any
/// of its steps succeeds vacuously at that tick, and the consequent starts
/// at the tick the antecedent ends, one tick later with `|=>`
/// (IEEE 1800-2023 16.12.7).
#[test]
fn implications_with_consequents_of_several_ways_to_match() {
    let source = r#"`timescale 1ns/1ns
module Top;
 bit clk;
 int cnt;
 int a_passed, a_failed, b_passed, b_failed, c_passed, c_failed, d_passed, d_failed;
 always @(posedge clk) cnt <= cnt + 1;
 // The consequent matches two ticks after the antecedent.
 assert property (@(posedge clk) cnt % 4 == 1 |-> ##[1:2] cnt % 4 == 3) a_passed++; else a_failed++;
 // Neither tick matches: the attempt fails at the second.
 assert property (@(posedge clk) cnt % 4 == 1 |-> ##[1:2] cnt % 4 == 0) b_passed++; else b_failed++;
 // An antecedent of two steps, `|=>`, and `or`: the shorter way misses,
 // the longer one at its second tick.
 assert property (@(posedge clk) cnt % 4 == 0 ##1 cnt % 4 == 1 |=> (cnt % 4 == 3) or (cnt % 4 == 2 ##1 cnt % 4 == 0)) c_passed++; else c_failed++;
 // The antecedent misses at its second step: vacuous then.
 assert property (@(posedge clk) cnt % 4 == 0 ##1 cnt % 4 == 2 |-> ##[0:1] cnt < 0) d_passed++; else d_failed++;
 initial repeat (20) begin #1 clk = 1; #1 clk = 0; end
endmodule"#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("implications.sv"))], "Top")
                .four_state(four_state)
                .build()
                .unwrap();
        sim.run_until(41).unwrap();
        // Ticks 0 to 19 sample `cnt` 0 to 19; the attempts of ticks 1, 5,
        // 9, 13 and 17 reach their consequent, the others are vacuous.
        for (name, expected) in [
            ("a_passed", 20u32),
            ("a_failed", 0),
            ("b_passed", 15),
            ("b_failed", 5),
            // Those of ticks 0, 4, 8, 12 and 16 fail at their fourth tick.
            ("c_passed", 15),
            ("c_failed", 5),
            ("d_passed", 20),
            ("d_failed", 0),
        ] {
            assert_eq!(
                sim.get(sim.signal(name)),
                expected.into(),
                "{name} four_state={four_state}"
            );
        }
    }
    // With `disable iff`: the attempt of tick 5 fails at 25, that of 45 is
    // disabled at 55, that of 65 passes at 85; every other one is vacuous.
    let source = r#"`timescale 1ns/1ns
module top;
  logic clk = 0, rst = 0, a = 0, b = 0;
  always #5 clk = ~clk;
  assert property (@(posedge clk) disable iff (rst) a |-> ##[1:2] b)
    $display("%0t pass", $time); else $display("%0t fail", $time);
  initial begin
    a = 1; #10 a = 0;          // attempt at 5: b at 15 or 25? none -> fail at 25
    #30;                        // t=40
    a = 1; #10 a = 0;           // attempt at 45; t=50
    rst = 1; #10 rst = 0;       // rst at edge 55 -> disabled; t=60
    a = 1; #10 a = 0;           // attempt at 65; t=70
    #10 b = 1; #10 b = 0;       // b at 85 -> pass at 85
    #20 $finish;
  end
endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("disable.sv"))], "top")
            .build()
            .unwrap();
    sim.run_until(200).unwrap();
    let lines: Vec<_> = sim
        .drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => {
                Some(String::from_utf8_lossy(&message).into_owned())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        lines,
        [
            "15 pass", "25 fail", "25 pass", "35 pass", "75 pass", "85 pass", "85 pass", "95 pass",
            "105 pass"
        ]
    );
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
        (
            "@(posedge clk) (a ##1 b) throughout b",
            "throughout whose left operand is not a Boolean expression",
        ),
        // The clocking event of an instance after the start of a sequence.
        (
            "@(posedge clk) a ##1 s",
            "multiclocked sequence: instance `s` has its own clocking event",
        ),
        // The tick `|=>` adds may not overflow the first delay.
        (
            "@(posedge clk) a |=> ##4294967295 b",
            "cycle delay longer than 2^32 - 1 ticks",
        ),
    ] {
        let source = format!(
            "module Top; bit clk, a, b; function automatic void note(); endfunction
             sequence s; @(negedge clk) a; endsequence
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

/// An interface declares properties and sequences and checks them with
/// concurrent assertions and cover statements (IEEE 1800-2023 16.2, 25.3),
/// each instance on its own members, as the APB interface of the UVM
/// examples does, also in a design that holds it in a virtual interface.
#[test]
fn interfaces_check_their_own_concurrent_assertions() {
    let source = r#"`timescale 1ns/1ns
interface chk_if(input clk);
 logic [3:0] v;
 int fails, covered;
 property bounded; @(posedge clk) v < 8; endproperty: bounded
 CHK: assert property (bounded) else fails++;
 sequence rise; @(posedge clk) v == 1 ##1 v == 2; endsequence
 COV: cover property (rise) covered++;
endinterface: chk_if
module Top;
 bit clk;
 int a_fails, b_fails, a_covered;
 chk_if a(clk);
 chk_if b(clk);
 virtual chk_if vif;
 initial forever #5 clk = ~clk;
 initial begin
  vif = b;
  a.v = 1; b.v = 9;
  #10 a.v = 2; b.v = 3;
  #10 b.v = 12;
  #20 a_fails = a.fails; b_fails = vif.fails; a_covered = a.covered;
 end
endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("interface.sv"))], "Top")
            .build()
            .unwrap();
    sim.run_until(41).unwrap();
    // Ticks at 5, 15, 25 and 35: b exceeds 7 at the first, third and fourth.
    for (name, expected) in [("a_fails", 0u32), ("b_fails", 3), ("a_covered", 1)] {
        assert_eq!(sim.get(sim.signal(name)), expected.into(), "{name}");
    }
}

/// A multiclocked sequence (IEEE 1800-2023 16.13.1): after `##1` the next
/// subsequence starts at the nearest tick of its clock strictly after the
/// end of the one before, after `##0` possibly at it.
#[test]
fn multiclocked_sequences_hand_attempts_to_the_next_clock() {
    // clk0 rises at 50, 450, 850, ...; clk1 at 200, 600, 1000, ...; out0
    // is sampled 1 from 450, out1 from 600 on. The attempt of each clk0
    // tick t ends its first subsequence at t + 400 and checks out1 at the
    // clk1 tick t + 550: those of 50, 450, 850 and 1250 end by 2000.
    let source = r#"`timescale 1ns/1ns
module Top;
 bit clk0, clk1, out0, out1;
 int passed, failed, n_passed, n_failed;
 always @(posedge clk0) out0 <= 1;
 always @(posedge clk1) out1 <= 1;
 assert property (@(posedge clk0) ##1 out0 ##1 @(posedge clk1) out1) passed++; else failed++;
 assert property (@(posedge clk0) ##1 out0 ##1 @(posedge clk1) !out1) n_passed++; else n_failed++;
 initial forever begin #50 clk0 = ~clk0; #150 clk1 = ~clk1; end
endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("multiclock.sv"))], "Top")
            .build()
            .unwrap();
    sim.run_until(2000).unwrap();
    for (name, expected) in [
        ("passed", 4u32),
        ("failed", 0),
        ("n_passed", 0),
        ("n_failed", 4),
    ] {
        assert_eq!(sim.get(sim.signal(name)), expected.into(), "{name}");
    }
    // ca and cb rise together, at 5, 15, ..., 95, when cnt is sampled 0 to
    // 9. With `##0` an attempt of an even cnt checks the second subsequence
    // at its own tick and passes; with `##1` at the next one, of an odd cnt,
    // and fails. A clocked subsequence in parentheses of its own is the
    // same sequence, and `$sampled` is the sampled value it reads anyway.
    let source = r#"`timescale 1ns/1ns
module Top;
 bit ca, cb;
 int cnt, z_passed, z_failed, o_passed, o_failed, g_passed, g_failed;
 always @(posedge ca) cnt <= cnt + 1;
 assert property (@(posedge ca) cnt % 2 == 0 ##0 @(posedge cb) cnt % 2 == 0) z_passed++; else z_failed++;
 assert property (@(posedge ca) cnt % 2 == 0 ##0 (@(posedge cb) $sampled(cnt) % 2 == 0)) g_passed++; else g_failed++;
 assert property (@(posedge ca) cnt % 2 == 0 ##1 @(posedge cb) cnt % 2 == 0) o_passed++; else o_failed++;
 initial forever begin #5 ca = 1; cb = 1; #5 ca = 0; cb = 0; end
endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("multiclock.sv"))], "Top")
            .build()
            .unwrap();
    sim.run_until(100).unwrap();
    for (name, expected) in [
        ("z_passed", 5u32),
        ("z_failed", 5),
        ("g_passed", 5),
        ("g_failed", 5),
        ("o_passed", 0),
        // The attempt of cnt 8 checks cnt 9 at 95; that of 9 fails at once.
        ("o_failed", 10),
    ] {
        assert_eq!(sim.get(sim.signal(name)), expected.into(), "{name}");
    }
}

/// A clocking event in parentheses clocks only the rest of its group: the
/// clock around the group resumes after it (IEEE 1800-2023 16.13.3).
#[test]
fn multiclocked_sequences_resume_the_clock_around_a_group() {
    // clk0 rises at 50, 450, 850, ...; clk1 at 200, 600, 1000, .... x is
    // set at each clk0 tick and cleared at each clk1 tick, so it is sampled
    // 0 at clk0 ticks and 1 at clk1 ticks. In the group form the attempt of
    // clk0 tick t checks !x at the clk0 tick t + 400 and passes; written
    // without parentheses clk1 clocks it, at t + 550, and it fails.
    let source = r#"`timescale 1ns/1ns
module Top;
 bit clk0, clk1, x, a = 1;
 int g_passed, g_failed, f_passed, f_failed, l_passed, l_failed;
 always @(posedge clk0) x <= 1;
 always @(posedge clk1) x <= 0;
 assert property (@(posedge clk0) a ##1 (@(posedge clk1) a) ##1 !x) g_passed++; else g_failed++;
 assert property (@(posedge clk0) a ##1 @(posedge clk1) a ##1 !x) f_passed++; else f_failed++;
 assert property (@(posedge clk0) (@(posedge clk1) a ##1 x) ##1 !x) l_passed++; else l_failed++;
 initial forever begin #50 clk0 = ~clk0; #150 clk1 = ~clk1; end
endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("multiclock.sv"))], "Top")
            .build()
            .unwrap();
    sim.run_until(2000).unwrap();
    for (name, expected) in [
        ("g_passed", 4u32),
        ("g_failed", 0),
        ("f_passed", 0),
        ("f_failed", 4),
        // A group that leads the sequence starts it at its own clock: the
        // attempt of clk1 tick t checks x at t + 400 and !x at the clk0
        // tick t + 650, those of 200, 600 and 1000 by 2000.
        ("l_passed", 3),
        ("l_failed", 0),
    ] {
        assert_eq!(sim.get(sim.signal(name)), expected.into(), "{name}");
    }
}

/// The clock of a subsequence that selects a member through a virtual
/// interface ticks as an event control on it does: at writes of the member
/// of the instance the handle denotes (IEEE 1800-2023 9.4.2, 25.9).
#[test]
fn multiclocked_sequences_tick_on_members_through_virtual_interfaces() {
    // As in `multiclocked_sequences_hand_attempts_to_the_next_clock`, with
    // clk1 in an interface instance, and an instance whose clock never ticks
    // selected for the attempts that do not pass.
    let source = r#"`timescale 1ns/1ns
interface ifc;
 bit clk;
endinterface
module Top;
 ifc i(), idle();
 virtual ifc vif = i;
 virtual ifc stuck = idle;
 bit clk0, out0, out1;
 int passed, failed, s_passed, s_failed;
 always @(posedge clk0) out0 <= 1;
 always @(posedge i.clk) out1 <= 1;
 assert property (@(posedge clk0) ##1 out0 ##1 @(posedge vif.clk) out1) passed++; else failed++;
 assert property (@(posedge clk0) ##1 out0 ##1 @(posedge stuck.clk) out1) s_passed++; else s_failed++;
 initial forever begin #50 clk0 = ~clk0; #150 i.clk = ~i.clk; end
endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("multiclock.sv"))], "Top")
            .build()
            .unwrap();
    sim.run_until(2000).unwrap();
    for (name, expected) in [
        ("passed", 4u32),
        ("failed", 0),
        ("s_passed", 0),
        ("s_failed", 0),
    ] {
        assert_eq!(sim.get(sim.signal(name)), expected.into(), "{name}");
    }
}

/// `$assertoff` keeps the first clock of a multiclocked sequence from
/// starting attempts, and those it started go on over the next clock to
/// their end (IEEE 1800-2023 20.11).
#[test]
fn multiclocked_sequences_start_no_attempt_while_assertions_are_off() {
    // ca and cb rise together at 5, 15, ..., 95. Each attempt fails at the
    // next cb tick. Those of 35, 45 and 55 do not start; that of 25 fails at
    // 35, after `$assertoff`, and that of 95 ends with the simulation.
    let source = r#"`timescale 1ns/1ns
module Top;
 bit ca, cb;
 int failed;
 assert property (@(posedge ca) 1 ##1 @(posedge cb) 0) else failed++;
 initial forever begin #5 ca = 1; cb = 1; #5 ca = 0; cb = 0; end
 initial begin #32 $assertoff; #30 $asserton; end
endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("multiclock.sv"))], "Top")
            .build()
            .unwrap();
    sim.run_until(100).unwrap();
    assert_eq!(sim.get(sim.signal("failed")), 6u32.into());
}

/// The clock of each subsequence of a multiclocked sequence ticks as its
/// event control would wake a process, by the edges of 4-state values
/// (IEEE 1800-2023 9.4.2, Table 9-2): `posedge` on 0 to X, `negedge` on X
/// to 0.
#[test]
fn multiclocked_sequences_tick_on_four_state_edges() {
    // c0 rises at 10, so each attempt starts then and checks `a`, 0, at the
    // next tick of the other clock, at 30: each fails once.
    let source = r#"`timescale 1ns/1ns
module Top;
 bit c0, a;
 logic up = 0, down;
 int x_rise, x_fall;
 assert property (@(posedge c0) 1 ##1 @(posedge up) a) else x_rise++;
 assert property (@(posedge c0) 1 ##1 @(negedge down) a) else x_fall++;
 initial begin
  #10 c0 = 1;
  #20 up = 1'bx; down = 0;
 end
endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("multiclock.sv"))], "Top")
            .four_state(true)
            .build()
            .unwrap();
    sim.run_until(50).unwrap();
    for name in ["x_rise", "x_fall"] {
        assert_eq!(sim.get(sim.signal(name)), 1u32.into(), "{name}");
    }
}

/// The `iff` of the clock of a subsequence is judged when its event occurs
/// (IEEE 1800-2023 9.4.2.3), not when the sequence decides in the Observed
/// region.
#[test]
fn multiclocked_sequences_judge_iff_when_the_clock_event_occurs() {
    // `en` is 1 when `gated` rises at 30 and 0 by the Observed region: the
    // event occurs, and the attempt of 10 checks `a`, 0, then and fails.
    // `off` is 0 when `blocked` rises and 1 after: no event, no decision.
    let source = r#"`timescale 1ns/1ns
module Top;
 bit c0, a, en = 1, off, gated, blocked;
 int at_event, without_event;
 assert property (@(posedge c0) 1 ##1 @(posedge gated iff en) a) else at_event++;
 assert property (@(posedge c0) 1 ##1 @(posedge blocked iff off) a) else without_event++;
 initial begin
  #10 c0 = 1;
  #20 gated = 1; en = 0; blocked = 1; off = 1;
 end
endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("multiclock.sv"))], "Top")
            .build()
            .unwrap();
    sim.run_until(50).unwrap();
    for (name, expected) in [("at_event", 1u32), ("without_event", 0)] {
        assert_eq!(sim.get(sim.signal(name)), expected.into(), "{name}");
    }
}

#[test]
fn deeply_nested_sequences_are_a_diagnostic() {
    // Each clocking event of a subsequence nests the rest of the sequence
    // (IEEE 1800-2023 16.13), as a group does: past a bound, a chain of them
    // is reported, not parsed into an ever deeper tree.
    let build = |property: String| {
        let source = format!(
            "module Top; bit c0, c1, a; assert property (@(posedge c0) {property}); endmodule"
        );
        celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("multiclock.sv"))],
            "Top",
        )
        .build()
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default()
    };
    for property in [
        format!("{}a", "@(posedge c1) ".repeat(1025)),
        format!("{}a ##1 a{}", "(".repeat(1025), ")".repeat(1025)),
    ] {
        let error = build(property);
        assert!(error.contains("nested more than 64 deep"), "{error}");
    }
    // Within the bound, a chain of clocks is no such diagnostic.
    let error = build(format!("a ##1 {}a", "@(posedge c1) ".repeat(64)));
    assert!(!error.contains("nested more than"), "{error}");
}

#[test]
fn multiclocked_sequences_name_what_they_do_not_support() {
    for (property, message) in [
        (
            "@(posedge c0) a ##2 @(posedge c1) b",
            "a cycle delay other than ##0 or ##1 at a clock change",
        ),
        (
            "@(posedge c0) a or @(posedge c1) b",
            "a clock change other than one joined by ##0 or ##1",
        ),
        (
            "@(posedge c0) disable iff (a) a ##1 @(posedge c1) b",
            "disable iff of a multiclocked sequence",
        ),
        (
            "@(posedge c0) a |-> ##1 @(posedge c1) b",
            "implication of multiclocked sequences",
        ),
        (
            "@(posedge c0) a ##1 @(posedge c1) $past(b)",
            "sampled value function other than $sampled in a multiclocked sequence",
        ),
        (
            "@(posedge c0) $rose(a) ##1 @(posedge c1) b",
            "sampled value function other than $sampled in a multiclocked sequence",
        ),
    ] {
        let source =
            format!("module Top; bit c0, c1, a, b; assert property ({property}); endmodule");
        let error = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("multiclock.sv"))],
            "Top",
        )
        .build()
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
        assert!(error.contains(message), "{property}: {error}");
    }
}

/// The counts of `names` after the ten rising edges at 5, 15, ..., 95 of
/// `source`, in two-state and four-state simulation alike.
fn counts_after_ten_edges(source: &str, path: &'static str, names: &[&str]) -> Vec<u32> {
    let mut results = Vec::new();
    for four_state in [false, true] {
        let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new(path))], "Top")
            .four_state(four_state)
            .build()
            .unwrap();
        sim.run_until(100).unwrap();
        let counts: Vec<u32> = names
            .iter()
            .map(|name| u32::try_from(sim.get(sim.signal(name))).unwrap())
            .collect();
        if four_state {
            assert_eq!(counts, results, "four-state against two-state");
        } else {
            results = counts;
        }
    }
    results
}

/// A function in a concurrent assertion operand is called on the sampled
/// values of its arguments, and its body reads current values (IEEE
/// 1800-2023 16.5.1): `cur()` reads `cnt` after the edge's update, `arg`
/// gets `cnt` before it. Verilator 5.054 prints `f1=10 f2=0 f3=0`: it
/// evaluates the body over sampled values too, which 16.5.1 does not.
#[test]
fn assertion_function_reads_current_values_on_sampled_arguments() {
    let source = r#"`timescale 1ns/1ns
module Top;
 bit clk;
 logic [3:0] cnt = 0;
 int f1, f2, f3;
 always #5 clk = ~clk;
 always_ff @(posedge clk) cnt <= cnt + 1;
 function automatic bit cur(); return cnt[0]; endfunction
 function automatic bit arg(input logic [3:0] v); return v[0]; endfunction
 a1: assert property (@(posedge clk) cur() == !cnt[0]) else f1++;
 a2: assert property (@(posedge clk) arg(cnt) == cnt[0]) else f2++;
 a3: assert property (@(posedge clk) arg(cnt) == cur()) else f3++;
endmodule"#;
    assert_eq!(
        counts_after_ten_edges(source, "assertion_function.sv", &["f1", "f2", "f3"]),
        [0, 0, 10]
    );
}

/// A function reading another instance's memory through a hierarchical
/// name, in nested loops, after `|->` and `|=>`, in a cover and under
/// `disable iff`. At 25 the memory's sampled value is still `8'h5a` and
/// its current value 0, which the function reads (16.5.1). Verilator
/// 5.054 prints `g1=1 g2=0 h=1 g3=1` (with `--coverage-user`, without which
/// it runs no cover action): it reads the sampled memory.
#[test]
fn assertion_function_reads_hierarchical_memory_in_implications() {
    let source = r#"`timescale 1ns/1ns
module Sub(input logic clk, input logic clr);
 logic [7:0] mem[4];
 initial foreach (mem[i]) mem[i] = 8'h5a;
 always_ff @(posedge clk) if (clr) foreach (mem[i]) mem[i] <= 0;
endmodule
module Top;
 bit clk, rst;
 logic [3:0] cnt = 0;
 logic start, clr;
 int g1, g2, g3, h;
 always #5 clk = ~clk;
 always_ff @(posedge clk) cnt <= cnt + 1;
 assign start = cnt == 2;
 assign clr = start;
 Sub u(.clk(clk), .clr(clr));
 function automatic logic all_zero();
  for (int i = 0; i < 4; i++)
   for (int j = 0; j < 8; j++)
    if (u.mem[i][j]) return 1'b0;
  return 1'b1;
 endfunction
 b1: assert property (@(posedge clk) $rose(start) |-> all_zero()) else g1++;
 b2: assert property (@(posedge clk) $rose(start) |=> all_zero()) else g2++;
 c1: cover property (@(posedge clk) $rose(start) ##1 all_zero()) h++;
 b3: assert property (@(posedge clk) disable iff (rst) $rose(start) |-> all_zero()) else g3++;
endmodule"#;
    assert_eq!(
        counts_after_ten_edges(
            source,
            "assertion_hierarchical.sv",
            &["g1", "g2", "h", "g3"]
        ),
        [0, 0, 1, 0]
    );
}

/// Purity is checked through the functions a function calls: static `f`
/// and `cur`, whose locals every call assigns before reading them, keep no
/// state (16.6) and call the pure `g`; `lsb` is an imported package
/// function. Verilator 5.054 prints `f1=10 f2=0 f3=0 f4=0`, reading `cnt`
/// sampled in `cur` as in the test above.
#[test]
fn assertion_function_calls_pure_functions() {
    let source = r#"`timescale 1ns/1ns
package pk;
 function automatic bit lsb(input logic [3:0] v); return v[0]; endfunction
endpackage
module Top;
 import pk::*;
 bit clk;
 logic [3:0] cnt = 0;
 int f1, f2, f3, f4;
 always #5 clk = ~clk;
 always_ff @(posedge clk) cnt <= cnt + 1;
 function automatic bit g(input logic [3:0] v); return v[0]; endfunction
 function bit f(input logic [3:0] v);
  logic [3:0] t;
  t = v;
  return g(t);
 endfunction
 function bit cur(); bit r; r = g(cnt); return r; endfunction
 a1: assert property (@(posedge clk) cur() == !cnt[0]) else f1++;
 a2: assert property (@(posedge clk) f(cnt) == cnt[0]) else f2++;
 a3: assert property (@(posedge clk) f(cnt) == cur()) else f3++;
 a4: assert property (@(posedge clk) lsb(cnt) == cnt[0]) else f4++;
endmodule"#;
    assert_eq!(
        counts_after_ten_edges(source, "assertion_pure_calls.sv", &["f1", "f2", "f3", "f4"]),
        [0, 0, 10, 0]
    );
}

/// Functions in concurrent assertion operands that 16.6 forbids, or that
/// Celox does not support there, are rejected by name.
#[test]
fn assertion_functions_that_are_not_pure_are_rejected() {
    for (declarations, operand, expected) in [
        (
            "int k; function automatic bit o(output int x); x = 1; return 1; endfunction",
            "o(k)",
            "call `o` with an output, inout or ref argument in a concurrent assertion operand",
        ),
        (
            "int k; function automatic bit c(const ref int x); return x[0]; endfunction",
            "c(k)",
            "call `c` with an output, inout or ref argument in a concurrent assertion operand",
        ),
        (
            "int n; function automatic bit s(); n++; return 1; endfunction",
            "s()",
            "function `s` in a concurrent assertion operand has a side effect (writes `n`)",
        ),
        (
            "function automatic bit d(); $display(\"d\"); return 1; endfunction",
            "d()",
            "function `d` in a concurrent assertion operand has a side effect (`$display`)",
        ),
        (
            "int n; function automatic bit s(); n++; return 1; endfunction
             function automatic bit w(); return s(); endfunction",
            "w()",
            "function `w` in a concurrent assertion operand has a side effect (through `s`)",
        ),
        (
            "function bit p(); int k; k++; return k[0]; endfunction",
            "p()",
            "function `p` in a concurrent assertion operand keeps state between calls (16.6)",
        ),
        // A static function's result keeps its value between calls, so one
        // that sets it on some paths only returns an earlier call's value.
        (
            "bit g; function bit q(); if (g) q = 1; endfunction",
            "q()",
            "function `q` in a concurrent assertion operand keeps state between calls (16.6)",
        ),
        // A call in a select bound that writes a module variable through
        // an output formal.
        (
            "int k; bit [7:0] data;
             function automatic int g(output int x); x = 1; return 0; endfunction
             function automatic bit f(); return data[g(k)]; endfunction",
            "f()",
            "has a side effect (calls `g` with an output, inout or ref argument",
        ),
        // A call in the select bound of a local the function assigns.
        (
            "int n; function automatic int s(); n++; return 0; endfunction
             function automatic bit t(); bit [3:0] x; x[s()] = 1; return x[0]; endfunction",
            "t()",
            "function `t` in a concurrent assertion operand has a side effect (through `s`)",
        ),
        (
            "import \"DPI-C\" pure function int dp(input int x);",
            "dp(1) == 1",
            "call of DPI import `dp` in a concurrent assertion operand",
        ),
        (
            "bit a; function automatic bit cur(); return a; endfunction",
            "$rose(cur())",
            "`$rose` of an operand that calls a function",
        ),
    ] {
        let source = format!(
            "`timescale 1ns/1ns\nmodule Top;\n bit clk;\n {declarations}\n always #5 clk = ~clk;\n assert property (@(posedge clk) {operand});\nendmodule"
        );
        let error = match celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("impure_assertion_function.sv"))],
            "Top",
        )
        .build()
        {
            Ok(_) => panic!("{operand} must be rejected"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains(expected), "{operand}: {error}");
    }
    // A package function, called by the name its import gives it.
    let source = "`timescale 1ns/1ns
package pk;
 int n;
 function automatic bit s(); n++; return 1; endfunction
endpackage
module Top;
 import pk::*;
 bit clk;
 always #5 clk = ~clk;
 assert property (@(posedge clk) s());
endmodule";
    let error = match celox::Simulation::from_sv_sources(
        vec![(source, Path::new("impure_package_function.sv"))],
        "Top",
    )
    .build()
    {
        Ok(_) => panic!("the package function must be rejected"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("in a concurrent assertion operand has a side effect (writes `"),
        "{error}"
    );
}
