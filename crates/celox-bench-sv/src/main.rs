//! Generate SystemVerilog from Veryl stdlib sources for Verilator benchmarks.
//!
//! Usage: cargo run -p celox-bench-sv
//!
//! Outputs .sv files to benches/verilator/

use std::path::{Path, PathBuf};
use veryl_analyzer::{Analyzer, Context, attribute_table, symbol_table};
use veryl_emitter::Emitter;
use veryl_metadata::Metadata;
use veryl_parser::Parser;

fn emit_sv(code: &str) -> String {
    symbol_table::clear();
    attribute_table::clear();

    let parser = Parser::parse(code, &"").unwrap();
    let mut metadata = Metadata::create_default("prj").unwrap();
    metadata.build.omit_project_prefix = true;
    metadata.build.strip_comments = true;

    let analyzer = Analyzer::new(&metadata);
    let mut context = Context::default();

    let errors = analyzer.analyze_pass1(&"prj", &parser.veryl);
    assert!(errors.is_empty(), "analyze_pass1 errors: {errors:?}");
    let errors = Analyzer::analyze_post_pass1();
    assert!(errors.is_empty(), "analyze_post_pass1 errors: {errors:?}");
    let errors = analyzer.analyze_pass2(&"prj", &parser.veryl, &mut context, None);
    assert!(errors.is_empty(), "analyze_pass2 errors: {errors:?}");
    let errors = Analyzer::analyze_post_pass2();
    assert!(errors.is_empty(), "analyze_post_pass2 errors: {errors:?}");

    let mut emitter = Emitter::new(
        &metadata,
        &PathBuf::from("bench.veryl"),
        &PathBuf::from("bench.sv"),
        &PathBuf::from("bench.sv.map"),
    );
    emitter.emit(&"prj", &parser.veryl);
    emitter.as_str().to_string()
}

/// Strip `#[test(...)] embed (inline) sv{{{ ... }}}` blocks from Veryl source.
fn strip_test_blocks(code: &str) -> String {
    let mut result = String::new();
    let mut chars = code.chars().peekable();

    while let Some(&c) = chars.peek() {
        // Check for #[test
        if c == '#' {
            let rest: String = chars.clone().take(6).collect();
            if rest.starts_with("#[test") {
                // Skip until we find the closing }}} of the embed block
                let mut found_triple_brace = false;
                while let Some(ch) = chars.next() {
                    if ch == '{' && !found_triple_brace {
                        // Check for {{{
                        let next2: String = chars.clone().take(2).collect();
                        if next2 == "{{" {
                            found_triple_brace = true;
                            chars.next();
                            chars.next();
                            continue;
                        }
                    }
                    if found_triple_brace {
                        if ch == '}' {
                            let next2: String = chars.clone().take(2).collect();
                            if next2 == "}}" {
                                chars.next();
                                chars.next();
                                break;
                            }
                        }
                    }
                }
                continue;
            }
        }
        result.push(chars.next().unwrap());
    }
    result
}

fn read_veryl(veryl_std: &Path, parts: &[&str]) -> String {
    let mut path = veryl_std.to_path_buf();
    for p in parts {
        path = path.join(p);
    }
    strip_test_blocks(&std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("Failed to read {}: {}", path.display(), e)
    }))
}

/// Strip the sourceMappingURL comment from emitter output.
fn strip_sourcemap(sv: &str) -> String {
    sv.lines()
        .filter(|l| !l.starts_with("//# sourceMappingURL="))
        .collect::<Vec<_>>()
        .join("\n")
}

fn main() {
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let veryl_std = project_root.join("deps/veryl/crates/std/veryl/src");
    let out_dir = project_root.join("benches/verilator");

    // --- Countones (W=64): combinational popcount tree ---
    let countones_code = format!(
        "{}\n{}",
        read_veryl(&veryl_std, &["countones", "countones.veryl"]),
        r#"
module Top (
    i_data: input  logic<64>,
    o_ones: output logic<7>,
) {
    inst u: countones #(W: 64) (i_data, o_ones);
}
"#
    );
    let sv = strip_sourcemap(&emit_sv(&countones_code));
    std::fs::write(out_dir.join("Countones.sv"), &sv).unwrap();
    println!("Wrote Countones.sv ({} bytes)", sv.len());

    // --- std::counter (WIDTH=32) ---
    let std_counter_code = format!(
        "{}\n{}",
        read_veryl(&veryl_std, &["counter", "counter.veryl"]),
        r#"
module Top (
    clk    : input  clock,
    rst    : input  reset,
    i_up   : input  logic,
    o_count: output logic<32>,
) {
    inst u: counter #(WIDTH: 32) (
        i_clk       : clk,
        i_rst       : rst,
        i_clear     : 1'b0,
        i_set       : 1'b0,
        i_set_value : 32'b0,
        i_up,
        i_down      : 1'b0,
        o_count,
        o_count_next: _,
        o_wrap_around: _,
    );
}
"#
    );
    let sv = strip_sourcemap(&emit_sv(&std_counter_code));
    std::fs::write(out_dir.join("StdCounter.sv"), &sv).unwrap();
    println!("Wrote StdCounter.sv ({} bytes)", sv.len());

    // --- std::gray_counter (WIDTH=32) ---
    let gray_counter_code = format!(
        "{}\n{}\n{}\n{}",
        read_veryl(&veryl_std, &["counter", "counter.veryl"]),
        read_veryl(&veryl_std, &["gray", "gray_encoder.veryl"]),
        read_veryl(&veryl_std, &["gray", "gray_counter.veryl"]),
        r#"
module Top (
    clk    : input  clock,
    rst    : input  reset,
    i_up   : input  logic,
    o_count: output logic<32>,
) {
    inst u: gray_counter #(WIDTH: 32) (
        i_clk       : clk,
        i_rst       : rst,
        i_clear     : 1'b0,
        i_set       : 1'b0,
        i_set_value : 32'b0,
        i_up,
        i_down      : 1'b0,
        o_count,
        o_count_next: _,
        o_wrap_around: _,
    );
}
"#
    );
    let sv = strip_sourcemap(&emit_sv(&gray_counter_code));
    std::fs::write(out_dir.join("GrayCounter.sv"), &sv).unwrap();
    println!("Wrote GrayCounter.sv ({} bytes)", sv.len());
}
