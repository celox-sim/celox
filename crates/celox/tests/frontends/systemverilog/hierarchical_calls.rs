//! Calls of the functions and tasks of other instances by hierarchical
//! names (IEEE 1800-2023 23.8).

use super::*;

fn displays(source: &str, four_state: bool, until: u64) -> Vec<String> {
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("calls.sv"))], "Top")
        .four_state(four_state)
        .build_cranelift()
        .unwrap();
    sim.run_until(until).unwrap();
    sim.drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => Some(String::from_utf8(message).unwrap()),
            _ => None,
        })
        .collect()
}

fn build_error(source: &str) -> String {
    Simulator::from_sv_sources(vec![(source, Path::new("calls.sv"))], "Top")
        .build_cranelift()
        .expect_err("the design must be rejected")
        .to_string()
        // The report wraps long messages.
        .replace('│', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// A task or function of another instance runs on that instance's objects,
// called by a name upward from an instance below it, as OpenPiton's checkers
// call `cmp_top.monitor.fail(...)`, or downward from above it. The body
// writes the callee's variables and, through the callee's own hierarchical
// name, a variable of another instance. Its static storage is the callee's,
// which its own calls share; automatic storage belongs to each call; the
// callee's parameters have their values, not the caller's of the same name.
// An omitted argument takes its default (13.5.3). An `always_comb` that
// calls a function of another instance runs again when what the function
// reads changes (9.2.2.2.1).
#[test]
fn subroutines_of_other_instances_run_on_their_objects() {
    let source = r#"
        `timescale 1ns/1ns
        module Monitor #(parameter int W = 4) (input logic clk);
            int count = 0;
            int total = 0;
            logic [7:0] last;
            logic [W-1:0] ones;
            task note(input [7:0] v);
                count = count + 1;
                last = v;
                Top.flag = 1'b1;
            endtask
            task bump;
                int n;
                n = n + 1;
                total = n;
                ones = {W{1'b1}};
            endtask
            task automatic get(output int value, input int add);
                int scratch;
                scratch = count + add;
                value = scratch;
            endtask
            task mark(input int value = 7);
                total = value;
            endtask
            function automatic int twice(int x);
                return 2 * x + count;
            endfunction
            initial #0 bump();
        endmodule
        module Probe #(parameter int W = 2) (
            input logic clk, input logic [7:0] v, input logic [7:0] at
        );
            int got;
            always @(posedge clk) if (v == at) begin
                Top.monitor.note(v);
                Top.monitor.bump();
                Top.monitor.get(got, W);
            end
        endmodule
        module Top;
            logic clk = 0;
            logic [7:0] v = 0;
            logic flag = 0;
            int y;
            Monitor #(.W(8)) monitor(.clk(clk));
            Probe a(.clk(clk), .v(v), .at(8'd3));
            Probe b(.clk(clk), .v(v), .at(8'd4));
            always_comb y = monitor.twice(1);
            initial begin
                repeat (6) begin
                    #1 clk = 1;
                    #1 clk = 0;
                    v = v + 1;
                end
                $display("%0d %h %b %0d %0d %h %0d %0d %0d", monitor.count, monitor.last, flag,
                    monitor.twice(5), monitor.total, monitor.ones, a.got, b.got, y);
                monitor.note(8'h9);
                monitor.mark();
                #1 $display("%0d %h %0d %0d", monitor.count, monitor.last, y, monitor.total);
            end
        endmodule
    "#;
    for four_state in [false, true] {
        assert_eq!(
            displays(source, four_state, 100),
            // The probes note 03 and 04, one after the other: the count is
            // 2, the static `n` counts the monitor's own call and both
            // probes', and each get adds the probe's W to the count it sees.
            ["2 04 1 12 3 ff 3 4 4", "3 09 5 7"],
            "four_state={four_state}"
        );
    }
}

// A call in an inactive generate branch calls nothing, so a module may call
// a subroutine of a module whose inactive branch would call it back. An
// instance index may be a constant expression (23.6), and a call in a type
// query may omit an argument that has a default (13.5.3).
#[test]
fn hierarchical_calls_of_generate_branches_arrays_and_type_queries() {
    let source = r#"
        `timescale 1ns/1ns
        module A;
            int n = 0;
            task ta;
                n = n + 1;
            endtask
            initial #1 Top.b.tb();
        endmodule
        module B;
            int m = 0;
            task tb;
                m = m + 10;
            endtask
            if (0) begin : never
                initial Top.a.ta();
            end
        endmodule
        module Monitor;
            int count = 0;
            task note(input int v);
                count = count + v;
            endtask
            function automatic logic [5:0] widest(int x = 1);
                return x;
            endfunction
        endmodule
        module Top;
            localparam int IDX = 1;
            A a();
            B b();
            Monitor ms[2]();
            initial begin
                ms[0].note(2);
                ms[IDX + 0].note(5);
                #2 $display("%0d %0d %0d %0d %0d", a.n, b.m, ms[0].count, ms[1].count,
                    $bits(ms[IDX].widest()));
            end
        endmodule
    "#;
    for four_state in [false, true] {
        assert_eq!(
            displays(source, four_state, 10),
            ["0 10 2 5 6"],
            "four_state={four_state}"
        );
    }
}

// A name that calls no subroutine of the instance it reaches is rejected,
// as are calls between the subroutines of two modules in both directions,
// whose bodies the analysis of each would need first.
#[test]
fn unresolved_and_mutual_hierarchical_calls_are_rejected() {
    let unknown = r#"
        module Monitor; task note; endtask endmodule
        module Top; Monitor monitor(); initial monitor.nothing(); endmodule
    "#;
    let error = build_error(unknown);
    assert!(
        error.contains("unresolved hierarchical reference `monitor.nothing`"),
        "{error}"
    );
    let mutual = r#"
        module A; int n; task ta; n = 1; Top.b.tb(); endtask endmodule
        module B; int m; task tb; m = 1; Top.a.ta(); endtask endmodule
        module Top; A a(); B b(); initial a.ta(); endmodule
    "#;
    let error = build_error(mutual);
    assert!(
        error.contains("hierarchical calls between the subroutines of modules"),
        "{error}"
    );
}
