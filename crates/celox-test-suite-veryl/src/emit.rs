//! Helpers for compiling Veryl test sources through the emitted SystemVerilog
//! frontend.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use veryl_analyzer::{Analyzer, Context, attribute_table, ir::Ir, symbol_table};
use veryl_emitter::Emitter;
use veryl_metadata::Metadata;
use veryl_parser::Parser;

/// Owned SystemVerilog sources produced from a set of Veryl sources.
pub struct EmittedSources {
    sources: Vec<(String, PathBuf)>,
    events: BTreeMap<String, BTreeMap<String, bool>>,
    testbench: bool,
}

impl EmittedSources {
    /// Whether the selected top is an emitted native testbench.
    pub fn is_testbench(&self) -> bool {
        self.testbench
    }

    /// Event polarities from declared port types, including ports forwarded to
    /// child modules. `true` means a rising edge, `false` a falling edge.
    pub fn event_edges(&self, top: &str) -> Option<&BTreeMap<String, bool>> {
        self.events.get(top)
    }

    /// Borrow the emitted sources in the form accepted by SV compiler adapters.
    pub fn as_sv_sources(&self) -> Vec<(&str, &Path)> {
        self.sources
            .iter()
            .map(|(source, path)| (source.as_str(), path.as_path()))
            .collect()
    }
}

/// Analyze and emit every Veryl source in `sources` as SystemVerilog.
pub fn emit_veryl_sources(sources: &[(&str, &Path)]) -> EmittedSources {
    emit_sources(sources, None)
}

/// Emit a verification design, including a native testbench when it is the top.
/// Assertion failures are fatal; success requires reaching an explicit $finish.
/// Native-only components (such as $tb::clock_gen) are not translated here.
pub fn emit_verification_sources(sources: &[(&str, &Path)], top: &str) -> EmittedSources {
    emit_sources(sources, Some(top))
}

fn emit_sources(sources: &[(&str, &Path)], testbench: Option<&str>) -> EmittedSources {
    symbol_table::clear();
    attribute_table::clear();

    let mut metadata = Metadata::create_default("prj").unwrap();
    // In-memory test designs name their top module without a project prefix.
    metadata.build.omit_project_prefix = true;
    metadata.build.strip_comments = true;

    let analyzer = Analyzer::new(&metadata);
    let mut parsed_sources = Vec::with_capacity(sources.len());

    for (code, path) in sources {
        let parsed = Parser::parse(code, path).unwrap_or_else(|error| {
            panic!("failed to parse Veryl source {}: {error}", path.display())
        });
        let errors = analyzer.analyze_pass1("prj", &parsed.veryl);
        assert!(
            !errors.iter().any(|error| error.is_error()),
            "Veryl analyze_pass1 errors in {}: {errors:?}",
            path.display()
        );
        parsed_sources.push((*code, *path, parsed));
    }

    let errors = Analyzer::analyze_post_pass1();
    assert!(
        !errors.iter().any(|error| error.is_error()),
        "Veryl analyze_post_pass1 errors: {errors:?}"
    );

    let mut context = Context::default();
    let mut ir = Ir::default();
    for (_, _, parsed) in &parsed_sources {
        let errors = analyzer.analyze_pass2(&parsed.veryl, &mut context, Some(&mut ir));
        assert!(
            !errors.iter().any(|error| error.is_error()),
            "Veryl analyze_pass2 errors: {errors:?}"
        );
    }

    let errors = Analyzer::analyze_post_pass2(&ir);
    assert!(
        !errors.iter().any(|error| error.is_error()),
        "Veryl analyze_post_pass2 errors: {errors:?}"
    );

    let mut test_defines = String::new();
    let mut native_testbench = false;
    if let Some(top) = testbench {
        for (name, _) in symbol_table::get_tests("prj") {
            test_defines.push_str(&format!("`define __veryl_test_prj_{name}__\n"));
        }

        // Analyze with the native-test rules first, then remove only the
        // emitter's omission gate. Keep the original AST and hierarchy writes.
        for mut symbol in symbol_table::get_all() {
            if symbol.token.to_string() == top
                && symbol.namespace.to_string() == "prj"
                && let veryl_analyzer::symbol::SymbolKind::Module(module) = &mut symbol.kind
                && module.test.as_ref().is_some_and(|test| {
                    matches!(test.r#type, veryl_analyzer::symbol::TestType::Native)
                })
            {
                module.test = None;
                symbol_table::update(symbol);
                native_testbench = true;
            }
        }
    }

    let emitted = parsed_sources
        .into_iter()
        .enumerate()
        .map(|(index, (code, source_path, parsed))| {
            let output_path = emitted_path(index, source_path);
            let map_path = output_path.with_extension("sv.map");
            let mut emitter = Emitter::new(&metadata, "prj", source_path, &output_path, &map_path);
            emitter.emit(&parsed.veryl, code);
            let source = if native_testbench {
                format!(
                    "{test_defines}{}{}",
                    TESTBENCH_MACROS,
                    translate_testbench_tasks(emitter.as_str())
                )
            } else {
                emitter.as_str().to_string()
            };
            (source, output_path)
        })
        .collect();

    let mut events = BTreeMap::new();
    for component in &ir.components {
        if let veryl_analyzer::ir::Component::Module(module) = component {
            let mut ports = BTreeMap::new();
            for (path, (ty, _)) in &module.port_types {
                use veryl_analyzer::ir::TypeKind;
                let rising = match ty.kind {
                    TypeKind::Clock | TypeKind::ClockPosedge | TypeKind::ResetAsyncHigh => true,
                    TypeKind::ClockNegedge | TypeKind::Reset | TypeKind::ResetAsyncLow => false,
                    _ => continue,
                };
                ports.insert(path.to_string(), rising);
            }
            events.insert(module.name.to_string(), ports);
        }
    }
    EmittedSources {
        sources: emitted,
        events,
        testbench: native_testbench,
    }
}

fn emitted_path(index: usize, source_path: &Path) -> PathBuf {
    if source_path.as_os_str().is_empty() {
        PathBuf::from(format!("source_{index}.sv"))
    } else {
        let mut path = source_path.to_path_buf();
        path.set_extension("sv");
        path
    }
}

const TESTBENCH_MACROS: &str = r#"
`define CELOX_SUITE_ASSERT(condition) assert (condition) else $fatal(1, "native testbench assertion failed")
`ifdef CELOX_SUITE_ICARUS
`define CELOX_SUITE_FINISH(unused) begin $celox_suite_finish; $finish; end
`else
`define CELOX_SUITE_FINISH(unused) $finish
`endif
"#;

// Replace task identifiers, not text inside strings or comments. The SV
// preprocessor handles balanced expressions and multiline assertion arguments.
fn translate_testbench_tasks(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut output = String::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                } else if bytes[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        } else if source[i..].starts_with("//") {
            i += source[i..].find('\n').unwrap_or(bytes.len() - i);
        } else if source[i..].starts_with("/*") {
            i += source[i + 2..]
                .find("*/")
                .map_or(bytes.len() - i, |n| n + 4);
        } else if bytes[i] == b'$' {
            let end = (i + 1..bytes.len())
                .find(|&n| !bytes[n].is_ascii_alphanumeric() && bytes[n] != b'_')
                .unwrap_or(bytes.len());
            let replacement = match &source[i..end] {
                "$assert" => Some("`CELOX_SUITE_ASSERT"),
                "$finish" => Some("`CELOX_SUITE_FINISH"),
                _ => None,
            };
            if let Some(replacement) = replacement {
                output.push_str(&source[start..i]);
                output.push_str(replacement);
                start = end;
            }
            i = end;
        } else {
            i += source[i..].chars().next().unwrap().len_utf8();
        }
    }
    output.push_str(&source[start..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn testbench_task_translation_preserves_strings_comments_and_other_tasks() {
        let source = r#"$display("日本語 $assert(0) and \"$finish()\"");
// $assert(0);
/* $finish(); */
$assertion(0);
$assert(
    (a == b)
);
$finish();"#;
        let translated = translate_testbench_tasks(source);
        assert!(translated.contains(r#"$display("日本語 $assert(0) and \"$finish()\"");"#));
        assert!(translated.contains("// $assert(0);"));
        assert!(translated.contains("/* $finish(); */"));
        assert!(translated.contains("$assertion(0);"));
        assert!(translated.contains("`CELOX_SUITE_ASSERT(\n    (a == b)\n);"));
        assert!(translated.contains("`CELOX_SUITE_FINISH();"));
    }
}
