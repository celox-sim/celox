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
        &format!("{BLOCK} initial begin @(cb); q = cb.d; @(cb.d); q = cb.d[3:0]; end"),
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
        (&format!("{BLOCK} initial cb.q <= 1;"), "synchronous drive"),
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
