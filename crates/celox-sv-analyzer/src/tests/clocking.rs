//! Clocking blocks (IEEE 1800-2023 clause 14): what the analysis rejects of
//! their declarations and uses.
use super::*;

fn analyze(source: &str) -> Result<Ir, AnalyzerError> {
    analyze_source(source, Path::new("clocking.sv"))
}

/// The error of a module `Top` with `items`, which declares `clk`, `d` and
/// `q`.
fn rejected(items: &str) -> AnalyzerError {
    let source = format!(
        "`timescale 1ns/1ns\nmodule Top; bit clk; logic [7:0] d, q; int i; {items} endmodule"
    );
    match analyze(&source) {
        Ok(_) => panic!("{source}: accepted"),
        Err(error) => error.into_error(),
    }
}

fn assert_unsupported(items: &str, construct: &str) {
    match rejected(items) {
        AnalyzerError::Unsupported(text) => {
            assert!(text.contains(construct), "{items}: unsupported `{text}`")
        }
        error => panic!("{items}: {error:?}, not unsupported `{construct}`"),
    }
}

fn assert_invalid(items: &str, rule: &str) {
    match rejected(items) {
        AnalyzerError::InvalidClocking(text) => {
            assert!(text.contains(rule), "{items}: invalid `{text}`")
        }
        error => panic!("{items}: {error:?}, not an invalid clocking `{rule}`"),
    }
}

const BLOCK: &str = "clocking cb @(posedge clk); input d; output q; endclocking";

#[test]
fn clocking_blocks_are_analyzed() {
    for items in [
        &format!("{BLOCK} initial begin @(cb); q = cb.d; cb.q <= cb.d + 1; cb.q[3:0] <= 4'h5; end"),
        // A default clocking or a global clocking declaration only matters
        // to `##` and `$global_clock`, which Celox rejects (14.12, 14.14).
        &"default clocking @(posedge clk); default input #10ns output #5ns; endclocking"
            .to_string(),
        &format!("{BLOCK} default clocking cb;"),
        &"default clocking dc @(posedge clk); input d; endclocking initial @(dc) q = dc.d;"
            .to_string(),
        &"global clocking g @(posedge clk); endclocking".to_string(),
        &"clocking cb @(posedge clk); input #2 d; input #1ns q; endclocking".to_string(),
        &"clocking cb @(posedge clk); input #0.5 d; output #1.25 q; endclocking".to_string(),
        // A local event of the block's name hides the block (14.10 makes
        // a trigger of the block event an error).
        &format!("{BLOCK} initial begin event cb; -> cb; @(cb); end"),
        &"parameter D = 2; clocking cb @(posedge clk); output #D q; endclocking \
          initial begin automatic int e = 1; @(cb); cb.q <= e; end"
            .to_string(),
    ] {
        let source =
            format!("`timescale 1ns/1ns\nmodule Top; bit clk; logic [7:0] d, q; {items} endmodule");
        analyze(&source).unwrap_or_else(|error| panic!("{source}: {error}"));
    }
}

#[test]
fn unsupported_clocking_constructs_name_themselves() {
    for (items, construct) in [
        (&format!("{BLOCK} initial ##2 q = 1;"), "cycle delay"),
        (&format!("{BLOCK} initial cb.q <= ##2 1;"), "cycle delay"),
        (
            &"clocking cb @(posedge clk); input negedge d; endclocking".to_string(),
            "edge",
        ),
        (
            &"clocking cb @(posedge clk); output negedge q; endclocking".to_string(),
            "edge",
        ),
        (
            &"clocking cb @(posedge clk); input e = d[3:0]; endclocking".to_string(),
            "hierarchical expression",
        ),
        (
            &"clocking cb @(posedge clk); input d; property p; d; endproperty endclocking"
                .to_string(),
            "clocking block",
        ),
        (&format!("{BLOCK} initial cb.q[i] <= 1;"), "run-time index"),
        (
            &"if (1) begin : g clocking cb @(posedge clk); input d; endclocking end".to_string(),
            "generate block",
        ),
        (
            &"parameter real D = 0.5; clocking cb @(posedge clk); input #D d; endclocking"
                .to_string(),
            "neither an integral constant",
        ),
        (
            &"typedef struct { logic a; } s_t; s_t s; clocking cb @(posedge clk); input s; endclocking"
                .to_string(),
            "unpacked structure",
        ),
        (
            &format!("{BLOCK} initial begin logic [7:0] q; cb.q <= 1; end"),
            "hides the clocking signal",
        ),
        (
            &"parameter D = 2; clocking cb @(posedge clk); output #D q; endclocking \
              initial begin localparam D = 4; cb.q <= 1; end"
                .to_string(),
            "hides a name of its output skew",
        ),
    ] {
        assert_unsupported(items, construct);
    }
}

#[test]
fn illegal_clockvar_uses_are_errors() {
    for (items, rule) in [
        // 14.3: an input clockvar is not written, an output one not read.
        (&format!("{BLOCK} initial cb.d <= 1;"), "no output clockvar"),
        (&format!("{BLOCK} initial d = cb.q;"), "no input clockvar"),
        // 14.16: a clockvar is written by synchronous drives alone.
        (&format!("{BLOCK} initial cb.q = 1;"), "synchronous drive"),
        (&format!("{BLOCK} assign cb.q = 1;"), "synchronous drive"),
        (
            &format!("{BLOCK} initial cb.q <= #1 1;"),
            "intra-assignment delay",
        ),
        // 14.10: the clocking block event is only waited for.
        (&format!("{BLOCK} initial -> cb;"), "no event variable"),
        (&format!("{BLOCK} initial cb = null;"), "no event variable"),
        (
            &"clocking cb @(posedge clk); input d; output d; endclocking".to_string(),
            "declared twice",
        ),
        // 14.12, 14.14: one default and one global clocking in a scope.
        (
            &format!("{BLOCK} default clocking cb; default clocking cb;"),
            "more than one default clocking",
        ),
        (
            &"global clocking g @(posedge clk); endclocking global clocking h @(clk); endclocking"
                .to_string(),
            "more than one global clocking",
        ),
        (
            &format!("{BLOCK} default clocking nope;"),
            "names no clocking block",
        ),
    ] {
        assert_invalid(items, rule);
    }
}

/// A variable that hides an interface instance of the same name is no
/// interface instance: a member path through it names no clockvar.
#[test]
fn variables_hide_interface_instances_with_clocking_blocks() {
    let source = "`timescale 1ns/1ns
interface bus(input bit clk); logic [3:0] x; clocking cb @(posedge clk); input x; endclocking endinterface
module Top;
    typedef struct packed { logic [3:0] x; } inner_t;
    typedef struct packed { inner_t cb; } outer_t;
    bit clk;
    bus bus(clk);
    function automatic logic [3:0] f(outer_t bus); return bus.cb.x; endfunction
    initial $display(\"%0d\", f(4'h5));
endmodule";
    analyze(source).unwrap_or_else(|error| panic!("{error}"));
}

/// A module that calls an interface task through an interface port works on
/// copies of the members it reaches, which the samples of a clocking block
/// would reach after the clocking block event: Celox rejects it by name.
#[test]
fn clocking_blocks_reached_through_interface_ports_are_unsupported() {
    for port in ["bus.m b", "bus b"] {
        let source = format!(
            "`timescale 1ns/1ns
interface bus(input bit clk);
    logic [3:0] d;
    clocking cb @(posedge clk); input d; endclocking
    task automatic read(output logic [3:0] v); @(cb); v = cb.d; endtask
    modport m(import read);
endinterface
module user({port});
    initial begin logic [3:0] v; b.read(v); end
endmodule
module Top; bit clk; bus i(clk); user u(i); endmodule"
        );
        match analyze(&source).map_err(AnalyzerError::into_error) {
            Err(AnalyzerError::Unsupported(text)) => assert!(
                text.contains("clocking block `cb` of interface `bus` used by a task"),
                "{port}: {text}"
            ),
            other => panic!("{port}: {other:?}"),
        }
    }
}
