//! `$tb::clock_gen` and `clk.next(n)` behave as the SystemVerilog the
//! execution semantics guide defines them by: every clock generator runs from
//! time 0, rising through a nonblocking assignment at multiples of its period
//! and falling half a period later, and `clk.next(n)` resumes a period after
//! the `n`-th rising edge it waits for.
//!
//! The expected values come from running that SystemVerilog under Icarus
//! Verilog 13.0 and Verilator 5.052, which agree:
//!
//! ```systemverilog
//! bit clk = 0, clk2 = 0;
//! initial forever begin clk <= 1; #2 clk <= 0; #2; end
//! initial forever begin clk2 <= 1; #3 clk2 <= 0; #3; end
//! task automatic clk_next(int unsigned k);
//!   if (k != 0) begin repeat (k) @(posedge clk); #4; end
//! endtask
//! ```

use celox::Simulator;
use celox::TestResult;
use celox::testbench::{compile_initial_testbench, run_compiled_testbench_to_finish};

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

/// A clock no process waits on still runs: `clk2` rises at 0, 6 and 12,
/// and the process resuming at 12 runs before that edge.
#[test]
fn clocks_run_whether_or_not_a_process_waits() {
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
        $assert(p == 8'd1 && q == 8'd6 && p2 == 8'd1 && q2 == 8'd10, "t=4: p=%d q=%d p2=%d q2=%d", p, q, p2, q2);
        x = 8'd7;
        clk.next(2);
        $assert(p == 8'd3 && q == 8'd8 && p2 == 8'd2 && q2 == 8'd10, "t=12: p=%d q=%d p2=%d q2=%d", p, q, p2, q2);
        $finish();
    }
}
"#);
    assert_eq!(result, TestResult::Pass);
}
