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
fn static_subroutine_locals_that_keep_their_value_are_unsupported() {
    for source in [
        "module Top(input logic clk, output logic [7:0] y);
           function int f(); int c; c = c + 1; return c; endfunction
           always_ff @(posedge clk) y <= f();
         endmodule",
        "module Top(output logic [7:0] y);
           function automatic int f(); static int c = 3; c++; return c; endfunction
           assign y = f();
         endmodule",
    ] {
        let detail = error(source);
        assert!(detail.contains("static variable `c`"), "{detail}");
    }
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
    assert!(detail.contains("latch"), "{detail}");
    assert_eq!(
        output(
            "module Top(input logic a, output logic [7:0] y);
               always_comb begin automatic int t; if (a) t = 5; y = a ? t : 1; end
             endmodule"
        ),
        1
    );
}
