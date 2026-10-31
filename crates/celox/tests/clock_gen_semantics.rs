//! `$tb::clock_gen` and `clk.next(n)` behave as the SystemVerilog the
//! execution semantics guide defines them by: a clock generator is an
//! oscillator that rises through a nonblocking assignment only while some
//! process waits on it, a whole period apart, and falls half a period after
//! each rise; `clk.next(n)` resumes a period after the `n`-th rising edge it
//! waits for.
//!
//! The expected values come from running that SystemVerilog under Icarus
//! Verilog 13.0, and agree with `veryl test` (Veryl 0.22):
//!
//! ```systemverilog
//! bit clk = 0; int unsigned clk_waiters = 0;
//! initial forever begin
//!   wait (clk_waiters != 0);
//!   clk <= 1; #H clk <= 0; #L;
//! end
//! task automatic clk_next(int unsigned n);
//!   if (n != 0) begin
//!     clk_waiters++; repeat (n) @(posedge clk); clk_waiters--; #P;
//!   end
//! endtask
//! ```

use celox::TestResult;
use celox::testbench::{compile_initial_testbench, run_compiled_testbench_to_finish};
use celox::{SchedulerMode, Simulator};

const COUNTERS: &str = r#"
module Cnt (
    i_clk: input  clock    ,
    i_x  : input  logic<8> ,
    o_p  : output logic<8> ,
    o_q  : output logic<8> ,
) {
    var p: logic<8>;
    var q: logic<8>;
    always_ff (i_clk) {
        p = p + 8'd1;
        q = i_x + 8'd1;
    }
    assign o_p = p;
    assign o_q = q;
}

module NegCnt (
    i_clk: input  clock_negedge,
    o_n  : output logic<8>     ,
) {
    var n: logic<8>;
    always_ff (i_clk) {
        n = n + 8'd1;
    }
    assign o_n = n;
}
"#;

fn run(top: &str) -> TestResult {
    let code = format!("{COUNTERS}\n{top}");
    let mut sim = Simulator::builder(&code, "Top").build().unwrap();
    let tb = compile_initial_testbench(&sim).unwrap();
    run_compiled_testbench_to_finish(&mut sim, &tb)
}

/// One process on one clock: the falling edge runs negative-edge domains,
/// and a write between two waits reaches the next rising edge.
#[test]
fn one_process_takes_both_edges_of_each_period() {
    let result = run(r#"
#[test(Top)]
module Top {
    inst clk: $tb::clock_gen #(period: 4);
    var x: logic<8>;
    var p: logic<8>;
    var q: logic<8>;
    var n: logic<8>;
    inst a: Cnt (i_clk: clk, i_x: x, o_p: p, o_q: q);
    inst an: NegCnt (i_clk: clk, o_n: n);
    initial {
        x = 8'd5;
        clk.next(1);
        $assert(p == 8'd1 && q == 8'd6 && n == 8'd1 && clk == 1'b0, "t=4: p=%d q=%d n=%d clk=%b", p, q, n, clk);
        x = 8'd7;
        clk.next(2);
        $assert(p == 8'd3 && q == 8'd8 && n == 8'd3 && clk == 1'b0, "t=12: p=%d q=%d n=%d clk=%b", p, q, n, clk);
        clk.next(0);
        $assert(p == 8'd3 && n == 8'd3, "next(0) waits for nothing: p=%d n=%d", p, n);
        $finish();
    }
}
"#);
    assert_eq!(result, TestResult::Pass);
}

/// The same with a second process: concurrency changes nothing.
#[test]
fn a_second_process_changes_nothing() {
    let result = run(r#"
#[test(Top)]
module Top {
    inst clk: $tb::clock_gen #(period: 4);
    var x: logic<8>;
    var p: logic<8>;
    var q: logic<8>;
    var n: logic<8>;
    inst a: Cnt (i_clk: clk, i_x: x, o_p: p, o_q: q);
    inst an: NegCnt (i_clk: clk, o_n: n);
    initial {
        x = 8'd5;
        clk.next(1);
        $assert(p == 8'd1 && q == 8'd6 && n == 8'd1 && clk == 1'b0, "t=4: p=%d q=%d n=%d clk=%b", p, q, n, clk);
        x = 8'd7;
        clk.next(2);
        $assert(p == 8'd3 && q == 8'd8 && n == 8'd3 && clk == 1'b0, "t=12: p=%d q=%d n=%d clk=%b", p, q, n, clk);
        $finish();
    }
    initial {
        clk.next(1);
    }
}
"#);
    assert_eq!(result, TestResult::Pass);
}

/// A clock no process waits on takes no edge, so its domains do not run.
#[test]
fn a_clock_nobody_waits_on_takes_no_edge() {
    let result = run(r#"
#[test(Top)]
module Top {
    inst clk: $tb::clock_gen #(period: 4);
    inst clk2: $tb::clock_gen #(period: 6);
    var x: logic<8>;
    var y: logic<8>;
    var p: logic<8>;
    var q: logic<8>;
    var p2: logic<8>;
    var q2: logic<8>;
    inst a: Cnt (i_clk: clk, i_x: x, o_p: p, o_q: q);
    inst b: Cnt (i_clk: clk2, i_x: y, o_p: p2, o_q: q2);
    initial {
        x = 8'd5;
        y = 8'd9;
        clk.next(1);
        $assert(p == 8'd1 && q == 8'd6 && p2 == 8'd0 && clk2 == 1'b0, "t=4: p=%d q=%d p2=%d clk2=%b", p, q, p2, clk2);
        x = 8'd7;
        clk.next(2);
        $assert(p == 8'd3 && q == 8'd8 && p2 == 8'd0, "t=12: p=%d q=%d p2=%d", p, q, p2);
        $finish();
    }
}
"#);
    assert_eq!(result, TestResult::Pass);
}

const COUNTER: &str = r#"
module Counter (clk: input clock, q: output bit<32>) {
    always_ff { q += 1; }
}
module Capture (clk: input clock, d: input bit<32>, q: output bit<32>) {
    always_ff { q = d; }
}
module ResetCounter (clk: input clock, rst: input reset, q: output bit<32>) {
    always_ff {
        if_reset { q = 0; } else { q += 1; }
    }
}
"#;

/// Run `top` on every backend with the reference and the compiled testbench
/// schedulers, in two-state and four-state storage, and expect it to pass.
fn run_everywhere(top: &str) {
    let code = format!("{COUNTER}\n{top}");
    let builder = |mode, four_state| {
        Simulator::builder(&code, "Top")
            .scheduler_mode(mode)
            .four_state(four_state)
    };
    fn check<B: celox::SimBackend>(name: &str, sim: Result<Simulator<B>, celox::SimulatorError>) {
        let mut sim = sim.unwrap_or_else(|error| panic!("{name}: {error}"));
        let tb = compile_initial_testbench(&sim).unwrap();
        assert_eq!(
            run_compiled_testbench_to_finish(&mut sim, &tb),
            TestResult::Pass,
            "{name}"
        );
    }
    for mode in [SchedulerMode::Reference, SchedulerMode::Compiled] {
        for four_state in [false, true] {
            let name = |backend| format!("{backend} {mode:?} four_state={four_state}");
            check(&name("native"), builder(mode, four_state).build_native());
            check(
                &name("interpreter"),
                builder(mode, four_state).build_interpreter(),
            );
            check(&name("tiered"), builder(mode, four_state).build_tiered());
            #[cfg(feature = "cranelift")]
            check(
                &name("cranelift"),
                builder(mode, four_state).build_cranelift(),
            );
            #[cfg(feature = "wasm-runtime")]
            check(&name("wasm"), builder(mode, four_state).build_wasm());
        }
    }
}

/// #2764: only the clock being waited on counts.
#[test]
fn only_the_awaited_clock_counts() {
    run_everywhere(
        r#"
#[test(Top)]
module Top {
    inst fast: $tb::clock_gen #(period: 2);
    inst slow: $tb::clock_gen #(period: 10);
    var a: bit<32>;
    var b: bit<32>;
    inst ca: Counter (clk: fast, q: a);
    inst cb: Counter (clk: slow, q: b);
    initial {
        fast.next(20);
        $assert(a == 20 && b == 0, "after fast: a=%d b=%d", a, b);
        slow.next(2);
        $assert(a == 20 && b == 2, "after slow: a=%d b=%d", a, b);
        $finish();
    }
}
"#,
    );
}

/// A clock left idle for several of its periods restarts at the instant a
/// process waits on it again, not on its old grid. `slow` rises at 0, then
/// idles while `fast` runs (rising at every even time), and rises again at
/// 34, when the process waits on it: `cap` samples `a` in the same
/// nonblocking region as `fast`'s rise at 34, so it reads the 17 rises at
/// 0..=32. On `slow`'s old grid it would have risen at 40 and read 20.
#[test]
fn an_idle_clock_restarts_when_waited_on() {
    run_everywhere(
        r#"
#[test(Top)]
module Top {
    inst fast: $tb::clock_gen #(period: 2);
    inst slow: $tb::clock_gen #(period: 10);
    var a: bit<32>;
    var b: bit<32>;
    var cap: bit<32>;
    inst ca: Counter (clk: fast, q: a);
    inst cb: Counter (clk: slow, q: b);
    inst cc: Capture (clk: slow, d: a, q: cap);
    initial {
        slow.next(1);
        $assert(a == 5 && b == 1, "t=10: a=%d b=%d", a, b);
        fast.next(12);
        $assert(a == 17 && b == 1, "t=34: a=%d b=%d", a, b);
        slow.next(1);
        $assert(b == 2 && cap == 17, "t=44: b=%d cap=%d", b, cap);
        $finish();
    }
    initial {
        for i in 0..100 { fast.next(); }
    }
}
"#,
    );
}

/// Processes waiting on the same clock count the same edges, and the clock
/// keeps running for the one still waiting after the other resumes.
#[test]
fn processes_on_one_clock_count_the_same_edges() {
    run_everywhere(
        r#"
#[test(Top)]
module Top {
    inst clk: $tb::clock_gen #(period: 4);
    var c: bit<32>;
    var seen: bit<32>;
    inst cc: Counter (clk, q: c);
    initial {
        clk.next(2);
        seen = c;
    }
    initial {
        clk.next(5);
        $assert(seen == 2 && c == 5, "seen=%d c=%d", seen, c);
        $finish();
    }
}
"#,
    );
}

/// `rst.assert(n)` alone runs its clock for `n` edges; before and after it,
/// nobody waits on the clock and it takes no edge.
#[test]
fn reset_assert_runs_its_clock_alone() {
    run_everywhere(
        r#"
#[test(Top)]
module Top {
    inst clk: $tb::clock_gen #(period: 4);
    inst rst: $tb::reset_gen (clk);
    inst other: $tb::clock_gen #(period: 2);
    var free: bit<32>;
    var held: bit<32>;
    inst cf: Counter (clk, q: free);
    inst ch: ResetCounter (clk, rst, q: held);
    initial {
        other.next(3);
        $assert(free == 0, "before reset: free=%d", free);
        rst.assert(2);
        $assert(free == 2 && held == 0, "after reset: free=%d held=%d", free, held);
        other.next(3);
        $assert(free == 2 && held == 0, "idle again: free=%d held=%d", free, held);
        clk.next(3);
        $assert(free == 5 && held == 3, "after next: free=%d held=%d", free, held);
        $finish();
    }
}
"#,
    );
}
