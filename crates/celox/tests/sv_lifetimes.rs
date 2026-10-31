#![cfg(feature = "systemverilog")]
//! Variable lifetimes (IEEE 1800-2023 6.21): static locals keep their value.

use celox::{Simulation, Simulator};

fn build(source: &str) -> Result<Simulator, String> {
    Simulator::from_sv_sources(vec![(source, std::path::Path::new("lifetimes.sv"))], "Top")
        .build()
        .map_err(|error| format!("{error:?}"))
}

/// `y` after each of `ticks` rising edges of `clk`.
fn ticks(source: &str, ticks: usize) -> Vec<u64> {
    let mut simulator = build(source).unwrap_or_else(|error| panic!("{error}"));
    let clk = simulator.event("clk");
    let y = simulator.signal("y");
    (0..ticks)
        .map(|_| {
            simulator.tick(clk).unwrap();
            u64::try_from(simulator.get(y)).unwrap()
        })
        .collect()
}

fn output(source: &str) -> u64 {
    let mut simulator = build(source).unwrap_or_else(|error| panic!("{error}"));
    let y = simulator.signal("y");
    u64::try_from(simulator.get(y)).unwrap()
}

/// `y` once the processes that start at time zero have run.
fn settled(source: &str) -> u64 {
    let mut simulation =
        Simulation::from_sv_sources(vec![(source, std::path::Path::new("lifetimes.sv"))], "Top")
            .build()
            .unwrap_or_else(|error| panic!("{error}"));
    simulation.run_until(1).unwrap();
    let y = simulation.signal("y");
    u64::try_from(simulation.get(y)).unwrap()
}

fn error(source: &str) -> String {
    match build(source) {
        Ok(_) => panic!("the design must be rejected"),
        Err(error) => error,
    }
}

#[test]
fn static_block_locals_keep_their_value() {
    // Initialized once, at time zero.
    assert_eq!(
        ticks(
            "module Top(input logic clk, output logic [7:0] y);
               always_ff @(posedge clk) begin static logic [7:0] c = 0; c = c + 1; y <= c; end
             endmodule",
            3
        ),
        [1, 2, 3]
    );
    // Static by default, from the default value of `int`.
    assert_eq!(
        ticks(
            "module Top(input logic clk, output logic [7:0] y);
               always_ff @(posedge clk) begin int c; c = c + 1; y <= c; end
             endmodule",
            3
        ),
        [1, 2, 3]
    );
    // A nonblocking assignment updates the local after the block.
    assert_eq!(
        ticks(
            "module Top(input logic clk, output logic [7:0] y);
               always_ff @(posedge clk) begin bit [7:0] c; c <= 8'd5; y <= c; end
             endmodule",
            3
        ),
        [0, 5, 5]
    );
}

#[test]
fn automatic_block_locals_are_initialized_on_entry() {
    assert_eq!(
        ticks(
            "module Top(input logic clk, output logic [7:0] y);
               always_ff @(posedge clk) begin automatic int c = 0; c = c + 1; y <= c; end
             endmodule",
            3
        ),
        [1, 1, 1]
    );
    assert_eq!(
        ticks(
            "module automatic Top(input logic clk, output logic [7:0] y);
               always_ff @(posedge clk) begin int c; c = c + 1; y <= c; end
             endmodule",
            3
        ),
        [1, 1, 1]
    );
}

/// The example of IEEE 1800-2023 6.21: a static initializer runs once, an
/// automatic one on every entry.
#[test]
fn loop_block_initializers_follow_their_lifetime() {
    let source = |lifetime: &str| {
        format!(
            "module Top(output logic [7:0] y);
               initial begin
                 for (int i = 0; i < 3; i++) begin
                   {lifetime} int count = 0;
                   for (int j = 0; j < 3; j++) begin count++; y = count; end
                 end
               end
             endmodule"
        )
    };
    assert_eq!(settled(&source("static")), 9);
    assert_eq!(settled(&source("automatic")), 3);
}

#[test]
fn static_locals_written_before_they_are_read_are_temporaries() {
    assert_eq!(
        output(
            "module Top(output logic [7:0] y);
               always_comb begin logic [7:0] c; c = 8'd3; y = c + 1; end
             endmodule"
        ),
        4
    );
    // Locals of static functions, and in nested blocks.
    assert_eq!(
        output(
            "package p;
               function int twice(input int a); int t; t = a * 2; return t; endfunction
             endpackage
             module Top(output logic [7:0] y);
               function int f(input int a);
                 int k = 1;
                 begin int t; if (a > 2) t = a; else t = 0; return t + k + p::twice(a); end
               endfunction
               assign y = f(3);
             endmodule"
        ),
        10
    );
}

#[test]
fn static_task_locals_that_keep_their_value_remain_unsupported() {
    let detail = error(
        "module Top(input logic clk, output int y);
        task count(output int result); static int saved=0; saved++; result=saved; endtask
        always @(posedge clk) count(y); endmodule",
    );
    assert!(detail.contains("static variable `saved`"), "{detail}");
}

#[test]
fn automatic_subroutines_may_read_their_locals_first() {
    assert_eq!(
        output(
            "module automatic Top(output logic [7:0] y);
               function int f(input int a); int t; t = t + a; return t; endfunction
               assign y = f(3) + f(4);
             endmodule"
        ),
        7
    );
}

/// A static local of `always_comb` holds its value on the paths that do not
/// write it, so it is a latch; one written on every path is not.
#[test]
fn static_comb_locals_written_on_some_paths_are_latches() {
    let detail = error(
        "module Top(input logic a, output logic [7:0] y);
           always_comb begin int t; if (a) t = 5; y = a ? t : 0; end
         endmodule",
    );
    assert!(detail.contains("InferredLatch"), "{detail}");
    assert_eq!(
        output(
            "module Top(input logic a, output logic [7:0] y);
               always_comb begin automatic int t; if (a) t = 5; y = a ? t : 1; end
             endmodule"
        ),
        1
    );
}

/// Review cases: initializers, output arguments, loop exits and interfaces.
#[test]
fn static_initializers_must_be_constant() {
    // A static initializer runs once; one that reads a variable or calls a
    // function with effects cannot.
    let detail = error(
        "module Top(input logic [7:0] a, output logic [7:0] y);
           always_comb begin logic [7:0] t = a; y = t; end
         endmodule",
    );
    assert!(
        detail.contains("initializer that is not constant"),
        "{detail}"
    );
    // A constant call folds.
    assert_eq!(
        ticks(
            "module Top(input logic clk, output logic [7:0] y);
               always_ff @(posedge clk) begin static int c = $countones(3); c++; y <= c; end
             endmodule",
            2
        ),
        [3, 4]
    );
}

#[test]
fn output_arguments_and_loop_exits_write_static_locals() {
    // IEEE 1800-2023 13.4(b): even a zero-time task cannot be called by a function.
    assert_eq!(
        output(
            "module Top(output logic [7:0] y);
               function void set(output int v, input int a); v = a; endfunction
               function int f(input int a);
                 int t, u;
                 set(t, a);
                 do begin u = t + 1; break; end while (1);
                 return u;
               endfunction
               assign y = f(4);
             endmodule"
        ),
        5
    );
}

#[test]
fn interfaces_keep_their_default_lifetime() {
    // IEEE 1800-2023 6.21: the interface's default lifetime reaches its
    // procedural locals. Lowering the original declaration preserves it.
    assert_eq!(
        ticks(
            "interface automatic I;
               logic clk; logic [7:0] v;
               always_ff @(posedge clk) begin int c = 0; c++; v <= c; end
             endinterface
             module Top(input logic clk, output logic [7:0] y);
               I i(); assign i.clk = clk; assign y = i.v;
             endmodule",
            3,
        ),
        [1, 1, 1],
    );
    assert_eq!(
        ticks(
            "interface I;
               logic clk; logic [7:0] v;
               always_ff @(posedge clk) begin static int c = 0; c++; v <= c; end
             endinterface
             module Top(input logic clk, output logic [7:0] y);
               I i(); assign i.clk = clk; assign y = i.v;
             endmodule",
            3,
        ),
        [1, 2, 3],
    );
}

/// Review cases: static function results, initializer order, and the
/// selectors of concatenated targets.
#[test]
fn static_function_results_keep_their_value() {
    assert_eq!(
        output(
            "module Top(output logic [7:0] y);
               function logic [7:0] f(input logic s); if (s) f = 8'd7; else f = 8'd1; endfunction
               assign y = f(1'b1) + f(1'b0);
             endmodule"
        ),
        8
    );
    // A case whose items cover every value of its selector assigns the
    // result on every path.
    assert_eq!(
        output(
            "module Top(output logic [7:0] y);
               function logic [3:0] dec(input bit [1:0] a);
                 case (a) 2'd0: dec = 4'd1; 2'd1: dec = 4'd2; 2'd2: dec = 4'd4; 2'd3: dec = 4'd8; endcase
               endfunction
               assign y = dec(2'd2);
             endmodule"
        ),
        4
    );
}

#[test]
fn static_initializers_run_before_initial_blocks() {
    assert_eq!(
        settled(
            "module Top(output logic [7:0] y);
               initial begin static int c = 3; y = c; c++; end
             endmodule"
        ),
        3
    );
}

/// A static initializer may be any run-time expression; it runs once,
/// before any procedure starts (IEEE 1800-2023 6.8, 6.21).
#[test]
fn run_time_static_initializers_run_once_before_any_procedure() {
    // The initializers read the module variable before the other `initial`
    // procedure writes it, and in declaration order. Without the `static`
    // keyword that 6.21 requires, an `initial` procedure's locals still
    // follow this rule.
    assert_eq!(
        settled(
            "module Top(output logic [31:0] y);
               typedef enum {a, b, c} e;
               int m = 1;
               initial m = 5;
               initial begin
                 e v = b;
                 string s = v.name();
                 int k = m + 1;
                 static int j = k * 10 + v;
                 v = c;
                 y = {s.len() == 1 && s == \"b\", 7'(k), 8'(j), 8'(m)};
               end
             endmodule"
        ),
        0x82_1505
    );
    // Also when an edge process that makes a host request reorders the
    // processes by creation.
    assert_eq!(
        settled(
            "module Top(input logic clk, output logic [7:0] y);
               int dummy;
               int m = dummy + 4;
               always @(posedge clk) $fflush;
               initial begin static int k = m + 1; y = k; end
             endmodule"
        ),
        5
    );
    // A static in a loop is initialized once, not on each iteration.
    assert_eq!(
        settled(
            "module Top(output logic [7:0] y);
               int m = 1;
               initial for (int i = 0; i < 3; i++) begin
                 static int once = m;
                 once++;
                 y = once;
               end
             endmodule"
        ),
        4
    );
    // An explicitly static local of an `always` procedure as well.
    assert_eq!(
        ticks(
            "module Top(input logic clk, output logic [7:0] y);
               int m = 4;
               always_ff @(posedge clk) begin static int c = m + 1; c++; y <= c; end
             endmodule",
            2
        ),
        [6, 7]
    );
    // The statics of a function it calls are initialized before it.
    assert_eq!(
        settled(
            "module Top(output logic [7:0] y);
               int m = 1;
               function int f(input int a);
                 static int c = 7;
                 c++;
                 return c + a;
               endfunction
               initial begin static int k = f(m); y = k + f(0); end
             endmodule"
        ),
        18
    );
}

/// An initializer that reads an automatic variable, or a keyword-less one
/// of a procedure that repeats, could only run on each entry.
#[test]
fn run_time_static_initializers_that_could_repeat_remain_unsupported() {
    for source in [
        "module Top(output logic [7:0] y);
           initial begin automatic int x = 3; int t = x + 1; y = t; end
         endmodule",
        "module Top(output logic [7:0] y);
           initial begin automatic int x = 3; static int t = x + 1; y = t; end
         endmodule",
        "module Top(input logic clk, output logic [7:0] y);
           int m = 4;
           always_ff @(posedge clk) begin int c = m + 1; c++; y <= c; end
         endmodule",
    ] {
        let detail = error(source);
        assert!(
            detail.contains("initializer that is not constant"),
            "{detail}"
        );
    }
}

#[test]
fn concatenated_targets_select_with_the_entry_value() {
    assert_eq!(
        ticks(
            "module Top(input logic clk, output logic [7:0] y);
               logic [7:0] bits = 0;
               always_ff @(posedge clk) begin
                 static int i = 0;
                 {i, bits[i]} = {32'd1, 1'b1};
                 y <= bits;
               end
             endmodule",
            2
        ),
        [1, 3]
    );
}

/// Review cases: four-state case selectors, guaranteed `repeat` iterations,
/// and writes through arguments of functions called in expressions.
#[test]
fn four_state_selectors_repeats_and_inout_arguments() {
    assert_eq!(
        output(
            "module Top(output logic [7:0] y);
               function int f(input int a); int t; repeat (1) t = a; return t; endfunction
               assign y = f(6);
             endmodule"
        ),
        6
    );
    assert_eq!(
        ticks(
            "module Top(input logic clk, output logic [7:0] y);
               function automatic int bump(inout int x); x++; return x; endfunction
               always_ff @(posedge clk) begin static int t = 1; y <= bump(t); end
             endmodule",
            2
        ),
        [2, 3]
    );
}
